//! Session actor: the serial state machine that owns NARS, run state, policy,
//! and the model conversation.

use std::collections::BTreeMap;
use std::sync::Arc;

use nars_agent_ir::{
    AgentError, AgentEvent, AgentIr, CandidateAction, ChatMessage, ChatRole, Clarification,
    FinalAnswer, GoalType, ModelPurpose, Observation, ObservationId, Outcome, RequestId, RunId,
    RunState, RunStatus, SessionId, TaskProposal, TaskSpec, TokenUsage, ToolCall, ToolCallId,
    ToolManifest, ToolResult, TruthValue, ValidationContext, atomize, validate_ir,
};
use nars_core::MemorySnapshot;
use nars_model::{ModelMessage, ModelRequest, agent_ir_schema};
use nars_tools::{ToolBroker, build_call};

use crate::plan::TaskPlan;
use crate::planner::{NarsPlanner, PlannerSnapshot};
use crate::prompt;

/// Model and runtime settings for one actor.
#[derive(Clone)]
pub struct ActorConfig {
    /// Provider model identifier.
    pub model: String,
    /// Sampling temperature.
    pub temperature: f64,
    /// Completion token budget.
    pub max_tokens: u32,
    /// Provider call timeout.
    pub model_timeout_ms: u64,
    /// Deterministic NARS seed.
    pub seed: u64,
    /// Validation repair attempts.
    pub repair_attempts: u32,
    /// Clock used for timestamps.
    pub clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    /// Workspace experience used to seed tool priors.
    pub experience: Option<nars_session::Experience>,
    /// Long-term memory to continue from, when the workspace has a snapshot.
    pub initial_memory: Option<MemorySnapshot>,
}

impl std::fmt::Debug for ActorConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ActorConfig")
            .field("model", &self.model)
            .field("seed", &self.seed)
            .field("max_tokens", &self.max_tokens)
            .finish_non_exhaustive()
    }
}

impl Default for ActorConfig {
    fn default() -> Self {
        Self {
            model: "gpt-4.1-mini".to_owned(),
            temperature: 0.2,
            max_tokens: 4_096,
            model_timeout_ms: 120_000,
            seed: 1,
            repair_attempts: 2,
            clock: Arc::new(system_now_ms),
            experience: None,
            initial_memory: None,
        }
    }
}

fn system_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

/// A command the driver must execute.
#[derive(Debug)]
pub enum Command {
    /// Call the model provider.
    CallModel {
        /// Request identifier.
        request_id: RequestId,
        /// Why the model was called.
        purpose: nars_agent_ir::ModelPurpose,
        /// Step number.
        step: u32,
        /// Fully rendered request.
        request: ModelRequest,
    },
    /// Execute an approved tool call.
    ExecuteTool {
        /// Call to execute.
        call: ToolCall,
    },
    /// Persist a checkpoint.
    Checkpoint,
    /// Stop the run loop.
    Finish,
}

/// Events and follow-up commands produced by one actor transition.
#[derive(Debug, Default)]
pub struct ActorOutput {
    /// Protocol events to publish.
    pub events: Vec<AgentEvent>,
    /// Commands to execute in order.
    pub commands: Vec<Command>,
}

impl ActorOutput {
    fn with_event(mut self, event: AgentEvent) -> Self {
        self.events.push(event);
        self
    }

    fn with_command(mut self, command: Command) -> Self {
        self.commands.push(command);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    AwaitingModel {
        request_id: RequestId,
        purpose: nars_agent_ir::ModelPurpose,
        step: u32,
    },
    AwaitingTool {
        call_id: ToolCallId,
    },
    AwaitingApproval {
        call_id: ToolCallId,
    },
    Finished,
}

struct PendingTool {
    call: ToolCall,
    candidate: CandidateAction,
}

/// The serial owner of one run.
pub struct SessionActor {
    state: RunState,
    planner: NarsPlanner,
    broker: Arc<ToolBroker>,
    manifests: BTreeMap<String, ToolManifest>,
    config: ActorConfig,
    system_prompt: String,
    phase: Phase,
    pending_tool: Option<PendingTool>,
    repair_attempts_left: u32,
    task_committed: bool,
    plan: TaskPlan,
    stage_guidance_left: u32,
    finish_deferrals_left: u32,
}

impl SessionActor {
    /// Creates an actor and starts a run.
    ///
    /// # Errors
    ///
    /// Returns an error when NARS cannot be initialized.
    pub fn new(
        session_id: SessionId,
        task: TaskSpec,
        manifests: Vec<ToolManifest>,
        broker: Arc<ToolBroker>,
        config: ActorConfig,
    ) -> Result<(Self, ActorOutput), AgentError> {
        Self::new_with_run_id(RunId::new(), session_id, task, manifests, broker, config)
    }

