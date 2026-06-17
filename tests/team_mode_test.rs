use std::sync::Arc;

use async_trait::async_trait;
use gantry::cancel::{shared_token, spawn_timeout_watchdog};
use gantry::cli::{Mode, Provider, Validated};
use gantry::emitter::TestEmitterGuard;
use gantry::events::{ErrorKind, ExitCode, GantryEvent};
use gantry::meter::TokenMeter;
use gantry::mode::team::TeamMode;
use gantry::provider::{
    ChatMessage, ProviderAdapter, ProviderResponse, ToolCallRequest, ToolSchema,
};
use gantry::skills::SkillLoader;
use gantry::tools::subagent::SubagentRoster;
use gantry::tools::ToolRegistry;
use tempfile::TempDir;

/// Stateless provider for the harness-driven team state machine: returns a
/// structured `respond` plan on the compose call, role text for subagents, and
/// structured findings on the unify call. Phase is inferred from the messages.
/// Optionally records the system prompts it is given (via a channel — no lock).
struct TeamScriptProvider {
    compose_json: String,
    unify_json: String,
    systems: Option<tokio::sync::mpsc::UnboundedSender<String>>,
}

impl TeamScriptProvider {
    fn new(compose_json: &str) -> Self {
        Self {
            compose_json: compose_json.into(),
            unify_json: r#"{"summary":"unified","verdict":"ready","findings":[],"strengths":[]}"#
                .into(),
            systems: None,
        }
    }

    fn capturing(compose_json: &str, tx: tokio::sync::mpsc::UnboundedSender<String>) -> Self {
        let mut p = Self::new(compose_json);
        p.systems = Some(tx);
        p
    }
}

fn respond(args_json: &str) -> ProviderResponse {
    ProviderResponse {
        text: String::new(),
        tool_calls: vec![ToolCallRequest {
            id: "respond".into(),
            name: "respond".into(),
            args_json: args_json.into(),
        }],
        input_tokens: 1,
        output_tokens: 1,
        cache_read: 0,
        cache_write: 0,
    }
}

#[async_trait]
impl ProviderAdapter for TeamScriptProvider {
    fn provider(&self) -> Provider {
        Provider::OpenAi
    }
    fn model(&self) -> &str {
        "gpt-team"
    }
    async fn complete(
        &self,
        system: &str,
        messages: &[ChatMessage],
        _tools: &[ToolSchema],
    ) -> anyhow::Result<ProviderResponse> {
        if let Some(tx) = &self.systems {
            let _ = tx.send(system.to_string());
        }

        // Subagent turn: first user line is "Role: <role>".
        for m in messages {
            if let ChatMessage::User(text) = m {
                if let Some(role) = text.lines().next().and_then(|l| l.strip_prefix("Role: ")) {
                    if role == "panic-role" {
                        panic!("subagent task panic");
                    }
                    return Ok(ProviderResponse {
                        text: format!("{role} report"),
                        tool_calls: vec![],
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_read: 0,
                        cache_write: 0,
                    });
                }
            }
        }

        // Unify turn carries the collected reports; otherwise it is the compose turn.
        let is_unify = messages
            .iter()
            .any(|m| matches!(m, ChatMessage::User(t) if t.contains("# Subagent reports")));
        if is_unify {
            Ok(respond(&self.unify_json))
        } else {
            Ok(respond(&self.compose_json))
        }
    }
}

fn test_validated(workdir: &TempDir, prompt_file: &std::path::Path) -> Validated {
    Validated {
        mode: Mode::Team,
        model: "gpt-4o".into(),
        provider: Provider::OpenAi,
        workdir: workdir.path().to_path_buf(),
        prompt_file: prompt_file.to_path_buf(),
        max_tokens: 10_000,
        timeout_ms: 60_000,
        inject_skills: vec![],
        system_prompt: None,
        subagent_system_prompt: None,
        compose_prompt: None,
        unify_prompt: None,
        tools: vec![],
        shell_allow: vec![],
        isolate: false,
        max_iterations: 5,
        context_limit: None,
        base_url: None,
        skills_dir: workdir.path().join(".claude/skills"),
    }
}

