//! Plan Task 7 tests: end-to-end matrix orchestration with a fake harness.
//! Keyless and networkless beyond loopback (invariant 4): the fake harness is
//! a shell script that talks through the live recorder proxy to an in-test
//! wiremock upstream, writes a file into the disposable workspace, and prints
//! a known answer; a second task sleeps past its timeout to prove the
//! process-group kill, the `timeout` outcome, incremental persistence, and
//! suite continuation.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use gantry_bench::grade::GradeSpec;
use gantry_bench::harness::{Harness, RunCtx};
use gantry_bench::runner::{self, RunnerConfig};
use gantry_bench::task::{RepoCache, Task, TaskKind, TaskManifest, WorkspaceSpec};
use gantry_bench::types::{RunOutcome, RunRecord, Usage};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Canned non-streaming Messages response with fixture-known usage numbers
/// and one tool_use block.
const JSON_RESPONSE: &str = r#"{"id":"msg_01","type":"message","role":"assistant","model":"claude-fake-1","content":[{"type":"text","text":"done"},{"type":"tool_use","id":"toolu_01","name":"read_file","input":{"path":"a.rs"}}],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":17,"cache_creation_input_tokens":3,"cache_read_input_tokens":5,"output_tokens":42}}"#;

const KNOWN_ANSWER: &str = "the-known-answer";
const ARTIFACT_FILE: &str = "bench-artifact.txt";

/// Hermetic git for fixture construction (same pattern as task_test.rs).
fn git(dir: &Path, args: &[&str]) -> String {
    let out = StdCommand::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@invalid")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// One-commit fixture origin repo; returns (origin_path, sha).
fn fixture_origin(root: &Path) -> (PathBuf, String) {
    let origin = root.join("origin");
    fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "--quiet", "-b", "main"]);
    fs::write(origin.join("README.md"), "fixture\n").unwrap();
    git(&origin, &["add", "-A"]);
    git(&origin, &["commit", "--quiet", "--no-gpg-sign", "-m", "c1"]);
    let sha = git(&origin, &["rev-parse", "HEAD"]).trim().to_owned();
    (origin, sha)
}

/// In-memory task (no task.toml on disk — the runner consumes parsed `Task`s).
fn mem_task(
    id: &str,
    kind: TaskKind,
    prompt: &str,
    timeout_ms: u64,
    origin: &Path,
    sha: &str,
    grading: GradeSpec,
) -> Task {
    Task {
        manifest: TaskManifest {
            id: id.to_string(),
            kind,
            timeout_ms,
            workspace: WorkspaceSpec {
                repo_url: origin.display().to_string(),
                sha: sha.to_string(),
            },
            grading,
        },
        dir: origin.to_path_buf(),
        prompt: prompt.to_string(),
        rubric: None,
    }
}

/// The fake harness script. Branches on the prompt file content (each task
/// has its own prompt), so one harness serves the whole matrix:
/// - `BENCH-SLEEP`  → sleep far past the task timeout (kill-path case)
/// - `BENCH-EXIT3`  → exit 3 with no stdout (crash-mapping case)
/// - otherwise      → one POST through the proxy, a workspace write, a known
///   answer on stdout, and a marker on stderr
const FAKE_SCRIPT: &str = r#"#!/bin/sh
if grep -q BENCH-SLEEP "$FAKE_PROMPT_FILE"; then
    sleep 30
    exit 0
fi
if grep -q BENCH-EXIT3 "$FAKE_PROMPT_FILE"; then
    exit 3
fi
curl -s -o /dev/null -X POST "$FAKE_PROXY_URL/v1/messages" \
    -H 'content-type: application/json' \
    -H "x-api-key: $FAKE_API_KEY" \
    --data '{"model":"claude-fake-1","stream":false,"max_tokens":64,"messages":[{"role":"user","content":"hi"}]}'
printf 'bench artifact\n' > bench-artifact.txt
echo "fake harness stderr marker" >&2
echo "ANSWER: the-known-answer"
"#;

