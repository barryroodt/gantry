use gantry::emitter::TestEmitterGuard;
use gantry::events::GantryEvent;
use gantry::tools::ToolRegistry;
use gantry_evals::assertions::assert_tool_call_pairing;
use tempfile::TempDir;

const EXPECTED_TOOL_NAMES: [&str; 7] = [
    "read_file",
    "list_files",
    "find_files",
    "git_diff",
    "ast_grep",
    "shell",
    "skill_load",
];

#[test]
fn schemas_returns_default_tool_names() {
    let registry = ToolRegistry::new(std::env::temp_dir(), vec![]);
    let schemas = registry.schemas();

    assert_eq!(schemas.len(), 7);
    let names: Vec<&str> = schemas.iter().map(|schema| schema.name.as_str()).collect();
    assert_eq!(names, EXPECTED_TOOL_NAMES);
}

#[tokio::test]
async fn dispatch_read_file_happy_path() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("hello.txt"), "hello world").unwrap();

    let registry = ToolRegistry::new(dir.path().to_path_buf(), vec![]);
    let out = registry
        .dispatch("assistant", 1, "read_file", r#"{"path":"hello.txt"}"#)
        .await;

    assert_eq!(out.content, "hello world");
    assert!(!out.truncated);
}

#[tokio::test]
async fn dispatch_unknown_tool_returns_error_content() {
    let registry = ToolRegistry::new(std::env::temp_dir(), vec![]);
    let out = registry
        .dispatch("assistant", 2, "unknown_tool", "{}")
        .await;

    assert!(
        out.content.starts_with("error: unknown tool:"),
        "unexpected content: {}",
        out.content
    );
}

#[tokio::test]
async fn dispatch_malformed_json_returns_invalid_input_error() {
    let registry = ToolRegistry::new(std::env::temp_dir(), vec![]);
    let out = registry.dispatch("assistant", 3, "read_file", "{").await;

    assert!(
        out.content.starts_with("error: invalid input:"),
        "unexpected content: {}",
        out.content
    );
}

#[tokio::test]
async fn dispatch_emits_paired_tool_call_and_tool_result_events() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("tracked.txt"), "tracked").unwrap();

    let guard = TestEmitterGuard::install();
    let registry = ToolRegistry::new(dir.path().to_path_buf(), vec![]);

    let _ = registry
        .dispatch("reviewer", 7, "read_file", r#"{"path":"tracked.txt"}"#)
        .await;

    let events = guard.drain_events();
    assert_eq!(
        events.len(),
        2,
        "expected one tool_call and one tool_result"
    );

    match (&events[0], &events[1]) {
        (
            GantryEvent::ToolCall {
                role: call_role,
                turn: call_turn,
                tool: call_tool,
                args,
                ..
            },
            GantryEvent::ToolResult {
                role: result_role,
                turn: result_turn,
                tool: result_tool,
                bytes,
                truncated,
                error,
                ..
            },
        ) => {
            assert_eq!(call_role, "reviewer");
            assert_eq!(result_role, "reviewer");
            assert_eq!(call_turn, &7);
            assert_eq!(result_turn, &7);
            assert_eq!(call_tool, "read_file");
            assert_eq!(result_tool, "read_file");
            assert_eq!(args, r#"{"path":"tracked.txt"}"#);
            assert_eq!(*bytes, 7);
            assert!(!truncated);
            assert!(error.is_none());
        }
        other => panic!("unexpected event sequence: {other:?}"),
    }

    assert_tool_call_pairing(&events).expect("tool_call/tool_result pairing");
}

#[tokio::test]
async fn dispatch_error_still_emits_paired_events_with_error_field() {
    let guard = TestEmitterGuard::install();
    let registry = ToolRegistry::new(std::env::temp_dir(), vec![]);

    let out = registry
        .dispatch("assistant", 4, "unknown_tool", "{}")
        .await;
    assert!(out.content.starts_with("error: unknown tool:"));

    let events = guard.drain_events();
    assert_eq!(events.len(), 2);

    let GantryEvent::ToolResult { error, .. } = &events[1] else {
        panic!("expected tool_result second");
    };
    assert_eq!(
        error.as_deref(),
        Some(out.content.as_str()),
        "tool_result.error should mirror returned content"
    );

    assert_tool_call_pairing(&events).expect("pairing on error path");
}

#[test]
fn base_tool_names_const_matches_schemas() {
    use gantry::tools::registry::{available_tool_names, BASE_TOOL_NAMES, OPTIN_TOOL_NAMES};
    let registry = ToolRegistry::new(std::env::temp_dir(), vec![]);
    let schemas = registry.schemas();
    let names: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, BASE_TOOL_NAMES);
    let expected: Vec<&str> = BASE_TOOL_NAMES
        .iter()
        .chain(OPTIN_TOOL_NAMES.iter())
        .copied()
        .collect();
    assert_eq!(available_tool_names(), expected);
}

