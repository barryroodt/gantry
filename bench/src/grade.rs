//! Programmatic grading checks + blinded LLM judge (plan Task 5).
//!
//! Invariants honored here (plan §Shared Architectural Invariants):
//! - **6**: judge calls go directly to the Anthropic API — never through the
//!   recording proxy — and judge usage is bookkept separately ([`JudgeUsage`]),
//!   never merged into [`crate::types::Ledger`].
//! - **7**: `success` = all programmatic checks pass AND judge score ≥ the
//!   per-task threshold (default 6).
//!
//! Blinding: the judge sees the task prompt, the rubric, and a "candidate
//! output" — never a harness name. See [`build_judge_prompt`].

use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use globset::GlobBuilder;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::types::{CheckResult, GradeResult, RunResult};

/// Default judge score threshold (spec §grade).
pub const DEFAULT_JUDGE_THRESHOLD: f32 = 6.0;

/// Direct Anthropic API endpoint for the judge. The recording proxy address
/// must never end up here (invariant 6).
pub const ANTHROPIC_API_URL: &str = "https://api.anthropic.com";

const ANTHROPIC_VERSION: &str = "2023-06-01";
const JUDGE_MAX_TOKENS: u32 = 1024;
/// Max bytes of check-command output kept in a [`CheckResult::detail`].
const COMMAND_DETAIL_TAIL: usize = 1024;

/// Grading section of `task.toml` (plan §task.toml contract). The runner
/// constructs this from the parsed task manifest.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GradeSpec {
    /// Regex patterns that must each match the extracted answer.
    #[serde(default)]
    pub answer_contains: Vec<String>,
    /// Shell command run in the post-run workspace; exit 0 = pass.
    #[serde(default)]
    pub check_command: Option<String>,
    /// Literal substrings that must each appear in the workspace diff.
    #[serde(default)]
    pub diff_contains: Vec<String>,
    /// Path globs (full repo-relative paths; `*` does not cross `/`, use
    /// `**` for recursion) that no changed path may match.
    #[serde(default)]
    pub diff_must_not_touch: Vec<String>,
    /// Minimum judge score for success (invariant 7).
    #[serde(default = "default_judge_threshold")]
    pub judge_threshold: f32,
}

fn default_judge_threshold() -> f32 {
    DEFAULT_JUDGE_THRESHOLD
}

impl Default for GradeSpec {
    fn default() -> Self {
        Self {
            answer_contains: Vec::new(),
            check_command: None,
            diff_contains: Vec::new(),
            diff_must_not_touch: Vec::new(),
            judge_threshold: DEFAULT_JUDGE_THRESHOLD,
        }
    }
}

// ---------------------------------------------------------------------------
// Programmatic checks
// ---------------------------------------------------------------------------

/// Run every programmatic check the spec declares against one finished run.
/// `workspace` is the post-run disposable workspace (`check_command` cwd).
pub fn run_checks(spec: &GradeSpec, run: &RunResult, workspace: &Path) -> Vec<CheckResult> {
    let mut checks = Vec::new();
    for (i, pattern) in spec.answer_contains.iter().enumerate() {
        checks.push(answer_contains_check(i, pattern, run.answer.as_deref()));
    }
    if let Some(cmd) = &spec.check_command {
        checks.push(check_command_check(cmd, workspace));
    }
    for (i, needle) in spec.diff_contains.iter().enumerate() {
        checks.push(diff_contains_check(i, needle, &run.workspace_diff));
    }
    if !spec.diff_must_not_touch.is_empty() {
        let changed = changed_paths(&run.workspace_diff);
        for (i, glob) in spec.diff_must_not_touch.iter().enumerate() {
            checks.push(diff_must_not_touch_check(i, glob, &changed));
        }
    }
    checks
}

fn answer_contains_check(i: usize, pattern: &str, answer: Option<&str>) -> CheckResult {
    let name = format!("answer_contains[{i}]");
    let re = match Regex::new(pattern) {
        Ok(re) => re,
        Err(e) => {
            return CheckResult {
                name,
                pass: false,
                detail: Some(format!("invalid regex {pattern:?}: {e}")),
            }
        }
    };
    let Some(text) = answer else {
        return CheckResult {
            name,
            pass: false,
            detail: Some(format!(
                "no answer extracted from harness stdout; pattern {pattern:?} unmatched"
            )),
        };
    };
    let pass = re.is_match(text);
    CheckResult {
        name,
        pass,
        detail: (!pass).then(|| format!("pattern {pattern:?} not found in answer")),
    }
}

fn check_command_check(cmd: &str, workspace: &Path) -> CheckResult {
    let name = "check_command".to_string();
    let output = match Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(workspace)
        .output()
    {
        Ok(out) => out,
        Err(e) => {
            return CheckResult {
                name,
                pass: false,
                detail: Some(format!("failed to spawn {cmd:?}: {e}")),
            }
        }
    };
    let exit = output
        .status
        .code()
        .map_or_else(|| "killed by signal".to_string(), |c| format!("{c}"));
    let mut detail = format!("exit: {exit}");
    for (label, bytes) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
        if !bytes.is_empty() {
            let text = String::from_utf8_lossy(bytes);
            detail.push_str(&format!(
                "; {label}: {}",
                tail_str(text.trim_end(), COMMAND_DETAIL_TAIL)
            ));
        }
    }
    CheckResult {
        name,
        pass: output.status.success(),
        detail: Some(detail),
    }
}

