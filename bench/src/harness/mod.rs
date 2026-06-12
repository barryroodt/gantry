//! Harness abstraction: build the subprocess command for one benchmark run and
//! extract the final answer text from captured stdout.
//!
//! Fairness contract (spec §Fairness): every adapter receives the same
//! [`RunCtx`] — same pinned model, same verbatim prompt file, same workspace —
//! and wires its process to the recording proxy with hermetic config dirs and
//! API-key-only auth. Every adapter builds on [`hermetic_command`], so the
//! spawned harness sees NO inherited environment beyond an explicit allowlist
//! (fairness §2/§5). Stdout is used ONLY for answer extraction; all
//! efficiency metrics come from the proxy ledger (invariant 1).

use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Command;

pub mod claude_code;
pub mod gantry;
pub mod pi;

pub use claude_code::ClaudeCode;
pub use gantry::Gantry;
pub use pi::Pi;

/// Everything an adapter needs to build one run's command. Assembled by the
/// runner per (task × harness × rep).
#[derive(Debug, Clone)]
pub struct RunCtx {
    /// Fresh per-run copy of the pinned task repo. Also the spawned process's
    /// working directory for every harness.
    pub workspace: PathBuf,
    /// File holding the verbatim task prompt (byte-identical across harnesses).
    pub prompt_file: PathBuf,
    /// Pinned dated model id, bare (no provider prefix),
    /// e.g. `claude-opus-4-5-20251101`. The gantry adapter prepends
    /// `anthropic/` to form its `provider/model` slug.
    pub model: String,
    /// Recording proxy base URL, e.g. `http://127.0.0.1:49152`.
    pub proxy_url: String,
    /// Hermetic per-run tempdir for harness config/state (fairness §2): no
    /// user settings, memory, MCP servers, or CLAUDE.md bleed.
    pub config_dir: PathBuf,
    /// True for `mutate` tasks → the adapter adds its own mutation flags.
    pub mutate: bool,
    /// Real API key, forwarded so the proxy sees authentic auth headers
    /// verbatim (fairness §5: API-key auth everywhere).
    pub api_key: String,
    /// Per-task timeout. gantry takes it as `--timeout-ms`; for all harnesses
    /// the runner additionally enforces it with a process-tree kill.
    pub timeout_ms: u64,
    /// gantry's required `--max-tokens` budget. The runner sets this high
    /// enough to be non-binding — the other harnesses have no equivalent cap,
    /// so a binding budget would skew the comparison.
    pub max_tokens: u64,
}

/// One benchmarked harness. Implementations never spawn from `command` —
/// they only configure program, args, env, and cwd; the runner spawns.
pub trait Harness {
    /// Stable name used in `RunResult.harness` and `--harness` filters:
    /// `gantry` | `claude-code` | `pi`.
    fn name(&self) -> &'static str;

    /// Build the full subprocess command for one run.
    ///
    /// Panics if `ctx.prompt_file` is unreadable (runner-violated invariant:
    /// the runner writes that file before building the command).
    fn command(&self, ctx: &RunCtx) -> Command;

    /// Extract the final answer text from the harness's captured stdout.
    /// `None` when the run produced no extractable answer (graded as failure).
    fn extract_answer(&self, stdout: &str) -> Option<String>;

    /// Installed harness version, best-effort; `"unknown"` when undeterminable.
    fn version(&self) -> String;
}

/// All benchmarked harnesses, in canonical report order.
pub fn all() -> Vec<Box<dyn Harness>> {
    vec![Box::new(Gantry::new()), Box::new(ClaudeCode), Box::new(Pi)]
}

/// Hermetic base command shared by every adapter: cwd = the disposable
/// workspace, environment cleared, then an explicit allowlist re-added.
///
/// Allowlist > denylist (fairness §2/§5): inheriting the parent env and
/// scrubbing known-bad names breaks the moment a provider adds another
/// precedence-taking auth var (`ANTHROPIC_AUTH_TOKEN`, `*_OAUTH_TOKEN`, …) or
/// a knob like `ANTHROPIC_MODEL` / `HTTP(S)_PROXY` bleeds in. Cleared away
/// here, none of them can reach the harness; adapters add back exactly the
/// vars their harness needs.
///
/// Allowlisted:
/// - `PATH`, `TMPDIR` — inherited verbatim; binary resolution and tempfiles.
/// - `HOME` — pointed at the hermetic per-run config dir, so dotfile
///   fallbacks (`~/.gitconfig`, `~/.claude*`, `~/.omp`, …) can never reach
///   the user's real home.
pub fn hermetic_command(program: impl AsRef<OsStr>, ctx: &RunCtx) -> Command {
    let mut cmd = Command::new(program);
    cmd.current_dir(&ctx.workspace);
    cmd.env_clear();
    for key in ["PATH", "TMPDIR"] {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    cmd.env("HOME", &ctx.config_dir);
    cmd
}

/// Read the verbatim prompt for adapters that pass it as an argv argument.
///
/// Panics on an unreadable file — see [`Harness::command`].
pub(crate) fn read_prompt(ctx: &RunCtx) -> String {
    std::fs::read_to_string(&ctx.prompt_file)
        .unwrap_or_else(|e| panic!("prompt file {} unreadable: {e}", ctx.prompt_file.display()))
}

/// Best-effort version probe: run `program args…`, return the first non-empty
/// stdout line trimmed, else `"unknown"`. Never errors.
pub(crate) fn version_from(program: impl AsRef<OsStr>, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|s| s.lines().next().map(|line| line.trim().to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}
