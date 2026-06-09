# Design: machine-grade terminal contract for embedders (opportunity A)

**Date:** 2026-06-08
**Status:** Approved — ready for implementation planning.
**Branch:** `feat/embedder-contract`
**Related:** `2026-06-08-murli-borrowable-opportunities.md` (opportunity A; B/C/D deferred)

## Problem

Gantry is embedded binary-first: a parent process spawns it, configures via
flags/profiles, and consumes the NDJSON event stream. Two gaps make that contract
fragile for an orchestrating embedder:

1. **No contract version.** The NDJSON/exit contract evolves (we just removed
   `template` from `subagent_spawn`) with no signal — a consumer's parser can't detect
   or branch on the change.
2. **Failures are opaque about retryability.** A provider 429 rate-limit, a transient
   5xx/network blip, and a hard fatal error all collapse to `error{kind:Provider}` /
   `exit 1`. An embedder cannot tell "back off and retry" from "give up", and gets no
   retry delay — even though gantry already computes that delay internally.

## Goals

- Stamp a contract version on the stream so embedders can version-guard their parser.
- Surface retryability + a retry delay on terminal failures, and a distinct exit code
  for the rate-limited case, so an orchestrator can make back-off decisions — with or
  without parsing the NDJSON.

## Non-goals (YAGNI)

- No `suggestion` / `doc_url` / `error_type` fields (murli has them; gantry's `kind` +
  `message` + `recoverable` already cover its needs).
- Only **one** new exit code (`RateLimited`); no full murli-style taxonomy.
- No new classification logic — reuse the existing `classify_error`.
- No change to `with_retry`'s signature.
- Opportunities B (`describe`/`--schema`) and C (`doctor`) are out of scope here.

## Current state (grounded)

- `ExitCode` (`src/events.rs`): `Ok(0) / Error(1) / Budget(2) / Timeout(3) / Config(4)`.
- `GantryEvent::Error { ts, kind: ErrorKind, message }`;
  `ErrorKind = Config | Provider | TeamCollapse | Internal`.
- `GantryEvent::Start { ts, model, provider, mode, workdir }` — the leading line.
- `src/provider/retry.rs` already defines
  `ErrorClass = RateLimited { retry_after: Option<Duration> } | Transient | Fatal`
  and `classify_error(&anyhow::Error) -> ErrorClass` (best-effort text match, since the
  rig adapters flatten HTTP status into stringified errors). `with_retry` computes the
  class on every attempt but **discards it** when retries are exhausted, returning a bare
  `anyhow::Error`.
- Provider-failure → `error` event + exit is **hand-duplicated**: the agent-pass site
  (`single.rs`, shared by `loop_mode` via `run_agent_pass`) and team's `structured_call`
  failure sites (`team.rs`). A shared `outcome(exit, meter)` (`mode/mod.rs`) and
  `config_error(msg)` (`mode/isolation.rs`) exist, but not for the provider path.

## Design

### 1. Contract versioning

Add `schema_version` to `GantryEvent::Start`, value `"1.0"`:

```rust
Start {
    ts: u64,
    schema_version: &'static str,  // const SCHEMA_VERSION = "1.0"
    model: String,
    provider: String,
    mode: String,
    workdir: String,
}
```

Define `pub const SCHEMA_VERSION: &str = "1.0";` in `events.rs`. Embedders read the
first NDJSON line to learn the version.

**Bump policy** (documented in README): semver string. **MAJOR** = remove/rename a field
or event, or change semantics. **MINOR** = additive field/event/exit-code. This feature
ships as the initial declared version `1.0`.

### 2. `error` event enrichment

```rust
Error {
    ts: u64,
    kind: ErrorKind,
    message: String,
    recoverable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after_ms: Option<u64>,
}
```

Non-provider error sites set `recoverable: false`, `retry_after_ms: None`
(`Config` = user must fix; `TeamCollapse` / `Internal` = not auto-retryable). The
provider-failure path computes both (Section 5).

### 3. Exit code

Add `ExitCode::RateLimited`, mapped to process exit **`5`** (next free):

