# Grounded Team Review via Generic Subagent Tools — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let team-mode subagents fetch the code under review through the generic tool mechanism (profile-selected), so reviews are grounded — without teaching the harness anything review-specific.

**Architecture:** Subagents change from single-turn toolless calls to a bounded per-round tool loop using the profile's registry (`registry.schemas()` + `registry.dispatch`), mirroring single mode. Compose stays a tool-less structured call (rewritten to not demand a diff). The eval fixture becomes a real git repo so subagents' `git_diff` works.

**Tech Stack:** Rust, tokio, `mise exec -- cargo …` (rust 1.96.0). Spec: `docs/superpowers/specs/2026-05-31-team-review-context-design.md`.

---

## File structure

- `src/tools/subagent.rs` — subagent gains a bounded tool loop (core change); `_registry` → `registry`; add `ToolResult` import + `SUBAGENT_MAX_TOOL_TURNS`.
- `tests/subagent_test.rs` — new unit test: a subagent dispatches a tool and feeds the result into its report.
- `profiles/review/compose.md` — rewrite: heuristic team, no diff dependency.
- `profiles/review/subagent.md` — instruct the reviewer to use its tools.
- `evals/src/runner.rs` — `run_fixture` sets up a real git repo (init + commit + apply).
- `evals/tests/runner_test.rs` — remove `#[ignore]` from `team_fixture_003_runs_live`.

---

## Task 1: Subagent bounded tool loop

**Files:**
- Modify: `src/tools/subagent.rs` (imports near top; `spawn_subagent` closure body ~lines 115-217)
- Test: `tests/subagent_test.rs`

- [ ] **Step 1: Write the failing test**

Add to `tests/subagent_test.rs` (a provider that calls `read_file` on first turn, then reports the file content it received back):

```rust
struct ToolThenReportProvider;

#[async_trait]
impl ProviderAdapter for ToolThenReportProvider {
    fn provider(&self) -> Provider {
        Provider::OpenAi
    }
    fn model(&self) -> &str {
        "gpt-tooluse"
    }
    async fn complete(
        &self,
        _system: &str,
        messages: &[ChatMessage],
        _tools: &[ToolSchema],
    ) -> anyhow::Result<ProviderResponse> {
        // After a tool result arrives, report its content; otherwise call read_file.
        if let Some(ChatMessage::ToolResults(results)) = messages.last() {
            let seen = results.first().map(|r| r.content.clone()).unwrap_or_default();
            return Ok(ProviderResponse {
                text: format!("report: {seen}"),
                tool_calls: vec![],
                input_tokens: 1,
                output_tokens: 1,
                cache_read: 0,
                cache_write: 0,
            });
        }
        Ok(ProviderResponse {
            text: String::new(),
            tool_calls: vec![ToolCallRequest {
                id: "c1".into(),
                name: "read_file".into(),
                args_json: r#"{"path":"marker.txt"}"#.into(),
            }],
            input_tokens: 1,
            output_tokens: 1,
            cache_read: 0,
            cache_write: 0,
        })
    }
}

#[tokio::test]
async fn subagent_tool_loop_dispatches_and_reports_result() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("marker.txt"), "MARKER_CONTENT_42").unwrap();
    let roster = Arc::new(SubagentRoster::new());
    let provider: Arc<dyn ProviderAdapter> = Arc::new(ToolThenReportProvider);
    let registry = Arc::new(ToolRegistry::new(dir.path().to_path_buf(), vec![]));
    let meter = test_meter();

    spawn_reviewer(&roster, &provider, &registry, &meter, "alpha", "alpha").await;

    let round1 = roster
        .collect_outputs(CollectOutputsArgs { round: 1, timeout_ms: 0 }, &shared_token())
        .await
        .unwrap();
    assert!(
        round1.contains("MARKER_CONTENT_42"),
        "subagent did not dispatch read_file and feed its result into the report: {round1}"
    );
}
```

Add `ToolCallRequest` to the `gantry::provider` import line at the top of the test file (it already imports `ChatMessage, ProviderAdapter, ProviderResponse, ToolSchema`).

- [ ] **Step 2: Run test to verify it fails**

