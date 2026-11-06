//! `list_files`: bounded, deterministic workspace listing.

use nars_agent_ir::{SideEffect, ToolCall, ToolManifest, ToolResult};
use serde_json::json;
use walkdir::WalkDir;

use crate::builtin::{failure, opt_bool, opt_string, opt_u64};
use crate::manifest::{
    Tool, ToolContext, boolean_schema, integer_schema, object_schema, string_schema,
};

const IGNORED_DIRECTORIES: &[&str] = &[".git", "target", "node_modules", ".next", "dist", ".turbo"];

/// Lists files and directories under a workspace path.
#[derive(Debug, Default, Clone, Copy)]
pub struct ListFilesTool;

impl Tool for ListFilesTool {
    fn manifest(&self) -> &ToolManifest {
        static MANIFEST: std::sync::OnceLock<ToolManifest> = std::sync::OnceLock::new();
        MANIFEST.get_or_init(|| ToolManifest {
            name: "list_files".to_owned(),
            description: "List files and directories inside the workspace. Build and dependency \
                 directories are skipped by default."
                .to_owned(),
            input_schema: object_schema(
                json!({
                    "path": string_schema("Workspace-relative directory (default '.')."),
                    "depth": integer_schema("Maximum directory depth (default 3).", 1, 10),
                    "limit": integer_schema("Maximum entries to return (default 500).", 1, 5000),
                    "include_hidden": boolean_schema("Include dotfiles and dot-directories.")
                }),
                &[],
            ),
            side_effect: SideEffect::ReadOnly,
            timeout_ms: 15_000,
            requires_approval: false,
            version: "0.1.0".to_owned(),
            tags: vec!["builtin".to_owned(), "filesystem".to_owned()],
        })
    }

    fn execute(&self, call: &ToolCall, context: &ToolContext) -> ToolResult {
        let path = match opt_string(&call.arguments, "path") {
            Ok(Some(path)) if !path.is_empty() => path,
            Ok(_) => ".".to_owned(),
            Err(error) => return failure(call, error),
        };
        let depth = match opt_u64(&call.arguments, "depth") {
            Ok(value) => value.unwrap_or(3).clamp(1, 10),
            Err(error) => return failure(call, error),
        };
        let limit = match opt_u64(&call.arguments, "limit") {
            Ok(value) => value.unwrap_or(500).clamp(1, 5000),
            Err(error) => return failure(call, error),
        };
        let include_hidden = match opt_bool(&call.arguments, "include_hidden") {
            Ok(value) => value.unwrap_or(false),
            Err(error) => return failure(call, error),
        };

        let resolved = match context.sandbox.resolve_existing(&path) {
            Ok(path) => path,
            Err(error) => return failure(call, error.to_string()),
        };

        let mut entries = Vec::new();
        let mut truncated = false;
        let mut walker = WalkDir::new(&resolved)
            .max_depth(usize::try_from(depth).unwrap_or(3) + 1)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter();
        loop {
            let Some(entry) = walker.next() else {
                break;
            };
            let Ok(entry) = entry else {
                continue;
            };
            if entry.path() == resolved {
                continue;
            }
            let name = entry.file_name().to_string_lossy();
            if !include_hidden && name.starts_with('.') {
                if entry.file_type().is_dir() {
                    walker.skip_current_dir();
                }
                continue;
            }
            if entry.file_type().is_dir() && IGNORED_DIRECTORIES.contains(&name.as_ref()) {
                walker.skip_current_dir();
                continue;
            }
            if entries.len() >= usize::try_from(limit).unwrap_or(500) {
                truncated = true;
                break;
            }
            let display = context.sandbox.display(entry.path());
            if entry.file_type().is_dir() {
                entries.push(format!("{display}/"));
            } else {
                entries.push(display);
            }
        }

        let mut result = ToolResult::success(call.id.clone(), entries.join("\n"));
        result.metadata.insert("path".to_owned(), json!(path));
        result
            .metadata
            .insert("entries".to_owned(), json!(entries.len()));
        result
            .metadata
            .insert("truncated".to_owned(), json!(truncated));
        result
    }
}
