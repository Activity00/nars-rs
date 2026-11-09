//! Asynchronous driver that connects the session actor to providers and tools.

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};

use nars_agent_ir::{AgentError, AgentEvent, RunId, RunState, SessionId, TaskSpec, ToolCallId};
use nars_model::{ModelProvider, RetryConfig, complete_with_retry};
use nars_tools::ToolBroker;
use tokio::sync::{broadcast, mpsc, watch};

use crate::actor::{ActorConfig, ActorOutput, Command, SessionActor};
use crate::planner::PlannerSnapshot;

/// Observes runtime output. Implementations persist events, update API state,
/// or forward events to a UI.
pub trait RuntimeSink: Send + Sync {
    /// Called for every published event, in order.
    fn on_event(&self, event: &AgentEvent);
    /// Called after the actor state changes.
    fn on_state(&self, _state: &RunState) {}
    /// Called when the runtime requests a durable checkpoint.
    fn on_checkpoint(&self, _state: &RunState, _planner: &PlannerSnapshot) {}
    /// Called once when a run terminates, with its updated long-term memory.
    fn on_memory(&self, _snapshot: &nars_core::MemorySnapshot) {}
}

/// A sink that discards everything.
#[derive(Debug, Default)]
pub struct NoopSink;

impl RuntimeSink for NoopSink {
    fn on_event(&self, _event: &AgentEvent) {}
}

/// Driver configuration.
#[derive(Clone, Debug, Default)]
pub struct RuntimeConfig {
    /// Actor settings.
    pub actor: ActorConfig,
    /// Provider retry settings.
    pub retry: RetryConfig,
}

#[derive(Debug)]
enum Control {
    Pause(String),
    Resume,
    Cancel(String),
    Approve {
        call_id: ToolCallId,
        approved: bool,
        note: Option<String>,
    },
}

/// The entry point for starting runs.
#[derive(Clone)]
pub struct AgentRuntime {
    provider: Arc<dyn ModelProvider>,
    broker: Arc<ToolBroker>,
    sink: Arc<dyn RuntimeSink>,
    config: RuntimeConfig,
}

impl std::fmt::Debug for AgentRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentRuntime")
            .field("provider", &self.provider.name())
            .field("tools", &self.broker.names())
            .finish_non_exhaustive()
    }
}

impl AgentRuntime {
    /// Builds a runtime.
    #[must_use]
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        broker: Arc<ToolBroker>,
        sink: Arc<dyn RuntimeSink>,
        config: RuntimeConfig,
    ) -> Self {
        Self {
            provider,
            broker,
            sink,
            config,
        }
    }

    /// Starts one run and returns a handle to observe and control it.
    ///
    /// # Errors
    ///
    /// Returns an error when the NARS planner cannot be initialized.
    pub fn start(&self, session_id: SessionId, task: TaskSpec) -> Result<RunHandle, AgentError> {
        self.start_with_run_id(session_id, task, RunId::new())
    }

    /// Starts one run with a caller-supplied identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when the NARS planner cannot be initialized.
    pub fn start_with_run_id(
        &self,
        session_id: SessionId,
        task: TaskSpec,
        run_id: RunId,
    ) -> Result<RunHandle, AgentError> {
        let manifests = self.broker.manifests();
        let (actor, initial) = SessionActor::new_with_run_id(
            run_id,
            session_id,
            task,
            manifests,
            Arc::clone(&self.broker),
            self.config.actor.clone(),
        )?;
        Ok(self.launch(actor, initial))
    }

    /// Resumes a run from a durable checkpoint.
    ///
    /// # Errors
    ///
    /// Returns an error when the run is terminal or planner restoration fails.
    pub fn resume(
        &self,
        state: RunState,
        planner: &PlannerSnapshot,
    ) -> Result<RunHandle, AgentError> {
        let manifests = self.broker.manifests();
        let (actor, initial) = SessionActor::restore(
            state,
            planner,
            manifests,
            Arc::clone(&self.broker),
            self.config.actor.clone(),
        )?;
        Ok(self.launch(actor, initial))
    }

    fn launch(&self, actor: SessionActor, initial: ActorOutput) -> RunHandle {
        let run_id = actor.state().run_id.clone();
        let session_id = actor.state().session_id.clone();
        let state = Arc::new(RwLock::new(actor.state().clone()));
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = broadcast::channel(1_024);
        let (done_tx, done_rx) = watch::channel(false);
        let loop_state = Arc::clone(&state);
        let sink = Arc::clone(&self.sink);
        let provider = Arc::clone(&self.provider);
        let broker = Arc::clone(&self.broker);
        let retry = self.config.retry;
        tokio::spawn(async move {
            run_loop(
                actor, initial, provider, broker, sink, control_rx, loop_state, event_tx, retry,
            )
            .await;
            let _ = done_tx.send(true);
        });
        RunHandle {
            run_id,
            session_id,
            state,
            control: control_tx,
            events: event_rx,
            done: done_rx,
        }
    }
}

