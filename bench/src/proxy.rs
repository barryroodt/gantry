//! Recorder proxy for the Anthropic Messages API — ground truth for all
//! efficiency metrics (plan Task 2).
//!
//! An in-process axum reverse proxy bound to `127.0.0.1:0`. `POST /v1/messages`
//! exchanges are forwarded verbatim — the client receives the upstream bytes
//! unmodified and unbuffered (stream pass-through) — while the proxy tees both
//! directions and parses the tee into a [`LedgerEntry`] once the exchange
//! completes. Every other path is forwarded too, but only counted as untracked
//! traffic ([`Ledger::untracked_requests`] / [`Ledger::untracked_bytes`]).
//!
//! One deliberate request-header rewrite: `accept-encoding` is stripped so the
//! upstream replies with identity encoding. This keeps the tee parseable (no
//! gzip in the SSE/JSON capture) and makes `response_bytes` comparable across
//! harnesses regardless of their compression defaults. Auth headers
//! (`x-api-key`, `authorization`) and `anthropic-*` headers pass through
//! untouched.

use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::response::Response;
use bytes::Bytes;
use futures_util::Stream;
use serde_json::Value;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::types::{Ledger, LedgerEntry, Usage};

/// Default upstream when [`UPSTREAM_ENV`] is unset.
pub const DEFAULT_UPSTREAM: &str = "https://api.anthropic.com";

/// Env var overriding the proxy upstream. Test hook for mock-upstream runs
/// only — never set in live benchmark runs.
pub const UPSTREAM_ENV: &str = "GANTRY_BENCH_UPSTREAM";

/// The tracked endpoint. POSTs here become [`LedgerEntry`]s; everything else
/// is untracked traffic.
const MESSAGES_PATH: &str = "/v1/messages";

/// Upstream base URL from [`UPSTREAM_ENV`], falling back to
/// [`DEFAULT_UPSTREAM`].
pub fn upstream_from_env() -> String {
    upstream_or_default(std::env::var(UPSTREAM_ENV).ok())
}

fn upstream_or_default(value: Option<String>) -> String {
    match value {
        Some(v) if !v.trim().is_empty() => normalize_upstream(&v),
        _ => DEFAULT_UPSTREAM.to_string(),
    }
}

fn normalize_upstream(v: &str) -> String {
    v.trim().trim_end_matches('/').to_string()
}

/// A running recorder proxy. Dropping it aborts nothing — call
/// [`RecorderProxy::shutdown`] to stop the server and collect the final
/// [`Ledger`].
pub struct RecorderProxy {
    addr: SocketAddr,
    state: Arc<ProxyState>,
    shutdown_tx: oneshot::Sender<()>,
    server: JoinHandle<()>,
}

impl RecorderProxy {
    /// Bind `127.0.0.1:0` and start serving, forwarding to `upstream`
    /// (scheme + authority; a trailing slash is tolerated).
    pub async fn start(upstream: &str) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            // Relay redirects verbatim instead of following them: retry/redirect
            // behavior is harness behavior and is measured, not masked.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("building upstream HTTP client")?;
        let state = Arc::new(ProxyState {
            upstream: normalize_upstream(upstream),
            client,
            seq: AtomicU32::new(0),
            ledger: Mutex::new(Ledger::default()),
        });
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .context("binding recorder proxy listener")?;
        let addr = listener
            .local_addr()
            .context("resolving recorder proxy address")?;
        let app = axum::Router::new()
            .fallback(handle)
            .with_state(Arc::clone(&state));
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
        });
        Ok(Self {
            addr,
            state,
            shutdown_tx,
            server,
        })
    }

    /// [`RecorderProxy::start`] with the upstream from [`upstream_from_env`].
    pub async fn start_from_env() -> anyhow::Result<Self> {
        Self::start(&upstream_from_env()).await
    }

    /// The bound local address.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `http://127.0.0.1:<port>` — the value harness env vars
    /// (`ANTHROPIC_API_BASE` / `ANTHROPIC_BASE_URL`) point at.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Snapshot of everything recorded so far, entries ordered by `seq`.
    pub fn ledger(&self) -> Ledger {
        self.state.snapshot()
    }

    /// Gracefully stop the server (in-flight exchanges complete) and return
    /// the final ledger.
    pub async fn shutdown(self) -> Ledger {
        let _ = self.shutdown_tx.send(());
        let _ = self.server.await;
        self.state.snapshot()
    }
}

