use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use nars_agent_ir::{
    AgentEvent, AgentIr, CandidateAction, FinalAnswer, GoalSpec, GoalType, SessionId, TaskProposal,
};
use nars_agent_runtime::ActorConfig;
use nars_model::{ModelProvider, ScriptedProvider};
use nars_server::{AppState, ServerConfig, build_router};
use nars_session::{FileSessionStore, SessionStore, SqliteSessionStore};
use nars_tools::policy::ApprovalMode;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Harness {
    app: Router,
    workspace: TempDir,
    _store: TempDir,
}

fn harness(scripts: Vec<AgentIr>, approval: ApprovalMode, token: Option<&str>) -> Harness {
    let workspace = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = Arc::new(FileSessionStore::new(store_dir.path()).unwrap());
    let provider = Arc::new(ScriptedProvider::new(scripts));
    let config = ServerConfig {
        provider_name: "scripted".to_owned(),
        token: token.map(str::to_owned),
        approval_mode: approval,
        max_steps: 12,
        actor: ActorConfig {
            model: "scripted".to_owned(),
            seed: 11,
            ..ActorConfig::default()
        },
        static_dir: None,
        ..ServerConfig::default()
    };
    let state = AppState::new(store, provider as Arc<dyn ModelProvider>, config);
    Harness {
        app: build_router(state),
        workspace,
        _store: store_dir,
    }
}

