# Gantry

**A standalone, headless LLM agent harness.** You drive it as a subprocess, configure it with CLI flags or a profile, and consume a structured **NDJSON event stream** on stdout. No SDK, no framework lock-in, no opinions about your domain — gantry is the *runtime*; your application owns the orchestration and parses the events.

```
your app ──spawn──▶ gantry --profile … --model … --workdir … --prompt-file …
         ◀─NDJSON── {"event":"start",…} {"event":"tool_call",…} … {"event":"result",…}
```

> Status: **0.1.0 / early.** The CLI + NDJSON contract are the stable surface; the Rust library API is internal and may change.

---

## Why

Most agent frameworks want to live *inside* your process and own your control flow. Gantry inverts that: it is a small, task-agnostic binary you run as a child process. Anything that can spawn a process and read lines of JSON can embed it — in any language. It is purpose-built for tight, efficient tool use and context management rather than breadth of integrations.

## Features

- **Three execution modes** — `single` (one agent to completion), `team` (a coordinator orchestrates parallel subagents over bounded rounds and unifies their outputs), and `loop` (iterate-until-done with a model-driven stop signal).
- **Curated tool set** — read, list, glob, `git diff`, structural AST search, and allowlisted `bash`, plus opt-in **mutation** tools (write / search-replace / AST rewrite) that are off by default.
- **Copy-on-write isolation** — `--isolate` runs the agent against a COW shadow of the workspace; the original is never touched and a terminal `changes` event reports exactly what was modified.
- **Output compression** — verbose tool output is capped (head+tail with a recovery hint) and deduped at the tool boundary to keep the model's context tight; savings are reported per call.
- **Profiles** — a directory of prompts + a `profile.toml` captures a reusable configuration (persona, toolset, mode, skills). Two examples ship in [`profiles/`](profiles/).
- **Skill injection** — inject Markdown "skills" from the workspace into the system prompt at startup.
- **Pluggable providers** — Anthropic, OpenAI, and Gemini via [rig](https://github.com/0xPlaygrounds/rig); each with a base-URL override for proxies and self-hosting.
- **Budget + timeout + signals** — a hard token budget and wall-clock timeout bound every run; SIGINT/SIGTERM shut down cleanly with a deterministic exit code.

## Install / Build

Requires Rust **1.96** (pinned via [mise](https://mise.jdx.dev/)). The tool layer pulls a few crates from [oh-my-pi](https://github.com/can1357/oh-my-pi) as pinned git dependencies, so the first build fetches and compiles them (this also means the crate is **not publishable to crates.io** as-is).

```bash
mise install                       # provisions the pinned Rust toolchain
mise exec -- cargo build --release # binary at target/release/gantry
```

Without mise, use any Rust ≥ 1.96 toolchain and plain `cargo build --release`.

## Quickstart

```bash
export ANTHROPIC_API_KEY=sk-ant-...          # or OPENAI_API_KEY / GEMINI_API_KEY

cat > /tmp/task.md <<'EOF'
Summarize what this project does, based on its source.
EOF

gantry \
  --mode single \
  --model anthropic/claude-opus-4-8 \
  --workdir . \
  --prompt-file /tmp/task.md \
  --max-tokens 200000 \
  --timeout-ms 120000
```

Each line of stdout is one JSON event; the final `result` event carries the exit status and token totals. The process exit code mirrors it (see [Exit codes](#exit-codes)).

## CLI reference

| Flag | Required | Description |
|---|---|---|
| `--model <provider/model>` | yes | Provider slug + model id, e.g. `anthropic/claude-opus-4-8`, `openai/gpt-4o`, `gemini/gemini-2.5-pro`. |
| `--workdir <dir>` | yes | Working directory; all file tools are confined to it. |
| `--prompt-file <path>` | yes | The user prompt, read from a file. |
| `--max-tokens <n>` | yes | Hard token budget; the run stops with exit `budget` if exceeded. |
| `--timeout-ms <n>` | yes | Wall-clock timeout; the run stops with exit `timeout`. |
| `--mode <single\|team\|loop>` | no¹ | Execution mode. ¹Optional when a `--profile` supplies one. |
| `--profile <dir>` | no | Load a profile directory (see [Profiles](#profiles)). Explicit flags override profile values. |
| `--system-file <path>` | no | System prompt (agent / coordinator persona). Defaults to a neutral prompt. |
| `--subagent-system-file <path>` | no | System prompt for spawned subagents (team mode). |
| `--tool <name>` | no | Restrict the exposed tools (repeatable). Default: all base tools for the mode. |
| `--inject-skill <name>` | no | Inject `<workdir>/.claude/skills/<name>/SKILL.md` into the system prompt (repeatable). |
| `--isolate` | no | Run against a copy-on-write shadow of the workdir (see [Isolation](#isolation)). |
| `--max-iterations <n>` | no | Iteration cap for `--mode loop` (default 5, min 1). |
| `--context-limit <TOKENS>` | no | Opt-in transcript compaction threshold. When the previous turn's `input_tokens` exceed this value, tool results older than the last 3 turns (>512 B, not already a stub) are folded into `retrieve`-able stubs to free context-window headroom. Off by default; single and loop modes only (no effect in team mode). |

### Providers

| Provider | Slug prefix | API key env | Base-URL override |
|---|---|---|---|
| Anthropic | `anthropic/` | `ANTHROPIC_API_KEY` | `ANTHROPIC_API_BASE` |
| OpenAI | `openai/` | `OPENAI_API_KEY` | `OPENAI_BASE_URL` |
| Gemini | `gemini/` | `GEMINI_API_KEY` | `GEMINI_API_BASE` |

Everything after the first `/` in `--model` is the bare model id forwarded to that provider. The base-URL overrides let you point at a proxy, gateway, or self-hosted endpoint.

## Modes

- **`single`** — one agent runs a bounded tool loop (model call → tool dispatch → repeat) until it stops calling tools or hits the turn cap. The general-purpose path.
- **`team`** — a coordinator composes a plan of subagents; each runs its own tool loop in parallel, their outputs are collected at a barrier, digested and broadcast between bounded rounds, then unified into a single structured result. The *output shape* is defined by the profile's `unify.md` (e.g. `profiles/review` produces a review report) — the harness itself is task-agnostic. (See [ADR-0005](docs/decisions/0005-team-orchestration.md), [ADR-0011](docs/decisions/0011-generalize-team-mode.md).)
- **`loop`** — runs the agent in bounded **iterations**, each a fresh pass seeded with a *compact summary* of the previous attempt (not a growing transcript). The agent ends early by calling the `decide_stop` control tool, otherwise it runs to `--max-iterations`. Ideal for "assess → improve, repeat" workflows. (See [ADR-0008](docs/decisions/0008-sp3-loop-mode.md).)

## Tools

Tools are workdir-confined. The exposed set is controlled by `--tool` / a profile `tools` list; with none specified, all **base** tools are available.

| Tool | Group | Description |
|---|---|---|
| `read_file` | base | Read a file (optional `outline` mode for a structural summary). |
| `list_files` | base | List files under a path (max depth 5). |
| `find_files` | base | Glob for files under the workdir. |
| `git_diff` | base | Run `git diff` in the workdir. |
| `ast_grep` | base | Structural (AST) code search → match locations. |
| `shell` | base | Run a `bash` command — **only allowlisted programs** may be invoked (`shell_allow`). |
| `skill_load` | base | Load a skill from `<workdir>/.claude/skills`. |
| `write_file` | opt-in | Create or overwrite a workdir file. **Mutating.** |
| `edit_file` | opt-in | Literal, occurrence-count-guarded search/replace. **Mutating.** |
| `ast_edit` | opt-in | Structural (AST) rewrite across files. **Mutating.** |
| `decide_stop` | control | Signal the iterative loop to stop (loop mode only; harness-granted). |
| `retrieve` | control | Recover content elided by output compression, by its `handle` (from a prior `tool_result`). Harness-granted; always available. |

**Allowlist model:** base tools are on by default and can be narrowed with `--tool`. **Opt-in (mutating) tools are off unless named explicitly** (`--tool write_file …` or a profile). **Control tools** (`decide_stop`) are granted by the mode, never requestable via `--tool`.

**Safety:** every path-taking tool is confined to the workdir (lexical `..` rejection + symlink-target/parent resolution). `shell` runs real `bash` and is **not** path-jailable — its containment is the program allowlist + `cwd=workdir` + (for writes) isolation. See [ADR-0007](docs/decisions/0007-sp2-mutation-isolation.md).

## Profiles

A profile is a directory with a `profile.toml` and the prompt files it references:

```toml
mode = "team"                      # single | team | loop  (optional)
system = "system.md"               # agent / coordinator persona
subagent_system = "subagent.md"    # team subagent persona
compose = "compose.md"             # team coordinator: compose phase
unify = "unify.md"                 # team coordinator: unify phase
tools = ["read_file", "git_diff"]  # tool allowlist
inject_skills = ["code-review"]    # skills to inject
shell_allow = ["git", "cat", "ls"] # programs the shell tool may run
isolate = true                     # default --isolate on
max_iterations = 5                 # loop cap
```

Explicit CLI flags override profile values. Two worked examples ship in-tree:

- [`profiles/review/`](profiles/review/) — multi-agent code review (`team` mode).
- [`profiles/refine/`](profiles/refine/) — iterate-and-mutate (`loop` + mutation + isolation).

## Isolation

`--isolate` (or a profile `isolate = true`) runs the agent against a **copy-on-write shadow** of the workdir via [pi-iso](https://github.com/can1357/oh-my-pi) (APFS `clonefile` on macOS, reflink/overlay or recursive copy elsewhere). All tool writes land in the overlay; the original workdir is untouched. At teardown gantry emits a terminal `changes` event listing every modified file. Isolation is **fail-closed**: if no backend is available the run errors rather than mutating the real workspace.

## Output compression

To keep context tight, every tool result passes through a compressor at the dispatch boundary before reaching the model: a recoverable **head+tail line cap** with a machine-readable hint (retained lines are byte-identical — never a heuristic drop), plus consecutive-line **dedup** for high-volume, non-faithful tools (`shell`). Faithful content (`read_file`, `git_diff`, …) is never deduped. Each `tool_result` reports `bytes` (raw) and `bytes_out` (emitted). The cap is now reversible: when output is capped, gantry stashes the byte-exact cap input under a content-addressed handle (surfaced in the recovery hint and as the additive `tool_result.handle` field), and the always-on `retrieve` control tool returns byte-faithful slices — by default the elided middle, or a 1-based inclusive `start`/`end` range, or a regex `pattern`. This reverses the compression cap only, not the tool's 256 KiB hard cap. See [ADR-0009](docs/decisions/0009-sp5-output-compression.md), [ADR-0012](docs/decisions/0012-sp7-reversible-retrieval.md).

## Event stream (NDJSON)

Gantry emits one JSON object per line to **stdout**, each tagged with an `"event"` field and a `ts` (epoch ms). Every run ends with exactly one `result`.

| `event` | When | Key fields |
|---|---|---|
| `start` | run begins | `model`, `provider`, `mode`, `workdir` |
| `skill_loaded` | a skill is injected | `name`, `bytes` |
| `agent_turn` | each model call | `role`, `turn`, `input_tokens`, `output_tokens`, `cache_read`, `cache_write` |
| `tool_call` | a tool is invoked | `role`, `turn`, `tool`, `args` |
| `tool_result` | a tool returns | `tool`, `bytes`, `bytes_out`, `truncated`, `handle?`, `error?` |
| `assistant_text` | model text output | `role`, `text` |
| `subagent_spawn` | team subagent starts | `name`, `scope` |
| `subagent_done` | team subagent finishes | `name`, `turns`, `input_tokens`, `output_tokens` |
| `subagent_failed` | team subagent errors | `name`, `reason` |
| `iteration_start` / `iteration_end` | loop iteration boundaries | `iteration`, `stopped` |
| `history_compacted` | transcript compaction ran | `role`, `turn`, `results_elided`, `input_tokens` |
| `budget_exceeded` | token budget tripped | `limit`, `total` |
| `changes` | `--isolate` teardown | `files: [{path, kind}]` |
| `error` | recoverable/terminal error | `kind` (`config`/`provider`/`team_collapse`/`internal`), `message` |
| `result` | terminal (always last) | `exit`, `total_input`, `total_output`, `total_cache_read`, `total_cache_write`, `duration_ms` |

Example:

```json
{"event":"start","ts":1730000000000,"model":"anthropic/claude-opus-4-8","provider":"anthropic","mode":"single","workdir":"/repo"}
{"event":"tool_call","ts":1730000000123,"role":"single","turn":0,"tool":"read_file","args":"{\"path\":\"src/main.rs\"}"}
{"event":"tool_result","ts":1730000000130,"role":"single","turn":0,"tool":"read_file","bytes":4096,"bytes_out":4096,"truncated":false}
{"event":"result","ts":1730000004567,"exit":"ok","total_input":12000,"total_output":800,"total_cache_read":0,"total_cache_write":0,"duration_ms":4567}
```

### Exit codes

The `result.exit` value maps to the process exit code:

| `exit` | code | Meaning |
|---|---|---|
| `ok` | 0 | Completed normally. |
| `error` | 1 | A run error (provider failure, panic, team collapse). |
| `budget` | 2 | Token budget exceeded. |
| `timeout` | 3 | Wall-clock timeout (or SIGINT/SIGTERM). |
| `config` | 4 | Invalid configuration (bad flags, missing prompt, unknown provider). |

## Architecture

A thin binary (`src/main.rs`) parses + validates the CLI, emits `start`, runs the selected mode, and emits exactly one terminal `result` (even on panic). The library is organized as:

- `cli` — flag parsing, validation, provider-slug parsing, profile merge.
- `mode` — `bootstrap` (shared run scaffolding) + `single`, `team`, `loop_mode`, `isolation`.
- `tools` — the registry (visibility/dispatch), each tool, the workdir guard, and output compression.
- `provider` — the `ProviderAdapter` trait + Anthropic/OpenAI/Gemini adapters over rig.
- `events` / `emitter` — the NDJSON event model and the stdout sink.
- `meter` — token accounting + budget enforcement; `cancel` — timeout + signal handling.
- `profile` — profile-directory loading; `skills` — skill resolution.

Design decisions are recorded as ADRs in [`docs/decisions/`](docs/decisions/).

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md). The gate is:

```bash
mise exec -- cargo build --workspace --all-targets
mise exec -- cargo test --workspace
mise exec -- cargo fmt --check
mise exec -- cargo clippy --workspace --all-targets -- -D warnings
```

## License

Licensed under the [Apache License, Version 2.0](LICENSE). See [NOTICE](NOTICE) for attributions.
