//! Shell tool backed by pi-shell (in-process bash + output minimizer +
//! cancellation), fronted by a best-effort program-name allowlist.
//!
//! The allowlist restricts which programs may appear at a command position. It
//! is a guard, NOT a sandbox: full bash is not path-jailed (an allowlisted
//! program can still touch paths outside the workdir), and exotic dispatch
//! (`eval`, dynamic `$VAR` program names) is denied conservatively rather than
//! resolved. The hard isolation boundary is SP2 (pi-iso) + the per-profile tool
//! grant; review keeps the read-only `ALLOWED_PROGRAMS` set.

use super::{truncate::truncated_output, ToolError, ToolOutput};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Default read-only program allowlist (used when a profile sets none).
pub const ALLOWED_PROGRAMS: &[&str] = &["git", "cat", "ls", "find"];

#[derive(Debug, Deserialize, Serialize)]
pub struct ShellArgs {
    /// Bash command line to execute (pipes, redirects, etc. supported).
    pub command: String,
}

/// Run `args.command` as bash via pi-shell in `workdir`, returning bounded
/// output. Every program at a command position must be in `allow`, else the
/// command is rejected WITHOUT executing.
pub async fn shell(
    workdir: &Path,
    args: ShellArgs,
    allow: &[String],
) -> Result<ToolOutput, ToolError> {
    if let Some(prog) = extract_programs(&args.command)
        .into_iter()
        .find(|p| !allow.iter().any(|a| a == p))
    {
        return Err(ToolError::InvalidInput(format!(
            "program not allowlisted: {prog}"
        )));
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let opts = pi_shell::ShellExecuteOptions {
        command: args.command.clone(),
        cwd: Some(workdir.to_string_lossy().into_owned()),
        ..Default::default()
    };
    let result = pi_shell::execute_shell(opts, Some(tx), pi_shell::cancel::CancelToken::default())
        .await
        .map_err(|e| ToolError::InvalidInput(format!("shell: {e}")))?;

    let mut content = String::new();
    while let Ok(chunk) = rx.try_recv() {
        content.push_str(&chunk);
    }
    if let Some(code) = result.exit_code {
        if code != 0 {
            content.push_str(&format!("\n[exit code {code}]\n"));
        }
    }
    Ok(truncated_output(content))
}

/// Best-effort extraction of command-position program names from a bash line.
/// Splits on shell operators (including substitution/redirection delimiters)
/// and takes the first non-assignment word of each segment. Dynamic words
/// (e.g. `$VAR`) are returned verbatim so they fail a restrictive allowlist.
fn extract_programs(command: &str) -> Vec<String> {
    let seps = ['\n', ';', '|', '&', '(', ')', '{', '}', '`'];
    let mut programs = Vec::new();
    for segment in command.split(|c| seps.contains(&c)) {
        let seg = segment.trim();
        if seg.is_empty() {
            continue;
        }
        for token in seg.split_whitespace() {
            if is_assignment(token) || is_redirect(token) {
                continue;
            }
            programs.push(token.to_string());
            break;
        }
    }
    programs
}

/// True for a leading `NAME=value` environment assignment.
fn is_assignment(token: &str) -> bool {
    match token.split_once('=') {
        Some((name, _)) => {
            !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// True for a redirection operator token (`>`, `>>`, `2>`, `&>`, `<`, `<<`).
fn is_redirect(token: &str) -> bool {
    let rest = token.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&');
    rest.starts_with('>') || rest.starts_with('<')
}

#[cfg(test)]
mod tests {
    use super::extract_programs;

    #[test]
    fn extracts_command_words_across_operators() {
        let progs = extract_programs("FOO=1 git status | cat && ls; echo $(rm x)");
        for prog in ["git", "cat", "ls", "echo", "rm"] {
            assert!(
                progs.contains(&prog.to_string()),
                "{prog} missing in {progs:?}"
            );
        }
    }

    #[test]
    fn redirect_target_is_not_a_program() {
        assert_eq!(
            extract_programs("git diff > out.txt"),
            vec!["git".to_string()]
        );
    }

    #[test]
    fn dynamic_program_returned_for_denial() {
        assert_eq!(extract_programs("$TOOL --flag"), vec!["$TOOL".to_string()]);
    }
}
