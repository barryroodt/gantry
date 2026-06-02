use crate::cli::Validated;
use crate::events::{now_ms, ErrorKind, ExitCode, GantryEvent};
use crate::meter::TokenMeter;
use crate::mode::ModeRunOutcome;
use crate::provider::{build_adapter, ChatMessage, ProviderAdapter, ToolResult};
use crate::skills::SkillLoader;
use crate::tools::ToolRegistry;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub struct SingleMode {
    pub validated: Validated,
    pub meter: Arc<TokenMeter>,
    pub cancel: CancellationToken,
    pub registry: ToolRegistry,
    pub skill_loader: SkillLoader,
    pub provider: Box<dyn ProviderAdapter>,
    pub prompt: String,
}

impl SingleMode {
    /// Run the single-mode loop. Always emits a terminal `result` event.
    /// Returns the ExitCode to translate to process exit.
    pub async fn run(self) -> ExitCode {
        let system_prefix = self
            .skill_loader
            .inject_core_skills(&self.validated.inject_skills);
        let body = self
            .validated
            .system_prompt
            .as_deref()
            .unwrap_or(crate::mode::DEFAULT_SYSTEM_PROMPT);
        let system = format!("{system_prefix}\n{body}");
        let pass = run_agent_pass(
            self.provider.as_ref(),
            &self.registry,
            &self.meter,
            &self.cancel,
            &system,
            vec![ChatMessage::User(self.prompt.clone())],
            "single",
            MAX_TURNS,
        )
        .await;
        pass.exit.unwrap_or(ExitCode::Ok)
    }
}

/// Per-pass turn cap shared by single mode and the loop mode's per-iteration pass.
pub const MAX_TURNS: u32 = 20;

/// Outcome of one agent pass. `exit = Some(..)` means the pass hit a terminal
/// condition (budget/timeout/provider error); `None` means it ended normally
/// (the model stopped calling tools, or the turn cap was reached).
pub struct PassResult {
    pub final_text: String,
    pub stop_requested: bool,
    pub exit: Option<ExitCode>,
}

/// Run one bounded agent pass: repeated model calls + tool dispatch until the
/// model stops calling tools, `max_turns` is hit, or budget/cancel fires. Emits
/// the per-turn `agent_turn` / `assistant_text` / tool events. `stop_requested`
/// is set when the model calls the `decide_stop` control tool.
#[allow(clippy::too_many_arguments)]
pub async fn run_agent_pass(
    provider: &dyn ProviderAdapter,
    registry: &ToolRegistry,
    meter: &TokenMeter,
    cancel: &CancellationToken,
    system: &str,
    initial_messages: Vec<ChatMessage>,
    role: &str,
    max_turns: u32,
) -> PassResult {
    let tools = registry.schemas();
    let mut messages = initial_messages;
    let mut turn: u32 = 0;
    let mut final_text = String::new();
    let mut stop_requested = false;

    loop {
        if cancel.is_cancelled() {
            let exit = if meter.tripped() {
                ExitCode::Budget
            } else {
                ExitCode::Timeout
            };
            return PassResult {
                final_text,
                stop_requested,
                exit: Some(exit),
            };
        }
        if turn >= max_turns {
            break;
        }

        let resp_fut = provider.complete(system, &messages, &tools);
        let resp = tokio::select! {
            r = resp_fut => r,
            _ = cancel.cancelled() => {
                let exit = if meter.tripped() { ExitCode::Budget } else { ExitCode::Timeout };
                return PassResult { final_text, stop_requested, exit: Some(exit) };
            }
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
                return PassResult {
                    final_text,
                    stop_requested,
                    exit: Some(ExitCode::Error),
                };
            }
        };

        if meter
            .add(
                resp.input_tokens,
                resp.output_tokens,
                resp.cache_read,
                resp.cache_write,
            )
            .is_err()
        {
            return PassResult {
                final_text,
                stop_requested,
                exit: Some(ExitCode::Budget),
            };
        }

        let _ = GantryEvent::AgentTurn {
            ts: now_ms(),
            role: role.into(),
            turn,
            input_tokens: resp.input_tokens,
            output_tokens: resp.output_tokens,
            cache_read: resp.cache_read,
            cache_write: resp.cache_write,
        }
        .emit();

        if !resp.text.is_empty() {
            final_text = resp.text.clone();
            let _ = GantryEvent::AssistantText {
                ts: now_ms(),
                role: role.into(),
                text: resp.text.clone(),
            }
            .emit();
        }

        if resp.tool_calls.is_empty() {
            break;
        }
        if resp
            .tool_calls
            .iter()
            .any(|c| c.name == crate::tools::decide_stop::DECIDE_STOP)
        {
            stop_requested = true;
        }

        let mut tool_results = Vec::with_capacity(resp.tool_calls.len());
        for call in &resp.tool_calls {
            let out = registry
                .dispatch(role, turn, &call.name, &call.args_json)
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
        turn += 1;
    }

    PassResult {
        final_text,
        stop_requested,
        exit: None,
    }
}

fn outcome(exit: ExitCode, meter: &TokenMeter) -> ModeRunOutcome {
    ModeRunOutcome {
        exit,
        meter: meter.snapshot(),
    }
}

/// Public entry point used by main.rs.
pub async fn run_single(validated: Validated) -> ModeRunOutcome {
    use crate::cancel::shared_token;
    use crate::cancel::spawn_timeout_watchdog;

    let cancel = shared_token();
    let meter = Arc::new(TokenMeter::new(validated.max_tokens, cancel.clone()));
    let _watchdog = spawn_timeout_watchdog(cancel.clone(), validated.timeout_ms);
    let _signal = crate::cancel::spawn_signal_handler(cancel.clone());

    let provider = match build_adapter(validated.provider.clone(), validated.model.clone()) {
        Ok(p) => p,
        Err(err) => {
            let _ = GantryEvent::Error {
                ts: now_ms(),
                kind: crate::events::ErrorKind::Config,
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
                kind: crate::events::ErrorKind::Config,
                message: format!("prompt file: {err}"),
            }
            .emit();
            return outcome(ExitCode::Config, &meter);
        }
    };

    let registry = ToolRegistry::new(validated.workdir.clone(), validated.tools.clone())
        .with_shell_allow(validated.shell_allow.clone());
    let skill_loader = SkillLoader::new(validated.workdir.clone());

    let exit = SingleMode {
        validated,
        meter: meter.clone(),
        cancel,
        registry,
        skill_loader,
        provider,
        prompt,
    }
    .run()
    .await;

    outcome(exit, &meter)
}
