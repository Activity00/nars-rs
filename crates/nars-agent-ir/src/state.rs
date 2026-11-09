//! Serializable run state shared with the API, the UI, and replay.

use serde::{Deserialize, Serialize};

use crate::error::AgentError;
use crate::event::TokenUsage;
use crate::ids::{RunId, SessionId, ToolCallId};
use crate::ir::GoalSpec;
use crate::tool::{Diff, NarsDecision, Observation, SideEffect, ToolCall, ToolResult, ToolStatus};

#[derive(Serialize)]
struct HashToolCall<'a> {
    id: &'a ToolCallId,
    run_id: &'a RunId,
    tool: &'a str,
    arguments: &'a serde_json::Value,
    side_effect: SideEffect,
    requires_approval: bool,
}

impl<'a> From<&'a ToolCall> for HashToolCall<'a> {
    fn from(call: &'a ToolCall) -> Self {
        Self {
            id: &call.id,
            run_id: &call.run_id,
            tool: &call.tool,
            arguments: &call.arguments,
            side_effect: call.side_effect,
            requires_approval: call.requires_approval,
        }
    }
}

#[derive(Serialize)]
struct HashToolResult<'a> {
    call_id: &'a ToolCallId,
    status: ToolStatus,
    exit_code: Option<i32>,
    stdout: &'a str,
    stderr: &'a str,
    truncated: bool,
    diff: &'a Option<Diff>,
    artifacts: &'a [crate::tool::Artifact],
    error: &'a Option<String>,
    metadata: &'a serde_json::Map<String, serde_json::Value>,
}

impl<'a> From<&'a ToolResult> for HashToolResult<'a> {
    fn from(result: &'a ToolResult) -> Self {
        Self {
            call_id: &result.call_id,
            status: result.status,
            exit_code: result.exit_code,
            stdout: &result.stdout,
            stderr: &result.stderr,
            truncated: result.truncated,
            diff: &result.diff,
            artifacts: &result.artifacts,
            error: &result.error,
            metadata: &result.metadata,
        }
    }
}

#[derive(Serialize)]
struct HashMessage<'a> {
    role: ChatRole,
    text: &'a str,
    run_id: &'a Option<RunId>,
}

impl<'a> From<&'a ChatMessage> for HashMessage<'a> {
    fn from(message: &'a ChatMessage) -> Self {
        Self {
            role: message.role,
            text: &message.text,
            run_id: &message.run_id,
        }
    }
}

/// One project-specific verification command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VerificationCommand {
    /// Short label shown in the plan.
    #[serde(default)]
    pub label: String,
    /// Shell command to run through `run_command`.
    pub command: String,
    /// Optional workspace-relative working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Optional per-command timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Declares what the user asked for and the limits of the run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskSpec {
    /// Structured goal.
    pub goal: GoalSpec,
    /// Explicit constraints.
    #[serde(default)]
    pub constraints: Vec<String>,
    /// Canonical workspace root.
    pub workspace: String,
    /// Original user message.
    pub user_message: String,
    /// Earlier turns of the same session, oldest first.
    ///
    /// The server projects this from the append-only event log when a run
    /// starts, so it is persisted with `RunStarted` and replay stays
    /// deterministic. The current message is not repeated here; it lives in
    /// `user_message`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<ChatMessage>,
    /// Compact summary of long-term memory known before this run started.
    ///
    /// Projected from the workspace memory snapshot when the run starts, so the
    /// model can use earlier knowledge and replay stays deterministic.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_brief: Vec<String>,
    /// Maximum agent steps.
    #[serde(default = "default_max_steps")]
    pub max_steps: u32,
    /// Project-specific verification commands, discovered from the workspace
    /// or `.nars-agent/project.json`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification: Vec<VerificationCommand>,
}

fn default_max_steps() -> u32 {
    24
}

impl TaskSpec {
    /// Builds a task with the default step budget.
    #[must_use]
    pub fn new(
        goal: GoalSpec,
        workspace: impl Into<String>,
        user_message: impl Into<String>,
    ) -> Self {
        Self {
            goal,
            constraints: Vec::new(),
            workspace: workspace.into(),
            user_message: user_message.into(),
            history: Vec::new(),
            memory_brief: Vec::new(),
            max_steps: default_max_steps(),
            verification: Vec::new(),
        }
    }
}

/// Lifecycle status of a run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Created but not started.
    #[default]
    Pending,
    /// The runtime is planning or calling the model.
    Planning,
    /// A tool call is waiting for approval.
    AwaitingApproval,
    /// A tool is executing.
    ExecutingTool,
    /// Paused by the user.
    Paused,
    /// Finished successfully.
    Completed,
    /// Failed with an error.
    Failed,
    /// Cancelled by the user.
    Cancelled,
}

impl RunStatus {
    /// Whether the run is still active.
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(
            self,
            Self::Pending
                | Self::Planning
                | Self::AwaitingApproval
                | Self::ExecutingTool
                | Self::Paused
        )
    }

    /// Whether no more events will arrive.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// A lowercase label for the UI.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Planning => "planning",
            Self::AwaitingApproval => "awaiting_approval",
            Self::ExecutingTool => "executing_tool",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Who produced a chat message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    /// The human user.
    User,
    /// The agent.
    Assistant,
    /// Runtime information.
    System,
}

