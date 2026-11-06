//! Server-Sent Events streams for sessions and runs.

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::{Stream, StreamExt, stream};
use nars_agent_ir::{RunId, SessionId};
use nars_session::EventEnvelope;
use tokio_stream::wrappers::BroadcastStream;

use crate::error::ApiError;
use crate::state::AppState;

/// Streams every event of a session: persisted history first, then live events.
///
/// # Errors
///
/// Returns an error when the session history cannot be read.
pub async fn session_events(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let session_id = SessionId::from_static(session_id);
    let sender = state.session_sender(&session_id);
    let live = sender.subscribe();
    let history = state.store.read_events(&session_id)?;
    let last_seq = history.last().map_or(0, |envelope| envelope.seq);
    Ok(build_stream(history, last_seq, live, None))
}

/// Streams the events of one run.
///
/// # Errors
///
/// Returns an error when the session history cannot be read.
pub async fn run_events(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let run_id = RunId::from_static(run_id);
    let run = state
        .run(&run_id)
        .ok_or_else(|| ApiError::NotFound(format!("run `{run_id}` does not exist")))?;
    let sender = state.session_sender(&run.session_id);
    let live = sender.subscribe();
    let all = state.store.read_events(&run.session_id)?;
    let last_seq = all.last().map_or(0, |envelope| envelope.seq);
    let history = all
        .into_iter()
        .filter(|envelope| envelope.event.run_id() == Some(&run_id))
        .collect();
    Ok(build_stream(history, last_seq, live, Some(run_id)))
}

fn build_stream(
    history: Vec<EventEnvelope>,
    last_seq: u64,
    live: tokio::sync::broadcast::Receiver<EventEnvelope>,
    run_filter: Option<RunId>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let history_stream = stream::iter(history.into_iter().map(|envelope| Ok(to_sse(&envelope))));
    let live_stream = BroadcastStream::new(live).filter_map(move |item| {
        let event = item.ok().and_then(|envelope| {
            if envelope.seq <= last_seq {
                return None;
            }
            if let Some(run_id) = &run_filter
                && envelope.event.run_id() != Some(run_id)
            {
                return None;
            }
            Some(Ok(to_sse(&envelope)))
        });
        async move { event }
    });
    Sse::new(history_stream.chain(live_stream))
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
}

fn to_sse(envelope: &EventEnvelope) -> Event {
    Event::default()
        .event("agent")
        .id(envelope.seq.to_string())
        .json_data(envelope)
        .unwrap_or_else(|_| Event::default().comment("failed to serialize event"))
}
