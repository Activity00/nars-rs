//! Belief and goal events with evidential provenance.

use crate::config::OCCURRENCE_ETERNAL;
use crate::{Stamp, Term, Truth};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum EventType {
    #[default]
    Deleted = 0,
    Goal = 1,
    Belief = 2,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Event {
    pub term: Term,
    pub event_type: EventType,
    pub truth: Truth,
    pub stamp: Stamp,
    pub occurrence_time: i64,
    pub occurrence_time_offset: f64,
    pub processed: bool,
    pub creation_time: i64,
    pub input: bool,
}

impl Event {
    #[must_use]
    pub fn evidence_eq(&self, other: &Self) -> bool {
        self.truth == other.truth
            && self.occurrence_time == other.occurrence_time
            && self.term == other.term
            && self.stamp.evidence_eq(&other.stamp)
    }

    #[must_use]
    pub fn eternalized(self) -> Self {
        if self.occurrence_time == OCCURRENCE_ETERNAL {
            self
        } else {
            Self {
                occurrence_time: OCCURRENCE_ETERNAL,
                truth: self.truth.eternalize(),
                ..self
            }
        }
    }

    #[must_use]
    pub fn equal_term_stamp_less_confident_than(&self, other: &Self) -> bool {
        self.truth.confidence <= other.truth.confidence
            && self.occurrence_time == other.occurrence_time
            && self.term == other.term
            && self.stamp.evidence_eq(&other.stamp)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct EventFactory {
    next_evidence_id: u64,
    import_stamp: Option<Stamp>,
}

impl Default for EventFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl EventFactory {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next_evidence_id: 1,
            import_stamp: None,
        }
    }

    pub const fn set_import_stamp(&mut self, stamp: Stamp) {
        self.import_stamp = Some(stamp);
    }

    pub const fn set_next_evidence_id(&mut self, evidence_id: u64) {
        self.next_evidence_id = evidence_id;
    }

    #[must_use]
    pub fn input_event(
        &mut self,
        term: Term,
        event_type: EventType,
        truth: Truth,
        occurrence_time_offset: f64,
        current_time: i64,
    ) -> Event {
        let stamp = self.import_stamp.take().unwrap_or_else(|| {
            let stamp = Stamp::from_evidence([self.next_evidence_id]);
            self.next_evidence_id += 1;
            stamp
        });
        Event {
            term,
            event_type,
            truth,
            stamp,
            occurrence_time: current_time,
            occurrence_time_offset,
            creation_time: current_time,
            input: true,
            ..Event::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EventFactory, EventType};
    use crate::{Stamp, Term, Truth};

    #[test]
    fn input_events_receive_monotonic_stamps() {
        let mut factory = EventFactory::new();
        let first = factory.input_event(
            Term::from_atom(1),
            EventType::Belief,
            Truth::new(1.0, 0.9),
            0.0,
            3,
        );
        let second = factory.input_event(
            Term::from_atom(2),
            EventType::Goal,
            Truth::new(1.0, 0.9),
            0.0,
            4,
        );
        assert_eq!(first.stamp, Stamp::from_evidence([1]));
        assert_eq!(second.stamp, Stamp::from_evidence([2]));
    }
}
