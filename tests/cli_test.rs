use clap::Parser;
use gantry::cli::{parse_model_slug, Cli, ConfigError, Mode, Provider};
use std::fs;
use std::path::{Path, PathBuf};

fn temp_workdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gantry-cli-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp workdir");
    dir
}

fn write_prompt_file(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, contents).expect("write prompt file");
    path
}

fn base_cli(workdir: &Path, prompt_file: &Path) -> Cli {
    Cli {
        mode: Mode::Single,
        model: "anthropic/claude-sonnet-4".into(),
        workdir: workdir.to_path_buf(),
        prompt_file: prompt_file.to_path_buf(),
        max_tokens: 8192,
        timeout_ms: 60_000,
        inject_skills: vec![],
        system_file: None,
        subagent_system_file: None,
        tools: vec![],
    }
}

#[test]
fn parse_slug_routes_each_provider() {
    let cases = [
        (
            "anthropic/claude-sonnet-4",
            Provider::Anthropic,
            "claude-sonnet-4",
        ),
        ("openai/gpt-4o", Provider::OpenAi, "gpt-4o"),
        ("openai/o3-mini", Provider::OpenAi, "o3-mini"),
        (
            "gemini/gemini-2.0-flash",
            Provider::Gemini,
            "gemini-2.0-flash",
        ),
        ("cursor/composer-2.5", Provider::Cursor, "composer-2.5"),
        (
            "cursor/cursor-composer-2.5-fast",
            Provider::Cursor,
            "cursor-composer-2.5-fast",
        ),
    ];

    for (slug, provider, model) in cases {
        let (parsed_provider, parsed_model) = parse_model_slug(slug).expect("slug parses");
        assert_eq!(parsed_provider, provider, "provider for {slug}");
        assert_eq!(parsed_model, model, "model for {slug}");
    }
}

#[test]
fn parse_slug_splits_on_first_slash_only() {
    // Only the first '/' separates provider from model; any further slashes are
    // part of the model id, forwarded verbatim to the provider.
    let (provider, model) = parse_model_slug("openai/ft:gpt-4o/acme").expect("slug parses");
    assert_eq!(provider, Provider::OpenAi);
    assert_eq!(model, "ft:gpt-4o/acme");
}

#[test]
fn parse_slug_missing_provider_prefix_rejected() {
    for bare in ["gpt-4o", "opus", "claude-sonnet-4"] {
        assert_eq!(
            parse_model_slug(bare),
            Err(ConfigError::MissingProviderPrefix { model: bare.into() }),
            "bare model {bare}"
        );
    }
}

#[test]
fn parse_slug_unknown_provider_rejected() {
    assert_eq!(
        parse_model_slug("llama/llama-3"),
        Err(ConfigError::UnknownProvider {
            provider: "llama".into(),
        })
    );
}

#[test]
fn parse_slug_empty_model_rejected() {
    assert_eq!(
        parse_model_slug("anthropic/"),
        Err(ConfigError::EmptyModel {
            slug: "anthropic/".into(),
        })
    );
}

#[test]
fn parse_and_validate_canonicalises_workdir() {
    let dir = temp_workdir("canonical");
    let prompt = write_prompt_file(&dir, "prompt.txt", "hello");
    let sub = dir.join("sub");
    fs::create_dir_all(&sub).expect("create subdir");
    let workdir_arg = sub.join("..");
    let canonical = dir.canonicalize().expect("canonicalize dir");

    let validated = Cli::parse_and_validate_from([
        "gantry",
        "--mode",
        "single",
        "--model",
        "anthropic/claude-sonnet-4",
        "--workdir",
        workdir_arg.to_str().unwrap(),
        "--prompt-file",
        prompt.to_str().unwrap(),
        "--max-tokens",
        "8192",
        "--timeout-ms",
        "60000",
    ])
    .expect("parse_and_validate");

    assert_eq!(validated.workdir, canonical);
}

#[test]
fn parse_and_validate_returns_resolved_provider_and_bare_model() {
    let workdir = temp_workdir("resolved-provider");
    let prompt = write_prompt_file(&workdir, "prompt.txt", "hello");
    let prompt_str = prompt.to_str().unwrap();

    let validated = Cli::parse_and_validate_from([
        "gantry",
        "--mode",
        "single",
        "--model",
        "openai/gpt-4o",
        "--workdir",
        workdir.to_str().unwrap(),
        "--prompt-file",
        prompt_str,
        "--max-tokens",
        "8192",
        "--timeout-ms",
        "60000",
    ])
    .expect("parse_and_validate");

    assert_eq!(validated.provider, Provider::OpenAi);
    assert_eq!(validated.model, "gpt-4o");
}

