//! Truth-expectation-ranked bounded implication table.

use crate::config::TABLE_SIZE;
use crate::inference::implication_revision;
use crate::{Implication, Narsese};

#[derive(Clone, Debug)]
pub struct ImplicationTable {
    entries: Vec<Implication>,
    capacity: usize,
}

impl Default for ImplicationTable {
    fn default() -> Self {
        Self::new(TABLE_SIZE)
    }
}

impl ImplicationTable {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
            capacity,
        }
    }

    /// Rebuilds a table from already-ordered entries, trimming to capacity.
    ///
    /// Restoring keeps the stored order instead of re-ranking, so a snapshot
    /// round trip cannot quietly reorder learned implications.
    #[must_use]
    pub fn from_entries(mut entries: Vec<Implication>) -> Self {
        let capacity = TABLE_SIZE;
        entries.truncate(capacity);
        Self { entries, capacity }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Implication> {
        self.entries.iter()
    }

    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Implication> {
        self.entries.get(index)
    }

    /// Inserts according to ONA's expectation/confidence ordering.
    ///
    /// Returns the insertion index, or `None` if a zero-capacity table cannot
    /// accept the item.
    pub fn add(&mut self, implication: Implication) -> Option<usize> {
        if self.capacity == 0 {
            return None;
        }
        let expectation = implication.truth.expectation();
        let insertion = (0..self.capacity).find(|index| {
            if *index == self.entries.len() {
                return true;
            }
            let Some(existing) = self.entries.get(*index) else {
                return false;
            };
            let same_term = existing.term == implication.term;
            (!same_term && expectation > existing.truth.expectation())
                || (same_term && implication.truth.confidence > existing.truth.confidence)
        })?;

        self.entries.insert(insertion, implication);
        if self.entries.len() > self.capacity {
            self.entries.pop();
        }
        Some(insertion)
    }

    pub fn remove(&mut self, index: usize) -> Option<Implication> {
        (index < self.entries.len()).then(|| self.entries.remove(index))
    }

    pub fn add_and_revise(&mut self, narsese: &Narsese, implication: Implication) -> Option<usize> {
        if let Some(index) = self
            .entries
            .iter()
            .position(|existing| existing.term == implication.term)
        {
            let old = self.entries.remove(index);
            self.add(implication_revision(narsese, old, implication))
        } else {
            self.add(implication)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::ImplicationTable;
    use crate::{Implication, Narsese, Stamp, Truth};

    #[test]
    fn upstream_table_test() {
        let mut language = Narsese::new();
        let term = language.atomic_term("test").unwrap();
        let mut table = ImplicationTable::default();
        for index in (1_u32..=240).rev() {
            table.add(Implication {
                term,
                truth: Truth::new(1.0, 1.0 / f64::from(index + 1)),
                stamp: Stamp::from_evidence([u64::from(index)]),
                occurrence_time_offset: 10.0,
                source_concept_id: Some(1),
                ..Implication::default()
            });
        }
        for (index, expected) in (1_u64..=120).enumerate() {
            assert_eq!(
                table.get(index).unwrap().stamp.evidential_base()[0],
                expected
            );
        }
        assert_eq!(table.get(0).unwrap().truth.confidence, 0.5);
        table.add_and_revise(
            &language,
            Implication {
                term,
                truth: Truth::new(1.0, 0.9),
                stamp: Stamp::from_evidence([241]),
                occurrence_time_offset: 10.0,
                source_concept_id: Some(1),
                ..Implication::default()
            },
        );
        assert!(table.get(0).unwrap().truth.confidence > 0.5);
    }
}
