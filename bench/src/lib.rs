//! gantry-bench: benchmarks gantry against other agent harnesses (Claude Code,
//! Pi) on identical tasks through a recording proxy, producing ground-truth
//! efficiency metrics plus graded output quality.
//!
//! Module map mirrors the canonical crate layout in the implementation plan;
//! all shared JSON artifact types live in [`types`].

pub mod grade;
pub mod harness;
pub mod price;
pub mod proxy;
pub mod report;
pub mod runner;
pub mod task;
pub mod types;
