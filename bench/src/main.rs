//! `gantry-bench` CLI entry point.
//!
//! Live benchmark runs spend real API money, so they are gated behind
//! `GANTRY_BENCH_LIVE=1` (mirroring the `GANTRY_LIVE_EVAL` pattern in
//! `evals/`). The one exception: `--smoke` with `GANTRY_BENCH_UPSTREAM` set
//! targets a mock upstream and may run without the gate.

use clap::Parser;
use std::path::PathBuf;

use gantry_bench::grade::JudgeConfig;
use gantry_bench::harness;
use gantry_bench::proxy;
use gantry_bench::runner::{self, RunnerConfig, NON_BINDING_MAX_TOKENS};
use gantry_bench::task::{self, RepoCache};
use gantry_bench::types::RunOutcome;

#[derive(Parser, Debug)]
#[command(
    name = "gantry-bench",
    about = "Benchmark gantry vs other agent harnesses through a recording proxy"
)]
struct Cli {
    /// Run only these task ids (repeatable). Default: every task under
    /// `bench/tasks/`.
    #[arg(long = "task", value_name = "ID")]
    tasks: Vec<String>,

    /// Run only these harnesses (repeatable): gantry | claude-code | pi.
    /// Default: all three.
    #[arg(long = "harness", value_name = "NAME")]
    harnesses: Vec<String>,

    /// Repetitions per (task × harness) cell.
    #[arg(long, value_name = "N", default_value_t = 3)]
    reps: u32,

    /// Pinned dated model id for benchmark runs. Required for live runs.
    #[arg(long, value_name = "DATED_MODEL_ID")]
    model: Option<String>,

    /// Smoke mode: one cheap task × gantry × 1 rep, validates plumbing.
    /// With GANTRY_BENCH_UPSTREAM set it runs against a mock upstream and
    /// bypasses the live gate.
    #[arg(long)]
    smoke: bool,

    /// Output directory. Default: `bench/results/<UTC yyyymmdd-HHMMSS>`.
    #[arg(long, value_name = "DIR")]
    out: Option<PathBuf>,
}

/// Whether this invocation may actually run. `live` / `upstream` are the
/// values of `GANTRY_BENCH_LIVE` / `GANTRY_BENCH_UPSTREAM`. The API key is
/// often ambiently present (mise loads .envrc), so a key check alone would
/// not be a reliable gate.
fn live_gate_open(live: Option<&str>, smoke: bool, upstream: Option<&str>) -> bool {
    live == Some("1") || (smoke && upstream.is_some())
}