fn diff_contains_check(i: usize, needle: &str, diff: &str) -> CheckResult {
    let pass = diff.contains(needle);
    CheckResult {
        name: format!("diff_contains[{i}]"),
        pass,
        detail: (!pass).then(|| format!("substring {needle:?} not found in workspace diff")),
    }
}

fn diff_must_not_touch_check(i: usize, glob: &str, changed: &[String]) -> CheckResult {
    let name = format!("diff_must_not_touch[{i}]");
    let matcher = match GlobBuilder::new(glob).literal_separator(true).build() {
        Ok(g) => g.compile_matcher(),
        Err(e) => {
            return CheckResult {
                name,
                pass: false,
                detail: Some(format!("invalid glob {glob:?}: {e}")),
            }
        }
    };
    let touched: Vec<&str> = changed
        .iter()
        .filter(|p| matcher.is_match(p.as_str()))
        .map(String::as_str)
        .collect();
    let pass = touched.is_empty();
    CheckResult {
        name,
        pass,
        detail: (!pass).then(|| {
            format!("glob {glob:?} matches changed paths: {}", touched.join(", "))
        }),
    }
}

/// Repo-relative paths touched by a unified `git diff`, in order, deduplicated.
/// Renames contribute both the old and the new path. `/dev/null` sides
/// (creations/deletions) are skipped.
pub fn changed_paths(diff: &str) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    let mut push = |p: &str| {
        let p = p.trim();
        if p.is_empty() || p == "/dev/null" {
            return;
        }
        if !paths.iter().any(|x| x == p) {
            paths.push(p.to_string());
        }
    };
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git a/") {
            // " b/" inside a filename would mis-split; git quotes such paths
            // and the task repos are pinned, so this is acceptable. The
            // ---/+++ lines below recover content changes regardless.
            if let Some(idx) = rest.find(" b/") {
                push(&rest[..idx]);
                push(&rest[idx + 3..]);
            }
        } else if let Some(p) = line.strip_prefix("--- a/") {
            push(p);
        } else if let Some(p) = line.strip_prefix("+++ b/") {
            push(p);
        } else if let Some(p) = line.strip_prefix("rename from ") {
            push(p);
        } else if let Some(p) = line.strip_prefix("rename to ") {
            push(p);
        }
    }
    paths
}

fn tail_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

// ---------------------------------------------------------------------------
// Blinded LLM judge
// ---------------------------------------------------------------------------

/// System prompt for the judge. Deliberately names no harness — the candidate
/// is anonymous (spec §grade, blinding).
pub const JUDGE_SYSTEM_PROMPT: &str = "\
You are an impartial grader. You will be given a task description, a grading \
rubric, and the output of one anonymous candidate. Grade the candidate output \
on its own merits against the rubric, using the full anchored 0-10 scale:
- 0-2: wrong, fabricated, or no real attempt.
- 3-5: partly right with major gaps or errors.
- 6-8: substantially right with minor gaps.
- 9-10: fully correct, complete, and precise.
Do not reward verbosity. Penalize fabricated claims. Reply with ONLY a JSON \
object, no prose and no code fences: \
{\"score\": <number 0-10>, \"rationale\": \"<2-4 sentences>\"}";

/// Configuration for the judge client.
#[derive(Debug, Clone)]
pub struct JudgeConfig {
    /// Pinned dated judge model id.
    pub model: String,
    pub api_key: String,
    /// Direct Anthropic endpoint. NEVER the recording proxy (invariant 6);
    /// overridable only so the bench's own tests can point at a local mock.
    pub base_url: String,
}

impl JudgeConfig {
    pub fn new(model: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            api_key: api_key.into(),
            base_url: ANTHROPIC_API_URL.to_string(),
        }
    }
}

/// Judge token usage — bookkeeping only, reported separately from benchmark
/// metrics and never merged into a [`crate::types::Ledger`] (invariant 6).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JudgeUsage {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// A successful judge verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct JudgeVerdict {
    /// Anchored 0–10 score, clamped to range.
    pub score: f32,
    pub rationale: String,
    pub usage: JudgeUsage,
}

/// How judging ended for one run; input to [`compute_grade`].
#[derive(Debug, Clone, PartialEq)]
pub enum JudgeOutcome {
    /// Task has no rubric — quality rests on programmatic checks alone.
    NotRequired,
    Scored(JudgeVerdict),
    /// Judge call or verdict parsing failed. Never kills the suite; surfaces
    /// as a failing synthetic `judge` check with the reason in `detail`.
    Failed(String),
}

/// Build the blinded user prompt: task + rubric + candidate output. No
/// harness identity appears anywhere in the constructed text.
pub fn build_judge_prompt(task_prompt: &str, rubric: &str, answer: &str) -> String {
    format!(
        "# Task given to the candidate\n\n{task_prompt}\n\n\
         # Grading rubric\n\n{rubric}\n\n\
         # Candidate output\n\n{answer}"
    )
}

