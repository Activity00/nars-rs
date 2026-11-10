//! The structured Agent IR that a language model is allowed to produce.
//!
//! The model never emits tool calls directly. It emits an [`AgentIr`] value;
//! the Rust side validates it, converts it into NARS goals and facts, and only
//! then asks the NARS decision kernel to choose an action.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Maximum number of candidate actions accepted from one model proposal.
pub const MAX_CANDIDATE_ACTIONS: usize = 16;
/// Maximum number of constraints accepted from one model proposal.
pub const MAX_CONSTRAINTS: usize = 32;
/// Maximum length of a user-facing final answer.
pub const MAX_ANSWER_CHARS: usize = 200_000;
/// Maximum length of a free-form constraint or description.
pub const MAX_TEXT_CHARS: usize = 8_000;

fn empty_object() -> Value {
    json!({})
}

fn default_confidence() -> f64 {
    0.5
}

fn default_true() -> bool {
    true
}

/// The top-level discriminated union produced by a model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentIr {
    /// The model understood a task and proposes candidate next actions.
    TaskProposal(TaskProposal),
    /// The model supplies parameters for an action NARS already selected.
    ActionParameters(ActionParameters),
    /// The model believes the run is finished.
    FinalAnswer(FinalAnswer),
    /// The model needs more information from the user.
    Clarification(Clarification),
}

impl AgentIr {
    /// A stable, human-readable discriminant used by the UI and event log.
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::TaskProposal(_) => "task_proposal",
            Self::ActionParameters(_) => "action_parameters",
            Self::FinalAnswer(_) => "final_answer",
            Self::Clarification(_) => "clarification",
        }
    }
}

/// Classification of the user's goal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalType {
    /// Change source code in the workspace.
    #[default]
    ModifyCode,
    /// Answer a question, possibly after reading files.
    AnswerQuestion,
    /// Explore and report on the workspace.
    Investigate,
    /// Run a build or test command and report the result.
    RunTests,
    /// Restructure code without changing behavior.
    Refactor,
    /// Create new files or directories.
    CreateFiles,
    /// Anything that does not fit the categories above.
    Other,
}

impl GoalType {
    /// A compact label suitable for a Narsese atom suffix.
    #[must_use]
    pub const fn atom_suffix(self) -> &'static str {
        match self {
            Self::ModifyCode => "modify_code",
            Self::AnswerQuestion => "answer_question",
            Self::Investigate => "investigate",
            Self::RunTests => "run_tests",
            Self::Refactor => "refactor",
            Self::CreateFiles => "create_files",
            Self::Other => "other",
        }
    }
}

/// A concrete goal, including an optional path target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalSpec {
    /// Goal classification.
    #[serde(rename = "type")]
    pub goal_type: GoalType,
    /// Optional workspace-relative path or symbol the goal is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Natural-language statement of the goal. Optional because models often
    /// omit it when a target and goal type are present; the runtime fills it in.
    #[serde(default)]
    pub description: String,
    /// Optional Narsese translation of the goal, e.g. `(search alpha)`.
    ///
    /// When present, NARS derives the concrete action and arguments from this
    /// term instead of relying on the model's candidate list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narsese: Option<String>,
}

impl GoalSpec {
    /// Creates a goal with no target.
    #[must_use]
    pub fn new(goal_type: GoalType, description: impl Into<String>) -> Self {
        Self {
            goal_type,
            target: None,
            narsese: None,
            description: description.into(),
        }
    }

    /// Attaches a Narsese translation of the goal.
    #[must_use]
    pub fn with_narsese(mut self, narsese: impl Into<String>) -> Self {
        self.narsese = Some(narsese.into());
        self
    }

    /// Attaches a target path.
    #[must_use]
    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }
}

/// One action the model considers plausible.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CandidateAction {
    /// Optional model-provided identifier. The runtime assigns `a1`, `a2`, ...
    /// when missing or duplicate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Tool name from the registry.
    pub tool: String,
    /// Structured tool arguments.
    #[serde(default = "empty_object")]
    pub arguments: Value,
    /// Why the model thinks this action helps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    /// Model confidence in `0.0..=1.0`.
    #[serde(default = "default_confidence")]
    pub confidence: f64,
}

