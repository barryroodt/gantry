use super::{resolve_workdir_path, truncate::truncated_output, ToolError, ToolOutput};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Deserialize, Serialize)]
pub struct ReadFileArgs {
    pub path: String,
    /// When true, return a pi-ast structural summary (signatures kept, large
    /// bodies elided) for supported source languages. Falls back to a plain
    /// read for unsupported / non-code files or unparseable sources.
    #[serde(default)]
    pub outline: bool,
}

/// Native read_file: workdir-rooted, 256 KiB truncation, traversal rejection.
/// With `outline`, returns a structural summary for supported source files.
pub async fn read_file(workdir: &Path, args: ReadFileArgs) -> Result<ToolOutput, ToolError> {
    let target = resolve_workdir_path(workdir, &args.path)?;
    let bytes = tokio::fs::read(&target).await?;
    let content = String::from_utf8_lossy(&bytes).into_owned();

    if args.outline && pi_ast::ops::is_supported_file(&target, None) {
        if let Some(summary) = outline_summary(&content, &args.path) {
            return Ok(truncated_output(summary));
        }
    }
    Ok(truncated_output(content))
}

/// Render a pi-ast structural summary, or `None` when summarization fails or
/// the source did not parse (caller then falls back to the plain content).
fn outline_summary(content: &str, path: &str) -> Option<String> {
    let result = pi_ast::summary::summarize_code(pi_ast::summary::SummaryOptions {
        code: content.to_string(),
        lang: None,
        path: Some(path.to_string()),
        min_body_lines: None,
        min_comment_lines: None,
        unfold_until_lines: None,
        unfold_limit_lines: None,
    })
    .ok()?;
    if !result.parsed {
        return None;
    }
    let mut out = String::new();
    for seg in &result.segments {
        match &seg.text {
            Some(text) => out.push_str(text),
            None => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                let n = seg.end_line.saturating_sub(seg.start_line) + 1;
                out.push_str(&format!("… {n} line(s) elided\n"));
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const RUST_SRC: &str = "fn short() -> i32 { 1 }\nfn long_function(x: i32) -> i32 {\n    let a = x + 1;\n    let b = a + 2;\n    let c = b + 3;\n    let d = c + 4;\n    let e = d + 5;\n    e\n}\n";

    async fn write_and_read(name: &str, body: &str, outline: bool) -> String {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(name), body).unwrap();
        read_file(
            dir.path(),
            ReadFileArgs {
                path: name.into(),
                outline,
            },
        )
        .await
        .unwrap()
        .content
    }

    #[tokio::test]
    async fn outline_elides_bodies_for_supported_lang() {
        let out = write_and_read("sample.rs", RUST_SRC, true).await;
        assert!(out.contains("fn long_function"), "signature kept: {out}");
        assert!(out.contains("elided"), "expected an elision marker: {out}");
        assert!(
            !out.contains("let e = d + 5"),
            "long body should be elided: {out}"
        );
    }

    #[tokio::test]
    async fn outline_false_returns_full_content() {
        let out = write_and_read("sample.rs", RUST_SRC, false).await;
        assert!(out.contains("let e = d + 5"), "full body present: {out}");
    }

    #[tokio::test]
    async fn outline_falls_back_for_unsupported_extension() {
        let out = write_and_read("notes.txt", RUST_SRC, true).await;
        assert!(
            out.contains("let e = d + 5"),
            "unsupported extension must fall back to full content: {out}"
        );
    }
}
