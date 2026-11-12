//! OpenAI-compatible chat-completions adapter.
//!
//! Works with OpenAI, DeepSeek, OpenRouter, vLLM, LM Studio, and Ollama's
//! OpenAI-compatible endpoint.

use async_trait::async_trait;
use nars_agent_ir::TokenUsage;
use serde_json::{Value, json};

use crate::http::{bearer, normalize_base, post_json};
use crate::provider::{ModelError, ModelProvider, ModelRequest, ModelResponse};
use crate::structured::parse_agent_ir;

/// Configuration for an OpenAI-compatible provider.
#[derive(Clone, Debug)]
pub struct OpenAiConfig {
    /// Base URL ending before `/chat/completions`.
    pub base_url: String,
    /// Optional API key.
    pub api_key: Option<String>,
    /// Default model identifier.
    pub model: String,
    /// Optional organization header.
    pub organization: Option<String>,
    /// Whether to request a strict JSON Schema response format.
    pub use_json_schema: bool,
}

impl OpenAiConfig {
    /// Creates a configuration for a base URL and model.
    #[must_use]
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: None,
            model: model.into(),
            organization: None,
            use_json_schema: false,
        }
    }
}

/// OpenAI-compatible provider.
pub struct OpenAiProvider {
    config: OpenAiConfig,
    client: reqwest::Client,
}

impl OpenAiProvider {
    /// Builds a provider.
    ///
    /// # Errors
    ///
    /// Returns an error when the base URL or HTTP client is invalid.
    pub fn new(config: OpenAiConfig) -> Result<Self, ModelError> {
        if config.base_url.trim().is_empty() {
            return Err(ModelError::provider("OpenAI base URL must not be empty"));
        }
        if config.model.trim().is_empty() {
            return Err(ModelError::provider("OpenAI model must not be empty"));
        }
        let client = reqwest::Client::builder().build().map_err(|error| {
            ModelError::provider(format!("failed to build HTTP client: {error}"))
        })?;
        Ok(Self { config, client })
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", normalize_base(&self.config.base_url))
    }

    fn build_body(&self, request: &ModelRequest) -> Value {
        let messages = request
            .messages
            .iter()
            .map(|message| json!({"role": message.role.as_str(), "content": message.content}))
            .collect::<Vec<_>>();
        let mut body = json!({
            "model": request.model,
            "messages": messages,
            "temperature": request.temperature,
            "max_tokens": request.max_tokens,
            "stream": false
        });
        if let Some(schema) = &request.json_schema {
            let response_format = if self.config.use_json_schema {
                json!({
                    "type": "json_schema",
                    "json_schema": {
                        "name": "agent_ir",
                        "schema": schema,
                        "strict": false
                    }
                })
            } else {
                json!({"type": "json_object"})
            };
            body["response_format"] = response_format;
        }
        body
    }

    fn headers(&self) -> Vec<(String, String)> {
        let mut headers = bearer(self.config.api_key.as_deref());
        if let Some(organization) = &self.config.organization {
            headers.push(("openai-organization".to_owned(), organization.clone()));
        }
        headers
    }

    fn parse(&self, request: &ModelRequest, body: &str) -> Result<ModelResponse, ModelError> {
        let value: Value = serde_json::from_str(body).map_err(|error| {
            ModelError::invalid_output(format!("provider response is not JSON: {error}"))
        })?;
        let content = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ModelError::invalid_output("response is missing choices[0].message.content")
            })?;
        let ir = parse_agent_ir(content).map_err(|error| error.with_raw(content.to_owned()))?;
        let usage = value
            .get("usage")
            .map_or_else(TokenUsage::default, |usage| TokenUsage {
                prompt_tokens: usage
                    .get("prompt_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                completion_tokens: usage
                    .get("completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                total_tokens: usage
                    .get("total_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            });
        let _ = request;
        Ok(ModelResponse {
            ir,
            usage,
            raw: content.to_owned(),
            provider: self.name().to_owned(),
            latency_ms: 0,
        })
    }
}

#[async_trait]
impl ModelProvider for OpenAiProvider {
    fn name(&self) -> &str {
        "openai-compatible"
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        let started = std::time::Instant::now();
        let body = self.build_body(&request);
        let (_, text) = post_json(
            &self.client,
            &self.endpoint(),
            &self.headers(),
            &body,
            request.timeout_ms,
        )
        .await?;
        let mut response = self.parse(&request, &text)?;
        response.latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ModelMessage, Role};

    fn provider() -> OpenAiProvider {
        OpenAiProvider::new(OpenAiConfig::new("https://example.test/v1", "gpt-test")).unwrap()
    }

    #[test]
    fn endpoint_appends_the_path_once() {
        assert_eq!(
            provider().endpoint(),
            "https://example.test/v1/chat/completions"
        );
        let provider =
            OpenAiProvider::new(OpenAiConfig::new("https://example.test/v1/", "m")).unwrap();
        assert_eq!(
            provider.endpoint(),
            "https://example.test/v1/chat/completions"
        );
    }

    #[test]
    fn body_carries_messages_and_json_mode() {
        let mut request = ModelRequest::new(
            "gpt-test",
            vec![ModelMessage::system("s"), ModelMessage::user("u")],
        );
        request.json_schema = Some(json!({"type": "object"}));
        let body = provider().build_body(&request);
        assert_eq!(body["messages"][0]["role"], Role::System.as_str());
        assert_eq!(body["response_format"]["type"], "json_object");
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn parses_content_and_usage() {
        let body = r#"{
            "choices": [{"message": {"role": "assistant", "content": "{\"kind\":\"final_answer\",\"text\":\"ok\"}"}}],
            "usage": {"prompt_tokens": 12, "completion_tokens": 3, "total_tokens": 15}
        }"#;
        let request = ModelRequest::new("m", vec![ModelMessage::user("u")]);
        let response = provider().parse(&request, body).unwrap();
        assert_eq!(response.usage.total_tokens, 15);
        assert!(matches!(
            response.ir,
            nars_agent_ir::AgentIr::FinalAnswer(_)
        ));
    }

    #[test]
    fn rejects_malformed_responses() {
        let request = ModelRequest::new("m", vec![ModelMessage::user("u")]);
        assert!(provider().parse(&request, "{}").is_err());
    }
}
