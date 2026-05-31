use crate::events::{now_ms, GantryEvent};
use std::path::{Path, PathBuf};

/// Failure modes for [`resolve_skill_file`].
pub(crate) enum SkillPathError {
    /// The workdir or skill file does not exist / cannot be canonicalized.
    NotFound,
    /// The resolved path escapes the workdir (e.g. via a symlink).
    OutsideWorkdir,
}

/// Resolve `<workdir>/.claude/skills/<name>/SKILL.md`, enforcing the workdir
/// escape guard. This is the single place the path-traversal / symlink boundary
/// lives; both [`SkillLoader::resolve`] and the `skill_load` tool route through
/// it. Name validation is the caller's responsibility.
pub(crate) fn resolve_skill_file(workdir: &Path, name: &str) -> Result<PathBuf, SkillPathError> {
    let workdir = workdir
        .canonicalize()
        .map_err(|_| SkillPathError::NotFound)?;
    let canonical = workdir
        .join(".claude")
        .join("skills")
        .join(name)
        .join("SKILL.md")
        .canonicalize()
        .map_err(|_| SkillPathError::NotFound)?;
    if !canonical.starts_with(&workdir) {
        return Err(SkillPathError::OutsideWorkdir);
    }
    Ok(canonical)
}

pub struct ResolvedSkill {
    pub name: String,
    pub content: String,
    pub bytes: u64,
}

pub struct SkillLoader {
    workdir: PathBuf,
}

impl SkillLoader {
    pub fn new(workdir: PathBuf) -> Self {
        Self { workdir }
    }

    /// Validate skill name per ADR-0002 (`^[A-Za-z0-9_-]+$`, max 64 chars).
    pub fn valid_name(name: &str) -> bool {
        !name.is_empty()
            && name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }

    /// Resolve a single skill from the workdir. Returns `None` if not found.
    pub fn resolve(&self, name: &str) -> Option<ResolvedSkill> {
        if !Self::valid_name(name) {
            return None;
        }
        let canonical = resolve_skill_file(&self.workdir, name).ok()?;
        let content = std::fs::read_to_string(&canonical).ok()?;
        let bytes = content.len() as u64;
        Some(ResolvedSkill {
            name: name.to_string(),
            content,
            bytes,
        })
    }

    /// Inject the orchestrator-supplied `names` into the system prompt: resolve
    /// each from `{workdir}/.claude/skills/<name>/SKILL.md`, emit one
    /// `skill_loaded` per skill found, and build the concatenated prefix. Names
    /// absent from the workdir are skipped with a stderr warning.
    pub fn inject_core_skills(&self, names: &[String]) -> String {
        let mut prefix = String::new();
        for name in names {
            match self.resolve(name) {
                Some(skill) => {
                    prefix.push_str(&format!("<skill name=\"{}\">\n", skill.name));
                    prefix.push_str(&skill.content);
                    prefix.push_str("\n</skill>\n\n");
                    let _ = GantryEvent::SkillLoaded {
                        ts: now_ms(),
                        name: skill.name.clone(),
                        bytes: skill.bytes,
                    }
                    .emit();
                }
                None => {
                    eprintln!("warning: auto-inject skill {name} not resolved");
                }
            }
        }
        prefix
    }
}