/// Model id used for keyless mock-upstream smoke runs when `--model` is
/// omitted (a mock upstream ignores the id; live runs always require
/// `--model`). MUST be an id rig's Anthropic registry recognizes: gantry
/// never sets a per-request `max_tokens`, so rig derives it from the model
/// id and hard-fails on unknown ids ("`max_tokens` must be set for
/// Anthropic") before any request reaches the proxy.
const SMOKE_FALLBACK_MODEL: &str = "claude-haiku-4-5";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let live = std::env::var("GANTRY_BENCH_LIVE").ok();
    let upstream = std::env::var("GANTRY_BENCH_UPSTREAM").ok();
    if !live_gate_open(live.as_deref(), cli.smoke, upstream.as_deref()) {
        println!(
            "gantry-bench: live gate closed — set GANTRY_BENCH_LIVE=1 to run the benchmark \
             (live runs cost real API money), or use --smoke with GANTRY_BENCH_UPSTREAM \
             pointed at a mock upstream"
        );
        return Ok(());
    }
    let is_live = live.as_deref() == Some("1");

    if is_live && cli.model.is_none() {
        anyhow::bail!("--model <dated-model-id> is required for live runs");
    }
    let api_key = std::env::var("ANTHROPIC_API_KEY").ok();
    if is_live && api_key.is_none() {
        anyhow::bail!("ANTHROPIC_API_KEY is required for live runs");
    }
    if cli.reps == 0 {
        anyhow::bail!("--reps must be at least 1");
    }

    let mut tasks = task::load_tasks(&task::default_tasks_dir())?;
    if !cli.tasks.is_empty() {
        for id in &cli.tasks {
            if !tasks.iter().any(|t| &t.manifest.id == id) {
                anyhow::bail!("unknown task id {id:?} (not found under bench/tasks/)");
            }
        }
        tasks.retain(|t| cli.tasks.contains(&t.manifest.id));
    }
    let mut harnesses = harness::all();
    if !cli.harnesses.is_empty() {
        for name in &cli.harnesses {
            if !harnesses.iter().any(|h| h.name() == name) {
                anyhow::bail!("unknown harness {name:?} (expected gantry | claude-code | pi)");
            }
        }
        harnesses.retain(|h| cli.harnesses.iter().any(|n| n == h.name()));
    }
    let mut reps = cli.reps;
    if cli.smoke {
        (tasks, harnesses) = runner::smoke_selection(tasks, harnesses)?;
        reps = 1;
    }

    let model = cli
        .model
        .clone()
        .unwrap_or_else(|| SMOKE_FALLBACK_MODEL.to_string());
    let out_dir = cli.out.clone().unwrap_or_else(runner::default_out_dir);
    // Judge with the benchmark's pinned model; live runs only — keyless mock
    // runs grade rubric tasks as failed-judge instead of calling out.
    let judge = if is_live {
        Some(JudgeConfig::new(
            model.clone(),
            api_key.clone().expect("checked above for live runs"),
        ))
    } else {
        None
    };

    let cfg = RunnerConfig {
        tasks,
        harnesses,
        reps,
        model,
        api_key: api_key.unwrap_or_else(|| "gantry-bench-keyless".to_string()),
        upstream: proxy::upstream_from_env(),
        out_dir: out_dir.clone(),
        cache: RepoCache::shared(),
        judge,
        gantry_sha: runner::gantry_sha(),
        max_tokens: NON_BINDING_MAX_TOKENS,
    };
    let records = runner::run_suite(&cfg).await?;

    // Assemble the run-level artifacts next to raw/ (canonical results layout:
    // raw/*.json + results.json + report.md).
    gantry_bench::report::write_artifacts(&out_dir, &records)?;

    let count = |o: RunOutcome| records.iter().filter(|r| r.run.outcome == o).count();
    println!(
        "gantry-bench: {} runs ({} completed, {} timeout, {} crashed) → {}",
        records.len(),
        count(RunOutcome::Completed),
        count(RunOutcome::Timeout),
        count(RunOutcome::Crashed),
        out_dir.display(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn cli_parses_full_contract() {
        let cli = Cli::try_parse_from([
            "gantry-bench",
            "--task",
            "locate-bug",
            "--task",
            "needle-haystack",
            "--harness",
            "gantry",
            "--harness",
            "claude-code",
            "--reps",
            "5",
            "--model",
            "claude-test-model-20260101",
            "--smoke",
            "--out",
            "bench/results/custom",
        ])
        .unwrap();
        assert_eq!(cli.tasks, ["locate-bug", "needle-haystack"]);
        assert_eq!(cli.harnesses, ["gantry", "claude-code"]);
        assert_eq!(cli.reps, 5);
        assert_eq!(cli.model.as_deref(), Some("claude-test-model-20260101"));
        assert!(cli.smoke);
        assert_eq!(cli.out, Some(PathBuf::from("bench/results/custom")));
    }

    #[test]
    fn cli_defaults() {
        let cli = Cli::try_parse_from(["gantry-bench"]).unwrap();
        assert!(cli.tasks.is_empty());
        assert!(cli.harnesses.is_empty());
        assert_eq!(cli.reps, 3);
        assert_eq!(cli.model, None);
        assert!(!cli.smoke);
        assert_eq!(cli.out, None);
    }

    #[test]
    fn live_gate_logic() {
        // Gate closed: no env, regardless of smoke.
        assert!(!live_gate_open(None, false, None));
        assert!(!live_gate_open(None, true, None));
        // GANTRY_BENCH_LIVE must be exactly "1".
        assert!(!live_gate_open(Some("0"), false, None));
        assert!(!live_gate_open(Some("true"), false, None));
        assert!(live_gate_open(Some("1"), false, None));
        // --smoke + mock upstream bypasses the gate.
        assert!(live_gate_open(None, true, Some("http://127.0.0.1:9999")));
        // Upstream alone (no --smoke) does not.
        assert!(!live_gate_open(None, false, Some("http://127.0.0.1:9999")));
    }
}