/// A handle to a running agent.
pub struct RunHandle {
    /// Run identifier.
    pub run_id: RunId,
    /// Session identifier.
    pub session_id: SessionId,
    state: Arc<RwLock<RunState>>,
    control: mpsc::UnboundedSender<Control>,
    events: broadcast::Receiver<AgentEvent>,
    done: watch::Receiver<bool>,
}

impl RunHandle {
    /// Snapshot of the current state.
    #[must_use]
    pub fn state(&self) -> RunState {
        self.state.read().unwrap().clone()
    }

    /// Shared state for API projections.
    #[must_use]
    pub fn state_handle(&self) -> Arc<RwLock<RunState>> {
        Arc::clone(&self.state)
    }

    /// Subscribes to the run's event broadcast.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.events.resubscribe()
    }

    /// Requests a pause.
    pub fn pause(&self, reason: impl Into<String>) -> bool {
        self.control.send(Control::Pause(reason.into())).is_ok()
    }

    /// Requests a resume.
    pub fn resume(&self) -> bool {
        self.control.send(Control::Resume).is_ok()
    }

    /// Requests cancellation.
    pub fn cancel(&self, reason: impl Into<String>) -> bool {
        self.control.send(Control::Cancel(reason.into())).is_ok()
    }

    /// Approves or rejects a pending tool call.
    pub fn approve(&self, call_id: ToolCallId, approved: bool, note: Option<String>) -> bool {
        self.control
            .send(Control::Approve {
                call_id,
                approved,
                note,
            })
            .is_ok()
    }

    /// Waits until the run terminates.
    pub async fn wait(&self) {
        let mut done = self.done.clone();
        while !*done.borrow() {
            if done.changed().await.is_err() {
                break;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_loop(
    mut actor: SessionActor,
    initial: ActorOutput,
    provider: Arc<dyn ModelProvider>,
    broker: Arc<ToolBroker>,
    sink: Arc<dyn RuntimeSink>,
    mut control_rx: mpsc::UnboundedReceiver<Control>,
    state: Arc<RwLock<RunState>>,
    event_tx: broadcast::Sender<AgentEvent>,
    retry: RetryConfig,
) {
    let mut queue: VecDeque<Command> = initial.commands.into();
    publish(&actor, &sink, &state, &event_tx, initial.events);

    loop {
        while let Some(command) = queue.pop_front() {
            while let Ok(control) = control_rx.try_recv() {
                let output = handle_control(&mut actor, control);
                queue.extend(output.commands);
                publish(&actor, &sink, &state, &event_tx, output.events);
            }
            if actor.is_paused() {
                queue.push_front(command);
                break;
            }
            match command {
                Command::CallModel {
                    request_id,
                    purpose: _,
                    step: _,
                    request,
                } => {
                    let outcome = complete_with_retry(provider.as_ref(), request, retry).await;
                    let output = match outcome {
                        Ok(response) => actor.on_model_ok(&request_id, response.ir, response.usage),
                        Err(error) => Ok(actor.on_model_err(&request_id, &error)),
                    };
                    let output = match output {
                        Ok(output) => output,
                        Err(error) => actor.on_internal_error(error),
                    };
                    queue.extend(output.commands);
                    publish(&actor, &sink, &state, &event_tx, output.events);
                }
                Command::ExecuteTool { call } => {
                    let result = broker.execute(call).await;
                    let output = match actor.on_tool_result(result) {
                        Ok(output) => output,
                        Err(error) => actor.on_internal_error(error),
                    };
                    queue.extend(output.commands);
                    publish(&actor, &sink, &state, &event_tx, output.events);
                }
                Command::Checkpoint => {
                    sink.on_checkpoint(actor.state(), &actor.planner_snapshot());
                }
                Command::Finish => {
                    sink.on_memory(&actor.memory_snapshot());
                    return;
                }
            }
        }

        if actor.is_finished() {
            sink.on_memory(&actor.memory_snapshot());
            return;
        }

        match control_rx.recv().await {
            Some(control) => {
                let output = handle_control(&mut actor, control);
                queue.extend(output.commands);
                publish(&actor, &sink, &state, &event_tx, output.events);
            }
            None => {
                sink.on_memory(&actor.memory_snapshot());
                return;
            }
        }
    }
}

fn handle_control(actor: &mut SessionActor, control: Control) -> ActorOutput {
    match control {
        Control::Pause(reason) => actor.pause(reason),
        Control::Resume => actor.resume(),
        Control::Cancel(reason) => actor.cancel(reason),
        Control::Approve {
            call_id,
            approved,
            note,
        } => match actor.on_approval(&call_id, approved, note) {
            Ok(output) => output,
            Err(error) => actor.on_internal_error(error),
        },
    }
}

fn publish(
    actor: &SessionActor,
    sink: &Arc<dyn RuntimeSink>,
    state: &Arc<RwLock<RunState>>,
    event_tx: &broadcast::Sender<AgentEvent>,
    events: Vec<AgentEvent>,
) {
    for event in events {
        sink.on_event(&event);
        let _ = event_tx.send(event);
    }
    let current = actor.state().clone();
    sink.on_state(&current);
    *state.write().unwrap() = current;
}
