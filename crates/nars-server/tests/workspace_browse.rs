use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use nars_agent_runtime::ActorConfig;
use nars_model::{ModelProvider, ScriptedProvider};
use nars_server::{AppState, ServerConfig, build_router};
use nars_session::FileSessionStore;
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

struct Harness {
    app: Router,
    workspace_root: TempDir,
    _store: TempDir,
}

fn harness(restrict_workspaces: bool) -> Harness {
    let workspace_root = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = Arc::new(FileSessionStore::new(store_dir.path()).unwrap());
    let provider = Arc::new(ScriptedProvider::new(Vec::new()));
    let config = ServerConfig {
        provider_name: "scripted".to_owned(),
        actor: ActorConfig {
            model: "scripted".to_owned(),
            ..ActorConfig::default()
        },
        workspace_root: restrict_workspaces.then(|| workspace_root.path().to_path_buf()),
        static_dir: None,
        ..ServerConfig::default()
    };
    let state = AppState::new(store, provider as Arc<dyn ModelProvider>, config);
    Harness {
        app: build_router(state),
        workspace_root,
        _store: store_dir,
    }
}

async fn list(app: &Router, path: Option<&Path>) -> (StatusCode, Value) {
    let uri = match path {
        Some(path) => format!(
            "/api/fs/dirs?path={}",
            urlencode(&path.display().to_string())
        ),
        None => "/api/fs/dirs".to_owned(),
    };
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn urlencode(raw: &str) -> String {
    raw.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn names(listing: &Value) -> Vec<&str> {
    listing["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect()
}

fn prepare(root: &Path) -> PathBuf {
    for name in ["alpha", "beta", ".cache"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
    }
    std::fs::write(root.join("notes.txt"), "not a directory").unwrap();
    root.canonicalize().unwrap()
}

#[tokio::test]
async fn lists_only_subdirectories_of_the_workspace_root() {
    let harness = harness(true);
    let root = prepare(harness.workspace_root.path());

    let (status, listing) = list(&harness.app, None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(listing["path"], root.display().to_string());
    assert_eq!(names(&listing), ["alpha", "beta", ".cache"]);
    assert_eq!(listing["entries"][2]["hidden"], true);
    assert_eq!(listing["entries"][0]["hidden"], false);
    assert_eq!(listing["parent"], Value::Null);
    assert_eq!(listing["truncated"], false);
}

#[tokio::test]
async fn navigates_into_a_subdirectory_and_back_out() {
    let harness = harness(true);
    let root = prepare(harness.workspace_root.path());
    let alpha = root.join("alpha");
    std::fs::create_dir_all(alpha.join("nested")).unwrap();

    let (status, listing) = list(&harness.app, Some(&alpha)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(listing["path"], alpha.display().to_string());
    assert_eq!(listing["parent"], root.display().to_string());
    assert_eq!(names(&listing), ["nested"]);
}

#[tokio::test]
async fn refuses_paths_outside_the_workspace_root() {
    let harness = harness(true);

    let (status, body) = list(&harness.app, Some(Path::new("/etc"))).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("outside the allowed workspace root")
    );
}

#[tokio::test]
async fn reports_missing_and_non_directory_paths() {
    let harness = harness(true);
    let root = prepare(harness.workspace_root.path());

    let (missing, body) = list(&harness.app, Some(&root.join("absent"))).await;
    assert_eq!(missing, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("cannot open")
    );

    let (file, _) = list(&harness.app, Some(&root.join("notes.txt"))).await;
    assert_eq!(file, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn browses_the_filesystem_when_no_root_is_configured() {
    let harness = harness(false);
    let root = prepare(harness.workspace_root.path());

    let (status, listing) = list(&harness.app, Some(&root)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&listing), ["alpha", "beta", ".cache"]);
    assert!(listing["parent"].is_string());
    assert_eq!(listing["root"], Value::Null);
    assert!(listing["home"].is_string() || listing["home"].is_null());
}

#[tokio::test]
async fn browsing_requires_the_configured_token() {
    let workspace_root = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let store = Arc::new(FileSessionStore::new(store_dir.path()).unwrap());
    let provider = Arc::new(ScriptedProvider::new(Vec::new()));
    let config = ServerConfig {
        provider_name: "scripted".to_owned(),
        token: Some("secret".to_owned()),
        workspace_root: Some(workspace_root.path().to_path_buf()),
        static_dir: None,
        ..ServerConfig::default()
    };
    let state = AppState::new(store, provider as Arc<dyn ModelProvider>, config);
    let app = build_router(state);

    let unauthorized = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/fs/dirs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let authorized = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/fs/dirs")
                .header(header::AUTHORIZATION, "Bearer secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(authorized.status(), StatusCode::OK);
}
