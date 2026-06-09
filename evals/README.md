# gantry evals

## Fixtures

### Review (finding-oriented)
- 001-sql-injection — security finding required
- 002-delta-clean — no findings expected
- 003-team-mode-multi-dir — multi-directory team review
- 004-budget-trip — token cap hit

### Skill refinement (loop + isolate + mutate)
- 005-refine-small-skill — small skill with obvious headroom; exercises the core refine loop
- 006-refine-large-skill — deliberately bloated >500-line skill; exercises reversible retrieval (#10) and transcript compaction (#11)

## Refinement assertions

`005`/`006` drive the `refine` profile (`--mode loop --profile profiles/refine`: loop +
COW isolate + mutate tools) against a `repo/SKILL.md`. They assert **gantry behaviour**,
not model quality (the output is the model's; the harness is what we're regression-testing):

| `expected.json` field | Asserts |
|---|---|
| `expect_decide_stop` | the loop converged via a `decide_stop` call (not the iteration cap) |
| `must_change_paths` | each substring appears in a terminal `changes`-event path (the file was edited) |
| `expect_retrieve_handle` | ≥1 `tool_result` carried a retrieval handle — output was capped + stashed (#10) |
| `context_limit` | passed as `--context-limit`; `006` sets it low to favour compaction (#11) |

`history_compacted` is **recorded as a metric** on `FixtureResult` (soft signal), not gated —
the model's turn count per pass varies, and #11 is already hard-tested in the gantry crate.

## Running the refinement evals (manual)

They hit a real model and are non-deterministic, so they're gated behind `GANTRY_LIVE_EVAL=1`:

```bash
GANTRY_LIVE_EVAL=1 ANTHROPIC_API_KEY=sk-... \
  cargo test -p gantry-evals --test runner_test -- --nocapture refine_
```

Each prints a metrics line:

```
[eval] 006-refine-large-skill: passed=true in=… out=… dur_ms=… cost=$… \
       files_changed=… retrieve_handles=… history_compacted=…
```

**Before/after an update:** capture the metrics line before your change and again after, and
compare. (The `baseline.json` auto-drift comparison below is described but **not yet wired**
in code — compare the printed lines manually for now.)

## Baseline drift policy (aspirational — not yet wired)

- tokens: ±15%
- duration: ±25%

When baseline value is 0, drift check skipped (treated as needs-bootstrap).

## Update flow (aspirational — not yet wired)

1. Run `cargo run --bin gantry-evals` in real env (live API keys).
2. Inspect `baseline.json.new` written alongside.
3. If drifts intentional, replace `baseline.json` with new file + commit `chore(evals): bump baseline for <reason>`.
