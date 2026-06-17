use crate::cli::Validated;
use crate::events::{now_ms, ErrorKind, ExitCode, GantryEvent};
use crate::meter::TokenMeter;
use crate::mode::agent_loop::LoopDriver;
use crate::mode::{bootstrap, emit_provider_failure, outcome, ModeRunOutcome, RunBootstrap};
use crate::provider::{ChatMessage, ProviderAdapter, ToolSchema};
use crate::skills::SkillLoader;
use crate::tools::subagent::{
    BroadcastSummaryArgs, CollectOutputsArgs, SpawnSubagentArgs, SubagentRoster,
};
use crate::tools::ToolRegistry;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const MAX_ROUNDS: u32 = 2;
const COORDINATOR_ROLE: &str = "coordinator";

pub struct TeamMode {
    pub validated: Validated,
    pub meter: Arc<TokenMeter>,
    pub cancel: CancellationToken,
    pub registry: Arc<ToolRegistry>,
    pub roster: Arc<SubagentRoster>,
    pub skill_loader: SkillLoader,
    pub provider: Arc<dyn ProviderAdapter>,
    pub prompt: String,
    pub spawned_subagents: u32,
}

impl TeamMode {
    /// Run team mode as a deterministic state machine (ADR-0005): compose →
    /// spawn → barrier rounds → unify → emit fence. The LLM is consulted only at
    /// compose and unify; the harness owns spawn/collect/broadcast.
    pub async fn run(mut self) -> ExitCode {
        let system_prefix = self
            .skill_loader
            .inject_core_skills(&self.validated.inject_skills);

        // 1. Compose the subagent plan (structured, metered).
        let compose_system =
            self.phase_system(&system_prefix, self.validated.compose_prompt.as_deref());
        let compose_msgs = vec![ChatMessage::User(self.prompt.clone())];
        let plan = match self
            .structured_call(&compose_system, &compose_msgs, &plan_schema())
            .await
        {
            Ok(v) => parse_plan(&v),
            Err(exit) => return exit,
        };
        if plan.is_empty() {
            return self.collapse("compose produced no subagents");
        }

        // 2. Compute per-subagent budget slice (G6): remaining global budget after
        //    compose, divided evenly across the N spawned subagents. Uses the G7
        //    formula (input + output + cache_write). A slice of 0 means the budget
        //    is already exhausted — every subagent will fail immediately on its
        //    first response.
        // plan is non-empty (empty → early return above), so n ≥ 1 and the division is safe.
        let n = plan.len() as u64;
        let slice = self.meter.remaining() / n;

        // 3. Spawn one subagent per plan entry, each carrying its budget slice.
        let subagent_template = self
            .validated
            .subagent_system_prompt
            .clone()
            .unwrap_or_else(|| crate::mode::DEFAULT_SUBAGENT_SYSTEM.to_string());
        for r in &plan {
            let _ = self
                .roster
                .spawn_subagent(
                    SpawnSubagentArgs {
                        name: r.name.clone(),
                        role: r.role.clone(),
                        scope: r.scope.clone(),
                        extra_context: r.extra_context.clone(),
                    },
                    self.provider.clone(),
                    self.registry.clone(),
                    subagent_template.clone(),
                    self.meter.clone(),
                    slice,
                )
                .await;
            self.spawned_subagents += 1;
        }

        // 3-4. Drive the barrier rounds + unify, then ALWAYS shut the subagents
        //      down and join them: this makes every `subagent_done` fire before
        //      the coordinator's fence and ensures no detached task outlives the
        //      run (ADR-0005 validation).
        let result = self.rounds_and_unify(&system_prefix).await;
        self.roster.shutdown_and_join().await;

        let unified = match result {
            Ok(v) => v,
            Err(exit) => return exit,
        };
        let fence = format!(
            "```json\n{}\n```",
            serde_json::to_string_pretty(&unified).unwrap_or_else(|_| unified.to_string())
        );
        let _ = GantryEvent::AssistantText {
            ts: now_ms(),
            role: COORDINATOR_ROLE.into(),
            text: fence,
        }
        .emit();
        ExitCode::Ok
    }

