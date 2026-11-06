//! Child-process execution with real timeouts and captured output.
//!
//! The implementation polls `try_wait`, kills the child at the deadline, and
//! drains stdout/stderr on dedicated reader threads so large outputs cannot
//! deadlock the child on a full pipe buffer.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A fully specified child process.
#[derive(Clone, Debug)]
pub struct CommandSpec {
    /// Executable name or path.
    pub program: String,
    /// Arguments.
    pub args: Vec<String>,
    /// Working directory.
    pub cwd: PathBuf,
    /// Hard timeout in milliseconds.
    pub timeout_ms: u64,
    /// Extra environment variables.
    pub env: Vec<(String, String)>,
}

impl CommandSpec {
    /// Creates a command specification with no arguments or environment.
    #[must_use]
    pub fn new(program: impl Into<String>, cwd: impl Into<PathBuf>, timeout_ms: u64) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            timeout_ms,
            env: Vec::new(),
        }
    }

    /// Appends one argument.
    #[must_use]
    pub fn arg(mut self, argument: impl Into<String>) -> Self {
        self.args.push(argument.into());
        self
    }

    /// Appends several arguments.
    #[must_use]
    pub fn args<I, S>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(arguments.into_iter().map(Into::into));
        self
    }

    /// Adds an environment variable.
    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
}

/// The result of one child process.
#[derive(Clone, Debug)]
pub struct CommandOutcome {
    /// Exit code when the process exited normally.
    pub exit_code: Option<i32>,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
    /// Whether the process was killed at the deadline.
    pub timed_out: bool,
    /// Wall-clock duration.
    pub duration_ms: u64,
    /// Spawn or wait error.
    pub error: Option<String>,
}

impl CommandOutcome {
    /// Whether the process exited with status 0 before the deadline.
    #[must_use]
    pub fn is_success(&self) -> bool {
        !self.timed_out && self.error.is_none() && self.exit_code == Some(0)
    }
}

/// Runs a child process.
#[must_use]
pub fn run(spec: &CommandSpec) -> CommandOutcome {
    let started = Instant::now();
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in &spec.env {
        command.env(key, value);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return CommandOutcome {
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                timed_out: false,
                duration_ms: elapsed_ms(started),
                error: Some(format!("failed to spawn `{}`: {error}", spec.program)),
            };
        }
    };

    let stdout_reader = child.stdout.take().map(spawn_reader);
    let stderr_reader = child.stderr.take().map(spawn_reader);
    let deadline = started + Duration::from_millis(spec.timeout_ms.max(1));
    let mut timed_out = false;
    let mut exit_code = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code();
                break;
            }
            Ok(None) => {}
            Err(error) => {
                return CommandOutcome {
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    timed_out: false,
                    duration_ms: elapsed_ms(started),
                    error: Some(format!("failed to wait for `{}`: {error}", spec.program)),
                };
            }
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let stdout = stdout_reader.map(join_reader).unwrap_or_default();
    let stderr = stderr_reader.map(join_reader).unwrap_or_default();
    CommandOutcome {
        exit_code,
        stdout: strip_ansi(&stdout),
        stderr: strip_ansi(&stderr),
        timed_out,
        duration_ms: elapsed_ms(started),
        error: None,
    }
}

fn spawn_reader<R: Read + Send + 'static>(mut reader: R) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = reader.read_to_end(&mut buffer);
        buffer
    })
}

fn join_reader(handle: std::thread::JoinHandle<Vec<u8>>) -> String {
    handle
        .join()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Removes ANSI CSI escape sequences so logs render cleanly in the UI.
#[must_use]
pub fn strip_ansi(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' && chars.peek() == Some(&'[') {
            let _ = chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            output.push(character);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn captures_stdout_and_exit_code() {
        let spec = CommandSpec::new("sh", std::env::temp_dir(), 5_000)
            .arg("-c")
            .arg("printf hello");
        let outcome = run(&spec);
        assert!(outcome.is_success());
        assert_eq!(outcome.stdout, "hello");
    }

    #[cfg(unix)]
    #[test]
    fn kills_processes_at_the_deadline() {
        let spec = CommandSpec::new("sh", std::env::temp_dir(), 100)
            .arg("-c")
            .arg("sleep 5");
        let outcome = run(&spec);
        assert!(outcome.timed_out);
        assert!(!outcome.is_success());
    }

    #[test]
    fn strips_ansi_sequences() {
        assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m"), "red");
    }
}
