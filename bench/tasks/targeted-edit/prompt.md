The current working directory contains the source code of a small Rust
library crate that formats integers into decimal strings.

Perform the following refactor, exactly and completely:

Inline the one-function module `u128_ext` into the crate root:

1. Move the function `mulhi` from `src/u128_ext.rs` into `src/lib.rs` as a
   crate-private function, keeping its documentation comment and its
   attributes intact.
2. Update the single call site in `src/lib.rs` that currently calls
   `u128_ext::mulhi(...)` so it calls the moved function directly.
3. Remove the `mod u128_ext;` declaration from `src/lib.rs`.
4. Delete the file `src/u128_ext.rs`.

Constraints:

- This is a pure refactor: the crate's behavior and public API must not
  change, and the crate must still compile and pass its existing tests.
- Do not modify `Cargo.toml`, anything under `tests/`, or any file other
  than the two source files involved.
- Leave no remaining references to `u128_ext` anywhere in `src/`.
