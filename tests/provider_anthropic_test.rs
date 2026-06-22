use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use gantry::provider::{
    AnthropicProvider, ChatMessage, ProviderAdapter, ToolCallRequest, ToolSchema,
};
use serde_json::{json, Value};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// Process-wide lock serializing env-mutating tests in this binary. Environment
/// variables are global, so without this the default multi-threaded test runner
/// races `ANTHROPIC_API_BASE`/`_KEY` between tests — one test's provider could
/// read another's mock-server URL, intermittently failing the request capture.
fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct EnvVarGuard {
    vars: Vec<(String, Option<String>)>,
    // Held for the guard's lifetime so env mutation + the provider call that
    // reads the vars stay serialized against other tests.
    _lock: MutexGuard<'static, ()>,
}

impl EnvVarGuard {
    fn set(key: &str, value: Option<&str>) -> Self {
        Self::set_many(&[(key, value)])
    }

    fn set_many(pairs: &[(&str, Option<&str>)]) -> Self {
        let lock = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let mut vars = Vec::with_capacity(pairs.len());
        for (key, value) in pairs {
            let previous = std::env::var(key).ok();
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            vars.push(((*key).to_string(), previous));
        }
        Self { vars, _lock: lock }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        for (key, previous) in self.vars.drain(..) {
            match previous {
                Some(value) => std::env::set_var(&key, value),
                None => std::env::remove_var(&key),
            }
        }
    }
}

fn anthropic_success_response() -> Value {
    json!({
        "id": "msg_01",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-4",
        "stop_reason": "tool_use",
        "content": [
            {
                "type": "text",
                "text": "I'll look that up."
            },
            {
                "type": "tool_use",
                "id": "toolu_01",
                "name": "search",
                "input": { "query": "rig harness" }
            }
        ],
        "usage": {
            "input_tokens": 120,
            "output_tokens": 45,
            "cache_creation_input_tokens": 30,
            "cache_read_input_tokens": 15
        }
    })
}

#[tokio::test]
async fn complete_hits_messages_endpoint_and_maps_response() {
    let mock_server = MockServer::start().await;
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_mock = Arc::clone(&captured);
    let response_body = anthropic_success_response();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-anthropic-key"))
        .and(header("anthropic-beta", "prompt-caching-2024-07-31"))
        .respond_with(move |request: &Request| {
            let body = serde_json::from_slice::<Value>(&request.body)
                .expect("request body should be valid JSON");
            *captured_for_mock.lock().expect("capture lock") = Some(body);
            ResponseTemplate::new(200).set_body_json(response_body.clone())
        })
        .mount(&mock_server)
        .await;

    let _env = EnvVarGuard::set_many(&[
        ("ANTHROPIC_API_KEY", Some("test-anthropic-key")),
        ("ANTHROPIC_API_BASE", Some(mock_server.uri().as_str())),
    ]);

    let provider = AnthropicProvider::new("claude-sonnet-4".into()).expect("provider");
    let tools = [ToolSchema {
        name: "search".into(),
        description: "Search the web".into(),
        json_schema: json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" }
            },
            "required": ["query"]
        }),
    }];

    let response = provider
        .complete(
            "You are a helpful assistant.",
            &[
                ChatMessage::User("Find rig harness docs".into()),
                ChatMessage::Assistant {
                    text: String::new(),
                    tool_calls: vec![ToolCallRequest {
                        id: "toolu_prev".into(),
                        name: "search".into(),
                        args_json: r#"{"query":"rig"}"#.into(),
                    }],
                },
                ChatMessage::ToolResults(vec![gantry::provider::ToolResult {
                    id: "toolu_prev".into(),
                    content: "prior result".into(),
                    is_error: false,
                }]),
            ],
            &tools,
        )
        .await
        .expect("complete");

    assert_eq!(response.text, "I'll look that up.");
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].id, "toolu_01");
    assert_eq!(response.tool_calls[0].name, "search");
    assert_eq!(
        response.tool_calls[0].args_json,
        r#"{"query":"rig harness"}"#
    );
    assert_eq!(response.input_tokens, 120);
    assert_eq!(response.output_tokens, 45);
    assert_eq!(response.cache_read, 15);
    assert_eq!(response.cache_write, 30);

    let request_body = captured
        .lock()
        .expect("capture lock")
        .clone()
        .expect("request body captured");

    assert_eq!(request_body["model"], "claude-sonnet-4");
    // The fix always sends a concrete `max_tokens` (never `None`, which makes
    // rig hard-fail before the request leaves the process). A hosted id rig
    // knows keeps its per-model cap (claude-sonnet-4 → 64000), not the
    // local-server fallback.
    assert_eq!(request_body["max_tokens"], 64000);
    assert_eq!(
        request_body["system"][0]["text"],
        "You are a helpful assistant."
    );
    assert_eq!(request_body["tools"][0]["name"], "search");
    assert!(request_body["messages"].is_array());
}

