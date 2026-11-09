//! The tool broker: policy, sandbox, timeout, and output-bounding boundary.

use std::sync::Arc;
use std::time::{Duration, Instant};

use nars_agent_ir::{SideEffect, ToolCall, ToolManifest, ToolPreview, ToolResult, ToolStatus};
use tracing::warn;

use crate::builtin;
use crate::manifest::{ToolContext, ToolRegistry};
use crate::policy::{Policy, PolicyDecision};
use crate::sandbox::Sandbox;
use crate::truncate::truncate_bytes;

/// Configuration for the broker.
#[derive(Clone, Debug)]
pub struct BrokerConfig {
    /// Maximum bytes returned per stream.
    pub max_output_bytes: usize,
}

impl Default for BrokerConfig {
    fn default() -> Self {
        Self {
            max_output_bytes: 64 * 1024,
        }
    }
}

/// Executes validated tool calls under policy and sandbox control.
#[derive(Clone)]
pub struct ToolBroker {
    registry: Arc<ToolRegistry>,
    policy: Policy,
    sandbox: Sandbox,
    config: BrokerConfig,
}

impl std::fmt::Debug for ToolBroker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolBroker")
            .field("tools", &self.registry.names())
            .field("sandbox", &self.sandbox.root())
            .finish_non_exhaustive()
    }
}

impl ToolBroker {
    /// Builds a broker.
    #[must_use]
    pub fn new(
        registry: ToolRegistry,
        policy: Policy,
        sandbox: Sandbox,
        config: BrokerConfig,
    ) -> Self {
        Self {
            registry: Arc::new(registry),
            policy,
            sandbox,
            config,
        }
    }

    /// Builds a broker with the built-in tools and default policy.
    ///
    /// # Errors
    ///
    /// Returns sandbox errors when the workspace cannot be canonicalized.
    pub fn with_builtin_tools(
        workspace: impl AsRef<std::path::Path>,
        policy: Policy,
    ) -> Result<Self, crate::sandbox::SandboxError> {
        let sandbox = Sandbox::new(workspace)?;
        Ok(Self::new(
            builtin::builtin_registry(),
            policy,
            sandbox,
            BrokerConfig::default(),
        ))
    }

    /// The sandbox.
    #[must_use]
    pub fn sandbox(&self) -> &Sandbox {
        &self.sandbox
    }

    /// The policy.
    #[must_use]
    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// All tool manifests.
    #[must_use]
    pub fn manifests(&self) -> Vec<ToolManifest> {
        self.registry.manifests()
    }

    /// All tool names.
    #[must_use]
    pub fn names(&self) -> std::collections::BTreeSet<String> {
        self.registry.names()
    }

    /// Evaluates the policy for a call without executing it.
    #[must_use]
    pub fn evaluate(&self, call: &ToolCall) -> PolicyDecision {
        self.policy.evaluate(call)
    }

    /// Produces a human-readable preview for calls that benefit from one.
    #[must_use]
    pub fn preview(&self, call: &ToolCall) -> Option<ToolPreview> {
        let manifest = self.registry.get(&call.tool)?.manifest().clone();
        if manifest.name == "run_command" {
            let command = call.arguments.get("command")?.as_str()?;
            let cwd = call
                .arguments
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(".");
            return Some(ToolPreview {
                summary: format!("$ {command}"),
                unified_diff: None,
                files: if cwd == "." {
                    Vec::new()
                } else {
                    vec![cwd.to_owned()]
                },
            });
        }
        if manifest.name != "apply_patch" {
            return None;
        }
        let context = self.tool_context();
        let diffs = builtin::apply_patch::preview(call, &context)?;
        let unified = diffs
            .iter()
            .map(|diff| diff.unified.clone())
            .collect::<Vec<_>>()
            .join("\n");
        Some(ToolPreview {
            summary: format!(
                "{} file(s), +{} -{} lines",
                diffs.len(),
                diffs.iter().map(|diff| diff.added).sum::<u32>(),
                diffs.iter().map(|diff| diff.removed).sum::<u32>(),
            ),
            unified_diff: Some(unified),
            files: diffs.into_iter().map(|diff| diff.path).collect(),
        })
    }

    /// Executes one call. The caller must already have satisfied approval.
    pub async fn execute(&self, call: ToolCall) -> ToolResult {
        let decision = self.policy.evaluate(&call);
        if let crate::policy::PolicyDecision::Deny { reason } = decision {
            return ToolResult::denied(call.id.clone(), reason);
        }
        let Some(tool) = self.registry.get(&call.tool) else {
            return ToolResult::failure(call.id.clone(), format!("unknown tool `{}`", call.tool));
        };
        let manifest = tool.manifest().clone();
        let context = self.tool_context();
        let timeout = Duration::from_millis(manifest.timeout_ms.max(1));
        let task_call = call.clone();
        let started = Instant::now();
        let join = tokio::task::spawn_blocking(move || tool.execute(&task_call, &context));
        let result = match tokio::time::timeout(timeout, join).await {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                ToolResult::failure(call.id.clone(), format!("tool task panicked: {error}"))
            }
            Err(_) => {
                warn!(tool = %call.tool, timeout_ms = manifest.timeout_ms, "tool timed out");
                let mut result = ToolResult::failure(
                    call.id.clone(),
                    format!("tool `{}` exceeded {} ms", call.tool, manifest.timeout_ms),
                );
                result.status = ToolStatus::TimedOut;
                result
            }
        };
        self.finalize(call, result, started)
    }

    fn tool_context(&self) -> ToolContext {
        ToolContext {
            sandbox: self.sandbox.clone(),
            max_output_bytes: self.config.max_output_bytes,
        }
    }

    fn finalize(&self, call: ToolCall, mut result: ToolResult, started: Instant) -> ToolResult {
        result.call_id = call.id;
        result.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (stdout, stdout_truncated) =
            truncate_bytes(&result.stdout, self.config.max_output_bytes);
        let (stderr, stderr_truncated) =
            truncate_bytes(&result.stderr, self.config.max_output_bytes);
        result.stdout = stdout;
        result.stderr = stderr;
        result.truncated |= stdout_truncated || stderr_truncated;
        result
    }
}

/// Builds a validated call from a manifest and structured arguments.
#[must_use]
pub fn build_call(
    id: nars_agent_ir::ToolCallId,
    run_id: nars_agent_ir::RunId,
    manifest: &ToolManifest,
    arguments: serde_json::Value,
    created_at_ms: i64,
) -> ToolCall {
    ToolCall {
        id,
        run_id,
        tool: manifest.name.clone(),
        arguments,
        side_effect: manifest.side_effect,
        requires_approval: manifest.requires_approval
            || manifest.side_effect.risk() >= SideEffect::NetworkAccess.risk(),
        created_at_ms,
    }
}
