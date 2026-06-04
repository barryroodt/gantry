# Tech Debt

Tracked debt and deferred work for gantry. Each item records **where it lives**, the
**problem**, the **proposed fix**, and a **priority**. Tick the box and link the PR when
resolved.

Priorities: **P1** active friction / correctness-adjacent · **P2** worth doing soon ·
**P3** optional / do-when-needed.

## Code & design

- [ ] **P2 — Vestigial `template` field on subagents.** `SpawnSubagentArgs.template`
  (`src/tools/subagent.rs:47`, commented "template skill name") is never resolved to a
  skill — it is only echoed into the `SubagentSpawn` NDJSON event and into the subagent's
  first user turn (`Template: {template}`). `src/mode/team.rs` spawns with
  `template: r.role.clone()`, so `template == role` (redundant and misleading). **Fix:**
  either wire it to real template-skill resolution (let profiles set it), or remove the
  field, the `Template:` prompt line, and the event field. Decide intentionally — the
  field is part of the public NDJSON contract.

- [ ] **P3 — `structured_call` echoes the permissive unify schema as prose.**
  `src/mode/team.rs` injects "conforming to this schema: `{"type":"object"}`" into the
  unify directive. Since ADR-0011 made the unify schema permissive, that line is near-
  useless for unify (the profile's `unify.md` is the real shape). It is harmless and
  shared with compose (where it *is* correct), so **do not special-case it** — noted only.
  Revisit if the compose and unify directives ever need to diverge.

## Build & CI

- [ ] **P2 — Rust version duplicated, no single source of truth.** `1.96.0` is pinned in
  both `mise.toml` and `.github/workflows/ci.yml` (the latter added when CI switched off
  `jdx/mise-action` to `dtolnay/rust-toolchain` for reliable `rustfmt`/`clippy`). Bumping
  one and not the other re-introduces "green locally, red in CI." **Fix:** derive the CI
  toolchain from a single source — have CI read `mise.toml`, or add a `rust-toolchain.toml`
  that both mise and `dtolnay/rust-toolchain` honor.

- [ ] **P2 — Close the obsolete `mise-action` Dependabot PR.** PR #2 (bump
  `jdx/mise-action` 2→4) is moot once the team-mode PR (#5) lands its CI fix — CI no longer
  uses `mise-action`. Close #2.

## Dependencies

- [ ] **P2 — Triage open Dependabot PRs.** #1 `actions/checkout` 4→6, #3 `rig-core`
  0.37→0.38.1 (read the changelog — even a minor bump can shift provider behavior), #4
  `toml` 0.8→1.1 (major). Each must pass `gate` before merge.

- [ ] **P3 — `oh-my-pi` git-deps are rev-pinned.** Several crates pin specific oh-my-pi
  revisions (e.g. `pi-shell` @ `8b619a2`). This blocks crates.io publication (documented in
  the README) and drifts from upstream over time. **Fix:** establish a bump cadence or a
  vendoring strategy and track upstream releases.

## Configuration / flexibility

- [ ] **P3 — Output-compression caps are hardcoded.** `src/tools/compress.rs`:
  `HEAD_LINES=400`, `TAIL_LINES=100`, `DEDUP_RUN=5`, `NOISY_TOOLS=["shell"]`. Fine as
  defaults; expose via flag/profile only when a consumer actually needs different limits.

- [ ] **P3 — Optional harness-side unify validation (deferred, ADR-0011).** The unify
  output schema is permissive by design (the profile owns shape). If a consumer wants the
  harness to validate unify output, add an optional profile-supplied JSON-schema field.
  Do not build it speculatively.

## Security & repo ops

- [ ] **P2 — Rotate the old Cursor API key.** Never committed (only a placeholder
  `crsr_test_key` appeared, in a since-deleted test) but it was exposed locally. Rotate as
  housekeeping.

- [ ] **P3 — Tighten review gates with a co-maintainer.** Required approvals are 0
  (solo repo). Add a `CODEOWNERS` file and bump required approvals 0→1 once a co-maintainer
  exists. The `non_provider_patterns` / `validity_checks` secret-scanning extras are UI-only
  on personal repos — enable via the GitHub web UI if desired.

## Docs / licensing (optional)

- [ ] **P3 — Per-file SPDX headers.** Not added; `LICENSE` + the Cargo `license` field are
  load-bearing. Purely mechanical churn — add only if a downstream policy requires it.

- [ ] **P3 — Copyright holder.** Currently "The Gantry Authors" (OSS-idiomatic). One-line
  change if a specific name/entity is preferred.

---

*Not tracked here: net-new features (e.g. multi-phase loop tooling, a public Rust library
API). This file is for debt — suboptimal current state — not the roadmap.*
