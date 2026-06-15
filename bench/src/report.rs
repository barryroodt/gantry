//! Aggregation + `results.json` / `report.md` rendering.
//!
//! Every efficiency number here derives from the proxy [`Ledger`] inside each
//! [`RunRecord`] (invariant 1) and is aggregated over **successful** runs only
//! (invariant 7) — failures count against the success rate but never feed the
//! efficiency medians, so there is no credit for failing cheaply.
//!
//! [`Ledger`]: crate::types::Ledger

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::price;
use crate::types::{RunRecord, RunResult};

/// Read every `*.json` file in `raw_dir` as one [`RunRecord`] and return them
/// sorted by (task, harness, rep) — the `results.json` assembly step.
pub fn assemble_results(raw_dir: &Path) -> Result<Vec<RunRecord>> {
    let mut records = Vec::new();
    for dirent in fs::read_dir(raw_dir)
        .with_context(|| format!("reading raw results dir {}", raw_dir.display()))?
    {
        let path = dirent?.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let bytes =
            fs::read(&path).with_context(|| format!("reading run record {}", path.display()))?;
        let record: RunRecord = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing run record {}", path.display()))?;
        records.push(record);
    }
    records.sort_by(|a, b| {
        (
            &a.run.task_id,
            harness_rank(&a.run.harness),
            &a.run.harness,
            a.run.rep,
        )
            .cmp(&(
                &b.run.task_id,
                harness_rank(&b.run.harness),
                &b.run.harness,
                b.run.rep,
            ))
    });
    Ok(records)
}

/// Serialize the full record set as pretty-printed `results.json` content.
pub fn results_json(records: &[RunRecord]) -> Result<String> {
    let mut json = serde_json::to_string_pretty(records).context("serializing results.json")?;
    json.push('\n');
    Ok(json)
}

/// Write `results.json` + `report.md` into `out_dir`.
pub fn write_artifacts(out_dir: &Path, records: &[RunRecord]) -> Result<()> {
    fs::create_dir_all(out_dir)
        .with_context(|| format!("creating output dir {}", out_dir.display()))?;
    fs::write(out_dir.join("results.json"), results_json(records)?)
        .context("writing results.json")?;
    fs::write(out_dir.join("report.md"), render_report(records)).context("writing report.md")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Aggregation
// ---------------------------------------------------------------------------

/// Per-run efficiency metrics, all derived from the proxy ledger (plus
/// orchestrator wall time). Stored as `f64` because medians of even-sized
/// samples land between values.
struct RunMetrics {
    cost: Option<f64>,
    uncached_in: f64,
    cache_read: f64,
    cache_write: f64,
    out: f64,
    model_calls: f64,
    tool_calls: f64,
    wall_ms: f64,
}

fn run_metrics(run: &RunResult) -> RunMetrics {
    let mut uncached_in = 0u64;
    let mut cache_read = 0u64;
    let mut cache_write = 0u64;
    let mut out = 0u64;
    let mut tool_calls = 0usize;
    for entry in &run.ledger.entries {
        if let Some(usage) = &entry.usage {
            uncached_in += usage.input_tokens;
            cache_read += usage.cache_read_input_tokens;
            cache_write += usage.cache_creation_input_tokens;
            out += usage.output_tokens;
        }
        tool_calls += entry.tool_uses.len();
    }
    RunMetrics {
        cost: price::cost_usd(&run.ledger),
        uncached_in: uncached_in as f64,
        cache_read: cache_read as f64,
        cache_write: cache_write as f64,
        out: out as f64,
        model_calls: run.ledger.entries.len() as f64,
        tool_calls: tool_calls as f64,
        wall_ms: run.wall_ms as f64,
    }
}

/// Success per invariant 7; an ungraded run is never a success.
fn is_success(record: &RunRecord) -> bool {
    record.grade.as_ref().is_some_and(|g| g.success)
}

/// One (task × harness) — or, for the headline, (× harness) — aggregate.
#[derive(Default)]
struct Cell {
    runs: u32,
    successes: u32,
    /// Metrics of successful runs only.
    metrics: Vec<RunMetrics>,
}

impl Cell {
    fn add(&mut self, record: &RunRecord) {
        self.runs += 1;
        if is_success(record) {
            self.successes += 1;
            self.metrics.push(run_metrics(&record.run));
        }
    }
}

struct Stats {
    median: f64,
    min: f64,
    max: f64,
}

fn stats(mut values: Vec<f64>) -> Option<Stats> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).expect("metric values are finite"));
    let n = values.len();
    let median = if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    };
    Some(Stats {
        median,
        min: values[0],
        max: values[n - 1],
    })
}

