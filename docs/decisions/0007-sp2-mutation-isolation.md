# ADR-0007: SP2 — mutation tools, path-jail hardening, optional COW isolation

**Status:** Accepted
**Date:** 2026-06-02
**Relates to:** SP2 design spec (Solo scratchpad 8) and the capability-first impl spec/plan (scratchpads 20/22).

## Context

SP2 was specified (scratchpad 8, 2026-05-31) as four thrusts: (1) extract a
`gantry-core` library, (2) convert the tool registry to a `Tool` trait, (3) add
gated mutating tools, (4) optional workspace isolation. That spec predated SP4,
which changed the landscape: `gantry` is already a lib crate with a thin
`main.rs`; `shell` is now real pi-shell bash (`cwd=workdir`, program-allowlisted);
`ast_edit` (opt-in structural rewrite) already established the
`OPTIN_TOOL_NAMES` default-out mutation pattern; and the oh-my-pi git-dep
sourcing is proven.

## Decision

Execute SP2 **capability-first** and defer the architectural refactors:

1. **`gantry-core` extraction — no-op.** The lib (`src/lib.rs`) + thin `main.rs`
   already let a consumer depend on the harness as a library. No rename/split.
2. **`Tool` trait — deferred.** No named consumer (wrily/sleuthly/refine) needs
   to register a *custom Rust tool* yet; they need built-ins + mutation +
   isolation. The name→fn match registry stays. Revisit in a later SP when a
   custom-tool consumer is real.
3. **`exec` tool — dropped.** SP4's `shell` is already real bash; "read-only" is
   the `shell_allow` program allowlist. A separate `exec` is redundant.
4. **Mutating tools added (opt-in, default-out via `OPTIN_TOOL_NAMES`):**
   - `write_file{path, content}` — create/overwrite within the workdir.
   - `edit_file{path, search, replace, expected_count?}` — literal,
     occurrence-count-guarded search/replace (`expected_count` defaults to 1, so
     a unique match is required unless the caller states the count).
5. **Path-jail hardening.** Added `resolve_workdir_path_for_create` (the existing
   `resolve_workdir_path` canonicalizes the joined path and so requires
   existence — unusable for creating a new file). It lexically resolves `.`/`..`
   with no FS access, rejects escapes, and then resolves symlinks on the real
   target location — the target itself when it exists (catching a symlinked or
   broken-symlink leaf), else the nearest existing ancestor (catching a
   symlinked parent). Audit finding: `ast_grep`/`ast_edit` are **already
   confined** — `pi_ast::ops::collect_matched_files` roots its walk at the
   workdir and matches globs against relative paths, so an escaping glob matches
   nothing; locked with a confinement test on each.
6. **Optional COW isolation (`--isolate`, also a profile `isolate` knob).**
   `mode::run` wraps the run in a `pi-iso` overlay of the workdir: it resolves a
   host backend (macOS APFS `clonefile`, else `Rcopy`), repoints the tool workdir
   at the overlay, captures a diff at teardown, and emits a terminal `changes`
   event (`FileChangeRec{path, kind}`). The original workdir is untouched.

## Security model

- **Mutating, structured-path tools** (`write_file`/`edit_file`) are
  workdir-confined by the guard above and are default-out (opt-in per profile or
  `--tool`).
- **bash (`shell`) is NOT path-jailable.** An allowlisted program can still read
  paths outside the workdir; exotic dispatch (`eval`, dynamic `$VAR` program
  names) is denied conservatively, not resolved. Its containment is the program
  allowlist + `cwd=workdir`.
- **Isolation contains WRITES, not reads.** The COW overlay means mutations land
  in the clone and the real workdir is untouched; it does **not** sandbox the
  process, so an allowlisted shell command can still read outside. A hard
  process sandbox (namespaces/seccomp/sandbox-exec) is out of scope.
- **Isolation is fail-closed.** `--isolate` with no host-available backend is a
  `config` error and exits — it never silently runs un-isolated. Teardown
  (`stop` + clone removal) runs on every exit path, including panic unwind, via
  an `OverlayGuard` `Drop`.

## Consequences

- refine-skill's mutation phase can run on gantry (`--tool write_file edit_file
  ast_edit`, `--isolate`) with reviewable, contained edits.
- New runtime dep `pi-iso` (git-dep, rev `8b619a2`; light — no tree-sitter).
- The `changes` event is a new terminal-adjacent NDJSON event (capped at 200
  files; file list only, no unified patch in v1).
- Deferred: the `Tool` trait + `gantry-core` rename (architectural, no current
  consumer); a hard process sandbox for shell reads (SP-future if needed).