fn build_team(dir: &TempDir, validated: Validated, provider: Arc<dyn ProviderAdapter>) -> TeamMode {
    let cancel = shared_token();
    let meter = Arc::new(TokenMeter::new(validated.max_tokens, cancel.clone()));
    let _watchdog = spawn_timeout_watchdog(cancel.clone(), validated.timeout_ms);
    let roster = Arc::new(SubagentRoster::new());
    let registry = Arc::new(ToolRegistry::new(validated.workdir.clone(), vec![]));
    TeamMode {
        validated,
        meter,
        cancel,
        registry,
        roster,
        skill_loader: SkillLoader::new(dir.path().join(".claude/skills")),
        provider,
        prompt: "team review this code".into(),
        spawned_subagents: 0,
    }
}

async fn run_team_mode(
    dir: &TempDir,
    provider: Arc<dyn ProviderAdapter>,
    max_tokens: u64,
) -> ExitCode {
    let prompt_path = dir.path().join("prompt.md");
    std::fs::write(&prompt_path, "team review this code").unwrap();
    let mut validated = test_validated(dir, &prompt_path);
    validated.max_tokens = max_tokens;
    build_team(dir, validated, provider).run().await
}

#[tokio::test]
async fn team_mode_completes_ok_spawns_plan_and_emits_unify_fence() {
    let guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(TeamScriptProvider::new(
        r#"{"subagents":[{"name":"correctness","role":"correctness","scope":"full"},{"name":"spec-compliance","role":"spec-compliance","scope":"full"}]}"#,
    ));

    let exit = run_team_mode(&dir, provider, 100_000).await;
    assert_eq!(exit, ExitCode::Ok);

    let events = guard.drain_events();
    let spawns = events
        .iter()
        .filter(|e| matches!(e, GantryEvent::SubagentSpawn { .. }))
        .count();
    assert_eq!(spawns, 2, "expected one spawn per planned subagent");

    // ADR-0005 validation: one subagent_done per spawned subagent, each emitted
    // before the terminal unify fence (clean shutdown + join, no leaked tasks).
    let fence_idx = events
        .iter()
        .position(
            |e| matches!(e, GantryEvent::AssistantText { text, .. } if text.contains("```json")),
        )
        .expect("terminal JSON fence emitted");
    let done_idxs: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| matches!(e, GantryEvent::SubagentDone { .. }).then_some(i))
        .collect();
    assert_eq!(
        done_idxs.len(),
        2,
        "expected one subagent_done per spawned subagent"
    );
    assert!(
        done_idxs.iter().all(|&i| i < fence_idx),
        "every subagent_done must precede the fence (done {done_idxs:?}, fence {fence_idx})"
    );

    let fence = match &events[fence_idx] {
        GantryEvent::AssistantText { text, .. } => text,
        _ => unreachable!(),
    };
    assert!(
        fence.contains("\"verdict\""),
        "fence missing verdict: {fence}"
    );
    assert!(
        fence.contains("ready"),
        "fence missing unified verdict: {fence}"
    );
}

#[tokio::test]
async fn team_mode_all_subagents_crash_emits_team_collapse() {
    let guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(TeamScriptProvider::new(
        r#"{"subagents":[{"name":"a","role":"panic-role","scope":"full"},{"name":"b","role":"panic-role","scope":"full"}]}"#,
    ));

    let exit = run_team_mode(&dir, provider, 100_000).await;
    assert_eq!(exit, ExitCode::Error);

    let collapsed = guard.drain_events().into_iter().any(|e| {
        matches!(
            e,
            GantryEvent::Error {
                kind: ErrorKind::TeamCollapse,
                ..
            }
        )
    });
    assert!(collapsed, "expected a team_collapse error");
}

#[tokio::test]
async fn team_mode_budget_trip_during_compose() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(TeamScriptProvider::new(
        r#"{"subagents":[{"name":"correctness","role":"correctness","scope":"full"}]}"#,
    ));

    // max_tokens = 1: the compose call's tokens trip the meter before spawning.
    let exit = run_team_mode(&dir, provider, 1).await;
    assert_eq!(exit, ExitCode::Budget);
}

#[tokio::test]
async fn team_mode_uses_supplied_compose_prompt() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();
    let prompt_path = dir.path().join("prompt.md");
    std::fs::write(&prompt_path, "team review this code").unwrap();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let provider = Arc::new(TeamScriptProvider::capturing(
        r#"{"subagents":[{"name":"correctness","role":"correctness","scope":"full"}]}"#,
        tx,
    ));

    let mut validated = test_validated(&dir, &prompt_path);
    validated.compose_prompt = Some("MARKER-COMPOSE-PERSONA".into());

    build_team(&dir, validated, provider).run().await;

    let mut systems = Vec::new();
    while let Ok(s) = rx.try_recv() {
        systems.push(s);
    }
    assert!(
        systems.iter().any(|s| s.contains("MARKER-COMPOSE-PERSONA")),
        "compose call did not use the supplied compose prompt: {systems:?}"
    );
}
// ── G3: --unify-file ─────────────────────────────────────────────────────────

