//! Tool protocol: manifests, calls, results, observations, and NARS facts.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ids::{ObservationId, RunId, ToolCallId};

/// How dangerous a tool is when executed.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SideEffect {
    /// Reads workspace state only.
    #[default]
    ReadOnly,
    /// Writes files inside the workspace.
    WorkspaceWrite,
    /// Starts a child process.
    ProcessExecution,
    /// Reaches the network.
    NetworkAccess,
    /// Rewrites git history.
    GitHistoryChange,
    /// Affects state outside the workspace.
    ExternalSideEffect,
}

impl SideEffect {
    /// A coarse risk score; higher values demand stricter policy.
    #[must_use]
    pub const fn risk(self) -> u8 {
        match self {
            Self::ReadOnly => 0,
            Self::WorkspaceWrite => 1,
            Self::ProcessExecution => 2,
            Self::NetworkAccess => 3,
            Self::GitHistoryChange => 4,
            Self::ExternalSideEffect => 5,
        }
    }

    /// Whether the effect is observable only.
    #[must_use]
    pub const fn is_read_only(self) -> bool {
        matches!(self, Self::ReadOnly)
    }

    /// A compact label for logs and the UI.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::WorkspaceWrite => "workspace_write",
            Self::ProcessExecution => "process_execution",
            Self::NetworkAccess => "network_access",
            Self::GitHistoryChange => "git_history_change",
            Self::ExternalSideEffect => "external_side_effect",
        }
    }
}

/// The declarative contract of one tool.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolManifest {
    /// Unique tool name, for example `read_file`.
    pub name: String,
    /// One-paragraph description shown to the model and the UI.
    pub description: String,
    /// JSON Schema for the `arguments` object.
    pub input_schema: Value,
    /// Declared side effect.
    pub side_effect: SideEffect,
    /// Hard execution timeout in milliseconds.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Whether the policy must ask the user before execution.
    #[serde(default)]
    pub requires_approval: bool,
    /// Tool version, used for compatibility checks with MCP adapters.
    #[serde(default = "default_version")]
    pub version: String,
    /// Free-form tags such as `builtin`, `git`, `mcp`.
    #[serde(default)]
    pub tags: Vec<String>,
}

fn default_timeout_ms() -> u64 {
    120_000
}

fn default_version() -> String {
    "0.1.0".to_owned()
}

impl ToolManifest {
    /// Builds a manifest with conservative defaults.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
        side_effect: SideEffect,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            side_effect,
            timeout_ms: default_timeout_ms(),
            requires_approval: false,
            version: default_version(),
            tags: vec!["builtin".to_owned()],
        }
    }

    /// Marks this tool as requiring explicit approval.
    #[must_use]
    pub fn with_approval(mut self) -> Self {
        self.requires_approval = true;
        self
    }

    /// Overrides the timeout.
    #[must_use]
    pub fn with_timeout(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }
}

/// A fully specified tool invocation produced by the runtime, never by the model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique call identifier.
    pub id: ToolCallId,
    /// Run that owns the call.
    pub run_id: RunId,
    /// Tool name.
    pub tool: String,
    /// Validated arguments.
    pub arguments: Value,
    /// Side effect copied from the manifest at call creation time.
    pub side_effect: SideEffect,
    /// Whether approval is required.
    pub requires_approval: bool,
    /// Creation timestamp in Unix milliseconds.
    pub created_at_ms: i64,
}

/// Terminal status of one tool call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    /// The tool completed successfully.
    Succeeded,
    /// The tool ran but reported a failure.
    Failed,
    /// Policy or the user denied the call.
    Denied,
    /// The tool exceeded its timeout.
    TimedOut,
    /// The run was cancelled.
    Cancelled,
}

impl ToolStatus {
    /// Whether the call is considered successful.
    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Succeeded)
    }
}

/// A unified diff produced by a write tool.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Diff {
    /// Workspace-relative path.
    pub path: String,
    /// Whether the file existed before the change.
    pub before_existed: bool,
    /// Unified diff text, suitable for rendering.
    pub unified: String,
    /// Number of added lines.
    pub added: u32,
    /// Number of removed lines.
    pub removed: u32,
}

/// A file or log produced by a tool.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    /// Stable artifact identifier.
    pub id: String,
    /// Artifact kind such as `log`, `file`, `report`.
    pub kind: String,
    /// Workspace-relative path or external URI.
    pub location: String,
    /// Human-readable description.
    pub description: String,
    /// Size in bytes when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

