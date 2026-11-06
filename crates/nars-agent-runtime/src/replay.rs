//! Event projection and replay verification.
//!
//! Replay folds the recorded event stream into a fresh [`RunState`]. The
//! resulting state digest must equal the digest captured by the live actor.
//! This is the invariant behind "the UI is a pure function of the event log".

use nars_agent_ir::{AgentError, AgentEvent, ChatMessage, ChatRole, RunState, RunStatus};
use nars_session::{EventEnvelope, SessionSnapshot};

/// Projects an event stream into run state.
///
/// # Errors
///
/// Returns an error when the stream has no `RunStarted` event or contains a
/// run-id mismatch.
pub fn project(events: &[EventEnvelope]) -> Result<RunState, AgentError> {
    let (created_at_ms, run_id, session_id, task) = events
        .iter()
        .find_map(|envelope| match &envelope.event {
            AgentEvent::RunStarted {
                run_id,
                session_id,
                task,
            } => Some((
                envelope.timestamp_ms,
                run_id.clone(),
                session_id.clone(),
                task.clone(),
            )),
            _ => None,
        })
        .ok_or_else(|| AgentError::Replay {
            message: "event stream has no run_started event".to_owned(),
        })?;

    let mut state = RunState::new(run_id.clone(), session_id, task, created_at_ms);
    state.status = RunStatus::Planning;

    for envelope in events {
        if envelope.event.run_id() != Some(&run_id) {
            continue;
        }
        state.touch(envelope.timestamp_ms);
        match &envelope.event {
            AgentEvent::RunStarted { .. } => {}
            AgentEvent::ModelProposed { usage, .. } => {
                state.usage = state.usage.saturating_add(*usage);
            }
            AgentEvent::NarsDecisionMade { step, decision, .. } => {
                state.step = state.step.max(*step);
                state.decisions.push(decision.clone());
            }
            AgentEvent::ApprovalRequested { call, .. } => {
                state.pending_approval = Some(call.clone());
                state.status = RunStatus::AwaitingApproval;
            }
            AgentEvent::ApprovalResolved { .. } => {
                state.pending_approval = None;
            }
            AgentEvent::ToolStarted { call, .. } => {
                if !state
                    .tool_calls
                    .iter()
                    .any(|existing| existing.id == call.id)
                {
                    state.tool_calls.push(call.clone());
                }
                state.status = RunStatus::ExecutingTool;
            }
            AgentEvent::ToolFinished { result, .. } => {
                state.push_result(result.clone());
                state.status = RunStatus::Planning;
            }
            AgentEvent::ObservationRecorded { observation, .. } => {
                state.push_observation(observation.clone());
            }
            AgentEvent::AssistantMessage { text, .. } => {
                state.messages.push(ChatMessage {
                    role: ChatRole::Assistant,
                    text: text.clone(),
                    run_id: Some(run_id.clone()),
                    created_at_ms: envelope.timestamp_ms,
                });
            }
            AgentEvent::RunPaused { .. } => state.status = RunStatus::Paused,
            AgentEvent::RunResumed { .. } => state.status = RunStatus::Planning,
            AgentEvent::RunCompleted { .. } => state.status = RunStatus::Completed,
            AgentEvent::RunFailed { error, .. } => {
                state.status = RunStatus::Failed;
                state.error = Some(error.clone());
            }
            AgentEvent::RunCancelled { .. } => state.status = RunStatus::Cancelled,
            AgentEvent::SessionCreated { .. }
            | AgentEvent::UserMessage { .. }
            | AgentEvent::ModelRequested { .. }
            | AgentEvent::ModelFailed { .. }
            | AgentEvent::NarsGoalCommitted { .. } => {}
        }
    }

    Ok(state)
}

/// Verifies that a snapshot matches the projected state.
///
/// # Errors
///
/// Returns an error when the digests differ.
pub fn verify_snapshot(snapshot: &SessionSnapshot, projected: &RunState) -> Result<(), AgentError> {
    let projected_hash = projected.state_hash();
    if snapshot.state_hash != projected_hash {
        return Err(AgentError::Replay {
            message: format!(
                "snapshot hash {} does not match projected hash {projected_hash}",
                snapshot.state_hash
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nars_agent_ir::{
        GoalSpec, GoalType, NarsDecision, RunId, SessionId, TaskSpec, ToolCall, ToolCallId,
    };
    use serde_json::json;

    fn task() -> TaskSpec {
        TaskSpec::new(
            GoalSpec::new(GoalType::Investigate, "look"),
            "/tmp/ws",
            "look around",
        )
    }

    #[test]
    fn projects_a_minimal_run() {
        let run_id = RunId::from_static("run_1");
        let session_id = SessionId::from_static("ses_1");
        let events = vec![
            EventEnvelope {
                seq: 1,
                timestamp_ms: 10,
                event: AgentEvent::RunStarted {
                    run_id: run_id.clone(),
                    session_id: session_id.clone(),
                    task: task(),
                },
            },
            EventEnvelope {
                seq: 2,
                timestamp_ms: 11,
                event: AgentEvent::NarsDecisionMade {
                    run_id: run_id.clone(),
                    step: 1,
                    decision: NarsDecision {
                        step: 1,
                        selected: None,
                        scored: Vec::new(),
                        rationale: "none".to_owned(),
                        goal_term: "goal".to_owned(),
                        source: nars_agent_ir::DecisionSource::None,
                        plan: None,
                    },
                },
            },
            EventEnvelope {
                seq: 3,
                timestamp_ms: 12,
                event: AgentEvent::ToolStarted {
                    run_id: run_id.clone(),
                    call: ToolCall {
                        id: ToolCallId::from_static("call_1"),
                        run_id: run_id.clone(),
                        tool: "read_file".to_owned(),
                        arguments: json!({"path": "a.rs"}),
                        side_effect: nars_agent_ir::SideEffect::ReadOnly,
                        requires_approval: false,
                        created_at_ms: 12,
                    },
                },
            },
            EventEnvelope {
                seq: 4,
                timestamp_ms: 13,
                event: AgentEvent::AssistantMessage {
                    run_id: run_id.clone(),
                    text: "done".to_owned(),
                },
            },
            EventEnvelope {
                seq: 5,
                timestamp_ms: 14,
                event: AgentEvent::RunCompleted {
                    run_id,
                    summary: "done".to_owned(),
                    success: true,
                },
            },
        ];
        let state = project(&events).unwrap();
        assert_eq!(state.status, RunStatus::Completed);
        assert_eq!(state.step, 1);
        assert_eq!(state.tool_calls.len(), 1);
        assert_eq!(state.messages.len(), 1);
    }

    #[test]
    fn rejects_streams_without_a_start() {
        let events = vec![EventEnvelope {
            seq: 1,
            timestamp_ms: 1,
            event: AgentEvent::RunPaused {
                run_id: RunId::from_static("run_1"),
                reason: "x".to_owned(),
            },
        }];
        assert!(project(&events).is_err());
    }
}
