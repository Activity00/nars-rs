//! File-backed session store.
//!
//! Layout:
//!
//! ```text
//! <root>/<session_id>/meta.json
//! <root>/<session_id>/events.jsonl
//! <root>/<session_id>/snapshot.json
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nars_agent_ir::{AgentEvent, RunId, SessionId};
use thiserror::Error;

use crate::event_log::{EventEnvelope, EventLog, LogError};
use crate::snapshot::{SessionMeta, SessionSnapshot};

/// Store errors.
#[derive(Debug, Error)]
pub enum StoreError {
    /// Invalid session identifier.
    #[error("invalid session id: {0}")]
    InvalidId(String),
    /// Session not found.
    #[error("session not found: {0}")]
    NotFound(String),
    /// Filesystem failure.
    #[error("store I/O error at {path}: {source}")]
    Io {
        /// Path involved.
        path: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// JSON serialization failure.
    #[error("store JSON error at {path}: {source}")]
    Json {
        /// Path involved.
        path: String,
        /// Underlying error.
        #[source]
        source: serde_json::Error,
    },
    /// Event log failure.
    #[error(transparent)]
    Log(#[from] LogError),
    /// SQLite or worker failure.
    #[error("session database error: {0}")]
    Database(String),
    /// The stored value changed since it was loaded.
    #[error("store conflict: {0}")]
    Conflict(String),
}

/// Persistence interface, implemented by the file store and future SQLite store.
pub trait SessionStore: Send + Sync {
    /// Creates a new session directory and metadata record.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identifiers or filesystem failures.
    fn create_session(
        &self,
        workspace: &str,
        title: &str,
        now_ms: i64,
    ) -> Result<SessionMeta, StoreError> {
        self.create_session_with_lineage(workspace, title, None, None, now_ms)
    }
    /// Creates a new session, optionally recording branch lineage.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identifiers or filesystem failures.
    fn create_session_with_lineage(
        &self,
        workspace: &str,
        title: &str,
        parent_session_id: Option<SessionId>,
        branched_from_run_id: Option<RunId>,
        now_ms: i64,
    ) -> Result<SessionMeta, StoreError>;
    /// Deletes a session and all of its events and snapshots.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identifiers, missing sessions, or
    /// filesystem failures.
    fn delete_session(&self, session_id: &SessionId) -> Result<(), StoreError>;
    /// Lists sessions sorted by most recently updated.
    ///
    /// # Errors
    ///
    /// Returns an error when the store root cannot be read.
    fn list_sessions(&self) -> Result<Vec<SessionMeta>, StoreError>;
    /// Reads one session's metadata.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identifiers or malformed metadata.
    fn get_session(&self, session_id: &SessionId) -> Result<Option<SessionMeta>, StoreError>;
    /// Appends one event to a session log.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identifiers or log failures.
    fn append(
        &self,
        session_id: &SessionId,
        event: AgentEvent,
        timestamp_ms: i64,
    ) -> Result<EventEnvelope, StoreError>;
    /// Reads all events of a session.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identifiers or malformed logs.
    fn read_events(&self, session_id: &SessionId) -> Result<Vec<EventEnvelope>, StoreError>;
    /// Saves a run snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identifiers or filesystem failures.
    fn save_snapshot(&self, snapshot: &SessionSnapshot) -> Result<(), StoreError>;
    /// Loads the most recent snapshot for a session.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identifiers or malformed snapshots.
    fn load_snapshot(&self, session_id: &SessionId) -> Result<Option<SessionSnapshot>, StoreError>;
    /// Updates session metadata timestamps and title.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identifiers or filesystem failures.
    fn touch_session(
        &self,
        session_id: &SessionId,
        now_ms: i64,
        title: Option<&str>,
    ) -> Result<(), StoreError>;
}

/// File-backed implementation.
#[derive(Debug)]
pub struct FileSessionStore {
    root: PathBuf,
    logs: Mutex<HashMap<SessionId, Arc<EventLog>>>,
}

impl FileSessionStore {
    /// Opens or creates a store rooted at a directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the root cannot be created.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|source| StoreError::Io {
            path: root.display().to_string(),
            source,
        })?;
        Ok(Self {
            root,
            logs: Mutex::new(HashMap::new()),
        })
    }

    /// The store root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn session_dir(&self, session_id: &SessionId) -> Result<PathBuf, StoreError> {
        validate_id(session_id.as_str())?;
        Ok(self.root.join(session_id.as_str()))
    }

    fn log_for(&self, session_id: &SessionId) -> Result<Arc<EventLog>, StoreError> {
        let mut logs = self.logs.lock().unwrap();
        if let Some(log) = logs.get(session_id) {
            return Ok(Arc::clone(log));
        }
        let log = Arc::new(EventLog::open(
            self.session_dir(session_id)?.join("events.jsonl"),
        )?);
        logs.insert(session_id.clone(), Arc::clone(&log));
        Ok(log)
    }
}

