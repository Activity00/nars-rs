//! Error types for the agent protocol and runtime boundaries.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A structured agent error that can be logged, serialized, and shown in the UI.
#[derive(Clone, Debug, PartialEq, Eq, Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentError {
    /// Agent IR failed validation.
    #[error("validation failed: {message}")]
    Validation {
        /// Human-readable details.
        message: String,
    },
    /// A model provider failed.
    #[error("model error: {message}")]
    Model {
        /// Human-readable details.
        message: String,
        /// Whether retrying could help.
        #[serde(default)]
        retryable: bool,
    },
    /// A tool failed internally.
    #[error("tool error: {message}")]
    Tool {
        /// Human-readable details.
        message: String,
    },
    /// The policy refused an action.
    #[error("policy denied action: {message}")]
    Policy {
        /// Human-readable details.
        message: String,
    },
    /// The sandbox refused a path or command.
    #[error("sandbox violation: {message}")]
    Sandbox {
        /// Human-readable details.
        message: String,
    },
    /// An operation exceeded its deadline.
    #[error("timeout: {message}")]
    Timeout {
        /// Human-readable details.
        message: String,
    },
    /// Replay diverged from the recorded stream.
    #[error("replay error: {message}")]
    Replay {
        /// Human-readable details.
        message: String,
    },
    /// The run hit its step limit.
    #[error("step limit reached: {limit}")]
    StepLimit {
        /// Configured limit.
        limit: u32,
    },
    /// A bug or unexpected state.
    #[error("internal error: {message}")]
    Internal {
        /// Human-readable details.
        message: String,
    },
}

impl AgentError {
    /// Builds a retryable model error.
    #[must_use]
    pub fn model(message: impl Into<String>, retryable: bool) -> Self {
        Self::Model {
            message: message.into(),
            retryable,
        }
    }

    /// Builds an internal error.
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
        }
    }

    /// Whether retrying the same operation could succeed.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Model {
                retryable: true,
                ..
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_round_trip_through_json() {
        let error = AgentError::model("rate limited", true);
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["kind"], "model");
        assert_eq!(json["retryable"], true);
        let decoded: AgentError = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, error);
        assert!(decoded.is_retryable());
    }
}
