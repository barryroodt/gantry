# ADR-0011: Generalize team mode off the review contract

**Status:** Accepted
**Date:** 2026-06-03
**Revises:** ADR-0004 (which framed team mode as "the review specialization"); complements ADR-0005 (team orchestration).

## Context

The SP1–SP6 generalization made gantry a task-agnostic, binary-first harness:
`single` and `loop` carry no domain assumptions, and review logic moved into the
`profiles/review` data. But **team mode kept a review-shaped contract hardcoded in
the harness**, contradicting that positioning:

- the compose plan was `ComposePlan { reviewers: Vec<ReviewerPlan> }` (`plan_schema`
  required a `"reviewers"` array);
- the unify output was forced through `findings_schema()` requiring
  `summary` / `verdict` / `findings`;
- strings like `"# Reviewer reports"` and `"# Cross-review digest"` leaked review
  vocabulary into the mechanism.

Crucially, `findings_schema()` was only **advisory** — the harness parses the
unify result as an opaque `serde_json::Value` and never validated it against the
schema. So the review shape added coupling without buying validation.

## Decision

Remove the review contract from the team harness; keep the mechanism, move the
*shape* to the profile.

- **Compose plan is generic by name now too:** `subagents: Vec<SubagentPlan>`
  (`name` / `role` / `scope` / `extra_context`); `plan_schema` keys `"subagents"`.
  The shape was always generic ("a list of subagents to spawn") — only the names
  leaked.
- **Unify output is profile-defined:** `findings_schema()` is replaced by a
  permissive `result_schema()` (`{"type": "object"}`). The harness imposes no
  shape; the profile's `unify.md` prompt defines the structured result. The review
  profile already specifies `summary`/`verdict`/`findings` inline in `unify.md`, so
  its output is unchanged.
- **Vocabulary neutralized:** `"# Subagent reports"`, `"# Round digest"`,
  `"compose produced no subagents"`.

`profiles/review/compose.md` now emits a `subagents` array (review planning rules
unchanged). What stays review-flavored is correct: it lives in `profiles/review`
(`compose.md` / `unify.md` / `subagent.md`), not in the harness.

## Consequences

- Team mode is now genuinely task-agnostic: any consumer can use it for a
  fan-out → collect → synthesize workflow by supplying their own
  `compose`/`subagent`/`unify` prompts; the unify output is whatever their
  `unify.md` specifies.
- No public contract change: the NDJSON events, CLI flags, and exit codes are
  unchanged. The review profile produces byte-identical output (the shape lives in
  its prompts).
- ADR-0004's "team mode = review specialization" no longer holds — `team` is a
  general structured-multi-agent mode; `profiles/review` is one consumer of it.

## Alternatives considered

- **Profile-supplied JSON schema for unify** (a new profile field pointing at a
  schema file): more machinery than needed, since the harness never validated the
  schema. Deferred — the permissive default + `unify.md` prompt is sufficient. Can
  be added later if a consumer wants harness-side validation.
