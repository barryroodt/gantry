//! (task × harness × rep) matrix orchestration (plan Task 7).
//!
//! Per run: materialize a fresh workspace (task API) → start a recorder proxy
//! (proxy API) → spawn the harness subprocess with the per-task timeout →
//! collect a [`RunResult`] (proxy ledger, wall time, exit status, extracted
//! answer, workspace `git diff`, stderr tail, versions) → grade completed
//! runs (grade API) → persist the [`RunRecord`] to `raw/` immediately, so a
//! half-finished suite is analyzable.
//!
//! The suite always continues: workspace/proxy/spawn failures become
//! `crashed` records, timeouts become `timeout` records, and only persistence
//! IO errors (broken disk) abort the suite.
//!
//! ## Process-tree kill — platform caveat
//!
//! On Unix the harness is spawned as its own process group (`pgid == pid`)
//! and a timeout kills the whole group (`kill -9 -- -<pgid>`), so harness
//! grandchildren (shells, language servers, …) die with it. On non-Unix
//! targets only the direct child is killed; grandchildren spawned by the
//! harness may outlive the timeout.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::io::AsyncReadExt;
use tokio::task::JoinHandle;

use crate::grade::{self, GradeSpec, JudgeConfig, JudgeOutcome};
use crate::harness::{Harness, RunCtx};
use crate::proxy::RecorderProxy;
use crate::task::{GradingSpec, RepoCache, Task, TaskKind};
use crate::types::{GradeResult, Ledger, RunOutcome, RunRecord, RunResult};

/// `--max-tokens` budget handed to gantry. Deliberately far beyond any real
/// run so the budget is never binding — the other harnesses have no
/// equivalent cap, and a binding budget would skew the comparison.
pub const NON_BINDING_MAX_TOKENS: u64 = 1_000_000;

/// Last bytes of harness stderr kept on the [`RunResult`] (canonical schema:
/// 4 KiB).
const STDERR_TAIL_BYTES: usize = 4096;

/// How long to keep draining stdout/stderr after the child is gone. Pipes
/// normally hit EOF immediately (the group kill takes the writers with it);
/// the grace bound only guards against a leaked non-Unix grandchild holding
/// the pipe open forever.
const READ_GRACE: Duration = Duration::from_secs(5);

/// Everything one suite invocation needs. Assembled by `main` (or a test).
pub struct RunnerConfig {
    /// Tasks to run, in suite order.
    pub tasks: Vec<Task>,
    /// Harnesses to run, in canonical report order.
    pub harnesses: Vec<Box<dyn Harness>>,
    /// Repetitions per (task × harness) cell; reps are numbered `1..=reps`.
    pub reps: u32,
    /// Pinned dated model id, identical for every harness (fairness §1).
    pub model: String,
    /// Real API key for live runs; a placeholder in keyless mock runs.
    pub api_key: String,
    /// Proxy upstream — `https://api.anthropic.com` live, a mock in tests
    /// (`GANTRY_BENCH_UPSTREAM`).
    pub upstream: String,
    /// Results directory; raw records land in `<out_dir>/raw/`.
    pub out_dir: PathBuf,
    /// Workspace materialization cache.
    pub cache: RepoCache,
    /// Judge for rubric-graded tasks; `None` in keyless runs (rubric tasks
    /// then grade as failed-judge, never killing the suite).
    pub judge: Option<JudgeConfig>,
    /// Benchmarked gantry build's git SHA, recorded on every run.
    pub gantry_sha: String,
    /// See [`NON_BINDING_MAX_TOKENS`].
    pub max_tokens: u64,
}

/// Run the full (task × harness × rep) matrix, persisting each
/// [`RunRecord`] to `<out_dir>/raw/<task>-<harness>-r<rep>.json` as it
/// finishes. Individual run failures never abort the suite.
pub async fn run_suite(cfg: &RunnerConfig) -> Result<Vec<RunRecord>> {
    let raw_dir = cfg.out_dir.join("raw");
    std::fs::create_dir_all(&raw_dir)
        .with_context(|| format!("creating results dir {}", raw_dir.display()))?;

    let mut records = Vec::new();
    for task in &cfg.tasks {
        for harness in &cfg.harnesses {
            for rep in 1..=cfg.reps {
                let record = run_one(cfg, task, harness.as_ref(), rep).await;
                eprintln!(
                    "gantry-bench: {} × {} × r{}: {:?} in {} ms",
                    record.run.task_id,
                    record.run.harness,
                    rep,
                    record.run.outcome,
                    record.run.wall_ms,
                );
                persist_record(&raw_dir, &record)?;
                records.push(record);
            }
        }
    }
    Ok(records)
}

