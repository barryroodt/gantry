//! Task **profile** loading (ADR-0004). A profile is a directory with a
//! `profile.toml` manifest plus prompt files; `--profile <DIR>` applies it,
//! with explicit CLI flags taking precedence over profile values.

use crate::cli::Mode;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct ProfileManifest {
    mode: Option<Mode>,
    system: Option<PathBuf>,
    subagent_system: Option<PathBuf>,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    inject_skills: Vec<String>,
}

/// A profile resolved to concrete values: the referenced prompt files are read
/// into strings (paths are relative to the profile directory).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedProfile {
    pub mode: Option<Mode>,
    pub system_prompt: Option<String>,
    pub subagent_system_prompt: Option<String>,
    pub tools: Vec<String>,
    pub inject_skills: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    #[error("profile manifest not found: {}", .0.display())]
    NotFound(PathBuf),
    #[error("profile manifest parse error in {path}: {msg}")]
    Parse { path: String, msg: String },
    #[error("profile file not found: {}", .0.display())]
    FileMissing(PathBuf),
    #[error("profile file is not readable: {}", .0.display())]
    FileUnreadable(PathBuf),
}

/// Load `<dir>/profile.toml` and read its referenced prompt files.
pub fn load_profile(dir: &Path) -> Result<LoadedProfile, ProfileError> {
    let manifest_path = dir.join("profile.toml");
    let raw = std::fs::read_to_string(&manifest_path)
        .map_err(|_| ProfileError::NotFound(manifest_path.clone()))?;
    let manifest: ProfileManifest = toml::from_str(&raw).map_err(|e| ProfileError::Parse {
        path: manifest_path.display().to_string(),
        msg: e.to_string(),
    })?;
    Ok(LoadedProfile {
        mode: manifest.mode,
        system_prompt: read_profile_file(dir, manifest.system.as_deref())?,
        subagent_system_prompt: read_profile_file(dir, manifest.subagent_system.as_deref())?,
        tools: manifest.tools,
        inject_skills: manifest.inject_skills,
    })
}

fn read_profile_file(dir: &Path, rel: Option<&Path>) -> Result<Option<String>, ProfileError> {
    let Some(rel) = rel else {
        return Ok(None);
    };
    let path = dir.join(rel);
    if !path.exists() {
        return Err(ProfileError::FileMissing(path));
    }
    std::fs::read_to_string(&path)
        .map(Some)
        .map_err(|_| ProfileError::FileUnreadable(path))
}
