//! Versioned, serializable snapshots of NARS long-term memory.
//!
//! The in-memory [`Memory`](crate::Memory) is shaped for reasoning, not for
//! storage: it mixes sparse concept slots, hash indexes, and transient event
//! queues. A [`MemorySnapshot`] keeps only durable knowledge — concepts,
//! belief/goal events, learned implications, priority and usage — and is
//! rebuilt into a live memory on load, with derived indexes recomputed.
//!
//! The snapshot is deliberately a separate schema instead of a `derive` on the
//! runtime types, so storing and loading cannot silently inherit an internal
//! representation that later changes.

use core::fmt;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::config::COMPOUND_TERM_SIZE_MAX;
use crate::{
    Concept, ConceptId, Event, EventType, Implication, ImplicationTable, Narsese, Stamp, Term,
    Truth, Usage,
};

/// Schema version written by this crate.
pub const MEMORY_SNAPSHOT_VERSION: u32 = 1;

/// Snapshot conversion errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemorySnapshotError {
    /// The snapshot was written by a newer or unknown schema.
    UnsupportedVersion(u32),
    /// A hex-encoded term is malformed or too long.
    InvalidTerm(String),
    /// A concept record carries an event type this version does not know.
    InvalidEventType(u8),
    /// The same term or concept identifier appears more than once.
    DuplicateConcept(String),
    /// The stored atom table cannot be rebuilt.
    InvalidLanguage(String),
}

impl fmt::Display for MemorySnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported memory snapshot version {version}")
            }
            Self::InvalidTerm(term) => write!(formatter, "invalid hex term `{term}`"),
            Self::InvalidEventType(kind) => write!(formatter, "invalid event type {kind}"),
            Self::DuplicateConcept(term) => write!(formatter, "duplicate concept `{term}`"),
            Self::InvalidLanguage(reason) => write!(formatter, "invalid atom table: {reason}"),
        }
    }
}

impl std::error::Error for MemorySnapshotError {}

/// Durable state of one NARS memory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemorySnapshot {
    /// Schema version.
    pub version: u32,
    /// Atom table the terms refer to.
    ///
    /// Term atoms are numeric ids into [`Narsese`](crate::Narsese), so a
    /// snapshot is meaningless without the table that produced it.
    pub language: LanguageRecord,
    /// Concept capacity the memory was created with.
    pub capacity: usize,
    /// Next concept identifier to hand out.
    pub next_concept_id: u64,
    /// Whether declarative implications were learned.
    #[serde(default)]
    pub has_declarative_implications: bool,
    /// Concepts holding declarative implications.
    #[serde(default)]
    pub declarative_concept_ids: Vec<u64>,
    /// Concepts holding generic input procedural implications.
    #[serde(default)]
    pub generic_input_procedural_concept_ids: Vec<u64>,
    /// Concepts holding variable procedural implications.
    #[serde(default)]
    pub variable_procedural_concept_ids: Vec<u64>,
    /// Concepts in slot order.
    pub concepts: Vec<ConceptRecord>,
}

/// Atom table that gives term atoms their meaning.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LanguageRecord {
    /// Interned atom names, indexed by `atom - 1`.
    pub atoms: Vec<String>,
    /// Optional measured values per atom, same indexing as `atoms`.
    #[serde(default)]
    pub values: Vec<Option<ValueRecord>>,
    /// Atom representing the agent itself.
    pub self_atom: u8,
}

/// A measured value attached to an atom.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ValueRecord {
    /// Numeric value.
    pub value: f64,
    /// Unit or measurement label.
    #[serde(default)]
    pub measurement: String,
}

impl MemorySnapshot {
    /// Whether this snapshot can be loaded by the current build.
    #[must_use]
    pub const fn is_supported(&self) -> bool {
        self.version == MEMORY_SNAPSHOT_VERSION
    }
}

