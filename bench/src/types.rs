//! Canonical serde types for every JSON artifact the bench produces.
//!
//! These names and fields are contracts (plan §Canonical Schema). All other
//! modules use these types; defining parallel result structs elsewhere is
//! prohibited by the shared architectural invariants.

use serde::{Deserialize, Serialize};

/// Token usage for one API exchange, as reported by the Anthropic Messages
/// API. Absent fields parse as 0 so partial `usage` objects (e.g. streaming
/// `message_delta`) deserialize cleanly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

/// One recorded `/v1/messages` exchange through the proxy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Order of the exchange within the run (0-based, assigned by the proxy).
    pub seq: u32,
    /// Request receipt time, ms since the Unix epoch.
    pub ts_ms: u64,
    /// Full exchange latency (request received → response fully relayed).
    pub latency_ms: u64,
    /// Model id from the request body.
    pub model: String,
    /// Whether the request asked for an SSE stream.
    pub stream: bool,
    /// Upstream HTTP status.
    pub status: u16,
    /// Usage from the response; `None` when the response carried none
    /// (e.g. error responses).
    pub usage: Option<Usage>,
    pub stop_reason: Option<String>,
    /// Names of `tool_use` content blocks in the response, in order.
    pub tool_uses: Vec<String>,
    pub request_bytes: u64,
    pub response_bytes: u64,
    /// Length of the `messages` array in the request.
    pub message_count: u32,
    /// Size of the request's `tools` field re-serialized to compact JSON
    /// (0 when absent). A *normalized* measure of tool-schema payload —
    /// comparable across harnesses because each harness's real whitespace and
    /// key order are canonicalized away — NOT an on-wire byte count;
    /// `request_bytes` is the as-received wire size.
    pub tools_bytes: u64,
}

/// Everything the proxy recorded for one run. The single source of truth for
/// all efficiency metrics.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ledger {
    pub entries: Vec<LedgerEntry>,
    /// Requests to paths other than `/v1/messages` (forwarded, not parsed).
    pub untracked_requests: u32,
    pub untracked_bytes: u64,
}

/// How a harness run ended, from the orchestrator's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Completed,
    Timeout,
    Crashed,
}

/// One (task × harness × rep) run, pre-grading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunResult {
    pub task_id: String,
    /// `gantry` | `claude-code` | `pi`.
    pub harness: String,
    pub rep: u32,
    pub outcome: RunOutcome,
    pub wall_ms: u64,
    pub exit_code: Option<i32>,
    /// Final answer text extracted from harness stdout (judging input only —
    /// never a metrics source).
    pub answer: Option<String>,
    pub ledger: Ledger,
    /// `git diff` of the disposable workspace after the run.
    pub workspace_diff: String,
    /// Last 4 KiB of harness stderr.
    pub stderr_tail: String,
    pub harness_version: String,
    pub gantry_sha: String,
    pub model: String,
}

/// One programmatic grading check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub pass: bool,
    pub detail: Option<String>,
}

/// Token usage of one blinded-judge call — bookkeeping only (invariant 6):
/// persisted and reported separately, never merged into a [`Ledger`] or any
/// efficiency metric.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JudgeUsage {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Grading verdict for one run. `success` = all programmatic checks pass AND
/// judge score ≥ the task threshold (invariant 7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GradeResult {
    pub checks: Vec<CheckResult>,
    pub judge_score: Option<f32>,
    pub judge_rationale: Option<String>,
    /// Usage of the judge call that produced `judge_score`; `None` when the
    /// task had no rubric or the judge failed. Canonical-schema amendment
    /// (review hb-review-1 m1): defaults on deserialize so pre-amendment
    /// records still parse.
    #[serde(default)]
    pub judge_usage: Option<JudgeUsage>,
    pub success: bool,
}

/// The unit persisted per run: `raw/<task>-<harness>-r<rep>.json` and the
/// elements of `results.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run: RunResult,
    pub grade: Option<GradeResult>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record() -> RunRecord {
        RunRecord {
            run: RunResult {
                task_id: "locate-bug".into(),
                harness: "claude-code".into(),
                rep: 2,
                outcome: RunOutcome::Completed,
                wall_ms: 73_421,
                exit_code: Some(0),
                answer: Some("the bug is in src/parser.rs".into()),
                ledger: Ledger {
                    entries: vec![LedgerEntry {
                        seq: 0,
                        ts_ms: 1_770_000_000_000,
                        latency_ms: 2_310,
                        model: "claude-test-model-20260101".into(),
                        stream: true,
                        status: 200,
                        usage: Some(Usage {
                            input_tokens: 1_204,
                            cache_creation_input_tokens: 0,
                            cache_read_input_tokens: 18_530,
                            output_tokens: 412,
                        }),
                        stop_reason: Some("tool_use".into()),
                        tool_uses: vec!["read_file".into(), "grep".into()],
                        request_bytes: 9_812,
                        response_bytes: 4_410,
                        message_count: 7,
                        tools_bytes: 3_120,
                    }],
                    untracked_requests: 1,
                    untracked_bytes: 512,
                },
                workspace_diff: String::new(),
                stderr_tail: String::new(),
                harness_version: "1.2.3".into(),
                gantry_sha: "f8975fa".into(),
                model: "claude-test-model-20260101".into(),
            },
            grade: Some(GradeResult {
                checks: vec![CheckResult {
                    name: "answer_contains".into(),
                    pass: true,
                    detail: None,
                }],
                judge_score: Some(8.5),
                judge_rationale: Some("correct root cause".into()),
                judge_usage: Some(JudgeUsage {
                    model: "claude-judge-20260101".into(),
                    input_tokens: 880,
                    output_tokens: 61,
                }),
                success: true,
            }),
        }
    }

    #[test]
    fn run_record_round_trips() {
        let record = sample_record();
        let json = serde_json::to_string(&record).unwrap();
        let back: RunRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record, back);
    }

    #[test]
    fn run_record_round_trips_with_nones() {
        let mut record = sample_record();
        record.run.outcome = RunOutcome::Crashed;
        record.run.exit_code = None;
        record.run.answer = None;
        record.run.ledger.entries[0].usage = None;
        record.run.ledger.entries[0].stop_reason = None;
        record.grade = None;
        let json = serde_json::to_string(&record).unwrap();
        let back: RunRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record, back);
    }

    #[test]
    fn absent_usage_fields_default_to_zero() {
        // Streaming message_delta events carry partial usage objects.
        let usage: Usage = serde_json::from_str(r#"{"output_tokens": 99}"#).unwrap();
        assert_eq!(
            usage,
            Usage {
                input_tokens: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                output_tokens: 99,
            }
        );
        let empty: Usage = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, Usage::default());
    }

    #[test]
    fn run_outcome_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&RunOutcome::Completed).unwrap(),
            r#""completed""#
        );
        assert_eq!(
            serde_json::to_string(&RunOutcome::Timeout).unwrap(),
            r#""timeout""#
        );
        assert_eq!(
            serde_json::to_string(&RunOutcome::Crashed).unwrap(),
            r#""crashed""#
        );
        let back: RunOutcome = serde_json::from_str(r#""timeout""#).unwrap();
        assert_eq!(back, RunOutcome::Timeout);
    }
}
