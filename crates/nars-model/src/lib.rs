//! Model providers for nars-agent.
//!
//! Providers translate messages to a vendor API and parse exactly one
//! structured [`nars_agent_ir::AgentIr`] value. Tool selection never happens
//! here.

pub mod anthropic;
pub mod demo;
pub mod fake;
mod http;
pub mod ollama;
pub mod openai;
pub mod provider;
pub mod schema;
pub mod structured;

use std::sync::Arc;

pub use anthropic::{AnthropicConfig, AnthropicProvider};
pub use demo::DemoProvider;
pub use fake::{EchoProvider, ScriptedProvider};
pub use ollama::{OllamaConfig, OllamaProvider};
pub use openai::{OpenAiConfig, OpenAiProvider};
pub use provider::{
    ModelError, ModelMessage, ModelProvider, ModelRequest, ModelResponse, RetryConfig, Role,
    complete_with_retry,
};
pub use schema::agent_ir_schema;

/// Declarative provider selection.
#[derive(Clone, Debug, Default)]
pub enum ProviderConfig {
    /// Any OpenAI-compatible chat-completions endpoint.
    OpenAi(OpenAiConfig),
    /// Anthropic Messages API.
    Anthropic(AnthropicConfig),
    /// Ollama native API.
    Ollama(OllamaConfig),
    /// Offline deterministic investigation provider.
    #[default]
    Demo,
    /// Offline echo provider.
    Echo,
}

/// Builds a provider from declarative configuration.
///
/// # Errors
///
/// Returns a configuration error when required fields are missing.
pub fn build_provider(config: &ProviderConfig) -> Result<Arc<dyn ModelProvider>, ModelError> {
    match config {
        ProviderConfig::OpenAi(config) => Ok(Arc::new(OpenAiProvider::new(config.clone())?)),
        ProviderConfig::Anthropic(config) => Ok(Arc::new(AnthropicProvider::new(config.clone())?)),
        ProviderConfig::Ollama(config) => Ok(Arc::new(OllamaProvider::new(config.clone())?)),
        ProviderConfig::Demo => Ok(Arc::new(DemoProvider::default())),
        ProviderConfig::Echo => Ok(Arc::new(EchoProvider::default())),
    }
}

/// Builds a provider from environment variables.
///
/// Supported variables:
///
/// - `NARS_AGENT_PROVIDER`: `openai`, `anthropic`, `ollama`, or `echo`
/// - `NARS_AGENT_MODEL`
/// - `NARS_AGENT_BASE_URL`
/// - `NARS_AGENT_API_KEY` (falls back to `OPENAI_API_KEY` / `ANTHROPIC_API_KEY`)
/// - `NARS_AGENT_JSON_SCHEMA=1`
///
/// # Errors
///
/// Returns a configuration error when a required value is missing.
pub fn from_env() -> Result<Arc<dyn ModelProvider>, ModelError> {
    let kind = std::env::var("NARS_AGENT_PROVIDER")
        .unwrap_or_else(|_| "demo".to_owned())
        .to_ascii_lowercase();
    let model = std::env::var("NARS_AGENT_MODEL").unwrap_or_else(|_| "gpt-4.1-mini".to_owned());
    let base_url = std::env::var("NARS_AGENT_BASE_URL").ok();
    let api_key = std::env::var("NARS_AGENT_API_KEY")
        .ok()
        .or_else(|| std::env::var("OPENAI_API_KEY").ok())
        .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok());

    let config = match kind.as_str() {
        "openai" | "openai-compatible" | "deepseek" => ProviderConfig::OpenAi(OpenAiConfig {
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_owned()),
            api_key,
            model,
            organization: std::env::var("NARS_AGENT_ORGANIZATION").ok(),
            use_json_schema: std::env::var("NARS_AGENT_JSON_SCHEMA")
                .is_ok_and(|value| value == "1"),
        }),
        "anthropic" | "claude" => ProviderConfig::Anthropic(AnthropicConfig {
            base_url: base_url.unwrap_or_else(|| "https://api.anthropic.com".to_owned()),
            api_key: api_key
                .ok_or_else(|| ModelError::provider("Anthropic requires an API key"))?,
            model,
        }),
        "ollama" => ProviderConfig::Ollama(OllamaConfig {
            base_url: base_url.unwrap_or_else(|| "http://localhost:11434".to_owned()),
            model,
        }),
        "demo" | "offline" => ProviderConfig::Demo,
        "echo" | "fake" => ProviderConfig::Echo,
        other => {
            return Err(ModelError::provider(format!(
                "unknown provider `{other}`; expected openai, anthropic, ollama, demo, or echo"
            )));
        }
    };
    build_provider(&config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_providers_build() {
        let provider = build_provider(&ProviderConfig::Echo).unwrap();
        assert_eq!(provider.name(), "echo");
        let provider = build_provider(&ProviderConfig::Demo).unwrap();
        assert_eq!(provider.name(), "demo");
    }

    #[test]
    fn openai_requires_a_base_url() {
        let mut config = OpenAiConfig::new("", "m");
        config.base_url = String::new();
        assert!(build_provider(&ProviderConfig::OpenAi(config)).is_err());
    }
}
