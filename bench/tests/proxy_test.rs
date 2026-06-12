//! Integration tests for the recorder proxy (plan Task 2) against an in-test
//! wiremock upstream. Keyless and networkless beyond loopback (invariant 4).

use std::time::Duration;

use futures_util::future::join_all;
use gantry_bench::proxy::RecorderProxy;
use gantry_bench::types::Usage;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Canned non-streaming Messages response with fixture-known usage numbers
/// and two tool_use blocks (order matters).
const JSON_RESPONSE: &str = r#"{"id":"msg_01","type":"message","role":"assistant","model":"claude-test-1","content":[{"type":"text","text":"Using tools."},{"type":"tool_use","id":"toolu_01","name":"get_weather","input":{"city":"Oslo"}},{"type":"tool_use","id":"toolu_02","name":"read_file","input":{"path":"a.rs"}}],"stop_reason":"tool_use","stop_sequence":null,"usage":{"input_tokens":17,"cache_creation_input_tokens":3,"cache_read_input_tokens":5,"output_tokens":42}}"#;

/// Canned SSE replay: message_start usage, a text block, a tool_use block,
/// a ping, and a message_delta with stop_reason + cumulative output_tokens.
const SSE_BODY: &str = concat!(
    "event: message_start\n",
    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_01\",\"model\":\"claude-test-1\",\"usage\":{\"input_tokens\":2051,\"cache_creation_input_tokens\":128,\"cache_read_input_tokens\":1024,\"output_tokens\":1}}}\n",
    "\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
    "\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"checking\"}}\n",
    "\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_01\",\"name\":\"search_code\",\"input\":{}}}\n",
    "\n",
    "event: ping\n",
    "data: {\"type\":\"ping\"}\n",
    "\n",
    "event: message_delta\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":89}}\n",
    "\n",
    "event: message_stop\n",
    "data: {\"type\":\"message_stop\"}\n",
    "\n",
);

const RATE_LIMIT_BODY: &str =
    r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#;

fn simple_request_body() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "model": "claude-test-1",
        "max_tokens": 64,
        "messages": [{"role": "user", "content": "hi"}],
    }))
    .expect("serialize request body")
}

#[tokio::test]
async fn non_streaming_exchange_recorded_exactly() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        // Forwarding must preserve auth + anthropic-* headers: the mock only
        // matches when they arrive, so a stripped header fails the test.
        .and(header("x-api-key", "k-test"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(JSON_RESPONSE, "application/json"))
        .mount(&upstream)
        .await;

    let proxy = RecorderProxy::start(&upstream.uri()).await.unwrap();

    let req_json = serde_json::json!({
        "model": "claude-test-1",
        "max_tokens": 64,
        "messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "yo"},
            {"role": "user", "content": "go"},
        ],
        "tools": [{"name": "get_weather", "input_schema": {"type": "object"}}],
    });
    let req_body = serde_json::to_vec(&req_json).unwrap();
    let expected_tools_bytes = serde_json::to_vec(req_json.get("tools").unwrap()).unwrap().len() as u64;

    let resp = reqwest::Client::new()
        .post(format!("{}/v1/messages", proxy.base_url()))
        .header("x-api-key", "k-test")
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .body(req_body.clone())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/json",
        "response headers pass through"
    );
    let resp_bytes = resp.bytes().await.unwrap();
    assert_eq!(
        &resp_bytes[..],
        JSON_RESPONSE.as_bytes(),
        "client receives upstream bytes unmodified"
    );

    let ledger = proxy.shutdown().await;
    assert_eq!(ledger.entries.len(), 1);
    assert_eq!(ledger.untracked_requests, 0);
    assert_eq!(ledger.untracked_bytes, 0);

    let e = &ledger.entries[0];
    assert_eq!(e.seq, 0);
    assert_eq!(e.model, "claude-test-1");
    assert!(!e.stream);
    assert_eq!(e.status, 200);
    assert_eq!(
        e.usage,
        Some(Usage {
            input_tokens: 17,
            cache_creation_input_tokens: 3,
            cache_read_input_tokens: 5,
            output_tokens: 42,
        })
    );
    assert_eq!(e.stop_reason.as_deref(), Some("tool_use"));
    assert_eq!(e.tool_uses, vec!["get_weather", "read_file"]);
    assert_eq!(e.request_bytes, req_body.len() as u64);
    assert_eq!(e.response_bytes, JSON_RESPONSE.len() as u64);
    assert_eq!(e.message_count, 3);
    assert_eq!(e.tools_bytes, expected_tools_bytes);
    assert!(e.ts_ms > 0);
}

