//! Structural (AST) code search via pi-ast's ast-grep engine. Read-only.
//! Registered default-allowed by the orchestrator (registry wiring in T6).

use super::{truncate::truncated_output, ToolError, ToolOutput};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct AstGrepArgs {
    /// ast-grep pattern (e.g. `println!($$$ARGS)`).
    pub pattern: String,
    /// Workdir-relative globs/dirs to search (empty = whole workdir).
    #[serde(default)]
    pub paths: Vec<String>,
    /// Optional language alias (e.g. "rust"); inferred per-file when omitted.
    #[serde(default)]
    pub lang: Option<String>,
}

/// Search `workdir` for AST matches of `args.pattern`, returning match
/// locations (`relative/path:line:col: <snippet>`) as a bounded `ToolOutput`.
pub async fn ast_grep(workdir: &Path, args: AstGrepArgs) -> Result<ToolOutput, ToolError> {
    let patterns = if args.paths.is_empty() {
        vec!["**".to_string()]
    } else {
        args.paths.clone()
    };
    let files = pi_ast::ops::collect_matched_files(workdir, &patterns)?;

    let mut body = String::new();
    let mut total = 0usize;
    for file in &files {
        // Skip files whose language can't be resolved (unsupported / non-code).
        let Ok(lang) = pi_ast::ops::resolve_language(args.lang.as_deref(), &file.absolute_path)
        else {
            continue;
        };
        let Ok(source) = std::fs::read_to_string(&file.absolute_path) else {
            continue;
        };
        // Skip files where the pattern doesn't compile for their language.
        let Ok(compiled) = pi_ast::ops::compile_search_patterns(&args.pattern, lang) else {
            continue;
        };
        for m in pi_ast::ops::collect_matches(&source, lang, &compiled) {
            let snippet = m.text.lines().next().unwrap_or("").trim();
            body.push_str(&format!(
                "{}:{}:{}: {snippet}\n",
                file.relative_path, m.line, m.column
            ));
            total += 1;
        }
    }

    let out = if total == 0 {
        "no matches\n".to_string()
    } else {
        format!("{total} match(es)\n{body}")
    };
    Ok(truncated_output(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn finds_matches_in_supported_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("main.rs"),
            "fn main() {\n    println!(\"hi\");\n    let x = 1;\n}\n",
        )
        .unwrap();
        let out = ast_grep(
            dir.path(),
            AstGrepArgs {
                pattern: "println!($$$A)".into(),
                paths: vec![],
                lang: Some("rust".into()),
            },
        )
        .await
        .unwrap()
        .content;
        assert!(out.contains("main.rs"), "match file: {out}");
        assert!(out.contains("match"), "match count line: {out}");
        assert!(out.contains("println"), "snippet present: {out}");
    }

    #[tokio::test]
    async fn reports_no_matches() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        let out = ast_grep(
            dir.path(),
            AstGrepArgs {
                pattern: "println!($$$A)".into(),
                paths: vec![],
                lang: Some("rust".into()),
            },
        )
        .await
        .unwrap()
        .content;
        assert!(out.contains("no matches"), "expected no matches: {out}");
    }
}
