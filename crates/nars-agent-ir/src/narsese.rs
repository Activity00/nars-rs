//! Narsese-safe atom generation.
//!
//! NARS terms are interned atoms; arbitrary user text, file paths, and model
//! output must never be interpolated directly into a term. This module converts
//! untrusted strings into a safe, bounded, stable atom alphabet and keeps the
//! original text in the event log instead.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

/// Maximum atom length used by the runtime.
pub const MAX_ATOM_LEN: usize = 48;

/// Converts arbitrary text into a Narsese-safe atom.
///
/// The mapping is stable for a given input: ASCII alphanumerics are lowercased,
/// every other character becomes `_`, runs are collapsed, the result is
/// truncated to [`MAX_ATOM_LEN`], and a leading digit is prefixed with `n_`.
#[must_use]
pub fn atomize(input: &str) -> String {
    let mut output = String::with_capacity(MAX_ATOM_LEN + 2);
    let mut last_underscore = false;
    for character in input.chars() {
        let mapped = if character.is_ascii_alphanumeric() {
            last_underscore = false;
            character.to_ascii_lowercase()
        } else {
            if last_underscore {
                continue;
            }
            last_underscore = true;
            '_'
        };
        if output.len() >= MAX_ATOM_LEN {
            break;
        }
        output.push(mapped);
    }
    let trimmed = output.trim_matches('_');
    if trimmed.is_empty() {
        return "nil".to_owned();
    }
    let mut result = trimmed.to_owned();
    if result.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        result.insert_str(0, "n_");
        result.truncate(MAX_ATOM_LEN);
    }
    result
}

/// Builds a short stable hash suffix used to disambiguate otherwise equal atoms.
#[must_use]
pub fn short_hash(input: &str) -> String {
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:08x}", hasher.finish() as u32)
}

/// Builds an atom that combines a human-readable prefix and a stable hash.
#[must_use]
pub fn keyed_atom(prefix: &str, key: &str) -> String {
    format!("{}_{}", atomize(prefix), short_hash(key))
}

/// A term together with its truth value, ready to be committed to NARS.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NarsTerm {
    /// The Narsese term.
    pub term: String,
    /// Optional debugging label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomize_maps_paths_and_text() {
        assert_eq!(
            atomize("crates/nars-core/src/reasoner.rs"),
            "crates_nars_core_src_reasoner_rs"
        );
        assert_eq!(atomize("Hello, World!"), "hello_world");
        assert_eq!(atomize("___"), "nil");
        assert_eq!(atomize(""), "nil");
        assert_eq!(atomize("42 things"), "n_42_things");
    }

    #[test]
    fn atomize_is_bounded_and_stable() {
        let long = "a".repeat(500);
        let atom = atomize(&long);
        assert!(atom.len() <= MAX_ATOM_LEN);
        assert_eq!(atom, atomize(&long));
    }

    #[test]
    fn keyed_atom_is_short_and_unique_enough() {
        let left = keyed_atom("cand", "read_file:{}");
        let right = keyed_atom("cand", "search:{}");
        assert_ne!(left, right);
        assert!(left.starts_with("cand_"));
        assert!(left.len() <= "cand_".len() + 8);
    }
}
