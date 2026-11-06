//! Task plan: the runtime-owned stage machine that keeps the model from
//! dictating the long-term flow.
//!
//! The plan decides which *kinds* of actions are admissible at a given moment.
//! The model still proposes concrete candidates and arguments, and NARS still
//! chooses among them. A model can ask to patch before investigating, or to
//! finish before verification; the plan rejects or defers that proposal and
//! supplies safe fallback candidates where it can (listing, reading, cargo).

use std::collections::BTreeSet;

use nars_agent_ir::{
    CandidateAction, GoalType, Observation, Outcome, PlanStageView, TaskPlanView, TaskSpec,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The role of a plan stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageKind {
    /// Read-only exploration.
    Investigate,
    /// Workspace modification.
    Change,
    /// Build or test verification.
    Verify,
    /// Final response.
    Answer,
}

impl StageKind {
    /// Stable identifier used in facts and the UI.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Investigate => "investigate",
            Self::Change => "change",
            Self::Verify => "verify",
            Self::Answer => "answer",
        }
    }
}

/// Lifecycle of one stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    /// Not reached yet.
    Pending,
    /// The stage currently owns the run.
    Active,
    /// Its success condition was met.
    Satisfied,
    /// No admissible action can make progress.
    Blocked,
}

impl StageStatus {
    /// Label used in events and the UI.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Satisfied => "satisfied",
            Self::Blocked => "blocked",
        }
    }
}

/// One plan stage.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanStage {
    /// Stable identifier.
    pub id: String,
    /// Stage role.
    pub kind: StageKind,
    /// Human-readable label.
    pub label: String,
    /// Current status.
    pub status: StageStatus,
    /// Tools that can satisfy this stage.
    pub tools: Vec<String>,
    /// Attempts on preferred tools.
    pub attempts: u32,
    /// Failed attempts on preferred tools.
    pub failures: u32,
    /// Step at which the stage became active.
    pub entered_step: u32,
    /// Soft step budget.
    pub deadline_steps: u32,
    /// Preferred tools that already failed in this stage.
    pub failed_tools: BTreeSet<String>,
    /// Read-only tools that already made progress but cannot satisfy the stage.
    #[serde(default)]
    pub progressed_tools: BTreeSet<String>,
}

impl PlanStage {
    /// Whether the stage exceeded its soft budget.
    #[must_use]
    pub fn is_overdue(&self, step: u32) -> bool {
        self.status == StageStatus::Active
            && step > self.entered_step.saturating_add(self.deadline_steps)
    }
}

/// Runtime-owned task plan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskPlan {
    /// Schema version.
    pub version: u32,
    /// Goal classification the plan was built from.
    pub goal_type: GoalType,
    /// Ordered stages. Empty for `other`, which leaves sequencing to the model.
    pub stages: Vec<PlanStage>,
    /// Active stage index.
    pub active: Option<usize>,
    /// Current decision step.
    pub step: u32,
    /// Files discovered by read-only tools.
    pub discovered_files: Vec<String>,
    /// Recent plan notes.
    pub notes: Vec<String>,
}

/// Result of feeding one observation into the plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanTransition {
    /// Whether the active stage advanced.
    pub advanced: bool,
    /// Stage identifier after the transition.
    pub stage: Option<String>,
    /// Status label after the transition.
    pub status: Option<&'static str>,
    /// Human-readable note for the trace.
    pub note: String,
}

impl TaskPlan {
    /// Builds the default stage template for a task.
    #[must_use]
    pub fn for_task(task: &TaskSpec, step: u32) -> Self {
        let max_steps = task.max_steps.max(1);
        let stages = match task.goal.goal_type {
            GoalType::ModifyCode | GoalType::Refactor | GoalType::CreateFiles => vec![
                stage(
                    StageKind::Investigate,
                    "Inspect the workspace",
                    &["read_file", "list_files", "search"],
                    (max_steps / 3).max(3),
                    step,
                ),
                stage(
                    StageKind::Change,
                    "Modify the workspace",
                    &["apply_patch"],
                    4,
                    step,
                ),
                stage(
                    StageKind::Verify,
                    "Verify the change",
                    &["cargo_check", "cargo_test"],
                    3,
                    step,
                ),
                stage(StageKind::Answer, "Report the result", &[], 2, step),
            ],
            GoalType::RunTests => vec![
                stage(
                    StageKind::Verify,
                    "Run the requested checks",
                    &["cargo_check", "cargo_test"],
                    (max_steps / 2).max(2),
                    step,
                ),
                stage(StageKind::Answer, "Report the result", &[], 2, step),
            ],
            GoalType::Investigate | GoalType::AnswerQuestion => vec![
                stage(
                    StageKind::Investigate,
                    "Investigate the workspace",
                    &["read_file", "list_files", "search"],
                    (max_steps / 2).max(2),
                    step,
                ),
                stage(StageKind::Answer, "Answer the question", &[], 2, step),
            ],
            GoalType::Other => Vec::new(),
        };
        let mut plan = Self {
            version: 1,
            goal_type: task.goal.goal_type,
            active: None,
            stages,
            step,
            discovered_files: Vec::new(),
            notes: Vec::new(),
        };
        if !plan.stages.is_empty() {
            plan.active = Some(0);
            plan.stages[0].status = StageStatus::Active;
            if let Some(active) = plan.active {
                plan.stages[active].entered_step = step;
            }
        }
        plan
    }

