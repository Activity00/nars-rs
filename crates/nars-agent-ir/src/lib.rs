//! Typed protocol shared by the NARS agent runtime, providers, tools, API, and UI.
//!
//! The crate is intentionally free of async runtimes, HTTP clients, databases,
//! and direct NARS dependencies. It defines *what may cross a boundary*:
//!
//! - [`ir::AgentIr`]: the only shape a model may produce.
//! - [`tool`]: tool manifests, validated calls, results, observations, facts.
//! - [`event::AgentEvent`]: the append-only stream used for UI and replay.
//! - [`state::RunState`]: materialized state with a deterministic digest.
//! - [`validation`]: rejection rules for untrusted model output.

pub mod error;
pub mod event;
pub mod ids;
pub mod ir;
pub mod narsese;
pub mod state;
pub mod tool;
pub mod validation;

pub use error::AgentError;
pub use event::{AgentEvent, ModelPurpose, TokenUsage};
pub use ids::{EventId, ObservationId, RequestId, RunId, SessionId, ToolCallId};
pub use ir::{
    ActionParameters, AgentIr, CandidateAction, Clarification, FinalAnswer, GoalSpec, GoalType,
    TaskProposal,
};
pub use narsese::{NarsTerm, atomize, keyed_atom, short_hash};
pub use state::{ChatMessage, ChatRole, RunState, RunStatus, TaskSpec, VerificationCommand};
pub use tool::{
    Artifact, DecisionSource, Diff, NarsDecision, NarsFact, NarsGoal, Observation, Outcome,
    PlanStageView, ScoredCandidate, SelectedAction, SideEffect, TaskPlanView, ToolCall,
    ToolManifest, ToolPreview, ToolResult, ToolStatus, TruthValue,
};
pub use validation::{Severity, ValidatedIr, ValidationContext, ValidationIssue, validate_ir};
