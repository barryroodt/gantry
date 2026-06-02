# Iterate-and-mutate profile (example)

An **illustrative example** profile for the gantry harness — the
"assess → improve, repeat" archetype (e.g. skill or code refinement). It exists
to show how gantry's binary-first knobs **compose**; it is not a consumer
adapter. Real consumers add their own profile directory (persona + skills) and
parse the NDJSON event stream in their own code (ADR-0010).

## Files
- `profile.toml` — manifest: loop mode, tool allowlist (read + mutate), isolation, iteration cap.
- `system.md` — the iterate-and-mutate persona (assess → one focused edit → `decide_stop`).

## What it composes
- **Loop mode (SP3):** `mode = "loop"` + `max_iterations = 5`. Each iteration is a
  fresh pass seeded with a compact summary of the previous attempt; the agent ends
  early by calling the `decide_stop` control tool.
- **Mutation + isolation (SP2):** `write_file`/`edit_file` in the tool allowlist,
  `isolate = true`. Edits land in a copy-on-write overlay; the original workdir is
  untouched and a terminal `changes` event lists what changed.
- **Compression (SP5):** verbose tool output is capped/deduped on the way to the
  model automatically (no knob).

## Usage

```
gantry --profile profiles/refine \
  --model <provider/model> --workdir <dir> --prompt-file <task.md> \
  --max-tokens <n> --timeout-ms <n>
```

Override the cap per run with `--max-iterations <n>`; add your own persona with
`--system-file`, skills with `--inject-skill`, or a tighter toolset with `--tool`.

## Consuming the output
Map gantry's NDJSON to your own telemetry: `iteration_start` / `iteration_end`
mark the loop; `tool_result` carries `bytes`/`bytes_out`; `changes` lists the
isolated edits; the terminal `result` carries the exit + token totals. gantry adds
no consumer-specific output format.

## Notes
- Explicit flags override profile values; `--mode` is optional when the profile
  supplies `mode`.
- `decide_stop` is granted automatically in loop mode — it is not requestable via
  `--tool` and never appears in the default tool surface.
