//! Runtime form of ONA's generated declarative NAL rule table.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use crate::{Atom, Narsese, Term, Truth};

const ONA_NAL_RULES: &str = include_str!("../rules/ona-nal.h");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleEngineError(String);

impl RuleEngineError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for RuleEngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for RuleEngineError {}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Derivation {
    pub term: Term,
    pub truth: Truth,
    pub variable_introduction: bool,
}

#[derive(Clone, Copy, Debug)]
struct Rule {
    first: Term,
    second: Option<Term>,
    conclusion: Term,
    truth_function: TruthFunction,
    switch_truth_arguments: bool,
    variable_introduction: bool,
}

#[derive(Clone, Copy, Debug)]
struct Reduction {
    pattern: Term,
    replacement: Term,
}

#[derive(Clone, Copy, Debug)]
enum TruthFunction {
    Deduction,
    Induction,
    Abduction,
    Exemplification,
    StructuralIntersection,
    Resemblance,
    Comparison,
    Analogy,
    StructuralDeduction,
    StructuralDeductionNegated,
    Intersection,
    Union,
    Difference,
    DecomposePnn,
    DecomposeNpp,
    DecomposePnp,
    DecomposeNnn,
    DecomposePpp,
    FrequencyGreater,
    FrequencyEqual,
    Negation,
}

impl TruthFunction {
    fn parse(name: &str) -> Result<Self, RuleEngineError> {
        match name.trim() {
            "Truth_Deduction" => Ok(Self::Deduction),
            "Truth_Induction" => Ok(Self::Induction),
            "Truth_Abduction" => Ok(Self::Abduction),
            "Truth_Exemplification" => Ok(Self::Exemplification),
            "Truth_StructuralIntersection" => Ok(Self::StructuralIntersection),
            "Truth_Resemblance" => Ok(Self::Resemblance),
            "Truth_Comparison" => Ok(Self::Comparison),
            "Truth_Analogy" => Ok(Self::Analogy),
            "Truth_StructuralDeduction" => Ok(Self::StructuralDeduction),
            "Truth_StructuralDeductionNegated" => Ok(Self::StructuralDeductionNegated),
            "Truth_Intersection" => Ok(Self::Intersection),
            "Truth_Union" => Ok(Self::Union),
            "Truth_Difference" => Ok(Self::Difference),
            "Truth_DecomposePNN" => Ok(Self::DecomposePnn),
            "Truth_DecomposeNPP" => Ok(Self::DecomposeNpp),
            "Truth_DecomposePNP" => Ok(Self::DecomposePnp),
            "Truth_DecomposeNNN" => Ok(Self::DecomposeNnn),
            "Truth_DecomposePPP" => Ok(Self::DecomposePpp),
            "Truth_FrequencyGreater" => Ok(Self::FrequencyGreater),
            "Truth_FrequencyEqual" => Ok(Self::FrequencyEqual),
            "Truth_Negation" => Ok(Self::Negation),
            other => Err(RuleEngineError::new(format!(
                "unsupported ONA truth function {other}"
            ))),
        }
    }

