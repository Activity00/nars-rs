//! Approval policy and command risk classification.

use std::collections::BTreeSet;

use nars_agent_ir::{SideEffect, ToolCall};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How the runtime handles calls that require approval.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// Emit an approval request and wait for the user.
    #[default]
    Ask,
    /// Automatically approve every call the policy would otherwise ask about.
    AutoApprove,
    /// Automatically deny every call the policy would otherwise ask about.
    Deny,
}

/// Configuration for the approval policy.
#[derive(Clone, Debug)]
pub struct PolicyConfig {
    /// Global approval mode.
    pub mode: ApprovalMode,
    /// Tools whose process execution is considered routine.
    pub trusted_process_tools: BTreeSet<String>,
    /// Command prefixes that may run without approval.
    pub trusted_command_prefixes: Vec<String>,
    /// Substrings that always cause a denial.
    pub denied_command_patterns: Vec<String>,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            mode: ApprovalMode::Ask,
            trusted_process_tools: ["cargo_check", "cargo_test", "git_diff", "git_status"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            trusted_command_prefixes: vec![
                "cargo check".to_owned(),
                "cargo test".to_owned(),
                "cargo build".to_owned(),
                "cargo clippy".to_owned(),
                "cargo fmt --check".to_owned(),
                "git diff".to_owned(),
                "git status".to_owned(),
                "git log".to_owned(),
            ],
            denied_command_patterns: vec![
                "sudo ".to_owned(),
                "rm -rf /".to_owned(),
                "curl | sh".to_owned(),
                "curl | bash".to_owned(),
                "wget | sh".to_owned(),
                ":(){".to_owned(),
                "mkfs".to_owned(),
                "shutdown".to_owned(),
                "reboot".to_owned(),
                "> /dev/sda".to_owned(),
            ],
        }
    }
}

/// The policy verdict for one call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PolicyDecision {
    /// Execute immediately.
    Allow {
        /// Explanation for the audit trail.
        reason: String,
    },
    /// Wait for explicit user approval.
    RequireApproval {
        /// Explanation shown to the user.
        reason: String,
    },
    /// Refuse the call.
    Deny {
        /// Explanation returned to the model.
        reason: String,
    },
}

impl PolicyDecision {
    /// Whether execution may proceed immediately.
    #[must_use]
    pub const fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow { .. })
    }

    /// Whether the user must approve.
    #[must_use]
    pub const fn requires_approval(&self) -> bool {
        matches!(self, Self::RequireApproval { .. })
    }

    /// Whether the call is denied.
    #[must_use]
    pub const fn is_denied(&self) -> bool {
        matches!(self, Self::Deny { .. })
    }

    /// The explanation string.
    #[must_use]
    pub fn reason(&self) -> &str {
        match self {
            Self::Allow { reason } | Self::RequireApproval { reason } | Self::Deny { reason } => {
                reason
            }
        }
    }
}

/// Stateless policy evaluator.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    config: PolicyConfig,
}

impl Policy {
    /// Creates a policy from configuration.
    #[must_use]
    pub fn new(config: PolicyConfig) -> Self {
        Self { config }
    }

    /// The active configuration.
    #[must_use]
    pub fn config(&self) -> &PolicyConfig {
        &self.config
    }

    /// Evaluates one call.
    #[must_use]
    pub fn evaluate(&self, call: &ToolCall) -> PolicyDecision {
        let base = self.classify(call);
        match (self.config.mode, base) {
            (ApprovalMode::AutoApprove, PolicyDecision::RequireApproval { reason }) => {
                PolicyDecision::Allow {
                    reason: format!("auto-approved: {reason}"),
                }
            }
            (ApprovalMode::Deny, PolicyDecision::RequireApproval { reason }) => {
                PolicyDecision::Deny {
                    reason: format!("approval mode is deny: {reason}"),
                }
            }
            (_, decision) => decision,
        }
    }

