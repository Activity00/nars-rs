//! Workspace sandbox.
//!
//! The sandbox is the first line of defense. It resolves workspace-relative
//! paths, rejects traversal and absolute paths, verifies canonical containment
//! (including symlinks), and protects repository metadata and secrets.

use std::path::{Component, Path, PathBuf};

use thiserror::Error;

/// Paths that may be read but never written by the agent.
pub const PROTECTED_WRITE_PATHS: &[&str] =
    &[".git", ".git/", ".env", ".env.local", ".env.production"];

/// Sandbox errors.
#[derive(Debug, Error)]
pub enum SandboxError {
    /// The path is absolute.
    #[error("absolute paths are not allowed: {0}")]
    Absolute(String),
    /// The path escapes the workspace.
    #[error("path escapes the workspace: {0}")]
    Escape(String),
    /// The path is empty or contains a NUL byte.
    #[error("invalid path: {0}")]
    Invalid(String),
    /// The path resolves outside the workspace through a symlink.
    #[error("resolved path is outside the workspace: {0}")]
    Outside(String),
    /// Writing to a protected path was refused.
    #[error("writing to protected path is refused: {0}")]
    Protected(String),
    /// The parent directory does not exist.
    #[error("parent directory does not exist: {0}")]
    MissingParent(String),
    /// Filesystem failure.
    #[error("filesystem error at {path}: {source}")]
    Io {
        /// Path involved.
        path: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// A canonical workspace root plus path policy.
#[derive(Clone, Debug)]
pub struct Sandbox {
    root: PathBuf,
    protect_write: Vec<String>,
}

impl Sandbox {
    /// Creates a sandbox rooted at an existing directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the root cannot be canonicalized.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, SandboxError> {
        let path = root.as_ref();
        let root = path.canonicalize().map_err(|source| SandboxError::Io {
            path: path.display().to_string(),
            source,
        })?;
        if !root.is_dir() {
            return Err(SandboxError::Invalid(format!(
                "workspace root is not a directory: {}",
                root.display()
            )));
        }
        Ok(Self {
            root,
            protect_write: PROTECTED_WRITE_PATHS
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
        })
    }

    /// The canonical workspace root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves a path that must already exist, following symlinks.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe, missing, or out-of-workspace paths.
    pub fn resolve_existing(&self, relative: &str) -> Result<PathBuf, SandboxError> {
        let joined = self.join_checked(relative)?;
        let resolved = joined.canonicalize().map_err(|source| SandboxError::Io {
            path: relative.to_owned(),
            source,
        })?;
        if !resolved.starts_with(&self.root) {
            return Err(SandboxError::Outside(relative.to_owned()));
        }
        Ok(resolved)
    }

    /// Resolves a path that may be created, checking the existing parent.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe paths, missing parents, protected writes, or
    /// a parent that resolves outside the workspace.
    pub fn resolve_new(&self, relative: &str) -> Result<PathBuf, SandboxError> {
        let joined = self.join_checked(relative)?;
        if self.is_protected(relative) {
            return Err(SandboxError::Protected(relative.to_owned()));
        }
        if joined.exists() {
            let resolved = joined.canonicalize().map_err(|source| SandboxError::Io {
                path: relative.to_owned(),
                source,
            })?;
            if !resolved.starts_with(&self.root) {
                return Err(SandboxError::Outside(relative.to_owned()));
            }
            return Ok(resolved);
        }
        let parent = joined
            .parent()
            .ok_or_else(|| SandboxError::MissingParent(relative.to_owned()))?;
        let parent = parent.canonicalize().map_err(|source| SandboxError::Io {
            path: relative.to_owned(),
            source,
        })?;
        if !parent.starts_with(&self.root) {
            return Err(SandboxError::Outside(relative.to_owned()));
        }
        let name = joined
            .file_name()
            .ok_or_else(|| SandboxError::Invalid(relative.to_owned()))?;
        Ok(parent.join(name))
    }

    /// Whether a workspace-relative path points into a protected area.
    #[must_use]
    pub fn is_protected(&self, relative: &str) -> bool {
        let normalized = relative.trim_start_matches("./");
        self.protect_write.iter().any(|protected| {
            normalized == protected
                || normalized.starts_with(&format!("{protected}/"))
                || (protected.ends_with('/') && normalized.starts_with(protected))
        })
    }

    /// Renders a path relative to the workspace for display.
    #[must_use]
    pub fn display(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .display()
            .to_string()
    }

    fn join_checked(&self, relative: &str) -> Result<PathBuf, SandboxError> {
        if relative.is_empty() || relative.contains('\0') {
            return Err(SandboxError::Invalid(relative.to_owned()));
        }
        let path = Path::new(relative);
        if path.is_absolute() {
            return Err(SandboxError::Absolute(relative.to_owned()));
        }
        if path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(SandboxError::Escape(relative.to_owned()));
        }
        Ok(self.root.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_and_absolute_paths() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        assert!(sandbox.resolve_existing("../x").is_err());
        assert!(sandbox.resolve_existing("/etc/passwd").is_err());
        assert!(sandbox.resolve_existing("").is_err());
    }

    #[test]
    fn resolves_files_inside_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hi").unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let path = sandbox.resolve_existing("hello.txt").unwrap();
        assert!(path.ends_with("hello.txt"));
        assert_eq!(sandbox.display(&path), "hello.txt");
    }

    #[test]
    fn protects_git_and_env_writes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        assert!(sandbox.resolve_new(".git/config").is_err());
        assert!(sandbox.resolve_new(".env").is_err());
        assert!(sandbox.resolve_new("src/new.rs").is_err()); // missing parent
        std::fs::create_dir(dir.path().join("src")).unwrap();
        assert!(sandbox.resolve_new("src/new.rs").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        assert!(sandbox.resolve_existing("link").is_err());
    }
}
