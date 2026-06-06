# ADR-0012: SP7 — reversible retrieval of elided tool output

**Status:** Accepted
**Date:** 2026-06-06
**Relates to:** ADR-0009 (SP5 — output compression at the tool-result boundary).

## Context

ADR-0009's head+tail line cap replaced elided lines with a prose hint:

```
[gantry: N lines omitted (R bytes raw); re-read with a narrower range/query for full detail]
```

This gives the model a recovery path, but the path is non-deterministic for `shell`
(re-running a command may produce different output) and always costs a full extra turn —
issue a new tool call, wait for dispatch, pay the token round-trip. For `read_file` and
`git_diff`, re-reading is deterministic but still wasteful when the model only needs a
targeted slice of what was already computed.

Headroom's CCR (cached-content retrieval) idea showed the direction: stash the cap input
and serve slices on demand. Gantry's architecture makes this trivial and zero-cost for
the common case: it **owns its tools** (no external proxy, no ML, no SQLite), so it can
stash at the compression boundary and expose a harness-granted control tool for retrieval.

## Decision

### Handle minting

A handle is minted **if and only if** the cap elides at least one line (i.e. the output
exceeds `HEAD_LINES + TAIL_LINES` lines after dedup). Dedup-only output — where no lines
were cut — is self-describing and does not need a handle.

The stored content is the cap's **input** (post-dedup for noisy tools such as `shell`,
raw otherwise), lines joined by `\n`. Note that `str::lines()` LF-normalises CRLF input,
so the stored bytes faithfully represent what the model is shown, not the original line
endings of the tool output.

Handles are content-addressed: `{tool}/{12-hex}` where the hex is the first 12 characters
of the SHA-256 of the stored content. Identical cap inputs (e.g. the same large file read
twice) map to the same handle and share one store entry.

### Storage

An in-memory `RetrievalStore` (a `HashMap<String, String>`) is owned by `ToolRegistry`
and lives for the duration of the run. In team mode the store is wrapped in an `Arc` so
all subagents share one instance; cross-subagent dedup is harmless (same content, same
handle). There is no eviction — runs are bounded by token budget and turn cap.

### `retrieve` control tool

A harness-granted `retrieve` tool is always available (like `decide_stop`), never
requestable via `--tool`. Parameters:

- `handle` (required) — the `{tool}/{12-hex}` handle from the `tool_result`.
- `start` (optional, 1-based inclusive) — first line to return; defaults to the first
  elided line (i.e. `HEAD_LINES + 1`).
- `end` (optional, 1-based inclusive) — last line to return; defaults to the last elided
  line (i.e. `total_lines - TAIL_LINES`).
- `pattern` (optional) — return only lines matching this regex (applied after the
  `start`/`end` slice).

Omitting `start`/`end` returns the elided middle by default — the natural recovery action.
The returned content is byte-faithful: it is a `\n`-joined slice of the stored lines.

### Event contract

`tool_result` gains an additive `handle?: String` field. Embedders that pattern-match on
`..` are unaffected; those that construct `tool_result` literals must add the field (it is
`None`/absent when no cap elision occurred).

## Boundary and consequences

- **Reverses the compression cap only.** The 256 KiB byte cap applied inside each tool by
  `truncate::truncated_output` is a separate, earlier gate. Content truncated at that layer
  is never stashed and cannot be retrieved.
- **No eviction.** The store grows monotonically within a run. Fine for normal workloads
  (bounded by budget + turns), but a long team session with many large capped outputs
  could accumulate significant memory. See tech-debt for a proposed LRU follow-up.
- **Team mode:** one shared `Arc<RetrievalStore>` means a subagent can retrieve a handle
  minted by another subagent or the coordinator. This is intentional and harmless.
- **Content-addressing:** duplicate cap inputs (same file read twice at the same size) hit
  the same slot — no wasted storage, handles are stable and reproducible.
- The `tool_result` event contract gains an additive `handle?` field (matchers use `..`;
  literal constructors updated).

## Non-goals (deferred)

- **Store eviction / size cap** — a size-capped LRU if memory pressure is observed in
  long team sessions (tracked in tech-debt).
- **Stashing dedup-only output** — dedup collapses are self-describing; no recovery
  handle needed.
- **Cross-run persistence** — retrieval handles are valid for the lifetime of one run
  only; persisting them across runs is not a stated requirement.
- **Retrieving content truncated by the 256 KiB hard cap** — that layer is inside the
  tool, before compression; reversing it would require a second in-tool stash and is
  deferred.