    /// Barrier rounds (LoopDriver, cap `MAX_ROUNDS`: collect, then digest +
    /// broadcast between rounds) followed by the unify call. Returns the
    /// validated result object, or the terminal exit on collapse / budget /
    /// cancel. Does not emit the fence — `run` does, after shutting subagents
    /// down — so the ordering invariant holds.
    async fn rounds_and_unify(&self, system_prefix: &str) -> Result<Value, ExitCode> {
        let driver = LoopDriver::new(MAX_ROUNDS);
        let mut reports = String::new();
        for round in 1..=driver.max_iterations {
            if self.cancel.is_cancelled() {
                return Err(self.cancel_exit());
            }
            reports = match self
                .roster
                .collect_outputs(
                    CollectOutputsArgs {
                        round,
                        timeout_ms: 0,
                    },
                    &self.cancel,
                )
                .await
            {
                Ok(r) => r,
                Err(e) => return Err(self.collapse(&e)),
            };
            if round == 1 && self.team_collapsed(&reports) {
                return Err(self.collapse("all subagents crashed or produced no output"));
            }
            if !driver.is_final_round(round) {
                let _ = self
                    .roster
                    .broadcast_summary(BroadcastSummaryArgs {
                        round,
                        summary: digest_of(&reports),
                    })
                    .await;
            }
        }

        let unify_system = self.phase_system(system_prefix, self.validated.unify_prompt.as_deref());
        let unify_msgs = vec![ChatMessage::User(format!(
            "{}\n\n# Subagent reports\n{reports}",
            self.prompt
        ))];
        self.structured_call(&unify_system, &unify_msgs, &result_schema())
            .await
    }

    /// Phase system prompt: skills prefix + the phase prompt (or the profile's
    /// system prompt, or the neutral default).
    fn phase_system(&self, prefix: &str, phase_prompt: Option<&str>) -> String {
        let body = phase_prompt
            .or(self.validated.system_prompt.as_deref())
            .unwrap_or(crate::mode::DEFAULT_SYSTEM_PROMPT);
        format!("{prefix}\n{body}")
    }

    /// One structured model call: metered, one retry, JSON-fence fallback.
    /// `Err(exit)` on budget / timeout / provider failure.
    async fn structured_call(
        &self,
        system: &str,
        messages: &[ChatMessage],
        schema: &Value,
    ) -> Result<Value, ExitCode> {
        let respond = ToolSchema {
            name: "respond".into(),
            description: "Return the final structured result as this tool's arguments.".into(),
            json_schema: schema.clone(),
        };
        // Belt-and-suspenders structured output: offer the `respond` tool AND
        // instruct the model in-prompt to emit one ```json fence conforming to
        // the schema, so providers that don't honor the forced tool still yield
        // a parseable result for the fence fallback below.
        let schema_pretty =
            serde_json::to_string_pretty(schema).unwrap_or_else(|_| schema.to_string());
        let directive = format!(
            "{system}\n\n# Output format\nReturn ONLY a single ```json fenced code block \
             — no prose before or after — whose contents are a JSON object conforming to \
             this schema:\n```json\n{schema_pretty}\n```"
        );
        let mut last_text = String::new();
        for turn in 0..2u32 {
            if self.cancel.is_cancelled() {
                return Err(self.cancel_exit());
            }
            let call_start = std::time::Instant::now();
            let resp = tokio::select! {
                r = self.provider.complete(&directive, messages, std::slice::from_ref(&respond)) => r,
                _ = self.cancel.cancelled() => return Err(self.cancel_exit()),
            };
            let resp = match resp {
                Ok(r) => r,
                Err(err) => return Err(emit_provider_failure(&err)),
            };
            let call_duration_ms = call_start.elapsed().as_millis() as u64;
            if self
                .meter
                .add(
                    resp.input_tokens,
                    resp.output_tokens,
                    resp.cache_read,
                    resp.cache_write,
                )
                .is_err()
            {
                return Err(ExitCode::Budget);
            }
            let _ = GantryEvent::AgentTurn {
                ts: now_ms(),
                role: COORDINATOR_ROLE.into(),
                turn,
                input_tokens: resp.input_tokens,
                output_tokens: resp.output_tokens,
                cache_read: resp.cache_read,
                cache_write: resp.cache_write,
                duration_ms: call_duration_ms,
            }
            .emit();
            if let Some(call) = resp.tool_calls.iter().find(|c| c.name == "respond") {
                if let Ok(v) = serde_json::from_str::<Value>(&call.args_json) {
                    return Ok(v);
                }
            }
            last_text = resp.text;
        }
        if let Some(v) = extract_json_fence(&last_text) {
            return Ok(v);
        }
        let _ = GantryEvent::unrecoverable(
            ErrorKind::Provider,
            "structured output: no respond tool call and no JSON fence",
        )
        .emit();
        Err(ExitCode::Error)
    }

