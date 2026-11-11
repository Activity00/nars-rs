//! Inverted atom-to-concept index for semantic candidate lookup.

use crate::config::{ATOMS_MAX, UNIFICATION_DEPTH};
use crate::{Atom, ConceptId, Narsese, Term};

#[derive(Clone, Debug)]
pub struct InvertedAtomIndex {
    chains: Box<[Vec<ConceptId>]>,
}

impl Default for InvertedAtomIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl InvertedAtomIndex {
    #[must_use]
    pub fn new() -> Self {
        Self {
            chains: (0..=ATOMS_MAX).map(|_| Vec::new()).collect(),
        }
    }

    pub fn add(&mut self, language: &Narsese, term: &Term, concept: ConceptId) {
        for atom in term.atoms().iter().copied().take(UNIFICATION_DEPTH) {
            if !language.is_simple_atom(atom) {
                continue;
            }
            let chain = &mut self.chains[usize::from(atom)];
            if !chain.contains(&concept) {
                chain.push(concept);
            }
        }
    }

    pub fn remove(&mut self, language: &Narsese, term: &Term, concept: ConceptId) {
        for atom in term.atoms().iter().copied().take(UNIFICATION_DEPTH) {
            if language.is_simple_atom(atom) {
                self.chains[usize::from(atom)].retain(|candidate| *candidate != concept);
            }
        }
    }

    #[must_use]
    pub fn concepts(&self, atom: Atom) -> &[ConceptId] {
        self.chains
            .get(usize::from(atom))
            .map_or(&[], Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::InvertedAtomIndex;
    use crate::{ConceptId, Narsese};

    #[test]
    fn upstream_add_remove_collision_scenario() {
        let mut language = Narsese::new();
        let first = language.parse_term("<a --> (b & c)>").unwrap();
        let second = language.parse_term("<b --> d>").unwrap();
        let a = language.atomic_term("a").unwrap().root();
        let b = language.atomic_term("b").unwrap().root();
        let mut index = InvertedAtomIndex::new();
        index.add(&language, &first, ConceptId(1));
        index.add(&language, &second, ConceptId(2));
        assert_eq!(index.concepts(a), &[ConceptId(1)]);
        assert_eq!(index.concepts(b), &[ConceptId(1), ConceptId(2)]);
        index.remove(&language, &first, ConceptId(1));
        assert!(index.concepts(a).is_empty());
        assert_eq!(index.concepts(b), &[ConceptId(2)]);
    }
}
