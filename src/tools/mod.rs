pub mod ast_edit;
pub mod ast_grep;
pub mod find_files;
pub mod git_diff;
pub mod list_files;
pub mod read_file;
pub mod registry;
pub mod shell;
pub mod skill_load;
pub mod subagent;
pub mod truncate;
pub mod write_file;

pub use registry::ToolRegistry;

use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

pub const MAX_TOOL_OUTPUT_BYTES: usize = 256 * 1024;

/// Common tool result shape returned by all native tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub content: String,
    pub bytes: usize,
    pub truncated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("path outside workdir: {0}")]
    OutsideWorkdir(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("unknown tool: {0}")]
    UnknownTool(String),
}

/// Resolve a relative path against `workdir`, rejecting symlink-traversal escapes.
pub fn resolve_workdir_path(
    workdir: &std::path::Path,
    rel: &str,
) -> Result<std::path::PathBuf, ToolError> {
    let workdir = workdir.canonicalize().map_err(ToolError::Io)?;
    let joined = workdir.join(rel);
    let canonical = joined
        .canonicalize()
        .map_err(|_| ToolError::OutsideWorkdir(rel.to_string()))?;
    if !canonical.starts_with(&workdir) {
        return Err(ToolError::OutsideWorkdir(rel.to_string()));
    }
    Ok(canonical)
}

/// Resolve a possibly-not-yet-existing path for create/write, confining it to
/// `workdir`. Lexically resolves `.`/`..` (no filesystem access) and rejects any
/// result outside `workdir`, then canonicalizes the deepest existing ancestor to
/// defeat a symlinked parent that would redirect writes outside the jail.
pub fn resolve_workdir_path_for_create(workdir: &Path, rel: &str) -> Result<PathBuf, ToolError> {
    let base = workdir.canonicalize().map_err(ToolError::Io)?;
    // An absolute `rel` makes `join` discard `base`; the containment check rejects it.
    let joined = base.join(rel);
    let mut normalized = PathBuf::new();
    for comp in joined.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(ToolError::OutsideWorkdir(rel.to_string()));
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    if !normalized.starts_with(&base) {
        return Err(ToolError::OutsideWorkdir(rel.to_string()));
    }
    // Symlink safety: the deepest existing ancestor must canonicalize under `base`.
    let mut probe = normalized.parent();
    while let Some(dir) = probe {
        if dir.exists() {
            let canon = dir.canonicalize().map_err(ToolError::Io)?;
            if !canon.starts_with(&base) {
                return Err(ToolError::OutsideWorkdir(rel.to_string()));
            }
            break;
        }
        probe = dir.parent();
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn create_guard_allows_nested_new_path() {
        let dir = TempDir::new().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let p = resolve_workdir_path_for_create(dir.path(), "a/b/c.txt").unwrap();
        assert!(p.starts_with(&base));
        assert!(p.ends_with("a/b/c.txt"));
    }

    #[test]
    fn create_guard_rejects_parent_escape() {
        let dir = TempDir::new().unwrap();
        assert!(matches!(
            resolve_workdir_path_for_create(dir.path(), "../escape.txt"),
            Err(ToolError::OutsideWorkdir(_))
        ));
    }

    #[test]
    fn create_guard_rejects_absolute_outside() {
        let dir = TempDir::new().unwrap();
        assert!(matches!(
            resolve_workdir_path_for_create(dir.path(), "/etc/passwd"),
            Err(ToolError::OutsideWorkdir(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn create_guard_rejects_symlinked_parent_escape() {
        let work = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), work.path().join("link")).unwrap();
        assert!(matches!(
            resolve_workdir_path_for_create(work.path(), "link/evil.txt"),
            Err(ToolError::OutsideWorkdir(_))
        ));
    }
}