/// The result of executing one tool call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    /// Call this result belongs to.
    pub call_id: ToolCallId,
    /// Terminal status.
    pub status: ToolStatus,
    /// Process exit code when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Standard output or primary textual payload.
    #[serde(default)]
    pub stdout: String,
    /// Standard error.
    #[serde(default)]
    pub stderr: String,
    /// Whether output was truncated to the configured cap.
    #[serde(default)]
    pub truncated: bool,
    /// Wall-clock duration.
    #[serde(default)]
    pub duration_ms: u64,
    /// Diff for write operations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<Diff>,
    /// Artifacts produced by the call.
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    /// Machine-readable error message when the tool failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Optional structured metadata for specialized tools.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,
}

impl ToolResult {
    /// Builds a successful result with textual output.
    #[must_use]
    pub fn success(call_id: ToolCallId, stdout: impl Into<String>) -> Self {
        Self {
            call_id,
            status: ToolStatus::Succeeded,
            exit_code: Some(0),
            stdout: stdout.into(),
            stderr: String::new(),
            truncated: false,
            duration_ms: 0,
            diff: None,
            artifacts: Vec::new(),
            error: None,
            metadata: Map::new(),
        }
    }

    /// Builds a failed result with an explanatory message.
    #[must_use]
    pub fn failure(call_id: ToolCallId, error: impl Into<String>) -> Self {
        let error = error.into();
        Self {
            call_id,
            status: ToolStatus::Failed,
            exit_code: None,
            stdout: String::new(),
            stderr: error.clone(),
            truncated: false,
            duration_ms: 0,
            diff: None,
            artifacts: Vec::new(),
            error: Some(error),
            metadata: Map::new(),
        }
    }

    /// Builds a denied result.
    #[must_use]
    pub fn denied(call_id: ToolCallId, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            call_id,
            status: ToolStatus::Denied,
            exit_code: None,
            stdout: String::new(),
            stderr: reason.clone(),
            truncated: false,
            duration_ms: 0,
            diff: None,
            artifacts: Vec::new(),
            error: Some(reason),
            metadata: Map::new(),
        }
    }

    /// Whether the call succeeded.
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }

    /// One-line summary for the model context and UI timeline.
    #[must_use]
    pub fn summary(&self) -> String {
        let head = match self.status {
            ToolStatus::Succeeded => "succeeded",
            ToolStatus::Failed => "failed",
            ToolStatus::Denied => "denied",
            ToolStatus::TimedOut => "timed_out",
            ToolStatus::Cancelled => "cancelled",
        };
        match &self.error {
            Some(error) => format!("{head}: {error}"),
            None => format!("{head} ({} bytes)", self.stdout.len()),
        }
    }
}

/// The semantic outcome recorded in NARS.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The action achieved its purpose.
    Success,
    /// The action ran but did not achieve its purpose.
    Failure,
    /// The action was refused by policy or the user.
    Denied,
    /// The action timed out.
    Timeout,
    /// The action was cancelled.
    Cancelled,
}

impl Outcome {
    /// Derives the outcome from a tool status.
    #[must_use]
    pub const fn from_status(status: ToolStatus) -> Self {
        match status {
            ToolStatus::Succeeded => Self::Success,
            ToolStatus::Failed => Self::Failure,
            ToolStatus::Denied => Self::Denied,
            ToolStatus::TimedOut => Self::Timeout,
            ToolStatus::Cancelled => Self::Cancelled,
        }
    }

    /// A Narsese-friendly label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Denied => "denied",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
        }
    }
}

/// A truth value stored in NARS.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TruthValue {
    /// Frequency in `0.0..=1.0`.
    pub frequency: f64,
    /// Confidence in `0.0..=1.0`.
    pub confidence: f64,
}

impl Default for TruthValue {
    fn default() -> Self {
        Self {
            frequency: 1.0,
            confidence: 0.9,
        }
    }
}

impl TruthValue {
    /// Creates a clamped truth value.
    #[must_use]
    pub fn new(frequency: f64, confidence: f64) -> Self {
        Self {
            frequency: frequency.clamp(0.0, 1.0),
            confidence: confidence.clamp(0.0, 1.0),
        }
    }
}

/// A symbolic fact committed to NARS.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NarsFact {
    /// Narsese term, always a safe atom or compound generated by the runtime.
    pub term: String,
    /// Truth value.
    pub truth: TruthValue,
    /// Whether the fact is eternal.
    pub eternal: bool,
}

