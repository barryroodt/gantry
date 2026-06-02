# ADR-0009: SP5 — output compression at the tool-result boundary

**Status:** Accepted
**Date:** 2026-06-02
**Relates to:** SP5 design spec (Solo scratchpad 11) and the impl spec/plan (scratchpads 25/26).

## Context

Every tool's output reached the model through only blunt byte-truncation
(`truncate::truncated_output`, a 256 KiB cap + `…[truncated]` marker). `shell`
collected raw streamed output + the same cap — it did not use pi-shell's
minimizer. So there was no structured compression anywhere, and the harness's
stated priority is tight, efficient context. rtk ("Rust Token Killer") solves
this for arbitrary agents via a PreToolUse hook/proxy + SQLite telemetry; gantry
**owns its tools** and consumes binary-first (NDJSON), so it can compress at the
source and report savings inline — porting rtk's *techniques*, not its
architecture.

## Decision

Add a single compression step at the dispatch boundary — `ToolRegistry::dispatch`
runs `compress::compress(name, output)` after `dispatch_inner`, before emitting
`tool_result`. The 256 KiB byte cap inside tools stays as an outer safety net.

### Correctness principle (load-bearing)
**Never corrupt byte-faithful content.** An agent that reads a file to edit it,
or reads a diff, needs exact bytes. Therefore:
- **Recoverable head+tail line cap (ALL tools):** over-budget output keeps the
  first `HEAD_LINES` (400) and last `TAIL_LINES` (100) lines and replaces the
  middle with a machine-readable hint (`[gantry: N lines omitted (R bytes raw);
  re-read with a narrower range/query for full detail]`). Retained lines are
  byte-identical; the cut is bounded and recoverable, never a heuristic drop.
  The tail is kept because errors/summaries cluster at the end of verbose output.
- **Consecutive-line dedup (NOISY tools only):** runs of ≥ `DEDUP_RUN` (5)
  identical consecutive lines collapse to one instance + `… (repeated K×)`.
  Applied only to `NOISY_TOOLS` (`["shell"]`) — high-volume, non-faithful.
  Never to `read_file`/`git_diff`/`skill_load`/etc.
- **Fail-safe:** `compress` is a total function over `&str` lines — no panics.
- **Zero-cost when idle:** small outputs with no collapsible run return unchanged
  with no allocation; the cap-only path borrows via `Cow` (no per-line `String`).

### Telemetry
`tool_result` gains `bytes_out: u64` = the byte length actually emitted to the
model (post-compression); `bytes` stays = raw tool output size. Embedders compute
savings from the pair over the NDJSON stream — no SQLite.

## Non-goals (deferred)

- **Lossy failure-focused line *dropping*** ("keep errors, drop progress") — it
  can hide signal the model needs; only dedup + recoverable caps ship in v1.
- rtk's command-hook/proxy/rewrite machinery; a TOML filter DSL; SQLite telemetry;
  compressing arbitrary external agent commands.
- Profile/CLI-configurable caps — constants in v1 (`HEAD_LINES`/`TAIL_LINES`/
  `DEDUP_RUN`); a knob is a trivial follow-up if a consumer needs it.
- Broadening `NOISY_TOOLS` / richer per-tool filters (v1 = shell dedup only).

## Consequences

- Verbose tool output (large reads, searches, shell logs) is bounded and
  token-light by default, with precise recovery, improving context efficiency for
  every embedder — no per-consumer wiring.
- The `tool_result` event contract gains an additive `bytes_out` field (matchers
  use `..`; literal constructors updated).
- Tuning the caps or adding lossy/per-tool filters becomes scoped follow-up work
  if real usage demands it.
