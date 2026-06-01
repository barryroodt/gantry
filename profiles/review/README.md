# Code-review profile

This directory is the **code-review profile** for the gantry harness — the
configuration that makes gantry run as the reviewer it used to be by default.
As of ADR-0004, gantry ships no review logic in its source; review is *data*.

## Files
- `profile.toml` — manifest: mode, system/subagent prompts, tool allowlist, inject-skills.
- `system.md` — coordinator/single review prompt (the JSON output contract).
- `subagent.md` — reviewer base prompt for spawned subagents.

## Usage

Team review (primary):

```
gantry --profile profiles/review \
  --model <provider/model> --workdir <dir> --prompt-file <rendered.md> \
  --max-tokens <n> --timeout-ms <n>
```

Single-mode review (same profile, `--mode` override):

```
gantry --profile profiles/review --mode single \
  --model <provider/model> --workdir <dir> --prompt-file <rendered.md> \
  --max-tokens <n> --timeout-ms <n>
```

## Notes
- `--profile` sets mode, the system + subagent prompts, the tool allowlist, and
  inject-skills from `profile.toml`. **Explicit flags override profile values**
  (`--mode`, `--system-file`, `--subagent-system-file`, `--tool`, `--inject-skill`).
- `--mode` is optional when a profile supplies `mode`.
- Profile tools are a cross-mode template: team tools (`spawn_subagent`,
  `collect_outputs`, `broadcast_summary`) are kept in team mode and dropped in
  single mode automatically. Explicit `--tool` flags are validated strictly.
- Other consumers (sleuthly, refine-skill) add their own profile directories and
  point `--profile` at them; gantry's source stays task-agnostic.
- Migration: wrily switches from individual flags to `--profile profiles/review`
  (or its own copy) — same posture as the earlier `--inject-skill` / `--system-file`
  migrations.
