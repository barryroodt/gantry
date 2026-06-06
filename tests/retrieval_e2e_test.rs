use std::sync::{Arc, Mutex};

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

// ---------------------------------------------------------------------------
// Minimal stub provider — pops scripted responses in order.
// ---------------------------------------------------------------------------

struct StubProvider {
    responses: Arc<Mutex<Vec<ProviderResponse>>>,
}

impl StubProvider {
    fn new(responses: Vec<ProviderResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
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
        _system: &str,
        _messages: &[ChatMessage],
        _tools: &[ToolSchema],
    ) -> anyhow::Result<ProviderResponse> {
        let mut guard = self.responses.lock().unwrap();
        if guard.is_empty() {
            anyhow::bail!("stub provider: no more responses");
        }
        Ok(guard.remove(0))
    }
}

// ---------------------------------------------------------------------------
// Validated builder — mirrors single_mode_test.rs.
// ---------------------------------------------------------------------------

fn test_validated(workdir: &TempDir, prompt_file: &std::path::Path) -> Validated {
    Validated {
        mode: Mode::Single,
        model: "gpt-4o".into(),
        provider: Provider::OpenAi,
        workdir: workdir.path().to_path_buf(),
        prompt_file: prompt_file.to_path_buf(),
        max_tokens: 100_000,
        timeout_ms: 60_000,
        inject_skills: vec![],
        system_prompt: None,
        subagent_system_prompt: None,
        compose_prompt: None,
        unify_prompt: None,
        tools: vec![],
        shell_allow: vec![],
        isolate: false,
        max_iterations: 10,
    }
}

// ---------------------------------------------------------------------------
// T6: end-to-end cap → stash → hint → retrieve roundtrip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cap_then_retrieve_roundtrips_over_event_stream() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();

    // 600 lines, NO trailing newline.
    let content = (1..=600)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dir.path().join("big.txt"), &content).unwrap();

    // The handle that compress will mint for read_file's output.
    // For a non-noisy tool, cap_input == lines.join("\n") == content (no trailing NL).
    let handle = gantry::tools::retrieval::mint_handle("read_file", &content);

    let retrieve_args = format!(r#"{{"handle":"{}"}}"#, handle);

    let provider = StubProvider::new(vec![
        // Turn 1 — ask for the large file.
        ProviderResponse {
            text: "reading".into(),
            tool_calls: vec![ToolCallRequest {
                id: "c1".into(),
                name: "read_file".into(),
                args_json: r#"{"path":"big.txt"}"#.into(),
            }],
            input_tokens: 10,
            output_tokens: 5,
            cache_read: 0,
            cache_write: 0,
        },
        // Turn 2 — use the handle from the recovery hint to retrieve the elided middle.
        ProviderResponse {
            text: String::new(),
            tool_calls: vec![ToolCallRequest {
                id: "c2".into(),
                name: "retrieve".into(),
                args_json: retrieve_args,
            }],
            input_tokens: 10,
            output_tokens: 5,
            cache_read: 0,
            cache_write: 0,
        },
        // Turn 3 — done.
        ProviderResponse {
            text: "done".into(),
            tool_calls: vec![],
            input_tokens: 8,
            output_tokens: 3,
            cache_read: 0,
            cache_write: 0,
        },
    ]);

    // Write the mandatory prompt file.
    let prompt_path = dir.path().join("prompt.md");
    std::fs::write(&prompt_path, "analyse big.txt").unwrap();

    let validated = test_validated(&dir, &prompt_path);
    let cancel = shared_token();
    let meter = Arc::new(TokenMeter::new(validated.max_tokens, cancel.clone()));
    let _watchdog = spawn_timeout_watchdog(cancel.clone(), validated.timeout_ms);

    let exit = SingleMode {
        validated,
        meter,
        cancel,
        registry: ToolRegistry::new(dir.path().to_path_buf(), vec![]),
        skill_loader: SkillLoader::new(dir.path().to_path_buf()),
        provider: Box::new(provider),
        prompt: "analyse big.txt".into(),
    }
    .run()
    .await;

    // -----------------------------------------------------------------------
    // Assertions
    // -----------------------------------------------------------------------

    assert_eq!(exit, ExitCode::Ok, "run should complete successfully");

    let events = _guard.drain_events();

    // 1. read_file result was capped → handle present.
    let read_file_result = events
        .iter()
        .find(|e| matches!(e, GantryEvent::ToolResult { tool, .. } if tool == "read_file"));
    assert!(
        read_file_result.is_some(),
        "expected ToolResult for read_file"
    );
    match read_file_result.unwrap() {
        GantryEvent::ToolResult {
            tool,
            handle: result_handle,
            error,
            ..
        } => {
            assert_eq!(tool, "read_file");
            assert_eq!(
                result_handle.as_deref(),
                Some(handle.as_str()),
                "read_file result must carry the stash handle (600-line output was capped)"
            );
            assert!(error.is_none(), "read_file must not error");
        }
        _ => unreachable!(),
    }

    // 2. retrieve was called with the handle in its args.
    let retrieve_call = events
        .iter()
        .find(|e| matches!(e, GantryEvent::ToolCall { tool, .. } if tool == "retrieve"));
    assert!(retrieve_call.is_some(), "expected ToolCall for retrieve");
    match retrieve_call.unwrap() {
        GantryEvent::ToolCall { tool, args, .. } => {
            assert_eq!(tool, "retrieve");
            assert!(
                args.contains(handle.as_str()),
                "retrieve call args must contain the handle; args={args:?}"
            );
        }
        _ => unreachable!(),
    }

    // 3. retrieve succeeded — no error, and its own output (100 lines) is below
    //    the compression cap so its handle is None.
    let retrieve_result = events
        .iter()
        .find(|e| matches!(e, GantryEvent::ToolResult { tool, .. } if tool == "retrieve"));
    assert!(
        retrieve_result.is_some(),
        "expected ToolResult for retrieve"
    );
    match retrieve_result.unwrap() {
        GantryEvent::ToolResult {
            tool,
            error,
            handle: retrieve_handle,
            ..
        } => {
            assert_eq!(tool, "retrieve");
            assert!(
                error.is_none(),
                "retrieve must succeed; got error={error:?}"
            );
            assert!(
                retrieve_handle.is_none(),
                "retrieve result (100 lines) must not itself be capped"
            );
        }
        _ => unreachable!(),
    }
}
