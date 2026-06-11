use crate::expected::Expected;
use gantry::events::{ExitCode, GantryEvent};
use regex::Regex;
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[error("{rule}: {detail}")]
pub struct AssertionFailure {
    pub rule: String,
    pub detail: String,
}

impl AssertionFailure {
    fn new(rule: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            rule: rule.into(),
            detail: detail.into(),
        }
    }
}

type ExpectedAssertion = fn(&[GantryEvent], &Expected) -> Result<(), AssertionFailure>;

/// Run every assertion against the events + expectations, collecting all
/// failures (no early return) so a fixture report lists every divergence.
pub fn run_all(events: &[GantryEvent], expected: &Expected) -> Vec<AssertionFailure> {
    let checks: &[(&str, ExpectedAssertion)] = &[
        ("assert_exit_matches", assert_exit_matches),
        ("assert_findings_in_range", assert_findings_in_range),
        ("assert_required_severities", assert_required_severities),
        ("assert_required_paths", assert_required_paths),
        ("assert_message_regex_matches", assert_message_regex_matches),
        (
            "assert_forbidden_message_regex",
            assert_forbidden_message_regex,
        ),
        ("assert_single_json_fence", assert_single_json_fence),
        ("assert_token_budget", assert_token_budget),
        ("assert_duration", assert_duration),
        ("assert_decide_stop", assert_decide_stop),
        ("assert_changed_paths", assert_changed_paths),
        ("assert_retrieve_handle", assert_retrieve_handle),
    ];

    let mut failures = Vec::new();
    for (name, check) in checks {
        if let Err(failure) = check(events, expected) {
            failures.push(AssertionFailure::new(*name, failure.detail));
        }
    }

    if let Err(failure) = assert_tool_call_pairing(events) {
        failures.push(failure);
    }

    if let Err(failure) = assert_subagent_lifecycle(events) {
        failures.push(failure);
    }

    failures
}

pub fn assert_exit_matches(
    events: &[GantryEvent],
    expected: &Expected,
) -> Result<(), AssertionFailure> {
    let want = parse_expected_exit(&expected.exit)?;
    let Some(GantryEvent::Result { exit, .. }) = terminal_result(events) else {
        return Err(AssertionFailure::new(
            "assert_exit_matches",
            "no terminal result event found",
        ));
    };

    if *exit != want {
        return Err(AssertionFailure::new(
            "assert_exit_matches",
            format!("expected exit {want:?}, got {exit:?}"),
        ));
    }

    Ok(())
}

/// Refinement convergence: the loop ended by calling the `decide_stop` control
/// tool rather than exhausting its iteration cap. No-op unless requested.
pub fn assert_decide_stop(
    events: &[GantryEvent],
    expected: &Expected,
) -> Result<(), AssertionFailure> {
    if expected.expect_decide_stop != Some(true) {
        return Ok(());
    }
    let stopped = events
        .iter()
        .any(|e| matches!(e, GantryEvent::ToolCall { tool, .. } if tool == "decide_stop"));
    if !stopped {
        return Err(AssertionFailure::new(
            "assert_decide_stop",
            "expected a decide_stop tool call (loop did not converge)",
        ));
    }
    Ok(())
}

/// Each `must_change_paths` entry must be a substring of at least one path in a
/// terminal `changes` event (isolate teardown). No-op when the list is empty.
pub fn assert_changed_paths(
    events: &[GantryEvent],
    expected: &Expected,
) -> Result<(), AssertionFailure> {
    if expected.must_change_paths.is_empty() {
        return Ok(());
    }
    let changed: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            GantryEvent::Changes { files, .. } => Some(files),
            _ => None,
        })
        .flatten()
        .map(|f| f.path.as_str())
        .collect();
    for needle in &expected.must_change_paths {
        if !changed.iter().any(|p| p.contains(needle.as_str())) {
            return Err(AssertionFailure::new(
                "assert_changed_paths",
                format!("no changed path matched {needle:?}; changed: {changed:?}"),
            ));
        }
    }
    Ok(())
}

