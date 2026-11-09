//! NARS-backed decision kernel.
//!
//! For every decision round the planner seeds one temporal implication per
//! candidate action:
//!
//! ```text
//! <(ctx_round &/ <({SELF} * aN) --> ^tool>) =/> goal_round>.
//! ```
//!
//! It then adds `ctx_round` as a present belief and `goal_round` as a goal.
//! NARS runs its own decision search, picks the highest-scoring precondition,
//! and invokes the registered operation callback. The callback does **not**
//! execute the tool; it records the selection and returns failure so NARS keeps
//! the goal open. The runtime then executes the selected tool through the
//! broker and reports the outcome back as facts.
//!
//! Model confidence, historical tool outcomes, and side-effect risk determine
//! the implication confidence. NARS therefore makes the actual selection among
//! model-proposed candidates, while the model can never call a tool directly.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use nars_agent_ir::{
    AgentError, CandidateAction, DecisionSource, NarsDecision, NarsFact, NarsGoal, Observation,
    Outcome, ScoredCandidate, SelectedAction, TaskSpec, ToolResult, TruthValue, atomize,
    keyed_atom, short_hash,
};
use nars_core::{MemorySnapshot, OperationFeedback, Reasoner, Term, Truth};
use nars_session::Experience;
use serde::{Deserialize, Serialize};

/// Statistics retained for one tool or action signature.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolStat {
    /// Attempts observed.
    pub attempts: u32,
    /// Successful attempts.
    pub successes: u32,
    /// Failed attempts.
    pub failures: u32,
    /// Consecutive failures, reset by success.
    pub consecutive_failures: u32,
}

impl ToolStat {
    /// Laplace-smoothed success rate.
    #[must_use]
    pub fn success_rate(&self) -> f64 {
        (f64::from(self.successes) + 1.0) / (f64::from(self.attempts) + 2.0)
    }

    fn record(&mut self, success: bool) {
        self.attempts = self.attempts.saturating_add(1);
        if success {
            self.successes = self.successes.saturating_add(1);
            self.consecutive_failures = 0;
        } else {
            self.failures = self.failures.saturating_add(1);
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        }
    }
}

/// Serializable planner state used for resume.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlannerSnapshot {
    /// Schema version.
    pub version: u32,
    /// Deterministic NARS seed.
    pub seed: u64,
    /// Stable short key for the run.
    pub run_key: String,
    /// Facts committed so far (bounded).
    pub facts: Vec<NarsFact>,
    /// Per-tool history.
    pub tool_stats: BTreeMap<String, ToolStat>,
    /// Per-signature history.
    pub signature_stats: BTreeMap<String, ToolStat>,
    /// The task's symbolic goal.
    pub task_goal: Option<NarsGoal>,
    /// Goal type committed by the model proposal, if any.
    #[serde(default)]
    pub task_goal_type: Option<nars_agent_ir::GoalType>,
    /// Number of decisions taken.
    pub decision_count: u32,
}

impl PlannerSnapshot {
    /// A digest for replay comparison.
    #[must_use]
    pub fn digest(&self) -> String {
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("{hash:016x}")
    }
}

#[derive(Clone, Debug)]
struct Captured {
    tool: String,
    argument_term: Term,
}

type ArgumentIndex = HashMap<(String, Term), String>;

/// The NARS decision kernel.
pub struct NarsPlanner {
    reasoner: Reasoner,
    capture: Arc<Mutex<Option<Captured>>>,
    argument_index: Arc<Mutex<ArgumentIndex>>,
    tool_names: BTreeSet<String>,
    fact_log: VecDeque<NarsFact>,
    tool_stats: BTreeMap<String, ToolStat>,
    signature_stats: BTreeMap<String, ToolStat>,
    task_goal: Option<NarsGoal>,
    task_goal_type: Option<nars_agent_ir::GoalType>,
    run_key: String,
    seed: u64,
    decision_count: u32,
}

const MAX_FACT_LOG: usize = 512;

