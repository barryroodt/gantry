//! Structural (AST) code rewrite via pi-ast's ast-grep engine. MUTATING.
//! Registered default-OUT of the allowlist by the orchestrator (registry T6).

use super::{truncate::truncated_output, ToolError, ToolOutput};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct AstEditArgs {
    /// ast-grep pattern to match.
    pub pattern: String,
    /// Replacement/rewrite template (may reference the pattern's metavars).
    pub rewrite: String,
    /// Workdir-relative globs/dirs to rewrite (empty = whole workdir).
    #[serde(default)]
    pub paths: Vec<String>,
}

/// Apply the `pattern -> rewrite` codemod across `workdir`, writing changed
/// files back, and return a summary (edits per file, totals) as a `ToolOutput`.
pub async fn ast_edit(workdir: &Path, args: AstEditArgs) -> Result<ToolOutput, ToolError> {
    let patterns = if args.paths.is_empty() {
        vec!["**".to_string()]
    } else {
        args.paths.clone()
    };
    let files = pi_ast::ops::collect_matched_files(workdir, &patterns)?;
    let rules = [(args.pattern.clone(), args.rewrite.clone())];

    let mut body = String::new();
    let mut total_edits: u32 = 0;
    let mut files_changed = 0usize;
    for file in &files {
        let Ok(lang) = pi_ast::ops::resolve_language(None, &file.absolute_path) else {
            continue;
        };
        let Ok(source) = std::fs::read_to_string(&file.absolute_path) else {
            continue;
        };
        let Ok(compiled) = pi_ast::ops::compile_rewrite_rules(&rules, lang) else {
            continue; // pattern/rewrite not valid for this language
        };
        if let Ok((new_source, count)) = pi_ast::ops::rewrite_source(&source, lang, &compiled) {
            if count > 0 && new_source != source {
                std::fs::write(&file.absolute_path, &new_source)?;
                files_changed += 1;
                total_edits += count;
                body.push_str(&format!("{}: {count} edit(s)\n", file.relative_path));
            }
        }
    }

    let out = if files_changed == 0 {
        "no edits applied\n".to_string()
    } else {
        format!("{total_edits} edit(s) across {files_changed} file(s)\n{body}")
    };
    Ok(truncated_output(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn applies_rewrite_to_supported_file() {
        let dir = TempDir::new().unwrap();
        let f = dir.path().join("lib.rs");
        std::fs::write(&f, "fn main() {\n    foo(1);\n    foo(2);\n}\n").unwrap();
        let out = ast_edit(
            dir.path(),
            AstEditArgs {
                pattern: "foo($A)".into(),
                rewrite: "bar($A)".into(),
                paths: vec![],
            },
        )
        .await
        .unwrap()
        .content;
        let after = std::fs::read_to_string(&f).unwrap();
        assert!(
            after.contains("bar(1)") && after.contains("bar(2)"),
            "rewritten: {after}"
        );
        assert!(!after.contains("foo("), "no original call left: {after}");
        assert!(out.contains("edit"), "summary reports edits: {out}");
    }

    #[tokio::test]
    async fn reports_no_edits_when_no_match() {
        let dir = TempDir::new().unwrap();
        let f = dir.path().join("lib.rs");
        std::fs::write(&f, "fn main() {}\n").unwrap();
        let out = ast_edit(
            dir.path(),
            AstEditArgs {
                pattern: "foo($A)".into(),
                rewrite: "bar($A)".into(),
                paths: vec![],
            },
        )
        .await
        .unwrap()
        .content;
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            "fn main() {}\n",
            "file unchanged"
        );
        assert!(out.contains("no edits"), "summary: {out}");
    }
}