/// At least one `tool_result` carried a retrieval `handle` — i.e. output was
/// capped and stashed (ADR-0012). No-op unless requested.
pub fn assert_retrieve_handle(
    events: &[GantryEvent],
    expected: &Expected,
) -> Result<(), AssertionFailure> {
    if expected.expect_retrieve_handle != Some(true) {
        return Ok(());
    }
    let any_handle = events.iter().any(|e| {
        matches!(
            e,
            GantryEvent::ToolResult {
                handle: Some(_),
                ..
            }
        )
    });
    if !any_handle {
        return Err(AssertionFailure::new(
            "assert_retrieve_handle",
            "expected at least one tool_result with a retrieval handle (no output was capped)",
        ));
    }
    Ok(())
}

pub fn assert_findings_in_range(
    events: &[GantryEvent],
    expected: &Expected,
) -> Result<(), AssertionFailure> {
    let count = findings(events).len();

    if let Some(min) = expected.min_findings {
        if count < min {
            return Err(AssertionFailure::new(
                "assert_findings_in_range",
                format!("findings count {count} below minimum {min}"),
            ));
        }
    }

    if let Some(max) = expected.max_findings {
        if count > max {
            return Err(AssertionFailure::new(
                "assert_findings_in_range",
                format!("findings count {count} above maximum {max}"),
            ));
        }
    }

    Ok(())
}

/// Every required severity appears on at least one finding (case-insensitive).
pub fn assert_required_severities(
    events: &[GantryEvent],
    expected: &Expected,
) -> Result<(), AssertionFailure> {
    if expected.must_contain_severity.is_empty() {
        return Ok(());
    }
    let findings = findings(events);
    let severities: Vec<String> = findings
        .iter()
        .filter_map(|f| f.get("severity").and_then(Value::as_str))
        .map(|s| s.to_lowercase())
        .collect();

    for want in &expected.must_contain_severity {
        let want_lc = want.to_lowercase();
        if !severities.contains(&want_lc) {
            return Err(AssertionFailure::new(
                "assert_required_severities",
                format!("required severity {want:?} not found among findings (got {severities:?})"),
            ));
        }
    }
    Ok(())
}

/// Every `must_match_path` entry appears as a substring of some `finding.path`.
pub fn assert_required_paths(
    events: &[GantryEvent],
    expected: &Expected,
) -> Result<(), AssertionFailure> {
    if expected.must_match_path.is_empty() {
        return Ok(());
    }
    let findings = findings(events);
    let paths: Vec<&str> = findings
        .iter()
        .filter_map(|f| f.get("path").and_then(Value::as_str))
        .collect();

    for want in &expected.must_match_path {
        if !paths.iter().any(|p| p.contains(want.as_str())) {
            return Err(AssertionFailure::new(
                "assert_required_paths",
                format!("required path {want:?} not found among finding paths ({paths:?})"),
            ));
        }
    }
    Ok(())
}

/// Every `must_match_message_regex` matches at least one `finding.message`.
pub fn assert_message_regex_matches(
    events: &[GantryEvent],
    expected: &Expected,
) -> Result<(), AssertionFailure> {
    if expected.must_match_message_regex.is_empty() {
        return Ok(());
    }
    let findings = findings(events);
    let messages: Vec<&str> = findings
        .iter()
        .filter_map(|f| f.get("message").and_then(Value::as_str))
        .collect();

    for pattern in &expected.must_match_message_regex {
        let re = compile(pattern, "assert_message_regex_matches")?;
        if !messages.iter().any(|m| re.is_match(m)) {
            return Err(AssertionFailure::new(
                "assert_message_regex_matches",
                format!("no finding.message matched required regex {pattern:?}"),
            ));
        }
    }
    Ok(())
}

/// No `forbid_match_message_regex` matches any `finding.message`.
pub fn assert_forbidden_message_regex(
    events: &[GantryEvent],
    expected: &Expected,
) -> Result<(), AssertionFailure> {
    if expected.forbid_match_message_regex.is_empty() {
        return Ok(());
    }
    let findings = findings(events);
    let messages: Vec<&str> = findings
        .iter()
        .filter_map(|f| f.get("message").and_then(Value::as_str))
        .collect();

    for pattern in &expected.forbid_match_message_regex {
        let re = compile(pattern, "assert_forbidden_message_regex")?;
        if let Some(hit) = messages.iter().find(|m| re.is_match(m)) {
            return Err(AssertionFailure::new(
                "assert_forbidden_message_regex",
                format!("finding.message matched forbidden regex {pattern:?}: {hit:?}"),
            ));
        }
    }
    Ok(())
}

