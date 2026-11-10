//! HTTP and Server-Sent Events API for nars-agent.
//!
//! The server owns durable sessions, starts runs, exposes run state, and
//! streams the same append-only event log the runtime writes. The web UI is a
//! pure consumer of that stream.

pub mod ask_model;
pub mod auth;
pub mod compare;
pub mod error;
pub mod fs_browse;
mod history;
mod memory_context;
pub mod routes;
pub mod sse;
pub mod state;
pub mod verify;

pub use error::ApiError;
pub use routes::build_router;
pub use state::{AppState, ServerConfig};
