//! Iterative `loop` mode (SP3): run an agent in bounded iterations until it
//! calls `decide_stop` or hits `--max-iterations`. Each iteration is a fresh
//! agent pass seeded with a COMPACT carry-forward summary (the prior iteration's
//! final text) — not an accumulating transcript — keeping per-iteration context
//! tight. Reuses `run_agent_pass`; the loop owns the body + the stop check.

use crate::cli::Validated;
use crate::events::{now_ms, ExitCode, GantryEvent};
use crate::meter::TokenMeter;
use crate::mode::single::{run_agent_pass, MAX_TURNS};
use crate::mode::ModeRunOutcome;
use crate::provider::{build_adapter, ChatMessage};
use crate::skills::SkillLoader;
use crate::tools::decide_stop::DECIDE_STOP;
use crate::tools::registry::BASE_TOOL_NAMES;
use crate::tools::ToolRegistry;
use std::sync::Arc;

fn outcome(exit: ExitCode, meter: &TokenMeter) -> ModeRunOutcome {
    ModeRunOutcome {
        exit,
        meter: meter.snapshot(),
    }
}

/// Public entry point used by `mode::dispatch`.
pub async fn run_loop(validated: Validated) -> ModeRunOutcome {
    use crate::cancel::{shared_token, spawn_signal_handler, spawn_timeout_watchdog};

    let cancel = shared_token();
    let meter = Arc::new(TokenMeter::new(validated.max_tokens, cancel.clone()));
    let _watchdog = spawn_timeout_watchdog(cancel.clone(), validated.timeout_ms);
    let _signal = spawn_signal_handler(cancel.clone());

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

    // Loop tools = the configured set (or all base tools by default) PLUS the
    // decide_stop control signal. We can't lean on the empty-allow "all base"
    // shortcut because decide_stop has to be named explicitly.
    let mut allow: Vec<String> = if validated.tools.is_empty() {
        BASE_TOOL_NAMES.iter().map(|s| s.to_string()).collect()
    } else {
        validated.tools.clone()
    };
    allow.push(DECIDE_STOP.to_string());
    let registry = ToolRegistry::new(validated.workdir.clone(), allow)
        .with_shell_allow(validated.shell_allow.clone());

    let skill_loader = SkillLoader::new(validated.workdir.clone());
    let system_prefix = skill_loader.inject_core_skills(&validated.inject_skills);
    let body = validated
        .system_prompt
        .as_deref()
        .unwrap_or(crate::mode::DEFAULT_SYSTEM_PROMPT);
    let system = format!("{system_prefix}\n{body}");

    let mut prior_final = String::new();
    let mut exit = ExitCode::Ok;
    for iteration in 1..=validated.max_iterations {
        if cancel.is_cancelled() {
            exit = if meter.tripped() {
                ExitCode::Budget
            } else {
                ExitCode::Timeout
            };
            break;
        }
        let _ = GantryEvent::IterationStart {
            ts: now_ms(),
            iteration,
        }
        .emit();

        let messages = if iteration == 1 {
            vec![ChatMessage::User(prompt.clone())]
        } else {
            vec![ChatMessage::User(format!(
                "{prompt}\n\n# Previous attempt\n{prior_final}\n\nImprove it, or call decide_stop if it is good enough."
            ))]
        };

        let pass = run_agent_pass(
            provider.as_ref(),
            &registry,
            &meter,
            &cancel,
            &system,
            messages,
            "loop",
            MAX_TURNS,
        )
        .await;
        prior_final = pass.final_text;

        let stopped =
            pass.stop_requested || pass.exit.is_some() || iteration == validated.max_iterations;
        let _ = GantryEvent::IterationEnd {
            ts: now_ms(),
            iteration,
            stopped,
        }
        .emit();

        if let Some(e) = pass.exit {
            exit = e;
            break;
        }
        if pass.stop_requested {
            break;
        }
    }

    outcome(exit, &meter)
}