```rust
pub enum ExitCode { Ok, Budget, Timeout, Config, Error, RateLimited }
// as_process_exit: Ok=0, Error=1, Budget=2, Timeout=3, Config=4, RateLimited=5
```

Existing codes are unchanged; the exit ABI only grows. A parent that checks `$?` can
back off on `5` without parsing NDJSON.

### 4. `ErrorClass` → contract mapping

| Final `ErrorClass` (retries exhausted) | `kind` | `recoverable` | `retry_after_ms` | exit |
|---|---|---|---|---|
| `RateLimited { retry_after }` | `Provider` | `true` | `retry_after` → ms, else `None` | `RateLimited` (5) |
| `Transient` | `Provider` | `true` | `None` | `Error` (1) |
| `Fatal` | `Provider` | `false` | `None` | `Error` (1) |

`retry_after_ms = retry_after.map(|d| d.as_millis() as u64)`.

### 5. Wiring

Add one shared helper beside `outcome` / `config_error`:

```rust
// src/mode/mod.rs
pub(crate) fn emit_provider_failure(err: &anyhow::Error) -> ExitCode {
    use crate::provider::retry::{classify_error, ErrorClass};
    let (recoverable, retry_after_ms, exit) = match classify_error(err) {
        ErrorClass::RateLimited { retry_after } => (
            true,
            retry_after.map(|d| d.as_millis() as u64),
            ExitCode::RateLimited,
        ),
        ErrorClass::Transient => (true, None, ExitCode::Error),
        ErrorClass::Fatal => (false, None, ExitCode::Error),
    };
    let _ = GantryEvent::Error {
        ts: now_ms(),
        kind: ErrorKind::Provider,
        message: format!("{err:#}"),
        recoverable,
        retry_after_ms,
    }
    .emit();
    exit
}
```

Replace the hand-rolled `GantryEvent::Error { kind: Provider, .. } + ExitCode` at each
provider-failure site (`single.rs` agent-pass, `team.rs` `structured_call` sites) with a
call to `emit_provider_failure(&err)`. `loop_mode` inherits via `run_agent_pass`. This
single-sources the new mapping **and removes the existing duplication** — an in-scope
cleanup of code we're already touching. `with_retry` is untouched (we re-classify the
final error at the surface via the existing `classify_error`, the single source of truth).

`TeamCollapse` / `Config` / `Internal` error sites are left as direct
`GantryEvent::Error` constructions, updated only to pass `recoverable: false,
retry_after_ms: None`.

## Contract change summary

All additive (no field/event/exit removed or renamed) → consistent with declaring `1.0`:

- `start`: `+ schema_version`
- `error`: `+ recoverable`, `+ retry_after_ms` (omitted when null)
- exit codes: `+ 5 RateLimited`

## Testing

- **Unit (`mode`):** `emit_provider_failure` over a stub error per class → asserts the
  `(recoverable, retry_after_ms, ExitCode)` triple (RateLimited→true/Some(when hinted)/5;
  Transient→true/None/Error; Fatal→false/None/Error). `classify_error` itself is already
  covered in `retry.rs` tests.
- **Integration:** extend the provider wiremock tests — a `429` response carrying a
  `retry-after` hint → assert the terminal `error` event has `recoverable: true` +
  `retry_after_ms`, and the process exits `5`.
- **Roundtrip:** `events_roundtrip` asserts `start` serializes `schema_version: "1.0"`
  and `error` round-trips the two new fields (incl. `retry_after_ms` omitted when `None`).

## Docs

- README NDJSON event table: `start` gains `schema_version`; `error` gains
  `recoverable`, `retry_after_ms`.
- README exit-code table: add `5  RateLimited`.
- A short "Contract versioning" note documenting the bump policy.

## Out of scope / follow-ups

- Opportunity B (`gantry describe` / `--schema`) — would report this same
  `schema_version`. Opportunity C (`gantry doctor`). Both deferred (see opportunities doc).
- If a future provider adapter exposes raw HTTP status/headers, `classify_error` /
  `parse_retry_after` can be upgraded from text-matching without touching this contract.
