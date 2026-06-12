//! Task manifest (`task.toml`) parsing + workspace materialization.
//!
//! A bench task lives in `bench/tasks/<id>/` as `task.toml` + `prompt.md` +
//! optional `rubric.md` (plan §Canonical Schema, task.toml contract). The
//! rubric is conveyed by the presence of `rubric.md`, not by a manifest field.
//!
//! Workspaces are materialized from a bare-clone cache
//! (`bench/.cache/repos/<slug>-<hash>`) at a pinned SHA into a per-run
//! tempdir. The materialized tree is re-initialized as a fresh single-commit
//! git repo so that (a) `git diff` after the run shows exactly the harness's
//! changes and (b) the harness cannot mine the upstream history for answers
//! (e.g. the fixing commit of a `locate-bug` task).

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tempfile::TempDir;

/// Default per-task timeout (10 minutes), per the task.toml contract.
pub const DEFAULT_TIMEOUT_MS: u64 = 600_000;
/// Default judge score threshold for run success, per the task.toml contract.
pub const DEFAULT_JUDGE_THRESHOLD: f32 = 6.0;

/// What kind of work the task demands. `mutate` switches harness adapters
/// into their file-mutation mode (plan Task 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskKind {
    Explore,
    Locate,
    Mutate,
}

impl fmt::Display for TaskKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TaskKind::Explore => "explore",
            TaskKind::Locate => "locate",
            TaskKind::Mutate => "mutate",
        })
    }
}

/// `[workspace]` table: the pinned third-party repo a run executes in.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSpec {
    /// Clone URL (or local path — used by tests and local mirrors).
    pub repo_url: String,
    /// Full 40-hex commit SHA the workspace is materialized at.
    pub sha: String,
}

/// `[grading]` table: programmatic checks + judge threshold (consumed by
/// `grade.rs`, plan Task 5).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GradingSpec {
    /// Minimum judge score (0–10) for run success. Default 6.0.
    #[serde(default = "default_judge_threshold")]
    pub judge_threshold: f32,
    /// Regexes that must each match the extracted answer.
    #[serde(default)]
    pub answer_contains: Vec<String>,
    /// Command run in the post-run workspace; exit 0 = pass.
    #[serde(default)]
    pub check_command: Option<String>,
    /// Substrings that must appear in the workspace diff.
    #[serde(default)]
    pub diff_contains: Option<Vec<String>>,
    /// Path globs that must not appear among changed paths.
    #[serde(default)]
    pub diff_must_not_touch: Option<Vec<String>>,
}

impl Default for GradingSpec {
    fn default() -> Self {
        Self {
            judge_threshold: DEFAULT_JUDGE_THRESHOLD,
            answer_contains: Vec::new(),
            check_command: None,
            diff_contains: None,
            diff_must_not_touch: None,
        }
    }
}

fn default_judge_threshold() -> f32 {
    DEFAULT_JUDGE_THRESHOLD
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

/// Parsed + validated `task.toml`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskManifest {
    /// Task id; must match the task directory name.
    pub id: String,
    pub kind: TaskKind,
    /// Per-run timeout in milliseconds. Default 600 000 (10 min).
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    pub workspace: WorkspaceSpec,
    #[serde(default)]
    pub grading: GradingSpec,
}

impl TaskManifest {
    /// Parse and validate a `task.toml` document.
    pub fn parse(toml_str: &str) -> Result<Self> {
        let manifest: TaskManifest =
            toml::from_str(toml_str).context("parsing task.toml")?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Semantic validation beyond what serde enforces; every error names the
    /// offending field.
    fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            bail!("task.toml: `id` must be non-empty");
        }
        if self.timeout_ms == 0 {
            bail!("task.toml: `timeout_ms` must be greater than 0");
        }
        if self.workspace.repo_url.trim().is_empty() {
            bail!("task.toml: `workspace.repo_url` must be non-empty");
        }
        let sha = &self.workspace.sha;
        if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!(
                "task.toml: `workspace.sha` must be a full 40-char hex commit SHA, got {sha:?}"
            );
        }
        let threshold = self.grading.judge_threshold;
        if !(0.0..=10.0).contains(&threshold) {
            bail!(
                "task.toml: `grading.judge_threshold` must be within 0..=10, got {threshold}"
            );
        }
        for pattern in &self.grading.answer_contains {
            regex::Regex::new(pattern).with_context(|| {
                format!(
                    "task.toml: `grading.answer_contains` entry {pattern:?} is not a valid regex"
                )
            })?;
        }
        if let Some(globs) = &self.grading.diff_must_not_touch {
            for glob in globs {
                globset::Glob::new(glob).with_context(|| {
                    format!(
                        "task.toml: `grading.diff_must_not_touch` entry {glob:?} is not a valid glob"
                    )
                })?;
            }
        }
        Ok(())
    }
}

