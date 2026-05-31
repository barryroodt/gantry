use clap::{Parser, ValueEnum};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Parser, Debug, Clone)]
#[command(name = "gantry", about = "Gantry harness sidecar")]
pub struct Cli {
    #[arg(long)]
    pub mode: Mode,

    /// Model identifier in `provider/model` slug form, e.g.
    /// `anthropic/claude-opus-4-8` or `openai/gpt-4o`. The provider segment
    /// (anthropic|openai|gemini|cursor) selects the adapter; everything after
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
}

#[derive(Clone, Debug, Eq, PartialEq, ValueEnum)]
#[clap(rename_all = "lowercase")]
pub enum Mode {
    Single,
    Team,
}

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Team => "team",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, ValueEnum)]
#[clap(rename_all = "lowercase")]
pub enum Provider {
    Anthropic,
    OpenAi,
    Gemini,
    Cursor,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::Gemini => "gemini",
            Self::Cursor => "cursor",
        }
    }

    /// Parse a provider slug segment (lowercase) into a `Provider`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "anthropic" => Some(Self::Anthropic),
            "openai" => Some(Self::OpenAi),
            "gemini" => Some(Self::Gemini),
            "cursor" => Some(Self::Cursor),
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
        "unknown provider '{provider}' in model slug (expected anthropic|openai|gemini|cursor)"
    )]
    UnknownProvider { provider: String },

    #[error("model id is empty after provider in slug: {slug}")]
    EmptyModel { slug: String },

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
        Ok(Validated {
            mode: self.mode,
            model,
            provider,
            workdir,
            prompt_file: self.prompt_file,
            max_tokens: self.max_tokens,
            timeout_ms: self.timeout_ms,
            inject_skills: self.inject_skills,
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