/// When required (default: `exit == "ok"`), the model output must be exactly one
/// valid ```json fence, with no prose before or after it in the final
/// assistant turn.
pub fn assert_single_json_fence(
    events: &[GantryEvent],
    expected: &Expected,
) -> Result<(), AssertionFailure> {
    if !expected.require_single_json_fence() {
        return Ok(());
    }

    let texts = assistant_texts(events);
    let total_fences: usize = texts
        .iter()
        .map(|t| extract_json_fence_bodies(t).len())
        .sum();

    if total_fences == 0 {
        return Err(AssertionFailure::new(
            "assert_single_json_fence",
            "expected exactly one ```json fence in assistant output, found none",
        ));
    }
    if total_fences > 1 {
        return Err(AssertionFailure::new(
            "assert_single_json_fence",
            format!("expected exactly one ```json fence, found {total_fences}"),
        ));
    }

    // The fence must parse as valid JSON.
    let body = texts
        .iter()
        .flat_map(|t| extract_json_fence_bodies(t))
        .next()
        .unwrap_or_default();
    if serde_json::from_str::<Value>(body).is_err() {
        return Err(AssertionFailure::new(
            "assert_single_json_fence",
            "the ```json fence did not contain valid JSON",
        ));
    }

    // No preamble/trailer: the terminal assistant turn must be exactly the fence.
    if let Some(last) = texts.last() {
        let trimmed = last.trim();
        if !(trimmed.starts_with("```json") && trimmed.ends_with("```")) {
            return Err(AssertionFailure::new(
                "assert_single_json_fence",
                "final assistant turn has prose outside the ```json fence",
            ));
        }
    }

    Ok(())
}

pub fn assert_token_budget(
    events: &[GantryEvent],
    expected: &Expected,
) -> Result<(), AssertionFailure> {
    if expected.max_input_tokens.is_none() && expected.max_output_tokens.is_none() {
        return Ok(());
    }

    let Some(GantryEvent::Result {
        total_input,
        total_output,
        ..
    }) = terminal_result(events)
    else {
        return Err(AssertionFailure::new(
            "assert_token_budget",
            "no terminal result event found",
        ));
    };

    if let Some(max_input) = expected.max_input_tokens {
        if *total_input > max_input {
            return Err(AssertionFailure::new(
                "assert_token_budget",
                format!("total_input {total_input} exceeds max_input_tokens {max_input}"),
            ));
        }
    }

    if let Some(max_output) = expected.max_output_tokens {
        if *total_output > max_output {
            return Err(AssertionFailure::new(
                "assert_token_budget",
                format!("total_output {total_output} exceeds max_output_tokens {max_output}"),
            ));
        }
    }

    Ok(())
}

pub fn assert_duration(
    events: &[GantryEvent],
    expected: &Expected,
) -> Result<(), AssertionFailure> {
    let Some(max_duration_ms) = expected.max_duration_ms else {
        return Ok(());
    };

    let Some(GantryEvent::Result { duration_ms, .. }) = terminal_result(events) else {
        return Err(AssertionFailure::new(
            "assert_duration",
            "no terminal result event found",
        ));
    };

    if *duration_ms > max_duration_ms {
        return Err(AssertionFailure::new(
            "assert_duration",
            format!("duration_ms {duration_ms} exceeds max_duration_ms {max_duration_ms}"),
        ));
    }

    Ok(())
}

pub fn assert_tool_call_pairing(events: &[GantryEvent]) -> Result<(), AssertionFailure> {
    let mut pending: Vec<(String, u32, String)> = Vec::new();

    for event in events {
        match event {
            GantryEvent::ToolCall {
                role, turn, tool, ..
            } => pending.push((role.clone(), *turn, tool.clone())),
            GantryEvent::ToolResult {
                role, turn, tool, ..
            } => {
                let key = (role.clone(), *turn, tool.clone());
                if let Some(index) = pending.iter().position(|candidate| candidate == &key) {
                    pending.remove(index);
                } else {
                    return Err(AssertionFailure::new(
                        "assert_tool_call_pairing",
                        format!(
                            "unmatched tool_result for role={role:?} turn={turn} tool={tool:?}"
                        ),
                    ));
                }
            }
            _ => {}
        }
    }

    if let Some((role, turn, tool)) = pending.into_iter().next() {
        return Err(AssertionFailure::new(
            "assert_tool_call_pairing",
            format!("unpaired tool_call for role={role:?} turn={turn} tool={tool:?}"),
        ));
    }

    Ok(())
}

