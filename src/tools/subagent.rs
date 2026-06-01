use crate::events::{now_ms, GantryEvent};
use crate::meter::TokenMeter;
use crate::provider::{ChatMessage, ProviderAdapter, ToolResult};
use crate::tools::ToolRegistry;
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

const SUBAGENT_MAX_TURNS: u32 = 5;

/// Max model turns a subagent may take *within one round* to use tools before
/// it must produce its report. Bounds tool-loop cost per round.
const SUBAGENT_MAX_TOOL_TURNS: u32 = 8;

/// Assemble a subagent's system prompt: the profile-supplied `base` plus the
/// per-spawn `role` / `scope` / `extra_context` as neutral labeled sections.
/// All task-specific framing (output format, constraints, etc.) lives in the
/// `base`, which the orchestrator supplies via the profile (ADR-0004).
fn build_subagent_system_prompt(
    base: &str,
    role: &str,
    scope: &str,
    extra_context: Option<&str>,
) -> String {
    let mut prompt = String::from(base);
    if !role.is_empty() {
        prompt.push_str("\n\n## Role\n");
        prompt.push_str(role);
    }
    if !scope.is_empty() {
        prompt.push_str("\n\n## Scope\n");
        prompt.push_str(scope);
    }
    if let Some(extra) = extra_context.filter(|s| !s.is_empty()) {
        prompt.push_str("\n\n");
        prompt.push_str(extra);
    }
    prompt
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SpawnSubagentArgs {
    pub name: String,
    pub role: String,     // "correctness" | "conventions" | etc.
    pub template: String, // template skill name
    pub scope: String,
    #[serde(default)]
    pub extra_context: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CollectOutputsArgs {
    pub round: u32,
    /// Per-call barrier cap. `0` = wait until each subagent reports (bounded by
    /// the global run timeout / cancellation).
    #[serde(default)]
    pub timeout_ms: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct BroadcastSummaryArgs {
    pub round: u32,
    pub summary: String,
}

/// Subagent state held by the coordinator. spawn_subagent adds to roster;
/// collect_outputs drains assistant_text per subagent; broadcast_summary feeds
/// summary back to all subagents as a user-turn for round 2.
pub struct SubagentRoster {
    pub subagents: Mutex<Vec<SubagentHandle>>,
}

pub struct SubagentHandle {
    pub name: String,
    pub role: String,
    pub messages_tx: tokio::sync::mpsc::UnboundedSender<String>,
    pub outputs_rx: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<String>>,
    pub join: tokio::task::JoinHandle<()>,
}

impl SubagentRoster {
    pub fn new() -> Self {
        Self {
            subagents: Mutex::new(Vec::new()),
        }
    }

    pub async fn spawn_subagent(
        &self,
        args: SpawnSubagentArgs,
        provider: Arc<dyn ProviderAdapter>,
        registry: Arc<ToolRegistry>,
        system_template: String,
        meter: Arc<TokenMeter>,
    ) -> Result<String, String> {
        let (msg_tx, mut msg_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let (find_tx, find_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        let name = args.name.clone();
        let role = args.role.clone();
        let _ = GantryEvent::SubagentSpawn {
            ts: now_ms(),
            name: name.clone(),
            template: args.template.clone(),
            scope: args.scope.clone(),
        }
        .emit();

        let subagent_name = name.clone();
        let cancel = meter.cancellation_token();
        let subagent_system = build_subagent_system_prompt(
            &system_template,
            &args.role,
            &args.scope,
            args.extra_context.as_deref(),
        );
        let join = tokio::spawn(async move {
            // First user turn carries the assignment; the "Role: " prefix stays so
            // the subagent's scope is explicit (and tests can detect a subagent turn).
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

                // Bounded tool loop within the round: call the model, dispatch any
                // tool calls, repeat until it returns a text-only report (or cap hit).
                let mut report = String::new();
                for _ in 0..SUBAGENT_MAX_TOOL_TURNS {
                    // Catch panics so one subagent cannot take down the run (invariant #5).
                    let attempt =
                        std::panic::AssertUnwindSafe(provider.complete(&subagent_system, &messages, &tools))
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
                            break 'rounds;
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

                // Barrier: exactly one report per round (possibly empty if tripped).
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

        self.subagents.lock().await.push(SubagentHandle {
            name: name.clone(),
            role,
            messages_tx: msg_tx,
            outputs_rx: tokio::sync::Mutex::new(find_rx),
            join,
        });
        Ok(format!("subagent spawned: {name}"))
    }

    pub async fn broadcast_summary(&self, args: BroadcastSummaryArgs) -> Result<String, String> {
        let roster = self.subagents.lock().await;
        for r in roster.iter() {
            let _ = r
                .messages_tx
                .send(format!("Round {} summary:\n{}", args.round, args.summary));
        }
        Ok(format!("broadcast to {} subagents", roster.len()))
    }

    /// Round barrier: block until each subagent produces its round report (or
    /// `timeout_ms` elapses), then return name-sorted structured results. A
    /// closed channel means the subagent ended without reporting (error, or
    /// `cancelled` when the run token is tripped).
    pub async fn collect_outputs(
        &self,
        args: CollectOutputsArgs,
        cancel: &CancellationToken,
    ) -> Result<String, String> {
        let roster = self.subagents.lock().await;
        let mut order: Vec<&SubagentHandle> = roster.iter().collect();
        order.sort_by(|a, b| a.name.cmp(&b.name));
        let mut subagents = Vec::with_capacity(order.len());
        for r in order {
            let mut rx = r.outputs_rx.lock().await;
            let (status, report) = if args.timeout_ms > 0 {
                match tokio::time::timeout(
                    std::time::Duration::from_millis(args.timeout_ms),
                    rx.recv(),
                )
                .await
                {
                    Ok(Some(text)) => ("complete", text),
                    Ok(None) if cancel.is_cancelled() => ("cancelled", String::new()),
                    Ok(None) => ("error", String::new()),
                    Err(_) => ("timeout", String::new()),
                }
            } else {
                match rx.recv().await {
                    Some(text) => ("complete", text),
                    None if cancel.is_cancelled() => ("cancelled", String::new()),
                    None => ("error", String::new()),
                }
            };
            subagents.push(serde_json::json!({
                "name": r.name,
                "status": status,
                "report": report,
            }));
        }
        Ok(serde_json::json!({ "round": args.round, "subagents": subagents }).to_string())
    }

    /// Terminate all spawned subagents and wait for them to finish. Dropping a
    /// subagent's input channel makes its loop's `recv` return `None`, so it
    /// breaks and emits `subagent_done`; joining the task guarantees that event
    /// is observed. Called after the final round so every `subagent_done`
    /// precedes the coordinator's unify fence and no task outlives the run.
    pub async fn shutdown_and_join(&self) {
        let handles = std::mem::take(&mut *self.subagents.lock().await);
        for handle in handles {
            let SubagentHandle {
                messages_tx, join, ..
            } = handle;
            drop(messages_tx);
            let _ = join.await;
        }
    }
}

impl Default for SubagentRoster {
    fn default() -> Self {
        Self::new()
    }
}
