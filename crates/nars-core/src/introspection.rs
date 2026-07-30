//! Structured, lock-independent snapshots of a running reasoner.

use std::fmt;

use crate::{EventType, Stamp, Truth};

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeStats {
    pub current_time: i64,
    pub total_concepts: usize,
    pub atoms_used: usize,
    pub belief_events: usize,
    pub goal_events: usize,
    pub operations: usize,
    pub pending_anticipations: usize,
    pub max_declarative_implications: usize,
    pub max_temporal_implications: usize,
    pub average_concept_priority: f64,
    pub average_concept_usefulness: f64,
    pub average_belief_event_priority: f64,
    pub average_goal_event_priority: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EventSnapshot {
    pub term: String,
    pub event_type: EventType,
    pub truth: Truth,
    pub stamp: Stamp,
    pub occurrence_time: i64,
    pub creation_time: i64,
    pub priority: f64,
    pub layer: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImplicationSnapshot {
    pub term: String,
    pub truth: Truth,
    pub stamp: Stamp,
    pub occurrence_time_offset: f64,
    pub creation_time: i64,
    pub operation_slot: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConceptSnapshot {
    pub term: String,
    pub priority: f64,
    pub usefulness: f64,
    pub use_count: u64,
    pub last_used: i64,
    pub belief: Option<EventSnapshot>,
    pub belief_spike: Option<EventSnapshot>,
    pub predicted_belief: Option<EventSnapshot>,
    pub goal_spike: Option<EventSnapshot>,
    pub temporal_implications: Vec<ImplicationSnapshot>,
    pub declarative_implications: Vec<ImplicationSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationSnapshot {
    pub slot: usize,
    pub name: String,
    pub arguments: Vec<(usize, String)>,
    pub stdin_output: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OperationConfigSnapshot {
    pub motor_babbling_chance: f64,
    pub babbling_operations: usize,
    pub operations: Vec<OperationSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtomIndexSnapshot {
    pub atom: String,
    pub concepts: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OccurrenceIndexSnapshot {
    pub newest_rank: usize,
    pub concept: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ShellInspection {
    Stats(RuntimeStats),
    Concepts(Vec<ConceptSnapshot>),
    BeliefQueue(Vec<EventSnapshot>),
    GoalQueue(Vec<EventSnapshot>),
    InvertedAtomIndex(Vec<AtomIndexSnapshot>),
    OccurrenceTimeIndex(Vec<OccurrenceIndexSnapshot>),
    OperationConfig(OperationConfigSnapshot),
}

impl fmt::Display for ShellInspection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stats(stats) => write!(formatter, "{stats}"),
            Self::Concepts(concepts) => {
                writeln!(formatter, "//*concepts")?;
                for concept in concepts {
                    writeln!(
                        formatter,
                        "//{}: {{ \"priority\": {:.6}, \"usefulness\": {:.6}, \"useCount\": {}, \"lastUsed\": {}, \"temporalImplications\": {}, \"declarativeImplications\": {} }}",
                        concept.term,
                        concept.priority,
                        concept.usefulness,
                        concept.use_count,
                        concept.last_used,
                        concept.temporal_implications.len(),
                        concept.declarative_implications.len()
                    )?;
                }
                write!(formatter, "//*done")
            }
            Self::BeliefQueue(events) => format_events(formatter, "cycling_belief_events", events),
            Self::GoalQueue(events) => format_events(formatter, "cycling_goal_events", events),
            Self::InvertedAtomIndex(entries) => {
                writeln!(formatter, "//*inverted_atom_index")?;
                for entry in entries {
                    writeln!(formatter, "{}: {}", entry.atom, entry.concepts.join(", "))?;
                }
                write!(formatter, "//*done")
            }
            Self::OccurrenceTimeIndex(entries) => {
                writeln!(formatter, "//*occurrence_time_index")?;
                for entry in entries {
                    writeln!(formatter, "{}: {}", entry.newest_rank, entry.concept)?;
                }
                write!(formatter, "//*done")
            }
            Self::OperationConfig(config) => {
                writeln!(formatter, "//*opconfig")?;
                writeln!(
                    formatter,
                    "*motorbabbling={:.6}",
                    config.motor_babbling_chance
                )?;
                writeln!(formatter, "*babblingops={}", config.babbling_operations)?;
                for operation in &config.operations {
                    writeln!(
                        formatter,
                        "*setopname {} {}",
                        operation.slot, operation.name
                    )?;
                    for (slot, argument) in &operation.arguments {
                        writeln!(
                            formatter,
                            "*setoparg {} {} {}",
                            operation.slot, slot, argument
                        )?;
                    }
                }
                write!(formatter, "//*done")
            }
        }
    }
}

impl fmt::Display for RuntimeStats {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "//*stats")?;
        writeln!(formatter, "Statistics\n----------")?;
        writeln!(formatter, "currentTime:\t\t\t{}", self.current_time)?;
        writeln!(formatter, "total concepts:\t\t\t{}", self.total_concepts)?;
        writeln!(formatter, "Count atomic terms used:\t\t{}", self.atoms_used)?;
        writeln!(
            formatter,
            "current belief events cnt:\t\t{}",
            self.belief_events
        )?;
        writeln!(
            formatter,
            "current goal events cnt:\t\t{}",
            self.goal_events
        )?;
        writeln!(formatter, "registered operations:\t\t{}", self.operations)?;
        writeln!(
            formatter,
            "pending anticipations:\t\t{}",
            self.pending_anticipations
        )?;
        writeln!(
            formatter,
            "DeclarativeImplicationTableMaxItems:\t{}",
            self.max_declarative_implications
        )?;
        writeln!(
            formatter,
            "TemporalImplicationTableMaxItems:\t{}",
            self.max_temporal_implications
        )?;
        writeln!(
            formatter,
            "current average concept priority:\t{:.6}",
            self.average_concept_priority
        )?;
        writeln!(
            formatter,
            "current average concept usefulness:\t{:.6}",
            self.average_concept_usefulness
        )?;
        writeln!(
            formatter,
            "current average belief event priority:\t{:.6}",
            self.average_belief_event_priority
        )?;
        writeln!(
            formatter,
            "current average goal event priority:\t{:.6}",
            self.average_goal_event_priority
        )?;
        write!(formatter, "//*done")
    }
}

fn format_events(
    formatter: &mut fmt::Formatter<'_>,
    name: &str,
    events: &[EventSnapshot],
) -> fmt::Result {
    writeln!(formatter, "//*{name}")?;
    for event in events {
        if let Some(layer) = event.layer {
            write!(formatter, "layer={layer} ")?;
        }
        writeln!(
            formatter,
            "{}: {{ \"priority\": {:.6}, \"time\": {} }} Truth: frequency={:.6}, confidence={:.6}",
            event.term,
            event.priority,
            event.occurrence_time,
            event.truth.frequency,
            event.truth.confidence
        )?;
    }
    write!(formatter, "//*done")
}
