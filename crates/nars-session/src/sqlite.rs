//! SQLite-backed session store.
//!
//! The public [`SessionStore`] trait is synchronous because the runtime sink
//! writes from blocking contexts. SQLite access is asynchronous through `sqlx`,
//! so this adapter owns a dedicated current-thread Tokio runtime on a worker
//! thread and exchanges jobs with it over a channel.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use nars_agent_ir::{AgentEvent, RunId, SessionId};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};

use crate::event_log::EventEnvelope;
use crate::snapshot::{SessionMeta, SessionSnapshot};
use crate::store::{SessionStore, StoreError, validate_id};

/// SQLite implementation of [`SessionStore`].
pub struct SqliteSessionStore {
    sender: Mutex<Option<Sender<Job>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    path: PathBuf,
}

impl std::fmt::Debug for SqliteSessionStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SqliteSessionStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

enum Job {
    CreateSession {
        workspace: String,
        title: String,
        parent_session_id: Option<SessionId>,
        branched_from_run_id: Option<RunId>,
        now_ms: i64,
        reply: Sender<Result<SessionMeta, StoreError>>,
    },
    DeleteSession {
        session_id: SessionId,
        reply: Sender<Result<(), StoreError>>,
    },
    ListSessions {
        reply: Sender<Result<Vec<SessionMeta>, StoreError>>,
    },
    GetSession {
        session_id: SessionId,
        reply: Sender<Result<Option<SessionMeta>, StoreError>>,
    },
    Append {
        session_id: SessionId,
        event: AgentEvent,
        timestamp_ms: i64,
        reply: Sender<Result<EventEnvelope, StoreError>>,
    },
    ReadEvents {
        session_id: SessionId,
        reply: Sender<Result<Vec<EventEnvelope>, StoreError>>,
    },
    SaveSnapshot {
        snapshot: SessionSnapshot,
        reply: Sender<Result<(), StoreError>>,
    },
    LoadSnapshot {
        session_id: SessionId,
        reply: Sender<Result<Option<SessionSnapshot>, StoreError>>,
    },
    TouchSession {
        session_id: SessionId,
        now_ms: i64,
        title: Option<String>,
        reply: Sender<Result<(), StoreError>>,
    },
}

impl SqliteSessionStore {
    /// Opens or creates a SQLite database and starts its worker thread.
    ///
    /// # Errors
    ///
    /// Returns an error when the parent directory cannot be created, the
    /// runtime cannot start, or the schema cannot be initialized.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        if path != Path::new(":memory:")
            && let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| StoreError::Io {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let (job_sender, job_receiver) = channel();
        let (ready_sender, ready_receiver) = channel();
        let worker_path = path.clone();
        let worker = std::thread::Builder::new()
            .name("nars-session-sqlite".to_owned())
            .spawn(move || run_worker(worker_path, job_receiver, ready_sender))
            .map_err(|error| StoreError::Database(format!("failed to start worker: {error}")))?;
        match ready_receiver.recv() {
            Ok(Ok(())) => Ok(Self {
                sender: Mutex::new(Some(job_sender)),
                worker: Mutex::new(Some(worker)),
                path,
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                let _ = worker.join();
                Err(StoreError::Database(
                    "SQLite worker exited during setup".to_owned(),
                ))
            }
        }
    }

    /// Database path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn request<T>(
        &self,
        build: impl FnOnce(Sender<Result<T, StoreError>>) -> Job,
    ) -> Result<T, StoreError> {
        let (reply_sender, reply_receiver) = channel();
        let sender = self
            .sender
            .lock()
            .unwrap()
            .as_ref()
            .cloned()
            .ok_or_else(|| StoreError::Database("store is closed".to_owned()))?;
        sender
            .send(build(reply_sender))
            .map_err(|_| StoreError::Database("SQLite worker is not running".to_owned()))?;
        reply_receiver
            .recv()
            .map_err(|_| StoreError::Database("SQLite worker did not reply".to_owned()))?
    }
}

impl Drop for SqliteSessionStore {
    fn drop(&mut self) {
        let _ = self.sender.lock().unwrap().take();
        if let Some(worker) = self.worker.lock().unwrap().take() {
            let _ = worker.join();
        }
    }
}

impl SessionStore for SqliteSessionStore {
    fn create_session_with_lineage(
        &self,
        workspace: &str,
        title: &str,
        parent_session_id: Option<SessionId>,
        branched_from_run_id: Option<RunId>,
        now_ms: i64,
    ) -> Result<SessionMeta, StoreError> {
        self.request(|reply| Job::CreateSession {
            workspace: workspace.to_owned(),
            title: title.to_owned(),
            parent_session_id,
            branched_from_run_id,
            now_ms,
            reply,
        })
    }

