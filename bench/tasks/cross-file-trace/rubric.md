# Rubric: cross-file-trace

Ground truth (verified against the pinned checkout, hyperfine v1.20.0):

1. Flag definition: `src/cli.rs` defines `Arg::new("export-json")`
   (`.long("export-json")`) in the clap command.
2. Runtime configuration: `src/main.rs` `run()` calls
   `ExportManager::from_cli_arguments(...)`; in `src/export/mod.rs` that
   constructor calls `add_exporter("export-json", ExportType::Json)`,
   pairing the flag's filename with a boxed exporter
   (`ExportType::Json => Box::<JsonExporter>::default()`). The
   `ExportManager` owns all exporters.
3. JSON serialization: `JsonExporter` in `src/export/json.rs`,
   implementing the `Exporter` trait (`serialize` to JSON bytes).
4. File write: `ExportManager::write_results` in `src/export/mod.rs`
   iterates exporters and calls the private `write_to_file(filename,
   content)` helper in the same file, which does the filesystem write.
5. Triggering: the `Scheduler` (`src/benchmark/scheduler.rs`) calls
   `write_results(&self.results, true)` after each completed benchmark
   (intermediate export, so partial results survive an abort) and
   `final_export()` -> `write_results(..., false)` at the end of the run,
   invoked from `run()` in `src/main.rs`.

Scoring (0-10):

- 0-2: Chain is wrong or fabricated; names files/types that do not exist
  or invents a write path.
- 3-5: Finds some real waypoints (e.g. the flag definition and the JSON
  exporter) but the chain has gaps, wrong ordering, or misattributes who
  triggers the write.
- 6-8: All five steps present and correctly ordered with at most one
  imprecise attribution (e.g. omits the intermediate-export trigger or
  the `write_to_file` helper name) and no fabrications.
- 9-10: Complete, ordered, and precise chain naming the files and symbols
  above, including both the per-benchmark intermediate export and the
  final export at run end.

Penalize fabricated claims harder than omissions. Do not reward verbosity.
