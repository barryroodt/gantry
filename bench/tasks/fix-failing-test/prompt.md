The current working directory contains the source code of a Rust library
for reading and writing CSV data.

The library has a real bug: when the underlying reader returns an I/O
error, the record iterators do not stop. Every subsequent attempt to read
the next record retries the broken reader and yields the same I/O error
again, forever. Callers that iterate until the iterator is exhausted never
terminate. The correct behavior is: the first I/O error is returned to the
caller as an error, and after that the reader behaves as if end-of-file
had been reached (unless it is explicitly seeked), so iteration stops.

The following test currently fails against this codebase. After your fix
it must pass. The grader will write exactly this file to
`tests/bench_regression.rs` and run
`cargo test --test bench_regression` in the working directory:

```rust
use std::io::{self, Read};

use csv::Reader;

#[test]
fn no_infinite_loop_on_io_errors() {
    struct FailingRead;
    impl Read for FailingRead {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::Other, "Broken reader"))
        }
    }

    let mut record_results = Reader::from_reader(FailingRead).into_records();
    let first_result = record_results.next();
    assert!(
        matches!(&first_result, Some(Err(e)) if matches!(e.kind(), csv::ErrorKind::Io(_)))
    );
    assert!(record_results.next().is_none());
}
```

Fix the bug in the library source so that this test passes.

Constraints:

- Do not add, modify, or delete anything under `tests/` — the grader
  supplies the test file itself.
- Keep all existing behavior for non-I/O parse errors: when reading and
  parsing a record fails for reasons other than an underlying I/O error,
  iteration must still continue to the next record as it does today.
- The rest of the library's test suite must still pass.
