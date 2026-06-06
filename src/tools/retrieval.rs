//! Content-addressed store for elided tool output, enabling lossless retrieval.
//!
//! [`RetrievalStore`] is a per-run, in-process store keyed by handles of the
//! form `"{tool}/{hex}"`. Handles are minted by [`mint_handle`] and are
//! deterministic for identical `(tool, content)` pairs so duplicate output
//! produces a single stored copy.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hasher};
use std::sync::{Arc, Mutex};

/// Per-run content-addressed store mapping handles to their full original text.
///
/// `Send + Sync` because `Mutex<HashMap<…>>` is both.  Lock-poison panics are
/// acceptable here — they indicate a bug elsewhere in the process.
pub struct RetrievalStore {
    inner: Mutex<HashMap<String, Arc<str>>>,
}

impl Default for RetrievalStore {
    fn default() -> Self {
        Self::new()
    }
}

impl RetrievalStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Store `original` under `handle`.  Idempotent: if `handle` is already
    /// present the existing value is kept and no allocation occurs.
    pub fn insert(&self, handle: &str, original: Arc<str>) {
        let mut guard = self.inner.lock().expect("RetrievalStore mutex poisoned");
        // Allocate the owned key only on a real insert — the already-present
        // path is a true no-op (content-addressed handles ⇒ identical content).
        if !guard.contains_key(handle) {
            guard.insert(handle.to_owned(), original);
        }
    }

    /// Return a clone of the `Arc` stored under `handle`, or `None` if absent.
    pub fn get(&self, handle: &str) -> Option<Arc<str>> {
        self.inner
            .lock()
            .expect("RetrievalStore mutex poisoned")
            .get(handle)
            .cloned()
    }

    /// Number of entries currently stored.  Only compiled for tests.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("RetrievalStore mutex poisoned")
            .len()
    }
}

/// Produce a deterministic handle of the form `"{tool}/{hex}"` where `{hex}` is
/// a zero-padded 12-character lowercase hex string derived from the low 48 bits
/// of a 64-bit hash of `content`.
///
/// Identical `(tool, content)` pairs always produce the same handle.  Different
/// contents are expected (with overwhelming probability) to produce different
/// handles.
pub fn mint_handle(tool: &str, content: &str) -> String {
    let mut hasher = DefaultHasher::new();
    hasher.write(content.as_bytes());
    let hash = hasher.finish();
    let low48 = hash & 0xFFFF_FFFF_FFFF;
    format!("{tool}/{low48:012x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_then_get_returns_stored_content() {
        let store = RetrievalStore::new();
        let content: Arc<str> = Arc::from("hello world");
        store.insert("shell/aabbccddeeff", Arc::clone(&content));
        let got = store.get("shell/aabbccddeeff").expect("should be present");
        assert_eq!(&*got, "hello world");
    }

    #[test]
    fn insert_same_handle_twice_keeps_one_entry() {
        let store = RetrievalStore::new();
        let handle = mint_handle("shell", "some output");
        let first: Arc<str> = Arc::from("some output");
        let second: Arc<str> = Arc::from("some output");
        store.insert(&handle, Arc::clone(&first));
        store.insert(&handle, Arc::clone(&second));
        // Idempotent: still exactly one entry.
        assert_eq!(store.len(), 1);
        // Value is the original insert (or any — they're equal content).
        let got = store.get(&handle).expect("should be present");
        assert_eq!(&*got, "some output");
    }

    #[test]
    fn get_absent_handle_returns_none() {
        let store = RetrievalStore::new();
        assert!(store.get("shell/000000000000").is_none());
    }

    #[test]
    fn mint_handle_is_deterministic_and_well_formed() {
        let tool = "read_file";
        let content = "line one\nline two\n";
        let h1 = mint_handle(tool, content);
        let h2 = mint_handle(tool, content);
        assert_eq!(h1, h2, "must be deterministic");
        assert!(h1.starts_with("read_file/"), "must start with '<tool>/'");
        let hex_part = h1.trim_start_matches("read_file/");
        assert!(
            hex_part.len() <= 12,
            "hex segment must be ≤12 chars, got {}",
            hex_part.len()
        );
        // Must parse as valid hex.
        u64::from_str_radix(hex_part, 16).expect("must be valid hex");
    }

    #[test]
    fn distinct_contents_produce_different_handles() {
        let h1 = mint_handle("shell", "output A");
        let h2 = mint_handle("shell", "output B");
        assert_ne!(h1, h2);
    }
}
