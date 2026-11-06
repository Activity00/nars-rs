//! Shared server state and the persistence sink attached to every run.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use nars_agent_ir::{
    AgentEvent, GoalSpec, GoalType, RunId, RunState, SessionId, TaskSpec, ToolCallId,
};
use nars_agent_runtime::{
    ActorConfig, AgentRuntime, PlannerSnapshot, RunHandle, RuntimeConfig, RuntimeSink,
};
use nars_model::ModelProvider;
use nars_session::{
    EventEnvelope, RunSummary, SessionMeta, SessionSnapshot, SessionStore, StoreError,
};
use nars_tools::ToolBroker;
use nars_tools::policy::{ApprovalMode, Policy, PolicyConfig};
use tokio::sync::broadcast;

use crate::error::ApiError;

/// Server configuration.
#[derive(Clone)]
pub struct ServerConfig {
    /// Provider label shown in the UI.
    pub provider_name: String,
    /// Optional bearer token. When set, every `/api` route except health requires it.
    pub token: Option<String>,
    /// Approval policy mode.
    pub approval_mode: ApprovalMode,
    /// Default step budget.
    pub max_steps: u32,
    /// Actor configuration.
    pub actor: ActorConfig,
    /// Provider retry configuration.
    pub retry: nars_model::RetryConfig,
    /// Optional directory of built web assets.
    pub static_dir: Option<PathBuf>,
    /// Optional restriction on session workspaces.
    pub workspace_root: Option<PathBuf>,
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerConfig")
            .field("provider", &self.provider_name)
            .field("approval_mode", &self.approval_mode)
            .field("max_steps", &self.max_steps)
            .field("static_dir", &self.static_dir)
            .finish_non_exhaustive()
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            provider_name: "echo".to_owned(),
            token: None,
            approval_mode: ApprovalMode::Ask,
            max_steps: 24,
            actor: ActorConfig::default(),
            retry: nars_model::RetryConfig::default(),
            static_dir: None,
            workspace_root: None,
        }
    }
}

