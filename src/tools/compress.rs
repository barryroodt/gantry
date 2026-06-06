//! Output compression at the tool-result boundary (SP5). Structured, recoverable
//! compression that cuts tokens while preserving signal:
//!
//! - a **head+tail line cap** with a machine-readable recovery hint for ALL
//!   tools (retained lines stay byte-identical — a bounded, recoverable cut, not
//!   a heuristic drop);
//! - **consecutive-line dedup** for NOISY (high-volume, non-faithful) tools only.
//!
//! It never corrupts byte-faithful content (`read_file`/`git_diff`/`skill_load`
//! are never deduped), small outputs pass through with zero allocation, and it is
//! a total function (no panics).

use super::ToolOutput;
use std::borrow::Cow;
use std::sync::Arc;

/// Tools whose output is high-volume and not byte-faithful, so consecutive-line
/// dedup is safe and worthwhile. Faithful-content tools are intentionally absent.
pub const NOISY_TOOLS: &[&str] = &["shell"];

/// Lines kept from the start when capping.
pub(crate) const HEAD_LINES: usize = 400;
/// Lines kept from the end when capping (errors/summaries cluster at the end).
pub(crate) const TAIL_LINES: usize = 100;
/// Minimum run of identical consecutive lines before dedup collapses it.
const DEDUP_RUN: usize = 5;

/// A stored copy of the pre-cap line sequence and the handle under which it can
/// be retrieved. Only produced when the head+tail cap actually elided lines.
pub(crate) struct Stash {
    pub handle: String,
    /// The line sequence the cap sliced (post-dedup for noisy tools, raw lines
    /// otherwise), joined by `\n`. Retrieved slices are byte-identical to this —
    /// i.e. to the content the model was shown (`str::lines()` drops a trailing
    /// `\r`, so CRLF input is LF-normalized here and in what the model sees).
    pub original: Arc<str>,
}

/// Return value of [`compress`]: the (possibly compressed) output and an
/// optional stash for lossless retrieval of the elided middle section.
pub(crate) struct CompressOutcome {
    pub output: ToolOutput,
    /// `Some` iff a head+tail cap elided at least one line.
    pub stash: Option<Stash>,
}

/// Compress a tool's output: dedup (noisy tools only), then a recoverable
/// head+tail line cap. Returns the input unchanged (no allocation) when it is
/// already small and has no collapsible run.
pub(crate) fn compress(tool_name: &str, output: ToolOutput) -> CompressOutcome {
    let needs_dedup = NOISY_TOOLS.contains(&tool_name) && has_dup_run(&output.content);
    let needs_cap = output.content.lines().count() > HEAD_LINES + TAIL_LINES;
    if !needs_dedup && !needs_cap {
        return CompressOutcome { output, stash: None };
    }

    let trailing_nl = output.content.ends_with('\n');
    let lines: Vec<Cow<str>> = if needs_dedup {
        dedup_runs(&output.content)
    } else {
        output.content.lines().map(Cow::Borrowed).collect()
    };
    let capped = lines.len() > HEAD_LINES + TAIL_LINES;

    let (content, stash) = if capped {
        // `cap_input` is the exact sequence `render` slices — post-dedup for
        // noisy tools, raw lines otherwise.  Stored as the stash original so
        // retrieved slices are byte-identical.
        let cap_input: String = lines.join("\n");
        let handle = crate::tools::retrieval::mint_handle(tool_name, &cap_input);
        let content = render(&lines, output.bytes, true, trailing_nl, &handle);
        let original: Arc<str> = cap_input.into();
        (content, Some(Stash { handle, original }))
    } else {
        let content = render(&lines, output.bytes, false, trailing_nl, "");
        (content, None)
    };

    CompressOutcome {
        output: ToolOutput {
            content,
            bytes: output.bytes,
            truncated: output.truncated || capped,
        },
        stash,
    }
}

/// True if `content` contains a run of at least `DEDUP_RUN` identical consecutive
/// lines. Scan-only (no allocation).
fn has_dup_run(content: &str) -> bool {
    let mut prev: Option<&str> = None;
    let mut count = 1usize;
    for line in content.lines() {
        if prev == Some(line) {
            count += 1;
            if count >= DEDUP_RUN {
                return true;
            }
        } else {
            prev = Some(line);
            count = 1;
        }
    }
    false
}

/// Collapse runs of `>= DEDUP_RUN` identical consecutive lines to one instance
/// plus a `… (repeated K×)` marker; shorter runs are kept verbatim.
fn dedup_runs(content: &str) -> Vec<Cow<'_, str>> {
    let mut out: Vec<Cow<str>> = Vec::new();
    let mut iter = content.lines();
    let Some(mut current) = iter.next() else {
        return out;
    };
    let mut count = 1usize;
    for line in iter {
        if line == current {
            count += 1;
        } else {
            push_run(&mut out, current, count);
            current = line;
            count = 1;
        }
    }
    push_run(&mut out, current, count);
    out
}

fn push_run<'a>(out: &mut Vec<Cow<'a, str>>, line: &'a str, count: usize) {
    if count >= DEDUP_RUN {
        out.push(Cow::Borrowed(line));
        out.push(Cow::Owned(format!("… (repeated {count}×)")));
    } else {
        for _ in 0..count {
            out.push(Cow::Borrowed(line));
        }
    }
}

