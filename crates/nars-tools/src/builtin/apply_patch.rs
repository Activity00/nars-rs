//! `apply_patch`: atomic, sandboxed exact-text replacement with diff preview.

use std::collections::BTreeMap;
use std::path::PathBuf;

use nars_agent_ir::{Diff, SideEffect, ToolCall, ToolManifest, ToolResult};
use serde_json::{Value, json};

use crate::builtin::{failure, opt_bool, opt_string};
use crate::diff::unified_diff;
use crate::manifest::{Tool, ToolContext, boolean_schema, object_schema, string_schema};

const MAX_PATCHES: usize = 50;
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Applies exact-text replacements to workspace files.
#[derive(Debug, Default, Clone, Copy)]
pub struct ApplyPatchTool;

#[derive(Debug, Clone)]
struct Edit {
    path: String,
    old_text: String,
    new_text: String,
    replace_all: bool,
}

impl Tool for ApplyPatchTool {
    fn manifest(&self) -> &ToolManifest {
        static MANIFEST: std::sync::OnceLock<ToolManifest> = std::sync::OnceLock::new();
        MANIFEST.get_or_init(|| ToolManifest {
            name: "apply_patch".to_owned(),
            description: "Modify workspace files by exact text replacement. Provide either a \
                          top-level path/old_text/new_text triple, or a `patches` array of such \
                          triples. Use an empty old_text to create a new file. Returns a unified \
                          diff; supports dry_run for previews."
                .to_owned(),
            input_schema: object_schema(
                json!({
                    "path": string_schema("Workspace-relative file path."),
                    "old_text": string_schema("Exact text to replace; empty to create a file."),
                    "new_text": string_schema("Replacement text."),
                    "replace_all": boolean_schema("Replace every occurrence instead of requiring exactly one."),
                    "patches": json!({
                        "type": "array",
                        "description": "Batch of edits applied atomically.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": {"type": "string"},
                                "old_text": {"type": "string"},
                                "new_text": {"type": "string"},
                                "replace_all": {"type": "boolean"}
                            },
                            "required": ["path", "old_text", "new_text"],
                            "additionalProperties": false
                        }
                    }),
                    "dry_run": boolean_schema("Compute the diff without writing files.")
                }),
                &[],
            ),
            side_effect: SideEffect::WorkspaceWrite,
            timeout_ms: 30_000,
            requires_approval: true,
            version: "0.1.0".to_owned(),
            tags: vec!["builtin".to_owned(), "filesystem".to_owned(), "patch".to_owned()],
        })
    }

    fn execute(&self, call: &ToolCall, context: &ToolContext) -> ToolResult {
        let dry_run = matches!(
            call.arguments.get("dry_run").and_then(Value::as_bool),
            Some(true)
        );
        let edits = match parse_edits(&call.arguments) {
            Ok(edits) => edits,
            Err(error) => return failure(call, error),
        };
        if edits.is_empty() {
            return failure(call, "no edits were provided");
        }
        if edits.len() > MAX_PATCHES {
            return failure(call, format!("at most {MAX_PATCHES} edits are accepted"));
        }

        let mut originals: BTreeMap<String, String> = BTreeMap::new();
        let mut working: BTreeMap<String, String> = BTreeMap::new();
        let mut targets: BTreeMap<String, PathBuf> = BTreeMap::new();

        for edit in &edits {
            if !working.contains_key(&edit.path) {
                let resolved = match context.sandbox.resolve_new(&edit.path) {
                    Ok(path) => path,
                    Err(error) => return failure(call, error.to_string()),
                };
                let content = if resolved.exists() {
                    if let Ok(metadata) = resolved.metadata()
                        && metadata.len() > MAX_FILE_BYTES
                    {
                        return failure(
                            call,
                            format!("`{}` is larger than the 16 MiB patch limit", edit.path),
                        );
                    }
                    match std::fs::read_to_string(&resolved) {
                        Ok(content) => content,
                        Err(error) => {
                            return failure(
                                call,
                                format!("failed to read `{}`: {error}", edit.path),
                            );
                        }
                    }
                } else {
                    String::new()
                };
                originals.insert(edit.path.clone(), content.clone());
                working.insert(edit.path.clone(), content);
                targets.insert(edit.path.clone(), resolved);
            }

            let current = working.get(&edit.path).cloned().unwrap_or_default();
            let existed = originals
                .get(&edit.path)
                .is_some_and(|value| !value.is_empty());
            let next = if edit.old_text.is_empty() {
                if existed {
                    return failure(
                        call,
                        format!(
                            "`{}` already exists; provide a non-empty old_text to modify it",
                            edit.path
                        ),
                    );
                }
                edit.new_text.clone()
            } else {
                let occurrences = current.matches(&edit.old_text).count();
                if occurrences == 0 {
                    return failure(
                        call,
                        format!(
                            "old_text was not found in `{}`; read the file and copy the exact text",
                            edit.path
                        ),
                    );
                }
                if occurrences > 1 && !edit.replace_all {
                    return failure(
                        call,
                        format!(
                            "old_text occurs {occurrences} times in `{}`; use replace_all or a longer unique snippet",
                            edit.path
                        ),
                    );
                }
                if edit.replace_all {
                    current.replace(&edit.old_text, &edit.new_text)
                } else {
                    current.replacen(&edit.old_text, &edit.new_text, 1)
                }
            };
            working.insert(edit.path.clone(), next);
        }

        let mut diffs = Vec::new();
        let mut changed_paths = Vec::new();
        for (path, next) in &working {
            let original = originals.get(path).cloned().unwrap_or_default();
            if &original == next {
                continue;
            }
            let resolved = targets
                .get(path)
                .expect("every changed path has a resolved target");
            let before_existed = resolved.exists() || !original.is_empty();
            diffs.push(unified_diff(path, &original, next, before_existed));
            changed_paths.push((path.clone(), resolved.clone(), next.clone()));
        }

        if !dry_run {
            for (_, resolved, next) in &changed_paths {
                if let Some(parent) = resolved.parent() {
                    let temp = parent.join(format!(
                        ".{}.nars-agent.tmp",
                        resolved
                            .file_name()
                            .map_or_else(|| "file".into(), |name| name.to_string_lossy())
                    ));
                    if let Err(error) = std::fs::write(&temp, next) {
                        let _ = std::fs::remove_file(&temp);
                        return failure(
                            call,
                            format!(
                                "failed to write `{}`: {error}",
                                context.sandbox.display(resolved)
                            ),
                        );
                    }
                    if let Err(error) = std::fs::rename(&temp, resolved) {
                        let _ = std::fs::remove_file(&temp);
                        return failure(
                            call,
                            format!(
                                "failed to commit `{}`: {error}",
                                context.sandbox.display(resolved)
                            ),
                        );
                    }
                }
            }
        }

        let mut summary = String::new();
        for diff in &diffs {
            summary.push_str(&format!(
                "{} {} (+{} -{})\n",
                if dry_run { "would patch" } else { "patched" },
                diff.path,
                diff.added,
                diff.removed
            ));
        }
        if summary.is_empty() {
            summary.push_str("no changes were necessary\n");
        }

        let mut result = ToolResult::success(call.id.clone(), summary);
        result.metadata.insert("dry_run".to_owned(), json!(dry_run));
        result
            .metadata
            .insert("files_changed".to_owned(), json!(diffs.len()));
        result.metadata.insert(
            "diffs".to_owned(),
            serde_json::to_value(&diffs).unwrap_or_default(),
        );
        if diffs.len() == 1 {
            result.diff = diffs.into_iter().next();
        }
        result
    }
}

