//! Regression guard for the shipped `profiles/review/` profile (ADR-0004):
//! it must load and carry the review output contract + reviewer format, so
//! `--profile profiles/review` reproduces review behavior.

use gantry::cli::Mode;
use gantry::profile::load_profile;
use std::path::{Path, PathBuf};

fn review_profile_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("profiles/review")
}

#[test]
fn review_profile_loads_with_team_mode_and_tools() {
    let p = load_profile(&review_profile_dir()).expect("load review profile");
    assert_eq!(p.mode, Some(Mode::Team));
    for t in ["read_file", "git_diff", "skill_load"] {
        assert!(
            p.tools.iter().any(|x| x == t),
            "review profile missing base tool {t}"
        );
    }
    for t in ["spawn_subagent", "collect_outputs", "broadcast_summary"] {
        assert!(
            !p.tools.iter().any(|x| x == t),
            "orchestration name {t} must not be a profile tool (now harness-internal)"
        );
    }
    assert!(
        p.inject_skills.iter().any(|s| s == "code-review"),
        "review profile missing code-review skill"
    );
}

#[test]
fn review_profile_system_carries_output_contract() {
    let p = load_profile(&review_profile_dir()).expect("load");
    let sys = p.system_prompt.expect("review profile has a system prompt");
    assert!(
        sys.contains("OUTPUT CONTRACT"),
        "system lost the output contract"
    );
    assert!(
        sys.contains("```json"),
        "system lost the JSON fence requirement"
    );
}

#[test]
fn review_profile_subagent_carries_reviewer_format() {
    let p = load_profile(&review_profile_dir()).expect("load");
    let sub = p
        .subagent_system_prompt
        .expect("review profile has a subagent prompt");
    assert!(
        sub.contains("Reviewer Output Format"),
        "subagent lost the output format"
    );
    assert!(
        sub.contains("Verdict"),
        "subagent lost the verdict structure"
    );
}

#[test]
fn review_profile_carries_compose_and_unify_prompts() {
    let p = load_profile(&review_profile_dir()).expect("load");
    let compose = p
        .compose_prompt
        .expect("review profile has a compose prompt");
    assert!(
        compose.contains("plan"),
        "compose lost the plan instruction"
    );
    assert!(
        compose.contains("correctness"),
        "compose lost the team composition rules"
    );
    let unify = p.unify_prompt.expect("review profile has a unify prompt");
    assert!(unify.contains("verdict"), "unify lost the verdict field");
    assert!(unify.contains("findings"), "unify lost the findings field");
}

fn refine_profile_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("profiles/refine")
}

#[test]
fn refine_profile_composes_loop_mutation_isolation() {
    // The shipped iterate-and-mutate example must compose the SP2/SP3 knobs so
    // `--profile profiles/refine` is a working template (SP6 / ADR-0010).
    let p = load_profile(&refine_profile_dir()).expect("load refine profile");
    assert_eq!(p.mode, Some(Mode::Loop), "loop mode (SP3)");
    assert!(p.isolate, "isolation enabled (SP2)");
    assert_eq!(p.max_iterations, Some(5), "iteration cap (SP3)");
    for t in ["read_file", "write_file", "edit_file"] {
        assert!(
            p.tools.iter().any(|x| x == t),
            "{t} missing from refine tools: {:?}",
            p.tools
        );
    }
    assert!(p.system_prompt.is_some(), "refine profile has a persona");
}
