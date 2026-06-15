# Harness Benchmark Report

All efficiency metrics come from the recording proxy ledger — never from harness self-reporting — and are aggregated over **successful runs only** (every programmatic check passes and the judge score meets the task threshold); failures count against the success rate but earn no efficiency credit. Cost is computed from the pinned price table in `bench/src/price.rs` and shown next to raw token counts because cache accounting differences make tokens alone misleading. `n/a` = model missing from the price table; `—` = no successful runs.

## Headline (all tasks, medians over successful runs)

| Harness | Success | Cost USD | Uncached in | Cache read | Cache write | Out | Model calls | Tool calls | Wall ms |
|---|---|---|---|---|---|---|---|---|---|
| gantry | 0/1 (0%) | — | — | — | — | — | — | — | — |

## Per-task results (median [min–max])

### explore-architecture

| Harness | Success | Cost USD | Uncached in | Cache read | Cache write | Out | Model calls | Tool calls | Wall ms |
|---|---|---|---|---|---|---|---|---|---|
| gantry | 0/1 (0%) | — | — | — | — | — | — | — | — |

## Transparency

- Untracked traffic (non-`/v1/messages` requests, forwarded unparsed): gantry: 0 req / 0 B
- Judge bookkeeping: 0 of 1 graded runs carry a judge score (0 in / 0 out judge tokens); judge calls bypass the recording proxy and are excluded from every metric above.

---

- Harness versions: gantry bae78dc8a950
- gantry SHA: bae78dc8a950f631dd25de3bd98e3f9eff52f98b
- Model: claude-haiku-4-5
