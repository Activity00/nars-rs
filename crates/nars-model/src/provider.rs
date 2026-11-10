//! Model provider abstraction.
//!
//! A provider only transports messages and returns a validated [`AgentIr`].
//! Provider adapters must never let a model choose a tool: they parse the
//! structured response and hand it to the runtime's validation layer.

use std::fmt;

use async_trait::async_trait;
use nars_agent_ir::{AgentIr, TokenUsage};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Author of a model message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// System instruction.
    System,
    /// User message.
    User,
    /// Previous assistant response.
    Assistant,
}

impl Role {
    /// The role string used by OpenAI-compatible and Ollama APIs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

/// One provider-neutral message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMessage {
    /// Author.
    pub role: Role,
    /// Text content.
    pub content: String,
}

impl ModelMessage {
    /// Builds a system message.
    #[must_use]
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
        }
    }

    /// Builds a user message.
    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }

    /// Builds an assistant message.
    #[must_use]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

/// A provider-neutral completion request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelRequest {
    /// Provider model identifier.
    pub model: String,
    /// Conversation messages.
    pub messages: Vec<ModelMessage>,
    /// Sampling temperature.
    pub temperature: f64,
    /// Completion token budget.
    pub max_tokens: u32,
    /// Optional JSON Schema the response should satisfy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<Value>,
    /// Provider request timeout.
    pub timeout_ms: u64,
}

impl ModelRequest {
    /// Builds a request with conservative defaults.
    #[must_use]
    pub fn new(model: impl Into<String>, messages: Vec<ModelMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            temperature: 0.2,
            max_tokens: 4096,
            json_schema: None,
            timeout_ms: 120_000,
        }
    }
}

/// A parsed model response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelResponse {
    /// Validated structured output.
    pub ir: AgentIr,
    /// Provider usage accounting.
    pub usage: TokenUsage,
    /// Raw response body, retained for debugging.
    pub raw: String,
    /// Provider name.
    pub provider: String,
    /// Wall-clock latency.
    pub latency_ms: u64,
}

/// Model errors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelError {
    /// Human-readable message.
    pub message: String,
    /// Whether a retry could succeed.
    pub retryable: bool,
    /// HTTP status when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

impl ModelError {
    /// A non-retryable provider error.
    #[must_use]
    pub fn provider(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
            status: None,
        }
    }

    /// A retryable transport or rate-limit error.
    #[must_use]
    pub fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
            status: None,
        }
    }

    /// An HTTP error with status-derived retryability.
    #[must_use]
    pub fn http(status: u16, body: impl Into<String>) -> Self {
        let body = body.into();
        let truncated = body.chars().take(2_000).collect::<String>();
        Self {
            message: format!("provider returned HTTP {status}: {truncated}"),
            retryable: status == 429 || status >= 500,
            status: Some(status),
        }
    }

    /// An invalid structured response.
    #[must_use]
    pub fn invalid_output(message: impl Into<String>) -> Self {
        Self {
            message: format!("invalid structured output: {}", message.into()),
            retryable: true,
            status: None,
        }
    }
}

impl fmt::Display for ModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ModelError {}

/// A text-completion provider constrained to Agent IR output.
#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// Stable provider name for logs and the UI.
    fn name(&self) -> &str;

    /// Performs one completion.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError`] for transport failures or invalid structured
    /// output.
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError>;
}

/// Retry policy for provider calls.
#[derive(Clone, Copy, Debug)]
pub struct RetryConfig {
    /// Maximum attempts including the first.
    pub max_attempts: u32,
    /// Initial backoff.
    pub base_delay_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay_ms: 250,
        }
    }
}

/// Calls a provider with bounded exponential backoff.
///
/// # Errors
///
/// Returns the last error when all attempts fail or the error is not retryable.
pub async fn complete_with_retry(
    provider: &dyn ModelProvider,
    request: ModelRequest,
    config: RetryConfig,
) -> Result<ModelResponse, ModelError> {
    let attempts = config.max_attempts.max(1);
    let mut last_error = None;
    let mut request = request;
    for attempt in 0..attempts {
        match provider.complete(request.clone()).await {
            Ok(response) => return Ok(response),
            Err(error) => {
                let retryable = error.retryable;
                // Models occasionally answer conversationally instead of with
                // the required JSON object. Retrying the identical prompt tends
                // to reproduce it, so ask explicitly for JSON on the next try.
                if error.message.contains("invalid structured output") {
                    request.messages.push(ModelMessage::user(
                        "Your previous reply was not a JSON object and was rejected. Reply with \
                         exactly one Agent IR JSON object and nothing else: no prose, no markdown \
                         fences. Even for greetings or questions about yourself, use a \
                         final_answer inside the JSON object.",
                    ));
                }
                last_error = Some(error);
                if !retryable || attempt + 1 == attempts {
                    break;
                }
                let delay = config.base_delay_ms.saturating_mul(1_u64 << attempt.min(6));
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
        }
    }
    Err(last_error.unwrap_or_else(|| ModelError::provider("provider produced no response")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_defaults_are_conservative() {
        let request = ModelRequest::new("m", vec![ModelMessage::user("hi")]);
        assert!(request.temperature < 0.5);
        assert_eq!(request.max_tokens, 4096);
    }

    #[tokio::test]
    async fn invalid_output_is_retried_with_a_json_reminder() {
        struct Flaky {
            calls: std::sync::atomic::AtomicU32,
        }

        #[async_trait::async_trait]
        impl ModelProvider for Flaky {
            fn name(&self) -> &str {
                "flaky"
            }

            async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
                let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if call == 0 {
                    return Err(ModelError::invalid_output(
                        "no JSON object found in response",
                    ));
                }
                assert!(
                    request
                        .messages
                        .iter()
                        .any(|message| message.content.contains("was not a JSON object")),
                    "retry should carry a repair instruction"
                );
                Ok(ModelResponse {
                    ir: nars_agent_ir::AgentIr::FinalAnswer(nars_agent_ir::FinalAnswer {
                        text: "ok".to_owned(),
                        success: true,
                        artifacts: Vec::new(),
                    }),
                    usage: TokenUsage::default(),
                    raw: String::new(),
                    provider: "flaky".to_owned(),
                    latency_ms: 0,
                })
            }
        }

        let provider = Flaky {
            calls: std::sync::atomic::AtomicU32::new(0),
        };
        let request = ModelRequest::new("m", vec![ModelMessage::user("hi")]);
        let response = complete_with_retry(
            &provider,
            request,
            RetryConfig {
                max_attempts: 2,
                base_delay_ms: 1,
            },
        )
        .await
        .expect("second attempt succeeds");
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        let _ = response;
    }

    #[test]
    fn http_errors_are_retryable_only_for_rate_limits_and_5xx() {
        assert!(ModelError::http(429, "slow down").retryable);
        assert!(ModelError::http(503, "unavailable").retryable);
        assert!(!ModelError::http(400, "bad request").retryable);
    }
}