    /// Creates an actor with a caller-supplied run identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when NARS cannot be initialized.
    pub fn new_with_run_id(
        run_id: RunId,
        session_id: SessionId,
        task: TaskSpec,
        manifests: Vec<ToolManifest>,
        broker: Arc<ToolBroker>,
        config: ActorConfig,
    ) -> Result<(Self, ActorOutput), AgentError> {
        let run_key = run_id.short().to_owned();
        let tool_names = manifests
            .iter()
            .map(|manifest| manifest.name.clone())
            .collect::<Vec<_>>();
        let planner = match config.initial_memory.as_ref() {
            Some(snapshot) => NarsPlanner::new_with_snapshot(
                run_key,
                tool_names,
                config.seed,
                config.experience.as_ref(),
                snapshot,
            )?,
            None => NarsPlanner::new_with_experience(
                run_key,
                tool_names,
                config.seed,
                config.experience.as_ref(),
            )?,
        };
        let now = (config.clock)();
        let mut state = RunState::new(run_id, session_id, task.clone(), now);
        state.status = RunStatus::Planning;
        let plan = TaskPlan::for_task(&state.task, state.step);
        let system_prompt = prompt::system_prompt(&manifests);
        let repair_attempts = config.repair_attempts;
        let manifests = manifests
            .into_iter()
            .map(|manifest| (manifest.name.clone(), manifest))
            .collect::<BTreeMap<_, _>>();
        let mut actor = Self {
            state,
            planner,
            broker,
            manifests,
            config,
            system_prompt,
            phase: Phase::Idle,
            pending_tool: None,
            repair_attempts_left: repair_attempts,
            task_committed: false,
            plan,
            stage_guidance_left: 2,
            finish_deferrals_left: 2,
        };
        let mut output = ActorOutput::default().with_event(AgentEvent::RunStarted {
            run_id: actor.state.run_id.clone(),
            session_id: actor.state.session_id.clone(),
            task,
        });
        let plan_view = actor.plan.view(actor.state.step);
        let request = actor.begin_model_call(
            ModelPurpose::Plan,
            actor.state.step,
            prompt::plan_message(&actor.state, Some(&plan_view)),
            &mut output,
        );
        output.commands.push(request);
        Ok((actor, output))
    }

