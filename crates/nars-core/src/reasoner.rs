//! Integrated reasoning runtime owning language, memory, rules, and clock.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::Arc;

use crate::Implication;
use crate::config::{
    ANTICIPATION_CONFIDENCE, ANTICIPATION_THRESHOLD, BELIEF_CONCEPT_MATCH_TARGET,
    BELIEF_LAST_USED_TOLERANCE, CONCEPTS_MAX, CONDITION_THRESHOLD, CYCLING_BELIEF_EVENTS_MAX,
    CYCLING_GOAL_EVENTS_MAX, DECISION_THRESHOLD, EVENT_BELIEF_DISTANCE,
    LEARNED_STATE_LOOKAHEAD_MAX_IMPLICATIONS, MAX_SEQUENCE_LEN, MAX_SEQUENCE_TIMEDIFF,
    MOTOR_BABBLING_CHANCE, MOTOR_BABBLING_SUPPRESSION_THRESHOLD, MY_RAND_MAX, OCCURRENCE_ETERNAL,
    OPERATIONS_BABBLE_ARGS_MAX, OPERATIONS_MAX, SUBSUMPTION_CONFIDENCE_THRESHOLD,
    SUBSUMPTION_FREQUENCY_THRESHOLD, TOP_K_DECLARATIVE_IMPLICATIONS,
};
use crate::inference::{
    belief_induction, belief_intersection, goal_deduction, revision_and_choice,
};
use crate::memory_snapshot::MemorySnapshot;
use crate::variable::{
    UnifyOptions, introduce_conjunction_variables, introduce_implication_variables,
};
use crate::{
    AddEventOptions, Atom, AtomIndexSnapshot, ConceptSnapshot, Event, EventFactory, EventSnapshot,
    EventType, ImplicationSnapshot, Memory, Narsese, OccurrenceIndexSnapshot, Operation,
    OperationAction, OperationConfigSnapshot, OperationFeedback, OperationSnapshot, Punctuation,
    RuleEngine, RuntimeStats, Stamp, Tense, Term, Truth,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReasonerError(String);

impl ReasonerError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ReasonerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for ReasonerError {}

/// Best matching belief or implication returned for a Narsese question.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Answer {
    pub term: Term,
    pub truth: Truth,
    pub stamp: Stamp,
    pub occurrence_time: i64,
    pub creation_time: i64,
}

#[derive(Clone, Debug)]
pub struct Reasoner {
    pub language: Narsese,
    pub memory: Memory,
    pub rules: RuleEngine,
    events: EventFactory,
    operations: Vec<Operation>,
    anticipations: Vec<PendingAnticipation>,
    motor_babbling_chance: f64,
    babbling_operations: usize,
    decision_threshold: f64,
    anticipation_confidence: f64,
    question_priming: f64,
    recent_question_answers: Vec<(Term, i64)>,
    similarity_distance: f64,
    random_state: u64,
    current_time: i64,
    forward_rules: Vec<(Term, Term, Truth)>,
}

#[derive(Clone, Copy, Debug)]
struct DecisionCandidate {
    operation_atom: Atom,
    precondition: Term,
    grounded: bool,
    goal_depth: usize,
    fresh_context: bool,
    context_time: i64,
    specificity: usize,
    score: f64,
    context_truth: Truth,
    missing_specific_implication: Option<crate::Implication>,
}

#[derive(Clone, Copy, Debug)]
struct PendingAnticipation {
    expected_term: Term,
    implication_term: Term,
    precondition_truth: Truth,
    occurrence_time_offset: i64,
    deadline: i64,
    implication_stamp: Stamp,
    implication_creation_time: i64,
    create_if_missing: bool,
}

impl Reasoner {
    /// Constructs a NAL-6 runtime from the bundled ONA rules.
    ///
    /// # Errors
    ///
    /// Returns an error if the bundled rule table cannot be loaded.
    pub fn new() -> Result<Self, ReasonerError> {
        let mut language = Narsese::new();
        let rules = RuleEngine::ona(&mut language)
            .map_err(|error| ReasonerError::new(error.to_string()))?;
        Ok(Self {
            language,
            memory: Memory::default(),
            rules,
            events: EventFactory::new(),
            operations: Vec::with_capacity(OPERATIONS_MAX),
            anticipations: Vec::new(),
            motor_babbling_chance: MOTOR_BABBLING_CHANCE,
            babbling_operations: OPERATIONS_MAX,
            decision_threshold: DECISION_THRESHOLD,
            anticipation_confidence: ANTICIPATION_CONFIDENCE,
            question_priming: 0.1,
            recent_question_answers: Vec::new(),
            similarity_distance: 1.0,
            random_state: 1,
            current_time: 1,
            forward_rules: Vec::new(),
        })
    }

    /// Builds a runtime on top of stored long-term memory.
    ///
    /// The stored atom table must extend this build's built-in table: atoms are
    /// numeric ids, so a shifted prefix would change what every stored term
    /// means.
    ///
    /// # Errors
    ///
    /// Returns an error when the atom table is incompatible or the memory
    /// snapshot is malformed.
    pub fn from_snapshot(snapshot: &MemorySnapshot) -> Result<Self, ReasonerError> {
        // A build that changes the built-in atom table shifts every id, so the
        // stored terms have to be remapped by atom name before they can be
        // trusted.
        let migrated;
        let snapshot = if crate::memory_snapshot::is_compatible(snapshot) {
            snapshot
        } else {
            migrated = crate::memory_snapshot::migrate(snapshot)
                .map_err(|error| ReasonerError::new(error.to_string()))?;
            &migrated
        };

        let stored = Narsese::from_language_record(&snapshot.language)
            .map_err(|error| ReasonerError::new(error.to_string()))?;

        let mut reasoner = Self::new()?;
        reasoner.language = stored;
        reasoner.rules = RuleEngine::ona(&mut reasoner.language)
            .map_err(|error| ReasonerError::new(error.to_string()))?;
        reasoner.memory = Memory::from_snapshot(snapshot)
            .map_err(|error| ReasonerError::new(error.to_string()))?;
        reasoner.set_next_evidence_id(reasoner.memory.max_evidence_id().saturating_add(1));
        Ok(reasoner)
    }

    /// Ages long-term memory and drops concepts that are no longer useful.
    ///
    /// Called once per new run so an old workspace does not keep stale
    /// priorities at the top of the decision order forever. Returns the number
    /// of pruned concepts.
    pub fn maintain_memory(&mut self) -> usize {
        self.memory.relative_forgetting();
        let now = self.current_time;
        let language = &self.language;
        self.memory.prune_stale(language, now)
    }

    /// Stores a learned `<precondition =/> goal>` implication directly in the
    /// consequence concept's precondition table.
    ///
    /// The normal sentence path drops operation-statement preconditions before
    /// they reach that table, so learned outcomes are written explicitly here
    /// with a valid source concept.
    pub fn store_implication_sentence(&mut self, source: &str) -> bool {
        let Ok(sentence) = self.language.parse_sentence(source) else {
            return false;
        };
        let term = sentence.term;
        let precondition = term.extract_subterm(1);
        let conclusion = term.extract_subterm(2);
        if precondition.is_empty() || conclusion.is_empty() {
            return false;
        }
        let now = self.current_time;
        let slot = self.operation_slot_for_implication(&term);
        let event = self
            .events
            .input_event(term, EventType::Belief, sentence.truth, 0.0, now);
        let source_term = self.language.precondition_without_operation(&precondition);
        let language = &self.language;
        let memory = &mut self.memory;
        let Some(source_id) = memory.conceptualize(language, source_term, now) else {
            return false;
        };
        let Some(target_id) = memory.conceptualize(language, conclusion, now) else {
            return false;
        };
        let implication = Implication {
            term,
            truth: event.truth,
            stamp: event.stamp,
            occurrence_time_offset: 0.0,
            source_concept_id: Some(source_id.0),
            creation_time: event.creation_time,
        };
        let Some(target) = memory.by_id_mut(target_id) else {
            return false;
        };
        target.precondition_beliefs[slot.min(OPERATIONS_MAX)]
            .add_and_revise(language, implication)
            .is_some()
    }

    /// Records how a tool fared for a goal, as a direct memory implication.
    pub fn learn_tool_outcome(&mut self, goal: &str, tool: &str, success: bool) -> bool {
        let action = format!("act_{tool}");
        let sentence = format!(
            "<(({{SELF}} * {action}) --> ^{tool}) =/> {goal}>. {{{} 0.9}}",
            if success { "1.0" } else { "0.0" }
        );
        self.store_implication_sentence(&sentence)
    }

    /// Ranks operations by learned implication expectation for `goal`.
    pub fn rank_goal_operations(&mut self, goal: &str) -> Vec<(String, f64)> {
        let Ok(goal_term) = self.language.parse_term(goal) else {
            return Vec::new();
        };
        let Some(concept) = self.memory.find(&goal_term) else {
            return Vec::new();
        };
        let mut best: BTreeMap<String, f64> = BTreeMap::new();
        let implications = concept
            .precondition_beliefs
            .iter()
            .flat_map(|table| table.iter())
            .chain(concept.implication_links.iter());
        for implication in implications {
            let precondition = implication.term.extract_subterm(1);
            let seeded = precondition.atoms().iter().any(|atom| {
                *atom != crate::term::ATOM_FREE
                    && self
                        .language
                        .atom_name(*atom)
                        .is_some_and(|name| name.starts_with("ctx_"))
            });
            if seeded {
                continue;
            }
            let mut components = Vec::new();
            flatten_sequence(&self.language, precondition, &mut components);
            for component in components {
                let Some(atom) = self.language.operation_atom(&component) else {
                    continue;
                };
                let Some(name) = self.language.atom_name(atom) else {
                    continue;
                };
                let Some(tool) = name.strip_prefix('^') else {
                    continue;
                };
                let score = implication.truth.expectation();
                let entry = best.entry(tool.to_owned()).or_insert(f64::MIN);
                *entry = entry.max(score);
            }
        }
        let mut ranked = best.into_iter().collect::<Vec<_>>();
        ranked.sort_by(|left, right| right.1.total_cmp(&left.1));
        ranked
    }

    /// Retracts a belief so it no longer supports derived decisions.
    ///
    /// Used when later evidence contradicts an earlier conclusion, e.g. a
    /// failed verification invalidating the change stage.
    pub fn retract_belief(&mut self, term: &str) -> bool {
        let Ok(term) = self.language.parse_term(term) else {
            return false;
        };
        let Some(concept) = self.memory.find_mut(&term) else {
            return false;
        };
        concept.belief.truth = Truth::default();
        concept.belief_spike.truth = Truth::default();
        true
    }

    /// Expectation of a stored belief, by rendered term text.
    pub fn belief_expectation(&mut self, term: &str) -> Option<f64> {
        let term = self.language.parse_term(term).ok()?;
        let concept = self.memory.find(&term)?;
        (concept.belief.truth.confidence > 0.0).then(|| concept.belief.truth.expectation())
    }

    /// Mutable atom table, for interning terms derived from runtime state.
    pub fn language_mut(&mut self) -> &mut Narsese {
        &mut self.language
    }

    /// The atom table shared by every term in this runtime.
    #[must_use]
    pub const fn language(&self) -> &Narsese {
        &self.language
    }

    /// Current long-term memory.
    #[must_use]
    pub const fn memory(&self) -> &Memory {
        &self.memory
    }

    /// Replaces long-term memory, for example after loading a snapshot.
    pub fn set_memory(&mut self, memory: Memory) {
        self.memory = memory;
    }

    #[must_use]
    pub const fn current_time(&self) -> i64 {
        self.current_time
    }

    pub fn set_motor_babbling_chance(&mut self, chance: f64) {
        self.motor_babbling_chance = chance.clamp(0.0, 1.0);
    }

    #[must_use]
    pub const fn motor_babbling_chance(&self) -> f64 {
        self.motor_babbling_chance
    }

    /// Selects how many leading registered operations participate in babbling.
    ///
    /// # Errors
    ///
    /// Returns an error when the count exceeds ONA's fixed operation capacity.
    pub fn set_babbling_operations(&mut self, count: usize) -> Result<(), ReasonerError> {
        if count > OPERATIONS_MAX {
            return Err(ReasonerError::new(
                "babbling operation count is out of bounds",
            ));
        }
        self.babbling_operations = count;
        Ok(())
    }

    #[must_use]
    pub const fn babbling_operations(&self) -> usize {
        self.babbling_operations
    }

    #[must_use]
    pub const fn decision_threshold(&self) -> f64 {
        self.decision_threshold
    }

    #[must_use]
    pub const fn anticipation_confidence(&self) -> f64 {
        self.anticipation_confidence
    }

    #[must_use]
    pub const fn question_priming(&self) -> f64 {
        self.question_priming
    }

    #[must_use]
    pub const fn similarity_distance(&self) -> f64 {
        self.similarity_distance
    }

    /// Sets the minimum desire required for action execution.
    ///
    /// # Errors
    ///
    /// Returns an error unless the threshold is finite and in 0..=1.
    pub fn set_decision_threshold(&mut self, threshold: f64) -> Result<(), ReasonerError> {
        validate_probability(threshold, "decision threshold")?;
        self.decision_threshold = threshold;
        Ok(())
    }

    /// Sets the confidence assigned to failed anticipations.
    ///
    /// # Errors
    ///
    /// Returns an error unless the confidence is finite and in 0..=1.
    pub fn set_anticipation_confidence(&mut self, confidence: f64) -> Result<(), ReasonerError> {
        validate_probability(confidence, "anticipation confidence")?;
        self.anticipation_confidence = confidence;
        Ok(())
    }

    /// Sets the concept priority boost applied to passive answers.
    ///
    /// # Errors
    ///
    /// Returns an error unless the priority is finite and in 0..=1.
    pub fn set_question_priming(&mut self, priority: f64) -> Result<(), ReasonerError> {
        validate_probability(priority, "question priming")?;
        self.question_priming = priority;
        Ok(())
    }

    /// Sets the numeric analogy distance scale for configurable unification.
    ///
    /// # Errors
    ///
    /// Returns an error unless the distance is finite and positive.
    pub fn set_similarity_distance(&mut self, distance: f64) -> Result<(), ReasonerError> {
        if !distance.is_finite() || distance <= 0.0 {
            return Err(ReasonerError::new("similarity distance must be positive"));
        }
        self.similarity_distance = distance;
        Ok(())
    }

    pub const fn set_current_time(&mut self, current_time: i64) {
        self.current_time = current_time;
    }

    pub const fn set_next_evidence_id(&mut self, evidence_id: u64) {
        self.events.set_next_evidence_id(evidence_id);
    }

    pub const fn set_import_stamp(&mut self, stamp: Stamp) {
        self.events.set_import_stamp(stamp);
    }

    pub const fn set_restricted_concept_creation(&mut self, restricted: bool) {
        self.memory.restricted_creation = restricted;
    }

    pub const fn set_random_seed(&mut self, seed: u64) {
        self.random_state = seed;
    }

    /// ONA's reproducible POSIX-style 15-bit pseudo-random generator.
    pub fn random_u15(&mut self) -> u32 {
        self.random_state = self
            .random_state
            .wrapping_mul(1_103_515_245)
            .wrapping_add(12_345);
        u32::try_from((self.random_state / 65_536) % 32_768).unwrap_or(0)
    }

    /// Adds a belief and performs ONA's input-triggered cycle.
    ///
    /// # Errors
    ///
    /// Returns an error if parsing fails or memory rejects the input.
    pub fn add_belief(
        &mut self,
        source: &str,
        truth: Truth,
        eternal: bool,
    ) -> Result<Event, ReasonerError> {
        let term = self
            .language
            .parse_term(source)
            .map_err(|error| ReasonerError::new(error.to_string()))?;
        self.add_belief_term(term, truth, eternal, 0.0)
    }

    fn add_belief_term(
        &mut self,
        term: Term,
        truth: Truth,
        eternal: bool,
        occurrence_time_offset: f64,
    ) -> Result<Event, ReasonerError> {
        let mut event = self.events.input_event(
            term,
            EventType::Belief,
            truth,
            occurrence_time_offset,
            self.current_time,
        );
        if eternal {
            event.occurrence_time = OCCURRENCE_ETERNAL;
        }
        if !eternal {
            self.confirm_anticipations(event);
        }
        let stored_event = Event {
            processed: !eternal,
            ..event
        };
        if self.language.copula_equals(stored_event.term.root(), '?') {
            let subject = stored_event.term.extract_subterm(1);
            let consequent = stored_event.term.extract_subterm(2);
            if !subject.is_empty() && !consequent.is_empty() {
                self.forward_rules
                    .push((subject, consequent, stored_event.truth));
            }
        }
        let operation_slot = self.operation_slot_for_implication(&stored_event.term);
        if !self.memory.add_input_event_at_operation(
            &self.language,
            stored_event,
            self.current_time,
            operation_slot,
        ) {
            return Err(ReasonerError::new("memory rejected belief input"));
        }
        if !eternal {
            self.learn_from_input_event(event);
            if self.registered_operation(&event.term).is_some() {
                self.arm_anticipations(event);
            } else {
                self.anticipate_nonoperation_input(event);
            }
        }
        self.cycle();
        Ok(event)
    }

    /// Registers an executable atomic operator such as `^move`.
    ///
    /// # Errors
    ///
    /// Re-registering a name replaces its callback in place, as in ONA.
    /// Returns an error for a non-operator name or a full ONA-compatible
    /// operation table.
    pub fn add_operation<F>(&mut self, name: &str, action: F) -> Result<usize, ReasonerError>
    where
        F: Fn(Term) -> OperationFeedback + Send + Sync + 'static,
    {
        let term = self
            .language
            .atomic_term(name)
            .map_err(|error| ReasonerError::new(error.to_string()))?;
        if !self.language.is_operator(term.root()) {
            return Err(ReasonerError::new("operation names must start with '^'"));
        }
        let action = Arc::new(action) as OperationAction;
        if let Some((index, operation)) = self
            .operations
            .iter_mut()
            .enumerate()
            .find(|(_, operation)| operation.term.root() == term.root())
        {
            operation.action = action;
            return Ok(index + 1);
        }
        if self.operations.len() >= OPERATIONS_MAX {
            return Err(ReasonerError::new("operation table is full"));
        }
        self.operations.push(Operation {
            term,
            action,
            arguments: Vec::new(),
            stdin_output: true,
        });
        Ok(self.operations.len())
    }

