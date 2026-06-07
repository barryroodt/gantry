# ADR-0013: SP8 — transcript compaction for context-window headroom

**Status:** Accepted
**Date:** 2026-06-06
**Relates to:** ADR-0009 (SP5 — output compression at the tool-result boundary), ADR-0012 (SP7 — reversible retrieval of elided tool output).

## Context

`run_agent_pass` is append-only: every turn re-sends the entire message history to the
model. Over `MAX_TURNS` (20) iterations with large tool outputs, the cumulative transcript
can approach the model's context window.

Prompt caching partially mitigates the token cost — unchanged prefix hits are billed at
~10 % of full-read rates. But cache-read pricing addresses *cost*, not *size*: if the
transcript exceeds the model's context limit the call fails regardless of how cheap the
cached tokens are. The goal here is **context-window headroom**, not cost reduction.

ADR-0012's `RetrievalStore` already provides the stash-and-stub primitive: content is
stored verbatim under a content-addressed handle and recovered via the `retrieve` control
tool. Transcript compaction reuses that infrastructure to fold old, large tool results out
of the live history without losing any bytes.

## Decision

### Opt-in via `--context-limit <TOKENS>`

Compaction is **off by default**. When `--context-limit N` is supplied, after each
completed turn gantry checks whether the just-completed turn's reported `input_tokens`
exceeded `N`. Only when the threshold is breached does compaction run.

This threshold-triggered design is deliberate (see the prompt-cache tradeoff below).

### Trigger: previous turn's `input_tokens`

The provider reports `input_tokens` in its response after each model call. This value
reflects the full transcript that was actually sent. Using it (rather than a local byte
estimate) makes the trigger accurate regardless of tokeniser internals.

### In-place stash-and-stub via `RetrievalStore`

When the threshold is exceeded, `compact_history` scans the message history and stubs out
`ToolResult` entries that are all of:

1. **Older than `KEEP_RECENT_TURNS` (= 3) turns** — the most-recent three turns are always
   kept verbatim so the model retains immediate working context.
2. **Larger than 512 bytes** — tiny results are not worth stubbing; the overhead of the
   stub hint would approach the savings.
3. **Not already a stub** — idempotent; a result stubbed in an earlier compaction pass is
   not re-processed.

Each qualifying result's content is stored in the `RetrievalStore` under a
`history/<12-hex>` handle (SHA-256 of the original content, first 12 hex chars). The
in-history content is replaced with:

```
[gantry: tool result (N lines) elided to free context; retrieve(handle="history/<hex>", start=1) to recover the elided lines]
```

The stub is self-describing: the model sees a recovery path, the handle is stable, and
`start=1` is explicit so the model can pass it directly to `retrieve`.

### Recovery via `retrieve`

The `retrieve` control tool (ADR-0012) is always available. Passing
`handle="history/<hex>", start=1` returns the full original content, byte-faithful.

### `HistoryCompacted` event

An additive `history_compacted` NDJSON event is emitted whenever `compact_history` stubs
at least one result:

```json
{"event":"history_compacted","ts":…,"role":"single","turn":5,"results_elided":3,"input_tokens":95000}
```

`results_elided` is the count of results stubbed in that compaction pass. `input_tokens`
is the value that triggered the threshold check (the previous turn's reported count).
When no results qualify (e.g. all recent results are already stubs or under 512 B), the
event is suppressed — zero-elision passes are silent.

## The prompt-cache tradeoff

Compaction mutates message history. Any `ToolResult` that is replaced with a stub changes
a token in the prompt prefix, which **invalidates the KV-cache** from that position
onward. The next turn pays full-read rates for the suffix after the first stub.

This is acceptable **only** because compaction is threshold-triggered, not continuous:

- In a typical run the threshold is never reached, compaction never fires, and the cache
  is never disturbed.
- When the threshold is reached (context pressure is real), the cache miss on the next
  turn is the lesser cost compared to a context-window overflow failure.

Per-turn or cost-driven compaction was considered and rejected precisely because it would
destroy cache prefix hits on every turn — negating the provider's cache design.

## Recovery fidelity

The `RetrievalStore` holds exact bytes. `retrieve` reconstructs content line-faithfully:
the stored content is the original result string, `\n`-joined. `str::lines()` normalises
CRLF on ingestion, so trailing newlines and Windows line endings are LF-normalised in
storage — the same normalisation applied by ADR-0012's handle minting.

## Scope and consequences

- **Single and loop modes only.** `run_agent_pass` is the shared entry point for both.
  Team mode spawns subagents via a separate code path; subagent tool-loop compaction is
  deferred (see Non-goals).
- **Additive event.** `history_compacted` follows the standard additive-field rule —
  embedders that pattern-match on `..` are unaffected.
- **No store eviction.** The compaction store entries are per-run and bounded by the turn
  cap; the same no-eviction rationale from ADR-0012 applies. Loop-mode runs accumulate
  entries across iterations but are bounded by `--max-iterations`.
- **Idempotent.** Running compaction multiple times (across turns) does not re-stub already
  stubbed results; the 512 B threshold filters them out automatically.
- **`KEEP_RECENT_TURNS = 3` is a constant**, not configurable. Tuning it is deferred.

## Non-goals (deferred)

- **Subagent tool-loop compaction** — the subagent loop in team mode shares `compact_history`
  but wiring it into the subagent dispatch path is deferred. Tracked in tech-debt.
- **`--keep-recent-turns` flag** — `KEEP_RECENT_TURNS = 3` is a compile-time constant;
  a CLI knob is a trivial follow-up if a consumer needs a different window.
- **Deduplicating re-stash of ADR-0012-capped results** — a result already capped by the
  output compressor carries a recovery hint; stubbing it again is correct but stores the
  hint text rather than the original content. Dedup is deferred.
- **Per-iteration `RetrievalStore` scoping in loop mode** — stubs from iteration N persist
  into iteration N+1; the blobs are orphaned (no in-history handle points at them) but
  still held in memory. Bounded; deferred.
