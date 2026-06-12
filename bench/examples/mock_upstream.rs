//! Canned Anthropic upstream for keyless `--smoke` runs.
//!
//! Serves `POST /v1/messages` with one fixed non-streaming completion so the
//! whole bench pipeline (real harness binary → recording proxy → upstream)
//! can be exercised without an API key:
//!
//! ```sh
//! cargo run -p gantry-bench --example mock_upstream &
//! GANTRY_BENCH_UPSTREAM=http://127.0.0.1:18099 cargo run -p gantry-bench -- --smoke
//! ```
//!
//! `MOCK_UPSTREAM_ADDR` overrides the bind address (default `127.0.0.1:18099`).

use axum::{routing::post, Json, Router};
use serde_json::{json, Value};

async fn messages() -> Json<Value> {
    Json(json!({
        "id": "msg_bench_smoke",
        "type": "message",
        "role": "assistant",
        "model": "claude-bench-mock",
        "content": [{
            "type": "text",
            "text": "Mock-upstream smoke response: canned answer used to validate \
                     gantry-bench plumbing end-to-end. It intentionally does not \
                     describe the repository.",
        }],
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 1200,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0,
            "output_tokens": 42,
        },
    }))
}

#[tokio::main]
async fn main() {
    let addr = std::env::var("MOCK_UPSTREAM_ADDR").unwrap_or_else(|_| "127.0.0.1:18099".into());
    let app = Router::new().route("/v1/messages", post(messages));
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("mock_upstream: cannot bind {addr}: {e}"));
    eprintln!("mock_upstream: listening on http://{addr}");
    axum::serve(listener, app).await.expect("serve");
}
