//! Harness adapter tests (plan Task 4). Keyless and networkless: assert the
//! built `Command` (program/args/env/cwd) without spawning, and answer
//! extraction from canned stdout fixtures for all three formats.

use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use gantry_bench::harness::{all, ClaudeCode, Gantry, Harness, Pi, RunCtx};
use tempfile::TempDir;

const PROMPT: &str = "Explain the architecture of this repository.\n";
const MODEL: &str = "claude-test-4-20260101";
const PROXY: &str = "http://127.0.0.1:4242";
const API_KEY: &str = "sk-ant-bench-test";

/// Tempdir-backed RunCtx; the tempdir must outlive the ctx.
fn make_ctx(mutate: bool) -> (TempDir, RunCtx) {
    let tmp = TempDir::new().expect("tempdir");
    let workspace = tmp.path().join("ws");
    let config_dir = tmp.path().join("cfg");
    fs::create_dir(&workspace).expect("workspace dir");
    fs::create_dir(&config_dir).expect("config dir");
    let prompt_file = tmp.path().join("prompt.md");
    fs::write(&prompt_file, PROMPT).expect("prompt file");
    let ctx = RunCtx {
        workspace,
        prompt_file,
        model: MODEL.to_string(),
        proxy_url: PROXY.to_string(),
        config_dir,
        mutate,
        api_key: API_KEY.to_string(),
        timeout_ms: 600_000,
        max_tokens: 2_000_000,
    };
    (tmp, ctx)
}

