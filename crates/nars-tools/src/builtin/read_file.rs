//! `read_file`: sandboxed UTF-8 file reading.

use nars_agent_ir::{SideEffect, ToolCall, ToolManifest, ToolResult};
use serde_json::json;

use crate::builtin::{failure, opt_u64, req_string};
use crate::manifest::{
    Tool, ToolContext, boolean_schema, integer_schema, object_schema, string_schema,
};
use crate::truncate::truncate_bytes;

/// Reads a file inside the workspace.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReadFileTool;

impl Tool for ReadFileTool {
    fn manifest(&self) -> &ToolManifest {
        static MANIFEST: std::sync::OnceLock<ToolManifest> = std::sync::OnceLock::new();
        MANIFEST.get_or_init(|| ToolManifest {
            name: "read_file".to_owned(),
            description: "Read a UTF-8 text file from the workspace. Returns at most 64 KiB; \
                          use offset and limit to page through larger files."
                .to_owned(),
            input_schema: object_schema(
                json!({
                    "path": string_schema("Workspace-relative file path."),
                    "offset": integer_schema("1-based first line to return.", 1, 1_000_000),
                    "limit": integer_schema("Maximum number of lines to return.", 1, 10_000),
                    "numbered": boolean_schema("Prefix each line with its 1-based line number.")
                }),
                &["path"],
            ),
            side_effect: SideEffect::ReadOnly,
            timeout_ms: 10_000,
            requires_approval: false,
            version: "0.1.0".to_owned(),
            tags: vec!["builtin".to_owned(), "filesystem".to_owned()],
        })
    }

    fn execute(&self, call: &ToolCall, context: &ToolContext) -> ToolResult {
        let path = match req_string(&call.arguments, "path") {
            Ok(path) => path,
            Err(error) => return failure(call, error),
        };
        let offset = match opt_u64(&call.arguments, "offset") {
            Ok(value) => value.unwrap_or(1).max(1),
            Err(error) => return failure(call, error),
        };
        let limit = match opt_u64(&call.arguments, "limit") {
            Ok(value) => value.unwrap_or(10_000).clamp(1, 10_000),
            Err(error) => return failure(call, error),
        };
        let numbered = matches!(
            call.arguments
                .get("numbered")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );

        let resolved = match context.sandbox.resolve_existing(&path) {
            Ok(path) => path,
            Err(error) => return failure(call, error.to_string()),
        };
        if !resolved.is_file() {
            return failure(call, format!("`{path}` is not a regular file"));
        }
        let bytes = match std::fs::read(&resolved) {
            Ok(bytes) => bytes,
            Err(error) => return failure(call, format!("failed to read `{path}`: {error}")),
        };
        if bytes.len() > 16 * 1024 * 1024 {
            return failure(
                call,
                format!("`{path}` is larger than the 16 MiB read limit"),
            );
        }
        if bytes.contains(&0) {
            return failure(call, format!("`{path}` appears to be binary"));
        }
        let text = String::from_utf8_lossy(&bytes);
        let total_lines = text.lines().count();
        let selected = text
            .lines()
            .skip(usize::try_from(offset.saturating_sub(1)).unwrap_or(usize::MAX))
            .take(usize::try_from(limit).unwrap_or(usize::MAX))
            .enumerate()
            .map(|(index, line)| {
                if numbered {
                    format!(
                        "{:>5} | {line}",
                        usize::try_from(offset).unwrap_or(1) + index
                    )
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let (output, truncated) = truncate_bytes(&selected, context.max_output_bytes);
        let mut result = ToolResult::success(call.id.clone(), output);
        result.metadata.insert("path".to_owned(), json!(path));
        result
            .metadata
            .insert("bytes".to_owned(), json!(bytes.len()));
        result
            .metadata
            .insert("total_lines".to_owned(), json!(total_lines));
        result
            .metadata
            .insert("returned_lines".to_owned(), json!(selected.lines().count()));
        result.metadata.insert("offset".to_owned(), json!(offset));
        result
            .metadata
            .insert("truncated".to_owned(), json!(truncated));
        result
    }
}
