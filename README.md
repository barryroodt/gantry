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
- **Pluggable providers** — Anthropic, OpenAI, Gemini, and a generic OpenAI-compatible `local` provider (oMLX, Ollama, vLLM, LM Studio) via [rig](https://github.com/0xPlaygrounds/rig); each with a base-URL override for proxies and self-hosting.
- **Budget + timeout + signals** — a hard token budget and wall-clock timeout bound every run; SIGINT/SIGTERM shut down cleanly with a deterministic exit code.

## Install / Build

Requires Rust **1.96** (pinned via [mise](https://mise.jdx.dev/)). The tool layer pulls a few crates from [oh-my-pi](https://github.com/can1357/oh-my-pi) as pinned git dependencies, so the first build fetches and compiles them (this also means the crate is **not publishable to crates.io** as-is).

```bash
mise install                       # provisions the pinned Rust toolchain
mise exec -- cargo build --release # binary at target/release/gantry
```

Without mise, use any Rust ≥ 1.96 toolchain and plain `cargo build --release`.

## Releases

Pushing a `v*` tag publishes a GitHub release with prebuilt Linux binaries, each built **natively** on its architecture:

- `gantry-<tag>-x86_64-unknown-linux-gnu.tar.gz`
- `gantry-<tag>-aarch64-unknown-linux-gnu.tar.gz`
- `SHA256SUMS` — coreutils `sha256sum` format, covering both tarballs

The `*-unknown-linux-gnu` binaries are built on **Ubuntu 22.04** and require **glibc ≥ 2.35** — they run on Debian 12 (bookworm, including `node:22-slim`), Ubuntu 22.04+, and newer. The build host's glibc sets this floor, so the release workflow runs a **portability smoke** (the freshly built binary must emit its `start` event under `debian:bookworm-slim`, glibc 2.36) before publishing — the floor can't silently rise.

**Pinned artifact contract** (consumers depend on this; it will not change without a MAJOR bump): each tarball contains exactly one member, `gantry`, at the archive root — flat, no directory prefix. Verify and extract a pinned binary with:

```bash
# from a directory holding the downloaded tarball + SHA256SUMS
asset=gantry-v0.1.0-x86_64-unknown-linux-gnu.tar.gz
grep " $asset$" SHA256SUMS | sha256sum -c -
tar -xzf "$asset" -C /usr/local/bin gantry
```

The workflow packs with `tar -czf <out> -C <distdir> gantry`, storing the member flat as `gantry` (never `./gantry`) so `tar -xzf … gantry` stays stable. See [`.github/workflows/release.yml`](.github/workflows/release.yml) and [ADR-0015](docs/decisions/0015-release-artifact-contract.md).

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
| `--model <provider/model>` | yes | Provider slug + model id, e.g. `anthropic/claude-opus-4-8`, `openai/gpt-4o`, `google/gemini-2.5-pro`, `openrouter/anthropic/claude-3.5-sonnet`, `local/qwen3-coder-next`. |
| `--workdir <dir>` | yes | Working directory; all file tools are confined to it. |
| `--prompt-file <path>` | yes | The user prompt, read from a file. |
| `--max-tokens <n>` | yes | Hard token budget (formula: `input + output + cache_write`; cache_read excluded). The run stops with exit `budget` if the running total meets or exceeds this value. |
| `--timeout-ms <n>` | yes | Wall-clock timeout; the run stops with exit `timeout`. |
| `--mode <single\|team\|loop>` | no¹ | Execution mode. ¹Optional when a `--profile` supplies one. |
| `--profile <dir>` | no | Load a profile directory (see [Profiles](#profiles)). Explicit flags override profile values. |
| `--system-file <path>` | no | System prompt (agent / coordinator persona). Defaults to a neutral prompt. |
| `--subagent-system-file <path>` | no | System prompt for spawned subagents (team mode). |
| `--compose-file <path>` | no | Compose-phase system prompt (team mode). Overrides the profile's `compose` file. Silently ignored outside team mode. |
| `--unify-file <path>` | no | Unify-phase system prompt (team mode). Overrides the profile's `unify` file. Silently ignored outside team mode. |
| `--tool <name>` | no | Restrict the exposed tools (repeatable). Default: all base tools for the mode. |
| `--inject-skill <name>` | no | Inject `<skills-dir>/<name>/SKILL.md` into the system prompt (repeatable). Skills dir defaults to `<workdir>/.claude/skills`; override with `--skills-dir`. |
| `--skills-dir <dir>` | no | Root directory for skill resolution (both `--inject-skill` and the `skill_load` tool). Defaults to `<workdir>/.claude/skills`. Trusted config — not workdir-confined. |
| `--isolate` | no | Run against a copy-on-write shadow of the workdir (see [Isolation](#isolation)). |
| `--max-iterations <n>` | no | Iteration cap for `--mode loop` (default 5, min 1). |
| `--base-url <URL>` | no | OpenAI-compatible endpoint for the `local` provider (e.g. `http://localhost:8000/v1`). Overrides `GANTRY_LOCAL_BASE_URL`. Rejected for non-local providers. |
| `--context-limit <TOKENS>` | no | Opt-in transcript compaction threshold. When the previous turn's total context occupancy (uncached `input_tokens` + `cache_read` + `cache_write`) exceeds this value, tool results older than the last 3 turns (>512 B, not already a stub) are folded into `retrieve`-able stubs to free context-window headroom. Off by default; single and loop modes only (no effect in team mode). |

### Providers

| Provider | Slug prefix | API key env | Base-URL override |
|---|---|---|---|
| Anthropic | `anthropic/` | `ANTHROPIC_API_KEY` | `ANTHROPIC_API_BASE` |
| OpenAI | `openai/` | `OPENAI_API_KEY` | `OPENAI_BASE_URL` |
| Gemini | `google/` | `GEMINI_API_KEY` | `GEMINI_API_BASE` |
| OpenRouter | `openrouter/` | `OPENROUTER_API_KEY` | `OPENROUTER_BASE_URL` |
| Local (OpenAI-compatible) | `local/` | `GANTRY_LOCAL_API_KEY` (optional) | `--base-url` / `GANTRY_LOCAL_BASE_URL` |

Everything after the first `/` in `--model` is the bare model id forwarded to that provider. The base-URL overrides let you point at a proxy, gateway, or self-hosted endpoint.

#### Self-hosted / local models (oMLX)

The `local` provider targets any OpenAI-compatible server — [oMLX](https://omlx.ai/) (native macOS MLX, the headline use case), Ollama, vLLM, LM Studio, llama.cpp. The model id after `local/` is whatever the server serves. Endpoint resolution is `--base-url` → `GANTRY_LOCAL_BASE_URL` → `http://localhost:8000/v1` (oMLX's default; the `/v1` suffix is required). No API key is needed — set `GANTRY_LOCAL_API_KEY` only if your server enforces auth.

```bash
gantry --mode single \
  --model local/qwen3-coder-next \
  --base-url http://localhost:8000/v1 \
  --workdir . --prompt-file /tmp/task.md \
  --max-tokens 200000 --timeout-ms 600000
```

#### OpenRouter

[OpenRouter](https://openrouter.ai/) is an OpenAI-compatible gateway fronting many vendors behind one key. Set `OPENROUTER_API_KEY` and prefix the model with `openrouter/`. OpenRouter ids are vendor-qualified (`anthropic/claude-3.5-sonnet`, `meta-llama/llama-3.1-70b-instruct`); since only the first `/` splits provider from model, the rest is forwarded verbatim.

```bash
export OPENROUTER_API_KEY=sk-or-...
# Optional attribution (OpenRouter leaderboard ranking only; not required):
export OPENROUTER_HTTP_REFERER=https://your.app
export OPENROUTER_X_TITLE="your app"

gantry --mode single \
  --model openrouter/anthropic/claude-3.5-sonnet \
  --workdir . --prompt-file /tmp/task.md \
  --max-tokens 200000 --timeout-ms 600000
```

`OPENROUTER_BASE_URL` overrides the default `https://openrouter.ai/api/v1` (e.g. to route through a compatible proxy).

**Routers.** OpenRouter's [routers](https://openrouter.ai/docs/guides/routing) are addressed like models, so they ride the same path — the slug is `openrouter/openrouter/<router>` (e.g. `openrouter/openrouter/free`, `openrouter/openrouter/auto`, `openrouter/openrouter/fusion`). Two caveats for gantry's tool-driven modes (`single`/`team`/`loop` with tools): the router must support **function calling** — `openrouter/free` and `openrouter/auto` do; `openrouter/fusion` does **not** (it runs its own multi-model deliberation pipeline) and is paid, so it fits a tool-less single-shot prompt rather than the agentic loop. Free models — including the `openrouter/free` router — require enabling the data-training toggle in your [OpenRouter privacy settings](https://openrouter.ai/settings/privacy) and are rate-limited (~20 req/min, ~200 req/day).

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
| `skill_load` | base | Load a skill from the skills root (`<workdir>/.claude/skills` by default; override with `--skills-dir`). |
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
| `start` | run begins | `schema_version`, `model`, `provider`, `mode`, `workdir` |
| `skill_loaded` | a skill is injected | `name`, `bytes` |
| `agent_turn` | each model call | `role`, `turn`, `input_tokens`, `output_tokens`, `cache_read`, `cache_write`, `duration_ms` |
| `tool_call` | a tool is invoked | `role`, `turn`, `tool`, `args` |
| `tool_result` | a tool returns | `tool`, `bytes`, `bytes_out`, `truncated`, `handle?`, `error?` |
| `assistant_text` | model text output | `role`, `text` |
| `subagent_spawn` | team subagent starts | `name`, `scope` |
| `subagent_done` | team subagent finishes | `name`, `turns`, `input_tokens`, `output_tokens`, `cache_read`, `cache_write`, `duration_ms` |
| `subagent_failed` | team subagent errors | `name`, `reason` — vocabulary: `budget` (slice exceeded), `panic` (subagent task panicked), otherwise free-form provider error text |
| `iteration_start` / `iteration_end` | loop iteration boundaries | `iteration`, `stopped` |
| `history_compacted` | transcript compaction ran | `role`, `turn`, `results_elided`, `input_tokens` |
| `budget_exceeded` | global token cap tripped (emitted at most once per run; `total = input + output + cache_write`) | `limit`, `total` |
| `changes` | `--isolate` teardown | `files: [{path, kind}]` |
| `error` | recoverable/terminal error | `kind` (`config`/`provider`/`team_collapse`/`internal`), `message`, `recoverable`, `retry_after_ms?` |
| `result` | terminal (always last) | `exit`, `total_input`, `total_output`, `total_cache_read`, `total_cache_write`, `duration_ms` |

Example:

```json
{"event":"start","ts":1730000000000,"schema_version":"1.1","model":"anthropic/claude-opus-4-8","provider":"anthropic","mode":"single","workdir":"/repo"}
{"event":"tool_call","ts":1730000000123,"role":"single","turn":0,"tool":"read_file","args":"{\"path\":\"src/main.rs\"}"}
{"event":"tool_result","ts":1730000000130,"role":"single","turn":0,"tool":"read_file","bytes":4096,"bytes_out":4096,"truncated":false}
{"event":"result","ts":1730000004567,"exit":"ok","total_input":12000,"total_output":800,"total_cache_read":0,"total_cache_write":0,"duration_ms":4567}
```

### Role semantics

The `role` field on `agent_turn`, `tool_call`, `tool_result`, and `assistant_text` events identifies the agent that produced the event:

| Mode | `role` value |
|---|---|
| Single mode | `"single"` |
| Team mode — coordinator turns | `"coordinator"` |
| Team mode — per-subagent events | the subagent's `name` |

Per-model-call token accounting flows through `agent_turn` for `single` and `coordinator` roles. Subagent aggregate token totals appear in `subagent_done` instead — subagents do **not** emit `agent_turn` events.

### Exit codes

The `result.exit` value maps to the process exit code:

| `exit` | code | Meaning |
|---|---|---|
| `ok` | 0 | Completed normally. |
| `error` | 1 | A run error (provider failure, panic, team collapse). |
| `budget` | 2 | Token budget exceeded. |
| `timeout` | 3 | Wall-clock timeout (or SIGINT/SIGTERM). |
| `config` | 4 | Invalid configuration (bad flags, missing prompt, unknown provider). |
| `rate_limited` | 5 | Provider rate-limited after retries — back off (honor `error.retry_after_ms` when present) and retry the run. |

> `--help` and `--version` exit `config` (4), **not** 0 — clap's help/version paths are captured as a config error because gantry is a non-interactive NDJSON sidecar with no human help path. Don't write a `--help`/`--version` liveness probe; invoke with real args and assert the `start` event instead.

### Contract versioning

The first event of any successfully-started run is `start`, carrying `schema_version` — version-guard your parser on it. CLI-validation failures emit `error{kind:"config"}` + `result` with **no** `start`, so exit-4 runs carry no version. The version is semver: **MAJOR** = a field/event is removed or renamed, or semantics change; **MINOR** = additive field, event, or exit code. Current: `1.1`.

On terminal failures, `error.recoverable` tells an embedder whether retrying the run may help (provider rate-limit or transient failure) — `false` means give up or fix the input (`config`, `team_collapse`, `internal`, fatal provider errors). `retry_after_ms` is present only when the provider supplied a back-off hint.

## Architecture

A thin binary (`src/main.rs`) parses + validates the CLI, emits `start`, runs the selected mode, and emits exactly one terminal `result` (even on panic). The library is organized as:

- `cli` — flag parsing, validation, provider-slug parsing, profile merge.
- `mode` — `bootstrap` (shared run scaffolding) + `single`, `team`, `loop_mode`, `isolation`.
- `tools` — the registry (visibility/dispatch), each tool, the workdir guard, and output compression.
- `provider` — the `ProviderAdapter` trait + Anthropic/OpenAI/Gemini adapters over rig; the `openrouter` and `local` providers reuse the OpenAI-compatible engine — OpenRouter on the fixed gateway base + key (plus optional attribution headers), `local` on a configurable base URL with an optional key.
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

## Benchmarks

[`bench/`](bench/) benchmarks gantry against other agent harnesses (Claude Code, Pi) on identical tasks through a recording reverse proxy, so tokens, cost, model calls, and tool calls are measured uniformly from the wire — never from harness self-reporting — and output quality is gated by programmatic checks plus a blinded LLM judge. See [`bench/README.md`](bench/README.md) for the fairness protocol and how to run it.

## License

Licensed under the [Apache License, Version 2.0](LICENSE). See [NOTICE](NOTICE) for attributions.
