//! Tool trait, registry, and schema helpers.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use nars_agent_ir::{ToolCall, ToolManifest, ToolResult};
use serde_json::{Value, json};
use thiserror::Error;

use crate::sandbox::Sandbox;

/// Runtime services available to a tool.
#[derive(Clone, Debug)]
pub struct ToolContext {
    /// Workspace sandbox.
    pub sandbox: Sandbox,
    /// Maximum bytes a tool should return in a single stream.
    pub max_output_bytes: usize,
}

impl ToolContext {
    /// Builds a context for a workspace.
    #[must_use]
    pub fn new(sandbox: Sandbox) -> Self {
        Self {
            sandbox,
            max_output_bytes: 64 * 1024,
        }
    }
}

/// A synchronous tool implementation.
///
/// Tools may perform blocking file or process I/O. The broker always calls them
/// on a blocking worker thread, so implementations must not spawn their own
/// async runtime.
pub trait Tool: Send + Sync {
    /// Declarative contract.
    fn manifest(&self) -> &ToolManifest;

    /// Executes one validated call.
    fn execute(&self, call: &ToolCall, context: &ToolContext) -> ToolResult;
}

/// Registry errors.
#[derive(Debug, Error)]
pub enum RegistryError {
    /// A tool with the same name is already registered.
    #[error("tool `{0}` is already registered")]
    Duplicate(String),
    /// The tool name is empty or contains invalid characters.
    #[error("invalid tool name: {0}")]
    InvalidName(String),
}

/// A name-indexed tool registry.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolRegistry")
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ToolRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a tool.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate or invalid names.
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), RegistryError> {
        let name = tool.manifest().name.clone();
        if name.is_empty()
            || !name
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            return Err(RegistryError::InvalidName(name));
        }
        if self.tools.contains_key(&name) {
            return Err(RegistryError::Duplicate(name));
        }
        self.tools.insert(name, tool);
        Ok(())
    }

    /// Looks up a tool by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Lists manifests in deterministic name order.
    #[must_use]
    pub fn manifests(&self) -> Vec<ToolManifest> {
        self.tools
            .values()
            .map(|tool| tool.manifest().clone())
            .collect()
    }

    /// Lists tool names.
    #[must_use]
    pub fn names(&self) -> BTreeSet<String> {
        self.tools.keys().cloned().collect()
    }

    /// Number of registered tools.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// Builds a JSON Schema object description.
#[must_use]
pub fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

/// A string schema with an optional description.
#[must_use]
pub fn string_schema(description: &str) -> Value {
    json!({"type": "string", "description": description})
}

/// An integer schema with bounds and an optional description.
#[must_use]
pub fn integer_schema(description: &str, minimum: i64, maximum: i64) -> Value {
    json!({
        "type": "integer",
        "description": description,
        "minimum": minimum,
        "maximum": maximum
    })
}

/// A boolean schema with a description.
#[must_use]
pub fn boolean_schema(description: &str) -> Value {
    json!({"type": "boolean", "description": description})
}

/// An array-of-strings schema.
#[must_use]
pub fn string_array_schema(description: &str) -> Value {
    json!({
        "type": "array",
        "description": description,
        "items": {"type": "string"}
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nars_agent_ir::SideEffect;

    struct EchoTool {
        manifest: ToolManifest,
    }

    impl Tool for EchoTool {
        fn manifest(&self) -> &ToolManifest {
            &self.manifest
        }

        fn execute(&self, call: &ToolCall, _context: &ToolContext) -> ToolResult {
            ToolResult::success(call.id.clone(), "echo")
        }
    }

    fn echo(name: &str) -> Arc<dyn Tool> {
        Arc::new(EchoTool {
            manifest: ToolManifest::new(
                name,
                "echo",
                object_schema(json!({}), &[]),
                SideEffect::ReadOnly,
            ),
        })
    }

    #[test]
    fn registry_rejects_duplicates_and_bad_names() {
        let mut registry = ToolRegistry::new();
        registry.register(echo("one")).unwrap();
        assert!(matches!(
            registry.register(echo("one")),
            Err(RegistryError::Duplicate(_))
        ));
        assert!(matches!(
            registry.register(echo("bad name")),
            Err(RegistryError::InvalidName(_))
        ));
    }

    #[test]
    fn registry_lists_in_name_order() {
        let mut registry = ToolRegistry::new();
        registry.register(echo("b")).unwrap();
        registry.register(echo("a")).unwrap();
        assert_eq!(registry.names().into_iter().collect::<Vec<_>>(), ["a", "b"]);
    }
}
