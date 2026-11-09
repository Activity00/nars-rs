//! Turns a stored memory snapshot into a compact prompt brief.

use nars_agent_ir::{NarsFact, TruthValue};
use nars_core::{ConceptRecord, MemorySnapshot, Narsese, decode_term};
use serde::Serialize;

/// Maximum number of concepts summarized for the model.
pub const MAX_BRIEF_CONCEPTS: usize = 12;
/// Term prefixes that mark agent-level knowledge rather than repository facts.
///
/// These describe how tools and plan stages behave, so they stay useful in any
/// workspace. Repository terms (files, symbols, goals) never match.
pub const AGENT_TERM_PREFIXES: [&str; 6] = ["tool_", "step_", "plan_", "op_", "approval_", "run_"];

/// Maximum number of concepts returned by the memory inspection endpoint.
pub const MAX_INSPECT_CONCEPTS: usize = 20;
/// Maximum number of implications returned by the memory inspection endpoint.
pub const MAX_INSPECT_IMPLICATIONS: usize = 20;

/// Read-only view of a workspace's long-term memory for the API and UI.
#[derive(Clone, Debug, Serialize)]
pub struct MemorySummary {
    /// Canonical workspace path.
    pub workspace: String,
    /// Whether a memory file was found and could be decoded.
    pub available: bool,
    /// Number of stored concepts.
    pub concept_count: usize,
    /// Number of atoms in the stored term table.
    pub atom_count: usize,
    /// Total stored implications.
    pub implication_count: usize,
    /// Highest-priority concepts.
    pub top_concepts: Vec<MemoryConceptView>,
    /// Strongest learned implications.
    pub top_implications: Vec<MemoryImplicationView>,
}

/// One concept in the inspection view.
#[derive(Clone, Debug, Serialize)]
pub struct MemoryConceptView {
    /// Narsese rendering of the concept term.
    pub term: String,
    /// Concept priority.
    pub priority: f64,
    /// Number of times the concept was used.
    pub use_count: u64,
    /// Belief frequency.
    pub frequency: f64,
    /// Belief confidence.
    pub confidence: f64,
    /// Learned implications attached to this concept.
    pub implications: usize,
}

/// One learned implication in the inspection view.
#[derive(Clone, Debug, Serialize)]
pub struct MemoryImplicationView {
    /// Condition concept.
    pub from: String,
    /// Consequent concept.
    pub to: String,
    /// Implication frequency.
    pub frequency: f64,
    /// Implication confidence.
    pub confidence: f64,
}

impl MemorySummary {
    fn empty(workspace: &str, available: bool) -> Self {
        Self {
            workspace: workspace.to_owned(),
            available,
            concept_count: 0,
            atom_count: 0,
            implication_count: 0,
            top_concepts: Vec::new(),
            top_implications: Vec::new(),
        }
    }
}

/// Selects agent-level beliefs that are worth sharing across workspaces.
///
/// Only atomic terms with an agent vocabulary prefix are promoted: they encode
/// how the agent itself behaves (tool outcomes, plan progress) rather than
/// facts about one repository.
#[must_use]
pub fn promote_facts(snapshot: &MemorySnapshot) -> Vec<NarsFact> {
    let Ok(language) = Narsese::from_language_record(&snapshot.language) else {
        return Vec::new();
    };
    let mut facts = Vec::new();
    for concept in &snapshot.concepts {
        if concept.belief.confidence <= 0.0 {
            continue;
        }
        let Ok(term) = decode_term(&concept.term) else {
            continue;
        };
        let rendered = language.format_term(&term);
        if !is_agent_term(&rendered) {
            continue;
        }
        facts.push(NarsFact {
            term: rendered,
            truth: TruthValue {
                frequency: concept.belief.frequency,
                confidence: concept.belief.confidence,
            },
            eternal: concept.belief.occurrence_time < 0,
        });
    }
    facts
}

fn is_agent_term(term: &str) -> bool {
    !term.contains(' ')
        && !term.contains('(')
        && AGENT_TERM_PREFIXES
            .iter()
            .any(|prefix| term.starts_with(prefix))
}

