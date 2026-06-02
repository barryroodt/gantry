//! End-to-end tests for `--mode loop` (SP3). Drive the real binary against a
//! wiremock OpenAI endpoint with a stateful, request-capturing responder, so
//! they exercise the full CLI -> mode::run -> loop_mode -> run_agent_pass path.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Returns `responses[i]` for the i-th request (last entry repeats) and records
/// each request body so tests can assert on the carried-forward context.
struct LoopResponder {
    calls: AtomicUsize,
    bodies: Arc<Mutex<Vec<String>>>,
    responses: Vec<Value>,
}

impl Respond for LoopResponder {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let i = self.calls.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut b) = self.bodies.lock() {
            b.push(String::from_utf8_lossy(&req.body).into_owned());
        }
        let idx = i.min(self.responses.len().saturating_sub(1));
        ResponseTemplate::new(200).set_body_json(self.responses[idx].clone())
    }
}

fn final_stop(text: &str) -> Value {
    json!({
        "id": "c", "object": "chat.completion", "created": 1, "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 42, "completion_tokens": 7, "total_tokens": 49}
    })
}

fn tool_call(name: &str, args: &str, id: &str) -> Value {
    json!({
        "id": "c", "object": "chat.completion", "created": 1, "model": "gpt-4o",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": null,
            "tool_calls": [{"id": id, "type": "function", "function": {"name": name, "arguments": args}}]},
            "finish_reason": "tool_calls"}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
    })
}

fn run_bin(args: &[String], env: &[(&str, &str)]) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_gantry");
    let mut cmd = Command::new(bin);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.args(args);
    let out = cmd.output().expect("run gantry subprocess");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

struct Run {
    code: i32,
    stdout: String,
    bodies: Vec<String>,
    workdir: PathBuf,
    _dir: TempDir,
}

async fn run_loop(responses: Vec<Value>, extra: &[&str], max_tokens: &str) -> Run {
    let mock = MockServer::start().await;
    let bodies = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(LoopResponder {
            calls: AtomicUsize::new(0),
            bodies: bodies.clone(),
            responses,
        })
        .mount(&mock)
        .await;

    let dir = TempDir::new().unwrap();
    let workdir = dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    fs::write(workdir.join("seed.txt"), "seed").unwrap();
    let prompt = dir.path().join("p.txt");
    fs::write(&prompt, "do the task").unwrap();

    let base = format!("{}/v1", mock.uri());
    let mut args = vec![
        "--mode".into(),
        "loop".into(),
        "--model".into(),
        "openai/gpt-4o".into(),
        "--workdir".into(),
        workdir.to_string_lossy().into_owned(),
        "--prompt-file".into(),
        prompt.to_string_lossy().into_owned(),
        "--max-tokens".into(),
        max_tokens.into(),
        "--timeout-ms".into(),
        "60000".into(),
    ];
    for a in extra {
        args.push((*a).to_string());
    }
    let (code, stdout) = run_bin(
        &args,
        &[
            ("OPENAI_API_KEY", "test-openai-key"),
            ("OPENAI_BASE_URL", base.as_str()),
        ],
    );
    let captured = bodies.lock().unwrap().clone();
    Run {
        code,
        stdout,
        bodies: captured,
        workdir,
        _dir: dir,
    }
}

fn events(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect()
}

fn iteration_starts(evs: &[Value]) -> Vec<u64> {
    evs.iter()
        .filter(|e| e["event"] == "iteration_start")
        .filter_map(|e| e["iteration"].as_u64())
        .collect()
}

#[tokio::test]
async fn loop_stops_on_decide_stop() {
    // iter 1: model calls decide_stop, then a final stop reply ends the pass.
    let run = run_loop(
        vec![
            tool_call("decide_stop", "{\"reason\":\"good enough\"}", "d1"),
            final_stop("done"),
        ],
        &[],
        "8192",
    )
    .await;
    let evs = events(&run.stdout);
    assert_eq!(
        iteration_starts(&evs),
        vec![1],
        "should run exactly one iteration; stdout:\n{}",
        run.stdout
    );
    let end = evs
        .iter()
        .find(|e| e["event"] == "iteration_end" && e["iteration"] == 1)
        .expect("iteration_end for iteration 1");
    assert_eq!(end["stopped"], true, "iteration 1 should be marked stopped");
    assert_eq!(run.code, 0, "exit ok; stdout:\n{}", run.stdout);
}

#[tokio::test]
async fn loop_runs_to_cap_and_carries_summary() {
    // Never calls decide_stop; each pass ends immediately (no tool calls). With
    // max-iterations 2 the loop runs exactly twice, and iteration 2's request
    // must carry iteration 1's final text.
    let run = run_loop(
        vec![final_stop("CARRY_MARKER_ALPHA")],
        &["--max-iterations", "2"],
        "8192",
    )
    .await;
    let evs = events(&run.stdout);
    assert_eq!(
        iteration_starts(&evs),
        vec![1, 2],
        "should run exactly max-iterations (2); stdout:\n{}",
        run.stdout
    );
    assert!(
        run.bodies.len() >= 2,
        "expected at least two model calls, got {}",
        run.bodies.len()
    );
    assert!(
        run.bodies[1].contains("CARRY_MARKER_ALPHA"),
        "iteration 2's request must carry iteration 1's final text; body:\n{}",
        run.bodies[1]
    );
    assert_eq!(run.code, 0, "exit ok; stdout:\n{}", run.stdout);
}

#[tokio::test]
async fn loop_budget_trip_exits_budget() {
    // max-tokens 1 with a 42-token response trips the meter on the first pass.
    let run = run_loop(vec![final_stop("x")], &[], "1").await;
    assert_eq!(run.code, 2, "budget exit (2); stdout:\n{}", run.stdout);
}

#[tokio::test]
async fn loop_with_isolate_writes_to_overlay() {
    // iter 1: write_file (lands in the COW overlay) then decide_stop.
    let run = run_loop(
        vec![
            tool_call(
                "write_file",
                "{\"path\":\"created.txt\",\"content\":\"from-loop\"}",
                "w1",
            ),
            tool_call("decide_stop", "{}", "d1"),
            final_stop("done"),
        ],
        &["--isolate", "--tool", "write_file", "--max-iterations", "3"],
        "8192",
    )
    .await;

    assert!(
        !run.workdir.join("created.txt").exists(),
        "isolated write must not touch the real workdir; stdout:\n{}",
        run.stdout
    );
    let changes = events(&run.stdout)
        .into_iter()
        .find(|e| e["event"] == "changes")
        .unwrap_or_else(|| {
            panic!(
                "expected a changes event under --isolate; stdout:\n{}",
                run.stdout
            )
        });
    let files = changes["files"].as_array().expect("changes.files");
    assert!(
        files.iter().any(|f| f["path"]
            .as_str()
            .is_some_and(|p| p.contains("created.txt"))),
        "changes should list created.txt: {changes}"
    );
    assert_eq!(run.code, 0, "exit ok; stdout:\n{}", run.stdout);
}
