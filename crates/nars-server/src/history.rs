//! Bounded conversation context for runs in the same session.
//!
//! A run is a self-contained, replayable task, so it must not read live state
//! from earlier runs. Instead the server folds the append-only event log into
//! a short transcript of finished turns and stores it on `TaskSpec::history`.
//! Because the history is embedded in `RunStarted`, replay and branching stay
//! deterministic: a branch copies the same prefix events and therefore sees
//! the same context.

use std::collections::HashMap;

use nars_agent_ir::{AgentEvent, ChatMessage, ChatRole, RunId};
use nars_session::EventEnvelope;

/// Maximum number of earlier turns injected into one run.
pub const MAX_TURNS: usize = 6;
/// Maximum characters kept from one user message or assistant answer.
pub const MAX_TEXT_CHARS: usize = 2_000;
/// Maximum characters kept across the whole injected transcript.
pub const MAX_TOTAL_CHARS: usize = 8_000;

#[derive(Default)]
struct Turn {
    user: Option<(u64, i64, String)>,
    assistant: Option<(i64, String)>,
}

/// Projects finished earlier turns of a session into chat messages.
///
/// Only runs that both received a user message and reached `RunCompleted`
/// contribute; failed, cancelled, and still-running runs are skipped because
/// they have no answer worth carrying forward. Messages are returned oldest
/// first and are already bounded by [`MAX_TURNS`], [`MAX_TEXT_CHARS`], and
/// [`MAX_TOTAL_CHARS`].
#[must_use]
pub fn conversation_history(events: &[EventEnvelope]) -> Vec<ChatMessage> {
    let mut turns: HashMap<RunId, Turn> = HashMap::new();

    for envelope in events {
        match &envelope.event {
            AgentEvent::UserMessage {
                run_id: Some(run_id),
                text,
                ..
            } => {
                let turn = turns.entry(run_id.clone()).or_default();
                if turn.user.is_none() && !text.trim().is_empty() {
                    turn.user = Some((envelope.seq, envelope.timestamp_ms, text.clone()));
                }
            }
            AgentEvent::RunStarted { run_id, .. } => {
                turns.entry(run_id.clone()).or_default();
            }
            AgentEvent::RunCompleted {
                run_id, summary, ..
            } => {
                if !summary.trim().is_empty() {
                    turns.entry(run_id.clone()).or_default().assistant =
                        Some((envelope.timestamp_ms, summary.clone()));
                }
            }
            _ => {}
        }
    }

    let mut ordered = turns
        .into_iter()
        .filter_map(|(run_id, turn)| {
            let (seq, user_at_ms, user_text) = turn.user?;
            let (assistant_at_ms, assistant_text) = turn.assistant?;
            Some((
                seq,
                run_id,
                user_at_ms,
                user_text,
                assistant_at_ms,
                assistant_text,
            ))
        })
        .collect::<Vec<_>>();
    ordered.sort_by_key(|(seq, ..)| *seq);

    let mut selected = Vec::new();
    let mut total = 0;
    for (_, run_id, user_at_ms, user_text, assistant_at_ms, assistant_text) in
        ordered.into_iter().rev()
    {
        if selected.len() >= MAX_TURNS * 2 {
            break;
        }
        let user_text = truncate(&user_text, MAX_TEXT_CHARS);
        let assistant_text = truncate(&assistant_text, MAX_TEXT_CHARS);
        let chars = user_text.chars().count() + assistant_text.chars().count();
        if !selected.is_empty() && total + chars > MAX_TOTAL_CHARS {
            break;
        }
        total += chars;
        selected.push(ChatMessage {
            role: ChatRole::Assistant,
            text: assistant_text,
            run_id: Some(run_id.clone()),
            created_at_ms: assistant_at_ms,
        });
        selected.push(ChatMessage {
            role: ChatRole::User,
            text: user_text,
            run_id: Some(run_id),
            created_at_ms: user_at_ms,
        });
    }
    selected.reverse();
    selected
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    text.chars().take(limit).collect()
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
            GoalSpec::new(GoalType::ModifyCode, "continue"),
            "/tmp/ws",
            "continue",
        )
    }

    fn completed_run(run: &str, user: &str, answer: &str, seq: u64) -> Vec<EventEnvelope> {
        let run_id = RunId::from_static(run);
        vec![
            envelope(
                seq,
                AgentEvent::UserMessage {
                    session_id: SessionId::from_static("ses_1"),
                    run_id: Some(run_id.clone()),
                    text: user.to_owned(),
                },
            ),
            envelope(
                seq + 1,
                AgentEvent::RunStarted {
                    run_id: run_id.clone(),
                    session_id: SessionId::from_static("ses_1"),
                    task: task(),
                },
            ),
            envelope(
                seq + 2,
                AgentEvent::RunCompleted {
                    run_id,
                    summary: answer.to_owned(),
                    success: true,
                },
            ),
        ]
    }

    #[test]
    fn returns_finished_turns_oldest_first() {
        let mut events = completed_run("run_1", "first", "answer one", 1);
        events.extend(completed_run("run_2", "second", "answer two", 4));
        let history = conversation_history(&events);
        assert_eq!(history.len(), 4);
        assert_eq!(history[0].role, ChatRole::User);
        assert_eq!(history[0].text, "first");
        assert_eq!(history[1].role, ChatRole::Assistant);
        assert_eq!(history[1].text, "answer one");
        assert_eq!(history[2].text, "second");
        assert_eq!(history[3].text, "answer two");
    }

    #[test]
    fn skips_unfinished_and_failed_runs() {
        let mut events = completed_run("run_1", "done", "ok", 1);
        events.push(envelope(
            4,
            AgentEvent::UserMessage {
                session_id: SessionId::from_static("ses_1"),
                run_id: Some(RunId::from_static("run_2")),
                text: "unfinished".to_owned(),
            },
        ));
        events.push(envelope(
            5,
            AgentEvent::RunStarted {
                run_id: RunId::from_static("run_2"),
                session_id: SessionId::from_static("ses_1"),
                task: task(),
            },
        ));
        events.push(envelope(
            6,
            AgentEvent::RunFailed {
                run_id: RunId::from_static("run_2"),
                error: nars_agent_ir::AgentError::internal("boom"),
            },
        ));
        let history = conversation_history(&events);
        assert_eq!(history.len(), 2);
    }

    #[test]
    fn keeps_only_the_latest_turns() {
        let mut events = Vec::new();
        for index in 0..8 {
            events.extend(completed_run(
                &format!("run_{index}"),
                &format!("user {index}"),
                &format!("answer {index}"),
                u64::try_from(index).unwrap() * 3 + 1,
            ));
        }
        let history = conversation_history(&events);
        assert_eq!(history.len(), MAX_TURNS * 2);
        assert_eq!(history[0].text, "user 2");
        assert_eq!(history.last().unwrap().text, "answer 7");
    }

    #[test]
    fn truncates_long_text() {
        let long = "x".repeat(MAX_TEXT_CHARS + 100);
        let events = completed_run("run_1", &long, &long, 1);
        let history = conversation_history(&events);
        assert_eq!(history[0].text.chars().count(), MAX_TEXT_CHARS);
        assert_eq!(history[1].text.chars().count(), MAX_TEXT_CHARS);
    }
}
