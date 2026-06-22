use clap::{Parser, ValueEnum};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Parser, Debug, Clone)]
#[command(name = "gantry", about = "Gantry harness sidecar")]
pub struct Cli {
    #[arg(long)]
    pub mode: Option<Mode>,

    /// Model identifier in `provider/model` slug form, e.g.
    /// `anthropic/claude-opus-4-8` or `openai/gpt-4o`. The provider segment
    /// (anthropic|openai|gemini) selects the adapter; everything after
    /// the first `/` is the bare model id forwarded to that provider's API.
    #[arg(long, value_name = "PROVIDER/MODEL")]
    pub model: String,

    #[arg(long)]
    pub workdir: PathBuf,

    #[arg(long = "prompt-file")]
    pub prompt_file: PathBuf,

    #[arg(long = "max-tokens")]
    pub max_tokens: u64,

    #[arg(long = "timeout-ms")]
    pub timeout_ms: u64,
    /// Skill names to auto-inject into the system prompt at startup, resolved
    /// from `<workdir>/.claude/skills/<name>/SKILL.md`. Repeatable; the
    /// orchestrator decides the set. Absent names are skipped with a warning.
    #[arg(long = "inject-skill", value_name = "NAME")]
    pub inject_skills: Vec<String>,
    /// System prompt body for the agent (single/team-coordinator persona),
    /// read from this file. If omitted, a minimal neutral default is used; the
    /// orchestrator supplies the real persona (e.g. a review profile).
    #[arg(long = "system-file", value_name = "PATH")]
    pub system_file: Option<PathBuf>,

    /// System prompt body for spawned subagents (team subagent base persona),
    /// read from this file. If omitted, a minimal neutral default is used.
    #[arg(long = "subagent-system-file", value_name = "PATH")]
    pub subagent_system_file: Option<PathBuf>,

    /// Compose phase system prompt for team mode, read from this file. Overrides
    /// the profile's `compose` file. Silently ignored outside team mode.
    #[arg(long = "compose-file", value_name = "PATH")]
    pub compose_file: Option<PathBuf>,

    /// Unify phase system prompt for team mode, read from this file. Overrides
    /// the profile's `unify` file. Silently ignored outside team mode.
    #[arg(long = "unify-file", value_name = "PATH")]
    pub unify_file: Option<PathBuf>,

    /// Root directory for skill resolution (both `--inject-skill` and the
    /// `skill_load` tool). Defaults to `<workdir>/.claude/skills`. Trusted
    /// config — not workdir-confined — but skill names within it still receive
    /// the lexical `..` / symlink guard.
    #[arg(long = "skills-dir", value_name = "DIR")]
    pub skills_dir: Option<PathBuf>,

    /// Restrict the tools exposed to the agent (repeatable). Default: all tools
    /// available for the selected mode. Each name must be valid for the mode.
    #[arg(long = "tool", value_name = "NAME")]
    pub tools: Vec<String>,

    /// Load a profile directory (`profile.toml` + prompt files): sets mode,
    /// system/subagent prompts, tool allowlist, and inject-skills. Explicit
    /// flags override profile values.
    #[arg(long = "profile", value_name = "DIR")]
    pub profile: Option<PathBuf>,

    /// Run the agent against a COW shadow of the workdir (pi-iso): mutations land
    /// in an overlay, the original workdir is untouched, and a terminal `changes`
    /// event lists what changed. Fail-closed — if no isolation backend is
    /// available the run errors. Also enabled by a profile `isolate = true`.
    #[arg(long)]
    pub isolate: bool,

    /// Max iterations for `--mode loop` (default 5; min 1). Overrides a profile
    /// `max_iterations`. Ignored in single/team modes.
    #[arg(long = "max-iterations", value_name = "N")]
    pub max_iterations: Option<u32>,

    /// Compact old tool results out of the transcript once the previous turn's
    /// total context occupancy (uncached input + cache reads + cache writes)
    /// exceeds this many tokens (opt-in; recoverable via the retrieve tool). Off
    /// by default. Keeps long runs under the model's context window. Single/loop
    /// modes only — no effect in team mode (its passes are bounded).
    #[arg(long = "context-limit", value_name = "TOKENS", value_parser = clap::value_parser!(u64).range(1..))]
    pub context_limit: Option<u64>,

    /// Base URL for the `local` provider's OpenAI-compatible endpoint, e.g.
    /// `http://localhost:8000/v1` (oMLX). Takes precedence over
    /// `GANTRY_LOCAL_BASE_URL`. Only valid with a `local/...` model slug.
    #[arg(long = "base-url", value_name = "URL")]
    pub base_url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, ValueEnum, Deserialize)]
#[clap(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Single,
    Team,
    Loop,
}

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Team => "team",
            Self::Loop => "loop",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, ValueEnum)]
