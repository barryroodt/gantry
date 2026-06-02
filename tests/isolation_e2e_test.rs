//! End-to-end isolation test (SP2): under `--isolate`, a `write_file` tool call
//! mutates the copy-on-write overlay — NOT the real workdir — and the terminal
//! `changes` event lists the created file. Drives the real binary against a
//! wiremock OpenAI endpoint, so it exercises the full
//! CLI -> mode::run -> isolation -> dispatch -> write_file path.

use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{json, Value};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Returns the `write_file` tool call on the first request, then a final
/// `stop` reply on every later request (so the agent loop terminates).
struct SeqResponder {
    calls: AtomicUsize,
}

impl Respond for SeqResponder {
    fn respond(&self, _req: &Request) -> ResponseTemplate {
        let body = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            write_file_tool_call()
        } else {
            final_response()
        };
        ResponseTemplate::new(200).set_body_json(body)
    }
}

fn write_file_tool_call() -> Value {
    json!({
        "id": "chatcmpl-tool",
        "object": "chat.completion",
        "created": 1_700_000_000,
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_write_1",
                    "type": "function",
                    "function": {
                        "name": "write_file",
                        "arguments": "{\"path\":\"created.txt\",\"content\":\"from-agent\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
    })
}

fn final_response() -> Value {
    json!({
        "id": "chatcmpl-final",
        "object": "chat.completion",
        "created": 1_700_000_001,
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "done"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 12, "completion_tokens": 3, "total_tokens": 15}
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

#[tokio::test]
async fn isolate_routes_write_file_to_overlay_and_emits_changes() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(SeqResponder {
            calls: AtomicUsize::new(0),
        })
        .mount(&mock)
        .await;

    let dir = TempDir::new().unwrap();
    let workdir = dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    fs::write(workdir.join("sample.txt"), "sample content").unwrap();
    let prompt = dir.path().join("prompt.txt");
    fs::write(&prompt, "please create created.txt").unwrap();

    let base = format!("{}/v1", mock.uri());
    let args = vec![
        "--mode".into(),
        "single".into(),
        "--model".into(),
        "openai/gpt-4o".into(),
        "--workdir".into(),
        workdir.to_string_lossy().into_owned(),
        "--prompt-file".into(),
        prompt.to_string_lossy().into_owned(),
        "--max-tokens".into(),
        "8192".into(),
        "--timeout-ms".into(),
        "60000".into(),
        "--isolate".into(),
        "--tool".into(),
        "write_file".into(),
    ];
    let (code, stdout) = run_bin(
        &args,
        &[
            ("OPENAI_API_KEY", "test-openai-key"),
            ("OPENAI_BASE_URL", base.as_str()),
        ],
    );

    // The write landed in the overlay, NOT the real workdir.
    assert!(
        !workdir.join("created.txt").exists(),
        "isolated write must not touch the real workdir; stdout:\n{stdout}"
    );
    assert_eq!(
        fs::read_to_string(workdir.join("sample.txt")).unwrap(),
        "sample content",
        "original workdir contents preserved"
    );

    // A terminal `changes` event lists the created file as added.
    let changes = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["event"] == "changes")
        .unwrap_or_else(|| panic!("expected a `changes` event under --isolate; stdout:\n{stdout}"));
    let files = changes["files"].as_array().expect("changes.files array");
    assert!(
        files.iter().any(|f| {
            f["path"]
                .as_str()
                .is_some_and(|p| p.contains("created.txt"))
                && f["kind"] == "added"
        }),
        "changes should list created.txt as added: {changes}"
    );

    assert_eq!(code, 0, "run should exit ok; stdout:\n{stdout}");
}
