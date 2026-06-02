//! edit_file tool: literal, occurrence-count-guarded search/replace on a
//! workdir-confined file. MUTATING. Registered default-OUT of the allowlist.

use super::{resolve_workdir_path_for_create, truncate::truncated_output, ToolError, ToolOutput};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Deserialize, Serialize)]
pub struct EditFileArgs {
    /// Workdir-relative path of the existing file to edit.
    pub path: String,
    /// Literal substring to find (NOT a regex).
    pub search: String,
    /// Replacement text.
    pub replace: String,
    /// Required occurrence count; defaults to 1 (a unique match). The edit is
    /// rejected unless the file contains exactly this many occurrences.
    #[serde(default)]
    pub expected_count: Option<u32>,
}

/// Replace every occurrence of `args.search` with `args.replace` in `args.path`,
/// but only when the occurrence count equals `expected_count` (default 1). The
/// search is literal. Rejects empty search, zero matches, or a count mismatch.
pub async fn edit_file(workdir: &Path, args: EditFileArgs) -> Result<ToolOutput, ToolError> {
    if args.search.is_empty() {
        return Err(ToolError::InvalidInput("empty search".into()));
    }
    let target = resolve_workdir_path_for_create(workdir, &args.path)?;
    let content = tokio::fs::read_to_string(&target).await?;
    let count = content.matches(args.search.as_str()).count();
    let want = args.expected_count.unwrap_or(1) as usize;
    if count == 0 {
        return Err(ToolError::InvalidInput(format!(
            "no match for search in {}",
            args.path
        )));
    }
    if count != want {
        return Err(ToolError::InvalidInput(format!(
            "expected {want} occurrence(s), found {count} in {}",
            args.path
        )));
    }
    let new_content = content.replace(args.search.as_str(), &args.replace);
    tokio::fs::write(&target, new_content.as_bytes()).await?;
    Ok(truncated_output(format!(
        "replaced {count} occurrence(s) in {}",
        args.path
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn args(path: &str, search: &str, replace: &str, expected: Option<u32>) -> EditFileArgs {
        EditFileArgs {
            path: path.into(),
            search: search.into(),
            replace: replace.into(),
            expected_count: expected,
        }
    }

    async fn seed(name: &str, body: &str) -> (TempDir, String) {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(name), body).unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        (dir, path)
    }

    #[tokio::test]
    async fn replaces_unique_match() {
        let (dir, _) = seed("a.rs", "let x = old_value;\n").await;
        let out = edit_file(dir.path(), args("a.rs", "old_value", "new_value", None))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "let x = new_value;\n"
        );
        assert!(
            out.content.contains("replaced 1"),
            "summary: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn rejects_zero_matches() {
        let (dir, _) = seed("a.rs", "nothing here\n").await;
        let err = edit_file(dir.path(), args("a.rs", "absent", "x", None)).await;
        assert!(matches!(err, Err(ToolError::InvalidInput(_))));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "nothing here\n",
            "unchanged on reject"
        );
    }

    #[tokio::test]
    async fn rejects_count_mismatch_default_one() {
        let (dir, _) = seed("a.rs", "dup dup dup\n").await;
        // Default expected = 1, but there are 3 -> reject.
        let err = edit_file(dir.path(), args("a.rs", "dup", "x", None)).await;
        assert!(matches!(err, Err(ToolError::InvalidInput(_))));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "dup dup dup\n",
            "unchanged on reject"
        );
    }

    #[tokio::test]
    async fn replaces_all_with_expected_count() {
        let (dir, _) = seed("a.rs", "dup dup dup\n").await;
        let out = edit_file(dir.path(), args("a.rs", "dup", "x", Some(3)))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "x x x\n"
        );
        assert!(
            out.content.contains("replaced 3"),
            "summary: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn rejects_empty_search() {
        let (dir, _) = seed("a.rs", "body\n").await;
        let err = edit_file(dir.path(), args("a.rs", "", "x", None)).await;
        assert!(matches!(err, Err(ToolError::InvalidInput(_))));
    }

    #[tokio::test]
    async fn rejects_escape() {
        let (dir, _) = seed("a.rs", "body\n").await;
        let err = edit_file(dir.path(), args("../a.rs", "body", "x", None)).await;
        assert!(matches!(err, Err(ToolError::OutsideWorkdir(_))));
    }
}
