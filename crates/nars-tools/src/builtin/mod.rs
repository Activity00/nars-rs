//! Built-in tools and the default registry.

pub(crate) mod apply_patch;
mod cargo;
mod list_files;
mod read_file;
mod search;

use std::sync::Arc;

use nars_agent_ir::{ToolCall, ToolResult};
use serde_json::Value;

pub use apply_patch::ApplyPatchTool;
pub use cargo::{CargoCheckTool, CargoTestTool};
pub use list_files::ListFilesTool;
pub use read_file::ReadFileTool;
pub use search::SearchTool;

use crate::manifest::{Tool, ToolRegistry};

/// Builds the default registry of built-in tools.
///
/// # Panics
///
/// Panics only when the built-in tool set contains a duplicate name, which is a
/// programming error covered by the crate's tests.
#[must_use]
pub fn builtin_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    for tool in [
        Arc::new(ReadFileTool) as Arc<dyn Tool>,
        Arc::new(ListFilesTool),
        Arc::new(SearchTool),
        Arc::new(ApplyPatchTool),
        Arc::new(CargoCheckTool),
        Arc::new(CargoTestTool),
    ] {
        registry
            .register(tool)
            .expect("built-in tool names are unique");
    }
    registry
}

pub(crate) fn opt_string(arguments: &Value, key: &str) -> Result<Option<String>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

pub(crate) fn req_string(arguments: &Value, key: &str) -> Result<String, String> {
    opt_string(arguments, key)?.ok_or_else(|| format!("missing required string `{key}`"))
}

pub(crate) fn opt_u64(arguments: &Value, key: &str) -> Result<Option<u64>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be a non-negative integer")),
        Some(_) => Err(format!("`{key}` must be a non-negative integer")),
    }
}

pub(crate) fn opt_bool(arguments: &Value, key: &str) -> Result<Option<bool>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("`{key}` must be a boolean")),
    }
}

pub(crate) fn string_list(arguments: &Value, key: &str) -> Result<Vec<String>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("`{key}` must be an array of strings"))
            })
            .collect(),
        Some(_) => Err(format!("`{key}` must be an array of strings")),
    }
}

pub(crate) fn failure(call: &ToolCall, message: impl Into<String>) -> ToolResult {
    ToolResult::failure(call.id.clone(), message)
}
