use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use gantry::cancel::shared_token;
use gantry::cli::Provider;
use gantry::emitter::TestEmitterGuard;
use gantry::events::GantryEvent;
use gantry::meter::TokenMeter;
use gantry::provider::{
    ChatMessage, ProviderAdapter, ProviderResponse, ToolCallRequest, ToolSchema,
};
use gantry::tools::subagent::{
    BroadcastSummaryArgs, CollectOutputsArgs, SpawnSubagentArgs, SubagentRoster,
};
use gantry::tools::ToolRegistry;
use tempfile::TempDir;

/// A meter with effectively unbounded budget for tests that do not exercise the cap.
fn test_meter() -> Arc<TokenMeter> {
    Arc::new(TokenMeter::new(1_000_000, shared_token()))
}

/// Stateless-after-construction provider keyed on the subagent role and round.
/// `complete` only reads `responses`, so no interior mutability is needed.
struct RoleTextProvider {
    /// Maps role string -> list of responses (first round, second round, …).
    responses: HashMap<String, Vec<String>>,
}

impl RoleTextProvider {
    fn new() -> Self {
        Self {
            responses: HashMap::new(),
        }
    }

    fn with_role(mut self, role: &str, texts: Vec<&str>) -> Self {
        self.responses.insert(
            role.to_string(),
            texts.into_iter().map(String::from).collect(),
        );
        self
    }
}

#[async_trait]
impl ProviderAdapter for RoleTextProvider {
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
        let role = messages
            .iter()
            .find_map(|m| match m {
                ChatMessage::User(text) if text.starts_with("Role: ") => text
                    .lines()
                    .next()
                    .map(|line| line.trim_start_matches("Role: ").to_string()),
                _ => None,
            })
            .unwrap_or_else(|| "unknown".into());

        if role == "panic-role" {
            panic!("subagent task panic");
        }

        let round_index = messages
            .iter()
            .filter(|m| matches!(m, ChatMessage::User(_)))
            .count()
            .saturating_sub(1);

        let text = self
            .responses
            .get(&role)
            .and_then(|v| v.get(round_index))
            .cloned()
            .unwrap_or_else(|| format!("finding for {role} round {round_index}"));

        Ok(ProviderResponse {
            text,
            tool_calls: vec![],
            input_tokens: 1,
            output_tokens: 1,
            cache_read: 0,
            cache_write: 0,
        })
    }
}

/// Spawn one subagent through the roster (the post-ADR-0005 mechanism the
/// harness drives directly — no registry tool dispatch). Returns the spawn ack.
async fn spawn_subagent(
    roster: &SubagentRoster,
    provider: &Arc<dyn ProviderAdapter>,
    registry: &Arc<ToolRegistry>,
    meter: &Arc<TokenMeter>,
    name: &str,
    role: &str,
) -> String {
    roster
        .spawn_subagent(
            SpawnSubagentArgs {
                name: name.into(),
                role: role.into(),
                template: role.into(),
                scope: "full".into(),
                extra_context: None,
            },
            provider.clone(),
            registry.clone(),
            "subagent system".into(),
            meter.clone(),
        )
        .await
        .expect("spawn_subagent")
}

#[tokio::test]
async fn spawn_subagent_adds_to_roster_and_emits_subagent_spawn() {
    let guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();
    let roster = Arc::new(SubagentRoster::new());
    let provider: Arc<dyn ProviderAdapter> =
        Arc::new(RoleTextProvider::new().with_role("correctness", vec!["round-1 report"]));
    let registry = Arc::new(ToolRegistry::new(dir.path().to_path_buf(), vec![]));
    let meter = test_meter();

    let ack = spawn_subagent(
        &roster,
        &provider,
        &registry,
        &meter,
        "correctness",
        "correctness",
    )
    .await;
    assert!(ack.contains("subagent spawned: correctness"));
    assert_eq!(roster.subagents.lock().await.len(), 1);

    let events = guard.drain_events();
    assert!(
        events.iter().any(
            |e| matches!(e, GantryEvent::SubagentSpawn { name, template, scope, .. }
                if name == "correctness" && template == "correctness" && scope == "full")
        ),
        "expected subagent_spawn event, got: {events:?}"
    );
}

