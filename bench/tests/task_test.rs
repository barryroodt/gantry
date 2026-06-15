//! Plan Task 3 tests: task.toml parsing/validation, the task-dir loader, and
//! workspace materialization. Keyless and networkless: every git operation
//! targets fixture repos constructed inside the test tempdir.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use gantry_bench::grade::DEFAULT_JUDGE_THRESHOLD;
use gantry_bench::task::{
    load_task, load_tasks, RepoCache, TaskKind, TaskManifest, WorkspaceSpec, DEFAULT_TIMEOUT_MS,
};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Hermetic git for fixture construction: user/system config isolated,
/// identity supplied via env, prompts disabled.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@invalid")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

const LIB_V1: &str = "pub fn alpha() -> u32 { 1 }\n";
const LIB_V2: &str = "pub fn alpha() -> u32 { 2 }\n";

/// Builds the fixture origin repo at `root/origin` with two commits:
/// commit 1: src/lib.rs (v1) + README.md + .gitignore (ignores build/)
/// commit 2: src/lib.rs (v2) + docs/notes.md
/// Returns (origin_path, sha1, sha2).
fn fixture_origin(root: &Path) -> (PathBuf, String, String) {
    let origin = root.join("origin");
    fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "--quiet", "-b", "main"]);

    write(&origin.join("src/lib.rs"), LIB_V1);
    write(&origin.join("README.md"), "fixture readme v1\n");
    write(&origin.join(".gitignore"), "build/\n");
    git(&origin, &["add", "-A"]);
    git(&origin, &["commit", "--quiet", "--no-gpg-sign", "-m", "c1"]);
    let sha1 = git(&origin, &["rev-parse", "HEAD"]).trim().to_owned();

    write(&origin.join("src/lib.rs"), LIB_V2);
    write(&origin.join("docs/notes.md"), "trace notes\n");
    git(&origin, &["add", "-A"]);
    git(&origin, &["commit", "--quiet", "--no-gpg-sign", "-m", "c2"]);
    let sha2 = git(&origin, &["rev-parse", "HEAD"]).trim().to_owned();

    (origin, sha1, sha2)
}

fn spec(origin: &Path, sha: &str) -> WorkspaceSpec {
    WorkspaceSpec {
        repo_url: origin.to_str().unwrap().to_owned(),
        sha: sha.to_owned(),
    }
}

fn fake_sha() -> String {
    "deadbeef".repeat(5)
}

fn minimal_manifest(id: &str) -> String {
    format!(
        r#"
id = "{id}"
kind = "explore"

[workspace]
repo_url = "https://example.com/fixture.git"
sha = "{sha}"
"#,
        sha = "a".repeat(40)
    )
}

fn parse_err(toml_str: &str) -> String {
    let err = TaskManifest::parse(toml_str).expect_err("manifest should be rejected");
    format!("{err:#}")
}

// ---------------------------------------------------------------------------
// manifest parsing
// ---------------------------------------------------------------------------

#[test]
fn parses_manifest_with_all_fields() {
    let sha = "0123456789abcdef0123456789abcdef01234567";
    let toml_str = format!(
        r#"
id = "fix-failing-test"
kind = "mutate"
timeout_ms = 120000

[workspace]
repo_url = "https://example.com/repo.git"
sha = "{sha}"

[grading]
judge_threshold = 7.5
answer_contains = ["src/[a-z]+\\.rs", "root cause"]
check_command = "cargo test -q"
diff_contains = ["fn alpha"]
diff_must_not_touch = ["tests/**"]
"#
    );
    let m = TaskManifest::parse(&toml_str).unwrap();
    assert_eq!(m.id, "fix-failing-test");
    assert_eq!(m.kind, TaskKind::Mutate);
    assert_eq!(m.timeout_ms, 120_000);
    assert_eq!(m.workspace.repo_url, "https://example.com/repo.git");
    assert_eq!(m.workspace.sha, sha);
    assert_eq!(m.grading.judge_threshold, 7.5);
    assert_eq!(
        m.grading.answer_contains,
        vec![r"src/[a-z]+\.rs", "root cause"]
    );
    assert_eq!(m.grading.check_command.as_deref(), Some("cargo test -q"));
    assert_eq!(m.grading.diff_contains, vec!["fn alpha".to_owned()]);
    assert_eq!(m.grading.diff_must_not_touch, vec!["tests/**".to_owned()]);
}