    fn delete_session(&self, session_id: &SessionId) -> Result<(), StoreError> {
        validate_id(session_id.as_str())?;
        self.request(|reply| Job::DeleteSession {
            session_id: session_id.clone(),
            reply,
        })
    }

    fn list_sessions(&self) -> Result<Vec<SessionMeta>, StoreError> {
        self.request(|reply| Job::ListSessions { reply })
    }

    fn get_session(&self, session_id: &SessionId) -> Result<Option<SessionMeta>, StoreError> {
        validate_id(session_id.as_str())?;
        self.request(|reply| Job::GetSession {
            session_id: session_id.clone(),
            reply,
        })
    }

    fn append(
        &self,
        session_id: &SessionId,
        event: AgentEvent,
        timestamp_ms: i64,
    ) -> Result<EventEnvelope, StoreError> {
        validate_id(session_id.as_str())?;
        self.request(|reply| Job::Append {
            session_id: session_id.clone(),
            event,
            timestamp_ms,
            reply,
        })
    }

    fn read_events(&self, session_id: &SessionId) -> Result<Vec<EventEnvelope>, StoreError> {
        validate_id(session_id.as_str())?;
        self.request(|reply| Job::ReadEvents {
            session_id: session_id.clone(),
            reply,
        })
    }

    fn save_snapshot(&self, snapshot: &SessionSnapshot) -> Result<(), StoreError> {
        validate_id(snapshot.session_id.as_str())?;
        self.request(|reply| Job::SaveSnapshot {
            snapshot: snapshot.clone(),
            reply,
        })
    }

    fn load_snapshot(&self, session_id: &SessionId) -> Result<Option<SessionSnapshot>, StoreError> {
        validate_id(session_id.as_str())?;
        self.request(|reply| Job::LoadSnapshot {
            session_id: session_id.clone(),
            reply,
        })
    }

    fn touch_session(
        &self,
        session_id: &SessionId,
        now_ms: i64,
        title: Option<&str>,
    ) -> Result<(), StoreError> {
        validate_id(session_id.as_str())?;
        self.request(|reply| Job::TouchSession {
            session_id: session_id.clone(),
            now_ms,
            title: title.map(str::to_owned),
            reply,
        })
    }
}

fn run_worker(path: PathBuf, receiver: Receiver<Job>, ready: Sender<Result<(), StoreError>>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(StoreError::Database(format!(
                "failed to build SQLite runtime: {error}"
            ))));
            return;
        }
    };
    let pool = match runtime.block_on(open_pool(&path)) {
        Ok(pool) => pool,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    if ready.send(Ok(())).is_err() {
        return;
    }
    while let Ok(job) = receiver.recv() {
        runtime.block_on(handle_job(&pool, job));
    }
}

async fn open_pool(path: &Path) -> Result<SqlitePool, StoreError> {
    let url = if path == Path::new(":memory:") {
        "sqlite::memory:".to_owned()
    } else {
        format!("sqlite://{}?mode=rwc", path.display())
    };
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .map_err(|error| StoreError::Database(error.to_string()))?;
    let _ = sqlx::query("PRAGMA journal_mode=WAL").execute(&pool).await;
    let _ = sqlx::query("PRAGMA foreign_keys=ON").execute(&pool).await;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sessions (
            session_id TEXT PRIMARY KEY,
            workspace TEXT NOT NULL,
            title TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            parent_session_id TEXT,
            branched_from_run_id TEXT
        )",
    )
    .execute(&pool)
    .await
    .map_err(|error| StoreError::Database(error.to_string()))?;
    let columns = sqlx::query("PRAGMA table_info(sessions)")
        .fetch_all(&pool)
        .await
        .map_err(|error| StoreError::Database(error.to_string()))?;
    let names = columns
        .iter()
        .filter_map(|row| row.try_get::<String, _>("name").ok())
        .collect::<std::collections::BTreeSet<_>>();
    if !names.contains("parent_session_id") {
        sqlx::query("ALTER TABLE sessions ADD COLUMN parent_session_id TEXT")
            .execute(&pool)
            .await
            .map_err(|error| StoreError::Database(error.to_string()))?;
    }
    if !names.contains("branched_from_run_id") {
        sqlx::query("ALTER TABLE sessions ADD COLUMN branched_from_run_id TEXT")
            .execute(&pool)
            .await
            .map_err(|error| StoreError::Database(error.to_string()))?;
    }
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS events (
            session_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            timestamp_ms INTEGER NOT NULL,
            event_json TEXT NOT NULL,
            PRIMARY KEY (session_id, seq)
        )",
    )
    .execute(&pool)
    .await
    .map_err(|error| StoreError::Database(error.to_string()))?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS snapshots (
            session_id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL,
            state_hash TEXT NOT NULL,
            last_seq INTEGER NOT NULL,
            created_at_ms INTEGER NOT NULL,
            snapshot_json TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .map_err(|error| StoreError::Database(error.to_string()))?;
    Ok(pool)
}