#[tokio::test]
async fn broadcast_summary_delivers_to_all_spawned_subagents() {
    let dir = TempDir::new().unwrap();
    let roster = Arc::new(SubagentRoster::new());
    let provider: Arc<dyn ProviderAdapter> = Arc::new(
        RoleTextProvider::new()
            .with_role(
                "correctness",
                vec!["first correctness", "after broadcast correctness"],
            )
            .with_role(
                "spec-compliance",
                vec!["first spec", "after broadcast spec"],
            ),
    );
    let registry = Arc::new(ToolRegistry::new(dir.path().to_path_buf(), vec![]));
    let meter = test_meter();

    spawn_subagent(
        &roster,
        &provider,
        &registry,
        &meter,
        "correctness",
        "correctness",
    )
    .await;
    spawn_subagent(
        &roster,
        &provider,
        &registry,
        &meter,
        "spec-compliance",
        "spec-compliance",
    )
    .await;

    let round1 = roster
        .collect_outputs(
            CollectOutputsArgs {
                round: 1,
                timeout_ms: 0,
            },
            &shared_token(),
        )
        .await
        .unwrap();
    assert!(round1.contains("first correctness"));
    assert!(round1.contains("first spec"));

    let broadcast = roster
        .broadcast_summary(BroadcastSummaryArgs {
            round: 1,
            summary: "cross-review digest".into(),
        })
        .await
        .unwrap();
    assert!(broadcast.contains("broadcast to 2 subagents"));

    let round2 = roster
        .collect_outputs(
            CollectOutputsArgs {
                round: 2,
                timeout_ms: 0,
            },
            &shared_token(),
        )
        .await
        .unwrap();
    assert!(round2.contains("after broadcast correctness"));
    assert!(round2.contains("after broadcast spec"));
}

#[tokio::test]
async fn collect_outputs_drains_text_from_finished_subagents_in_order() {
    let dir = TempDir::new().unwrap();
    let roster = Arc::new(SubagentRoster::new());
    let provider: Arc<dyn ProviderAdapter> = Arc::new(
        RoleTextProvider::new()
            .with_role("alpha", vec!["alpha report"])
            .with_role("beta", vec!["beta report"]),
    );
    let registry = Arc::new(ToolRegistry::new(dir.path().to_path_buf(), vec![]));
    let meter = test_meter();

    spawn_subagent(&roster, &provider, &registry, &meter, "alpha", "alpha").await;
    spawn_subagent(&roster, &provider, &registry, &meter, "beta", "beta").await;

    let out = roster
        .collect_outputs(
            CollectOutputsArgs {
                round: 1,
                timeout_ms: 0,
            },
            &shared_token(),
        )
        .await
        .unwrap();
    let alpha_pos = out.find("alpha report").expect("alpha report");
    let beta_pos = out.find("beta report").expect("beta report");
    assert!(
        alpha_pos < beta_pos,
        "expected name-sorted order, got: {out}"
    );
}

#[tokio::test]
async fn partial_failure_one_subagent_panics_broadcast_and_collect_still_work() {
    let dir = TempDir::new().unwrap();
    let roster = Arc::new(SubagentRoster::new());
    let provider: Arc<dyn ProviderAdapter> = Arc::new(
        RoleTextProvider::new()
            .with_role("panic-role", vec!["never emitted"])
            .with_role("healthy", vec!["healthy round 1", "healthy round 2"]),
    );
    let registry = Arc::new(ToolRegistry::new(dir.path().to_path_buf(), vec![]));
    let meter = test_meter();

    spawn_subagent(
        &roster,
        &provider,
        &registry,
        &meter,
        "broken",
        "panic-role",
    )
    .await;
    spawn_subagent(&roster, &provider, &registry, &meter, "healthy", "healthy").await;

    let round1 = roster
        .collect_outputs(
            CollectOutputsArgs {
                round: 1,
                timeout_ms: 0,
            },
            &shared_token(),
        )
        .await
        .unwrap();
    assert!(round1.contains("healthy round 1"));
    assert!(!round1.contains("never emitted"));

    let broadcast = roster
        .broadcast_summary(BroadcastSummaryArgs {
            round: 1,
            summary: "digest".into(),
        })
        .await
        .unwrap();
    assert!(broadcast.contains("broadcast to 2 subagents"));

    let round2 = roster
        .collect_outputs(
            CollectOutputsArgs {
                round: 2,
                timeout_ms: 0,
            },
            &shared_token(),
        )
        .await
        .unwrap();
    assert!(round2.contains("healthy round 2"));
}