Run: `mise exec -- cargo test --test subagent_test subagent_tool_loop_dispatches_and_reports_result`
Expected: FAIL — the report is `report: ` (empty) because the current subagent calls `complete(.., &[])` and ignores tool calls, so `read_file` is never dispatched.

- [ ] **Step 3: Add imports + constant**

In `src/tools/subagent.rs`, add to the imports (the crate already uses `ChatMessage` via `super`/`crate::provider`):

```rust
use crate::provider::ToolResult;
```

Near the existing `SUBAGENT_MAX_TURNS` const, add:

```rust
/// Max model turns a subagent may take *within one round* to use tools before
/// it must produce its report. Bounds tool-loop cost per round.
const SUBAGENT_MAX_TOOL_TURNS: u32 = 8;
```

- [ ] **Step 4: Replace the subagent loop body with a per-round tool loop**

In `spawn_subagent`, change the parameter `_registry: Arc<ToolRegistry>` to `registry: Arc<ToolRegistry>`. Replace the spawned closure's body (currently the `let mut messages …` through the `SubagentDone` emit) with:

```rust
let join = tokio::spawn(async move {
    // First user turn carries the assignment; the "Role: " prefix stays so the
    // subagent's scope is explicit (and tests can detect a subagent turn).
    let mut messages: Vec<ChatMessage> = vec![ChatMessage::User(format!(
        "Role: {role}\nScope: {scope}\nTemplate: {template}",
        role = args.role,
        scope = args.scope,
        template = args.template,
    ))];
    let tools = registry.schemas();
    let mut round: u32 = 0;
    let mut input_tokens: u64 = 0;
    let mut output_tokens: u64 = 0;
    let mut stop = false;

    'rounds: loop {
        if cancel.is_cancelled() {
            break;
        }

        // Bounded tool loop within the round: call the model, dispatch any tool
        // calls, repeat until it returns a text-only report (or the cap is hit).
        let mut report = String::new();
        for _ in 0..SUBAGENT_MAX_TOOL_TURNS {
            // Catch panics so one subagent cannot take down the run (invariant #5).
            let attempt = std::panic::AssertUnwindSafe(provider.complete(
                &subagent_system,
                &messages,
                &tools,
            ))
            .catch_unwind()
            .await;
            let resp = match attempt {
                Ok(Ok(resp)) => resp,
                Ok(Err(err)) => {
                    let _ = GantryEvent::SubagentFailed {
                        ts: now_ms(),
                        name: subagent_name.clone(),
                        reason: err.to_string(),
                    }
                    .emit();
                    break 'rounds; // no report this round → collect sees channel close → error
                }
                Err(_panic) => {
                    let _ = GantryEvent::SubagentFailed {
                        ts: now_ms(),
                        name: subagent_name.clone(),
                        reason: "subagent task panicked".into(),
                    }
                    .emit();
                    break 'rounds;
                }
            };

            // Invariant #4: every response feeds the shared meter.
            input_tokens += resp.input_tokens;
            output_tokens += resp.output_tokens;
            if meter
                .add(resp.input_tokens, resp.output_tokens, resp.cache_read, resp.cache_write)
                .is_err()
            {
                stop = true;
            }

            if resp.tool_calls.is_empty() {
                report = resp.text.clone();
                if !resp.text.is_empty() {
                    let _ = GantryEvent::AssistantText {
                        ts: now_ms(),
                        role: subagent_name.clone(),
                        text: resp.text.clone(),
                    }
                    .emit();
                }
                messages.push(ChatMessage::Assistant {
                    text: resp.text,
                    tool_calls: vec![],
                });
                break;
            }

            // Dispatch the requested tools and feed results back.
            let mut tool_results = Vec::with_capacity(resp.tool_calls.len());
            for call in &resp.tool_calls {
                let out = registry
                    .dispatch(&subagent_name, round, &call.name, &call.args_json)
                    .await;
                tool_results.push(ToolResult {
                    id: call.id.clone(),
                    content: out.content,
                    is_error: false,
                });
            }
            messages.push(ChatMessage::Assistant {
                text: resp.text,
                tool_calls: resp.tool_calls,
            });
            messages.push(ChatMessage::ToolResults(tool_results));

            if stop || cancel.is_cancelled() {
                break;
            }
        }

        // Barrier: exactly one report per round (possibly empty if budget tripped).
        let _ = find_tx.send(report);

        if stop || meter.tripped() || cancel.is_cancelled() {
            break;
        }
        round += 1;
        match msg_rx.recv().await {
            Some(next) => messages.push(ChatMessage::User(next)),
            None => break,
        }
        if round >= SUBAGENT_MAX_TURNS {
            break;
        }
    }

    let _ = GantryEvent::SubagentDone {
        ts: now_ms(),
        name: subagent_name,
        turns: round,
        input_tokens,
        output_tokens,
    }
    .emit();
});
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `mise exec -- cargo test --test subagent_test subagent_tool_loop_dispatches_and_reports_result`
Expected: PASS — the report contains `MARKER_CONTENT_42`.

- [ ] **Step 6: Run the full subagent + team suites (no regressions)**

Run: `mise exec -- cargo test --test subagent_test --test team_mode_test`
Expected: all PASS (existing roster/lifecycle/mechanics tests still green; the toolless stub providers simply never emit tool calls, so they report on the first inner turn exactly as before).

- [ ] **Step 7: Commit**

```bash
git add src/tools/subagent.rs tests/subagent_test.rs
git commit -m "feat(team): subagents run a bounded per-round tool loop (generic context access)"
```

---

## Task 2: Compose plans without a diff

**Files:**
- Modify: `profiles/review/compose.md`

- [ ] **Step 1: Rewrite compose.md so it never demands a diff**

Replace the contents of `profiles/review/compose.md` with:

```markdown
You are composing the reviewer team for an automated code review.

