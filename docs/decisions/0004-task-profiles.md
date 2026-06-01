# ADR-0004: Task profiles and fully generic team mode

**Status:** Accepted
**Date:** 2026-05-31
**Deciders:** gantry SP1 follow-up (profiles)
**Supersedes:** ADR-0003 §3 (harness-built reviewer prompt); revises the SP1 decision to leave team mode review-shaped.

## Context

SP1 made **single mode** task-agnostic: the system prompt (`--system-file`) and tool set (`--tool`) are configuration, and the review prompts were extracted to loose files under `docs/profiles/review/` passed via individual flags. Two things remain:

1. **Team mode still has review logic in source.** `src/tools/subagent.rs::build_reviewer_system_prompt` hardcodes review scaffolding — reviewer "lanes", the "Reviewer Output Format" (verdicts, "Notes for Other Reviewers"), CI/cross-review-digest context, and a conventions override. ADR-0003 §3 specifies this harness-built review prompt. So `gantry` source still asserts code-review semantics.
2. **"Profile" is not first-class.** A task type today is an ad-hoc collection of four flags (`--system-file`, `--subagent-system-file`, `--tool`, `--inject-skill`) plus a directory of prompt files by convention. There is no named, shareable unit, which makes it awkward to add sleuthly (incident investigation) and refine-skill (skill refinement) as task types.

Goal: make **task type** a first-class **profile** abstraction — review as the first profile, with room for others — and remove the last review-specific code so both single and team modes are fully generic.

## Decision

### 1. Profiles are a first-class, data-only abstraction

A **profile** is a directory containing a manifest plus its prompt files. It declares the task-type configuration; it contains no run-specific values (model, workdir, prompt-file, budgets stay per-invocation flags).

```
profiles/review/
  profile.toml
  system.md          # single/coordinator system prompt
  subagent.md        # team subagent base prompt (team profiles only)
```

```toml
# profiles/review/profile.toml
mode = "team"                     # default mode for this profile; --mode overrides
system = "system.md"              # path (relative to profile dir)
subagent_system = "subagent.md"   # team only; omit for single-only profiles
tools = ["read_file", "list_files", "find_files", "git_diff", "shell",
         "skill_load", "spawn_subagent", "collect_outputs", "broadcast_summary"]
inject_skills = ["caveman-review", "agent-team-review", "code-review", "confidence-rating"]
```

- New flag `--profile <DIR>` loads the manifest and applies `mode` / `system` / `subagent_system` / `tools` / `inject_skills`.
- **Precedence:** explicit flags override profile values; profile values override defaults. (e.g. `--profile profiles/review --mode single` runs the review profile in single mode.)
- **gantry source contains no profile data.** Profiles are versioned files. gantry ships `profiles/review/` as the **reference profile**; other consumers point `--profile` at their own directories (in their repos, or contributed under `profiles/`). The harness only ships the *loader*.
- Manifest format: TOML (consistent with `Cargo.toml`/`mise.toml`; parsed via the `toml` crate + serde). The current `docs/profiles/review/*.md` files move to `profiles/review/` and gain a `profile.toml`.

### 2. Team mode becomes fully generic

`build_reviewer_system_prompt` → `build_subagent_system_prompt`, doing **only generic composition**:

```
{subagent_base}              # from the profile's `subagent_system`; carries ALL
                             # task framing (for review: output format, lanes, CI context)
{per-spawn fields}           # role / scope / extra_context, joined with neutral connectives
```

All review-specific text (Reviewer Output Format, lane language, "Notes for Other Reviewers", CI/cross-review-digest framing, conventions override) **moves out of `src/` into the review profile's `subagent.md`** (data). The harness keeps only the mechanism: spawn N subagents with `base + per-spawn context`, collect their outputs, broadcast a digest. This **supersedes ADR-0003 §3** — the harness no longer builds review-shaped prompts.

The coordinator system prompt (the JSON-fence output contract, the compose-team/round rules from ADR-0003 §2) likewise lives entirely in the review profile's `system.md` (already true after SP1; this ADR keeps it there).

### 3. Rename team tools/types to generic terms

To remove review semantics from the harness surface (not just behavior):

| Before | After |
|--------|-------|
| `spawn_reviewer` | `spawn_subagent` |
| `collect_findings` | `collect_outputs` |
| `broadcast_summary` | `broadcast_summary` (unchanged) |
| `ReviewerRoster` / `ReviewerHandle` | `SubagentRoster` / `SubagentHandle` |
| `SpawnReviewerArgs.diff_scope` | `scope` (generic; the profile's base interprets it) |

The `subagent_spawn` / `subagent_done` / `subagent_failed` NDJSON events are already generic and unchanged. The review profile's `system.md` is updated to call the renamed tools. This is a **wire/contract change**; wrily (adapted separately) updates its review profile prompts to match — same migration posture as `--inject-skill` and `--system-file`.

## Resolved decisions

Confirmed before implementation:

1. **One `review` profile** with `mode = "team"` default and a `system.md` usable in single mode via `--profile profiles/review --mode single` (no separate `review-single`).
2. **Do the generic rename now** (§3) for a clean fully-generic surface.
3. **Add the `--profile` flag** (raw flags remain supported and override profile values).

## Consequences

### Positive
- gantry source is fully task-agnostic — **no review strings in `src/`** (single and team). Review is data.
- Profiles are named, shareable, extensible; sleuthly and refine-skill become profiles (the mechanism SP6 builds on).
- One `--profile` flag replaces four flags for the common case; orchestrator wiring simplifies.

### Negative / costs
- Wire change: team tool names rename (§3); wrily's review profile prompts update to match.
- New manifest surface + a `toml` dependency.
- The review profile's `subagent.md` grows large (it absorbs the §3 scaffolding) — acceptable: it is versioned data, not code.

### Neutral
- Revises SP1's "team stays review-shaped" decision; supersedes ADR-0003 §3. ADR-0003's tool *semantics* (spawn/collect/broadcast lifecycle, JSON-fence contract) remain valid, now expressed through the review profile.

## Implementation note

This is a revision/extension of SP1 (not one of SP2–SP6). After this ADR is accepted, implement via a dedicated plan (`SP1.5 — profiles`): add the `toml` dep + manifest loader + `--profile`, generalize `subagent.rs`, rename the team tools/types, move `docs/profiles/review/` → `profiles/review/` + add `profile.toml` + fold the §3 scaffolding into `subagent.md`, and update tests + the migration note.
