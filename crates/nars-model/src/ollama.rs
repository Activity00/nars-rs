//! Ollama native `/api/chat` adapter.

use async_trait::async_trait;
use nars_agent_ir::TokenUsage;
use serde_json::{Value, json};

use crate::http::{normalize_base, post_json};
use crate::provider::{ModelError, ModelProvider, ModelRequest, ModelResponse};
use crate::structured::parse_agent_ir;

/// Configuration for Ollama.
#[derive(Clone, Debug)]
pub struct OllamaConfig {
    /// Base URL, normally `http://localhost:11434`.
    pub base_url: String,
    /// Default model identifier.
    pub model: String,
}

impl OllamaConfig {
    /// Creates a configuration.
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            base_url: "http://localhost:11434".to_owned(),
            model: model.into(),
        }
    }
}

/// Ollama provider.
pub struct OllamaProvider {
    config: OllamaConfig,
    client: reqwest::Client,
}

impl OllamaProvider {
    /// Builds a provider.
    ///
    /// # Errors
    ///
    /// Returns an error when the model is empty or the client cannot be built.
    pub fn new(config: OllamaConfig) -> Result<Self, ModelError> {
        if config.model.trim().is_empty() {
            return Err(ModelError::provider("Ollama model must not be empty"));
        }
        let client = reqwest::Client::builder().build().map_err(|error| {
            ModelError::provider(format!("failed to build HTTP client: {error}"))
        })?;
        Ok(Self { config, client })
    }

    fn endpoint(&self) -> String {
        format!("{}/api/chat", normalize_base(&self.config.base_url))
    }

    fn build_body(request: &ModelRequest) -> Value {
        let messages = request
            .messages
            .iter()
            .map(|message| json!({"role": message.role.as_str(), "content": message.content}))
            .collect::<Vec<_>>();
        json!({
            "model": request.model,
            "messages": messages,
            "stream": false,
            "format": "json",
            "options": {
                "temperature": request.temperature,
                "num_predict": request.max_tokens
            }
        })
    }

    fn parse(&self, body: &str) -> Result<ModelResponse, ModelError> {
        let value: Value = serde_json::from_str(body).map_err(|error| {
            ModelError::invalid_output(format!("provider response is not JSON: {error}"))
        })?;
        let content = value
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .ok_or_else(|| ModelError::invalid_output("response is missing message.content"))?;
        let ir = parse_agent_ir(content).map_err(|error| error.with_raw(content.to_owned()))?;
        let prompt = value
            .get("prompt_eval_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let completion = value.get("eval_count").and_then(Value::as_u64).unwrap_or(0);
        Ok(ModelResponse {
            ir,
            usage: TokenUsage {
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: prompt + completion,
            },
            raw: content.to_owned(),
            provider: self.name().to_owned(),
            latency_ms: 0,
        })
    }
}

#[async_trait]
impl ModelProvider for OllamaProvider {
    fn name(&self) -> &str {
        "ollama"
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        let started = std::time::Instant::now();
        let body = Self::build_body(&request);
        let (_, text) = post_json(
            &self.client,
            &self.endpoint(),
            &[],
            &body,
            request.timeout_ms,
        )
        .await?;
        let mut response = self.parse(&text)?;
        response.latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ModelMessage;

    fn provider() -> OllamaProvider {
        OllamaProvider::new(OllamaConfig::new("qwen3")).unwrap()
    }

    #[test]
    fn endpoint_and_body_use_native_shape() {
        assert_eq!(provider().endpoint(), "http://localhost:11434/api/chat");
        let request = ModelRequest::new("qwen3", vec![ModelMessage::user("hi")]);
        let body = OllamaProvider::build_body(&request);
        assert_eq!(body["format"], "json");
        assert_eq!(body["stream"], false);
        assert_eq!(body["options"]["num_predict"], 4096);
    }

    #[test]
    fn parses_message_content() {
        let body = r#"{
            "message": {"role": "assistant", "content": "{\"kind\":\"final_answer\",\"text\":\"ok\"}"},
            "prompt_eval_count": 4,
            "eval_count": 2
        }"#;
        let response = provider().parse(body).unwrap();
        assert_eq!(response.usage.total_tokens, 6);
    }
}
