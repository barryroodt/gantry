//! `decide_stop`: a loop-mode control signal. The agent calls it to tell the
//! iterative loop to stop after the current pass. It is NOT a workdir tool and
//! never appears in the default (single/team) tool sets — the loop registry
//! grants it explicitly, and the loop detects the call by name.

use super::{truncate::truncated_output, ToolError, ToolOutput};
use serde::{Deserialize, Serialize};

/// Tool name the loop watches for to end iteration early.
pub const DECIDE_STOP: &str = "decide_stop";

#[derive(Debug, Deserialize, Serialize)]
pub struct DecideStopArgs {
    /// Optional human-readable reason for stopping.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Acknowledge a stop request. The loop detects the call by name and ends after
/// the current pass; this just returns a confirming message.
pub async fn decide_stop(args: DecideStopArgs) -> Result<ToolOutput, ToolError> {
    let msg = match args.reason.as_deref().map(str::trim) {
        Some(r) if !r.is_empty() => format!("stop requested: {r}"),
        _ => "stop requested".to_string(),
    };
    Ok(truncated_output(msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reports_reason_when_present() {
        let out = decide_stop(DecideStopArgs {
            reason: Some("good enough".into()),
        })
        .await
        .unwrap();
        assert_eq!(out.content, "stop requested: good enough");
    }

    #[tokio::test]
    async fn reports_bare_when_absent_or_blank() {
        for reason in [None, Some("   ".to_string())] {
            let out = decide_stop(DecideStopArgs { reason }).await.unwrap();
            assert_eq!(out.content, "stop requested");
        }
    }
}
