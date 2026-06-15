use crate::cancel::{shared_token, spawn_signal_handler, spawn_timeout_watchdog};
use crate::cli::{Mode, Validated};
use crate::events::{now_ms, ErrorKind, ExitCode, GantryEvent};
use crate::meter::{MeterSnapshot, TokenMeter};
use crate::provider::{build_adapter, ProviderAdapter};
use crate::skills::SkillLoader;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub mod agent_loop;
pub mod compaction;
pub mod isolation;
pub mod loop_mode;
pub mod single;
pub mod team;

/// Neutral default system prompt used when no `--system-file` is supplied.
/// Orchestrators supply the real persona (e.g. a review profile).
pub const DEFAULT_SYSTEM_PROMPT: &str =
    "You are an agent running in the gantry harness. Use the available tools to complete the task.";

/// Neutral default system prompt for spawned subagents when no
/// `--subagent-system-file` is supplied.
pub const DEFAULT_SUBAGENT_SYSTEM: &str =
    "You are a subagent. Complete the task delegated by the coordinator using the available tools.";

/// Outcome of a single or team mode run, including token totals for the terminal result.
pub struct ModeRunOutcome {
    pub exit: ExitCode,
    pub meter: MeterSnapshot,
}

/// Shared per-run scaffolding built once by [`bootstrap`]: cancellation, the
/// token meter, the resolved provider, the prompt text, and the skill loader.
/// The watchdog/signal tasks are detached but their handles are held here for
/// the run's lifetime.
pub(crate) struct RunBootstrap {
    pub cancel: CancellationToken,
    pub meter: Arc<TokenMeter>,
    pub provider: Box<dyn ProviderAdapter>,
    pub prompt: String,
    pub skill_loader: SkillLoader,
    pub watchdog: JoinHandle<()>,
    pub signal: JoinHandle<()>,
}

/// Build the common run scaffolding shared by every mode, or return a terminal
/// `config` outcome when the provider can't be resolved or the prompt can't be read.
pub(crate) fn bootstrap(validated: &Validated) -> Result<RunBootstrap, ModeRunOutcome> {
    let cancel = shared_token();
    let meter = Arc::new(TokenMeter::new(validated.max_tokens, cancel.clone()));
    let watchdog = spawn_timeout_watchdog(cancel.clone(), validated.timeout_ms);
    let signal = spawn_signal_handler(cancel.clone());

    let provider = build_adapter(
        validated.provider.clone(),
        validated.model.clone(),
        validated.base_url.clone(),
    )
        .map_err(|e| config_error(&meter, &e.to_string()))?;
    let prompt = std::fs::read_to_string(&validated.prompt_file)
        .map_err(|e| config_error(&meter, &format!("prompt file: {e}")))?;
    let skill_loader = SkillLoader::new(validated.workdir.clone());

    Ok(RunBootstrap {
        cancel,
        meter,
        provider,
        prompt,
        skill_loader,
        watchdog,
        signal,
    })
}

/// Terminal `result` outcome carrying the meter's current token totals.
pub(crate) fn outcome(exit: ExitCode, meter: &TokenMeter) -> ModeRunOutcome {
    ModeRunOutcome {
        exit,
        meter: meter.snapshot(),
    }
}

/// Emit a `config` error event and return the matching terminal outcome.
pub(crate) fn config_error(meter: &TokenMeter, message: &str) -> ModeRunOutcome {
    let _ = GantryEvent::unrecoverable(ErrorKind::Config, message).emit();
    outcome(ExitCode::Config, meter)
}

/// Map a terminal provider failure (retries already exhausted) to the
/// embedder contract: `(recoverable, retry_after_ms, exit)`. Reuses
/// [`classify_error`](crate::provider::retry::classify_error) — the single
/// source of error classification.
fn map_provider_failure(err: &anyhow::Error) -> (bool, Option<u64>, ExitCode) {
    use crate::provider::retry::{classify_error, ErrorClass};
    match classify_error(err) {
        ErrorClass::RateLimited { retry_after } => (
            true,
            // Saturate: the hint text is provider-controlled, and a u64::MAX-seconds
            // hint would otherwise truncate to garbage milliseconds.
            retry_after.map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
            ExitCode::RateLimited,
        ),
        ErrorClass::Transient => (true, None, ExitCode::Error),
        ErrorClass::Fatal => (false, None, ExitCode::Error),
    }
}

/// Emit the `error{kind:provider}` event for a terminal provider failure and
/// return the exit code the run should end with. Single-sources the
/// provider-failure contract for `single`/`loop` (via `run_agent_pass`) and
/// team's `structured_call` transport site.
pub(crate) fn emit_provider_failure(err: &anyhow::Error) -> ExitCode {
    let (recoverable, retry_after_ms, exit) = map_provider_failure(err);
    let _ = GantryEvent::Error {
        ts: now_ms(),
        kind: ErrorKind::Provider,
        message: format!("{err:#}"),
        recoverable,
        retry_after_ms,
    }
    .emit();
    exit
}

/// Entry point: run the selected mode, transparently wrapping it in copy-on-write
/// workspace isolation when `--isolate` is set.
pub async fn run(v: Validated) -> ModeRunOutcome {
    if v.isolate {
        isolation::run_isolated(v).await
    } else {
        dispatch(v).await
    }
}

/// Dispatch to the concrete mode runner against `v.workdir` — which the isolation
/// wrapper may have repointed at an overlay before calling.
pub(crate) async fn dispatch(v: Validated) -> ModeRunOutcome {
    match v.mode {
        Mode::Single => single::run_single(v).await,
        Mode::Team => team::run_team(v).await,
        Mode::Loop => loop_mode::run_loop(v).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn rate_limited_with_hint_maps_to_exit_5_and_delay() {
        let err = anyhow!("429 too many requests; retry-after=30");
        assert_eq!(
            map_provider_failure(&err),
            (true, Some(30_000), ExitCode::RateLimited)
        );
    }

    #[test]
    fn absurd_provider_controlled_hint_saturates_instead_of_truncating() {
        let err = anyhow!("429; retry-after=18446744073709551615");
        assert_eq!(
            map_provider_failure(&err),
            (true, Some(u64::MAX), ExitCode::RateLimited)
        );
    }

    #[test]
    fn rate_limited_without_hint_maps_to_exit_5_no_delay() {
        let err = anyhow!("rate limit exceeded");
        assert_eq!(
            map_provider_failure(&err),
            (true, None, ExitCode::RateLimited)
        );
    }

    #[test]
    fn transient_maps_to_recoverable_error_exit() {
        let err = anyhow!("503 service unavailable");
        assert_eq!(map_provider_failure(&err), (true, None, ExitCode::Error));
    }

    #[test]
    fn fatal_maps_to_unrecoverable_error_exit() {
        let err = anyhow!("401 unauthorized");
        assert_eq!(map_provider_failure(&err), (false, None, ExitCode::Error));
    }
}
