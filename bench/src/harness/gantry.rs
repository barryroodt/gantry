//! gantry harness adapter.
//!
//! Invocation contract verified against this workspace's `src/cli.rs` (clap
//! struct `Cli`): required flags `--model <PROVIDER/MODEL>`, `--workdir`,
//! `--prompt-file`, `--max-tokens`, `--timeout-ms`; mode via `--mode single`.
//! API base override verified at `src/provider/anthropic.rs:36`
//! (`ANTHROPIC_API_BASE`); the key is read from `ANTHROPIC_API_KEY`
//! (`src/provider/anthropic.rs:28`).

use std::path::{Path, PathBuf};
use std::process::Command;

use super::{hermetic_command, version_from, Harness, RunCtx};

/// Tools granted on `mutate` tasks.
///
/// gantry's `--tool` flag is an *allowlist*, not an additive grant: a
/// non-empty list disables every base tool not named (see
/// `src/tools/registry.rs` `ToolRegistry::is_allowed` — base tools pass only
/// when `allow` is empty or names them). So the spec's "add mutation tools"
/// means: name the full default base set (`BASE_TOOL_NAMES`) plus the two
/// mutation tools. Keep in sync with `src/tools/registry.rs`.
const MUTATE_TOOLS: &[&str] = &[
    // BASE_TOOL_NAMES (single-mode defaults)
    "read_file",
    "list_files",
    "find_files",
    "git_diff",
    "ast_grep",
    "shell",
    "skill_load",
    // gantry's mutation path (spec §harness table)
    "write_file",
    "edit_file",
];

/// gantry adapter. Holds the resolved binary path so command construction is
/// deterministic (and test-injectable via [`Gantry::with_bin`]).
#[derive(Debug, Clone)]
pub struct Gantry {
    bin: PathBuf,
}

impl Gantry {
    /// Resolve the gantry binary: `GANTRY_BENCH_GANTRY_BIN` env override
    /// (tests / pinned-binary runs), else the workspace `target/<profile>/`
    /// sibling of the running `gantry-bench` executable.
    pub fn new() -> Self {
        Self { bin: resolve_bin() }
    }

    /// Use an explicit binary path (test hook; also useful for benchmarking a
    /// pinned gantry build).
    pub fn with_bin(bin: PathBuf) -> Self {
        Self { bin }
    }
}

impl Default for Gantry {
    fn default() -> Self {
        Self::new()
    }
}

fn resolve_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("GANTRY_BENCH_GANTRY_BIN") {
        return PathBuf::from(p);
    }
    // `gantry` and `gantry-bench` are workspace siblings: both land in
    // target/<profile>/. Test binaries live one level deeper in deps/.
    let mut dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    if dir.file_name().is_some_and(|n| n == "deps") {
        dir.pop();
    }
    dir.join("gantry")
}

impl Harness for Gantry {
    fn name(&self) -> &'static str {
        "gantry"
    }

    fn command(&self, ctx: &RunCtx) -> Command {
        // Hermetic env (fairness §2/§5): cleared + allowlist; only the two
        // vars below reach gantry beyond the shared allowlist.
        let mut cmd = hermetic_command(&self.bin, ctx);
        cmd.args(["--mode", "single"]);
        // gantry takes a provider/model slug; the bench is Anthropic-only
        // (the proxy speaks the Anthropic Messages API).
        cmd.arg("--model").arg(format!("anthropic/{}", ctx.model));
        cmd.arg("--workdir").arg(&ctx.workspace);
        cmd.arg("--prompt-file").arg(&ctx.prompt_file);
        cmd.arg("--max-tokens").arg(ctx.max_tokens.to_string());
        cmd.arg("--timeout-ms").arg(ctx.timeout_ms.to_string());
        if ctx.mutate {
            // Deliberately NO `--isolate` (spec §harness): grading inspects
            // the post-run workspace; isolation would divert edits into the
            // COW shadow. The per-run workspace is already disposable.
            for tool in MUTATE_TOOLS {
                cmd.arg("--tool").arg(tool);
            }
        }
        cmd.env("ANTHROPIC_API_BASE", &ctx.proxy_url);
        cmd.env("ANTHROPIC_API_KEY", &ctx.api_key);
        cmd
    }

    fn extract_answer(&self, stdout: &str) -> Option<String> {
        // The terminal `result` NDJSON event (src/events.rs
        // `GantryEvent::Result`) carries only token totals — no text. The
        // run's deliverable is the LAST `assistant_text` event (single mode
        // emits one per agent turn; the final one is the answer).
        let mut answer = None;
        for line in stdout.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                continue;
            };
            if v.get("event").and_then(|e| e.as_str()) == Some("assistant_text") {
                if let Some(text) = v.get("text").and_then(|t| t.as_str()) {
                    answer = Some(text.to_string());
                }
            }
        }
        answer
    }

    fn version(&self) -> String {
        // gantry's clap derive defines no `--version`; the meaningful identity
        // is the workspace git SHA (the runner also records it as
        // `RunResult.gantry_sha`). CARGO_MANIFEST_DIR is bench/, its parent is
        // the gantry repo root — a compile-time path, fine for a dev-only
        // bench binary that runs from this repo.
        version_from(
            "git",
            &[
                "-C",
                concat!(env!("CARGO_MANIFEST_DIR"), "/.."),
                "rev-parse",
                "--short=12",
                "HEAD",
            ],
        )
    }
}
