//! `gantry-bench` CLI entry point.
//!
//! Live benchmark runs spend real API money, so they are gated behind
//! `GANTRY_BENCH_LIVE=1` (mirroring the `GANTRY_LIVE_EVAL` pattern in
//! `evals/`). The one exception: `--smoke` with `GANTRY_BENCH_UPSTREAM` set
//! targets a mock upstream and may run without the gate.

use clap::Parser;
use std::path::PathBuf;

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

    if live.as_deref() == Some("1") && cli.model.is_none() {
        anyhow::bail!("--model <dated-model-id> is required for live runs");
    }

    // Matrix orchestration lands in Task 7 (runner); until it is wired in,
    // a gate-open invocation has nothing to execute.
    anyhow::bail!("the run matrix is not wired up yet (plan Task 7: runner)")
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
