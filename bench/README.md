# gantry-bench

Benchmarks gantry against other agent harnesses — **Claude Code** (headless `claude -p`) and **Pi / oh-my-pi** (`omp -p`) — on identical tasks, with every efficiency metric measured by an interposed **recording reverse proxy** rather than trusted from harness self-reporting.

Each `(task × harness × rep)` cell runs in a fresh copy of a pinned third-party repo. The harness is pointed at an in-process proxy (`ANTHROPIC_BASE_URL` / `ANTHROPIC_API_BASE`) that forwards to the real API while teeing every `/v1/messages` exchange into a ledger: token usage (uncached / cache read / cache write / out), model calls, `tool_use` counts, request bytes, latency, per attempt. Output quality is gated by programmatic checks (`answer_contains`, `check_command`, diff rules) plus a blinded LLM judge — the judge never sees harness names and its calls bypass the proxy.

## Running

Live runs cost real API money and are gated:

```bash
GANTRY_BENCH_LIVE=1 ANTHROPIC_API_KEY=sk-... \
  mise exec -- cargo run -p gantry-bench -- \
  --model <dated-model-id> [--task <id>]... [--harness gantry|claude-code|pi]... [--reps N]
```

Keyless plumbing check (no API key, canned upstream):

```bash
mise exec -- cargo run -p gantry-bench --example mock_upstream &
GANTRY_BENCH_UPSTREAM=http://127.0.0.1:18099 mise exec -- cargo run -p gantry-bench -- --smoke
```

Artifacts land in `bench/results/<UTC timestamp>/` (override with `--out`): `raw/<task>-<harness>-r<rep>.json` written as each run finishes, plus `results.json` and `report.md` assembled at the end. One keyless smoke output is committed at [`results/sample/`](results/sample/).

## Fairness protocol

1. Same pinned dated model id, same verbatim prompt, same workspace SHA for every harness.
2. Hermetic environment: adapters `env_clear` + allowlist; fresh `CLAUDE_CONFIG_DIR` / `PI_CONFIG_DIR` per run — no user settings, memory, MCP servers, or CLAUDE.md bleed.
3. Nonessential side-traffic disabled where the harness has a switch (`CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`, `PI_NO_TITLE=1`); whatever still hits `/v1/messages` counts — it is the harness's real footprint. Non-API traffic is forwarded and reported as untracked.
4. Default shipped toolsets; mutation tasks enable each harness's own mutation path.
5. API-key auth everywhere (OAuth env vars are scrubbed — they bypass the base-URL override).
6. Harness versions + gantry git SHA recorded in every results file.
7. Upstream 429/5xx pass through untouched: retry behavior is harness behavior and is measured, not masked. The ledger records one entry per attempt.

## Reading the report

`report.md` shows, per task and per harness: success rate, then **median [min–max] over successful runs only** for cost (USD, from the pinned price table in `src/price.rs`), token splits, model calls, tool calls, and wall time. A harness gets no efficiency credit for failing cheaply. Cost is the headline equalizer: cache accounting differences make raw token counts misleading on their own. `n/a` = model missing from the price table; `—` = no successful runs. A transparency section reports untracked traffic and judge bookkeeping (judge usage is never mixed into benchmark metrics).

## Adding a task

Create `bench/tasks/<id>/` with:

- `task.toml` — `id`, `kind` (`explore` | `locate` | `mutate`), `timeout_ms`, `[workspace]` (`repo_url` + pinned `sha`), `[grading]` (`answer_contains` regexes, `check_command`, `diff_contains`, `diff_must_not_touch`, `judge_threshold`).
- `prompt.md` — the harness-neutral prompt (no tool names, no harness vocabulary).
- `rubric.md` — optional anchored 0–10 judge rubric; its presence enables judge grading.

Verify every grading claim against the pinned checkout before committing (run the check command; confirm expected substrings exist in a ground-truth answer). `suite_test.rs` smoke-loads all manifests.

## Price table policy

`src/price.rs` pins per-MTok prices (input / output / cache-write / cache-read) with the source URL cited inline. Unknown models render as `n/a`, never a guess. Updating the table is a **reviewed change**: it silently rescales cost comparisons across historical results.
