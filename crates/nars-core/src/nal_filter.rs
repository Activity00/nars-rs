//! Structural filters applied by ONA before a derived event enters memory.

use crate::{Narsese, Term};

pub(crate) fn derived_term_valid(language: &Narsese, term: &Term) -> bool {
    !atom_appears_twice(language, term)
        && !nested_higher_order_statement(language, term)
        && !inheritance_or_similarity_has_dependent_variable(language, term)
        && !junction_is_right_nested(language, term)
        && !invalid_set_operation(language, term)
        && !independent_or_dependent_variable_appears_once(language, term)
        && !declarative_implication_has_leftmost_operation(language, term)
}

pub(crate) fn variable_introduction_valid(language: &Narsese, term: &Term) -> bool {
    !higher_order_component_is_invalid(language, term, true)
        && !declarative_implication_without_independent_variable(language, term)
}

fn atom_appears_twice(language: &Narsese, term: &Term) -> bool {
    if (language.copula_equals(term.root(), '^') || language.copula_equals(term.root(), '?'))
        && term.extract_subterm(1) == term.extract_subterm(2)
    {
        return true;
    }
    if !(language.copula_equals(term.root(), ':') || language.copula_equals(term.root(), '=')) {
        return false;
    }
    let relational =
        language.copula_equals(term.atoms()[1], '*') && language.is_simple_atom(term.atoms()[2]);
    if relational {
        return false;
    }
    let mut appeared = std::collections::HashSet::new();
    for atom in term.atoms().iter().copied() {
        if !language.is_simple_atom(atom) {
            continue;
        }
        if !appeared.insert(atom) {
            return true;
        }
    }
    false
}

fn nested_higher_order_statement(language: &Narsese, term: &Term) -> bool {
    let mut declarative = 0;
    let mut temporal = 0;
    for atom in term.atoms().iter().copied() {
        if language.copula_equals(atom, '?') || language.copula_equals(atom, '^') {
            declarative += 1;
        }
        if language.copula_equals(atom, '$') {
            temporal += 1;
        }
        if declarative >= 2 || temporal > 2 {
            return true;
        }
    }
    false
}

fn inheritance_or_similarity_has_dependent_variable(language: &Narsese, term: &Term) -> bool {
    (language.copula_equals(term.root(), ':') || language.copula_equals(term.root(), '='))
        && language.has_variable(term, false, true, false)
}

fn higher_order_component_is_invalid(
    language: &Narsese,
    term: &Term,
    first_iteration: bool,
) -> bool {
    if ['^', '?', ';', '_']
        .into_iter()
        .any(|copula| language.copula_equals(term.root(), copula))
    {
        return higher_order_component_is_invalid(language, &term.extract_subterm(1), false)
            || higher_order_component_is_invalid(language, &term.extract_subterm(2), false);
    }
    if first_iteration
        || !(language.copula_equals(term.root(), ':') || language.copula_equals(term.root(), '='))
    {
        return false;
    }

    let subject = term.extract_subterm(1);
    let predicate = term.extract_subterm(2);
    let subject_is_variable = language.is_independent_variable(subject.root())
        || language.is_dependent_variable(subject.root());
    let predicate_is_variable = language.is_independent_variable(predicate.root())
        || language.is_dependent_variable(predicate.root());
    if subject == predicate || (subject_is_variable && predicate_is_variable) {
        return true;
    }
    if !language.has_variable(term, true, true, false) || !has_simple_atom(language, term) {
        return true;
    }

    let subject_has_product = language.copula_equals(subject.root(), '*');
    let predicate_has_product = language.copula_equals(predicate.root(), '*');
    let subject_is_image =
        language.copula_equals(subject.root(), '\\') || language.copula_equals(subject.root(), '#');
    let predicate_is_image = language.copula_equals(predicate.root(), '/')
        || language.copula_equals(predicate.root(), '%');
    let subject_is_set_element = language.copula_equals(subject.root(), '.');
    let predicate_is_set_element = language.copula_equals(predicate.root(), '.');
    if !subject_is_image
        && !predicate_is_image
        && !subject_is_set_element
        && !predicate_is_set_element
        && ((!subject_has_product
            && language.has_variable(&subject, true, true, false)
            && has_simple_atom(language, &subject))
            || (!predicate_has_product
                && language.has_variable(&predicate, true, true, false)
                && has_simple_atom(language, &predicate)))
    {
        return true;
    }
    if subject_is_image && image_component_mixes_variables_and_atoms(language, &subject) {
        return true;
    }
    predicate_is_image && image_component_mixes_variables_and_atoms(language, &predicate)
}

