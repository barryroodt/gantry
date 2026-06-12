//! Pi / oh-my-pi (`omp` print mode) harness adapter.
//!
//! CLI contract verified against the installed binary, `omp --version` =
//! `omp/15.11.0` (2026-06-11), via `omp --help`:
//! - `-p, --print` — "Non-interactive mode: process prompt and exit"; prompt
//!   is the trailing positional MESSAGES argument. The prompt is passed as
//!   text, NOT as `@file` — `@`-inclusion wraps content with file metadata,
//!   which would break the verbatim-identical-prompt guarantee (fairness §1).
//! - `--model <value>` — accepts a full model id.
//! - `--no-title` — "Disable title auto-generation" (fairness §3 side-traffic
//!   switch; the `PI_NO_TITLE=1` env is set as well — both present in the
//!   installed bundle).
//! - `--auto-approve` — "Auto-approve all tool calls" (mutate tasks only).
//!
//! Env (verified present in the installed package bundle
//! `@oh-my-pi/pi-coding-agent` v15.11.0; `--help` documents the starred ones):
//! - `PI_CONFIG_DIR` — config dir override → hermetic tempdir (fairness §2).
//! - `PI_CODING_AGENT_DIR` (*) — session storage dir (default `~/.omp/agent`)
//!   → kept inside the hermetic tempdir so runs leave no shared state.
//! - `ANTHROPIC_BASE_URL` (*) — API base override → recording proxy.
//! - `ANTHROPIC_OAUTH_TOKEN` (*) — "takes precedence over API key" per
//!   `--help`, so it must never reach the harness (fairness §5: OAuth would
//!   bypass the keyed proxy auth path). [`hermetic_command`]'s cleared env
//!   guarantees that without name-by-name scrubbing.

use std::process::Command;

use super::{hermetic_command, read_prompt, version_from, Harness, RunCtx};

/// Pi adapter (`omp` resolved from `PATH`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Pi;

impl Harness for Pi {
    fn name(&self) -> &'static str {
        "pi"
    }

    fn command(&self, ctx: &RunCtx) -> Command {
        let prompt = read_prompt(ctx);
        // .env-leakage guard (plan Task 4): omp ingests `$PWD/.env`. We run
        // with cwd = the disposable workspace, and the workspace must not
        // ship one. Task materialization (task.rs) owns the guarantee; this
        // assert catches violations in debug/test runs.
        debug_assert!(
            !ctx.workspace.join(".env").exists(),
            "workspace {} contains .env — it would leak into omp's environment",
            ctx.workspace.display()
        );
        // Hermetic env (fairness §2/§5): cleared + allowlist, so inherited
        // `ANTHROPIC_OAUTH_TOKEN` / `PI_*` / proxy vars never reach omp; only
        // the vars set below get through.
        let mut cmd = hermetic_command("omp", ctx);
        cmd.arg("-p");
        cmd.arg("--model").arg(&ctx.model);
        cmd.arg("--no-title");
        if ctx.mutate {
            cmd.arg("--auto-approve");
        }
        cmd.arg(prompt);
        cmd.env("ANTHROPIC_BASE_URL", &ctx.proxy_url);
        cmd.env("ANTHROPIC_API_KEY", &ctx.api_key);
        cmd.env("PI_CONFIG_DIR", &ctx.config_dir);
        cmd.env("PI_CODING_AGENT_DIR", ctx.config_dir.join("agent"));
        cmd.env("PI_NO_TITLE", "1");
        cmd
    }

    fn extract_answer(&self, stdout: &str) -> Option<String> {
        // `omp -p` (default text output mode) writes ONLY the final assistant
        // message to stdout — symmetric with the other adapters' structured
        // final-message extraction. Verified against the installed bundle
        // (@oh-my-pi/pi-coding-agent v15.11.0, dist/cli.js `runPrintMode`,
        // mode "text"): it prints the text content blocks of the LAST
        // `messages` entry only when `role == "assistant"`; interim turns are
        // never echoed and errors/warnings go to stderr. The whole trimmed
        // stream therefore IS the final message (possibly multi-line).
        let text = stdout.trim();
        (!text.is_empty()).then(|| text.to_string())
    }

    fn version(&self) -> String {
        // `omp --version` → "omp/15.11.0" (stdout; runtime warnings go to stderr)
        version_from("omp", &["--version"])
    }
}