/// One concept: its term, usage, priority, current events, and implications.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConceptRecord {
    /// Concept identifier.
    pub id: u64,
    /// Hex-encoded term atoms.
    pub term: String,
    /// Use count and recency.
    pub usage: UsageRecord,
    /// Concept priority.
    pub priority: f64,
    /// Processing identifiers used by the inference engine.
    #[serde(default)]
    pub process_id: [u64; 3],
    /// Last time this concept was selected.
    #[serde(default)]
    pub last_selection_time: i64,
    /// Current belief.
    pub belief: EventRecord,
    /// Belief priority spike.
    pub belief_spike: EventRecord,
    /// Predicted belief.
    pub predicted_belief: EventRecord,
    /// Goal priority spike.
    pub goal_spike: EventRecord,
    /// Temporal/declarative implications.
    #[serde(default)]
    pub implication_links: Vec<ImplicationRecord>,
    /// Implications conditioned on each operation slot.
    #[serde(default)]
    pub precondition_beliefs: Vec<Vec<ImplicationRecord>>,
}

/// Use count and recency of one concept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRecord {
    /// Number of times the concept was used.
    pub use_count: u64,
    /// Timestamp of the last use.
    pub last_used: i64,
}

/// One belief or goal event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventRecord {
    /// Hex-encoded term atoms.
    pub term: String,
    /// `0` deleted, `1` goal, `2` belief.
    pub event_type: u8,
    /// Truth frequency.
    pub frequency: f64,
    /// Truth confidence.
    pub confidence: f64,
    /// Evidential base.
    #[serde(default)]
    pub stamp: Vec<u64>,
    /// Occurrence time in the NARS timeline.
    pub occurrence_time: i64,
    /// Temporal offset used for projections.
    #[serde(default)]
    pub occurrence_time_offset: f64,
    /// Whether the event has been processed.
    #[serde(default)]
    pub processed: bool,
    /// Creation time.
    pub creation_time: i64,
    /// Whether the event came from outside the system.
    #[serde(default)]
    pub input: bool,
}

/// One learned implication.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImplicationRecord {
    /// Hex-encoded term atoms.
    pub term: String,
    /// Truth frequency.
    pub frequency: f64,
    /// Truth confidence.
    pub confidence: f64,
    /// Evidential base.
    #[serde(default)]
    pub stamp: Vec<u64>,
    /// Temporal offset of the consequent.
    #[serde(default)]
    pub occurrence_time_offset: f64,
    /// Concept that produced this implication, when known.
    #[serde(default)]
    pub source_concept_id: Option<u64>,
    /// Creation time.
    pub creation_time: i64,
}

impl From<&Concept> for ConceptRecord {
    fn from(concept: &Concept) -> Self {
        Self {
            id: concept.id.0,
            term: encode_term(&concept.term),
            usage: UsageRecord {
                use_count: concept.usage.use_count,
                last_used: concept.usage.last_used,
            },
            priority: concept.priority,
            process_id: concept.process_id,
            last_selection_time: concept.last_selection_time,
            belief: EventRecord::from(&concept.belief),
            belief_spike: EventRecord::from(&concept.belief_spike),
            predicted_belief: EventRecord::from(&concept.predicted_belief),
            goal_spike: EventRecord::from(&concept.goal_spike),
            implication_links: concept
                .implication_links
                .iter()
                .map(ImplicationRecord::from)
                .collect(),
            precondition_beliefs: concept
                .precondition_beliefs
                .iter()
                .map(|table| table.iter().map(ImplicationRecord::from).collect())
                .collect(),
        }
    }
}

impl ConceptRecord {
    /// Rebuilds a live concept record.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed terms or unknown event types.
    pub fn to_concept(&self) -> Result<Concept, MemorySnapshotError> {
        let mut precondition_beliefs = self
            .precondition_beliefs
            .iter()
            .map(|records| table_from_records(records))
            .collect::<Result<Vec<_>, _>>()?;
        let mut next = precondition_beliefs.drain(..);
        Ok(Concept {
            id: ConceptId(self.id),
            usage: Usage {
                use_count: self.usage.use_count,
                last_used: self.usage.last_used,
            },
            term: decode_term(&self.term)?,
            belief: self.belief.to_event()?,
            belief_spike: self.belief_spike.to_event()?,
            predicted_belief: self.predicted_belief.to_event()?,
            goal_spike: self.goal_spike.to_event()?,
            precondition_beliefs: core::array::from_fn(|_| {
                next.next().unwrap_or_else(ImplicationTable::default)
            }),
            implication_links: table_from_records(&self.implication_links)?,
            priority: self.priority,
            process_id: self.process_id,
            last_selection_time: self.last_selection_time,
        })
    }
}

