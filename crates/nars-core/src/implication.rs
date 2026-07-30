//! Learned temporal or declarative implication.

use crate::{Stamp, Term, Truth};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Implication {
    pub term: Term,
    pub truth: Truth,
    pub stamp: Stamp,
    pub occurrence_time_offset: f64,
    pub source_concept_id: Option<u64>,
    pub creation_time: i64,
}
