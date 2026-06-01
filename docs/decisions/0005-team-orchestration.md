# ADR-0005: Harness-driven team orchestration

**Status:** Accepted
**Date:** 2026-05-31
**Deciders:** gantry team-orchestration (from wrily note `team-mode-workflows`)
**Supersedes:** ADR-0003 §Decision / §2 (coordinator prompt owns control flow). Builds on ADR-0004 (profiles) and the Change-2 collect barrier.

## Context

Team mode today is a generic 20-turn agent loop whose **coordinator LLM owns control flow** via prompt: the profile's `system.md` tells the model *when* to call `spawn_subagent` / `collect_outputs` / `broadcast_summary` and *how* to unify. ADR-0003 itself flagged the fragility (§Consequences): the model can skip the collect, call tools out of order, or hallucinate the dedup.

Claude Code's `/workflows` solves the same fan-out/collect/unify problem with **deterministic code** — agents only think; control flow (`pipeline`/`parallel`/`loop`/`budget`/schema) is the runtime's. `/workflows` cannot replace team mode (gantry is provider-agnostic, ships in Docker/CF, and exists to remove the Agent-Teams dependency), but its *pattern* ports: **move orchestration from the coordinator prompt into the Rust harness; reduce the coordinator LLM to two decision points — compose the team, and unify findings.**

Two enablers already landed: ADR-0004 made the team prompts *profile data* (not hardcoded), and Change 2 made `collect_outputs` a real barrier. This ADR inverts the remaining control flow.

## Decision

### 1. Team mode becomes an explicit Rust state machine
Replace the prompt-driven 20-turn loop in `team.rs::run` with a deterministic driver that calls the LLM only at decision points:

```
detect scope (git diff --stat, read CLAUDE/AGENTS.md, list dirs)   ← harness (deterministic)
  → LLM "compose": returns a structured reviewer plan[]            ← model thinks
  → spawn_subagent × plan.len()                                    ← harness
  → BARRIER: collect_outputs round 1 (Change 2)                    ← harness
  → [loop] LLM builds digest → broadcast_summary → barrier N+1     ← harness drives the loop
  → LLM "unify": returns structured findings                       ← model thinks
  → emit the JSON fence from the validated object                  ← harness
```

The coordinator LLM no longer "remembers" to call tools in order — the harness does. `spawn_subagent` / `collect_outputs` / `broadcast_summary` remain as the **mechanism** (the roster + barrier), but the harness invokes them; the model is consulted only for *compose* and *unify*.

### 2. Two profile prompts replace one coordinator prompt
The team profile supplies a **compose** prompt and a **unify** prompt instead of one orchestration `system.md`. The harness owns Steps "spawn → barrier → loop → broadcast"; the profile owns only the thinking prompts. (Mechanism: extend the profile manifest with `compose`/`unify` prompt files for team profiles, or reuse `system`=unify + a new `compose` key — resolved in the plan.)

### 3. Schema-forced structured output (provider-gated)
`compose` and `unify` use the provider's **forced tool-use / structured-output** capability with the relevant JSON schema (reviewer plan; unified findings), validated in Rust with **one retry** on mismatch. The terminal `assistant_text` is still the `` ```json `` fence — but generated from the validated object, not free model prose — so the downstream consumer (wrily TS `extractFindings`) is unchanged.

`ProviderAdapter` gains a structured-output capability flag. Providers without it (e.g. gemini/cursor today) fall back to the current fence-and-parse path.

### 4. Harness-driven rounds (folds in the optional Change 4)
The number of review rounds is decided by the **harness**, not hardcoded in the prompt: loop until no-new-findings convergence or a round/budget cap. The model still authors each digest. This is the same loop primitive as SP3 (iterative mode); the two are reconciled into one driver (see the SP3 spec).

## Non-goals
- Adversarial verify stage and per-subagent model override (note Changes 5–6) — optional, later, flag-gated.
- Changing the unified-findings JSON schema or the fence contract (preserved for the consumer).
- Single mode (unchanged; only team-mode orchestration is inverted).

## Consequences

### Positive
- Deterministic control flow: the model can no longer skip/reorder/hallucinate orchestration; rounds and barriers are guaranteed.
- Robust output: schema-forced compose/unify with retry removes the "model forgot the fence" failure class (where the provider supports it).
- The coordinator prompt shrinks to two focused prompts; Steps 3–5 process management leaves the prompt entirely.

### Negative / costs
- More harness complexity (a real state machine + a provider capability flag).
- Provider-gated structured output means two code paths (forced tool-use vs fence fallback).
- The team profile grows a `compose`/`unify` prompt split (data change).

### Neutral
- Supersedes ADR-0003 §Decision/§2; ADR-0003's tool *semantics* (spawn/collect/broadcast lifecycle) and the JSON output schema remain valid.
- The harness loop is shared with SP3 — one loop primitive, two consumers (team rounds, refine judge→act).

## Open questions (resolve in the plan / SP3 reconcile)
1. **Profile prompt split:** new manifest keys `compose`/`unify`, or `system`=unify + `compose`? Recommend explicit `compose`/`unify` keys for team profiles; `system` stays for single mode.
2. **Provider capability mechanism:** a `supports_structured_output()` on `ProviderAdapter` + a typed request path, vs a generic "tool-use with one tool" helper. Recommend a capability flag + a small forced-tool-call helper, fence fallback otherwise.
3. **Rounds convergence:** no-new-findings vs fixed cap vs budget-bounded — unify with SP3's pluggable stop.

## Implementation
After this ADR, implement via the orchestration plan (scratchpad), sequenced after Change 2 (done) and reconciled with SP3 so the loop driver is shared. Validation: `cargo test` unit coverage of the state machine + the `003-team-mode` eval asserting one `subagent_done` per spawned subagent before the unify fence, and exactly one valid JSON fence.

## Known limitation (2026-05-31) — RESOLVED 2026-05-31

This inversion initially left team mode blind: the coordinator has no tool loop
and subagents ran toolless, with nothing injecting the code, so `compose` refused
("I need the diff …"). **Resolved** via the generic tools mechanism (spec
`docs/superpowers/specs/2026-05-31-team-review-context-design.md`, plan
`docs/superpowers/plans/2026-05-31-team-review-context.md`): subagents now run a
bounded per-round tool loop with the profile's toolset (`registry.schemas()` +
`dispatch`), `compose` plans the team without a diff, and eval fixture `003` is a
real git repo. The `team_fixture_003_runs_live` eval — no longer `#[ignore]`d —
passes a grounded review live. The harness stays domain-agnostic; review's diff
access is just `git_diff` in the review profile's toolset.
