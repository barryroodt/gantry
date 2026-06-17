use super::compress::CompressOutcome;
use super::retrieval::RetrievalStore;
use super::{
    ast_edit, ast_grep, decide_stop, edit_file, find_files, git_diff, list_files, read_file,
    retrieve, shell, skill_load, write_file, ToolError, ToolOutput,
};
use crate::events::{now_ms, truncate_args, GantryEvent};
use crate::provider::ToolSchema;
use std::path::PathBuf;

/// Tool names available in single mode (and the base set in team mode).
pub const BASE_TOOL_NAMES: &[&str] = &[
    "read_file",
    "list_files",
    "find_files",
    "git_diff",
    "ast_grep",
    "shell",
    "skill_load",
];

/// Mutating / opt-in tools: never default-allowed; surfaced + dispatchable only
/// when a profile's allowlist names them explicitly.
pub const OPTIN_TOOL_NAMES: &[&str] = &["ast_edit", "edit_file", "write_file"];

/// Control tools: harness-granted only (never via `--tool`, never in the default
/// sets). Explicit-only at dispatch, like opt-in tools.
pub const CONTROL_TOOL_NAMES: &[&str] = &[decide_stop::DECIDE_STOP];

/// Tool names the model may be granted via `--tool` or a profile `tools` list.
pub fn available_tool_names() -> Vec<&'static str> {
    BASE_TOOL_NAMES
        .iter()
        .chain(OPTIN_TOOL_NAMES.iter())
        .copied()
        .collect()
}

/// Native tool dispatcher. Owns workdir + emits `tool_call` / `tool_result` pairs around each call.
pub struct ToolRegistry {
    workdir: PathBuf,
    skills_dir: PathBuf,
    allow: Vec<String>,
    shell_allow: Vec<String>,
    control: Vec<String>,
    store: RetrievalStore,
}

impl ToolRegistry {
    pub fn new(workdir: PathBuf, allow: Vec<String>) -> Self {
        let skills_dir = workdir.join(".claude/skills");
        Self {
            workdir,
            skills_dir,
            allow,
            shell_allow: shell::ALLOWED_PROGRAMS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            control: vec![retrieve::RETRIEVE.to_string()],
            store: RetrievalStore::new(),
        }
    }

    /// Returns a reference to the underlying [`RetrievalStore`] used by this registry.
    pub(crate) fn retrieval_store(&self) -> &RetrievalStore {
        &self.store
    }

    /// Override the shell program allowlist (from a profile's `shell_allow`).
    /// Empty input keeps the default read-only set.
    #[must_use]
    pub fn with_shell_allow(mut self, allow: Vec<String>) -> Self {
        if !allow.is_empty() {
            self.shell_allow = allow;
        }
        self
    }

    /// Override the skills resolution root (from `--skills-dir`). Defaults to
    /// `<workdir>/.claude/skills`. Pass `validated.skills_dir` at call sites
    /// where the flag may have been supplied.
    #[must_use]
    pub fn with_skills_dir(mut self, skills_dir: PathBuf) -> Self {
        self.skills_dir = skills_dir;
        self
    }

    /// Grant a control tool (e.g. `decide_stop`) for this registry only. Control
    /// tools are always allowed here and surfaced in `schemas()`, independent of
    /// the `--tool`/profile allowlist — so loop mode can add `decide_stop` without
    /// collapsing the default "empty allow = all base tools" semantics.
    #[must_use]
    pub fn with_control(mut self, name: &str) -> Self {
        self.control.push(name.to_string());
        self
    }