#[tokio::test]
async fn complete_sends_fallback_max_tokens_for_unknown_model() {
    // An Anthropic-compatible local server (e.g. oMLX) serves a custom model id
    // rig's table doesn't know. Without a fallback rig hard-fails before
    // sending ("`max_tokens` must be set for Anthropic"); the fix must put the
    // fallback on the wire so the request actually goes out. Keep this value in
    // sync with `LOCAL_ANTHROPIC_FALLBACK_MAX_TOKENS` in
    // `src/provider/anthropic.rs`.
    let mock_server = MockServer::start().await;
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_mock = Arc::clone(&captured);
    let response_body = anthropic_success_response();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-anthropic-key"))
        .respond_with(move |request: &Request| {
            let body = serde_json::from_slice::<Value>(&request.body)
                .expect("request body should be valid JSON");
            *captured_for_mock.lock().expect("capture lock") = Some(body);
            ResponseTemplate::new(200).set_body_json(response_body.clone())
        })
        .mount(&mock_server)
        .await;

    let _env = EnvVarGuard::set_many(&[
        ("ANTHROPIC_API_KEY", Some("test-anthropic-key")),
        ("ANTHROPIC_API_BASE", Some(mock_server.uri().as_str())),
    ]);

    let provider = AnthropicProvider::new("local-custom-model".into()).expect("provider");
    provider
        .complete(
            "You are a helpful assistant.",
            &[ChatMessage::User("hi".into())],
            &[],
        )
        .await
        .expect("complete");

    let request_body = captured
        .lock()
        .expect("capture lock")
        .clone()
        .expect("request body captured");

    // Unknown id → the documented fallback, proving the fallback path reaches
    // the wire (not just the `resolve_max_tokens` unit).
    assert_eq!(request_body["max_tokens"], 8192);
}

#[tokio::test]
async fn complete_sets_prompt_cache_breakpoints_on_system_and_last_user_message() {
    let mock_server = MockServer::start().await;
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_mock = Arc::clone(&captured);

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |request: &Request| {
            let body = serde_json::from_slice::<Value>(&request.body)
                .expect("request body should be valid JSON");
            *captured_for_mock.lock().expect("capture lock") = Some(body);
            ResponseTemplate::new(200).set_body_json(anthropic_success_response())
        })
        .mount(&mock_server)
        .await;

    let _env = EnvVarGuard::set_many(&[
        ("ANTHROPIC_API_KEY", Some("test-anthropic-key")),
        ("ANTHROPIC_API_BASE", Some(mock_server.uri().as_str())),
    ]);

    let provider = AnthropicProvider::new("claude-sonnet-4".into()).expect("provider");
    provider
        .complete(
            "Cached system prompt",
            &[
                ChatMessage::User("first turn".into()),
                ChatMessage::Assistant {
                    text: "ack".into(),
                    tool_calls: vec![],
                },
                ChatMessage::User("latest user turn".into()),
            ],
            &[],
        )
        .await
        .expect("complete");

    let request_body = captured
        .lock()
        .expect("capture lock")
        .clone()
        .expect("request body captured");

    assert_eq!(
        request_body["system"][0]["cache_control"]["type"], "ephemeral",
        "system prompt should be marked for prompt caching"
    );

    let messages = request_body["messages"].as_array().expect("messages array");
    let last_message = messages.last().expect("last message");
    assert_eq!(last_message["role"], "user");
    let last_content = last_message["content"]
        .as_array()
        .and_then(|items| items.last())
        .or_else(|| {
            last_message
                .get("content")
                .filter(|value| value.is_object())
        })
        .expect("last user content block");
    assert_eq!(
        last_content["cache_control"]["type"], "ephemeral",
        "last user message should be marked for prompt caching"
    );
    assert_eq!(last_content["text"], "latest user turn");
}

#[test]
fn new_errors_when_api_key_missing() {
    let _env = EnvVarGuard::set("ANTHROPIC_API_KEY", None);
    let result = AnthropicProvider::new("claude-sonnet-4".into());
    assert!(result.is_err(), "expected missing API key to fail");
    assert_eq!(
        result.err().expect("error").to_string(),
        "ANTHROPIC_API_KEY not set"
    );
}