    /// Restores an actor from a checkpoint and asks the model for the next step.
    ///
    /// # Errors
    ///
    /// Returns an error for terminal runs or planner restoration failures.
    pub fn restore(
        state: RunState,
        planner_snapshot: &PlannerSnapshot,
        manifests: Vec<ToolManifest>,
        broker: Arc<ToolBroker>,
        config: ActorConfig,
    ) -> Result<(Self, ActorOutput), AgentError> {
        if state.status.is_terminal() {
            return Err(AgentError::internal("cannot resume a terminal run"));
        }
        let tool_names = manifests
            .iter()
            .map(|manifest| manifest.name.clone())
            .collect::<Vec<_>>();
        let planner = match config.initial_memory.as_ref() {
            Some(memory) => NarsPlanner::restore_with_memory(
                planner_snapshot,
                memory,
                tool_names,
                config.experience.as_ref(),
            )?,
            None => NarsPlanner::restore(planner_snapshot, tool_names)?,
        };
        let system_prompt = prompt::system_prompt(&manifests);
        let repair_attempts = config.repair_attempts;
        let task_committed = planner_snapshot.task_goal.is_some();
        let manifests = manifests
            .into_iter()
            .map(|manifest| (manifest.name.clone(), manifest))
            .collect::<BTreeMap<_, _>>();
        let mut state = state;
        state.status = RunStatus::Planning;
        state.touch((config.clock)());
        let plan = TaskPlan::recover(&state.task, &state.observations, state.step);
        let mut actor = Self {
            state,
            planner,
            broker,
            manifests,
            config,
            system_prompt,
            phase: Phase::Idle,
            pending_tool: None,
            repair_attempts_left: repair_attempts,
            task_committed,
            plan,
            stage_guidance_left: 2,
            finish_deferrals_left: 2,
        };
        let mut output = ActorOutput::default().with_event(AgentEvent::RunResumed {
            run_id: actor.state.run_id.clone(),
        });
        let plan_view = actor.plan.view(actor.state.step);
        let command = actor.begin_model_call(
            ModelPurpose::Plan,
            actor.state.step,
            prompt::plan_message(&actor.state, Some(&plan_view)),
            &mut output,
        );
        output.commands.push(command);
        Ok((actor, output))
    }

    /// Materialized run state.
    #[must_use]
    pub fn state(&self) -> &RunState {
        &self.state
    }

    /// Planner checkpoint.
    #[must_use]
    pub fn planner_snapshot(&self) -> PlannerSnapshot {
        self.planner.snapshot()
    }

    /// Long-term memory accumulated by this run so far.
    #[must_use]
    pub fn memory_snapshot(&self) -> MemorySnapshot {
        self.planner.memory_snapshot()
    }

    /// Whether the run is finished.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        matches!(self.phase, Phase::Finished)
    }

    /// Whether the run is paused.
    #[must_use]
    pub const fn is_paused(&self) -> bool {
        matches!(self.state.status, RunStatus::Paused)
    }

    /// Handles a successful model response.
    ///
    /// # Errors
    ///
    /// Returns an error when the response does not match the pending request.
    pub fn on_model_ok(
        &mut self,
        request_id: &RequestId,
        ir: AgentIr,
        usage: TokenUsage,
    ) -> Result<ActorOutput, AgentError> {
        let expected = match &self.phase {
            Phase::AwaitingModel {
                request_id: expected,
                ..
            } => expected.clone(),
            _ => {
                return Err(AgentError::internal(
                    "model response arrived while no model call was pending",
                ));
            }
        };
        if &expected != request_id {
            return Err(AgentError::internal("model response request id mismatch"));
        }
        self.state.usage = self.state.usage.saturating_add(usage);
        let mut output = ActorOutput::default().with_event(AgentEvent::ModelProposed {
            run_id: self.state.run_id.clone(),
            request_id: request_id.clone(),
            step: self.state.step,
            ir: ir.clone(),
            usage,
        });
        self.phase = Phase::Idle;

        let validation_context = self.validation_context();
        match validate_ir(&ir, &validation_context) {
            Ok(validated) => {
                self.repair_attempts_left = self.config.repair_attempts;
                match validated.ir {
                    AgentIr::TaskProposal(proposal) => self.handle_proposal(proposal, output),
                    AgentIr::FinalAnswer(answer) => Ok(self.handle_final_answer(answer, output)),
                    AgentIr::Clarification(clarification) => {
                        Ok(self.handle_clarification(clarification, output))
                    }
                    AgentIr::ActionParameters(parameters) => Ok(self.fail(
                        AgentError::Validation {
                            message: format!(
                                "unexpected action_parameters response for `{}`",
                                parameters.tool
                            ),
                        },
                        output,
                    )),
                }
            }
            Err(issues) => {
                let message = issues
                    .iter()
                    .map(|issue| format!("{}: {}", issue.field, issue.message))
                    .collect::<Vec<_>>()
                    .join("; ");
                if self.repair_attempts_left > 0 {
                    self.repair_attempts_left -= 1;
                    output.events.push(AgentEvent::ModelFailed {
                        run_id: self.state.run_id.clone(),
                        request_id: request_id.clone(),
                        step: self.state.step,
                        error: format!("validation rejected: {message}"),
                    });
                    let command = self.begin_model_call(
                        ModelPurpose::Plan,
                        self.state.step,
                        prompt::repair_message(&self.state, &message),
                        &mut output,
                    );
                    output.commands.push(command);
                    Ok(output)
                } else {
                    Ok(self.fail(AgentError::Validation { message }, output))
                }
            }
        }
    }