async fn handle_job(pool: &SqlitePool, job: Job) {
    match job {
        Job::CreateSession {
            workspace,
            title,
            parent_session_id,
            branched_from_run_id,
            now_ms,
            reply,
        } => {
            let _ = reply.send(
                create_session(
                    pool,
                    &workspace,
                    &title,
                    parent_session_id,
                    branched_from_run_id,
                    now_ms,
                )
                .await,
            );
        }
        Job::DeleteSession { session_id, reply } => {
            let _ = reply.send(delete_session(pool, &session_id).await);
        }
        Job::ListSessions { reply } => {
            let _ = reply.send(list_sessions(pool).await);
        }
        Job::GetSession { session_id, reply } => {
            let _ = reply.send(get_session(pool, &session_id).await);
        }
        Job::Append {
            session_id,
            event,
            timestamp_ms,
            reply,
        } => {
            let _ = reply.send(append(pool, &session_id, event, timestamp_ms).await);
        }
        Job::ReadEvents { session_id, reply } => {
            let _ = reply.send(read_events(pool, &session_id).await);
        }
        Job::SaveSnapshot { snapshot, reply } => {
            let _ = reply.send(save_snapshot(pool, &snapshot).await);
        }
        Job::LoadSnapshot { session_id, reply } => {
            let _ = reply.send(load_snapshot(pool, &session_id).await);
        }
        Job::TouchSession {
            session_id,
            now_ms,
            title,
            reply,
        } => {
            let _ = reply.send(touch_session(pool, &session_id, now_ms, title.as_deref()).await);
        }
    }
}