/// Test-only harness driven through the same `Harness` trait as the real
/// adapters: asserts the runner's env/cwd wiring end to end.
struct FakeHarness {
    script: PathBuf,
}

impl FakeHarness {
    fn create(dir: &Path) -> Self {
        let script = dir.join("fake-harness.sh");
        fs::write(&script, FAKE_SCRIPT).unwrap();
        Self { script }
    }
}

impl Harness for FakeHarness {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn command(&self, ctx: &RunCtx) -> StdCommand {
        let mut cmd = StdCommand::new("sh");
        cmd.arg(&self.script)
            .current_dir(&ctx.workspace)
            .env("FAKE_PROXY_URL", &ctx.proxy_url)
            .env("FAKE_PROMPT_FILE", &ctx.prompt_file)
            .env("FAKE_API_KEY", &ctx.api_key);
        cmd
    }

    fn extract_answer(&self, stdout: &str) -> Option<String> {
        stdout
            .lines()
            .find_map(|l| l.strip_prefix("ANSWER: "))
            .map(str::to_string)
    }

    fn version(&self) -> String {
        "fake-1.0".to_string()
    }
}

/// Harness whose binary does not exist: spawn-failure → `crashed`.
struct BrokenHarness;

impl Harness for BrokenHarness {
    fn name(&self) -> &'static str {
        "broken"
    }

    fn command(&self, ctx: &RunCtx) -> StdCommand {
        let mut cmd = StdCommand::new("/nonexistent/gantry-bench-no-such-binary");
        cmd.current_dir(&ctx.workspace);
        cmd
    }

    fn extract_answer(&self, _stdout: &str) -> Option<String> {
        None
    }

    fn version(&self) -> String {
        "broken-0.0".to_string()
    }
}

fn config(
    root: &TempDir,
    upstream: String,
    tasks: Vec<Task>,
    harnesses: Vec<Box<dyn Harness>>,
) -> RunnerConfig {
    RunnerConfig {
        tasks,
        harnesses,
        reps: 1,
        model: "claude-fake-1".to_string(),
        api_key: "test-key".to_string(),
        upstream,
        out_dir: root.path().join("results"),
        cache: RepoCache::new(root.path().join("cache")),
        judge: None,
        gantry_sha: "test-sha-1234".to_string(),
        max_tokens: 1_000_000,
    }
}

fn read_record(raw_dir: &Path, name: &str) -> RunRecord {
    let path = raw_dir.join(name);
    let json = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("raw record {} missing: {e}", path.display()));
    serde_json::from_str(&json).expect("raw record parses as RunRecord")
}

