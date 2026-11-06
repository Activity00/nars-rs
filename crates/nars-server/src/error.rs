//! HTTP API errors.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use nars_agent_ir::AgentError;
use nars_session::StoreError;
use serde_json::json;

/// An API error with a stable status and JSON body.
#[derive(Debug)]
pub enum ApiError {
    /// Resource not found.
    NotFound(String),
    /// Invalid request.
    BadRequest(String),
    /// The request conflicts with current state.
    Conflict(String),
    /// Internal failure.
    Internal(String),
}

impl ApiError {
    fn status(&self) -> StatusCode {
        match self {
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Human-readable message.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::NotFound(message)
            | Self::BadRequest(message)
            | Self::Conflict(message)
            | Self::Internal(message) => message,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let body = Json(json!({
            "error": {
                "status": status.as_u16(),
                "message": self.message()
            }
        }));
        (status, body).into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        match &error {
            StoreError::NotFound(_) => Self::NotFound(error.to_string()),
            StoreError::InvalidId(_) => Self::BadRequest(error.to_string()),
            _ => Self::Internal(error.to_string()),
        }
    }
}

impl From<AgentError> for ApiError {
    fn from(error: AgentError) -> Self {
        match &error {
            AgentError::Validation { .. }
            | AgentError::Policy { .. }
            | AgentError::Sandbox { .. } => Self::BadRequest(error.to_string()),
            AgentError::StepLimit { .. } => Self::Conflict(error.to_string()),
            _ => Self::Internal(error.to_string()),
        }
    }
}
