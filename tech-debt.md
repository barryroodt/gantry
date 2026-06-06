# Tech Debt

Tracked debt and deferred work for gantry. Each item records **where it lives**, the
**problem**, the **proposed fix**, and a **priority**. Tick the box and link the PR when
resolved.

Priorities: **P1** active friction / correctness-adjacent · **P2** worth doing soon ·
**P3** optional / do-when-needed.

## Code & design

- [x] **P2 — Vestigial `template` field on subagents.** Done — removed the field from
  `SpawnSubagentArgs`, the `SubagentSpawn` NDJSON event, the `Template:` line in the
  subagent's first user turn, and the team spawn site (it always equalled `role` and was
  never resolved to a skill). README event table updated; `subagent_spawn` no longer carries
  `template`.

- [ ] **P3 — `structured_call` echoes the permissive unify schema as prose.**
  `src/mode/team.rs` injects "conforming to this schema: `{"type":"object"}`" into the
  unify directive. Since ADR-0011 made the unify schema permissive, that line is near-
  useless for unify (the profile's `unify.md` is the real shape). It is harmless and
  shared with compose (where it *is* correct), so **do not special-case it** — noted only.
  Revisit if the compose and unify directives ever need to diverge.

## Build & CI

- [x] **P2 — Rust version duplicated, no single source of truth.** Done — CI now reads the
  Rust version + components from `mise.toml` (a `tomllib` step feeding `dtolnay/rust-toolchain`
  via `steps.rust.outputs`), so `mise.toml` is the single source and `1.96.0` is no longer
  duplicated in `.github/workflows/ci.yml`. (`Cargo.toml`'s `rust-version` MSRV is a separate
  floor, intentionally not coupled.)

- [x] **P2 — Close the obsolete `mise-action` Dependabot PR.** Done — Dependabot
  auto-closed #2 when the CI rewrite (#5) removed `jdx/mise-action`.

- [ ] **P3 — Enable the `cargo-deny` license check in CI.** The supply-chain gate (#7)
  runs `cargo deny check advisories bans sources`; `deny.toml`'s `[licenses]` allow-list is
  tuned and passes locally but is not yet gated, since it is only verified on macOS and may
  differ on the Linux runner. **Fix:** run `cargo deny check licenses` on the CI runner,
  reconcile the allow-list, then add `licenses` to the gated checks.

## Dependencies

- [x] **P2 — Triage open Dependabot PRs.** Done. #2 auto-closed; #3 (`rig-core`
  0.37→0.38.1) and #4 (`toml` 0.8→1.1) **closed as defective** — both were lock-only and
  manifest-inconsistent (the lock pinned a version the `Cargo.toml` caret excludes), a green
  no-op that the new `--locked` gate (#7) now fails outright. #1 (`actions/checkout` 4→6)
  superseded by #7 (pinned to the v6.0.3 SHA). **Re-adopt rig 0.38 / toml 1.x deliberately
  later:** bump the manifest + lock, run the gate, review the changelogs (rig 0.38 is a
  breaking-API bump).

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
