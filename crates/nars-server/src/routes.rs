//! HTTP routes.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, Uri, header};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use nars_agent_ir::{RunId, SessionId, ToolCallId};
use serde::Deserialize;
use serde_json::{Value, json};
use tower_http::cors::{Any, CorsLayer};

use crate::auth;
use crate::error::ApiError;
use crate::sse;
use crate::state::{AppState, create_session, now_ms};

/// Builds the application router.
#[must_use]
pub fn build_router(state: Arc<AppState>) -> Router {
    let protected = Router::new()
        .route("/api/tools", get(tools))
        .route(
            "/api/sessions",
            post(create_session_handler).get(list_sessions),
        )
        .route("/api/sessions/{id}", get(get_session))
        .route("/api/sessions/{id}/messages", post(send_message))
        .route("/api/sessions/{id}/runs", get(list_runs))
        .route("/api/sessions/{id}/events", get(sse::session_events))
        .route("/api/sessions/{id}/replay", get(replay_session))
        .route("/api/sessions/{id}/resume", post(resume_checkpoint))
        .route("/api/runs", post(create_run))
        .route("/api/runs/{id}", get(get_run))
        .route("/api/runs/{id}/events", get(sse::run_events))
        .route("/api/runs/{id}/pause", post(pause_run))
        .route("/api/runs/{id}/resume", post(resume_run))
        .route("/api/runs/{id}/cancel", post(cancel_run))
        .route("/api/tool-calls/{id}/approve", post(approve_call))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth::require_token,
        ))
        .with_state(Arc::clone(&state));

    let public = Router::new()
        .route("/api/health", get(health))
        .with_state(Arc::clone(&state));

    let mut app = public.merge(protected).layer(
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any),
    );

    let static_routes = Router::new()
        .fallback(get(static_handler))
        .with_state(Arc::clone(&state));
    app = app.fallback_service(static_routes);
    app
}

async fn static_handler(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let Some(directory) = state.config.static_dir.clone() else {
        return not_found().into_response();
    };
    if !directory.is_dir() {
        return not_found().into_response();
    }
    let requested = uri.path().trim_start_matches('/');
    let requested = if requested.is_empty() {
        "index.html"
    } else {
        requested
    };
    let relative = std::path::Path::new(requested);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return not_found().into_response();
    }
    let mut file = directory.join(relative);
    if !file.is_file() {
        file = directory.join("index.html");
    }
    let bytes = match tokio::fs::read(&file).await {
        Ok(bytes) => bytes,
        Err(_) => return not_found().into_response(),
    };
    let mime = mime_for(&file);
    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, header::HeaderValue::from_static(mime));
    response
}

fn mime_for(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
    {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "ico" => "image/x-icon",
        "wasm" => "application/wasm",
        "woff2" => "font/woff2",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "provider": state.provider.name(),
        "model": state.config.actor.model,
        "approval_mode": state.config.approval_mode,
        "tools": state.tool_manifests(),
        "max_steps": state.config.max_steps,
        "time_ms": now_ms()
    }))
}

async fn tools(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "tools": state.tool_manifests(),
        "approval_mode": state.config.approval_mode
    }))
}

#[derive(Debug, Deserialize)]
struct CreateSessionRequest {
    workspace: String,
    #[serde(default)]
    title: Option<String>,
}

async fn create_session_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if body.workspace.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "workspace must not be empty".to_owned(),
        ));
    }
    let meta = create_session(&state, body.workspace.trim(), body.title.as_deref())?;
    Ok((StatusCode::CREATED, Json(json!({"session": meta}))))
}

async fn list_sessions(State(state): State<Arc<AppState>>) -> Result<Json<Value>, ApiError> {
    let sessions = state.store.list_sessions()?;
    Ok(Json(json!({"sessions": sessions})))
}

async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let session_id = SessionId::from_static(session_id);
    let meta = state
        .store
        .get_session(&session_id)?
        .ok_or_else(|| ApiError::NotFound(format!("session `{session_id}` does not exist")))?;
    let runs = state.runs_for_session(&session_id);
    Ok(Json(json!({"session": meta, "runs": runs})))
}

#[derive(Debug, Deserialize)]
struct SendMessageRequest {
    text: String,
    #[serde(default)]
    max_steps: Option<u32>,
}

async fn send_message(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(body): Json<SendMessageRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if body.text.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "message text must not be empty".to_owned(),
        ));
    }
    let session_id = SessionId::from_static(session_id);
    if state.store.get_session(&session_id)?.is_none() {
        return Err(ApiError::NotFound(format!(
            "session `{session_id}` does not exist"
        )));
    }
    let run_id = state.start_run(&session_id, body.text.trim(), body.max_steps)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "run_id": run_id,
            "session_id": session_id,
            "status": "started"
        })),
    ))
}

#[derive(Debug, Deserialize)]
struct CreateRunRequest {
    session_id: SessionId,
    message: String,
    #[serde(default)]
    max_steps: Option<u32>,
}

async fn create_run(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateRunRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if body.message.trim().is_empty() {
        return Err(ApiError::BadRequest("message must not be empty".to_owned()));
    }
    let run_id = state.start_run(&body.session_id, body.message.trim(), body.max_steps)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"run_id": run_id, "session_id": body.session_id, "status": "started"})),
    ))
}