#[test]
fn allowlist_filters_exposed_schemas() {
    let registry = ToolRegistry::new(
        std::env::temp_dir(),
        vec!["read_file".into(), "git_diff".into()],
    );
    let schemas = registry.schemas();
    let names: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["read_file", "git_diff"]);
}

#[tokio::test]
async fn disallowed_tool_dispatch_returns_unknown_tool() {
    let dir = TempDir::new().unwrap();
    let _guard = TestEmitterGuard::install();
    let registry = ToolRegistry::new(dir.path().to_path_buf(), vec!["read_file".into()]);

    let out = registry
        .dispatch("assistant", 1, "shell", r#"{"command":"git --version"}"#)
        .await;

    assert!(out.content.contains("unknown tool"), "got: {}", out.content);
}

#[tokio::test]
async fn allowlisted_tool_dispatches_normally() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("hello.txt"), "hello world").unwrap();
    let _guard = TestEmitterGuard::install();
    let registry = ToolRegistry::new(dir.path().to_path_buf(), vec!["read_file".into()]);

    let out = registry
        .dispatch("assistant", 1, "read_file", r#"{"path":"hello.txt"}"#)
        .await;

    assert!(out.content.contains("hello world"), "got: {}", out.content);
}

#[test]
fn ast_edit_is_opt_in_default_out() {
    // Default registry: ast_grep is default-in, ast_edit is default-out.
    let default = ToolRegistry::new(std::env::temp_dir(), vec![]);
    let names: Vec<String> = default.schemas().iter().map(|s| s.name.clone()).collect();
    assert!(
        names.contains(&"ast_grep".to_string()),
        "ast_grep should be default-in: {names:?}"
    );
    assert!(
        !names.contains(&"ast_edit".to_string()),
        "ast_edit must be default-out: {names:?}"
    );

    // Opting in surfaces ast_edit in the schemas.
    let optin = ToolRegistry::new(std::env::temp_dir(), vec!["ast_edit".into()]);
    let names: Vec<String> = optin.schemas().iter().map(|s| s.name.clone()).collect();
    assert!(
        names.contains(&"ast_edit".to_string()),
        "ast_edit should be exposed when allowlisted: {names:?}"
    );
}

#[tokio::test]
async fn ast_edit_dispatch_denied_without_optin() {
    let registry = ToolRegistry::new(std::env::temp_dir(), vec![]);
    let out = registry
        .dispatch(
            "assistant",
            1,
            "ast_edit",
            r#"{"pattern":"a","rewrite":"b"}"#,
        )
        .await;
    assert!(
        out.content.contains("unknown tool"),
        "default-out ast_edit must be denied at dispatch: {}",
        out.content
    );
}

#[test]
fn mutation_tools_are_opt_in_default_out() {
    let default = ToolRegistry::new(std::env::temp_dir(), vec![]);
    let names: Vec<String> = default.schemas().iter().map(|s| s.name.clone()).collect();
    for t in ["write_file", "edit_file"] {
        assert!(
            !names.contains(&t.to_string()),
            "{t} must be default-out: {names:?}"
        );
    }
    let optin = ToolRegistry::new(
        std::env::temp_dir(),
        vec!["write_file".into(), "edit_file".into()],
    );
    let names: Vec<String> = optin.schemas().iter().map(|s| s.name.clone()).collect();
    for t in ["write_file", "edit_file"] {
        assert!(
            names.contains(&t.to_string()),
            "{t} should be exposed when allowlisted: {names:?}"
        );
    }
}