    fn base_schemas() -> Vec<ToolSchema> {
        vec![
            ToolSchema {
                name: "read_file".into(),
                description: "Read a file from the workdir.".into(),
                json_schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"outline":{"type":"boolean"}},"required":["path"]}),
            },
            ToolSchema {
                name: "list_files".into(),
                description: "List files under a workdir path (max depth 5).".into(),
                json_schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"max_depth":{"type":"integer"}},"required":["path"]}),
            },
            ToolSchema {
                name: "find_files".into(),
                description: "Glob for files under workdir.".into(),
                json_schema: serde_json::json!({"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern"]}),
            },
            ToolSchema {
                name: "git_diff".into(),
                description: "Run git diff in the workdir.".into(),
                json_schema: serde_json::json!({"type":"object","properties":{"range":{"type":"string"},"paths":{"type":"array","items":{"type":"string"}}}}),
            },
            ToolSchema {
                name: "ast_grep".into(),
                description: "Structural (AST) code search: pattern -> match locations.".into(),
                json_schema: serde_json::json!({"type":"object","properties":{"pattern":{"type":"string"},"paths":{"type":"array","items":{"type":"string"}},"lang":{"type":"string"}},"required":["pattern"]}),
            },
            ToolSchema {
                name: "shell".into(),
                description:
                    "Run a bash command in the workdir; only allowlisted programs may be invoked."
                        .into(),
                json_schema: serde_json::json!({"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}),
            },
            ToolSchema {
                name: "skill_load".into(),
                description: "Load a skill from workdir .claude/skills (no bundled fallback)."
                    .into(),
                json_schema: serde_json::json!({"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}),
            },
        ]
    }

    /// Opt-in (mutating) tool schemas — surfaced only when explicitly allowed.
    fn optin_schemas() -> Vec<ToolSchema> {
        vec![
            ToolSchema {
                name: "ast_edit".into(),
                description:
                    "Structural (AST) code REWRITE (mutating): pattern -> rewrite across files."
                        .into(),
                json_schema: serde_json::json!({"type":"object","properties":{"pattern":{"type":"string"},"rewrite":{"type":"string"},"paths":{"type":"array","items":{"type":"string"}}},"required":["pattern","rewrite"]}),
            },
            ToolSchema {
                name: "write_file".into(),
                description: "Create or overwrite a workdir file (mutating).".into(),
                json_schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}),
            },
            ToolSchema {
                name: "edit_file".into(),
                description: "Literal, count-guarded search/replace in a workdir file (mutating)."
                    .into(),
                json_schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"search":{"type":"string"},"replace":{"type":"string"},"expected_count":{"type":"integer"}},"required":["path","search","replace"]}),
            },
        ]
    }

    /// Control-tool schemas (e.g. `decide_stop`) — surfaced only when the loop
    /// registry grants them explicitly.
    fn control_schemas() -> Vec<ToolSchema> {
        vec![
            ToolSchema {
                name: decide_stop::DECIDE_STOP.into(),
                description: "Stop the iterative loop after this pass (loop mode only).".into(),
                json_schema: serde_json::json!({"type":"object","properties":{"reason":{"type":"string"}}}),
            },
            ToolSchema {
                name: retrieve::RETRIEVE.into(),
                description: "Recover elided content from a prior compressed tool_result by its handle (shown in the tool_result hint). Omit start/end/pattern for the elided middle; or pass a 1-based inclusive start/end line range; or a regex `pattern` (returns matching lines + context).".into(),
                json_schema: serde_json::json!({"type":"object","properties":{"handle":{"type":"string"},"start":{"type":"integer"},"end":{"type":"integer"},"pattern":{"type":"string"}},"required":["handle"]}),
            },
        ]
    }

    /// Every tool schema (base ++ opt-in ++ control), unfiltered.
    fn all_schemas() -> Vec<ToolSchema> {
        let mut all = Self::base_schemas();
        all.extend(Self::optin_schemas());
        all.extend(Self::control_schemas());
        all
    }

    /// Single source of truth for tool visibility, shared by `schemas` and
    /// `dispatch`: base tools are on by default (or when named); opt-in/control
    /// tools require an explicit grant; control tools may also be granted
    /// out-of-band via `with_control`.
    fn is_allowed(&self, name: &str) -> bool {
        if self.control.iter().any(|t| t == name) {
            return true;
        }
        let explicit_only = OPTIN_TOOL_NAMES.contains(&name) || CONTROL_TOOL_NAMES.contains(&name);
        if explicit_only {
            self.allow.iter().any(|t| t == name)
        } else {
            self.allow.is_empty() || self.allow.iter().any(|t| t == name)
        }
    }

    /// JSON schemas to send to the provider as available tools.
    pub fn schemas(&self) -> Vec<ToolSchema> {
        let mut schemas = Self::all_schemas();
        schemas.retain(|s| self.is_allowed(&s.name));
        schemas
    }

    /// Dispatch one tool call. Emits `tool_call` and `tool_result` NDJSON events around execution.
    /// On tool error returns the error string in the ToolOutput.content with `error: <msg>` so the agent loop sees a tool_result, not a Rust Err
    /// (invariant #5: tools never abort the run).
    pub async fn dispatch(&self, role: &str, turn: u32, name: &str, args_json: &str) -> ToolOutput {
        let args_for_event = truncate_args(args_json, 1024);
        let _ = GantryEvent::ToolCall {
            ts: now_ms(),
            role: role.into(),
            turn,
            tool: name.into(),
            args: args_for_event,
        }
        .emit();

        let result = self.dispatch_inner(name, args_json).await;
        let (output, error) = match result {
            Ok(o) => (o, None),
            Err(e) => {
                let msg = format!("error: {e}");
                (
                    ToolOutput {
                        bytes: msg.len(),
                        truncated: false,
                        content: msg.clone(),
                    },
                    Some(msg),
                )
            }
        };

        // SP5: structured, recoverable compression at the tool-result boundary.
        let CompressOutcome { output, stash } = super::compress::compress(name, output);
        let handle = match stash {
            Some(s) => {
                self.store.insert(&s.handle, s.original);
                Some(s.handle)
            }
            None => None,
        };

        let _ = GantryEvent::ToolResult {
            ts: now_ms(),
            role: role.into(),
            turn,
            tool: name.into(),
            bytes: output.bytes as u64,
            bytes_out: output.content.len() as u64,
            truncated: output.truncated,
            error,
            handle,
        }
        .emit();
        output
    }

    async fn dispatch_inner(&self, name: &str, args_json: &str) -> Result<ToolOutput, ToolError> {
        let allowed = self.is_allowed(name);
        if !allowed {
            return Err(ToolError::UnknownTool(name.to_string()));
        }
        match name {
            "read_file" => {
                let args: read_file::ReadFileArgs = serde_json::from_str(args_json)
                    .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
                read_file::read_file(&self.workdir, args).await
            }
            "list_files" => {
                let args: list_files::ListFilesArgs = serde_json::from_str(args_json)
                    .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
                list_files::list_files(&self.workdir, args).await
            }
            "find_files" => {
                let args: find_files::FindFilesArgs = serde_json::from_str(args_json)
                    .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
                find_files::find_files(&self.workdir, args).await
            }
            "git_diff" => {
                let args: git_diff::GitDiffArgs = serde_json::from_str(args_json)
                    .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
                git_diff::git_diff(&self.workdir, args).await
            }
            "ast_grep" => {
                let args: ast_grep::AstGrepArgs = serde_json::from_str(args_json)
                    .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
                ast_grep::ast_grep(&self.workdir, args).await
            }
            "ast_edit" => {
                let args: ast_edit::AstEditArgs = serde_json::from_str(args_json)
                    .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
                ast_edit::ast_edit(&self.workdir, args).await
            }
            "shell" => {
                let args: shell::ShellArgs = serde_json::from_str(args_json)
                    .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
                shell::shell(&self.workdir, args, &self.shell_allow).await
            }
            "skill_load" => {
                let args: skill_load::SkillLoadArgs = serde_json::from_str(args_json)
                    .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
                skill_load::skill_load(&self.skills_dir, args).await
            }
            "write_file" => {
                let args: write_file::WriteFileArgs = serde_json::from_str(args_json)
                    .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
                write_file::write_file(&self.workdir, args).await
            }
            "edit_file" => {
                let args: edit_file::EditFileArgs = serde_json::from_str(args_json)
                    .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
                edit_file::edit_file(&self.workdir, args).await
            }
            "decide_stop" => {
                let args: decide_stop::DecideStopArgs = serde_json::from_str(args_json)
                    .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
                decide_stop::decide_stop(args).await
            }
            "retrieve" => {
                let args: retrieve::RetrieveArgs = serde_json::from_str(args_json)
                    .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
                retrieve::retrieve(&self.store, args)
            }
            other => Err(ToolError::UnknownTool(other.into())),
        }
    }
}
