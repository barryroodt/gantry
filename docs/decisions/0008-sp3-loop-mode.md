# ADR-0008: SP3 — iterative `loop` mode (binary-first), not a shared `LoopDriver`

**Status:** Accepted
**Date:** 2026-06-02
**Relates to:** SP3 design spec (Solo scratchpad 9) and the binary-first impl spec/plan (scratchpads 23/24).

## Context

SP3 (scratchpad 9) framed the work as a *shared internal `LoopDriver`* used by two consumers — team-mode review rounds and refine-skill's judge→act. Grounding that against reality showed the premise was void:

- **team mode** uses `LoopDriver` as a bare counter — a fixed `MAX_ROUNDS=2` barrier with no real stop policy.
- **refine-skill** is a shipped, pi-based external product (`@jumptag/refine-skill@0.1.2`) with no plan to migrate onto gantry.

So neither "real consumer" exercises a generic driver. The original spec's own top risk — *"over-abstraction before two real consumers"* — was the live situation.

The user reframed the product: **gantry is a standalone, general, headless Rust agent harness that other projects embed binary-first** (spawn as a subprocess, configure via flags/profiles, consume NDJSON). Tight tool-use + context management is a priority. Under that vision the iterative loop is a **core capability surfaced as a mode**, not an internal abstraction for team/refine.

## Decision

Ship SP3 as a new **`--mode loop`**, not a generic shared driver:

1. **`run_agent_pass` extracted** from single mode into a reusable primitive (`PassResult { final_text, stop_requested, exit }`). single mode is now a one-pass caller; the loop calls it per iteration. (Real DRY; single behavior unchanged.)
2. **`loop` mode** runs bounded iterations (`--max-iterations`, default 5, min 1) until the agent calls **`decide_stop`** or the cap is hit.
3. **Context-efficient carry-forward:** each iteration is a *fresh* pass seeded with the prompt + the **prior iteration's final text** — never an accumulating transcript. This is the mode's defining difference from single mode and serves the tight-context priority.
4. **`decide_stop` control tool:** harness-granted only (a new `CONTROL_TOOL_NAMES` set — never in BASE/OPTIN, never requestable via `--tool`, absent from `available_tool_names()`). The loop registry grants it; the pass detects the call by name.
5. **`iteration_start` / `iteration_end{stopped}`** NDJSON events; budget spans iterations; cancellation aborts between/within (inherited from `run_agent_pass`).
6. **Composes with SP2 isolation for free:** `--mode loop --isolate --tool write_file` runs a contained iterate-and-mutate loop (the refine archetype) — verified by an e2e test.

**`LoopDriver` stays thin** (cap + index). The mode owns the loop body + stop check. No body/hook/stop trait or closure API — that was the two-consumer framing; with a single consumer it would be the over-abstraction the original spec flagged.

## Non-goals (deferred)

- **Multi-phase iterations** with per-phase tool allowlists (a hard judge-read-only → act-mutating split). v1 is single-phase per iteration; the agent's own tools (incl. mutation, when granted) act within the pass.
- **A generic convergence predicate** over per-iteration metrics — no generic metric exists under binary-first; the `decide_stop` model signal is the general stop.
- **Refactoring team mode** onto the loop (its multi-agent barrier loop is a different shape; untouched).
- **A public Rust library `LoopDriver` API** — binary-first; the loop type stays internal.

## Consequences

- An embedder can drive an "iterate until done" workflow purely via the CLI + NDJSON (`--mode loop`, `--max-iterations`, `decide_stop`), with bounded per-iteration context.
- The refine archetype (assess → mutate, repeat, contained) is expressible today via `--mode loop --isolate --tool write_file edit_file` without gantry depending on refine.
- If a real need for hard per-phase tool separation or a public loop API appears, the deferred items become their own scoped work.
