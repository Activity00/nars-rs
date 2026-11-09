//! Session metadata and snapshots.

use nars_agent_ir::{RunId, RunState, SessionId};
use serde::{Deserialize, Serialize};

/// Metadata for a session directory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    /// Session identifier.
    pub session_id: SessionId,
    /// Canonical workspace root.
    pub workspace: String,
    /// Display title, usually derived from the first user message.
    pub title: String,
    /// Creation timestamp.
    pub created_at_ms: i64,
    /// Last update timestamp.
    pub updated_at_ms: i64,
    /// Parent session when this session is a branch.
    #[serde(default)]
    pub parent_session_id: Option<SessionId>,
    /// Source run that was retried when this session is a branch.
    #[serde(default)]
    pub branched_from_run_id: Option<RunId>,
}

/// A recoverable checkpoint of one run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// Snapshot schema version.
    pub version: u32,
    /// Session identifier.
    pub session_id: SessionId,
    /// Run identifier.
    pub run_id: RunId,
    /// Materialized run state.
    pub state: RunState,
    /// Runtime-internal state (planner memory, counters).
    pub runtime: serde_json::Value,
    /// Digest of `state` at checkpoint time.
    pub state_hash: String,
    /// Last consumed event sequence.
    pub last_seq: u64,
    /// Creation timestamp.
    pub created_at_ms: i64,
}

/// A compact run listing derived from events or snapshots.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    /// Run identifier.
    pub run_id: RunId,
    /// Status label.
    pub status: String,
    /// Goal description.
    pub goal: String,
    /// Current step.
    pub step: u32,
    /// Creation timestamp.
    pub created_at_ms: i64,
    /// Last update timestamp.
    pub updated_at_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use nars_agent_ir::RunStatus;

    #[test]
    fn snapshots_round_trip_through_json() {
        let state = RunState::new(
            RunId::from_static("run_1"),
            SessionId::from_static("ses_1"),
            nars_agent_ir::TaskSpec::new(
                nars_agent_ir::GoalSpec::new(nars_agent_ir::GoalType::Investigate, "x"),
                "/tmp",
                "x",
            ),
            0,
        );
        let snapshot = SessionSnapshot {
            version: 1,
            session_id: SessionId::from_static("ses_1"),
            run_id: RunId::from_static("run_1"),
            state_hash: state.state_hash(),
            state,
            runtime: serde_json::json!({}),
            last_seq: 1,
            created_at_ms: 0,
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        let decoded: SessionSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.state.status, RunStatus::Pending);
        assert_eq!(decoded.version, 1);
    }
}