impl SessionStore for FileSessionStore {
    fn create_session_with_lineage(
        &self,
        workspace: &str,
        title: &str,
        parent_session_id: Option<SessionId>,
        branched_from_run_id: Option<RunId>,
        now_ms: i64,
    ) -> Result<SessionMeta, StoreError> {
        let session_id = SessionId::new();
        let dir = self.session_dir(&session_id)?;
        std::fs::create_dir_all(&dir).map_err(|source| StoreError::Io {
            path: dir.display().to_string(),
            source,
        })?;
        let meta = SessionMeta {
            session_id,
            workspace: workspace.to_owned(),
            title: title.to_owned(),
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            parent_session_id,
            branched_from_run_id,
        };
        write_json(&dir.join("meta.json"), &meta)?;
        Ok(meta)
    }

    fn delete_session(&self, session_id: &SessionId) -> Result<(), StoreError> {
        let dir = self.session_dir(session_id)?;
        if !dir.exists() {
            return Err(StoreError::NotFound(session_id.to_string()));
        }
        self.logs.lock().unwrap().remove(session_id);
        std::fs::remove_dir_all(&dir).map_err(|source| StoreError::Io {
            path: dir.display().to_string(),
            source,
        })?;
        Ok(())
    }

    fn list_sessions(&self) -> Result<Vec<SessionMeta>, StoreError> {
        let mut sessions = Vec::new();
        let entries = std::fs::read_dir(&self.root).map_err(|source| StoreError::Io {
            path: self.root.display().to_string(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| StoreError::Io {
                path: self.root.display().to_string(),
                source,
            })?;
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let meta_path = entry.path().join("meta.json");
            let Ok(content) = std::fs::read_to_string(&meta_path) else {
                continue;
            };
            if let Ok(meta) = serde_json::from_str::<SessionMeta>(&content) {
                sessions.push(meta);
            }
        }
        sessions.sort_by(|left, right| right.updated_at_ms.cmp(&left.updated_at_ms));
        Ok(sessions)
    }

    fn get_session(&self, session_id: &SessionId) -> Result<Option<SessionMeta>, StoreError> {
        let path = self.session_dir(session_id)?.join("meta.json");
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(StoreError::Io {
                    path: path.display().to_string(),
                    source,
                });
            }
        };
        let meta = serde_json::from_str(&content).map_err(|source| StoreError::Json {
            path: path.display().to_string(),
            source,
        })?;
        Ok(Some(meta))
    }

    fn append(
        &self,
        session_id: &SessionId,
        event: AgentEvent,
        timestamp_ms: i64,
    ) -> Result<EventEnvelope, StoreError> {
        Ok(self.log_for(session_id)?.append(event, timestamp_ms)?)
    }

    fn read_events(&self, session_id: &SessionId) -> Result<Vec<EventEnvelope>, StoreError> {
        Ok(self.log_for(session_id)?.read_all()?)
    }

    fn save_snapshot(&self, snapshot: &SessionSnapshot) -> Result<(), StoreError> {
        let dir = self.session_dir(&snapshot.session_id)?;
        write_json(&dir.join("snapshot.json"), snapshot)
    }

    fn load_snapshot(&self, session_id: &SessionId) -> Result<Option<SessionSnapshot>, StoreError> {
        let path = self.session_dir(session_id)?.join("snapshot.json");
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(StoreError::Io {
                    path: path.display().to_string(),
                    source,
                });
            }
        };
        let snapshot = serde_json::from_str(&content).map_err(|source| StoreError::Json {
            path: path.display().to_string(),
            source,
        })?;
        Ok(Some(snapshot))
    }

    fn touch_session(
        &self,
        session_id: &SessionId,
        now_ms: i64,
        title: Option<&str>,
    ) -> Result<(), StoreError> {
        let Some(mut meta) = self.get_session(session_id)? else {
            return Err(StoreError::NotFound(session_id.to_string()));
        };
        meta.updated_at_ms = now_ms;
        if let Some(title) = title
            && !title.trim().is_empty()
        {
            meta.title = title.chars().take(120).collect();
        }
        write_json(&self.session_dir(session_id)?.join("meta.json"), &meta)
    }
}