/// ADR-0005: in team mode every spawned subagent must terminate — one
/// `subagent_done` per `subagent_spawn` — and all before the coordinator's unify
/// fence. Vacuously satisfied for single-mode fixtures (no subagents).
pub fn assert_subagent_lifecycle(events: &[GantryEvent]) -> Result<(), AssertionFailure> {
    let spawned = events
        .iter()
        .filter(|e| matches!(e, GantryEvent::SubagentSpawn { .. }))
        .count();
    if spawned == 0 {
        return Ok(());
    }
    let done_idxs: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| matches!(e, GantryEvent::SubagentDone { .. }).then_some(i))
        .collect();
    if done_idxs.len() != spawned {
        return Err(AssertionFailure::new(
            "assert_subagent_lifecycle",
            format!(
                "expected one subagent_done per spawned subagent (spawned {spawned}, done {})",
                done_idxs.len()
            ),
        ));
    }
    // The unify fence is the last assistant_text carrying a ```json fence; every
    // subagent must have finished before it.
    if let Some(fence_idx) = events.iter().enumerate().rev().find_map(|(i, e)| match e {
        GantryEvent::AssistantText { text, .. } if !extract_json_fence_bodies(text).is_empty() => {
            Some(i)
        }
        _ => None,
    }) {
        if let Some(&late) = done_idxs.iter().find(|&&i| i > fence_idx) {
            return Err(AssertionFailure::new(
                "assert_subagent_lifecycle",
                format!(
                    "subagent_done at event {late} emitted after the unify fence at {fence_idx}"
                ),
            ));
        }
    }
    Ok(())
}

fn compile(pattern: &str, rule: &'static str) -> Result<Regex, AssertionFailure> {
    Regex::new(pattern)
        .map_err(|e| AssertionFailure::new(rule, format!("invalid regex {pattern:?}: {e}")))
}

fn terminal_result(events: &[GantryEvent]) -> Option<&GantryEvent> {
    events
        .iter()
        .rev()
        .find(|event| matches!(event, GantryEvent::Result { .. }))
}

fn parse_expected_exit(exit: &str) -> Result<ExitCode, AssertionFailure> {
    match exit {
        "ok" => Ok(ExitCode::Ok),
        "budget" => Ok(ExitCode::Budget),
        "timeout" => Ok(ExitCode::Timeout),
        "error" => Ok(ExitCode::Error),
        "config" => Ok(ExitCode::Config),
        "rate_limited" => Ok(ExitCode::RateLimited),
        other => Err(AssertionFailure::new(
            "assert_exit_matches",
            format!("unknown expected exit: {other:?}"),
        )),
    }
}

fn assistant_texts(events: &[GantryEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            GantryEvent::AssistantText { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Parse the findings array from the last valid ```json fence in assistant text.
fn findings(events: &[GantryEvent]) -> Vec<Value> {
    let mut last: Vec<Value> = Vec::new();
    for text in assistant_texts(events) {
        for body in extract_json_fence_bodies(text) {
            if let Ok(value) = serde_json::from_str::<Value>(body) {
                if let Some(arr) = value.get("findings").and_then(|f| f.as_array()) {
                    last = arr.clone();
                }
            }
        }
    }
    last
}

fn extract_json_fence_bodies(text: &str) -> Vec<&str> {
    let mut bodies = Vec::new();
    let mut rest = text;

    while let Some(start) = rest.find("```json") {
        let after_marker = &rest[start + "```json".len()..];
        let content_start = after_marker
            .find('\n')
            .map(|index| index + 1)
            .unwrap_or(after_marker.len());
        let after_newline = &after_marker[content_start..];
        if let Some(end) = after_newline.find("```") {
            bodies.push(after_newline[..end].trim());
            rest = &after_newline[end + "```".len()..];
        } else {
            break;
        }
    }

    bodies
}
