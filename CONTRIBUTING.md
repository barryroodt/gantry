# Contributing to Gantry

Thanks for your interest. This is a small, deliberately-scoped Rust project; the bar is high on correctness and maintainability, but the workflow is simple.

## Prerequisites

- **Rust 1.96** — pinned via [mise](https://mise.jdx.dev/) (`mise install`). Any Rust ≥ 1.96 toolchain works otherwise.
- A network connection for the first build: the tool layer depends on a few pinned git crates from [oh-my-pi](https://github.com/can1357/oh-my-pi), which Cargo fetches and compiles once.

All commands below assume mise; drop `mise exec --` if you manage Rust yourself.

## The gate

Every change must pass the full gate before it lands. Run all four:

```bash
mise exec -- cargo build --workspace --all-targets
mise exec -- cargo test  --workspace
mise exec -- cargo fmt --check
mise exec -- cargo clippy --workspace --all-targets -- -D warnings
```

`cargo fmt` and `clippy -D warnings` are non-negotiable — formatting and lint cleanliness are part of the diff, not a follow-up.

### Running a subset

```bash
mise exec -- cargo test --lib tools::            # one module's unit tests
mise exec -- cargo test --test loop_e2e_test     # one integration suite
```

### Live tests

The test suite is deterministic by default. The one test that hits a real model — `team_fixture_003_runs_live` — is **opt-in** and skipped unless you ask for it (real-model output is non-deterministic):

```bash
GANTRY_LIVE_EVAL=1 ANTHROPIC_API_KEY=… \
  mise exec -- cargo test -p gantry-evals --test runner_test team_fixture_003_runs_live
```

## Conventions

- **Verification, not vibes.** New behavior ships with tests that exercise the behavior (unit or wiremock-driven binary e2e — see `tests/*_e2e_test.rs` for the pattern). No mocks of our own code.
- **Design decisions are ADRs.** Anything that changes architecture, a contract, or a notable trade-off gets a short ADR in [`docs/decisions/`](docs/decisions/) (`NNNN-title.md`), referenced from the relevant commit.
- **Keep modules focused.** Prefer extracting a helper/module over growing a file past ~1k lines or bolting special-case branches onto an unrelated flow.
- **The CLI + NDJSON event stream are the public contract.** Treat additions as additive where possible; the Rust library API is internal.
- **No secrets in commits.** `.env`, `.env.*`, and `.envrc` are gitignored — keep keys there (or in your shell), never in tracked files or test fixtures (use obvious placeholders like `test-...-key`).
- **Commit messages**: a `type(scope): summary` subject (e.g. `feat(tools):`, `refactor(mode):`, `test(evals):`) plus a body explaining the *why*.

## Submitting changes

1. Open an issue to discuss anything non-trivial before investing in it.
2. Branch, implement with tests, and get the full gate green.
3. Open a pull request describing the change and its rationale; link any ADR.

## License of contributions

By contributing, you agree that your contributions are licensed under the project's [Apache License 2.0](LICENSE) (inbound = outbound). No separate CLA is required.
