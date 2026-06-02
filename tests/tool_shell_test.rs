use gantry::tools::shell::{shell, ShellArgs};
use gantry::tools::ToolError;
use tempfile::TempDir;

fn allow() -> Vec<String> {
    ["git", "cat", "ls", "echo"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[tokio::test]
async fn runs_allowlisted_command() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello world\n").unwrap();
    let out = shell(
        dir.path(),
        ShellArgs {
            command: "cat a.txt".into(),
        },
        &allow(),
    )
    .await
    .unwrap();
    assert!(
        out.content.contains("hello world"),
        "output: {}",
        out.content
    );
}

#[tokio::test]
async fn supports_pipelines() {
    let dir = TempDir::new().unwrap();
    let out = shell(
        dir.path(),
        ShellArgs {
            command: "echo hi | cat".into(),
        },
        &allow(),
    )
    .await
    .unwrap();
    assert!(out.content.contains("hi"), "pipe output: {}", out.content);
}

#[tokio::test]
async fn denies_non_allowlisted_program() {
    let dir = TempDir::new().unwrap();
    let err = shell(
        dir.path(),
        ShellArgs {
            command: "whoami".into(),
        },
        &allow(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, ToolError::InvalidInput(_)),
        "expected denial: {err:?}"
    );
}

#[tokio::test]
async fn denies_non_allowlisted_in_pipeline() {
    let dir = TempDir::new().unwrap();
    // A non-allowlisted program anywhere in the pipeline is rejected before running.
    let err = shell(
        dir.path(),
        ShellArgs {
            command: "echo x | whoami".into(),
        },
        &allow(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, ToolError::InvalidInput(_)),
        "expected denial: {err:?}"
    );
}