    /// Handles a provider failure after retries.
    pub fn on_model_err(
        &mut self,
        request_id: &RequestId,
        error: &nars_model::ModelError,
    ) -> ActorOutput {
        let output = ActorOutput::default().with_event(AgentEvent::ModelFailed {
            run_id: self.state.run_id.clone(),
            request_id: request_id.clone(),
            step: self.state.step,
            error: error.message.clone(),
        });
        self.fail(
            AgentError::model(error.message.clone(), error.retryable),
            output,
        )
    }

    /// Handles a completed tool call.
    ///
    /// # Errors
    ///
    /// Returns an error when no matching call is pending.
    pub fn on_tool_result(&mut self, result: ToolResult) -> Result<ActorOutput, AgentError> {
        let Some(pending) = self.pending_tool.take() else {
            return Err(AgentError::internal(
                "tool result arrived with no pending call",
            ));
        };
        if pending.call.id != result.call_id {
            return Err(AgentError::internal("tool result call id mismatch"));
        }
        self.phase = Phase::Idle;
        self.state.push_result(result.clone());

        let outcome = Outcome::from_status(result.status);
        let path = result
            .metadata
            .get("path")
            .and_then(serde_json::Value::as_str);
        if pending.call.tool == "list_files" && result.is_success() {
            let files = result
                .stdout
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.ends_with('/'))
                .filter(|line| !line.starts_with('.'))
                .filter(|line| !line.contains("node_modules"))
                .take(8)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            self.plan.note_files(files);
        }
        self.plan.record_observation(
            &pending.call.tool,
            result.is_success(),
            path,
            self.state.step,
        );
        let outcome_facts =
            self.planner
                .record_outcome(self.state.step, &pending.candidate, &result)?;
        let mut facts = outcome_facts;
        facts.extend(observation_facts(&pending.call, &result));
        facts.extend(self.plan.facts());
        let observation = Observation {
            id: ObservationId::new(),
            run_id: self.state.run_id.clone(),
            call_id: result.call_id.clone(),
            tool: pending.call.tool.clone(),
            summary: format!("{} — {}", pending.call.tool, result.summary()),
            outcome,
            facts: facts.clone(),
            artifacts: result.artifacts.clone(),
        };
        self.planner.record_observation(&observation)?;
        self.state.push_observation(observation.clone());