    /// Rebuilds a plan by replaying observations from a resumed run.
    #[must_use]
    pub fn recover(task: &TaskSpec, observations: &[Observation], step: u32) -> Self {
        let mut plan = Self::for_task(task, 0);
        for observation in observations {
            plan.record_observation(
                &observation.tool,
                observation.outcome == Outcome::Success,
                None,
                step,
            );
        }
        plan.set_step(step);
        plan
    }

    /// Advances the plan's internal step counter.
    pub fn set_step(&mut self, step: u32) {
        self.step = step;
    }

    /// The active stage, if any.
    #[must_use]
    pub fn active_stage(&self) -> Option<&PlanStage> {
        self.active.and_then(|index| self.stages.get(index))
    }

    /// Whether this goal uses a plan.
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.stages.is_empty()
    }

    /// Whether an action may be executed in the current stage.
    #[must_use]
    pub fn allows(&self, tool: &str) -> bool {
        let Some(stage) = self.active_stage() else {
            return true;
        };
        if stage.status == StageStatus::Blocked || stage.kind == StageKind::Answer {
            return false;
        }
        if stage.tools.iter().any(|candidate| candidate == tool) {
            return true;
        }
        matches!(
            tool,
            "read_file" | "list_files" | "search" | "git_diff" | "git_status"
        )
    }

    /// Whether a tool is the stage's preferred tool.
    #[must_use]
    pub fn preferred(&self, tool: &str) -> bool {
        self.active_stage()
            .is_some_and(|stage| stage.tools.iter().any(|candidate| candidate == tool))
    }

    /// Splits candidates into admissible and deferred sets.
    #[must_use]
    pub fn filter(
        &self,
        candidates: &[CandidateAction],
    ) -> (Vec<CandidateAction>, Vec<CandidateAction>) {
        let mut allowed = Vec::new();
        let mut deferred = Vec::new();
        let progressed = self.progressed_tools();
        for candidate in candidates {
            if progressed.contains(&candidate.tool) {
                deferred.push(candidate.clone());
            } else if self.allows(&candidate.tool) {
                allowed.push(candidate.clone());
            } else {
                deferred.push(candidate.clone());
            }
        }
        (allowed, deferred)
    }

    /// Safe fallback candidates for the active stage.
    ///
    /// These never include writes: a patch needs model-authored content.
    #[must_use]
    pub fn fallback_candidates(&self, task: &TaskSpec) -> Vec<CandidateAction> {
        let Some(stage) = self.active_stage() else {
            return Vec::new();
        };
        let mut candidates = Vec::new();
        match stage.kind {
            StageKind::Investigate => {
                if let Some(path) = self.discovered_files.last() {
                    candidates.push(
                        CandidateAction::new("read_file")
                            .with_arguments(json!({"path": path, "limit": 160}))
                            .with_confidence(0.82),
                    );
                }
                candidates.push(
                    CandidateAction::new("list_files")
                        .with_arguments(json!({"path": ".", "depth": 2, "limit": 80}))
                        .with_confidence(0.80),
                );
                if let Some(query) = first_word(&task.goal.description)
                    && query.len() >= 3
                {
                    candidates.push(
                        CandidateAction::new("search")
                            .with_arguments(json!({"query": query, "max_results": 30}))
                            .with_confidence(0.74),
                    );
                }
            }
            StageKind::Verify => {
                candidates.push(
                    CandidateAction::new("cargo_check")
                        .with_arguments(json!({}))
                        .with_confidence(0.84),
                );
                candidates.push(
                    CandidateAction::new("cargo_test")
                        .with_arguments(json!({}))
                        .with_confidence(0.80),
                );
            }
            StageKind::Change | StageKind::Answer => {}
        }
        let progressed = self.progressed_tools();
        candidates.retain(|candidate| {
            self.allows(&candidate.tool) && !progressed.contains(&candidate.tool)
        });
        candidates
    }

    /// Tools that already made progress in the active stage and should not be
    /// repeated before the stage can advance.
    #[must_use]
    pub fn progressed_tools(&self) -> BTreeSet<String> {
        self.active_stage()
            .map(|stage| stage.progressed_tools.clone())
            .unwrap_or_default()
    }

    /// Adds discovered files to the working memory.
    pub fn note_files(&mut self, files: impl IntoIterator<Item = String>) {
        for file in files {
            if file.is_empty()
                || self
                    .discovered_files
                    .iter()
                    .any(|existing| existing == &file)
            {
                continue;
            }
            self.discovered_files.push(file);
            while self.discovered_files.len() > 16 {
                self.discovered_files.remove(0);
            }
        }
    }

    /// Records one observation and advances or regresses the plan.
    pub fn record_observation(
        &mut self,
        tool: &str,
        success: bool,
        path: Option<&str>,
        step: u32,
    ) -> PlanTransition {
        self.set_step(step);
        if success
            && tool == "read_file"
            && let Some(path) = path.filter(|path| !path.is_empty())
        {
            if !self
                .discovered_files
                .iter()
                .any(|existing| existing == path)
            {
                self.discovered_files.push(path.to_owned());
                while self.discovered_files.len() > 16 {
                    self.discovered_files.remove(0);
                }
            }
        }

        let Some(index) = self.active else {
            return PlanTransition {
                advanced: false,
                stage: None,
                status: None,
                note: "plan has no active stage".to_owned(),
            };
        };
        let kind = self.stages[index].kind;
        let preferred = self.stages[index]
            .tools
            .iter()
            .any(|candidate| candidate == tool);

        if !preferred {
            let note = if success {
                format!("`{tool}` succeeded outside the active stage")
            } else {
                format!("`{tool}` failed outside the active stage")
            };
            self.note(note.clone());
            return PlanTransition {
                advanced: false,
                stage: Some(self.stages[index].id.clone()),
                status: Some(self.stages[index].status.label()),
                note,
            };
        }

        if success && kind == StageKind::Investigate && tool != "read_file" {
            self.stages[index].attempts = self.stages[index].attempts.saturating_add(1);
            self.stages[index].progressed_tools.insert(tool.to_owned());
            let note = format!(
                "`{tool}` progressed the investigation; a read_file is required before leaving this stage"
            );
            self.note(note.clone());
            return PlanTransition {
                advanced: false,
                stage: Some(self.stages[index].id.clone()),
                status: Some(self.stages[index].status.label()),
                note,
            };
        }

        self.stages[index].attempts = self.stages[index].attempts.saturating_add(1);
        if success {
            self.stages[index].status = StageStatus::Satisfied;
            let satisfied = self.stages[index].id.clone();
            self.note(format!("stage `{satisfied}` satisfied by `{tool}`"));
            let next = self.advance(index);
            return PlanTransition {
                advanced: true,
                stage: next.clone(),
                status: self
                    .active_stage()
                    .map(|stage| stage.status.label())
                    .or(Some("done")),
                note: format!(
                    "stage `{satisfied}` satisfied; next stage: {}",
                    next.as_deref().unwrap_or("done")
                ),
            };
        }

        self.stages[index].failures = self.stages[index].failures.saturating_add(1);
        self.stages[index].failed_tools.insert(tool.to_owned());

        if kind == StageKind::Verify {
            if let Some(change_index) = self
                .stages
                .iter()
                .position(|stage| stage.kind == StageKind::Change)
            {
                self.stages[index].status = StageStatus::Pending;
                self.stages[change_index].status = StageStatus::Active;
                self.stages[change_index].entered_step = step;
                self.active = Some(change_index);
                let note = format!("`{tool}` failed verification; returning to the change stage");
                self.note(note.clone());
                return PlanTransition {
                    advanced: false,
                    stage: Some(self.stages[change_index].id.clone()),
                    status: Some(StageStatus::Active.label()),
                    note,
                };
            }
        }

        let note = format!(
            "`{tool}` failed in stage `{}` ({} failure(s))",
            self.stages[index].id, self.stages[index].failures
        );
        self.note(note.clone());
        PlanTransition {
            advanced: false,
            stage: Some(self.stages[index].id.clone()),
            status: Some(self.stages[index].status.label()),
            note,
        }
    }

    /// Guidance text for the model when no admissible candidate exists.
    #[must_use]
    pub fn stage_guidance(&self, task: &TaskSpec) -> String {
        let Some(stage) = self.active_stage() else {
            return "No plan stage is active. Return a final answer or clarification.".to_owned();
        };
        let tools = if stage.tools.is_empty() {
            "no tools (return final_answer)".to_owned()
        } else {
            stage.tools.join(", ")
        };
        let hint = match stage.kind {
            StageKind::Investigate => {
                "Inspect the workspace with read-only tools before changing anything."
            }
            StageKind::Change => {
                "Propose an apply_patch action. old_text must be copied exactly from a file you have read."
            }
            StageKind::Verify => {
                "Run cargo_check or cargo_test to verify the current workspace state."
            }
            StageKind::Answer => "Return a final_answer summarizing the work.",
        };
        let files = if self.discovered_files.is_empty() {
            "none".to_owned()
        } else {
            self.discovered_files.join(", ")
        };
        format!(
            "Current plan stage: `{}` ({}). Allowed tools: {tools}. {hint} \
             Task: {}. Files already discovered: {files}.",
            stage.id, stage.label, task.goal.description
        )
    }

    /// Whether the run is ready to produce a final answer.
    #[must_use]
    pub fn can_finish(&self) -> bool {
        if self.stages.is_empty() {
            return true;
        }
        match self.active {
            None => true,
            Some(index) => self.stages[index].kind == StageKind::Answer,
        }
    }

    /// Serializable view for decisions and the UI.
    #[must_use]
    pub fn view(&self, step: u32) -> TaskPlanView {
        TaskPlanView {
            active: self.active_stage().map(|stage| stage.id.clone()),
            stages: self
                .stages
                .iter()
                .map(|stage| PlanStageView {
                    id: stage.id.clone(),
                    label: stage.label.clone(),
                    status: stage.status.label().to_owned(),
                    tools: stage.tools.clone(),
                    overdue: stage.is_overdue(step),
                    attempts: stage.attempts,
                    failures: stage.failures,
                })
                .collect(),
            notes: self.notes.iter().rev().take(8).rev().cloned().collect(),
            working_memory: self
                .discovered_files
                .iter()
                .rev()
                .take(8)
                .rev()
                .cloned()
                .collect(),
            can_finish: self.can_finish(),
        }
    }

    /// Facts derived from the plan after an observation.
    #[must_use]
    pub fn facts(&self) -> Vec<nars_agent_ir::NarsFact> {
        let mut facts = Vec::new();
        for stage in &self.stages {
            facts.push(nars_agent_ir::NarsFact {
                term: format!("plan_{}_{}", stage.id, stage.status.label()),
                truth: nars_agent_ir::TruthValue::new(1.0, 0.95),
                eternal: true,
            });
            if stage.is_overdue(self.step) {
                facts.push(nars_agent_ir::NarsFact {
                    term: format!("plan_{}_overdue", stage.id),
                    truth: nars_agent_ir::TruthValue::new(1.0, 0.9),
                    eternal: true,
                });
            }
        }
        facts
    }

    fn advance(&mut self, index: usize) -> Option<String> {
        let next = index + 1;
        if next < self.stages.len() {
            self.stages[next].status = StageStatus::Active;
            self.stages[next].entered_step = self.step;
            self.active = Some(next);
            Some(self.stages[next].id.clone())
        } else {
            self.active = None;
            None
        }
    }

    fn note(&mut self, note: String) {
        self.notes.push(note);
        while self.notes.len() > 12 {
            self.notes.remove(0);
        }
    }
}

