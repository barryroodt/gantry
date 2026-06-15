# Rubric: locate-bug

Ground truth (verified against the pinned checkout and the upstream fix):

- Defect location: `crates/printer/src/json.rs`, in the `finish()` method
  of the `Sink` implementation for `JSONSink`.
- Mechanism: `finish()` starts with `if !self.begin_printed { return
  Ok(()); }`. The JSON printer only prints a `begin` message for files
  that produce at least one match, so for every file without a match,
  `finish()` returns before the statistics-tally block
  (`add_elapsed`, `add_searches`, `add_searches_with_match`,
  `add_bytes_searched`, `add_bytes_printed`). As a result the summary
  statistics aggregate only files that had matches: `searches` always
  equals `searches_with_match` and `bytes_searched` undercounts.
- Correct fix shape: tally the statistics before the `begin_printed`
  early-return (equivalently: move the early-return to just before the
  `End` message is written), so stats accumulate for every search while
  per-file `end` messages still only appear for files with a `begin`.

Scoring (0-10):

- 0-2: Wrong file, fabricated mechanism, or generic guessing (e.g. blames
  the search engine, the walker, or output buffering with no evidence).
- 3-5: Right neighborhood (the JSON printer / stats plumbing) but wrong or
  vague mechanism; or names the file without explaining why unmatched
  files are excluded from the tallies.
- 6-8: Names `json.rs` and `finish()` and correctly identifies the
  `begin_printed` early-return ordering as the cause, with a plausible
  fix; explanation has minor gaps or imprecision.
- 9-10: Fully precise: file, method, the exact early-return-before-tally
  ordering, why `begin` is absent for matchless files, the visible
  symptom linkage (searches == searches_with_match, undercounted
  bytes_searched), and a fix that preserves the begin/end message
  behavior while fixing the stats.

Penalize fabricated claims harder than omissions. Do not reward verbosity.
