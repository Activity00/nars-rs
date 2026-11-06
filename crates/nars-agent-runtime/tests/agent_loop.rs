use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use nars_agent_ir::{
    AgentEvent, AgentIr, CandidateAction, FinalAnswer, GoalSpec, GoalType, RunId, RunState,
    RunStatus, SessionId, TaskProposal, TaskSpec,
};
use nars_agent_runtime::{
    ActorConfig, AgentRuntime, PlannerSnapshot, RuntimeConfig, RuntimeSink, project,
};
use nars_model::{ModelError, ModelProvider, ModelRequest, ModelResponse, ScriptedProvider};
use nars_session::EventEnvelope;
use nars_tools::ToolBroker;
use nars_tools::policy::{ApprovalMode, Policy, PolicyConfig};
use serde_json::json;
use tempfile::TempDir;

#[derive(Default)]
struct CollectSink {
    events: Mutex<Vec<EventEnvelope>>,
    seq: AtomicU64,
}

impl RuntimeSink for CollectSink {
    fn on_event(&self, event: &AgentEvent) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.events.lock().unwrap().push(EventEnvelope {
            seq,
            timestamp_ms: i64::try_from(seq).unwrap_or(0) * 10,
            event: event.clone(),
        });
    }
}

fn proposal(tool: &str, arguments: serde_json::Value, confidence: f64) -> AgentIr {
    AgentIr::TaskProposal(TaskProposal {
        goal: GoalSpec::new(GoalType::ModifyCode, "rename alpha to beta").with_target("src/lib.rs"),
        constraints: vec!["keep the changes minimal".to_owned()],
        candidate_actions: vec![
            CandidateAction::new(tool)
                .with_arguments(arguments)
                .with_confidence(confidence),
        ],
        confidence: 0.9,
        notes: None,
        summary: None,
    })
}

fn final_answer() -> AgentIr {
    AgentIr::FinalAnswer(FinalAnswer {
        text: "Renamed `alpha` to `beta` in `src/lib.rs`.".to_owned(),
        success: true,
        artifacts: vec!["src/lib.rs".to_owned()],
    })
}

fn task(dir: &TempDir) -> TaskSpec {
    TaskSpec::new(
        GoalSpec::new(GoalType::ModifyCode, "rename alpha to beta").with_target("src/lib.rs"),
        dir.path().display().to_string(),
        "rename alpha to beta in src/lib.rs",
    )
}

fn runtime(
    dir: &TempDir,
    scripts: Vec<AgentIr>,
    mode: ApprovalMode,
) -> (AgentRuntime, Arc<ScriptedProvider>, Arc<CollectSink>) {
    let scripted = Arc::new(ScriptedProvider::new(scripts));
    let policy = Policy::new(PolicyConfig {
        mode,
        ..PolicyConfig::default()
    });
    let broker = Arc::new(ToolBroker::with_builtin_tools(dir.path(), policy).unwrap());
    let sink = Arc::new(CollectSink::default());
    let config = RuntimeConfig {
        actor: ActorConfig {
            model: "scripted".to_owned(),
            seed: 7,
            ..ActorConfig::default()
        },
        ..RuntimeConfig::default()
    };
    let runtime = AgentRuntime::new(
        Arc::clone(&scripted) as Arc<dyn ModelProvider>,
        broker,
        Arc::clone(&sink) as Arc<dyn RuntimeSink>,
        config,
    );
    (runtime, scripted, sink)
}

#[tokio::test(flavor = "multi_thread")]
async fn runs_read_patch_final_end_to_end_and_replays() {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "fn alpha() {}\n").unwrap();
    let (runtime, scripted, sink) = runtime(
        &dir,
        vec![
            proposal("read_file", json!({"path": "src/lib.rs"}), 0.9),
            proposal(
                "apply_patch",
                json!({"path": "src/lib.rs", "old_text": "alpha", "new_text": "beta"}),
                0.95,
            ),
            final_answer(),
        ],
        ApprovalMode::AutoApprove,
    );

    let handle = runtime
        .start(nars_agent_ir::SessionId::new(), task(&dir))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), handle.wait())
        .await
        .expect("run timed out");

    let state = handle.state();
    assert_eq!(state.status, RunStatus::Completed, "{:?}", state.error);
    assert_eq!(state.step, 2);
    assert_eq!(state.decisions.len(), 2);
    assert_eq!(state.observations.len(), 2);
    assert_eq!(state.tool_calls.len(), 2);
    assert!(state.messages.last().unwrap().text.contains("Renamed"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap(),
        "fn beta() {}\n"
    );
    assert_eq!(scripted.requests().len(), 3);
    assert_eq!(handle.state().status, RunStatus::Completed);

    let events = sink.events.lock().unwrap().clone();
    let projected = project(&events).unwrap();
    assert_eq!(projected.status, RunStatus::Completed);
    assert_eq!(
        projected.state_hash(),
        state.state_hash(),
        "replay diverged from live state"
    );
    assert_eq!(projected.messages.len(), state.messages.len());
}

