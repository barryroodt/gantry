The current working directory contains the complete source code of a
command-line text search tool written in Rust. It supports a JSON output
mode and can report summary statistics about a search.

A user filed the following bug report:

> When I run a search in JSON mode, the summary statistics in the final
> `summary` message are wrong. Searching a directory tree where 218 files
> are searched and 80 files contain matches, the plain statistics output
> correctly reports 218 files searched, but the JSON summary reports
> `"searches": 80` — and `"searches"` always equals
> `"searches_with_match"`. `"bytes_searched"` is also far smaller than the
> value reported by the plain statistics output for the identical search.
> It looks like the JSON summary only counts the files that had matches,
> instead of every file that was searched. The match counts themselves
> (`"matches"`, `"matched_lines"`) are correct.

Find the root cause of this bug in the source code. This is an
investigation task only — do not modify any files.

Your final answer must include:

1. The path of the source file containing the defect.
2. The function or method where the defect lives.
3. A precise explanation of the mechanism: why exactly the statistics only
   reflect files with matches.
4. A short description of how you would fix it.
