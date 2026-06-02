use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use gantry::cancel::{shared_token, spawn_timeout_watchdog};
use gantry::cli::{Mode, Provider, Validated};
use gantry::emitter::TestEmitterGuard;
use gantry::events::{ExitCode, GantryEvent};
use gantry::meter::TokenMeter;
use gantry::mode::single::SingleMode;
use gantry::provider::{
    ChatMessage, ProviderAdapter, ProviderResponse, ToolCallRequest, ToolSchema,
};
use gantry::skills::SkillLoader;
use gantry::tools::ToolRegistry;
use tempfile::TempDir;

struct StubProvider {
    responses: Arc<Mutex<Vec<ProviderResponse>>>,
    captured_system: Arc<Mutex<Vec<String>>>,
}

impl StubProvider {
    fn new(responses: Vec<ProviderResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            captured_system: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl ProviderAdapter for StubProvider {
    fn provider(&self) -> Provider {
        Provider::OpenAi
    }

    fn model(&self) -> &str {
        "gpt-test"
    }

    async fn complete(
        &self,
        system: &str,
        _messages: &[ChatMessage],
        _tools: &[ToolSchema],
    ) -> anyhow::Result<ProviderResponse> {
        self.captured_system
            .lock()
            .unwrap()
            .push(system.to_string());
        let mut guard = self.responses.lock().unwrap();
        if guard.is_empty() {
            anyhow::bail!("stub provider: no more responses");
        }
        Ok(guard.remove(0))
    }
}

struct HangingProvider;

#[async_trait]
impl ProviderAdapter for HangingProvider {
    fn provider(&self) -> Provider {
        Provider::OpenAi
    }

    fn model(&self) -> &str {
        "gpt-hang"
    }

    async fn complete(
        &self,
        _system: &str,
        _messages: &[ChatMessage],
        _tools: &[ToolSchema],
    ) -> anyhow::Result<ProviderResponse> {
        tokio::time::sleep(Duration::from_secs(3600)).await;
        Ok(ProviderResponse {
            text: "never".into(),
            tool_calls: vec![],
            input_tokens: 1,
            output_tokens: 1,
            cache_read: 0,
            cache_write: 0,
        })
    }
}

struct AlwaysToolCallProvider;

#[async_trait]
impl ProviderAdapter for AlwaysToolCallProvider {
    fn provider(&self) -> Provider {
        Provider::OpenAi
    }

    fn model(&self) -> &str {
        "gpt-loop"
    }

    async fn complete(
        &self,
        _system: &str,
        _messages: &[ChatMessage],
        _tools: &[ToolSchema],
    ) -> anyhow::Result<ProviderResponse> {
        Ok(ProviderResponse {
            text: String::new(),
            tool_calls: vec![ToolCallRequest {
                id: "call_1".into(),
                name: "read_file".into(),
                args_json: r#"{"path":"missing.txt"}"#.into(),
            }],
            input_tokens: 1,
            output_tokens: 1,
            cache_read: 0,
            cache_write: 0,
        })
    }
}

fn test_validated(workdir: &TempDir, prompt_file: &std::path::Path) -> Validated {
    Validated {
        mode: Mode::Single,
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
    }
}

async fn run_mode(
    dir: &TempDir,
    provider: Box<dyn ProviderAdapter>,
    max_tokens: u64,
    timeout_ms: u64,
) -> ExitCode {
    let prompt_path = dir.path().join("prompt.md");
    std::fs::write(&prompt_path, "review this code").unwrap();
    std::fs::write(dir.path().join("sample.txt"), "sample content").unwrap();

    let mut validated = test_validated(dir, &prompt_path);
    validated.max_tokens = max_tokens;
    validated.timeout_ms = timeout_ms;

    let cancel = shared_token();
    let meter = Arc::new(TokenMeter::new(validated.max_tokens, cancel.clone()));
    let _watchdog = spawn_timeout_watchdog(cancel.clone(), validated.timeout_ms);

    SingleMode {
        validated,
        meter,
        cancel,
        registry: ToolRegistry::new(dir.path().to_path_buf(), vec![]),
        skill_loader: SkillLoader::new(dir.path().to_path_buf()),
        provider,
        prompt: "review this code".into(),
    }
    .run()
    .await
}

#[tokio::test]
async fn single_mode_completes_ok_after_tool_loop() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();

    let provider = StubProvider::new(vec![
        ProviderResponse {
            text: "checking file".into(),
            tool_calls: vec![ToolCallRequest {
                id: "call_1".into(),
                name: "read_file".into(),
                args_json: r#"{"path":"sample.txt"}"#.into(),
            }],
            input_tokens: 10,
            output_tokens: 5,
            cache_read: 0,
            cache_write: 0,
        },
        ProviderResponse {
            text: "review complete".into(),
            tool_calls: vec![],
            input_tokens: 8,
            output_tokens: 12,
            cache_read: 0,
            cache_write: 0,
        },
    ]);

    let exit = run_mode(&dir, Box::new(provider), 10_000, 60_000).await;

    assert_eq!(exit, ExitCode::Ok);

    let events = _guard.drain_events();
    assert!(
        events.iter().any(
            |e| matches!(e, GantryEvent::AssistantText { text, .. } if text == "checking file")
        ),
        "expected assistant text from first turn"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GantryEvent::ToolCall { tool, .. } if tool == "read_file")),
        "expected read_file tool call"
    );
    assert!(
        events.iter().any(
            |e| matches!(e, GantryEvent::AssistantText { text, .. } if text == "review complete")
        ),
        "expected final assistant text"
    );
}