/// Render the (possibly deduped) lines back to a string, applying a head+tail cap
/// with a recovery hint when `capped`.  `handle` is only used in the capped branch.
fn render(
    lines: &[Cow<str>],
    raw_bytes: usize,
    capped: bool,
    trailing_nl: bool,
    handle: &str,
) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    if !capped {
        join_into(&mut s, lines);
        if trailing_nl {
            s.push('\n');
        }
        return s;
    }
    let total = lines.len();
    let omitted = total.saturating_sub(HEAD_LINES + TAIL_LINES);
    join_into(&mut s, &lines[..HEAD_LINES]);
    s.push('\n');
    let _ = write!(
        s,
        "[gantry: {omitted} lines omitted ({raw_bytes} bytes raw); retrieve(handle=\"{handle}\") for the elided middle — add start/end or pattern to slice]"
    );
    s.push('\n');
    join_into(&mut s, &lines[total - TAIL_LINES..]);
    if trailing_nl {
        s.push('\n');
    }
    s
}

fn join_into(s: &mut String, lines: &[Cow<str>]) {
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            s.push('\n');
        }
        s.push_str(line.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(content: &str) -> ToolOutput {
        ToolOutput {
            bytes: content.len(),
            truncated: false,
            content: content.to_string(),
        }
    }

    #[test]
    fn small_output_unchanged() {
        let co = compress("read_file", out("a\nb\nc"));
        assert_eq!(co.output.content, "a\nb\nc");
        assert!(!co.output.truncated);
        assert!(co.stash.is_none(), "no stash for small output");
    }

    #[test]
    fn caps_large_output_head_tail_with_hint() {
        let content = (1..=600)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let co = compress("git_diff", out(&content));
        assert!(co.output.truncated, "capped output is marked truncated");
        assert!(co.output.content.contains("line1"), "head kept");
        assert!(co.output.content.contains("line400"), "last head line kept");
        assert!(co.output.content.contains("line600"), "tail kept");
        assert!(co.output.content.contains("line501"), "first tail line kept");
        assert!(!co.output.content.contains("line450"), "middle omitted");
        assert!(
            co.output.content.contains("100 lines omitted"),
            "accurate omitted count: {}",
            co.output.content
        );
        assert!(co.output.content.contains("bytes raw"), "raw byte hint present");
        assert!(
            co.output.content.contains("retrieve(handle=\""),
            "handle embedded in hint"
        );
        assert!(co.stash.is_some(), "stash populated when capped");
    }

    #[test]
    fn retained_lines_byte_identical() {
        let content = (1..=600)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let co = compress("git_diff", out(&content));
        // The first retained lines are exactly the originals, in order.
        assert!(co.output.content.starts_with("line1\nline2\nline3\n"));
    }

    #[test]
    fn shell_dedups_repeated_run() {
        let content = format!("{}end", "x\n".repeat(10));
        let co = compress("shell", out(&content));
        assert_eq!(co.output.content, "x\n… (repeated 10×)\nend");
        assert!(co.stash.is_none(), "dedup only, not capped — no stash");
    }

    #[test]
    fn faithful_tool_not_deduped() {
        let content = format!("{}end", "x\n".repeat(10));
        let co = compress("read_file", out(&content));
        assert_eq!(
            co.output.content, content,
            "read_file content must be faithful"
        );
        assert!(co.stash.is_none());
    }

    #[test]
    fn dedup_below_threshold_untouched() {
        let content = format!("{}end", "y\n".repeat(3));
        let co = compress("shell", out(&content));
        assert_eq!(
            co.output.content, content,
            "runs shorter than DEDUP_RUN are kept"
        );
        assert!(co.stash.is_none());
    }

    /// Feed >500 lines; verify stash is populated and its fields are correct.
    #[test]
    fn capped_stash_fields_correct() {
        let content = (1..=600)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let co = compress("git_diff", out(&content));
        assert!(co.stash.is_some(), "stash Some when capped");

        let stash = co.stash.as_ref().unwrap();

        // Handle must appear verbatim in the hint.
        assert!(
            co.output
                .content
                .contains(&format!("retrieve(handle=\"{}\")", stash.handle)),
            "handle embedded in hint: {}",
            co.output.content
        );

        // Original == the raw line sequence (git_diff is non-noisy, so no dedup).
        let expected_original: String = content.lines().collect::<Vec<_>>().join("\n");
        assert_eq!(&*stash.original, expected_original, "original is the cap input");

        // Handle is deterministic for the same (tool, content) pair.
        let expected_handle =
            crate::tools::retrieval::mint_handle("git_diff", &expected_original);
        assert_eq!(stash.handle, expected_handle, "handle is mint_handle output");
    }

    /// Shell input with a ≥5 identical-line run but total deduped lines ≤500:
    /// dedup marker present AND stash is None.
    #[test]
    fn shell_dedup_not_capped_stash_none() {
        // 10 identical lines → deduped to 2 lines ("x" + marker), well under 500.
        let content = format!("{}end", "x\n".repeat(10));
        let co = compress("shell", out(&content));
        assert!(
            co.output.content.contains("… (repeated"),
            "dedup marker present"
        );
        assert!(co.stash.is_none(), "dedup-but-not-capped: no stash");
    }
}
