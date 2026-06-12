//! Task 6 tests: price-table cost arithmetic, aggregation semantics
//! (failures excluded from efficiency medians, included in success rate),
//! `results.json` assembly, and the golden-file `report.md` render.

use std::fs;
use std::path::Path;

use gantry_bench::price::{cost_usd, price_for};
use gantry_bench::report::{assemble_results, render_report, results_json, write_artifacts};
use gantry_bench::types::{
    GradeResult, Ledger, LedgerEntry, RunOutcome, RunRecord, RunResult, Usage,
};

const SONNET: &str = "claude-sonnet-4-5-20250929";

fn usage(input: u64, cache_write: u64, cache_read: u64, output: u64) -> Usage {
    Usage {
        input_tokens: input,
        cache_creation_input_tokens: cache_write,
        cache_read_input_tokens: cache_read,
        output_tokens: output,
    }
}

fn entry(model: &str, usage: Option<Usage>, tool_uses: &[&str]) -> LedgerEntry {
    LedgerEntry {
        seq: 0,
        ts_ms: 1_760_000_000_000,
        latency_ms: 1000,
        model: model.to_string(),
        stream: false,
        status: 200,
        usage,
        stop_reason: Some("end_turn".to_string()),
        tool_uses: tool_uses.iter().map(|t| t.to_string()).collect(),
        request_bytes: 1000,
        response_bytes: 500,
        message_count: 1,
        tools_bytes: 0,
    }
}

/// One-entry record; `graded` of `None` = no grade at all (e.g. timeout).
fn record(
    task: &str,
    harness: &str,
    rep: u32,
    graded: Option<bool>,
    uncached_in: u64,
    wall_ms: u64,
) -> RunRecord {
    RunRecord {
        run: RunResult {
            task_id: task.to_string(),
            harness: harness.to_string(),
            rep,
            outcome: RunOutcome::Completed,
            wall_ms,
            exit_code: Some(0),
            answer: Some("answer".to_string()),
            ledger: Ledger {
                entries: vec![entry(SONNET, Some(usage(uncached_in, 0, 0, 100)), &[])],
                untracked_requests: 0,
                untracked_bytes: 0,
            },
            workspace_diff: String::new(),
            stderr_tail: String::new(),
            harness_version: "1.0.0".to_string(),
            gantry_sha: "abc1234".to_string(),
            model: SONNET.to_string(),
        },
        grade: graded.map(|success| GradeResult {
            checks: vec![],
            judge_score: Some(7.0),
            judge_rationale: None,
            success,
        }),
    }
}

// ---------------------------------------------------------------------------
// Cost arithmetic (price.rs)
// ---------------------------------------------------------------------------

#[test]
fn cost_arithmetic_on_known_ledger() {
    let ledger = Ledger {
        entries: vec![
            entry(SONNET, Some(usage(1_000_000, 2_000_000, 10_000_000, 200_000)), &[]),
            entry(SONNET, Some(usage(500_000, 0, 0, 100_000)), &[]),
            // Error response: no usage, unknown model — carries no billable
            // tokens, must not poison the run cost.
            entry("claude-experimental-9000", None, &[]),
        ],
        untracked_requests: 0,
        untracked_bytes: 0,
    };
    // Sonnet 4.5: $3 in, $15 out, $3.75 cache-write (5m), $0.30 cache-read.
    //   entry 0: 1.0M*3 + 2.0M*3.75 + 10.0M*0.30 + 0.2M*15 = 3.0+7.5+3.0+3.0 = 16.5
    //   entry 1: 0.5M*3 + 0.1M*15                           = 1.5+1.5       =  3.0
    let cost = cost_usd(&ledger).expect("known model prices");
    assert!((cost - 19.5).abs() < 1e-9, "got {cost}");
}

#[test]
fn unknown_model_with_usage_makes_cost_none() {
    let ledger = Ledger {
        entries: vec![
            entry(SONNET, Some(usage(1000, 0, 0, 100)), &[]),
            entry("claude-experimental-9000", Some(usage(1, 0, 0, 1)), &[]),
        ],
        untracked_requests: 0,
        untracked_bytes: 0,
    };
    assert_eq!(cost_usd(&ledger), None, "partial pricing must never be reported");
}

#[test]
fn empty_ledger_costs_zero() {
    assert_eq!(cost_usd(&Ledger::default()), Some(0.0));
}

