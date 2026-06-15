//! Claude Code (headless `claude -p`) harness adapter.
//!
//! CLI contract verified against the installed binary, `claude --version` =
//! `2.1.157 (Claude Code)` (2026-06-11), via `claude --help` plus one probe
//! run:
//! - `-p, --print` — non-interactive mode; prompt is the trailing positional.
//! - `--output-format <text|json|stream-json>` — "only works with --print".
//! - `--verbose` — REQUIRED with stream-json in print mode. Verified
//!   empirically: `claude -p --output-format stream-json "hi"` exits 1 with
//!   "Error: When using --print, --output-format=stream-json requires
//!   --verbose" before any network traffic.
//! - `--dangerously-skip-permissions` — bypasses permission prompts (mutate
//!   tasks only, per spec).
//! - `--model <model>` — accepts a full dated model name.
//!
//! Env (not listed in `--help`; documented Claude Code environment contract):
//! - `CLAUDE_CONFIG_DIR` — config/state dir; pointed at the hermetic tempdir
//!   so no user settings, memory, or MCP servers bleed in (fairness §2).
//! - `ANTHROPIC_BASE_URL` — API base override → recording proxy.
//! - `ANTHROPIC_API_KEY` — API-key auth (fairness §5).
//! - `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` — supported side-traffic
//!   switch (fairness §3).

use std::process::Command;

use super::{hermetic_command, read_prompt, version_from, Harness, RunCtx};

/// Claude Code adapter (`claude` resolved from `PATH`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudeCode;

impl Harness for ClaudeCode {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn command(&self, ctx: &RunCtx) -> Command {
        let prompt = read_prompt(ctx);
        // Hermetic env (fairness §2/§5): cleared + allowlist, so inherited
        // auth/config vars (`ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`,
        // `ANTHROPIC_MODEL`, other `CLAUDE_CODE_*`, proxies, …) never reach
        // the harness. Token auth would bypass the base-URL override (and the
        // proxy with it); only the vars set below get through.
        let mut cmd = hermetic_command("claude", ctx);
        cmd.args(["-p", "--output-format", "stream-json", "--verbose"]);
        cmd.arg("--model").arg(&ctx.model);
        if ctx.mutate {
            cmd.arg("--dangerously-skip-permissions");
        }
        // Verbatim prompt as the trailing positional (task prompts are
        // markdown prose; none start with `-`).
        cmd.arg(prompt);
        cmd.env("ANTHROPIC_BASE_URL", &ctx.proxy_url);
        cmd.env("ANTHROPIC_API_KEY", &ctx.api_key);
        cmd.env("CLAUDE_CONFIG_DIR", &ctx.config_dir);
        cmd.env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1");
        cmd
    }

    fn extract_answer(&self, stdout: &str) -> Option<String> {
        // stream-json emits one JSON object per line; the terminal line is
        //   {"type":"result","subtype":"success","result":"<text>",…}
        // (claude 2.1.157). Error subtypes (e.g. error_max_turns) omit the
        // `result` field → None. Non-JSON noise lines are skipped.
        let mut answer = None;
        for line in stdout.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                continue;
            };
            if v.get("type").and_then(|t| t.as_str()) == Some("result") {
                answer = v.get("result").and_then(|r| r.as_str()).map(str::to_string);
            }
        }
        answer
    }

    fn version(&self) -> String {
        // `claude --version` → "2.1.157 (Claude Code)"
        version_from("claude", &["--version"])
    }
}
