//! The append-only event protocol shared by the runtime, the API, the event
//! log, and the web UI.
//!
//! Everything that materially changes a run is represented here. Replay feeds
//! the same events into a fresh runtime, so any hidden state must be derivable
//! from this stream.

use serde::{Deserialize, Serialize};

use crate::error::AgentError;
use crate::ids::{RequestId, RunId, SessionId, ToolCallId};
use crate::ir::AgentIr;
use crate::state::TaskSpec;
use crate::tool::{
    NarsDecision, NarsFact, NarsGoal, Observation, ToolCall, ToolPreview, ToolResult,
};

/// Why the runtime called the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelPurpose {
    /// Understand the task and propose candidate actions.
    Plan,
    /// Fill in parameters for a NARS-selected action.
    CompleteArguments,
    /// Explain a tool result to the user.
    Explain,
    /// Produce the final answer.
    Finalize,
}

/// Token accounting reported by a provider when available.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Prompt tokens.
    #[serde(default)]
    pub prompt_tokens: u64,
    /// Completion tokens.
    #[serde(default)]
    pub completion_tokens: u64,
    /// Total tokens.
    #[serde(default)]
    pub total_tokens: u64,
}

impl TokenUsage {
    /// Sums two usage records.
    #[must_use]
    pub fn saturating_add(self, other: Self) -> Self {
        Self {
            prompt_tokens: self.prompt_tokens.saturating_add(other.prompt_tokens),
            completion_tokens: self
                .completion_tokens
                .saturating_add(other.completion_tokens),
            total_tokens: self.total_tokens.saturating_add(other.total_tokens),
        }
    }
}

/// A protocol event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// A session was created for a workspace.
    SessionCreated {
        /// Session identifier.
        session_id: SessionId,
        /// Canonical workspace root.
        workspace: String,
    },
    /// The user sent a message.
    UserMessage {
        /// Session identifier.
        session_id: SessionId,
        /// Run this message belongs to, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        /// Message text.
        text: String,
    },
    /// A run started for a task.
    RunStarted {
        /// Run identifier.
        run_id: RunId,
        /// Session identifier.
        session_id: SessionId,
        /// Task being executed.
        task: TaskSpec,
    },
    /// The runtime asked the model for a structured response.
    ModelRequested {
        /// Run identifier.
        run_id: RunId,
        /// Request identifier.
        request_id: RequestId,
        /// Step number.
        step: u32,
        /// Why the model was called.
        purpose: ModelPurpose,
    },
    /// The model returned a valid Agent IR value.
    ModelProposed {
        /// Run identifier.
        run_id: RunId,
        /// Request identifier.
        request_id: RequestId,
        /// Step number.
        step: u32,
        /// Structured model output.
        ir: AgentIr,
        /// Provider usage.
        #[serde(default)]
        usage: TokenUsage,
    },
    /// The model call failed after retries.
    ModelFailed {
        /// Run identifier.
        run_id: RunId,
        /// Request identifier.
        request_id: RequestId,
        /// Step number.
        step: u32,
        /// Error description.
        error: String,
    },
    /// NARS accepted goals and facts derived from the proposal.
    NarsGoalCommitted {
        /// Run identifier.
        run_id: RunId,
        /// Step number.
        step: u32,
        /// Goal committed.
        goal: NarsGoal,
        /// Facts committed.
        facts: Vec<NarsFact>,
    },
    /// NARS finished one decision round.
    NarsDecisionMade {
        /// Run identifier.
        run_id: RunId,
        /// Step number.
        step: u32,
        /// Full decision record.
        decision: NarsDecision,
    },
    /// A tool call needs explicit user approval.
    ApprovalRequested {
        /// Run identifier.
        run_id: RunId,
        /// Call awaiting approval.
        call: ToolCall,
        /// Why approval is required.
        reason: String,
        /// Optional preview of the effect, such as a patch diff.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preview: Option<ToolPreview>,
    },
    /// The user approved or rejected a call.
    ApprovalResolved {
        /// Run identifier.
        run_id: RunId,
        /// Call identifier.
        call_id: ToolCallId,
        /// Whether the call may proceed.
        approved: bool,
        /// Optional user note.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// Tool execution began.
    ToolStarted {
        /// Run identifier.
        run_id: RunId,
        /// Call being executed.
        call: ToolCall,
    },
    /// Tool execution finished.
    ToolFinished {
        /// Run identifier.
        run_id: RunId,
        /// Result.
        result: ToolResult,
    },
    /// A semantic observation was derived from a tool result.
    ObservationRecorded {
        /// Run identifier.
        run_id: RunId,
        /// Observation.
        observation: Observation,
    },
    /// The agent sent a message to the user.
    AssistantMessage {
        /// Run identifier.
        run_id: RunId,
        /// Markdown text.
        text: String,
    },
    /// The run was paused by the user or the scheduler.
    RunPaused {
        /// Run identifier.
        run_id: RunId,
        /// Reason.
        reason: String,
    },
    /// The run resumed.
    RunResumed {
        /// Run identifier.
        run_id: RunId,
    },
    /// The run completed successfully or with a reported failure.
    RunCompleted {
        /// Run identifier.
        run_id: RunId,
        /// Final summary.
        summary: String,
        /// Whether the task succeeded.
        success: bool,
    },
    /// The run failed with an error.
    RunFailed {
        /// Run identifier.
        run_id: RunId,
        /// Error.
        error: AgentError,
    },
    /// The run was cancelled.
    RunCancelled {
        /// Run identifier.
        run_id: RunId,
        /// Reason.
        reason: String,
    },
}

