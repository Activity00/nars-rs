//! Project verification discovery.
//!
//! Resolution order:
//! 1. `.nars-agent/project.json` (`verify` object or array);
//! 2. common project manifests (`package.json`, `pyproject.toml`, `go.mod`, ...).
//!
//! Cargo projects intentionally return no commands unless configured, so the
//! existing trusted `cargo_check`/`cargo_test` path remains the default.

use std::path::Path;

use nars_agent_ir::VerificationCommand;
use serde_json::Value;

/// Discovers verification commands for a workspace.
#[must_use]
pub fn detect(workspace: &Path) -> Vec<VerificationCommand> {
    if let Some(commands) = load_config(workspace) {
        return commands;
    }
    if workspace.join("Cargo.toml").is_file() {
        // Keep the dedicated cargo tools as the default. Polyglot Cargo
        // workspaces can opt into other commands through project.json.
        return Vec::new();
    }
    let mut commands = Vec::new();
    if let Some(command) = npm_test(workspace) {
        commands.push(command);
    }
    if python_tests(workspace) {
        commands.push(VerificationCommand {
            label: "pytest".to_owned(),
            command: "python3 -m pytest -q".to_owned(),
            cwd: None,
            timeout_ms: None,
        });
    }
    if workspace.join("go.mod").is_file() {
        commands.push(VerificationCommand {
            label: "go test".to_owned(),
            command: "go test ./...".to_owned(),
            cwd: None,
            timeout_ms: None,
        });
    }
    if make_test(workspace) {
        commands.push(VerificationCommand {
            label: "make test".to_owned(),
            command: "make test".to_owned(),
            cwd: None,
            timeout_ms: None,
        });
    }
    if workspace.join("justfile").is_file() || workspace.join("Justfile").is_file() {
        commands.push(VerificationCommand {
            label: "just test".to_owned(),
            command: "just test".to_owned(),
            cwd: None,
            timeout_ms: None,
        });
    }
    if workspace.join("gradlew").is_file() {
        commands.push(VerificationCommand {
            label: "gradle tests".to_owned(),
            command: "./gradlew test".to_owned(),
            cwd: None,
            timeout_ms: None,
        });
    }
    if workspace.join("mvnw").is_file() {
        commands.push(VerificationCommand {
            label: "maven tests".to_owned(),
            command: "./mvnw test".to_owned(),
            cwd: None,
            timeout_ms: None,
        });
    }
    if workspace.join("Package.swift").is_file() {
        commands.push(VerificationCommand {
            label: "swift test".to_owned(),
            command: "swift test".to_owned(),
            cwd: None,
            timeout_ms: None,
        });
    }
    if workspace.join("mix.exs").is_file() {
        commands.push(VerificationCommand {
            label: "mix test".to_owned(),
            command: "mix test".to_owned(),
            cwd: None,
            timeout_ms: None,
        });
    }
    commands.truncate(4);
    commands
}

fn load_config(workspace: &Path) -> Option<Vec<VerificationCommand>> {
    let path = workspace.join(".nars-agent/project.json");
    let text = std::fs::read_to_string(&path).ok()?;
    let value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "invalid project config");
            return None;
        }
    };
    let verify = value.get("verify")?;
    let items = match verify {
        Value::Array(items) => items.clone(),
        Value::Object(_) | Value::String(_) => vec![verify.clone()],
        _ => return None,
    };
    let mut commands = Vec::new();
    for item in items {
        let mut command: VerificationCommand = match item {
            Value::String(command) => VerificationCommand {
                label: command.chars().take(40).collect(),
                command,
                cwd: None,
                timeout_ms: None,
            },
            Value::Object(object) => match serde_json::from_value(Value::Object(object.clone())) {
                Ok(command) => command,
                Err(error) => {
                    tracing::warn!(%error, "invalid verification command in project config");
                    continue;
                }
            },
            _ => continue,
        };
        if command.command.trim().is_empty() {
            continue;
        }
        if command.label.trim().is_empty() {
            command.label = command.command.chars().take(40).collect();
        }
        commands.push(command);
        if commands.len() >= 8 {
            break;
        }
    }
    if commands.is_empty() {
        None
    } else {
        Some(commands)
    }
}

fn npm_test(workspace: &Path) -> Option<VerificationCommand> {
    let text = std::fs::read_to_string(workspace.join("package.json")).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let script = value.get("scripts")?.get("test")?.as_str()?;
    if script.trim().is_empty() {
        return None;
    }
    Some(VerificationCommand {
        label: "npm test".to_owned(),
        command: "npm test".to_owned(),
        cwd: None,
        timeout_ms: None,
    })
}

fn python_tests(workspace: &Path) -> bool {
    workspace.join("pytest.ini").is_file()
        || workspace.join("pyproject.toml").is_file()
        || workspace.join("tox.ini").is_file()
        || workspace.join("tests").is_dir()
        || workspace.join("test").is_dir()
}

fn make_test(workspace: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(workspace.join("Makefile")) else {
        return false;
    };
    text.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("test:") || line.starts_with(".PHONY: test")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_npm_test_script() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"test":"vitest run"}}"#,
        )
        .unwrap();
        let commands = detect(dir.path());
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].command, "npm test");
    }

    #[test]
    fn ignores_package_without_test_script() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), r#"{"scripts":{}}"#).unwrap();
        assert!(detect(dir.path()).is_empty());
    }

    #[test]
    fn detects_python_and_go() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("tests")).unwrap();
        std::fs::write(dir.path().join("go.mod"), "module example.com/x\n").unwrap();
        let commands = detect(dir.path());
        assert!(
            commands
                .iter()
                .any(|command| command.command.contains("pytest"))
        );
        assert!(
            commands
                .iter()
                .any(|command| command.command == "go test ./...")
        );
    }

    #[test]
    fn project_config_overrides_detection() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"test":"vitest run"}}"#,
        )
        .unwrap();
        std::fs::create_dir(dir.path().join(".nars-agent")).unwrap();
        std::fs::write(
            dir.path().join(".nars-agent/project.json"),
            r#"{"verify":{"label":"custom","command":"pnpm test","timeout_ms":5000}}"#,
        )
        .unwrap();
        let commands = detect(dir.path());
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].label, "custom");
        assert_eq!(commands[0].command, "pnpm test");
        assert_eq!(commands[0].timeout_ms, Some(5000));
    }

    #[test]
    fn cargo_only_uses_default_tools() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        std::fs::create_dir(dir.path().join("tests")).unwrap();
        assert!(detect(dir.path()).is_empty());
    }
}