impl ChatRole {
    /// A lowercase label for prompts and the UI.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
        }
    }
}

/// One message in the session transcript.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Author.
    pub role: ChatRole,
    /// Markdown text.
    pub text: String,
    /// Run that produced the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// Creation timestamp.
    pub created_at_ms: i64,
}

/// Full materialized state of a run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunState {
    /// Run identifier.
    pub run_id: RunId,
    /// Session identifier.
    pub session_id: SessionId,
    /// Lifecycle status.
    pub status: RunStatus,
    /// Current step number.
    pub step: u32,
    /// Task definition.
    pub task: TaskSpec,
    /// Tool call waiting for approval, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_approval: Option<ToolCall>,
    /// Executed or pending tool calls.
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    /// Tool results.
    #[serde(default)]
    pub tool_results: Vec<ToolResult>,
    /// Semantic observations.
    #[serde(default)]
    pub observations: Vec<Observation>,
    /// NARS decisions.
    #[serde(default)]
    pub decisions: Vec<NarsDecision>,
    /// Conversation transcript.
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
    /// Accumulated token usage.
    #[serde(default)]
    pub usage: TokenUsage,
    /// Terminal error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<AgentError>,
    /// Creation timestamp.
    pub created_at_ms: i64,
    /// Last update timestamp.
    pub updated_at_ms: i64,
}

#[derive(Serialize)]
struct HashView<'a> {
    run_id: &'a RunId,
    session_id: &'a SessionId,
    status: RunStatus,
    step: u32,
    task: &'a TaskSpec,
    pending_approval: Option<HashToolCall<'a>>,
    tool_calls: Vec<HashToolCall<'a>>,
    tool_results: Vec<HashToolResult<'a>>,
    observations: &'a [Observation],
    decisions: &'a [NarsDecision],
    messages: Vec<HashMessage<'a>>,
    usage: TokenUsage,
    error: &'a Option<AgentError>,
}

impl RunState {
    /// Creates the initial state of a run.
    #[must_use]
    pub fn new(run_id: RunId, session_id: SessionId, task: TaskSpec, now_ms: i64) -> Self {
        Self {
            run_id,
            session_id,
            status: RunStatus::Pending,
            step: 0,
            task,
            pending_approval: None,
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            observations: Vec::new(),
            decisions: Vec::new(),
            messages: Vec::new(),
            usage: TokenUsage::default(),
            error: None,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        }
    }

    /// Refreshes the update timestamp.
    pub fn touch(&mut self, now_ms: i64) {
        self.updated_at_ms = now_ms;
    }

    /// Looks up a tool call by identifier.
    #[must_use]
    pub fn tool_call(&self, call_id: &ToolCallId) -> Option<&ToolCall> {
        self.tool_calls.iter().find(|call| &call.id == call_id)
    }

    /// Returns the most recent observation.
    #[must_use]
    pub fn latest_observation(&self) -> Option<&Observation> {
        self.observations.last()
    }

    /// Appends a tool result and its call is expected to exist already.
    pub fn push_result(&mut self, result: ToolResult) {
        self.tool_results.push(result);
    }

    /// Appends an observation.
    pub fn push_observation(&mut self, observation: Observation) {
        self.observations.push(observation);
    }

    /// A deterministic digest of everything except timestamps.
    ///
    /// Replay compares this digest after feeding the same event stream into a
    /// fresh state machine.
    #[must_use]
    pub fn state_hash(&self) -> String {
        let view = HashView {
            run_id: &self.run_id,
            session_id: &self.session_id,
            status: self.status,
            step: self.step,
            task: &self.task,
            pending_approval: self.pending_approval.as_ref().map(HashToolCall::from),
            tool_calls: self.tool_calls.iter().map(HashToolCall::from).collect(),
            tool_results: self.tool_results.iter().map(HashToolResult::from).collect(),
            observations: &self.observations,
            decisions: &self.decisions,
            messages: self.messages.iter().map(HashMessage::from).collect(),
            usage: self.usage,
            error: &self.error,
        };
        let bytes = serde_json::to_vec(&view).unwrap_or_default();
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("{hash:016x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::GoalType;

    fn task() -> TaskSpec {
        TaskSpec::new(
            GoalSpec::new(GoalType::Investigate, "look around"),
            "/tmp/ws",
            "look around",
        )
    }

    #[test]
    fn state_hash_ignores_timestamps() {
        let mut left = RunState::new(
            RunId::from_static("run_1"),
            SessionId::from_static("ses_1"),
            task(),
            1,
        );
        let mut right = left.clone();
        left.touch(10);
        right.touch(99);
        assert_eq!(left.state_hash(), right.state_hash());
    }

    #[test]
    fn state_hash_changes_with_material_state() {
        let mut left = RunState::new(
            RunId::from_static("run_1"),
            SessionId::from_static("ses_1"),
            task(),
            1,
        );
        let before = left.state_hash();
        left.status = RunStatus::Planning;
        assert_ne!(before, left.state_hash());
    }
}