#[test]
fn parses_repeated_inject_skill_flags() {
    let workdir = temp_workdir("inject-skills");
    let prompt = write_prompt_file(&workdir, "prompt.txt", "hello");

    let validated = Cli::parse_and_validate_from([
        "gantry",
        "--mode",
        "single",
        "--model",
        "openai/gpt-4o",
        "--workdir",
        workdir.to_str().unwrap(),
        "--prompt-file",
        prompt.to_str().unwrap(),
        "--max-tokens",
        "8192",
        "--timeout-ms",
        "60000",
        "--inject-skill",
        "code-review",
        "--inject-skill",
        "confidence-rating",
    ])
    .expect("parse_and_validate");

    assert_eq!(
        validated.inject_skills,
        ["code-review", "confidence-rating"]
    );
}

#[test]
fn inject_skills_default_empty_when_flag_absent() {
    let workdir = temp_workdir("inject-skills-default");
    let prompt = write_prompt_file(&workdir, "prompt.txt", "hello");

    let validated = Cli::parse_and_validate_from([
        "gantry",
        "--mode",
        "single",
        "--model",
        "openai/gpt-4o",
        "--workdir",
        workdir.to_str().unwrap(),
        "--prompt-file",
        prompt.to_str().unwrap(),
        "--max-tokens",
        "8192",
        "--timeout-ms",
        "60000",
    ])
    .expect("parse_and_validate");

    assert!(validated.inject_skills.is_empty());
}

#[test]
fn unroutable_model_returns_config_error() {
    let workdir = temp_workdir("unroutable");
    let prompt = write_prompt_file(&workdir, "prompt.txt", "hello");

    let result = Cli::parse_and_validate_from([
        "gantry",
        "--mode",
        "single",
        "--model",
        "llama-3",
        "--workdir",
        workdir.to_str().unwrap(),
        "--prompt-file",
        prompt.to_str().unwrap(),
        "--max-tokens",
        "8192",
        "--timeout-ms",
        "60000",
    ]);

    assert_eq!(
        result,
        Err(ConfigError::MissingProviderPrefix {
            model: "llama-3".into(),
        })
    );
}

#[test]
fn missing_workdir_returns_workdir_not_found() {
    let workdir = temp_workdir("missing-workdir-check");
    let prompt = write_prompt_file(&workdir, "prompt.txt", "hello");
    let cli = Cli {
        workdir: PathBuf::from("/nonexistent"),
        ..base_cli(&workdir, &prompt)
    };

    assert_eq!(
        cli.validate(),
        Err(ConfigError::WorkdirNotFound(PathBuf::from("/nonexistent")))
    );
}

#[test]
fn missing_prompt_file_returns_prompt_file_missing() {
    let workdir = temp_workdir("missing-prompt");
    let missing_prompt = workdir.join("does-not-exist.txt");
    let cli = base_cli(&workdir, &missing_prompt);

    assert_eq!(
        cli.validate(),
        Err(ConfigError::PromptFileMissing(missing_prompt))
    );
}

#[test]
fn parses_all_six_flags_from_argv() {
    let workdir = temp_workdir("argv");
    let prompt = write_prompt_file(&workdir, "prompt.txt", "hello");

    let cli = Cli::try_parse_from([
        "gantry",
        "--mode",
        "team",
        "--model",
        "openai/gpt-4o",
        "--workdir",
        workdir.to_str().unwrap(),
        "--prompt-file",
        prompt.to_str().unwrap(),
        "--max-tokens",
        "4096",
        "--timeout-ms",
        "120000",
    ])
    .expect("parse argv");

    assert_eq!(cli.mode, Mode::Team);
    assert_eq!(cli.model, "openai/gpt-4o");
    assert_eq!(cli.workdir, workdir);
    assert_eq!(cli.prompt_file, prompt);
    assert_eq!(cli.max_tokens, 4096);
    assert_eq!(cli.timeout_ms, 120_000);
}

#[test]
fn parses_system_file_into_validated() {
    let workdir = temp_workdir("system-file");
    let prompt = write_prompt_file(&workdir, "prompt.txt", "hello");
    let sys = write_prompt_file(&workdir, "system.md", "CUSTOM SYSTEM PERSONA");

    let validated = Cli::parse_and_validate_from([
        "gantry",
        "--mode",
        "single",
        "--model",
        "openai/gpt-4o",
        "--workdir",
        workdir.to_str().unwrap(),
        "--prompt-file",
        prompt.to_str().unwrap(),
        "--max-tokens",
        "8192",
        "--timeout-ms",
        "60000",
        "--system-file",
        sys.to_str().unwrap(),
    ])
    .expect("parse_and_validate");

    assert_eq!(
        validated.system_prompt.as_deref(),
        Some("CUSTOM SYSTEM PERSONA")
    );
    assert_eq!(validated.subagent_system_prompt, None);
}

