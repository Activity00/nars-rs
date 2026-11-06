//! Cross-run experience reused by the planner.
//!
//! Tool and action-signature statistics survive runs and sessions for the same
//! workspace. This is the mechanism behind "the same kind of task reuses
//! historical experience": candidate scoring starts from observed outcomes
//! instead of a blank prior.

use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::store::StoreError;

/// Accumulated outcomes for one tool, signature, or task type.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperienceStat {
    /// Attempts observed.
    pub attempts: u32,
    /// Successful attempts.
    pub successes: u32,
    /// Failed attempts.
    pub failures: u32,
}

impl ExperienceStat {
    /// Laplace-smoothed success rate.
    #[must_use]
    pub fn success_rate(&self) -> f64 {
        (f64::from(self.successes) + 1.0) / (f64::from(self.attempts) + 2.0)
    }
}

/// Workspace-scoped historical experience.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Experience {
    /// Schema version.
    pub version: u32,
    /// Canonical workspace path.
    pub workspace: String,
    /// Last update timestamp in Unix milliseconds.
    pub updated_at_ms: i64,
    /// Per-tool outcomes.
    #[serde(default)]
    pub tools: BTreeMap<String, ExperienceStat>,
    /// Per action-signature outcomes.
    #[serde(default)]
    pub signatures: BTreeMap<String, ExperienceStat>,
    /// Per goal-type outcomes.
    #[serde(default)]
    pub tasks: BTreeMap<String, ExperienceStat>,
}

impl Default for Experience {
    fn default() -> Self {
        Self {
            version: 1,
            workspace: String::new(),
            updated_at_ms: 0,
            tools: BTreeMap::new(),
            signatures: BTreeMap::new(),
            tasks: BTreeMap::new(),
        }
    }
}

impl Experience {
    /// Creates empty experience for a workspace.
    #[must_use]
    pub fn new(workspace: impl Into<String>) -> Self {
        Self {
            workspace: workspace.into(),
            ..Self::default()
        }
    }

    /// Records a tool outcome.
    pub fn record_tool(&mut self, tool: &str, success: bool) {
        let stat = self.tools.entry(tool.to_owned()).or_default();
        stat.attempts = stat.attempts.saturating_add(1);
        if success {
            stat.successes = stat.successes.saturating_add(1);
        } else {
            stat.failures = stat.failures.saturating_add(1);
        }
    }

    /// Records an action-signature outcome.
    pub fn record_signature(&mut self, signature: &str, success: bool) {
        let stat = self.signatures.entry(signature.to_owned()).or_default();
        stat.attempts = stat.attempts.saturating_add(1);
        if success {
            stat.successes = stat.successes.saturating_add(1);
        } else {
            stat.failures = stat.failures.saturating_add(1);
        }
    }

    /// Records a task-type outcome.
    pub fn record_task(&mut self, goal_type: &str, success: bool) {
        let stat = self.tasks.entry(goal_type.to_owned()).or_default();
        stat.attempts = stat.attempts.saturating_add(1);
        if success {
            stat.successes = stat.successes.saturating_add(1);
        } else {
            stat.failures = stat.failures.saturating_add(1);
        }
    }
}

/// Persistence interface for workspace experience.
pub trait ExperienceStore: Send + Sync {
    /// Loads experience for a canonical workspace.
    ///
    /// # Errors
    ///
    /// Returns an error for I/O or malformed JSON.
    fn load(&self, workspace: &str) -> Result<Experience, StoreError>;
    /// Stores experience for a canonical workspace.
    ///
    /// # Errors
    ///
    /// Returns an error for I/O failures.
    fn save(&self, experience: &Experience) -> Result<(), StoreError>;
}

/// File-backed experience store.
#[derive(Debug)]
pub struct FileExperienceStore {
    root: PathBuf,
    lock: Mutex<()>,
}

impl FileExperienceStore {
    /// Opens or creates the experience directory.
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

impl ExperienceStore for FileExperienceStore {
    fn load(&self, workspace: &str) -> Result<Experience, StoreError> {
        let _guard = self.lock.lock().unwrap();
        let path = self.path_for(workspace);
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Experience::new(workspace));
            }
            Err(source) => {
                return Err(StoreError::Io {
                    path: path.display().to_string(),
                    source,
                });
            }
        };
        let mut experience: Experience =
            serde_json::from_str(&content).map_err(|source| StoreError::Json {
                path: path.display().to_string(),
                source,
            })?;
        if experience.workspace.is_empty() {
            experience.workspace = workspace.to_owned();
        }
        Ok(experience)
    }

    fn save(&self, experience: &Experience) -> Result<(), StoreError> {
        let _guard = self.lock.lock().unwrap();
        let path = self.path_for(&experience.workspace);
        let temp = path.with_extension("json.tmp");
        let content = serde_json::to_vec_pretty(experience).map_err(|source| StoreError::Json {
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

    #[test]
    fn records_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileExperienceStore::new(dir.path()).unwrap();
        let mut experience = store.load("/tmp/ws").unwrap();
        experience.record_tool("search", false);
        experience.record_tool("search", true);
        experience.record_signature("search:{}", true);
        experience.record_task("modify_code", true);
        store.save(&experience).unwrap();

        let loaded = store.load("/tmp/ws").unwrap();
        assert_eq!(loaded.tools["search"].attempts, 2);
        assert_eq!(loaded.tools["search"].successes, 1);
        assert_eq!(loaded.tasks["modify_code"].successes, 1);
        assert!((loaded.tools["search"].success_rate() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn workspaces_are_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileExperienceStore::new(dir.path()).unwrap();
        let mut first = store.load("/tmp/a").unwrap();
        first.record_tool("read_file", true);
        store.save(&first).unwrap();
        let second = store.load("/tmp/b").unwrap();
        assert!(second.tools.is_empty());
    }
}