/// `--smoke` selection: the first `explore` task in suite order × the gantry
/// harness (× 1 rep, set by the caller). Validates plumbing as cheaply as
/// possible; honors `GANTRY_BENCH_UPSTREAM` because the upstream is resolved
/// by the caller exactly as for a full run.
pub fn smoke_selection(
    tasks: Vec<Task>,
    harnesses: Vec<Box<dyn Harness>>,
) -> Result<(Vec<Task>, Vec<Box<dyn Harness>>)> {
    let task = tasks
        .into_iter()
        .find(|t| t.manifest.kind == TaskKind::Explore)
        .context("--smoke requires at least one `explore` task in the selection")?;
    let harness = harnesses
        .into_iter()
        .find(|h| h.name() == "gantry")
        .context("--smoke requires the gantry harness (excluded by --harness?)")?;
    Ok((vec![task], vec![harness]))
}

/// Default results directory: `bench/results/<UTC yyyymmdd-HHMMSS>`.
pub fn default_out_dir() -> PathBuf {
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    Path::new(env!("CARGO_MANIFEST_DIR")).join("results").join(ts)
}

/// Git SHA of this gantry checkout, `"unknown"` when undeterminable.
pub fn gantry_sha() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

// ---------------------------------------------------------------------------
// One run
// ---------------------------------------------------------------------------

/// Execute one (task × harness × rep) cell. Infallible by contract: every
/// failure mode maps to an outcome on the record (suite always continues).
async fn run_one(cfg: &RunnerConfig, task: &Task, harness: &dyn Harness, rep: u32) -> RunRecord {
    // Infrastructure that can fail before the harness exists → `crashed`
    // records with the reason in stderr_tail.
    let workspace = match cfg.cache.materialize(&task.manifest.workspace) {
        Ok(ws) => ws,
        Err(e) => return infra_crash(cfg, task, harness, rep, &format!("workspace: {e:#}")),
    };
    let config_dir = match tempfile::TempDir::with_prefix("gantry-bench-cfg-") {
        Ok(dir) => dir,
        Err(e) => return infra_crash(cfg, task, harness, rep, &format!("config dir: {e}")),
    };
    let prompt_file = config_dir.path().join("prompt.md");
    if let Err(e) = std::fs::write(&prompt_file, &task.prompt) {
        return infra_crash(cfg, task, harness, rep, &format!("prompt file: {e}"));
    }
    let proxy = match RecorderProxy::start(&cfg.upstream).await {
        Ok(p) => p,
        Err(e) => return infra_crash(cfg, task, harness, rep, &format!("proxy: {e:#}")),
    };

    let ctx = RunCtx {
        workspace: workspace.path().to_path_buf(),
        prompt_file,
        model: cfg.model.clone(),
        proxy_url: proxy.base_url(),
        config_dir: config_dir.path().to_path_buf(),
        mutate: task.manifest.kind == TaskKind::Mutate,
        api_key: cfg.api_key.clone(),
        timeout_ms: task.manifest.timeout_ms,
        max_tokens: cfg.max_tokens,
    };
    let exec = execute(
        harness.command(&ctx),
        Duration::from_millis(task.manifest.timeout_ms),
    )
    .await;
    let ledger = proxy.shutdown().await;

    let answer = harness.extract_answer(&exec.stdout);
    let workspace_diff = workspace.diff().unwrap_or_else(|e| {
        eprintln!(
            "gantry-bench: {} × {} × r{rep}: workspace diff failed: {e:#}",
            task.manifest.id,
            harness.name(),
        );
        String::new()
    });

    let run = RunResult {
        task_id: task.manifest.id.clone(),
        harness: harness.name().to_string(),
        rep,
        outcome: exec.outcome,
        wall_ms: exec.wall_ms,
        exit_code: exec.exit_code,
        answer,
        ledger,
        workspace_diff,
        stderr_tail: exec.stderr_tail,
        harness_version: harness.version(),
        gantry_sha: cfg.gantry_sha.clone(),
        model: cfg.model.clone(),
    };

    // Only completed runs are graded; timeout/crashed runs have no meaningful
    // answer or workspace state to judge and carry `grade: None`.
    let grade = match run.outcome {
        RunOutcome::Completed => Some(grade_completed(cfg, task, &run, workspace.path()).await),
        RunOutcome::Timeout | RunOutcome::Crashed => None,
    };
    RunRecord { run, grade }
}

