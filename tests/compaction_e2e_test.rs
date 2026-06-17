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
// StubProvider — pops scripted responses in order; captures each complete call's
// message slice so tests can inspect what the provider saw.
// ---------------------------------------------------------------------------

struct StubProvider {
    responses: Arc<Mutex<Vec<ProviderResponse>>>,
    captured_messages: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
}

impl StubProvider {
    fn new(responses: Vec<ProviderResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            captured_messages: Arc::new(Mutex::new(Vec::new())),
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
        messages: &[ChatMessage],
        _tools: &[ToolSchema],
    ) -> anyhow::Result<ProviderResponse> {
        self.captured_messages
            .lock()
            .unwrap()
            .push(messages.to_vec());
        let mut guard = self.responses.lock().unwrap();
        if guard.is_empty() {
            anyhow::bail!("stub provider: no more responses");
        }
        Ok(guard.remove(0))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
        isolate: false,
        max_iterations: 5,
        context_limit: None,
        base_url: None,
    }
}

/// Deterministic content for big.txt — 30 lines, each ~47 bytes → >512 B total,
/// so every ToolResult carrying it is a compaction candidate.
fn big_txt_content() -> String {
    (1..=30)
        .map(|i| format!("line {i} ........................................"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn read_file_response(id: &str, path: &str, input_tokens: u64) -> ProviderResponse {
    ProviderResponse {
        text: String::new(),
        tool_calls: vec![ToolCallRequest {
            id: id.into(),
            name: "read_file".into(),
            args_json: format!(r#"{{"path":"{}"}}"#, path),
        }],
        input_tokens,
        output_tokens: 1,
        cache_read: 0,
        cache_write: 0,
    }
}

fn done_response() -> ProviderResponse {
    ProviderResponse {
        text: "done".into(),
        tool_calls: vec![],
        input_tokens: 1000,
        output_tokens: 1,
        cache_read: 0,
        cache_write: 0,
    }
}

async fn run_single_with(
    validated: Validated,
    provider: Box<dyn ProviderAdapter>,
    dir: &TempDir,
    prompt: &str,
) -> ExitCode {
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
        prompt: prompt.into(),
    }
    .run()
    .await
}

// ---------------------------------------------------------------------------
// Test 1: compaction fires and emits HistoryCompacted when over the limit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn compaction_triggers_and_emits_when_over_limit() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();
    let big_content = big_txt_content();
    std::fs::write(dir.path().join("big.txt"), &big_content).unwrap();
    let prompt_path = dir.path().join("prompt.md");
    std::fs::write(&prompt_path, "summarize big.txt").unwrap();

    // 4 read_file turns → after the 4th ToolResults is inserted (turn 3→4),
    // compact_history sees 4 ToolResults with KEEP_RECENT_TURNS=3 and elides
    // the oldest one. Each response reports input_tokens=1000 > context_limit=100.
    let provider = StubProvider::new(vec![
        read_file_response("c1", "big.txt", 1000),
        read_file_response("c2", "big.txt", 1000),
        read_file_response("c3", "big.txt", 1000),
        read_file_response("c4", "big.txt", 1000),
        done_response(),
    ]);
    let captured = provider.captured_messages.clone();

    let mut validated = test_validated(&dir, &prompt_path);
    validated.context_limit = Some(100);

    run_single_with(validated, Box::new(provider), &dir, "summarize big.txt").await;

    let events = _guard.drain_events();

    assert!(
        events.iter().any(|e| matches!(
            e,
            GantryEvent::HistoryCompacted { results_elided, .. } if *results_elided >= 1
        )),
        "expected at least one HistoryCompacted event with results_elided >= 1"
    );

    // The last complete call must have seen at least one ToolResults message
    // whose content is already a compaction stub (starts with "[gantry: tool result").
    let msgs = captured.lock().unwrap();
    let last_call = msgs.last().expect("at least one complete call expected");
    let has_stub = last_call.iter().any(|msg| {
        if let ChatMessage::ToolResults(results) = msg {
            results
                .iter()
                .any(|r| r.content.starts_with("[gantry: tool result"))
        } else {
            false
        }
    });
    assert!(
        has_stub,
        "last complete call should contain at least one stub ToolResult"
    );
}

// ---------------------------------------------------------------------------
// Test 2: no compaction when context_limit is None
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_compaction_when_limit_unset() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();
    let big_content = big_txt_content();
    std::fs::write(dir.path().join("big.txt"), &big_content).unwrap();
    let prompt_path = dir.path().join("prompt.md");
    std::fs::write(&prompt_path, "summarize big.txt").unwrap();

    let provider = StubProvider::new(vec![
        read_file_response("c1", "big.txt", 1000),
        read_file_response("c2", "big.txt", 1000),
        read_file_response("c3", "big.txt", 1000),
        read_file_response("c4", "big.txt", 1000),
        done_response(),
    ]);
    let captured = provider.captured_messages.clone();

    let mut validated = test_validated(&dir, &prompt_path);
    validated.context_limit = None;

    run_single_with(validated, Box::new(provider), &dir, "summarize big.txt").await;

    let events = _guard.drain_events();

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GantryEvent::HistoryCompacted { .. })),
        "expected no HistoryCompacted event when context_limit is None"
    );

    let msgs = captured.lock().unwrap();
    let stub_prefix = "[gantry: tool result";
    let any_stub = msgs.iter().any(|call_msgs| {
        call_msgs.iter().any(|msg| {
            if let ChatMessage::ToolResults(results) = msg {
                results.iter().any(|r| r.content.starts_with(stub_prefix))
            } else {
                false
            }
        })
    });
    assert!(
        !any_stub,
        "no captured messages should contain a stub when context_limit is None"
    );
}