fn image_component_mixes_variables_and_atoms(language: &Narsese, image: &Term) -> bool {
    [image.extract_subterm(1), image.extract_subterm(2)]
        .into_iter()
        .any(|component| {
            language.has_variable(&component, true, true, false)
                && has_simple_atom(language, &component)
        })
}

fn junction_is_right_nested(language: &Narsese, term: &Term) -> bool {
    for (index, atom) in term.atoms().iter().copied().enumerate() {
        if !(language.copula_equals(atom, ';') || language.copula_equals(atom, '_')) {
            continue;
        }
        let right_child = index * 2 + 2;
        if right_child < term.atoms().len()
            && (language.copula_equals(term.atoms()[right_child], ';')
                || language.copula_equals(term.atoms()[right_child], '_'))
        {
            return true;
        }
    }
    false
}

fn invalid_set_operation(language: &Narsese, term: &Term) -> bool {
    language.copula_equals(term.root(), ':')
        && ((language.copula_equals(term.atoms()[1], '&')
            && language.copula_equals(term.atoms()[3], '"')
            && language.copula_equals(term.atoms()[4], '"'))
            || (language.copula_equals(term.atoms()[2], '|')
                && language.copula_equals(term.atoms()[5], '\'')
                && language.copula_equals(term.atoms()[6], '\'')))
}

fn independent_or_dependent_variable_appears_once(language: &Narsese, term: &Term) -> bool {
    let mut counts = std::collections::HashMap::new();
    for atom in term.atoms().iter().copied() {
        *counts.entry(atom).or_insert(0_u8) += 1;
    }
    term.atoms().iter().copied().any(|atom| {
        (language.is_independent_variable(atom) || language.is_dependent_variable(atom))
            && counts.get(&atom).copied().unwrap_or(0) == 1
    })
}

fn declarative_implication_without_independent_variable(language: &Narsese, term: &Term) -> bool {
    (language.copula_equals(term.root(), '?') || language.copula_equals(term.root(), '^'))
        && !language.has_variable(term, true, false, false)
}

fn declarative_implication_has_leftmost_operation(language: &Narsese, term: &Term) -> bool {
    if !(language.copula_equals(term.root(), '?') || language.copula_equals(term.root(), '^'))
        || !language.copula_equals(term.atoms()[1], ';')
    {
        return false;
    }
    leftmost_conjunction_component_is_operation(language, &term.extract_subterm(1))
}

fn leftmost_conjunction_component_is_operation(language: &Narsese, term: &Term) -> bool {
    if !language.copula_equals(term.root(), ';') {
        return false;
    }
    let left = term.extract_subterm(1);
    language.is_operation(&left)
        || (language.copula_equals(left.root(), ';')
            && leftmost_conjunction_component_is_operation(language, &left))
}

fn has_simple_atom(language: &Narsese, term: &Term) -> bool {
    term.atoms()
        .iter()
        .copied()
        .any(|atom| language.is_simple_atom(atom))
}

#[cfg(test)]
mod tests {
    use super::{derived_term_valid, variable_introduction_valid};
    use crate::Narsese;

    #[test]
    fn rejects_ona_invalid_derived_structures() {
        let mut language = Narsese::new();
        for source in [
            "<a --> a>",
            "<<a --> b> ==> <c ==> d>>",
            "<#x --> a>",
            "(a && (b && c))",
            "<({a} & {b}) --> c>",
            "<a --> $x>",
            "<(^act && a) ==> g>",
        ] {
            let term = language.parse_term(source).unwrap();
            assert!(!derived_term_valid(&language, &term), "{source}");
        }
    }

    #[test]
    fn accepts_ona_valid_derived_boundaries() {
        let mut language = Narsese::new();
        for source in [
            "<(a * a) --> relation>",
            "<<a =/> b> =/> c>",
            "(($x && a) && $x)",
            "<($x * $x) --> relation>",
            "<(a && ^act) ==> g>",
        ] {
            let term = language.parse_term(source).unwrap();
            assert!(derived_term_valid(&language, &term), "{source}");
        }
    }

    #[test]
    fn validates_variable_introduced_higher_order_terms() {
        let mut language = Narsese::new();
        let valid = language.parse_term("<<$x --> a> ==> <$x --> b>>").unwrap();
        let no_independent = language.parse_term("<<#x --> a> ==> <#x --> b>>").unwrap();
        let mixed_component = language
            .parse_term("<<($x && a) --> b> ==> <$x --> c>>")
            .unwrap();
        assert!(variable_introduction_valid(&language, &valid));
        assert!(!variable_introduction_valid(&language, &no_independent));
        assert!(!variable_introduction_valid(&language, &mixed_component));
    }
}