#[tokio::test]
async fn orchestration_tools_are_not_dispatchable() {
    // After ADR-0005 the orchestration ops are roster methods, not LLM tools:
    // the registry no longer dispatches them in any mode.
    let registry = ToolRegistry::new(std::env::temp_dir(), vec![]);
    let out = registry
        .dispatch(
            "coordinator",
            1,
            "spawn_subagent",
            r#"{"name":"x","role":"x","template":"x","scope":"full"}"#,
        )
        .await;
    assert!(
        out.content.contains("unknown tool: spawn_subagent"),
        "orchestration name should not be a dispatchable tool: {}",
        out.content
    );
}

#[tokio::test]
async fn collect_outputs_returns_structured_name_sorted_status() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();
    let roster = Arc::new(SubagentRoster::new());
    let provider: Arc<dyn ProviderAdapter> = Arc::new(
        RoleTextProvider::new()
            .with_role("alpha", vec!["alpha report"])
            .with_role("beta", vec!["beta report"]),
    );
    let registry = Arc::new(ToolRegistry::new(dir.path().to_path_buf(), vec![]));
    let meter = test_meter();

    // Spawn out of name order to prove collect_outputs sorts by name.
    spawn_subagent(&roster, &provider, &registry, &meter, "beta", "beta").await;
    spawn_subagent(&roster, &provider, &registry, &meter, "alpha", "alpha").await;

    let c = roster
        .collect_outputs(
            CollectOutputsArgs {
                round: 1,
                timeout_ms: 0,
            },
            &shared_token(),
        )
        .await
        .unwrap();
    let ai = c.find(r#""name":"alpha""#).expect("alpha present");
    let bi = c.find(r#""name":"beta""#).expect("beta present");
    assert!(ai < bi, "subagents not name-sorted: {c}");
    assert!(c.contains(r#""status":"complete""#), "missing status: {c}");
    assert!(
        c.contains("alpha report") && c.contains("beta report"),
        "missing reports: {c}"
    );
}

struct SlowProvider {
    delay_ms: u64,
}

#[async_trait]
impl ProviderAdapter for SlowProvider {
    fn provider(&self) -> Provider {
        Provider::OpenAi
    }
    fn model(&self) -> &str {
        "slow"
    }
    async fn complete(
        &self,
        _system: &str,
        _messages: &[ChatMessage],
        _tools: &[ToolSchema],
    ) -> anyhow::Result<ProviderResponse> {
        tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
        Ok(ProviderResponse {
            text: "late".into(),
            tool_calls: vec![],
            input_tokens: 1,
            output_tokens: 1,
            cache_read: 0,
            cache_write: 0,
        })
    }
}

#[tokio::test]
async fn collect_outputs_times_out_slow_subagent() {
    let _guard = TestEmitterGuard::install();
    let dir = TempDir::new().unwrap();
    let roster = Arc::new(SubagentRoster::new());
    let provider: Arc<dyn ProviderAdapter> = Arc::new(SlowProvider { delay_ms: 1000 });
    let registry = Arc::new(ToolRegistry::new(dir.path().to_path_buf(), vec![]));
    let meter = test_meter();

    spawn_subagent(&roster, &provider, &registry, &meter, "slow", "slow").await;

    let out = roster
        .collect_outputs(
            CollectOutputsArgs {
                round: 1,
                timeout_ms: 50,
            },
            &shared_token(),
        )
        .await
        .unwrap();
    assert!(
        out.contains(r#""status":"timeout""#),
        "expected timeout status: {out}"
    );
}

/// Calls `read_file` on the first turn, then (once the tool result arrives)
/// reports the content it received back — proving the subagent dispatched the
/// tool and fed the result into its next turn.
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
        if let Some(ChatMessage::ToolResults(results)) = messages.last() {
            let seen = results
                .first()
                .map(|r| r.content.clone())
                .unwrap_or_default();
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

    spawn_subagent(&roster, &provider, &registry, &meter, "alpha", "alpha").await;

    let round1 = roster
        .collect_outputs(
            CollectOutputsArgs {
                round: 1,
                timeout_ms: 0,
            },
            &shared_token(),
        )
        .await
        .unwrap();
    assert!(
        round1.contains("MARKER_CONTENT_42"),
        "subagent did not dispatch read_file and feed its result into the report: {round1}"
    );
}
