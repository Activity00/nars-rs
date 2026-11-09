//! A deterministic demonstration provider for offline runs and UI demos.
//!
//! It parses the JSON state envelope the runtime puts in the user message and
//! performs a fixed, safe investigation: list files, read one file, summarize.
//! It never fabricates tool output and never bypasses the NARS decision step.

use async_trait::async_trait;
use nars_agent_ir::{
    AgentIr, CandidateAction, FinalAnswer, GoalSpec, GoalType, TaskProposal, TokenUsage,
};
use serde_json::{Value, json};

use crate::provider::{ModelError, ModelProvider, ModelRequest, ModelResponse};

/// Deterministic offline demonstration provider.
pub struct DemoProvider {
    name: String,
}

impl Default for DemoProvider {
    fn default() -> Self {
        Self {
            name: "demo".to_owned(),
        }
    }
}

#[async_trait]
impl ModelProvider for DemoProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        let state = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == crate::provider::Role::User)
            .and_then(|message| serde_json::from_str::<Value>(&message.content).ok());

        let ir = state.as_ref().map_or_else(
            || fallback_ir("demo provider received no state envelope"),
            plan,
        );
        Ok(ModelResponse {
            ir,
            usage: TokenUsage::default(),
            raw: String::new(),
            provider: self.name.clone(),
            latency_ms: 0,
        })
    }
}

fn plan(state: &Value) -> AgentIr {
    let phase = state.get("phase").and_then(Value::as_str).unwrap_or("plan");
    let user_message = state
        .pointer("/task/user_message")
        .and_then(Value::as_str)
        .unwrap_or("");
    let observations = state
        .get("observations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if phase == "finalize" || observations.len() >= 2 {
        return final_answer(&observations);
    }

    match observations.last() {
        None => proposal(
            "list_files",
            json!({"path": ".", "depth": 2, "limit": 80}),
            "Start by listing the workspace to find relevant files.",
        ),
        Some(last) => {
            let tool = last.get("tool").and_then(Value::as_str).unwrap_or("");
            if tool == "list_files" {
                let output = last.get("output").and_then(Value::as_str).unwrap_or("");
                if let Some(path) = pick_file(output) {
                    proposal(
                        "read_file",
                        json!({"path": path, "limit": 120}),
                        "Read the most relevant file before summarizing.",
                    )
                } else {
                    proposal(
                        "search",
                        json!({"query": first_word(user_message), "max_results": 20}),
                        "No obvious file was listed; search for the task keyword.",
                    )
                }
            } else {
                final_answer(&observations)
            }
        }
    }
}

fn proposal(tool: &str, arguments: Value, rationale: &str) -> AgentIr {
    AgentIr::TaskProposal(TaskProposal {
        goal: GoalSpec::new(
            GoalType::Investigate,
            "Inspect the workspace and summarize it",
        ),
        constraints: vec!["Read-only investigation".to_owned()],
        candidate_actions: vec![
            CandidateAction::new(tool)
                .with_arguments(arguments)
                .with_confidence(0.9),
        ],
        confidence: 0.85,
        notes: Some("demo provider".to_owned()),
        summary: Some(rationale.to_owned()),
    })
}

fn final_answer(observations: &[Value]) -> AgentIr {
    let mut lines = vec![
        "**Offline demo mode**".to_owned(),
        String::new(),
        "No real model is configured, so this run used the deterministic demo provider. It only \
         performs a read-only investigation (list files, read one file, summarize); it cannot \
         write code or run tests."
            .to_owned(),
        String::new(),
        "To do real coding work, restart `narsd` with a model provider, for example:".to_owned(),
        String::new(),
        "```bash".to_owned(),
        "NARS_AGENT_PROVIDER=openai \\
NARS_AGENT_API_KEY=sk-... \\
NARS_AGENT_MODEL=gpt-4.1-mini \\
cargo run -p narsd"
            .to_owned(),
        "```".to_owned(),
        String::new(),
        "Or use `NARS_AGENT_PROVIDER=ollama` with a local model.".to_owned(),
        String::new(),
        "Investigation summary:".to_owned(),
    ];
    if observations.is_empty() {
        lines.push("- no observations were recorded".to_owned());
    }
    for observation in observations {
        let tool = observation
            .get("tool")
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let summary = observation
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("completed");
        lines.push(format!("- `{tool}`: {summary}"));
    }
    AgentIr::FinalAnswer(FinalAnswer {
        text: lines.join("\n"),
        success: true,
        artifacts: Vec::new(),
    })
}

fn fallback_ir(message: &str) -> AgentIr {
    AgentIr::FinalAnswer(FinalAnswer {
        text: format!("Demo provider could not parse state: {message}"),
        success: false,
        artifacts: Vec::new(),
    })
}

fn pick_file(listing: &str) -> Option<String> {
    let candidates = listing
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.ends_with('/'))
        .filter(|line| !line.starts_with('.'))
        .filter(|line| !line.contains("node_modules"))
        .filter(|line| !line.starts_with("target"))
        .collect::<Vec<_>>();
    candidates
        .iter()
        .find(|line| line.ends_with(".rs"))
        .or_else(|| candidates.iter().find(|line| line.ends_with(".md")))
        .or_else(|| candidates.iter().find(|line| line.ends_with(".toml")))
        .or_else(|| candidates.first())
        .map(|line| (*line).to_owned())
}

fn first_word(input: &str) -> String {
    input
        .split(|character: char| !character.is_alphanumeric())
        .find(|word| word.len() >= 3)
        .unwrap_or("readme")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ModelMessage;

    fn state(observations: Value) -> ModelRequest {
        ModelRequest::new(
            "demo",
            vec![ModelMessage::user(
                json!({
                    "phase": "plan",
                    "task": {"user_message": "summarize the repository"},
                    "observations": observations
                })
                .to_string(),
            )],
        )
    }

    #[tokio::test]
    async fn proposes_listing_first() {
        let response = DemoProvider::default()
            .complete(state(json!([])))
            .await
            .unwrap();
        let AgentIr::TaskProposal(proposal) = response.ir else {
            panic!("expected proposal");
        };
        assert_eq!(proposal.candidate_actions[0].tool, "list_files");
    }

    #[tokio::test]
    async fn reads_a_file_after_listing() {
        let request = state(json!([{
            "tool": "list_files",
            "outcome": "success",
            "summary": "succeeded",
            "output": "Cargo.toml\ncrates/\ncrates/nars-core/src/lib.rs\n"
        }]));
        let response = DemoProvider::default().complete(request).await.unwrap();
        let AgentIr::TaskProposal(proposal) = response.ir else {
            panic!("expected proposal");
        };
        assert_eq!(proposal.candidate_actions[0].tool, "read_file");
        assert_eq!(
            proposal.candidate_actions[0].arguments["path"],
            "crates/nars-core/src/lib.rs"
        );
    }

    #[tokio::test]
    async fn summarizes_after_two_observations() {
        let request = state(json!([
            {"tool": "list_files", "summary": "listed"},
            {"tool": "read_file", "summary": "read lib.rs"}
        ]));
        let response = DemoProvider::default().complete(request).await.unwrap();
        let AgentIr::FinalAnswer(answer) = response.ir else {
            panic!("expected final answer");
        };
        assert!(answer.text.contains("Offline demo mode"));
        assert!(answer.text.contains("NARS_AGENT_PROVIDER=openai"));
    }
}