fn stage(
    kind: StageKind,
    label: &str,
    tools: &[&str],
    deadline_steps: u32,
    step: u32,
) -> PlanStage {
    PlanStage {
        id: kind.id().to_owned(),
        kind,
        label: label.to_owned(),
        status: StageStatus::Pending,
        tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
        attempts: 0,
        failures: 0,
        entered_step: step,
        deadline_steps,
        failed_tools: BTreeSet::new(),
        progressed_tools: BTreeSet::new(),
    }
}

fn first_word(input: &str) -> Option<String> {
    input
        .split(|character: char| !character.is_alphanumeric())
        .find(|word| word.len() >= 3)
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nars_agent_ir::{GoalSpec, ObservationId, RunId, ToolCallId};

    fn task(goal_type: GoalType) -> TaskSpec {
        TaskSpec::new(
            GoalSpec::new(goal_type, "fix the parser in src/lib.rs"),
            "/tmp/ws",
            "fix the parser",
        )
    }

    fn observation(tool: &str, outcome: Outcome) -> Observation {
        Observation {
            id: ObservationId::new(),
            run_id: RunId::from_static("run_1"),
            call_id: ToolCallId::new(),
            tool: tool.to_owned(),
            summary: String::new(),
            outcome,
            facts: Vec::new(),
            artifacts: Vec::new(),
        }
    }

    #[test]
    fn modify_code_template_starts_with_investigation() {
        let plan = TaskPlan::for_task(&task(GoalType::ModifyCode), 0);
        assert_eq!(plan.active_stage().unwrap().kind, StageKind::Investigate);
        assert!(plan.allows("read_file"));
        assert!(plan.allows("search"));
        assert!(!plan.allows("apply_patch"));
        assert!(!plan.allows("cargo_test"));
    }

    #[test]
    fn listing_alone_does_not_satisfy_investigation() {
        let mut plan = TaskPlan::for_task(&task(GoalType::ModifyCode), 0);
        let transition = plan.record_observation("list_files", true, None, 1);
        assert!(!transition.advanced);
        assert_eq!(plan.active_stage().unwrap().kind, StageKind::Investigate);
        assert!(!plan.allows("apply_patch"));
    }

    #[test]
    fn investigation_satisfaction_unlocks_change() {
        let mut plan = TaskPlan::for_task(&task(GoalType::ModifyCode), 0);
        let transition = plan.record_observation("read_file", true, Some("src/lib.rs"), 1);
        assert!(transition.advanced);
        assert_eq!(plan.active_stage().unwrap().kind, StageKind::Change);
        assert!(plan.allows("apply_patch"));
        assert!(!plan.allows("cargo_test"));
        assert_eq!(plan.discovered_files, ["src/lib.rs"]);
    }

    #[test]
    fn verification_failure_returns_to_change() {
        let mut plan = TaskPlan::for_task(&task(GoalType::ModifyCode), 0);
        plan.record_observation("read_file", true, Some("src/lib.rs"), 1);
        plan.record_observation("apply_patch", true, None, 2);
        assert_eq!(plan.active_stage().unwrap().kind, StageKind::Verify);
        let transition = plan.record_observation("cargo_test", false, None, 3);
        assert!(!transition.advanced);
        assert_eq!(plan.active_stage().unwrap().kind, StageKind::Change);
        assert!(!plan.can_finish());
    }

    #[test]
    fn cannot_finish_before_verification() {
        let mut plan = TaskPlan::for_task(&task(GoalType::ModifyCode), 0);
        assert!(!plan.can_finish());
        plan.record_observation("read_file", true, Some("src/lib.rs"), 1);
        assert!(!plan.can_finish());
        plan.record_observation("apply_patch", true, None, 2);
        assert!(!plan.can_finish());
        plan.record_observation("cargo_check", true, None, 3);
        assert!(plan.can_finish());
    }

    #[test]
    fn fallback_candidates_are_read_only_and_stage_appropriate() {
        let plan = TaskPlan::for_task(&task(GoalType::ModifyCode), 0);
        let candidates = plan.fallback_candidates(&task(GoalType::ModifyCode));
        assert!(
            candidates
                .iter()
                .all(|candidate| plan.allows(&candidate.tool))
        );
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.tool == "list_files")
        );
        assert!(
            !candidates
                .iter()
                .any(|candidate| candidate.tool == "apply_patch")
        );
    }

    #[test]
    fn recover_replays_observations() {
        let observations = vec![
            observation("read_file", Outcome::Success),
            observation("apply_patch", Outcome::Success),
        ];
        let plan = TaskPlan::recover(&task(GoalType::ModifyCode), &observations, 3);
        assert_eq!(plan.active_stage().unwrap().kind, StageKind::Verify);
    }

    #[test]
    fn other_goal_has_no_gating() {
        let plan = TaskPlan::for_task(&task(GoalType::Other), 0);
        assert!(!plan.is_active());
        assert!(plan.allows("apply_patch"));
        assert!(plan.can_finish());
    }

    #[test]
    fn overdue_stage_is_reported() {
        let mut plan = TaskPlan::for_task(&task(GoalType::Investigate), 0);
        plan.set_step(100);
        let view = plan.view(100);
        assert!(view.stages[0].overdue);
        assert!(
            plan.facts()
                .iter()
                .any(|fact| fact.term.contains("overdue"))
        );
    }
}
