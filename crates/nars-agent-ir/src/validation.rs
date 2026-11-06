//! Validation of untrusted Agent IR before it can influence the runtime.
//!
//! Validation is intentionally strict about names, shapes, bounds, and paths.
//! It does not execute anything; it only decides whether a value is safe to
//! translate into NARS goals, facts, and eventually a [`crate::tool::ToolCall`].

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ir::{
    AgentIr, MAX_ANSWER_CHARS, MAX_CANDIDATE_ACTIONS, MAX_CONSTRAINTS, MAX_TEXT_CHARS,
};

/// Severity of one validation finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// The IR is rejected.
    Error,
    /// The IR is accepted but the model should know.
    Warning,
}

impl fmt::Display for Severity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Error => formatter.write_str("error"),
            Self::Warning => formatter.write_str("warning"),
        }
    }
}

/// A single validation finding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    /// JSON-path-like location.
    pub field: String,
    /// Human-readable message.
    pub message: String,
    /// Severity.
    pub severity: Severity,
}

impl ValidationIssue {
    fn error(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
            severity: Severity::Error,
        }
    }

    fn warning(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
            severity: Severity::Warning,
        }
    }
}

/// Context required to validate model output.
#[derive(Clone, Debug)]
pub struct ValidationContext {
    /// Tool names the agent is allowed to use.
    pub available_tools: BTreeSet<String>,
    /// Maximum candidate actions.
    pub max_candidates: usize,
    /// Maximum constraints.
    pub max_constraints: usize,
    /// Whether a task proposal must contain at least one candidate.
    pub require_candidates: bool,
}

impl ValidationContext {
    /// Builds a context from a tool catalog.
    #[must_use]
    pub fn new(tools: impl IntoIterator<Item = String>) -> Self {
        Self {
            available_tools: tools.into_iter().collect(),
            max_candidates: MAX_CANDIDATE_ACTIONS,
            max_constraints: MAX_CONSTRAINTS,
            require_candidates: false,
        }
    }

    /// Requires at least one candidate action.
    #[must_use]
    pub fn requiring_candidates(mut self) -> Self {
        self.require_candidates = true;
        self
    }
}

/// Valid IR plus non-fatal findings.
#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedIr {
    /// The accepted value.
    pub ir: AgentIr,
    /// Non-fatal findings.
    pub warnings: Vec<ValidationIssue>,
}

/// Validates one model output value.
///
/// # Errors
///
/// Returns every error-severity finding when the value must be rejected.
pub fn validate_ir(
    ir: &AgentIr,
    context: &ValidationContext,
) -> Result<ValidatedIr, Vec<ValidationIssue>> {
    let mut issues = Vec::new();
    match ir {
        AgentIr::TaskProposal(proposal) => validate_proposal(proposal, context, &mut issues),
        AgentIr::ActionParameters(parameters) => {
            if !context.available_tools.contains(&parameters.tool) {
                issues.push(ValidationIssue::error(
                    "tool",
                    format!("unknown tool `{}`", parameters.tool),
                ));
            }
            if !parameters.arguments.is_object() {
                issues.push(ValidationIssue::error(
                    "arguments",
                    "arguments must be a JSON object",
                ));
            }
            check_argument_paths(&parameters.arguments, "arguments", &mut issues);
        }
        AgentIr::FinalAnswer(answer) => {
            if answer.text.trim().is_empty() {
                issues.push(ValidationIssue::error(
                    "text",
                    "final answer must not be empty",
                ));
            }
            if answer.text.chars().count() > MAX_ANSWER_CHARS {
                issues.push(ValidationIssue::error(
                    "text",
                    format!("final answer exceeds {MAX_ANSWER_CHARS} characters"),
                ));
            }
        }
        AgentIr::Clarification(clarification) => {
            if clarification.question.trim().is_empty() {
                issues.push(ValidationIssue::error(
                    "question",
                    "clarification question must not be empty",
                ));
            }
        }
    }

    let errors = issues
        .iter()
        .filter(|issue| issue.severity == Severity::Error)
        .count();
    if errors > 0 {
        return Err(issues);
    }
    Ok(ValidatedIr {
        ir: ir.clone(),
        warnings: issues,
    })
}

