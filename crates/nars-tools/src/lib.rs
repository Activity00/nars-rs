//! Tool protocol implementation: registry, sandbox, policy, broker, and the
//! built-in development tools.
//!
//! The broker is the only component allowed to touch the filesystem or start a
//! process on behalf of the agent. Every call is policy-checked, sandboxed,
//! timeout-bounded, and output-bounded before its result reaches the runtime.

pub mod broker;
pub mod builtin;
pub mod diff;
pub mod manifest;
pub mod policy;
pub mod process;
pub mod sandbox;
pub mod truncate;

pub use broker::{BrokerConfig, ToolBroker, build_call};
pub use manifest::{
    RegistryError, Tool, ToolContext, ToolRegistry, boolean_schema, integer_schema, object_schema,
    string_array_schema, string_schema,
};
pub use policy::{ApprovalMode, Policy, PolicyConfig, PolicyDecision};
pub use sandbox::{Sandbox, SandboxError};
pub use truncate::truncate_bytes;
