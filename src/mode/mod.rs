use crate::events::ExitCode;
use crate::meter::MeterSnapshot;

pub mod agent_loop;
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