fn validate_proposal(
    proposal: &crate::ir::TaskProposal,
    context: &ValidationContext,
    issues: &mut Vec<ValidationIssue>,
) {
    let description = proposal.goal.description.trim();
    if description.is_empty() {
        issues.push(ValidationIssue::warning(
            "goal.description",
            "goal description is empty; the runtime will derive one",
        ));
    } else if description.chars().count() > MAX_TEXT_CHARS {
        issues.push(ValidationIssue::error(
            "goal.description",
            format!("goal description exceeds {MAX_TEXT_CHARS} characters"),
        ));
    }

    if let Some(target) = &proposal.goal.target
        && !is_safe_relative_path(target)
    {
        issues.push(ValidationIssue::error(
            "goal.target",
            format!("unsafe target path `{target}`"),
        ));
    }

    if proposal.confidence.is_nan() || !(0.0..=1.0).contains(&proposal.confidence) {
        issues.push(ValidationIssue::error(
            "confidence",
            "confidence must be a finite number in 0.0..=1.0",
        ));
    }

    if proposal.constraints.len() > context.max_constraints {
        issues.push(ValidationIssue::error(
            "constraints",
            format!(
                "at most {} constraints are accepted",
                context.max_constraints
            ),
        ));
    }
    for (index, constraint) in proposal.constraints.iter().enumerate() {
        if constraint.trim().is_empty() {
            issues.push(ValidationIssue::warning(
                format!("constraints[{index}]"),
                "empty constraint is ignored",
            ));
        }
        if constraint.chars().count() > MAX_TEXT_CHARS {
            issues.push(ValidationIssue::error(
                format!("constraints[{index}]"),
                format!("constraint exceeds {MAX_TEXT_CHARS} characters"),
            ));
        }
    }

    if proposal.candidate_actions.is_empty() && context.require_candidates {
        issues.push(ValidationIssue::error(
            "candidate_actions",
            "at least one candidate action is required",
        ));
    }
    if proposal.candidate_actions.len() > context.max_candidates {
        issues.push(ValidationIssue::error(
            "candidate_actions",
            format!(
                "at most {} candidate actions are accepted",
                context.max_candidates
            ),
        ));
    }

    let mut seen_ids = BTreeSet::new();
    let mut seen_signatures = BTreeSet::new();
    for (index, candidate) in proposal.candidate_actions.iter().enumerate() {
        let field = format!("candidate_actions[{index}]");
        if !context.available_tools.contains(&candidate.tool) {
            issues.push(ValidationIssue::error(
                format!("{field}.tool"),
                format!("unknown tool `{}`", candidate.tool),
            ));
        }
        if !candidate.arguments.is_object() {
            issues.push(ValidationIssue::error(
                format!("{field}.arguments"),
                "arguments must be a JSON object",
            ));
        }
        if candidate.confidence.is_nan() || !(0.0..=1.0).contains(&candidate.confidence) {
            issues.push(ValidationIssue::error(
                format!("{field}.confidence"),
                "confidence must be a finite number in 0.0..=1.0",
            ));
        }
        if let Some(id) = &candidate.id
            && !seen_ids.insert(id.clone())
        {
            issues.push(ValidationIssue::error(
                format!("{field}.id"),
                format!("duplicate candidate id `{id}`"),
            ));
        }
        let signature = candidate.signature();
        if !seen_signatures.insert(signature) {
            issues.push(ValidationIssue::warning(
                &field,
                "duplicate candidate action",
            ));
        }
        check_argument_paths(&candidate.arguments, &format!("{field}.arguments"), issues);
    }
}

