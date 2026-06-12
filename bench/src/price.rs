//! Pinned per-MTok price table + run cost computation.
//!
//! Source: <https://platform.claude.com/docs/en/about-claude/pricing>
//! (also published at <https://claude.com/pricing>), retrieved 2026-06-11.
//!
//! Updating this table is a **reviewed change** (spec §Open Items 4): prices
//! feed the headline cost-normalized comparison, so silent edits would break
//! comparability across benchmark runs.
//!
//! Cache-write pricing note: the Anthropic usage object reports a single
//! `cache_creation_input_tokens` count without distinguishing 5-minute from
//! 1-hour cache writes. This table prices cache writes at the **5-minute
//! (default TTL) rate of 1.25x base input**, which is what harnesses get
//! unless they explicitly request the 1-hour TTL (2x). If a harness under
//! test uses 1-hour cache writes its cost is understated here; none of the
//! benchmarked harnesses do so today.

use crate::types::Ledger;

/// USD per million tokens for one model family.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    /// 5-minute cache write (1.25x input) — see module docs.
    pub cache_write: f64,
    /// Cache hit / refresh (0.1x input).
    pub cache_read: f64,
}

const fn price(input: f64, output: f64, cache_write: f64, cache_read: f64) -> Price {
    Price { input, output, cache_write, cache_read }
}

/// Model-family → price, exact-matched after [`normalize`] strips a trailing
/// dated snapshot (`-YYYYMMDD`) or `-latest` alias suffix. Every dated
/// snapshot of a family shares the family's published price, so this is a
/// lookup, never a guess; genuinely unknown families return `None`.
///
/// Values verbatim from the published table (per MTok):
/// input / output / 5m-cache-write / cache-read.
static PRICES: &[(&str, Price)] = &[
    ("claude-fable-5", price(10.0, 50.0, 12.50, 1.0)),
    ("claude-mythos-5", price(10.0, 50.0, 12.50, 1.0)),
    ("claude-opus-4-8", price(5.0, 25.0, 6.25, 0.50)),
    ("claude-opus-4-7", price(5.0, 25.0, 6.25, 0.50)),
    ("claude-opus-4-6", price(5.0, 25.0, 6.25, 0.50)),
    ("claude-opus-4-5", price(5.0, 25.0, 6.25, 0.50)),
    ("claude-opus-4-1", price(15.0, 75.0, 18.75, 1.50)),
    ("claude-opus-4", price(15.0, 75.0, 18.75, 1.50)),
    ("claude-sonnet-4-6", price(3.0, 15.0, 3.75, 0.30)),
    ("claude-sonnet-4-5", price(3.0, 15.0, 3.75, 0.30)),
    ("claude-sonnet-4", price(3.0, 15.0, 3.75, 0.30)),
    ("claude-haiku-4-5", price(1.0, 5.0, 1.25, 0.10)),
    ("claude-3-5-haiku", price(0.80, 4.0, 1.0, 0.08)),
];

/// Strip a trailing dated snapshot (`-20250929`) or `-latest` suffix so both
/// dated ids and aliases resolve to their family entry.
fn normalize(model: &str) -> &str {
    if let Some(base) = model.strip_suffix("-latest") {
        return base;
    }
    if let Some((base, tail)) = model.rsplit_once('-') {
        if tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_digit()) {
            return base;
        }
    }
    model
}

/// Price for a model id (dated snapshot, `-latest` alias, or bare family).
/// `None` for unknown models — the report renders those as `n/a` rather than
/// guessing.
pub fn price_for(model: &str) -> Option<&'static Price> {
    let family = normalize(model);
    PRICES.iter().find(|(m, _)| *m == family).map(|(_, p)| p)
}

/// Total USD cost of every priced exchange in the ledger.
///
/// Entries without a usage object (error responses) carry no billable tokens
/// and contribute nothing. If any entry **with** usage names an unknown
/// model the whole run's cost is `None` ("n/a") — a partial sum would be a
/// silent lie in a cost comparison.
pub fn cost_usd(ledger: &Ledger) -> Option<f64> {
    let mut total = 0.0;
    for entry in &ledger.entries {
        let Some(usage) = &entry.usage else { continue };
        let p = price_for(&entry.model)?;
        total += usage.input_tokens as f64 * p.input / 1e6
            + usage.cache_creation_input_tokens as f64 * p.cache_write / 1e6
            + usage.cache_read_input_tokens as f64 * p.cache_read / 1e6
            + usage.output_tokens as f64 * p.output / 1e6;
    }
    Some(total)
}