#[clap(rename_all = "lowercase")]
pub enum Provider {
    Anthropic,
    OpenAi,
    Gemini,
    /// OpenRouter unified gateway (OpenAI wire-compatible). Model ids keep
    /// their vendor prefix and are forwarded verbatim, e.g.
    /// `openrouter/anthropic/claude-3.5-sonnet` → model `anthropic/claude-3.5-sonnet`.
    OpenRouter,
    /// Generic OpenAI-compatible local/self-hosted server (e.g. oMLX, Ollama,
    /// vLLM, LM Studio). Endpoint via `--base-url`/`GANTRY_LOCAL_BASE_URL`.
    Local,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::Gemini => "google",
            Self::OpenRouter => "openrouter",
            Self::Local => "local",
        }
    }

    /// Parse a provider slug segment (lowercase) into a `Provider`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "anthropic" => Some(Self::Anthropic),
            "openai" => Some(Self::OpenAi),
            "google" => Some(Self::Gemini),
            "openrouter" => Some(Self::OpenRouter),
            "local" => Some(Self::Local),
            _ => None,
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error(
        "model must be in 'provider/model' form (e.g. anthropic/claude-opus-4-8), got: {model}"
    )]
    MissingProviderPrefix { model: String },

    #[error(
        "unknown provider '{provider}' in model slug (expected anthropic|openai|gemini|local)"
    )]
    UnknownProvider { provider: String },

    #[error("model id is empty after provider in slug: {slug}")]
    EmptyModel { slug: String },

    #[error("--base-url is only valid with the local provider (a 'local/...' model slug), but provider is '{provider}'")]
    BaseUrlNotLocal { provider: String },

    #[error("CLI parse error: {0}")]
    CliParse(String),

    #[error("prompt file not found: {}", .0.display())]
    PromptFileMissing(PathBuf),

    #[error("prompt file is not readable: {}", .0.display())]
    PromptFileNotReadable(PathBuf),

    #[error("workdir not found: {}", .0.display())]
    WorkdirNotFound(PathBuf),

    #[error("workdir is not a directory: {}", .0.display())]
    WorkdirNotDirectory(PathBuf),
    #[error("system file not found: {}", .0.display())]
    SystemFileMissing(PathBuf),

    #[error("system file is not readable: {}", .0.display())]
    SystemFileNotReadable(PathBuf),
    #[error("unknown tool '{name}' (available for mode: {available})")]
    UnknownTool { name: String, available: String },
    #[error("mode is required: pass --mode or set `mode` in the profile")]
    ModeRequired,

    #[error(transparent)]
    Profile(#[from] crate::profile::ProfileError),
}

impl From<clap::Error> for ConfigError {
    fn from(err: clap::Error) -> Self {
        ConfigError::CliParse(err.to_string())
    }
}

/// Fully validated CLI configuration ready for the harness run loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validated {
    pub mode: Mode,
    pub model: String,
    pub provider: Provider,
    pub workdir: PathBuf,
    pub prompt_file: PathBuf,
    pub max_tokens: u64,
    pub timeout_ms: u64,
    pub inject_skills: Vec<String>,
    pub system_prompt: Option<String>,
    pub subagent_system_prompt: Option<String>,
    pub compose_prompt: Option<String>,
    pub unify_prompt: Option<String>,
    pub tools: Vec<String>,
    pub shell_allow: Vec<String>,
    pub isolate: bool,
    pub max_iterations: u32,
    pub context_limit: Option<u64>,
    pub base_url: Option<String>,
    pub skills_dir: PathBuf,
}

impl Cli {
    pub fn parse_and_validate() -> Result<Validated, ConfigError> {
        Self::try_parse()
            .map_err(ConfigError::from)
            .and_then(Self::into_validated)
    }