pub(crate) fn validate_id(id: &str) -> Result<(), StoreError> {
    if id.is_empty()
        || id.len() > 128
        || !id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
    {
        return Err(StoreError::InvalidId(id.to_owned()));
    }
    Ok(())
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), StoreError> {
    let temp = path.with_extension("json.tmp");
    let content = serde_json::to_vec_pretty(value).map_err(|source| StoreError::Json {
        path: path.display().to_string(),
        source,
    })?;
    std::fs::write(&temp, content).map_err(|source| StoreError::Io {
        path: temp.display().to_string(),
        source,
    })?;
    std::fs::rename(&temp, path).map_err(|source| StoreError::Io {
        path: path.display().to_string(),
        source,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nars_agent_ir::RunId;

    fn message(text: &str) -> AgentEvent {
        AgentEvent::AssistantMessage {
            run_id: RunId::from_static("run_1"),
            text: text.to_owned(),
        }
    }

    #[test]
    fn creates_lists_and_appends_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSessionStore::new(dir.path()).unwrap();
        let session = store.create_session("/tmp/ws", "demo", 10).unwrap();
        assert_eq!(store.list_sessions().unwrap().len(), 1);
        store
            .append(&session.session_id, message("hello"), 11)
            .unwrap();
        let events = store.read_events(&session.session_id).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].seq, 1);
    }

    #[test]
    fn persists_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        let session_id = {
            let store = FileSessionStore::new(dir.path()).unwrap();
            let session = store.create_session("/tmp/ws", "demo", 10).unwrap();
            store
                .append(&session.session_id, message("one"), 11)
                .unwrap();
            session.session_id
        };
        let store = FileSessionStore::new(dir.path()).unwrap();
        assert_eq!(store.read_events(&session_id).unwrap().len(), 1);
        store.append(&session_id, message("two"), 12).unwrap();
        assert_eq!(store.read_events(&session_id).unwrap().len(), 2);
    }

    #[test]
    fn snapshots_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSessionStore::new(dir.path()).unwrap();
        let session = store.create_session("/tmp/ws", "demo", 10).unwrap();
        let state = nars_agent_ir::RunState::new(
            RunId::from_static("run_1"),
            session.session_id.clone(),
            nars_agent_ir::TaskSpec::new(
                nars_agent_ir::GoalSpec::new(nars_agent_ir::GoalType::Other, "x"),
                "/tmp/ws",
                "x",
            ),
            10,
        );
        let snapshot = SessionSnapshot {
            version: 1,
            session_id: session.session_id.clone(),
            run_id: RunId::from_static("run_1"),
            state_hash: state.state_hash(),
            state,
            runtime: serde_json::json!({"facts": []}),
            last_seq: 3,
            created_at_ms: 12,
        };
        store.save_snapshot(&snapshot).unwrap();
        let loaded = store.load_snapshot(&session.session_id).unwrap().unwrap();
        assert_eq!(loaded.last_seq, 3);
        assert_eq!(loaded.state_hash, snapshot.state_hash);
    }

    #[test]
    fn rejects_path_traversal_in_ids() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSessionStore::new(dir.path()).unwrap();
        let id = SessionId::from_static("../escape");
        assert!(matches!(
            store.read_events(&id),
            Err(StoreError::InvalidId(_))
        ));
    }
}

#[cfg(test)]
mod lineage_tests {
    use super::*;
    use nars_agent_ir::RunId;

    #[test]
    fn file_store_records_lineage_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSessionStore::new(dir.path()).unwrap();
        let parent = store.create_session("/tmp/ws", "parent", 10).unwrap();
        let source = RunId::from_static("run_source");
        let branch = store
            .create_session_with_lineage(
                "/tmp/ws",
                "branch",
                Some(parent.session_id.clone()),
                Some(source.clone()),
                11,
            )
            .unwrap();
        let meta = store.get_session(&branch.session_id).unwrap().unwrap();
        assert_eq!(meta.parent_session_id, Some(parent.session_id.clone()));
        assert_eq!(meta.branched_from_run_id, Some(source));
        store.delete_session(&branch.session_id).unwrap();
        assert!(store.get_session(&branch.session_id).unwrap().is_none());
        assert_eq!(store.list_sessions().unwrap().len(), 1);
        assert!(matches!(
            store.delete_session(&branch.session_id),
            Err(StoreError::NotFound(_))
        ));
    }
}