#[test]
fn minimal_manifest_gets_contract_defaults() {
    let m = TaskManifest::parse(&minimal_manifest("explore-architecture")).unwrap();
    assert_eq!(m.kind, TaskKind::Explore);
    assert_eq!(m.timeout_ms, DEFAULT_TIMEOUT_MS);
    assert_eq!(m.grading.judge_threshold, DEFAULT_JUDGE_THRESHOLD);
    assert!(m.grading.answer_contains.is_empty());
    assert_eq!(m.grading.check_command, None);
    assert!(m.grading.diff_contains.is_empty());
    assert!(m.grading.diff_must_not_touch.is_empty());
}

#[test]
fn missing_id_is_rejected_naming_the_field() {
    let toml_str = minimal_manifest("x").replace("id = \"x\"\n", "");
    assert!(parse_err(&toml_str).contains("id"));
}

#[test]
fn invalid_kind_is_rejected_listing_valid_variants() {
    let toml_str = minimal_manifest("t").replace("kind = \"explore\"", "kind = \"poke\"");
    let err = parse_err(&toml_str);
    assert!(
        err.contains("poke"),
        "error should quote the bad value: {err}"
    );
    assert!(
        err.contains("explore") && err.contains("locate") && err.contains("mutate"),
        "error should list valid kinds: {err}"
    );
}

#[test]
fn missing_workspace_sha_is_rejected_naming_the_field() {
    let toml_str = minimal_manifest("t")
        .lines()
        .filter(|l| !l.starts_with("sha = "))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(parse_err(&toml_str).contains("sha"));
}

#[test]
fn malformed_sha_is_rejected_naming_the_field() {
    let toml_str = minimal_manifest("t").replace(&"a".repeat(40), "abc123");
    let err = parse_err(&toml_str);
    assert!(err.contains("workspace.sha"), "{err}");
    assert!(err.contains("40-char"), "{err}");
}

#[test]
fn zero_timeout_is_rejected_naming_the_field() {
    let toml_str = format!("timeout_ms = 0\n{}", minimal_manifest("t"));
    assert!(parse_err(&toml_str).contains("timeout_ms"));
}

#[test]
fn empty_repo_url_is_rejected_naming_the_field() {
    let toml_str = minimal_manifest("t").replace(
        "repo_url = \"https://example.com/fixture.git\"",
        "repo_url = \"\"",
    );
    assert!(parse_err(&toml_str).contains("workspace.repo_url"));
}

#[test]
fn invalid_answer_regex_is_rejected_naming_the_field() {
    let toml_str = format!(
        "{}\n[grading]\nanswer_contains = [\"[\"]\n",
        minimal_manifest("t")
    );
    let err = parse_err(&toml_str);
    assert!(err.contains("answer_contains"), "{err}");
}

#[test]
fn invalid_diff_glob_is_rejected_naming_the_field() {
    let toml_str = format!(
        "{}\n[grading]\ndiff_must_not_touch = [\"tests/[\"]\n",
        minimal_manifest("t")
    );
    let err = parse_err(&toml_str);
    assert!(err.contains("diff_must_not_touch"), "{err}");
}

#[test]
fn out_of_range_judge_threshold_is_rejected_naming_the_field() {
    let toml_str = format!(
        "{}\n[grading]\njudge_threshold = 10.5\n",
        minimal_manifest("t")
    );
    assert!(parse_err(&toml_str).contains("judge_threshold"));
}

#[test]
fn unknown_field_is_rejected_naming_the_field() {
    let toml_str = format!("timout_ms = 5\n{}", minimal_manifest("t"));
    assert!(parse_err(&toml_str).contains("timout_ms"));
}

#[test]
fn unknown_grading_field_is_rejected_naming_the_field() {
    // deny_unknown_fields holds on the embedded [grading] table too.
    let toml_str = format!(
        "{}\n[grading]\njudge_treshold = 5.0\n",
        minimal_manifest("t")
    );
    assert!(parse_err(&toml_str).contains("judge_treshold"));
}

// ---------------------------------------------------------------------------
// loader
// ---------------------------------------------------------------------------

/// Creates `root/<id>/` with a valid manifest + prompt (+ optional rubric).
fn make_task_dir(root: &Path, id: &str, rubric: Option<&str>) {
    let dir = root.join(id);
    write(&dir.join("task.toml"), &minimal_manifest(id));
    write(&dir.join("prompt.md"), &format!("Prompt for {id}.\n"));
    if let Some(text) = rubric {
        write(&dir.join("rubric.md"), text);
    }
}