impl std::fmt::Debug for NarsPlanner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NarsPlanner")
            .field("run_key", &self.run_key)
            .field("decisions", &self.decision_count)
            .field("facts", &self.fact_log.len())
            .field("tools", &self.tool_names)
            .finish_non_exhaustive()
    }
}

impl NarsPlanner {
    /// Creates a planner and registers one operation per tool.
    ///
    /// # Errors
    ///
    /// Returns an error when the NARS runtime cannot be initialized or an
    /// operation name is rejected.
    pub fn new(
        run_key: impl Into<String>,
        tool_names: impl IntoIterator<Item = String>,
        seed: u64,
    ) -> Result<Self, AgentError> {
        let reasoner = Self::fresh_reasoner(seed)?;
        Self::with_reasoner(reasoner, run_key, tool_names, seed)
    }

    /// Creates a planner that continues an earlier agent's long-term memory.
    ///
    /// # Errors
    ///
    /// Returns an error when the stored atom table is incompatible with this
    /// build or the memory snapshot is malformed.
    pub fn new_with_snapshot(
        run_key: impl Into<String>,
        tool_names: impl IntoIterator<Item = String>,
        seed: u64,
        experience: Option<&Experience>,
        snapshot: &MemorySnapshot,
    ) -> Result<Self, AgentError> {
        let mut reasoner =
            Reasoner::from_snapshot(snapshot).map_err(|error| AgentError::Internal {
                message: error.to_string(),
            })?;
        Self::configure_reasoner(&mut reasoner, seed)?;
        let mut planner = Self::with_reasoner(reasoner, run_key, tool_names, seed)?;
        planner.apply_experience(experience);
        Ok(planner)
    }

    fn fresh_reasoner(seed: u64) -> Result<Reasoner, AgentError> {
        let mut reasoner = Reasoner::new().map_err(|error| AgentError::Internal {
            message: error.to_string(),
        })?;
        Self::configure_reasoner(&mut reasoner, seed)?;
        Ok(reasoner)
    }

    fn configure_reasoner(reasoner: &mut Reasoner, seed: u64) -> Result<(), AgentError> {
        reasoner.set_motor_babbling_chance(0.0);
        reasoner.set_random_seed(seed);
        // The planner pre-filters and scores candidates, so NARS only needs to
        // rank the seeded set. A low threshold keeps NARS from rejecting every
        // candidate merely because the workspace's concept-priority mass grew.
        reasoner
            .set_decision_threshold(0.01)
            .map_err(|error| AgentError::Internal {
                message: error.to_string(),
            })?;
        Ok(())
    }

    fn with_reasoner(
        mut reasoner: Reasoner,
        run_key: impl Into<String>,
        tool_names: impl IntoIterator<Item = String>,
        seed: u64,
    ) -> Result<Self, AgentError> {
        let capture: Arc<Mutex<Option<Captured>>> = Arc::new(Mutex::new(None));
        let argument_index: Arc<Mutex<ArgumentIndex>> = Arc::new(Mutex::new(HashMap::new()));
        let mut tools = BTreeSet::new();
        for tool in tool_names {
            let operator = format!("^{tool}");
            let tool_capture = Arc::clone(&capture);
            let tool_index = Arc::clone(&argument_index);
            let tool_name = tool.clone();
            reasoner
                .add_operation(&operator, move |arguments| {
                    let resolved = tool_index
                        .lock()
                        .unwrap()
                        .get(&(tool_name.clone(), arguments))
                        .cloned();
                    if let Some(candidate_id) = resolved {
                        let mut slot = tool_capture.lock().unwrap();
                        if slot.is_none() {
                            *slot = Some(Captured {
                                tool: tool_name.clone(),
                                argument_term: arguments,
                            });
                        }
                        let _ = candidate_id;
                    }
                    OperationFeedback {
                        failed: true,
                        ..OperationFeedback::default()
                    }
                })
                .map_err(|error| AgentError::Internal {
                    message: error.to_string(),
                })?;
            tools.insert(tool);
        }

        Ok(Self {
            reasoner,
            capture,
            argument_index,
            tool_names: tools,
            fact_log: VecDeque::new(),
            tool_stats: BTreeMap::new(),
            signature_stats: BTreeMap::new(),
            task_goal: None,
            task_goal_type: None,
            run_key: run_key.into(),
            seed,
            decision_count: 0,
        })
    }