#[tokio::test]
async fn sse_exchange_streams_through_and_parses_tee() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(SSE_BODY, "text/event-stream"))
        .mount(&upstream)
        .await;

    let proxy = RecorderProxy::start(&upstream.uri()).await.unwrap();

    let req_body = serde_json::to_vec(&serde_json::json!({
        "model": "claude-test-1",
        "max_tokens": 1024,
        "stream": true,
        "messages": [{"role": "user", "content": "find the bug"}],
    }))
    .unwrap();

    let resp = reqwest::Client::new()
        .post(format!("{}/v1/messages", proxy.base_url()))
        .header("content-type", "application/json")
        .body(req_body.clone())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    let resp_bytes = resp.bytes().await.unwrap();
    assert_eq!(
        &resp_bytes[..],
        SSE_BODY.as_bytes(),
        "SSE bytes relayed unmodified"
    );

    let ledger = proxy.shutdown().await;
    assert_eq!(ledger.entries.len(), 1);

    let e = &ledger.entries[0];
    assert!(e.stream, "stream flag teed from the request body");
    assert_eq!(e.status, 200);
    assert_eq!(
        e.usage,
        Some(Usage {
            input_tokens: 2051,
            cache_creation_input_tokens: 128,
            cache_read_input_tokens: 1024,
            output_tokens: 89, // message_delta overrides message_start's 1
        })
    );
    assert_eq!(e.stop_reason.as_deref(), Some("tool_use"));
    assert_eq!(e.tool_uses, vec!["search_code"]);
    assert_eq!(e.request_bytes, req_body.len() as u64);
    assert_eq!(e.response_bytes, SSE_BODY.len() as u64);
    assert_eq!(e.message_count, 1);
    assert_eq!(e.tools_bytes, 0, "no tools field in request");
}

#[tokio::test]
async fn rate_limit_429_passes_through_and_is_recorded() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "7")
                .set_body_raw(RATE_LIMIT_BODY, "application/json"),
        )
        .mount(&upstream)
        .await;

    let proxy = RecorderProxy::start(&upstream.uri()).await.unwrap();

    let resp = reqwest::Client::new()
        .post(format!("{}/v1/messages", proxy.base_url()))
        .body(simple_request_body())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 429, "status passes through untouched");
    assert_eq!(
        resp.headers().get("retry-after").unwrap(),
        "7",
        "retry-after relayed so harness retry behavior is measured, not masked"
    );
    let resp_bytes = resp.bytes().await.unwrap();
    assert_eq!(&resp_bytes[..], RATE_LIMIT_BODY.as_bytes());

    let ledger = proxy.shutdown().await;
    assert_eq!(ledger.entries.len(), 1, "every attempt gets a ledger entry");

    let e = &ledger.entries[0];
    assert_eq!(e.status, 429);
    assert_eq!(e.usage, None, "error body carries no usage");
    assert_eq!(e.stop_reason, None);
    assert!(e.tool_uses.is_empty());
    assert_eq!(e.response_bytes, RATE_LIMIT_BODY.len() as u64);
}

#[tokio::test]
async fn non_messages_paths_counted_as_untracked() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(r#"{"data":[]}"#, "application/json"))
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/event"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("ok", "text/plain"))
        .mount(&upstream)
        .await;

    let proxy = RecorderProxy::start(&upstream.uri()).await.unwrap();
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/v1/models", proxy.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), r#"{"data":[]}"#);

    let resp = client
        .post(format!("{}/api/event", proxy.base_url()))
        .body("abc")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");

    let base_url = proxy.base_url();
    let ledger = proxy.shutdown().await;
    assert!(ledger.entries.is_empty(), "untracked traffic is never parsed");
    assert_eq!(ledger.untracked_requests, 2);
    // GET: 0 req + 11 resp; POST: 3 req + 2 resp.
    assert_eq!(ledger.untracked_bytes, 11 + 3 + 2);

    // Graceful shutdown released the listener: new connections are refused.
    let err = client
        .get(format!("{base_url}/v1/models"))
        .send()
        .await;
    assert!(err.is_err(), "proxy no longer accepts connections after shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_requests_get_distinct_seq() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(JSON_RESPONSE, "application/json")
                .set_delay(Duration::from_millis(25)),
        )
        .mount(&upstream)
        .await;

    let proxy = RecorderProxy::start(&upstream.uri()).await.unwrap();
    let url = format!("{}/v1/messages", proxy.base_url());
    let client = reqwest::Client::new();

    let posts = (0..8).map(|_| {
        let client = client.clone();
        let url = url.clone();
        async move {
            let resp = client
                .post(&url)
                .body(simple_request_body())
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 200);
            resp.bytes().await.unwrap()
        }
    });
    join_all(posts).await;

    let ledger = proxy.shutdown().await;
    assert_eq!(ledger.entries.len(), 8);
    let seqs: Vec<u32> = ledger.entries.iter().map(|e| e.seq).collect();
    assert_eq!(
        seqs,
        (0..8).collect::<Vec<u32>>(),
        "distinct seq per exchange, snapshot ordered by seq"
    );
    for e in &ledger.entries {
        assert_eq!(e.status, 200);
        assert_eq!(e.usage.as_ref().map(|u| u.output_tokens), Some(42));
    }
}
