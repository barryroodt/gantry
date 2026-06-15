//! Grading tests (plan Task 5): programmatic check semantics, glob behavior,
//! verdict assembly per invariant 7, and judge prompt blinding. All tests are
//! keyless and networkless.

use std::path::Path;

use gantry_bench::grade::{
    build_judge_prompt, build_judge_request_body, changed_paths, compute_grade, grade_run,
    parse_judge_response, parse_verdict, run_checks, GradeSpec, JudgeConfig, JudgeOutcome,
    JudgeVerdict, ANTHROPIC_API_URL, DEFAULT_JUDGE_THRESHOLD, JUDGE_SYSTEM_PROMPT,
};
use gantry_bench::types::{CheckResult, JudgeUsage, Ledger, RunOutcome, RunResult};
use regex::Regex;
use serde_json::json;

fn run_result(answer: Option<&str>, diff: &str) -> RunResult {
    RunResult {
        task_id: "t".into(),
        harness: "h".into(),
        rep: 0,
        outcome: RunOutcome::Completed,
        wall_ms: 0,
        exit_code: Some(0),
        answer: answer.map(Into::into),
        ledger: Ledger::default(),
        workspace_diff: diff.into(),
        stderr_tail: String::new(),
        harness_version: "v0".into(),
        gantry_sha: "deadbeef".into(),
        model: "m".into(),
    }
}

fn spec() -> GradeSpec {
    GradeSpec::default()
}

const SAMPLE_DIFF: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,2 +1,2 @@
-fn old_thing() {}
+fn new_thing() {}
diff --git a/tests/foo_test.rs b/tests/foo_test.rs
new file mode 100644
--- /dev/null
+++ b/tests/foo_test.rs
@@ -0,0 +1 @@
+#[test] fn t() {}
diff --git a/old_name.rs b/new_name.rs
similarity index 100%
rename from old_name.rs
rename to new_name.rs
";

fn passing_check() -> CheckResult {
    CheckResult {
        name: "c".into(),
        pass: true,
        detail: None,
    }
}

fn failing_check() -> CheckResult {
    CheckResult {
        name: "c".into(),
        pass: false,
        detail: None,
    }
}

fn usage(input: u64, output: u64) -> JudgeUsage {
    JudgeUsage {
        model: "judge-model".into(),
        input_tokens: input,
        output_tokens: output,
    }
}

fn verdict(score: f32) -> JudgeVerdict {
    JudgeVerdict {
        score,
        rationale: "because".into(),
        usage: usage(100, 10),
    }
}

// --------------------------------------------------------------------------
// answer_contains
// --------------------------------------------------------------------------

#[test]
fn answer_contains_passes_when_all_regexes_match() {
    let mut s = spec();
    s.answer_contains = vec!["fn main".into(), r"src/\w+\.rs".into()];
    let run = run_result(Some("the bug is in src/lib.rs near fn main"), "");
    let checks = run_checks(&s, &run, Path::new("."));
    assert_eq!(checks.len(), 2);
    assert!(checks.iter().all(|c| c.pass), "{checks:?}");
    assert_eq!(checks[0].name, "answer_contains[0]");
    assert_eq!(checks[1].name, "answer_contains[1]");
}

#[test]
fn answer_contains_fails_on_unmatched_pattern() {
    let mut s = spec();
    s.answer_contains = vec!["nonexistent_symbol".into()];
    let run = run_result(Some("something else entirely"), "");
    let checks = run_checks(&s, &run, Path::new("."));
    assert!(!checks[0].pass);
    assert!(
        checks[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("nonexistent_symbol"),
        "{checks:?}"
    );
}

#[test]
fn answer_contains_fails_when_no_answer_extracted() {
    let mut s = spec();
    s.answer_contains = vec!["anything".into()];
    let run = run_result(None, "");
    let checks = run_checks(&s, &run, Path::new("."));
    assert!(!checks[0].pass);
    assert!(
        checks[0].detail.as_deref().unwrap().contains("no answer"),
        "{checks:?}"
    );
}

// Invalid patterns are a bench bug, not a gradable outcome: GradeSpec is
// validated fail-fast at manifest load, and the check path trusts that.
#[test]
#[should_panic(expected = "unvalidated answer_contains regex")]
fn answer_contains_unvalidated_regex_panics() {
    let mut s = spec();
    s.answer_contains = vec!["(".into()];
    let run = run_result(Some("text"), "");
    run_checks(&s, &run, Path::new("."));
}

// --------------------------------------------------------------------------
// check_command
// --------------------------------------------------------------------------

#[test]
fn check_command_runs_in_workspace_and_passes_on_exit_zero() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("marker.txt"), "x").unwrap();
    let mut s = spec();
    s.check_command = Some("test -f marker.txt".into());
    let run = run_result(None, "");
    let checks = run_checks(&s, &run, ws.path());
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].name, "check_command");
    assert!(checks[0].pass, "{checks:?}");
}

