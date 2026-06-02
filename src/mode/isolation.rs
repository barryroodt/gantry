//! Optional copy-on-write workspace isolation (`--isolate`, ADR-0007 / SP2).
//!
//! Runs the agent against a COW shadow of the workdir via `pi-iso`: mutations
//! land in an overlay, the original workdir is untouched, and a terminal
//! `changes` event lists what changed. Fail-closed — if no isolation backend is
//! host-available the run errors instead of silently mutating the real workdir.

use std::path::{Path, PathBuf};

use pi_iso::{ChangeKind, IsolationBackend};

use super::{dispatch, ModeRunOutcome};
use crate::cli::Validated;
use crate::events::{now_ms, ErrorKind, ExitCode, FileChangeRec, GantryEvent};
use crate::meter::MeterSnapshot;

/// Upper bound on files listed in the terminal `changes` event.
const MAX_CHANGES: usize = 200;

/// Run the mode against a COW overlay of `v.workdir`. The caller guarantees
/// `v.isolate` is set.
pub async fn run_isolated(mut v: Validated) -> ModeRunOutcome {
    let lower = v.workdir.clone();
    let merged = overlay_path(&lower);

    // Try host-available backends in fallback order; fail-closed if none start.
    let resolution = pi_iso::resolve(None);
    let backend = match start_first_available(&resolution, &lower, &merged).await {
        Ok(backend) => backend,
        Err(reason) => return config_error(&format!("--isolate: no usable backend: {reason}")),
    };

    // Teardown (stop = remove the clone) must run on every exit path, including a
    // panic unwind — hence a Drop guard in addition to the explicit teardown.
    let mut guard = OverlayGuard {
        backend,
        merged: merged.clone(),
        active: true,
    };

    v.workdir = merged.clone();
    let outcome = dispatch(v).await;

    // Capture and emit the change set before tearing the overlay down.
    match backend.diff(&lower, &merged).await {
        Ok(diff) => emit_changes(diff.files),
        Err(err) => {
            let _ = GantryEvent::Error {
                ts: now_ms(),
                kind: ErrorKind::Internal,
                message: format!("--isolate: diff capture failed: {err}"),
            }
            .emit();
        }
    }
    guard.teardown();
    outcome
}

/// Place the overlay as a sibling of the workdir so it lands on the same volume
/// (APFS `clonefile` is intra-volume); cross-device falls back to `Rcopy`.
fn overlay_path(lower: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = format!(".gantry-iso-{}-{nanos}", std::process::id());
    lower.parent().unwrap_or(lower).join(name)
}

/// Start the first host-available backend that accepts this `lower`/`merged`
/// pair. `start` is a blocking syscall, so it runs on a blocking thread.
async fn start_first_available(
    resolution: &pi_iso::Resolution,
    lower: &Path,
    merged: &Path,
) -> Result<&'static dyn IsolationBackend, String> {
    let mut last = String::from("no host-available candidates");
    for kind in &resolution.candidates {
        let backend = pi_iso::backend(*kind);
        let (l, m) = (lower.to_path_buf(), merged.to_path_buf());
        match tokio::task::spawn_blocking(move || backend.start(&l, &m)).await {
            Ok(Ok(())) => return Ok(backend),
            Ok(Err(err)) => last = format!("{kind}: {err}"),
            Err(join) => last = format!("{kind}: join error: {join}"),
        }
    }
    Err(last)
}

/// Map pi-iso file changes to event records, capped at [`MAX_CHANGES`].
fn map_changes(files: Vec<pi_iso::FileChange>) -> Vec<FileChangeRec> {
    files
        .into_iter()
        .take(MAX_CHANGES)
        .map(|f| FileChangeRec {
            path: f.path.to_string_lossy().into_owned(),
            kind: match f.op {
                ChangeKind::Added => "added",
                ChangeKind::Modified => "modified",
                ChangeKind::Removed => "removed",
            }
            .to_string(),
        })
        .collect()
}

/// Emit the terminal `changes` event.
fn emit_changes(files: Vec<pi_iso::FileChange>) {
    let _ = GantryEvent::Changes {
        ts: now_ms(),
        files: map_changes(files),
    }
    .emit();
}

/// Emit a `config` error and return the corresponding terminal outcome.
fn config_error(message: &str) -> ModeRunOutcome {
    let _ = GantryEvent::Error {
        ts: now_ms(),
        kind: ErrorKind::Config,
        message: message.to_string(),
    }
    .emit();
    ModeRunOutcome {
        exit: ExitCode::Config,
        meter: MeterSnapshot::default(),
    }
}

/// Owns the live overlay; removes it on `teardown()` or on drop (panic unwind).
struct OverlayGuard {
    backend: &'static dyn IsolationBackend,
    merged: PathBuf,
    active: bool,
}

impl OverlayGuard {
    fn teardown(&mut self) {
        if !std::mem::replace(&mut self.active, false) {
            return;
        }
        let _ = self.backend.stop(&self.merged);
    }
}

impl Drop for OverlayGuard {
    fn drop(&mut self) {
        self.teardown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn map_changes_maps_kinds() {
        let files = vec![
            pi_iso::FileChange {
                path: PathBuf::from("a.txt"),
                op: ChangeKind::Added,
                diff: None,
            },
            pi_iso::FileChange {
                path: PathBuf::from("b.txt"),
                op: ChangeKind::Removed,
                diff: None,
            },
        ];
        let recs = map_changes(files);
        assert_eq!(recs[0].kind, "added");
        assert_eq!(recs[1].kind, "removed");
    }

    #[tokio::test]
    async fn isolation_roundtrip_contains_mutation() {
        // Real pi-iso round-trip on this host: clone, mutate the overlay, diff,
        // and confirm the original workdir is untouched. Proves --isolate's core.
        let lower = TempDir::new().unwrap();
        std::fs::write(lower.path().join("keep.txt"), "original").unwrap();
        let merged = overlay_path(lower.path());

        let resolution = pi_iso::resolve(None);
        let backend = start_first_available(&resolution, lower.path(), &merged)
            .await
            .expect("an isolation backend should be available on this host");

        // Mutate inside the overlay only.
        std::fs::write(merged.join("new.txt"), "added").unwrap();

        let diff = backend.diff(lower.path(), &merged).await.unwrap();
        let recs = map_changes(diff.files);
        assert!(
            recs.iter().any(|r| r.path.contains("new.txt")),
            "overlay mutation should appear in the diff: {recs:?}"
        );

        // Original workdir untouched.
        assert!(!lower.path().join("new.txt").exists());
        assert_eq!(
            std::fs::read_to_string(lower.path().join("keep.txt")).unwrap(),
            "original"
        );

        // Teardown removes the overlay.
        backend.stop(&merged).unwrap();
        assert!(!merged.exists(), "overlay removed after stop");
    }
}