fn parse_edits(arguments: &Value) -> Result<Vec<Edit>, String> {
    if let Some(Value::Array(items)) = arguments.get("patches") {
        return items
            .iter()
            .map(|item| {
                if !item.is_object() {
                    return Err("each patch must be an object".to_owned());
                }
                let path = crate::builtin::req_string(item, "path")?;
                let old_text = opt_string(item, "old_text")?.unwrap_or_default();
                let new_text = opt_string(item, "new_text")?.unwrap_or_default();
                let replace_all = opt_bool(item, "replace_all")?.unwrap_or(false);
                Ok(Edit {
                    path,
                    old_text,
                    new_text,
                    replace_all,
                })
            })
            .collect();
    }
    let path = crate::builtin::req_string(arguments, "path")?;
    let old_text = opt_string(arguments, "old_text")?.unwrap_or_default();
    let new_text = opt_string(arguments, "new_text")?.unwrap_or_default();
    let replace_all = opt_bool(arguments, "replace_all")?.unwrap_or(false);
    Ok(vec![Edit {
        path,
        old_text,
        new_text,
        replace_all,
    }])
}

/// Builds a preview diff for a patch call without writing anything.
#[must_use]
pub fn preview(call: &ToolCall, context: &ToolContext) -> Option<Vec<Diff>> {
    let mut dry_run_call = call.clone();
    if let Some(object) = dry_run_call.arguments.as_object_mut() {
        object.insert("dry_run".to_owned(), Value::Bool(true));
    }
    let result = ApplyPatchTool.execute(&dry_run_call, context);
    if !result.is_success() {
        return None;
    }
    serde_json::from_value::<Vec<Diff>>(result.metadata.get("diffs")?.clone()).ok()
}
