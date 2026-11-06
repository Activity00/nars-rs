//! Durable session storage for nars-agent.
//!
//! The event log is authoritative. Snapshots are checkpoints that make resume
//! fast, and are always validated against the log's sequence and the runtime's
//! state digest.

pub mod event_log;
pub mod snapshot;
pub mod store;

pub use event_log::{EventEnvelope, EventLog, LogError};
pub use snapshot::{RunSummary, SessionMeta, SessionSnapshot};
pub use store::{FileSessionStore, SessionStore, StoreError};
