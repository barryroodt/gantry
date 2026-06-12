The current working directory contains the complete source code of a
command-line benchmarking tool written in Rust. The tool accepts a flag
`--export-json <FILE>` which causes benchmark results to be written to the
given file as JSON when the run finishes.

Trace, end to end, how that flag becomes a JSON file on disk. This is an
investigation task only — do not modify any files.

Your final answer must walk the chain in order and, for every step, name
the source file plus the key type or function involved:

1. Where the `--export-json` command-line flag is defined.
2. How the parsed flag is turned into runtime export configuration, and
   which component owns exporting.
3. Which type implements the JSON serialization of results.
4. Which function performs the actual write of bytes to the target file.
5. Where during a run the export writes are triggered, including what
   happens at the very end of the run.