struct ProxyState {
    /// Normalized upstream base, e.g. `https://api.anthropic.com`.
    upstream: String,
    client: reqwest::Client,
    /// Next `seq`, assigned at request receipt so concurrent exchanges get
    /// distinct, arrival-ordered numbers.
    seq: AtomicU32,
    ledger: Mutex<Ledger>,
}

impl ProxyState {
    fn snapshot(&self) -> Ledger {
        let mut ledger = self.ledger.lock().expect("ledger mutex poisoned").clone();
        // Entries are pushed at completion time, which under concurrency can
        // differ from arrival order; seq order is the canonical order.
        ledger.entries.sort_by_key(|e| e.seq);
        ledger
    }

    fn push_entry(&self, entry: LedgerEntry) {
        self.ledger
            .lock()
            .expect("ledger mutex poisoned")
            .entries
            .push(entry);
    }

    fn count_untracked(&self, bytes: u64) {
        let mut ledger = self.ledger.lock().expect("ledger mutex poisoned");
        ledger.untracked_requests += 1;
        ledger.untracked_bytes += bytes;
    }
}

async fn handle(State(state): State<Arc<ProxyState>>, req: Request) -> Response {
    if req.method() == Method::POST && req.uri().path() == MESSAGES_PATH {
        handle_messages(state, req).await
    } else {
        handle_untracked(state, req).await
    }
}

/// Tracked exchange: tee request metadata, stream the response through
/// unmodified, parse the tee into a [`LedgerEntry`] when the stream ends.
async fn handle_messages(state: Arc<ProxyState>, req: Request) -> Response {
    let seq = state.seq.fetch_add(1, Ordering::Relaxed);
    let ts_ms = epoch_ms();
    let started = Instant::now();

    let (parts, body) = req.into_parts();
    let req_bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(err) => {
            return text_response(
                StatusCode::BAD_REQUEST,
                format!("gantry-bench proxy: failed to read request body: {err}"),
            )
        }
    };
    let meta = RequestMeta::parse(&req_bytes);
    let request_bytes = req_bytes.len() as u64;

    let upstream_resp = state
        .client
        .request(parts.method, upstream_url(&state.upstream, &parts.uri))
        .headers(forward_request_headers(&parts.headers))
        .body(req_bytes)
        .send()
        .await;

    let upstream_resp = match upstream_resp {
        Ok(resp) => resp,
        Err(err) => {
            // No upstream status exists; record the exchange attempt with the
            // 502 the client sees so the ledger stays one-entry-per-attempt.
            state.push_entry(LedgerEntry {
                seq,
                ts_ms,
                latency_ms: started.elapsed().as_millis() as u64,
                model: meta.model,
                stream: meta.stream,
                status: StatusCode::BAD_GATEWAY.as_u16(),
                usage: None,
                stop_reason: None,
                tool_uses: Vec::new(),
                request_bytes,
                response_bytes: 0,
                message_count: meta.message_count,
                tools_bytes: meta.tools_bytes,
            });
            return text_response(
                StatusCode::BAD_GATEWAY,
                format!("gantry-bench proxy: upstream request failed: {err}"),
            );
        }
    };

    let status = upstream_resp.status();
    let headers = forward_response_headers(upstream_resp.headers());
    let sse = upstream_resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("text/event-stream"));

    let tee = TeeStream {
        inner: Box::pin(upstream_resp.bytes_stream()),
        buf: Vec::new(),
        pending: Some(PendingEntry {
            state,
            seq,
            ts_ms,
            started,
            status: status.as_u16(),
            request_bytes,
            meta,
            sse,
        }),
    };

    build_response(status, headers, Body::from_stream(tee))
}