impl CandidateAction {
    /// Creates a candidate with default arguments and confidence.
    #[must_use]
    pub fn new(tool: impl Into<String>) -> Self {
        Self {
            id: None,
            tool: tool.into(),
            arguments: json!({}),
            rationale: None,
            confidence: default_confidence(),
        }
    }

    /// Sets the structured arguments.
    #[must_use]
    pub fn with_arguments(mut self, arguments: Value) -> Self {
        self.arguments = arguments;
        self
    }

    /// Sets the model confidence, clamped to `0.0..=1.0`.
    #[must_use]
    pub fn with_confidence(mut self, confidence: f64) -> Self {
        self.confidence = confidence.clamp(0.0, 1.0);
        self
    }

    /// A stable signature used to aggregate historical tool outcomes.
    #[must_use]
    pub fn signature(&self) -> String {
        format!("{}:{}", self.tool, self.arguments)
    }
}

/// A model proposal for a task.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskProposal {
    /// What the user wants.
    pub goal: GoalSpec,
    /// Hard constraints the agent must respect.
    #[serde(default)]
    pub constraints: Vec<String>,
    /// Candidate next actions. NARS selects at most one per decision round.
    #[serde(default)]
    pub candidate_actions: Vec<CandidateAction>,
    /// Model confidence in its understanding.
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    /// Optional free-form notes for the trace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Optional message to show to the user before execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Parameters for a tool that NARS selected but the model must fill in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionParameters {
    /// Tool name; must match the NARS-selected tool.
    pub tool: String,
    /// Structured arguments for the call.
    #[serde(default = "empty_object")]
    pub arguments: Value,
    /// Optional explanation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

/// A final response to the user.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FinalAnswer {
    /// Markdown response.
    pub text: String,
    /// Whether the task is considered successful.
    #[serde(default = "default_true")]
    pub success: bool,
    /// Optional artifact references (paths or IDs).
    #[serde(default)]
    pub artifacts: Vec<String>,
}

/// A request for clarification.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clarification {
    /// The question to ask.
    pub question: String,
    /// Suggested answers.
    #[serde(default)]
    pub options: Vec<String>,
}

impl AgentIr {
    /// Returns the candidate list when this is a task proposal.
    #[must_use]
    pub fn candidates(&self) -> &[CandidateAction] {
        match self {
            Self::TaskProposal(proposal) => &proposal.candidate_actions,
            _ => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_task_proposal_shape() {
        let source = r#"{
            "kind": "task_proposal",
            "goal": {
                "type": "modify_code",
                "target": "crates/nars-core/src/reasoner.rs"
            },
            "constraints": ["keep the API stable", "run tests"],
            "candidate_actions": [
                {"tool": "search", "arguments": {"query": "decision queue", "path": "crates/nars-core"}}
            ]
        }"#;
        let ir: AgentIr = serde_json::from_str(source).unwrap();
        let AgentIr::TaskProposal(proposal) = &ir else {
            panic!("expected task proposal");
        };
        assert_eq!(proposal.goal.goal_type, GoalType::ModifyCode);
        assert_eq!(proposal.candidate_actions.len(), 1);
        assert_eq!(proposal.candidate_actions[0].tool, "search");
        assert!((proposal.candidate_actions[0].confidence - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn unknown_fields_do_not_break_forward_compatibility() {
        let source = r#"{
            "kind": "final_answer",
            "text": "done",
            "future_field": {"nested": true}
        }"#;
        let ir: AgentIr = serde_json::from_str(source).unwrap();
        assert!(matches!(ir, AgentIr::FinalAnswer(_)));
    }

    #[test]
    fn candidate_confidence_is_clamped() {
        let candidate = CandidateAction::new("read_file").with_confidence(7.0);
        assert!((candidate.confidence - 1.0).abs() < f64::EPSILON);
        let candidate = CandidateAction::new("read_file").with_confidence(-2.0);
        assert!((candidate.confidence - 0.0).abs() < f64::EPSILON);
    }
}
