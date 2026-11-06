//! `cargo_check` and `cargo_test`: trusted, timeout-bounded build commands.

use nars_agent_ir::{SideEffect, ToolCall, ToolManifest, ToolResult, ToolStatus};
use serde_json::json;

use crate::builtin::{failure, opt_string, string_list};
use crate::manifest::{Tool, ToolContext, object_schema, string_array_schema, string_schema};
use crate::process::{CommandOutcome, CommandSpec};

const CARGO_TIMEOUT_MS: u64 = 300_000;

/// Runs `cargo check`.
#[derive(Debug, Default, Clone, Copy)]
pub struct CargoCheckTool;

/// Runs `cargo test`.
#[derive(Debug, Default, Clone, Copy)]
pub struct CargoTestTool;

impl Tool for CargoCheckTool {
    fn manifest(&self) -> &ToolManifest {
        static MANIFEST: std::sync::OnceLock<ToolManifest> = std::sync::OnceLock::new();
        MANIFEST.get_or_init(|| ToolManifest {
            name: "cargo_check".to_owned(),
            description: "Run `cargo check` in the workspace and return compiler output. \
                          Prefer this over a generic shell to verify a change."
                .to_owned(),
            input_schema: cargo_schema(),
            side_effect: SideEffect::ProcessExecution,
            timeout_ms: CARGO_TIMEOUT_MS,
            requires_approval: false,
            version: "0.1.0".to_owned(),
            tags: vec!["builtin".to_owned(), "cargo".to_owned()],
        })
    }

    fn execute(&self, call: &ToolCall, context: &ToolContext) -> ToolResult {
        run_cargo("check", call, context, None)
    }
}

impl Tool for CargoTestTool {
    fn manifest(&self) -> &ToolManifest {
        static MANIFEST: std::sync::OnceLock<ToolManifest> = std::sync::OnceLock::new();
        MANIFEST.get_or_init(|| {
            let mut schema = cargo_schema();
            if let Some(properties) = schema
                .get_mut("properties")
                .and_then(|value| value.as_object_mut())
            {
                properties.insert(
                    "test_name".to_owned(),
                    string_schema("Optional test name filter passed to cargo test."),
                );
            }
            ToolManifest {
                name: "cargo_test".to_owned(),
                description: "Run `cargo test` in the workspace and return the test output. \
                              Use after a code change to verify behavior."
                    .to_owned(),
                input_schema: schema,
                side_effect: SideEffect::ProcessExecution,
                timeout_ms: CARGO_TIMEOUT_MS,
                requires_approval: false,
                version: "0.1.0".to_owned(),
                tags: vec!["builtin".to_owned(), "cargo".to_owned()],
            }
        })
    }

    fn execute(&self, call: &ToolCall, context: &ToolContext) -> ToolResult {
        let test_name = match opt_string(&call.arguments, "test_name") {
            Ok(value) => value,
            Err(error) => return failure(call, error),
        };
        run_cargo("test", call, context, test_name)
    }
}

fn cargo_schema() -> serde_json::Value {
    object_schema(
        json!({
            "package": string_schema("Optional workspace package to build or test."),
            "manifest_path": string_schema("Optional workspace-relative Cargo.toml path."),
            "extra_args": string_array_schema("Additional cargo flags, for example ['--all-features'].")
        }),
        &[],
    )
}

fn run_cargo(
    subcommand: &str,
    call: &ToolCall,
    context: &ToolContext,
    test_name: Option<String>,
) -> ToolResult {
    let package = match opt_string(&call.arguments, "package") {
        Ok(value) => value,
        Err(error) => return failure(call, error),
    };
    let manifest_path = match opt_string(&call.arguments, "manifest_path") {
        Ok(value) => value,
        Err(error) => return failure(call, error),
    };
    let extra_args = match string_list(&call.arguments, "extra_args") {
        Ok(value) => value,
        Err(error) => return failure(call, error),
    };
    if let Some(argument) = extra_args
        .iter()
        .find(|argument| argument.contains('\0') || argument.contains('\n'))
    {
        return failure(call, format!("invalid extra argument `{argument}`"));
    }

    let mut spec = CommandSpec::new("cargo", context.sandbox.root(), CARGO_TIMEOUT_MS)
        .arg(subcommand)
        .arg("--message-format=short")
        .env("CARGO_TERM_COLOR", "never")
        .env("RUST_BACKTRACE", "0");
    if let Some(package) = &package {
        spec = spec.arg("--package").arg(package);
    }
    if let Some(manifest_path) = &manifest_path {
        match context.sandbox.resolve_existing(manifest_path) {
            Ok(path) => spec = spec.arg("--manifest-path").arg(path.display().to_string()),
            Err(error) => return failure(call, error.to_string()),
        }
    }
    if let Some(test_name) = test_name.filter(|value| !value.is_empty()) {
        spec = spec.arg(test_name);
    }
    spec = spec.args(extra_args.iter().cloned());

    let display = format!("cargo {} {}", spec.args.join(" "), "");
    let outcome = crate::process::run(&spec);
    outcome_to_result(call, &display, &outcome)
}

fn outcome_to_result(call: &ToolCall, display: &str, outcome: &CommandOutcome) -> ToolResult {
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
        result.error = Some(format!("command timed out: {display}"));
    } else if !outcome.is_success() {
        result.error = Some(format!(
            "command exited with code {}: {display}",
            outcome
                .exit_code
                .map_or_else(|| "unknown".to_owned(), |code| code.to_string())
        ));
    }
    result
        .metadata
        .insert("command".to_owned(), json!(display.trim()));
    result
        .metadata
        .insert("duration_ms".to_owned(), json!(outcome.duration_ms));
    result
}