#[test]
fn check_command_fails_on_nonzero_exit_and_captures_output() {
    let ws = tempfile::tempdir().unwrap();
    let mut s = spec();
    s.check_command = Some("echo boom; echo bang >&2; exit 3".into());
    let run = run_result(None, "");
    let checks = run_checks(&s, &run, ws.path());
    assert!(!checks[0].pass);
    let detail = checks[0].detail.as_deref().unwrap();
    assert!(detail.contains("exit: 3"), "{detail}");
    assert!(detail.contains("boom"), "{detail}");
    assert!(detail.contains("bang"), "{detail}");
}

// --------------------------------------------------------------------------
// diff_contains
// --------------------------------------------------------------------------

#[test]
fn diff_contains_substring_pass_and_fail() {
    let mut s = spec();
    s.diff_contains = vec!["+fn new_thing()".into(), "no_such_line".into()];
    let run = run_result(None, SAMPLE_DIFF);
    let checks = run_checks(&s, &run, Path::new("."));
    assert!(checks[0].pass, "{checks:?}");
    assert!(!checks[1].pass, "{checks:?}");
    assert!(
        checks[1]
            .detail
            .as_deref()
            .unwrap()
            .contains("no_such_line"),
        "{checks:?}"
    );
}

// --------------------------------------------------------------------------
// changed_paths + diff_must_not_touch
// --------------------------------------------------------------------------

#[test]
fn changed_paths_parses_modifications_creations_and_renames() {
    let paths = changed_paths(SAMPLE_DIFF);
    assert_eq!(
        paths,
        vec![
            "src/lib.rs".to_string(),
            "tests/foo_test.rs".to_string(),
            "old_name.rs".to_string(),
            "new_name.rs".to_string(),
        ]
    );
}