/// Full Messages API request body for one judge call.
pub fn build_judge_request_body(
    model: &str,
    task_prompt: &str,
    rubric: &str,
    answer: &str,
) -> Value {
    json!({
        "model": model,
        "max_tokens": JUDGE_MAX_TOKENS,
        "system": JUDGE_SYSTEM_PROMPT,
        "messages": [{
            "role": "user",
            "content": build_judge_prompt(task_prompt, rubric, answer),
        }],
    })
}

/// Call the judge once for one run's answer. Direct API call — `.no_proxy()`
/// guarantees an `HTTP(S)_PROXY` env var can never route this through the
/// recording proxy (invariant 6).
pub async fn run_judge(
    cfg: &JudgeConfig,
    task_prompt: &str,
    rubric: &str,
    answer: &str,
) -> Result<JudgeVerdict> {
    let body = build_judge_request_body(&cfg.model, task_prompt, rubric, answer);
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .context("building judge HTTP client")?;
    let resp = client
        .post(format!("{}/v1/messages", cfg.base_url.trim_end_matches('/')))
        .header("x-api-key", &cfg.api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .json(&body)
        .send()
        .await
        .context("judge request failed")?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .context("judge response was not JSON")?;
    if !status.is_success() {
        return Err(anyhow!("judge API returned {status}: {body}"));
    }
    parse_judge_response(&body, &cfg.model)
}

/// Extract the verdict + usage from a Messages API response body.
pub fn parse_judge_response(body: &Value, model: &str) -> Result<JudgeVerdict> {
    let text = body["content"]
        .as_array()
        .and_then(|blocks| {
            blocks
                .iter()
                .find(|b| b["type"] == "text")
                .and_then(|b| b["text"].as_str())
        })
        .ok_or_else(|| anyhow!("judge response has no text content block"))?;
    let (score, rationale) = parse_verdict(text)?;
    Ok(JudgeVerdict {
        score,
        rationale,
        usage: JudgeUsage {
            model: model.to_string(),
            input_tokens: body["usage"]["input_tokens"].as_u64().unwrap_or(0),
            output_tokens: body["usage"]["output_tokens"].as_u64().unwrap_or(0),
        },
    })
}

/// Parse `{"score": N, "rationale": "..."}` out of judge reply text,
/// tolerating code fences and surrounding prose. Scores are clamped to 0–10.
pub fn parse_verdict(text: &str) -> Result<(f32, String)> {
    let start = text
        .find('{')
        .ok_or_else(|| anyhow!("no JSON object in judge reply: {text:?}"))?;
    let end = text
        .rfind('}')
        .filter(|&e| e > start)
        .ok_or_else(|| anyhow!("unterminated JSON object in judge reply: {text:?}"))?;
    #[derive(Deserialize)]
    struct Raw {
        score: f32,
        #[serde(default)]
        rationale: String,
    }
    let raw: Raw = serde_json::from_str(&text[start..=end])
        .with_context(|| format!("unparsable judge verdict: {:?}", &text[start..=end]))?;
    if !raw.score.is_finite() {
        return Err(anyhow!("judge score is not a finite number"));
    }
    Ok((raw.score.clamp(0.0, 10.0), raw.rationale))
}

// ---------------------------------------------------------------------------
// Verdict assembly
// ---------------------------------------------------------------------------

/// Combine programmatic checks and the judge outcome into a [`GradeResult`]
/// per invariant 7. A failed judge becomes a failing synthetic `judge` check
/// and forces `success: false` (the rubric was required but could not be
/// applied); a missing rubric ([`JudgeOutcome::NotRequired`]) leaves success
/// to the programmatic checks alone.
pub fn compute_grade(
    mut checks: Vec<CheckResult>,
    judge: JudgeOutcome,
    threshold: f32,
) -> GradeResult {
    let mut judge_score = None;
    let mut judge_rationale = None;
    // None = no judge gate; Some(pass) = judge gate result.
    let judge_gate = match judge {
        JudgeOutcome::NotRequired => None,
        JudgeOutcome::Scored(v) => {
            judge_score = Some(v.score);
            judge_rationale = Some(v.rationale);
            Some(v.score >= threshold)
        }
        JudgeOutcome::Failed(reason) => {
            checks.push(CheckResult {
                name: "judge".to_string(),
                pass: false,
                detail: Some(format!("judge unavailable: {reason}")),
            });
            Some(false)
        }
    };
    let checks_pass = checks.iter().all(|c| c.pass);
    let success = checks_pass && judge_gate.unwrap_or(true);
    GradeResult {
        checks,
        judge_score,
        judge_rationale,
        success,
    }
}

/// Convenience for the runner: run checks, then fold in the judge outcome.
pub fn grade_run(
    spec: &GradeSpec,
    run: &RunResult,
    workspace: &Path,
    judge: JudgeOutcome,
) -> GradeResult {
    compute_grade(run_checks(spec, run, workspace), judge, spec.judge_threshold)
}
