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
    for t in [
        "read_file",
        "spawn_subagent",
        "collect_outputs",
        "broadcast_summary",
    ] {
        assert!(
            p.tools.iter().any(|x| x == t),
            "review profile missing tool {t}"
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
    assert!(
        sys.contains("spawn_subagent"),
        "system lost orchestration guidance"
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