/// Untracked traffic: forward, count request+response body bytes, never parse.
async fn handle_untracked(state: Arc<ProxyState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let req_bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(err) => {
            return text_response(
                StatusCode::BAD_REQUEST,
                format!("gantry-bench proxy: failed to read request body: {err}"),
            )
        }
    };
    let request_bytes = req_bytes.len() as u64;

    let mut builder = state
        .client
        .request(parts.method, upstream_url(&state.upstream, &parts.uri))
        .headers(forward_request_headers(&parts.headers));
    if !req_bytes.is_empty() {
        builder = builder.body(req_bytes);
    }

    match builder.send().await {
        Ok(resp) => {
            let status = resp.status();
            let headers = forward_response_headers(resp.headers());
            let resp_bytes = resp.bytes().await.unwrap_or_default();
            state.count_untracked(request_bytes + resp_bytes.len() as u64);
            build_response(status, headers, Body::from(resp_bytes))
        }
        Err(err) => {
            state.count_untracked(request_bytes);
            text_response(
                StatusCode::BAD_GATEWAY,
                format!("gantry-bench proxy: upstream request failed: {err}"),
            )
        }
    }
}

fn upstream_url(upstream: &str, uri: &Uri) -> String {
    match uri.path_and_query() {
        Some(pq) => format!("{upstream}{pq}"),
        None => format!("{upstream}{}", uri.path()),
    }
}

fn build_response(status: StatusCode, headers: HeaderMap, body: Body) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

fn text_response(status: StatusCode, msg: String) -> Response {
    let mut response = Response::new(Body::from(msg));
    *response.status_mut() = status;
    response
}

fn is_hop_header(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// All request headers pass through except hop-by-hop ones, `host` (the
/// upstream's, not ours), framing headers the forwarding client recomputes,
/// and `accept-encoding` (see module docs).
fn forward_request_headers(src: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::with_capacity(src.len());
    for (name, value) in src {
        let n = name.as_str();
        if is_hop_header(n)
            || n == "host"
            || n == "content-length"
            || n == "accept-encoding"
            || n == "expect"
        {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    out
}

/// All response headers pass through except hop-by-hop and framing headers
/// (the body is re-framed by our server; its bytes are untouched).
fn forward_response_headers(src: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::with_capacity(src.len());
    for (name, value) in src {
        let n = name.as_str();
        if is_hop_header(n) || n == "content-length" {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    out
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Request-body fields teed before forwarding.
#[derive(Default)]
struct RequestMeta {
    model: String,
    stream: bool,
    message_count: u32,
    tools_bytes: u64,
}

impl RequestMeta {
    fn parse(body: &[u8]) -> Self {
        let Ok(v) = serde_json::from_slice::<Value>(body) else {
            return Self::default();
        };
        Self {
            model: v
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            stream: v.get("stream").and_then(Value::as_bool).unwrap_or(false),
            message_count: v
                .get("messages")
                .and_then(Value::as_array)
                .map_or(0, |m| m.len() as u32),
            tools_bytes: v
                .get("tools")
                .map_or(0, |t| serde_json::to_vec(t).map_or(0, |b| b.len() as u64)),
        }
    }
}

/// Everything needed to mint a [`LedgerEntry`] once the response tee is
/// complete.
struct PendingEntry {
    state: Arc<ProxyState>,
    seq: u32,
    ts_ms: u64,
    started: Instant,
    status: u16,
    request_bytes: u64,
    meta: RequestMeta,
    sse: bool,
}

impl PendingEntry {
    fn finalize(self, body: Vec<u8>) {
        let parsed = if self.sse {
            parse_sse_response(&body)
        } else {
            parse_json_response(&body)
        };
        let entry = LedgerEntry {
            seq: self.seq,
            ts_ms: self.ts_ms,
            latency_ms: self.started.elapsed().as_millis() as u64,
            model: self.meta.model,
            stream: self.meta.stream,
            status: self.status,
            usage: parsed.usage,
            stop_reason: parsed.stop_reason,
            tool_uses: parsed.tool_uses,
            request_bytes: self.request_bytes,
            response_bytes: body.len() as u64,
            message_count: self.meta.message_count,
            tools_bytes: self.meta.tools_bytes,
        };
        self.state.push_entry(entry);
    }
}

/// Relays upstream chunks to the client untouched while accumulating a copy;
/// finalizes the ledger entry when the stream ends, errors, or is dropped
/// (client disconnect) — exactly once.
struct TeeStream {
    inner: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
    buf: Vec<u8>,
    pending: Option<PendingEntry>,
}

impl TeeStream {
    fn finalize(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.finalize(std::mem::take(&mut self.buf));
        }
    }
}

impl Stream for TeeStream {
    type Item = reqwest::Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                this.buf.extend_from_slice(&chunk);
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(err))) => {
                this.finalize();
                Poll::Ready(Some(Err(err)))
            }
            Poll::Ready(None) => {
                this.finalize();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for TeeStream {
    fn drop(&mut self) {
        self.finalize();
    }
}

/// What the response tee parses out of a completed exchange.
#[derive(Default)]
struct ParsedResponse {
    usage: Option<Usage>,
    stop_reason: Option<String>,
    tool_uses: Vec<String>,
}

/// Non-streaming Messages response: top-level `usage`, `stop_reason`, and
/// `tool_use` content-block names in order. Error bodies (no such fields)
/// fall through to defaults — usage stays `None`.
fn parse_json_response(body: &[u8]) -> ParsedResponse {
    let Ok(v) = serde_json::from_slice::<Value>(body) else {
        return ParsedResponse::default();
    };
    let usage = v
        .get("usage")
        .and_then(|u| serde_json::from_value::<Usage>(u.clone()).ok());
    let stop_reason = v
        .get("stop_reason")
        .and_then(Value::as_str)
        .map(str::to_string);
    let tool_uses = v
        .get("content")
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
                .filter_map(|b| b.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        });
    ParsedResponse {
        usage,
        stop_reason,
        tool_uses,
    }
}

/// SSE Messages response: `message_start` carries the initial usage,
/// `content_block_start` carries `tool_use` names, `message_delta` carries the
/// final `stop_reason` and cumulative usage updates. Events are dispatched on
/// the JSON `type` field of each `data:` line, so `event:` framing lines and
/// unknown events (`ping`, `content_block_delta`, …) are skipped naturally.
fn parse_sse_response(body: &[u8]) -> ParsedResponse {
    let text = String::from_utf8_lossy(body);
    let mut out = ParsedResponse::default();
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(data.trim()) else {
            continue;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(u) = v.pointer("/message/usage") {
                    merge_usage(&mut out.usage, u);
                }
            }
            Some("content_block_start") => {
                if v.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use") {
                    if let Some(name) = v.pointer("/content_block/name").and_then(Value::as_str) {
                        out.tool_uses.push(name.to_string());
                    }
                }
            }
            Some("message_delta") => {
                if let Some(sr) = v.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    out.stop_reason = Some(sr.to_string());
                }
                if let Some(u) = v.get("usage") {
                    merge_usage(&mut out.usage, u);
                }
            }
            _ => {}
        }
    }
    out
}