    /// Configures the finite argument set used when this operation is explored.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown operation, invalid argument term, or
    /// more than ONA's ten configured exploration arguments.
    pub fn set_operation_arguments(
        &mut self,
        name: &str,
        arguments: &[&str],
    ) -> Result<(), ReasonerError> {
        if arguments.len() > OPERATIONS_BABBLE_ARGS_MAX {
            return Err(ReasonerError::new("too many motor-babbling arguments"));
        }
        let operation_term = self
            .language
            .atomic_term(name)
            .map_err(|error| ReasonerError::new(error.to_string()))?;
        let parsed = arguments
            .iter()
            .map(|argument| {
                self.language
                    .parse_term(argument)
                    .map_err(|error| ReasonerError::new(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let operation = self
            .operations
            .iter_mut()
            .find(|operation| operation.term == operation_term)
            .ok_or_else(|| ReasonerError::new("operation is not registered"))?;
        operation.arguments = parsed;
        Ok(())
    }

    /// Reassigns a one-based operation slot while preserving its callback.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid slot, name, or duplicate operation.
    pub fn set_operation_name(&mut self, slot: usize, name: &str) -> Result<(), ReasonerError> {
        let index = slot
            .checked_sub(1)
            .filter(|index| *index < self.operations.len())
            .ok_or_else(|| ReasonerError::new("operation slot is out of bounds"))?;
        let term = self
            .language
            .atomic_term(name)
            .map_err(|error| ReasonerError::new(error.to_string()))?;
        if !self.language.is_operator(term.root()) {
            return Err(ReasonerError::new("operation names must start with '^'"));
        }
        if let Some(existing) = self
            .operations
            .iter()
            .position(|operation| operation.term == term)
        {
            for operation in &mut self.operations[existing..] {
                operation.term = Term::EMPTY;
            }
        }
        self.operations[index].term = term;
        Ok(())
    }

    /// Assigns one motor-babbling argument to a one-based operation slot.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid operation/argument slot or term.
    pub fn set_operation_argument(
        &mut self,
        operation_slot: usize,
        argument_slot: usize,
        source: &str,
    ) -> Result<(), ReasonerError> {
        let operation_index = operation_slot
            .checked_sub(1)
            .filter(|index| *index < self.operations.len())
            .ok_or_else(|| ReasonerError::new("operation slot is out of bounds"))?;
        let argument_index = argument_slot
            .checked_sub(1)
            .filter(|index| *index < OPERATIONS_BABBLE_ARGS_MAX)
            .ok_or_else(|| ReasonerError::new("operation argument slot is out of bounds"))?;
        let argument = self
            .language
            .parse_term(source)
            .map_err(|error| ReasonerError::new(error.to_string()))?;
        let arguments = &mut self.operations[operation_index].arguments;
        arguments.resize(argument_index + 1, Term::EMPTY);
        arguments[argument_index] = argument;
        Ok(())
    }

    /// Enables or disables shell output for one operation slot.
    ///
    /// # Errors
    ///
    /// Returns an error when the one-based slot is not registered.
    pub fn set_operation_stdin(
        &mut self,
        operation_slot: usize,
        enabled: bool,
    ) -> Result<(), ReasonerError> {
        let operation = operation_slot
            .checked_sub(1)
            .and_then(|index| self.operations.get_mut(index))
            .ok_or_else(|| ReasonerError::new("operation slot is out of bounds"))?;
        operation.stdin_output = enabled;
        Ok(())
    }

    /// Creates the atom spelling used by ONA's `*setvalue` command.
    ///
    /// # Errors
    ///
    /// Returns an error for granularity outside 1..=1000 or atom exhaustion.
    pub fn set_numeric_value(
        &mut self,
        value: f64,
        granularity: usize,
        measurement: &str,
    ) -> Result<String, ReasonerError> {
        if !(1..=1_000).contains(&granularity) || !value.is_finite() {
            return Err(ReasonerError::new("numeric value configuration is invalid"));
        }
        let formatted = if granularity <= 10 {
            format!("{value:.1}")
        } else if granularity <= 100 {
            format!("{value:.2}")
        } else {
            format!("{value:.3}")
        };
        let atom_name = if measurement.is_empty() {
            formatted
        } else {
            format!("{measurement}_{formatted}")
        };
        let term = self
            .language
            .atomic_term(&atom_name)
            .map_err(|error| ReasonerError::new(error.to_string()))?;
        self.language
            .set_atom_value(term.root(), value, measurement)
            .map_err(|error| ReasonerError::new(error.to_string()))?;
        Ok(atom_name)
    }

    /// Populates ONA's evenly spaced numeric atom family.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid granularity or atom exhaustion.
    pub fn define_numeric_space(
        &mut self,
        granularity: usize,
        measurement: &str,
    ) -> Result<(), ReasonerError> {
        if !(1..=1_000).contains(&granularity) {
            return Err(ReasonerError::new(
                "numeric space granularity is out of bounds",
            ));
        }
        let denominator = f64::from(
            u32::try_from(granularity)
                .map_err(|_| ReasonerError::new("numeric space granularity is too large"))?,
        );
        for index in 0..granularity {
            let value = f64::from(
                u32::try_from(index)
                    .map_err(|_| ReasonerError::new("numeric space index is too large"))?,
            ) / denominator;
            self.set_numeric_value(value, granularity, measurement)?;
        }
        Ok(())
    }

    /// Adds a present goal and immediately runs goal-directed decision search.
    ///
    /// # Errors
    ///
    /// Returns an error if parsing fails or memory rejects the goal.
    pub fn add_goal(&mut self, source: &str, truth: Truth) -> Result<Event, ReasonerError> {
        let term = self
            .language
            .parse_term(source)
            .map_err(|error| ReasonerError::new(error.to_string()))?;
        self.add_goal_term(term, truth, 0.0)
    }

    fn add_goal_term(
        &mut self,
        term: Term,
        truth: Truth,
        occurrence_time_offset: f64,
    ) -> Result<Event, ReasonerError> {
        let event = self.events.input_event(
            term,
            EventType::Goal,
            truth,
            occurrence_time_offset,
            self.current_time,
        );
        if !self
            .memory
            .add_input_event(&self.language, event, self.current_time)
        {
            return Err(ReasonerError::new("memory rejected goal input"));
        }
        self.cycle();
        Ok(event)
    }

    /// Parses and adds one complete Narsese belief or goal sentence.
    ///
    /// This is the shared input path used by interactive and network shells.
    /// Present beliefs and all non-eternal goals are attributed to the current
    /// reasoner time, matching ONA's `NAR_AddInputNarsese` behavior.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed input, unsupported questions, eternal
    /// goals, or past/future belief events.
    pub fn add_sentence(&mut self, source: &str) -> Result<Event, ReasonerError> {
        let sentence = self
            .language
            .parse_sentence(source)
            .map_err(|error| ReasonerError::new(error.to_string()))?;
        self.add_parsed_sentence(sentence)
    }

    /// Adds a sentence which was previously parsed by this reasoner's
    /// [`Narsese`] language instance.
    ///
    /// Real-time integrations can parse stable sensor and goal templates once
    /// and reuse them without performing text tokenization in every control
    /// step. Terms are interned per language instance, so a prepared sentence
    /// must not be transferred between different reasoners.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported questions, eternal goals,
    /// past/future belief events, or an input rejected by memory.
    pub fn add_parsed_sentence(
        &mut self,
        sentence: crate::Sentence,
    ) -> Result<Event, ReasonerError> {
        match sentence.punctuation {
            Punctuation::Belief => match sentence.tense {
                Tense::Eternal => self.add_belief_term(
                    sentence.term,
                    sentence.truth,
                    true,
                    sentence.occurrence_time_offset,
                ),
                Tense::Present => self.add_belief_term(
                    sentence.term,
                    sentence.truth,
                    false,
                    sentence.occurrence_time_offset,
                ),
                Tense::Past | Tense::Future => Err(ReasonerError::new(
                    "past and future belief inputs are not supported",
                )),
            },
            Punctuation::Goal => {
                if sentence.tense == Tense::Eternal {
                    Err(ReasonerError::new("eternal goals are not supported"))
                } else {
                    self.add_goal_term(
                        sentence.term,
                        sentence.truth,
                        sentence.occurrence_time_offset,
                    )
                }
            }
            Punctuation::Question => Err(ReasonerError::new(
                "question answering is not implemented yet",
            )),
        }
    }

    /// Answers one passive Narsese question from current memory.
    ///
    /// Query variables are unified against stored terms. Event answers are
    /// ranked by truth projected to the current time, while implication and
    /// eternal answers are ranked by their stored expectation, matching ONA's
    /// `NAR_AddInputNarsese2` selection policy.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed input or non-question punctuation.
    pub fn ask(&mut self, source: &str) -> Result<Option<Answer>, ReasonerError> {
        let sentence = self
            .language
            .parse_sentence(source)
            .map_err(|error| ReasonerError::new(error.to_string()))?;
        if sentence.punctuation != Punctuation::Question {
            return Err(ReasonerError::new("query input must end with '?'"));
        }
        let _ = self
            .memory
            .conceptualize(&self.language, sentence.term, self.current_time);

        let implication_question = self.language.copula_equals(sentence.term.root(), '$')
            || self.language.copula_equals(sentence.term.root(), '?');
        let answer = if implication_question {
            self.best_implication_answer(sentence.term)
        } else {
            self.best_nonimplication_answer(sentence.term, sentence.tense)
        };
        if !implication_question
            && self.question_priming > 0.0
            && let Some(answer) = answer
            && let Some(concept) = self.memory.find_mut(&answer.term)
        {
            concept.priority = concept.priority.max(self.question_priming);
            concept.usage = concept
                .usage
                .used(self.current_time, sentence.tense == Tense::Eternal);
            self.recent_question_answers
                .retain(|(term, time)| *term != answer.term && *time >= self.current_time - 20);
            self.recent_question_answers
                .push((answer.term, self.current_time));
        }
        Ok(answer)
    }

    fn best_implication_answer(&self, query: Term) -> Option<Answer> {
        let temporal = self.language.copula_equals(query.root(), '$');
        let mut best = None;
        for concept in self.memory.concepts() {
            if temporal {
                for table in &concept.precondition_beliefs {
                    for implication in table.iter() {
                        if self.memory.implication_valid(implication)
                            && self.query_matches(&query, &implication.term)
                        {
                            select_answer(
                                &mut best,
                                Answer {
                                    term: implication.term,
                                    truth: implication.truth,
                                    stamp: implication.stamp,
                                    occurrence_time: OCCURRENCE_ETERNAL,
                                    creation_time: implication.creation_time,
                                },
                                implication.truth,
                            );
                        }
                    }
                }
            } else {
                for implication in concept.implication_links.iter() {
                    if self.memory.implication_valid(implication)
                        && self.query_matches(&query, &implication.term)
                    {
                        select_answer(
                            &mut best,
                            Answer {
                                term: implication.term,
                                truth: implication.truth,
                                stamp: implication.stamp,
                                occurrence_time: OCCURRENCE_ETERNAL,
                                creation_time: implication.creation_time,
                            },
                            implication.truth,
                        );
                    }
                }
            }
        }
        best.map(|(answer, _)| answer)
    }

    fn best_event_answer(&self, query: Term, tense: Tense) -> Option<Answer> {
        let mut best = None;
        for concept in self.memory.concepts() {
            if !self.query_matches(&query, &concept.term) {
                continue;
            }
            match tense {
                Tense::Eternal => self.consider_event_answer(&mut best, concept.belief, false),
                Tense::Present => {
                    self.consider_event_answer(&mut best, concept.belief_spike, true);
                    self.consider_event_answer(&mut best, concept.predicted_belief, true);
                }
                Tense::Past => {
                    self.consider_event_answer(&mut best, concept.belief_spike, true);
                }
                Tense::Future => {
                    self.consider_event_answer(&mut best, concept.predicted_belief, true);
                }
            }
        }
        best.map(|(answer, _)| answer)
    }

    fn best_nonimplication_answer(&self, query: Term, tense: Tense) -> Option<Answer> {
        let mut best = None;
        for answer in [
            self.best_event_answer(query, tense),
            self.best_relational_image_answer(query, tense),
            self.best_relational_category_answer(query, tense),
            self.best_two_premise_rule_answer(query, tense),
        ]
        .into_iter()
        .flatten()
        {
            let ranking_truth = if tense == Tense::Eternal {
                answer.truth
            } else {
                answer
                    .truth
                    .project(answer.occurrence_time, self.current_time)
            };
            select_answer(&mut best, answer, ranking_truth);
        }
        best.map(|(answer, _)| answer)
    }

    fn best_relational_category_answer(&self, query: Term, tense: Tense) -> Option<Answer> {
        if !self.language.copula_equals(query.root(), ':')
            || !self.language.copula_equals(query.atoms()[1], '*')
        {
            return None;
        }
        let query_product = query.extract_subterm(1);
        let query_left = query_product.extract_subterm(1);
        let query_right = query_product.extract_subterm(2);
        let query_predicate = query.extract_subterm(2);
        let mut best = None;
        for concept in self.memory.concepts() {
            if !self.language.copula_equals(concept.term.root(), ':')
                || !self.language.copula_equals(concept.term.atoms()[1], '*')
                || concept.term.extract_subterm(2) != query_predicate
            {
                continue;
            }
            for relation in Self::events_for_tense(concept, tense) {
                if relation.event_type == EventType::Deleted {
                    continue;
                }
                let relation_product = relation.term.extract_subterm(1);
                let relation_left = relation_product.extract_subterm(1);
                let relation_right = relation_product.extract_subterm(2);
                let right_substitution = crate::Substitution::unify_with(
                    &self.language,
                    &query_right,
                    &relation_right,
                    UnifyOptions {
                        query_variables_only: true,
                        numeric_similarity: false,
                        ..UnifyOptions::default()
                    },
                );
                if !right_substitution.success {
                    continue;
                }
                let Some(term) = right_substitution.apply(&self.language, query) else {
                    continue;
                };
                for (subject, predicate, upward) in [
                    (query_left, relation_left, false),
                    (relation_left, query_left, true),
                ] {
                    let mut inheritance = Term::from_atom(query.root());
                    if !inheritance.override_subterm(1, &subject)
                        || !inheritance.override_subterm(2, &predicate)
                    {
                        continue;
                    }
                    let Some(category) = self.memory.find(&inheritance) else {
                        continue;
                    };
                    for category_event in Self::events_for_tense(category, tense) {
                        if category_event.event_type == EventType::Deleted
                            || relation.stamp.overlaps(&category_event.stamp)
                        {
                            continue;
                        }
                        let truth = if upward {
                            relation.truth.induction(category_event.truth)
                        } else {
                            relation.truth.deduction(category_event.truth)
                        };
                        let occurrence_time = if relation.occurrence_time == OCCURRENCE_ETERNAL
                            && category_event.occurrence_time == OCCURRENCE_ETERNAL
                        {
                            OCCURRENCE_ETERNAL
                        } else {
                            relation.occurrence_time.max(category_event.occurrence_time)
                        };
                        let answer = Answer {
                            term,
                            truth,
                            stamp: relation.stamp.zip(&category_event.stamp),
                            occurrence_time,
                            creation_time: relation.creation_time.max(category_event.creation_time),
                        };
                        let ranking_truth = if tense == Tense::Eternal {
                            answer.truth
                        } else {
                            answer.truth.project(occurrence_time, self.current_time)
                        };
                        select_answer(&mut best, answer, ranking_truth);
                    }
                }
            }
        }
        best.map(|(answer, _)| answer)
    }

    fn best_two_premise_rule_answer(&self, query: Term, tense: Tense) -> Option<Answer> {
        if !self.language.copula_equals(query.root(), ':')
            || !self.language.copula_equals(query.atoms()[1], '*')
        {
            return None;
        }
        let events = self
            .memory
            .related_concept_ids(&self.language, &query)
            .into_iter()
            .filter_map(|id| self.memory.by_id(id))
            .flat_map(|concept| Self::events_for_tense(concept, tense))
            .filter(|event| event.event_type == EventType::Belief)
            .take(48)
            .collect::<Vec<_>>();
        let mut best = None;
        for (first_index, first) in events.iter().copied().enumerate() {
            for second in events.iter().copied().skip(first_index + 1) {
                if first.stamp.overlaps(&second.stamp) {
                    continue;
                }
                for (left, right) in [(first, second), (second, first)] {
                    for derivation in self.rules.apply(
                        &self.language,
                        left.term,
                        Some(right.term),
                        left.truth,
                        right.truth,
                    ) {
                        if !self.query_matches(&query, &derivation.term) {
                            continue;
                        }
                        let occurrence_time = if left.occurrence_time == OCCURRENCE_ETERNAL
                            && right.occurrence_time == OCCURRENCE_ETERNAL
                        {
                            OCCURRENCE_ETERNAL
                        } else {
                            left.occurrence_time.max(right.occurrence_time)
                        };
                        let answer = Answer {
                            term: derivation.term,
                            truth: derivation.truth,
                            stamp: left.stamp.zip(&right.stamp),
                            occurrence_time,
                            creation_time: left.creation_time.max(right.creation_time),
                        };
                        let ranking_truth = if tense == Tense::Eternal {
                            answer.truth
                        } else {
                            answer.truth.project(occurrence_time, self.current_time)
                        };
                        select_answer(&mut best, answer, ranking_truth);
                    }
                }
            }
        }
        best.map(|(answer, _)| answer)
    }

    /// Resolves image questions from a relational product and the properties
    /// describing one of its arguments. ONA's category examples rely on this
    /// composition even though the intermediate intersection/image concepts
    /// are deliberately filtered from ordinary concept creation.
    fn best_relational_image_answer(&self, query: Term, tense: Tense) -> Option<Answer> {
        if !self.language.copula_equals(query.root(), ':') {
            return None;
        }
        let description = query.extract_subterm(1);
        let query_image = query.extract_subterm(2);
        if !(self.language.copula_equals(query_image.root(), '/')
            || self.language.copula_equals(query_image.root(), '%'))
        {
            return None;
        }

        let mut best = None;
        for concept in self.memory.concepts() {
            for relation in Self::events_for_tense(concept, tense) {
                if relation.event_type == EventType::Deleted
                    || !self.language.copula_equals(relation.term.root(), ':')
                    || !self.language.copula_equals(relation.term.atoms()[1], '*')
                {
                    continue;
                }
                for derivation in self.rules.apply(
                    &self.language,
                    relation.term,
                    None,
                    relation.truth,
                    Truth::default(),
                ) {
                    if !self.language.copula_equals(derivation.term.root(), ':') {
                        continue;
                    }
                    let entity = derivation.term.extract_subterm(1);
                    let image = derivation.term.extract_subterm(2);
                    let substitution = crate::Substitution::unify_with(
                        &self.language,
                        &query_image,
                        &image,
                        UnifyOptions {
                            query_variables_only: true,
                            numeric_similarity: false,
                            ..UnifyOptions::default()
                        },
                    );
                    if !substitution.success {
                        continue;
                    }
                    let Some((property_truth, property_stamp, property_time, property_creation)) =
                        self.description_evidence(&entity, &description, query.root(), tense)
                    else {
                        continue;
                    };
                    let Some(term) = substitution.apply(&self.language, query) else {
                        continue;
                    };
                    let truth = derivation.truth.deduction(property_truth);
                    let occurrence_time = if relation.occurrence_time == OCCURRENCE_ETERNAL {
                        property_time
                    } else {
                        relation.occurrence_time.max(property_time)
                    };
                    let answer = Answer {
                        term,
                        truth,
                        stamp: relation.stamp.zip(&property_stamp),
                        occurrence_time,
                        creation_time: relation.creation_time.max(property_creation),
                    };
                    let ranking_truth = if tense == Tense::Eternal {
                        truth
                    } else {
                        truth.project(occurrence_time, self.current_time)
                    };
                    select_answer(&mut best, answer, ranking_truth);
                }
            }
        }
        best.map(|(answer, _)| answer)
    }

    fn events_for_tense(concept: &crate::Concept, tense: Tense) -> Vec<Event> {
        match tense {
            Tense::Eternal => vec![concept.belief],
            Tense::Present => vec![concept.belief_spike, concept.predicted_belief],
            Tense::Past => vec![concept.belief_spike],
            Tense::Future => vec![concept.predicted_belief],
        }
    }

    fn description_evidence(
        &self,
        entity: &Term,
        description: &Term,
        inheritance_atom: Atom,
        tense: Tense,
    ) -> Option<(Truth, Stamp, i64, i64)> {
        if description == entity {
            return Some((Truth::new(1.0, 0.9), Stamp::default(), self.current_time, 0));
        }
        if self.language.copula_equals(description.root(), '&') {
            let left = self.description_evidence(
                entity,
                &description.extract_subterm(1),
                inheritance_atom,
                tense,
            )?;
            let right = self.description_evidence(
                entity,
                &description.extract_subterm(2),
                inheritance_atom,
                tense,
            )?;
            return Some((
                left.0.intersection(right.0),
                left.1.zip(&right.1),
                left.2.max(right.2),
                left.3.max(right.3),
            ));
        }

        let mut statement = Term::from_atom(inheritance_atom);
        if !statement.override_subterm(1, entity) || !statement.override_subterm(2, description) {
            return None;
        }
        let concept = self.memory.find(&statement)?;
        Self::events_for_tense(concept, tense)
            .into_iter()
            .filter(|event| event.event_type != EventType::Deleted)
            .max_by(|left, right| {
                let left_truth = if tense == Tense::Eternal {
                    left.truth
                } else {
                    left.truth.project(left.occurrence_time, self.current_time)
                };
                let right_truth = if tense == Tense::Eternal {
                    right.truth
                } else {
                    right
                        .truth
                        .project(right.occurrence_time, self.current_time)
                };
                left_truth
                    .expectation()
                    .total_cmp(&right_truth.expectation())
            })
            .map(|event| {
                (
                    event.truth,
                    event.stamp,
                    event.occurrence_time,
                    event.creation_time,
                )
            })
    }

    fn consider_event_answer(
        &self,
        best: &mut Option<(Answer, Truth)>,
        event: Event,
        project: bool,
    ) {
        if event.event_type == EventType::Deleted {
            return;
        }
        let ranking_truth = if project {
            event
                .truth
                .project(event.occurrence_time, self.current_time)
        } else {
            event.truth
        };
        select_answer(
            best,
            Answer {
                term: event.term,
                truth: event.truth,
                stamp: event.stamp,
                occurrence_time: event.occurrence_time,
                creation_time: event.creation_time,
            },
            ranking_truth,
        );
    }

    fn query_matches(&self, query: &Term, candidate: &Term) -> bool {
        crate::Substitution::unify_with(
            &self.language,
            query,
            candidate,
            UnifyOptions {
                query_variables_only: true,
                numeric_similarity: false,
                ..UnifyOptions::default()
            },
        )
        .success
    }

    pub fn cycles(&mut self, cycles: usize) {
        for _ in 0..cycles {
            self.cycle();
        }
    }

    #[allow(clippy::too_many_lines)]
    pub fn cycle(&mut self) {
        self.process_declarative_implications();
        if let Some((selected, selected_priority)) = self.memory.pop_belief() {
            self.forward_chain_implications(&selected);
            let mut pending = Vec::new();
            self.collect_temporal_derivations(selected, selected_priority, &mut pending);
            for derivation in self.rules.apply(
                &self.language,
                selected.term,
                None,
                selected.truth,
                Truth::default(),
            ) {
                self.collect_derivation(
                    derivation.term,
                    derivation.truth,
                    derivation.variable_introduction,
                    selected.stamp,
                    selected.occurrence_time,
                    selected.occurrence_time_offset,
                    selected_priority,
                    1.0,
                    selected.occurrence_time == OCCURRENCE_ETERNAL,
                    &mut pending,
                );
            }

            let revised_property = self.memory.find(&selected.term).and_then(|concept| {
                let property = selected.term.extract_subterm(2);
                (selected.occurrence_time == OCCURRENCE_ETERNAL
                    && self.language.copula_equals(selected.term.root(), ':')
                    && self.language.copula_equals(property.root(), '\'')
                    && concept.belief.event_type == EventType::Belief
                    && concept.belief.truth.confidence > selected.truth.confidence)
                    .then_some(concept.belief)
            });
            if let Some(property) = revised_property {
                let entity = property.term.extract_subterm(1);
                let mut relations = self
                    .memory
                    .related_concept_ids(&self.language, &entity)
                    .into_iter()
                    .filter_map(|id| {
                        let concept = self.memory.by_id(id)?;
                        let relation = concept.belief;
                        let subject = relation.term.extract_subterm(1);
                        let predicate = relation.term.extract_subterm(2);
                        (relation.event_type == EventType::Belief
                            && self.language.copula_equals(relation.term.root(), ':')
                            && self.language.copula_equals(subject.root(), '*')
                            && self.language.copula_equals(predicate.root(), '*')
                            && !relation.stamp.overlaps(&property.stamp))
                        .then_some((relation, concept.priority.max(f64::EPSILON)))
                    })
                    .collect::<Vec<_>>();
                relations.sort_by(|left, right| {
                    right
                        .1
                        .total_cmp(&left.1)
                        .then_with(|| right.0.truth.confidence.total_cmp(&left.0.truth.confidence))
                });
                for (relation, relation_priority) in relations.into_iter().take(16) {
                    for derivation in self.rules.apply(
                        &self.language,
                        relation.term,
                        Some(property.term),
                        relation.truth,
                        property.truth,
                    ) {
                        let subject = derivation.term.extract_subterm(1);
                        let predicate = derivation.term.extract_subterm(2);
                        let property_product =
                            self.language.copula_equals(derivation.term.root(), ':')
                                && self.language.copula_equals(subject.root(), '*')
                                && self.language.copula_equals(predicate.root(), '*')
                                && [predicate.extract_subterm(1), predicate.extract_subterm(2)]
                                    .into_iter()
                                    .any(|component| {
                                        self.language.copula_equals(component.root(), '\'')
                                    });
                        if !property_product {
                            continue;
                        }
                        for reduction in self.rules.apply(
                            &self.language,
                            derivation.term,
                            None,
                            derivation.truth,
                            Truth::default(),
                        ) {
                            if reduction.term.complexity() >= derivation.term.complexity()
                                || !crate::nal_filter::derived_term_valid(
                                    &self.language,
                                    &reduction.term,
                                )
                            {
                                continue;
                            }
                            let reduced = Event {
                                term: reduction.term,
                                event_type: EventType::Belief,
                                truth: reduction.truth,
                                stamp: relation.stamp.zip(&property.stamp),
                                occurrence_time: OCCURRENCE_ETERNAL,
                                creation_time: self.current_time,
                                ..Event::default()
                            };
                            if pending
                                .iter()
                                .any(|(existing, _, _)| existing.evidence_eq(&reduced))
                            {
                                continue;
                            }
                            pending.push((
                                reduced,
                                selected_priority
                                    * relation_priority
                                    * reduction.truth.expectation(),
                                true,
                            ));
                        }
                    }
                }
            }

            self.collect_exemplar_classifications(selected, selected_priority, &mut pending);

            let candidates = self
                .memory
                .related_concept_ids(&self.language, &selected.term)
                .into_iter()
                .take(BELIEF_CONCEPT_MATCH_TARGET)
                .filter_map(|id| {
                    let concept = self.memory.by_id(id)?;
                    let belief = if concept.belief.event_type == EventType::Deleted {
                        if selected.occurrence_time == OCCURRENCE_ETERNAL {
                            concept.belief_spike
                        } else {
                            return None;
                        }
                    } else {
                        concept.belief
                    };
                    (belief.event_type != EventType::Deleted).then_some((
                        concept.term,
                        belief,
                        concept.priority.max(f64::EPSILON),
                    ))
                })
                .collect::<Vec<_>>();

            for (concept_term, belief, concept_priority) in candidates {
                if selected.stamp.overlaps(&belief.stamp)
                    || selected.stamp.has_duplicate()
                    || belief.stamp.has_duplicate()
                {
                    continue;
                }
                let stamp = selected.stamp.zip(&belief.stamp);
                for derivation in self.rules.apply(
                    &self.language,
                    selected.term,
                    Some(belief.term),
                    selected.truth,
                    belief.truth,
                ) {
                    self.collect_derivation(
                        derivation.term,
                        derivation.truth,
                        derivation.variable_introduction,
                        stamp,
                        selected.occurrence_time,
                        selected.occurrence_time_offset,
                        selected_priority,
                        concept_priority,
                        selected.occurrence_time == OCCURRENCE_ETERNAL
                            && belief.occurrence_time == OCCURRENCE_ETERNAL,
                        &mut pending,
                    );
                }
                self.collect_special_inferences(
                    selected.term,
                    belief.term,
                    selected.truth,
                    belief.truth,
                    stamp,
                    selected.occurrence_time,
                    selected.occurrence_time_offset,
                    selected_priority,
                    concept_priority,
                    selected.occurrence_time == OCCURRENCE_ETERNAL
                        && belief.occurrence_time == OCCURRENCE_ETERNAL,
                    &mut pending,
                );
                self.collect_special_inferences(
                    concept_term,
                    selected.term,
                    belief.truth,
                    selected.truth,
                    stamp,
                    selected.occurrence_time,
                    selected.occurrence_time_offset,
                    selected_priority,
                    concept_priority,
                    selected.occurrence_time == OCCURRENCE_ETERNAL
                        && belief.occurrence_time == OCCURRENCE_ETERNAL,
                    &mut pending,
                );
            }

            for (event, priority, eternalize) in pending {
                let operation_slot = self.operation_slot_for_implication(&event.term);
                self.memory.add_event(
                    &self.language,
                    event,
                    self.current_time,
                    AddEventOptions {
                        priority,
                        derived: true,
                        eternalize,
                        operation_slot,
                        ..AddEventOptions::default()
                    },
                );
            }
        }
        self.process_goal_layers();
        self.finish_cycle();
    }

    #[allow(clippy::cast_possible_truncation)]
    fn process_declarative_implications(&mut self) {
        if !self.memory.has_declarative_implications() {
            return;
        }
        let implications = self
            .memory
            .declarative_concept_ids()
            .iter()
            .filter_map(|id| self.memory.by_id(*id))
            .flat_map(|concept| {
                concept
                    .implication_links
                    .iter()
                    .take(TOP_K_DECLARATIVE_IMPLICATIONS)
                    .copied()
            })
            .filter(|implication| self.memory.implication_valid(implication))
            .collect::<Vec<_>>();
        let fresh_premises = self
            .memory
            .concepts()
            .filter(|concept| !self.language.has_variable(&concept.term, true, true, true))
            .filter(|concept| {
                concept.belief.creation_time >= self.current_time - BELIEF_LAST_USED_TOLERANCE
                    || concept.belief_spike.creation_time
                        >= self.current_time - BELIEF_LAST_USED_TOLERANCE
            })
            .map(|concept| (concept.belief, concept.belief_spike))
            .collect::<Vec<_>>();
        let mut derived = Vec::new();
        for implication in implications {
            let precondition = implication.term.extract_subterm(1);
            let predicate = implication.term.extract_subterm(2);
            for (belief, belief_spike) in &fresh_premises {
                for premise in [*belief, *belief_spike] {
                    if premise.event_type != EventType::Belief {
                        continue;
                    }
                    if implication.creation_time < self.current_time - 1
                        && premise.creation_time < self.current_time - 1
                    {
                        continue;
                    }
                    let substitution =
                        crate::Substitution::unify(&self.language, &precondition, &premise.term);
                    if !substitution.success {
                        continue;
                    }
                    let Some(mut term) = substitution.apply(&self.language, predicate) else {
                        continue;
                    };
                    let mut truth = implication.truth.deduction(premise.truth);
                    if self.language.copula_equals(term.root(), '!') {
                        term = term.extract_subterm(1);
                        truth = truth.negation();
                    }
                    derived.push(Event {
                        term,
                        event_type: EventType::Belief,
                        truth,
                        stamp: premise.stamp.zip(&implication.stamp),
                        occurrence_time: if premise.occurrence_time == OCCURRENCE_ETERNAL {
                            OCCURRENCE_ETERNAL
                        } else {
                            premise.occurrence_time
                                + implication.occurrence_time_offset.round() as i64
                        },
                        occurrence_time_offset: implication.occurrence_time_offset,
                        creation_time: self.current_time.max(implication.creation_time),
                        ..Event::default()
                    });
                }
            }
        }
        for event in derived {
            let _ =
                self.memory
                    .integrate_declarative_result(&self.language, event, self.current_time);
        }
    }

    fn process_goal_layers(&mut self) {
        let mut start = 0;
        while let Some(layer) = self.memory.next_goal_layer(start) {
            start = layer + 1;
            let Some((goal, priority)) = self.memory.pop_goal(layer) else {
                continue;
            };
            if self.decompose_sequence_goal(goal, priority, layer) {
                continue;
            }
            let id = self
                .memory
                .conceptualize(&self.language, goal.term, self.current_time);
            if let Some(id) = id
                && let Some(concept) = self.memory.by_id_mut(id)
                && (concept.goal_spike.event_type == EventType::Deleted
                    || goal.occurrence_time >= concept.goal_spike.occurrence_time)
            {
                concept.goal_spike = goal;
            }

            let mut visited = Vec::new();
            if self.process_goal(goal, &mut visited, layer) {
                self.memory.clear_goals();
                return;
            }
            self.propagate_subgoals(goal, priority, layer);
        }
    }

    fn propagate_subgoals(&mut self, goal: Event, priority: f64, layer: usize) {
        let implications = self
            .memory
            .find(&goal.term)
            .map_or_else(Vec::new, |concept| {
                concept
                    .precondition_beliefs
                    .iter()
                    .flat_map(|table| table.iter().copied())
                    .filter(|implication| self.memory.implication_valid(implication))
                    .collect::<Vec<_>>()
            });
        for implication in implications {
            let subgoal = goal_deduction(&self.language, goal, implication, self.current_time);
            if subgoal.term.is_empty() || subgoal.term == goal.term {
                continue;
            }
            self.memory.add_event(
                &self.language,
                subgoal,
                self.current_time,
                AddEventOptions {
                    priority: priority * subgoal.truth.expectation(),
                    derived: true,
                    layer,
                    ..AddEventOptions::default()
                },
            );
        }
    }

    fn decompose_sequence_goal(&mut self, goal: Event, priority: f64, layer: usize) -> bool {
        if !self.language.copula_equals(goal.term.root(), '+') {
            return false;
        }
        let mut components = Vec::new();
        flatten_sequence(&self.language, goal.term, &mut components);
        let Some(component) = components.into_iter().find(|component| {
            self.memory.find(component).is_none_or(|concept| {
                concept.belief_spike.event_type == EventType::Deleted
                    || concept
                        .belief_spike
                        .truth
                        .project(concept.belief_spike.occurrence_time, self.current_time)
                        .expectation()
                        < CONDITION_THRESHOLD
            })
        }) else {
            return true;
        };
        let subgoal = Event {
            term: component,
            truth: goal.truth.structural_deduction(),
            creation_time: self.current_time,
            ..goal
        };
        self.memory.add_event(
            &self.language,
            subgoal,
            self.current_time,
            AddEventOptions {
                priority: priority * subgoal.truth.expectation(),
                derived: true,
                layer,
                ..AddEventOptions::default()
            },
        );
        true
    }

    fn finish_cycle(&mut self) {
        self.expire_anticipations();
        self.memory.relative_forgetting();
        self.current_time += 1;
    }

    fn learn_from_input_event(&mut self, event: Event) {
        let mut pending = Vec::new();
        self.collect_temporal_derivations(event, 1.0, &mut pending);
        for (derived, priority, eternalize) in pending {
            let operation_slot = self.operation_slot_for_implication(&derived.term);
            self.memory.add_event(
                &self.language,
                derived,
                self.current_time,
                AddEventOptions {
                    priority,
                    derived: true,
                    eternalize,
                    operation_slot,
                    ..AddEventOptions::default()
                },
            );
        }
    }

    fn confirm_anticipations(&mut self, observation: Event) {
        self.anticipations.retain(|anticipation| {
            anticipation.expected_term != observation.term
                || observation.occurrence_time > anticipation.deadline
        });
    }

    #[allow(clippy::cast_possible_truncation)]
    fn arm_anticipations(&mut self, operation: Event) {
        let implications = self
            .memory
            .concepts()
            .flat_map(|concept| {
                concept
                    .precondition_beliefs
                    .iter()
                    .flat_map(|table| table.iter().copied())
                    .map(move |implication| (concept.term, implication))
            })
            .filter(|(_, implication)| self.memory.implication_valid(implication))
            .collect::<Vec<_>>();

        for (expected_term, implication) in implications {
            let failing_specific = implication.truth.frequency == 0.0
                && implication.truth.confidence < SUBSUMPTION_CONFIDENCE_THRESHOLD;
            if implication.truth.expectation() <= ANTICIPATION_THRESHOLD && !failing_specific {
                continue;
            }
            let precondition = implication.term.extract_subterm(1);
            let Some(expected_operation) = self.language.operation_term(&precondition) else {
                continue;
            };
            if !crate::Substitution::unify(&self.language, &expected_operation, &operation.term)
                .success
            {
                continue;
            }
            let condition = self.language.precondition_without_operation(&precondition);
            let precondition_truth = self.condition_truth(&condition).or_else(|| {
                self.variable_operation_contexts(condition, precondition)
                    .into_iter()
                    .find(|(concrete, _, _, _)| {
                        self.language
                            .operation_term(concrete)
                            .is_some_and(|concrete_operation| {
                                crate::Substitution::unify(
                                    &self.language,
                                    &concrete_operation,
                                    &operation.term,
                                )
                                .success
                            })
                    })
                    .map(|(_, _, _, truth)| truth)
            });
            let Some(precondition_truth) = precondition_truth else {
                continue;
            };
            self.add_negative_confirmation(expected_term, implication, precondition_truth);
        }
    }

    fn anticipate_nonoperation_input(&mut self, _observation: Event) {
        let implications = self
            .memory
            .concepts()
            .flat_map(|concept| {
                concept.precondition_beliefs[0]
                    .iter()
                    .copied()
                    .filter_map(|implication| {
                        let source = implication
                            .source_concept_id
                            .and_then(|id| self.memory.by_id(crate::ConceptId(id)))?;
                        (source.belief_spike.event_type == EventType::Belief).then_some((
                            concept.term,
                            implication,
                            source.belief_spike,
                        ))
                    })
            })
            .filter(|(_, implication, _)| self.memory.implication_valid(implication))
            .collect::<Vec<_>>();
        for (expected_term, implication, precondition_event) in implications {
            let precondition = implication.term.extract_subterm(1);
            if self.language.operation_term(&precondition).is_some() {
                continue;
            }
            let failing_specific = implication.truth.frequency == 0.0
                && implication.truth.confidence < SUBSUMPTION_CONFIDENCE_THRESHOLD;
            let predicted = implication.truth.deduction(precondition_event.truth);
            if predicted.expectation() <= ANTICIPATION_THRESHOLD && !failing_specific {
                continue;
            }
            self.add_negative_confirmation(expected_term, implication, precondition_event.truth);
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    fn add_negative_confirmation(
        &mut self,
        expected_term: Term,
        implication: crate::Implication,
        precondition_truth: Truth,
    ) {
        let delay = (implication.occurrence_time_offset.round() as i64).max(0);
        let past = precondition_truth.project(0, delay);
        let negative = Truth::new(0.0, self.anticipation_confidence)
            .induction(past)
            .eternalize();
        let _ = self.memory.revise_implication(
            &self.language,
            &expected_term,
            &implication.term,
            negative,
            Stamp::default(),
            self.current_time,
        );
    }

    fn expire_anticipations(&mut self) {
        let anticipations = std::mem::take(&mut self.anticipations);
        let mut retained = Vec::with_capacity(anticipations.len());
        for anticipation in anticipations {
            if anticipation.deadline > self.current_time {
                retained.push(anticipation);
                continue;
            }
            let past = anticipation
                .precondition_truth
                .project(0, anticipation.occurrence_time_offset);
            let negative = Truth::new(0.0, self.anticipation_confidence)
                .induction(past)
                .eternalize();
            let revised = self.memory.revise_implication(
                &self.language,
                &anticipation.expected_term,
                &anticipation.implication_term,
                negative,
                Stamp::default(),
                self.current_time,
            );
            if !revised && anticipation.create_if_missing {
                let event = Event {
                    term: anticipation.implication_term,
                    event_type: EventType::Belief,
                    truth: negative,
                    stamp: anticipation.implication_stamp,
                    occurrence_time: self.current_time,
                    creation_time: anticipation.implication_creation_time,
                    ..Event::default()
                };
                let operation_slot = self.operation_slot_for_implication(&event.term);
                self.memory.integrate_specific_implication(
                    &self.language,
                    event,
                    self.current_time,
                    operation_slot,
                );
            }
        }
        self.anticipations = retained;
    }

    fn collect_temporal_derivations(
        &mut self,
        postcondition: Event,
        parent_priority: f64,
        pending: &mut Vec<(Event, f64, bool)>,
    ) {
        if postcondition.processed
            || postcondition.occurrence_time == OCCURRENCE_ETERNAL
            || !postcondition.input
            || self
                .language
                .is_continuous_property_statement(&postcondition.term)
            || self.current_time - postcondition.occurrence_time >= EVENT_BELIEF_DISTANCE
        {
            return;
        }
        let candidates = (0..self.memory.occurrence_time_index.len())
            .filter_map(|index| self.memory.occurrence_time_index.kth_newest(index))
            .filter_map(|id| self.memory.by_id(id).map(|concept| concept.belief_spike))
            .filter(|event| event.event_type == EventType::Belief)
            .collect::<Vec<_>>();

        let postcondition_is_operation = self.registered_operation(&postcondition.term).is_some();

        if !postcondition_is_operation {
            let mut operation_events = candidates
                .iter()
                .copied()
                .filter(|event| {
                    self.language.is_operation(&event.term)
                        && self.registered_operation(&event.term).is_some()
                        && event.occurrence_time < postcondition.occurrence_time
                })
                .collect::<Vec<_>>();
            if let Some(latest) = operation_events
                .iter()
                .map(|event| event.occurrence_time)
                .max()
            {
                operation_events.retain(|event| event.occurrence_time == latest);
            }
            for operation_event in operation_events {
                let mut conditions = candidates
                    .iter()
                    .copied()
                    .filter(|event| {
                        self.registered_operation(&event.term).is_none()
                            && event.occurrence_time < operation_event.occurrence_time
                            && (event.occurrence_time - postcondition.occurrence_time).abs()
                                <= MAX_SEQUENCE_TIMEDIFF
                            && !event.stamp.overlaps(&postcondition.stamp)
                            && !self.language.copula_equals(event.term.root(), '^')
                            && !self.language.copula_equals(event.term.root(), '?')
                    })
                    .collect::<Vec<_>>();
                if let Some(latest) = conditions.iter().map(|event| event.occurrence_time).max() {
                    conditions.retain(|event| event.occurrence_time == latest);
                }
                for condition in conditions {
                    if let Ok(sequence) =
                        belief_intersection(&mut self.language, condition, operation_event)
                    {
                        if let Ok(implication) =
                            belief_induction(&mut self.language, sequence, postcondition)
                        {
                            self.push_temporal_implication(implication, parent_priority, pending);
                        }
                    }
                }
            }
        }

        for precondition in candidates {
            if precondition.occurrence_time > postcondition.occurrence_time
                || (precondition.occurrence_time - postcondition.occurrence_time).abs()
                    > MAX_SEQUENCE_TIMEDIFF
                || precondition.stamp.overlaps(&postcondition.stamp)
                || self.language.copula_equals(precondition.term.root(), '^')
                || self.language.copula_equals(precondition.term.root(), '?')
            {
                continue;
            }

            let precondition_is_operation = self.registered_operation(&precondition.term).is_some();
            if postcondition_is_operation || precondition_is_operation {
                continue;
            }

            if let Ok(implication) =
                belief_induction(&mut self.language, precondition, postcondition)
            {
                self.push_temporal_implication(implication, parent_priority, pending);
            }

            if let Ok(sequence) =
                belief_intersection(&mut self.language, precondition, postcondition)
                && self.language.sequence_length(&sequence.term) <= MAX_SEQUENCE_LEN
            {
                pending.push((
                    sequence,
                    parent_priority * sequence.truth.expectation(),
                    false,
                ));
            }

            if precondition.occurrence_time == postcondition.occurrence_time
                && let Ok(reverse) =
                    belief_induction(&mut self.language, postcondition, precondition)
            {
                self.push_temporal_implication(reverse, parent_priority, pending);
            }
        }
    }

    fn registered_operation(&self, term: &Term) -> Option<Atom> {
        let atom = self.language.operation_atom(term)?;
        self.operations
            .iter()
            .any(|operation| operation.term.root() == atom)
            .then_some(atom)
    }

    fn operation_slot_for_implication(&self, term: &Term) -> usize {
        if !self.language.copula_equals(term.root(), '$') {
            return 0;
        }
        let precondition = term.extract_subterm(1);
        let Some(operation_atom) = self.language.operation_atom(&precondition) else {
            return 0;
        };
        self.operations
            .iter()
            .position(|operation| operation.term.root() == operation_atom)
            .map_or(0, |index| index + 1)
    }

    fn push_temporal_implication(
        &mut self,
        implication: crate::Implication,
        parent_priority: f64,
        pending: &mut Vec<(Event, f64, bool)>,
    ) {
        for term in self.variable_introductions(implication.term) {
            if !crate::nal_filter::derived_term_valid(&self.language, &term) {
                continue;
            }
            if pending.iter().any(|(event, _, _)| {
                event.term == term
                    && event.truth == implication.truth
                    && event.stamp.evidence_eq(&implication.stamp)
            }) {
                continue;
            }
            pending.push((
                Event {
                    term,
                    event_type: EventType::Belief,
                    truth: implication.truth,
                    stamp: implication.stamp,
                    occurrence_time: self.current_time,
                    occurrence_time_offset: implication.occurrence_time_offset,
                    creation_time: self.current_time,
                    ..Event::default()
                },
                parent_priority * implication.truth.expectation(),
                true,
            ));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_derivation(
        &mut self,
        term: Term,
        truth: Truth,
        variable_introduction: bool,
        stamp: Stamp,
        occurrence_time: i64,
        occurrence_time_offset: f64,
        parent_priority: f64,
        concept_priority: f64,
        eternalize: bool,
        pending: &mut Vec<(Event, f64, bool)>,
    ) {
        let terms = if variable_introduction {
            self.variable_introductions(term)
        } else {
            vec![term]
        };
        let priority = parent_priority * concept_priority * truth.expectation();
        for term in terms {
            if !crate::nal_filter::derived_term_valid(&self.language, &term) {
                continue;
            }
            pending.push((
                Event {
                    term,
                    event_type: EventType::Belief,
                    truth,
                    stamp,
                    occurrence_time,
                    occurrence_time_offset,
                    creation_time: self.current_time,
                    ..Event::default()
                },
                priority,
                eternalize,
            ));
        }
    }

    #[allow(clippy::too_many_lines)]
    fn collect_exemplar_classifications(
        &mut self,
        selected: Event,
        selected_priority: f64,
        pending: &mut Vec<(Event, f64, bool)>,
    ) {
        let current_subject = selected.term.extract_subterm(1);
        let current_property = selected.term.extract_subterm(2);
        if selected.stamp.evidential_base()[1] != 0
            || selected.occurrence_time == OCCURRENCE_ETERNAL
            || !self.language.copula_equals(selected.term.root(), ':')
            || !self.language.copula_equals(current_property.root(), '\'')
        {
            return;
        }

        let mut exemplars = self
            .memory
            .concepts()
            .filter_map(|concept| {
                let belief = concept.belief;
                (belief.event_type == EventType::Belief
                    && belief.stamp.evidential_base()[1] == 0
                    && belief.occurrence_time == OCCURRENCE_ETERNAL
                    && self.language.copula_equals(concept.term.root(), ':')
                    && concept.term.extract_subterm(1) != current_subject
                    && concept.term.extract_subterm(2) == current_property)
                    .then_some(belief)
            })
            .collect::<Vec<_>>();
        exemplars.sort_by_key(|event| std::cmp::Reverse(event.creation_time));

        for exemplar in exemplars.into_iter().take(4) {
            if selected.stamp.overlaps(&exemplar.stamp) {
                continue;
            }
            let exemplar_subject = exemplar.term.extract_subterm(1);
            let categories = self
                .memory
                .concepts()
                .filter_map(|concept| {
                    let belief = concept.belief;
                    let predicate = concept.term.extract_subterm(2);
                    if belief.event_type != EventType::Belief
                        || belief.truth.confidence < 0.8
                        || belief.occurrence_time != OCCURRENCE_ETERNAL
                        || !self.language.copula_equals(concept.term.root(), ':')
                        || concept.term.extract_subterm(1) != exemplar_subject
                        || !self.language.is_simple_atom(predicate.root())
                    {
                        return None;
                    }
                    let evidence = belief.stamp.evidential_base().iter().copied().find(|id| {
                        *id != 0
                            && !exemplar.stamp.evidential_base().contains(id)
                            && !selected.stamp.evidential_base().contains(id)
                    })?;
                    Some(Event {
                        truth: Truth::new(belief.truth.frequency, 0.9),
                        stamp: Stamp::from_evidence([evidence]),
                        input: true,
                        ..belief
                    })
                })
                .collect::<Vec<_>>();
            let relation_stamp = selected.stamp.zip(&exemplar.stamp);
            let relations = self.rules.apply(
                &self.language,
                selected.term,
                Some(exemplar.term),
                selected.truth,
                exemplar.truth,
            );
            for category in categories {
                let target_predicate = category.term.extract_subterm(2);
                for relation in &relations {
                    let stamp = relation_stamp.zip(&category.stamp);
                    let first = self.rules.apply(
                        &self.language,
                        relation.term,
                        Some(category.term),
                        relation.truth,
                        category.truth,
                    );
                    let second = self.rules.apply(
                        &self.language,
                        category.term,
                        Some(relation.term),
                        category.truth,
                        relation.truth,
                    );
                    for classification in first.into_iter().chain(second) {
                        if !self.language.copula_equals(classification.term.root(), ':')
                            || classification.term.extract_subterm(1) != current_subject
                            || classification.term.extract_subterm(2) != target_predicate
                            || !crate::nal_filter::derived_term_valid(
                                &self.language,
                                &classification.term,
                            )
                        {
                            continue;
                        }
                        let event = Event {
                            term: classification.term,
                            event_type: EventType::Belief,
                            truth: classification.truth,
                            stamp,
                            occurrence_time: selected.occurrence_time,
                            creation_time: self.current_time,
                            ..Event::default()
                        };
                        if pending
                            .iter()
                            .any(|(existing, _, _)| existing.evidence_eq(&event))
                        {
                            continue;
                        }
                        pending.push((
                            event,
                            selected_priority * classification.truth.expectation(),
                            false,
                        ));
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn collect_special_inferences(
        &mut self,
        first: Term,
        second: Term,
        first_truth: Truth,
        second_truth: Truth,
        stamp: Stamp,
        occurrence_time: i64,
        occurrence_time_offset: f64,
        parent_priority: f64,
        concept_priority: f64,
        eternalize: bool,
        pending: &mut Vec<(Event, f64, bool)>,
    ) {
        let implication = self.language.copula_equals(second.root(), '?');
        if implication || self.language.copula_equals(second.root(), '^') {
            let subject = second.extract_subterm(1);
            let predicate = second.extract_subterm(2);

            let subject_substitution = crate::Substitution::unify(&self.language, &subject, &first);
            if subject_substitution.success
                && let Some(conclusion) = subject_substitution.apply(&self.language, predicate)
            {
                let truth = if implication {
                    second_truth.deduction(first_truth)
                } else {
                    second_truth.analogy(first_truth)
                };
                self.collect_derivation(
                    conclusion,
                    truth,
                    false,
                    stamp,
                    occurrence_time,
                    occurrence_time_offset,
                    parent_priority,
                    concept_priority,
                    eternalize,
                    pending,
                );
            }

            if implication && self.language.copula_equals(subject.root(), ';') {
                let left = subject.extract_subterm(1);
                let right = subject.extract_subterm(2);
                let mut unifying_term = left;
                let mut remaining_condition = right;
                let mut substitution =
                    crate::Substitution::unify(&self.language, &unifying_term, &first);
                if !substitution.success {
                    unifying_term = right;
                    remaining_condition = left;
                    substitution =
                        crate::Substitution::unify(&self.language, &unifying_term, &first);
                }
                if substitution.success {
                    let mut conclusion = Term::from_atom(
                        self.language
                            .copula_atom('?')
                            .expect("built-in implication copula remains interned"),
                    );
                    if conclusion.override_subterm(1, &remaining_condition)
                        && conclusion.override_subterm(2, &predicate)
                        && let Some(conclusion) = substitution.apply(&self.language, conclusion)
                    {
                        self.collect_derivation(
                            conclusion,
                            second_truth.deduction(first_truth),
                            false,
                            stamp,
                            occurrence_time,
                            occurrence_time_offset,
                            parent_priority,
                            concept_priority,
                            eternalize,
                            pending,
                        );
                    }
                }
            }

            let predicate_substitution =
                crate::Substitution::unify(&self.language, &predicate, &first);
            if predicate_substitution.success
                && let Some(conclusion) = predicate_substitution.apply(&self.language, subject)
            {
                let truth = if implication {
                    second_truth.abduction(first_truth)
                } else {
                    second_truth.analogy(first_truth)
                };
                self.collect_derivation(
                    conclusion,
                    truth,
                    false,
                    stamp,
                    occurrence_time,
                    occurrence_time_offset,
                    parent_priority,
                    concept_priority,
                    eternalize,
                    pending,
                );
            }
        }

        if self.language.copula_equals(second.root(), ';') {
            let subject = second.extract_subterm(1);
            let predicate = second.extract_subterm(2);
            let substitution = crate::Substitution::unify(&self.language, &subject, &first);
            if substitution.success
                && let Some(conclusion) = substitution.apply(&self.language, predicate)
            {
                self.collect_derivation(
                    conclusion,
                    second_truth.anonymous_analogy(first_truth),
                    false,
                    stamp,
                    occurrence_time,
                    occurrence_time_offset,
                    parent_priority,
                    concept_priority,
                    eternalize,
                    pending,
                );
            }
        }
    }

    fn variable_introductions(&mut self, term: Term) -> Vec<Term> {
        let mut output = Vec::new();
        if ['$', '?', '^']
            .into_iter()
            .any(|copula| self.language.copula_equals(term.root(), copula))
        {
            for extensional in [true, false] {
                if let Some(introduced) =
                    introduce_implication_variables(&mut self.language, term, extensional, 0)
                    && introduced != term
                    && crate::nal_filter::variable_introduction_valid(&self.language, &introduced)
                    && !output.contains(&introduced)
                {
                    output.push(introduced);
                }
            }
            if self.language.copula_equals(term.root(), '$') {
                output.push(term);
            }
        } else if self.language.copula_equals(term.root(), ';') {
            for extensional in [true, false] {
                if let Some(introduced) =
                    introduce_conjunction_variables(&mut self.language, term, extensional)
                    && introduced != term
                    && crate::nal_filter::variable_introduction_valid(&self.language, &introduced)
                    && !output.contains(&introduced)
                {
                    output.push(introduced);
                }
            }
        }
        output
    }

    fn process_goal(&mut self, goal: Event, visited: &mut Vec<Term>, depth: usize) -> bool {
        let mut best = None;
        self.collect_goal_candidates(goal.term, goal.truth, visited, depth, 1.0, &mut best);
        let babble =
            f64::from(self.random_u15()) < self.motor_babbling_chance * f64::from(MY_RAND_MAX);
        if let Some(candidate) = best
            && candidate.fresh_context
            && candidate.score > MOTOR_BABBLING_SUPPRESSION_THRESHOLD
            && self.execute_candidate(&candidate)
        {
            return true;
        }
        if babble && self.execute_random_operation() {
            return true;
        }
        if let Some(candidate) = best
            && candidate.fresh_context
            && candidate.score > self.decision_threshold
            && self.execute_candidate(&candidate)
        {
            return true;
        }
        false
    }

    #[allow(clippy::cast_possible_truncation)]
    fn execute_candidate(&mut self, candidate: &DecisionCandidate) -> bool {
        let mut components = Vec::new();
        flatten_sequence(&self.language, candidate.precondition, &mut components);
        let operations = components
            .into_iter()
            .filter(|component| self.language.is_operation(component))
            .collect::<Vec<_>>();
        if operations.is_empty()
            || !operations.iter().any(|operation| {
                self.language.operation_atom(operation) == Some(candidate.operation_atom)
            })
        {
            return false;
        }
        for concrete in operations {
            let Some(operation_atom) = self.language.operation_atom(&concrete) else {
                return false;
            };
            let Some(operation) = self
                .operations
                .iter()
                .find(|operation| operation.term.root() == operation_atom)
                .cloned()
            else {
                return false;
            };
            let arguments = self.language.operation_arguments(&concrete);
            if !self.execute_operation(&operation, concrete, arguments) {
                return false;
            }
        }
        let executed = true;
        if executed && let Some(implication) = candidate.missing_specific_implication {
            let delay = (implication.occurrence_time_offset.round() as i64).max(0);
            let anticipation = PendingAnticipation {
                expected_term: implication.term.extract_subterm(2),
                implication_term: implication.term,
                precondition_truth: candidate.context_truth,
                occurrence_time_offset: delay,
                deadline: self.current_time + delay,
                implication_stamp: implication.stamp,
                implication_creation_time: implication.creation_time,
                create_if_missing: true,
            };
            if !self.anticipations.iter().any(|existing| {
                existing.expected_term == anticipation.expected_term
                    && existing.implication_term == anticipation.implication_term
                    && existing.deadline == anticipation.deadline
            }) {
                self.anticipations.push(anticipation);
            }
        }
        executed
    }

    fn execute_random_operation(&mut self) -> bool {
        let available_operations = self.operations.len().min(self.babbling_operations);
        if available_operations == 0 {
            return false;
        }
        let random = u64::from(self.random_u15());
        let operation_count = u64::try_from(available_operations).unwrap_or(u64::MAX);
        let index = usize::try_from(random * operation_count / (u64::from(MY_RAND_MAX) + 1))
            .unwrap_or(0)
            .min(available_operations - 1);
        let operation = self.operations[index].clone();
        let arguments = operation
            .arguments
            .iter()
            .copied()
            .filter(|argument| !argument.is_empty())
            .collect::<Vec<_>>();
        let concrete = if arguments.is_empty() {
            operation.term
        } else {
            let argument_count = u64::try_from(arguments.len()).unwrap_or(u64::MAX);
            let argument_index = usize::try_from(
                u64::from(self.random_u15()) * argument_count / (u64::from(MY_RAND_MAX) + 1),
            )
            .unwrap_or(0)
            .min(arguments.len() - 1);
            self.language
                .operation_with_argument(&operation.term, &arguments[argument_index])
                .unwrap_or(operation.term)
        };
        let arguments = self.language.operation_arguments(&concrete);
        self.execute_operation(&operation, concrete, arguments)
    }

    fn execute_operation(
        &mut self,
        operation: &Operation,
        concrete: Term,
        arguments: Term,
    ) -> bool {
        let feedback = (operation.action)(arguments);
        if feedback.failed {
            return false;
        }
        self.memory.clear_goals();
        let event = self.events.input_event(
            concrete,
            EventType::Belief,
            Truth::new(1.0, 0.9),
            0.0,
            self.current_time,
        );
        if self
            .memory
            .add_input_event(&self.language, event, self.current_time)
        {
            self.arm_anticipations(event);
        }
        self.cycle();
        true
    }

    /// Derives consequents of learned implications from a new belief.
    ///
    /// Input implications are stored for backward (goal) reasoning; this adds
    /// the forward direction so a fact like `tool_x_observed_success` can
    /// produce `stage_y_satisfied` without waiting for a goal.
    fn forward_chain_implications(&mut self, belief: &Event) {
        if belief.event_type != EventType::Belief
            || self.language.copula_equals(belief.term.root(), '?')
            || self.language.copula_equals(belief.term.root(), '$')
        {
            return;
        }
        let derived = self
            .forward_rules
            .iter()
            .filter(|(subject, _, _)| *subject == belief.term)
            .map(|(_, consequent, truth)| (*consequent, belief.truth.deduction(*truth)))
            .collect::<Vec<_>>();
        for (term, truth) in derived {
            let event =
                self.events
                    .input_event(term, EventType::Belief, truth, 0.0, self.current_time);
            let slot = self.operation_slot_for_implication(&event.term);
            self.memory.add_event(
                &self.language,
                Event {
                    occurrence_time: OCCURRENCE_ETERNAL,
                    processed: false,
                    input: false,
                    ..event
                },
                self.current_time,
                AddEventOptions {
                    priority: 0.9,
                    derived: true,
                    eternalize: true,
                    operation_slot: slot,
                    ..AddEventOptions::default()
                },
            );
        }
    }

    #[allow(clippy::too_many_lines)]
    fn collect_goal_candidates(
        &self,
        goal: Term,
        goal_truth: Truth,
        path: &mut Vec<Term>,
        depth: usize,
        chain_score: f64,
        best: &mut Option<DecisionCandidate>,
    ) {
        if depth >= 30 || path.contains(&goal) {
            return;
        }
        path.push(goal);
        if self.language.has_variable(&goal, true, true, true) {
            let concrete_goal = self
                .memory
                .related_concept_ids(&self.language, &goal)
                .into_iter()
                .filter_map(|id| self.memory.by_id(id))
                .filter(|concept| !self.language.has_variable(&concept.term, true, true, true))
                .filter_map(|concept| {
                    let event = if concept.belief_spike.event_type == EventType::Belief {
                        concept.belief_spike
                    } else {
                        concept.belief
                    };
                    if event.event_type != EventType::Belief {
                        return None;
                    }
                    let substitution =
                        crate::Substitution::unify(&self.language, &goal, &concept.term);
                    if !substitution.success {
                        return None;
                    }
                    let concrete = substitution.apply(&self.language, goal)?;
                    Some((concrete, event.occurrence_time, event.truth))
                })
                .max_by(|left, right| {
                    left.1
                        .cmp(&right.1)
                        .then_with(|| left.2.expectation().total_cmp(&right.2.expectation()))
                });
            if let Some((concrete_goal, _, _)) = concrete_goal
                && concrete_goal != goal
            {
                self.collect_goal_candidates(
                    concrete_goal,
                    goal_truth,
                    path,
                    depth + 1,
                    chain_score,
                    best,
                );
            }
            for implication in self.variable_implications_for_goal(goal) {
                self.collect_implication_candidate(
                    implication,
                    goal_truth,
                    path,
                    depth,
                    chain_score,
                    best,
                );
            }
        } else if let Some(concept) = self.memory.find(&goal) {
            let implications = concept
                .precondition_beliefs
                .iter()
                .flat_map(|table| table.iter().copied())
                .filter(|implication| self.memory.implication_valid(implication))
                .collect::<Vec<_>>();
            let allow_learned_lookahead =
                implications.len() <= LEARNED_STATE_LOOKAHEAD_MAX_IMPLICATIONS;
            let mut learned_state_links = Vec::new();
            for implication in implications {
                let precondition = implication.term.extract_subterm(1);
                let is_learned_state_link = self.language.operation_atom(&precondition).is_none()
                    && implication.stamp.evidential_base()[1] != 0;
                if is_learned_state_link {
                    learned_state_links.push(implication);
                    continue;
                }
                self.collect_implication_candidate(
                    implication,
                    goal_truth,
                    path,
                    depth,
                    chain_score,
                    best,
                );
            }
            if !allow_learned_lookahead && best.is_some() {
                learned_state_links.clear();
            } else if !allow_learned_lookahead {
                learned_state_links.truncate(1);
            }
            for implication in learned_state_links {
                self.collect_implication_candidate(
                    implication,
                    goal_truth,
                    path,
                    depth,
                    chain_score,
                    best,
                );
            }
        }
        if !self.language.has_variable(&goal, true, true, true) {
            for implication in self.generic_input_implications_for_goal(goal) {
                self.collect_implication_candidate(
                    implication,
                    goal_truth,
                    path,
                    depth,
                    chain_score,
                    best,
                );
            }
            for implication in self.learned_variable_implications_for_goal(goal) {
                self.collect_implication_candidate(
                    implication,
                    goal_truth,
                    path,
                    depth,
                    chain_score,
                    best,
                );
            }
        }
        path.pop();
    }

    #[allow(clippy::too_many_lines)]
    fn collect_implication_candidate(
        &self,
        implication: crate::Implication,
        goal_truth: Truth,
        path: &mut Vec<Term>,
        depth: usize,
        chain_score: f64,
        best: &mut Option<DecisionCandidate>,
    ) {
        let precondition = implication.term.extract_subterm(1);
        let implication_score = chain_score * implication.truth.expectation();
        let Some(operation_atom) = self.language.operation_atom(&precondition) else {
            if implication.stamp.evidential_base()[1] != 0 {
                if self.language.has_variable(&precondition, true, true, true) {
                    return;
                }
                if !self.goal_has_direct_operation(&precondition) {
                    if let Some(subgoal) = self.next_unmet_sequence_component(&precondition) {
                        self.collect_goal_candidates(
                            subgoal,
                            goal_truth,
                            path,
                            depth + 1,
                            implication_score,
                            best,
                        );
                    }
                    return;
                }
            }
            if self
                .language
                .has_variable(&precondition, false, true, false)
            {
                return;
            }
            self.collect_goal_candidates(
                precondition,
                goal_truth,
                path,
                depth + 1,
                implication_score,
                best,
            );
            return;
        };
        let condition = self.language.precondition_without_operation(&precondition);
        if self.language.has_variable(&condition, true, true, true) {
            let contexts = self.variable_operation_contexts(condition, precondition);
            if contexts.is_empty() {
                if let Some(subgoal) = self.next_unmet_sequence_component(&condition) {
                    self.collect_goal_candidates(
                        subgoal,
                        goal_truth,
                        path,
                        depth + 1,
                        implication_score,
                        best,
                    );
                } else {
                    self.collect_goal_candidates(
                        condition,
                        goal_truth,
                        path,
                        depth + 1,
                        implication_score,
                        best,
                    );
                }
            } else {
                for (concrete_precondition, context_time, fresh_context, context_truth) in contexts
                {
                    self.push_decision_candidate(
                        operation_atom,
                        concrete_precondition,
                        depth,
                        context_time,
                        fresh_context,
                        context_truth,
                        goal_truth,
                        implication,
                        chain_score,
                        best,
                    );
                }
            }
        } else if let Some((context_time, fresh_context, context_truth)) =
            self.condition_decision_context(&condition)
        {
            self.push_decision_candidate(
                operation_atom,
                precondition,
                depth,
                context_time,
                fresh_context,
                context_truth,
                goal_truth,
                implication,
                chain_score,
                best,
            );
        } else {
            self.collect_goal_candidates(
                condition,
                goal_truth,
                path,
                depth + 1,
                implication_score,
                best,
            );
        }
    }

    fn condition_decision_context(&self, condition: &Term) -> Option<(i64, bool, Truth)> {
        if condition.is_empty() || self.language.is_operation(condition) {
            return Some((self.current_time, true, Truth::new(1.0, 0.9)));
        }
        if self.language.copula_equals(condition.root(), '+') {
            let left = self.condition_decision_context(&condition.extract_subterm(1))?;
            let right = self.condition_decision_context(&condition.extract_subterm(2))?;
            return Some((
                left.0.max(right.0),
                left.1 && right.1,
                left.2.intersection(right.2),
            ));
        }
        if let Some(context) = self.similarity_condition_context(condition) {
            return Some(context);
        }
        if let Some((context_time, fresh_context)) = self.condition_context(condition) {
            return Some((
                context_time,
                fresh_context,
                self.condition_truth(condition)
                    .or_else(|| self.condition_current_truth(condition))
                    .unwrap_or_default(),
            ));
        }
        if !condition
            .atoms()
            .iter()
            .any(|atom| self.language.atom_value(*atom).is_some())
        {
            return None;
        }

        self.memory
            .concepts()
            .filter_map(|concept| {
                let belief = concept.belief_spike;
                if belief.event_type == EventType::Deleted {
                    return None;
                }
                let age = self.current_time - belief.occurrence_time;
                if age > EVENT_BELIEF_DISTANCE {
                    return None;
                }
                let substitution = crate::Substitution::unify_with(
                    &self.language,
                    condition,
                    &concept.term,
                    UnifyOptions {
                        truth: belief.truth,
                        similarity_distance: self.similarity_distance,
                        ..UnifyOptions::default()
                    },
                );
                substitution.success.then_some({
                    (
                        belief.occurrence_time,
                        age <= BELIEF_LAST_USED_TOLERANCE,
                        substitution.truth,
                    )
                })
            })
            .max_by(|left, right| left.2.expectation().total_cmp(&right.2.expectation()))
    }

    fn similarity_condition_context(&self, condition: &Term) -> Option<(i64, bool, Truth)> {
        if !self.language.copula_equals(condition.root(), ':') {
            return None;
        }
        let expected_subject = condition.extract_subterm(1);
        let expected_predicate = condition.extract_subterm(2);
        self.memory
            .concepts()
            .filter_map(|concept| {
                if !self.language.copula_equals(concept.term.root(), ':')
                    || concept.term.extract_subterm(2) != expected_predicate
                {
                    return None;
                }
                let belief = concept.belief_spike;
                if belief.event_type == EventType::Deleted {
                    return None;
                }
                let age = self.current_time - belief.occurrence_time;
                if age > EVENT_BELIEF_DISTANCE {
                    return None;
                }
                let observed_subject = concept.term.extract_subterm(1);
                let similarity_truth = self
                    .similarity_truth(&expected_subject, &observed_subject)
                    .or_else(|| self.similarity_truth(&observed_subject, &expected_subject))?;
                Some((
                    belief.occurrence_time,
                    true,
                    belief.truth.analogy(similarity_truth),
                ))
            })
            .max_by(|left, right| left.2.expectation().total_cmp(&right.2.expectation()))
    }

    fn similarity_truth(&self, left: &Term, right: &Term) -> Option<Truth> {
        self.memory.concepts().find_map(|concept| {
            (self.language.copula_equals(concept.term.root(), '=')
                && concept.term.extract_subterm(1) == *left
                && concept.term.extract_subterm(2) == *right
                && concept.belief.event_type != EventType::Deleted)
                .then_some(concept.belief.truth)
        })
    }

    fn goal_has_direct_operation(&self, goal: &Term) -> bool {
        self.memory.find(goal).is_some_and(|concept| {
            concept
                .precondition_beliefs
                .iter()
                .flat_map(crate::ImplicationTable::iter)
                .filter(|implication| self.memory.implication_valid(implication))
                .any(|implication| {
                    self.language
                        .operation_atom(&implication.term.extract_subterm(1))
                        .is_some()
                })
        })
    }

    fn next_unmet_sequence_component(&self, sequence: &Term) -> Option<Term> {
        if !self.language.copula_equals(sequence.root(), '+') {
            return None;
        }
        let mut components = Vec::new();
        flatten_sequence(&self.language, *sequence, &mut components);
        components
            .into_iter()
            .find(|component| self.condition_decision_context(component).is_none())
    }

    #[allow(clippy::too_many_arguments)]
    fn push_decision_candidate(
        &self,
        operation_atom: Atom,
        precondition: Term,
        goal_depth: usize,
        context_time: i64,
        fresh_context: bool,
        context_truth: Truth,
        goal_truth: Truth,
        implication: crate::Implication,
        chain_score: f64,
        best: &mut Option<DecisionCandidate>,
    ) {
        if !self.operation_allowed_by_recent_goals(operation_atom, precondition) {
            return;
        }
        let condition = self.language.precondition_without_operation(&precondition);
        let specificity = self.language.sequence_length(&condition);
        let decision_context_truth = if context_time <= i64::MIN / 4 {
            context_truth
        } else {
            context_truth.project(context_time, self.current_time)
        };
        let desire = goal_truth
            .goal_deduction(implication.truth)
            .goal_deduction(decision_context_truth)
            .expectation();
        let score = 0.5 + (desire - 0.5) * chain_score;
        let conclusion = implication.term.extract_subterm(2);
        let missing_specific_implication =
            (!self
                .language
                .has_variable(&implication.term, true, true, true)
                && self.memory.find(&conclusion).is_none_or(|concept| {
                    concept
                        .precondition_beliefs
                        .iter()
                        .flat_map(crate::ImplicationTable::iter)
                        .all(|specific| specific.term != implication.term)
                }))
            .then_some(implication);
        let candidate = DecisionCandidate {
            operation_atom,
            precondition,
            grounded: !self.language.has_variable(&precondition, true, true, true),
            goal_depth,
            fresh_context,
            context_time,
            specificity,
            score,
            context_truth,
            missing_specific_implication,
        };
        if best
            .as_ref()
            .is_none_or(|current| decision_candidate_is_better(&candidate, current))
        {
            *best = Some(candidate);
        }
    }

    fn operation_allowed_by_recent_goals(
        &self,
        operation_atom: Atom,
        candidate_precondition: Term,
    ) -> bool {
        let Some(operation_slot) = self
            .operations
            .iter()
            .position(|operation| operation.term.root() == operation_atom)
            .map(|index| index + 1)
        else {
            return false;
        };
        let candidate_operation = self.language.operation_term(&candidate_precondition);
        let mut combined: Option<Event> = None;
        for outcome_concept in self.memory.concepts() {
            let goal = outcome_concept.goal_spike;
            if goal.event_type != EventType::Goal
                || self.current_time - goal.occurrence_time >= EVENT_BELIEF_DISTANCE
            {
                continue;
            }
            for implication in outcome_concept.precondition_beliefs[operation_slot]
                .iter()
                .copied()
                .filter(|implication| self.memory.implication_valid(implication))
            {
                let implication_precondition = implication.term.extract_subterm(1);
                if self.language.operation_term(&implication_precondition) != candidate_operation {
                    continue;
                }
                let Some(source) = implication
                    .source_concept_id
                    .and_then(|id| self.memory.by_id(crate::ConceptId(id)))
                else {
                    continue;
                };
                let belief = source.belief_spike;
                if belief.event_type != EventType::Belief
                    || self.current_time - belief.occurrence_time >= EVENT_BELIEF_DISTANCE
                {
                    continue;
                }
                let local = Event {
                    term: self
                        .language
                        .precondition_without_operation(&implication_precondition),
                    event_type: EventType::Goal,
                    truth: goal
                        .truth
                        .goal_deduction(implication.truth)
                        .goal_deduction(belief.truth),
                    stamp: goal.stamp.zip(&implication.stamp).zip(&belief.stamp),
                    occurrence_time: self.current_time,
                    creation_time: self.current_time,
                    ..Event::default()
                };
                combined = Some(combined.map_or(local, |existing| {
                    revision_and_choice(&self.language, existing, local).0
                }));
            }
        }
        combined.is_none_or(|outcome| outcome.truth.expectation() >= 0.5)
    }

    fn variable_implications_for_goal(&self, goal: Term) -> Vec<crate::Implication> {
        let mut output = Vec::new();
        for concept in self.memory.concepts() {
            let substitution = crate::Substitution::unify(&self.language, &goal, &concept.term);
            if !substitution.success {
                continue;
            }
            for implication in concept
                .precondition_beliefs
                .iter()
                .flat_map(|table| table.iter().copied())
                .filter(|implication| self.memory.implication_valid(implication))
            {
                let Some(term) = substitution.apply(&self.language, implication.term) else {
                    continue;
                };
                if self.memory.find(&goal).is_some_and(|specific_concept| {
                    specific_concept
                        .precondition_beliefs
                        .iter()
                        .flat_map(crate::ImplicationTable::iter)
                        .any(|specific| {
                            specific.term == term
                                && specific.truth.confidence > SUBSUMPTION_CONFIDENCE_THRESHOLD
                                && specific.truth.frequency < SUBSUMPTION_FREQUENCY_THRESHOLD
                        })
                }) {
                    continue;
                }
                output.push(crate::Implication {
                    term,
                    ..implication
                });
            }
        }
        output
    }

    fn generic_input_implications_for_goal(&self, goal: Term) -> Vec<crate::Implication> {
        let mut output = Vec::new();
        for id in self.memory.generic_input_procedural_concept_ids() {
            let Some(concept) = self.memory.by_id(*id) else {
                continue;
            };
            let substitution = crate::Substitution::unify(&self.language, &concept.term, &goal);
            if !substitution.success {
                continue;
            }
            for implication in concept
                .precondition_beliefs
                .iter()
                .flat_map(|table| table.iter().copied())
                .filter(|implication| {
                    implication.stamp.evidential_base()[1] == 0
                        && self.memory.implication_valid(implication)
                })
            {
                let Some(term) = substitution.apply(&self.language, implication.term) else {
                    continue;
                };
                if self.memory.find(&goal).is_some_and(|specific_concept| {
                    specific_concept
                        .precondition_beliefs
                        .iter()
                        .flat_map(crate::ImplicationTable::iter)
                        .any(|specific| {
                            specific.term == term
                                && specific.truth.confidence > SUBSUMPTION_CONFIDENCE_THRESHOLD
                                && specific.truth.frequency < SUBSUMPTION_FREQUENCY_THRESHOLD
                        })
                }) {
                    continue;
                }
                output.push(crate::Implication {
                    term,
                    ..implication
                });
            }
        }
        output
    }

    fn learned_variable_implications_for_goal(&self, goal: Term) -> Vec<crate::Implication> {
        let mut output = Vec::new();
        for id in self.memory.variable_procedural_concept_ids() {
            let Some(concept) = self.memory.by_id(*id) else {
                continue;
            };
            let substitution = crate::Substitution::unify(&self.language, &concept.term, &goal);
            if !substitution.success {
                continue;
            }
            for implication in concept
                .precondition_beliefs
                .iter()
                .flat_map(|table| table.iter().copied())
                .filter(|implication| {
                    implication.stamp.evidential_base()[1] != 0
                        && self.memory.implication_valid(implication)
                })
            {
                let Some(term) = substitution.apply(&self.language, implication.term) else {
                    continue;
                };
                output.push(crate::Implication {
                    term,
                    ..implication
                });
            }
        }
        output
    }

    fn variable_operation_contexts(
        &self,
        condition: Term,
        precondition: Term,
    ) -> Vec<(Term, i64, bool, Truth)> {
        let mut output = Vec::new();
        for concept in self.memory.concepts() {
            if concept.belief_spike.event_type == EventType::Deleted {
                continue;
            }
            let substitution = crate::Substitution::unify_with(
                &self.language,
                &condition,
                &concept.term,
                UnifyOptions {
                    truth: concept.belief_spike.truth,
                    similarity_distance: self.similarity_distance,
                    ..UnifyOptions::default()
                },
            );
            let (substitution, property_matched) = if substitution.success {
                (substitution, false)
            } else if let Some(substitution) = self.property_matching_substitution(
                &condition,
                &concept.term,
                concept.belief_spike.truth,
            ) {
                (substitution, true)
            } else {
                continue;
            };
            let Some(concrete_precondition) = substitution.apply(&self.language, precondition)
            else {
                continue;
            };
            let age = self.current_time - concept.belief_spike.occurrence_time;
            if age > EVENT_BELIEF_DISTANCE {
                continue;
            }
            output.push((
                concrete_precondition,
                concept.belief_spike.occurrence_time,
                property_matched
                    || age <= BELIEF_LAST_USED_TOLERANCE
                    || self.question_answer_recent(&concept.term),
                substitution.truth,
            ));
        }
        output
    }

    /// Matches a relational condition through an inherited property. For
    /// example, `<plastic --> [bendable]>` lets
    /// `<({toothbrush} * [bendable]) --> made_of>` satisfy
    /// `<({$1} * plastic) --> made_of>` while binding `$1` to the toothbrush.
    fn property_matching_substitution(
        &self,
        condition: &Term,
        observation: &Term,
        observation_truth: Truth,
    ) -> Option<crate::Substitution> {
        if !self.language.copula_equals(condition.root(), ':')
            || observation.root() != condition.root()
        {
            return None;
        }
        let condition_subject = condition.extract_subterm(1);
        let observation_subject = observation.extract_subterm(1);
        if !self.language.copula_equals(condition_subject.root(), '*')
            || observation_subject.root() != condition_subject.root()
            || condition.extract_subterm(2) != observation.extract_subterm(2)
        {
            return None;
        }

        let required_property = condition_subject.extract_subterm(2);
        let observed_property = observation_subject.extract_subterm(2);
        let mut property_statement = Term::from_atom(condition.root());
        if !property_statement.override_subterm(1, &required_property)
            || !property_statement.override_subterm(2, &observed_property)
        {
            return None;
        }
        let property_concept = self.memory.find(&property_statement)?;
        let property = if property_concept.belief.event_type != EventType::Deleted {
            property_concept.belief
        } else if property_concept.belief_spike.event_type != EventType::Deleted {
            property_concept.belief_spike
        } else {
            return None;
        };

        let mut substitution = crate::Substitution::unify_with(
            &self.language,
            &condition_subject.extract_subterm(1),
            &observation_subject.extract_subterm(1),
            UnifyOptions {
                truth: observation_truth,
                similarity_distance: self.similarity_distance,
                ..UnifyOptions::default()
            },
        );
        if !substitution.success {
            return None;
        }
        substitution.truth = substitution.truth.analogy(property.truth);
        Some(substitution)
    }

    fn condition_context(&self, condition: &Term) -> Option<(i64, bool)> {
        if condition.is_empty() {
            return Some((self.current_time, true));
        }
        if self.language.is_operation(condition) {
            return Some((self.current_time, true));
        }
        if self.language.copula_equals(condition.root(), '+') {
            let left = self.condition_context(&condition.extract_subterm(1))?;
            let right = self.condition_context(&condition.extract_subterm(2))?;
            return Some((left.0.max(right.0), left.1 && right.1));
        }
        let concept = self.memory.find(condition)?;
        if concept.belief_spike.event_type != EventType::Deleted
            && self.current_time - concept.belief_spike.occurrence_time <= EVENT_BELIEF_DISTANCE
        {
            Some((
                concept.belief_spike.occurrence_time,
                self.current_time - concept.belief_spike.occurrence_time
                    <= BELIEF_LAST_USED_TOLERANCE
                    || self.question_answer_recent(condition),
            ))
        } else if concept.belief.event_type != EventType::Deleted {
            Some((i64::MIN / 2, false))
        } else {
            None
        }
    }

    fn question_answer_recent(&self, term: &Term) -> bool {
        self.recent_question_answers.iter().any(|(answer, time)| {
            answer == term && self.current_time - *time <= BELIEF_LAST_USED_TOLERANCE
        })
    }

    fn condition_truth(&self, condition: &Term) -> Option<Truth> {
        if condition.is_empty() || self.language.is_operation(condition) {
            return Some(Truth::new(1.0, 0.9));
        }
        if let Some(concept) = self.memory.find(condition)
            && concept.belief_spike.event_type != EventType::Deleted
            && self.current_time - concept.belief_spike.occurrence_time <= EVENT_BELIEF_DISTANCE
        {
            return Some(concept.belief_spike.truth);
        }
        if self.language.copula_equals(condition.root(), '+') {
            return Some(
                self.condition_truth(&condition.extract_subterm(1))?
                    .intersection(self.condition_truth(&condition.extract_subterm(2))?),
            );
        }
        None
    }

    fn condition_current_truth(&self, condition: &Term) -> Option<Truth> {
        if condition.is_empty() || self.language.is_operation(condition) {
            return Some(Truth::new(1.0, 0.9));
        }
        if let Some(concept) = self.memory.find(condition) {
            if concept.belief_spike.event_type != EventType::Deleted
                && self.current_time - concept.belief_spike.occurrence_time <= EVENT_BELIEF_DISTANCE
            {
                return Some(
                    concept
                        .belief_spike
                        .truth
                        .project(concept.belief_spike.occurrence_time, self.current_time),
                );
            }
            if concept.belief.event_type != EventType::Deleted {
                return Some(concept.belief.truth);
            }
        }
        if self.language.copula_equals(condition.root(), '+') {
            return Some(
                self.condition_current_truth(&condition.extract_subterm(1))?
                    .intersection(self.condition_current_truth(&condition.extract_subterm(2))?),
            );
        }
        None
    }

    #[must_use]
    pub fn belief(&mut self, source: &str) -> Option<Event> {
        let term = self.language.parse_term(source).ok()?;
        self.memory.find(&term).map(|concept| concept.belief)
    }

    #[must_use]
    pub fn stats_snapshot(&self) -> RuntimeStats {
        let concepts = self.memory.concepts().collect::<Vec<_>>();
        let belief_priority_sum = self
            .memory
            .cycling_beliefs
            .iter()
            .map(|item| item.priority)
            .sum::<f64>();
        let goal_priority_sum = self
            .memory
            .cycling_goals
            .iter()
            .flat_map(crate::PriorityQueue::iter)
            .map(|item| item.priority)
            .sum::<f64>();
        let concept_capacity = f64::from(u32::try_from(CONCEPTS_MAX).unwrap_or(u32::MAX));
        let belief_capacity =
            f64::from(u32::try_from(CYCLING_BELIEF_EVENTS_MAX).unwrap_or(u32::MAX));
        let goal_capacity = f64::from(u32::try_from(CYCLING_GOAL_EVENTS_MAX).unwrap_or(u32::MAX));
        RuntimeStats {
            current_time: self.current_time,
            total_concepts: concepts.len(),
            atoms_used: self.language.atom_count(),
            belief_events: self.memory.cycling_beliefs.len(),
            goal_events: self
                .memory
                .cycling_goals
                .iter()
                .map(crate::PriorityQueue::len)
                .sum(),
            operations: self.operations.len(),
            pending_anticipations: self.anticipations.len(),
            max_declarative_implications: concepts
                .iter()
                .map(|concept| concept.implication_links.len())
                .max()
                .unwrap_or(0),
            max_temporal_implications: concepts
                .iter()
                .flat_map(|concept| concept.precondition_beliefs.iter())
                .map(crate::ImplicationTable::len)
                .max()
                .unwrap_or(0),
            average_concept_priority: concepts.iter().map(|concept| concept.priority).sum::<f64>()
                / concept_capacity,
            average_concept_usefulness: concepts
                .iter()
                .map(|concept| concept.usage.usefulness(self.current_time))
                .sum::<f64>()
                / concept_capacity,
            average_belief_event_priority: belief_priority_sum / belief_capacity,
            average_goal_event_priority: goal_priority_sum / goal_capacity,
        }
    }

    #[must_use]
    pub fn concepts_snapshot(&self) -> Vec<ConceptSnapshot> {
        self.memory
            .concepts()
            .map(|concept| ConceptSnapshot {
                term: self.language.format_term(&concept.term),
                priority: concept.priority,
                usefulness: concept.usage.usefulness(self.current_time),
                use_count: concept.usage.use_count,
                last_used: concept.usage.last_used,
                belief: self.snapshot_stored_event(concept.belief),
                belief_spike: self.snapshot_stored_event(concept.belief_spike),
                predicted_belief: self.snapshot_stored_event(concept.predicted_belief),
                goal_spike: self.snapshot_stored_event(concept.goal_spike),
                temporal_implications: concept
                    .precondition_beliefs
                    .iter()
                    .enumerate()
                    .flat_map(|(slot, table)| {
                        table.iter().map(move |implication| {
                            self.snapshot_implication(*implication, (slot > 0).then_some(slot))
                        })
                    })
                    .collect(),
                declarative_implications: concept
                    .implication_links
                    .iter()
                    .map(|implication| self.snapshot_implication(*implication, None))
                    .collect(),
            })
            .collect()
    }

    #[must_use]
    pub fn belief_queue_snapshot(&self) -> Vec<EventSnapshot> {
        self.memory
            .cycling_beliefs
            .iter()
            .map(|item| self.snapshot_queued_event(item.value, item.priority, None))
            .collect()
    }

    #[must_use]
    pub fn goal_queue_snapshot(&self) -> Vec<EventSnapshot> {
        self.memory
            .cycling_goals
            .iter()
            .enumerate()
            .flat_map(|(layer, queue)| {
                queue.iter().map(move |item| {
                    self.snapshot_queued_event(item.value, item.priority, Some(layer))
                })
            })
            .collect()
    }

    #[must_use]
    pub fn operation_config_snapshot(&self) -> OperationConfigSnapshot {
        let operations = self
            .operations
            .iter()
            .enumerate()
            .filter(|(_, operation)| !operation.term.is_empty())
            .map(|(index, operation)| OperationSnapshot {
                slot: index + 1,
                name: self.language.format_term(&operation.term),
                arguments: operation
                    .arguments
                    .iter()
                    .enumerate()
                    .filter(|(_, argument)| !argument.is_empty())
                    .map(|(argument, term)| (argument + 1, self.language.format_term(term)))
                    .collect(),
                stdin_output: operation.stdin_output,
            })
            .collect();
        OperationConfigSnapshot {
            motor_babbling_chance: self.motor_babbling_chance,
            babbling_operations: self.babbling_operations,
            operations,
        }
    }

    #[must_use]
    pub fn inverted_atom_index_snapshot(&self) -> Vec<AtomIndexSnapshot> {
        (1..=self.language.atom_count())
            .filter_map(|index| u8::try_from(index).ok())
            .filter_map(|atom| {
                let concepts = self.memory.inverted_index.concepts(atom);
                (!concepts.is_empty()).then(|| AtomIndexSnapshot {
                    atom: self.language.atom_name(atom).unwrap_or("@").to_string(),
                    concepts: concepts
                        .iter()
                        .filter_map(|id| self.memory.by_id(*id))
                        .map(|concept| self.language.format_term(&concept.term))
                        .collect(),
                })
            })
            .collect()
    }

    #[must_use]
    pub fn occurrence_time_index_snapshot(&self) -> Vec<OccurrenceIndexSnapshot> {
        (0..self.memory.occurrence_time_index.len())
            .filter_map(|newest_rank| {
                let id = self.memory.occurrence_time_index.kth_newest(newest_rank)?;
                let concept = self.memory.by_id(id)?;
                Some(OccurrenceIndexSnapshot {
                    newest_rank,
                    concept: self.language.format_term(&concept.term),
                })
            })
            .collect()
    }

    fn snapshot_stored_event(&self, event: Event) -> Option<EventSnapshot> {
        (event.event_type != EventType::Deleted)
            .then(|| self.snapshot_queued_event(event, 1.0, None))
    }

    fn snapshot_queued_event(
        &self,
        event: Event,
        priority: f64,
        layer: Option<usize>,
    ) -> EventSnapshot {
        EventSnapshot {
            term: self.language.format_term(&event.term),
            event_type: event.event_type,
            truth: event.truth,
            stamp: event.stamp,
            occurrence_time: event.occurrence_time,
            creation_time: event.creation_time,
            priority,
            layer,
        }
    }

    fn snapshot_implication(
        &self,
        implication: crate::Implication,
        operation_slot: Option<usize>,
    ) -> ImplicationSnapshot {
        ImplicationSnapshot {
            term: self.language.format_term(&implication.term),
            truth: implication.truth,
            stamp: implication.stamp,
            occurrence_time_offset: implication.occurrence_time_offset,
            creation_time: implication.creation_time,
            operation_slot,
        }
    }
}

fn flatten_sequence(language: &Narsese, term: Term, output: &mut Vec<Term>) {
    if language.copula_equals(term.root(), '+') {
        flatten_sequence(language, term.extract_subterm(1), output);
        flatten_sequence(language, term.extract_subterm(2), output);
    } else {
        output.push(term);
    }
}

fn decision_candidate_is_better(
    candidate: &DecisionCandidate,
    current: &DecisionCandidate,
) -> bool {
    let freshness = candidate.fresh_context.cmp(&current.fresh_context);
    if !freshness.is_eq() {
        return freshness.is_gt();
    }
    let context_time = candidate.context_time.cmp(&current.context_time);
    if candidate.context_time.abs_diff(current.context_time) > 1 && !context_time.is_eq() {
        return context_time.is_gt();
    }
    let score = candidate.score.total_cmp(&current.score);
    if (candidate.score - current.score).abs() > 0.02 && !score.is_eq() {
        return score.is_gt();
    }
    let grounded = candidate.grounded.cmp(&current.grounded);
    if !grounded.is_eq() {
        return grounded.is_gt();
    }
    candidate
        .specificity
        .cmp(&current.specificity)
        .reverse()
        .then(context_time)
        .then(score)
        .then_with(|| current.goal_depth.cmp(&candidate.goal_depth))
        .is_gt()
}

fn validate_probability(value: f64, name: &str) -> Result<(), ReasonerError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(ReasonerError::new(format!(
            "{name} must be between 0 and 1"
        )))
    }
}

fn select_answer(best: &mut Option<(Answer, Truth)>, candidate: Answer, ranking_truth: Truth) {
    let replace = best.as_ref().is_none_or(|(current, current_ranking)| {
        ranking_truth
            .expectation()
            .total_cmp(&current_ranking.expectation())
            .then_with(|| {
                candidate
                    .truth
                    .expectation()
                    .total_cmp(&current.truth.expectation())
            })
            .then_with(|| candidate.occurrence_time.cmp(&current.occurrence_time))
            .is_gt()
    });
    if replace {
        *best = Some((candidate, ranking_truth));
    }
}

impl Default for Reasoner {
    fn default() -> Self {
        Self::new().expect("bundled ONA rule table must be valid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_rule_derives_satisfaction() {
        let mut reasoner = Reasoner::new().expect("reasoner");
        reasoner
            .add_sentence("<tool_apply_patch_outcome_success ==> stage_change_satisfied>.")
            .expect("rule");
        reasoner
            .add_sentence("tool_apply_patch_outcome_success.")
            .expect("fact");
        reasoner.cycles(5);
        let expectation = reasoner.belief_expectation("stage_change_satisfied");
        eprintln!("stage={expectation:?}");
        assert!(
            expectation.is_some_and(|value| value > 0.5),
            "{expectation:?}"
        );
    }

    #[test]
    fn implication_forward_chains_a_belief() {
        let mut reasoner = Reasoner::new().expect("reasoner");
        reasoner.add_sentence("<a ==> b>.").expect("rule");
        reasoner.add_sentence("a.").expect("belief");
        reasoner.cycles(10);
        eprintln!("b={:?}", reasoner.belief_expectation("b"));
        assert!(
            reasoner.belief_expectation("b").is_some(),
            "forward chaining should derive b"
        );
    }

    use super::*;

    #[test]
    fn learned_outcomes_are_visible_to_ranking() {
        let mut reasoner = Reasoner::new().expect("reasoner");
        reasoner
            .add_operation("^read_file", |_| OperationFeedback::default())
            .expect("operation");
        reasoner
            .add_operation("^search", |_| OperationFeedback::default())
            .expect("operation");
        assert!(reasoner.learn_tool_outcome("goal_test", "read_file", false));
        assert!(reasoner.learn_tool_outcome("goal_test", "search", true));
        let ranked = reasoner.rank_goal_operations("goal_test");
        assert_eq!(ranked.first().map(|entry| entry.0.as_str()), Some("search"));
    }

    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::Reasoner;
    use crate::{EventType, OperationFeedback, Stamp, Term, Truth};

    #[test]
    fn derived_event_path_applies_ona_structural_filters() {
        let mut reasoner = Reasoner::new().unwrap();
        let invalid = reasoner.language.parse_term("<a --> a>").unwrap();
        let valid = reasoner
            .language
            .parse_term("<(a * a) --> relation>")
            .unwrap();
        let mut pending = Vec::new();
        reasoner.collect_derivation(
            invalid,
            Truth::new(1.0, 0.9),
            false,
            Stamp::from_evidence([1]),
            1,
            0.0,
            1.0,
            1.0,
            true,
            &mut pending,
        );
        assert!(pending.is_empty());

        reasoner.collect_derivation(
            valid,
            Truth::new(1.0, 0.9),
            false,
            Stamp::from_evidence([1]),
            1,
            0.0,
            1.0,
            1.0,
            true,
            &mut pending,
        );
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0.term, valid);
    }

    #[test]
    fn parsed_sentence_path_matches_text_input_path() {
        let mut text_reasoner = Reasoner::new().unwrap();
        let mut prepared_reasoner = Reasoner::new().unwrap();
        let prepared = prepared_reasoner
            .language
            .parse_sentence("state. :|: {0.8 0.7}")
            .unwrap();

        let text_event = text_reasoner.add_sentence("state. :|: {0.8 0.7}").unwrap();
        let prepared_event = prepared_reasoner.add_parsed_sentence(prepared).unwrap();

        assert_eq!(prepared_event, text_event);
        assert_eq!(
            prepared_reasoner.current_time(),
            text_reasoner.current_time()
        );
        assert_eq!(prepared_reasoner.memory.len(), text_reasoner.memory.len());
    }

    #[test]
    fn passive_question_returns_best_eternal_query_match() {
        let mut reasoner = Reasoner::new().unwrap();
        reasoner
            .add_sentence("<cat --> animal>. {1.0 0.9}")
            .unwrap();
        reasoner
            .add_sentence("<dog --> animal>. {1.0 0.7}")
            .unwrap();

        let answer = reasoner.ask("<?what --> animal>?").unwrap().unwrap();
        assert_eq!(
            reasoner.language.format_term(&answer.term),
            "<cat --> animal>"
        );
        assert_eq!(answer.truth, Truth::new(1.0, 0.9));
    }

    #[test]
    fn relational_category_question_uses_induction_for_supertype() {
        let mut reasoner = Reasoner::new().unwrap();
        reasoner.add_sentence("<chicken --> animal>.").unwrap();
        reasoner
            .add_sentence("<(chicken * 2) --> has_leg>.")
            .unwrap();
        let query = reasoner
            .language
            .parse_term("<(animal * 2) --> has_leg>")
            .unwrap();
        let answer = reasoner
            .best_relational_category_answer(query, super::Tense::Eternal)
            .expect("relational category composition failed");
        assert!((answer.truth.confidence - 0.447_513_812_154_696_16).abs() < 1.0e-12);
    }

    #[test]
    fn present_question_returns_concrete_event_for_query_variables() {
        let mut reasoner = Reasoner::new().unwrap();
        reasoner
            .add_sentence("<({object} * {room}) --> at>. :|:")
            .unwrap();

        let answer = reasoner
            .ask("<({object} * ?where) --> at>? :|:")
            .unwrap()
            .unwrap();
        assert_eq!(
            reasoner.language.format_term(&answer.term),
            "<({object} * {room}) --> at>"
        );
        assert_ne!(answer.occurrence_time, crate::config::OCCURRENCE_ETERNAL);
    }

    #[test]
    fn temporal_implication_question_searches_procedure_tables() {
        let mut reasoner = Reasoner::new().unwrap();
        reasoner.add_sentence("<(a &/ ^left) =/> g>.").unwrap();

        let answer = reasoner
            .ask("<(?condition &/ ^left) =/> g>?")
            .unwrap()
            .unwrap();
        assert_eq!(
            reasoner.language.format_term(&answer.term),
            "<(a &/ ^left) =/> g>"
        );
    }

    #[test]
    fn recent_question_answer_primes_an_older_operation_context() {
        let executed = Arc::new(AtomicBool::new(false));
        let callback = Arc::clone(&executed);
        let mut reasoner = Reasoner::new().unwrap();
        reasoner.set_motor_babbling_chance(0.0);
        reasoner
            .add_operation("^wave", move |_| {
                callback.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();
        reasoner
            .add_sentence("<(<B2 --> [seen]> &/ ^wave) =/> G>.")
            .unwrap();
        reasoner.add_sentence("<B2 --> [seen]>. :|:").unwrap();
        reasoner.cycles(10);
        assert!(reasoner.ask("<B2 --> [seen]>? :|:").unwrap().is_some());
        reasoner.add_sentence("G! :|:").unwrap();

        assert!(executed.load(Ordering::SeqCst));
    }

    #[test]
    fn reregistering_operation_replaces_callback_in_place() {
        let first_called = Arc::new(AtomicBool::new(false));
        let first_flag = Arc::clone(&first_called);
        let second_called = Arc::new(AtomicBool::new(false));
        let second_flag = Arc::clone(&second_called);
        let mut reasoner = Reasoner::new().unwrap();
        assert_eq!(
            reasoner
                .add_operation("^left", move |_| {
                    first_flag.store(true, Ordering::Release);
                    OperationFeedback::default()
                })
                .unwrap(),
            1
        );
        assert_eq!(
            reasoner
                .add_operation("^left", move |_| {
                    second_flag.store(true, Ordering::Release);
                    OperationFeedback::default()
                })
                .unwrap(),
            1
        );
        assert_eq!(reasoner.operations.len(), 1);
        (reasoner.operations[0].action)(Term::EMPTY);
        assert!(!first_called.load(Ordering::Acquire));
        assert!(second_called.load(Ordering::Acquire));
    }

    #[test]
    fn shell_operation_rename_clears_duplicate_and_following_slots() {
        let mut reasoner = Reasoner::new().unwrap();
        for name in ["^left", "^right", "^up", "^go", "^activate"] {
            reasoner
                .add_operation(name, |_| OperationFeedback::default())
                .unwrap();
        }
        reasoner.set_operation_name(1, "^go").unwrap();
        let operations = reasoner.operation_config_snapshot().operations;
        assert_eq!(operations.len(), 3);
        assert_eq!(operations[0].name, "^go");
        assert_eq!(operations[1].name, "^right");
        assert_eq!(operations[2].name, "^up");
    }

    #[test]
    fn procedural_implications_are_partitioned_by_operation_slot() {
        let mut reasoner = Reasoner::new().unwrap();
        reasoner
            .add_operation("^left", |_| OperationFeedback::default())
            .unwrap();
        reasoner
            .add_operation("^right", |_| OperationFeedback::default())
            .unwrap();
        reasoner.add_sentence("<(a &/ ^right) =/> g>.").unwrap();

        let goal = reasoner.language.parse_term("g").unwrap();
        let concept = reasoner.memory.find(&goal).unwrap();
        assert!(concept.precondition_beliefs[0].is_empty());
        assert!(concept.precondition_beliefs[1].is_empty());
        assert_eq!(concept.precondition_beliefs[2].len(), 1);
    }

    #[test]
    fn procedural_knowledge_about_another_agents_operation_is_rejected() {
        let mut reasoner = Reasoner::new().unwrap();
        reasoner
            .add_operation("^give", |_| OperationFeedback::default())
            .unwrap();
        reasoner
            .add_sentence("<(seen &/ <({OTHER} * item) --> ^give>) =/> reward>.")
            .unwrap();

        let reward = reasoner.language.parse_term("reward").unwrap();
        let reward_concept = reasoner.memory.find(&reward).unwrap();
        assert!(
            reward_concept
                .precondition_beliefs
                .iter()
                .all(crate::ImplicationTable::is_empty)
        );

        let seen = reasoner.language.parse_term("seen").unwrap();
        assert!(reasoner.memory.find(&seen).is_none());
    }

    #[test]
    fn layered_goal_queue_propagates_no_operation_subgoals() {
        let mut reasoner = Reasoner::new().unwrap();
        reasoner.set_motor_babbling_chance(0.0);
        reasoner.add_sentence("<b =/> c>.").unwrap();
        reasoner.add_sentence("<(a &/ ^missing) =/> b>.").unwrap();
        reasoner.add_goal("c", Truth::new(1.0, 0.9)).unwrap();

        for name in ["c", "b", "a"] {
            let term = reasoner.language.parse_term(name).unwrap();
            let concept = reasoner.memory.find(&term).unwrap();
            assert_eq!(concept.goal_spike.event_type, EventType::Goal, "{name}");
        }
    }

    #[test]
    fn sequence_goal_advances_to_first_unfulfilled_component() {
        let mut reasoner = Reasoner::new().unwrap();
        reasoner.set_motor_babbling_chance(0.0);
        reasoner.add_sentence("(a &/ b)! :|:").unwrap();
        let a = reasoner.language.parse_term("a").unwrap();
        assert_eq!(
            reasoner.memory.find(&a).unwrap().goal_spike.event_type,
            EventType::Goal
        );

        reasoner.add_sentence("a. :|:").unwrap();
        reasoner.add_sentence("(a &/ b)! :|:").unwrap();
        let b = reasoner.language.parse_term("b").unwrap();
        assert_eq!(
            reasoner.memory.find(&b).unwrap().goal_spike.event_type,
            EventType::Goal
        );
    }

    #[test]
    fn upstream_active_question_binds_location_into_go_and_say() {
        let go_argument = Arc::new(Mutex::new(None));
        let go_capture = Arc::clone(&go_argument);
        let say_argument = Arc::new(Mutex::new(None));
        let say_capture = Arc::clone(&say_argument);
        let mut reasoner = Reasoner::new().unwrap();
        reasoner.set_motor_babbling_chance(0.0);
        reasoner
            .add_operation("^go", move |argument| {
                *go_capture.lock().unwrap() = Some(argument);
                OperationFeedback::default()
            })
            .unwrap();
        reasoner
            .add_operation("^say", move |argument| {
                *say_capture.lock().unwrap() = Some(argument);
                OperationFeedback::default()
            })
            .unwrap();

        reasoner
            .add_sentence("<corridor --> location>. :|:")
            .unwrap();
        reasoner
            .add_sentence("<({SELF} * bedroom) --> ^go>. :|:")
            .unwrap();
        reasoner
            .add_sentence("<({cat} * bedroom) --> in>. :|:")
            .unwrap();
        reasoner.cycles(6);
        reasoner
            .add_sentence("<corridor --> location>. :|:")
            .unwrap();
        reasoner
            .add_sentence("<(<({cat} * #where) --> in> &/ <({SELF} * #where) --> ^say>) =/> G>.")
            .unwrap();

        reasoner.add_sentence("G! :|:").unwrap();
        let go = go_argument.lock().unwrap().expect("^go was not executed");
        assert_eq!(reasoner.language.format_term(&go), "({SELF} * bedroom)");

        reasoner
            .add_sentence("<({cat} * bedroom) --> in>. :|:")
            .unwrap();
        reasoner.add_sentence("G! :|:").unwrap();
        let say = say_argument.lock().unwrap().expect("^say was not executed");
        assert_eq!(reasoner.language.format_term(&say), "({SELF} * bedroom)");
    }

    #[test]
    fn upstream_rule_table_integration_derives_syllogism() {
        let mut reasoner = Reasoner::new().unwrap();
        reasoner
            .add_belief("<cat --> animal>", Truth::new(1.0, 0.9), true)
            .unwrap();
        reasoner
            .add_belief("<animal --> being>", Truth::new(1.0, 0.9), true)
            .unwrap();
        reasoner.cycles(1);
        let derived = reasoner.belief("<cat --> being>").unwrap();
        assert_eq!(derived.event_type, EventType::Belief);
        assert!(derived.truth.confidence > 0.0);
    }

    #[test]
    fn upstream_penguin_example_preserves_stronger_negative_exception() {
        let mut reasoner = Reasoner::new().unwrap();
        for sentence in [
            "<pingu --> penguin>.",
            "<sam --> raven>.",
            "<<$1 --> penguin> ==> (! <$1 --> [fly]>)>.",
            "<raven --> bird>.",
            "<penguin --> bird>.",
            "<bird --> [fly]>.",
        ] {
            reasoner.add_sentence(sentence).unwrap();
        }
        reasoner.cycles(100);

        let sam = reasoner.belief("<sam --> [fly]>").unwrap();
        assert!((sam.truth.frequency - 1.0).abs() < f64::EPSILON);
        assert!((sam.truth.confidence - 0.729).abs() < 1.0e-12);
        let pingu = reasoner.belief("<pingu --> [fly]>").unwrap();
        let negated = reasoner.belief("(! <pingu --> [fly]>)");
        assert!(
            pingu.truth.frequency.abs() < f64::EPSILON,
            "pingu={pingu:?}, negated={negated:?}"
        );
        assert!((pingu.truth.confidence - 0.81).abs() < 1.0e-12);
    }

    #[test]
    fn upstream_avoid_example_rejects_operation_with_negative_goal_outcome() {
        let left = Arc::new(AtomicBool::new(false));
        let right = Arc::new(AtomicBool::new(false));
        let left_capture = Arc::clone(&left);
        let right_capture = Arc::clone(&right);
        let mut reasoner = Reasoner::new().unwrap();
        reasoner.set_motor_babbling_chance(0.0);
        reasoner
            .add_operation("^left", move |_| {
                left_capture.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();
        reasoner
            .add_operation("^right", move |_| {
                right_capture.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();
        for sentence in [
            "<(a &/ ^left) =/> T>.",
            "<(a &/ ^left) =/> G>.",
            "<(a &/ ^right) =/> G>. {0.7 0.9}",
            "G! :|:",
            "a. :|:",
            "T! :|: {0.0 0.99}",
            "G! :|:",
        ] {
            reasoner.add_sentence(sentence).unwrap();
        }
        assert!(!left.load(Ordering::SeqCst));
        assert!(right.load(Ordering::SeqCst));
    }

    #[test]
    fn special_inference_eliminates_dependent_variable_condition() {
        let mut reasoner = Reasoner::new().unwrap();
        let rule = reasoner
            .language
            .parse_term(
                "<(<($1 * #1) --> smaller> && <(#1 * $2) --> smaller>) ==> <($1 * $2) --> smaller>>",
            )
            .unwrap();
        let first = reasoner
            .language
            .parse_term("<(1 * 2) --> smaller>")
            .unwrap();
        let second = reasoner
            .language
            .parse_term("<(2 * 3) --> smaller>")
            .unwrap();
        let expected = reasoner
            .language
            .parse_term("<(1 * 3) --> smaller>")
            .unwrap();
        let mut pending = Vec::new();
        reasoner.collect_special_inferences(
            first,
            rule,
            Truth::new(1.0, 0.9),
            Truth::new(1.0, 0.9),
            Stamp::from_evidence([1, 2]),
            1,
            0.0,
            1.0,
            1.0,
            true,
            &mut pending,
        );
        let partial = pending
            .iter()
            .find(|(event, _, _)| reasoner.language.copula_equals(event.term.root(), '?'))
            .map(|(event, _, _)| *event)
            .expect("remaining-condition implication was not derived");
        pending.clear();
        reasoner.collect_special_inferences(
            second,
            partial.term,
            Truth::new(1.0, 0.9),
            partial.truth,
            Stamp::from_evidence([1, 2, 3]),
            2,
            0.0,
            1.0,
            1.0,
            true,
            &mut pending,
        );
        let conclusion = pending
            .iter()
            .find(|(event, _, _)| event.term == expected)
            .map(|(event, _, _)| *event)
            .expect("transitive conclusion was not derived");
        assert!((conclusion.truth.confidence - 0.729).abs() < 1.0e-12);
    }

    #[test]
    fn upstream_numbers_example_runs_declarative_implications_each_cycle() {
        let mut reasoner = Reasoner::new().unwrap();
        for sentence in [
            "<(<($1 * #1) --> smaller> && <(#1 * $2) --> smaller>) ==> <($1 * $2) --> smaller>>.",
            "<(1 * 2) --> smaller>.",
            "<(2 * 3) --> smaller>.",
            "<(3 * 4) --> smaller>.",
        ] {
            reasoner.add_sentence(sentence).unwrap();
        }
        for source in ["<(1 * 3) --> smaller>", "<(2 * 4) --> smaller>"] {
            let answer = reasoner.belief(source).expect(source);
            assert!((answer.truth.frequency - 1.0).abs() < f64::EPSILON);
            assert!((answer.truth.confidence - 0.729).abs() < 1.0e-12);
        }
    }

    #[test]
    fn upstream_duck_example_uses_event_analogy_as_procedure_context() {
        let mut reasoner = Reasoner::new().unwrap();
        reasoner
            .add_operation("^pick", |_| OperationFeedback::default())
            .unwrap();
        reasoner.add_sentence("<duck --> [yellow]>.").unwrap();
        reasoner.cycles(10);
        reasoner
            .add_sentence("<seenThing --> [yellow]>. :|:")
            .unwrap();

        let inferred = reasoner
            .language
            .parse_term("<seenThing --> duck>")
            .unwrap();
        let inferred_concept = reasoner
            .memory
            .find(&inferred)
            .expect("event analogy did not create the context concept");
        assert_eq!(inferred_concept.belief_spike.event_type, EventType::Belief);

        reasoner.add_sentence("^pick. :|:").unwrap();
        reasoner.add_sentence("G. :|:").unwrap();
        let answer = reasoner
            .ask("<(<seenThing --> duck> &/ ^pick) =/> G>?")
            .unwrap()
            .expect("learned procedure was not stored");
        assert!((answer.truth.frequency - 1.0).abs() < f64::EPSILON);
        assert!(answer.truth.confidence >= 0.15846 - 1.0e-6);
    }

    #[test]
    fn upstream_category_example_answers_relational_image_question() {
        let mut reasoner = Reasoner::new().unwrap();
        for sentence in [
            "<{shape1} --> rectangle>. :|:",
            "<{shape1} --> [filled]>. :|:",
            "<({shape1} * {shape2}) --> leftOf>. :|:",
            "<({shape1} * {shape3}) --> aboveOf>. :|:",
            "<{shape2} --> circle>. :|:",
            "<{shape2} --> [unfilled]>. :|:",
            "<({shape2} * {shape3}) --> aboveOf>. :|:",
            "<{shape3} --> triangle>. :|:",
            "<{shape3} --> [unfilled]>. :|:",
            "<({shape3} * {shape2}) --> leftOf>. :|:",
        ] {
            reasoner.add_sentence(sentence).unwrap();
        }
        reasoner.cycles(50);
        let answer = reasoner
            .ask("<([filled] & rectangle) --> (leftOf /1 {?1})>? :|:")
            .unwrap();
        let images = reasoner
            .memory
            .concepts()
            .map(|concept| reasoner.language.format_term(&concept.term))
            .filter(|term| term.contains("leftOf /1"))
            .collect::<Vec<_>>();
        let answer = answer.unwrap_or_else(|| panic!("available images: {images:?}"));
        assert_eq!(
            reasoner.language.format_term(&answer.term),
            "<([filled] & rectangle) --> (leftOf /1 {shape2})>"
        );
    }

    #[test]
    fn event_stream_learns_temporal_implication() {
        let mut reasoner = Reasoner::new().unwrap();
        reasoner
            .add_belief("a", Truth::new(1.0, 0.9), false)
            .unwrap();
        reasoner
            .add_belief("b", Truth::new(1.0, 0.9), false)
            .unwrap();
        let a = reasoner.language.parse_term("a").unwrap();
        let b = reasoner.language.parse_term("b").unwrap();
        let truth = reasoner.memory.temporal_link_truth(&a, &b);
        assert!(truth.confidence > 0.0);
    }

    #[test]
    fn learned_procedure_keeps_single_observation_truth() {
        let mut reasoner = Reasoner::new().unwrap();
        reasoner
            .add_operation("^pick", |_| OperationFeedback::default())
            .unwrap();
        reasoner.add_sentence("<{t1} --> [quacks]>. :|:").unwrap();
        reasoner.cycles(100);
        reasoner.add_sentence("<{t1} --> [quacks]>. :|:").unwrap();
        reasoner.add_sentence("^pick. :|:").unwrap();
        reasoner.add_sentence("G. :|:").unwrap();
        let answer = reasoner
            .ask("<(<{t1} --> [quacks]> &/ ^pick) =/> G>?")
            .unwrap()
            .unwrap();
        assert!(
            (answer.truth.confidence - 0.241_351).abs() < 1.0e-6,
            "{answer:?}"
        );
    }

    #[test]
    fn declarative_network_reaches_low_priority_smoking_chain() {
        let mut reasoner = Reasoner::new().unwrap();
        for sentence in [
            "<<$1 --> [smokes]> ==> <$1 --> [cancerous]>>. %0.6%",
            "<(<#1 --> [smokes]> && <($1 * #1) --> friend>) ==> <$1 --> [smokes]>>.",
            "<(<#1 --> [smokes]> && <(#1 * $1) --> friend>) ==> <$1 --> [smokes]>>.",
            "<({Anna} * {Bob}) --> friend>.",
            "<({Anna} * {Edward}) --> friend>.",
            "<({Anna} * {Frank}) --> friend>.",
            "<({Edward} * {Frank}) --> friend>.",
            "<({Gary} * {Helen}) --> friend>.",
            "<({Gary} * {Frank}) --> friend>. %0%",
            "<{Anna} --> [smokes]>.",
            "<{Edward} --> [smokes]>.",
        ] {
            reasoner.add_sentence(sentence).unwrap();
        }
        let edward = reasoner
            .language
            .parse_term("<{Edward} --> [smokes]>")
            .unwrap();
        reasoner.cycles(100);
        let edward_belief = reasoner.memory.find(&edward).unwrap().belief;
        assert!(
            (edward_belief.truth.confidence - 0.921_198).abs() < 1.0e-6,
            "{edward_belief:?}"
        );
        {
            let (source, confidence) = ("<{Gary} --> [smokes]>", 0.371_024);
            let term = reasoner.language.parse_term(source).unwrap();
            let belief = reasoner
                .memory
                .find(&term)
                .unwrap_or_else(|| panic!("missing {source}"))
                .belief;
            assert!(
                (belief.truth.confidence - confidence).abs() < 1.0e-6,
                "{belief:?}"
            );
        }
        for (person, confidence) in [("Gary", 0.200_353), ("Helen", 0.162_286)] {
            let answer = reasoner
                .ask(&format!("<{{{person}}} --> [cancerous]>?"))
                .unwrap()
                .unwrap();
            assert!((answer.truth.frequency - 0.6).abs() < 1.0e-6);
            assert!(
                (answer.truth.confidence - confidence).abs() < 1.0e-6,
                "{person}: {answer:?}"
            );
        }
    }

    #[test]
    fn three_leg_dog_generalization_keeps_single_analogy() {
        let mut reasoner = Reasoner::new().unwrap();
        for sentence in [
            "<{fido} --> dog>.",
            "<{snoopy} --> dog>.",
            "<dog --> animal>.",
            "<(dog * 4) --> has_leg>.",
            "<({fido} * 3) --> has_leg>.",
            "<(chicken * 2) --> has_leg>.",
            "<chicken --> animal>.",
        ] {
            reasoner.add_sentence(sentence).unwrap();
        }
        reasoner.cycles(100);
        for question in [
            "<({fido} * ?1) --> has_leg>?",
            "<({snoopy} * ?1) --> has_leg>?",
            "<(dog * ?1) --> has_leg>?",
        ] {
            let _ = reasoner.ask(question).unwrap();
        }
        let term = reasoner
            .language
            .parse_term("<(animal * 2) --> has_leg>")
            .unwrap();
        let answer = reasoner
            .ask("<(animal * 2) --> has_leg>?")
            .unwrap()
            .unwrap();
        let belief = reasoner.memory.find(&term).unwrap().belief;
        assert!(
            (answer.truth.confidence - 0.447_514).abs() < 1.0e-6,
            "{answer:?}"
        );
        assert!(
            (belief.truth.confidence - 0.447_514).abs() < 1.0e-6,
            "{belief:?}"
        );
        assert!(belief.stamp.evidence_eq(&Stamp::from_evidence([7, 6])));
    }

    #[test]
    fn learned_variable_procedure_transfers_to_new_operation_argument() {
        let executions = Arc::new(Mutex::new(Vec::new()));
        let go_executions = Arc::clone(&executions);
        let activate_executions = Arc::clone(&executions);
        let mut reasoner = Reasoner::new().unwrap();
        reasoner.set_motor_babbling_chance(0.0);
        reasoner
            .add_operation("^go", move |arguments| {
                go_executions.lock().unwrap().push(("go", arguments));
                OperationFeedback::default()
            })
            .unwrap();
        reasoner
            .add_operation("^activate", move |arguments| {
                activate_executions
                    .lock()
                    .unwrap()
                    .push(("activate", arguments));
                OperationFeedback::default()
            })
            .unwrap();

        for switch in ["switch1", "switch2"] {
            for event in [
                "<{start} --> [at]>".to_owned(),
                format!("<({{SELF}} * {{{switch}}}) --> ^go>"),
                format!("<{{{switch}}} --> [at]>"),
                format!("<({{SELF}} * {{{switch}}}) --> ^activate>"),
                format!("<{{{switch}}} --> [on]>"),
            ] {
                reasoner
                    .add_belief(&event, Truth::new(1.0, 0.9), false)
                    .unwrap();
            }
            reasoner.cycles(20);
        }

        reasoner
            .add_belief("<{start} --> [at]>", Truth::new(1.0, 0.9), false)
            .unwrap();
        reasoner
            .add_goal("<{switch3} --> [on]>", Truth::new(1.0, 0.9))
            .unwrap();
        reasoner
            .add_belief("<{switch3} --> [at]>", Truth::new(1.0, 0.9), false)
            .unwrap();
        reasoner
            .add_goal("<{switch3} --> [on]>", Truth::new(1.0, 0.9))
            .unwrap();

        let expected = reasoner
            .language
            .parse_term("({SELF} * {switch3})")
            .unwrap();
        assert_eq!(
            *executions.lock().unwrap(),
            [("go", expected), ("activate", expected)]
        );
    }

    #[test]
    fn learned_procedure_executes_registered_operation_for_goal() {
        let executed = Arc::new(AtomicBool::new(false));
        let callback_flag = Arc::clone(&executed);
        let mut reasoner = Reasoner::new().unwrap();
        reasoner
            .add_operation("^act", move |_| {
                callback_flag.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();

        for _ in 0..3 {
            reasoner
                .add_belief("state", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner
                .add_belief("^act", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner
                .add_belief("goal", Truth::new(1.0, 0.9), false)
                .unwrap();
        }

        reasoner
            .add_belief("state", Truth::new(1.0, 0.9), false)
            .unwrap();
        reasoner.add_goal("goal", Truth::new(1.0, 0.9)).unwrap();
        assert!(executed.load(Ordering::SeqCst));
    }

    #[test]
    fn upstream_procedure_test_executes_after_single_observation() {
        let executed = Arc::new(AtomicBool::new(false));
        let callback_flag = Arc::clone(&executed);
        let mut reasoner = Reasoner::new().unwrap();
        reasoner
            .add_operation("^op", move |_| {
                callback_flag.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();

        reasoner
            .add_belief("a", Truth::new(1.0, 0.9), false)
            .unwrap();
        reasoner.cycles(1);
        reasoner
            .add_belief("^op", Truth::new(1.0, 0.9), false)
            .unwrap();
        reasoner.cycles(1);
        reasoner
            .add_belief("result", Truth::new(1.0, 0.9), false)
            .unwrap();
        reasoner.cycles(1);
        reasoner
            .add_belief("a", Truth::new(1.0, 0.9), false)
            .unwrap();
        reasoner.cycles(1);
        reasoner.add_goal("result", Truth::new(1.0, 0.9)).unwrap();
        reasoner.cycles(1);

        assert!(executed.load(Ordering::SeqCst));
    }

    #[test]
    fn deep_goal_chain_reaches_property_matched_operation() {
        let executed_arguments = Arc::new(Mutex::new(None));
        let callback_arguments = Arc::clone(&executed_arguments);
        let mut reasoner = Reasoner::new().unwrap();
        reasoner.set_motor_babbling_chance(0.0);
        reasoner
            .add_operation("^lighter", move |arguments| {
                *callback_arguments.lock().unwrap() = Some(arguments);
                OperationFeedback::default()
            })
            .unwrap();
        for sentence in [
            "<(<({$1} * plastic) --> made_of> &/ <({SELF} * {$1}) --> ^lighter>) =/> <{$1} --> [heated]>>.",
            "<<{$1} --> [heated]> =/> <{$1} --> [melted]>>.",
            "<<{$1} --> [melted]> =/> <{$1} --> [pliable]>>.",
            "<(<{$1} --> [pliable]> &/ <({SELF} * {$1}) --> ^reshape>) =/> <{$1} --> [screwlike]>>.",
            "<(<{$1} --> [screwlike]> &/ <({SELF} * {$1}) --> ^wait>) =/> <{$1} --> [hardened]>>.",
            "<(<{#1} --> [hardened]> &/ <({SELF} * {#1}) --> ^unscrew>) =/> <{screw1} --> [removed]>>.",
            "<plastic --> [bendable]>.",
            "<({toothbrush1} * [bendable]) --> made_of>. :|:",
        ] {
            reasoner.add_sentence(sentence).unwrap();
        }
        reasoner.cycles(10);
        reasoner
            .add_sentence("<{screw1} --> [removed]>! :|:")
            .unwrap();
        reasoner.cycles(2);

        let expected = reasoner
            .language
            .parse_term("({SELF} * {toothbrush1})")
            .unwrap();
        assert_eq!(*executed_arguments.lock().unwrap(), Some(expected));
    }

    #[test]
    fn numeric_similarity_prefers_closer_operation_context() {
        let mut reasoner = Reasoner::new().unwrap();
        reasoner.define_numeric_space(10, "B").unwrap();
        reasoner
            .add_operation("^left", |_| OperationFeedback::default())
            .unwrap();
        reasoner
            .add_operation("^right", |_| OperationFeedback::default())
            .unwrap();
        reasoner
            .add_sentence("<(<(left * B_0.1) --> brightness> &/ ^left) =/> G>.")
            .unwrap();
        reasoner
            .add_sentence("<(<(left * B_0.9) --> brightness> &/ ^right) =/> G>.")
            .unwrap();
        reasoner
            .add_sentence("<(left * B_0.8) --> brightness>. :|:")
            .unwrap();

        let goal = reasoner.language.parse_term("G").unwrap();
        let mut candidate = None;
        reasoner.collect_goal_candidates(
            goal,
            Truth::new(1.0, 0.9),
            &mut Vec::new(),
            0,
            1.0,
            &mut candidate,
        );
        let candidate = candidate.expect("numeric context produced no decision candidate");
        assert_eq!(
            reasoner.language.atom_name(candidate.operation_atom),
            Some("^right"),
            "{candidate:?}"
        );
        assert!(
            candidate.score > super::MOTOR_BABBLING_SUPPRESSION_THRESHOLD,
            "{candidate:?}"
        );
    }

    #[test]
    fn declared_similarity_transfers_operation_context() {
        let mut reasoner = Reasoner::new().unwrap();
        reasoner
            .add_operation("^pick", |_| OperationFeedback::default())
            .unwrap();
        reasoner
            .add_sentence("<(<{t1} --> [quacks]> &/ ^pick) =/> G>.")
            .unwrap();
        reasoner.add_sentence("<{t2} <-> {t1}>.").unwrap();
        reasoner.add_sentence("<{t2} --> [quacks]>. :|:").unwrap();
        reasoner.cycles(5);

        let condition = reasoner.language.parse_term("<{t1} --> [quacks]>").unwrap();
        let context = reasoner
            .condition_decision_context(&condition)
            .expect("similar observation did not satisfy the condition");
        assert!(context.1);
        assert!(context.2.expectation() > 0.5, "{context:?}");
    }

    #[test]
    fn generic_operation_candidate_creates_specific_negative_hypothesis() {
        let mut reasoner = Reasoner::new().unwrap();
        reasoner.set_motor_babbling_chance(0.0);
        reasoner
            .add_operation("^activate", |_| OperationFeedback::default())
            .unwrap();
        reasoner
            .add_sentence(
                "<(<$1 --> room> &/ <({SELF} * light) --> ^activate>) =/> <$1 --> [lit]>>.",
            )
            .unwrap();
        reasoner
            .add_sentence(
                "<(<{room1} --> room> &/ <({SELF} * torch) --> ^activate>) =/> <{room1} --> [lit]>>. {1.0 0.6}",
            )
            .unwrap();
        reasoner.add_sentence("<{room1} --> room>. :|:").unwrap();
        reasoner.add_sentence("<{room1} --> [lit]>! :|:").unwrap();

        let answer = reasoner
            .ask(
                "<(<{room1} --> room> &/ <({SELF} * light) --> ^activate>) =/> <{room1} --> [lit]>>?",
            )
            .unwrap()
            .expect("missing specific negative implication");
        assert!(answer.truth.frequency.abs() < f64::EPSILON);
        assert!(answer.truth.confidence > 0.0, "{answer:?}");
        let torch = reasoner
            .ask(
                "<(<{room1} --> room> &/ <({SELF} * torch) --> ^activate>) =/> <{room1} --> [lit]>>?",
            )
            .unwrap()
            .expect("specific alternative disappeared");
        assert_eq!(torch.truth, Truth::new(1.0, 0.6));
    }

    #[test]
    fn delayed_higher_reward_beats_immediate_lower_reward() {
        let mut reasoner = Reasoner::new().unwrap();
        reasoner.set_motor_babbling_chance(0.0);
        reasoner
            .add_operation("^eat", |_| OperationFeedback::default())
            .unwrap();
        reasoner
            .add_operation("^leave", |_| OperationFeedback::default())
            .unwrap();
        for sentence in [
            "marshmallow1. :|:",
            "^eat. :|:",
            "replete. :|: {0.6 0.9}",
            "marshmallow1. :|:",
            "^leave. :|:",
            "marshmallow2. :|:",
            "^eat. :|:",
            "replete. :|: {0.9 0.9}",
            "marshmallow1. :|:",
        ] {
            reasoner.add_sentence(sentence).unwrap();
        }
        let goal = reasoner.language.parse_term("replete").unwrap();
        let mut candidate = None;
        reasoner.collect_goal_candidates(
            goal,
            Truth::new(1.0, 0.9),
            &mut Vec::new(),
            0,
            1.0,
            &mut candidate,
        );
        let candidate = candidate.expect("training produced no operation candidate");
        let mut components = Vec::new();
        super::flatten_sequence(&reasoner.language, candidate.precondition, &mut components);
        let operations = components
            .iter()
            .filter_map(|term| reasoner.language.operation_atom(term))
            .filter_map(|atom| reasoner.language.atom_name(atom))
            .collect::<Vec<_>>();
        assert_eq!(operations, ["^leave"], "{candidate:?}");
    }

    #[test]
    fn compound_candidate_executes_operations_in_temporal_order() {
        let execution_order = Arc::new(Mutex::new(Vec::new()));
        let first_order = Arc::clone(&execution_order);
        let second_order = Arc::clone(&execution_order);
        let mut reasoner = Reasoner::new().unwrap();
        reasoner
            .add_operation("^first", move |_| {
                first_order.lock().unwrap().push("first");
                OperationFeedback::default()
            })
            .unwrap();
        reasoner
            .add_operation("^second", move |_| {
                second_order.lock().unwrap().push("second");
                OperationFeedback::default()
            })
            .unwrap();
        let precondition = reasoner
            .language
            .parse_term("((state &/ ^first) &/ ^second)")
            .unwrap();
        let second = reasoner.language.parse_term("^second").unwrap().root();
        assert!(reasoner.execute_candidate(&super::DecisionCandidate {
            operation_atom: second,
            precondition,
            grounded: true,
            goal_depth: 0,
            fresh_context: true,
            context_time: reasoner.current_time,
            specificity: 1,
            score: 1.0,
            context_truth: Truth::new(1.0, 0.9),
            missing_specific_implication: None,
        }));
        assert_eq!(*execution_order.lock().unwrap(), ["first", "second"]);
    }

    #[test]
    fn upstream_multistep_test_selects_operation_for_current_subgoal() {
        let goto_executed = Arc::new(AtomicBool::new(false));
        let activate_executed = Arc::new(AtomicBool::new(false));
        let goto_callback = Arc::clone(&goto_executed);
        let activate_callback = Arc::clone(&activate_executed);
        let mut reasoner = Reasoner::new().unwrap();
        reasoner.set_motor_babbling_chance(0.0);
        reasoner
            .add_operation("^goto_switch", move |_| {
                goto_callback.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();
        reasoner
            .add_operation("^activate_switch", move |_| {
                activate_callback.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();

        for _ in 0..5 {
            reasoner
                .add_belief("start_at", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner
                .add_belief("^goto_switch", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner.cycles(1);
            reasoner
                .add_belief("switch_at", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner
                .add_belief("^activate_switch", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner
                .add_belief("switch_active", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner.cycles(1);
            reasoner
                .add_belief("light_active", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner.cycles(10);
        }

        reasoner.cycles(10);

        reasoner
            .add_belief("start_at", Truth::new(1.0, 0.9), false)
            .unwrap();
        reasoner
            .add_goal("light_active", Truth::new(1.0, 0.9))
            .unwrap();
        assert!(
            goto_executed.load(Ordering::SeqCst),
            "goto={}, activate={}",
            goto_executed.load(Ordering::SeqCst),
            activate_executed.load(Ordering::SeqCst)
        );
        assert!(!activate_executed.load(Ordering::SeqCst));

        goto_executed.store(false, Ordering::SeqCst);
        reasoner
            .add_belief("switch_at", Truth::new(1.0, 0.9), false)
            .unwrap();
        reasoner
            .add_goal("light_active", Truth::new(1.0, 0.9))
            .unwrap();
        assert!(!goto_executed.load(Ordering::SeqCst));
        assert!(activate_executed.load(Ordering::SeqCst));
    }

    #[test]
    fn upstream_sequence_test_learns_from_failed_anticipations() {
        let executed = Arc::new(AtomicUsize::new(0));
        let mut reasoner = Reasoner::new().unwrap();
        reasoner.set_motor_babbling_chance(0.0);
        for (name, id) in [("^1", 1), ("^2", 2), ("^3", 3)] {
            let callback = Arc::clone(&executed);
            reasoner
                .add_operation(name, move |_| {
                    callback.store(id, Ordering::SeqCst);
                    OperationFeedback::default()
                })
                .unwrap();
        }

        for _ in 0..5 {
            reasoner
                .add_belief("a", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner
                .add_belief("b", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner
                .add_belief("^1", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner
                .add_belief("g", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner.cycles(100);
        }
        for _ in 0..100 {
            reasoner
                .add_belief("a", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner
                .add_belief("^1", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner.cycles(100);
        }
        for _ in 0..100 {
            reasoner
                .add_belief("b", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner
                .add_belief("^1", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner.cycles(100);
        }
        for _ in 0..2 {
            reasoner
                .add_belief("b", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner
                .add_belief("^2", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner
                .add_belief("g", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner.cycles(100);
        }
        for _ in 0..2 {
            reasoner
                .add_belief("a", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner
                .add_belief("^3", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner
                .add_belief("g", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner.cycles(100);
        }

        reasoner
            .add_belief("a", Truth::new(1.0, 0.9), false)
            .unwrap();
        reasoner
            .add_belief("b", Truth::new(1.0, 0.9), false)
            .unwrap();
        executed.store(0, Ordering::SeqCst);
        reasoner.add_goal("g", Truth::new(1.0, 0.9)).unwrap();
        assert_eq!(executed.load(Ordering::SeqCst), 1);

        executed.store(0, Ordering::SeqCst);
        reasoner.cycles(100);
        reasoner
            .add_belief("b", Truth::new(1.0, 0.9), false)
            .unwrap();
        reasoner.add_goal("g", Truth::new(1.0, 0.9)).unwrap();
        assert_eq!(executed.load(Ordering::SeqCst), 2);

        executed.store(0, Ordering::SeqCst);
        reasoner.cycles(100);
        reasoner
            .add_belief("a", Truth::new(1.0, 0.9), false)
            .unwrap();
        reasoner.add_goal("g", Truth::new(1.0, 0.9)).unwrap();
        assert_eq!(executed.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn upstream_multistep2_test_chains_separately_learned_procedures() {
        let goto_executed = Arc::new(AtomicBool::new(false));
        let activate_executed = Arc::new(AtomicBool::new(false));
        let goto_callback = Arc::clone(&goto_executed);
        let activate_callback = Arc::clone(&activate_executed);
        let mut reasoner = Reasoner::new().unwrap();
        reasoner.set_motor_babbling_chance(0.0);
        reasoner
            .add_operation("^goto_switch", move |_| {
                goto_callback.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();
        reasoner
            .add_operation("^activate_switch", move |_| {
                activate_callback.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();

        for _ in 0..5 {
            reasoner
                .add_belief("start_at", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner
                .add_belief("^goto_switch", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner.cycles(1);
            reasoner
                .add_belief("switch_at", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner.cycles(10);
        }
        reasoner.cycles(1000);

        for _ in 0..5 {
            reasoner
                .add_belief("switch_at", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner
                .add_belief("^activate_switch", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner
                .add_belief("switch_active", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner.cycles(1);
            reasoner
                .add_belief("light_active", Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner.cycles(10);
        }

        reasoner.cycles(10);
        reasoner
            .add_belief("start_at", Truth::new(1.0, 0.9), false)
            .unwrap();
        reasoner
            .add_goal("light_active", Truth::new(1.0, 0.9))
            .unwrap();
        reasoner.cycles(10);
        assert!(goto_executed.load(Ordering::SeqCst));
        assert!(!activate_executed.load(Ordering::SeqCst));

        goto_executed.store(false, Ordering::SeqCst);
        reasoner
            .add_belief("switch_at", Truth::new(1.0, 0.9), false)
            .unwrap();
        reasoner
            .add_goal("light_active", Truth::new(1.0, 0.9))
            .unwrap();
        assert!(!goto_executed.load(Ordering::SeqCst));
        assert!(activate_executed.load(Ordering::SeqCst));
    }

    #[test]
    fn upstream_alphabet_test_runs_periodic_event_stream() {
        let mut reasoner = Reasoner::new().unwrap();
        reasoner
            .add_belief("a", Truth::new(1.0, 0.9), false)
            .unwrap();
        for index in 0_u8..50 {
            if index % 3 == 0 {
                let letter = char::from(b'a' + index % 10).to_string();
                reasoner
                    .add_belief(&letter, Truth::new(1.0, 0.9), false)
                    .unwrap();
            }
            reasoner.cycles(1);
        }
        assert_eq!(reasoner.current_time(), 69);
    }

    #[test]
    fn motor_babbling_executes_and_injects_operation_event() {
        let executed = Arc::new(AtomicBool::new(false));
        let callback = Arc::clone(&executed);
        let mut reasoner = Reasoner::new().unwrap();
        reasoner.set_motor_babbling_chance(1.0);
        reasoner.set_random_seed(1);
        reasoner
            .add_operation("^explore", move |_| {
                callback.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();

        reasoner
            .add_goal("unknown_result", Truth::new(1.0, 0.9))
            .unwrap();

        assert!(executed.load(Ordering::SeqCst));
        let operation = reasoner.language.parse_term("^explore").unwrap();
        assert_eq!(
            reasoner.memory.find(&operation).unwrap().belief_spike.term,
            operation
        );
    }

    #[test]
    fn motor_babbling_uses_configured_operation_argument() {
        let received = Arc::new(Mutex::new(Term::default()));
        let callback_value = Arc::clone(&received);
        let mut reasoner = Reasoner::new().unwrap();
        reasoner.set_motor_babbling_chance(1.0);
        reasoner
            .add_operation("^move", move |arguments| {
                *callback_value.lock().unwrap() = arguments;
                OperationFeedback::default()
            })
            .unwrap();
        reasoner
            .set_operation_arguments("^move", &["distance"])
            .unwrap();

        reasoner
            .add_goal("unknown_result", Truth::new(1.0, 0.9))
            .unwrap();

        assert_eq!(
            reasoner.language.format_term(&received.lock().unwrap()),
            "({SELF} * distance)"
        );
        let concrete = reasoner
            .language
            .parse_term("<({SELF} * distance) --> ^move>")
            .unwrap();
        assert!(reasoner.memory.find(&concrete).is_some());
    }

    #[test]
    fn learned_parameterized_operation_passes_product_to_callback() {
        let received = Arc::new(Mutex::new(Term::default()));
        let callback_value = Arc::clone(&received);
        let mut reasoner = Reasoner::new().unwrap();
        reasoner.set_motor_babbling_chance(0.0);
        reasoner
            .add_operation("^move", move |arguments| {
                *callback_value.lock().unwrap() = arguments;
                OperationFeedback::default()
            })
            .unwrap();

        reasoner
            .add_belief("state", Truth::new(1.0, 0.9), false)
            .unwrap();
        reasoner
            .add_belief(
                "<({SELF} * distance) --> ^move>",
                Truth::new(1.0, 0.9),
                false,
            )
            .unwrap();
        reasoner
            .add_belief("result", Truth::new(1.0, 0.9), false)
            .unwrap();
        reasoner
            .add_belief("state", Truth::new(1.0, 0.9), false)
            .unwrap();
        reasoner.add_goal("result", Truth::new(1.0, 0.9)).unwrap();

        assert_eq!(
            reasoner.language.format_term(&received.lock().unwrap()),
            "({SELF} * distance)"
        );
    }

    #[test]
    fn executed_operation_learns_the_current_context() {
        let mut reasoner = Reasoner::new().unwrap();
        reasoner.set_motor_babbling_chance(1.0);
        reasoner
            .add_operation("^right", |_| OperationFeedback::default())
            .unwrap();
        reasoner
            .add_belief("ball_right", Truth::new(1.0, 0.9), false)
            .unwrap();
        reasoner.add_goal("good_nar", Truth::new(1.0, 0.9)).unwrap();
        reasoner
            .add_belief("good_nar", Truth::new(1.0, 0.9), false)
            .unwrap();

        let goal = reasoner.language.parse_term("good_nar").unwrap();
        let mut best = None;
        reasoner.collect_goal_candidates(
            goal,
            Truth::new(1.0, 0.9),
            &mut Vec::new(),
            0,
            1.0,
            &mut best,
        );
        assert_eq!(
            reasoner.language.format_term(&best.unwrap().precondition),
            "(ball_right &/ ^right)"
        );
    }

    #[test]
    fn upstream_follow_test_learns_closed_loop_policy() {
        let left_executed = Arc::new(AtomicBool::new(false));
        let right_executed = Arc::new(AtomicBool::new(false));
        let left_callback = Arc::clone(&left_executed);
        let right_callback = Arc::clone(&right_executed);
        let mut reasoner = Reasoner::new().unwrap();
        reasoner
            .add_operation("^left", move |_| {
                left_callback.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();
        reasoner
            .add_operation("^right", move |_| {
                right_callback.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();

        let mut ball_right = true;
        let mut score = 0_i32;
        let mut goods = 0_u32;
        let mut bads = 0_u32;
        for _ in 0..1_000_000 {
            reasoner
                .add_belief(
                    if ball_right {
                        "ball_right"
                    } else {
                        "ball_left"
                    },
                    Truth::new(1.0, 0.9),
                    false,
                )
                .unwrap();
            reasoner.add_goal("good_nar", Truth::new(1.0, 0.9)).unwrap();

            let moved_right = right_executed.swap(false, Ordering::SeqCst);
            let moved_left = left_executed.swap(false, Ordering::SeqCst);
            if moved_right || moved_left {
                if moved_right == ball_right {
                    reasoner
                        .add_belief("good_nar", Truth::new(1.0, 0.9), false)
                        .unwrap();
                    score += 1;
                    goods += 1;
                } else {
                    score -= 1;
                    bads += 1;
                }
            }
            ball_right = reasoner.random_u15() % 2 == 1;
            assert!(score > -100, "score={score}, goods={goods}, bads={bads}");
            assert!(bads < 500, "score={score}, goods={goods}, bads={bads}");
            if score >= 500 {
                assert!(goods > bads);
                return;
            }
            reasoner.cycles(10);
        }
        panic!("follow policy did not converge: score={score}, goods={goods}, bads={bads}");
    }

    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn upstream_cartpole_test_runs_continuous_control_loop() {
        #[derive(Debug)]
        struct Cartpole {
            velocity: f64,
            angle: f64,
            angle_velocity: f64,
            position: f64,
        }

        let state = Arc::new(Mutex::new(Cartpole {
            velocity: 0.0,
            angle: -std::f64::consts::FRAC_PI_2,
            angle_velocity: 0.0,
            position: 0.0,
        }));
        let left_state = Arc::clone(&state);
        let right_state = Arc::clone(&state);
        let left_count = Arc::new(AtomicUsize::new(0));
        let right_count = Arc::new(AtomicUsize::new(0));
        let left_counter = Arc::clone(&left_count);
        let right_counter = Arc::clone(&right_count);
        let mut reasoner = Reasoner::new().unwrap();
        reasoner
            .add_operation("^left", move |_| {
                let mut cart = left_state.lock().unwrap();
                let reverse = if cart.angle > 0.0 { 1.0 } else { -1.0 };
                cart.angle_velocity -= reverse * 0.2;
                cart.velocity -= 0.1;
                left_counter.fetch_add(1, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();
        reasoner
            .add_operation("^right", move |_| {
                let mut cart = right_state.lock().unwrap();
                let reverse = if cart.angle > 0.0 { 1.0 } else { -1.0 };
                cart.angle_velocity += reverse * 0.2;
                cart.velocity += 0.1;
                right_counter.fetch_add(1, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();

        let mut successes = 0_u32;
        let mut failures = 0_u32;
        for _ in 0..=10_000 {
            let angle = {
                let mut cart = state.lock().unwrap();
                cart.position = (cart.position + cart.velocity).clamp(0.0, 1.0);
                cart.angle += cart.angle_velocity;
                cart.angle_velocity += 0.2 * cart.angle.cos();
                cart.angle_velocity = cart.angle_velocity.clamp(-0.3, 0.3);
                if cart.angle > std::f64::consts::PI {
                    cart.angle = -std::f64::consts::PI;
                }
                if cart.angle < -std::f64::consts::PI {
                    cart.angle = std::f64::consts::PI;
                }
                cart.velocity = 0.0;
                cart.angle
            };

            if (angle + std::f64::consts::FRAC_PI_2).abs() <= 0.5 {
                reasoner
                    .add_belief("good", Truth::new(1.0, 0.9), false)
                    .unwrap();
                successes += 1;
            } else if (0.0..=std::f64::consts::PI).contains(&angle) {
                reasoner
                    .add_belief("good", Truth::new(0.0, 0.9), false)
                    .unwrap();
                failures += 1;
            }

            let encoded = (((angle + std::f64::consts::PI) / (2.0 * std::f64::consts::PI) * 8.0)
                as i32)
                .clamp(0, 8)
                .to_string();
            reasoner
                .add_belief(&encoded, Truth::new(1.0, 0.9), false)
                .unwrap();
            reasoner.add_goal("good", Truth::new(1.0, 0.9)).unwrap();
        }

        let operations = left_count.load(Ordering::SeqCst) + right_count.load(Ordering::SeqCst);
        assert!(operations > 0);
        assert!(successes > 0);
        assert!(failures > 0);
        assert!(successes + failures <= 10_001);
    }

    #[test]
    fn upstream_alien_test_runs_three_action_tracking_loop() {
        let left_executed = Arc::new(AtomicBool::new(false));
        let right_executed = Arc::new(AtomicBool::new(false));
        let shoot_executed = Arc::new(AtomicBool::new(false));
        let left_callback = Arc::clone(&left_executed);
        let right_callback = Arc::clone(&right_executed);
        let shoot_callback = Arc::clone(&shoot_executed);
        let mut reasoner = Reasoner::new().unwrap();
        reasoner
            .add_operation("^left", move |_| {
                left_callback.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();
        reasoner
            .add_operation("^right", move |_| {
                right_callback.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();
        reasoner
            .add_operation("^shoot", move |_| {
                shoot_callback.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();

        let mut alien_x = 0.5_f64;
        let mut defender_x = 0.5_f64;
        let alien_width = 0.18_f64;
        let mut hits = 0_u32;
        let mut shots = 0_u32;
        let mut movements = 0_u32;
        for _ in 0..=20_000 {
            let target_right = defender_x <= alien_x - alien_width;
            let target_left = defender_x > alien_x + alien_width;
            reasoner
                .add_belief(
                    if target_right {
                        "r0"
                    } else if target_left {
                        "l0"
                    } else {
                        "c0"
                    },
                    Truth::new(1.0, 0.9),
                    false,
                )
                .unwrap();
            reasoner.add_goal("s0", Truth::new(1.0, 0.9)).unwrap();

            if shoot_executed.swap(false, Ordering::SeqCst) {
                shots += 1;
                if !target_right && !target_left {
                    hits += 1;
                    reasoner
                        .add_belief("s0", Truth::new(1.0, 0.9), false)
                        .unwrap();
                    alien_x = f64::from(reasoner.random_u15() % 1_000) / 1_000.0;
                }
            }
            if left_executed.swap(false, Ordering::SeqCst) {
                defender_x = (defender_x - 0.1).max(0.0);
                movements += 1;
            }
            if right_executed.swap(false, Ordering::SeqCst) {
                defender_x = (defender_x + 0.1).min(1.0);
                movements += 1;
            }
        }

        assert!(shots > 0);
        assert!(hits > 0);
        assert!(movements > 0);
        assert!(hits <= shots);
        assert!((0.0..=1.0).contains(&alien_x));
        assert!((0.0..=1.0).contains(&defender_x));
    }

    #[test]
    fn upstream_pong_test_runs_two_action_rally() {
        let left_executed = Arc::new(AtomicBool::new(false));
        let right_executed = Arc::new(AtomicBool::new(false));
        let left_callback = Arc::clone(&left_executed);
        let right_callback = Arc::clone(&right_executed);
        let mut reasoner = Reasoner::new().unwrap();
        reasoner
            .add_operation("^left", move |_| {
                left_callback.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();
        reasoner
            .add_operation("^right", move |_| {
                right_callback.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();

        let size_x = 50_i32;
        let size_y = 20_i32;
        let mut ball_x = size_x / 2;
        let mut ball_y = size_y / 5;
        let mut bat_x = 20_i32;
        let mut bat_velocity = 0_i32;
        let bat_width = 4_i32;
        let mut velocity_x = 1_i32;
        let mut velocity_y = 1_i32;
        let mut hits = 0_u32;
        let mut misses = 0_u32;
        let mut actions = 0_u32;

        for _ in 0..=10_000 {
            if bat_x < ball_x {
                reasoner
                    .add_belief("ball_right", Truth::new(1.0, 0.9), false)
                    .unwrap();
            }
            if ball_x < bat_x {
                reasoner
                    .add_belief("ball_left", Truth::new(1.0, 0.9), false)
                    .unwrap();
            }
            reasoner.add_goal("good_nar", Truth::new(1.0, 0.9)).unwrap();

            if ball_x <= 0 {
                velocity_x = 1;
            }
            if ball_x >= size_x - 1 {
                velocity_x = -1;
            }
            if ball_y <= 0 {
                velocity_y = 1;
            }
            if ball_y >= size_y - 1 {
                velocity_y = -1;
            }
            ball_x += velocity_x;
            ball_y += velocity_y;

            if ball_y == 0 {
                if (ball_x - bat_x).abs() <= bat_width {
                    reasoner
                        .add_belief("good_nar", Truth::new(1.0, 0.9), false)
                        .unwrap();
                    hits += 1;
                } else {
                    misses += 1;
                }
            }
            if ball_y == 0 || ball_x == 0 || ball_x >= size_x - 1 {
                ball_y = size_y / 2
                    + i32::try_from(reasoner.random_u15() % u32::try_from(size_y / 2).unwrap())
                        .unwrap();
                ball_x =
                    i32::try_from(reasoner.random_u15() % u32::try_from(size_x).unwrap()).unwrap();
                velocity_x = if reasoner.random_u15() % 2 == 0 {
                    1
                } else {
                    -1
                };
            }
            if left_executed.swap(false, Ordering::SeqCst) {
                bat_velocity = -2;
                actions += 1;
            }
            if right_executed.swap(false, Ordering::SeqCst) {
                bat_velocity = 2;
                actions += 1;
            }
            bat_x = (bat_x + bat_velocity * bat_width / 2).clamp(0, size_x - 1);
            reasoner.cycles(5);
        }

        assert!(actions > 0);
        assert!(hits > 0);
        assert!(misses > 0);
        assert!(hits + misses > 0);
        assert!((0..size_x).contains(&bat_x));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn upstream_pong2_test_runs_three_action_rally() {
        let left_executed = Arc::new(AtomicBool::new(false));
        let right_executed = Arc::new(AtomicBool::new(false));
        let stop_executed = Arc::new(AtomicBool::new(false));
        let left_callback = Arc::clone(&left_executed);
        let right_callback = Arc::clone(&right_executed);
        let stop_callback = Arc::clone(&stop_executed);
        let mut reasoner = Reasoner::new().unwrap();
        reasoner
            .add_operation("^left", move |_| {
                left_callback.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();
        reasoner
            .add_operation("^right", move |_| {
                right_callback.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();
        reasoner
            .add_operation("^stop", move |_| {
                stop_callback.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();

        let size_x = 50_i32;
        let size_y = 20_i32;
        let mut ball_x = size_x / 2;
        let mut ball_y = size_y / 5;
        let mut bat_x = 20_i32;
        let mut bat_velocity = 0_i32;
        let bat_width = 6_i32;
        let mut velocity_x = 1_i32;
        let mut velocity_y = 1_i32;
        let mut hits = 0_u32;
        let mut misses = 0_u32;
        let mut stops = 0_u32;

        for _ in 0..=10_000 {
            reasoner
                .add_belief(
                    if bat_x <= ball_x - bat_width {
                        "ball_right"
                    } else if ball_x + bat_width < bat_x {
                        "ball_left"
                    } else {
                        "ball_equal"
                    },
                    Truth::new(1.0, 0.9),
                    false,
                )
                .unwrap();
            reasoner.add_goal("good_nar", Truth::new(1.0, 0.9)).unwrap();

            if ball_x <= 0 {
                velocity_x = 1;
            }
            if ball_x >= size_x - 1 {
                velocity_x = -1;
            }
            if ball_y <= 0 {
                velocity_y = 1;
            }
            if ball_y >= size_y - 1 {
                velocity_y = -1;
            }
            // Upstream's `t % 2 == -1` condition is unreachable for positive t,
            // so horizontal movement only changes when a rally is reset.
            let _ = velocity_x;
            ball_y += velocity_y;

            if ball_y == 0 {
                if (ball_x - bat_x).abs() <= bat_width {
                    reasoner
                        .add_belief("good_nar", Truth::new(1.0, 0.9), false)
                        .unwrap();
                    hits += 1;
                } else {
                    misses += 1;
                }
            }
            if ball_y == 0 || ball_x == 0 || ball_x >= size_x - 1 {
                ball_y = size_y / 2
                    + i32::try_from(reasoner.random_u15() % u32::try_from(size_y / 2).unwrap())
                        .unwrap();
                ball_x =
                    i32::try_from(reasoner.random_u15() % u32::try_from(size_x).unwrap()).unwrap();
                velocity_x = if reasoner.random_u15() % 2 == 0 {
                    1
                } else {
                    -1
                };
            }
            if left_executed.swap(false, Ordering::SeqCst) {
                bat_velocity = -3;
            }
            if right_executed.swap(false, Ordering::SeqCst) {
                bat_velocity = 3;
            }
            if stop_executed.swap(false, Ordering::SeqCst) {
                bat_velocity = 0;
                stops += 1;
            }
            bat_x = (bat_x + bat_velocity * bat_width / 2)
                .clamp(-bat_width * 2, size_x - 1 + bat_width);
            reasoner.cycles(5);
        }

        assert!(hits > 0);
        assert!(misses > 0);
        assert!(stops > 0);
        assert!((-bat_width * 2..=size_x - 1 + bat_width).contains(&bat_x));
    }

    #[test]
    #[allow(clippy::cast_possible_truncation, clippy::too_many_lines)]
    fn upstream_bandrobot_test_runs_numeric_delivery_loop() {
        let left_executed = Arc::new(AtomicBool::new(false));
        let right_executed = Arc::new(AtomicBool::new(false));
        let pick_executed = Arc::new(AtomicBool::new(false));
        let drop_executed = Arc::new(AtomicBool::new(false));
        let left_callback = Arc::clone(&left_executed);
        let right_callback = Arc::clone(&right_executed);
        let pick_callback = Arc::clone(&pick_executed);
        let drop_callback = Arc::clone(&drop_executed);
        let mut reasoner = Reasoner::new().unwrap();
        reasoner
            .add_operation("^left", move |_| {
                left_callback.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();
        reasoner
            .add_operation("^right", move |_| {
                right_callback.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();
        reasoner
            .add_operation("^pick", move |_| {
                pick_callback.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();
        reasoner
            .add_operation("^drop", move |_| {
                drop_callback.store(true, Ordering::SeqCst);
                OperationFeedback::default()
            })
            .unwrap();

        let min_position = 0_i32;
        let max_position = 20_i32;
        let mut position = 0_i32;
        let mut target_position = 1_i32;
        let mut goal_position = 3_i32;
        let mut picked = false;
        let mut last_picked = false;
        let mut has_object = false;
        let mut successes = 0_u32;
        let mut operations = 0_u32;

        for _ in 0..=10_000 {
            if left_executed.swap(false, Ordering::SeqCst) {
                position -= 1;
                operations += 1;
            }
            if right_executed.swap(false, Ordering::SeqCst) {
                position += 1;
                operations += 1;
            }
            position = position.clamp(min_position, max_position);
            if picked {
                target_position = position;
            }
            if pick_executed.swap(false, Ordering::SeqCst) {
                operations += 1;
                if position == target_position {
                    picked = true;
                    has_object = true;
                }
            }
            if drop_executed.swap(false, Ordering::SeqCst) {
                operations += 1;
                picked = false;
                has_object = false;
            }

            let property = if has_object { "dropPosX" } else { "pickPosX" };
            let position_truth =
                f64::from(position - min_position) / f64::from(max_position - min_position);
            let used_position = if has_object {
                goal_position
            } else {
                target_position
            };
            let target_truth =
                f64::from(used_position - min_position) / f64::from(max_position - min_position);
            reasoner
                .add_belief(
                    &format!("<{{position}} |-> [{property}]>"),
                    Truth::new(position_truth.clamp(0.0, 1.0), 0.9),
                    false,
                )
                .unwrap();
            reasoner
                .add_belief(
                    &format!("<{{targetposition}} |-> [{property}]>"),
                    Truth::new(target_truth.clamp(0.0, 1.0), 0.9),
                    false,
                )
                .unwrap();

            if picked && !last_picked {
                reasoner
                    .add_belief("picked", Truth::new(1.0, 0.9), false)
                    .unwrap();
            } else if !picked && last_picked && position == goal_position {
                reasoner.cycles(1);
                reasoner
                    .add_belief("delivered", Truth::new(1.0, 0.9), false)
                    .unwrap();
                target_position = (f64::from(reasoner.random_u15())
                    / f64::from(crate::config::MY_RAND_MAX)
                    * f64::from(max_position)) as i32;
                goal_position = (f64::from(reasoner.random_u15())
                    / f64::from(crate::config::MY_RAND_MAX)
                    * f64::from(max_position)) as i32;
                successes += 1;
            }
            last_picked = picked;
            reasoner
                .add_goal("delivered", Truth::new(1.0, 0.9))
                .unwrap();
        }

        assert!(operations > 0);
        assert!((min_position..=max_position).contains(&position));
        assert!((min_position..=max_position).contains(&target_position));
        assert!((min_position..=max_position).contains(&goal_position));
        assert!(successes <= 10_001);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn upstream_robot_test_runs_room_navigation_loop() {
        const WIDTH: usize = 41;
        const HEIGHT: usize = 21;
        const DIRECTIONS: [(i32, i32); 8] = [
            (1, 0),
            (1, 1),
            (0, 1),
            (-1, 1),
            (-1, 0),
            (-1, -1),
            (0, -1),
            (1, -1),
        ];

        #[derive(Clone, Copy, Debug, Default)]
        struct Cell {
            wall: bool,
            food: bool,
        }

        #[derive(Debug)]
        struct RobotWorld {
            cells: [[Cell; HEIGHT]; WIDTH],
            x: i32,
            y: i32,
            direction: usize,
            allow_action: bool,
            moves: u32,
            eaten: u32,
        }

        fn cell_state(world: &RobotWorld, x: i32, y: i32) -> char {
            let Ok(x) = usize::try_from(x) else {
                return 'w';
            };
            let Ok(y) = usize::try_from(y) else {
                return 'w';
            };
            let Some(cell) = world.cells.get(x).and_then(|column| column.get(y)) else {
                return 'w';
            };
            if cell.wall {
                'w'
            } else if cell.food {
                'f'
            } else {
                'o'
            }
        }

        fn observe(
            world: &RobotWorld,
            x: i32,
            y: i32,
            distance: i32,
            blocked: &mut bool,
            value: &mut char,
        ) {
            if *blocked {
                return;
            }
            let state = cell_state(world, x, y);
            if state == 'o' {
                return;
            }
            *blocked = true;
            if state != 'w' || distance <= 2 {
                *value = state;
            }
        }

        fn view(world: &RobotWorld) -> ([char; 3], i32, i32, bool) {
            let mut viewfield = ['o'; 3];
            let mut blocked = [false; 3];
            let (forward_x, forward_y) = DIRECTIONS[world.direction];
            let left_x = -forward_y;
            let left_y = forward_x;
            let right_x = forward_y;
            let right_y = -forward_x;
            for distance in 1..=50_i32 {
                for offset in 1..=distance {
                    observe(
                        world,
                        world.x + forward_x * offset + left_x * distance,
                        world.y + forward_y * offset + left_y * distance,
                        distance,
                        &mut blocked[0],
                        &mut viewfield[0],
                    );
                    observe(
                        world,
                        world.x + forward_x * distance + left_x * offset,
                        world.y + forward_y * distance + left_y * offset,
                        distance,
                        &mut blocked[0],
                        &mut viewfield[0],
                    );
                    observe(
                        world,
                        world.x + forward_x * distance + right_x * offset,
                        world.y + forward_y * distance + right_y * offset,
                        distance,
                        &mut blocked[2],
                        &mut viewfield[2],
                    );
                    observe(
                        world,
                        world.x + forward_x * offset + right_x * distance,
                        world.y + forward_y * offset + right_y * distance,
                        distance,
                        &mut blocked[2],
                        &mut viewfield[2],
                    );
                }
                observe(
                    world,
                    world.x + forward_x * distance,
                    world.y + forward_y * distance,
                    distance,
                    &mut blocked[1],
                    &mut viewfield[1],
                );
                if viewfield.contains(&'f') {
                    break;
                }
            }
            let (dx, dy) = DIRECTIONS[world.direction];
            let next_x = world.x + dx;
            let next_y = world.y + dy;
            let collision = cell_state(world, next_x, next_y) == 'w';
            (viewfield, next_x, next_y, collision)
        }

        fn uniform(reasoner: &mut Reasoner, upper: u32) -> u32 {
            let divisor = crate::config::MY_RAND_MAX / upper;
            let limit = upper * divisor;
            loop {
                let value = reasoner.random_u15();
                if value < limit {
                    return value / divisor;
                }
            }
        }

        fn spawn(reasoner: &mut Reasoner, state: &Arc<Mutex<RobotWorld>>, food: bool) {
            loop {
                let x = usize::try_from(uniform(reasoner, u32::try_from(WIDTH).unwrap())).unwrap();
                let y = usize::try_from(uniform(reasoner, u32::try_from(HEIGHT).unwrap())).unwrap();
                let mut world = state.lock().unwrap();
                let cell = &mut world.cells[x][y];
                if !cell.wall && !cell.food {
                    if food {
                        cell.food = true;
                    } else {
                        cell.wall = true;
                    }
                    return;
                }
            }
        }

        let mut cells = [[Cell::default(); HEIGHT]; WIDTH];
        for (x, column) in cells.iter_mut().enumerate() {
            for (y, cell) in column.iter_mut().enumerate() {
                if x % 20 == 0 || y % 20 == 0 {
                    cell.wall = true;
                }
            }
        }
        for x in (0..WIDTH).step_by(20) {
            for y in (0..HEIGHT).step_by(20) {
                for offset in -2..=2_i32 {
                    let door_x = i32::try_from(x).unwrap() - 10 + offset;
                    let door_y = i32::try_from(y).unwrap() - 10 + offset;
                    if let Ok(door_x) = usize::try_from(door_x)
                        && door_x < WIDTH
                    {
                        cells[door_x][y].wall = false;
                    }
                    if let Ok(door_y) = usize::try_from(door_y)
                        && door_y < HEIGHT
                    {
                        cells[x][door_y].wall = false;
                    }
                }
            }
        }

        let state = Arc::new(Mutex::new(RobotWorld {
            cells,
            x: 5,
            y: 5,
            direction: 0,
            allow_action: false,
            moves: 0,
            eaten: 0,
        }));
        let left_state = Arc::clone(&state);
        let right_state = Arc::clone(&state);
        let forward_state = Arc::clone(&state);
        let actions = Arc::new(AtomicUsize::new(0));
        let left_actions = Arc::clone(&actions);
        let right_actions = Arc::clone(&actions);
        let forward_actions = Arc::clone(&actions);
        let mut reasoner = Reasoner::new().unwrap();
        reasoner.set_motor_babbling_chance(0.3);
        reasoner
            .add_operation("^left", move |_| {
                let mut world = left_state.lock().unwrap();
                if world.allow_action {
                    world.direction = (world.direction + 1) % DIRECTIONS.len();
                    world.allow_action = false;
                    left_actions.fetch_add(1, Ordering::SeqCst);
                }
                OperationFeedback::default()
            })
            .unwrap();
        reasoner
            .add_operation("^right", move |_| {
                let mut world = right_state.lock().unwrap();
                if world.allow_action {
                    world.direction = (world.direction + DIRECTIONS.len() - 1) % DIRECTIONS.len();
                    world.allow_action = false;
                    right_actions.fetch_add(1, Ordering::SeqCst);
                }
                OperationFeedback::default()
            })
            .unwrap();
        reasoner
            .add_operation("^forward", move |_| {
                let mut world = forward_state.lock().unwrap();
                if world.allow_action {
                    let (_, next_x, next_y, collision) = view(&world);
                    if !collision {
                        world.x = next_x;
                        world.y = next_y;
                        world.moves += 1;
                    }
                    world.allow_action = false;
                    forward_actions.fetch_add(1, Ordering::SeqCst);
                }
                OperationFeedback::default()
            })
            .unwrap();

        for _ in 0..23 {
            spawn(&mut reasoner, &state, true);
        }
        for _ in 0..23 {
            spawn(&mut reasoner, &state, false);
        }

        for _ in 0..1_200 {
            let (perception, reward) = {
                let mut world = state.lock().unwrap();
                let (viewfield, _, _, _) = view(&world);
                let x = usize::try_from(world.x).unwrap();
                let y = usize::try_from(world.y).unwrap();
                let reward = world.cells[x][y].food;
                if reward {
                    world.cells[x][y].food = false;
                    world.eaten += 1;
                }
                world.allow_action = true;
                let perception = if viewfield[1] != 'o' {
                    Some(format!("m{}", viewfield[1]))
                } else if viewfield[0] != 'o' {
                    Some(format!("l{}", viewfield[0]))
                } else if viewfield[2] != 'o' {
                    Some(format!("r{}", viewfield[2]))
                } else {
                    None
                };
                (perception, reward)
            };
            if reward {
                spawn(&mut reasoner, &state, true);
                reasoner
                    .add_belief("eaten", Truth::new(1.0, 0.9), false)
                    .unwrap();
            }
            if let Some(perception) = perception {
                reasoner
                    .add_belief(&perception, Truth::new(1.0, 0.9), false)
                    .unwrap();
            }
            reasoner.add_goal("eaten", Truth::new(1.0, 0.9)).unwrap();
        }

        let world = state.lock().unwrap();
        assert!(actions.load(Ordering::SeqCst) > 0);
        assert!(world.moves > 0);
        let remaining_food = world
            .cells
            .iter()
            .flat_map(|column| column.iter())
            .filter(|cell| cell.food)
            .count();
        assert!(remaining_food > 0);
        assert!(world.eaten <= 1_200);
        assert!((0..i32::try_from(WIDTH).unwrap()).contains(&world.x));
        assert!((0..i32::try_from(HEIGHT).unwrap()).contains(&world.y));
        assert!(
            !world.cells[usize::try_from(world.x).unwrap()][usize::try_from(world.y).unwrap()].wall
        );
    }

    #[test]
    #[allow(clippy::struct_excessive_bools, clippy::too_many_lines)]
    fn upstream_testchamber_script_learns_navigation_and_switches() {
        const GOTO_S0: usize = 1;
        const GOTO_S1: usize = 2;
        const GOTO_S2: usize = 3;
        const GOTO_S3: usize = 4;
        const GOTO_L0: usize = 5;
        const GOTO_L1: usize = 6;
        const ACTIVATE: usize = 7;
        const DEACTIVATE: usize = 8;

        #[derive(Debug, Default)]
        struct Chamber {
            position: usize,
            s1: bool,
            s2: bool,
            s3: bool,
            l0: bool,
            l1: bool,
            door: bool,
        }

        fn add_state(reasoner: &mut Reasoner, name: &str) {
            reasoner
                .add_belief(name, Truth::new(1.0, 0.9), false)
                .unwrap();
        }

        fn step(reasoner: &mut Reasoner, chamber: &mut Chamber, action: &AtomicUsize) {
            let command = action.swap(0, Ordering::SeqCst);
            match command {
                GOTO_S0 => chamber.position = 0,
                GOTO_S1 => chamber.position = 1,
                GOTO_S3 => chamber.position = 3,
                GOTO_L1 => chamber.position = 5,
                GOTO_L0 if chamber.door => chamber.position = 4,
                GOTO_S2 if chamber.door => chamber.position = 2,
                _ => {}
            }
            if command != ACTIVATE && command != DEACTIVATE {
                add_state(
                    reasoner,
                    ["at_s0", "at_s1", "at_s2", "at_s3", "at_l0", "at_l1"][chamber.position],
                );
            }

            let activated = command == ACTIVATE;
            let deactivated = command == DEACTIVATE;
            match chamber.position {
                1 if deactivated || (!chamber.s1 && !activated) => {
                    chamber.s1 = false;
                    chamber.door = false;
                    add_state(reasoner, "s1_is_0");
                    add_state(reasoner, "door_is_closed");
                }
                2 if deactivated || (!chamber.s2 && !activated) => {
                    chamber.s2 = false;
                    chamber.l0 = false;
                    add_state(reasoner, "s2_is_0");
                    add_state(reasoner, "l0_is_0");
                }
                3 if deactivated || (!chamber.s3 && !activated) => {
                    chamber.s3 = false;
                    chamber.l1 = false;
                    add_state(reasoner, "s3_is_0");
                    add_state(reasoner, "l1_is_0");
                }
                1 if activated || chamber.s1 => {
                    chamber.s1 = true;
                    chamber.door = true;
                    add_state(reasoner, "s1_is_1");
                    add_state(reasoner, "door_is_open");
                }
                2 if activated || chamber.s2 => {
                    chamber.s2 = true;
                    chamber.l0 = true;
                    add_state(reasoner, "s2_is_1");
                    add_state(reasoner, "l0_is_1");
                }
                3 if activated || chamber.s3 => {
                    chamber.s3 = true;
                    chamber.l1 = true;
                    add_state(reasoner, "s3_is_1");
                    add_state(reasoner, "l1_is_1");
                }
                _ => {}
            }
        }

        fn demonstrate(
            reasoner: &mut Reasoner,
            chamber: &mut Chamber,
            action: &AtomicUsize,
            command: usize,
            operation: &str,
        ) {
            action.store(command, Ordering::SeqCst);
            reasoner
                .add_belief(operation, Truth::new(1.0, 0.9), false)
                .unwrap();
            step(reasoner, chamber, action);
        }

        fn pursue(
            reasoner: &mut Reasoner,
            chamber: &mut Chamber,
            action: &AtomicUsize,
            goal: &str,
            achieved: fn(&Chamber) -> bool,
        ) -> bool {
            for _ in 0..20 {
                step(reasoner, chamber, action);
                if achieved(chamber) {
                    return true;
                }
                reasoner.add_goal(goal, Truth::new(1.0, 0.9)).unwrap();
            }
            step(reasoner, chamber, action);
            achieved(chamber)
        }

        let action = Arc::new(AtomicUsize::new(0));
        let mut reasoner = Reasoner::new().unwrap();
        reasoner.set_motor_babbling_chance(0.0);
        for (name, id) in [
            ("^goto_s0", GOTO_S0),
            ("^goto_s1", GOTO_S1),
            ("^goto_s2", GOTO_S2),
            ("^goto_s3", GOTO_S3),
            ("^goto_l0", GOTO_L0),
            ("^goto_l1", GOTO_L1),
            ("^activate", ACTIVATE),
            ("^deactivate", DEACTIVATE),
        ] {
            let callback = Arc::clone(&action);
            reasoner
                .add_operation(name, move |_| {
                    callback.store(id, Ordering::SeqCst);
                    OperationFeedback::default()
                })
                .unwrap();
        }

        let mut chamber = Chamber::default();
        for _ in 0..5 {
            demonstrate(&mut reasoner, &mut chamber, &action, GOTO_S1, "^goto_s1");
            demonstrate(&mut reasoner, &mut chamber, &action, ACTIVATE, "^activate");
            demonstrate(
                &mut reasoner,
                &mut chamber,
                &action,
                DEACTIVATE,
                "^deactivate",
            );
            demonstrate(&mut reasoner, &mut chamber, &action, ACTIVATE, "^activate");
            demonstrate(&mut reasoner, &mut chamber, &action, GOTO_S2, "^goto_s2");
            demonstrate(&mut reasoner, &mut chamber, &action, ACTIVATE, "^activate");
            demonstrate(
                &mut reasoner,
                &mut chamber,
                &action,
                DEACTIVATE,
                "^deactivate",
            );
            demonstrate(&mut reasoner, &mut chamber, &action, GOTO_L0, "^goto_l0");
            demonstrate(&mut reasoner, &mut chamber, &action, GOTO_S3, "^goto_s3");
            demonstrate(&mut reasoner, &mut chamber, &action, ACTIVATE, "^activate");
            demonstrate(
                &mut reasoner,
                &mut chamber,
                &action,
                DEACTIVATE,
                "^deactivate",
            );
            demonstrate(&mut reasoner, &mut chamber, &action, GOTO_L1, "^goto_l1");
            demonstrate(&mut reasoner, &mut chamber, &action, GOTO_S0, "^goto_s0");
        }
        demonstrate(&mut reasoner, &mut chamber, &action, GOTO_S1, "^goto_s1");
        demonstrate(
            &mut reasoner,
            &mut chamber,
            &action,
            DEACTIVATE,
            "^deactivate",
        );
        demonstrate(&mut reasoner, &mut chamber, &action, GOTO_S0, "^goto_s0");
        reasoner.cycles(20);

        assert!(pursue(
            &mut reasoner,
            &mut chamber,
            &action,
            "door_is_open",
            |state| state.door
        ));
        assert!(pursue(
            &mut reasoner,
            &mut chamber,
            &action,
            "door_is_closed",
            |state| !state.door
        ));
        assert!(pursue(
            &mut reasoner,
            &mut chamber,
            &action,
            "at_s3",
            |state| state.position == 3
        ));
        assert!(pursue(
            &mut reasoner,
            &mut chamber,
            &action,
            "s3_is_1",
            |state| state.s3
        ));
        assert!(chamber.l1);
        assert!(pursue(
            &mut reasoner,
            &mut chamber,
            &action,
            "at_l1",
            |state| state.position == 5
        ));
        assert!(pursue(
            &mut reasoner,
            &mut chamber,
            &action,
            "door_is_open",
            |state| state.door
        ));
        assert!(pursue(
            &mut reasoner,
            &mut chamber,
            &action,
            "at_l0",
            |state| state.position == 4
        ));
        assert!(pursue(
            &mut reasoner,
            &mut chamber,
            &action,
            "s2_is_1",
            |state| state.s2
        ));
        assert!(chamber.l0);
    }
}
