use std::sync::{Arc, Mutex};
use std::time::Duration;

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

struct TeamCoordinatorProvider {
    coordinator: Mutex<Vec<ProviderResponse>>,
    reviewer_text: String,
    captured_system: Arc<Mutex<Vec<String>>>,
}

impl TeamCoordinatorProvider {
    fn new(coordinator: Vec<ProviderResponse>, reviewer_text: &str) -> Self {
        Self {
            coordinator: Mutex::new(coordinator),
            reviewer_text: reviewer_text.into(),
            captured_system: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl ProviderAdapter for TeamCoordinatorProvider {
    fn provider(&self) -> Provider {
        Provider::OpenAi
    }

    fn model(&self) -> &str {
        "gpt-team-test"
    }

    async fn complete(
        &self,
        system: &str,
        messages: &[ChatMessage],
        _tools: &[ToolSchema],
    ) -> anyhow::Result<ProviderResponse> {
        self.captured_system
            .lock()
            .unwrap()
            .push(system.to_string());
        if messages
            .iter()
            .any(|m| matches!(m, ChatMessage::User(text) if text.starts_with("Role: ")))
        {
            return Ok(ProviderResponse {
                text: self.reviewer_text.clone(),
                tool_calls: vec![],
                input_tokens: 1,
                output_tokens: 1,
                cache_read: 0,
                cache_write: 0,
            });
        }

        let mut guard = self.coordinator.lock().unwrap();
        if guard.is_empty() {
            anyhow::bail!("team coordinator stub: no more responses");
        }
        Ok(guard.remove(0))
    }
}

struct PanicReviewerProvider {
    coordinator: Mutex<Vec<ProviderResponse>>,
}

impl PanicReviewerProvider {
    fn new(coordinator: Vec<ProviderResponse>) -> Self {
        Self {
            coordinator: Mutex::new(coordinator),
        }
    }
}

#[async_trait]
impl ProviderAdapter for PanicReviewerProvider {
    fn provider(&self) -> Provider {
        Provider::OpenAi
    }

    fn model(&self) -> &str {
        "gpt-team-collapse"
    }

    async fn complete(
        &self,
        _system: &str,
        messages: &[ChatMessage],
        _tools: &[ToolSchema],
    ) -> anyhow::Result<ProviderResponse> {
        if messages
            .iter()
            .any(|m| matches!(m, ChatMessage::User(text) if text.starts_with("Role: ")))
        {
            panic!("subagent task panic");
        }

        let mut guard = self.coordinator.lock().unwrap();
        Ok(guard.remove(0))
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
        tools: vec![],
    }
}

async fn run_team_mode(
    dir: &TempDir,
    provider: Arc<dyn ProviderAdapter>,
    max_tokens: u64,
    timeout_ms: u64,
) -> ExitCode {
    let prompt_path = dir.path().join("prompt.md");
    std::fs::write(&prompt_path, "team review this code").unwrap();

    let mut validated = test_validated(dir, &prompt_path);
    validated.max_tokens = max_tokens;
    validated.timeout_ms = timeout_ms;

    let cancel = shared_token();
    let meter = Arc::new(TokenMeter::new(validated.max_tokens, cancel.clone()));
    let _watchdog = spawn_timeout_watchdog(cancel.clone(), validated.timeout_ms);

    let roster = Arc::new(SubagentRoster::new());
    let registry = ToolRegistry::team(
        validated.workdir.clone(),
        roster.clone(),
        provider.clone(),
        "reviewer system".into(),
        meter.clone(),
        vec![],
    );

    TeamMode {
        validated,
        meter,
        cancel,
        registry,
        roster,
        skill_loader: SkillLoader::new(dir.path().to_path_buf()),
        provider,
        prompt: "team review this code".into(),
        spawned_subagents: 0,
    }
    .run()
    .await
}

#[tokio::test]
async fn team_mode_completes_ok_with_subagent_events() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();

    let provider = Arc::new(TeamCoordinatorProvider::new(
        vec![
            ProviderResponse {
                text: String::new(),
                tool_calls: vec![ToolCallRequest {
                    id: "call_spawn".into(),
                    name: "spawn_subagent".into(),
                    args_json: r#"{"name":"correctness","role":"correctness","template":"correctness","scope":"full"}"#.into(),
                }],
                input_tokens: 5,
                output_tokens: 5,
                cache_read: 0,
                cache_write: 0,
            },
            ProviderResponse {
                text: String::new(),
                tool_calls: vec![ToolCallRequest {
                    id: "call_collect".into(),
                    name: "collect_outputs".into(),
                    args_json: r#"{"round":1}"#.into(),
                }],
                input_tokens: 5,
                output_tokens: 5,
                cache_read: 0,
                cache_write: 0,
            },
            ProviderResponse {
                text: "```json\n{\"summary\":\"ok\",\"verdict\":\"ready\",\"findings\":[],\"strengths\":[]}\n```".into(),
                tool_calls: vec![],
                input_tokens: 5,
                output_tokens: 5,
                cache_read: 0,
                cache_write: 0,
            },
        ],
        "round-1 reviewer report",
    ));

    let exit = run_team_mode(&dir, provider, 10_000, 60_000).await;

    assert_eq!(exit, ExitCode::Ok);

    tokio::time::sleep(Duration::from_millis(50)).await;

    let events = _guard.drain_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GantryEvent::SubagentSpawn { name, .. } if name == "correctness")),
        "expected subagent_spawn, got: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GantryEvent::SubagentDone { name, .. } if name == "correctness")),
        "expected subagent_done, got: {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            GantryEvent::AssistantText { text, role, .. }
                if role == "coordinator" && text.contains("```json")
        )),
        "expected coordinator JSON fence"
    );
}

