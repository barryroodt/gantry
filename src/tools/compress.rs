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

/// Tools whose output is high-volume and not byte-faithful, so consecutive-line
/// dedup is safe and worthwhile. Faithful-content tools are intentionally absent.
pub const NOISY_TOOLS: &[&str] = &["shell"];

/// Lines kept from the start when capping.
const HEAD_LINES: usize = 400;
/// Lines kept from the end when capping (errors/summaries cluster at the end).
const TAIL_LINES: usize = 100;
/// Minimum run of identical consecutive lines before dedup collapses it.
const DEDUP_RUN: usize = 5;

/// Compress a tool's output: dedup (noisy tools only), then a recoverable
/// head+tail line cap. Returns the input unchanged (no allocation) when it is
/// already small and has no collapsible run.
pub fn compress(tool_name: &str, output: ToolOutput) -> ToolOutput {
    let needs_dedup = NOISY_TOOLS.contains(&tool_name) && has_dup_run(&output.content);
    let needs_cap = output.content.lines().count() > HEAD_LINES + TAIL_LINES;
    if !needs_dedup && !needs_cap {
        return output;
    }

    let trailing_nl = output.content.ends_with('\n');
    let lines: Vec<Cow<str>> = if needs_dedup {
        dedup_runs(&output.content)
    } else {
        output.content.lines().map(Cow::Borrowed).collect()
    };
    let capped = lines.len() > HEAD_LINES + TAIL_LINES;
    let content = render(&lines, output.bytes, capped, trailing_nl);
    ToolOutput {
        content,
        bytes: output.bytes,
        truncated: output.truncated || capped,
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
/// with a recovery hint when `capped`.
fn render(lines: &[Cow<str>], raw_bytes: usize, capped: bool, trailing_nl: bool) -> String {
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
    s.push_str(&format!(
        "[gantry: {omitted} lines omitted ({raw_bytes} bytes raw); re-read with a narrower range/query for full detail]"
    ));
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
        let o = compress("read_file", out("a\nb\nc"));
        assert_eq!(o.content, "a\nb\nc");
        assert!(!o.truncated);
    }

    #[test]
    fn caps_large_output_head_tail_with_hint() {
        let content = (1..=600)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let o = compress("git_diff", out(&content));
        assert!(o.truncated, "capped output is marked truncated");
        assert!(o.content.contains("line1"), "head kept");
        assert!(o.content.contains("line400"), "last head line kept");
        assert!(o.content.contains("line600"), "tail kept");
        assert!(o.content.contains("line501"), "first tail line kept");
        assert!(!o.content.contains("line450"), "middle omitted");
        assert!(
            o.content.contains("100 lines omitted"),
            "accurate omitted count: {}",
            o.content
        );
        assert!(o.content.contains("bytes raw"), "raw byte hint present");
    }

    #[test]
    fn retained_lines_byte_identical() {
        let content = (1..=600)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let o = compress("git_diff", out(&content));
        // The first retained lines are exactly the originals, in order.
        assert!(o.content.starts_with("line1\nline2\nline3\n"));
    }

    #[test]
    fn shell_dedups_repeated_run() {
        let content = format!("{}end", "x\n".repeat(10));
        let o = compress("shell", out(&content));
        assert_eq!(o.content, "x\n… (repeated 10×)\nend");
    }

    #[test]
    fn faithful_tool_not_deduped() {
        let content = format!("{}end", "x\n".repeat(10));
        let o = compress("read_file", out(&content));
        assert_eq!(o.content, content, "read_file content must be faithful");
    }

    #[test]
    fn dedup_below_threshold_untouched() {
        let content = format!("{}end", "y\n".repeat(3));
        let o = compress("shell", out(&content));
        assert_eq!(o.content, content, "runs shorter than DEDUP_RUN are kept");
    }
}