impl From<&Event> for EventRecord {
    fn from(event: &Event) -> Self {
        Self {
            term: encode_term(&event.term),
            event_type: event.event_type as u8,
            frequency: event.truth.frequency,
            confidence: event.truth.confidence,
            stamp: event.stamp.iter().collect(),
            occurrence_time: event.occurrence_time,
            occurrence_time_offset: event.occurrence_time_offset,
            processed: event.processed,
            creation_time: event.creation_time,
            input: event.input,
        }
    }
}

impl EventRecord {
    /// Rebuilds a live event.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed terms or unknown event types.
    pub fn to_event(&self) -> Result<Event, MemorySnapshotError> {
        Ok(Event {
            term: decode_term(&self.term)?,
            event_type: event_type(self.event_type)?,
            truth: Truth::new(self.frequency, self.confidence),
            stamp: Stamp::from_evidence(self.stamp.iter().copied()),
            occurrence_time: self.occurrence_time,
            occurrence_time_offset: self.occurrence_time_offset,
            processed: self.processed,
            creation_time: self.creation_time,
            input: self.input,
        })
    }
}

impl From<&Implication> for ImplicationRecord {
    fn from(implication: &Implication) -> Self {
        Self {
            term: encode_term(&implication.term),
            frequency: implication.truth.frequency,
            confidence: implication.truth.confidence,
            stamp: implication.stamp.iter().collect(),
            occurrence_time_offset: implication.occurrence_time_offset,
            source_concept_id: implication.source_concept_id,
            creation_time: implication.creation_time,
        }
    }
}

impl ImplicationRecord {
    /// Rebuilds a live implication.
    ///
    /// # Errors
    ///
    /// Returns an error when the term is malformed.
    pub fn to_implication(&self) -> Result<Implication, MemorySnapshotError> {
        Ok(Implication {
            term: decode_term(&self.term)?,
            truth: Truth::new(self.frequency, self.confidence),
            stamp: Stamp::from_evidence(self.stamp.iter().copied()),
            occurrence_time_offset: self.occurrence_time_offset,
            source_concept_id: self.source_concept_id,
            creation_time: self.creation_time,
        })
    }
}

fn table_from_records(
    records: &[ImplicationRecord],
) -> Result<ImplicationTable, MemorySnapshotError> {
    let entries = records
        .iter()
        .map(ImplicationRecord::to_implication)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ImplicationTable::from_entries(entries))
}

fn event_type(value: u8) -> Result<EventType, MemorySnapshotError> {
    match value {
        0 => Ok(EventType::Deleted),
        1 => Ok(EventType::Goal),
        2 => Ok(EventType::Belief),
        other => Err(MemorySnapshotError::InvalidEventType(other)),
    }
}

/// Whether the stored atom table can be used without remapping.
///
/// A snapshot is compatible when its table extends this build's built-in
/// table and keeps the same `SELF` atom: atom ids are positional, so a shifted
/// prefix would change what every stored term means.
#[must_use]
pub fn is_compatible(snapshot: &MemorySnapshot) -> bool {
    let Ok(stored) = Narsese::from_language_record(&snapshot.language) else {
        return false;
    };
    let builtins = Narsese::new();
    stored
        .to_language_record()
        .atoms
        .starts_with(&builtins.to_language_record().atoms)
        && stored.self_atom() == builtins.self_atom()
}

