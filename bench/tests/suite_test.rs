//! Plan Task 8: loader smoke over the real shipped task suite in
//! `bench/tasks/`. Keyless and networkless — only parses the committed
//! manifests, prompts, and rubrics; never clones a workspace. Synthetic
//! loader/materialization coverage lives in `task_test.rs` (plan Task 3).

use gantry_bench::task::{default_tasks_dir, load_tasks, Task, TaskKind};

fn suite() -> Vec<Task> {
    load_tasks(&default_tasks_dir()).expect("shipped task suite must load")
}

fn task<'a>(tasks: &'a [Task], id: &str) -> &'a Task {
    tasks
        .iter()
        .find(|t| t.manifest.id == id)
        .unwrap_or_else(|| panic!("task {id:?} missing from suite"))
}

#[test]
fn suite_loads_all_six_tasks_sorted_by_id() {
    let tasks = suite();
    let ids: Vec<&str> = tasks.iter().map(|t| t.manifest.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "cross-file-trace",
            "explore-architecture",
            "fix-failing-test",
            "locate-bug",
            "needle-haystack",
            "targeted-edit",
        ]
    );
}

#[test]
fn kinds_match_the_spec_table() {
    let tasks = suite();
    let kind = |id| task(&tasks, id).manifest.kind;
    assert_eq!(kind("explore-architecture"), TaskKind::Explore);
    assert_eq!(kind("needle-haystack"), TaskKind::Explore);
    assert_eq!(kind("locate-bug"), TaskKind::Locate);
    assert_eq!(kind("cross-file-trace"), TaskKind::Locate);
    assert_eq!(kind("targeted-edit"), TaskKind::Mutate);
    assert_eq!(kind("fix-failing-test"), TaskKind::Mutate);
}

#[test]
fn judge_rubrics_present_exactly_where_required() {
    let tasks = suite();
    for (id, rubric) in [
        ("explore-architecture", true),
        ("locate-bug", true),
        ("cross-file-trace", true),
        ("needle-haystack", false),
        ("targeted-edit", false),
        ("fix-failing-test", false),
    ] {
        assert_eq!(
            task(&tasks, id).has_rubric(),
            rubric,
            "rubric presence mismatch for {id}"
        );
    }
}

#[test]
fn programmatic_grading_specs_are_wired() {
    let tasks = suite();

    // Mutate tasks grade via a check command and protect their test/manifest
    // surface from edits.
    for id in ["targeted-edit", "fix-failing-test"] {
        let grading = &task(&tasks, id).manifest.grading;
        assert!(grading.check_command.is_some(), "{id} needs check_command");
        let globs = &grading.diff_must_not_touch;
        assert!(!globs.is_empty(), "{id} needs diff_must_not_touch");
        assert!(
            globs.iter().any(|g| g == "tests/**"),
            "{id} must shield tests/ from harness edits"
        );
    }

    // Answer-checked tasks carry at least one verified pattern.
    for id in ["locate-bug", "cross-file-trace", "needle-haystack"] {
        assert!(
            !task(&tasks, id).manifest.grading.answer_contains.is_empty(),
            "{id} needs answer_contains patterns"
        );
    }
}

#[test]
fn workspaces_are_pinned_github_checkouts() {
    for t in suite() {
        let ws = &t.manifest.workspace;
        assert!(
            ws.repo_url.starts_with("https://github.com/"),
            "{}: workspace must be a public https clone URL, got {}",
            t.manifest.id,
            ws.repo_url
        );
        // Full-SHA shape is already enforced by manifest validation; this
        // guards against someone swapping in a branch name via a refactor
        // of that validation.
        assert_eq!(ws.sha.len(), 40, "{}: sha must stay pinned", t.manifest.id);
    }
}

#[test]
fn prompts_are_harness_neutral() {
    // Fairness protocol: prompts are verbatim-identical across harnesses and
    // must not name any harness or nudge toward harness-specific vocabulary.
    for t in suite() {
        let prompt = t.prompt.to_lowercase();
        for banned in ["gantry", "claude", "oh-my-pi", "omp "] {
            assert!(
                !prompt.contains(banned),
                "prompt for {} mentions {banned:?}",
                t.manifest.id
            );
        }
    }
}
