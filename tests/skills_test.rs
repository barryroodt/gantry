use gantry::emitter::TestEmitterGuard;
use gantry::events::GantryEvent;
use gantry::skills::SkillLoader;
use tempfile::TempDir;

#[test]
fn valid_name_accepts_good_names() {
    assert!(SkillLoader::valid_name("good-name"));
    assert!(SkillLoader::valid_name("good_name"));
    assert!(SkillLoader::valid_name("Skill123"));
}

#[test]
fn valid_name_rejects_bad_names() {
    assert!(!SkillLoader::valid_name("../bad"));
    assert!(!SkillLoader::valid_name(""));
    assert!(!SkillLoader::valid_name(&"a".repeat(65)));
}

#[test]
fn resolve_finds_workdir_skill() {
    let dir = TempDir::new().unwrap();
    let skill_dir = dir.path().join(".claude/skills/caveman-review");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), "workdir override content").unwrap();

    let loader = SkillLoader::new(dir.path().to_path_buf());
    let skill = loader.resolve("caveman-review").expect("skill");
    assert_eq!(skill.content, "workdir override content");
}

#[test]
fn resolve_returns_none_without_workdir_copy() {
    // No bundled fallback: an auto-inject-set name absent from the workdir does
    // not resolve. Orchestrators must materialize skills under the workdir.
    let dir = TempDir::new().unwrap();
    let loader = SkillLoader::new(dir.path().to_path_buf());
    assert!(loader.resolve("caveman-review").is_none());
}

#[test]
fn resolve_returns_none_for_unknown_skill() {
    let dir = TempDir::new().unwrap();
    let loader = SkillLoader::new(dir.path().to_path_buf());
    assert!(loader.resolve("not-a-real-skill").is_none());
}

#[test]
fn resolve_rejects_path_traversal_name() {
    let dir = TempDir::new().unwrap();
    let loader = SkillLoader::new(dir.path().to_path_buf());
    assert!(loader.resolve("../etc").is_none());
}

#[test]
fn inject_core_skills_injects_workdir_skills() {
    let dir = TempDir::new().unwrap();
    let names: Vec<String> = vec![
        "caveman-review".into(),
        "agent-team-review".into(),
        "code-review".into(),
    ];
    for name in &names {
        let skill_dir = dir.path().join(".claude/skills").join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), format!("{name} body")).unwrap();
    }

    let loader = SkillLoader::new(dir.path().to_path_buf());
    let guard = TestEmitterGuard::install();
    let prefix = loader.inject_core_skills(&names);

    for name in &names {
        assert!(
            prefix.contains(&format!("<skill name=\"{name}\">")),
            "missing wrapper for {name}"
        );
        assert!(
            prefix.contains(&format!("{name} body")),
            "missing body for {name}"
        );
    }
    assert!(prefix.contains("</skill>"));

    let events = guard.drain_events();
    assert_eq!(events.len(), names.len());
    for event in events {
        match event {
            GantryEvent::SkillLoaded { .. } => {}
            other => panic!("unexpected event: {other:?}"),
        }
    }
}

#[test]
fn inject_core_skills_skips_absent_skills() {
    // No workdir copies for these names -> empty prefix and no skill_loaded
    // events (each name is warned + skipped).
    let dir = TempDir::new().unwrap();
    let loader = SkillLoader::new(dir.path().to_path_buf());
    let guard = TestEmitterGuard::install();
    let names = vec!["absent-skill".to_string(), "also-missing".to_string()];
    let prefix = loader.inject_core_skills(&names);

    assert!(prefix.is_empty());
    assert!(guard.drain_events().is_empty());
}
