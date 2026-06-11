# Borrowable opportunities from murli-rs

**Date:** 2026-06-08
**Status:** Exploration — option **A** selected for design; **B/C/D** deferred.
**Source:** https://github.com/murli-cli/murli-rs (MIT) — Rust middleware that makes
clap/argh CLIs "speak natively to AI agents."

## Context

`murli-rs` and gantry sit at different layers — murli helps a CLI tool *be consumed
by* agents; gantry *runs* agents — but they share one thesis: **the binary's machine
contract is the product.** murli's contract ergonomics are more mature than gantry's
in a few specific, borrowable ways. This doc records the comparison and the four
opportunities surfaced, so the deferred ones can be picked up later.

### What murli-rs does

Injects agent flags (`--schema`, `--agent`, `--dry-run`, `--force`, `--profile`);
emits structured JSON envelopes (success / error / plan) stamped with
`schema_version` + `tool_version`; a rich error shape (`error_type`, `suggestion`,
`recoverable`, `retry_after_ms`, `doc_url`); a 10-code exit taxonomy
(OK, USER_ERROR, TOOL_ERROR, PARTIAL, TIMEOUT, NOT_FOUND, PERMISSION, CONFLICT,
RATE_LIMITED, CANCELLED); and `describe` / `doctor` subcommands.

### Gantry's contract today (baseline, `src/events.rs`)

- **Exit codes (5):** `Ok(0) / Error(1) / Budget(2) / Timeout(3) / Config(4)`.
- **`Error` event:** `{ kind: Config|Provider|TeamCollapse|Internal, message }` — no
  recoverability or retry hint; a provider 429, a transient blip, and an auth failure
  all collapse to `kind=Provider` / `exit 1`.
- **`Start` event** already exists (`model/provider/mode/workdir`) — a natural home
  for a contract version. No version is emitted today.
- No self-description (`--schema`/`describe`); no preflight (`doctor`).

## Opportunities

### A — Retry-aware failures + versioned stream  *(SELECTED)*

Bundle two synergistic contract hardenings:

1. **`schema_version`** on the `Start` event so embedders can detect/branch on
   contract evolution (we just removed `template` from `subagent_spawn` — silent today).
2. **Retry-aware failures**: enrich the terminal failure path so an orchestrating
   embedder can tell "back off and retry" from "give up" — e.g. `recoverable` +
   `retry_after_ms`, and a clear signal for provider rate-limits (today everything is
   `exit 1` / `kind=Provider`).

- **Gap closed:** embedders running gantry at scale cannot currently make retry/back-off
  decisions, and cannot version-guard their NDJSON parsers.
- **Value:** High — direct operational win, on-vision (the CLI+NDJSON *is* the API).
- **Cost:** Modest. Design captured separately in
  `2026-06-08-embedder-contract-design.md`.

### B — Self-describing contract (`gantry describe` / `--schema`)  *(deferred)*

Emit a machine-readable JSON description of gantry's own contract: CLI flags, modes,
the tool catalog (names + opt-in status), the NDJSON event schema, and the exit-code
taxonomy. Lets embedders codegen types / validate at build time instead of hand-reading
the README.

- **Value:** Medium-High; most on-vision but a bigger lift. Pairs naturally with A's
  `schema_version` (describe would report the same version).

### C — `gantry doctor` preflight  *(deferred)*

Preflight check: provider key presence/reachability, isolation backend availability
(APFS/overlay), oh-my-pi tool dependencies. Surfaces environment problems upfront
instead of mid-run.

- **Value:** Medium; quality-of-life for embedders.

### D — Other / combinations  *(open)*

Placeholder for further ideas or combining the above (e.g. A + B as a single
"contract" milestone).

## Deliberately NOT borrowed (poor fit)

- **`--dry-run` / plan envelope + `--force` mutation guard** — gantry's mutation model
  is agent-driven + `--isolate` COW; a single "planned mutation" does not map.
- **Config-dir profiles (`dirs::config_dir()`)** — gantry profiles are path-based by
  design (`--profile <dir>`), bundled or consumer-supplied.

## Decision

Proceed with **A**. **B**, **C** remain documented here for future pickup; **D** open.