#[test]
fn diff_must_not_touch_fails_when_glob_matches_changed_path() {
    let mut s = spec();
    s.diff_must_not_touch = vec!["tests/**".into()];
    let run = run_result(None, SAMPLE_DIFF);
    let checks = run_checks(&s, &run, Path::new("."));
    assert_eq!(checks[0].name, "diff_must_not_touch[0]");
    assert!(!checks[0].pass);
    assert!(
        checks[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("tests/foo_test.rs"),
        "{checks:?}"
    );
}

#[test]
fn diff_must_not_touch_passes_when_nothing_matches() {
    let mut s = spec();
    s.diff_must_not_touch = vec!["docs/**".into()];
    let run = run_result(None, SAMPLE_DIFF);
    let checks = run_checks(&s, &run, Path::new("."));
    assert!(checks[0].pass, "{checks:?}");
}

#[test]
fn diff_must_not_touch_double_star_crosses_directories() {
    let mut s = spec();
    s.diff_must_not_touch = vec!["**/*_test.rs".into()];
    let run = run_result(None, SAMPLE_DIFF);
    let checks = run_checks(&s, &run, Path::new("."));
    assert!(!checks[0].pass, "**/*_test.rs must match tests/foo_test.rs");
}

#[test]
fn diff_must_not_touch_single_star_does_not_cross_separator() {
    // Diff touching only a nested file: a top-level "*.rs" glob must not match.
    let diff = "\
diff --git a/tests/foo_test.rs b/tests/foo_test.rs
index 1111111..2222222 100644
--- a/tests/foo_test.rs
+++ b/tests/foo_test.rs
@@ -1 +1 @@
-a
+b
";
    let mut s = spec();
    s.diff_must_not_touch = vec!["*.rs".into()];
    let run = run_result(None, diff);
    let checks = run_checks(&s, &run, Path::new("."));
    assert!(checks[0].pass, "*.rs must not match tests/foo_test.rs");
}

// See answer_contains_unvalidated_regex_panics: grade trusts load-time
// validation (GradeSpec::validate) instead of re-checking defensively.
#[test]
#[should_panic(expected = "unvalidated diff_must_not_touch glob")]
fn diff_must_not_touch_unvalidated_glob_panics() {
    let mut s = spec();
    s.diff_must_not_touch = vec!["[".into()];
    let run = run_result(None, SAMPLE_DIFF);
    run_checks(&s, &run, Path::new("."));
}

// --------------------------------------------------------------------------
// compute_grade (invariant 7)
// --------------------------------------------------------------------------

#[test]
fn success_requires_checks_pass_and_judge_at_or_above_threshold() {
    let g = compute_grade(
        vec![passing_check()],
        JudgeOutcome::Scored(verdict(7.0)),
        6.0,
    );
    assert!(g.success);
    assert_eq!(g.judge_score, Some(7.0));
    assert_eq!(g.judge_rationale.as_deref(), Some("because"));
    // The verdict's usage is persisted as bookkeeping (invariant 6).
    assert_eq!(g.judge_usage, Some(usage(100, 10)));

    // Exactly at threshold counts (>=).
    let g = compute_grade(
        vec![passing_check()],
        JudgeOutcome::Scored(verdict(6.0)),
        6.0,
    );
    assert!(g.success);

    let g = compute_grade(
        vec![passing_check()],
        JudgeOutcome::Scored(verdict(5.9)),
        6.0,
    );
    assert!(!g.success);

    // High judge score never rescues a failing programmatic check.
    let g = compute_grade(
        vec![failing_check()],
        JudgeOutcome::Scored(verdict(10.0)),
        6.0,
    );
    assert!(!g.success);
}

#[test]
fn no_rubric_means_checks_alone_decide() {
    let g = compute_grade(vec![passing_check()], JudgeOutcome::NotRequired, 6.0);
    assert!(g.success);
    assert_eq!(g.judge_score, None);
    assert_eq!(g.judge_rationale, None);
    assert_eq!(g.judge_usage, None);

    let g = compute_grade(vec![failing_check()], JudgeOutcome::NotRequired, 6.0);
    assert!(!g.success);
}

#[test]
fn no_rubric_and_no_checks_is_vacuous_success() {
    let g = compute_grade(Vec::new(), JudgeOutcome::NotRequired, 6.0);
    assert!(g.success);
    assert!(g.checks.is_empty());
}

#[test]
fn judge_failure_with_required_rubric_fails_and_is_flagged_via_detail() {
    let g = compute_grade(
        vec![passing_check()],
        JudgeOutcome::Failed("connection refused".into()),
        6.0,
    );
    assert!(!g.success);
    assert_eq!(g.judge_score, None);
    assert_eq!(g.judge_rationale, None);
    assert_eq!(g.judge_usage, None, "failed judge call has no usable usage");
    let flag = g.checks.last().unwrap();
    assert_eq!(flag.name, "judge");
    assert!(!flag.pass);
    assert!(
        flag.detail
            .as_deref()
            .unwrap()
            .contains("connection refused"),
        "{flag:?}"
    );
}

#[test]
fn grade_run_wires_checks_threshold_and_judge_together() {
    let mut s = spec();
    s.answer_contains = vec!["root cause".into()];
    s.diff_must_not_touch = vec!["tests/**".into()];
    s.judge_threshold = 8.0;
    let run = run_result(Some("the root cause is X"), ""); // empty diff: touches nothing
    let g = grade_run(&s, &run, Path::new("."), JudgeOutcome::Scored(verdict(8.5)));
    assert!(g.success, "{g:?}");
    // Same inputs, judge under the per-task threshold.
    let g = grade_run(&s, &run, Path::new("."), JudgeOutcome::Scored(verdict(7.9)));
    assert!(!g.success);
}

// --------------------------------------------------------------------------
// GradeSpec deserialization (task.toml [grading] contract)
// --------------------------------------------------------------------------

#[test]
fn grade_spec_toml_defaults() {
    let s: GradeSpec = toml::from_str("answer_contains = [\"x\"]").unwrap();
    assert_eq!(s.answer_contains, vec!["x".to_string()]);
    assert_eq!(s.judge_threshold, DEFAULT_JUDGE_THRESHOLD);
    assert_eq!(s.check_command, None);
    assert!(s.diff_contains.is_empty());
    assert!(s.diff_must_not_touch.is_empty());

    let s: GradeSpec = toml::from_str("judge_threshold = 7.5").unwrap();
    assert_eq!(s.judge_threshold, 7.5);
}

// --------------------------------------------------------------------------
// Judge prompt construction: blinding (no network anywhere below)
// --------------------------------------------------------------------------

#[test]
fn judge_prompt_is_blinded_and_carries_inputs_verbatim() {
    let task_prompt = "Explain the architecture of this repository.";
    let rubric = "10 = names key modules and data flow; 0 = fabrications.";
    let answer = "The repo has a parser feeding an evaluator.";
    let prompt = build_judge_prompt(task_prompt, rubric, answer);

    // Inputs pass through verbatim; the candidate framing is present.
    assert!(prompt.contains(task_prompt));
    assert!(prompt.contains(rubric));
    assert!(prompt.contains(answer));
    assert!(prompt.to_lowercase().contains("candidate output"));

    // No harness identity anywhere in the constructed text (system prompt
    // included). "pi"/"omp" are checked as whole words so that ordinary
    // vocabulary (complete, compare, ...) cannot mask a real leak.
    let forbidden = Regex::new(r"(?i)\b(gantry|claude|pi|omp)\b|claude-code|oh-my-pi").unwrap();
    for text in [prompt.as_str(), JUDGE_SYSTEM_PROMPT] {
        assert!(
            !forbidden.is_match(text),
            "harness name leaked into judge text: {text}"
        );
    }
}

#[test]
fn judge_request_body_shape_and_blinding() {
    let body = build_judge_request_body(
        "judge-model-2026-01-01",
        "Find the bug.",
        "10 = exact file named.",
        "It is in src/parser.rs.",
    );
    assert_eq!(body["model"], "judge-model-2026-01-01");
    assert!(body["max_tokens"].as_u64().unwrap() > 0);
    assert_eq!(body["system"], JUDGE_SYSTEM_PROMPT);
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["messages"][0]["role"], "user");

    let content = body["messages"][0]["content"].as_str().unwrap();
    let forbidden = Regex::new(r"(?i)\b(gantry|claude|pi|omp)\b|claude-code|oh-my-pi").unwrap();
    assert!(!forbidden.is_match(content), "{content}");
}

