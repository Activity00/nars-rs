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

use crate::store::StoreError;

/// Persistence interface for workspace long-term memory.
pub trait MemoryStore: Send + Sync {
    /// Loads the stored memory, or `None` when the workspace has never run.
    ///
    /// # Errors
    ///
    /// Returns an error when the file exists but cannot be read or decoded.
    fn load(&self, workspace: &str) -> Result<Option<MemorySnapshot>, StoreError>;
    /// Persists memory for a workspace.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory or file cannot be written.
    fn save(&self, workspace: &str, snapshot: &MemorySnapshot) -> Result<(), StoreError>;
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
    fn load(&self, workspace: &str) -> Result<Option<MemorySnapshot>, StoreError> {
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
        let snapshot: MemorySnapshot =
            serde_json::from_str(&content).map_err(|source| StoreError::Json {
                path: path.display().to_string(),
                source,
            })?;
        Ok(Some(snapshot))
    }

    fn save(&self, workspace: &str, snapshot: &MemorySnapshot) -> Result<(), StoreError> {
        let _guard = self.lock.lock().unwrap();
        let path = self.path_for(workspace);
        let temp = path.with_extension("json.tmp");
        let content = serde_json::to_vec_pretty(snapshot).map_err(|source| StoreError::Json {
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
        Ok(())
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
        let memory = Memory::default();
        let term = language.atomic_term("cat").unwrap();
        let mut memory = memory;
        memory.conceptualize(&language, term, 0);
        let snapshot = memory.to_snapshot(&language);

        store.save("/tmp/ws", &snapshot).unwrap();
        let loaded = store.load("/tmp/ws").unwrap().expect("stored");
        assert_eq!(loaded, snapshot);

        let restored = Memory::from_snapshot(&loaded).unwrap();
        assert!(restored.find(&term).is_some());
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
            .save("/tmp/one", &memory.to_snapshot(&language))
            .unwrap();
        assert!(store.load("/tmp/two").unwrap().is_none());
    }
}