#[tokio::test]
async fn team_mode_uses_supplied_unify_prompt() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();
    let prompt_path = dir.path().join("prompt.md");
    std::fs::write(&prompt_path, "team review this code").unwrap();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let provider = Arc::new(TeamScriptProvider::capturing(
        r#"{"subagents":[{"name":"correctness","role":"correctness","scope":"full"}]}"#,
        tx,
    ));

    let mut validated = test_validated(&dir, &prompt_path);
    validated.unify_prompt = Some("MARKER-UNIFY-PERSONA".into());

    build_team(&dir, validated, provider).run().await;

    let mut systems = Vec::new();
    while let Ok(s) = rx.try_recv() {
        systems.push(s);
    }
    assert!(
        systems.iter().any(|s| s.contains("MARKER-UNIFY-PERSONA")),
        "unify call did not use the supplied unify prompt: {systems:?}"
    );
}

// ── G4: --skills-dir ─────────────────────────────────────────────────────────

#[tokio::test]
async fn skill_loader_with_custom_root_resolves_from_that_root() {
    use gantry::emitter::TestEmitterGuard;
    use gantry::events::GantryEvent;

    let guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();

    // Create skill at <dir>/foo/SKILL.md — NOT under .claude/skills.
    let skill_dir = dir.path().join("foo");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), "custom root skill body").unwrap();

    // SkillLoader pointed at dir.path() directly (the custom root).
    let loader = SkillLoader::new(dir.path().to_path_buf());
    let names = vec!["foo".to_string()];
    let prefix = loader.inject_core_skills(&names);

    assert!(
        prefix.contains("custom root skill body"),
        "skill body not in prefix: {prefix}"
    );

    let events = guard.drain_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GantryEvent::SkillLoaded { name, .. } if name == "foo")),
        "expected skill_loaded event for 'foo'"
    );
}

#[tokio::test]
async fn skill_load_tool_resolves_from_skills_dir() {
    use gantry::emitter::TestEmitterGuard;

    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();

    // Create skill at <dir>/staged/bar/SKILL.md — the override root.
    let staged = dir.path().join("staged");
    let skill_dir = staged.join("bar");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), "staged skill content").unwrap();

    // ToolRegistry with the custom skills_dir override.
    let registry =
        ToolRegistry::new(dir.path().to_path_buf(), vec![]).with_skills_dir(staged.clone());

    let out = registry
        .dispatch("coordinator", 1, "skill_load", r#"{"name":"bar"}"#)
        .await;

    assert!(
        out.content.contains("staged skill content"),
        "skill_load did not return staged content: {}",
        out.content
    );
    assert!(
        !out.content.starts_with("error:"),
        "unexpected error: {}",
        out.content
    );
}