    pub fn parse_and_validate_from<I, T>(iter: I) -> Result<Validated, ConfigError>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        Self::try_parse_from(iter)
            .map_err(ConfigError::from)
            .and_then(Self::into_validated)
    }

    fn into_validated(self) -> Result<Validated, ConfigError> {
        let workdir = self
            .workdir
            .canonicalize()
            .map_err(|_| ConfigError::WorkdirNotFound(self.workdir.clone()))?;
        if !workdir.is_dir() {
            return Err(ConfigError::WorkdirNotFound(workdir));
        }
        if !self.prompt_file.exists() {
            return Err(ConfigError::PromptFileMissing(self.prompt_file.clone()));
        }
        let (provider, model) = parse_model_slug(&self.model)?;
        if self.base_url.is_some() && provider != Provider::Local {
            return Err(ConfigError::BaseUrlNotLocal {
                provider: provider.as_str().to_string(),
            });
        }

        let profile = match self.profile.as_deref() {
            Some(dir) => Some(crate::profile::load_profile(dir)?),
            None => None,
        };

        let mode = self
            .mode
            .or_else(|| profile.as_ref().and_then(|p| p.mode.clone()))
            .ok_or(ConfigError::ModeRequired)?;

        let system_prompt = match read_optional_system_file(self.system_file.as_deref())? {
            Some(s) => Some(s),
            None => profile.as_ref().and_then(|p| p.system_prompt.clone()),
        };
        let subagent_system_prompt =
            match read_optional_system_file(self.subagent_system_file.as_deref())? {
                Some(s) => Some(s),
                None => profile
                    .as_ref()
                    .and_then(|p| p.subagent_system_prompt.clone()),
            };
        let compose_prompt = match read_optional_system_file(self.compose_file.as_deref())? {
            Some(s) => Some(s),
            None => profile.as_ref().and_then(|p| p.compose_prompt.clone()),
        };
        let unify_prompt = match read_optional_system_file(self.unify_file.as_deref())? {
            Some(s) => Some(s),
            None => profile.as_ref().and_then(|p| p.unify_prompt.clone()),
        };
        let skills_dir = self
            .skills_dir
            .unwrap_or_else(|| workdir.join(".claude/skills"));

        let available = crate::tools::registry::available_tool_names();
        let tools = if self.tools.is_empty() {
            // Profile tools are a template validated against the available set;
            // unknown names (e.g. legacy orchestration names) are dropped.
            profile
                .as_ref()
                .map(|p| {
                    p.tools
                        .iter()
                        .filter(|t| available.contains(&t.as_str()))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        } else {
            // Explicit --tool flags are validated strictly.
            for tool in &self.tools {
                if !available.contains(&tool.as_str()) {
                    return Err(ConfigError::UnknownTool {
                        name: tool.clone(),
                        available: available.join(", "),
                    });
                }
            }
            self.tools
        };

        let inject_skills = if self.inject_skills.is_empty() {
            profile
                .as_ref()
                .map(|p| p.inject_skills.clone())
                .unwrap_or_default()
        } else {
            self.inject_skills
        };
        let shell_allow = profile
            .as_ref()
            .map(|p| p.shell_allow.clone())
            .unwrap_or_default();
        // Isolation is opt-in and additive: enabled by the flag or the profile.
        let isolate = self.isolate || profile.as_ref().map(|p| p.isolate).unwrap_or(false);
        // Loop iteration cap: CLI flag, else profile, else default 5 (min 1).
        let max_iterations = self
            .max_iterations
            .or_else(|| profile.as_ref().and_then(|p| p.max_iterations))
            .unwrap_or(5)
            .max(1);

        Ok(Validated {
            mode,
            model,
            provider,
            workdir,
            prompt_file: self.prompt_file,
            max_tokens: self.max_tokens,
            timeout_ms: self.timeout_ms,
            inject_skills,
            system_prompt,
            subagent_system_prompt,
            compose_prompt,
            unify_prompt,
            tools,
            shell_allow,
            isolate,
            max_iterations,
            context_limit: self.context_limit,
            base_url: self.base_url,
            skills_dir,
        })
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        validate_workdir(&self.workdir)?;
        validate_prompt_file(&self.prompt_file)?;
        Ok(())
    }
}

/// Parse a `provider/model` slug into its provider and bare model id. The
/// provider segment selects the adapter; everything after the first `/` is the
/// model id forwarded verbatim to that provider's API.
pub fn parse_model_slug(slug: &str) -> Result<(Provider, String), ConfigError> {
    let (provider, model) =
        slug.split_once('/')
            .ok_or_else(|| ConfigError::MissingProviderPrefix {
                model: slug.to_string(),
            })?;
    let provider = Provider::from_name(provider).ok_or_else(|| ConfigError::UnknownProvider {
        provider: provider.to_string(),
    })?;
    if model.is_empty() {
        return Err(ConfigError::EmptyModel {
            slug: slug.to_string(),
        });
    }
    Ok((provider, model.to_string()))
}

fn validate_workdir(workdir: &Path) -> Result<(), ConfigError> {
    if !workdir.exists() {
        return Err(ConfigError::WorkdirNotFound(workdir.to_path_buf()));
    }
    if !workdir.is_dir() {
        return Err(ConfigError::WorkdirNotDirectory(workdir.to_path_buf()));
    }
    Ok(())
}

fn validate_prompt_file(prompt_file: &Path) -> Result<(), ConfigError> {
    if !prompt_file.exists() {
        return Err(ConfigError::PromptFileMissing(prompt_file.to_path_buf()));
    }
    if !prompt_file.is_file() {
        return Err(ConfigError::PromptFileNotReadable(
            prompt_file.to_path_buf(),
        ));
    }
    if std::fs::File::open(prompt_file).is_err() {
        return Err(ConfigError::PromptFileNotReadable(
            prompt_file.to_path_buf(),
        ));
    }
    Ok(())
}

/// Read an optional system-prompt file. `None` path → `None`. Missing file →
/// [`ConfigError::SystemFileMissing`]; unreadable → [`ConfigError::SystemFileNotReadable`].
fn read_optional_system_file(path: Option<&Path>) -> Result<Option<String>, ConfigError> {
    match path {
        None => Ok(None),
        Some(p) if !p.exists() => Err(ConfigError::SystemFileMissing(p.to_path_buf())),
        Some(p) => std::fs::read_to_string(p)
            .map(Some)
            .map_err(|_| ConfigError::SystemFileNotReadable(p.to_path_buf())),
    }
}