/// Shared application state.
pub struct AppState {
    /// Durable session store.
    pub store: Arc<dyn SessionStore>,
    /// Global model provider.
    pub provider: Arc<dyn ModelProvider>,
    /// Live runs by identifier.
    runs: Mutex<HashMap<RunId, Arc<RunHandle>>>,
    /// Per-session event broadcasters.
    session_events: Mutex<HashMap<SessionId, broadcast::Sender<EventEnvelope>>>,
    /// Configuration.
    pub config: ServerConfig,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppState")
            .field("provider", &self.provider.name())
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl AppState {
    /// Builds shared state.
    #[must_use]
    pub fn new(
        store: Arc<dyn SessionStore>,
        provider: Arc<dyn ModelProvider>,
        config: ServerConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            provider,
            runs: Mutex::new(HashMap::new()),
            session_events: Mutex::new(HashMap::new()),
            config,
        })
    }

    /// Returns or creates the broadcaster for a session.
    #[must_use]
    pub fn session_sender(&self, session_id: &SessionId) -> broadcast::Sender<EventEnvelope> {
        let mut senders = self.session_events.lock().unwrap();
        senders
            .entry(session_id.clone())
            .or_insert_with(|| broadcast::channel(2_048).0)
            .clone()
    }

    /// Starts a run for a user message.
    ///
    /// # Errors
    ///
    /// Returns an error for missing sessions, invalid workspaces, or runtime
    /// initialization failures.
    pub fn start_run(
        &self,
        session_id: &SessionId,
        message: &str,
        max_steps: Option<u32>,
    ) -> Result<RunId, ApiError> {
        let meta = self
            .store
            .get_session(session_id)?
            .ok_or_else(|| ApiError::NotFound(format!("session `{session_id}` does not exist")))?;
        let workspace = self.validate_workspace(&meta.workspace)?;
        let policy = Policy::new(PolicyConfig {
            mode: self.config.approval_mode,
            ..PolicyConfig::default()
        });
        let broker = Arc::new(
            ToolBroker::with_builtin_tools(&workspace, policy)
                .map_err(|error| ApiError::Internal(error.to_string()))?,
        );
        let sender = self.session_sender(session_id);
        let sink = Arc::new(StoreSink::new(
            session_id.clone(),
            Arc::clone(&self.store),
            sender.clone(),
        ));

        let user_event = self.store.append(
            session_id,
            AgentEvent::UserMessage {
                session_id: session_id.clone(),
                run_id: None,
                text: message.to_owned(),
            },
            now_ms(),
        )?;
        let _ = sender.send(user_event);

        let task = TaskSpec {
            goal: GoalSpec::new(
                GoalType::Other,
                message.chars().take(500).collect::<String>(),
            ),
            constraints: Vec::new(),
            workspace: workspace.display().to_string(),
            user_message: message.to_owned(),
            max_steps: max_steps.unwrap_or(self.config.max_steps).clamp(1, 100),
        };
        let runtime = AgentRuntime::new(
            Arc::clone(&self.provider),
            broker,
            sink as Arc<dyn RuntimeSink>,
            RuntimeConfig {
                actor: self.config.actor.clone(),
                retry: self.config.retry,
            },
        );
        let handle = Arc::new(runtime.start(session_id.clone(), task)?);
        let run_id = handle.run_id.clone();
        self.runs.lock().unwrap().insert(run_id.clone(), handle);
        let title = message.lines().next().unwrap_or("agent run");
        self.store
            .touch_session(session_id, now_ms(), Some(title))?;
        Ok(run_id)
    }

    /// Resumes the latest durable checkpoint of a session.
    ///
    /// # Errors
    ///
    /// Returns an error when no checkpoint exists, the run is terminal, or the
    /// run is already live in this process.
    pub fn resume_run(&self, session_id: &SessionId) -> Result<RunId, ApiError> {
        let meta = self
            .store
            .get_session(session_id)?
            .ok_or_else(|| ApiError::NotFound(format!("session `{session_id}` does not exist")))?;
        let snapshot = self
            .store
            .load_snapshot(session_id)?
            .ok_or_else(|| ApiError::NotFound("session has no checkpoint".to_owned()))?;
        if snapshot.state.status.is_terminal() {
            return Err(ApiError::Conflict(
                "checkpoint belongs to a terminal run".to_owned(),
            ));
        }
        if self.run(&snapshot.run_id).is_some() {
            return Err(ApiError::Conflict("run is already live".to_owned()));
        }
        let planner: PlannerSnapshot = serde_json::from_value(snapshot.runtime)
            .map_err(|error| ApiError::Internal(format!("invalid planner snapshot: {error}")))?;
        let workspace = self.validate_workspace(&meta.workspace)?;
        let policy = Policy::new(PolicyConfig {
            mode: self.config.approval_mode,
            ..PolicyConfig::default()
        });
        let broker = Arc::new(
            ToolBroker::with_builtin_tools(&workspace, policy)
                .map_err(|error| ApiError::Internal(error.to_string()))?,
        );
        let sender = self.session_sender(session_id);
        let sink = Arc::new(StoreSink::new(
            session_id.clone(),
            Arc::clone(&self.store),
            sender,
        ));
        let runtime = AgentRuntime::new(
            Arc::clone(&self.provider),
            broker,
            sink as Arc<dyn RuntimeSink>,
            RuntimeConfig {
                actor: self.config.actor.clone(),
                retry: self.config.retry,
            },
        );
        let handle = Arc::new(runtime.resume(snapshot.state, &planner)?);
        let run_id = handle.run_id.clone();
        self.runs.lock().unwrap().insert(run_id.clone(), handle);
        Ok(run_id)
    }

    /// Looks up a live run.
    #[must_use]
    pub fn run(&self, run_id: &RunId) -> Option<Arc<RunHandle>> {
        self.runs.lock().unwrap().get(run_id).cloned()
    }

    /// Summaries of runs started in this process for a session.
    #[must_use]
    pub fn runs_for_session(&self, session_id: &SessionId) -> Vec<RunSummary> {
        let runs = self.runs.lock().unwrap();
        let mut summaries = runs
            .values()
            .filter(|run| &run.session_id == session_id)
            .map(|run| {
                let state = run.state();
                RunSummary {
                    run_id: state.run_id.clone(),
                    status: state.status.label().to_owned(),
                    goal: state.task.goal.description.clone(),
                    step: state.step,
                    created_at_ms: state.created_at_ms,
                    updated_at_ms: state.updated_at_ms,
                }
            })
            .collect::<Vec<_>>();
        summaries.sort_by(|left, right| right.created_at_ms.cmp(&left.created_at_ms));
        summaries
    }

    /// Finds the run that owns a pending tool call.
    #[must_use]
    pub fn run_for_call(&self, call_id: &ToolCallId) -> Option<Arc<RunHandle>> {
        let runs = self.runs.lock().unwrap();
        runs.values()
            .find(|run| {
                run.state()
                    .pending_approval
                    .as_ref()
                    .is_some_and(|call| &call.id == call_id)
            })
            .cloned()
    }

    /// Validates and canonicalizes a workspace path.
    ///
    /// # Errors
    ///
    /// Returns an error when the path is missing or outside the configured root.
    pub fn validate_workspace(&self, workspace: &str) -> Result<PathBuf, ApiError> {
        let path = Path::new(workspace);
        let canonical = path.canonicalize().map_err(|error| {
            ApiError::BadRequest(format!("invalid workspace `{workspace}`: {error}"))
        })?;
        if !canonical.is_dir() {
            return Err(ApiError::BadRequest(format!(
                "workspace `{workspace}` is not a directory"
            )));
        }
        if let Some(root) = &self.config.workspace_root {
            let root = root.canonicalize().map_err(|error| {
                ApiError::Internal(format!("invalid configured workspace root: {error}"))
            })?;
            if !canonical.starts_with(&root) {
                return Err(ApiError::BadRequest(format!(
                    "workspace `{workspace}` is outside the allowed root"
                )));
            }
        }
        Ok(canonical)
    }
}

