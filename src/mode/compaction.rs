//! Transcript compaction: fold old tool results into the [`RetrievalStore`] to
//! free context-window headroom while keeping them losslessly recoverable via
//! the `retrieve` tool.

use std::sync::Arc;

use crate::provider::ChatMessage;
use crate::tools::retrieval::{mint_handle, RetrievalStore};

/// Number of most-recent `ToolResults` turns to leave untouched.
pub(crate) const KEEP_RECENT_TURNS: usize = 3;

/// Minimum content size in bytes that triggers compaction; smaller results are
/// not worth eliding.
const MIN_COMPACT_BYTES: usize = 512;

/// Prefix that identifies an already-compacted stub; used to ensure idempotency.
const STUB_PREFIX: &str = "[gantry: tool result";

/// Compact old tool results in `messages` by replacing their content with a
/// retrieval stub and stashing the original text in `store`.
///
/// Only [`ChatMessage::ToolResults`] messages beyond the most-recent
/// `keep_recent_turns` are candidates; [`ChatMessage::User`] and
/// [`ChatMessage::Assistant`] are never touched.
///
/// Returns the number of individual [`crate::provider::ToolResult`] entries
/// that were elided.
pub(crate) fn compact_history(
    messages: &mut [ChatMessage],
    store: &RetrievalStore,
    keep_recent_turns: usize,
) -> u32 {
    // Collect the slice-indices of every ToolResults message.
    let tr_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter_map(|(i, msg)| {
            if matches!(msg, ChatMessage::ToolResults(_)) {
                Some(i)
            } else {
                None
            }
        })
        .collect();

    // Nothing to compact when we have keep_recent_turns or fewer ToolResults.
    if tr_indices.len() <= keep_recent_turns {
        return 0;
    }

    // Messages at or beyond this index are in the "keep" window.
    let cutoff = tr_indices[tr_indices.len() - keep_recent_turns];

    let mut count = 0u32;
    for msg in &mut messages[..cutoff] {
        if let ChatMessage::ToolResults(results) = msg {
            for r in results.iter_mut() {
                if r.content.starts_with(STUB_PREFIX) {
                    continue; // already a stub — idempotent
                }
                if r.content.len() <= MIN_COMPACT_BYTES {
                    continue; // too small to bother
                }
                let n_lines = r.content.lines().count();
                let handle = mint_handle("history", &r.content);
                store.insert(&handle, Arc::from(r.content.as_str()));
                r.content = format!(
                    "[gantry: tool result ({n_lines} lines) elided to free context; \
                     retrieve(handle=\"{handle}\", start=1) to recover in full]"
                );
                count += 1;
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ToolResult};
    use crate::tools::retrieval::RetrievalStore;

    fn big_content() -> String {
        "x".repeat(1000)
    }

    /// Build a transcript: one leading `User` message then `n_turns` of
    /// (`Assistant` + `ToolResults`) pairs, each with a single result using
    /// `content`.
    fn make_messages(n_turns: usize, content: &str) -> Vec<ChatMessage> {
        let mut msgs = vec![ChatMessage::User("task".into())];
        for _ in 0..n_turns {
            msgs.push(ChatMessage::Assistant {
                text: String::new(),
                tool_calls: vec![],
            });
            msgs.push(ChatMessage::ToolResults(vec![ToolResult {
                id: "i".into(),
                content: content.to_string(),
                is_error: false,
            }]));
        }
        msgs
    }

    /// Collect in-order references to the inner `Vec<ToolResult>` of every
    /// `ToolResults` message.
    fn tool_results(msgs: &[ChatMessage]) -> Vec<&Vec<ToolResult>> {
        msgs.iter()
            .filter_map(|m| {
                if let ChatMessage::ToolResults(r) = m {
                    Some(r)
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn older_turns_compacted_recent_preserved() {
        let store = RetrievalStore::new();
        let mut msgs = make_messages(6, &big_content());
        let count = compact_history(&mut msgs, &store, 3);

        // 6 turns, keep last 3 → first 3 elided
        assert_eq!(count, 3);

        let tr = tool_results(&msgs);
        for r in &tr[..3] {
            assert!(r[0].content.starts_with(STUB_PREFIX), "expected stub, got: {}", r[0].content);
        }
        for r in &tr[3..] {
            assert_eq!(r[0].content, big_content(), "recent turn must be untouched");
        }
    }

    #[test]
    fn user_and_assistant_untouched() {
        let store = RetrievalStore::new();
        let mut msgs = make_messages(6, &big_content());
        compact_history(&mut msgs, &store, 3);

        for msg in &msgs {
            match msg {
                ChatMessage::User(text) => assert_eq!(text, "task"),
                ChatMessage::Assistant { text, tool_calls } => {
                    assert!(text.is_empty());
                    assert!(tool_calls.is_empty());
                }
                ChatMessage::ToolResults(_) => {}
            }
        }
    }

    #[test]
    fn idempotent_second_call_returns_zero() {
        let store = RetrievalStore::new();
        let mut msgs = make_messages(6, &big_content());

        let first = compact_history(&mut msgs, &store, 3);
        assert_eq!(first, 3);

        let second = compact_history(&mut msgs, &store, 3);
        assert_eq!(second, 0, "second call must be a no-op");

        // Verify stubs are still stubs and were not double-processed
        let tr = tool_results(&msgs);
        for r in &tr[..3] {
            assert!(r[0].content.starts_with(STUB_PREFIX));
        }
    }

    #[test]
    fn short_content_not_stubbed() {
        let store = RetrievalStore::new();
        let mut msgs = make_messages(6, "short");
        let count = compact_history(&mut msgs, &store, 3);

        assert_eq!(count, 0, "content <= MIN_COMPACT_BYTES must not be elided");
        for r in tool_results(&msgs) {
            assert_eq!(r[0].content, "short");
        }
    }

    #[test]
    fn message_count_unchanged() {
        let store = RetrievalStore::new();
        let mut msgs = make_messages(6, &big_content());
        let original_len = msgs.len();
        compact_history(&mut msgs, &store, 3);
        assert_eq!(msgs.len(), original_len);
    }

    #[test]
    fn handle_stored_and_recoverable() {
        let store = RetrievalStore::new();
        let content = big_content();
        // 4 turns, keep 3 → exactly 1 elided
        let mut msgs = make_messages(4, &content);
        let count = compact_history(&mut msgs, &store, 3);
        assert_eq!(count, 1);

        // Find the stub
        let stub = msgs.iter().find_map(|m| {
            if let ChatMessage::ToolResults(r) = m {
                if r[0].content.starts_with(STUB_PREFIX) {
                    return Some(r[0].content.clone());
                }
            }
            None
        });
        let stub = stub.expect("one stub must exist");

        // Extract handle between `handle="` and the closing `"`
        let after_key = stub.split("handle=\"").nth(1).expect("handle= present in stub");
        let handle = after_key.split('"').next().expect("closing quote after handle");

        let recovered = store.get(handle).expect("original must be in store under handle");
        assert_eq!(recovered.as_ref(), content.as_str());
    }

    #[test]
    fn fewer_turns_than_keep_is_noop() {
        let store = RetrievalStore::new();
        let mut msgs = make_messages(2, &big_content());
        let count = compact_history(&mut msgs, &store, 3);
        assert_eq!(count, 0, "fewer turns than keep_recent_turns: nothing to compact");
    }

    #[test]
    fn exactly_keep_turns_is_noop() {
        let store = RetrievalStore::new();
        let mut msgs = make_messages(3, &big_content());
        let count = compact_history(&mut msgs, &store, 3);
        assert_eq!(count, 0, "exactly keep_recent_turns: nothing to compact");
    }
}