/// A `crashed` record for failures in the bench's own plumbing (no harness
/// process ever ran). The ledger is empty by construction.
fn infra_crash(
    cfg: &RunnerConfig,
    task: &Task,
    harness: &dyn Harness,
    rep: u32,
    reason: &str,
) -> RunRecord {
    RunRecord {
        run: RunResult {
            task_id: task.manifest.id.clone(),
            harness: harness.name().to_string(),
            rep,
            outcome: RunOutcome::Crashed,
            wall_ms: 0,
            exit_code: None,
            answer: None,
            ledger: Ledger::default(),
            workspace_diff: String::new(),
            stderr_tail: format!("bench infrastructure error: {reason}"),
            harness_version: harness.version(),
            gantry_sha: cfg.gantry_sha.clone(),
            model: cfg.model.clone(),
        },
        grade: None,
    }
}

/// Grade one completed run: convert the manifest's grading table, resolve the
/// judge outcome (never fatally), and fold both into a [`GradeResult`].
async fn grade_completed(
    cfg: &RunnerConfig,
    task: &Task,
    run: &RunResult,
    workspace: &Path,
) -> GradeResult {
    let spec = to_grade_spec(&task.manifest.grading);
    let judge = judge_outcome(cfg, task, run.answer.as_deref()).await;
    grade::grade_run(&spec, run, workspace, judge)
}

/// Manifest grading table → grade-module spec (the two differ only in
/// `Option<Vec>` vs `Vec` for the diff checks).
fn to_grade_spec(g: &GradingSpec) -> GradeSpec {
    GradeSpec {
        answer_contains: g.answer_contains.clone(),
        check_command: g.check_command.clone(),
        diff_contains: g.diff_contains.clone().unwrap_or_default(),
        diff_must_not_touch: g.diff_must_not_touch.clone().unwrap_or_default(),
        judge_threshold: g.judge_threshold,
    }
}

/// Resolve the judge outcome for one run. Judge problems surface as
/// [`JudgeOutcome::Failed`] (a failing synthetic check downstream) — they
/// never abort the suite (spec §error handling).
async fn judge_outcome(cfg: &RunnerConfig, task: &Task, answer: Option<&str>) -> JudgeOutcome {
    let Some(rubric) = task.rubric.as_deref() else {
        return JudgeOutcome::NotRequired;
    };
    let Some(judge) = &cfg.judge else {
        return JudgeOutcome::Failed("rubric present but no judge configured (keyless run)".into());
    };
    let Some(answer) = answer else {
        return JudgeOutcome::Failed("no answer extracted from harness stdout".into());
    };
    match grade::run_judge(judge, &task.prompt, rubric, answer).await {
        Ok(verdict) => {
            // Judge usage is bookkeeping only (invariant 6): GradeResult has
            // no usage field in the canonical schema, so report it here —
            // never into the Ledger.
            eprintln!(
                "gantry-bench: judge ({}) usage for {}: {} in / {} out tokens",
                verdict.usage.model,
                task.manifest.id,
                verdict.usage.input_tokens,
                verdict.usage.output_tokens,
            );
            JudgeOutcome::Scored(verdict)
        }
        Err(e) => JudgeOutcome::Failed(format!("{e:#}")),
    }
}

