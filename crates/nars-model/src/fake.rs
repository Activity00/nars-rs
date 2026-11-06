//! Deterministic providers used by tests, replay, and offline demos.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use nars_agent_ir::{AgentIr, FinalAnswer, TokenUsage};

use crate::provider::{ModelError, ModelProvider, ModelRequest, ModelResponse};

/// A provider that returns a pre-scripted sequence of responses.
///
/// Tests use it to drive the runtime without network access. Every request is
/// recorded so assertions can inspect what the runtime actually asked for.
pub struct ScriptedProvider {
    name: String,
    script: Mutex<VecDeque<Result<AgentIr, ModelError>>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedProvider {
    /// Creates a provider with a sequence of successful responses.
    #[must_use]
    pub fn new(responses: Vec<AgentIr>) -> Self {
        Self {
            name: "scripted".to_owned(),
            script: Mutex::new(responses.into_iter().map(Ok).collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Creates a provider with an explicit result sequence.
    #[must_use]
    pub fn from_results(results: Vec<Result<AgentIr, ModelError>>) -> Self {
        Self {
            name: "scripted".to_owned(),
            script: Mutex::new(results.into()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Appends one response.
    pub fn push(&self, response: AgentIr) {
        self.script.lock().unwrap().push_back(Ok(response));
    }

    /// Appends one error.
    pub fn push_error(&self, error: ModelError) {
        self.script.lock().unwrap().push_back(Err(error));
    }

    /// Number of unserved responses.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.script.lock().unwrap().len()
    }

    /// All requests received so far.
    #[must_use]
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl ModelProvider for ScriptedProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        self.requests.lock().unwrap().push(request);
        let next = self.script.lock().unwrap().pop_front();
        match next {
            Some(Ok(ir)) => Ok(ModelResponse {
                ir,
                usage: TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                },
                raw: String::from("scripted"),
                provider: self.name.clone(),
                latency_ms: 0,
            }),
            Some(Err(error)) => Err(error),
            None => Err(ModelError::provider(
                "scripted provider has no remaining responses",
            )),
        }
    }
}

/// A provider that echoes the last user message as a final answer.
///
/// This is the default offline mode: it exercises the full server and UI path
/// without pretending to be a real model.
pub struct EchoProvider {
    name: String,
}

impl Default for EchoProvider {
    fn default() -> Self {
        Self {
            name: "echo".to_owned(),
        }
    }
}

#[async_trait]
impl ModelProvider for EchoProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        let last_user = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == crate::provider::Role::User)
            .map_or("", |message| message.content.as_str());
        Ok(ModelResponse {
            ir: AgentIr::FinalAnswer(FinalAnswer {
                text: format!(
                    "**Offline echo provider** — configure `NARS_AGENT_PROVIDER` to use a real model.\n\n\
                     Received:\n\n> {}",
                    last_user.lines().take(20).collect::<Vec<_>>().join("\n> ")
                ),
                success: true,
                artifacts: Vec::new(),
            }),
            usage: TokenUsage::default(),
            raw: String::new(),
            provider: self.name.clone(),
            latency_ms: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ModelMessage;

    #[tokio::test]
    async fn scripted_provider_returns_responses_in_order() {
        let provider = ScriptedProvider::new(vec![
            AgentIr::Clarification(nars_agent_ir::Clarification {
                question: "which?".to_owned(),
                options: Vec::new(),
            }),
            AgentIr::FinalAnswer(FinalAnswer {
                text: "done".to_owned(),
                success: true,
                artifacts: Vec::new(),
            }),
        ]);
        let request = ModelRequest::new("m", vec![ModelMessage::user("hi")]);
        let first = provider.complete(request.clone()).await.unwrap();
        assert!(matches!(first.ir, AgentIr::Clarification(_)));
        let second = provider.complete(request).await.unwrap();
        assert!(matches!(second.ir, AgentIr::FinalAnswer(_)));
        assert_eq!(provider.remaining(), 0);
        assert_eq!(provider.requests().len(), 2);
    }

    #[tokio::test]
    async fn exhausted_script_is_an_error() {
        let provider = ScriptedProvider::new(Vec::new());
        let request = ModelRequest::new("m", vec![ModelMessage::user("hi")]);
        assert!(provider.complete(request).await.is_err());
    }
}