/// Persists and broadcasts every runtime event.
pub struct StoreSink {
    session_id: SessionId,
    store: Arc<dyn SessionStore>,
    sender: broadcast::Sender<EventEnvelope>,
    last_seq: AtomicU64,
}

impl StoreSink {
    /// Builds a sink for one session.
    #[must_use]
    pub fn new(
        session_id: SessionId,
        store: Arc<dyn SessionStore>,
        sender: broadcast::Sender<EventEnvelope>,
    ) -> Self {
        Self {
            session_id,
            store,
            sender,
            last_seq: AtomicU64::new(0),
        }
    }
}

impl RuntimeSink for StoreSink {
    fn on_event(&self, event: &AgentEvent) {
        match self.store.append(&self.session_id, event.clone(), now_ms()) {
            Ok(envelope) => {
                self.last_seq.store(envelope.seq, Ordering::Relaxed);
                let _ = self.sender.send(envelope);
            }
            Err(error) => {
                tracing::error!(%error, "failed to append agent event");
            }
        }
    }

    fn on_checkpoint(&self, state: &RunState, planner: &PlannerSnapshot) {
        let snapshot = SessionSnapshot {
            version: 1,
            session_id: self.session_id.clone(),
            run_id: state.run_id.clone(),
            state: state.clone(),
            runtime: serde_json::to_value(planner).unwrap_or(serde_json::Value::Null),
            state_hash: state.state_hash(),
            last_seq: self.last_seq.load(Ordering::Relaxed),
            created_at_ms: now_ms(),
        };
        if let Err(error) = self.store.save_snapshot(&snapshot) {
            tracing::error!(%error, "failed to save session snapshot");
        }
    }
}

/// Creates session metadata and records the creation event.
///
/// # Errors
///
/// Returns an error for invalid workspaces or storage failures.
pub fn create_session(
    state: &AppState,
    workspace: &str,
    title: Option<&str>,
) -> Result<SessionMeta, ApiError> {
    let canonical = state.validate_workspace(workspace)?;
    let title = title
        .filter(|title| !title.trim().is_empty())
        .unwrap_or("New session");
    let meta = state
        .store
        .create_session(&canonical.display().to_string(), title, now_ms())?;
    let sender = state.session_sender(&meta.session_id);
    let envelope = state.store.append(
        &meta.session_id,
        AgentEvent::SessionCreated {
            session_id: meta.session_id.clone(),
            workspace: canonical.display().to_string(),
        },
        now_ms(),
    )?;
    let _ = sender.send(envelope);
    Ok(meta)
}

/// Current Unix time in milliseconds.
#[must_use]
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

/// Maps a store error to an API error.
pub fn store_error(error: StoreError) -> ApiError {
    error.into()
}
