use crate::cli::{Mode, Validated};
use crate::events::ExitCode;
use crate::meter::MeterSnapshot;

pub mod agent_loop;
pub mod isolation;
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
    }
}