    /// Creates a planner seeded with workspace experience.
    ///
    /// # Errors
    ///
    /// Returns an error when the NARS runtime cannot be initialized.
    pub fn new_with_experience(
        run_key: impl Into<String>,
        tool_names: impl IntoIterator<Item = String>,
        seed: u64,
        experience: Option<&Experience>,
    ) -> Result<Self, AgentError> {
        let mut planner = Self::new(run_key, tool_names, seed)?;
        planner.apply_experience(experience);
        Ok(planner)
    }

    fn apply_experience(&mut self, experience: Option<&Experience>) {
        if let Some(experience) = experience {
            for (tool, stat) in &experience.tools {
                self.tool_stats.insert(
                    tool.clone(),
                    ToolStat {
                        attempts: stat.attempts,
                        successes: stat.successes,
                        failures: stat.failures,
                        consecutive_failures: 0,
                    },
                );
            }
            for (signature, stat) in &experience.signatures {
                self.signature_stats.insert(
                    signature.clone(),
                    ToolStat {
                        attempts: stat.attempts,
                        successes: stat.successes,
                        failures: stat.failures,
                        consecutive_failures: 0,
                    },
                );
            }
        }
    }

    /// Rebuilds a planner from a snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when NARS initialization or fact replay fails.
    pub fn restore(
        snapshot: &PlannerSnapshot,
        tool_names: impl IntoIterator<Item = String>,
    ) -> Result<Self, AgentError> {
        let mut planner = Self::new(snapshot.run_key.clone(), tool_names, snapshot.seed)?;
        planner.apply_planner_snapshot(snapshot)?;
        Ok(planner)
    }

    /// Rebuilds a paused run on top of existing long-term memory.
    ///
    /// The checkpoint supplies the run's own facts and statistics; the memory
    /// snapshot supplies everything learned by earlier runs. Both are needed,
    /// otherwise resuming one run would erase the workspace's history.
    ///
    /// # Errors
    ///
    /// Returns an error when the memory snapshot is incompatible or fact replay
    /// fails.
    pub fn restore_with_memory(
        snapshot: &PlannerSnapshot,
        memory: &MemorySnapshot,
        tool_names: impl IntoIterator<Item = String>,
        experience: Option<&Experience>,
    ) -> Result<Self, AgentError> {
        let mut planner = Self::new_with_snapshot(
            snapshot.run_key.clone(),
            tool_names,
            snapshot.seed,
            experience,
            memory,
        )?;
        planner.apply_planner_snapshot(snapshot)?;
        Ok(planner)
    }

    fn apply_planner_snapshot(&mut self, snapshot: &PlannerSnapshot) -> Result<(), AgentError> {
        self.tool_stats = snapshot.tool_stats.clone();
        self.signature_stats = snapshot.signature_stats.clone();
        self.decision_count = snapshot.decision_count;
        for fact in &snapshot.facts {
            self.commit_fact(fact)?;
        }
        if let Some(goal) = &snapshot.task_goal {
            self.add_task_goal(goal)?;
            self.task_goal = Some(goal.clone());
        }
        self.task_goal_type = snapshot.task_goal_type;
        Ok(())
    }

    /// Long-term memory together with the atom table it depends on.
    #[must_use]
    pub fn memory_snapshot(&self) -> MemorySnapshot {
        self.reasoner.memory().to_snapshot(self.reasoner.language())
    }

    /// Snapshot of the planner state.
    #[must_use]
    pub fn snapshot(&self) -> PlannerSnapshot {
        PlannerSnapshot {
            version: 1,
            seed: self.seed,
            run_key: self.run_key.clone(),
            facts: self.fact_log.iter().cloned().collect(),
            tool_stats: self.tool_stats.clone(),
            signature_stats: self.signature_stats.clone(),
            task_goal: self.task_goal.clone(),
            task_goal_type: self.task_goal_type,
            decision_count: self.decision_count,
        }
    }

