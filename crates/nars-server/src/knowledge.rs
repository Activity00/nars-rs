//! Workspace knowledge learned from `ask_model` answers.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One question/answer pair learned from the model.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeEntry {
    /// Normalized question.
    pub question: String,
    /// Answer text.
    pub answer: String,
    /// Last update in Unix milliseconds.
    pub updated_ms: i64,
}

/// Append-only JSONL store under the workspace.
#[derive(Debug)]
pub struct KnowledgeStore {
    path: PathBuf,
}

impl KnowledgeStore {
    /// Opens the store for a workspace, creating the directory lazily.
    #[must_use]
    pub fn for_workspace(workspace: &Path) -> Self {
        Self {
            path: workspace.join(".nars-agent").join("knowledge.jsonl"),
        }
    }

    /// Loads all entries, ignoring malformed lines.
    #[must_use]
    pub fn load(&self) -> Vec<KnowledgeEntry> {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// Records an answer, replacing an earlier answer for the same question.
    pub fn upsert(&self, question: &str, answer: &str, now_ms: i64) {
        let question = normalize(question);
        let answer = answer.trim();
        if question.is_empty() || answer.is_empty() {
            return;
        }
        let mut entries = self.load();
        entries.retain(|entry| entry.question != question);
        entries.push(KnowledgeEntry {
            question,
            answer: answer.to_owned(),
            updated_ms: now_ms,
        });
        while entries.len() > 200 {
            entries.remove(0);
        }
        if let Some(parent) = self.path.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return;
        }
        let mut text = String::new();
        for entry in &entries {
            if let Ok(line) = serde_json::to_string(entry) {
                text.push_str(&line);
                text.push('\n');
            }
        }
        let _ = std::fs::write(&self.path, text);
    }

    /// Returns the stored answer for a question, if any.
    #[must_use]
    pub fn answer(&self, question: &str) -> Option<String> {
        self.load()
            .into_iter()
            .rev()
            .find(|entry| matches(&entry.question, question))
            .map(|entry| entry.answer)
    }

    /// Returns up to `limit` entries whose question matches, newest first.
    #[must_use]
    pub fn matching(&self, question: &str, limit: usize) -> Vec<KnowledgeEntry> {
        let mut entries = self
            .load()
            .into_iter()
            .rev()
            .filter(|entry| matches(&entry.question, question))
            .take(limit)
            .collect::<Vec<_>>();
        entries.reverse();
        entries
    }
}

/// Whether a stored question covers a new one.
///
/// Containment only applies once both sides carry at least six characters;
/// otherwise "what" would match "what is this project".
#[must_use]
pub fn matches(stored: &str, question: &str) -> bool {
    let stored = normalize(stored);
    let question = normalize(question);
    if stored.is_empty() || question.is_empty() {
        return false;
    }
    if stored == question {
        return true;
    }
    stored.len() >= 6
        && question.len() >= 6
        && (stored.contains(&question) || question.contains(&stored))
}

/// Normalizes a question for matching.
#[must_use]
pub fn normalize(question: &str) -> String {
    question
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_reuses_answers_per_workspace() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = KnowledgeStore::for_workspace(dir.path());
        assert!(store.answer("what is NARS?").is_none());
        store.upsert("what is NARS?", "a reasoning system", 1);
        assert_eq!(
            store.answer("  What is NARS? ").as_deref(),
            Some("a reasoning system")
        );
        // A newer answer replaces the older one instead of duplicating it.
        store.upsert("what is NARS?", "a non-axiomatic reasoner", 2);
        let entries = store.load();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].answer, "a non-axiomatic reasoner");
    }
}
