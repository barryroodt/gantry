use crate::cli::Validated;
use crate::events::{now_ms, ErrorKind, ExitCode, GantryEvent};
use crate::meter::TokenMeter;
use crate::mode::agent_loop::LoopDriver;
use crate::mode::ModeRunOutcome;
use crate::provider::{build_adapter, ChatMessage, ProviderAdapter, ToolSchema};
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

        // 1. Compose the reviewer plan (structured, metered).
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
            return self.collapse("compose produced no reviewers");
        }

        // 2. Spawn one subagent per plan entry.
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
                        template: r.role.clone(),
                        scope: r.scope.clone(),
                        extra_context: r.extra_context.clone(),
                    },
                    self.provider.clone(),
                    self.registry.clone(),
                    subagent_template.clone(),
                    self.meter.clone(),
                )
                .await;
            self.spawned_subagents += 1;
        }

        // 3. Barrier rounds (LoopDriver, cap MAX_ROUNDS): collect, then digest +
        //    broadcast between rounds.
        let driver = LoopDriver::new(MAX_ROUNDS);
        let mut reports = String::new();
        for round in 1..=driver.max_iterations {
            if self.cancel.is_cancelled() {
                return self.cancel_exit();
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
                Err(e) => return self.collapse(&e),
            };
            if round == 1 && self.team_collapsed(&reports) {
                return self.collapse("all subagents crashed or produced no output");
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

        // 4. Unify into findings (structured, metered); emit the JSON fence.
        let unify_system =
            self.phase_system(&system_prefix, self.validated.unify_prompt.as_deref());
        let unify_msgs = vec![ChatMessage::User(format!(
            "{}\n\n# Reviewer reports\n{reports}",
            self.prompt
        ))];
        let findings = match self
            .structured_call(&unify_system, &unify_msgs, &findings_schema())
            .await
        {
            Ok(v) => v,
            Err(exit) => return exit,
        };
        let fence = format!(
            "```json\n{}\n```",
            serde_json::to_string_pretty(&findings).unwrap_or_else(|_| findings.to_string())
        );
        let _ = GantryEvent::AssistantText {
            ts: now_ms(),
            role: COORDINATOR_ROLE.into(),
            text: fence,
        }
        .emit();

        ExitCode::Ok
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
        let mut last_text = String::new();
        for _ in 0..2 {
            if self.cancel.is_cancelled() {
                return Err(self.cancel_exit());
            }
            let resp = tokio::select! {
                r = self.provider.complete(system, messages, std::slice::from_ref(&respond)) => r,
                _ = self.cancel.cancelled() => return Err(self.cancel_exit()),
            };
            let resp = match resp {
                Ok(r) => r,
                Err(err) => {
                    let _ = GantryEvent::Error {
                        ts: now_ms(),
                        kind: ErrorKind::Provider,
                        message: err.to_string(),
                    }
                    .emit();
                    return Err(ExitCode::Error);
                }
            };
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
        let _ = GantryEvent::Error {
            ts: now_ms(),
            kind: ErrorKind::Provider,
            message: "structured output: no respond tool call and no JSON fence".into(),
        }
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
        let _ = GantryEvent::Error {
            ts: now_ms(),
            kind: ErrorKind::TeamCollapse,
            message: message.into(),
        }
        .emit();
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
    reviewers: Vec<ReviewerPlan>,
}

#[derive(Debug, Deserialize)]
struct ReviewerPlan {
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

/// Parse a compose result into a reviewer plan (tolerant: invalid → empty).
fn parse_plan(value: &Value) -> Vec<ReviewerPlan> {
    serde_json::from_value::<ComposePlan>(value.clone())
        .map(|p| p.reviewers)
        .unwrap_or_default()
}

fn plan_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "reviewers": {
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
        "required": ["reviewers"]
    })
}

fn findings_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "summary": {"type": "string"},
            "verdict": {"type": "string"},
            "findings": {"type": "array"},
            "strengths": {"type": "array"}
        },
        "required": ["summary", "verdict", "findings"]
    })
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
    let mut out = String::from("# Cross-review digest\n");
    if let Some(arr) = v.get("subagents").and_then(|s| s.as_array()) {
        for r in arr {
            let name = r.get("name").and_then(|n| n.as_str()).unwrap_or("?");
            let report = r.get("report").and_then(|n| n.as_str()).unwrap_or("");
            out.push_str(&format!("\n## {name}\n{report}\n"));
        }
    }
    out
}

fn outcome(exit: ExitCode, meter: &TokenMeter) -> ModeRunOutcome {
    ModeRunOutcome {
        exit,
        meter: meter.snapshot(),
    }
}

/// Public entry point used by main.rs.
pub async fn run_team(validated: Validated) -> ModeRunOutcome {
    use crate::cancel::shared_token;
    use crate::cancel::spawn_timeout_watchdog;

    let cancel = shared_token();
    let meter = Arc::new(TokenMeter::new(validated.max_tokens, cancel.clone()));
    let _watchdog = spawn_timeout_watchdog(cancel.clone(), validated.timeout_ms);
    let _signal = crate::cancel::spawn_signal_handler(cancel.clone());

    let provider: Arc<dyn ProviderAdapter> =
        match build_adapter(validated.provider.clone(), validated.model.clone()) {
            Ok(p) => Arc::from(p),
            Err(err) => {
                let _ = GantryEvent::Error {
                    ts: now_ms(),
                    kind: ErrorKind::Config,
                    message: err.to_string(),
                }
                .emit();
                return outcome(ExitCode::Config, &meter);
            }
        };

    let prompt = match std::fs::read_to_string(&validated.prompt_file) {
        Ok(p) => p,
        Err(err) => {
            let _ = GantryEvent::Error {
                ts: now_ms(),
                kind: ErrorKind::Config,
                message: format!("prompt file: {err}"),
            }
            .emit();
            return outcome(ExitCode::Config, &meter);
        }
    };

    let roster = Arc::new(SubagentRoster::new());
    let registry = ToolRegistry::team(
        validated.workdir.clone(),
        roster.clone(),
        provider.clone(),
        validated
            .subagent_system_prompt
            .clone()
            .unwrap_or_else(|| crate::mode::DEFAULT_SUBAGENT_SYSTEM.to_string()),
        meter.clone(),
        validated.tools.clone(),
    );
    let skill_loader = SkillLoader::new(validated.workdir.clone());

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