    /// Commits a task's goal, constraints, and facts to NARS.
    ///
    /// # Errors
    ///
    /// Returns an error when any term cannot be parsed.
    pub fn commit_task(
        &mut self,
        task: &TaskSpec,
        proposal_confidence: f64,
    ) -> Result<(NarsGoal, Vec<NarsFact>), AgentError> {
        let task_key = keyed_atom("task", &self.run_key);
        let mut facts = Vec::new();
        facts.push(NarsFact {
            term: format!("{task_key}_goal_{}", task.goal.goal_type.atom_suffix()),
            truth: TruthValue::new(1.0, 0.99),
            eternal: true,
        });
        facts.push(NarsFact {
            term: format!("{task_key}_status_active"),
            truth: TruthValue::new(1.0, 0.99),
            eternal: true,
        });
        if let Some(target) = &task.goal.target {
            facts.push(NarsFact {
                term: format!("{task_key}_target_{}", atomize(target)),
                truth: TruthValue::new(1.0, 0.95),
                eternal: true,
            });
        }
        for (index, constraint) in task.constraints.iter().enumerate() {
            facts.push(NarsFact {
                term: format!(
                    "{task_key}_constraint_{index}_{}",
                    atomize(&constraint.chars().take(32).collect::<String>())
                ),
                truth: TruthValue::new(1.0, 0.95),
                eternal: true,
            });
        }
        for fact in &facts {
            self.commit_fact(fact)?;
        }
        self.task_goal_type = Some(task.goal.goal_type);
        let goal = NarsGoal {
            id: format!("{task_key}_goal"),
            description: task.goal.description.clone(),
            term: format!("{task_key}_done"),
            priority: proposal_confidence.clamp(0.05, 1.0),
        };
        self.add_task_goal(&goal)?;
        self.task_goal = Some(goal.clone());
        Ok((goal, facts))
    }

