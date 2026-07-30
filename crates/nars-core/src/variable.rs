//! NAL-6 variable recognition, unification, and substitution.
#![allow(clippy::float_cmp)]

use std::collections::HashMap;

use crate::config::COMPOUND_TERM_SIZE_MAX;
use crate::{Atom, Narsese, Term, Truth};

const VARIABLE_SLOTS: usize = 28;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Substitution {
    pub map: [Term; VARIABLE_SLOTS],
    pub success: bool,
    pub truth: Truth,
}

impl Default for Substitution {
    fn default() -> Self {
        Self {
            map: [Term::EMPTY; VARIABLE_SLOTS],
            success: false,
            truth: Truth::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UnifyOptions {
    pub truth: Truth,
    pub query_variables_only: bool,
    pub numeric_similarity: bool,
    pub similarity_distance: f64,
}

impl Default for UnifyOptions {
    fn default() -> Self {
        Self {
            truth: Truth::default(),
            query_variables_only: false,
            numeric_similarity: true,
            similarity_distance: 1.0,
        }
    }
}

impl Substitution {
    #[must_use]
    pub fn unify(language: &Narsese, general: &Term, specific: &Term) -> Self {
        Self::unify_with(language, general, specific, UnifyOptions::default())
    }

    #[must_use]
    pub fn unify_with(
        language: &Narsese,
        general: &Term,
        specific: &Term,
        options: UnifyOptions,
    ) -> Self {
        let mut substitution = Self {
            truth: options.truth,
            ..Self::default()
        };

        for index in 0..COMPOUND_TERM_SIZE_MAX {
            let general_atom = general.atoms()[index];
            if general_atom == 0 {
                continue;
            }
            let allowed_variable = if options.query_variables_only {
                language.is_query_variable(general_atom)
            } else {
                language.is_variable(general_atom)
            };
            if allowed_variable {
                let slot = usize::from(general_atom);
                if slot >= VARIABLE_SLOTS {
                    return substitution;
                }
                let subtree = specific.extract_subterm(index);
                if language.is_query_variable(general_atom) && language.is_variable(subtree.root())
                {
                    return substitution;
                }
                if !substitution.map[slot].is_empty() && substitution.map[slot] != subtree {
                    return substitution;
                }
                if language.copula_equals(subtree.root(), '@') {
                    return substitution;
                }
                substitution.map[slot] = subtree;
            } else {
                let specific_atom = specific.atoms()[index];
                if general_atom == specific_atom {
                    continue;
                }
                if !options.numeric_similarity
                    || options.truth.confidence == 0.0
                    || options.similarity_distance <= 0.0
                {
                    return substitution;
                }
                let analogous = language
                    .atom_value(general_atom)
                    .zip(language.atom_value(specific_atom));
                let Some((
                    (general_value, general_measurement),
                    (specific_value, specific_measurement),
                )) = analogous
                else {
                    return substitution;
                };
                if general_measurement != specific_measurement {
                    return substitution;
                }
                let confidence = (1.0_f64
                    - (general_value - specific_value).abs() / options.similarity_distance)
                    .max(0.0);
                substitution.truth = substitution.truth.analogy(Truth::new(1.0, confidence));
                if substitution.truth.confidence == 0.0 {
                    return substitution;
                }
            }
        }
        substitution.success = true;
        substitution
    }

    /// Applies this substitution using ONA's subtree overwrite semantics.
    ///
    /// Returns `None` for an unsuccessful substitution or if a replacement
    /// exceeds the fixed term capacity.
    #[must_use]
    pub fn apply(&self, language: &Narsese, mut term: Term) -> Option<Term> {
        if !self.success {
            return None;
        }
        for index in 0..COMPOUND_TERM_SIZE_MAX {
            let atom = term.atoms()[index];
            if !language.is_variable(atom) {
                continue;
            }
            let slot = usize::from(atom);
            if slot >= VARIABLE_SLOTS {
                return None;
            }
            let replacement = self.map[slot];
            if !replacement.is_empty() && !term.override_subterm(index, &replacement) {
                return None;
            }
        }
        Some(term)
    }
}

fn increment_simple_atoms(language: &Narsese, term: &Term, counts: &mut HashMap<Atom, usize>) {
    for atom in term
        .atoms()
        .iter()
        .copied()
        .filter(|atom| language.is_simple_atom(*atom))
    {
        *counts.entry(atom).or_default() += 1;
    }
}

#[allow(clippy::too_many_lines)]
fn count_statement_atoms(
    language: &Narsese,
    statement: &Term,
    counts: &mut HashMap<Atom, usize>,
    extensional: bool,
    ignore_structure: bool,
    spatial_composition: bool,
    side_of_product: u8,
) {
    let similarity = language.copula_equals(statement.root(), '=');
    if (language.copula_equals(statement.root(), ',')
        && !language.copula_equals(statement.atoms()[1], '*')
        && extensional)
        || language.copula_equals(statement.root(), ':')
        || similarity
    {
        let subject = statement.extract_subterm(1);
        let predicate = statement.extract_subterm(2);
        if extensional || similarity {
            if language.copula_equals(subject.root(), '\\')
                || language.copula_equals(subject.root(), '#')
            {
                count_statement_atoms(
                    language,
                    &subject.extract_subterm(1),
                    counts,
                    extensional,
                    true,
                    spatial_composition,
                    side_of_product,
                );
            } else if side_of_product != 0 && language.copula_equals(subject.root(), '*') {
                if side_of_product == 1 {
                    count_statement_atoms(
                        language,
                        &subject.extract_subterm(1),
                        counts,
                        extensional,
                        true,
                        spatial_composition,
                        side_of_product,
                    );
                } else if side_of_product == 2 {
                    count_statement_atoms(
                        language,
                        &subject.extract_subterm(2),
                        counts,
                        extensional,
                        true,
                        spatial_composition,
                        side_of_product,
                    );
                }
            } else {
                count_statement_atoms(
                    language,
                    &subject,
                    counts,
                    extensional,
                    true,
                    spatial_composition,
                    side_of_product,
                );
            }
            if language.copula_equals(predicate.root(), '/')
                || language.copula_equals(predicate.root(), '%')
            {
                count_statement_atoms(
                    language,
                    &predicate.extract_subterm(2),
                    counts,
                    extensional,
                    true,
                    spatial_composition,
                    side_of_product,
                );
            }
        }
        if !extensional || similarity {
            if language.copula_equals(predicate.root(), '/')
                || language.copula_equals(predicate.root(), '%')
            {
                count_statement_atoms(
                    language,
                    &predicate.extract_subterm(1),
                    counts,
                    extensional,
                    true,
                    spatial_composition,
                    side_of_product,
                );
            } else {
                count_statement_atoms(
                    language,
                    &predicate,
                    counts,
                    extensional,
                    true,
                    spatial_composition,
                    side_of_product,
                );
            }
            if language.copula_equals(subject.root(), '\\')
                || language.copula_equals(subject.root(), '#')
            {
                count_statement_atoms(
                    language,
                    &subject.extract_subterm(2),
                    counts,
                    extensional,
                    true,
                    spatial_composition,
                    side_of_product,
                );
            }
        }
    }

    if ignore_structure || spatial_composition {
        let multi_element_set = (language.copula_equals(statement.root(), '"')
            || language.copula_equals(statement.root(), '\''))
            && !language.copula_equals(statement.atoms()[2], '@');
        if multi_element_set && !spatial_composition {
            return;
        }
        increment_simple_atoms(language, statement, counts);
    }
}

fn count_higher_order_atoms(
    language: &Narsese,
    term: &Term,
    counts: &mut HashMap<Atom, usize>,
    extensional: bool,
    side_of_product: u8,
) {
    if language.copula_equals(term.root(), '!') {
        count_higher_order_atoms(
            language,
            &term.extract_subterm(1),
            counts,
            extensional,
            side_of_product,
        );
    } else if ['+', ';', '$', '?', '^']
        .into_iter()
        .any(|copula| language.copula_equals(term.root(), copula))
    {
        count_higher_order_atoms(
            language,
            &term.extract_subterm(1),
            counts,
            extensional,
            side_of_product,
        );
        count_higher_order_atoms(
            language,
            &term.extract_subterm(2),
            counts,
            extensional,
            side_of_product,
        );
    } else {
        count_statement_atoms(
            language,
            term,
            counts,
            extensional,
            false,
            language.copula_equals(term.root(), '\''),
            side_of_product,
        );
    }
}

fn next_variable_id(language: &mut Narsese, term: &Term, independent: bool) -> Option<u8> {
    for id in 1..=9 {
        let prefix = if independent { '$' } else { '#' };
        let atom = language.intern(&format!("{prefix}{id}")).ok()?;
        if !term.has_atom(atom) {
            return Some(id);
        }
    }
    None
}

/// Introduces independent and dependent variables into an implication.
///
/// `side_of_product` uses ONA's values: `0` for both, `1` for left, and `2`
/// for right.
#[must_use]
pub fn introduce_implication_variables(
    language: &mut Narsese,
    implication: Term,
    extensional: bool,
    side_of_product: u8,
) -> Option<Term> {
    if !['$', '?', '^']
        .into_iter()
        .any(|copula| language.copula_equals(implication.root(), copula))
    {
        return None;
    }
    if !extensional
        && side_of_product == 0
        && implication
            .atoms()
            .iter()
            .copied()
            .any(|atom| language.copula_equals(atom, '*'))
    {
        return None;
    }

    let left = implication.extract_subterm(1);
    let right = implication.extract_subterm(2);
    let operation_sequence = if extensional {
        language.operation_sequence(&left).ok().flatten()
    } else {
        None
    };
    let mut left_counts = HashMap::new();
    let mut right_counts = HashMap::new();
    let mut operation_counts = HashMap::new();
    count_higher_order_atoms(
        language,
        &left,
        &mut left_counts,
        extensional,
        side_of_product,
    );
    count_higher_order_atoms(
        language,
        &right,
        &mut right_counts,
        extensional,
        side_of_product,
    );
    if operation_sequence.is_some() {
        count_higher_order_atoms(
            language,
            &left,
            &mut operation_counts,
            extensional,
            side_of_product,
        );
    }

    let mut dependent_id = next_variable_id(language, &implication, false)?;
    let mut independent_id = next_variable_id(language, &implication, true)?;
    let original = implication;
    let mut output = implication;
    let mut assigned: HashMap<Atom, u8> = HashMap::new();
    for index in 0..COMPOUND_TERM_SIZE_MAX {
        let atom = original.atoms()[index];
        let left_count = left_counts.get(&atom).copied().unwrap_or(0);
        let right_count = right_counts.get(&atom).copied().unwrap_or(0);
        if left_count < 2 && right_count < 2 && (left_count == 0 || right_count == 0) {
            continue;
        }

        let appears_both = left_count > 0 && right_count > 0;
        let (prefix, variable_id) = if appears_both {
            let id = *assigned.entry(atom).or_insert_with(|| {
                let id = independent_id;
                independent_id += 1;
                id
            });
            ('$', id)
        } else {
            let operation_count = operation_counts.get(&atom).copied().unwrap_or(0);
            if atom == language.self_atom()
                && operation_sequence.is_some()
                && operation_count == left_count
            {
                continue;
            }
            let id = *assigned.entry(atom).or_insert_with(|| {
                let id = dependent_id;
                dependent_id += 1;
                id
            });
            ('#', id)
        };
        if variable_id <= 9 {
            output.atoms_mut()[index] = language.intern(&format!("{prefix}{variable_id}")).ok()?;
        }
    }
    Some(output)
}

#[must_use]
pub fn introduce_conjunction_variables(
    language: &mut Narsese,
    conjunction: Term,
    extensional: bool,
) -> Option<Term> {
    if !language.copula_equals(conjunction.root(), ';') {
        return None;
    }
    if !extensional
        && conjunction
            .atoms()
            .iter()
            .copied()
            .any(|atom| language.copula_equals(atom, '*'))
    {
        return None;
    }
    let mut counts = HashMap::new();
    count_higher_order_atoms(language, &conjunction, &mut counts, extensional, 0);
    let mut dependent_id = next_variable_id(language, &conjunction, false)?;
    let original = conjunction;
    let mut output = conjunction;
    let mut assigned: HashMap<Atom, u8> = HashMap::new();
    for index in 0..COMPOUND_TERM_SIZE_MAX {
        let atom = original.atoms()[index];
        if counts.get(&atom).copied().unwrap_or(0) < 2 {
            continue;
        }
        let id = *assigned.entry(atom).or_insert_with(|| {
            let id = dependent_id;
            dependent_id += 1;
            id
        });
        if id <= 9 {
            output.atoms_mut()[index] = language.intern(&format!("#{id}")).ok()?;
        }
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::{
        Substitution, UnifyOptions, introduce_conjunction_variables,
        introduce_implication_variables,
    };
    use crate::{Narsese, Truth};

    #[test]
    fn unifies_and_substitutes_repeated_variable() {
        let mut language = Narsese::new();
        let general = language.parse_term("<$x --> animal>").unwrap();
        let specific = language.parse_term("<cat --> animal>").unwrap();
        let substitution = Substitution::unify(&language, &general, &specific);
        assert!(substitution.success);
        assert_eq!(
            language.format_term(&substitution.apply(&language, general).unwrap()),
            "<cat --> animal>"
        );
    }

    #[test]
    fn repeated_variable_must_be_consistent() {
        let mut language = Narsese::new();
        let general = language.parse_term("<$x --> $x>").unwrap();
        let specific = language.parse_term("<cat --> dog>").unwrap();
        assert!(!Substitution::unify(&language, &general, &specific).success);
    }

    #[test]
    fn query_variable_cannot_substitute_another_variable() {
        let mut language = Narsese::new();
        let general = language.parse_term("<?what --> animal>").unwrap();
        let specific = language.parse_term("<$something --> animal>").unwrap();
        assert!(!Substitution::unify(&language, &general, &specific).success);
    }

    #[test]
    fn numeric_similarity_adjusts_unification_truth() {
        let mut language = Narsese::new();
        let general = language.parse_term("temperature20").unwrap();
        let specific = language.parse_term("temperature20_2").unwrap();
        language
            .set_atom_value(general.root(), 20.0, "celsius")
            .unwrap();
        language
            .set_atom_value(specific.root(), 20.2, "celsius")
            .unwrap();
        let substitution = Substitution::unify_with(
            &language,
            &general,
            &specific,
            UnifyOptions {
                truth: Truth::new(1.0, 0.9),
                ..UnifyOptions::default()
            },
        );
        assert!(substitution.success);
        assert!(substitution.truth.confidence > 0.0);
        assert!(substitution.truth.confidence < 0.9);
    }

    #[test]
    fn implication_introduces_independent_variable_across_sides() {
        let mut language = Narsese::new();
        let implication = language
            .parse_term("<(<cat --> animal> && <cat --> furry>) ==> <cat --> good>>")
            .unwrap();
        let introduced =
            introduce_implication_variables(&mut language, implication, true, 0).unwrap();
        assert_eq!(
            language.format_term(&introduced),
            "<(<$1 --> animal> && <$1 --> furry>) ==> <$1 --> good>>"
        );
    }

    #[test]
    fn conjunction_introduces_dependent_variable_for_repetition() {
        let mut language = Narsese::new();
        let conjunction = language
            .parse_term("(<cat --> animal> && <cat --> furry>)")
            .unwrap();
        let introduced = introduce_conjunction_variables(&mut language, conjunction, true).unwrap();
        assert_eq!(
            language.format_term(&introduced),
            "(<#1 --> animal> && <#1 --> furry>)"
        );
    }
}