#[test]
fn missing_system_file_returns_config_error() {
    let workdir = temp_workdir("system-file-missing");
    let prompt = write_prompt_file(&workdir, "prompt.txt", "hello");
    let missing = workdir.join("nope.md");

    let err = Cli::parse_and_validate_from([
        "gantry",
        "--mode",
        "single",
        "--model",
        "openai/gpt-4o",
        "--workdir",
        workdir.to_str().unwrap(),
        "--prompt-file",
        prompt.to_str().unwrap(),
        "--max-tokens",
        "8192",
        "--timeout-ms",
        "60000",
        "--system-file",
        missing.to_str().unwrap(),
    ])
    .unwrap_err();

    assert_eq!(err, ConfigError::SystemFileMissing(missing));
}

#[test]
fn system_prompt_default_none_when_flag_absent() {
    let workdir = temp_workdir("system-file-absent");
    let prompt = write_prompt_file(&workdir, "prompt.txt", "hello");

    let validated = Cli::parse_and_validate_from([
        "gantry",
        "--mode",
        "single",
        "--model",
        "openai/gpt-4o",
        "--workdir",
        workdir.to_str().unwrap(),
        "--prompt-file",
        prompt.to_str().unwrap(),
        "--max-tokens",
        "8192",
        "--timeout-ms",
        "60000",
    ])
    .expect("parse_and_validate");

    assert!(validated.system_prompt.is_none());
    assert!(validated.subagent_system_prompt.is_none());
}

#[test]
fn parses_repeated_tool_flags() {
    let workdir = temp_workdir("tool-allowlist");
    let prompt = write_prompt_file(&workdir, "prompt.txt", "hello");

    let validated = Cli::parse_and_validate_from([
        "gantry",
        "--mode",
        "single",
        "--model",
        "openai/gpt-4o",
        "--workdir",
        workdir.to_str().unwrap(),
        "--prompt-file",
        prompt.to_str().unwrap(),
        "--max-tokens",
        "8192",
        "--timeout-ms",
        "60000",
        "--tool",
        "read_file",
        "--tool",
        "git_diff",
    ])
    .expect("parse_and_validate");

    assert_eq!(validated.tools, ["read_file", "git_diff"]);
}

#[test]
fn unknown_tool_returns_config_error() {
    let workdir = temp_workdir("tool-unknown");
    let prompt = write_prompt_file(&workdir, "prompt.txt", "hello");

    let err = Cli::parse_and_validate_from([
        "gantry",
        "--mode",
        "single",
        "--model",
        "openai/gpt-4o",
        "--workdir",
        workdir.to_str().unwrap(),
        "--prompt-file",
        prompt.to_str().unwrap(),
        "--max-tokens",
        "8192",
        "--timeout-ms",
        "60000",
        "--tool",
        "bogus_tool",
    ])
    .unwrap_err();

    assert!(
        matches!(err, ConfigError::UnknownTool { .. }),
        "got {err:?}"
    );
}

#[test]
fn team_tool_rejected_in_single_mode() {
    let workdir = temp_workdir("tool-team-in-single");
    let prompt = write_prompt_file(&workdir, "prompt.txt", "hello");

    let err = Cli::parse_and_validate_from([
        "gantry",
        "--mode",
        "single",
        "--model",
        "openai/gpt-4o",
        "--workdir",
        workdir.to_str().unwrap(),
        "--prompt-file",
        prompt.to_str().unwrap(),
        "--max-tokens",
        "8192",
        "--timeout-ms",
        "60000",
        "--tool",
        "spawn_subagent",
    ])
    .unwrap_err();

    assert!(
        matches!(err, ConfigError::UnknownTool { .. }),
        "got {err:?}"
    );
}

#[test]
fn team_tool_allowed_in_team_mode() {
    let workdir = temp_workdir("tool-team-in-team");
    let prompt = write_prompt_file(&workdir, "prompt.txt", "hello");

    let validated = Cli::parse_and_validate_from([
        "gantry",
        "--mode",
        "team",
        "--model",
        "openai/gpt-4o",
        "--workdir",
        workdir.to_str().unwrap(),
        "--prompt-file",
        prompt.to_str().unwrap(),
        "--max-tokens",
        "8192",
        "--timeout-ms",
        "60000",
        "--tool",
        "spawn_subagent",
        "--tool",
        "read_file",
    ])
    .expect("parse_and_validate");

    assert_eq!(validated.tools, ["spawn_subagent", "read_file"]);
}

#[test]
fn tools_default_empty_when_flag_absent() {
    let workdir = temp_workdir("tool-default");
    let prompt = write_prompt_file(&workdir, "prompt.txt", "hello");

    let validated = Cli::parse_and_validate_from([
        "gantry",
        "--mode",
        "single",
        "--model",
        "openai/gpt-4o",
        "--workdir",
        workdir.to_str().unwrap(),
        "--prompt-file",
        prompt.to_str().unwrap(),
        "--max-tokens",
        "8192",
        "--timeout-ms",
        "60000",
    ])
    .expect("parse_and_validate");

    assert!(validated.tools.is_empty());
}