        let mut output = ActorOutput::default()
            .with_event(AgentEvent::ToolFinished {
                run_id: self.state.run_id.clone(),
                result,
            })
            .with_event(AgentEvent::ObservationRecorded {
                run_id: self.state.run_id.clone(),
                observation,
                plan: Some(self.plan.view(self.state.step)),
            })
            .with_command(Command::Checkpoint);
        self.state.status = RunStatus::Planning;
        self.state.touch((self.config.clock)());
        let plan_view = self.plan.view(self.state.step);
        let command = self.begin_model_call(
            ModelPurpose::Plan,
            self.state.step,
            prompt::plan_message(&self.state, Some(&plan_view)),
            &mut output,
        );
        output.commands.push(command);
        Ok(output)
    }

    /// Handles an approval decision.
    ///
    /// # Errors
    ///
    /// Returns an error when no matching approval is pending.
    pub fn on_approval(
        &mut self,
        call_id: &ToolCallId,
        approved: bool,
        note: Option<String>,
    ) -> Result<ActorOutput, AgentError> {
        let Some(pending) = self.pending_tool.take() else {
            return Err(AgentError::internal(
                "approval arrived with no pending call",
            ));
        };
        if Some(&pending.call.id) != self.state.pending_approval.as_ref().map(|call| &call.id) {
            return Err(AgentError::internal("approval call id mismatch"));
        }
        self.state.pending_approval = None;
        let mut output = ActorOutput::default().with_event(AgentEvent::ApprovalResolved {
            run_id: self.state.run_id.clone(),
            call_id: call_id.clone(),
            approved,
            note: note.clone(),
        });
        if approved {
            let call = pending.call.clone();
            self.pending_tool = Some(pending);
            self.start_tool(call, &mut output);
            Ok(output)
        } else {
            let reason = note.unwrap_or_else(|| "user denied the call".to_owned());
            let result = ToolResult::denied(pending.call.id.clone(), reason);
            self.finish_denied(pending, result, output)
        }
    }

    /// Pauses the run.
    pub fn pause(&mut self, reason: impl Into<String>) -> ActorOutput {
        if self.phase == Phase::Finished || self.state.status == RunStatus::Paused {
            return ActorOutput::default();
        }
        self.state.status = RunStatus::Paused;
        self.state.touch((self.config.clock)());
        ActorOutput::default().with_event(AgentEvent::RunPaused {
            run_id: self.state.run_id.clone(),
            reason: reason.into(),
        })
    }

    /// Resumes a paused run.
    pub fn resume(&mut self) -> ActorOutput {
        if self.state.status != RunStatus::Paused {
            return ActorOutput::default();
        }
        self.state.status = RunStatus::Planning;
        self.state.touch((self.config.clock)());
        ActorOutput::default().with_event(AgentEvent::RunResumed {
            run_id: self.state.run_id.clone(),
        })
    }

    /// Cancels the run.
    pub fn cancel(&mut self, reason: impl Into<String>) -> ActorOutput {
        if self.phase == Phase::Finished {
            return ActorOutput::default();
        }
        self.phase = Phase::Finished;
        self.pending_tool = None;
        self.state.pending_approval = None;
        self.state.status = RunStatus::Cancelled;
        self.state.touch((self.config.clock)());
        ActorOutput::default()
            .with_event(AgentEvent::RunCancelled {
                run_id: self.state.run_id.clone(),
                reason: reason.into(),
            })
            .with_command(Command::Checkpoint)
            .with_command(Command::Finish)
    }

    fn handle_proposal(
        &mut self,
        proposal: TaskProposal,
        mut output: ActorOutput,
    ) -> Result<ActorOutput, AgentError> {
        if !self.task_committed {
            let mut committed = self.state.task.clone();
            if committed.goal.description.trim().is_empty() {
                committed.goal.description = proposal.goal.description.clone();
            }
            if committed.goal.target.is_none() {
                committed.goal.target = proposal.goal.target.clone();
            }
            for constraint in &proposal.constraints {
                if committed.constraints.len() >= 32 {
                    break;
                }
                if !constraint.trim().is_empty() && !committed.constraints.contains(constraint) {
                    committed.constraints.push(constraint.clone());
                }
            }
            // A generic user task can be refined into a concrete goal type by
            // the model. Build the stage template from the refined goal, before
            // marking the task committed so the plan is created exactly once.
            if committed.goal.goal_type == GoalType::Other {
                committed.goal.goal_type = proposal.goal.goal_type;
            }
            if !self.plan.is_active() && committed.goal.goal_type != GoalType::Other {
                self.plan = TaskPlan::for_task(&committed, self.state.step);
            }
            let (goal, facts) = self.planner.commit_task(&committed, proposal.confidence)?;
            self.task_committed = true;
            output.events.push(AgentEvent::NarsGoalCommitted {
                run_id: self.state.run_id.clone(),
                step: self.state.step,
                goal,
                facts,
            });
        }

        if self.state.step >= self.state.task.max_steps {
            return Ok(self.fail(
                AgentError::StepLimit {
                    limit: self.state.task.max_steps,
                },
                output,
            ));
        }

        let (mut candidates, deferred) = self.plan.filter(&proposal.candidate_actions);
        candidates.extend(self.plan.fallback_candidates(&self.state.task));
        dedupe_candidates(&mut candidates);

        if candidates.is_empty() {
            if self.plan.is_active() {
                if self.stage_guidance_left > 0 {
                    self.stage_guidance_left -= 1;
                    let guidance = self.plan.stage_guidance(&self.state.task);
                    let plan_view = self.plan.view(self.state.step);
                    let command = self.begin_model_call(
                        ModelPurpose::Plan,
                        self.state.step,
                        prompt::stage_message(&self.state, &guidance, Some(&plan_view)),
                        &mut output,
                    );
                    output.commands.push(command);
                    return Ok(output);
                }
                return Ok(self.fail(
                    AgentError::internal("plan stage has no admissible action"),
                    output,
                ));
            }
            let plan_view = self.plan.view(self.state.step);
            let command = self.begin_model_call(
                ModelPurpose::Finalize,
                self.state.step,
                prompt::finalize_message(&self.state, Some(&plan_view)),
                &mut output,
            );
            output.commands.push(command);
            return Ok(output);
        }
        self.stage_guidance_left = 2;

        self.state.step += 1;
        self.plan.set_step(self.state.step);
        let mut decision = self.planner.decide(self.state.step, &candidates)?;
        decision.plan = Some(self.plan.view(self.state.step));
        if !deferred.is_empty() {
            let stage = self
                .plan
                .active_stage()
                .map_or("none", |stage| stage.id.as_str());
            decision.rationale = format!(
                "deferred {} candidate(s) outside stage `{stage}`; {}",
                deferred.len(),
                decision.rationale
            );
        }
        self.state.decisions.push(decision.clone());
        output.events.push(AgentEvent::NarsDecisionMade {
            run_id: self.state.run_id.clone(),
            step: self.state.step,
            decision: decision.clone(),
        });

        let Some(selected) = decision.selected else {
            return Ok(self.fail(
                AgentError::internal("NARS selected no action and no fallback was available"),
                output,
            ));
        };
        let candidate = selected
            .candidate_id
            .strip_prefix('a')
            .and_then(|index| index.parse::<usize>().ok())
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| candidates.get(index))
            .cloned()
            .or_else(|| {
                candidates
                    .iter()
                    .find(|candidate| candidate.tool == selected.tool)
                    .cloned()
            });
        let Some(candidate) = candidate else {
            return Ok(self.fail(
                AgentError::internal("selected candidate is missing from the proposal"),
                output,
            ));
        };
        let Some(manifest) = self.manifests.get(&selected.tool).cloned() else {
            return Ok(self.fail(
                AgentError::Validation {
                    message: format!("selected tool `{}` is not registered", selected.tool),
                },
                output,
            ));
        };

        let call = build_call(
            ToolCallId::new(),
            self.state.run_id.clone(),
            &manifest,
            selected.arguments.clone(),
            (self.config.clock)(),
        );
        self.state.tool_calls.push(call.clone());
        self.state.touch((self.config.clock)());

        let policy = self.broker.evaluate(&call);
        if let nars_tools::PolicyDecision::Deny { reason } = &policy {
            let result = ToolResult::denied(call.id.clone(), reason.clone());
            return self.finish_denied(PendingTool { call, candidate }, result, output);
        }

        if policy.requires_approval() {
            self.state.status = RunStatus::AwaitingApproval;
            self.state.pending_approval = Some(call.clone());
            self.phase = Phase::AwaitingApproval {
                call_id: call.id.clone(),
            };
            self.pending_tool = Some(PendingTool {
                call: call.clone(),
                candidate,
            });
            let preview = self.broker.preview(&call);
            output.events.push(AgentEvent::ApprovalRequested {
                run_id: self.state.run_id.clone(),
                call,
                reason: policy.reason().to_owned(),
                preview,
            });
            return Ok(output);
        }

        self.pending_tool = Some(PendingTool {
            call: call.clone(),
            candidate,
        });
        self.start_tool(call, &mut output);
        Ok(output)
    }

    fn handle_final_answer(&mut self, answer: FinalAnswer, mut output: ActorOutput) -> ActorOutput {
        if !self.plan.can_finish() && self.finish_deferrals_left > 0 {
            self.finish_deferrals_left -= 1;
            let guidance = self.plan.stage_guidance(&self.state.task);
            let plan_view = self.plan.view(self.state.step);
            let message = format!(
                "A final_answer is premature because the task plan is not complete. {guidance}                  Continue with an admissible action, or return a clarification if you are blocked."
            );
            let command = self.begin_model_call(
                ModelPurpose::Plan,
                self.state.step,
                prompt::stage_message(&self.state, &message, Some(&plan_view)),
                &mut output,
            );
            output.commands.push(command);
            return output;
        }
        self.state.messages.push(ChatMessage {
            role: ChatRole::Assistant,
            text: answer.text.clone(),
            run_id: Some(self.state.run_id.clone()),
            created_at_ms: (self.config.clock)(),
        });
        output.events.push(AgentEvent::AssistantMessage {
            run_id: self.state.run_id.clone(),
            text: answer.text.clone(),
        });
        self.phase = Phase::Finished;
        self.state.status = RunStatus::Completed;
        self.state.touch((self.config.clock)());
        output.events.push(AgentEvent::RunCompleted {
            run_id: self.state.run_id.clone(),
            summary: answer.text,
            success: answer.success,
        });
        output.commands.push(Command::Checkpoint);
        output.commands.push(Command::Finish);
        output
    }

    fn handle_clarification(
        &mut self,
        clarification: Clarification,
        mut output: ActorOutput,
    ) -> ActorOutput {
        self.state.messages.push(ChatMessage {
            role: ChatRole::Assistant,
            text: clarification.question.clone(),
            run_id: Some(self.state.run_id.clone()),
            created_at_ms: (self.config.clock)(),
        });
        output.events.push(AgentEvent::AssistantMessage {
            run_id: self.state.run_id.clone(),
            text: clarification.question.clone(),
        });
        self.phase = Phase::Finished;
        self.state.status = RunStatus::Completed;
        self.state.touch((self.config.clock)());
        output.events.push(AgentEvent::RunCompleted {
            run_id: self.state.run_id.clone(),
            summary: clarification.question,
            success: false,
        });
        output.commands.push(Command::Checkpoint);
        output.commands.push(Command::Finish);
        output
    }

    fn start_tool(&mut self, call: ToolCall, output: &mut ActorOutput) {
        self.phase = Phase::AwaitingTool {
            call_id: call.id.clone(),
        };
        self.state.status = RunStatus::ExecutingTool;
        self.state.touch((self.config.clock)());
        output.events.push(AgentEvent::ToolStarted {
            run_id: self.state.run_id.clone(),
            call: call.clone(),
        });
        output.commands.push(Command::ExecuteTool { call });
    }

    fn finish_denied(
        &mut self,
        pending: PendingTool,
        result: ToolResult,
        mut output: ActorOutput,
    ) -> Result<ActorOutput, AgentError> {
        self.phase = Phase::Idle;
        self.pending_tool = None;
        output.events.push(AgentEvent::ToolStarted {
            run_id: self.state.run_id.clone(),
            call: pending.call.clone(),
        });
        self.state.push_result(result.clone());
        let outcome = Outcome::from_status(result.status);
        self.plan.record_observation(
            &pending.call.tool,
            result.is_success(),
            None,
            self.state.step,
        );
        let mut facts =
            self.planner
                .record_outcome(self.state.step, &pending.candidate, &result)?;
        facts.extend(observation_facts(&pending.call, &result));
        facts.extend(self.plan.facts());
        let observation = Observation {
            id: ObservationId::new(),
            run_id: self.state.run_id.clone(),
            call_id: result.call_id.clone(),
            tool: pending.call.tool.clone(),
            summary: format!("{} — {}", pending.call.tool, result.summary()),
            outcome,
            facts,
            artifacts: result.artifacts.clone(),
        };
        self.planner.record_observation(&observation)?;
        self.state.push_observation(observation.clone());
        self.state.touch((self.config.clock)());
        output.events.push(AgentEvent::ToolFinished {
            run_id: self.state.run_id.clone(),
            result,
        });
        output.events.push(AgentEvent::ObservationRecorded {
            run_id: self.state.run_id.clone(),
            observation,
            plan: Some(self.plan.view(self.state.step)),
        });
        output.commands.push(Command::Checkpoint);
        self.state.status = RunStatus::Planning;
        let plan_view = self.plan.view(self.state.step);
        let command = self.begin_model_call(
            ModelPurpose::Plan,
            self.state.step,
            prompt::plan_message(&self.state, Some(&plan_view)),
            &mut output,
        );
        output.commands.push(command);
        Ok(output)
    }

    /// Fails the run from a driver-level error.
    pub fn on_internal_error(&mut self, error: AgentError) -> ActorOutput {
        self.fail(error, ActorOutput::default())
    }

    fn fail(&mut self, error: AgentError, mut output: ActorOutput) -> ActorOutput {
        self.phase = Phase::Finished;
        self.pending_tool = None;
        self.state.pending_approval = None;
        self.state.status = RunStatus::Failed;
        self.state.error = Some(error.clone());
        self.state.touch((self.config.clock)());
        output.events.push(AgentEvent::RunFailed {
            run_id: self.state.run_id.clone(),
            error,
        });
        output.commands.push(Command::Checkpoint);
        output.commands.push(Command::Finish);
        output
    }

    fn begin_model_call(
        &mut self,
        purpose: nars_agent_ir::ModelPurpose,
        step: u32,
        message: ModelMessage,
        output: &mut ActorOutput,
    ) -> Command {
        let request_id = RequestId::new();
        output.events.push(AgentEvent::ModelRequested {
            run_id: self.state.run_id.clone(),
            request_id: request_id.clone(),
            step,
            purpose,
        });
        let request = ModelRequest {
            model: self.config.model.clone(),
            messages: vec![ModelMessage::system(self.system_prompt.clone()), message],
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
            json_schema: Some(agent_ir_schema()),
            timeout_ms: self.config.model_timeout_ms,
        };
        self.phase = Phase::AwaitingModel {
            request_id: request_id.clone(),
            purpose,
            step,
        };
        Command::CallModel {
            request_id,
            purpose,
            step,
            request,
        }
    }

    fn validation_context(&self) -> ValidationContext {
        ValidationContext::new(self.manifests.keys().cloned())
    }
}

