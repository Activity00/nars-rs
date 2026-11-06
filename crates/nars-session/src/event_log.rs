//! Append-only event log.
//!
//! Each session owns one JSONL file. Every line is a self-contained
//! [`EventEnvelope`] with a monotonically increasing sequence number. The log
//! is the source of truth for replay and audit; snapshots are only caches.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use nars_agent_ir::{AgentEvent, RunId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One persisted event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Monotonic sequence number within the session, starting at 1.
    pub seq: u64,
    /// Wall-clock timestamp in Unix milliseconds.
    pub timestamp_ms: i64,
    /// The protocol event.
    pub event: AgentEvent,
}

/// Event log errors.
#[derive(Debug, Error)]
pub enum LogError {
    /// Filesystem failure.
    #[error("event log I/O error at {path}: {source}")]
    Io {
        /// Path involved.
        path: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// A line is not valid JSON.
    #[error("malformed event at {path}:{line}: {message}")]
    Malformed {
        /// Path involved.
        path: String,
        /// One-based line number.
        line: usize,
        /// Parse error.
        message: String,
    },
    /// The log's sequence numbers are not monotonic.
    #[error("event log sequence error at {path}:{line}")]
    Sequence {
        /// Path involved.
        path: String,
        /// One-based line number.
        line: usize,
    },
}

/// A single JSONL log file.
#[derive(Debug)]
pub struct EventLog {
    path: PathBuf,
    next_seq: Mutex<u64>,
}

impl EventLog {
    /// Opens or creates a log, scanning existing events for the next sequence.
    ///
    /// # Errors
    ///
    /// Returns an error when the parent directory or existing file cannot be read.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, LogError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| LogError::Io {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let max_seq = if path.exists() {
            read_envelopes(&path)?
                .last()
                .map_or(0, |envelope| envelope.seq)
        } else {
            0
        };
        Ok(Self {
            path,
            next_seq: Mutex::new(max_seq + 1),
        })
    }

    /// The log file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one event.
    ///
    /// # Errors
    ///
    /// Returns an error when serialization or the append fails.
    pub fn append(&self, event: AgentEvent, timestamp_ms: i64) -> Result<EventEnvelope, LogError> {
        let mut guard = self.next_seq.lock().unwrap();
        let envelope = EventEnvelope {
            seq: *guard,
            timestamp_ms,
            event,
        };
        let line = serde_json::to_string(&envelope).map_err(|error| LogError::Malformed {
            path: self.path.display().to_string(),
            line: usize::try_from(*guard).unwrap_or(0),
            message: error.to_string(),
        })?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|source| LogError::Io {
                path: self.path.display().to_string(),
                source,
            })?;
        writeln!(file, "{line}").map_err(|source| LogError::Io {
            path: self.path.display().to_string(),
            source,
        })?;
        file.flush().map_err(|source| LogError::Io {
            path: self.path.display().to_string(),
            source,
        })?;
        *guard += 1;
        Ok(envelope)
    }

    /// Reads all events.
    ///
    /// # Errors
    ///
    /// Returns an error for missing files, malformed lines, or bad sequencing.
    pub fn read_all(&self) -> Result<Vec<EventEnvelope>, LogError> {
        read_envelopes(&self.path)
    }

    /// Reads events that belong to one run.
    ///
    /// # Errors
    ///
    /// Returns an error for missing files, malformed lines, or bad sequencing.
    pub fn read_run(&self, run_id: &RunId) -> Result<Vec<EventEnvelope>, LogError> {
        Ok(self
            .read_all()?
            .into_iter()
            .filter(|envelope| envelope.event.run_id() == Some(run_id))
            .collect())
    }

    /// Rewrites the log from a filtered set of events.
    ///
    /// # Errors
    ///
    /// Returns an error when the replacement file cannot be written.
    pub fn rewrite(&self, events: &[EventEnvelope]) -> Result<(), LogError> {
        let temp = self.path.with_extension("jsonl.tmp");
        {
            let mut file = File::create(&temp).map_err(|source| LogError::Io {
                path: temp.display().to_string(),
                source,
            })?;
            for envelope in events {
                let line =
                    serde_json::to_string(envelope).map_err(|error| LogError::Malformed {
                        path: temp.display().to_string(),
                        line: usize::try_from(envelope.seq).unwrap_or(0),
                        message: error.to_string(),
                    })?;
                writeln!(file, "{line}").map_err(|source| LogError::Io {
                    path: temp.display().to_string(),
                    source,
                })?;
            }
        }
        std::fs::rename(&temp, &self.path).map_err(|source| LogError::Io {
            path: self.path.display().to_string(),
            source,
        })?;
        let next = events.last().map_or(1, |envelope| envelope.seq + 1);
        *self.next_seq.lock().unwrap() = next;
        Ok(())
    }
}

fn read_envelopes(path: &Path) -> Result<Vec<EventEnvelope>, LogError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(LogError::Io {
                path: path.display().to_string(),
                source,
            });
        }
    };
    let mut envelopes = Vec::new();
    let mut expected = 1_u64;
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|source| LogError::Io {
            path: path.display().to_string(),
            source,
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let envelope: EventEnvelope =
            serde_json::from_str(&line).map_err(|error| LogError::Malformed {
                path: path.display().to_string(),
                line: index + 1,
                message: error.to_string(),
            })?;
        if envelope.seq != expected {
            return Err(LogError::Sequence {
                path: path.display().to_string(),
                line: index + 1,
            });
        }
        expected += 1;
        envelopes.push(envelope);
    }
    Ok(envelopes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nars_agent_ir::SessionId;

    fn event(text: &str) -> AgentEvent {
        AgentEvent::AssistantMessage {
            run_id: RunId::from_static("run_1"),
            text: text.to_owned(),
        }
    }

    #[test]
    fn appends_and_reads_monotonic_events() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::open(dir.path().join("events.jsonl")).unwrap();
        log.append(event("one"), 1).unwrap();
        log.append(event("two"), 2).unwrap();
        let events = log.read_all().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 1);
        assert_eq!(events[1].seq, 2);
    }

    #[test]
    fn reopening_continues_the_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        {
            let log = EventLog::open(&path).unwrap();
            log.append(event("one"), 1).unwrap();
        }
        let log = EventLog::open(&path).unwrap();
        let envelope = log.append(event("two"), 2).unwrap();
        assert_eq!(envelope.seq, 2);
    }

    #[test]
    fn filters_events_by_run() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::open(dir.path().join("events.jsonl")).unwrap();
        log.append(event("one"), 1).unwrap();
        log.append(
            AgentEvent::RunPaused {
                run_id: RunId::from_static("run_2"),
                reason: "test".to_owned(),
            },
            2,
        )
        .unwrap();
        assert_eq!(log.read_run(&RunId::from_static("run_1")).unwrap().len(), 1);
        assert_eq!(log.read_run(&RunId::from_static("run_2")).unwrap().len(), 1);
    }

    #[test]
    fn reports_malformed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, "not json\n").unwrap();
        assert!(matches!(
            EventLog::open(&path),
            Err(LogError::Malformed { .. })
        ));
    }

    #[test]
    fn session_ids_are_path_safe() {
        let id = SessionId::new();
        assert!(
            id.as_str()
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
        );
    }
}
