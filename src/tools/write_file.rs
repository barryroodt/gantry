//! write_file tool: create or overwrite a workdir-confined file. MUTATING.
//! Registered default-OUT of the allowlist by the orchestrator (registry wiring).

use super::{resolve_workdir_path_for_create, truncate::truncated_output, ToolError, ToolOutput};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Deserialize, Serialize)]
pub struct WriteFileArgs {
    /// Workdir-relative path to create or overwrite.
    pub path: String,
    /// Full file contents to write.
    pub content: String,
}

/// Create or overwrite `args.path` within `workdir`, creating parent directories
/// as needed. Path escapes are rejected (workdir-confined via the create guard).
pub async fn write_file(workdir: &Path, args: WriteFileArgs) -> Result<ToolOutput, ToolError> {
    if args.path.trim().is_empty() {
        return Err(ToolError::InvalidInput("empty path".into()));
    }
    let target = resolve_workdir_path_for_create(workdir, &args.path)?;
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let bytes = args.content.len();
    tokio::fs::write(&target, args.content.as_bytes()).await?;
    Ok(truncated_output(format!(
        "wrote {bytes} bytes to {}",
        args.path
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn args(path: &str, content: &str) -> WriteFileArgs {
        WriteFileArgs {
            path: path.into(),
            content: content.into(),
        }
    }

    #[tokio::test]
    async fn writes_new_file() {
        let dir = TempDir::new().unwrap();
        let out = write_file(dir.path(), args("a.txt", "hello"))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "hello"
        );
        assert!(
            out.content.contains("wrote 5 bytes"),
            "summary: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), args("x/y/z.txt", "deep"))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("x/y/z.txt")).unwrap(),
            "deep"
        );
    }

    #[tokio::test]
    async fn overwrites_existing() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "old").unwrap();
        write_file(dir.path(), args("a.txt", "new")).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "new"
        );
    }

    #[tokio::test]
    async fn rejects_escape() {
        let dir = TempDir::new().unwrap();
        let err = write_file(dir.path(), args("../evil.txt", "x")).await;
        assert!(matches!(err, Err(ToolError::OutsideWorkdir(_))));
    }

    #[tokio::test]
    async fn rejects_empty_path() {
        let dir = TempDir::new().unwrap();
        let err = write_file(dir.path(), args("  ", "x")).await;
        assert!(matches!(err, Err(ToolError::InvalidInput(_))));
    }
}
