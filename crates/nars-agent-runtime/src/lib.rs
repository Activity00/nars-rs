//! NARS-driven agent runtime.
//!
//! The runtime is a serial session actor: NARS, run state, policy, and the
//! model conversation are all mutated by one owner. Asynchronous work (model
//! calls, tool execution) happens in the driver, which feeds results back into
//! the actor as events. The event log is the source of truth for replay.
//!
//! ```text
//! user message
//!   -> model -> AgentIr
//!   -> validator -> NARS goals/facts
//!   -> NARS decision (candidate + implication scoring)
//!   -> policy -> approval -> broker -> observation
//!   -> NARS facts -> next model call
//! ```

pub mod actor;
pub mod driver;
pub mod planner;
pub mod prompt;
pub mod replay;

pub use actor::{ActorConfig, ActorOutput, Command, SessionActor};
pub use driver::{AgentRuntime, NoopSink, RunHandle, RuntimeConfig, RuntimeSink};
pub use planner::{NarsPlanner, PlannerSnapshot, ToolStat};
pub use replay::{project, verify_snapshot};
