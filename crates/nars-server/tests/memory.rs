//! Long-term memory must survive both run boundaries and process restarts.

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use nars_agent_ir::{AgentIr, CandidateAction, FinalAnswer, GoalSpec, GoalType, TaskProposal};
use nars_agent_runtime::ActorConfig;
use nars_model::{ModelProvider, ScriptedProvider};
use nars_server::{AppState, ServerConfig, build_router};
use nars_session::{FileMemoryStore, FileSessionStore, MemoryStore, SessionStore};
use nars_tools::policy::ApprovalMode;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Harness {
    app: Router,
    memory: Arc<FileMemoryStore>,
}

fn harness(store_dir: &Path, memory_dir: &Path, rounds: usize) -> Harness {
    let store = Arc::new(FileSessionStore::new(store_dir).unwrap());
    let memory = Arc::new(FileMemoryStore::new(memory_dir).unwrap());
    let mut scripts = Vec::new();
    for round in 0..rounds {
        scripts.push(proposal());
        // The runtime may ask again when the task plan is not satisfied yet, so
        // keep enough final answers queued; unused ones are simply ignored.
        for _ in 0..6 {
            scripts.push(AgentIr::FinalAnswer(FinalAnswer {
                text: format!("round {round}"),
                success: true,
                artifacts: Vec::new(),
            }));
        }
    }
    let provider = Arc::new(ScriptedProvider::new(scripts));
    let config = ServerConfig {
        provider_name: "scripted".to_owned(),
        approval_mode: ApprovalMode::AutoApprove,
        max_steps: 8,
        actor: ActorConfig {
            model: "scripted".to_owned(),
            seed: 11,
            ..ActorConfig::default()
        },
        memory: Some(Arc::clone(&memory) as Arc<dyn MemoryStore>),
        ..ServerConfig::default()
    };
    let state = AppState::new(
        store as Arc<dyn SessionStore>,
        provider as Arc<dyn ModelProvider>,
        config,
    );
    Harness {
        app: build_router(state),
        memory,
    }
}

fn proposal() -> AgentIr {
    AgentIr::TaskProposal(TaskProposal {
        goal: GoalSpec::new(GoalType::Investigate, "inspect the workspace"),
        constraints: Vec::new(),
        candidate_actions: vec![
            CandidateAction::new("list_files")
                .with_arguments(json!({"path": "."}))
                .with_confidence(0.9),
        ],
        confidence: 0.9,
        notes: None,
        summary: None,
    })
}

async fn request(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(value) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn create_session(app: &Router, workspace: &Path) -> String {
    let (status, body) = request(
        app,
        Method::POST,
        "/api/sessions",
        Some(json!({"workspace": workspace.display().to_string(), "title": "memory"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["session"]["session_id"].as_str().unwrap().to_owned()
}

async fn run_message(app: &Router, session: &str, text: &str) -> String {
    let (status, sent) = request(
        app,
        Method::POST,
        &format!("/api/sessions/{session}/messages"),
        Some(json!({"text": text})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{sent}");
    let run_id = sent["run_id"].as_str().unwrap().to_owned();
    for _ in 0..300 {
        let (_, body) = request(app, Method::GET, &format!("/api/runs/{run_id}"), None).await;
        match body["run"]["status"].as_str() {
            Some("completed") => return run_id,
            Some("failed") => panic!("run failed: {body}"),
            _ => {}
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("run {run_id} timed out");
}

async fn wait_for_memory(
    memory: &Arc<FileMemoryStore>,
    workspace: &Path,
) -> nars_core::MemorySnapshot {
    let key = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf())
        .display()
        .to_string();
    for _ in 0..100 {
        if let Some(snapshot) = memory.load(&key).unwrap() {
            return snapshot;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("memory was never persisted for {key}");
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_survives_runs_and_a_restart() {
    let workspace = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let memory_dir = TempDir::new().unwrap();

    // First process: one run, which must persist memory to disk.
    let first = harness(store_dir.path(), memory_dir.path(), 1);
    let session = create_session(&first.app, workspace.path()).await;
    run_message(&first.app, &session, "remember that the cat is an animal").await;
    let after_first = wait_for_memory(&first.memory, workspace.path()).await;
    assert!(!after_first.concepts.is_empty(), "no concepts stored");
    let stored_terms = after_first
        .concepts
        .iter()
        .map(|concept| concept.term.clone())
        .collect::<Vec<_>>();

    // Second process: a fresh AppState over the same directories, so nothing
    // survives in memory and the snapshot has to come from disk.
    let second = harness(store_dir.path(), memory_dir.path(), 1);
    let session = create_session(&second.app, workspace.path()).await;
    run_message(&second.app, &session, "what did I tell you about the cat?").await;
    let after_second = wait_for_memory(&second.memory, workspace.path()).await;

    assert!(
        after_second.concepts.len() >= after_first.concepts.len(),
        "memory shrank: {} -> {}",
        after_first.concepts.len(),
        after_second.concepts.len()
    );
    for term in stored_terms {
        assert!(
            after_second
                .concepts
                .iter()
                .any(|concept| concept.term == term),
            "concept {term} was dropped across the restart"
        );
    }
}