fn args_of(cmd: &Command) -> Vec<String> {
    cmd.get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

/// Value following `flag` in argv.
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

/// Env state for `key` on the command: `None` = untouched (inherited),
/// `Some(None)` = explicitly removed, `Some(Some(v))` = explicitly set.
fn env_of(cmd: &Command, key: &str) -> Option<Option<String>> {
    cmd.get_envs()
        .find(|(k, _)| *k == OsStr::new(key))
        .map(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
}

fn env_value(cmd: &Command, key: &str) -> String {
    env_of(cmd, key)
        .unwrap_or_else(|| panic!("env {key} not set on command"))
        .unwrap_or_else(|| panic!("env {key} was removed, expected a value"))
}

// ---------------------------------------------------------------------------
// registry

#[test]
fn all_returns_canonical_harness_names_in_report_order() {
    let names: Vec<&str> = all().iter().map(|h| h.name()).collect();
    assert_eq!(names, ["gantry", "claude-code", "pi"]);
}

// ---------------------------------------------------------------------------
// gantry adapter

#[test]
fn gantry_command_args_env_and_cwd() {
    let (_tmp, ctx) = make_ctx(false);
    let h = Gantry::with_bin(PathBuf::from("/fake/target/debug/gantry"));
    let cmd = h.command(&ctx);

    assert_eq!(cmd.get_program(), OsStr::new("/fake/target/debug/gantry"));
    assert_eq!(cmd.get_current_dir(), Some(ctx.workspace.as_path()));

    let args = args_of(&cmd);
    assert_eq!(flag_value(&args, "--mode").as_deref(), Some("single"));
    // bare model id gets the anthropic/ provider prefix (gantry slug form)
    assert_eq!(
        flag_value(&args, "--model").as_deref(),
        Some(format!("anthropic/{MODEL}").as_str())
    );
    assert_eq!(
        flag_value(&args, "--workdir").as_deref(),
        Some(ctx.workspace.to_str().unwrap())
    );
    assert_eq!(
        flag_value(&args, "--prompt-file").as_deref(),
        Some(ctx.prompt_file.to_str().unwrap())
    );
    assert_eq!(flag_value(&args, "--max-tokens").as_deref(), Some("2000000"));
    assert_eq!(flag_value(&args, "--timeout-ms").as_deref(), Some("600000"));

    // read-only task: shipped default toolset, no allowlist restriction
    assert!(!args.iter().any(|a| a == "--tool"));
    // never isolate: grading inspects the post-run workspace
    assert!(!args.iter().any(|a| a == "--isolate"));

    assert_eq!(env_value(&cmd, "ANTHROPIC_API_BASE"), PROXY);
    assert_eq!(env_value(&cmd, "ANTHROPIC_API_KEY"), API_KEY);
}

#[test]
fn gantry_mutate_grants_base_tools_plus_mutation_tools() {
    let (_tmp, ctx) = make_ctx(true);
    let h = Gantry::with_bin(PathBuf::from("/fake/gantry"));
    let args = args_of(&h.command(&ctx));

    let granted: Vec<&str> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| *a == "--tool")
        .filter_map(|(i, _)| args.get(i + 1).map(String::as_str))
        .collect();

    // --tool is an allowlist (a non-empty list disables unnamed base tools),
    // so the mutate grant must carry the full base set + mutation tools.
    assert_eq!(
        granted,
        [
            "read_file",
            "list_files",
            "find_files",
            "git_diff",
            "ast_grep",
            "shell",
            "skill_load",
            "write_file",
            "edit_file",
        ]
    );
    assert!(!args.iter().any(|a| a == "--isolate"));
}

#[test]
fn gantry_bin_env_override_wins() {
    // Only this test touches GANTRY_BENCH_GANTRY_BIN / Gantry::new();
    // all other tests inject via with_bin, so there is no env race.
    std::env::set_var("GANTRY_BENCH_GANTRY_BIN", "/pinned/gantry");
    let h = Gantry::new();
    std::env::remove_var("GANTRY_BENCH_GANTRY_BIN");

    let (_tmp, ctx) = make_ctx(false);
    assert_eq!(h.command(&ctx).get_program(), OsStr::new("/pinned/gantry"));
}

#[test]
fn gantry_answer_is_last_assistant_text_event() {
    let stdout = concat!(
        r#"{"event":"start","ts":1,"schema_version":"1.0","model":"claude-test","provider":"anthropic","mode":"single","workdir":"/ws"}"#,
        "\n",
        r#"{"event":"tool_call","ts":2,"role":"agent","turn":1,"tool":"read_file","args":"{\"path\":\"src/main.rs\"}"}"#,
        "\n",
        "non-json noise line that must be skipped\n",
        r#"{"event":"assistant_text","ts":3,"role":"agent","text":"Interim reasoning."}"#,
        "\n",
        r#"{"event":"assistant_text","ts":4,"role":"agent","text":"The final answer."}"#,
        "\n",
        r#"{"event":"result","ts":5,"exit":"ok","total_input":10,"total_output":5,"total_cache_read":0,"total_cache_write":0,"duration_ms":100}"#,
        "\n",
    );
    assert_eq!(
        Gantry::with_bin(PathBuf::from("/fake")).extract_answer(stdout),
        Some("The final answer.".to_string())
    );
}

#[test]
fn gantry_answer_none_without_assistant_text() {
    let stdout = concat!(
        r#"{"event":"start","ts":1,"schema_version":"1.0","model":"m","provider":"anthropic","mode":"single","workdir":"/ws"}"#,
        "\n",
        r#"{"event":"error","ts":2,"kind":"provider","message":"boom","recoverable":false}"#,
        "\n",
    );
    assert_eq!(
        Gantry::with_bin(PathBuf::from("/fake")).extract_answer(stdout),
        None
    );
}

// ---------------------------------------------------------------------------
// claude-code adapter

#[test]
fn claude_command_args_env_and_cwd() {
    let (_tmp, ctx) = make_ctx(false);
    let cmd = ClaudeCode.command(&ctx);

    assert_eq!(cmd.get_program(), OsStr::new("claude"));
    assert_eq!(cmd.get_current_dir(), Some(ctx.workspace.as_path()));

    let args = args_of(&cmd);
    assert!(args.iter().any(|a| a == "-p"));
    assert_eq!(
        flag_value(&args, "--output-format").as_deref(),
        Some("stream-json")
    );
    // verified: claude 2.1.157 hard-errors without --verbose in print
    // stream-json mode
    assert!(args.iter().any(|a| a == "--verbose"));
    assert_eq!(flag_value(&args, "--model").as_deref(), Some(MODEL));
    // read-only task: never skip permissions
    assert!(!args.iter().any(|a| a == "--dangerously-skip-permissions"));
    // verbatim prompt text is the trailing positional
    assert_eq!(args.last().map(String::as_str), Some(PROMPT));

    assert_eq!(env_value(&cmd, "ANTHROPIC_BASE_URL"), PROXY);
    assert_eq!(env_value(&cmd, "ANTHROPIC_API_KEY"), API_KEY);
    assert_eq!(
        env_value(&cmd, "CLAUDE_CONFIG_DIR"),
        ctx.config_dir.to_str().unwrap()
    );
    assert_eq!(
        env_value(&cmd, "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"),
        "1"
    );
    // OAuth/token auth paths bypass the base-URL override → must be scrubbed
    assert_eq!(env_of(&cmd, "ANTHROPIC_AUTH_TOKEN"), Some(None));
    assert_eq!(env_of(&cmd, "CLAUDE_CODE_OAUTH_TOKEN"), Some(None));
}

#[test]
fn claude_mutate_skips_permissions() {
    let (_tmp, ctx) = make_ctx(true);
    let args = args_of(&ClaudeCode.command(&ctx));
    assert!(args.iter().any(|a| a == "--dangerously-skip-permissions"));
    assert_eq!(args.last().map(String::as_str), Some(PROMPT));
}

#[test]
fn claude_answer_from_stream_json_result_message() {
    let stdout = concat!(
        r#"{"type":"system","subtype":"init","cwd":"/ws","session_id":"abc","model":"claude-test"}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Interim."}]}}"#,
        "\n",
        "stray non-json output\n",
        r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":1200,"num_turns":2,"result":"The final answer.","session_id":"abc"}"#,
        "\n",
    );
    assert_eq!(
        ClaudeCode.extract_answer(stdout),
        Some("The final answer.".to_string())
    );
}

#[test]
fn claude_answer_none_when_result_has_no_text() {
    // error subtypes omit the `result` field
    let stdout = concat!(
        r#"{"type":"system","subtype":"init","cwd":"/ws","session_id":"abc","model":"claude-test"}"#,
        "\n",
        r#"{"type":"result","subtype":"error_max_turns","is_error":true,"duration_ms":1200,"num_turns":50,"session_id":"abc"}"#,
        "\n",
    );
    assert_eq!(ClaudeCode.extract_answer(stdout), None);
}

#[test]
fn claude_answer_none_on_empty_stdout() {
    assert_eq!(ClaudeCode.extract_answer(""), None);
}

// ---------------------------------------------------------------------------
// pi adapter

#[test]
fn pi_command_args_env_and_cwd() {
    let (_tmp, ctx) = make_ctx(false);
    let cmd = Pi.command(&ctx);

    assert_eq!(cmd.get_program(), OsStr::new("omp"));
    assert_eq!(cmd.get_current_dir(), Some(ctx.workspace.as_path()));

    let args = args_of(&cmd);
    assert!(args.iter().any(|a| a == "-p"));
    // bare model id — omp takes no provider prefix
    assert_eq!(flag_value(&args, "--model").as_deref(), Some(MODEL));
    assert!(args.iter().any(|a| a == "--no-title"));
    // read-only task: no auto-approve
    assert!(!args.iter().any(|a| a == "--auto-approve"));
    // verbatim prompt text as trailing positional (not @file inclusion)
    assert_eq!(args.last().map(String::as_str), Some(PROMPT));

    assert_eq!(env_value(&cmd, "ANTHROPIC_BASE_URL"), PROXY);
    assert_eq!(env_value(&cmd, "ANTHROPIC_API_KEY"), API_KEY);
    assert_eq!(
        env_value(&cmd, "PI_CONFIG_DIR"),
        ctx.config_dir.to_str().unwrap()
    );
    assert_eq!(
        env_value(&cmd, "PI_CODING_AGENT_DIR"),
        ctx.config_dir.join("agent").to_str().unwrap()
    );
    assert_eq!(env_value(&cmd, "PI_NO_TITLE"), "1");
    // omp help: ANTHROPIC_OAUTH_TOKEN takes precedence over the API key →
    // would bypass the keyed proxy auth path; must be scrubbed
    assert_eq!(env_of(&cmd, "ANTHROPIC_OAUTH_TOKEN"), Some(None));
}

#[test]
fn pi_mutate_auto_approves() {
    let (_tmp, ctx) = make_ctx(true);
    let args = args_of(&Pi.command(&ctx));
    assert!(args.iter().any(|a| a == "--auto-approve"));
    assert_eq!(args.last().map(String::as_str), Some(PROMPT));
}

#[test]
fn pi_answer_is_trimmed_stdout() {
    assert_eq!(
        Pi.extract_answer("  The final answer.\n\n"),
        Some("The final answer.".to_string())
    );
}

#[test]
fn pi_answer_none_when_stdout_blank() {
    assert_eq!(Pi.extract_answer(""), None);
    assert_eq!(Pi.extract_answer("   \n\t\n"), None);
}