async fn create_session(
    pool: &SqlitePool,
    workspace: &str,
    title: &str,
    parent_session_id: Option<SessionId>,
    branched_from_run_id: Option<RunId>,
    now_ms: i64,
) -> Result<SessionMeta, StoreError> {
    let meta = SessionMeta {
        session_id: SessionId::new(),
        workspace: workspace.to_owned(),
        title: title.to_owned(),
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
        parent_session_id,
        branched_from_run_id,
    };
    sqlx::query(
        "INSERT INTO sessions
            (session_id, workspace, title, created_at_ms, updated_at_ms,
             parent_session_id, branched_from_run_id)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(meta.session_id.as_str())
    .bind(&meta.workspace)
    .bind(&meta.title)
    .bind(meta.created_at_ms)
    .bind(meta.updated_at_ms)
    .bind(meta.parent_session_id.as_ref().map(SessionId::as_str))
    .bind(meta.branched_from_run_id.as_ref().map(RunId::as_str))
    .execute(pool)
    .await
    .map_err(|error| StoreError::Database(error.to_string()))?;
    Ok(meta)
}

async fn delete_session(pool: &SqlitePool, session_id: &SessionId) -> Result<(), StoreError> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| StoreError::Database(error.to_string()))?;
    sqlx::query("DELETE FROM events WHERE session_id = ?")
        .bind(session_id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(|error| StoreError::Database(error.to_string()))?;
    sqlx::query("DELETE FROM snapshots WHERE session_id = ?")
        .bind(session_id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(|error| StoreError::Database(error.to_string()))?;
    let result = sqlx::query("DELETE FROM sessions WHERE session_id = ?")
        .bind(session_id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(|error| StoreError::Database(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| StoreError::Database(error.to_string()))?;
    if result.rows_affected() == 0 {
        return Err(StoreError::NotFound(session_id.to_string()));
    }
    Ok(())
}

async fn list_sessions(pool: &SqlitePool) -> Result<Vec<SessionMeta>, StoreError> {
    let rows = sqlx::query(
        "SELECT session_id, workspace, title, created_at_ms, updated_at_ms,
                parent_session_id, branched_from_run_id
         FROM sessions ORDER BY updated_at_ms DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(|error| StoreError::Database(error.to_string()))?;
    rows.iter().map(row_to_meta).collect()
}

async fn get_session(
    pool: &SqlitePool,
    session_id: &SessionId,
) -> Result<Option<SessionMeta>, StoreError> {
    let row = sqlx::query(
        "SELECT session_id, workspace, title, created_at_ms, updated_at_ms,
                parent_session_id, branched_from_run_id
         FROM sessions WHERE session_id = ?",
    )
    .bind(session_id.as_str())
    .fetch_optional(pool)
    .await
    .map_err(|error| StoreError::Database(error.to_string()))?;
    row.as_ref().map(row_to_meta).transpose()
}

fn row_to_meta(row: &SqliteRow) -> Result<SessionMeta, StoreError> {
    Ok(SessionMeta {
        session_id: SessionId::from_static(
            row.try_get::<String, _>("session_id")
                .map_err(|error| StoreError::Database(error.to_string()))?,
        ),
        workspace: row
            .try_get("workspace")
            .map_err(|error| StoreError::Database(error.to_string()))?,
        title: row
            .try_get("title")
            .map_err(|error| StoreError::Database(error.to_string()))?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|error| StoreError::Database(error.to_string()))?,
        updated_at_ms: row
            .try_get("updated_at_ms")
            .map_err(|error| StoreError::Database(error.to_string()))?,
        parent_session_id: row
            .try_get::<Option<String>, _>("parent_session_id")
            .map_err(|error| StoreError::Database(error.to_string()))?
            .map(SessionId::from_static),
        branched_from_run_id: row
            .try_get::<Option<String>, _>("branched_from_run_id")
            .map_err(|error| StoreError::Database(error.to_string()))?
            .map(RunId::from_static),
    })
}

async fn append(
    pool: &SqlitePool,
    session_id: &SessionId,
    event: AgentEvent,
    timestamp_ms: i64,
) -> Result<EventEnvelope, StoreError> {
    let event_json =
        serde_json::to_string(&event).map_err(|error| StoreError::Database(error.to_string()))?;
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| StoreError::Database(error.to_string()))?;
    let next: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE session_id = ?")
            .bind(session_id.as_str())
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| StoreError::Database(error.to_string()))?;
    sqlx::query(
        "INSERT INTO events (session_id, seq, timestamp_ms, event_json) VALUES (?, ?, ?, ?)",
    )
    .bind(session_id.as_str())
    .bind(next)
    .bind(timestamp_ms)
    .bind(&event_json)
    .execute(&mut *transaction)
    .await
    .map_err(|error| StoreError::Database(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| StoreError::Database(error.to_string()))?;
    Ok(EventEnvelope {
        seq: u64::try_from(next).unwrap_or(0),
        timestamp_ms,
        event,
    })
}

async fn read_events(
    pool: &SqlitePool,
    session_id: &SessionId,
) -> Result<Vec<EventEnvelope>, StoreError> {
    let rows = sqlx::query(
        "SELECT seq, timestamp_ms, event_json FROM events
         WHERE session_id = ? ORDER BY seq ASC",
    )
    .bind(session_id.as_str())
    .fetch_all(pool)
    .await
    .map_err(|error| StoreError::Database(error.to_string()))?;
    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        let seq: i64 = row
            .try_get("seq")
            .map_err(|error| StoreError::Database(error.to_string()))?;
        let timestamp_ms: i64 = row
            .try_get("timestamp_ms")
            .map_err(|error| StoreError::Database(error.to_string()))?;
        let event_json: String = row
            .try_get("event_json")
            .map_err(|error| StoreError::Database(error.to_string()))?;
        let event: AgentEvent = serde_json::from_str(&event_json)
            .map_err(|error| StoreError::Database(error.to_string()))?;
        events.push(EventEnvelope {
            seq: u64::try_from(seq).unwrap_or(0),
            timestamp_ms,
            event,
        });
    }
    Ok(events)
}

async fn save_snapshot(pool: &SqlitePool, snapshot: &SessionSnapshot) -> Result<(), StoreError> {
    let json =
        serde_json::to_string(snapshot).map_err(|error| StoreError::Database(error.to_string()))?;
    sqlx::query(
        "INSERT INTO snapshots (session_id, run_id, state_hash, last_seq, created_at_ms, snapshot_json)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(session_id) DO UPDATE SET
            run_id = excluded.run_id,
            state_hash = excluded.state_hash,
            last_seq = excluded.last_seq,
            created_at_ms = excluded.created_at_ms,
            snapshot_json = excluded.snapshot_json",
    )
    .bind(snapshot.session_id.as_str())
    .bind(snapshot.run_id.as_str())
    .bind(&snapshot.state_hash)
    .bind(i64::try_from(snapshot.last_seq).unwrap_or(i64::MAX))
    .bind(snapshot.created_at_ms)
    .bind(&json)
    .execute(pool)
    .await
    .map_err(|error| StoreError::Database(error.to_string()))?;
    Ok(())
}

async fn load_snapshot(
    pool: &SqlitePool,
    session_id: &SessionId,
) -> Result<Option<SessionSnapshot>, StoreError> {
    let row = sqlx::query("SELECT snapshot_json FROM snapshots WHERE session_id = ?")
        .bind(session_id.as_str())
        .fetch_optional(pool)
        .await
        .map_err(|error| StoreError::Database(error.to_string()))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let json: String = row
        .try_get("snapshot_json")
        .map_err(|error| StoreError::Database(error.to_string()))?;
    let snapshot =
        serde_json::from_str(&json).map_err(|error| StoreError::Database(error.to_string()))?;
    Ok(Some(snapshot))
}

async fn touch_session(
    pool: &SqlitePool,
    session_id: &SessionId,
    now_ms: i64,
    title: Option<&str>,
) -> Result<(), StoreError> {
    let result = sqlx::query(
        "UPDATE sessions SET updated_at_ms = ?, title = COALESCE(?, title) WHERE session_id = ?",
    )
    .bind(now_ms)
    .bind(title)
    .bind(session_id.as_str())
    .execute(pool)
    .await
    .map_err(|error| StoreError::Database(error.to_string()))?;
    if result.rows_affected() == 0 {
        return Err(StoreError::NotFound(session_id.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nars_agent_ir::{RunId, RunState, TaskSpec};

    fn message(text: &str) -> AgentEvent {
        AgentEvent::AssistantMessage {
            run_id: RunId::from_static("run_1"),
            text: text.to_owned(),
        }
    }

    fn temporary_store() -> (tempfile::TempDir, SqliteSessionStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteSessionStore::new(dir.path().join("sessions.db")).unwrap();
        (dir, store)
    }

    #[test]
    fn creates_lists_and_appends() {
        let (_dir, store) = temporary_store();
        let session = store.create_session("/tmp/ws", "sqlite", 10).unwrap();
        assert_eq!(store.list_sessions().unwrap().len(), 1);
        store
            .append(&session.session_id, message("one"), 11)
            .unwrap();
        store
            .append(&session.session_id, message("two"), 12)
            .unwrap();
        let events = store.read_events(&session.session_id).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 1);
        assert_eq!(events[1].seq, 2);
        store
            .touch_session(&session.session_id, 20, Some("renamed"))
            .unwrap();
        let meta = store.get_session(&session.session_id).unwrap().unwrap();
        assert_eq!(meta.title, "renamed");
        assert_eq!(meta.updated_at_ms, 20);
    }

    #[test]
    fn persists_across_store_instances() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let session_id = {
            let store = SqliteSessionStore::new(&path).unwrap();
            let session = store.create_session("/tmp/ws", "sqlite", 10).unwrap();
            store
                .append(&session.session_id, message("one"), 11)
                .unwrap();
            session.session_id
        };
        let store = SqliteSessionStore::new(&path).unwrap();
        assert_eq!(store.read_events(&session_id).unwrap().len(), 1);
        store.append(&session_id, message("two"), 12).unwrap();
        assert_eq!(store.read_events(&session_id).unwrap().len(), 2);
    }

    #[test]
    fn snapshots_round_trip() {
        let (_dir, store) = temporary_store();
        let session = store.create_session("/tmp/ws", "sqlite", 10).unwrap();
        let state = RunState::new(
            RunId::from_static("run_1"),
            session.session_id.clone(),
            TaskSpec::new(
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
            runtime: serde_json::json!({}),
            last_seq: 2,
            created_at_ms: 12,
        };
        store.save_snapshot(&snapshot).unwrap();
        let loaded = store.load_snapshot(&session.session_id).unwrap().unwrap();
        assert_eq!(loaded.last_seq, 2);
        assert_eq!(loaded.state_hash, snapshot.state_hash);
    }

    #[test]
    fn rejects_invalid_ids() {
        let (_dir, store) = temporary_store();
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
    fn sqlite_store_records_lineage_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteSessionStore::new(dir.path().join("lineage.db")).unwrap();
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
        assert_eq!(meta.parent_session_id, Some(parent.session_id));
        assert_eq!(meta.branched_from_run_id, Some(source));
        store.delete_session(&branch.session_id).unwrap();
        assert!(store.get_session(&branch.session_id).unwrap().is_none());
        assert_eq!(store.list_sessions().unwrap().len(), 1);
    }
}