/// Rewrites a snapshot onto this build's atom table.
///
/// Migration maps every stored atom by *name* to its id in the current build,
/// interning names the build no longer knows, and rewrites every term byte.
/// Concept identities and learned implications are preserved, so an old
/// workspace keeps its memory across builds that change the built-in table.
///
/// # Errors
///
/// Returns an error when a stored term is malformed or the atom table cannot be
/// rebuilt.
pub fn migrate(snapshot: &MemorySnapshot) -> Result<MemorySnapshot, MemorySnapshotError> {
    if snapshot.language.atoms.len() > crate::config::ATOMS_MAX {
        return Err(MemorySnapshotError::InvalidLanguage(
            "stored atom table exceeds ATOMS_MAX".to_owned(),
        ));
    }
    let mut language = Narsese::new();
    let mut mapping: HashMap<u8, u8> = HashMap::new();
    for (index, name) in snapshot.language.atoms.iter().enumerate() {
        let old = u8::try_from(index + 1).map_err(|_| {
            MemorySnapshotError::InvalidLanguage("stored atom index overflow".to_owned())
        })?;
        let new = language
            .intern(name)
            .map_err(|error| MemorySnapshotError::InvalidLanguage(error.to_string()))?;
        if let Some(Some(value)) = snapshot.language.values.get(index) {
            language
                .set_atom_value(new, value.value, value.measurement.clone())
                .map_err(|error| MemorySnapshotError::InvalidLanguage(error.to_string()))?;
        }
        mapping.insert(old, new);
    }

    let mut migrated = snapshot.clone();
    migrated.version = MEMORY_SNAPSHOT_VERSION;
    migrated.language = language.to_language_record();
    for concept in &mut migrated.concepts {
        concept.term = remap_term(&concept.term, &mapping)?;
        remap_event(&mut concept.belief, &mapping)?;
        remap_event(&mut concept.belief_spike, &mapping)?;
        remap_event(&mut concept.predicted_belief, &mapping)?;
        remap_event(&mut concept.goal_spike, &mapping)?;
        for implication in &mut concept.implication_links {
            implication.term = remap_term(&implication.term, &mapping)?;
        }
        for table in &mut concept.precondition_beliefs {
            for implication in table {
                implication.term = remap_term(&implication.term, &mapping)?;
            }
        }
    }
    Ok(migrated)
}

fn remap_event(
    event: &mut EventRecord,
    mapping: &HashMap<u8, u8>,
) -> Result<(), MemorySnapshotError> {
    event.term = remap_term(&event.term, mapping)?;
    Ok(())
}

fn remap_term(encoded: &str, mapping: &HashMap<u8, u8>) -> Result<String, MemorySnapshotError> {
    let term = decode_term(encoded)?;
    let mut atoms = *term.atoms();
    for atom in &mut atoms {
        if *atom == crate::term::ATOM_FREE {
            continue;
        }
        *atom = *mapping
            .get(atom)
            .ok_or_else(|| MemorySnapshotError::InvalidTerm(encoded.to_owned()))?;
    }
    let mut remapped = Term::EMPTY;
    remapped.atoms_mut().copy_from_slice(&atoms);
    Ok(encode_term(&remapped))
}

/// Encodes a term as the hex of its non-empty atom prefix.
#[must_use]
pub fn encode_term(term: &Term) -> String {
    let mut encoded = String::with_capacity(COMPOUND_TERM_SIZE_MAX * 2);
    for atom in term
        .atoms()
        .iter()
        .copied()
        .take_while(|atom| *atom != crate::term::ATOM_FREE)
    {
        use core::fmt::Write as _;
        let _ = write!(encoded, "{atom:02x}");
    }
    encoded
}

/// Decodes a term produced by [`encode_term`].
///
/// # Errors
///
/// Returns an error for odd-length, non-hex, or oversized input.
pub fn decode_term(encoded: &str) -> Result<Term, MemorySnapshotError> {
    if encoded.len() > COMPOUND_TERM_SIZE_MAX * 2 || encoded.len() % 2 != 0 {
        return Err(MemorySnapshotError::InvalidTerm(encoded.to_owned()));
    }
    let mut term = Term::EMPTY;
    for (slot, pair) in encoded.as_bytes().chunks(2).enumerate() {
        let pair = core::str::from_utf8(pair)
            .map_err(|_| MemorySnapshotError::InvalidTerm(encoded.to_owned()))?;
        let atom = u8::from_str_radix(pair, 16)
            .map_err(|_| MemorySnapshotError::InvalidTerm(encoded.to_owned()))?;
        term.atoms_mut()[slot] = atom;
    }
    Ok(term)
}