    fn classify(&self, call: &ToolCall) -> PolicyDecision {
        if let Some(command) = call.arguments.get("command").and_then(Value::as_str) {
            let lowered = command.to_ascii_lowercase();
            if let Some(pattern) = self
                .config
                .denied_command_patterns
                .iter()
                .find(|pattern| lowered.contains(&pattern.to_ascii_lowercase()))
            {
                return PolicyDecision::Deny {
                    reason: format!("command matches denied pattern `{pattern}`"),
                };
            }
            if self
                .config
                .trusted_command_prefixes
                .iter()
                .any(|prefix| command.trim_start().starts_with(prefix))
            {
                return PolicyDecision::Allow {
                    reason: "command matches the trusted prefix list".to_owned(),
                };
            }
        }

        match call.side_effect {
            SideEffect::ReadOnly => PolicyDecision::Allow {
                reason: "read-only tool".to_owned(),
            },
            SideEffect::WorkspaceWrite => {
                if call.requires_approval {
                    PolicyDecision::RequireApproval {
                        reason: "workspace writes require a diff review".to_owned(),
                    }
                } else {
                    PolicyDecision::Allow {
                        reason: "workspace write tool is pre-approved".to_owned(),
                    }
                }
            }
            SideEffect::ProcessExecution => {
                if self.config.trusted_process_tools.contains(&call.tool) {
                    PolicyDecision::Allow {
                        reason: format!("`{}` is a trusted process tool", call.tool),
                    }
                } else {
                    PolicyDecision::RequireApproval {
                        reason: "process execution requires approval".to_owned(),
                    }
                }
            }
            SideEffect::NetworkAccess => PolicyDecision::RequireApproval {
                reason: "network access requires approval".to_owned(),
            },
            SideEffect::GitHistoryChange => PolicyDecision::RequireApproval {
                reason: "git history changes require approval".to_owned(),
            },
            SideEffect::ExternalSideEffect => PolicyDecision::RequireApproval {
                reason: "external side effects always require approval".to_owned(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nars_agent_ir::{RunId, ToolCallId};
    use serde_json::json;

    fn call(
        tool: &str,
        side_effect: SideEffect,
        requires_approval: bool,
        arguments: Value,
    ) -> ToolCall {
        ToolCall {
            id: ToolCallId::from_static("call_1"),
            run_id: RunId::from_static("run_1"),
            tool: tool.to_owned(),
            arguments,
            side_effect,
            requires_approval,
            created_at_ms: 0,
        }
    }

    #[test]
    fn read_only_is_allowed() {
        let policy = Policy::default();
        let decision = policy.evaluate(&call("read_file", SideEffect::ReadOnly, false, json!({})));
        assert!(decision.is_allowed());
    }

    #[test]
    fn patch_requires_approval() {
        let policy = Policy::default();
        let decision = policy.evaluate(&call(
            "apply_patch",
            SideEffect::WorkspaceWrite,
            true,
            json!({}),
        ));
        assert!(decision.requires_approval());
    }

    #[test]
    fn auto_approve_mode_allows_patch() {
        let policy = Policy::new(PolicyConfig {
            mode: ApprovalMode::AutoApprove,
            ..PolicyConfig::default()
        });
        let decision = policy.evaluate(&call(
            "apply_patch",
            SideEffect::WorkspaceWrite,
            true,
            json!({}),
        ));
        assert!(decision.is_allowed());
    }

    #[test]
    fn deny_mode_denies_patch() {
        let policy = Policy::new(PolicyConfig {
            mode: ApprovalMode::Deny,
            ..PolicyConfig::default()
        });
        let decision = policy.evaluate(&call(
            "apply_patch",
            SideEffect::WorkspaceWrite,
            true,
            json!({}),
        ));
        assert!(decision.is_denied());
    }

    #[test]
    fn trusted_cargo_command_is_allowed() {
        let policy = Policy::default();
        let decision = policy.evaluate(&call(
            "shell",
            SideEffect::ProcessExecution,
            true,
            json!({"command": "cargo check -p nars-core"}),
        ));
        assert!(decision.is_allowed());
    }

    #[test]
    fn dangerous_command_is_denied() {
        let policy = Policy::default();
        let decision = policy.evaluate(&call(
            "shell",
            SideEffect::ProcessExecution,
            true,
            json!({"command": "sudo rm -rf /"}),
        ));
        assert!(decision.is_denied());
    }
}