/// Write one record to `raw/<task>-<harness>-r<rep>.json`. The only fatal
/// error path in the suite loop (persistence IO).
fn persist_record(raw_dir: &Path, record: &RunRecord) -> Result<PathBuf> {
    let name = format!(
        "{}-{}-r{}.json",
        record.run.task_id, record.run.harness, record.run.rep
    );
    let path = raw_dir.join(name);
    let json = serde_json::to_string_pretty(record).context("serializing RunRecord")?;
    std::fs::write(&path, json)
        .with_context(|| format!("writing run record {}", path.display()))?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// Subprocess execution
// ---------------------------------------------------------------------------

/// What one harness subprocess execution produced.
struct Exec {
    outcome: RunOutcome,
    exit_code: Option<i32>,
    wall_ms: u64,
    stdout: String,
    stderr_tail: String,
}

/// Spawn the harness command (its own process group on Unix), enforce the
/// per-task timeout with a process-group kill, and capture output.
///
/// Outcome mapping (plan Task 7): clean exit → `completed`; non-zero exit
/// with stdout → `completed` (grading decides success); timeout → `timeout`;
/// spawn failure or non-zero exit with empty stdout → `crashed`.
async fn execute(mut cmd: std::process::Command, timeout: Duration) -> Exec {
    let started = Instant::now();

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // New process group with pgid == pid, so a timeout can kill the whole
        // tree. See module docs for the non-Unix caveat.
        cmd.process_group(0);
    }
    let mut command = tokio::process::Command::from(cmd);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return Exec {
                outcome: RunOutcome::Crashed,
                exit_code: None,
                wall_ms: started.elapsed().as_millis() as u64,
                stdout: String::new(),
                stderr_tail: format!("spawn failed: {e}"),
            }
        }
    };
    let pid = child.id();

    // Incremental readers: partial output survives a kill or a read-grace
    // timeout (a one-shot read_to_end would lose everything).
    let (stdout_buf, stdout_task) = spawn_reader(child.stdout.take().expect("stdout piped"));
    let (stderr_buf, stderr_task) = spawn_reader(child.stderr.take().expect("stderr piped"));

    let (timed_out, status) = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => (false, Some(status)),
        Ok(Err(_)) => (false, None),
        Err(_) => {
            kill_tree(&mut child, pid).await;
            (true, None)
        }
    };
    let wall_ms = started.elapsed().as_millis() as u64;

    for task in [stdout_task, stderr_task] {
        if tokio::time::timeout(READ_GRACE, task).await.is_err() {
            // Leaked writer holding the pipe (non-Unix caveat); keep whatever
            // the reader accumulated so far.
        }
    }
    let stdout_bytes = take_buf(&stdout_buf);
    let stderr_bytes = take_buf(&stderr_buf);
    let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();

    let outcome = if timed_out {
        RunOutcome::Timeout
    } else {
        match &status {
            Some(s) if s.success() || !stdout.trim().is_empty() => RunOutcome::Completed,
            _ => RunOutcome::Crashed,
        }
    };
    Exec {
        outcome,
        exit_code: status.and_then(|s| s.code()),
        wall_ms,
        stdout,
        stderr_tail: tail_lossy(&stderr_bytes, STDERR_TAIL_BYTES),
    }
}

/// Drain a pipe into a shared buffer, chunk by chunk, until EOF or error.
fn spawn_reader<R>(mut pipe: R) -> (Arc<Mutex<Vec<u8>>>, JoinHandle<()>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let buf = Arc::new(Mutex::new(Vec::new()));
    let writer = Arc::clone(&buf);
    let task = tokio::spawn(async move {
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => writer
                    .lock()
                    .expect("reader buffer mutex poisoned")
                    .extend_from_slice(&chunk[..n]),
            }
        }
    });
    (buf, task)
}

fn take_buf(buf: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
    std::mem::take(&mut *buf.lock().expect("reader buffer mutex poisoned"))
}

/// SIGKILL the harness and everything it spawned.
///
/// Unix: the child is its own process group leader (`process_group(0)` at
/// spawn), so `kill -9 -- -<pgid>` (POSIX `kill(1)` group syntax) takes down
/// the whole tree. Non-Unix: only the direct child dies (`Child::kill`);
/// grandchildren may leak — documented platform caveat.
async fn kill_tree(child: &mut tokio::process::Child, pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        let _ = tokio::process::Command::new("kill")
            .args(["-9", "--", &format!("-{pid}")])
            .status()
            .await;
    }
    #[cfg(not(unix))]
    let _ = pid;
    // Direct-child kill: non-Unix path, and a no-op backstop on Unix.
    // tokio's kill() also reaps the exit status.
    let _ = child.kill().await;
}

/// Last `max` bytes as lossy UTF-8 (a mid-codepoint cut becomes U+FFFD).
fn tail_lossy(bytes: &[u8], max: usize) -> String {
    let start = bytes.len().saturating_sub(max);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}
