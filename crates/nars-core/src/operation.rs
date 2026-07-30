//! Embeddable operation callbacks for agent actions.

use std::sync::Arc;

use crate::{Substitution, Term};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OperationFeedback {
    pub substitution: Option<Substitution>,
    pub failed: bool,
}

pub type OperationAction = Arc<dyn Fn(Term) -> OperationFeedback + Send + Sync + 'static>;

#[derive(Clone)]
pub struct Operation {
    pub term: Term,
    pub action: OperationAction,
    pub arguments: Vec<Term>,
    pub stdin_output: bool,
}

impl std::fmt::Debug for Operation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Operation")
            .field("term", &self.term)
            .field("arguments", &self.arguments)
            .field("stdin_output", &self.stdin_output)
            .finish_non_exhaustive()
    }
}
