# Design: grounded team review via generic subagent tools

**Date:** 2026-05-31
**Status:** Draft (awaiting review)
**Related:** ADR-0005 (team orchestration; "Known limitation: the team is blind to the code"), ADR-0004 (task profiles), SP4 scratchpad (richer tools)

## Problem

ADR-0005 inverted team orchestration so the harness drives spawn/collect/broadcast and the coordinator has no tool loop. Subagents run single-turn and toolless (`complete(system, msgs, &[])`). Nothing feeds the code under review to any model, so live validation against a real model showed `compose` refusing ("I need the diff / repository structure / conventions files…") and no grounded review is possible. The orchestration *mechanics* are validated (deterministic `team_mode_test`; live plumbing reaches the model call); what is missing is the team's access to the code.

## Constraint: keep the harness generic

gantry's thesis (ADR-0004/0005): the harness provides domain-agnostic *mechanisms*; profiles supply domain-specific *data* (prompts + tool allowlist). Therefore the harness must **not** learn about "diffs" — that is a review concept. Injecting `git diff` into the team harness would re-specialize the generic orchestration for one consumer (review), breaking reuse for sleuthly (incident → logs/`shell`) and refine-skill (→ `read_file` on skill files).

## Decision

Context access uses the **existing generic tool mechanism**, with the toolset chosen by the **profile**. Subagents fetch their own context via profile-selected tools. The "diff" stops being special — it is simply `git_diff` (+ `read_file`) appearing in the *review profile's* toolset. This is SP4 ("richer tools") converging with the team-context need, and it vindicates the earlier registry cleanup (base tools were deliberately kept "plumbed for future subagent access"; `_registry` is already threaded into `spawn_subagent`).

## Design

### 1. Subagent tool loop (`src/tools/subagent.rs`) — core change
Replace the single-turn subagent call with a **bounded agent loop** (the same shape as single mode, scoped):
- Each turn: `complete(subagent_system, messages, registry.schemas())`.
- If the response has tool calls → dispatch each via the (already-threaded) registry, append tool results as messages, continue.
- If the response is text with no tool calls → that is the subagent's report for the round.
- Bound by the existing `SUBAGENT_MAX_TURNS`. The round/broadcast structure is unchanged (one *report* per round; possibly several *tool turns* within a round). Panic-catch (`subagent_failed`), cancellation checks, and metering are preserved.
- Rename the unused `_registry` parameter to `registry` (now used).

### 2. Profile toolset — reuse `tools` (default)
The subagent toolset is the profile's existing `tools` allowlist. `run_team` already builds the registry from `validated.tools`; the subagent loop exposes `registry.schemas()` (allowlist-filtered). The review profile already lists `read_file, list_files, find_files, git_diff, shell, skill_load`, so subagents get those with no new profile field. (A separate `subagent_tools` key can be added later if the coordinator and subagent toolsets ever need to differ — YAGNI for now.)

### 3. Compose stays diff-free (`profiles/review/compose.md`)
Rewrite so compose plans the team from the prompt + heuristics and never demands a diff:
- always include `correctness` + `spec-compliance` (scope `full`);
- one `<dir>-conventions` reviewer per top-level directory named or evident in the task; `contracts` if ≥2 directories;
- state explicitly that compose does **not** receive the diff — the spawned reviewers read the code themselves.

Compose remains a single structured call (no tools): cheap and deterministic; grounding lives in subagents. (The in-prompt fenced-JSON directive added in commit `7e6545e` stays.)

### 4. Scope is advisory (default)
A reviewer's `scope` (e.g. `api`) is carried in its prompt ("restrict your review to `api/`"); the reviewer uses `git_diff -- api` / `read_file` accordingly. No hard path-jail enforcement now — tool-level path restriction is deferred to SP2 (isolation).

### 5. Subagent prompt (`profiles/review/subagent.md`)
Instruct the reviewer to actively use its tools (inspect the diff/files within its scope) before reporting findings. Minor edit.

### 6. Eval fixture `003` → real git repo (`evals/src/runner.rs`)
`run_fixture` currently `git apply`s the patch into a non-git tmpdir, so there is no `git diff`. Change the setup to: copy `repo/` → `git init` → `git add -A && git commit` (base) → `git apply` the patch (left unstaged). Subagents' `git_diff` then shows the real changes. Remove `#[ignore]` from `team_fixture_003_runs_live` and assert a grounded review (exit ok + ≥1 finding).

## Data flow

`prompt → compose (structured, heuristic team) → spawn N subagents (profile toolset + scope) → each subagent: bounded tool loop (git_diff/read_file in its scope) → report → collect (barrier) → [digest broadcast → round 2] → unify (structured) → JSON fence`.

## Error handling

- Subagent tool-loop panics are caught (`catch_unwind` → `subagent_failed`), unchanged.
- Tool dispatch errors return an error tool-result; the loop continues (same as single mode).
- Budget/timeout/cancel honored via the meter + cancellation token checked each turn.
- `team_collapse` when every subagent fails to report a round (unchanged).

## Testing

- **Unit** (`tests/subagent_test.rs`): a stub provider that emits a tool call then a text report — assert the tool was dispatched through the registry and the report returned. Existing mechanics/lifecycle tests (`team_mode_test`) stay green.
- **Live** (`evals/tests/runner_test.rs::team_fixture_003_runs_live`): re-enabled on the git-repo fixture; asserts a grounded review (exit ok + ≥1 finding) via `run_all` assertions.

## Genericity check

The harness gains zero review-specific logic — only "subagents run a tool loop with the profile's tools." All review specifics stay in the review profile (compose.md heuristics, subagent.md, the toolset). A future incident profile swaps the toolset/prompts; the orchestration is untouched. ✓

## Scope / YAGNI (chosen defaults)

- Subagent toolset = reuse `tools` (no separate `subagent_tools` field).
- Scope = advisory via prompt (no hard tool path-jail).
- Compose = tool-less heuristic planning (no compose toolset).

## Out of scope (future)

- Hard per-scope tool path restriction / sandboxing → SP2 (isolation).
- Separate coordinator vs subagent toolsets → add `subagent_tools` if a need appears.
- Giving compose a light toolset (e.g. `git diff --stat`) if heuristic planning proves insufficient.
- Token-budget-aware diff chunking for very large reviews.