/// Builds the inspection view from a stored snapshot.
#[must_use]
pub fn summary(workspace: &str, snapshot: Option<&MemorySnapshot>) -> MemorySummary {
    let Some(snapshot) = snapshot else {
        return MemorySummary::empty(workspace, false);
    };
    let Ok(language) = Narsese::from_language_record(&snapshot.language) else {
        return MemorySummary::empty(workspace, false);
    };

    let mut concepts = snapshot.concepts.iter().collect::<Vec<&ConceptRecord>>();
    concepts.sort_by(|left, right| {
        right
            .priority
            .total_cmp(&left.priority)
            .then_with(|| right.usage.use_count.cmp(&left.usage.use_count))
            .then_with(|| left.term.cmp(&right.term))
    });

    let implication_count = concepts
        .iter()
        .map(|concept| implication_count_of(concept))
        .sum::<usize>();

    let top_concepts = concepts
        .iter()
        .take(MAX_INSPECT_CONCEPTS)
        .filter_map(|concept| {
            let term = decode_term(&concept.term).ok()?;
            if term.is_empty() {
                return None;
            }
            Some(MemoryConceptView {
                term: language.format_term(&term),
                priority: concept.priority,
                use_count: concept.usage.use_count,
                frequency: concept.belief.frequency,
                confidence: concept.belief.confidence,
                implications: implication_count_of(concept),
            })
        })
        .collect::<Vec<_>>();

    let mut implications = Vec::new();
    for concept in &concepts {
        let Ok(from) = decode_term(&concept.term) else {
            continue;
        };
        let from = language.format_term(&from);
        for implication in &concept.implication_links {
            let Ok(to) = decode_term(&implication.term) else {
                continue;
            };
            implications.push(MemoryImplicationView {
                from: from.clone(),
                to: language.format_term(&to),
                frequency: implication.frequency,
                confidence: implication.confidence,
            });
        }
    }
    implications.sort_by(|left, right| {
        expectation(right.frequency, right.confidence)
            .total_cmp(&expectation(left.frequency, left.confidence))
            .then_with(|| left.from.cmp(&right.from))
            .then_with(|| left.to.cmp(&right.to))
    });
    implications.truncate(MAX_INSPECT_IMPLICATIONS);

    MemorySummary {
        workspace: workspace.to_owned(),
        available: true,
        concept_count: snapshot.concepts.len(),
        atom_count: snapshot.language.atoms.len(),
        implication_count,
        top_concepts,
        top_implications: implications,
    }
}

fn implication_count_of(concept: &ConceptRecord) -> usize {
    concept.implication_links.len()
        + concept
            .precondition_beliefs
            .iter()
            .map(Vec::len)
            .sum::<usize>()
}

fn expectation(frequency: f64, confidence: f64) -> f64 {
    confidence.mul_add(frequency - 0.5, 0.5)
}

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
    fn promotes_only_agent_vocabulary_facts() {
        let mut language = Narsese::new();
        let mut memory = Memory::default();
        let mut events = EventFactory::new();
        for name in ["tool_read_file_observed_success", "cat"] {
            let term = language.atomic_term(name).unwrap();
            let event = events.input_event(term, EventType::Belief, Truth::new(1.0, 0.9), 0.0, 0);
            assert!(memory.add_input_event(&language, event, 0));
            let id = memory.conceptualize(&language, term, 1).unwrap();
            memory.by_id_mut(id).unwrap().belief = event;
        }

        let facts = promote_facts(&memory.to_snapshot(&language));
        assert_eq!(facts.len(), 1, "{facts:?}");
        assert_eq!(facts[0].term, "tool_read_file_observed_success");
        assert!(facts[0].truth.confidence > 0.0);
    }

    #[test]
    fn summary_reports_stored_concepts_and_implications() {
        let mut language = Narsese::new();
        let mut memory = Memory::default();
        let mut events = EventFactory::new();
        let term = language.atomic_term("cat").unwrap();
        let event = events.input_event(term, EventType::Belief, Truth::new(1.0, 0.9), 0.0, 0);
        assert!(memory.add_input_event(&language, event, 0));
        let id = memory.conceptualize(&language, term, 1).unwrap();
        let concept = memory.by_id_mut(id).unwrap();
        concept.belief = event;
        concept.priority = 0.8;
        let consequent = language.atomic_term("animal").unwrap();
        let _ = concept.implication_links.add(nars_core::Implication {
            term: consequent,
            truth: Truth::new(0.9, 0.8),
            stamp: nars_core::Stamp::from_evidence([1]),
            occurrence_time_offset: 0.0,
            source_concept_id: Some(id.0),
            creation_time: 1,
        });

        let view = summary("/tmp/ws", Some(&memory.to_snapshot(&language)));
        assert!(view.available);
        assert_eq!(view.concept_count, 1);
        assert_eq!(view.implication_count, 1);
        assert_eq!(view.top_concepts[0].term, "cat");
        assert_eq!(view.top_concepts[0].implications, 1);
        assert_eq!(view.top_implications[0].from, "cat");
        assert_eq!(view.top_implications[0].to, "animal");
    }

    #[test]
    fn summary_marks_missing_memory_unavailable() {
        let view = summary("/tmp/ws", None);
        assert!(!view.available);
        assert_eq!(view.concept_count, 0);
        assert!(view.top_concepts.is_empty());
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
