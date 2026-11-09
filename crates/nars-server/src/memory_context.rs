//! Turns a stored memory snapshot into a compact prompt brief.

use nars_core::{ConceptRecord, MemorySnapshot, Narsese, decode_term};

/// Maximum number of concepts summarized for the model.
pub const MAX_BRIEF_CONCEPTS: usize = 12;

/// Renders the most relevant known concepts as `term [freq, conf]` lines.
///
/// Only concepts with an actual belief are considered, and ties fall back to
/// usage so the brief is deterministic for a given snapshot.
#[must_use]
pub fn memory_brief(snapshot: &MemorySnapshot) -> Vec<String> {
    let Ok(language) = Narsese::from_language_record(&snapshot.language) else {
        return Vec::new();
    };
    let mut concepts = snapshot
        .concepts
        .iter()
        .filter(|concept| concept.belief.confidence > 0.0)
        .collect::<Vec<&ConceptRecord>>();
    concepts.sort_by(|left, right| {
        right
            .priority
            .total_cmp(&left.priority)
            .then_with(|| right.usage.use_count.cmp(&left.usage.use_count))
            .then_with(|| left.term.cmp(&right.term))
    });
    concepts
        .into_iter()
        .take(MAX_BRIEF_CONCEPTS)
        .filter_map(|concept| {
            let term = decode_term(&concept.term).ok()?;
            if term.is_empty() {
                return None;
            }
            Some(format!(
                "{} [frequency {:.2}, confidence {:.2}]",
                language.format_term(&term),
                concept.belief.frequency,
                concept.belief.confidence
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nars_core::{EventFactory, EventType, Memory, Truth};

    #[test]
    fn summarizes_learned_concepts() {
        let mut language = Narsese::new();
        let mut memory = Memory::default();
        let mut events = EventFactory::new();
        for (name, frequency) in [("cat", 1.0), ("dog", 0.4)] {
            let term = language.atomic_term(name).unwrap();
            let event =
                events.input_event(term, EventType::Belief, Truth::new(frequency, 0.9), 0.0, 0);
            assert!(memory.add_input_event(&language, event, 0));
            let id = memory.conceptualize(&language, term, 1).unwrap();
            let concept = memory.by_id_mut(id).unwrap();
            concept.priority = frequency;
            concept.belief = event;
        }

        let brief = memory_brief(&memory.to_snapshot(&language));
        assert!(!brief.is_empty(), "{brief:?}");
        assert!(brief[0].starts_with("cat"), "{brief:?}");
        assert!(brief.iter().any(|line| line.contains("dog")), "{brief:?}");
    }

    #[test]
    fn empty_memory_has_an_empty_brief() {
        let language = Narsese::new();
        let memory = Memory::default();
        assert!(memory_brief(&memory.to_snapshot(&language)).is_empty());
    }

    #[test]
    fn brief_is_bounded() {
        let mut language = Narsese::new();
        let mut memory = Memory::default();
        let mut events = EventFactory::new();
        for index in 0..(MAX_BRIEF_CONCEPTS + 6) {
            let term = language.atomic_term(&format!("concept{index}")).unwrap();
            let event = events.input_event(term, EventType::Belief, Truth::new(1.0, 0.9), 0.0, 0);
            assert!(memory.add_input_event(&language, event, 0));
            let id = memory.conceptualize(&language, term, 1).unwrap();
            memory.by_id_mut(id).unwrap().belief = event;
        }
        let brief = memory_brief(&memory.to_snapshot(&language));
        assert_eq!(brief.len(), MAX_BRIEF_CONCEPTS);
    }
}
