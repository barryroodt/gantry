//! Regression guard: the extracted review profile under `docs/profiles/review/`
//! reproduces the pre-SP1 hardcoded prompts, so wrily's `--system-file` /
//! `--subagent-system-file` migration preserves review behavior. The harness's
//! `{skill_prefix}\n{system_body}` composition is exercised in
//! `single_mode_test` / `team_mode_test`; this file pins the profile *content*.

const REVIEW_SINGLE_SYSTEM: &str = include_str!("../docs/profiles/review/single-system.md");
const REVIEW_TEAM_SYSTEM: &str = include_str!("../docs/profiles/review/team-system.md");
const REVIEW_SUBAGENT_SYSTEM: &str = include_str!("../docs/profiles/review/reviewer-system.md");

#[test]
fn review_single_profile_matches_pre_sp1_prompt() {
    assert_eq!(
        REVIEW_SINGLE_SYSTEM.trim_end(),
        "You are gantry running a code review task. Use the available tools."
    );
}

#[test]
fn review_team_profile_carries_persona_and_output_contract() {
    assert!(
        REVIEW_TEAM_SYSTEM.contains("You are the Gantry team lead in an automated CI code review"),
        "team profile lost its coordinator persona"
    );
    assert!(
        REVIEW_TEAM_SYSTEM.contains("OUTPUT CONTRACT"),
        "team profile lost its output contract"
    );
    assert!(
        REVIEW_TEAM_SYSTEM.contains("exactly ONE ```json fenced code block"),
        "team profile lost its JSON-fence requirement"
    );
}

#[test]
fn review_subagent_profile_matches_pre_sp1_template() {
    assert_eq!(
        REVIEW_SUBAGENT_SYSTEM.trim_end(),
        "You are a Gantry reviewer subagent. Read-only tools only. Emit markdown per output-format.md — no JSON fence."
    );
}