/// One fully loaded task: manifest + prompt + optional rubric.
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub manifest: TaskManifest,
    /// Task directory (`bench/tasks/<id>`).
    pub dir: PathBuf,
    /// Verbatim contents of `prompt.md` — identical across harnesses by
    /// construction (fairness protocol).
    pub prompt: String,
    /// Contents of `rubric.md` when present; presence means the task is
    /// judge-graded.
    pub rubric: Option<String>,
}

impl Task {
    pub fn has_rubric(&self) -> bool {
        self.rubric.is_some()
    }
}

/// Default task suite location: `bench/tasks/`.
pub fn default_tasks_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tasks")
}

/// Load a single task from its directory (`<dir>/task.toml` + `prompt.md` +
/// optional `rubric.md`).
pub fn load_task(dir: &Path) -> Result<Task> {
    let manifest_path = dir.join("task.toml");
    let raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest = TaskManifest::parse(&raw)
        .with_context(|| format!("in {}", manifest_path.display()))?;

    let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if manifest.id != dir_name {
        bail!(
            "task.toml: `id` {:?} does not match task directory name {:?} ({})",
            manifest.id,
            dir_name,
            dir.display()
        );
    }

    let prompt_path = dir.join("prompt.md");
    let prompt = fs::read_to_string(&prompt_path)
        .with_context(|| format!("reading {}", prompt_path.display()))?;
    if prompt.trim().is_empty() {
        bail!("{}: prompt.md must be non-empty", dir.display());
    }

    let rubric_path = dir.join("rubric.md");
    let rubric = if rubric_path.is_file() {
        Some(
            fs::read_to_string(&rubric_path)
                .with_context(|| format!("reading {}", rubric_path.display()))?,
        )
    } else {
        None
    };

    Ok(Task { manifest, dir: dir.to_path_buf(), prompt, rubric })
}

/// Load every task under `tasks_dir` (one subdirectory per task), sorted by
/// id. Plain files and dot-directories are skipped; a non-hidden subdirectory
/// without a `task.toml` is an error (it is either a typo or a stray dir).
pub fn load_tasks(tasks_dir: &Path) -> Result<Vec<Task>> {
    let entries = fs::read_dir(tasks_dir)
        .with_context(|| format!("reading tasks dir {}", tasks_dir.display()))?;
    let mut tasks = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("reading entry in {}", tasks_dir.display()))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        if !path.join("task.toml").is_file() {
            bail!("{} has no task.toml", path.display());
        }
        tasks.push(load_task(&path)?);
    }
    tasks.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    Ok(tasks)
}

// ---------------------------------------------------------------------------
// Workspace materialization
// ---------------------------------------------------------------------------

/// Bare-clone cache keyed by repo URL. One clone per repo at first use;
/// `git fetch` only when a requested SHA is missing locally.
pub struct RepoCache {
    root: PathBuf,
}

