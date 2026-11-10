//! `ask_model`: a registered operation that lets the agent query the language
//! model as a tool instead of routing every decision through it.

use std::sync::Arc;

use nars_agent_ir::{AgentIr, SideEffect, ToolCall, ToolManifest, ToolResult};
use nars_model::{
    ModelMessage, ModelProvider, ModelRequest, RetryConfig, agent_ir_schema, complete_with_retry,
};
use nars_tools::manifest::{Tool, ToolContext, object_schema, string_schema};
use serde_json::json;

const SYSTEM: &str = "You are a knowledge assistant for a reasoning agent. Answer the question \
     concisely and factually. Put the answer in the final_answer `text` field of the Agent IR \
     JSON object and nowhere else. Do not invent file contents; say when you are unsure.";

/// A tool that asks the configured model a question and returns its answer.
pub struct AskModelTool {
    provider: Arc<dyn ModelProvider>,
    model: String,
    handle: tokio::runtime::Handle,
}

impl AskModelTool {
    /// Builds the tool from the server's provider and model.
    #[must_use]
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        model: impl Into<String>,
        handle: tokio::runtime::Handle,
    ) -> Self {
        Self {
            provider,
            model: model.into(),
            handle,
        }
    }
}

impl Tool for AskModelTool {
    fn manifest(&self) -> &ToolManifest {
        static MANIFEST: std::sync::OnceLock<ToolManifest> = std::sync::OnceLock::new();
        MANIFEST.get_or_init(|| ToolManifest {
            name: "ask_model".to_owned(),
            description: "Ask the language model a question when the agent's own knowledge is \
                          insufficient. Returns the answer as text; the answer becomes an \
                          observation and can be remembered."
                .to_owned(),
            input_schema: object_schema(
                json!({
                    "question": string_schema("The question to ask, in natural language.")
                }),
                &["question"],
            ),
            side_effect: SideEffect::NetworkAccess,
            timeout_ms: 120_000,
            requires_approval: false,
            version: "0.1.0".to_owned(),
            tags: vec!["builtin".to_owned(), "model".to_owned()],
        })
    }

    fn execute(&self, call: &ToolCall, _context: &ToolContext) -> ToolResult {
        let question = call
            .arguments
            .get("question")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if question.is_empty() {
            return ToolResult::failure(call.id.clone(), "question is required");
        }
        let mut request = ModelRequest::new(
            self.model.clone(),
            vec![ModelMessage::system(SYSTEM), ModelMessage::user(question)],
        );
        request.json_schema = Some(agent_ir_schema());
        let provider = Arc::clone(&self.provider);
        match self.handle.block_on(async move {
            complete_with_retry(provider.as_ref(), request, RetryConfig::default()).await
        }) {
            Ok(response) => match response.ir {
                AgentIr::FinalAnswer(answer) => ToolResult::success(call.id.clone(), answer.text),
                _ => ToolResult::failure(
                    call.id.clone(),
                    "ask_model expected a final_answer response",
                ),
            },
            Err(error) => ToolResult::failure(call.id.clone(), error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use nars_agent_ir::{FinalAnswer, RunId, ToolCallId};
    use nars_model::ScriptedProvider;
    use nars_tools::sandbox::Sandbox;

    use super::*;

    fn provider() -> Arc<dyn ModelProvider> {
        Arc::new(ScriptedProvider::new(vec![AgentIr::FinalAnswer(
            FinalAnswer {
                text: "NARS is a non-axiomatic reasoning system.".to_owned(),
                success: true,
                artifacts: Vec::new(),
            },
        )]))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn returns_the_model_answer_as_tool_output() {
        let tool = Arc::new(AskModelTool::new(
            provider(),
            "test-model",
            tokio::runtime::Handle::current(),
        ));
        let call = ToolCall {
            id: ToolCallId::new(),
            run_id: RunId::new(),
            tool: "ask_model".to_owned(),
            arguments: json!({"question": "what is NARS?"}),
            side_effect: SideEffect::NetworkAccess,
            requires_approval: false,
            created_at_ms: 0,
        };
        let sandbox = Sandbox::new(std::env::temp_dir()).expect("sandbox");
        let outcome =
            tokio::task::spawn_blocking(move || tool.execute(&call, &ToolContext::new(sandbox)))
                .await
                .expect("join");
        assert_eq!(outcome.status, nars_agent_ir::ToolStatus::Succeeded);
        assert!(outcome.stdout.contains("non-axiomatic"));
    }
}