impl AgentEvent {
    /// A stable discriminant for filtering and UI rendering.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::SessionCreated { .. } => "session_created",
            Self::UserMessage { .. } => "user_message",
            Self::RunStarted { .. } => "run_started",
            Self::ModelRequested { .. } => "model_requested",
            Self::ModelProposed { .. } => "model_proposed",
            Self::ModelFailed { .. } => "model_failed",
            Self::NarsGoalCommitted { .. } => "nars_goal_committed",
            Self::NarsDecisionMade { .. } => "nars_decision_made",
            Self::ApprovalRequested { .. } => "approval_requested",
            Self::ApprovalResolved { .. } => "approval_resolved",
            Self::ToolStarted { .. } => "tool_started",
            Self::ToolFinished { .. } => "tool_finished",
            Self::ObservationRecorded { .. } => "observation_recorded",
            Self::AssistantMessage { .. } => "assistant_message",
            Self::RunPaused { .. } => "run_paused",
            Self::RunResumed { .. } => "run_resumed",
            Self::RunCompleted { .. } => "run_completed",
            Self::RunFailed { .. } => "run_failed",
            Self::RunCancelled { .. } => "run_cancelled",
        }
    }

    /// The run identifier carried by this event, if any.
    #[must_use]
    pub const fn run_id(&self) -> Option<&RunId> {
        match self {
            Self::RunStarted { run_id, .. }
            | Self::ModelRequested { run_id, .. }
            | Self::ModelProposed { run_id, .. }
            | Self::ModelFailed { run_id, .. }
            | Self::NarsGoalCommitted { run_id, .. }
            | Self::NarsDecisionMade { run_id, .. }
            | Self::ApprovalRequested { run_id, .. }
            | Self::ApprovalResolved { run_id, .. }
            | Self::ToolStarted { run_id, .. }
            | Self::ToolFinished { run_id, .. }
            | Self::ObservationRecorded { run_id, .. }
            | Self::AssistantMessage { run_id, .. }
            | Self::RunPaused { run_id, .. }
            | Self::RunResumed { run_id, .. }
            | Self::RunCompleted { run_id, .. }
            | Self::RunFailed { run_id, .. }
            | Self::RunCancelled { run_id, .. } => Some(run_id),
            Self::SessionCreated { .. } | Self::UserMessage { .. } => None,
        }
    }

    /// The session identifier carried by this event, if any.
    #[must_use]
    pub const fn session_id(&self) -> Option<&SessionId> {
        match self {
            Self::SessionCreated { session_id, .. } | Self::UserMessage { session_id, .. } => {
                Some(session_id)
            }
            Self::RunStarted { session_id, .. } => Some(session_id),
            _ => None,
        }
    }

    /// Whether this event is terminal for a run.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::RunCompleted { .. } | Self::RunFailed { .. } | Self::RunCancelled { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::Diff;

    #[test]
    fn events_round_trip_through_json_with_a_type_tag() {
        let event = AgentEvent::RunPaused {
            run_id: RunId::from_static("run_1"),
            reason: "user".to_owned(),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "run_paused");
        let decoded: AgentEvent = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, event);
    }

    #[test]
    fn terminal_detection() {
        let event = AgentEvent::RunCompleted {
            run_id: RunId::from_static("run_1"),
            summary: "done".to_owned(),
            success: true,
        };
        assert!(event.is_terminal());
        let event = AgentEvent::ToolFinished {
            run_id: RunId::from_static("run_1"),
            result: ToolResult {
                call_id: ToolCallId::from_static("call_1"),
                status: crate::tool::ToolStatus::Succeeded,
                exit_code: Some(0),
                stdout: "ok".to_owned(),
                stderr: String::new(),
                truncated: false,
                duration_ms: 1,
                diff: None::<Diff>,
                artifacts: Vec::new(),
                error: None,
                metadata: serde_json::Map::new(),
            },
        };
        assert!(!event.is_terminal());
    }
}
