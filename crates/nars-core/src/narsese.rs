//! Narsese atom interning, parsing, normalization, and printing.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use crate::config::{
    ATOMIC_TERM_LEN_MAX, ATOMS_MAX, COMPOUND_TERM_SIZE_MAX, NAR_DEFAULT_CONFIDENCE,
    NAR_DEFAULT_FREQUENCY,
};
use crate::memory_snapshot::{LanguageRecord, ValueRecord};
use crate::{Atom, Term, Truth};

const RULE_TABLE_VARS: &str = "ABCMRSPXYZ";
const CANONICAL_COPULAS: &str = "@*&|;:=$'\"/\\.-%#~+!?^_,";
const SET_TERMINATOR: char = '@';

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NarseseError(String);

impl NarseseError {
    pub(crate) fn from_message(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    fn new(message: impl Into<String>) -> Self {
        Self::from_message(message)
    }
}

impl fmt::Display for NarseseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for NarseseError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Punctuation {
    Belief,
    Goal,
    Question,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tense {
    #[default]
    Eternal,
    Present,
    Past,
    Future,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sentence {
    pub term: Term,
    pub punctuation: Punctuation,
    pub tense: Tense,
    pub truth: Truth,
    pub occurrence_time_offset: f64,
}

#[derive(Clone, Debug)]
pub struct Narsese {
    atom_names: Vec<String>,
    atom_ids: HashMap<String, Atom>,
    atom_values: Vec<Option<AtomValue>>,
    self_atom: Atom,
}

#[derive(Clone, Debug, PartialEq)]
struct AtomValue {
    value: f64,
    measurement: String,
}

impl Default for Narsese {
    fn default() -> Self {
        Self::new()
    }
}

impl Narsese {
    /// Creates a parser with the exact built-in atom registration order used
    /// by ONA.
    ///
    /// # Panics
    ///
    /// Panics only if the fixed ONA built-in vocabulary no longer fits the
    /// configured atom table.
    #[must_use]
    pub fn new() -> Self {
        let mut this = Self {
            atom_names: Vec::with_capacity(ATOMS_MAX),
            atom_ids: HashMap::with_capacity(ATOMS_MAX),
            atom_values: Vec::with_capacity(ATOMS_MAX),
            self_atom: 0,
        };
        for index in 1..=9 {
            for prefix in ['#', '$', '?'] {
                this.intern(&format!("{prefix}{index}"))
                    .expect("built-in variables fit atom table");
            }
        }
        for name in RULE_TABLE_VARS.chars() {
            this.intern(&name.to_string())
                .expect("rule variables fit atom table");
        }
        this.intern("Op1").expect("built-ins fit atom table");
        this.intern("Op2").expect("built-ins fit atom table");
        for copula in CANONICAL_COPULAS.chars() {
            this.intern(&copula.to_string())
                .expect("copulas fit atom table");
        }
        this.self_atom = this.intern("SELF").expect("SELF fits atom table");
        this
    }

    #[must_use]
    pub fn atom_count(&self) -> usize {
        self.atom_names.len()
    }

    #[must_use]
    pub const fn self_atom(&self) -> Atom {
        self.self_atom
    }

    /// Interns an atomic term and returns its ONA-compatible one-based ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the fixed atom table is full.
    pub fn intern(&mut self, name: &str) -> Result<Atom, NarseseError> {
        let name = name
            .chars()
            .take(ATOMIC_TERM_LEN_MAX - 1)
            .collect::<String>();
        if let Some(atom) = self.atom_ids.get(&name) {
            return Ok(*atom);
        }
        if self.atom_names.len() >= ATOMS_MAX {
            return Err(NarseseError::new("too many atomic terms"));
        }
        let atom = u16::try_from(self.atom_names.len() + 1)
            .map_err(|_| NarseseError::new("atom index overflow"))?;
        self.atom_names.push(name.clone());
        self.atom_values.push(None);
        self.atom_ids.insert(name, atom);
        Ok(atom)
    }

    #[must_use]
    pub fn atom_name(&self, atom: Atom) -> Option<&str> {
        atom.checked_sub(1)
            .and_then(|index| self.atom_names.get(usize::from(index)))
            .map(String::as_str)
    }

    /// Captures the atom table so stored terms keep their meaning across
    /// processes. Term atoms are numeric ids, not names.
    #[must_use]
    pub fn to_language_record(&self) -> LanguageRecord {
        LanguageRecord {
            atoms: self.atom_names.clone(),
            values: self
                .atom_values
                .iter()
                .map(|value| {
                    value.as_ref().map(|value| ValueRecord {
                        value: value.value,
                        measurement: value.measurement.clone(),
                    })
                })
                .collect(),
            self_atom: self.self_atom,
        }
    }

    /// Rebuilds a parser from a stored atom table.
    ///
    /// # Errors
    ///
    /// Returns an error when the table is oversized, inconsistent, or contains
    /// duplicate or empty names.
    pub fn from_language_record(record: &LanguageRecord) -> Result<Self, NarseseError> {
        if record.atoms.len() > ATOMS_MAX {
            return Err(NarseseError::new("stored atom table exceeds ATOMS_MAX"));
        }
        if record.values.len() > record.atoms.len() {
            return Err(NarseseError::new(
                "stored atom values outnumber stored atoms",
            ));
        }
        let mut this = Self {
            atom_names: Vec::with_capacity(ATOMS_MAX),
            atom_ids: HashMap::with_capacity(ATOMS_MAX),
            atom_values: Vec::with_capacity(ATOMS_MAX),
            self_atom: record.self_atom,
        };
        for (index, name) in record.atoms.iter().enumerate() {
            if name.is_empty() {
                return Err(NarseseError::new("stored atom name is empty"));
            }
            let atom = u16::try_from(index + 1)
                .map_err(|_| NarseseError::new("stored atom index overflow"))?;
            if this.atom_ids.insert(name.clone(), atom).is_some() {
                return Err(NarseseError::new(format!("duplicate stored atom `{name}`")));
            }
            this.atom_names.push(name.clone());
            this.atom_values.push(
                record
                    .values
                    .get(index)
                    .and_then(Option::as_ref)
                    .map(|value| AtomValue {
                        value: value.value,
                        measurement: value.measurement.clone(),
                    }),
            );
        }
        Ok(this)
    }

    /// Creates a single-atom term.
    ///
    /// # Errors
    ///
    /// Returns an error if the atom table is full.
    pub fn atomic_term(&mut self, name: &str) -> Result<Term, NarseseError> {
        Ok(Term::from_atom(self.intern(name)?))
    }

    /// Returns the interned ID for a canonical one-character copula.
    ///
    /// # Errors
    ///
    /// Returns an error only if the atom table is exhausted.
    pub fn copula_atom(&mut self, copula: char) -> Result<Atom, NarseseError> {
        self.intern(&copula.to_string())
    }

    /// Constructs the binary sequence `(left &/ right)`.
    ///
    /// # Errors
    ///
    /// Returns an error if either subterm exceeds the fixed term encoding.
    pub fn sequence(&mut self, left: &Term, right: &Term) -> Result<Term, NarseseError> {
        let mut output = Term::from_atom(self.copula_atom('+')?);
        if output.override_subterm(1, left) && output.override_subterm(2, right) {
            Ok(output)
        } else {
            Err(NarseseError::new("sequence exceeds compound term capacity"))
        }
    }

    /// Parses and variable-normalizes a Narsese term.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed input, atom exhaustion, or a term that
    /// exceeds ONA's fixed compound-term capacity.
    pub fn parse_term(&mut self, input: &str) -> Result<Term, NarseseError> {
        let canonical = canonicalize(input)?;
        let mut tokens = tokenize(&canonical);
        prefix_transform(&mut tokens);
        if tokens.is_empty() {
            return Err(NarseseError::new("empty Narsese term"));
        }
        let mut term = Term::EMPTY;
        self.build_binary_tree(&mut term, &tokens, 0, 1)?;
        self.normalize_variables(&mut term)?;
        Ok(term)
    }

    /// Parses punctuation, tense, truth, optional time offset, and the term.
    ///
    /// # Errors
    ///
    /// Returns an error when any sentence component is malformed.
    pub fn parse_sentence(&mut self, input: &str) -> Result<Sentence, NarseseError> {
        let mut source = input.trim();
        let mut occurrence_time_offset = 0.0;
        if let Some(rest) = source.strip_prefix("dt=") {
            let (offset, sentence) = rest
                .split_once(char::is_whitespace)
                .ok_or_else(|| NarseseError::new("dt= requires a following sentence"))?;
            occurrence_time_offset = offset
                .parse()
                .map_err(|_| NarseseError::new("invalid dt offset"))?;
            source = sentence.trim_start();
        }

        let mut truth = Truth::new(NAR_DEFAULT_FREQUENCY, NAR_DEFAULT_CONFIDENCE);
        let mut body = source.trim_end();
        if body.ends_with('}') {
            let opening = body
                .rfind('{')
                .ok_or_else(|| NarseseError::new("truth value opener not found"))?;
            let fields = body[opening + 1..body.len() - 1]
                .split_whitespace()
                .collect::<Vec<_>>();
            if fields.len() != 2 {
                return Err(NarseseError::new(
                    "truth value must contain frequency and confidence",
                ));
            }
            truth = Truth::new(
                fields[0]
                    .parse()
                    .map_err(|_| NarseseError::new("invalid truth frequency"))?,
                fields[1]
                    .parse()
                    .map_err(|_| NarseseError::new("invalid truth confidence"))?,
            );
            body = body[..opening].trim_end();
        } else if body.ends_with('%') {
            let opening = body[..body.len() - 1]
                .rfind('%')
                .ok_or_else(|| NarseseError::new("old-format truth opener not found"))?;
            let fields = &body[opening + 1..body.len() - 1];
            let (frequency, confidence) = fields.split_once(';').map_or_else(
                || {
                    Ok((
                        fields
                            .parse()
                            .map_err(|_| NarseseError::new("invalid truth frequency"))?,
                        NAR_DEFAULT_CONFIDENCE,
                    ))
                },
                |(frequency, confidence)| {
                    Ok::<_, NarseseError>((
                        frequency
                            .parse()
                            .map_err(|_| NarseseError::new("invalid truth frequency"))?,
                        confidence
                            .parse()
                            .map_err(|_| NarseseError::new("invalid truth confidence"))?,
                    ))
                },
            )?;
            truth = Truth::new(frequency, confidence);
            body = body[..opening].trim_end();
        }

        let (body, tense) = if let Some(body) = body.strip_suffix(":|:") {
            (body.trim_end(), Tense::Present)
        } else if let Some(body) = body.strip_suffix(":\\:") {
            (body.trim_end(), Tense::Past)
        } else if let Some(body) = body.strip_suffix(":/:") {
            (body.trim_end(), Tense::Future)
        } else {
            (body, Tense::Eternal)
        };

        let punctuation_char = body
            .chars()
            .next_back()
            .ok_or_else(|| NarseseError::new("sentence is missing punctuation"))?;
        let punctuation = match punctuation_char {
            '.' => Punctuation::Belief,
            '!' => Punctuation::Goal,
            '?' => Punctuation::Question,
            _ => return Err(NarseseError::new("punctuation must be '.', '!' or '?'")),
        };
        let term_source = body[..body.len() - punctuation_char.len_utf8()].trim_end();

        Ok(Sentence {
            term: self.parse_term(term_source)?,
            punctuation,
            tense,
            truth,
            occurrence_time_offset,
        })
    }

    fn build_binary_tree(
        &mut self,
        term: &mut Term,
        tokens: &[String],
        token_index: usize,
        tree_index: usize,
    ) -> Result<(), NarseseError> {
        if tree_index == 0 || tree_index > COMPOUND_TERM_SIZE_MAX {
            return Err(NarseseError::new(
                "compound term exceeds fixed term capacity",
            ));
        }
        let token = tokens
            .get(token_index)
            .ok_or_else(|| NarseseError::new("unexpected end of Narsese term"))?;
        if token == "(" {
            let copula_index = token_index + 1;
            let first_index = token_index + 2;
            let second_index = skip_compound(tokens, first_index);
            let copula = tokens
                .get(copula_index)
                .ok_or_else(|| NarseseError::new("compound term is missing a copula"))?;
            term.atoms_mut()[tree_index - 1] = self.intern(copula)?;
            if first_index < tokens.len() {
                self.build_binary_tree(term, tokens, first_index, tree_index * 2)?;
            }
            if second_index < tokens.len() {
                self.build_binary_tree(term, tokens, second_index, tree_index * 2 + 1)?;
            }
        } else if token == ")" {
            term.atoms_mut()[tree_index - 1] = self.intern(&SET_TERMINATOR.to_string())?;
        } else {
            term.atoms_mut()[tree_index - 1] = self.intern(token)?;
        }
        Ok(())
    }

    fn normalize_variables(&mut self, term: &mut Term) -> Result<(), NarseseError> {
        let mut next = [1_u8; 3];
        let mut normalized = [false; COMPOUND_TERM_SIZE_MAX];
        for index in 0..COMPOUND_TERM_SIZE_MAX {
            let atom = term.atoms()[index];
            let Some(name) = self.atom_name(atom) else {
                continue;
            };
            let Some(kind) = variable_kind(name) else {
                continue;
            };
            if normalized[index] {
                continue;
            }
            if next[kind] > 9 {
                return Err(NarseseError::new("variable normalization overflow"));
            }
            let prefix = ['$', '#', '?'][kind];
            let normalized_atom = self.intern(&format!("{prefix}{}", next[kind]))?;
            next[kind] += 1;
            for (later, is_normalized) in normalized.iter_mut().enumerate().skip(index) {
                if term.atoms()[later] == atom {
                    term.atoms_mut()[later] = normalized_atom;
                    *is_normalized = true;
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn format_term(&self, term: &Term) -> String {
        let mut output = String::new();
        self.format_recursive(term, 1, &mut output);
        output
    }

    fn format_recursive(&self, term: &Term, index: usize, output: &mut String) {
        if index == 0 || index > COMPOUND_TERM_SIZE_MAX {
            return;
        }
        let atom = term.atoms()[index - 1];
        if atom == 0 {
            return;
        }
        let child1 = index * 2;
        let child2 = index * 2 + 1;
        let has_left = child1 <= COMPOUND_TERM_SIZE_MAX && term.atoms()[child1 - 1] != 0;
        let has_right = child2 <= COMPOUND_TERM_SIZE_MAX
            && term.atoms()[child2 - 1] != 0
            && !self.copula_equals(term.atoms()[child2 - 1], SET_TERMINATOR);
        let singular_product = self.copula_equals(atom, '*') && !has_right;
        let negation = self.copula_equals(atom, '!');
        let frequency_greater = self.copula_equals(atom, '+') && !has_right;
        let frequency_equal = self.copula_equals(atom, '=') && !has_right;
        let ext_set = self.copula_equals(atom, '"');
        let int_set = self.copula_equals(atom, '\'');
        let statement = !frequency_equal
            && [':', '$', '=', '?', '^', ',']
                .into_iter()
                .any(|copula| self.copula_equals(atom, copula));

        if ext_set {
            if has_left {
                output.push('{');
            }
        } else if int_set {
            if has_left {
                output.push('[');
            }
        } else if statement {
            if has_left {
                output.push('<');
            }
        } else {
            if has_left {
                output.push('(');
            }
            if negation || singular_product || frequency_greater || frequency_equal {
                if frequency_greater {
                    output.push('+');
                } else if frequency_equal {
                    output.push('=');
                } else {
                    self.format_atom(atom, output);
                }
                output.push(' ');
            }
        }

        if child1 <= COMPOUND_TERM_SIZE_MAX {
            self.format_recursive(term, child1, output);
        }
        if has_right && has_left {
            output.push(' ');
        }
        if !ext_set
            && !int_set
            && !self.copula_equals(atom, SET_TERMINATOR)
            && !negation
            && !singular_product
            && !frequency_equal
            && !frequency_greater
        {
            self.format_atom(atom, output);
            if has_left {
                output.push(' ');
            }
        }
        if child2 <= COMPOUND_TERM_SIZE_MAX {
            self.format_recursive(term, child2, output);
        }
        if ext_set {
            if has_left {
                output.push('}');
            }
        } else if int_set {
            if has_left {
                output.push(']');
            }
        } else if statement {
            if has_left {
                output.push('>');
            }
        } else if has_left {
            output.push(')');
        }
    }

    fn format_atom(&self, atom: Atom, output: &mut String) {
        let Some(name) = self.atom_name(atom) else {
            output.push('@');
            return;
        };
        output.push_str(match name {
            ":" => "-->",
            "$" => "=/>",
            "^" => "<=>",
            "_" => "||",
            "+" => "&/",
            "," => "|->",
            "?" => "==>",
            ";" => "&&",
            "=" => "<->",
            "/" => "/1",
            "%" => "/2",
            "\\" => "\\1",
            "#" => "\\2",
            other => other,
        });
    }

    #[must_use]
    pub fn copula_equals(&self, atom: Atom, copula: char) -> bool {
        self.atom_name(atom).is_some_and(|name| {
            let mut chars = name.chars();
            chars.next() == Some(copula) && chars.next().is_none()
        })
    }

    #[must_use]
    pub fn is_continuous_property_statement(&self, term: &Term) -> bool {
        self.copula_equals(term.root(), ',') && !self.copula_equals(term.atoms()[1], '*')
    }

    #[must_use]
    pub fn is_operator(&self, atom: Atom) -> bool {
        self.atom_name(atom)
            .is_some_and(|name| name.starts_with('^') && name.len() > 1)
    }

    #[must_use]
    pub fn is_rule_meta_variable(&self, atom: Atom) -> bool {
        self.atom_name(atom)
            .is_some_and(|name| name.as_bytes().first().is_some_and(u8::is_ascii_uppercase))
    }

    #[must_use]
    pub fn is_operation(&self, term: &Term) -> bool {
        self.is_operator(term.root())
            || (self.copula_equals(term.root(), ':')
                && self.copula_equals(term.atoms()[1], '*')
                && self.is_operator(term.atoms()[2])
                && self.copula_equals(term.atoms()[3], '"'))
    }

    #[must_use]
    pub fn is_executable_operation(&self, term: &Term) -> bool {
        self.is_operation(term)
            && (self.is_operator(term.root())
                || term.atoms()[7] == self.self_atom
                || self.is_variable(term.atoms()[7]))
    }

    #[must_use]
    pub fn operation_atom(&self, term: &Term) -> Option<Atom> {
        if self.copula_equals(term.root(), '+') {
            let potential = term.extract_subterm(2);
            if self.copula_equals(potential.root(), '+') {
                return None;
            }
            return self.operation_atom(&potential);
        }
        if self.is_operator(term.root()) {
            Some(term.root())
        } else if self.is_operation(term) {
            Some(term.atoms()[2])
        } else {
            None
        }
    }

    /// Returns the concrete rightmost operation from a procedure precondition.
    #[must_use]
    pub fn operation_term(&self, term: &Term) -> Option<Term> {
        if self.copula_equals(term.root(), '$') {
            return self.operation_term(&term.extract_subterm(1));
        }
        if self.copula_equals(term.root(), '+') {
            let potential = term.extract_subterm(2);
            if self.is_operation(&potential) {
                return Some(potential);
            }
            return self.operation_term(&term.extract_subterm(1));
        }
        self.is_operation(term).then_some(*term)
    }

    /// Returns ONA's callback argument product, for example
    /// `({SELF} * distance)`, or an empty term for an atomic operation.
    #[must_use]
    pub fn operation_arguments(&self, term: &Term) -> Term {
        self.operation_term(term)
            .filter(|operation| !self.is_operator(operation.root()))
            .map_or_else(Term::default, |operation| operation.extract_subterm(1))
    }

    /// Builds `<({SELF} * argument) --> operator>` for motor babbling.
    ///
    /// # Errors
    ///
    /// Returns an error if `operator` is not atomic or the compound cannot be
    /// represented by ONA's fixed-size term.
    pub fn operation_with_argument(
        &mut self,
        operator: &Term,
        argument: &Term,
    ) -> Result<Term, NarseseError> {
        if !self.is_operator(operator.root()) {
            return Err(NarseseError::new(
                "operation term requires an atomic operator",
            ));
        }
        let source = format!(
            "<({{SELF}} * {}) --> {}>",
            self.format_term(argument),
            self.format_term(operator)
        );
        self.parse_term(&source)
    }

    #[must_use]
    pub fn precondition_without_operation(&self, precondition: &Term) -> Term {
        if self.copula_equals(precondition.root(), '+') {
            let potential_operation = precondition.extract_subterm(2);
            if self.is_operation(&potential_operation) {
                return self.precondition_without_operation(&precondition.extract_subterm(1));
            }
        }
        *precondition
    }

    #[must_use]
    pub fn sequence_length(&self, sequence: &Term) -> usize {
        if self.copula_equals(sequence.root(), '+') {
            self.sequence_length(&sequence.extract_subterm(1))
                + self.sequence_length(&sequence.extract_subterm(2))
        } else {
            1
        }
    }

    #[must_use]
    pub fn is_simple_atom(&self, atom: Atom) -> bool {
        self.atom_name(atom).is_some_and(|name| {
            name.as_bytes()
                .first()
                .is_some_and(|first| *first == b'^' || first.is_ascii_alphanumeric())
        })
    }

    #[must_use]
    pub fn has_operation(&self, term: &Term) -> bool {
        self.operation_atom(term).is_some()
            || (self.copula_equals(term.root(), '+')
                && term
                    .atoms()
                    .iter()
                    .copied()
                    .any(|atom| self.is_operator(atom)))
    }

    /// Collects operations from a sequence into a left-nested operation-only
    /// sequence, matching ONA's helper used during variable introduction.
    ///
    /// # Errors
    ///
    /// Returns an error if the collected sequence exceeds term capacity.
    pub fn operation_sequence(&mut self, sequence: &Term) -> Result<Option<Term>, NarseseError> {
        fn append(
            language: &mut Narsese,
            output: &mut Option<Term>,
            current: &Term,
        ) -> Result<(), NarseseError> {
            if language.copula_equals(current.root(), '+') {
                append(language, output, &current.extract_subterm(1))?;
                append(language, output, &current.extract_subterm(2))?;
            } else if language.is_operation(current) {
                *output = Some(match output {
                    Some(existing) => language.sequence(existing, current)?,
                    None => *current,
                });
            }
            Ok(())
        }

        let mut output = None;
        append(self, &mut output, sequence)?;
        Ok(output)
    }

    #[must_use]
    pub fn is_variable(&self, atom: Atom) -> bool {
        self.atom_name(atom).and_then(variable_kind).is_some()
    }

    #[must_use]
    pub fn is_independent_variable(&self, atom: Atom) -> bool {
        self.atom_name(atom).and_then(variable_kind) == Some(0)
    }

    #[must_use]
    pub fn is_dependent_variable(&self, atom: Atom) -> bool {
        self.atom_name(atom).and_then(variable_kind) == Some(1)
    }

    #[must_use]
    pub fn is_query_variable(&self, atom: Atom) -> bool {
        self.atom_name(atom).and_then(variable_kind) == Some(2)
    }

    #[must_use]
    pub fn has_variable(
        &self,
        term: &Term,
        independent: bool,
        dependent: bool,
        query: bool,
    ) -> bool {
        term.atoms().iter().copied().any(|atom| {
            self.atom_name(atom)
                .and_then(variable_kind)
                .is_some_and(|kind| {
                    (kind == 0 && independent) || (kind == 1 && dependent) || (kind == 2 && query)
                })
        })
    }

    /// Associates a numeric value and measurement family with an atom.
    ///
    /// # Errors
    ///
    /// Returns an error if the atom does not exist.
    pub fn set_atom_value(
        &mut self,
        atom: Atom,
        value: f64,
        measurement: impl Into<String>,
    ) -> Result<(), NarseseError> {
        let index = atom
            .checked_sub(1)
            .map(usize::from)
            .filter(|index| *index < self.atom_values.len())
            .ok_or_else(|| NarseseError::new("cannot assign a value to an unknown atom"))?;
        self.atom_values[index] = Some(AtomValue {
            value,
            measurement: measurement.into(),
        });
        Ok(())
    }

    #[must_use]
    pub fn atom_value(&self, atom: Atom) -> Option<(f64, &str)> {
        atom.checked_sub(1)
            .map(usize::from)
            .and_then(|index| self.atom_values.get(index))
            .and_then(Option::as_ref)
            .map(|value| (value.value, value.measurement.as_str()))
    }
}

fn variable_kind(name: &str) -> Option<usize> {
    (name.len() > 1).then(|| match name.as_bytes()[0] {
        b'$' => Some(0),
        b'#' => Some(1),
        b'?' => Some(2),
        _ => None,
    })?
}

fn tokenize(canonical: &str) -> Vec<String> {
    let mut expanded = String::with_capacity(canonical.len() * 2);
    for character in canonical.chars() {
        if matches!(character, '(' | ')') {
            expanded.push(' ');
            expanded.push(character);
            expanded.push(' ');
        } else {
            expanded.push(character);
        }
    }
    expanded.split_whitespace().map(str::to_owned).collect()
}

fn skip_compound(tokens: &[String], mut index: usize) -> usize {
    let mut parentheses = 0_i32;
    while index < tokens.len() {
        if tokens[index] == "(" {
            parentheses += 1;
        } else if tokens[index] == ")" {
            parentheses -= 1;
        }
        index += 1;
        if parentheses == 0 {
            return index;
        }
    }
    index
}

fn prefix_transform(tokens: &mut Vec<String>) {
    let mut index = 0;
    while index + 2 < tokens.len() {
        if tokens[index] == "("
            && !tokens[index + 1].chars().next().is_some_and(|character| {
                tokens[index + 1].len() == 1 && CANONICAL_COPULAS.contains(character)
            })
        {
            let copula_index = skip_compound(tokens, index + 1);
            if copula_index < tokens.len() {
                let copula = tokens.remove(copula_index);
                tokens.insert(index + 1, copula);
            }
        }
        index += 1;
    }
}

fn canonicalize(input: &str) -> Result<String, NarseseError> {
    if !input.is_ascii() {
        return Err(NarseseError::new(
            "ONA-compatible Narsese currently requires ASCII",
        ));
    }
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len() * 2);
    let mut index = 0;
    while index < bytes.len() {
        let rest = &input[index..];
        let replacement = [
            ("-->", ":"),
            ("|->", ","),
            ("<->", "="),
            ("=/>", "$"),
            ("==>", "?"),
            ("<=>", "^"),
            ("&/", "+"),
            ("&&", ";"),
            ("||", "_"),
            ("/1", "/"),
            ("/2", "%"),
            ("\\1", "\\"),
            ("\\2", "#"),
        ]
        .into_iter()
        .find(|(pattern, _)| rest.starts_with(pattern));
        if let Some((pattern, canonical)) = replacement {
            output.push_str(canonical);
            index += pattern.len();
            continue;
        }
        if rest.starts_with("--") && !rest.starts_with("-->") {
            output.push('!');
            index += 2;
            continue;
        }
        match bytes[index] as char {
            ',' => output.push(' '),
            '[' => output.push_str("(' "),
            '{' => output.push_str("(\" "),
            ']' | '}' | '>' => output.push(')'),
            '<' => output.push('('),
            character => output.push(character),
        }
        index += 1;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::{Narsese, Punctuation, Tense};

    #[test]
    fn upstream_narsese_test_round_trips() {
        let mut narsese = Narsese::new();
        let term = narsese
            .parse_term("<<$sth --> (&,[furry,meowing],animal)> =/> <$sth --> [good]>>")
            .unwrap();
        assert_eq!(
            narsese.format_term(&term),
            "<<$1 --> ([furry meowing] & animal)> =/> <$1 --> [good]>>"
        );
    }

    #[test]
    fn sentence_defaults_and_event_tense_match_ona() {
        let mut narsese = Narsese::new();
        let sentence = narsese.parse_sentence("a. :|:").unwrap();
        assert_eq!(sentence.punctuation, Punctuation::Belief);
        assert_eq!(sentence.tense, Tense::Present);
        assert_eq!(sentence.truth.frequency, 1.0);
        assert_eq!(sentence.truth.confidence, 0.9);
        assert_eq!(narsese.format_term(&sentence.term), "a");
    }

    #[test]
    fn sentence_parses_offset_and_truth() {
        let mut narsese = Narsese::new();
        let sentence = narsese
            .parse_sentence("dt=2.5 <a --> b>! {0.7 0.8}")
            .unwrap();
        assert_eq!(sentence.punctuation, Punctuation::Goal);
        assert_eq!(sentence.occurrence_time_offset, 2.5);
        assert_eq!(sentence.truth.frequency, 0.7);
        assert_eq!(sentence.truth.confidence, 0.8);
        assert_eq!(narsese.format_term(&sentence.term), "<a --> b>");
    }

    #[test]
    fn operation_arguments_round_trip_ona_product_shape() {
        let mut narsese = Narsese::new();
        let operator = narsese.parse_term("^move").unwrap();
        let distance = narsese.parse_term("distance").unwrap();
        let operation = narsese
            .operation_with_argument(&operator, &distance)
            .unwrap();
        assert_eq!(
            narsese.format_term(&operation),
            "<({SELF} * distance) --> ^move>"
        );
        assert_eq!(narsese.operation_atom(&operation), Some(operator.root()));
        assert_eq!(
            narsese.format_term(&narsese.operation_arguments(&operation)),
            "({SELF} * distance)"
        );
    }

    #[test]
    fn numeric_property_term_used_by_bandrobot_round_trips() {
        let mut narsese = Narsese::new();
        let term = narsese.parse_term("<{position} |-> [pickPosX]>").unwrap();
        assert_eq!(narsese.format_term(&term), "<{position} |-> [pickPosX]>");
    }
}
