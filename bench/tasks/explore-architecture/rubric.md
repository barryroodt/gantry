# Rubric: explore-architecture

Ground truth (verified against the pinned checkout, hyperfine v1.20.0):

- Entry flow: `src/main.rs` `run()` parses CLI args via `get_cli_arguments`
  (clap definitions in `src/cli.rs`), builds `Options`
  (`src/options.rs`), builds `Commands` (`src/command.rs`, with parameter
  scans expanded by `src/parameter/`), and builds an `ExportManager`
  (`src/export/mod.rs`).
- Core engine: `Scheduler` in `src/benchmark/scheduler.rs` drives
  `run_benchmarks()`. Execution goes through the `Executor` trait in
  `src/benchmark/executor.rs` with `ShellExecutor` (command run through a
  shell, with shell spawn-overhead calibration), `RawExecutor` (direct
  execution without a shell), and a `MockExecutor`.
- Timing: `src/timer/` (wall-clock timer plus platform CPU timers in
  `unix_timer.rs` / `windows_timer.rs`).
- Results: `BenchmarkResult` (`src/benchmark/benchmark_result.rs`),
  relative speed comparison (`src/benchmark/relative_speed.rs`), outlier
  detection (`src/outlier_detection.rs`).
- User-facing output: `src/output/` (progress bars, warnings, formatting).
  Exports: `ExportManager::write_results` writes through per-format
  exporters (`src/export/{json,csv,markdown,asciidoc,orgmode}.rs`);
  the scheduler triggers intermediate exports during the run and
  `final_export()` at the end.

Scoring (0-10):

- 0-2: Wrong or fabricated. Invents modules, flows, or responsibilities
  that do not exist in the code; or describes a generic CLI tool with no
  grounding in this repository.
- 3-5: Surface-level. Lists some real directories/files but misses the
  run flow, conflates subsystems, or cannot say how execution and timing
  actually happen. Minor fabrications.
- 6-8: Substantially correct. Names the real entry flow (cli -> options /
  commands -> scheduler -> executor -> output/export), correctly assigns
  responsibilities to most subsystems above, and describes the
  shell-vs-raw execution distinction or the calibration step. Small gaps
  or one minor inaccuracy allowed; no fabrications.
- 9-10: Complete and precise. Covers all four prompt points with correct
  module paths and key type/function names (Scheduler, Executor variants,
  ExportManager, BenchmarkResult), including how exports are triggered and
  how timing is measured. No errors, no fabrications, no padding.

Penalize fabricated claims harder than omissions. Do not reward verbosity.