#[tokio::test]
async fn write_file_dispatch_denied_without_optin() {
    let dir = TempDir::new().unwrap();
    let _guard = TestEmitterGuard::install();
    let registry = ToolRegistry::new(dir.path().to_path_buf(), vec![]);
    let out = registry
        .dispatch(
            "assistant",
            1,
            "write_file",
            r#"{"path":"x.txt","content":"y"}"#,
        )
        .await;
    assert!(
        out.content.contains("unknown tool"),
        "default-out write_file must be denied: {}",
        out.content
    );
    assert!(
        !dir.path().join("x.txt").exists(),
        "no file created when the tool is denied"
    );
}

#[tokio::test]
async fn write_file_dispatches_when_allowed() {
    let dir = TempDir::new().unwrap();
    let _guard = TestEmitterGuard::install();
    let registry = ToolRegistry::new(dir.path().to_path_buf(), vec!["write_file".into()]);
    let out = registry
        .dispatch(
            "assistant",
            1,
            "write_file",
            r#"{"path":"x.txt","content":"hello"}"#,
        )
        .await;
    assert!(out.content.contains("wrote"), "got: {}", out.content);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("x.txt")).unwrap(),
        "hello"
    );
}

#[test]
fn decide_stop_is_control_only_invisible_by_default() {
    use gantry::tools::registry::available_tool_names;
    // Not user-grantable via --tool, and absent from the default tool surface.
    assert!(!available_tool_names().contains(&"decide_stop"));
    let default = ToolRegistry::new(std::env::temp_dir(), vec![]);
    let names: Vec<String> = default.schemas().iter().map(|s| s.name.clone()).collect();
    assert!(
        !names.contains(&"decide_stop".to_string()),
        "decide_stop must be invisible by default: {names:?}"
    );
    // Surfaced only when the registry grants it explicitly (loop mode).
    let loop_reg = ToolRegistry::new(std::env::temp_dir(), vec!["decide_stop".into()]);
    let names: Vec<String> = loop_reg.schemas().iter().map(|s| s.name.clone()).collect();
    assert!(
        names.contains(&"decide_stop".to_string()),
        "granted: {names:?}"
    );
}

#[tokio::test]
async fn decide_stop_dispatch_gated_then_allowed() {
    let _guard = TestEmitterGuard::install();
    let denied = ToolRegistry::new(std::env::temp_dir(), vec![]);
    let out = denied
        .dispatch("assistant", 1, "decide_stop", r#"{"reason":"x"}"#)
        .await;
    assert!(
        out.content.contains("unknown tool"),
        "gated: {}",
        out.content
    );

    let granted = ToolRegistry::new(std::env::temp_dir(), vec!["decide_stop".into()]);
    let out = granted
        .dispatch("assistant", 1, "decide_stop", r#"{"reason":"good enough"}"#)
        .await;
    assert!(
        out.content.contains("stop requested"),
        "allowed: {}",
        out.content
    );
}

#[tokio::test]
async fn dispatch_compresses_verbose_output_and_reports_bytes_out() {
    let dir = TempDir::new().unwrap();
    let big = (1..=600)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dir.path().join("big.txt"), &big).unwrap();

    let guard = TestEmitterGuard::install();
    let registry = ToolRegistry::new(dir.path().to_path_buf(), vec![]);
    let out = registry
        .dispatch("reviewer", 1, "read_file", r#"{"path":"big.txt"}"#)
        .await;

    // dispatch applied the recoverable head+tail cap.
    assert!(
        out.content.contains("lines omitted"),
        "verbose output should be capped: {}",
        out.content
    );
    assert!(
        (out.content.len() as u64) < (out.bytes as u64),
        "compressed content ({}) should be below raw ({})",
        out.content.len(),
        out.bytes
    );

    // tool_result reports raw `bytes` plus the smaller emitted `bytes_out`.
    let events = guard.drain_events();
    let GantryEvent::ToolResult {
        bytes, bytes_out, ..
    } = &events[1]
    else {
        panic!("expected tool_result at index 1, got {:?}", events[1]);
    };
    assert!(*bytes_out < *bytes, "bytes_out {bytes_out} < bytes {bytes}");
    assert_eq!(*bytes_out, out.content.len() as u64);
}