#[tokio::test(flavor = "multi_thread")]
async fn waits_for_patch_approval_then_completes() {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "fn alpha() {}\n").unwrap();
    let (runtime, _scripted, sink) = runtime(
        &dir,
        vec![
            proposal(
                "apply_patch",
                json!({"path": "src/lib.rs", "old_text": "alpha", "new_text": "beta"}),
                0.95,
            ),
            final_answer(),
        ],
        ApprovalMode::Ask,
    );

    let handle = runtime
        .start(nars_agent_ir::SessionId::new(), task(&dir))
        .unwrap();

    let mut call_id = None;
    for _ in 0..200 {
        let state = handle.state();
        if state.status == RunStatus::AwaitingApproval {
            call_id = state.pending_approval.as_ref().map(|call| call.id.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let call_id = call_id.expect("approval was not requested");
    let events = sink.events.lock().unwrap().clone();
    assert!(events.iter().any(|envelope| matches!(
        &envelope.event,
        AgentEvent::ApprovalRequested { preview, .. } if preview.is_some()
    )));

    assert!(handle.approve(call_id, true, None));
    tokio::time::timeout(Duration::from_secs(15), handle.wait())
        .await
        .expect("run timed out after approval");

    assert_eq!(handle.state().status, RunStatus::Completed);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap(),
        "fn beta() {}\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn denied_patch_is_reported_and_the_run_continues() {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "fn alpha() {}\n").unwrap();
    let (runtime, _scripted, sink) = runtime(
        &dir,
        vec![
            proposal(
                "apply_patch",
                json!({"path": "src/lib.rs", "old_text": "alpha", "new_text": "beta"}),
                0.95,
            ),
            AgentIr::FinalAnswer(FinalAnswer {
                text: "The patch was denied; the file is unchanged.".to_owned(),
                success: false,
                artifacts: Vec::new(),
            }),
        ],
        ApprovalMode::Ask,
    );

    let handle = runtime
        .start(nars_agent_ir::SessionId::new(), task(&dir))
        .unwrap();
    let mut call_id = None;
    for _ in 0..200 {
        let state = handle.state();
        if state.status == RunStatus::AwaitingApproval {
            call_id = state.pending_approval.as_ref().map(|call| call.id.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(handle.approve(call_id.expect("approval request"), false, None));
    tokio::time::timeout(Duration::from_secs(15), handle.wait())
        .await
        .expect("run timed out after denial");

    let state = handle.state();
    assert_eq!(state.status, RunStatus::Completed);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap(),
        "fn alpha() {}\n"
    );
    assert!(
        state
            .observations
            .iter()
            .any(|observation| observation.outcome == nars_agent_ir::Outcome::Denied)
    );
    let events = sink.events.lock().unwrap().clone();
    let projected = project(&events).unwrap();
    assert_eq!(projected.state_hash(), state.state_hash());
}

struct DelayedProvider {
    inner: Arc<ScriptedProvider>,
    delay: Duration,
}

#[async_trait]
impl ModelProvider for DelayedProvider {
    fn name(&self) -> &str {
        "delayed-scripted"
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        tokio::time::sleep(self.delay).await;
        self.inner.complete(request).await
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pauses_and_resumes_between_steps() {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "fn alpha() {}\n").unwrap();
    let scripted = Arc::new(ScriptedProvider::new(vec![
        proposal("read_file", json!({"path": "src/lib.rs"}), 0.9),
        final_answer(),
    ]));
    let provider = Arc::new(DelayedProvider {
        inner: Arc::clone(&scripted),
        delay: Duration::from_millis(150),
    });
    let policy = Policy::new(PolicyConfig {
        mode: ApprovalMode::AutoApprove,
        ..PolicyConfig::default()
    });
    let broker = Arc::new(ToolBroker::with_builtin_tools(dir.path(), policy).unwrap());
    let sink = Arc::new(CollectSink::default());
    let runtime = AgentRuntime::new(
        provider as Arc<dyn ModelProvider>,
        broker,
        Arc::clone(&sink) as Arc<dyn RuntimeSink>,
        RuntimeConfig {
            actor: ActorConfig {
                model: "scripted".to_owned(),
                seed: 7,
                ..ActorConfig::default()
            },
            ..RuntimeConfig::default()
        },
    );

    let handle = runtime
        .start(nars_agent_ir::SessionId::new(), task(&dir))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(handle.pause("test pause"));
    for _ in 0..200 {
        if handle.state().status == RunStatus::Paused {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(handle.state().status, RunStatus::Paused);
    assert!(handle.resume());
    tokio::time::timeout(Duration::from_secs(15), handle.wait())
        .await
        .expect("run timed out after resume");
    assert_eq!(handle.state().status, RunStatus::Completed);
    assert_eq!(scripted.requests().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn resumes_from_a_durable_checkpoint() {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "fn alpha() {}\n").unwrap();
    let (runtime, scripted, sink) = runtime(&dir, vec![final_answer()], ApprovalMode::AutoApprove);

    let run_id = RunId::new();
    let mut state = RunState::new(run_id.clone(), SessionId::new(), task(&dir), 0);
    state.status = RunStatus::Planning;
    let planner = PlannerSnapshot {
        version: 1,
        seed: 7,
        run_key: run_id.short().to_owned(),
        ..PlannerSnapshot::default()
    };

    let handle = runtime.resume(state, &planner).unwrap();
    tokio::time::timeout(Duration::from_secs(10), handle.wait())
        .await
        .expect("resumed run timed out");
    assert_eq!(handle.state().status, RunStatus::Completed);
    assert_eq!(scripted.requests().len(), 1);
    let events = sink.events.lock().unwrap().clone();
    assert!(
        events
            .iter()
            .any(|envelope| matches!(envelope.event, AgentEvent::RunResumed { .. }))
    );
}
