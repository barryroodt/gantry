//! Lossless retrieval of elided tool output stored by [`compress`].
//!
//! Callers can request:
//! - **No args** → the elided middle (lines `HEAD_LINES+1 ..= len-TAIL_LINES`).
//! - **`start`/`end`** → a 1-based inclusive slice.
//! - **`pattern`** → lines matching the regex plus ±3-line context windows.

use super::{ToolError, ToolOutput};
use crate::tools::retrieval::RetrievalStore;

pub const RETRIEVE: &str = "retrieve";

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct RetrieveArgs {
    pub handle: String,
    #[serde(default)]
    pub start: Option<usize>,
    #[serde(default)]
    pub end: Option<usize>,
    #[serde(default)]
    pub pattern: Option<String>,
}

pub fn retrieve(store: &RetrievalStore, args: RetrieveArgs) -> Result<ToolOutput, ToolError> {
    let original = store.get(&args.handle).ok_or_else(|| {
        ToolError::InvalidInput(format!("unknown retrieval handle '{}'", args.handle))
    })?;

    let lines: Vec<&str> = original.lines().collect();
    let len = lines.len();

    if len == 0 {
        return Ok(crate::tools::truncate::truncated_output(String::new()));
    }

    // Build the sorted, deduplicated set of 1-based line indices to emit.
    let selected: Vec<usize> = if let Some(p) = &args.pattern {
        let re = regex::Regex::new(p)
            .map_err(|e| ToolError::InvalidInput(format!("invalid pattern: {e}")))?;

        // Candidate range for matching (1-based, clamped to [1, len]).
        let cand_start = args.start.unwrap_or(1).max(1).min(len);
        let cand_end = args.end.unwrap_or(len).max(1).min(len);

        let mut set = std::collections::BTreeSet::new();
        for i in cand_start..=cand_end {
            if re.is_match(lines[i - 1]) {
                let lo = i.saturating_sub(3).max(1);
                let hi = (i + 3).min(len);
                for j in lo..=hi {
                    set.insert(j);
                }
            }
        }
        set.into_iter().collect()
    } else if args.start.is_some() || args.end.is_some() {
        // Slice mode: 1-based inclusive range, clamped to [1, len]. An inverted
        // range (start > end) collapses to empty and is reported by the
        // post-selection empty guard below.
        let s = args.start.unwrap_or(1).max(1).min(len);
        let e = args.end.unwrap_or(len).max(1).min(len);
        (s..=e).collect()
    } else {
        // No args: return the elided middle that compress omitted.
        use crate::tools::compress::{HEAD_LINES, TAIL_LINES};
        if len <= HEAD_LINES + TAIL_LINES {
            return Ok(crate::tools::truncate::truncated_output(
                original.to_string(),
            ));
        }
        let middle_start = HEAD_LINES + 1;
        let middle_end = len - TAIL_LINES;
        (middle_start..=middle_end).collect()
    };

    if selected.is_empty() {
        let note = match &args.pattern {
            Some(p) => format!("[gantry: no lines matched pattern '{p}']"),
            None => "[gantry: empty selection]".to_string(),
        };
        return Ok(crate::tools::truncate::truncated_output(note));
    }

    let joined = selected
        .iter()
        .map(|&i| lines[i - 1])
        .collect::<Vec<_>>()
        .join("\n");

    Ok(crate::tools::truncate::truncated_output(joined))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::retrieval::RetrievalStore;
    use std::sync::Arc;

    fn make_store_600() -> (RetrievalStore, String) {
        let store = RetrievalStore::new();
        let content: String = (1..=600)
            .map(|i| format!("line{}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let handle = "shell/abc".to_string();
        store.insert(&handle, Arc::from(content.as_str()));
        (store, handle)
    }

    #[test]
    fn no_args_returns_elided_middle() {
        let (store, handle) = make_store_600();
        let args = RetrieveArgs {
            handle,
            start: None,
            end: None,
            pattern: None,
        };
        let output = retrieve(&store, args).unwrap();
        let content = &output.content;
        // HEAD_LINES=400, TAIL_LINES=100 → elided middle is lines 401..=500
        assert_eq!(content.lines().next().unwrap(), "line401");
        assert_eq!(content.lines().last().unwrap(), "line500");
    }

    #[test]
    fn start_end_range() {
        let (store, handle) = make_store_600();
        let args = RetrieveArgs {
            handle,
            start: Some(10),
            end: Some(12),
            pattern: None,
        };
        let output = retrieve(&store, args).unwrap();
        assert_eq!(output.content, "line10\nline11\nline12");
    }

    #[test]
    fn pattern_match_with_context() {
        let (store, handle) = make_store_600();
        let args = RetrieveArgs {
            handle,
            start: None,
            end: None,
            pattern: Some("^line42$".to_string()),
        };
        let output = retrieve(&store, args).unwrap();
        let content = &output.content;
        // Should include line42 and ±3 context: lines 39..=45
        for i in 39..=45 {
            assert!(
                content.contains(&format!("line{}", i)),
                "missing line{i} in output: {content}"
            );
        }
    }

    #[test]
    fn unknown_handle_returns_err() {
        let store = RetrievalStore::new();
        let args = RetrieveArgs {
            handle: "shell/deadbeef".to_string(),
            start: None,
            end: None,
            pattern: None,
        };
        let err = retrieve(&store, args).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("shell/deadbeef"), "msg: {msg}");
    }

    #[test]
    fn invalid_regex_returns_err() {
        let (store, handle) = make_store_600();
        let args = RetrieveArgs {
            handle,
            start: None,
            end: None,
            pattern: Some("[".to_string()),
        };
        let err = retrieve(&store, args).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid pattern"), "msg: {msg}");
    }

    #[test]
    fn out_of_range_end_clamped() {
        let (store, handle) = make_store_600();
        let args = RetrieveArgs {
            handle,
            start: Some(595),
            end: Some(99999),
            pattern: None,
        };
        let output = retrieve(&store, args).unwrap();
        let content = &output.content;
        assert_eq!(content.lines().next().unwrap(), "line595");
        assert_eq!(content.lines().last().unwrap(), "line600");
    }

    #[test]
    fn pattern_no_match_returns_note() {
        let (store, handle) = make_store_600();
        let args = RetrieveArgs {
            handle,
            start: None,
            end: None,
            pattern: Some("nonexistent_token".to_string()),
        };
        let output = retrieve(&store, args).unwrap();
        assert!(
            output.content.contains("no lines matched pattern"),
            "expected no-match note, got: {}",
            output.content
        );
    }
}