/// Whether a path is relative, non-empty, free of NUL, and free of `..`.
#[must_use]
pub fn is_safe_relative_path(raw: &str) -> bool {
    if raw.is_empty() || raw.contains('\0') {
        return false;
    }
    let path = Path::new(raw);
    if path.is_absolute() {
        return false;
    }
    !path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

const PATH_KEYS: &[&str] = &[
    "path",
    "file",
    "file_path",
    "filename",
    "target",
    "cwd",
    "directory",
    "dir",
    "glob",
    "root",
];

fn check_argument_paths(value: &Value, field: &str, issues: &mut Vec<ValidationIssue>) {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                let nested_field = format!("{field}.{key}");
                if PATH_KEYS.contains(&key.as_str())
                    && let Value::String(path) = nested
                    && !is_safe_relative_path(path)
                {
                    issues.push(ValidationIssue::error(
                        nested_field.clone(),
                        format!("unsafe path `{path}`"),
                    ));
                }
                check_argument_paths(nested, &nested_field, issues);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                check_argument_paths(item, &format!("{field}[{index}]"), issues);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{CandidateAction, GoalSpec, GoalType, TaskProposal};
    use serde_json::json;

    fn context() -> ValidationContext {
        ValidationContext::new(["read_file".to_owned(), "search".to_owned()])
    }

    fn proposal(candidates: Vec<CandidateAction>) -> AgentIr {
        AgentIr::TaskProposal(TaskProposal {
            goal: GoalSpec::new(GoalType::Investigate, "look around"),
            constraints: Vec::new(),
            candidate_actions: candidates,
            confidence: 0.8,
            notes: None,
            summary: None,
        })
    }

    #[test]
    fn accepts_a_valid_proposal() {
        let ir = proposal(vec![
            CandidateAction::new("read_file").with_arguments(json!({"path": "src/lib.rs"})),
        ]);
        let validated = validate_ir(&ir, &context()).unwrap();
        assert!(validated.warnings.is_empty());
    }

    #[test]
    fn rejects_unknown_tools() {
        let ir = proposal(vec![CandidateAction::new("rm_rf")]);
        let issues = validate_ir(&ir, &context()).unwrap_err();
        assert!(issues.iter().any(|issue| issue.field.contains("tool")));
    }

    #[test]
    fn rejects_path_escape() {
        let ir = proposal(vec![
            CandidateAction::new("read_file").with_arguments(json!({"path": "../../etc/passwd"})),
        ]);
        let issues = validate_ir(&ir, &context()).unwrap_err();
        assert!(issues.iter().any(|issue| issue.field.contains("path")));
    }

    #[test]
    fn rejects_absolute_paths() {
        assert!(!is_safe_relative_path("/etc/passwd"));
        assert!(!is_safe_relative_path("../x"));
        assert!(is_safe_relative_path("crates/nars-core"));
        assert!(is_safe_relative_path("."));
    }

    #[test]
    fn enforces_candidate_limit() {
        let candidates = (0..40)
            .map(|_| CandidateAction::new("read_file"))
            .collect::<Vec<_>>();
        let ir = proposal(candidates);
        let issues = validate_ir(&ir, &context()).unwrap_err();
        assert!(
            issues
                .iter()
                .any(|issue| issue.field == "candidate_actions")
        );
    }

    #[test]
    fn rejects_bad_confidence() {
        let mut candidate = CandidateAction::new("read_file");
        candidate.confidence = 1.5;
        let issues = validate_ir(&proposal(vec![candidate]), &context()).unwrap_err();
        assert!(
            issues
                .iter()
                .any(|issue| issue.field.contains("confidence"))
        );
    }

    #[test]
    fn rejects_empty_final_answer() {
        let ir = AgentIr::FinalAnswer(crate::ir::FinalAnswer {
            text: "  ".to_owned(),
            success: true,
            artifacts: Vec::new(),
        });
        assert!(validate_ir(&ir, &context()).is_err());
    }
}
