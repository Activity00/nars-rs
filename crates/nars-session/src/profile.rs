//! Cross-workspace profile memory.
//!
//! Workspace memory is repo-specific. Some knowledge is not: how tools
//! behave, which plan stages tend to succeed, and preferences the user states
//! once. Those facts live here and are seeded into every workspace run as
//! prior beliefs, so a new repository starts from what the agent already
//! learned elsewhere.

use std::path::PathBuf;

use nars_agent_ir::NarsFact;
use serde::{Deserialize, Serialize};

use crate::store::StoreError;

/// Maximum number of facts kept in the profile.
pub const PROFILE_FACT_LIMIT: usize = 200;

/// Agent-level fact base shared by every workspace.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProfileMemory {
    /// Schema version.
    pub version: u32,
    /// Facts, ordered by insertion.
    #[serde(default)]
    pub facts: Vec<NarsFact>,
}

impl ProfileMemory {
    /// Merges facts, keeping the most confident version of each term.
    ///
    /// Later facts with the same term replace earlier ones only when they are
    /// at least as confident, and the list is clamped to `limit`.
    pub fn merge(&mut self, facts: impl IntoIterator<Item = NarsFact>, limit: usize) {
        for fact in facts {
            if let Some(existing) = self.facts.iter_mut().find(|entry| entry.term == fact.term) {
                if fact.truth.confidence >= existing.truth.confidence {
                    *existing = fact;
                }
            } else {
                self.facts.push(fact);
            }
        }
        self.facts
            .sort_by(|left, right| right.truth.confidence.total_cmp(&left.truth.confidence));
        self.facts.truncate(limit);
    }
}

/// Profile plus the generation it was saved as.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredProfile {
    /// Monotonic write counter.
    pub generation: u64,
    /// Profile contents.
    pub profile: ProfileMemory,
}

/// Persistence interface for the cross-workspace profile.
pub trait ProfileStore: Send + Sync {
    /// Loads the profile, or `None` when nothing was stored yet.
    ///
    /// # Errors
    ///
    /// Returns an error when the file exists but cannot be read or decoded.
    fn load(&self) -> Result<Option<StoredProfile>, StoreError>;
    /// Persists the profile when the generation still matches.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Conflict`] when another writer moved the
    /// generation, and I/O or JSON errors when the file cannot be written.
    fn save(
        &self,
        expected_generation: Option<u64>,
        profile: &ProfileMemory,
    ) -> Result<u64, StoreError>;
}

/// File-backed profile store.
#[derive(Debug)]
pub struct FileProfileStore {
    root: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct ProfileFile {
    version: u32,
    generation: u64,
    profile: ProfileMemory,
}

impl FileProfileStore {
    /// Opens or creates the directory that holds `profile.json`.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory cannot be created.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|source| StoreError::Io {
            path: root.display().to_string(),
            source,
        })?;
        Ok(Self { root })
    }

    fn path(&self) -> PathBuf {
        self.root.join("profile.json")
    }
}

impl ProfileStore for FileProfileStore {
    fn load(&self) -> Result<Option<StoredProfile>, StoreError> {
        let path = self.path();
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(StoreError::Io {
                    path: path.display().to_string(),
                    source,
                });
            }
        };
        let file: ProfileFile =
            serde_json::from_str(&content).map_err(|source| StoreError::Json {
                path: path.display().to_string(),
                source,
            })?;
        Ok(Some(StoredProfile {
            generation: file.generation,
            profile: file.profile,
        }))
    }

    fn save(
        &self,
        expected_generation: Option<u64>,
        profile: &ProfileMemory,
    ) -> Result<u64, StoreError> {
        let path = self.path();
        let current = self.load()?;
        let current_generation = current.as_ref().map(|stored| stored.generation);
        if current_generation != expected_generation {
            return Err(StoreError::Conflict(format!(
                "profile moved from generation {current_generation:?}"
            )));
        }
        let generation = current_generation.map_or(1, |generation| generation.saturating_add(1));
        let file = ProfileFile {
            version: 1,
            generation,
            profile: profile.clone(),
        };
        let temp = path.with_extension("json.tmp");
        let content = serde_json::to_vec_pretty(&file).map_err(|source| StoreError::Json {
            path: path.display().to_string(),
            source,
        })?;
        std::fs::write(&temp, content).map_err(|source| StoreError::Io {
            path: temp.display().to_string(),
            source,
        })?;
        std::fs::rename(&temp, &path).map_err(|source| StoreError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Ok(generation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nars_agent_ir::TruthValue;

    fn fact(term: &str, confidence: f64) -> NarsFact {
        NarsFact {
            term: term.to_owned(),
            truth: TruthValue {
                frequency: 1.0,
                confidence,
            },
            eternal: true,
        }
    }

    #[test]
    fn round_trips_and_guards_generations() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileProfileStore::new(dir.path()).unwrap();
        assert!(store.load().unwrap().is_none());

        let mut profile = ProfileMemory::default();
        profile.merge([fact("user_prefers_pnpm", 0.9)], PROFILE_FACT_LIMIT);
        assert_eq!(store.save(None, &profile).unwrap(), 1);

        let loaded = store.load().unwrap().expect("stored");
        assert_eq!(loaded.generation, 1);
        assert_eq!(loaded.profile.facts.len(), 1);
        assert!(matches!(
            store.save(None, &profile),
            Err(StoreError::Conflict(_))
        ));
        assert_eq!(store.save(Some(1), &profile).unwrap(), 2);
    }

    #[test]
    fn merge_keeps_the_most_confident_version() {
        let mut profile = ProfileMemory::default();
        profile.merge([fact("tool_read_file_observed_success", 0.5)], 10);
        profile.merge([fact("tool_read_file_observed_success", 0.9)], 10);
        assert_eq!(profile.facts.len(), 1);
        assert!((profile.facts[0].truth.confidence - 0.9).abs() < f64::EPSILON);

        profile.merge([fact("tool_read_file_observed_success", 0.3)], 10);
        assert!((profile.facts[0].truth.confidence - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn merge_is_bounded() {
        let mut profile = ProfileMemory::default();
        profile.merge((0..20).map(|index| fact(&format!("fact_{index}"), 0.9)), 5);
        assert_eq!(profile.facts.len(), 5);
    }
}