    fn apply(self, first: Truth, second: Truth) -> Truth {
        match self {
            Self::Deduction => first.deduction(second),
            Self::Induction => first.induction(second),
            Self::Abduction => first.abduction(second),
            Self::Exemplification => first.exemplification(second),
            Self::StructuralIntersection => first.structural_intersection(),
            Self::Resemblance => first.resemblance(second),
            Self::Comparison => first.comparison(second),
            Self::Analogy => first.analogy(second),
            Self::StructuralDeduction => first.structural_deduction(),
            Self::StructuralDeductionNegated => first.structural_deduction_negated(),
            Self::Intersection => first.intersection(second),
            Self::Union => first.union(second),
            Self::Difference => first.difference(second),
            Self::DecomposePnn => first.decompose_pnn(second),
            Self::DecomposeNpp => first.decompose_npp(second),
            Self::DecomposePnp => first.decompose_pnp(second),
            Self::DecomposeNnn => first.decompose_nnn(second),
            Self::DecomposePpp => first.decompose_ppp(second),
            Self::FrequencyGreater => first.frequency_greater(second),
            Self::FrequencyEqual => first.frequency_equal(second),
            Self::Negation => first.negation(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct RuleEngine {
    rules: Vec<Rule>,
    reductions: Vec<Reduction>,
}

impl RuleEngine {
    /// Loads the fixed ONA NAL-6 rule declarations bundled with this crate.
    ///
    /// # Errors
    ///
    /// Returns an error if the vendored rule declarations cannot be parsed as
    /// Narsese or reference an unsupported truth function.
    pub fn ona(language: &mut Narsese) -> Result<Self, RuleEngineError> {
        let (rules, reductions) = parse_ona_rules(language, ONA_NAL_RULES)?;
        Ok(Self { rules, reductions })
    }

    #[must_use]
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    #[must_use]
    pub fn reduction_count(&self) -> usize {
        self.reductions.len()
    }

    #[must_use]
    pub fn apply(
        &self,
        language: &Narsese,
        first: Term,
        second: Option<Term>,
        first_truth: Truth,
        second_truth: Truth,
    ) -> Vec<Derivation> {
        self.rules
            .iter()
            .filter(|rule| rule.second.is_some() == second.is_some())
            .filter_map(|rule| {
                let mut substitutions = HashMap::new();
                if !match_pattern(language, &rule.first, &first, &mut substitutions) {
                    return None;
                }
                if let (Some(pattern), Some(actual)) = (rule.second, second)
                    && !match_pattern(language, &pattern, &actual, &mut substitutions)
                {
                    return None;
                }
                let conclusion = build_conclusion(language, &rule.conclusion, &substitutions)?;
                let (left_truth, right_truth) = if rule.switch_truth_arguments {
                    (second_truth, first_truth)
                } else {
                    (first_truth, second_truth)
                };
                Some(Derivation {
                    term: self.reduce(language, conclusion),
                    truth: rule.truth_function.apply(left_truth, right_truth),
                    variable_introduction: rule.variable_introduction,
                })
            })
            .collect()
    }

    #[must_use]
    pub fn reduce(&self, language: &Narsese, term: Term) -> Term {
        for reduction in &self.reductions {
            let mut substitutions = HashMap::new();
            if match_pattern(language, &reduction.pattern, &term, &mut substitutions)
                && let Some(replacement) =
                    build_conclusion(language, &reduction.replacement, &substitutions)
            {
                return replacement;
            }
        }
        term
    }
}

fn match_pattern(
    language: &Narsese,
    pattern: &Term,
    actual: &Term,
    substitutions: &mut HashMap<Atom, Term>,
) -> bool {
    for (index, pattern_atom) in pattern.atoms().iter().copied().enumerate() {
        if pattern_atom == 0 {
            continue;
        }
        if language.is_rule_meta_variable(pattern_atom) {
            let subtree = actual.extract_subterm(index);
            if language.copula_equals(subtree.root(), '@') {
                return false;
            }
            if language
                .atom_name(pattern_atom)
                .is_some_and(|name| name.starts_with("Op"))
                && !language.is_operation(&subtree)
            {
                return false;
            }
            if let Some(existing) = substitutions.get(&pattern_atom) {
                if *existing != subtree {
                    return false;
                }
            } else {
                substitutions.insert(pattern_atom, subtree);
            }
        } else if actual.atoms()[index] != pattern_atom {
            return false;
        }
    }
    true
}

fn build_conclusion(
    language: &Narsese,
    pattern: &Term,
    substitutions: &HashMap<Atom, Term>,
) -> Option<Term> {
    let mut conclusion = Term::EMPTY;
    for (index, atom) in pattern.atoms().iter().copied().enumerate() {
        if atom == 0 {
            continue;
        }
        if language.is_rule_meta_variable(atom) {
            if !conclusion.override_subterm(index, substitutions.get(&atom)?) {
                return None;
            }
        } else {
            conclusion.atoms_mut()[index] = atom;
        }
    }
    Some(conclusion)
}

fn parse_ona_rules(
    language: &mut Narsese,
    source: &str,
) -> Result<(Vec<Rule>, Vec<Reduction>), RuleEngineError> {
    let mut rules = Vec::new();
    let mut reductions = Vec::new();
    let mut in_rules = false;
    let mut in_reductions = false;
    let mut skip_depth = 0_usize;

    for original_line in source.lines() {
        let line = original_line
            .split_once("//")
            .map_or(original_line, |(code, _)| code)
            .trim();
        if line == "#ifdef H_NAL_RULES" {
            in_rules = true;
            continue;
        }
        if line == "#ifdef H_NAL_REDUCTIONS" {
            in_rules = false;
            in_reductions = true;
            skip_depth = 0;
            continue;
        }
        if in_rules && line.starts_with("#if ") {
            if skip_depth > 0 || line.contains("== 5") {
                skip_depth += 1;
            }
            continue;
        }
        if in_rules && line == "#endif" {
            skip_depth = skip_depth.saturating_sub(1);
            continue;
        }
        if skip_depth > 0 || line.is_empty() {
            continue;
        }

        if in_rules {
            parse_rule_line(language, line, &mut rules)?;
        } else if in_reductions {
            parse_reduction_line(language, line, &mut reductions)?;
        }
    }
    Ok((rules, reductions))
}

fn parse_rule_line(
    language: &mut Narsese,
    line: &str,
    rules: &mut Vec<Rule>,
) -> Result<(), RuleEngineError> {
    if let Some(arguments) = macro_arguments(line, "R2VarIntro") {
        let arguments = split_arguments(arguments);
        add_double_rule(language, &arguments, rules, true)?;
    } else if let Some(arguments) = macro_arguments(line, "R2") {
        let arguments = split_arguments(arguments);
        add_double_rule(language, &arguments, rules, false)?;
    } else if let Some(arguments) = macro_arguments(line, "R1Bidirectional") {
        let arguments = split_arguments(arguments);
        require_arguments(&arguments, 4, line)?;
        let first = parse_term(language, arguments[0])?;
        let second = parse_term(language, arguments[2])?;
        let truth_function = TruthFunction::parse(arguments[3])?;
        rules.push(Rule {
            first,
            second: None,
            conclusion: second,
            truth_function,
            switch_truth_arguments: false,
            variable_introduction: false,
        });
        rules.push(Rule {
            first: second,
            second: None,
            conclusion: first,
            truth_function,
            switch_truth_arguments: false,
            variable_introduction: false,
        });
    } else if let Some(arguments) = macro_arguments(line, "R1") {
        let arguments = split_arguments(arguments);
        require_arguments(&arguments, 4, line)?;
        rules.push(Rule {
            first: parse_term(language, arguments[0])?,
            second: None,
            conclusion: parse_term(language, arguments[2])?,
            truth_function: TruthFunction::parse(arguments[3])?,
            switch_truth_arguments: false,
            variable_introduction: false,
        });
    }
    Ok(())
}

fn add_double_rule(
    language: &mut Narsese,
    arguments: &[&str],
    rules: &mut Vec<Rule>,
    variable_introduction: bool,
) -> Result<(), RuleEngineError> {
    require_arguments(arguments, 5, "double-premise rule")?;
    let first = parse_term(language, arguments[0])?;
    let second = parse_term(language, arguments[1])?;
    let conclusion = parse_term(language, arguments[3])?;
    let truth_function = TruthFunction::parse(arguments[4])?;
    rules.push(Rule {
        first,
        second: Some(second),
        conclusion,
        truth_function,
        switch_truth_arguments: false,
        variable_introduction,
    });
    rules.push(Rule {
        first: second,
        second: Some(first),
        conclusion,
        truth_function,
        switch_truth_arguments: true,
        variable_introduction,
    });
    Ok(())
}

fn parse_reduction_line(
    language: &mut Narsese,
    line: &str,
    reductions: &mut Vec<Reduction>,
) -> Result<(), RuleEngineError> {
    if let Some(arguments) = macro_arguments(line, "ReduceTerm") {
        let arguments = split_arguments(arguments);
        require_arguments(&arguments, 2, line)?;
        for (pattern, replacement) in [
            (
                format!("({} --> M)", arguments[0]),
                format!("({} --> M)", arguments[1]),
            ),
            (
                format!("(M --> {})", arguments[0]),
                format!("(M --> {})", arguments[1]),
            ),
        ] {
            reductions.push(Reduction {
                pattern: parse_term(language, &pattern)?,
                replacement: parse_term(language, &replacement)?,
            });
        }
    } else if let Some(arguments) = macro_arguments(line, "ReduceStatement") {
        let arguments = split_arguments(arguments);
        require_arguments(&arguments, 2, line)?;
        reductions.push(Reduction {
            pattern: parse_term(language, arguments[0])?,
            replacement: parse_term(language, arguments[1])?,
        });
    }
    Ok(())
}

fn macro_arguments<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    line.strip_prefix(name)?
        .trim_start()
        .strip_prefix('(')?
        .trim_end()
        .strip_suffix(')')
}

fn split_arguments(input: &str) -> Vec<&str> {
    let mut arguments = Vec::new();
    let mut depth = 0_i32;
    let mut start = 0;
    for (index, character) in input.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                arguments.push(input[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    arguments.push(input[start..].trim());
    arguments
}

fn require_arguments(
    arguments: &[&str],
    expected: usize,
    context: &str,
) -> Result<(), RuleEngineError> {
    if arguments.len() == expected {
        Ok(())
    } else {
        Err(RuleEngineError::new(format!(
            "expected {expected} arguments in {context}, got {}",
            arguments.len()
        )))
    }
}

fn parse_term(language: &mut Narsese, source: &str) -> Result<Term, RuleEngineError> {
    language
        .parse_term(source.trim())
        .map_err(|error| RuleEngineError::new(format!("cannot parse rule term {source}: {error}")))
}

#[cfg(test)]
mod tests {
    use super::RuleEngine;
    use crate::{Narsese, Truth};

    #[test]
    fn parses_exact_upstream_generated_rule_counts() {
        let mut language = Narsese::new();
        let engine = RuleEngine::ona(&mut language).unwrap();
        assert_eq!(engine.rule_count(), 235);
        assert_eq!(engine.reduction_count(), 22);
    }

    #[test]
    fn upstream_rule_table_syllogism_is_present() {
        let mut language = Narsese::new();
        let engine = RuleEngine::ona(&mut language).unwrap();
        let cat_animal = language.parse_term("<cat --> animal>").unwrap();
        let animal_being = language.parse_term("<animal --> being>").unwrap();
        let expected = language.parse_term("<cat --> being>").unwrap();
        let results = engine.apply(
            &language,
            cat_animal,
            Some(animal_being),
            Truth::new(1.0, 0.9),
            Truth::new(1.0, 0.9),
        );
        assert!(results.iter().any(|result| result.term == expected));
    }

    #[test]
    fn unary_relational_image_rule_is_present() {
        let mut language = Narsese::new();
        let engine = RuleEngine::ona(&mut language).unwrap();
        let relation = language
            .parse_term("<({shape1} * {shape2}) --> leftOf>")
            .unwrap();
        let expected = language
            .parse_term("<{shape1} --> (leftOf /1 {shape2})>")
            .unwrap();
        let results = engine.apply(
            &language,
            relation,
            None,
            Truth::new(1.0, 0.9),
            Truth::default(),
        );
        let formatted = results
            .iter()
            .map(|result| language.format_term(&result.term))
            .collect::<Vec<_>>();
        assert!(
            results.iter().any(|result| result.term == expected),
            "{formatted:?}"
        );
    }

    #[test]
    fn shared_predicate_rule_compares_relational_products() {
        let mut language = Narsese::new();
        let engine = RuleEngine::ona(&mut language).unwrap();
        let first = language
            .parse_term("<({Gary} * {Helen}) --> friend>")
            .unwrap();
        let second = language
            .parse_term("<([smokes] * {Frank}) --> friend>")
            .unwrap();
        let expected = language
            .parse_term("<({Gary} * {Helen}) --> ([smokes] * {Frank})>")
            .unwrap();
        let results = engine.apply(
            &language,
            first,
            Some(second),
            Truth::new(1.0, 0.9),
            Truth::new(1.0, 0.455_011),
        );
        let comparison = results
            .iter()
            .find(|result| result.term == expected)
            .copied()
            .expect("relational product comparison is present");
        let gary_smokes = language.parse_term("<{Gary} --> [smokes]>").unwrap();
        let decomposed = engine.apply(
            &language,
            comparison.term,
            None,
            comparison.truth,
            Truth::default(),
        );
        assert!(decomposed.iter().any(|result| result.term == gary_smokes));
    }

    #[test]
    fn relational_product_analogy_uses_revised_component_truth() {
        let mut language = Narsese::new();
        let engine = RuleEngine::ona(&mut language).unwrap();
        let relation = language
            .parse_term("<({Gary} * {Helen}) --> ({Edward} * {Frank})>")
            .unwrap();
        let property = language.parse_term("<{Edward} --> [smokes]>").unwrap();
        let expected = language
            .parse_term("<({Gary} * {Helen}) --> ([smokes] * {Frank})>")
            .unwrap();
        let results = engine.apply(
            &language,
            relation,
            Some(property),
            Truth::new(1.0, 0.447_514),
            Truth::new(1.0, 0.921_198),
        );
        let answer = results
            .iter()
            .find(|result| result.term == expected)
            .expect("ONA product analogy rule is present");
        assert!((answer.truth.confidence - 0.412_249).abs() < 1.0e-6);
    }

    #[test]
    fn reductions_match_ona_set_and_idempotence_rules() {
        let mut language = Narsese::new();
        let engine = RuleEngine::ona(&mut language).unwrap();
        let repeated = language.parse_term("<(a & a) --> property>").unwrap();
        assert_eq!(
            language.format_term(&engine.reduce(&language, repeated)),
            "<a --> property>"
        );
    }
}
