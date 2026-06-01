use crate::events::{now_ms, GantryEvent};
use crate::meter::TokenMeter;
use crate::provider::{ChatMessage, ProviderAdapter};
use crate::tools::ToolRegistry;
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

const SUBAGENT_MAX_TURNS: u32 = 5;

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
        _registry: Arc<ToolRegistry>,
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
        tokio::spawn(async move {
            // The first user turn keeps the "Role: " prefix so the subagent's
            // assignment is explicit in its history.
            let mut messages: Vec<ChatMessage> = vec![ChatMessage::User(format!(
                "Role: {role}\nScope: {scope}\nTemplate: {template}",
                role = args.role,
                scope = args.scope,
                template = args.template,
            ))];
            let mut turn: u32 = 0;
            let mut input_tokens: u64 = 0;
            let mut output_tokens: u64 = 0;

            loop {
                if cancel.is_cancelled() {
                    break;
                }

                // Catch panics so a single subagent cannot take down the run;
                // surface them as `subagent_failed` (invariant #5).
                let attempt = std::panic::AssertUnwindSafe(provider.complete(
                    &subagent_system,
                    &messages,
                    &[],
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
                        break;
                    }
                    Err(_panic) => {
                        let _ = GantryEvent::SubagentFailed {
                            ts: now_ms(),
                            name: subagent_name.clone(),
                            reason: "subagent task panicked".into(),
                        }
                        .emit();
                        break;
                    }
                };

                // Invariant #4: every provider response feeds the shared meter,
                // so subagent tokens count against the run budget.
                input_tokens += resp.input_tokens;
                output_tokens += resp.output_tokens;
                let tripped = meter
                    .add(
                        resp.input_tokens,
                        resp.output_tokens,
                        resp.cache_read,
                        resp.cache_write,
                    )
                    .is_err();

                // Always emit one round result so `collect_outputs` can join on
                // exactly one message per subagent per round (the barrier);
                // surface assistant_text only for non-empty turns.
                let _ = find_tx.send(resp.text.clone());
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

                if tripped || cancel.is_cancelled() {
                    break;
                }

                turn += 1;
                match msg_rx.recv().await {
                    Some(next) => messages.push(ChatMessage::User(next)),
                    None => break,
                }
                if turn >= SUBAGENT_MAX_TURNS {
                    break;
                }
            }

            let _ = GantryEvent::SubagentDone {
                ts: now_ms(),
                name: subagent_name,
                turns: turn,
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
}

impl Default for SubagentRoster {
    fn default() -> Self {
        Self::new()
    }
}