async fn request(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(value) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

fn tool_proposal(tool: &str, arguments: Value) -> AgentIr {
    AgentIr::TaskProposal(TaskProposal {
        goal: GoalSpec::new(GoalType::ModifyCode, "rename alpha"),
        constraints: Vec::new(),
        candidate_actions: vec![
            CandidateAction::new(tool)
                .with_arguments(arguments)
                .with_confidence(0.95),
        ],
        confidence: 0.9,
        notes: None,
        summary: None,
    })
}

fn final_answer(text: &str) -> AgentIr {
    AgentIr::FinalAnswer(FinalAnswer {
        text: text.to_owned(),
        success: true,
        artifacts: Vec::new(),
    })
}

async fn create_session(harness: &Harness) -> String {
    let (status, body) = request(
        &harness.app,
        Method::POST,
        "/api/sessions",
        Some(json!({"workspace": harness.workspace.path().display().to_string(), "title": "test"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["session"]["session_id"].as_str().unwrap().to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn health_and_tools_are_public() {
    let harness = harness(Vec::new(), ApprovalMode::AutoApprove, None);
    let (status, body) = request(&harness.app, Method::GET, "/api/health", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert!(body["tools"].as_array().unwrap().len() >= 6);

    let (status, body) = request(&harness.app, Method::GET, "/api/tools", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["tools"].as_array().unwrap().len() >= 6);
}

#[tokio::test(flavor = "multi_thread")]
async fn creates_session_sends_message_and_completes() {
    let harness = harness(
        vec![final_answer("Hello from the scripted provider.")],
        ApprovalMode::AutoApprove,
        None,
    );
    let session_id = create_session(&harness).await;

    let (status, sessions) = request(&harness.app, Method::GET, "/api/sessions", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sessions["sessions"].as_array().unwrap().len(), 1);

    let (status, created) = request(
        &harness.app,
        Method::POST,
        &format!("/api/sessions/{session_id}/messages"),
        Some(json!({"text": "say hello"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{created}");
    let run_id = created["run_id"].as_str().unwrap().to_owned();

    let mut state = Value::Null;
    for _ in 0..200 {
        let (status, body) = request(
            &harness.app,
            Method::GET,
            &format!("/api/runs/{run_id}"),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        state = body["run"].clone();
        if state["status"] == "completed" || state["status"] == "failed" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(state["status"], "completed", "{state}");
    assert_eq!(
        state["messages"][0]["text"],
        "Hello from the scripted provider."
    );

    let (status, runs) = request(
        &harness.app,
        Method::GET,
        &format!("/api/sessions/{session_id}/runs"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(runs["runs"].as_array().unwrap().len(), 1);

    let mut replay = Value::Null;
    for _ in 0..100 {
        let (status, body) = request(
            &harness.app,
            Method::GET,
            &format!("/api/sessions/{session_id}/replay?run_id={run_id}"),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        replay = body;
        if replay["matches_snapshot"] == true {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(replay["state"]["status"], "completed", "{replay}");
    assert_eq!(replay["matches_snapshot"], true, "{replay}");

    let (status, body) = request(
        &harness.app,
        Method::POST,
        &format!("/api/sessions/{session_id}/resume"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn optional_token_protects_api_routes() {
    let harness = harness(Vec::new(), ApprovalMode::AutoApprove, Some("secret"));
    let (status, _) = request(&harness.app, Method::GET, "/api/sessions", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = request(
        &harness.app,
        Method::GET,
        "/api/sessions",
        None,
        Some("secret"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(&harness.app, Method::GET, "/api/health", None, None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn approval_route_executes_a_patch() {
    let workspace = TempDir::new().unwrap();
    std::fs::create_dir_all(workspace.path().join("src")).unwrap();
    std::fs::write(
        workspace.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(workspace.path().join("src/lib.rs"), "fn alpha() {}\n").unwrap();

    let mut harness = harness(
        vec![
            tool_proposal("read_file", json!({"path": "src/lib.rs"})),
            tool_proposal(
                "apply_patch",
                json!({"path": "src/lib.rs", "old_text": "alpha", "new_text": "beta"}),
            ),
            tool_proposal("cargo_check", json!({})),
            final_answer("Patched."),
        ],
        ApprovalMode::Ask,
        None,
    );
    harness.workspace = workspace;

    let session_id = create_session(&harness).await;
    let (status, created) = request(
        &harness.app,
        Method::POST,
        &format!("/api/sessions/{session_id}/messages"),
        Some(json!({"text": "rename alpha to beta"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{created}");
    let run_id = created["run_id"].as_str().unwrap().to_owned();

    let mut call_id = None;
    for _ in 0..200 {
        let (_, body) = request(
            &harness.app,
            Method::GET,
            &format!("/api/runs/{run_id}"),
            None,
            None,
        )
        .await;
        if body["run"]["status"] == "awaiting_approval" {
            call_id = body["run"]["pending_approval"]["id"]
                .as_str()
                .map(str::to_owned);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let call_id = call_id.expect("approval was not requested");

    let (status, body) = request(
        &harness.app,
        Method::POST,
        &format!("/api/tool-calls/{call_id}/approve"),
        Some(json!({"approved": true})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let mut status_value = Value::Null;
    for _ in 0..200 {
        let (_, body) = request(
            &harness.app,
            Method::GET,
            &format!("/api/runs/{run_id}"),
            None,
            None,
        )
        .await;
        status_value = body["run"]["status"].clone();
        if status_value == "completed" || status_value == "failed" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(status_value, "completed");
    assert_eq!(
        std::fs::read_to_string(harness.workspace.path().join("src/lib.rs")).unwrap(),
        "fn beta() {}\n"
    );
}

struct FakeMcpTool {
    manifest: nars_agent_ir::ToolManifest,
}

impl nars_tools::manifest::Tool for FakeMcpTool {
    fn manifest(&self) -> &nars_agent_ir::ToolManifest {
        &self.manifest
    }

    fn execute(
        &self,
        call: &nars_agent_ir::ToolCall,
        _context: &nars_tools::manifest::ToolContext,
    ) -> nars_agent_ir::ToolResult {
        nars_agent_ir::ToolResult::success(call.id.clone(), "mcp")
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn extra_mcp_tools_are_exposed_by_the_api() {
    let store_dir = TempDir::new().unwrap();
    let store = Arc::new(FileSessionStore::new(store_dir.path()).unwrap());
    let provider = Arc::new(ScriptedProvider::new(Vec::new()));
    let manifest = nars_agent_ir::ToolManifest::new(
        "mcp_files_read".to_owned(),
        "remote read".to_owned(),
        json!({"type": "object"}),
        nars_agent_ir::SideEffect::ExternalSideEffect,
    )
    .with_approval();
    let config = ServerConfig {
        provider_name: "scripted".to_owned(),
        extra_tools: vec![Arc::new(FakeMcpTool { manifest })],
        static_dir: None,
        ..ServerConfig::default()
    };
    let state = AppState::new(store, provider as Arc<dyn ModelProvider>, config);
    let app = build_router(state);
    let (status, tools) = request(&app, Method::GET, "/api/tools", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let names = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"mcp_files_read"), "{names:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn sqlite_store_runs_and_replays_a_session() {
    let workspace = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store = Arc::new(SqliteSessionStore::new(db_dir.path().join("sessions.db")).unwrap());
    let provider = Arc::new(ScriptedProvider::new(vec![final_answer("sqlite ok")]));
    let config = ServerConfig {
        provider_name: "scripted".to_owned(),
        static_dir: None,
        ..ServerConfig::default()
    };
    let state = AppState::new(store, provider as Arc<dyn ModelProvider>, config);
    let app = build_router(state);

    let (status, created) = request(
        &app,
        Method::POST,
        "/api/sessions",
        Some(json!({"workspace": workspace.path().display().to_string(), "title": "sqlite"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let session_id = created["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let (status, sent) = request(
        &app,
        Method::POST,
        &format!("/api/sessions/{session_id}/messages"),
        Some(json!({"text": "hello sqlite"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{sent}");
    let run_id = sent["run_id"].as_str().unwrap().to_owned();

    for _ in 0..200 {
        let (_, body) = request(
            &app,
            Method::GET,
            &format!("/api/runs/{run_id}"),
            None,
            None,
        )
        .await;
        if body["run"]["status"] == "completed" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let (status, sessions) = request(&app, Method::GET, "/api/sessions", None, None).await;
    assert_eq!(status, StatusCode::OK, "{sessions}");
    assert_eq!(sessions["sessions"].as_array().unwrap().len(), 1);

    let mut replay = Value::Null;
    for _ in 0..100 {
        let (status, body) = request(
            &app,
            Method::GET,
            &format!("/api/sessions/{session_id}/replay?run_id={run_id}"),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        replay = body;
        if replay["matches_snapshot"] == true {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(replay["state"]["status"], "completed", "{replay}");
    assert_eq!(replay["matches_snapshot"], true, "{replay}");
}

#[tokio::test(flavor = "multi_thread")]
async fn run_history_survives_a_restart() {
    let workspace = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        final_answer("persisted"),
        final_answer("persisted again"),
    ]));
    let store = Arc::new(FileSessionStore::new(store_dir.path()).unwrap());
    let config = ServerConfig {
        provider_name: "scripted".to_owned(),
        static_dir: None,
        ..ServerConfig::default()
    };
    let app = build_router(AppState::new(
        store.clone(),
        provider as Arc<dyn ModelProvider>,
        config.clone(),
    ));
    let (status, created) = request(
        &app,
        Method::POST,
        "/api/sessions",
        Some(json!({"workspace": workspace.path().display().to_string(), "title": "history"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let session_id = created["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut run_ids = Vec::new();
    for text in ["hello history", "second run"] {
        let (status, sent) = request(
            &app,
            Method::POST,
            &format!("/api/sessions/{session_id}/messages"),
            Some(json!({"text": text})),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{sent}");
        let run_id = sent["run_id"].as_str().unwrap().to_owned();
        for _ in 0..200 {
            let (_, body) = request(
                &app,
                Method::GET,
                &format!("/api/runs/{run_id}"),
                None,
                None,
            )
            .await;
            if body["run"]["status"] == "completed" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        run_ids.push(run_id);
    }
    let first_run = run_ids[0].clone();

    let events = store
        .read_events(&SessionId::from_static(session_id.clone()))
        .unwrap();
    assert!(events.iter().any(|envelope| matches!(
        &envelope.event,
        AgentEvent::UserMessage { run_id: Some(id), .. } if id.as_str() == first_run
    )));

    drop(app);
    let reopened = Arc::new(FileSessionStore::new(store_dir.path()).unwrap());
    let app = build_router(AppState::new(
        reopened,
        Arc::new(ScriptedProvider::new(Vec::new())) as Arc<dyn ModelProvider>,
        config,
    ));
    let (status, runs) = request(
        &app,
        Method::GET,
        &format!("/api/sessions/{session_id}/runs"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{runs}");
    let summaries = runs["runs"].as_array().unwrap();
    assert_eq!(summaries.len(), 2, "{runs}");
    assert_eq!(summaries[0]["run_id"], run_ids[1]);
    assert_eq!(summaries[1]["run_id"], first_run);
    assert_eq!(summaries[1]["status"], "completed");

    let (status, projected) = request(
        &app,
        Method::GET,
        &format!("/api/runs/{first_run}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{projected}");
    assert_eq!(projected["live"], false);
    assert_eq!(projected["run"]["status"], "completed");
    assert_eq!(projected["run"]["run_id"], first_run);
}

#[tokio::test(flavor = "multi_thread")]
async fn branches_a_historical_run_into_a_new_session() {
    let workspace = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let config = ServerConfig {
        provider_name: "scripted".to_owned(),
        static_dir: None,
        ..ServerConfig::default()
    };
    let store = Arc::new(FileSessionStore::new(store_dir.path()).unwrap());
    let provider = Arc::new(ScriptedProvider::new(vec![
        final_answer("first"),
        final_answer("second"),
    ]));
    let app = build_router(AppState::new(
        store.clone(),
        provider as Arc<dyn ModelProvider>,
        config.clone(),
    ));
    let (status, created) = request(
        &app,
        Method::POST,
        "/api/sessions",
        Some(json!({"workspace": workspace.path().display().to_string(), "title": "parent"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let parent = created["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let mut parent_runs = Vec::new();
    for text in ["first task", "second task"] {
        let (status, sent) = request(
            &app,
            Method::POST,
            &format!("/api/sessions/{parent}/messages"),
            Some(json!({"text": text})),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{sent}");
        let run_id = sent["run_id"].as_str().unwrap().to_owned();
        for _ in 0..200 {
            let (_, body) = request(
                &app,
                Method::GET,
                &format!("/api/runs/{run_id}"),
                None,
                None,
            )
            .await;
            if body["run"]["status"] == "completed" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        parent_runs.push(run_id);
    }
    drop(app);

    // Branch after a restart: the target run is no longer live.
    let reopened = Arc::new(FileSessionStore::new(store_dir.path()).unwrap());
    let provider = Arc::new(ScriptedProvider::new(vec![final_answer("branched")]));
    let app = build_router(AppState::new(
        reopened.clone(),
        provider as Arc<dyn ModelProvider>,
        config,
    ));
    let (status, branch) = request(
        &app,
        Method::POST,
        &format!("/api/sessions/{parent}/branch"),
        Some(json!({"run_id": parent_runs[1]})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{branch}");
    let branch_session = branch["session"]["session_id"].as_str().unwrap().to_owned();
    let branch_run = branch["run_id"].as_str().unwrap().to_owned();
    assert_ne!(branch_session, parent);
    assert_eq!(branch["session"]["parent_session_id"], parent);
    assert_eq!(branch["session"]["branched_from_run_id"], parent_runs[1]);
    assert!(branch["copied_events"].as_u64().unwrap() > 0, "{branch}");

    for _ in 0..200 {
        let (_, body) = request(
            &app,
            Method::GET,
            &format!("/api/runs/{branch_run}"),
            None,
            None,
        )
        .await;
        if body["run"]["status"] == "completed" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let (status, runs) = request(
        &app,
        Method::GET,
        &format!("/api/sessions/{branch_session}/runs"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{runs}");
    let summaries = runs["runs"].as_array().unwrap();
    assert_eq!(summaries.len(), 2, "{runs}");
    assert!(summaries.iter().any(|run| run["run_id"] == branch_run));
    assert!(!summaries.iter().any(|run| run["run_id"] == parent_runs[1]));

    let branch_events = reopened
        .read_events(&SessionId::from_static(branch_session.clone()))
        .unwrap();
    let branch_run_ids = branch_events
        .iter()
        .filter_map(|envelope| envelope.event.run_id())
        .collect::<Vec<_>>();
    assert!(
        !branch_run_ids
            .iter()
            .any(|id| id.as_str() == parent_runs[0])
    );
    assert!(branch_run_ids.iter().any(|id| id.as_str() == branch_run));
    assert!(branch_events.iter().any(|envelope| matches!(
        &envelope.event,
        AgentEvent::RunStarted { session_id, .. } if session_id.as_str() == branch_session
    )));
    assert!(branch_events.iter().any(|envelope| matches!(
        &envelope.event,
        AgentEvent::UserMessage { run_id: Some(id), text, .. }
            if id.as_str() == branch_run && text == "second task"
    )));

    let parent_events = reopened
        .read_events(&SessionId::from_static(parent))
        .unwrap();
    assert!(parent_events.iter().any(|envelope| matches!(
        &envelope.event,
        AgentEvent::RunStarted { run_id, .. } if run_id.as_str() == parent_runs[0]
    )));
    assert!(parent_events.iter().any(|envelope| matches!(
        &envelope.event,
        AgentEvent::RunStarted { run_id, .. } if run_id.as_str() == parent_runs[1]
    )));
}

#[tokio::test(flavor = "multi_thread")]
async fn renames_and_deletes_a_session() {
    let workspace = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = Arc::new(FileSessionStore::new(store_dir.path()).unwrap());
    let provider = Arc::new(ScriptedProvider::new(Vec::new()));
    let config = ServerConfig {
        provider_name: "scripted".to_owned(),
        static_dir: None,
        ..ServerConfig::default()
    };
    let app = build_router(AppState::new(
        store,
        provider as Arc<dyn ModelProvider>,
        config,
    ));
    let (status, created) = request(
        &app,
        Method::POST,
        "/api/sessions",
        Some(json!({"workspace": workspace.path().display().to_string(), "title": "old"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let session_id = created["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let (status, renamed) = request(
        &app,
        Method::PATCH,
        &format!("/api/sessions/{session_id}"),
        Some(json!({"title": "new title"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{renamed}");
    assert_eq!(renamed["session"]["title"], "new title");

    let (status, listed) = request(&app, Method::GET, "/api/sessions", None, None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["sessions"][0]["title"], "new title");

    let (status, _) = request(
        &app,
        Method::DELETE,
        &format!("/api/sessions/{session_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = request(
        &app,
        Method::GET,
        &format!("/api/sessions/{session_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, listed) = request(&app, Method::GET, "/api/sessions", None, None).await;
    assert_eq!(listed["sessions"].as_array().unwrap().len(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_to_delete_a_parent_with_branches() {
    let workspace = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let config = ServerConfig {
        provider_name: "scripted".to_owned(),
        static_dir: None,
        ..ServerConfig::default()
    };
    let store = Arc::new(FileSessionStore::new(store_dir.path()).unwrap());
    let provider = Arc::new(ScriptedProvider::new(vec![
        final_answer("done"),
        final_answer("branch done"),
    ]));
    let app = build_router(AppState::new(
        store,
        provider as Arc<dyn ModelProvider>,
        config,
    ));
    let (_, created) = request(
        &app,
        Method::POST,
        "/api/sessions",
        Some(json!({"workspace": workspace.path().display().to_string(), "title": "parent"})),
        None,
    )
    .await;
    let parent = created["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (_, sent) = request(
        &app,
        Method::POST,
        &format!("/api/sessions/{parent}/messages"),
        Some(json!({"text": "task"})),
        None,
    )
    .await;
    let run_id = sent["run_id"].as_str().unwrap().to_owned();
    for _ in 0..200 {
        let (_, body) = request(
            &app,
            Method::GET,
            &format!("/api/runs/{run_id}"),
            None,
            None,
        )
        .await;
        if body["run"]["status"] == "completed" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let (status, branch) = request(
        &app,
        Method::POST,
        &format!("/api/sessions/{parent}/branch"),
        Some(json!({"run_id": run_id})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{branch}");
    let branch_id = branch["session"]["session_id"].as_str().unwrap().to_owned();
    let branch_run = branch["run_id"].as_str().unwrap().to_owned();
    for _ in 0..200 {
        let (_, body) = request(
            &app,
            Method::GET,
            &format!("/api/runs/{branch_run}"),
            None,
            None,
        )
        .await;
        if body["run"]["status"].is_string()
            && ["completed", "failed", "cancelled"]
                .contains(&body["run"]["status"].as_str().unwrap_or_default())
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let (status, body) = request(
        &app,
        Method::DELETE,
        &format!("/api/sessions/{parent}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let (status, _) = request(
        &app,
        Method::DELETE,
        &format!("/api/sessions/{branch_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = request(
        &app,
        Method::DELETE,
        &format!("/api/sessions/{parent}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
