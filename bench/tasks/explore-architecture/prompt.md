The current working directory contains the complete source code of a
command-line benchmarking tool written in Rust.

Explain the architecture of this codebase. Your explanation must cover:

1. The end-to-end flow of a run: from process start to final output, naming
   the main stages in order and the source files or modules that implement
   each stage.
2. The major subsystems and the responsibility of each (command-line
   handling, benchmark execution, time measurement, result aggregation and
   comparison, terminal output, file export).
3. How a benchmarked command is actually executed and measured at a high
   level, including any distinct execution strategies the code supports.
4. How results reach the user: both what is printed to the terminal and how
   export files in the various supported formats are produced.

Be specific: refer to actual module paths, type names, and function names
from the source. Only make claims you can ground in the code — do not
speculate about behavior you have not seen. Finish with your complete
explanation as the final answer.