/// Field-presence-aware merge: only fields present in `v` overwrite, so a
/// `message_delta` usage carrying only `output_tokens` cannot zero the input
/// counts recorded at `message_start`.
fn merge_usage(usage: &mut Option<Usage>, v: &Value) {
    if !v.is_object() {
        return;
    }
    let u = usage.get_or_insert_with(Usage::default);
    if let Some(n) = v.get("input_tokens").and_then(Value::as_u64) {
        u.input_tokens = n;
    }
    if let Some(n) = v.get("cache_creation_input_tokens").and_then(Value::as_u64) {
        u.cache_creation_input_tokens = n;
    }
    if let Some(n) = v.get("cache_read_input_tokens").and_then(Value::as_u64) {
        u.cache_read_input_tokens = n;
    }
    if let Some(n) = v.get("output_tokens").and_then(Value::as_u64) {
        u.output_tokens = n;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_default_and_override() {
        assert_eq!(upstream_or_default(None), DEFAULT_UPSTREAM);
        assert_eq!(upstream_or_default(Some(String::new())), DEFAULT_UPSTREAM);
        assert_eq!(
            upstream_or_default(Some("http://127.0.0.1:9".into())),
            "http://127.0.0.1:9"
        );
        assert_eq!(upstream_or_default(Some("http://h/".into())), "http://h");
    }

    #[test]
    fn merge_usage_only_overwrites_present_fields() {
        let mut usage = None;
        merge_usage(
            &mut usage,
            &serde_json::json!({"input_tokens": 10, "output_tokens": 1}),
        );
        merge_usage(&mut usage, &serde_json::json!({"output_tokens": 42}));
        assert_eq!(
            usage,
            Some(Usage {
                input_tokens: 10,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                output_tokens: 42,
            })
        );
    }
}