#[tokio::test]
async fn single_mode_budget_trip_mid_loop() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();

    let provider = StubProvider::new(vec![ProviderResponse {
        text: String::new(),
        tool_calls: vec![ToolCallRequest {
            id: "call_1".into(),
            name: "read_file".into(),
            args_json: r#"{"path":"sample.txt"}"#.into(),
        }],
        input_tokens: 60,
        output_tokens: 50,
        cache_read: 0,
        cache_write: 0,
    }]);

    let exit = run_mode(&dir, Box::new(provider), 100, 60_000).await;

    assert_eq!(exit, ExitCode::Budget);
    assert!(
        _guard
            .drain_events()
            .iter()
            .any(|e| matches!(e, GantryEvent::BudgetExceeded { .. })),
        "expected budget_exceeded event"
    );
}

#[tokio::test]
async fn single_mode_timeout_mid_loop() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();

    let exit = run_mode(&dir, Box::new(HangingProvider), 10_000, 50).await;

    assert_eq!(exit, ExitCode::Timeout);
}

#[tokio::test]
async fn single_mode_max_turns_cap_exits_ok() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();

    let exit = run_mode(&dir, Box::new(AlwaysToolCallProvider), 1_000_000, 60_000).await;

    assert_eq!(exit, ExitCode::Ok);

    let agent_turns = _guard
        .drain_events()
        .into_iter()
        .filter(|e| matches!(e, GantryEvent::AgentTurn { .. }))
        .count();
    assert_eq!(
        agent_turns, 20,
        "loop should cap at MAX_TURNS provider calls"
    );
}

#[tokio::test]
async fn single_mode_uses_supplied_system_prompt() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();
    let prompt_path = dir.path().join("prompt.md");
    std::fs::write(&prompt_path, "do the task").unwrap();

    let provider = StubProvider::new(vec![ProviderResponse {
        text: "done".into(),
        tool_calls: vec![],
        input_tokens: 1,
        output_tokens: 1,
        cache_read: 0,
        cache_write: 0,
    }]);
    let captured = provider.captured_system.clone();

    let mut validated = test_validated(&dir, &prompt_path);
    validated.system_prompt = Some("MARKER-SYSTEM-PERSONA".into());

    let cancel = shared_token();
    let meter = Arc::new(TokenMeter::new(validated.max_tokens, cancel.clone()));
    let _watchdog = spawn_timeout_watchdog(cancel.clone(), validated.timeout_ms);
    SingleMode {
        validated,
        meter,
        cancel,
        registry: ToolRegistry::new(dir.path().to_path_buf(), vec![]),
        skill_loader: SkillLoader::new(dir.path().to_path_buf()),
        provider: Box::new(provider),
        prompt: "do the task".into(),
    }
    .run()
    .await;

    let systems = captured.lock().unwrap();
    assert!(
        systems.iter().any(|s| s.contains("MARKER-SYSTEM-PERSONA")),
        "supplied system prompt not used: {systems:?}"
    );
}

#[tokio::test]
async fn single_mode_default_system_prompt_when_none() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();
    let prompt_path = dir.path().join("prompt.md");
    std::fs::write(&prompt_path, "do the task").unwrap();

    let provider = StubProvider::new(vec![ProviderResponse {
        text: "done".into(),
        tool_calls: vec![],
        input_tokens: 1,
        output_tokens: 1,
        cache_read: 0,
        cache_write: 0,
    }]);
    let captured = provider.captured_system.clone();

    let validated = test_validated(&dir, &prompt_path); // system_prompt: None

    let cancel = shared_token();
    let meter = Arc::new(TokenMeter::new(validated.max_tokens, cancel.clone()));
    let _watchdog = spawn_timeout_watchdog(cancel.clone(), validated.timeout_ms);
    SingleMode {
        validated,
        meter,
        cancel,
        registry: ToolRegistry::new(dir.path().to_path_buf(), vec![]),
        skill_loader: SkillLoader::new(dir.path().to_path_buf()),
        provider: Box::new(provider),
        prompt: "do the task".into(),
    }
    .run()
    .await;

    let systems = captured.lock().unwrap();
    assert!(
        systems.iter().any(|s| s.contains("gantry harness")),
        "default system prompt not used: {systems:?}"
    );
}
