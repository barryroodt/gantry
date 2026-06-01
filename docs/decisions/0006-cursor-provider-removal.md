# ADR-0006: Remove the Cursor provider; defer until a usable single-turn API/SDK exists

**Status:** Accepted
**Date:** 2026-05-31
**Supersedes:** [ADR-0001](./0001-cursor-provider.md)

## Context

ADR-0001 specified a `CursorProvider` (`src/provider/cursor.rs`) that POSTed a
JSON `TurnRequest` to a **local Node "bridge"** (`CURSOR_BRIDGE_URL`, default
`http://127.0.0.1:8765/v1/turns`) authed by `CURSOR_API_KEY`, with the bridge
embedding `@cursor/sdk`. That bridge was never built (ADR-0001 deferred it to
"Phase 8 packaging" with the Connect protocol marked TBD), so the provider has
never worked end-to-end. A live investigation (real Cursor **user** API key +
current Cursor docs, 2026-05-31) established why, and what the real path is.

## Findings

1. **No Rust Cursor SDK.** Cursor ships official SDKs for TypeScript
   (`@cursor/sdk`) and Python only; `rig-core` (which backs our
   anthropic/openai/gemini adapters) has no Cursor provider, because Cursor has
   no standard chat-completions REST API to wrap.

2. **Every Cursor programmatic surface is agent-scoped.** The SDK (`Agent.create`
   / `Agent.prompt` → `Run`), the headless `cursor-agent` CLI, and the Cloud
   Agents REST API (`POST /v1/agents`) all run *Cursor's own* agent loop with
   *Cursor-managed* tools and return a final result. There is **no single
   model-turn / chat-completion primitive**. Composer is not exposed as a
   BYOK chat-completions endpoint — Cursor blocks custom/agent models from
   external-key chat routing by design. This is the inverse of gantry's
   `ProviderAdapter` contract ("return one turn of text + tool-use intents;
   *gantry* owns the loop").

3. **Credential shape.** A **user** API key (`crsr_…`, from Dashboard → API Keys)
   works with `@cursor/sdk`, the headless CLI, and the Cloud Agents API. **Team
   Admin** keys (`key_…`) are *not* supported by the SDK. The removed adapter
   sent `CURSOR_API_KEY` as a bearer to a JSON bridge — wrong transport shape.

4. **How oh-my-pi/pi does it (reference).** pi does not use `@cursor/sdk`; it
   reimplements the Cursor CLI's native backend protocol in TypeScript:
   **Connect/gRPC + protobuf over HTTP/2** to `https://api2.cursor.sh`,
   method `/agent.v1.AgentService/Run`, `Authorization: Bearer <token>`,
   impersonating the CLI (`x-cursor-client-type: cli`,
   `x-cursor-client-version: …`, `x-ghost-mode: true`). Auth is a
   `CURSOR_ACCESS_TOKEN` (an OAuth access token from the PKCE login flow
   `cursor.com/loginDeepControl` → poll `api2.cursor.sh/auth/poll`, refreshable
   via `api2.cursor.sh/auth/exchange_user_api_key`).

5. **Proven working path (today).** The installed `cursor-agent` CLI drives
   Composer with the user key, headless:
   ```
   cursor-agent -p --trust --mode ask --output-format json --model composer-2.5 "<prompt>"
   ```
   returns `{ "type": "result", "result": "<text>", "usage": { inputTokens, outputTokens, cacheReadTokens } }`, exit 0.
   Costs: ~10–20 s/call, ~14k *injected* input tokens/call (Cursor prepends its
   own context), agent non-determinism, and Cursor-owned tool execution.

## Decision

**Remove the non-functional `CursorProvider` and the `Provider::Cursor` variant.**
gantry supports `anthropic`, `openai`, and `gemini` (direct provider REST via
`rig-core`). `--model cursor/<id>` now fails with `UnknownProvider`. Cursor work
is **deferred until a usable single-turn API/SDK exists**, or until we
deliberately invest in an agent-wrapping transport.

Removed: `src/provider/cursor.rs`, `Provider::Cursor` + slug routing,
`tests/provider_cursor_test.rs`, `tests/fixtures/cursor/`, the `composer-2.5`
eval price entry, and the now-unused `reqwest` + `eventsource-stream` deps.

## Reinstatement path (when revisited)

Because Cursor is agent-scoped, any reinstatement must wrap an agent and is
naturally scoped to **team mode first** (its model calls — compose, toolless
subagent rounds, unify — need no gantry-owned tools; structured calls already
have a JSON-fence fallback). Options, simplest first:

- **A — CLI subprocess transport:** spawn `cursor-agent -p --output-format json`
  (tool execution suppressed via `--mode ask` / hooks); map `result` → assistant
  text and `usage` → token counts. No sidecar; uses the user `CURSOR_API_KEY`.
- **B — `@cursor/sdk` Node sidecar:** `Agent.prompt()` with deny-tool hooks,
  exposing a turn endpoint the Rust adapter calls.
- **C — Native Rust port** of pi's Connect/protobuf `agent.v1.AgentService/Run`
  client (largest; matches pi exactly).

Any option must reconcile with gantry's invariant that gantry owns the tool loop.

## Consequences

- Smaller dependency tree; no dead bridge code.
- The eval default model was already `anthropic/claude-haiku-4-5` — no impact.
- ADR-0001's reasoning is preserved here as history; its decision is withdrawn.