    fn cancel_exit(&self) -> ExitCode {
        if self.meter.tripped() {
            ExitCode::Budget
        } else {
            ExitCode::Timeout
        }
    }

    fn collapse(&self, message: &str) -> ExitCode {
        let _ = GantryEvent::unrecoverable(ErrorKind::TeamCollapse, message).emit();
        ExitCode::Error
    }

    fn team_collapsed(&self, collect_output: &str) -> bool {
        if self.spawned_subagents == 0 {
            return false;
        }
        serde_json::from_str::<Value>(collect_output)
            .ok()
            .and_then(|v| {
                v.get("subagents").and_then(|s| s.as_array()).map(|arr| {
                    !arr.iter()
                        .any(|r| r.get("status").and_then(|s| s.as_str()) == Some("complete"))
                })
            })
            .unwrap_or(true)
    }
}

#[derive(Debug, Deserialize)]
struct ComposePlan {
    #[serde(default)]
    subagents: Vec<SubagentPlan>,
}

#[derive(Debug, Deserialize)]
struct SubagentPlan {
    name: String,
    role: String,
    #[serde(default = "full_scope")]
    scope: String,
    #[serde(default)]
    extra_context: Option<String>,
}

fn full_scope() -> String {
    "full".to_string()
}

/// Parse a compose result into a subagent plan (tolerant: invalid → empty).
fn parse_plan(value: &Value) -> Vec<SubagentPlan> {
    serde_json::from_value::<ComposePlan>(value.clone())
        .map(|p| p.subagents)
        .unwrap_or_default()
}

fn plan_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "subagents": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "role": {"type": "string"},
                        "scope": {"type": "string"},
                        "extra_context": {"type": "string"}
                    },
                    "required": ["name", "role"]
                }
            }
        },
        "required": ["subagents"]
    })
}

/// The unify output schema. Permissive by design — the harness imposes no shape;
/// the profile's `unify.md` prompt defines the structured result it produces.
fn result_schema() -> Value {
    serde_json::json!({"type": "object"})
}

/// Extract and parse the first ```json fenced object from model text.
fn extract_json_fence(text: &str) -> Option<Value> {
    let start = text.find("```json")? + "```json".len();
    let rest = &text[start..];
    let end = rest.find("```")?;
    serde_json::from_str(rest[..end].trim()).ok()
}

/// Round digest broadcast to subagents: each subagent's report under its name.
fn digest_of(collect_output: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(collect_output) else {
        return collect_output.to_string();
    };
    let mut out = String::from("# Round digest\n");
    if let Some(arr) = v.get("subagents").and_then(|s| s.as_array()) {
        for r in arr {
            let name = r.get("name").and_then(|n| n.as_str()).unwrap_or("?");
            let report = r.get("report").and_then(|n| n.as_str()).unwrap_or("");
            out.push_str(&format!("\n## {name}\n{report}\n"));
        }
    }
    out
}

/// Public entry point used by main.rs.
pub async fn run_team(validated: Validated) -> ModeRunOutcome {
    let RunBootstrap {
        cancel,
        meter,
        provider,
        prompt,
        skill_loader,
        watchdog: _watchdog,
        signal: _signal,
    } = match bootstrap(&validated) {
        Ok(b) => b,
        Err(o) => return o,
    };
    let provider: Arc<dyn ProviderAdapter> = Arc::from(provider);
    let roster = Arc::new(SubagentRoster::new());
    let registry = Arc::new(
        ToolRegistry::new(validated.workdir.clone(), validated.tools.clone())
            .with_shell_allow(validated.shell_allow.clone())
            .with_skills_dir(validated.skills_dir.clone()),
    );

    let exit = TeamMode {
        validated,
        meter: meter.clone(),
        cancel,
        registry,
        roster,
        skill_loader,
        provider,
        prompt,
        spawned_subagents: 0,
    }
    .run()
    .await;

    outcome(exit, &meter)
}
