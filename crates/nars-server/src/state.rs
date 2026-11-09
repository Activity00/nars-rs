//! Shared server state and the persistence sink attached to every run.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use nars_agent_ir::{
    AgentEvent, GoalSpec, GoalType, RunId, RunState, RunStatus, SessionId, TaskSpec, ToolCallId,
    ToolManifest,
};
use nars_agent_runtime::{
    ActorConfig, AgentRuntime, PlannerSnapshot, RunHandle, RuntimeConfig, RuntimeSink,
};
use nars_model::ModelProvider;
use nars_session::{
    EventEnvelope, Experience, ExperienceStore, RunSummary, SessionMeta, SessionSnapshot,
    SessionStore, StoreError, summarize_runs,
};
use nars_tools::policy::{ApprovalMode, Policy, PolicyConfig};
use nars_tools::{BrokerConfig, Sandbox, Tool, ToolBroker};
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
    /// Optional cross-run experience store.
    pub experience: Option<Arc<dyn ExperienceStore>>,
    /// Additional tools, typically MCP adapters.
    pub extra_tools: Vec<Arc<dyn Tool>>,
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerConfig")
            .field("provider", &self.provider_name)
            .field("approval_mode", &self.approval_mode)
            .field("max_steps", &self.max_steps)
            .field("static_dir", &self.static_dir)
            .field("extra_tools", &self.extra_tools.len())
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
            experience: None,
            extra_tools: Vec::new(),
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

    /// All tool manifests exposed by the API and the model prompt.
    #[must_use]
    pub fn tool_manifests(&self) -> Vec<ToolManifest> {
        let mut registry = nars_tools::builtin::builtin_registry();
        for tool in &self.config.extra_tools {
            if let Err(error) = registry.register(Arc::clone(tool)) {
                tracing::warn!(%error, "failed to register extra tool");
            }
        }
        registry.manifests()
    }

    fn broker_for(&self, workspace: &Path, policy: Policy) -> Result<Arc<ToolBroker>, ApiError> {
        let mut registry = nars_tools::builtin::builtin_registry();
        for tool in &self.config.extra_tools {
            registry
                .register(Arc::clone(tool))
                .map_err(|error| ApiError::Internal(error.to_string()))?;
        }
        let sandbox =
            Sandbox::new(workspace).map_err(|error| ApiError::Internal(error.to_string()))?;
        Ok(Arc::new(ToolBroker::new(
            registry,
            policy,
            sandbox,
            BrokerConfig::default(),
        )))
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
        let task = TaskSpec {
            goal: GoalSpec::new(
                GoalType::Other,
                message.chars().take(500).collect::<String>(),
            ),
            constraints: Vec::new(),
            workspace: String::new(),
            user_message: message.to_owned(),
            max_steps: max_steps.unwrap_or(self.config.max_steps).clamp(1, 100),
        };
        self.start_task(session_id, task)
    }

    fn start_task(&self, session_id: &SessionId, mut task: TaskSpec) -> Result<RunId, ApiError> {
        let meta = self
            .store
            .get_session(session_id)?
            .ok_or_else(|| ApiError::NotFound(format!("session `{session_id}` does not exist")))?;
        let workspace = self.validate_workspace(&meta.workspace)?;
        task.workspace = workspace.display().to_string();
        let policy = Policy::new(PolicyConfig {
            mode: self.config.approval_mode,
            ..PolicyConfig::default()
        });
        let broker = self.broker_for(&workspace, policy)?;
        let sender = self.session_sender(session_id);
        let workspace_text = workspace.display().to_string();
        let experience = self
            .config
            .experience
            .as_ref()
            .and_then(|store| store.load(&workspace_text).ok());
        let mut actor_config = self.config.actor.clone();
        actor_config.experience = experience;
        let sink = Arc::new(StoreSink::new(
            session_id.clone(),
            Arc::clone(&self.store),
            sender.clone(),
            self.config.experience.clone(),
            workspace_text,
        ));

        let run_id = RunId::new();
        let user_event = self.store.append(
            session_id,
            AgentEvent::UserMessage {
                session_id: session_id.clone(),
                run_id: Some(run_id.clone()),
                text: task.user_message.clone(),
            },
            now_ms(),
        )?;
        let _ = sender.send(user_event);

        let runtime = AgentRuntime::new(
            Arc::clone(&self.provider),
            broker,
            sink as Arc<dyn RuntimeSink>,
            RuntimeConfig {
                actor: actor_config,
                retry: self.config.retry,
            },
        );
        let handle = Arc::new(runtime.start_with_run_id(
            session_id.clone(),
            task.clone(),
            run_id.clone(),
        )?);
        self.runs.lock().unwrap().insert(run_id.clone(), handle);
        let title = task
            .user_message
            .lines()
            .next()
            .unwrap_or("agent run")
            .to_owned();
        self.store
            .touch_session(session_id, now_ms(), Some(&title))?;
        Ok(run_id)
    }

    /// Creates a new session that branches from the start of an existing run.
    ///
    /// Earlier runs are copied and their run identifiers are rewritten, so the
    /// branch has no collision with the parent session. The selected run itself
    /// is not copied; it is started again with its original task.
    ///
    /// # Errors
    ///
    /// Returns an error when the session or run is missing, the run is still
    /// live, or the run has no terminal event.
    pub fn branch_session(
        &self,
        session_id: &SessionId,
        source_run_id: &RunId,
    ) -> Result<(SessionMeta, RunId, usize), ApiError> {
        let meta = self
            .store
            .get_session(session_id)?
            .ok_or_else(|| ApiError::NotFound(format!("session `{session_id}` does not exist")))?;
        let events = self.store.read_events(session_id)?;
        let first_index = events
            .iter()
            .position(|envelope| envelope.event.run_id() == Some(source_run_id))
            .ok_or_else(|| {
                ApiError::NotFound(format!("run `{source_run_id}` does not exist in session"))
            })?;
        let started = events[..first_index]
            .iter()
            .find(|envelope| {
                matches!(
                    &envelope.event,
                    AgentEvent::RunStarted { run_id, .. } if run_id == source_run_id
                )
            })
            .or_else(|| {
                events.iter().find(|envelope| {
                    matches!(
                        &envelope.event,
                        AgentEvent::RunStarted { run_id, .. } if run_id == source_run_id
                    )
                })
            })
            .ok_or_else(|| {
                ApiError::NotFound(format!("run `{source_run_id}` has no start event"))
            })?;
        let AgentEvent::RunStarted { task, .. } = &started.event else {
            return Err(ApiError::Internal(
                "run start event has the wrong type".to_owned(),
            ));
        };
        let task = task.clone();
        if let Some(run) = self.run(source_run_id)
            && !run.state().status.is_terminal()
        {
            return Err(ApiError::Conflict(
                "cannot branch from a live run".to_owned(),
            ));
        }
        if self.run(source_run_id).is_none()
            && !events.iter().any(|envelope| {
                envelope.event.run_id() == Some(source_run_id)
                    && match &envelope.event {
                        AgentEvent::RunCompleted { .. }
                        | AgentEvent::RunFailed { .. }
                        | AgentEvent::RunCancelled { .. } => true,
                        _ => false,
                    }
            })
        {
            return Err(ApiError::Conflict(
                "run has no terminal event and cannot be branched".to_owned(),
            ));
        }

        let prefix = &events[..first_index];
        let mut mapping: BTreeMap<String, String> = BTreeMap::new();
        for envelope in prefix {
            if let Some(run_id) = envelope.event.run_id() {
                mapping
                    .entry(run_id.to_string())
                    .or_insert_with(|| RunId::new().to_string());
            }
        }
        let title = format!(
            "Branch · {}",
            meta.title.chars().take(48).collect::<String>()
        );
        let branch = self.store.create_session_with_lineage(
            &meta.workspace,
            &title,
            Some(session_id.clone()),
            Some(source_run_id.clone()),
            now_ms(),
        )?;
        mapping.insert(session_id.to_string(), branch.session_id.to_string());
        let mut copied = 0;
        for envelope in prefix {
            let event = rewrite_run_ids(&envelope.event, &mapping)?;
            self.store
                .append(&branch.session_id, event, envelope.timestamp_ms)?;
            copied += 1;
        }
        let new_run_id = self.start_task(&branch.session_id, task)?;
        self.store
            .touch_session(&branch.session_id, now_ms(), Some(&title))?;
        Ok((branch, new_run_id, copied))
    }

    /// Deletes a session and its in-memory run handles.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is missing or has a live run.
    pub fn delete_session(&self, session_id: &SessionId) -> Result<(), ApiError> {
        let has_live = {
            let runs = self.runs.lock().unwrap();
            runs.values()
                .any(|run| &run.session_id == session_id && !run.state().status.is_terminal())
        };
        if has_live {
            return Err(ApiError::Conflict(
                "cannot delete a session with a live run".to_owned(),
            ));
        }
        let has_children = self
            .store
            .list_sessions()?
            .iter()
            .any(|session| session.parent_session_id.as_ref() == Some(session_id));
        if has_children {
            return Err(ApiError::Conflict(
                "delete branch sessions before deleting their parent".to_owned(),
            ));
        }
        self.store.delete_session(session_id)?;
        self.runs
            .lock()
            .unwrap()
            .retain(|_, run| &run.session_id != session_id);
        Ok(())
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
        let broker = self.broker_for(&workspace, policy)?;
        let sender = self.session_sender(session_id);
        let workspace_text = workspace.display().to_string();
        let experience = self
            .config
            .experience
            .as_ref()
            .and_then(|store| store.load(&workspace_text).ok());
        let mut actor_config = self.config.actor.clone();
        actor_config.experience = experience;
        let sink = Arc::new(StoreSink::new(
            session_id.clone(),
            Arc::clone(&self.store),
            sender,
            self.config.experience.clone(),
            workspace_text,
        ));
        let runtime = AgentRuntime::new(
            Arc::clone(&self.provider),
            broker,
            sink as Arc<dyn RuntimeSink>,
            RuntimeConfig {
                actor: actor_config,
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

    /// Summaries of live and historical runs for a session.
    #[must_use]
    pub fn runs_for_session(&self, session_id: &SessionId) -> Vec<RunSummary> {
        let live = {
            let runs = self.runs.lock().unwrap();
            runs.values()
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
                .collect::<Vec<_>>()
        };
        let mut summaries = std::collections::BTreeMap::new();
        match self.store.read_events(session_id) {
            Ok(events) => {
                for summary in summarize_runs(&events) {
                    summaries.insert(summary.run_id.clone(), summary);
                }
            }
            Err(error) => tracing::warn!(%error, "failed to read runs for session"),
        }
        for summary in live {
            summaries.insert(summary.run_id.clone(), summary);
        }
        let mut summaries = summaries.into_values().collect::<Vec<_>>();
        summaries.sort_by(|left, right| right.updated_at_ms.cmp(&left.updated_at_ms));
        summaries
    }

    /// Projects a historical run from the event log.
    ///
    /// # Errors
    ///
    /// Returns an error when sessions or events cannot be read.
    pub fn persisted_run(&self, run_id: &RunId) -> Result<Option<RunState>, StoreError> {
        for session in self.store.list_sessions()? {
            let events = self.store.read_events(&session.session_id)?;
            if !events
                .iter()
                .any(|envelope| envelope.event.run_id() == Some(run_id))
            {
                continue;
            }
            let filtered = events
                .into_iter()
                .filter(|envelope| match &envelope.event {
                    AgentEvent::RunStarted {
                        run_id: started, ..
                    } => started == run_id,
                    event => event.run_id() == Some(run_id),
                })
                .collect::<Vec<_>>();
            if let Ok(state) = nars_agent_runtime::project(&filtered)
                && &state.run_id == run_id
            {
                return Ok(Some(state));
            }
        }
        Ok(None)
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
    experience: Option<Arc<dyn ExperienceStore>>,
    workspace: String,
    task_counted: std::sync::atomic::AtomicBool,
}

impl StoreSink {
    /// Builds a sink for one session.
    #[must_use]
    pub fn new(
        session_id: SessionId,
        store: Arc<dyn SessionStore>,
        sender: broadcast::Sender<EventEnvelope>,
        experience: Option<Arc<dyn ExperienceStore>>,
        workspace: String,
    ) -> Self {
        Self {
            session_id,
            store,
            sender,
            last_seq: AtomicU64::new(0),
            experience,
            workspace,
            task_counted: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn persist_experience(&self, state: &RunState, planner: &PlannerSnapshot) {
        let Some(store) = &self.experience else {
            return;
        };
        let mut experience = store
            .load(&self.workspace)
            .unwrap_or_else(|_| Experience::new(&self.workspace));
        experience.workspace = self.workspace.clone();
        for (tool, stat) in &planner.tool_stats {
            let entry = experience.tools.entry(tool.clone()).or_default();
            if stat.attempts >= entry.attempts {
                entry.attempts = stat.attempts;
                entry.successes = stat.successes;
                entry.failures = stat.failures;
            }
        }
        for (signature, stat) in &planner.signature_stats {
            let entry = experience.signatures.entry(signature.clone()).or_default();
            if stat.attempts >= entry.attempts {
                entry.attempts = stat.attempts;
                entry.successes = stat.successes;
                entry.failures = stat.failures;
            }
        }
        if state.status.is_terminal()
            && !self
                .task_counted
                .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let success = state.status == RunStatus::Completed && state.error.is_none();
            let goal_type = planner.task_goal_type.unwrap_or(state.task.goal.goal_type);
            experience.record_task(goal_type.atom_suffix(), success);
        }
        experience.updated_at_ms = now_ms();
        if let Err(error) = store.save(&experience) {
            tracing::warn!(%error, "failed to persist workspace experience");
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
        self.persist_experience(state, planner);
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
pub fn rewrite_run_ids(
    event: &AgentEvent,
    mapping: &BTreeMap<String, String>,
) -> Result<AgentEvent, ApiError> {
    let mut value = serde_json::to_value(event)
        .map_err(|error| ApiError::Internal(format!("failed to encode event: {error}")))?;
    rewrite_strings(&mut value, mapping);
    serde_json::from_value(value)
        .map_err(|error| ApiError::Internal(format!("failed to decode event: {error}")))
}

fn rewrite_strings(value: &mut serde_json::Value, mapping: &BTreeMap<String, String>) {
    match value {
        serde_json::Value::String(text) => {
            if let Some(replacement) = mapping.get(text) {
                *text = replacement.clone();
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                rewrite_strings(item, mapping);
            }
        }
        serde_json::Value::Object(fields) => {
            for item in fields.values_mut() {
                rewrite_strings(item, mapping);
            }
        }
        _ => {}
    }
}

pub(crate) fn now_ms() -> i64 {
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