impl RepoCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The shared on-disk cache, `bench/.cache/repos` (gitignored).
    pub fn shared() -> Self {
        Self::new(Path::new(env!("CARGO_MANIFEST_DIR")).join(".cache").join("repos"))
    }

    /// Materialize a fresh, independent workspace at the pinned SHA.
    ///
    /// The returned workspace is a single-commit git repo containing exactly
    /// the tree at `spec.sha`; `Workspace::diff` reports any post-run change
    /// against that baseline.
    pub fn materialize(&self, spec: &WorkspaceSpec) -> Result<Workspace> {
        let cache = self.ensure_repo(&spec.repo_url, &spec.sha)?;
        let tmp = TempDir::with_prefix("gantry-bench-ws-")
            .context("creating workspace tempdir")?;
        let root = tmp.path();
        let root_str = path_str(root)?;

        // Export the pinned tree: local clone (hardlinked objects, cheap),
        // detach at the SHA, then strip and rewrite history to one commit.
        git(&["clone", "--quiet", "--no-checkout", path_str(&cache)?, root_str], None)
            .with_context(|| format!("cloning cache for {}", spec.repo_url))?;
        git(&["checkout", "--quiet", "--detach", &spec.sha], Some(root))
            .with_context(|| format!("checking out {}", spec.sha))?;
        fs::remove_dir_all(root.join(".git")).context("stripping upstream history")?;
        git(&["init", "--quiet", "-b", "main"], Some(root))?;
        git(&["add", "-A"], Some(root))?;
        git(
            &[
                "-c",
                "user.name=gantry-bench",
                "-c",
                "user.email=bench@invalid",
                "commit",
                "--quiet",
                "--no-gpg-sign",
                "--allow-empty",
                "-m",
                &format!("bench baseline {}", spec.sha),
            ],
            Some(root),
        )
        .context("committing workspace baseline")?;
        Ok(Workspace { dir: tmp })
    }

    /// Ensure the bare clone for `url` exists and contains `sha`; fetch only
    /// when the SHA is missing. Returns the cache directory.
    fn ensure_repo(&self, url: &str, sha: &str) -> Result<PathBuf> {
        let dir = self.root.join(cache_dir_name(url));
        if !dir.exists() {
            fs::create_dir_all(&self.root)
                .with_context(|| format!("creating cache root {}", self.root.display()))?;
            // Clone into a staging dir, then rename into place so a
            // concurrent materialization of the same repo cannot observe a
            // half-written cache.
            let staging = tempfile::Builder::new()
                .prefix(".clone-")
                .tempdir_in(&self.root)
                .context("creating clone staging dir")?;
            let staged = staging.path().join("repo.git");
            git(&["clone", "--quiet", "--bare", url, path_str(&staged)?], None)
                .with_context(|| format!("cloning {url}"))?;
            match fs::rename(&staged, &dir) {
                Ok(()) => {}
                // Lost a race with a concurrent clone of the same URL; the
                // winner's cache is equivalent.
                Err(_) if dir.exists() => {}
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("moving clone of {url} into {}", dir.display())
                    })
                }
            }
        }
        if !has_commit(&dir, sha) {
            git(
                &[
                    "fetch",
                    "--quiet",
                    "origin",
                    "+refs/heads/*:refs/heads/*",
                    "+refs/tags/*:refs/tags/*",
                ],
                Some(&dir),
            )
            .with_context(|| format!("fetching {url} to find commit {sha}"))?;
            if !has_commit(&dir, sha) {
                bail!("commit {sha} not found in {url} (after fetch)");
            }
        }
        Ok(dir)
    }
}

/// A disposable per-run workspace; the directory is deleted on drop.
#[derive(Debug)]
pub struct Workspace {
    dir: TempDir,
}

impl Workspace {
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Unified diff of everything that changed since materialization,
    /// including new files (deliberately excluding files matched by the
    /// repo's own `.gitignore`, so build artifacts don't drown the diff).
    /// Idempotent: stages the current state, then diffs against the baseline.
    pub fn diff(&self) -> Result<String> {
        git(&["add", "-A"], Some(self.path()))?;
        git(&["diff", "--cached"], Some(self.path()))
    }
}

/// `<slug>-<fnv1a64(url)>`: stable, filesystem-safe, debuggable.
fn cache_dir_name(url: &str) -> String {
    let tail = url
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit(['/', ':'])
        .next()
        .unwrap_or("repo");
    let mut slug: String = tail
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' })
        .take(40)
        .collect();
    if slug.is_empty() {
        slug.push_str("repo");
    }
    format!("{slug}-{:016x}", fnv1a64(url))
}

/// FNV-1a 64-bit: deterministic across runs and Rust versions (unlike
/// `DefaultHasher`), with no extra dependency.
fn fnv1a64(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in s.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn has_commit(bare_dir: &Path, sha: &str) -> bool {
    git_cmd(&["cat-file", "-e", &format!("{sha}^{{commit}}")], Some(bare_dir))
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Hermetic git invocation: user/system config, hooks, and credential
/// prompts are all isolated so behavior is deterministic and networkless
/// operations can never hang on a terminal prompt.
fn git_cmd(args: &[&str], cwd: Option<&Path>) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd
}

fn git(args: &[&str], cwd: Option<&Path>) -> Result<String> {
    let out = git_cmd(args, cwd)
        .output()
        .with_context(|| format!("spawning git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "git {} failed ({}): {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("non-UTF-8 path {}", path.display()))
}
