# Code-review profile

These files are the **code-review profile** for the gantry harness — the system
prompts wrily supplies so gantry runs as the reviewer it used to be by default.

As of SP1, gantry ships **no review strings in its source**; review behavior is
*configuration*. With no task flags, gantry is a generic agent, not a reviewer.

## Reproducing pre-SP1 review behavior

**Single mode**

```
gantry --mode single \
  --system-file docs/profiles/review/single-system.md \
  --inject-skill code-review --inject-skill confidence-rating \
  --model <provider/model> --workdir <dir> --prompt-file <rendered.md> \
  --max-tokens <n> --timeout-ms <n>
```

**Team mode**

```
gantry --mode team \
  --system-file docs/profiles/review/team-system.md \
  --subagent-system-file docs/profiles/review/reviewer-system.md \
  --inject-skill caveman-review --inject-skill agent-team-review \
  --inject-skill code-review --inject-skill confidence-rating \
  --model <provider/model> --workdir <dir> --prompt-file <rendered.md> \
  --max-tokens <n> --timeout-ms <n>
```

## Notes

- `--system-file` is the single/team-coordinator persona; `--subagent-system-file`
  is the team reviewer base persona. Both default to a neutral generic system
  prompt when omitted.
- Omitting `--tool` exposes all tools for the mode (the prior review tool set).
  Pass `--tool <name>` (repeatable) to restrict.
- Migration shape mirrors `--inject-skill`: wrily must pass these flags
  explicitly, exactly as it already passes `--inject-skill`.
- Team mode remains the code-review construct (its subagent scaffolding in
  `src/tools/subagent.rs` is review-shaped). Single mode is the general-purpose
  path used by other consumers (e.g. sleuthly, refine-skill).