#[test]
fn loader_scans_task_dirs_sorted_with_optional_rubric() {
    let root = TempDir::new().unwrap();
    make_task_dir(root.path(), "beta-task", None);
    make_task_dir(root.path(), "alpha-task", Some("Score 0-10: anchors.\n"));
    // Noise the loader must skip: dot-dir and a plain file.
    fs::create_dir(root.path().join(".cache")).unwrap();
    write(&root.path().join("notes.txt"), "not a task\n");

    let tasks = load_tasks(root.path()).unwrap();
    assert_eq!(
        tasks
            .iter()
            .map(|t| t.manifest.id.as_str())
            .collect::<Vec<_>>(),
        ["alpha-task", "beta-task"],
        "sorted by id"
    );
    assert_eq!(tasks[0].rubric.as_deref(), Some("Score 0-10: anchors.\n"));
    assert!(tasks[0].has_rubric());
    assert_eq!(tasks[1].rubric, None);
    assert_eq!(tasks[1].prompt, "Prompt for beta-task.\n");
    assert_eq!(tasks[1].dir, root.path().join("beta-task"));
}

#[test]
fn loader_rejects_task_dir_without_manifest() {
    let root = TempDir::new().unwrap();
    make_task_dir(root.path(), "ok-task", None);
    fs::create_dir(root.path().join("stray-dir")).unwrap();
    let err = format!("{:#}", load_tasks(root.path()).unwrap_err());
    assert!(
        err.contains("task.toml") && err.contains("stray-dir"),
        "{err}"
    );
}

#[test]
fn load_task_rejects_id_directory_mismatch() {
    let root = TempDir::new().unwrap();
    let dir = root.path().join("gamma-task");
    write(&dir.join("task.toml"), &minimal_manifest("delta-task"));
    write(&dir.join("prompt.md"), "p\n");
    let err = format!("{:#}", load_task(&dir).unwrap_err());
    assert!(
        err.contains("delta-task") && err.contains("gamma-task"),
        "{err}"
    );
}

#[test]
fn load_task_requires_nonempty_prompt() {
    let root = TempDir::new().unwrap();
    let dir = root.path().join("no-prompt");
    write(&dir.join("task.toml"), &minimal_manifest("no-prompt"));
    let err = format!("{:#}", load_task(&dir).unwrap_err());
    assert!(err.contains("prompt.md"), "{err}");

    write(&dir.join("prompt.md"), "  \n");
    let err = format!("{:#}", load_task(&dir).unwrap_err());
    assert!(
        err.contains("prompt.md") && err.contains("non-empty"),
        "{err}"
    );
}

// ---------------------------------------------------------------------------
// workspace materialization
// ---------------------------------------------------------------------------

#[test]
fn materializes_pinned_sha_with_single_commit_history() {
    let root = TempDir::new().unwrap();
    let (origin, sha1, _sha2) = fixture_origin(root.path());
    let cache = RepoCache::new(root.path().join("cache"));

    let ws = cache.materialize(&spec(&origin, &sha1)).unwrap();
    assert_eq!(read(&ws.path().join("src/lib.rs")), LIB_V1);
    assert_eq!(read(&ws.path().join("README.md")), "fixture readme v1\n");
    assert!(
        !ws.path().join("docs/notes.md").exists(),
        "later commit must not leak in"
    );

    // Upstream history is stripped: exactly one baseline commit, so the
    // harness cannot mine `git log` for answers.
    let count = git(ws.path(), &["rev-list", "--count", "HEAD"]);
    assert_eq!(count.trim(), "1");

    assert_eq!(
        ws.diff().unwrap(),
        "",
        "fresh workspace must have an empty diff"
    );
}

#[test]
fn materializes_different_sha_from_same_cache() {
    let root = TempDir::new().unwrap();
    let (origin, sha1, sha2) = fixture_origin(root.path());
    let cache = RepoCache::new(root.path().join("cache"));

    let ws1 = cache.materialize(&spec(&origin, &sha1)).unwrap();
    let ws2 = cache.materialize(&spec(&origin, &sha2)).unwrap();
    assert_eq!(read(&ws1.path().join("src/lib.rs")), LIB_V1);
    assert_eq!(read(&ws2.path().join("src/lib.rs")), LIB_V2);
    assert_eq!(read(&ws2.path().join("docs/notes.md")), "trace notes\n");
}

