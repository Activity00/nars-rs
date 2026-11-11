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

fn write_crate(dir: &TempDir) {
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "fn alpha() {}\n").unwrap();
}

fn task_other(dir: &TempDir) -> TaskSpec {
    TaskSpec::new(
        GoalSpec::new(GoalType::Other, "resume a checkpoint"),
        dir.path().display().to_string(),
        "resume a checkpoint",
    )
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
    write_crate(&dir);
    let (runtime, scripted, sink) = runtime(
        &dir,
        vec![
            proposal("read_file", json!({"path": "src/lib.rs"}), 0.9),
            proposal(
                "apply_patch",
                json!({"path": "src/lib.rs", "old_text": "alpha", "new_text": "beta"}),
                0.95,
            ),
            proposal("cargo_check", json!({}), 0.9),
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
    assert_eq!(state.step, 3);
    assert_eq!(state.decisions.len(), 3);
    assert_eq!(state.observations.len(), 3);
    assert_eq!(state.tool_calls.len(), 3);
    assert!(state.messages.last().unwrap().text.contains("Renamed"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap(),
        "fn beta() {}\n"
    );
    assert_eq!(scripted.requests().len(), 4);
    let final_plan = state
        .decisions
        .last()
        .and_then(|decision| decision.plan.as_ref())
        .expect("decision carries a plan view");
    assert_eq!(final_plan.active.as_deref(), Some("verify"));
    assert!(
        final_plan
            .stages
            .iter()
            .any(|stage| stage.id == "change" && stage.status == "satisfied")
    );
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
    write_crate(&dir);
    let (runtime, _scripted, sink) = runtime(
        &dir,
        vec![
            proposal("read_file", json!({"path": "src/lib.rs"}), 0.9),
            proposal(
                "apply_patch",
                json!({"path": "src/lib.rs", "old_text": "alpha", "new_text": "beta"}),
                0.95,
            ),
            proposal("cargo_check", json!({}), 0.9),
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
    write_crate(&dir);
    let (runtime, _scripted, sink) = runtime(
        &dir,
        vec![
            proposal("read_file", json!({"path": "src/lib.rs"}), 0.9),
            proposal(
                "apply_patch",
                json!({"path": "src/lib.rs", "old_text": "alpha", "new_text": "beta"}),
                0.95,
            ),
            AgentIr::Clarification(nars_agent_ir::Clarification {
                question: "The patch was denied; the file is unchanged. How should I proceed?"
                    .to_owned(),
                options: vec!["stop".to_owned()],
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
    write_crate(&dir);
    let scripted = Arc::new(ScriptedProvider::new(vec![
        proposal("read_file", json!({"path": "src/lib.rs"}), 0.9),
        proposal(
            "apply_patch",
            json!({"path": "src/lib.rs", "old_text": "alpha", "new_text": "beta"}),
            0.95,
        ),
        proposal("cargo_check", json!({}), 0.9),
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
    assert_eq!(scripted.requests().len(), 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn resumes_from_a_durable_checkpoint() {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "fn alpha() {}\n").unwrap();
    let (runtime, scripted, sink) = runtime(&dir, vec![final_answer()], ApprovalMode::AutoApprove);

    let run_id = RunId::new();
    let mut state = RunState::new(run_id.clone(), SessionId::new(), task_other(&dir), 0);
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

#[tokio::test(flavor = "multi_thread")]
async fn plan_overrides_model_order_and_premature_finish() {
    let dir = TempDir::new().unwrap();
    write_crate(&dir);
    let (runtime, scripted, sink) = runtime(
        &dir,
        vec![
            // The model wants to patch before investigating. The plan defers
            // the patch and supplies safe read-only fallbacks.
            proposal(
                "apply_patch",
                json!({"path": "src/lib.rs", "old_text": "alpha", "new_text": "beta"}),
                0.99,
            ),
            // Still locked: listing alone does not satisfy investigation.
            proposal(
                "apply_patch",
                json!({"path": "src/lib.rs", "old_text": "alpha", "new_text": "beta"}),
                0.99,
            ),
            // After read_file, investigation is satisfied and the patch is admissible.
            proposal(
                "apply_patch",
                json!({"path": "src/lib.rs", "old_text": "alpha", "new_text": "beta"}),
                0.95,
            ),
            // Premature finish: the plan requires verification first.
            final_answer(),
            proposal("cargo_check", json!({}), 0.9),
            final_answer(),
        ],
        ApprovalMode::AutoApprove,
    );

    let handle = runtime
        .start(nars_agent_ir::SessionId::new(), task(&dir))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), handle.wait())
        .await
        .expect("run timed out");

    let state = handle.state();
    assert_eq!(state.status, RunStatus::Completed, "{:?}", state.error);
    let tools = state
        .tool_calls
        .iter()
        .map(|call| call.tool.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        tools,
        ["list_files", "read_file", "apply_patch", "cargo_check"]
    );
    assert_eq!(scripted.requests().len(), 6);
    assert!(
        state.decisions[0]
            .rationale
            .contains("deferred 1 candidate(s) outside stage `investigate`"),
        "{:?}",
        state.decisions[0].rationale
    );
    let events = sink.events.lock().unwrap();
    let plan = events
        .iter()
        .rev()
        .find_map(|envelope| match &envelope.event {
            AgentEvent::ObservationRecorded { plan, .. } => plan.clone(),
            _ => None,
        })
        .expect("observation carries a plan view");
    assert_eq!(plan.active.as_deref(), Some("answer"), "{plan:?}");
    for stage in ["investigate", "change", "verify"] {
        assert_eq!(
            plan.stages
                .iter()
                .find(|candidate| candidate.id == stage)
                .unwrap()
                .status,
            "satisfied",
            "{stage}"
        );
    }
}

fn write_failing_crate(dir: &TempDir) {
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"failing_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "// fixture\n\
         pub fn value() -> i32 { 1 }\n\
         \n\
         #[cfg(test)]\n\
         mod tests {\n\
             #[test]\n\
             fn it_works() { assert_eq!(super::value(), 2); }\n\
         }\n",
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn verification_failure_returns_to_change_and_switches_tool() {
    let dir = TempDir::new().unwrap();
    write_failing_crate(&dir);
    let (runtime, _scripted, _sink) = runtime(
        &dir,
        vec![
            proposal("read_file", json!({"path": "src/lib.rs"}), 0.9),
            proposal(
                "apply_patch",
                json!({"path": "src/lib.rs", "old_text": "// fixture", "new_text": "// fixture updated"}),
                0.9,
            ),
            proposal("cargo_test", json!({}), 0.9),
            proposal(
                "apply_patch",
                json!({"path": "src/lib.rs", "old_text": "pub fn value() -> i32 { 1 }", "new_text": "pub fn value() -> i32 { 2 }"}),
                0.95,
            ),
            proposal("cargo_test", json!({}), 0.9),
            final_answer(),
        ],
        ApprovalMode::AutoApprove,
    );

    let handle = runtime
        .start(nars_agent_ir::SessionId::new(), task(&dir))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), handle.wait())
        .await
        .expect("run timed out");

    let state = handle.state();
    assert_eq!(state.status, RunStatus::Completed, "{:?}", state.error);
    let tools = state
        .tool_calls
        .iter()
        .map(|call| call.tool.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        tools,
        [
            "read_file",
            "apply_patch",
            "cargo_test",
            "apply_patch",
            "cargo_check"
        ]
    );
    assert_eq!(
        state
            .tool_results
            .iter()
            .map(|result| result.status)
            .collect::<Vec<_>>(),
        [
            nars_agent_ir::ToolStatus::Succeeded,
            nars_agent_ir::ToolStatus::Succeeded,
            nars_agent_ir::ToolStatus::Failed,
            nars_agent_ir::ToolStatus::Succeeded,
            nars_agent_ir::ToolStatus::Succeeded,
        ]
    );
    let repair_decision = &state.decisions[3];
    assert_eq!(
        repair_decision
            .plan
            .as_ref()
            .and_then(|plan| plan.active.as_deref()),
        Some("change"),
        "verification failure should return to the change stage"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap(),
        "// fixture updated\n\
         pub fn value() -> i32 { 2 }\n\
         \n\
         #[cfg(test)]\n\
         mod tests {\n\
             #[test]\n\
             fn it_works() { assert_eq!(super::value(), 2); }\n\
         }\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn generic_run_command_verifies_a_non_rust_workspace() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("app.js"), "const value = 'alpha';\n").unwrap();
    let (runtime, _scripted, _sink) = runtime(
        &dir,
        vec![
            proposal("read_file", json!({"path": "app.js"}), 0.9),
            proposal(
                "apply_patch",
                json!({"path": "app.js", "old_text": "alpha", "new_text": "beta"}),
                0.95,
            ),
            proposal("run_command", json!({"command": "printf verified"}), 0.9),
            final_answer(),
        ],
        ApprovalMode::AutoApprove,
    );
    let mut spec = task(&dir);
    spec.goal.target = Some("app.js".to_owned());
    spec.verification = vec![nars_agent_ir::VerificationCommand {
        label: "syntax check".to_owned(),
        command: "printf verified".to_owned(),
        cwd: None,
        timeout_ms: Some(5_000),
    }];
    let handle = runtime.start(SessionId::new(), spec).unwrap();
    tokio::time::timeout(Duration::from_secs(15), handle.wait())
        .await
        .expect("run timed out");
    let state = handle.state();
    assert_eq!(state.status, RunStatus::Completed, "{:?}", state.error);
    assert_eq!(state.tool_calls.len(), 3);
    assert!(
        state
            .tool_calls
            .iter()
            .any(|call| call.tool == "run_command")
    );
    assert!(
        state
            .observations
            .iter()
            .any(|observation| observation.tool == "run_command")
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("app.js")).unwrap(),
        "const value = 'beta';\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_when_the_model_replies_without_json() {
    struct Flaky {
        calls: AtomicU64,
        saw_repair: Mutex<bool>,
    }

    #[async_trait]
    impl ModelProvider for Flaky {
        fn name(&self) -> &str {
            "flaky"
        }

        async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                return Err(ModelError::invalid_output(
                    "no JSON object found in response",
                ));
            }
            if request
                .messages
                .iter()
                .any(|message| message.content.contains("was not a JSON object"))
            {
                *self.saw_repair.lock().unwrap() = true;
            }
            Ok(ModelResponse {
                ir: AgentIr::FinalAnswer(FinalAnswer {
                    text: "ok".to_owned(),
                    success: true,
                    artifacts: Vec::new(),
                }),
                usage: Default::default(),
                raw: String::new(),
                provider: "flaky".to_owned(),
                latency_ms: 0,
            })
        }
    }

    let workspace = TempDir::new().unwrap();
    write_crate(&workspace);
    let broker = Arc::new(
        ToolBroker::with_builtin_tools(
            workspace.path(),
            Policy::new(PolicyConfig {
                mode: ApprovalMode::AutoApprove,
                ..PolicyConfig::default()
            }),
        )
        .unwrap(),
    );
    let provider = Arc::new(Flaky {
        calls: AtomicU64::new(0),
        saw_repair: Mutex::new(false),
    });
    let runtime = AgentRuntime::new(
        Arc::clone(&provider) as Arc<dyn ModelProvider>,
        broker,
        Arc::new(CollectSink::default()),
        RuntimeConfig {
            actor: ActorConfig {
                model: "flaky".to_owned(),
                seed: 7,
                ..ActorConfig::default()
            },
            ..RuntimeConfig::default()
        },
    );
    let task = TaskSpec::new(
        GoalSpec::new(GoalType::Investigate, "say hi"),
        workspace.path().display().to_string(),
        "say hi",
    );
    let handle = runtime.start(SessionId::new(), task).unwrap();
    handle.wait().await;

    assert_eq!(handle.state().status, RunStatus::Completed);
    assert!(provider.calls.load(Ordering::SeqCst) >= 2);
    assert!(*provider.saw_repair.lock().unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn nars_derives_action_from_a_translated_goal() {
    let dir = TempDir::new().unwrap();
    write_crate(&dir);
    std::fs::write(dir.path().join("alpha"), "alpha\n").unwrap();

    let mut proposal_ir = proposal("list_files", json!({"path": "."}), 0.9);
    if let AgentIr::TaskProposal(ref mut proposal) = proposal_ir {
        proposal.goal =
            GoalSpec::new(GoalType::Other, "read alpha").with_narsese("(read_file * alpha)");
        proposal.candidate_actions.clear();
    }
    let (runtime, _scripted, _sink) = runtime(
        &dir,
        vec![proposal_ir, final_answer()],
        ApprovalMode::AutoApprove,
    );
    let task = TaskSpec::new(
        GoalSpec::new(GoalType::Other, "read alpha"),
        dir.path().display().to_string(),
        "read alpha",
    );
    let handle = runtime.start(SessionId::new(), task).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(30), handle.wait()).await;

    let state = handle.state();
    assert_eq!(state.status, RunStatus::Completed, "{:?}", state.error);
    let tools = state
        .tool_calls
        .iter()
        .map(|call| call.tool.as_str())
        .collect::<Vec<_>>();
    assert_eq!(tools, ["read_file"]);
    assert_eq!(state.tool_calls[0].arguments, json!({"path": "alpha"}));
    assert!(
        state.decisions[0]
            .selected
            .as_ref()
            .is_some_and(|selected| !selected.model_proposed),
        "the action should be NARS-derived, not model-proposed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_question_is_answered_without_tool_work() {
    let dir = TempDir::new().unwrap();
    write_crate(&dir);
    let mut proposal_ir = proposal("list_files", json!({"path": "."}), 0.9);
    if let AgentIr::TaskProposal(ref mut proposal) = proposal_ir {
        proposal.goal = GoalSpec::new(GoalType::AnswerQuestion, "what is this project?");
        proposal.candidate_actions.clear();
    }
    let (runtime, scripted, _sink) = runtime(
        &dir,
        vec![proposal_ir, final_answer(), final_answer(), final_answer()],
        ApprovalMode::AutoApprove,
    );
    let task = TaskSpec::new(
        GoalSpec::new(GoalType::AnswerQuestion, "what is this project?"),
        dir.path().display().to_string(),
        "what is this project?",
    );
    let handle = runtime.start(SessionId::new(), task).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(30), handle.wait()).await;

    let state = handle.state();
    assert_eq!(state.status, RunStatus::Completed, "{:?}", state.error);
    assert!(
        state.tool_calls.is_empty(),
        "a question should not require tools: {:?}",
        state
            .tool_calls
            .iter()
            .map(|call| call.tool.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        scripted.requests().len() <= 3,
        "{}",
        scripted.requests().len()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_question_that_needs_evidence_reads_before_answering() {
    let dir = TempDir::new().unwrap();
    write_crate(&dir);
    std::fs::write(dir.path().join("README.md"), "# fixture\n").unwrap();

    let mut proposal_ir = proposal("read_file", json!({"path": "README.md"}), 0.9);
    if let AgentIr::TaskProposal(ref mut proposal) = proposal_ir {
        proposal.goal = GoalSpec::new(GoalType::AnswerQuestion, "summarize the README");
    }
    let (runtime, _scripted, _sink) = runtime(
        &dir,
        vec![proposal_ir, final_answer(), final_answer(), final_answer()],
        ApprovalMode::AutoApprove,
    );
    let task = TaskSpec::new(
        GoalSpec::new(GoalType::AnswerQuestion, "summarize the README"),
        dir.path().display().to_string(),
        "summarize the README",
    );
    let handle = runtime.start(SessionId::new(), task).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(30), handle.wait()).await;

    let state = handle.state();
    assert_eq!(state.status, RunStatus::Completed, "{:?}", state.error);
    let tools = state
        .tool_calls
        .iter()
        .map(|call| call.tool.as_str())
        .collect::<Vec<_>>();
    assert_eq!(tools, ["read_file"]);
}
