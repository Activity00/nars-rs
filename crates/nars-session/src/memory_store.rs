//! Workspace-scoped long-term NARS memory.
//!
//! Runs are short-lived, but the concepts, beliefs, and learned implications
//! they build are not: the agent loads one memory per workspace, continues it
//! in every run, and writes it back when a run finishes. The store keeps one
//! JSON file per canonical workspace, mirroring the experience store.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::Mutex;

use nars_core::MemorySnapshot;
use serde::{Deserialize, Serialize};

use crate::store::StoreError;

/// A memory snapshot plus the generation it was saved as.
///
/// Generations are the store's optimistic concurrency guard: a run writes back
/// only if nobody else replaced the file since it loaded it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredMemory {
    /// Monotonic write counter.
    pub generation: u64,
    /// Snapshot at this generation.
    pub snapshot: MemorySnapshot,
}

/// Persistence interface for workspace long-term memory.
pub trait MemoryStore: Send + Sync {
    /// Loads the stored memory, or `None` when the workspace has never run.
    ///
    /// # Errors
    ///
    /// Returns an error when the file exists but cannot be read or decoded.
    fn load(&self, workspace: &str) -> Result<Option<StoredMemory>, StoreError>;
    /// Persists memory for a workspace, guarding against lost updates.
    ///
    /// `expected_generation` is the generation the caller loaded (`None` for a
    /// workspace with no file yet). The save is rejected with
    /// [`StoreError::Conflict`] when the file has moved on, so a slow run
    /// cannot overwrite work another run already committed.
    ///
    /// Returns the new generation.
    ///
    /// # Errors
    ///
    /// Returns an error when the generation does not match or the file cannot
    /// be written.
    fn save(
        &self,
        workspace: &str,
        expected_generation: Option<u64>,
        snapshot: &MemorySnapshot,
    ) -> Result<u64, StoreError>;
}

#[derive(Serialize, Deserialize)]
struct MemoryFile {
    version: u32,
    generation: u64,
    snapshot: MemorySnapshot,
}

fn decode(content: &str, path: &std::path::Path) -> Result<StoredMemory, StoreError> {
    let value: serde_json::Value =
        serde_json::from_str(content).map_err(|source| StoreError::Json {
            path: path.display().to_string(),
            source,
        })?;
    if value.get("snapshot").is_some() {
        let file: MemoryFile =
            serde_json::from_value(value).map_err(|source| StoreError::Json {
                path: path.display().to_string(),
                source,
            })?;
        return Ok(StoredMemory {
            generation: file.generation,
            snapshot: file.snapshot,
        });
    }
    // Files written before generations existed are generation zero.
    let snapshot: MemorySnapshot =
        serde_json::from_value(value).map_err(|source| StoreError::Json {
            path: path.display().to_string(),
            source,
        })?;
    Ok(StoredMemory {
        generation: 0,
        snapshot,
    })
}

/// File-backed memory store.
#[derive(Debug)]
pub struct FileMemoryStore {
    root: PathBuf,
    lock: Mutex<()>,
}

impl FileMemoryStore {
    /// Opens or creates the memory directory.
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
        Ok(Self {
            root,
            lock: Mutex::new(()),
        })
    }

    fn path_for(&self, workspace: &str) -> PathBuf {
        let mut hasher = DefaultHasher::new();
        workspace.hash(&mut hasher);
        self.root.join(format!("{:016x}.json", hasher.finish()))
    }
}

impl MemoryStore for FileMemoryStore {
    fn load(&self, workspace: &str) -> Result<Option<StoredMemory>, StoreError> {
        let _guard = self.lock.lock().unwrap();
        let path = self.path_for(workspace);
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
        decode(&content, &path).map(Some)
    }

    fn save(
        &self,
        workspace: &str,
        expected_generation: Option<u64>,
        snapshot: &MemorySnapshot,
    ) -> Result<u64, StoreError> {
        let _guard = self.lock.lock().unwrap();
        let path = self.path_for(workspace);
        let current = match std::fs::read_to_string(&path) {
            Ok(content) => Some(decode(&content, &path)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(StoreError::Io {
                    path: path.display().to_string(),
                    source,
                });
            }
        };
        let current_generation = current.as_ref().map(|stored| stored.generation);
        if current_generation != expected_generation {
            return Err(StoreError::Conflict(format!(
                "workspace memory moved from generation {current_generation:?} to a different one"
            )));
        }
        let generation = current_generation.map_or(1, |generation| generation.saturating_add(1));
        let file = MemoryFile {
            version: 1,
            generation,
            snapshot: snapshot.clone(),
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
    use nars_core::{Memory, Narsese};

    #[test]
    fn missing_workspace_loads_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileMemoryStore::new(dir.path()).unwrap();
        assert!(store.load("/tmp/ws").unwrap().is_none());
    }

    #[test]
    fn round_trips_a_memory_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileMemoryStore::new(dir.path()).unwrap();
        let mut language = Narsese::new();
        let mut memory = Memory::default();
        let term = language.atomic_term("cat").unwrap();
        memory.conceptualize(&language, term, 0);
        let snapshot = memory.to_snapshot(&language);

        let generation = store.save("/tmp/ws", None, &snapshot).unwrap();
        assert_eq!(generation, 1);
        let loaded = store.load("/tmp/ws").unwrap().expect("stored");
        assert_eq!(loaded.generation, 1);
        assert_eq!(loaded.snapshot, snapshot);

        let restored = Memory::from_snapshot(&loaded.snapshot).unwrap();
        assert!(restored.find(&term).is_some());
    }

    #[test]
    fn rejects_a_stale_generation() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileMemoryStore::new(dir.path()).unwrap();
        let language = Narsese::new();
        let snapshot = Memory::default().to_snapshot(&language);

        store.save("/tmp/ws", None, &snapshot).unwrap();
        assert!(matches!(
            store.save("/tmp/ws", None, &snapshot),
            Err(StoreError::Conflict(_))
        ));
        assert_eq!(
            store.save("/tmp/ws", Some(1), &snapshot).unwrap(),
            2,
            "the second writer advances the generation"
        );
        assert!(matches!(
            store.save("/tmp/ws", Some(1), &snapshot),
            Err(StoreError::Conflict(_))
        ));
    }

    #[test]
    fn workspaces_do_not_share_memory() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileMemoryStore::new(dir.path()).unwrap();
        let mut language = Narsese::new();
        let mut memory = Memory::default();
        let term = language.atomic_term("cat").unwrap();
        memory.conceptualize(&language, term, 0);
        store
            .save("/tmp/one", None, &memory.to_snapshot(&language))
            .unwrap();
        assert!(store.load("/tmp/two").unwrap().is_none());
    }
}
