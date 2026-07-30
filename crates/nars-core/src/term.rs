//! ONA-compatible fixed-size binary-heap term encoding.

use crate::config::COMPOUND_TERM_SIZE_MAX;

pub type Atom = u8;
pub const ATOM_FREE: Atom = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Term {
    atoms: [Atom; COMPOUND_TERM_SIZE_MAX],
}

impl Default for Term {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl Term {
    pub const EMPTY: Self = Self {
        atoms: [ATOM_FREE; COMPOUND_TERM_SIZE_MAX],
    };

    #[must_use]
    pub const fn from_atom(atom: Atom) -> Self {
        let mut atoms = [ATOM_FREE; COMPOUND_TERM_SIZE_MAX];
        atoms[0] = atom;
        Self { atoms }
    }

    #[must_use]
    pub const fn atoms(&self) -> &[Atom; COMPOUND_TERM_SIZE_MAX] {
        &self.atoms
    }

    pub(crate) const fn atoms_mut(&mut self) -> &mut [Atom; COMPOUND_TERM_SIZE_MAX] {
        &mut self.atoms
    }

    #[must_use]
    pub const fn root(&self) -> Atom {
        self.atoms[0]
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.root() == ATOM_FREE
    }

    #[must_use]
    pub fn complexity(&self) -> usize {
        self.atoms.iter().filter(|atom| **atom != ATOM_FREE).count()
    }

    #[must_use]
    pub fn has_atom(&self, atom: Atom) -> bool {
        self.atoms.contains(&atom)
    }

    /// ONA's deterministic word-wise hash over the 64-byte atom array.
    #[must_use]
    pub fn ona_hash(&self) -> u64 {
        self.atoms.chunks_exact(8).fold(0_u64, |mut hash, bytes| {
            let mut word_bytes = [0_u8; 8];
            word_bytes.copy_from_slice(bytes);
            let word = u64::from_ne_bytes(word_bytes);
            hash ^= word;
            hash = hash.wrapping_add(!(hash << 15));
            hash ^= hash >> 10;
            hash = hash.wrapping_add(hash << 3);
            hash ^= hash >> 6;
            hash = hash.wrapping_add(!(hash << 11));
            hash ^= hash >> 16;
            hash
        })
    }

    /// Overwrites the binary-heap subtree rooted at `index` (zero based).
    ///
    /// This intentionally preserves ONA's behavior: positions absent in the
    /// replacement are not proactively cleared.
    pub fn override_subterm(&mut self, index: usize, subterm: &Self) -> bool {
        fn copy_relative(
            target: &mut Term,
            target_index: usize,
            source: &Term,
            source_index: usize,
        ) -> bool {
            if target_index >= COMPOUND_TERM_SIZE_MAX {
                return false;
            }
            if source_index >= COMPOUND_TERM_SIZE_MAX {
                return true;
            }
            target.atoms[target_index] = source.atoms[source_index];

            let source_left = (source_index + 1) * 2 - 1;
            if source_left < COMPOUND_TERM_SIZE_MAX && source.atoms[source_left] != ATOM_FREE {
                let target_left = (target_index + 1) * 2 - 1;
                if !copy_relative(target, target_left, source, source_left) {
                    return false;
                }
            }
            let source_right = (source_index + 1) * 2;
            if source_right < COMPOUND_TERM_SIZE_MAX && source.atoms[source_right] != ATOM_FREE {
                let target_right = (target_index + 1) * 2;
                if !copy_relative(target, target_right, source, source_right) {
                    return false;
                }
            }
            true
        }

        copy_relative(self, index, subterm, 0)
    }

    #[must_use]
    pub fn extract_subterm(&self, index: usize) -> Self {
        let mut output = Self::EMPTY;
        let _ = output.override_from_relative(0, self, index);
        output
    }

    fn override_from_relative(
        &mut self,
        target_index: usize,
        source: &Self,
        source_index: usize,
    ) -> bool {
        if target_index >= COMPOUND_TERM_SIZE_MAX {
            return false;
        }
        if source_index >= COMPOUND_TERM_SIZE_MAX {
            return true;
        }
        self.atoms[target_index] = source.atoms[source_index];
        let source_left = (source_index + 1) * 2 - 1;
        if source_left < COMPOUND_TERM_SIZE_MAX && source.atoms[source_left] != ATOM_FREE {
            let target_left = (target_index + 1) * 2 - 1;
            if !self.override_from_relative(target_left, source, source_left) {
                return false;
            }
        }
        let source_right = (source_index + 1) * 2;
        if source_right < COMPOUND_TERM_SIZE_MAX && source.atoms[source_right] != ATOM_FREE {
            let target_right = (target_index + 1) * 2;
            if !self.override_from_relative(target_right, source, source_right) {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::Term;

    #[test]
    fn subterm_round_trip_uses_binary_heap_positions() {
        let mut root = Term::from_atom(10);
        let mut child = Term::from_atom(20);
        child.atoms_mut()[1] = 21;
        child.atoms_mut()[2] = 22;
        assert!(root.override_subterm(1, &child));
        assert_eq!(root.atoms()[1], 20);
        assert_eq!(root.atoms()[3], 21);
        assert_eq!(root.atoms()[4], 22);
        assert_eq!(root.extract_subterm(1), child);
    }
}
