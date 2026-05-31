use super::truncate::truncated_output;
use super::{ToolError, ToolOutput};
use crate::skills::{resolve_skill_file, SkillLoader, SkillPathError};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Deserialize, Serialize)]
pub struct SkillLoadArgs {
    pub name: String,
}

/// Lazy skill_load: workdir-only resolution (no bundled fallback, per ADR-0002).
///
/// Distinguishes a missing skill (`InvalidInput "skill not found"`) from a
/// symlink that escapes the workdir (`OutsideWorkdir`). The wrapped output is
/// bounded by the shared 256 KiB truncation cap — truncating the body *before*
/// wrapping so the `<skill>` envelope always closes even when a large
/// `SKILL.md` is cut at the boundary.
pub async fn skill_load(workdir: &Path, args: SkillLoadArgs) -> Result<ToolOutput, ToolError> {
    if !SkillLoader::valid_name(&args.name) {
        return Err(ToolError::InvalidInput(format!(
            "invalid skill name: {}",
            args.name
        )));
    }
    let canonical = resolve_skill_file(workdir, &args.name).map_err(|e| match e {
        SkillPathError::NotFound => {
            ToolError::InvalidInput(format!("skill not found: {}", args.name))
        }
        SkillPathError::OutsideWorkdir => ToolError::OutsideWorkdir(args.name.clone()),
    })?;
    let content = tokio::fs::read_to_string(&canonical).await?;
    // Truncate first, then wrap, so the envelope always closes.
    let body = truncated_output(content);
    let wrapped = format!("<skill name=\"{}\">\n{}\n</skill>", args.name, body.content);
    let bytes = wrapped.len();
    Ok(ToolOutput {
        content: wrapped,
        bytes,
        truncated: body.truncated,
    })
}
