//! Anthropic Messages API adapter.

use async_trait::async_trait;
use nars_agent_ir::TokenUsage;
use serde_json::{Value, json};

use crate::http::{normalize_base, post_json};
use crate::provider::{ModelError, ModelProvider, ModelRequest, ModelResponse, Role};
use crate::structured::parse_agent_ir;

/// Configuration for the Anthropic provider.
#[derive(Clone, Debug)]
pub struct AnthropicConfig {
    /// Base URL, normally `https://api.anthropic.com`.
    pub base_url: String,
    /// API key.
    pub api_key: String,
    /// Default model identifier.
    pub model: String,
}

impl AnthropicConfig {
    /// Creates a configuration.
    #[must_use]
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: "https://api.anthropic.com".to_owned(),
            api_key: api_key.into(),
            model: model.into(),
        }
    }
}

/// Anthropic provider.
pub struct AnthropicProvider {
    config: AnthropicConfig,
    client: reqwest::Client,
}

impl AnthropicProvider {
    /// Builds a provider.
    ///
    /// # Errors
    ///
    /// Returns an error when the API key, model, or client is invalid.
    pub fn new(config: AnthropicConfig) -> Result<Self, ModelError> {
        if config.api_key.trim().is_empty() {
            return Err(ModelError::provider("Anthropic API key must not be empty"));
        }
        if config.model.trim().is_empty() {
            return Err(ModelError::provider("Anthropic model must not be empty"));
        }
        let client = reqwest::Client::builder().build().map_err(|error| {
            ModelError::provider(format!("failed to build HTTP client: {error}"))
        })?;
        Ok(Self { config, client })
    }

    fn endpoint(&self) -> String {
        let base = normalize_base(&self.config.base_url);
        if base.ends_with("/v1") {
            format!("{base}/messages")
        } else {
            format!("{base}/v1/messages")
        }
    }

    fn build_body(request: &ModelRequest) -> Value {
        let system = request
            .messages
            .iter()
            .filter(|message| message.role == Role::System)
            .map(|message| message.content.clone())
            .collect::<Vec<_>>()
            .join("\n\n");
        let messages = request
            .messages
            .iter()
            .filter(|message| message.role != Role::System)
            .map(|message| {
                json!({
                    "role": if message.role == Role::Assistant { "assistant" } else { "user" },
                    "content": message.content
                })
            })
            .collect::<Vec<_>>();
        let mut body = json!({
            "model": request.model,
            "max_tokens": request.max_tokens,
            "temperature": request.temperature,
            "messages": messages
        });
        if !system.is_empty() {
            body["system"] = Value::String(system);
        }
        body
    }

    fn headers(&self) -> Vec<(String, String)> {
        vec![
            ("x-api-key".to_owned(), self.config.api_key.clone()),
            ("anthropic-version".to_owned(), "2023-06-01".to_owned()),
        ]
    }

    fn parse(&self, body: &str) -> Result<ModelResponse, ModelError> {
        let value: Value = serde_json::from_str(body).map_err(|error| {
            ModelError::invalid_output(format!("provider response is not JSON: {error}"))
        })?;
        let content = value
            .get("content")
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|block| block.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .filter(|text| !text.is_empty())
            .ok_or_else(|| ModelError::invalid_output("response has no text content block"))?;
        let ir = parse_agent_ir(&content)?;
        let usage = value
            .get("usage")
            .map_or_else(TokenUsage::default, |usage| {
                let prompt = usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let completion = usage
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                TokenUsage {
                    prompt_tokens: prompt,
                    completion_tokens: completion,
                    total_tokens: prompt + completion,
                }
            });
        Ok(ModelResponse {
            ir,
            usage,
            raw: content,
            provider: self.name().to_owned(),
            latency_ms: 0,
        })
    }
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        let started = std::time::Instant::now();
        let body = Self::build_body(&request);
        let (_, text) = post_json(
            &self.client,
            &self.endpoint(),
            &self.headers(),
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

    fn provider() -> AnthropicProvider {
        AnthropicProvider::new(AnthropicConfig::new("key", "claude-test")).unwrap()
    }

    #[test]
    fn endpoint_handles_both_base_shapes() {
        assert_eq!(
            provider().endpoint(),
            "https://api.anthropic.com/v1/messages"
        );
        let mut config = AnthropicConfig::new("key", "m");
        config.base_url = "https://example.test/v1".to_owned();
        assert_eq!(
            AnthropicProvider::new(config).unwrap().endpoint(),
            "https://example.test/v1/messages"
        );
    }

    #[test]
    fn system_messages_are_hoisted() {
        let request = ModelRequest::new(
            "claude-test",
            vec![ModelMessage::system("be careful"), ModelMessage::user("hi")],
        );
        let body = AnthropicProvider::build_body(&request);
        assert_eq!(body["system"], "be careful");
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn parses_text_blocks() {
        let body = r#"{
            "content": [{"type": "text", "text": "{\"kind\":\"final_answer\",\"text\":\"ok\"}"}],
            "usage": {"input_tokens": 8, "output_tokens": 2}
        }"#;
        let response = provider().parse(body).unwrap();
        assert_eq!(response.usage.total_tokens, 10);
    }
}