#[tokio::test]
async fn team_mode_all_subagents_crash_emits_team_collapse() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();

    let provider = Arc::new(PanicReviewerProvider::new(vec![
        ProviderResponse {
            text: String::new(),
            tool_calls: vec![
                ToolCallRequest {
                    id: "call_spawn_a".into(),
                    name: "spawn_subagent".into(),
                    args_json: r#"{"name":"broken-a","role":"panic-role","template":"panic-role","scope":"full"}"#.into(),
                },
                ToolCallRequest {
                    id: "call_spawn_b".into(),
                    name: "spawn_subagent".into(),
                    args_json: r#"{"name":"broken-b","role":"panic-role","template":"panic-role","scope":"full"}"#.into(),
                },
            ],
            input_tokens: 5,
            output_tokens: 5,
            cache_read: 0,
            cache_write: 0,
        },
        ProviderResponse {
            text: String::new(),
            tool_calls: vec![ToolCallRequest {
                id: "call_collect".into(),
                name: "collect_outputs".into(),
                args_json: r#"{"round":1}"#.into(),
            }],
            input_tokens: 5,
            output_tokens: 5,
            cache_read: 0,
            cache_write: 0,
        },
    ]));

    let exit = run_team_mode(&dir, provider, 10_000, 60_000).await;

    assert_eq!(exit, ExitCode::Error);

    let events = _guard.drain_events();
    assert!(
        events.iter().any(|e| matches!(
            e,
            GantryEvent::Error {
                kind: ErrorKind::TeamCollapse,
                ..
            }
        )),
        "expected team_collapse error, got: {events:?}"
    );
}

#[tokio::test]
async fn team_mode_budget_trip_during_reviewer_round() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();

    let provider = Arc::new(TeamCoordinatorProvider::new(
        vec![
            ProviderResponse {
                text: String::new(),
                tool_calls: vec![ToolCallRequest {
                    id: "call_spawn".into(),
                    name: "spawn_subagent".into(),
                    args_json: r#"{"name":"correctness","role":"correctness","template":"correctness","scope":"full"}"#.into(),
                }],
                input_tokens: 5,
                output_tokens: 5,
                cache_read: 0,
                cache_write: 0,
            },
            ProviderResponse {
                text: String::new(),
                tool_calls: vec![ToolCallRequest {
                    id: "call_collect".into(),
                    name: "collect_outputs".into(),
                    args_json: r#"{"round":1}"#.into(),
                }],
                input_tokens: 60,
                output_tokens: 50,
                cache_read: 0,
                cache_write: 0,
            },
        ],
        "slow reviewer report",
    ));

    let cancel = shared_token();
    let meter = Arc::new(TokenMeter::new(100, cancel.clone()));
    let _watchdog = spawn_timeout_watchdog(cancel.clone(), 60_000);

    let prompt_path = dir.path().join("prompt.md");
    std::fs::write(&prompt_path, "team review").unwrap();
    let validated = test_validated(&dir, &prompt_path);

    let roster = Arc::new(SubagentRoster::new());
    let provider_for_registry = provider.clone();
    let registry = ToolRegistry::team(
        validated.workdir.clone(),
        roster.clone(),
        provider_for_registry,
        "reviewer system".into(),
        meter.clone(),
        vec![],
    );

    let exit = TeamMode {
        validated,
        meter: meter.clone(),
        cancel: cancel.clone(),
        registry,
        roster,
        skill_loader: SkillLoader::new(dir.path().to_path_buf()),
        provider,
        prompt: "team review".into(),
        spawned_subagents: 0,
    }
    .run()
    .await;

    assert_eq!(exit, ExitCode::Budget);
    assert!(
        cancel.is_cancelled(),
        "cancel should propagate on budget trip"
    );
    assert!(
        _guard
            .drain_events()
            .iter()
            .any(|e| matches!(e, GantryEvent::BudgetExceeded { .. })),
        "expected budget_exceeded event"
    );
}

#[tokio::test]
async fn team_mode_uses_supplied_coordinator_system_prompt() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();
    let prompt_path = dir.path().join("prompt.md");
    std::fs::write(&prompt_path, "team task").unwrap();

    let provider = Arc::new(TeamCoordinatorProvider::new(
        vec![ProviderResponse {
            text: "done".into(),
            tool_calls: vec![],
            input_tokens: 1,
            output_tokens: 1,
            cache_read: 0,
            cache_write: 0,
        }],
        "unused reviewer text",
    ));
    let captured = provider.captured_system.clone();

    let mut validated = test_validated(&dir, &prompt_path);
    validated.system_prompt = Some("MARKER-TEAM-COORD".into());

    let cancel = shared_token();
    let meter = Arc::new(TokenMeter::new(validated.max_tokens, cancel.clone()));
    let _watchdog = spawn_timeout_watchdog(cancel.clone(), validated.timeout_ms);
    let roster = Arc::new(SubagentRoster::new());
    let registry = ToolRegistry::team(
        validated.workdir.clone(),
        roster.clone(),
        provider.clone(),
        "reviewer system".into(),
        meter.clone(),
        vec![],
    );
    TeamMode {
        validated,
        meter,
        cancel,
        registry,
        roster,
        skill_loader: SkillLoader::new(dir.path().to_path_buf()),
        provider,
        prompt: "team task".into(),
        spawned_subagents: 0,
    }
    .run()
    .await;

    let systems = captured.lock().unwrap();
    assert!(
        systems.iter().any(|s| s.contains("MARKER-TEAM-COORD")),
        "supplied coordinator system not used: {systems:?}"
    );
}
