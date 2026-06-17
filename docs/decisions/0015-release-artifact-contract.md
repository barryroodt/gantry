# ADR-0015: Release pipeline + pinned artifact contract

**Status:** Accepted
**Date:** 2026-06-17

## Context

Gantry is consumed as a subprocess. The first downstream consumer (wrily) pins a
specific gantry build in `.gantry-version` and extracts the binary in its
Dockerfile, then generates its NDJSON test fixtures against that exact binary. A
consumer that pulls a prebuilt artifact needs two guarantees that must not drift
silently: **which artifacts exist** and **the exact layout inside them**.

Before this change gantry had no tagged releases — consumers would have to build
from source. The dependency tree pulls `aws-lc-sys` and `ring` (C-heavy crypto),
which are painful to cross-compile but build cleanly on a native runner.

## Decision

Add a tag-driven release workflow (`.github/workflows/release.yml`, trigger
`push: tags: ["v*"]`) that publishes a GitHub release with:

- `gantry-<tag>-x86_64-unknown-linux-gnu.tar.gz`
- `gantry-<tag>-aarch64-unknown-linux-gnu.tar.gz`
- `SHA256SUMS` (coreutils `sha256sum` format, bare filenames, covering both)

**Pinned layout contract (load-bearing):** each tarball contains exactly one
member, `gantry`, at the archive root — flat, no directory prefix. The build
packs with `tar -czf <out> -C <distdir> gantry` (never `./gantry`, which would
store the member as `./gantry` and break the consumer's `tar -xzf … gantry`
extraction). The build job asserts `tar -tzf "$asset" == "gantry"` before upload
so a layout regression fails the release instead of shipping. A consumer
verifies with `grep " <asset>$" SHA256SUMS | sha256sum -c -`.

This contract is documented in the README (`## Releases`) and treated as part of
the stable surface: it will not change without a MAJOR version signal.

## Options considered

- **A — Native per-arch runners (chosen).** Matrix builds `x86_64` on
  `ubuntu-latest` and `aarch64` on `ubuntu-24.04-arm` (free for public repos).
  No cross toolchain; `aws-lc-sys`/`ring` build natively on each arch. A separate
  `release` job downloads both tarballs, computes `SHA256SUMS`, and publishes via
  the preinstalled `gh` CLI (no extra third-party action to pin).
- **B — Cross-compile aarch64 from x86_64** (`gcc-aarch64-linux-gnu` + linker/CC
  env, or `cross`). Single runner class, but `aws-lc-sys`/`ring` cross builds are
  fragile and unverifiable until the tag is pushed. Rejected.
- **C — `*-musl` static targets.** Simpler distribution, but the contract
  specifies `-gnu` (matches the consumer's glibc base image). Rejected.

## Consequences

- Releasing is `git tag vX.Y.Z && git push --tags`; the workflow does the rest.
  The package version in `Cargo.toml` should match the tag (currently `0.1.0`).
- Toolchain is read from `mise.toml` (same single-source-of-truth as CI), so the
  release binary can't drift from the gate.
- The release workflow's correctness is only fully exercised on a real tag push
  (GitHub-only steps); the layout assertion in the build job is the in-pipeline
  guard. macOS/Windows artifacts are out of scope (the consumer targets Linux).
