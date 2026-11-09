//! `run_command`: project-agnostic command execution.
//!
//! Commands run through the platform shell inside the workspace sandbox. They
//! are classified as process execution and require approval unless the policy
//! explicitly trusts the command prefix.

use nars_agent_ir::{SideEffect, ToolCall, ToolManifest, ToolResult, ToolStatus};
use serde_json::json;

use crate::builtin::{failure, opt_string, opt_u64, req_string};
use crate::manifest::{Tool, ToolContext, object_schema, string_schema};
use crate::process::{CommandOutcome, CommandSpec};

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_TIMEOUT_MS: u64 = 300_000;
const MAX_COMMAND_LEN: usize = 8_192;

/// Runs a project command such as `npm test`, `pytest`, or `go test`.
#[derive(Debug, Default, Clone, Copy)]
pub struct RunCommandTool;

impl Tool for RunCommandTool {
    fn manifest(&self) -> &ToolManifest {
        static MANIFEST: std::sync::OnceLock<ToolManifest> = std::sync::OnceLock::new();
        MANIFEST.get_or_init(|| ToolManifest {
            name: "run_command".to_owned(),
            description: "Run a project command in the workspace, for example \
                          `npm test`, `python3 -m pytest -q`, `go test ./...`, or \
                          `make test`. Use this for non-Rust verification. The command \
                          requires approval unless the policy trusts its prefix."
                .to_owned(),
            input_schema: object_schema(
                json!({
                    "command": string_schema("Shell command to execute in the workspace."),
                    "cwd": string_schema("Optional workspace-relative working directory."),
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 1000,
                        "maximum": MAX_TIMEOUT_MS,
                        "description": "Optional timeout in milliseconds."
                    }
                }),
                &["command"],
            ),
            side_effect: SideEffect::ProcessExecution,
            timeout_ms: MAX_TIMEOUT_MS,
            requires_approval: true,
            version: "0.1.0".to_owned(),
            tags: vec![
                "builtin".to_owned(),
                "process".to_owned(),
                "verify".to_owned(),
            ],
        })
    }

    fn execute(&self, call: &ToolCall, context: &ToolContext) -> ToolResult {
        let command = match req_string(&call.arguments, "command") {
            Ok(command) if !command.trim().is_empty() => command,
            Ok(_) => return failure(call, "`command` must not be empty"),
            Err(error) => return failure(call, error),
        };
        if command.len() > MAX_COMMAND_LEN {
            return failure(call, "`command` is too long");
        }
        if command.contains('\0') {
            return failure(call, "`command` contains a NUL byte");
        }
        let cwd = match opt_string(&call.arguments, "cwd") {
            Ok(Some(cwd)) => match context.sandbox.resolve_existing(&cwd) {
                Ok(path) => path,
                Err(error) => return failure(call, error.to_string()),
            },
            Ok(None) => context.sandbox.root().to_path_buf(),
            Err(error) => return failure(call, error),
        };
        let timeout_ms = match opt_u64(&call.arguments, "timeout_ms") {
            Ok(Some(value)) => value.clamp(1_000, MAX_TIMEOUT_MS),
            Ok(None) => DEFAULT_TIMEOUT_MS,
            Err(error) => return failure(call, error),
        };

        let mut spec = shell_spec(&cwd, timeout_ms);
        spec = spec.arg(&command);
        let display_cwd = context.sandbox.display(&cwd);
        let outcome = crate::process::run(&spec);
        let mut result = outcome_to_result(call, &command, &outcome);
        result.metadata.insert("command".to_owned(), json!(command));
        result.metadata.insert("cwd".to_owned(), json!(display_cwd));
        result
            .metadata
            .insert("duration_ms".to_owned(), json!(outcome.duration_ms));
        result
    }
}

#[cfg(unix)]
fn shell_spec(cwd: &std::path::Path, timeout_ms: u64) -> CommandSpec {
    CommandSpec::new("sh", cwd, timeout_ms).arg("-c")
}

#[cfg(windows)]
fn shell_spec(cwd: &std::path::Path, timeout_ms: u64) -> CommandSpec {
    CommandSpec::new("cmd", cwd, timeout_ms).arg("/C")
}

fn outcome_to_result(call: &ToolCall, command: &str, outcome: &CommandOutcome) -> ToolResult {
    let mut result = ToolResult {
        call_id: call.id.clone(),
        status: if outcome.is_success() {
            ToolStatus::Succeeded
        } else if outcome.timed_out {
            ToolStatus::TimedOut
        } else {
            ToolStatus::Failed
        },
        exit_code: outcome.exit_code,
        stdout: outcome.stdout.clone(),
        stderr: outcome.stderr.clone(),
        truncated: false,
        duration_ms: outcome.duration_ms,
        diff: None,
        artifacts: Vec::new(),
        error: None,
        metadata: serde_json::Map::new(),
    };
    if let Some(error) = &outcome.error {
        result.error = Some(error.clone());
    } else if outcome.timed_out {
        result.error = Some(format!("command timed out: {command}"));
    } else if !outcome.is_success() {
        result.error = Some(format!(
            "command exited with code {}: {command}",
            outcome
                .exit_code
                .map_or_else(|| "unknown".to_owned(), |code| code.to_string())
        ));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use nars_agent_ir::{RunId, ToolCallId};
    use tempfile::tempdir;

    fn call(arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: ToolCallId::from_static("call_1"),
            run_id: RunId::from_static("run_1"),
            tool: "run_command".to_owned(),
            arguments,
            side_effect: SideEffect::ProcessExecution,
            requires_approval: true,
            created_at_ms: 0,
        }
    }

    fn context(dir: &std::path::Path) -> ToolContext {
        ToolContext::new(crate::sandbox::Sandbox::new(dir).unwrap())
    }

    #[test]
    fn runs_a_shell_command_in_the_workspace() {
        let dir = tempdir().unwrap();
        let tool = RunCommandTool;
        let result = tool.execute(
            &call(json!({"command": "printf hello"})),
            &context(dir.path()),
        );
        assert!(result.is_success(), "{result:?}");
        assert_eq!(result.stdout, "hello");
        assert_eq!(result.metadata["command"], "printf hello");
    }

    #[test]
    fn reports_non_zero_exit_codes() {
        let dir = tempdir().unwrap();
        let tool = RunCommandTool;
        let result = tool.execute(&call(json!({"command": "exit 7"})), &context(dir.path()));
        assert!(!result.is_success());
        assert_eq!(result.exit_code, Some(7));
    }

    #[test]
    fn resolves_cwd_inside_the_sandbox() {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let tool = RunCommandTool;
        let result = tool.execute(
            &call(json!({"command": "pwd", "cwd": "sub"})),
            &context(dir.path()),
        );
        assert!(result.is_success(), "{result:?}");
        assert!(result.stdout.trim().ends_with("/sub"));
    }

    #[test]
    fn rejects_cwd_escape() {
        let dir = tempdir().unwrap();
        let tool = RunCommandTool;
        let result = tool.execute(
            &call(json!({"command": "pwd", "cwd": "../"})),
            &context(dir.path()),
        );
        assert!(!result.is_success());
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("escapes")
        );
    }

    #[test]
    fn policy_requires_approval_for_generic_commands() {
        let policy = crate::policy::Policy::default();
        let generic = policy.evaluate(&call(json!({"command": "npm test"})));
        assert!(generic.requires_approval());
        let trusted = policy.evaluate(&call(json!({"command": "cargo test"})));
        assert!(trusted.is_allowed());
    }
}