#[tokio::test]
async fn skill_load_tool_default_unchanged_when_skills_dir_absent() {
    use gantry::emitter::TestEmitterGuard;

    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();

    // Skill at the default location: <workdir>/.claude/skills/myskill/SKILL.md.
    let default_skill = dir.path().join(".claude/skills/myskill");
    std::fs::create_dir_all(&default_skill).unwrap();
    std::fs::write(default_skill.join("SKILL.md"), "default skill content").unwrap();

    // ToolRegistry without .with_skills_dir — defaults to workdir/.claude/skills.
    let registry = ToolRegistry::new(dir.path().to_path_buf(), vec![]);

    let out = registry
        .dispatch("coordinator", 1, "skill_load", r#"{"name":"myskill"}"#)
        .await;

    assert!(
        out.content.contains("default skill content"),
        "default skill_load failed: {}",
        out.content
    );
    assert!(
        !out.content.starts_with("error:"),
        "unexpected error: {}",
        out.content
    );
}

/// Provider that returns configurable token counts per role, used to exercise
/// per-subagent budget slices (G6).
struct SliceTestProvider {
    /// Tokens (input+output) to return for a subagent with this role. Roles not
    /// in the map return 1+1 by default.
    role_tokens: std::collections::HashMap<String, u64>,
}

impl SliceTestProvider {
    fn new(role_tokens: &[(&str, u64)]) -> Self {
        Self {
            role_tokens: role_tokens
                .iter()
                .map(|(r, t)| ((*r).to_string(), *t))
                .collect(),
        }
    }
}

#[async_trait]
impl ProviderAdapter for SliceTestProvider {
    fn provider(&self) -> gantry::cli::Provider {
        gantry::cli::Provider::OpenAi
    }
    fn model(&self) -> &str {
        "gpt-slice-test"
    }
    async fn complete(
        &self,
        _system: &str,
        messages: &[ChatMessage],
        _tools: &[ToolSchema],
    ) -> anyhow::Result<ProviderResponse> {
        // Subagent turn: first user line is "Role: <role>".
        for m in messages {
            if let ChatMessage::User(text) = m {
                if let Some(role) = text.lines().next().and_then(|l| l.strip_prefix("Role: ")) {
                    let tokens = self.role_tokens.get(role).copied().unwrap_or(1);
                    return Ok(ProviderResponse {
                        text: format!("{role} report"),
                        tool_calls: vec![],
                        input_tokens: tokens,
                        output_tokens: 1,
                        cache_read: 0,
                        cache_write: 0,
                    });
                }
            }
        }
        // Unify: contains "# Subagent reports".
        let is_unify = messages
            .iter()
            .any(|m| matches!(m, ChatMessage::User(t) if t.contains("# Subagent reports")));
        if is_unify {
            Ok(respond(
                r#"{"summary":"ok","verdict":"ok","findings":[],"strengths":[]}"#,
            ))
        } else {
            // Compose: spawn heavy + light.
            Ok(respond(
                r#"{"subagents":[{"name":"heavy","role":"heavy","scope":"full"},{"name":"light","role":"light","scope":"full"}]}"#,
            ))
        }
    }
}

/// A subagent whose slice is exceeded must emit `subagent_failed(reason=budget)`
/// and the run must still reach exit `ok` (dropped-lane semantics) because the
/// other subagent completes within its slice and the global cap is not hit.
#[tokio::test]
async fn team_mode_subagent_slice_exceeded_emits_failed_reason_budget() {
    let guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();

    // heavy uses 5001 tokens (input=5000, output=1); light uses 2 (input=1, output=1).
    // max_tokens=10_000; compose uses 2 → remaining=9998; slice=9998/2=4999.
    // heavy consumes 5001 > 4999 → slice exceeded → subagent_failed(reason=budget).
    // light consumes 2 ≤ 4999 → completes.
    // global total ≈ 2+5001+2+2 = 5007 < 10_000 → no global trip → exit ok.
    let provider = Arc::new(SliceTestProvider::new(&[("heavy", 5000), ("light", 1)]));
    let exit = run_team_mode(&dir, provider, 10_000).await;
    assert_eq!(exit, ExitCode::Ok, "run must still exit ok");

    let events = guard.drain_events();

    let failed_budget = events.iter().any(|e| {
        matches!(
            e,
            GantryEvent::SubagentFailed { name, reason, .. }
            if name == "heavy" && reason == "budget"
        )
    });
    assert!(
        failed_budget,
        "expected subagent_failed(name=heavy, reason=budget)"
    );

    // Global budget_exceeded must NOT have been emitted.
    let global_tripped = events
        .iter()
        .any(|e| matches!(e, GantryEvent::BudgetExceeded { .. }));
    assert!(
        !global_tripped,
        "global budget_exceeded must not fire for a slice-only failure"
    );
}

/// When aggregate token consumption reaches the global `--max-tokens` cap the
/// run must exit with code `budget` (2). Here the cap is tripped during the
/// unify call, after both subagents complete within their slices.
#[tokio::test]
async fn team_mode_global_budget_cap_exits_budget() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();

    // Standard provider: every call returns 1 input + 1 output = 2 tokens.
    // max_tokens=7: compose(2) + subA(2) + subB(2) = 6; unify(2) → total=8 > 7 → Budget.
    // Slices: remaining after compose = 5; 5/2 = 2 each; each subagent uses 2 ≤ 2 → OK.
    let provider = Arc::new(TeamScriptProvider::new(
        r#"{"subagents":[{"name":"a","role":"a","scope":"full"},{"name":"b","role":"b","scope":"full"}]}"#,
    ));
    let exit = run_team_mode(&dir, provider, 7).await;
    assert_eq!(
        exit,
        ExitCode::Budget,
        "global cap must produce exit budget (2)"
    );
}