// ---------------------------------------------------------------------------
// end-to-end matrix
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn matrix_end_to_end_with_fake_harness() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(JSON_RESPONSE, "application/json"))
        .mount(&mock)
        .await;

    let root = TempDir::with_prefix("runner-e2e-").unwrap();
    let (origin, sha) = fixture_origin(root.path());

    let grading = GradeSpec {
        answer_contains: vec![KNOWN_ANSWER.to_string()],
        ..GradeSpec::default()
    };
    let tasks = vec![
        mem_task(
            "t1-exchange",
            TaskKind::Explore,
            "Fetch one completion through the proxy and report.",
            10_000,
            &origin,
            &sha,
            grading,
        ),
        // Timeout in the middle of the suite: everything after it proves the
        // suite continued.
        mem_task(
            "t2-timeout",
            TaskKind::Explore,
            "BENCH-SLEEP: ignore the proxy and sleep forever.",
            750,
            &origin,
            &sha,
            GradeSpec::default(),
        ),
        mem_task(
            "t3-after",
            TaskKind::Explore,
            "Fetch one completion through the proxy and report.",
            10_000,
            &origin,
            &sha,
            GradeSpec::default(),
        ),
        mem_task(
            "t4-exit3",
            TaskKind::Explore,
            "BENCH-EXIT3: exit non-zero with no output.",
            10_000,
            &origin,
            &sha,
            GradeSpec::default(),
        ),
    ];

    let fake = FakeHarness::create(root.path());
    let cfg = config(&root, mock.uri(), tasks, vec![Box::new(fake)]);
    let records = runner::run_suite(&cfg).await.expect("suite runs");
    assert_eq!(records.len(), 4, "one record per matrix cell");

    let raw_dir = cfg.out_dir.join("raw");

    // --- t1: completed exchange ------------------------------------------
    let t1 = &records[0];
    assert_eq!(t1.run.task_id, "t1-exchange");
    assert_eq!(t1.run.harness, "fake");
    assert_eq!(t1.run.rep, 1);
    assert_eq!(t1.run.outcome, RunOutcome::Completed);
    assert_eq!(t1.run.exit_code, Some(0));
    assert_eq!(t1.run.answer.as_deref(), Some(KNOWN_ANSWER));
    assert_eq!(t1.run.model, "claude-fake-1");
    assert_eq!(t1.run.harness_version, "fake-1.0");
    assert_eq!(t1.run.gantry_sha, "test-sha-1234");
    assert!(
        t1.run.stderr_tail.contains("fake harness stderr marker"),
        "stderr captured: {:?}",
        t1.run.stderr_tail
    );

    // Ledger populated from the proxy tee, not from harness output.
    assert_eq!(
        t1.run.ledger.entries.len(),
        1,
        "exactly one tracked exchange"
    );
    let entry = &t1.run.ledger.entries[0];
    assert_eq!(entry.model, "claude-fake-1");
    assert_eq!(entry.status, 200);
    assert!(!entry.stream);
    assert_eq!(entry.message_count, 1);
    assert_eq!(
        entry.usage,
        Some(Usage {
            input_tokens: 17,
            cache_creation_input_tokens: 3,
            cache_read_input_tokens: 5,
            output_tokens: 42,
        })
    );
    assert_eq!(entry.stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(entry.tool_uses, vec!["read_file".to_string()]);

    // Workspace diff shows the file the harness wrote.
    assert!(
        t1.run.workspace_diff.contains(ARTIFACT_FILE),
        "diff names the artifact: {}",
        t1.run.workspace_diff
    );
    assert!(t1.run.workspace_diff.contains("bench artifact"));

    // Graded: answer_contains passed, no rubric → success.
    let grade = t1.grade.as_ref().expect("completed runs are graded");
    assert!(grade.success);
    assert!(grade.checks.iter().all(|c| c.pass));
    assert_eq!(grade.judge_score, None);

    // --- t2: timeout, killed long before its 30s sleep --------------------
    let t2 = &records[1];
    assert_eq!(t2.run.task_id, "t2-timeout");
    assert_eq!(t2.run.outcome, RunOutcome::Timeout);
    assert_eq!(t2.run.exit_code, None);
    assert!(
        t2.run.wall_ms >= 750,
        "ran at least the timeout: {} ms",
        t2.run.wall_ms
    );
    assert!(
        t2.run.wall_ms < 10_000,
        "killed well before the 30s sleep: {} ms",
        t2.run.wall_ms
    );
    assert_eq!(t2.run.answer, None);
    assert!(t2.run.ledger.entries.is_empty(), "sleep made no API calls");
    assert!(t2.grade.is_none(), "timed-out runs are not graded");

    // --- t3: the suite continued past the timeout -------------------------
    let t3 = &records[2];
    assert_eq!(t3.run.task_id, "t3-after");
    assert_eq!(t3.run.outcome, RunOutcome::Completed);
    assert_eq!(t3.run.answer.as_deref(), Some(KNOWN_ANSWER));

    // --- t4: non-zero exit with no stdout → crashed ------------------------
    let t4 = &records[3];
    assert_eq!(t4.run.task_id, "t4-exit3");
    assert_eq!(t4.run.outcome, RunOutcome::Crashed);
    assert_eq!(t4.run.exit_code, Some(3));
    assert!(t4.grade.is_none());

    // --- incremental persistence -------------------------------------------
    // Every record was written to raw/ as its run finished: t1's file exists
    // (and matches) even though a later run timed out and another crashed.
    for (name, record) in [
        ("t1-exchange-fake-r1.json", t1),
        ("t2-timeout-fake-r1.json", t2),
        ("t3-after-fake-r1.json", t3),
        ("t4-exit3-fake-r1.json", t4),
    ] {
        assert_eq!(&read_record(&raw_dir, name), record, "{name} round-trips");
    }

    // Workspaces are disposable and independent: t3 saw a fresh copy, so the
    // artifact from t1 was not present when it ran (its diff still contains
    // only its own write).
    assert!(t3.run.workspace_diff.contains(ARTIFACT_FILE));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_failure_is_crashed_and_persisted() {
    let mock = MockServer::start().await;
    let root = TempDir::with_prefix("runner-crash-").unwrap();
    let (origin, sha) = fixture_origin(root.path());

    let tasks = vec![mem_task(
        "t-spawnfail",
        TaskKind::Explore,
        "any prompt",
        5_000,
        &origin,
        &sha,
        GradeSpec::default(),
    )];
    let cfg = config(&root, mock.uri(), tasks, vec![Box::new(BrokenHarness)]);
    let records = runner::run_suite(&cfg)
        .await
        .expect("suite survives spawn failure");

    assert_eq!(records.len(), 1);
    let rec = &records[0];
    assert_eq!(rec.run.outcome, RunOutcome::Crashed);
    assert_eq!(rec.run.exit_code, None);
    assert!(
        rec.run.stderr_tail.contains("spawn failed"),
        "spawn error captured: {:?}",
        rec.run.stderr_tail
    );
    assert!(rec.run.ledger.entries.is_empty());
    assert!(rec.grade.is_none());

    let persisted = read_record(&cfg.out_dir.join("raw"), "t-spawnfail-broken-r1.json");
    assert_eq!(&persisted, rec);
}

// ---------------------------------------------------------------------------
// --smoke selection
// ---------------------------------------------------------------------------

fn kind_task(id: &str, kind: TaskKind) -> Task {
    mem_task(
        id,
        kind,
        "p",
        1_000,
        Path::new("/nonexistent"),
        "0000000000000000000000000000000000000000",
        GradeSpec::default(),
    )
}

#[test]
fn smoke_selects_first_explore_task_and_gantry_only() {
    let tasks = vec![
        kind_task("a-locate", TaskKind::Locate),
        kind_task("b-explore", TaskKind::Explore),
        kind_task("c-explore", TaskKind::Explore),
    ];
    let (tasks, harnesses) =
        runner::smoke_selection(tasks, gantry_bench::harness::all()).expect("smoke selection");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].manifest.id, "b-explore", "first explore task wins");
    assert_eq!(harnesses.len(), 1);
    assert_eq!(harnesses[0].name(), "gantry");
}

#[test]
fn smoke_requires_an_explore_task() {
    let tasks = vec![kind_task("only-mutate", TaskKind::Mutate)];
    let err = runner::smoke_selection(tasks, gantry_bench::harness::all())
        .err()
        .expect("selection must fail without an explore task");
    assert!(
        err.to_string().contains("explore"),
        "names the gap: {err:#}"
    );
}

#[test]
fn smoke_requires_the_gantry_harness() {
    let tasks = vec![kind_task("t-explore", TaskKind::Explore)];
    let root = TempDir::with_prefix("runner-smoke-").unwrap();
    let fake: Vec<Box<dyn Harness>> = vec![Box::new(FakeHarness::create(root.path()))];
    let err = runner::smoke_selection(tasks, fake)
        .err()
        .expect("selection must fail without the gantry harness");
    assert!(err.to_string().contains("gantry"), "names the gap: {err:#}");
}