#[test]
fn judge_config_defaults_to_direct_anthropic_endpoint() {
    // Invariant 6: the judge talks to the API directly, never the proxy.
    let cfg = JudgeConfig::new("m", "k");
    assert_eq!(cfg.base_url, ANTHROPIC_API_URL);
    assert_eq!(cfg.base_url, "https://api.anthropic.com");
}

// --------------------------------------------------------------------------
// Verdict parsing
// --------------------------------------------------------------------------

#[test]
fn parse_verdict_accepts_bare_json() {
    let (score, rationale) =
        parse_verdict(r#"{"score": 7, "rationale": "names the modules"}"#).unwrap();
    assert_eq!(score, 7.0);
    assert_eq!(rationale, "names the modules");
}

#[test]
fn parse_verdict_tolerates_fences_and_prose() {
    let text = "Here is my verdict:\n```json\n{\"score\": 7.5, \"rationale\": \"solid\"}\n```\n";
    let (score, rationale) = parse_verdict(text).unwrap();
    assert_eq!(score, 7.5);
    assert_eq!(rationale, "solid");
}

#[test]
fn parse_verdict_clamps_out_of_range_scores() {
    let (score, _) = parse_verdict(r#"{"score": 14, "rationale": "x"}"#).unwrap();
    assert_eq!(score, 10.0);
    let (score, _) = parse_verdict(r#"{"score": -2, "rationale": "x"}"#).unwrap();
    assert_eq!(score, 0.0);
}

#[test]
fn parse_verdict_missing_rationale_defaults_empty() {
    let (score, rationale) = parse_verdict(r#"{"score": 3}"#).unwrap();
    assert_eq!(score, 3.0);
    assert_eq!(rationale, "");
}

#[test]
fn parse_verdict_rejects_non_json_replies() {
    assert!(parse_verdict("I would give this a seven out of ten.").is_err());
    assert!(parse_verdict("{\"score\": \"high\"}").is_err());
}

#[test]
fn parse_judge_response_extracts_text_block_and_usage() {
    let body = json!({
        "content": [
            {"type": "tool_use", "name": "irrelevant", "input": {}},
            {"type": "text", "text": "{\"score\": 6, \"rationale\": \"ok\"}"},
        ],
        "usage": {"input_tokens": 321, "output_tokens": 55},
    });
    let v = parse_judge_response(&body, "judge-model").unwrap();
    assert_eq!(v.score, 6.0);
    assert_eq!(v.rationale, "ok");
    assert_eq!(
        v.usage,
        JudgeUsage {
            model: "judge-model".into(),
            input_tokens: 321,
            output_tokens: 55,
        }
    );
}

#[test]
fn parse_judge_response_errors_without_text_block() {
    let body = json!({"content": [], "usage": {"input_tokens": 1, "output_tokens": 1}});
    assert!(parse_judge_response(&body, "m").is_err());
}
