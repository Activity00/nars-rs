//! Core event and implication inference operations.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use crate::config::OCCURRENCE_ETERNAL;
use crate::{Event, EventType, Implication, Narsese, NarseseError, Truth};

#[must_use]
pub fn event_update(event: Event, target_time: i64) -> Event {
    Event {
        truth: event.truth.project(event.occurrence_time, target_time),
        occurrence_time: target_time,
        ..event
    }
}

#[must_use]
pub fn event_revision(first: Event, second: Event) -> Event {
    let conclusion_time = second.occurrence_time;
    Event {
        term: first.term,
        event_type: first.event_type,
        truth: first
            .truth
            .project(first.occurrence_time, conclusion_time)
            .revision(second.truth),
        stamp: first.stamp.zip(&second.stamp),
        occurrence_time: conclusion_time,
        creation_time: first.creation_time.max(second.creation_time),
        ..Event::default()
    }
}

/// Intersects two time-ordered belief events into a sequence.
///
/// # Errors
///
/// Returns an error if events are out of temporal order or the sequence cannot
/// fit in the fixed term representation.
pub fn belief_intersection(
    narsese: &mut Narsese,
    first: Event,
    second: Event,
) -> Result<Event, NarseseError> {
    if second.occurrence_time < first.occurrence_time {
        return Err(NarseseError::from_message(
            "belief intersection requires second event after first",
        ));
    }
    Ok(Event {
        term: narsese.sequence(&first.term, &second.term)?,
        event_type: EventType::Belief,
        truth: first
            .truth
            .project(first.occurrence_time, second.occurrence_time)
            .intersection(second.truth),
        stamp: first.stamp.zip(&second.stamp),
        occurrence_time: second.occurrence_time,
        creation_time: first.creation_time.max(second.creation_time),
        ..Event::default()
    })
}

/// Induces a temporal implication from two ordered belief events.
///
/// # Errors
///
/// Returns an error if events are out of order or the implication term exceeds
/// the fixed term representation.
pub fn belief_induction(
    narsese: &mut Narsese,
    first: Event,
    second: Event,
) -> Result<Implication, NarseseError> {
    if second.occurrence_time < first.occurrence_time {
        return Err(NarseseError::from_message(
            "belief induction requires second event after first",
        ));
    }
    let mut term = crate::Term::from_atom(narsese.copula_atom('$')?);
    if !term.override_subterm(1, &first.term) || !term.override_subterm(2, &second.term) {
        return Err(NarseseError::from_message(
            "implication exceeds compound term capacity",
        ));
    }
    let first_truth = first
        .truth
        .project(first.occurrence_time, second.occurrence_time);
    Ok(Implication {
        term,
        truth: second.truth.induction(first_truth),
        stamp: first.stamp.zip(&second.stamp),
        occurrence_time_offset: (second.occurrence_time - first.occurrence_time) as f64,
        creation_time: first.creation_time.max(second.creation_time),
        ..Implication::default()
    })
}

#[must_use]
pub fn implication_revision(
    narsese: &Narsese,
    first: Implication,
    second: Implication,
) -> Implication {
    let first_weight = Truth::confidence_to_weight(first.truth.confidence);
    let second_weight = Truth::confidence_to_weight(second.truth.confidence);
    let offset = (first.occurrence_time_offset * first_weight
        + second.occurrence_time_offset * second_weight)
        / (first_weight + second_weight);
    let overlap = first.stamp.overlaps(&second.stamp);
    let (choice_truth, choice_stamp) = if overlap {
        if first.truth.confidence > second.truth.confidence {
            (first.truth, first.stamp)
        } else {
            (second.truth, second.stamp)
        }
    } else {
        (
            first.truth.revision(second.truth),
            first.stamp.zip(&second.stamp),
        )
    };
    let declarative = narsese.copula_equals(first.term.root(), '?');
    Implication {
        term: first.term,
        truth: if declarative {
            choice_truth
        } else {
            first.truth.revision(second.truth)
        },
        stamp: if declarative {
            choice_stamp
        } else {
            first.stamp.zip(&second.stamp)
        },
        occurrence_time_offset: offset,
        source_concept_id: first.source_concept_id,
        creation_time: first.creation_time.max(second.creation_time),
    }
}

#[must_use]
pub fn belief_deduction(narsese: &Narsese, component: Event, implication: Implication) -> Event {
    debug_assert!(
        narsese.copula_equals(implication.term.root(), '$')
            || narsese.copula_equals(implication.term.root(), '?')
    );
    Event {
        term: implication.term.extract_subterm(2),
        event_type: EventType::Belief,
        truth: implication.truth.deduction(component.truth),
        stamp: component.stamp.zip(&implication.stamp),
        occurrence_time: if component.occurrence_time == OCCURRENCE_ETERNAL {
            OCCURRENCE_ETERNAL
        } else {
            component.occurrence_time + implication.occurrence_time_offset as i64
        },
        creation_time: component.creation_time.max(implication.creation_time),
        ..Event::default()
    }
}