async fn get_run(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let run_id = RunId::from_static(run_id);
    if let Some(run) = state.run(&run_id) {
        return Ok(Json(json!({"run": run.state(), "live": true})));
    }
    if let Some(projected) = state.persisted_run(&run_id)? {
        return Ok(Json(json!({"run": projected, "live": false})));
    }
    Err(ApiError::NotFound(format!("run `{run_id}` does not exist")))
}

async fn list_runs(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let session_id = SessionId::from_static(session_id);
    if state.store.get_session(&session_id)?.is_none() {
        return Err(ApiError::NotFound(format!(
            "session `{session_id}` does not exist"
        )));
    }
    Ok(Json(json!({
        "runs": state.runs_for_session(&session_id)
    })))
}

async fn resume_checkpoint(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let session_id = SessionId::from_static(session_id);
    let run_id = state.resume_run(&session_id)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "run_id": run_id,
            "session_id": session_id,
            "status": "resumed"
        })),
    ))
}

#[derive(Debug, Default, Deserialize)]
struct ReplayQuery {
    #[serde(default)]
    run_id: Option<String>,
}

async fn replay_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Query(query): Query<ReplayQuery>,
) -> Result<Json<Value>, ApiError> {
    let session_id = SessionId::from_static(session_id);
    if state.store.get_session(&session_id)?.is_none() {
        return Err(ApiError::NotFound(format!(
            "session `{session_id}` does not exist"
        )));
    }
    let mut events = state.store.read_events(&session_id)?;
    let run_id = query.run_id.map(RunId::from_static).or_else(|| {
        events
            .iter()
            .rev()
            .find_map(|envelope| match &envelope.event {
                nars_agent_ir::AgentEvent::RunStarted { run_id, .. } => Some(run_id.clone()),
                _ => None,
            })
    });
    let Some(run_id) = run_id else {
        return Err(ApiError::NotFound("session has no runs".to_owned()));
    };
    events.retain(|envelope| {
        matches!(
            &envelope.event,
            nars_agent_ir::AgentEvent::RunStarted { .. }
        ) || envelope.event.run_id() == Some(&run_id)
    });
    let projected = nars_agent_runtime::project(&events)?;
    let snapshot = state.store.load_snapshot(&session_id)?;
    let snapshot_hash = snapshot
        .as_ref()
        .filter(|snapshot| snapshot.run_id == run_id)
        .map(|snapshot| snapshot.state_hash.clone());
    let matches_snapshot = snapshot_hash.as_deref() == Some(projected.state_hash().as_str());
    Ok(Json(json!({
        "run_id": run_id,
        "state_hash": projected.state_hash(),
        "snapshot_hash": snapshot_hash,
        "matches_snapshot": matches_snapshot,
        "event_count": events.len(),
        "state": projected
    })))
}

#[derive(Debug, Default, Deserialize)]
struct ControlQuery {
    #[serde(default)]
    reason: Option<String>,
}

async fn pause_run(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
    Query(query): Query<ControlQuery>,
) -> Result<Json<Value>, ApiError> {
    let run = lookup_run(&state, &run_id)?;
    let reason = query
        .reason
        .unwrap_or_else(|| "user paused the run".to_owned());
    if run.pause(reason.clone()) {
        Ok(Json(
            json!({"ok": true, "status": "pausing", "reason": reason}),
        ))
    } else {
        Err(ApiError::Conflict(
            "run is no longer accepting control".to_owned(),
        ))
    }
}

async fn resume_run(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let run = lookup_run(&state, &run_id)?;
    if run.resume() {
        Ok(Json(json!({"ok": true, "status": "resuming"})))
    } else {
        Err(ApiError::Conflict(
            "run is no longer accepting control".to_owned(),
        ))
    }
}

async fn cancel_run(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
    Query(query): Query<ControlQuery>,
) -> Result<Json<Value>, ApiError> {
    let run = lookup_run(&state, &run_id)?;
    let reason = query
        .reason
        .unwrap_or_else(|| "user cancelled the run".to_owned());
    if run.cancel(reason) {
        Ok(Json(json!({"ok": true, "status": "cancelling"})))
    } else {
        Err(ApiError::Conflict(
            "run is no longer accepting control".to_owned(),
        ))
    }
}

#[derive(Debug, Deserialize)]
struct ApproveRequest {
    approved: bool,
    #[serde(default)]
    note: Option<String>,
}

async fn approve_call(
    State(state): State<Arc<AppState>>,
    Path(call_id): Path<String>,
    Json(body): Json<ApproveRequest>,
) -> Result<Json<Value>, ApiError> {
    let call_id = ToolCallId::from_static(call_id);
    let run = state
        .run_for_call(&call_id)
        .ok_or_else(|| ApiError::NotFound(format!("no pending call `{call_id}`")))?;
    if run.approve(call_id.clone(), body.approved, body.note) {
        Ok(Json(
            json!({"ok": true, "call_id": call_id, "approved": body.approved}),
        ))
    } else {
        Err(ApiError::Conflict(
            "run is no longer accepting approval".to_owned(),
        ))
    }
}

fn lookup_run(
    state: &AppState,
    run_id: &str,
) -> Result<Arc<nars_agent_runtime::RunHandle>, ApiError> {
    let run_id = RunId::from_static(run_id);
    state
        .run(&run_id)
        .ok_or_else(|| ApiError::NotFound(format!("run `{run_id}` does not exist")))
}

/// A fallback handler for unknown API routes.
pub fn not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": {"status": 404, "message": "route not found"}})),
    )
}
