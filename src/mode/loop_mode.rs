//! Iterative `loop` mode (SP3): run an agent in bounded iterations until it
//! calls `decide_stop` or hits `--max-iterations`. Each iteration is a fresh
//! agent pass seeded with a COMPACT carry-forward summary (the prior iteration's
//! final text) — not an accumulating transcript — keeping per-iteration context
//! tight. Reuses `run_agent_pass`; the loop owns the body + the stop check.

use crate::cli::Validated;
use crate::events::{now_ms, ExitCode, GantryEvent};
use crate::mode::single::{run_agent_pass, PassCtx, MAX_TURNS};
use crate::mode::{bootstrap, outcome, ModeRunOutcome, RunBootstrap};
use crate::provider::ChatMessage;
use crate::tools::decide_stop::DECIDE_STOP;
use crate::tools::ToolRegistry;

/// Public entry point used by `mode::dispatch`.
pub async fn run_loop(validated: Validated) -> ModeRunOutcome {
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

    // decide_stop is granted as a control tool, so the --tool/profile allowlist
    // keeps its normal "empty = all base tools" semantics.
    let registry = ToolRegistry::new(validated.workdir.clone(), validated.tools.clone())
        .with_shell_allow(validated.shell_allow.clone())
        .with_control(DECIDE_STOP);

    let system_prefix = skill_loader.inject_core_skills(&validated.inject_skills);
    let body = validated
        .system_prompt
        .as_deref()
        .unwrap_or(crate::mode::DEFAULT_SYSTEM_PROMPT);
    let system = format!("{system_prefix}\n{body}");
    let ctx = PassCtx {
        provider: provider.as_ref(),
        registry: &registry,
        meter: &meter,
        cancel: &cancel,
        context_limit: validated.context_limit,
    };

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

        let pass = run_agent_pass(&ctx, &system, messages, "loop", MAX_TURNS).await;
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
