use gantry::cli::Mode;
use gantry::profile::{load_profile, ProfileError};
use tempfile::TempDir;

#[test]
fn load_profile_reads_manifest_and_files() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("system.md"), "SYS").unwrap();
    std::fs::write(dir.path().join("subagent.md"), "SUB").unwrap();
    std::fs::write(dir.path().join("compose.md"), "COMPOSE").unwrap();
    std::fs::write(dir.path().join("unify.md"), "UNIFY").unwrap();
    std::fs::write(
        dir.path().join("profile.toml"),
        "mode = \"team\"\nsystem = \"system.md\"\nsubagent_system = \"subagent.md\"\ncompose = \"compose.md\"\nunify = \"unify.md\"\ntools = [\"read_file\"]\ninject_skills = [\"code-review\"]\n",
    )
    .unwrap();

    let p = load_profile(dir.path()).expect("load");
    assert_eq!(p.mode, Some(Mode::Team));
    assert_eq!(p.system_prompt.as_deref(), Some("SYS"));
    assert_eq!(p.subagent_system_prompt.as_deref(), Some("SUB"));
    assert_eq!(p.tools, ["read_file"]);
    assert_eq!(p.inject_skills, ["code-review"]);
    assert_eq!(p.compose_prompt.as_deref(), Some("COMPOSE"));
    assert_eq!(p.unify_prompt.as_deref(), Some("UNIFY"));
}

#[test]
fn load_profile_minimal_manifest_defaults() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("profile.toml"), "mode = \"single\"\n").unwrap();

    let p = load_profile(dir.path()).expect("load");
    assert_eq!(p.mode, Some(Mode::Single));
    assert!(p.system_prompt.is_none());
    assert!(p.subagent_system_prompt.is_none());
    assert!(p.compose_prompt.is_none());
    assert!(p.unify_prompt.is_none());
    assert!(p.tools.is_empty());
    assert!(p.inject_skills.is_empty());
}

#[test]
fn load_profile_missing_manifest_is_not_found() {
    let dir = TempDir::new().unwrap();
    let err = load_profile(dir.path()).unwrap_err();
    assert!(matches!(err, ProfileError::NotFound(_)), "got {err:?}");
}

#[test]
fn load_profile_bad_toml_is_parse_error() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("profile.toml"), "mode = = nope").unwrap();
    let err = load_profile(dir.path()).unwrap_err();
    assert!(matches!(err, ProfileError::Parse { .. }), "got {err:?}");
}

#[test]
fn load_profile_missing_referenced_file_is_file_missing() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("profile.toml"),
        "mode = \"single\"\nsystem = \"nope.md\"\n",
    )
    .unwrap();
    let err = load_profile(dir.path()).unwrap_err();
    assert!(matches!(err, ProfileError::FileMissing(_)), "got {err:?}");
}
