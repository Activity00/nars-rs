//! `search`: bounded literal text search over workspace files.

use nars_agent_ir::{SideEffect, ToolCall, ToolManifest, ToolResult};
use serde_json::json;
use walkdir::WalkDir;

use crate::builtin::{failure, opt_bool, opt_string, opt_u64};
use crate::manifest::{
    Tool, ToolContext, boolean_schema, integer_schema, object_schema, string_schema,
};

const IGNORED_DIRECTORIES: &[&str] = &[".git", "target", "node_modules", ".next", "dist", ".turbo"];
const BINARY_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "ico", "pdf", "zip", "gz", "tar", "bz2", "xz", "7z", "so",
    "dylib", "dll", "exe", "bin", "o", "a", "rlib", "rmeta", "woff", "woff2", "ttf", "otf", "mp3",
    "mp4", "mov", "avi", "wasm", "class", "jar",
];
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Searches for a literal substring.
#[derive(Debug, Default, Clone, Copy)]
pub struct SearchTool;

impl Tool for SearchTool {
    fn manifest(&self) -> &ToolManifest {
        static MANIFEST: std::sync::OnceLock<ToolManifest> = std::sync::OnceLock::new();
        MANIFEST.get_or_init(|| ToolManifest {
            name: "search".to_owned(),
            description: "Search workspace text files for a literal string. Returns \
                          `path:line: text` matches, capped by max_results."
                .to_owned(),
            input_schema: object_schema(
                json!({
                    "query": string_schema("Literal text to find."),
                    "path": string_schema("Workspace-relative directory (default '.')."),
                    "case_sensitive": boolean_schema("Match case exactly (default false)."),
                    "max_results": integer_schema("Maximum matches (default 100).", 1, 1000),
                    "file_glob": string_schema("Optional filename glob such as '*.rs'.")
                }),
                &["query"],
            ),
            side_effect: SideEffect::ReadOnly,
            timeout_ms: 30_000,
            requires_approval: false,
            version: "0.1.0".to_owned(),
            tags: vec!["builtin".to_owned(), "search".to_owned()],
        })
    }

    fn execute(&self, call: &ToolCall, context: &ToolContext) -> ToolResult {
        let query = match opt_string(&call.arguments, "query") {
            Ok(Some(query)) if !query.is_empty() => query,
            Ok(_) => return failure(call, "missing required string `query`"),
            Err(error) => return failure(call, error),
        };
        if query.chars().count() > 500 {
            return failure(call, "`query` is limited to 500 characters");
        }
        let path = match opt_string(&call.arguments, "path") {
            Ok(Some(path)) if !path.is_empty() => path,
            Ok(_) => ".".to_owned(),
            Err(error) => return failure(call, error),
        };
        let case_sensitive = match opt_bool(&call.arguments, "case_sensitive") {
            Ok(value) => value.unwrap_or(false),
            Err(error) => return failure(call, error),
        };
        let max_results = match opt_u64(&call.arguments, "max_results") {
            Ok(value) => value.unwrap_or(100).clamp(1, 1000),
            Err(error) => return failure(call, error),
        };
        let file_glob = match opt_string(&call.arguments, "file_glob") {
            Ok(value) => value,
            Err(error) => return failure(call, error),
        };

        let resolved = match context.sandbox.resolve_existing(&path) {
            Ok(path) => path,
            Err(error) => return failure(call, error.to_string()),
        };
        let needle = if case_sensitive {
            query.clone()
        } else {
            query.to_lowercase()
        };

        let mut matches = Vec::new();
        let mut scanned = 0_u64;
        let mut skipped_binary = 0_u64;
        'walk: for entry in WalkDir::new(&resolved)
            .follow_links(false)
            .sort_by_file_name()
        {
            let Ok(entry) = entry else {
                continue;
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            if entry.path().components().any(|component| {
                IGNORED_DIRECTORIES.contains(&component.as_os_str().to_string_lossy().as_ref())
            }) {
                continue;
            }
            if let Some(glob) = &file_glob
                && !matches_glob(&name, glob)
            {
                continue;
            }
            if let Some(extension) = entry.path().extension().and_then(|value| value.to_str())
                && BINARY_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
            {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.len() > MAX_FILE_BYTES {
                continue;
            }
            let Ok(bytes) = std::fs::read(entry.path()) else {
                continue;
            };
            if bytes.iter().take(8192).any(|byte| *byte == 0) {
                skipped_binary += 1;
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            scanned += 1;
            let relative = context.sandbox.display(entry.path());
            for (index, line) in text.lines().enumerate() {
                let haystack = if case_sensitive {
                    line.to_owned()
                } else {
                    line.to_lowercase()
                };
                if haystack.contains(&needle) {
                    let trimmed = line.trim_end();
                    let shown = if trimmed.chars().count() > 400 {
                        let mut clipped = trimmed.chars().take(400).collect::<String>();
                        clipped.push('…');
                        clipped
                    } else {
                        trimmed.to_owned()
                    };
                    matches.push(format!("{relative}:{}: {shown}", index + 1));
                    if matches.len() >= usize::try_from(max_results).unwrap_or(100) {
                        break 'walk;
                    }
                }
            }
        }

        let mut result = ToolResult::success(call.id.clone(), matches.join("\n"));
        result.metadata.insert("query".to_owned(), json!(query));
        result
            .metadata
            .insert("matches".to_owned(), json!(matches.len()));
        result
            .metadata
            .insert("files_scanned".to_owned(), json!(scanned));
        result
            .metadata
            .insert("binary_skipped".to_owned(), json!(skipped_binary));
        result.metadata.insert(
            "truncated".to_owned(),
            json!(matches.len() >= usize::try_from(max_results).unwrap_or(100)),
        );
        result
    }
}

fn matches_glob(name: &str, pattern: &str) -> bool {
    match pattern.split_once('*') {
        Some((prefix, suffix)) => {
            name.starts_with(prefix)
                && name.ends_with(suffix)
                && name.len() >= prefix.len() + suffix.len()
        }
        None => name == pattern,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matching_is_conservative() {
        assert!(matches_glob("main.rs", "*.rs"));
        assert!(matches_glob("lib.rs", "lib*"));
        assert!(!matches_glob("lib.rs", "*.toml"));
        assert!(matches_glob("exact", "exact"));
    }
}