/// A goal committed to NARS.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NarsGoal {
    /// Stable goal identifier.
    pub id: String,
    /// Human-readable description.
    pub description: String,
    /// Narsese term that represents the desired state.
    pub term: String,
    /// Priority in `0.0..=1.0`.
    pub priority: f64,
}

/// A semantically meaningful observation derived from a tool result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    /// Observation identifier.
    pub id: ObservationId,
    /// Run that produced it.
    pub run_id: RunId,
    /// Tool call that produced it.
    pub call_id: ToolCallId,
    /// Tool name.
    pub tool: String,
    /// One-line summary.
    pub summary: String,
    /// Outcome.
    pub outcome: Outcome,
    /// Facts derived from the result.
    pub facts: Vec<NarsFact>,
    /// Referenced artifacts.
    pub artifacts: Vec<Artifact>,
}

/// A preview shown to the user before an approval decision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolPreview {
    /// One-line summary.
    pub summary: String,
    /// Unified diff text for write tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unified_diff: Option<String>,
    /// Files touched by the call.
    #[serde(default)]
    pub files: Vec<String>,
}

/// A candidate that NARS scored during one decision round.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScoredCandidate {
    /// Candidate identifier (`a1`, `a2`, ...).
    pub candidate_id: String,
    /// Tool name.
    pub tool: String,
    /// Final score used to seed the NARS implication.
    pub score: f64,
    /// Explanation of how the score was derived.
    pub reason: String,
}

/// The action NARS selected.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SelectedAction {
    /// Candidate identifier.
    pub candidate_id: String,
    /// Tool name.
    pub tool: String,
    /// Arguments copied from the candidate.
    pub arguments: Value,
    /// Score associated with the selection.
    pub score: f64,
}

/// How the decision was produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSource {
    /// NARS inference selected the action.
    Nars,
    /// Deterministic fallback when NARS produced no candidate.
    Fallback,
    /// No action was available.
    None,
}

/// One stage of the runtime's task plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStageView {
    /// Stable stage identifier such as `investigate`.
    pub id: String,
    /// Human-readable label.
    pub label: String,
    /// `pending`, `active`, `satisfied`, or `blocked`.
    pub status: String,
    /// Tools that can satisfy this stage.
    pub tools: Vec<String>,
    /// Whether the stage exceeded its soft step budget.
    pub overdue: bool,
    /// Attempts recorded for this stage.
    pub attempts: u32,
    /// Failed attempts recorded for this stage.
    pub failures: u32,
}

/// Serializable view of the task plan for events and the UI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPlanView {
    /// Active stage identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    /// Ordered stages.
    pub stages: Vec<PlanStageView>,
    /// Recent plan notes.
    #[serde(default)]
    pub notes: Vec<String>,
    /// Files discovered by read-only tools.
    #[serde(default)]
    pub working_memory: Vec<String>,
    /// Whether the plan regards the task as ready to finalize.
    pub can_finish: bool,
}

/// The auditable result of one NARS decision round.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NarsDecision {
    /// Step number within the run.
    pub step: u32,
    /// Selected action, if any.
    pub selected: Option<SelectedAction>,
    /// All scored candidates, ordered by score descending.
    pub scored: Vec<ScoredCandidate>,
    /// Human-readable rationale.
    pub rationale: String,
    /// The synthetic NARS goal atom for this round.
    pub goal_term: String,
    /// Which mechanism produced the selection.
    pub source: DecisionSource,
    /// Task plan state at decision time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<TaskPlanView>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn side_effect_risk_is_ordered() {
        assert!(SideEffect::ReadOnly.risk() < SideEffect::WorkspaceWrite.risk());
        assert!(SideEffect::WorkspaceWrite.risk() < SideEffect::ProcessExecution.risk());
        assert!(SideEffect::ProcessExecution.risk() < SideEffect::GitHistoryChange.risk());
        assert!(SideEffect::GitHistoryChange.risk() < SideEffect::ExternalSideEffect.risk());
    }

    #[test]
    fn denied_results_are_not_successful() {
        let result = ToolResult::denied(ToolCallId::from_static("call_1"), "no");
        assert!(!result.is_success());
        assert_eq!(result.status, ToolStatus::Denied);
    }

    #[test]
    fn tool_manifest_defaults_are_conservative() {
        let manifest = ToolManifest::new(
            "read_file",
            "reads",
            json!({"type": "object"}),
            SideEffect::ReadOnly,
        );
        assert!(!manifest.requires_approval);
        assert_eq!(manifest.timeout_ms, 120_000);
    }
}