#[test]
fn price_lookup_handles_dated_latest_and_bare_ids() {
    assert!(price_for(SONNET).is_some());
    assert!(price_for("claude-3-5-haiku-latest").is_some());
    assert!(price_for("claude-opus-4").is_some());
    // A dated bare-Opus-4 snapshot resolves to the Opus 4 family ($15/MTok
    // input), not any 4.x sub-family.
    let opus4 = price_for("claude-opus-4-20250514").expect("opus 4 dated id");
    assert!((opus4.input - 15.0).abs() < 1e-9);
    let opus45 = price_for("claude-opus-4-5-20251101").expect("opus 4.5 dated id");
    assert!((opus45.input - 5.0).abs() < 1e-9);
    // Unknown families are never guessed.
    assert!(price_for("claude-experimental-9000").is_none());
    assert!(price_for("gpt-codex-6").is_none());
}

// ---------------------------------------------------------------------------
// Aggregation semantics (report.rs)
// ---------------------------------------------------------------------------

#[test]
fn failures_excluded_from_medians_included_in_success_rate() {
    let records = vec![
        record("t", "gantry", 1, Some(true), 1000, 5000),
        record("t", "gantry", 2, Some(true), 2000, 7000),
        // Failed run with enormous token usage: must move the success rate,
        // must NOT move any efficiency median.
        record("t", "gantry", 3, Some(false), 999_999, 600_000),
    ];
    let md = render_report(&records);
    assert!(md.contains("| gantry | 2/3 (67%) |"), "success rate counts failures:\n{md}");
    assert!(md.contains("| 1500 [1000–2000] |"), "median over successes only:\n{md}");
    assert!(!md.contains("999999"), "failed-run tokens leaked into the report:\n{md}");
}

#[test]
fn ungraded_run_is_not_a_success() {
    let records = vec![record("t", "gantry", 1, None, 1000, 5000)];
    let md = render_report(&records);
    // Zero successes: rate is 0/1 and every efficiency cell is em-dash.
    assert!(md.contains("| gantry | 0/1 (0%) | — | — |"), "{md}");
}

#[test]
fn unpriceable_successful_run_renders_cost_na() {
    let mut rec = record("t", "gantry", 1, Some(true), 1000, 5000);
    rec.run.ledger.entries[0].model = "claude-experimental-9000".to_string();
    let md = render_report(&[rec]);
    assert!(md.contains("| gantry | 1/1 (100%) | n/a |"), "{md}");
}

// ---------------------------------------------------------------------------
// results.json assembly
// ---------------------------------------------------------------------------

#[test]
fn assemble_results_reads_sorts_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let a = record("b-task", "pi", 1, Some(true), 10, 1);
    let b = record("a-task", "claude-code", 2, Some(true), 20, 2);
    let c = record("a-task", "gantry", 1, Some(true), 30, 3);
    fs::write(dir.path().join("z.json"), serde_json::to_string(&a).unwrap()).unwrap();
    fs::write(dir.path().join("y.json"), serde_json::to_string(&b).unwrap()).unwrap();
    fs::write(dir.path().join("x.json"), serde_json::to_string(&c).unwrap()).unwrap();
    fs::write(dir.path().join("notes.txt"), "not a record").unwrap();

    let records = assemble_results(dir.path()).unwrap();
    let keys: Vec<(&str, &str, u32)> = records
        .iter()
        .map(|r| (r.run.task_id.as_str(), r.run.harness.as_str(), r.run.rep))
        .collect();
    // Task alphabetical, then canonical harness order (gantry < claude-code), then rep.
    assert_eq!(
        keys,
        vec![("a-task", "gantry", 1), ("a-task", "claude-code", 2), ("b-task", "pi", 1)]
    );

    let parsed: Vec<RunRecord> = serde_json::from_str(&results_json(&records).unwrap()).unwrap();
    assert_eq!(parsed, records);
}

#[test]
fn write_artifacts_emits_both_files() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let records = vec![record("t", "gantry", 1, Some(true), 100, 50)];
    write_artifacts(&out, &records).unwrap();
    assert!(out.join("results.json").exists());
    let md = fs::read_to_string(out.join("report.md")).unwrap();
    assert!(md.starts_with("# Harness Benchmark Report"));
}

// ---------------------------------------------------------------------------
// Golden file
// ---------------------------------------------------------------------------

#[test]
fn golden_report_md_from_canned_results_json() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/report");
    let records: Vec<RunRecord> =
        serde_json::from_str(&fs::read_to_string(dir.join("results.json")).unwrap()).unwrap();
    let rendered = render_report(&records);
    let golden_path = dir.join("report.golden.md");
    if std::env::var_os("GANTRY_BENCH_BLESS").is_some() {
        fs::write(&golden_path, &rendered).unwrap();
    }
    let golden = fs::read_to_string(&golden_path).unwrap();
    assert_eq!(
        rendered, golden,
        "report.md drifted from golden; rerun with GANTRY_BENCH_BLESS=1 and review the diff"
    );
}