#[must_use]
pub fn goal_deduction(
    narsese: &Narsese,
    component: Event,
    implication: Implication,
    current_time: i64,
) -> Event {
    debug_assert!(
        narsese.copula_equals(implication.term.root(), '$')
            || narsese.copula_equals(implication.term.root(), '?')
    );
    let precondition = implication.term.extract_subterm(1);
    Event {
        term: narsese.precondition_without_operation(&precondition),
        event_type: EventType::Goal,
        truth: component.truth.goal_deduction(implication.truth),
        stamp: component.stamp.zip(&implication.stamp),
        occurrence_time: current_time,
        creation_time: component.creation_time.max(implication.creation_time),
        ..Event::default()
    }
}

#[must_use]
pub fn goal_sequence_deduction(compound: Event, component: Event, current_time: i64) -> Event {
    let compound_updated = event_update(compound, current_time);
    let component_updated = event_update(component, current_time);
    Event {
        term: compound.term,
        event_type: EventType::Goal,
        truth: compound_updated
            .truth
            .goal_deduction(component_updated.truth),
        stamp: component.stamp.zip(&compound.stamp),
        occurrence_time: current_time,
        creation_time: component.creation_time.max(compound.creation_time),
        ..Event::default()
    }
}

#[must_use]
pub fn revision_and_choice(narsese: &Narsese, existing: Event, incoming: Event) -> (Event, bool) {
    if existing.event_type == EventType::Deleted {
        return (incoming, false);
    }
    let later = existing.occurrence_time.max(incoming.occurrence_time);
    let existing_updated = event_update(existing, later);
    let incoming_updated = event_update(incoming, later);
    let dependent_variable_conjunction = (narsese.copula_equals(incoming.term.root(), ';')
        || narsese.copula_equals(incoming.term.root(), '+'))
        && narsese.has_variable(&incoming.term, false, true, false);
    let must_choose = incoming.stamp.overlaps(&existing.stamp)
        || dependent_variable_conjunction
        || (existing.occurrence_time != OCCURRENCE_ETERNAL
            && existing.occurrence_time != incoming.occurrence_time)
        || existing.term != incoming.term;

    if must_choose {
        if incoming_updated.truth.confidence > existing_updated.truth.confidence {
            (incoming, false)
        } else {
            (existing, false)
        }
    } else {
        let revised = event_revision(existing_updated, incoming_updated);
        if revised.truth.confidence >= existing_updated.truth.confidence {
            (revised, true)
        } else {
            (existing, false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        belief_deduction, belief_induction, belief_intersection, goal_deduction,
        revision_and_choice,
    };
    use crate::{Event, EventFactory, EventType, Implication, Narsese, Stamp, Truth};

    #[test]
    fn temporal_induction_and_deduction_round_trip_terms() {
        let mut language = Narsese::new();
        let a = language.parse_term("a").unwrap();
        let b = language.parse_term("b").unwrap();
        let mut factory = EventFactory::new();
        let first = factory.input_event(a, EventType::Belief, Truth::new(1.0, 0.9), 0.0, 1);
        let second = factory.input_event(b, EventType::Belief, Truth::new(1.0, 0.9), 0.0, 3);
        let implication = belief_induction(&mut language, first, second).unwrap();
        assert_eq!(language.format_term(&implication.term), "<a =/> b>");
        let result = belief_deduction(&language, first, implication);
        assert_eq!(language.format_term(&result.term), "b");
        assert_eq!(result.occurrence_time, 3);
    }

    #[test]
    fn intersection_builds_sequence() {
        let mut language = Narsese::new();
        let mut factory = EventFactory::new();
        let a = factory.input_event(
            language.parse_term("a").unwrap(),
            EventType::Belief,
            Truth::new(1.0, 0.9),
            0.0,
            1,
        );
        let b = factory.input_event(
            language.parse_term("b").unwrap(),
            EventType::Belief,
            Truth::new(1.0, 0.9),
            0.0,
            2,
        );
        let sequence = belief_intersection(&mut language, a, b).unwrap();
        assert_eq!(language.format_term(&sequence.term), "(a &/ b)");
    }

    #[test]
    fn independent_evidence_revises() {
        let mut language = Narsese::new();
        let term = language.parse_term("a").unwrap();
        let mut factory = EventFactory::new();
        let first = factory.input_event(term, EventType::Belief, Truth::new(1.0, 0.9), 0.0, -1);
        let second = factory.input_event(term, EventType::Belief, Truth::new(0.0, 0.9), 0.0, -1);
        let (revised, did_revise) = revision_and_choice(&language, first, second);
        assert!(did_revise);
        assert!((revised.truth.frequency - 0.5).abs() < 1.0e-12);
    }

    #[test]
    fn procedural_goal_deduction_removes_trailing_operation() {
        let mut language = Narsese::new();
        let component = Event {
            term: language.parse_term("reward").unwrap(),
            event_type: EventType::Goal,
            truth: Truth::new(1.0, 0.9),
            stamp: Stamp::from_evidence([1]),
            occurrence_time: 5,
            creation_time: 5,
            ..Event::default()
        };
        let implication = Implication {
            term: language
                .parse_term("<(<state --> active> &/ ^move) =/> reward>")
                .unwrap(),
            truth: Truth::new(1.0, 0.9),
            stamp: Stamp::from_evidence([2]),
            occurrence_time_offset: 1.0,
            creation_time: 4,
            ..Implication::default()
        };
        let subgoal = goal_deduction(&language, component, implication, 5);
        assert_eq!(language.format_term(&subgoal.term), "<state --> active>");
        assert_eq!(subgoal.event_type, EventType::Goal);
    }
}
