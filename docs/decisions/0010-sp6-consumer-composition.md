# ADR-0010: SP6 — consumer composition is binary-first + consumer-side; no adapters in gantry

**Status:** Accepted
**Date:** 2026-06-02
**Relates to:** SP6 design spec (Solo scratchpad 12); the binary-first product vision.

## Context

SP6 was specified as "consumer adapters" — composing the gantry primitives for
each named consumer (sleuthly, refine-skill). The spec itself scoped this as
*"mostly consumer-side work… consumers parse [the NDJSON]; output contracts stay
harness-agnostic."* The product vision is now explicit: **gantry is a standalone,
general, headless agent harness consumed binary-first** — embedders spawn it as a
subprocess, configure it via flags/profiles, and consume its NDJSON event stream
in their own code. Named consumers are *examples*, not design drivers.

## Findings

Under binary-first, gantry ships **no consumer adapters** — composition happens in
each consumer's repo. The grounding confirmed every gantry-side primitive each
named consumer needs already exists after SP1–SP5, and the two gaps the SP6 spec
flagged are closed:

- **sleuthly** (run integration CLIs like Axiom, read-only posture) → the
  per-profile **`shell_allow`** program allowlist (SP4). A sleuthly profile sets
  `shell_allow = ["axiom", …]`. No gantry change needed.
- **refine-skill** (per-iteration telemetry for its `log.json`) → **`iteration_start`/
  `iteration_end`** events + `tool_result`/`assistant_text`/`result` (SP3), plus
  `--mode loop` (SP3), `write_file`/`edit_file` + `--isolate` (SP2). A consumer maps
  the NDJSON stream to its own log format. No gantry change needed.

So **SP6 requires no gantry code.** The only residual gantry-side value is
*demonstrating* the composition.

## Decision

1. **No consumer adapters, no consumer-repo work, no consumer output parsers**
   (Block Kit, `log.json`) in gantry — those live in the consumers' repos.
2. **Ship one illustrative example profile** — `profiles/refine/` (the
   iterate-and-mutate archetype) — alongside the existing `profiles/review/`, to
   demonstrate composing loop mode (SP3) + mutation/isolation (SP2) +
   compression (SP5). A deterministic test asserts it loads and composes
   (`profile_regression_test::refine_profile_composes_loop_mutation_isolation`).
3. **No new live eval fixtures.** The live team eval is already opt-in
   (`GANTRY_LIVE_EVAL=1`, ADR-0009 follow-up) because real-model tests are flaky;
   adding more consumer live fixtures to the default gate is rejected.

This closes the gantry-side of the SP1–SP6 generalization roadmap: the harness is
a task-agnostic, binary-first agent runtime with profiles, per-phase tool grants,
mutation, optional COW isolation, an iterative loop, and output compression — all
driven via flags/profiles and observed via NDJSON.

## Consequences

- Embedders integrate by writing a profile + parsing NDJSON; `profiles/review`
  and `profiles/refine` are the worked references.
- sleuthly/refine-skill adoption is tracked in their own repos (out of scope here).
- Optional future harness work (per-subagent model, adversarial verify, multi-phase
  loop tools, configurable compression caps, a public Rust library API) remains
  available if a concrete need appears — none is required for consumer composition.
