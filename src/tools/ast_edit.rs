//! Structural (AST) code rewrite via pi-ast's ast-grep engine. MUTATING.
//!
//! SP4 T4 fills in `ast_edit`. Mutating → the orchestrator registers it
//! default-OUT of the allowlist (registry wiring is NOT part of this task).
//! Implement to this exact signature so registration composes.

use super::{ToolError, ToolOutput};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct AstEditArgs {
    /// ast-grep pattern to match.
    pub pattern: String,
    /// Replacement/rewrite template.
    pub rewrite: String,
    /// Workdir-relative globs/dirs to rewrite (empty = whole workdir).
    #[serde(default)]
    pub paths: Vec<String>,
}

/// Apply the `pattern -> rewrite` codemod across `workdir`, writing changed
/// files back, and return a summary (files changed, hunk count) as a `ToolOutput`.
pub async fn ast_edit(_workdir: &Path, _args: AstEditArgs) -> Result<ToolOutput, ToolError> {
    unimplemented!("SP4 T4: implement using pi_ast::ops (compile_rewrite_rules/rewrite_source) + write files back")
}
