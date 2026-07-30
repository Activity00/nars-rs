//! Concept storage for beliefs, goals, and learned implications.

use std::array;

use crate::config::OPERATIONS_MAX;
use crate::{Event, ImplicationTable, Term, Usage};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ConceptId(pub u64);

#[derive(Clone, Debug)]
pub struct Concept {
    pub id: ConceptId,
    pub usage: Usage,
    pub term: Term,
    pub belief: Event,
    pub belief_spike: Event,
    pub predicted_belief: Event,
    pub goal_spike: Event,
    pub precondition_beliefs: [ImplicationTable; OPERATIONS_MAX + 1],
    pub implication_links: ImplicationTable,
    pub priority: f64,
    pub process_id: [u64; 3],
    pub last_selection_time: i64,
}

impl Concept {
    #[must_use]
    pub fn new(id: ConceptId, term: Term, current_time: i64) -> Self {
        Self {
            id,
            usage: Usage {
                use_count: 1,
                last_used: current_time,
            },
            term,
            belief: Event::default(),
            belief_spike: Event::default(),
            predicted_belief: Event::default(),
            goal_spike: Event::default(),
            precondition_beliefs: array::from_fn(|_| ImplicationTable::default()),
            implication_links: ImplicationTable::default(),
            priority: 0.0,
            process_id: [0; 3],
            last_selection_time: 0,
        }
    }
}