You do NOT receive the diff or repo contents — the reviewers you spawn read the
code themselves with their tools. Plan a sensible team from the task description
and these rules; return it as structured output (an array of reviewers):

- `name`: stable id (e.g. `correctness`, `spec-compliance`, `<dir>-conventions`, `contracts`).
- `role`: the focus area.
- `scope`: `full` for cross-cutting reviewers, else a top-level directory prefix.
- `extra_context`: optional extra instruction for that reviewer; empty otherwise.

Rules: always include `correctness` and `spec-compliance` (scope `full`). Add one
`<dir>-conventions` reviewer per top-level directory named or clearly implied in
the task; add `contracts` (scope `full`) if two or more directories are involved.
Add a language specialist only when the task names specific languages/frameworks.
When in doubt, prefer a small full-scope team (correctness + spec-compliance).
```

- [ ] **Step 2: Verify the profile still loads**

Run: `mise exec -- cargo test --test profile_regression_test`
Expected: PASS (the profile loader reads `compose.md`; content is free-form).

- [ ] **Step 3: Commit**

```bash
git add profiles/review/compose.md
git commit -m "feat(review): compose plans the team without requiring a diff"
```

---

## Task 3: Subagent prompt instructs tool use

**Files:**
- Modify: `profiles/review/subagent.md`

- [ ] **Step 1: Ensure subagent.md tells the reviewer to use its tools**

Read the current file first: `mise exec -- cat profiles/review/subagent.md` is FORBIDDEN — use the read tool. Then ensure it contains an explicit instruction block (append if missing):

```markdown
# Gathering context
Use your tools to read the code in your scope before reporting. Typically:
`git_diff` (optionally `-- <scope>` to focus on your directory) to see changes,
then `read_file` / `list_files` for surrounding context. Do not ask for the diff —
fetch it. Base every finding on code you actually read.
```

- [ ] **Step 2: Verify the profile still loads**

Run: `mise exec -- cargo test --test profile_regression_test`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add profiles/review/subagent.md
git commit -m "feat(review): subagent prompt directs reviewers to read code with tools"
```

---

## Task 4: Eval fixture is a real git repo

**Files:**
- Modify: `evals/src/runner.rs` (`run_fixture`, the repo-copy + patch block ~lines 94-104)

- [ ] **Step 1: Make the tmp workdir a git repo with the patch as unstaged changes**

Replace the patch-application block in `run_fixture`:

```rust
    // Set up a real git repo so subagents' `git_diff` shows the patch as the
    // changes under review (mirrors real usage: a repo with uncommitted work).
    let tmpdir = tempfile::tempdir()?;
    copy_dir_recursive(&repo, tmpdir.path())?;
    let git = |args: &[&str]| {
        let mut c = Command::new("git");
        c.args(args).current_dir(tmpdir.path());
        c
    };
    git(&["init", "-q"]).output().await?;
    git(&["config", "user.email", "eval@gantry.test"]).output().await?;
    git(&["config", "user.name", "gantry-eval"]).output().await?;
    git(&["add", "-A"]).output().await?;
    git(&["commit", "-q", "-m", "base"]).output().await?;
    if patch.exists() {
        git(&["apply", patch.to_str().unwrap()]).output().await?;
    }
```

(Keep the existing `let tmpdir`/`copy_dir_recursive` only once — this block replaces them.)

- [ ] **Step 2: Verify the eval crate builds**

Run: `mise exec -- cargo build -p gantry-evals --tests`
Expected: build succeeds (exit 0).

- [ ] **Step 3: Commit**

```bash
git add evals/src/runner.rs
git commit -m "test(evals): fixtures run in a real git repo so git_diff sees the patch"
```

---

## Task 5: Re-enable the live team eval

**Files:**
- Modify: `evals/tests/runner_test.rs` (`team_fixture_003_runs_live`)

- [ ] **Step 1: Remove the `#[ignore]` and stale comment**

Delete the `#[ignore = "blocked on team code-context injection (SP2/SP4)…"]` attribute above `async fn team_fixture_003_runs_live`, and replace the "BLOCKED for now…" comment with:

```rust
    // Live end-to-end team run on the real fixture: compose → spawned reviewers
    // read the diff with their tools → unify. run_all asserts the contract
    // (exit ok, one subagent_done per spawn before the single JSON fence, ≥1 finding).
```

- [ ] **Step 2: Run it live (requires a key)**

Run: `set -a; source ./.envrc; set +a; mise exec -- cargo test -p gantry-evals --test runner_test team_fixture_003_runs_live -- --nocapture`
Expected: PASS — a grounded review with ≥1 finding. (Without `ANTHROPIC_API_KEY` the test skips via its early return.)

- [ ] **Step 3: Commit**

```bash
git add evals/tests/runner_test.rs
git commit -m "test(evals): re-enable the live team fixture (grounded review)"
```

---

## Task 6: Full gate

- [ ] **Step 1: Build + test + fmt + clippy**

```bash
mise exec -- cargo build --workspace --all-targets
mise exec -- cargo test --workspace
mise exec -- cargo fmt --check
mise exec -- cargo clippy --workspace --all-targets -- -D warnings
```
Expected: build 0; all suites pass (live eval skips without a key); fmt 0; clippy 0.

- [ ] **Step 2: Live grounded validation (with key)**

```bash
set -a; source ./.envrc; set +a
mise exec -- cargo test -p gantry-evals --test runner_test team_fixture_003_runs_live -- --nocapture
```
Expected: PASS — closes the team-blindness gap; update ADR-0005's "Known limitation" note to resolved and remove the SP4 deferral entry if desired.

---

## Self-review

- **Spec coverage:** subagent tool loop (Task 1) ✓; reuse `tools` toolset (Task 1 uses `registry.schemas()` built from `validated.tools`) ✓; compose diff-free (Task 2) ✓; scope advisory via prompt (Tasks 2/3 carry scope; no hard jail) ✓; subagent prompt (Task 3) ✓; fixture git repo + re-enable (Tasks 4/5) ✓; testing unit+live (Tasks 1/5) ✓.
- **Placeholders:** none — every code/step is concrete. Task 3 Step 1 requires reading the current `subagent.md` first (content appended is given).
- **Type consistency:** `ChatMessage::{User, Assistant{text,tool_calls}, ToolResults}`, `ToolResult{id,content,is_error}`, `registry.dispatch(role,turn,name,args).await.content`, `registry.schemas()` — all match single mode (`src/mode/single.rs:36,110-127`). `SUBAGENT_MAX_TOOL_TURNS` (new) vs `SUBAGENT_MAX_TURNS` (existing, round cap) are distinct by design.
