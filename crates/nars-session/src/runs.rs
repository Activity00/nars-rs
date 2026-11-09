//! Run summaries derived from the append-only event log.
//!
//! The in-memory run map disappears when `narsd` restarts. The event log does
//! not, so the API can still list historical runs and project their state.

use std::collections::BTreeMap;

use nars_agent_ir::{AgentEvent, RunId};

use crate::event_log::EventEnvelope;
use crate::snapshot::RunSummary;

/// Folds a session's events into one summary per run.
///
/// Summaries are sorted by last update, newest first.
#[must_use]
pub fn summarize_runs(events: &[EventEnvelope]) -> Vec<RunSummary> {
    let mut runs: BTreeMap<RunId, RunSummary> = BTreeMap::new();
    for envelope in events {
        let Some(run_id) = envelope.event.run_id() else {
            continue;
        };
        let summary = runs.entry(run_id.clone()).or_insert_with(|| RunSummary {
            run_id: run_id.clone(),
            status: "pending".to_owned(),
            goal: String::new(),
            step: 0,
            created_at_ms: envelope.timestamp_ms,
            updated_at_ms: envelope.timestamp_ms,
        });
        summary.updated_at_ms = envelope.timestamp_ms;
        match &envelope.event {
            AgentEvent::RunStarted { task, .. } => {
                summary.created_at_ms = envelope.timestamp_ms;
                summary.goal = if task.goal.description.trim().is_empty() {
                    task.user_message.chars().take(120).collect()
                } else {
                    task.goal.description.clone()
                };
                summary.status = "planning".to_owned();
            }
            AgentEvent::NarsDecisionMade { step, .. } => {
                summary.step = summary.step.max(*step);
            }
            AgentEvent::RunPaused { .. } => summary.status = "paused".to_owned(),
            AgentEvent::RunResumed { .. } => summary.status = "planning".to_owned(),
            AgentEvent::RunCompleted { .. } => summary.status = "completed".to_owned(),
            AgentEvent::RunFailed { .. } => summary.status = "failed".to_owned(),
            AgentEvent::RunCancelled { .. } => summary.status = "cancelled".to_owned(),
            _ => {}
        }
    }
    let mut summaries = runs.into_values().collect::<Vec<_>>();
    summaries.sort_by(|left, right| right.updated_at_ms.cmp(&left.updated_at_ms));
    summaries
}

#[cfg(test)]
mod tests {
    use super::*;
    use nars_agent_ir::{GoalSpec, GoalType, SessionId, TaskSpec};

    fn envelope(seq: u64, event: AgentEvent) -> EventEnvelope {
        EventEnvelope {
            seq,
            timestamp_ms: i64::try_from(seq).unwrap() * 10,
            event,
        }
    }

    fn task() -> TaskSpec {
        TaskSpec::new(
            GoalSpec::new(GoalType::ModifyCode, "rename alpha"),
            "/tmp/ws",
            "rename alpha",
        )
    }

    #[test]
    fn summarizes_multiple_runs_newest_first() {
        let first = RunId::from_static("run_1");
        let second = RunId::from_static("run_2");
        let events = vec![
            envelope(
                1,
                AgentEvent::RunStarted {
                    run_id: first.clone(),
                    session_id: SessionId::from_static("ses_1"),
                    task: task(),
                },
            ),
            envelope(
                2,
                AgentEvent::NarsDecisionMade {
                    run_id: first.clone(),
                    step: 1,
                    decision: nars_agent_ir::NarsDecision {
                        step: 1,
                        selected: None,
                        scored: Vec::new(),
                        rationale: String::new(),
                        goal_term: String::new(),
                        source: nars_agent_ir::DecisionSource::None,
                        plan: None,
                    },
                },
            ),
            envelope(
                3,
                AgentEvent::RunCompleted {
                    run_id: first.clone(),
                    summary: String::new(),
                    success: true,
                },
            ),
            envelope(
                4,
                AgentEvent::RunStarted {
                    run_id: second.clone(),
                    session_id: SessionId::from_static("ses_1"),
                    task: task(),
                },
            ),
            envelope(
                5,
                AgentEvent::RunFailed {
                    run_id: second.clone(),
                    error: nars_agent_ir::AgentError::internal("boom"),
                },
            ),
        ];
        let summaries = summarize_runs(&events);
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].run_id, second);
        assert_eq!(summaries[0].status, "failed");
        assert_eq!(summaries[1].run_id, first);
        assert_eq!(summaries[1].status, "completed");
        assert_eq!(summaries[1].step, 1);
        assert_eq!(summaries[1].goal, "rename alpha");
    }

    #[test]
    fn ignores_events_without_a_run() {
        let events = vec![envelope(
            1,
            AgentEvent::UserMessage {
                session_id: SessionId::from_static("ses_1"),
                run_id: None,
                text: "hello".to_owned(),
            },
        )];
        assert!(summarize_runs(&events).is_empty());
    }
}