#[test]
fn materializations_are_independent_and_diff_tracks_changes() {
    let root = TempDir::new().unwrap();
    let (origin, sha1, _) = fixture_origin(root.path());
    let cache = RepoCache::new(root.path().join("cache"));

    let ws1 = cache.materialize(&spec(&origin, &sha1)).unwrap();
    let ws2 = cache.materialize(&spec(&origin, &sha1)).unwrap();
    assert_ne!(ws1.path(), ws2.path());

    write(
        &ws1.path().join("README.md"),
        "fixture readme v1\nchanged by harness\n",
    );
    write(&ws1.path().join("src/new_file.txt"), "brand new\n");

    let diff1 = ws1.diff().unwrap();
    assert!(diff1.contains("+changed by harness"), "{diff1}");
    assert!(
        diff1.contains("src/new_file.txt"),
        "new untracked files must appear: {diff1}"
    );
    // diff() is idempotent.
    assert_eq!(ws1.diff().unwrap(), diff1);

    assert_eq!(
        ws2.diff().unwrap(),
        "",
        "sibling workspace must be untouched"
    );
    assert_eq!(read(&ws2.path().join("README.md")), "fixture readme v1\n");
    assert!(!ws2.path().join("src/new_file.txt").exists());
}

#[test]
fn diff_respects_workspace_gitignore() {
    let root = TempDir::new().unwrap();
    let (origin, sha1, _) = fixture_origin(root.path());
    let cache = RepoCache::new(root.path().join("cache"));

    let ws = cache.materialize(&spec(&origin, &sha1)).unwrap();
    write(&ws.path().join("build/junk.txt"), "artifact\n");
    write(&ws.path().join("src/extra.rs"), "pub fn extra() {}\n");

    let diff = ws.diff().unwrap();
    assert!(diff.contains("src/extra.rs"), "{diff}");
    assert!(
        !diff.contains("junk.txt"),
        "gitignored artifacts must stay out of the diff: {diff}"
    );
}

#[test]
fn cached_sha_needs_no_origin_contact() {
    let root = TempDir::new().unwrap();
    let (origin, sha1, _) = fixture_origin(root.path());
    let cache = RepoCache::new(root.path().join("cache"));

    // Populate the cache, then take the origin away entirely: a cached SHA
    // must materialize without any fetch (the networkless guarantee).
    cache.materialize(&spec(&origin, &sha1)).unwrap();
    let moved = root.path().join("origin-moved");
    fs::rename(&origin, &moved).unwrap();

    let ws = cache.materialize(&spec(&origin, &sha1)).unwrap();
    assert_eq!(read(&ws.path().join("src/lib.rs")), LIB_V1);

    // A SHA the cache does not have forces a fetch, which now fails loudly
    // and names the commit.
    let missing = fake_sha();
    let err = format!(
        "{:#}",
        cache.materialize(&spec(&origin, &missing)).unwrap_err()
    );
    assert!(err.contains(&missing), "{err}");
}

#[test]
fn fetches_only_when_sha_is_missing_from_cache() {
    let root = TempDir::new().unwrap();
    let (origin, sha1, _) = fixture_origin(root.path());
    let cache = RepoCache::new(root.path().join("cache"));
    cache.materialize(&spec(&origin, &sha1)).unwrap();

    // New upstream commit after the cache was populated.
    write(&origin.join("README.md"), "fixture readme v3\n");
    git(&origin, &["add", "-A"]);
    git(&origin, &["commit", "--quiet", "--no-gpg-sign", "-m", "c3"]);
    let sha3 = git(&origin, &["rev-parse", "HEAD"]).trim().to_owned();

    let ws = cache.materialize(&spec(&origin, &sha3)).unwrap();
    assert_eq!(read(&ws.path().join("README.md")), "fixture readme v3\n");
}

#[test]
fn unknown_sha_errors_after_fetch_naming_the_commit() {
    let root = TempDir::new().unwrap();
    let (origin, _, _) = fixture_origin(root.path());
    let cache = RepoCache::new(root.path().join("cache"));

    let missing = fake_sha();
    let err = format!(
        "{:#}",
        cache.materialize(&spec(&origin, &missing)).unwrap_err()
    );
    assert!(err.contains(&missing) && err.contains("not found"), "{err}");
}