// ---------------------------------------------------------------------------
// Test 3: elided content is retrievable via the retrieve tool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn elided_result_is_retrievable() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();
    let big_content = big_txt_content();
    std::fs::write(dir.path().join("big.txt"), &big_content).unwrap();
    let prompt_path = dir.path().join("prompt.md");
    std::fs::write(&prompt_path, "summarize big.txt").unwrap();

    // The compaction module stores the ToolResult content under
    // mint_handle("history", content). read_file returns the raw file bytes as
    // a string; compress passes big.txt through unchanged (30 lines < 500-line
    // head+tail cap, non-noisy tool). So the stashed content == big_content.
    let expected_handle = gantry::tools::retrieval::mint_handle("history", &big_content);

    let provider = StubProvider::new(vec![
        // 4 read_file turns: after the 4th ToolResults is inserted, compaction
        // elides ToolResults[0] and stores it under expected_handle.
        read_file_response("c1", "big.txt", 1000),
        read_file_response("c2", "big.txt", 1000),
        read_file_response("c3", "big.txt", 1000),
        read_file_response("c4", "big.txt", 1000),
        // Ask the harness to run retrieve with the expected handle.
        ProviderResponse {
            text: String::new(),
            tool_calls: vec![ToolCallRequest {
                id: "c5".into(),
                name: "retrieve".into(),
                args_json: format!(r#"{{"handle":"{}","start":1}}"#, expected_handle),
            }],
            input_tokens: 1000,
            output_tokens: 1,
            cache_read: 0,
            cache_write: 0,
        },
        done_response(),
    ]);

    let mut validated = test_validated(&dir, &prompt_path);
    validated.context_limit = Some(100);

    run_single_with(validated, Box::new(provider), &dir, "summarize big.txt").await;

    let events = _guard.drain_events();

    // Locate the ToolResult event for the retrieve call.
    let retrieve_event = events
        .iter()
        .find(|e| matches!(e, GantryEvent::ToolResult { tool, .. } if tool == "retrieve"));
    assert!(
        retrieve_event.is_some(),
        "expected a ToolResult event for the retrieve call; events: {events:#?}"
    );
    assert!(
        matches!(
            retrieve_event.unwrap(),
            GantryEvent::ToolResult { error: None, .. }
        ),
        "retrieve should succeed with error == None; event: {retrieve_event:#?}"
    );
}

// ---------------------------------------------------------------------------
// Test 4: compaction triggers on TOTAL context occupancy, not uncached
// input_tokens alone. Regression guard for the prompt-caching trigger bug:
// once the prefix is cached, resp.input_tokens collapses to near-zero while the
// real context lives in cache_read — an input_tokens-only check never fires.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn compaction_triggers_on_cached_context_not_just_input_tokens() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("big.txt"), big_txt_content()).unwrap();
    let prompt_path = dir.path().join("prompt.md");
    std::fs::write(&prompt_path, "summarize big.txt").unwrap();

    // Uncached input_tokens (50) is BELOW the limit (1000) on every turn, but the
    // cached prefix (cache_read=5000) puts true occupancy at 5050, far above it.
    let cached = |id: &str| ProviderResponse {
        text: String::new(),
        tool_calls: vec![ToolCallRequest {
            id: id.into(),
            name: "read_file".into(),
            args_json: r#"{"path":"big.txt"}"#.into(),
        }],
        input_tokens: 50,
        output_tokens: 1,
        cache_read: 5000,
        cache_write: 0,
    };
    let provider = StubProvider::new(vec![
        cached("c1"),
        cached("c2"),
        cached("c3"),
        cached("c4"),
        done_response(),
    ]);

    let mut validated = test_validated(&dir, &prompt_path);
    validated.context_limit = Some(1000);

    run_single_with(validated, Box::new(provider), &dir, "summarize big.txt").await;

    let events = _guard.drain_events();
    assert!(
        events.iter().any(|e| matches!(
            e,
            GantryEvent::HistoryCompacted { results_elided, .. } if *results_elided >= 1
        )),
        "compaction must fire on total occupancy: input_tokens=50 < limit=1000, but \
         input_tokens + cache_read = 5050 > 1000 (input_tokens-only trigger would miss this)"
    );
}
