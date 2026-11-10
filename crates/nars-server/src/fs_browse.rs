//! Directory listing for the web workspace picker.
//!
//! Browsers cannot open a native folder dialog, so the daemon offers a
//! read-only, directory-only listing that the UI turns into a picker. When a
//! `workspace_root` is configured the listing is clamped to it, mirroring the
//! restriction [`AppState::validate_workspace`] enforces for new sessions.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::state::AppState;

/// Maximum number of subdirectories returned in one response.
const MAX_ENTRIES: usize = 500;

/// One subdirectory offered by the picker.
#[derive(Debug, Serialize)]
pub struct DirEntry {
    /// Directory name, without its parent path.
    pub name: String,
    /// Absolute path of the directory.
    pub path: String,
    /// Whether the name starts with a dot.
    pub hidden: bool,
}

/// One directory of subdirectories plus the navigation anchors around it.
#[derive(Debug, Serialize)]
pub struct DirListing {
    /// Canonical path of the listed directory.
    pub path: String,
    /// Canonical parent path, absent at a filesystem or workspace root.
    pub parent: Option<String>,
    /// Configured workspace root, when the daemon restricts workspaces.
    pub root: Option<String>,
    /// The daemon user's home directory, when it can be determined.
    pub home: Option<String>,
    /// Subdirectories, sorted with visible entries first.
    pub entries: Vec<DirEntry>,
    /// Whether the directory holds more subdirectories than were returned.
    pub truncated: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct DirQuery {
    /// Directory to list; defaults to the workspace root or the home directory.
    #[serde(default)]
    pub path: Option<String>,
}

/// Lists the subdirectories of one directory.
///
/// # Errors
///
/// Returns an error when the path is missing, is not a directory, or falls
/// outside the configured workspace root.
pub async fn list_dirs(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DirQuery>,
) -> Result<Json<DirListing>, ApiError> {
    let root = match state.config.workspace_root.as_deref() {
        Some(root) => Some(canonical_dir(root).map_err(|error| {
            ApiError::Internal(format!("invalid configured workspace root: {error}"))
        })?),
        None => None,
    };
    let home = home_dir();
    let requested = query
        .path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty());
    let start = match requested {
        Some(raw) => expand_tilde(raw, home.as_deref()),
        None => root
            .clone()
            .or_else(|| home.clone())
            .unwrap_or_else(|| PathBuf::from("/")),
    };
    let directory = canonical_dir(&start).map_err(|error| {
        ApiError::BadRequest(format!("cannot open `{}`: {error}", start.display()))
    })?;
    if let Some(root) = &root
        && !directory.starts_with(root)
    {
        return Err(ApiError::BadRequest(format!(
            "`{}` is outside the allowed workspace root",
            directory.display()
        )));
    }

    let mut entries = Vec::new();
    let mut truncated = false;
    let mut reader = tokio::fs::read_dir(&directory).await.map_err(|error| {
        ApiError::BadRequest(format!("cannot read `{}`: {error}", directory.display()))
    })?;
    while let Some(entry) = reader.next_entry().await.map_err(|error| {
        ApiError::BadRequest(format!("cannot read `{}`: {error}", directory.display()))
    })? {
        let Ok(metadata) = entry.metadata().await else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if let Some(root) = &root
            && !path
                .canonicalize()
                .is_ok_and(|canonical| canonical.starts_with(root))
        {
            continue;
        }
        if entries.len() == MAX_ENTRIES {
            truncated = true;
            break;
        }
        entries.push(DirEntry {
            hidden: name.starts_with('.'),
            name,
            path: path.display().to_string(),
        });
    }
    entries.sort_by_cached_key(|entry| (entry.hidden, entry.name.to_lowercase()));

    Ok(Json(DirListing {
        parent: parent_of(&directory, root.as_deref()),
        path: directory.display().to_string(),
        root: root.as_ref().map(|root| root.display().to_string()),
        home: home.map(|home| home.display().to_string()),
        entries,
        truncated,
    }))
}

fn canonical_dir(path: &Path) -> std::io::Result<PathBuf> {
    let canonical = path.canonicalize()?;
    if canonical.is_dir() {
        Ok(canonical)
    } else {
        Err(std::io::Error::other("not a directory"))
    }
}

/// Parent directory, unless it would leave the filesystem or the workspace root.
fn parent_of(directory: &Path, root: Option<&Path>) -> Option<String> {
    let parent = directory.parent()?;
    if parent == directory {
        return None;
    }
    if let Some(root) = root
        && !parent.starts_with(root)
    {
        return None;
    }
    Some(parent.display().to_string())
}

fn home_dir() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .find_map(std::env::var_os)
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
}

fn expand_tilde(raw: &str, home: Option<&Path>) -> PathBuf {
    if raw == "~" {
        return home.map_or_else(|| PathBuf::from(raw), Path::to_path_buf);
    }
    match raw.strip_prefix("~/") {
        Some(rest) => home.map_or_else(|| PathBuf::from(raw), |home| home.join(rest)),
        None => PathBuf::from(raw),
    }
}