fn dedupe_candidates(candidates: &mut Vec<CandidateAction>) {
    let mut seen = std::collections::BTreeSet::new();
    candidates.retain(|candidate| seen.insert(candidate.signature()));
}

fn observation_facts(call: &ToolCall, result: &ToolResult) -> Vec<nars_agent_ir::NarsFact> {
    let tool = atomize(&call.tool);
    let outcome = Outcome::from_status(result.status);
    let mut facts = vec![nars_agent_ir::NarsFact {
        term: format!("tool_{tool}_observed_{}", outcome.label()),
        truth: TruthValue::new(1.0, 0.95),
        eternal: true,
    }];
    if let Some(path) = result
        .metadata
        .get("path")
        .and_then(serde_json::Value::as_str)
        && call.tool == "read_file"
    {
        facts.push(nars_agent_ir::NarsFact {
            term: format!("file_{}_read", atomize(path)),
            truth: TruthValue::new(1.0, 0.95),
            eternal: true,
        });
    }
    if let Some(diffs) = result
        .metadata
        .get("diffs")
        .and_then(serde_json::Value::as_array)
    {
        for diff in diffs {
            if let Some(path) = diff.get("path").and_then(serde_json::Value::as_str) {
                facts.push(nars_agent_ir::NarsFact {
                    term: format!("file_{}_modified", atomize(path)),
                    truth: TruthValue::new(1.0, 0.95),
                    eternal: true,
                });
            }
        }
    }
    if call.tool.starts_with("cargo_") {
        facts.push(nars_agent_ir::NarsFact {
            term: if result.is_success() {
                "check_passed".to_owned()
            } else {
                "check_failed".to_owned()
            },
            truth: TruthValue::new(1.0, 0.95),
            eternal: true,
        });
    }
    facts
}
