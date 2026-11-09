//! Bounded concept memory and cycling event queues.
#![allow(clippy::cast_precision_loss, clippy::struct_excessive_bools)]

use std::collections::HashMap;

use crate::config::{
    CONCEPT_DURABILITY, CONCEPTS_MAX, CYCLING_BELIEF_EVENTS_MAX, CYCLING_GOAL_EVENTS_LAYERS,
    CYCLING_GOAL_EVENTS_MAX, EVENT_DURABILITY, MIN_CONFIDENCE, MIN_PRIORITY, OCCURRENCE_ETERNAL,
    OPERATIONS_MAX, UNIFICATION_DEPTH,
};
use crate::inference::revision_and_choice;
use crate::memory_snapshot::{MemorySnapshot, MemorySnapshotError};
use crate::{
    Concept, ConceptId, Event, EventType, Implication, InvertedAtomIndex, Narsese,
    OccurrenceTimeIndex, PriorityQueue, Stamp, Substitution, Term, Truth,
};

#[derive(Clone, Copy, Debug)]
pub struct AddEventOptions {
    pub priority: f64,
    pub input: bool,
    pub derived: bool,
    pub revised: bool,
    pub layer: usize,
    pub eternalize: bool,
    pub operation_slot: usize,
}

impl Default for AddEventOptions {
    fn default() -> Self {
        Self {
            priority: 1.0,
            input: false,
            derived: false,
            revised: false,
            layer: 0,
            eternalize: false,
            operation_slot: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Memory {
    concepts: Vec<Option<Concept>>,
    term_slots: HashMap<crate::Term, usize>,
    id_slots: HashMap<ConceptId, usize>,
    next_concept_id: u64,
    capacity: usize,
    pub inverted_index: InvertedAtomIndex,
    pub occurrence_time_index: OccurrenceTimeIndex<ConceptId>,
    pub cycling_beliefs: PriorityQueue<Event>,
    pub cycling_goals: Vec<PriorityQueue<Event>>,
    active_goal_layers: u32,
    pub restricted_creation: bool,
    derivation_restriction: Option<crate::Term>,
    has_declarative_implications: bool,
    declarative_concept_ids: Vec<ConceptId>,
    generic_input_procedural_concept_ids: Vec<ConceptId>,
    variable_procedural_concept_ids: Vec<ConceptId>,
}

impl Default for Memory {
    fn default() -> Self {
        Self::new(CONCEPTS_MAX)
    }
}

impl Memory {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            concepts: Vec::with_capacity(capacity),
            term_slots: HashMap::with_capacity(capacity),
            id_slots: HashMap::with_capacity(capacity),
            next_concept_id: 0,
            capacity,
            inverted_index: InvertedAtomIndex::new(),
            occurrence_time_index: OccurrenceTimeIndex::new(),
            cycling_beliefs: PriorityQueue::new(CYCLING_BELIEF_EVENTS_MAX),
            cycling_goals: (0..CYCLING_GOAL_EVENTS_LAYERS)
                .map(|_| PriorityQueue::new(CYCLING_GOAL_EVENTS_MAX))
                .collect(),
            active_goal_layers: 0,
            restricted_creation: false,
            derivation_restriction: None,
            has_declarative_implications: false,
            declarative_concept_ids: Vec::new(),
            generic_input_procedural_concept_ids: Vec::new(),
            variable_procedural_concept_ids: Vec::new(),
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.term_slots.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.term_slots.is_empty()
    }

    #[must_use]
    pub fn find(&self, term: &crate::Term) -> Option<&Concept> {
        self.term_slots
            .get(term)
            .and_then(|slot| self.concepts.get(*slot))
            .and_then(Option::as_ref)
    }

    pub fn find_mut(&mut self, term: &crate::Term) -> Option<&mut Concept> {
        let slot = *self.term_slots.get(term)?;
        self.concepts.get_mut(slot)?.as_mut()
    }

    #[must_use]
    pub fn by_id(&self, id: ConceptId) -> Option<&Concept> {
        self.id_slots
            .get(&id)
            .and_then(|slot| self.concepts.get(*slot))
            .and_then(Option::as_ref)
            .filter(|concept| concept.id == id)
    }

    pub fn by_id_mut(&mut self, id: ConceptId) -> Option<&mut Concept> {
        let slot = *self.id_slots.get(&id)?;
        self.concepts
            .get_mut(slot)?
            .as_mut()
            .filter(|concept| concept.id == id)
    }

    pub fn concepts(&self) -> impl Iterator<Item = &Concept> {
        self.concepts.iter().filter_map(Option::as_ref)
    }

    /// Captures durable knowledge as a serializable snapshot.
    ///
    /// Transient state (event queues, occurrence and atom indexes) is not
    /// stored; it is rebuilt from the concepts when the snapshot is loaded.
    #[must_use]
    pub fn to_snapshot(&self, language: &Narsese) -> MemorySnapshot {
        crate::memory_snapshot::snapshot_of(self, language)
    }

    /// Rebuilds memory from a snapshot, recomputing derived indexes.
    ///
    /// The atom table stored in the snapshot is rebuilt internally, so the
    /// restored terms keep their meaning even in a fresh process.
    ///
    /// # Errors
    ///
    /// Returns an error when the snapshot version is unknown, the atom table is
    /// invalid, or two concepts claim the same term.
    pub fn from_snapshot(snapshot: &MemorySnapshot) -> Result<Self, MemorySnapshotError> {
        crate::memory_snapshot::restore_memory(snapshot)
    }

    /// Highest evidence identifier present in stored events and implications.
    ///
    /// A restored runtime must continue numbering from here; otherwise new
    /// evidence reuses old ids and revision silently treats it as already
    /// counted.
    #[must_use]
    pub fn max_evidence_id(&self) -> u64 {
        let mut max = 0;
        for concept in self.concepts() {
            for event in [
                &concept.belief,
                &concept.belief_spike,
                &concept.predicted_belief,
                &concept.goal_spike,
            ] {
                max = max.max(event.stamp.iter().max().unwrap_or(0));
            }
            for table in &concept.precondition_beliefs {
                for implication in table.iter() {
                    max = max.max(implication.stamp.iter().max().unwrap_or(0));
                }
            }
            for implication in concept.implication_links.iter() {
                max = max.max(implication.stamp.iter().max().unwrap_or(0));
            }
        }
        max
    }

    pub(crate) const fn capacity(&self) -> usize {
        self.capacity
    }

    pub(crate) const fn next_concept_id(&self) -> u64 {
        self.next_concept_id
    }

    /// Creates an empty memory with a restored identifier counter.
    pub(crate) fn restore_base(capacity: usize, next_concept_id: u64) -> Self {
        let mut memory = Self::new(capacity);
        memory.next_concept_id = next_concept_id;
        memory
    }

    pub(crate) fn set_declarative_implications(&mut self, value: bool) {
        self.has_declarative_implications = value;
    }

    pub(crate) fn set_concept_id_lists(
        &mut self,
        declarative: Vec<ConceptId>,
        generic_input_procedural: Vec<ConceptId>,
        variable_procedural: Vec<ConceptId>,
    ) {
        self.declarative_concept_ids = declarative;
        self.generic_input_procedural_concept_ids = generic_input_procedural;
        self.variable_procedural_concept_ids = variable_procedural;
    }

    /// Inserts a restored concept without inference bookkeeping.
    ///
    /// Returns `false` when the term or identifier is already present.
    pub(crate) fn insert_restored(&mut self, language: &Narsese, concept: Concept) -> bool {
        if self.term_slots.contains_key(&concept.term) || self.id_slots.contains_key(&concept.id) {
            return false;
        }
        let slot = self.concepts.len();
        self.term_slots.insert(concept.term, slot);
        self.id_slots.insert(concept.id, slot);
        self.inverted_index.add(language, &concept.term, concept.id);
        self.concepts.push(Some(concept));
        true
    }

    #[must_use]
    pub const fn has_declarative_implications(&self) -> bool {
        self.has_declarative_implications
    }

    #[must_use]
    pub fn declarative_concept_ids(&self) -> &[ConceptId] {
        &self.declarative_concept_ids
    }

    #[must_use]
    pub fn generic_input_procedural_concept_ids(&self) -> &[ConceptId] {
        &self.generic_input_procedural_concept_ids
    }

    #[must_use]
    pub fn variable_procedural_concept_ids(&self) -> &[ConceptId] {
        &self.variable_procedural_concept_ids
    }

    #[must_use]
    pub fn related_concept_ids(&self, language: &Narsese, term: &crate::Term) -> Vec<ConceptId> {
        let mut result = Vec::new();
        if let Some(exact) = self.find(term) {
            result.push(exact.id);
        }
        for atom in term.atoms().iter().copied().take(UNIFICATION_DEPTH) {
            if !language.is_simple_atom(atom) {
                continue;
            }
            for id in self.inverted_index.concepts(atom) {
                if !result.contains(id) {
                    result.push(*id);
                }
            }
        }
        result
    }

    pub fn pop_belief(&mut self) -> Option<(Event, f64)> {
        self.cycling_beliefs
            .pop_max()
            .map(|item| (item.value, item.priority))
    }

    pub fn pop_goal(&mut self, layer: usize) -> Option<(Event, f64)> {
        let queue = self.cycling_goals.get_mut(layer)?;
        let item = queue.pop_max()?;
        if queue.is_empty() {
            self.active_goal_layers &= !(1_u32 << layer);
        }
        Some((item.value, item.priority))
    }

    #[must_use]
    pub fn next_goal_layer(&self, start: usize) -> Option<usize> {
        if start >= 32 {
            return None;
        }
        let remaining = self.active_goal_layers & (u32::MAX << start);
        (remaining != 0).then(|| usize::try_from(remaining.trailing_zeros()).unwrap_or(0))
    }

    pub fn clear_goals(&mut self) {
        for queue in &mut self.cycling_goals {
            while queue.pop_max().is_some() {}
        }
        self.active_goal_layers = 0;
    }

    pub fn relative_forgetting(&mut self) {
        for item in self.cycling_beliefs.iter_mut() {
            item.priority *= EVENT_DURABILITY;
        }
        for queue in &mut self.cycling_goals {
            for item in queue.iter_mut() {
                item.priority *= EVENT_DURABILITY;
            }
        }
        for concept in self.concepts.iter_mut().filter_map(Option::as_mut) {
            concept.priority *= CONCEPT_DURABILITY;
        }
    }

    pub fn conceptualize(
        &mut self,
        language: &Narsese,
        term: crate::Term,
        current_time: i64,
    ) -> Option<ConceptId> {
        if let Some(existing) = self.find(&term) {
            return Some(existing.id);
        }
        if self.capacity == 0 {
            return None;
        }

        let slot = if self.term_slots.len() < self.capacity {
            if let Some(empty) = self.concepts.iter().position(Option::is_none) {
                empty
            } else {
                self.concepts.push(None);
                self.concepts.len() - 1
            }
        } else {
            let (slot, evicted) = self
                .concepts
                .iter()
                .enumerate()
                .filter_map(|(slot, concept)| concept.as_ref().map(|concept| (slot, concept)))
                .min_by(|(_, left), (_, right)| {
                    left.usage
                        .usefulness(current_time)
                        .total_cmp(&right.usage.usefulness(current_time))
                })?;
            let evicted_id = evicted.id;
            let evicted_term = evicted.term;
            self.term_slots.remove(&evicted_term);
            self.id_slots.remove(&evicted_id);
            self.declarative_concept_ids.retain(|id| *id != evicted_id);
            self.generic_input_procedural_concept_ids
                .retain(|id| *id != evicted_id);
            self.variable_procedural_concept_ids
                .retain(|id| *id != evicted_id);
            self.inverted_index
                .remove(language, &evicted_term, evicted_id);
            slot
        };

        let id = ConceptId(self.next_concept_id);
        self.next_concept_id += 1;
        self.concepts[slot] = Some(Concept::new(id, term, current_time));
        self.term_slots.insert(term, slot);
        self.id_slots.insert(id, slot);
        self.inverted_index.add(language, &term, id);
        Some(id)
    }

    pub const fn restrict_derivations_to(&mut self, term: Option<crate::Term>) {
        self.derivation_restriction = term;
    }

    pub fn add_input_event(&mut self, language: &Narsese, event: Event, current_time: i64) -> bool {
        self.add_input_event_at_operation(language, event, current_time, 0)
    }

    pub fn add_input_event_at_operation(
        &mut self,
        language: &Narsese,
        event: Event,
        current_time: i64,
        operation_slot: usize,
    ) -> bool {
        self.add_event(
            language,
            event,
            current_time,
            AddEventOptions {
                priority: 1.0,
                input: true,
                eternalize: event.occurrence_time == OCCURRENCE_ETERNAL,
                operation_slot,
                ..AddEventOptions::default()
            },
        )
    }

    #[allow(clippy::too_many_lines)]
    pub fn add_event(
        &mut self,
        language: &Narsese,
        event: Event,
        current_time: i64,
        mut options: AddEventOptions,
    ) -> bool {
        if let Some(restriction) = self.derivation_restriction
            && !Substitution::unify(language, &restriction, &event.term).success
        {
            return false;
        }
        if event.event_type == EventType::Deleted
            || event.truth.confidence < MIN_CONFIDENCE
            || options.priority <= MIN_PRIORITY
        {
            return false;
        }
        if [':', '=']
            .into_iter()
            .any(|copula| language.copula_equals(event.term.root(), copula))
            && event.term.extract_subterm(1) == event.term.extract_subterm(2)
        {
            return false;
        }
        if self.derived_term_requires_existing_concept(language, &event, options.input)
            && self.find(&event.term).is_none()
        {
            return false;
        }
        if self.restricted_creation
            && !options.input
            && !language.copula_equals(event.term.root(), '$')
            && self.find(&event.term).is_none()
        {
            return false;
        }
        if !options.revised && !options.input {
            let complexity = event.term.complexity() as f64;
            options.priority *= 1.0 / (1.0 + complexity).log2();
        }

        match event.event_type {
            EventType::Belief => {
                let exclude_continuous_property = language.copula_equals(event.term.root(), ',')
                    && (language.copula_equals(event.term.atoms()[2], '=')
                        || language.copula_equals(event.term.atoms()[2], '+'));
                if !language.copula_equals(event.term.root(), '$')
                    && !exclude_continuous_property
                    && !event.stamp.has_duplicate()
                    && (options.input || !language.copula_equals(event.term.root(), ';'))
                {
                    self.add_cycling_event(event, options.priority, options.layer);
                }
                self.process_new_belief(
                    language,
                    event,
                    current_time,
                    options.priority,
                    options.input,
                    options.eternalize,
                    options.operation_slot,
                );
            }
            EventType::Goal => {
                if event.occurrence_time == OCCURRENCE_ETERNAL {
                    return false;
                }
                self.add_cycling_event(event, options.priority, options.layer);
            }
            EventType::Deleted => return false,
        }
        true
    }

    fn derived_term_requires_existing_concept(
        &self,
        language: &Narsese,
        event: &Event,
        input: bool,
    ) -> bool {
        if input {
            return false;
        }
        let term = &event.term;
        let is_image = |atom| {
            ['\\', '#', '/', '%']
                .into_iter()
                .any(|copula| language.copula_equals(atom, copula))
        };
        let root_requires_images =
            language.copula_equals(term.root(), ';') || language.copula_equals(term.root(), '?');
        if root_requires_images && term.atoms().iter().copied().any(is_image) {
            return true;
        }
        if self.derivation_restriction.is_some()
            || (event.stamp.evidential_base()[1] == 0
                && event.occurrence_time == OCCURRENCE_ETERNAL)
        {
            return false;
        }
        let top_level_image = term.atoms()[1..=2].iter().copied().any(is_image);
        let has_filtered_compound = term.atoms().iter().copied().any(|atom| {
            ['-', '~', '|', '&']
                .into_iter()
                .any(|copula| language.copula_equals(atom, copula))
        });
        let temporal_or_dependent_conjunction = language.copula_equals(term.root(), ';')
            && (event.occurrence_time != OCCURRENCE_ETERNAL
                || term
                    .atoms()
                    .iter()
                    .copied()
                    .any(|atom| language.is_dependent_variable(atom)));
        top_level_image || has_filtered_compound || temporal_or_dependent_conjunction
    }

    fn add_cycling_event(&mut self, event: Event, priority: f64, layer: usize) -> bool {
        if self.contains_stored_event(&event) {
            return false;
        }
        if event.event_type == EventType::Belief {
            return self.cycling_beliefs.push(priority, event).added;
        }
        let target = (layer + 1).min(CYCLING_GOAL_EVENTS_LAYERS - 1);
        let added = self.cycling_goals[target].push(priority, event).added;
        if added {
            self.active_goal_layers |= 1_u32 << target;
        }
        added
    }

    fn contains_stored_event(&mut self, event: &Event) -> bool {
        let in_queues = self
            .cycling_beliefs
            .iter()
            .chain(self.cycling_goals.iter().flat_map(PriorityQueue::iter))
            .any(|item| item.value.evidence_eq(event));
        if in_queues {
            return true;
        }
        self.find(&event.term)
            .is_some_and(|concept| match event.event_type {
                EventType::Belief if event.occurrence_time == OCCURRENCE_ETERNAL => {
                    concept.belief.event_type != EventType::Deleted
                        && concept.belief.evidence_eq(event)
                }
                EventType::Belief => {
                    concept.belief_spike.event_type != EventType::Deleted
                        && event.equal_term_stamp_less_confident_than(&concept.belief_spike)
                }
                EventType::Goal => {
                    concept.goal_spike.event_type != EventType::Deleted
                        && concept.goal_spike.evidence_eq(event)
                }
                EventType::Deleted => false,
            })
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn process_new_belief(
        &mut self,
        language: &Narsese,
        event: Event,
        current_time: i64,
        priority: f64,
        input: bool,
        eternalize: bool,
        operation_slot: usize,
    ) {
        let eternal_input = input && event.occurrence_time == OCCURRENCE_ETERNAL;
        let eternal_event = event.eternalized();

        if eternalize
            && (language.copula_equals(event.term.root(), '$')
                || language.copula_equals(event.term.root(), '?'))
        {
            let subject = event.term.extract_subterm(1);
            let predicate = event.term.extract_subterm(2);
            let Some(target_id) = self.conceptualize(language, predicate, current_time) else {
                return;
            };
            if let Some(target) = self.by_id_mut(target_id) {
                target.usage = target.usage.used(current_time, eternal_input);
            }

            let mut source_term = subject;
            if language.copula_equals(event.term.root(), '$')
                && language.copula_equals(subject.root(), '+')
            {
                let potential_operation = subject.extract_subterm(2);
                if language.is_operation(&potential_operation) {
                    if !language.is_executable_operation(&potential_operation) {
                        return;
                    }
                    source_term = language.precondition_without_operation(&subject);
                }
            }
            let Some(source_id) = self.conceptualize(language, source_term, current_time) else {
                return;
            };
            if let Some(source) = self.by_id_mut(source_id) {
                source.usage = source.usage.used(current_time, eternal_input);
            }
            if let Some(target_slot) = self.id_slots.get(&target_id).copied()
                && let Some(target) = self.concepts[target_slot].as_mut()
            {
                let implication = Implication {
                    term: event.term,
                    truth: eternal_event.truth,
                    stamp: eternal_event.stamp,
                    occurrence_time_offset: event.occurrence_time_offset,
                    source_concept_id: Some(source_id.0),
                    creation_time: current_time,
                };
                let declarative = language.copula_equals(event.term.root(), '?');
                let was_empty = declarative && target.implication_links.is_empty();
                let generic_input_procedural_was_empty = !declarative
                    && event.stamp.evidential_base()[1] == 0
                    && language.has_variable(&target.term, true, true, true)
                    && target
                        .precondition_beliefs
                        .iter()
                        .all(crate::ImplicationTable::is_empty);
                let variable_procedural_was_empty = !declarative
                    && language.has_variable(&target.term, true, true, true)
                    && target
                        .precondition_beliefs
                        .iter()
                        .all(crate::ImplicationTable::is_empty);
                let table = if declarative {
                    &mut target.implication_links
                } else {
                    let slot = operation_slot.min(OPERATIONS_MAX);
                    &mut target.precondition_beliefs[slot]
                };
                table.add_and_revise(language, implication);
                if declarative {
                    self.has_declarative_implications = true;
                    if was_empty {
                        self.declarative_concept_ids.push(target_id);
                    }
                } else if generic_input_procedural_was_empty {
                    self.generic_input_procedural_concept_ids.push(target_id);
                }
                if variable_procedural_was_empty {
                    self.variable_procedural_concept_ids.push(target_id);
                }
            }
        }

        let exclude_derived_implication = !input
            && (language.copula_equals(event.term.root(), '?')
                || language.copula_equals(event.term.root(), '^'));
        if language.copula_equals(event.term.root(), '$') || exclude_derived_implication {
            return;
        }
        let Some(id) = self.conceptualize(language, event.term, current_time) else {
            return;
        };
        let slot = self.id_slots[&id];
        let concept = self.concepts[slot]
            .as_mut()
            .expect("concept ID points to live slot");
        concept.usage = concept.usage.used(current_time, eternal_input);
        concept.priority = concept.priority.max(priority);
        if event.occurrence_time != OCCURRENCE_ETERNAL && event.occurrence_time <= current_time {
            concept.belief_spike = revision_and_choice(language, concept.belief_spike, event).0;
            concept.belief_spike.creation_time = current_time;
            if !language.is_continuous_property_statement(&event.term) {
                self.occurrence_time_index.add(id);
            }
        } else if event.occurrence_time > current_time {
            concept.predicted_belief =
                revision_and_choice(language, concept.predicted_belief, event).0;
            concept.predicted_belief.creation_time = current_time;
        }
        if eternalize {
            concept.belief = revision_and_choice(language, concept.belief, eternal_event).0;
            concept.belief.creation_time = current_time;
        }
    }

    #[must_use]
    pub fn implication_valid(&self, implication: &Implication) -> bool {
        implication
            .source_concept_id
            .is_some_and(|id| self.by_id(ConceptId(id)).is_some())
    }

    pub(crate) fn integrate_declarative_result(
        &mut self,
        language: &Narsese,
        event: Event,
        current_time: i64,
    ) -> bool {
        let Some(id) = self.conceptualize(language, event.term, current_time) else {
            return false;
        };
        let Some(concept) = self.by_id_mut(id) else {
            return false;
        };
        if event.occurrence_time == OCCURRENCE_ETERNAL {
            if concept.belief.stamp != event.stamp {
                concept.belief = revision_and_choice(language, concept.belief, event).0;
                concept.belief.creation_time = current_time;
            }
            return true;
        }

        if event.occurrence_time >= concept.belief_spike.occurrence_time {
            concept.belief_spike = revision_and_choice(language, concept.belief_spike, event).0;
            concept.belief_spike.creation_time = current_time;
            let eternal = concept.belief_spike.eternalized();
            concept.belief = revision_and_choice(language, concept.belief, eternal).0;
            concept.belief.creation_time = current_time;
        }
        true
    }

    /// Revises an existing learned implication with additional evidence.
    ///
    /// ONA uses this path for anticipation confirmations. The implication is
    /// located by its conclusion concept and exact implication term so that
    /// evidence is applied to the same contingency that produced a
    /// prediction, rather than creating a parallel link.
    pub fn revise_implication(
        &mut self,
        language: &Narsese,
        conclusion: &Term,
        implication_term: &Term,
        truth: Truth,
        stamp: Stamp,
        current_time: i64,
    ) -> bool {
        let Some(concept) = self.find_mut(conclusion) else {
            return false;
        };
        for table in &mut concept.precondition_beliefs {
            let Some(existing) = table
                .iter()
                .find(|implication| implication.term == *implication_term)
                .copied()
            else {
                continue;
            };
            let evidence = Implication {
                truth,
                stamp,
                creation_time: current_time,
                ..existing
            };
            return table.add_and_revise(language, evidence).is_some();
        }
        false
    }

    pub(crate) fn integrate_specific_implication(
        &mut self,
        language: &Narsese,
        event: Event,
        current_time: i64,
        operation_slot: usize,
    ) -> bool {
        if !language.copula_equals(event.term.root(), '$') {
            return false;
        }
        let precondition = event.term.extract_subterm(1);
        let conclusion = event.term.extract_subterm(2);
        let source_term = language.precondition_without_operation(&precondition);
        let Some(source_id) = self.conceptualize(language, source_term, current_time) else {
            return false;
        };
        let Some(target_id) = self.conceptualize(language, conclusion, current_time) else {
            return false;
        };
        let implication = Implication {
            term: event.term,
            truth: event.truth,
            stamp: event.stamp,
            occurrence_time_offset: event.occurrence_time_offset,
            source_concept_id: Some(source_id.0),
            creation_time: event.creation_time,
        };
        let Some(target) = self.by_id_mut(target_id) else {
            return false;
        };
        target.precondition_beliefs[operation_slot.min(OPERATIONS_MAX)]
            .add_and_revise(language, implication)
            .is_some()
    }

    #[must_use]
    pub fn temporal_link_truth(
        &self,
        precondition: &crate::Term,
        postcondition: &crate::Term,
    ) -> crate::Truth {
        let Some(concept) = self.find(postcondition) else {
            return crate::Truth::default();
        };
        concept.precondition_beliefs[0]
            .iter()
            .find_map(|implication| {
                let implication_precondition = implication.term.extract_subterm(1);
                let implication_postcondition = implication.term.extract_subterm(2);
                (implication_postcondition == *postcondition
                    && implication_precondition == *precondition)
                    .then_some(implication.truth)
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::{AddEventOptions, Memory};
    use crate::config::{ETERNAL_INPUT_USAGE_BOOST, OCCURRENCE_ETERNAL};
    use crate::{EventFactory, EventType, Narsese, Stamp, Truth};

    #[test]
    fn upstream_memory_concept_creation_test() {
        let mut language = Narsese::new();
        let mut memory = Memory::default();
        let mut events = EventFactory::new();
        let first_term = language.atomic_term("a").unwrap();
        let first = events.input_event(first_term, EventType::Belief, Truth::new(1.0, 0.9), 0.0, 0);
        assert!(memory.add_input_event(&language, first, 0));
        memory.conceptualize(&language, first_term, 1);
        assert!(memory.find(&first_term).is_some());

        let second_term = language.atomic_term("b").unwrap();
        let second =
            events.input_event(second_term, EventType::Belief, Truth::new(1.0, 0.9), 0.0, 0);
        assert!(memory.add_input_event(&language, second, 0));
        memory.conceptualize(&language, second_term, 1);
        assert!(memory.find(&second_term).is_some());
    }

    #[test]
    fn bounded_memory_recycles_least_useful_concept() {
        let mut language = Narsese::new();
        let mut memory = Memory::new(2);
        let a = language.atomic_term("a").unwrap();
        let b = language.atomic_term("b").unwrap();
        let c = language.atomic_term("c").unwrap();
        memory.conceptualize(&language, a, 0);
        memory.conceptualize(&language, b, 0);
        memory.find_mut(&b).unwrap().usage.use_count = 100;
        memory.conceptualize(&language, c, 10);
        assert!(memory.find(&a).is_none());
        assert!(memory.find(&b).is_some());
        assert!(memory.find(&c).is_some());
    }

    #[test]
    fn derived_compounds_require_an_existing_concept() {
        let mut language = Narsese::new();
        let compound = language.parse_term("(a & b)").unwrap();
        let event = crate::Event {
            term: compound,
            event_type: EventType::Belief,
            truth: Truth::new(1.0, 0.9),
            stamp: Stamp::from_evidence([1, 2]),
            occurrence_time: OCCURRENCE_ETERNAL,
            ..crate::Event::default()
        };

        let mut absent = Memory::default();
        assert!(!absent.add_event(
            &language,
            event,
            0,
            AddEventOptions {
                priority: 1.0,
                derived: true,
                eternalize: true,
                ..AddEventOptions::default()
            }
        ));
        assert!(absent.find(&compound).is_none());

        let mut existing = Memory::default();
        existing.conceptualize(&language, compound, 0);
        assert!(existing.add_event(
            &language,
            event,
            0,
            AddEventOptions {
                priority: 1.0,
                derived: true,
                eternalize: true,
                ..AddEventOptions::default()
            }
        ));

        let mut input = Memory::default();
        assert!(input.add_input_event(&language, event, 0));
        assert!(input.find(&compound).is_some());

        let image_conjunction = language.parse_term("((a /1 b) && c)").unwrap();
        let image_event = crate::Event {
            term: image_conjunction,
            event_type: EventType::Belief,
            truth: Truth::new(1.0, 0.9),
            stamp: Stamp::from_evidence([3]),
            occurrence_time: OCCURRENCE_ETERNAL,
            ..crate::Event::default()
        };
        let mut image_memory = Memory::default();
        assert!(!image_memory.add_event(
            &language,
            image_event,
            0,
            AddEventOptions {
                priority: 1.0,
                derived: true,
                eternalize: true,
                ..AddEventOptions::default()
            }
        ));

        let temporal_conjunction = language.parse_term("(a && b)").unwrap();
        let temporal_event = crate::Event {
            term: temporal_conjunction,
            event_type: EventType::Belief,
            truth: Truth::new(1.0, 0.9),
            stamp: Stamp::from_evidence([4]),
            occurrence_time: 0,
            ..crate::Event::default()
        };
        let mut temporal_memory = Memory::default();
        assert!(!temporal_memory.add_event(
            &language,
            temporal_event,
            0,
            AddEventOptions {
                priority: 1.0,
                derived: true,
                ..AddEventOptions::default()
            }
        ));
    }

    #[test]
    fn implication_input_updates_source_and_target_usage() {
        let mut language = Narsese::new();
        let implication = language.parse_term("<a =/> g>").unwrap();
        let mut event = EventFactory::new().input_event(
            implication,
            EventType::Belief,
            Truth::new(1.0, 0.9),
            1.0,
            0,
        );
        event.occurrence_time = OCCURRENCE_ETERNAL;
        let mut memory = Memory::default();
        assert!(memory.add_input_event(&language, event, 0));
        let source = language.parse_term("a").unwrap();
        let target = language.parse_term("g").unwrap();
        assert_eq!(
            memory.find(&source).unwrap().usage.use_count,
            ETERNAL_INPUT_USAGE_BOOST + 1
        );
        assert_eq!(
            memory.find(&target).unwrap().usage.use_count,
            ETERNAL_INPUT_USAGE_BOOST + 1
        );
    }

    #[test]
    fn continuous_properties_do_not_enter_occurrence_index() {
        let mut language = Narsese::new();
        let property = language.parse_term("<{position} |-> [x]>").unwrap();
        let ordinary = language.parse_term("state").unwrap();
        let mut events = EventFactory::new();
        let mut memory = Memory::default();
        let property_event =
            events.input_event(property, EventType::Belief, Truth::new(1.0, 0.9), 0.0, 0);
        assert!(memory.add_input_event(&language, property_event, 0));
        assert!(memory.occurrence_time_index.is_empty());

        let ordinary_event =
            events.input_event(ordinary, EventType::Belief, Truth::new(1.0, 0.9), 0.0, 0);
        assert!(memory.add_input_event(&language, ordinary_event, 0));
        assert_eq!(memory.occurrence_time_index.len(), 1);
    }

    #[test]
    fn compound_continuous_properties_do_not_enter_belief_cycling() {
        let mut language = Narsese::new();
        let mut events = EventFactory::new();
        for source in ["<{x} |-> <a <-> b>>", "<{x} |-> (a &/ b)>"] {
            let term = language.parse_term(source).unwrap();
            let event = events.input_event(term, EventType::Belief, Truth::new(1.0, 0.9), 0.0, 0);
            let mut memory = Memory::default();
            assert!(memory.add_input_event(&language, event, 0), "{source}");
            assert!(memory.cycling_beliefs.is_empty(), "{source}");
            assert_eq!(
                memory.find(&term).unwrap().belief_spike.event_type,
                EventType::Belief,
                "{source}"
            );
        }

        let ordinary_property = language.parse_term("<{x} |-> [value]>").unwrap();
        let event = events.input_event(
            ordinary_property,
            EventType::Belief,
            Truth::new(1.0, 0.9),
            0.0,
            0,
        );
        let mut memory = Memory::default();
        assert!(memory.add_input_event(&language, event, 0));
        assert_eq!(memory.cycling_beliefs.len(), 1);
    }
}