    /// Runs one NARS decision round over the candidate actions.
    ///
    /// # Errors
    ///
    /// Returns an error only when NARS cannot accept the generated sentences.
    #[allow(clippy::too_many_lines)]
    pub fn decide(
        &mut self,
        step: u32,
        candidates: &[CandidateAction],
    ) -> Result<NarsDecision, AgentError> {
        self.decision_count = self.decision_count.saturating_add(1);
        let context = format!("ctx_{}_{}", short_hash(&self.run_key), step);
        let goal_term = format!("goal_{}_{}", short_hash(&self.run_key), step);

        self.capture.lock().unwrap().take();
        self.argument_index.lock().unwrap().clear();
        // Only the decision goal may sit in layer 0. The task goal stays in
        // NARS memory as a concept-level goal but is removed from the active
        // goal queue so it cannot consume the single layer-0 slot.
        self.reasoner.memory.clear_goals();

        let indexed = candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| self.tool_names.contains(&candidate.tool))
            .map(|(index, candidate)| {
                let (score, reason) = self.score_candidate(candidate);
                (
                    format!("a{}", index + 1),
                    candidate,
                    ScoredCandidate {
                        candidate_id: format!("a{}", index + 1),
                        tool: candidate.tool.clone(),
                        score,
                        reason,
                    },
                )
            })
            .collect::<Vec<_>>();
        let mut scored = indexed
            .iter()
            .map(|(_, _, scored)| scored.clone())
            .collect::<Vec<_>>();
        scored.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.candidate_id.cmp(&right.candidate_id))
        });

        let mut seeded = Vec::new();
        for (rank, (candidate_id, candidate, _)) in indexed.iter().enumerate() {
            match self.seed_candidate(&context, &goal_term, candidate_id, candidate, rank) {
                Ok(()) => seeded.push(candidate_id.clone()),
                Err(error) => {
                    tracing::warn!(
                        tool = %candidate.tool,
                        error = %error,
                        "failed to seed NARS candidate"
                    );
                }
            }
        }

        if seeded.is_empty() {
            return Ok(NarsDecision {
                step,
                selected: None,
                scored,
                rationale: "no candidate action could be encoded for NARS".to_owned(),
                goal_term,
                source: DecisionSource::None,
                plan: None,
            });
        }

        self.reasoner
            .add_sentence(&format!("{context}. :|:"))
            .map_err(|error| AgentError::Internal {
                message: format!("failed to commit decision context: {error}"),
            })?;
        self.reasoner
            .add_goal(&goal_term, Truth::new(1.0, 0.9))
            .map_err(|error| AgentError::Internal {
                message: format!("failed to commit decision goal: {error}"),
            })?;

        let captured = self.capture.lock().unwrap().take();
        self.reasoner.memory.clear_goals();

        if let Some(captured) = captured {
            let candidate_id = self
                .argument_index
                .lock()
                .unwrap()
                .get(&(captured.tool.clone(), captured.argument_term))
                .cloned();
            if let Some(candidate_id) = candidate_id
                && let Some((_, selected_candidate, _)) =
                    indexed.iter().find(|(id, _, _)| id == &candidate_id)
            {
                let score = scored
                    .iter()
                    .find(|candidate| candidate.candidate_id == candidate_id)
                    .map_or(selected_candidate.confidence, |candidate| candidate.score);
                let reason = scored
                    .iter()
                    .find(|candidate| candidate.candidate_id == candidate_id)
                    .map_or_else(
                        || "selected by NARS".to_owned(),
                        |candidate| candidate.reason.clone(),
                    );
                return Ok(NarsDecision {
                    step,
                    selected: Some(SelectedAction {
                        candidate_id: candidate_id.clone(),
                        tool: captured.tool.clone(),
                        arguments: selected_candidate.arguments.clone(),
                        score,
                    }),
                    scored,
                    rationale: format!(
                        "NARS selected `{}` ({candidate_id}) via goal inference; {reason}",
                        captured.tool
                    ),
                    goal_term,
                    source: DecisionSource::Nars,
                    plan: None,
                });
            }
        }

        // NARS can fail to reach its decision threshold when every candidate is
        // penalized. Keep the run moving with a deterministic fallback, but make
        // the weaker provenance explicit in the event log.
        let fallback = scored.first().cloned();
        let selected = fallback.as_ref().and_then(|candidate| {
            indexed
                .iter()
                .find(|(id, _, _)| id == &candidate.candidate_id)
                .map(|(_, action, _)| SelectedAction {
                    candidate_id: candidate.candidate_id.clone(),
                    tool: candidate.tool.clone(),
                    arguments: action.arguments.clone(),
                    score: candidate.score,
                })
        });
        Ok(NarsDecision {
            step,
            selected,
            scored,
            rationale: "NARS returned no candidate above its decision threshold; used deterministic score fallback"
                .to_owned(),
            goal_term,
            source: if fallback.is_some() {
                DecisionSource::Fallback
            } else {
                DecisionSource::None
            },
            plan: None,
        })
    }

    /// Records the outcome of a selected action and emits symbolic facts.
    ///
    /// # Errors
    ///
    /// Returns an error when a fact cannot be committed.
    pub fn record_outcome(
        &mut self,
        step: u32,
        candidate: &CandidateAction,
        result: &ToolResult,
    ) -> Result<Vec<NarsFact>, AgentError> {
        let outcome = Outcome::from_status(result.status);
        let success = outcome == Outcome::Success;
        self.tool_stats
            .entry(candidate.tool.clone())
            .or_default()
            .record(success);
        self.signature_stats
            .entry(candidate.signature())
            .or_default()
            .record(success);

        let mut facts = vec![NarsFact {
            term: format!("step_{step}_outcome_{}", outcome.label()),
            truth: TruthValue::new(1.0, 0.95),
            eternal: true,
        }];
        facts.push(NarsFact {
            term: format!(
                "tool_{}_outcome_{}",
                atomize(&candidate.tool),
                outcome.label()
            ),
            truth: TruthValue::new(1.0, 0.95),
            eternal: true,
        });
        if !success {
            facts.push(NarsFact {
                term: format!("tool_{}_needs_retry", atomize(&candidate.tool)),
                truth: TruthValue::new(1.0, 0.9),
                eternal: true,
            });
        }
        for fact in &facts {
            self.commit_fact(fact)?;
        }
        Ok(facts)
    }

    /// Records an observation and its facts.
    ///
    /// # Errors
    ///
    /// Returns an error when a fact cannot be committed.
    pub fn record_observation(
        &mut self,
        observation: &Observation,
    ) -> Result<Vec<NarsFact>, AgentError> {
        let mut facts = observation.facts.clone();
        facts.push(NarsFact {
            term: format!(
                "obs_{}_{}",
                short_hash(observation.id.as_str()),
                observation.outcome.label()
            ),
            truth: TruthValue::new(1.0, 0.95),
            eternal: true,
        });
        for fact in &facts {
            self.commit_fact(fact)?;
        }
        Ok(facts)
    }

    /// Current per-tool statistics.
    #[must_use]
    pub fn tool_stats(&self) -> &BTreeMap<String, ToolStat> {
        &self.tool_stats
    }

    /// Number of decisions taken.
    #[must_use]
    pub const fn decision_count(&self) -> u32 {
        self.decision_count
    }

    fn add_task_goal(&mut self, goal: &NarsGoal) -> Result<(), AgentError> {
        self.reasoner
            .add_goal(&goal.term, Truth::new(1.0, goal.priority.clamp(0.05, 1.0)))
            .map_err(|error| AgentError::Internal {
                message: format!("failed to commit task goal: {error}"),
            })?;
        Ok(())
    }

    fn commit_fact(&mut self, fact: &NarsFact) -> Result<(), AgentError> {
        self.reasoner
            .add_belief(
                &fact.term,
                Truth::new(fact.truth.frequency, fact.truth.confidence),
                fact.eternal,
            )
            .map_err(|error| AgentError::Internal {
                message: format!("failed to commit fact `{}`: {error}", fact.term),
            })?;
        self.fact_log.push_back(fact.clone());
        while self.fact_log.len() > MAX_FACT_LOG {
            self.fact_log.pop_front();
        }
        Ok(())
    }

    fn score_candidate(&self, candidate: &CandidateAction) -> (f64, String) {
        let model_score = candidate.confidence.clamp(0.0, 1.0);
        let mut score = 0.55 * model_score + 0.20;
        let mut notes = vec![format!("model confidence {model_score:.2}")];

        if let Some(stat) = self.tool_stats.get(&candidate.tool) {
            let prior = stat.success_rate();
            score = score * 0.85 + prior * 0.15;
            if stat.failures > 0 {
                let penalty = 0.06 * f64::from(stat.failures.min(4));
                score -= penalty;
                notes.push(format!(
                    "tool failed {} time(s) (-{penalty:.2})",
                    stat.failures
                ));
            }
            notes.push(format!(
                "tool history {}/{} ({prior:.2})",
                stat.successes, stat.attempts
            ));
        }
        if let Some(stat) = self.signature_stats.get(&candidate.signature()) {
            if stat.failures > 0 {
                let penalty = 0.10 * f64::from(stat.failures.min(3));
                score -= penalty;
                notes.push(format!(
                    "signature failed {} time(s) (-{penalty:.2})",
                    stat.failures
                ));
            }
            if stat.successes > 0 {
                let bonus = 0.03 * f64::from(stat.successes.min(2));
                score += bonus;
                notes.push(format!(
                    "signature succeeded {} time(s) (+{bonus:.2})",
                    stat.successes
                ));
            }
        }
        let risk = match candidate.tool.as_str() {
            "read_file" | "list_files" | "search" => 0.03,
            "cargo_check" | "cargo_test" | "git_diff" | "git_status" => 0.02,
            _ => 0.0,
        };
        score += risk;
        score = score.clamp(0.05, 0.98);
        (score, notes.join("; "))
    }

    fn seed_candidate(
        &mut self,
        context: &str,
        goal_term: &str,
        candidate_id: &str,
        candidate: &CandidateAction,
        rank: usize,
    ) -> Result<(), AgentError> {
        let operator = self
            .reasoner
            .language
            .atomic_term(&format!("^{}", candidate.tool))
            .map_err(|error| AgentError::Internal {
                message: error.to_string(),
            })?;
        let action_term = self
            .reasoner
            .language
            .atomic_term(candidate_id)
            .map_err(|error| AgentError::Internal {
                message: error.to_string(),
            })?;
        let operation = self
            .reasoner
            .language
            .operation_with_argument(&operator, &action_term)
            .map_err(|error| AgentError::Internal {
                message: error.to_string(),
            })?;
        let arguments_term = self.reasoner.language.operation_arguments(&operation);
        self.argument_index.lock().unwrap().insert(
            (candidate.tool.clone(), arguments_term),
            candidate_id.to_owned(),
        );

        // Distinct tiny offsets keep ties deterministic without changing the
        // intended ranking.
        let confidence = (self.score_candidate(candidate).0
            - f64::from(u32::try_from(rank).unwrap_or(0)) * 0.0001)
            .clamp(0.05, 0.98);
        let sentence = format!(
            "<({context} &/ <({{SELF}} * {candidate_id}) --> ^{}>) =/> {goal_term}>. {{1.0 {confidence:.4}}}",
            candidate.tool
        );
        self.reasoner
            .add_sentence(&sentence)
            .map_err(|error| AgentError::Internal {
                message: format!("failed to seed candidate `{candidate_id}`: {error}"),
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nars_agent_ir::{GoalSpec, GoalType, ToolCallId, ToolStatus};
    use serde_json::json;

    fn planner() -> NarsPlanner {
        NarsPlanner::new(
            "run_demo",
            [
                "read_file".to_owned(),
                "search".to_owned(),
                "apply_patch".to_owned(),
            ],
            7,
        )
        .unwrap()
    }

    fn task() -> TaskSpec {
        TaskSpec::new(
            GoalSpec::new(GoalType::ModifyCode, "fix the parser").with_target("src/lib.rs"),
            "/tmp/ws",
            "fix the parser",
        )
    }

    #[test]
    fn selects_the_highest_confidence_candidate() {
        let mut planner = planner();
        planner.commit_task(&task(), 0.8).unwrap();
        let candidates = vec![
            CandidateAction::new("read_file")
                .with_arguments(json!({"path": "src/lib.rs"}))
                .with_confidence(0.55),
            CandidateAction::new("search")
                .with_arguments(json!({"query": "parser"}))
                .with_confidence(0.92),
            CandidateAction::new("apply_patch")
                .with_arguments(json!({"path": "src/lib.rs"}))
                .with_confidence(0.7),
        ];
        let decision = planner.decide(1, &candidates).unwrap();
        assert_eq!(decision.source, DecisionSource::Nars, "{decision:?}");
        assert_eq!(decision.selected.as_ref().unwrap().tool, "search");
    }

    #[test]
    fn failed_signature_is_penalized() {
        let mut planner = planner();
        planner.commit_task(&task(), 0.8).unwrap();
        let preferred = CandidateAction::new("search")
            .with_arguments(json!({"query": "x"}))
            .with_confidence(0.8);
        for _ in 0..4 {
            planner
                .record_outcome(1, &preferred, &{
                    let mut result = ToolResult::failure(ToolCallId::new(), "boom");
                    result.status = ToolStatus::Failed;
                    result
                })
                .unwrap();
        }
        let candidates = vec![
            preferred,
            CandidateAction::new("read_file")
                .with_arguments(json!({"path": "src/lib.rs"}))
                .with_confidence(0.7),
        ];
        let decision = planner.decide(2, &candidates).unwrap();
        assert_eq!(
            decision.selected.as_ref().unwrap().tool,
            "read_file",
            "{decision:?}"
        );
    }

    #[test]
    fn snapshot_restore_preserves_statistics() {
        let mut planner = planner();
        planner.commit_task(&task(), 0.8).unwrap();
        let candidate = CandidateAction::new("search").with_arguments(json!({"query": "x"}));
        planner
            .record_outcome(
                1,
                &candidate,
                &ToolResult::failure(ToolCallId::new(), "boom"),
            )
            .unwrap();
        let snapshot = planner.snapshot();
        let restored = NarsPlanner::restore(
            &snapshot,
            [
                "read_file".to_owned(),
                "search".to_owned(),
                "apply_patch".to_owned(),
            ],
        )
        .unwrap();
        assert_eq!(restored.snapshot().tool_stats, snapshot.tool_stats);
        assert_eq!(restored.snapshot().task_goal, snapshot.task_goal);
    }

    #[test]
    fn decisions_are_deterministic_for_a_seed() {
        let mut left = planner();
        let mut right = planner();
        left.commit_task(&task(), 0.8).unwrap();
        right.commit_task(&task(), 0.8).unwrap();
        let candidates = vec![
            CandidateAction::new("read_file").with_confidence(0.6),
            CandidateAction::new("search").with_confidence(0.6),
        ];
        let left_decision = left.decide(1, &candidates).unwrap();
        let right_decision = right.decide(1, &candidates).unwrap();
        assert_eq!(left_decision, right_decision);
    }

    #[test]
    fn workspace_experience_penalizes_unreliable_tools() {
        let mut experience = Experience::new("/tmp/ws");
        for _ in 0..5 {
            experience.record_tool("search", false);
        }
        let mut planner = NarsPlanner::new_with_experience(
            "run_demo",
            ["read_file".to_owned(), "search".to_owned()],
            7,
            Some(&experience),
        )
        .unwrap();
        planner.commit_task(&task(), 0.8).unwrap();
        let candidates = vec![
            CandidateAction::new("search")
                .with_arguments(json!({"query": "parser"}))
                .with_confidence(0.9),
            CandidateAction::new("read_file")
                .with_arguments(json!({"path": "src/lib.rs"}))
                .with_confidence(0.7),
        ];
        let decision = planner.decide(1, &candidates).unwrap();
        assert_eq!(decision.selected.as_ref().unwrap().tool, "read_file");
    }

    #[test]
    fn empty_candidates_produce_no_selection() {
        let mut planner = planner();
        planner.commit_task(&task(), 0.8).unwrap();
        let decision = planner.decide(1, &[]).unwrap();
        assert!(decision.selected.is_none());
        assert_eq!(decision.source, DecisionSource::None);
    }

    #[test]
    fn long_term_memory_survives_a_new_planner() {
        let mut planner = planner();
        planner.commit_task(&task(), 0.8).unwrap();
        let before = planner.memory_snapshot();
        assert!(!before.concepts.is_empty(), "{before:?}");

        let restored = NarsPlanner::new_with_snapshot(
            "run_2",
            [
                "read_file".to_owned(),
                "search".to_owned(),
                "apply_patch".to_owned(),
            ],
            7,
            None,
            &before,
        )
        .unwrap();
        let after = restored.memory_snapshot();
        assert_eq!(after.concepts.len(), before.concepts.len());
        assert_eq!(after.language.atoms, before.language.atoms);
        assert_eq!(after.next_concept_id, before.next_concept_id);
    }

    #[test]
    fn refuses_memory_with_a_drifted_atom_table() {
        let mut planner = planner();
        planner.commit_task(&task(), 0.8).unwrap();
        let mut snapshot = planner.memory_snapshot();
        snapshot.language.atoms[0] = "renamed".to_owned();
        assert!(
            NarsPlanner::new_with_snapshot(
                "run_2",
                ["read_file".to_owned(), "search".to_owned()],
                7,
                None,
                &snapshot,
            )
            .is_err()
        );
    }
}
