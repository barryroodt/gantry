//! Structural (AST) code search via pi-ast's ast-grep engine.
//!
//! SP4 T3 fills in `ast_grep`. Read-only; registered default-allowed by the
//! orchestrator (registry wiring is NOT part of this task). Implement to this
//! exact signature so registration composes.

use super::{ToolError, ToolOutput};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct AstGrepArgs {
    /// ast-grep pattern (e.g. `console.log($$$ARGS)`).
    pub pattern: String,
    /// Workdir-relative globs/dirs to search (empty = whole workdir).
    #[serde(default)]
    pub paths: Vec<String>,
    /// Optional language alias (e.g. "rust"); inferred per-file when omitted.
    #[serde(default)]
    pub lang: Option<String>,
}

/// Search `workdir` for AST matches of `args.pattern`, returning match
/// locations (file, line range, captured metavars) as a bounded `ToolOutput`.
pub async fn ast_grep(_workdir: &Path, _args: AstGrepArgs) -> Result<ToolOutput, ToolError> {
    unimplemented!("SP4 T3: implement using pi_ast::ops (compile_search_patterns/collect_matches/collect_matched_files)")
}