/// Cost aggregates separately because any unpriceable run (unknown model)
/// makes the whole cell `n/a` — a partial median would silently misrank.
enum CostStats {
    Empty,
    NotApplicable,
    Known(Stats),
}

impl Cell {
    fn stat(&self, get: fn(&RunMetrics) -> f64) -> Option<Stats> {
        stats(self.metrics.iter().map(get).collect())
    }

    fn cost_stats(&self) -> CostStats {
        let mut values = Vec::with_capacity(self.metrics.len());
        for m in &self.metrics {
            match m.cost {
                Some(c) => values.push(c),
                None => return CostStats::NotApplicable,
            }
        }
        match stats(values) {
            Some(s) => CostStats::Known(s),
            None => CostStats::Empty,
        }
    }
}

/// Canonical harness column order: gantry first, then the comparison set,
/// then anything unexpected (alphabetical).
fn harness_rank(name: &str) -> u8 {
    match name {
        "gantry" => 0,
        "claude-code" => 1,
        "pi" => 2,
        _ => 3,
    }
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// Integer-valued metrics print bare; medians of even samples may carry .5.
fn fmt_num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

fn fmt_cost(v: f64) -> String {
    format!("{v:.4}")
}

fn fmt_range(s: &Stats, fmt: fn(f64) -> String) -> String {
    format!("{} [{}–{}]", fmt(s.median), fmt(s.min), fmt(s.max))
}

fn num_cell(s: Option<Stats>, with_range: bool) -> String {
    match s {
        None => "—".to_string(),
        Some(s) if with_range => fmt_range(&s, fmt_num),
        Some(s) => fmt_num(s.median),
    }
}

fn cost_cell(c: CostStats, with_range: bool) -> String {
    match c {
        CostStats::Empty => "—".to_string(),
        CostStats::NotApplicable => "n/a".to_string(),
        CostStats::Known(s) if with_range => fmt_range(&s, fmt_cost),
        CostStats::Known(s) => fmt_cost(s.median),
    }
}

fn success_cell(cell: &Cell) -> String {
    let pct = (f64::from(cell.successes) / f64::from(cell.runs) * 100.0).round() as u32;
    format!("{}/{} ({pct}%)", cell.successes, cell.runs)
}

const TABLE_HEADER: &str = "| Harness | Success | Cost USD | Uncached in | Cache read | \
                            Cache write | Out | Model calls | Tool calls | Wall ms |\n\
                            |---|---|---|---|---|---|---|---|---|---|\n";

fn table_row(out: &mut String, harness: &str, cell: &Cell, with_range: bool) {
    let _ = writeln!(
        out,
        "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
        harness,
        success_cell(cell),
        cost_cell(cell.cost_stats(), with_range),
        num_cell(cell.stat(|m| m.uncached_in), with_range),
        num_cell(cell.stat(|m| m.cache_read), with_range),
        num_cell(cell.stat(|m| m.cache_write), with_range),
        num_cell(cell.stat(|m| m.out), with_range),
        num_cell(cell.stat(|m| m.model_calls), with_range),
        num_cell(cell.stat(|m| m.tool_calls), with_range),
        num_cell(cell.stat(|m| m.wall_ms), with_range),
    );
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Render the full `report.md` from a set of run records. Pure function of
/// its input — no clocks, no environment — so golden-file tests stay stable.
pub fn render_report(records: &[RunRecord]) -> String {
    // Harnesses in canonical order, tasks alphabetical.
    let mut harnesses: Vec<&str> = Vec::new();
    let mut tasks: Vec<&str> = Vec::new();
    for r in records {
        if !harnesses.contains(&r.run.harness.as_str()) {
            harnesses.push(&r.run.harness);
        }
        if !tasks.contains(&r.run.task_id.as_str()) {
            tasks.push(&r.run.task_id);
        }
    }
    harnesses.sort_by_key(|h| (harness_rank(h), *h));
    tasks.sort_unstable();

    let mut headline: BTreeMap<&str, Cell> = BTreeMap::new();
    let mut per_task: BTreeMap<(&str, &str), Cell> = BTreeMap::new();
    let mut untracked: BTreeMap<&str, (u32, u64)> = BTreeMap::new();
    for r in records {
        headline.entry(&r.run.harness).or_default().add(r);
        per_task
            .entry((&r.run.task_id, &r.run.harness))
            .or_default()
            .add(r);
        let (req, bytes) = untracked.entry(&r.run.harness).or_default();
        *req += r.run.ledger.untracked_requests;
        *bytes += r.run.ledger.untracked_bytes;
    }

    let mut out = String::new();
    out.push_str("# Harness Benchmark Report\n\n");
    out.push_str(
        "All efficiency metrics come from the recording proxy ledger — never from \
         harness self-reporting — and are aggregated over **successful runs only** \
         (every programmatic check passes and the judge score meets the task \
         threshold); failures count against the success rate but earn no efficiency \
         credit. Cost is computed from the pinned price table in `bench/src/price.rs` \
         and shown next to raw token counts because cache accounting differences make \
         tokens alone misleading. `n/a` = model missing from the price table; `—` = \
         no successful runs.\n\n",
    );

    out.push_str("## Headline (all tasks, medians over successful runs)\n\n");
    out.push_str(TABLE_HEADER);
    for h in &harnesses {
        if let Some(cell) = headline.get(h) {
            table_row(&mut out, h, cell, false);
        }
    }

    out.push_str("\n## Per-task results (median [min–max])\n");
    for t in &tasks {
        let _ = write!(out, "\n### {t}\n\n");
        out.push_str(TABLE_HEADER);
        for h in &harnesses {
            if let Some(cell) = per_task.get(&(*t, *h)) {
                table_row(&mut out, h, cell, true);
            }
        }
    }

    out.push_str("\n## Transparency\n\n");
    let untracked_line = harnesses
        .iter()
        .map(|h| {
            let (req, bytes) = untracked.get(h).copied().unwrap_or_default();
            format!("{h}: {req} req / {bytes} B")
        })
        .collect::<Vec<_>>()
        .join("; ");
    let _ = writeln!(
        out,
        "- Untracked traffic (non-`/v1/messages` requests, forwarded unparsed): {untracked_line}"
    );
    let graded = records.iter().filter(|r| r.grade.is_some()).count();
    let scored = records
        .iter()
        .filter(|r| r.grade.as_ref().is_some_and(|g| g.judge_score.is_some()))
        .count();
    // Judge spend comes from the persisted GradeResult.judge_usage field —
    // bookkeeping only, never an efficiency metric (invariant 6).
    let (judge_in, judge_out) = records
        .iter()
        .filter_map(|r| r.grade.as_ref().and_then(|g| g.judge_usage.as_ref()))
        .fold((0u64, 0u64), |(i, o), u| {
            (i + u.input_tokens, o + u.output_tokens)
        });
    let _ = writeln!(
        out,
        "- Judge bookkeeping: {scored} of {graded} graded runs carry a judge score \
         ({judge_in} in / {judge_out} out judge tokens); judge calls bypass the \
         recording proxy and are excluded from every metric above."
    );

    out.push_str("\n---\n\n");
    let mut versions: Vec<String> = Vec::new();
    let mut shas: Vec<&str> = Vec::new();
    let mut models: Vec<&str> = Vec::new();
    for r in records {
        let v = format!("{} {}", r.run.harness, r.run.harness_version);
        if !versions.contains(&v) {
            versions.push(v);
        }
        if !shas.contains(&r.run.gantry_sha.as_str()) {
            shas.push(&r.run.gantry_sha);
        }
        if !models.contains(&r.run.model.as_str()) {
            models.push(&r.run.model);
        }
    }
    versions.sort_unstable();
    shas.sort_unstable();
    models.sort_unstable();
    let _ = writeln!(out, "- Harness versions: {}", versions.join("; "));
    let _ = writeln!(out, "- gantry SHA: {}", shas.join(", "));
    let _ = writeln!(out, "- Model: {}", models.join(", "));
    out
}