/// Builds the snapshot of a live memory. Kept here so `Memory` keeps its fields
/// private while the schema stays in one place.
#[must_use]
pub(crate) fn snapshot_of(memory: &crate::Memory, language: &Narsese) -> MemorySnapshot {
    MemorySnapshot {
        version: MEMORY_SNAPSHOT_VERSION,
        language: language.to_language_record(),
        capacity: memory.capacity(),
        next_concept_id: memory.next_concept_id(),
        has_declarative_implications: memory.has_declarative_implications(),
        declarative_concept_ids: memory
            .declarative_concept_ids()
            .iter()
            .map(|id| id.0)
            .collect(),
        generic_input_procedural_concept_ids: memory
            .generic_input_procedural_concept_ids()
            .iter()
            .map(|id| id.0)
            .collect(),
        variable_procedural_concept_ids: memory
            .variable_procedural_concept_ids()
            .iter()
            .map(|id| id.0)
            .collect(),
        concepts: memory.concepts().map(ConceptRecord::from).collect(),
    }
}

/// Rebuilds a live memory from a snapshot.
///
/// # Errors
///
/// Returns an error for unsupported versions, malformed records, or duplicate
/// concepts.
pub(crate) fn restore_memory(
    snapshot: &MemorySnapshot,
) -> Result<crate::Memory, MemorySnapshotError> {
    if !snapshot.is_supported() {
        return Err(MemorySnapshotError::UnsupportedVersion(snapshot.version));
    }
    let language = Narsese::from_language_record(&snapshot.language)
        .map_err(|error| MemorySnapshotError::InvalidLanguage(error.to_string()))?;
    let mut memory = crate::Memory::restore_base(snapshot.capacity, snapshot.next_concept_id);
    memory.set_declarative_implications(snapshot.has_declarative_implications);
    memory.set_concept_id_lists(
        snapshot
            .declarative_concept_ids
            .iter()
            .copied()
            .map(ConceptId)
            .collect(),
        snapshot
            .generic_input_procedural_concept_ids
            .iter()
            .copied()
            .map(ConceptId)
            .collect(),
        snapshot
            .variable_procedural_concept_ids
            .iter()
            .copied()
            .map(ConceptId)
            .collect(),
    );
    for record in &snapshot.concepts {
        let concept = record.to_concept()?;
        if !memory.insert_restored(&language, concept) {
            return Err(MemorySnapshotError::DuplicateConcept(record.term.clone()));
        }
    }
    Ok(memory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventFactory, Implication, Memory};

    fn seeded() -> (Narsese, Memory, Term) {
        let mut language = Narsese::new();
        let mut memory = Memory::default();
        let mut events = EventFactory::new();
        let term = language.atomic_term("cat").expect("atomic term");
        let event = events.input_event(term, EventType::Belief, Truth::new(1.0, 0.9), 0.0, 0);
        assert!(memory.add_input_event(&language, event, 0));
        memory.conceptualize(&language, term, 1);
        let concept = memory.find_mut(&term).expect("concept");
        concept.priority = 0.75;
        concept.usage.use_count = 4;
        let consequent = language.atomic_term("animal").expect("atomic term");
        let _ = concept.implication_links.add(Implication {
            term: consequent,
            truth: Truth::new(0.9, 0.8),
            stamp: Stamp::from_evidence([7, 9]),
            occurrence_time_offset: 1.5,
            source_concept_id: Some(concept.id.0),
            creation_time: 12,
        });
        (language, memory, term)
    }

    #[test]
    fn snapshot_round_trips_through_json() {
        let (language, memory, term) = seeded();
        let snapshot = memory.to_snapshot(&language);
        let encoded = serde_json::to_string(&snapshot).expect("encode");
        let decoded: MemorySnapshot = serde_json::from_str(&encoded).expect("decode");
        let restored = Memory::from_snapshot(&decoded).expect("restore");
        assert_eq!(restored.to_snapshot(&language), snapshot);

        let concept = restored.find(&term).expect("term restored");
        assert!((concept.priority - 0.75).abs() < f64::EPSILON);
        assert_eq!(concept.usage.use_count, 4);
        assert_eq!(concept.implication_links.len(), 1);
        assert_eq!(
            concept.implication_links.get(0).expect("implication").truth,
            Truth::new(0.9, 0.8)
        );
    }

    #[test]
    fn reports_the_highest_evidence_id() {
        let (_, memory, _) = seeded();
        assert!(
            memory.max_evidence_id() >= 1,
            "seeded belief should carry evidence"
        );
    }

    fn shift_term(encoded: &str, offset: u8) -> String {
        let term = decode_term(encoded).expect("term");
        let mut atoms = *term.atoms();
        for atom in &mut atoms {
            if *atom != crate::term::ATOM_FREE {
                *atom = atom.checked_add(offset).expect("no overflow");
            }
        }
        let mut shifted = Term::EMPTY;
        shifted.atoms_mut().copy_from_slice(&atoms);
        encode_term(&shifted)
    }

    #[test]
    fn migrates_a_shifted_atom_table() {
        let (language, memory, _) = seeded();
        let snapshot = memory.to_snapshot(&language);
        let original = snapshot
            .concepts
            .iter()
            .map(|concept| language.format_term(&decode_term(&concept.term).expect("term")))
            .collect::<Vec<_>>();

        // Simulate an older build whose built-in table had one more atom at the
        // front, shifting every stored id by one.
        let mut shifted = snapshot.clone();
        shifted
            .language
            .atoms
            .insert(0, "legacy_builtin".to_owned());
        shifted.language.values.insert(0, None);
        for concept in &mut shifted.concepts {
            concept.term = shift_term(&concept.term, 1);
            concept.belief.term = shift_term(&concept.belief.term, 1);
            concept.belief_spike.term = shift_term(&concept.belief_spike.term, 1);
            concept.predicted_belief.term = shift_term(&concept.predicted_belief.term, 1);
            concept.goal_spike.term = shift_term(&concept.goal_spike.term, 1);
            for implication in &mut concept.implication_links {
                implication.term = shift_term(&implication.term, 1);
            }
            for table in &mut concept.precondition_beliefs {
                for implication in table {
                    implication.term = shift_term(&implication.term, 1);
                }
            }
        }

        assert!(!is_compatible(&shifted));
        let migrated = migrate(&shifted).expect("migrates");
        assert!(is_compatible(&migrated));

        let migrated_language =
            Narsese::from_language_record(&migrated.language).expect("language");
        let migrated_terms = migrated
            .concepts
            .iter()
            .map(|concept| {
                migrated_language.format_term(&decode_term(&concept.term).expect("term"))
            })
            .collect::<Vec<_>>();
        assert_eq!(migrated_terms, original);

        let restored = Memory::from_snapshot(&migrated).expect("restore migrated");
        assert_eq!(restored.concepts().count(), memory.concepts().count());
    }

    #[test]
    fn rejects_unknown_versions() {
        let (language, memory, _) = seeded();
        let mut snapshot = memory.to_snapshot(&language);
        snapshot.version = MEMORY_SNAPSHOT_VERSION + 1;
        assert!(matches!(
            Memory::from_snapshot(&snapshot),
            Err(MemorySnapshotError::UnsupportedVersion(_))
        ));
    }

    #[test]
    fn rejects_a_duplicated_atom_table() {
        let (language, memory, _) = seeded();
        let mut snapshot = memory.to_snapshot(&language);
        let first = snapshot.language.atoms[0].clone();
        snapshot.language.atoms.push(first);
        assert!(matches!(
            Memory::from_snapshot(&snapshot),
            Err(MemorySnapshotError::InvalidLanguage(_))
        ));
    }

    #[test]
    fn rejects_malformed_terms() {
        assert_eq!(
            decode_term("abc"),
            Err(MemorySnapshotError::InvalidTerm("abc".to_owned()))
        );
        assert_eq!(
            decode_term("zz"),
            Err(MemorySnapshotError::InvalidTerm("zz".to_owned()))
        );
        assert_eq!(
            decode_term("0102").map(|term| encode_term(&term)),
            Ok("0102".to_owned())
        );
    }

    #[test]
    fn rejects_duplicate_concepts() {
        let (language, memory, _) = seeded();
        let mut snapshot = memory.to_snapshot(&language);
        let duplicate = snapshot.concepts[0].clone();
        snapshot.concepts.push(duplicate);
        assert!(matches!(
            Memory::from_snapshot(&snapshot),
            Err(MemorySnapshotError::DuplicateConcept(_))
        ));
    }
}
