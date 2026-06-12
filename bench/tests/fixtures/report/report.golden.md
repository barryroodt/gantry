# Harness Benchmark Report

All efficiency metrics come from the recording proxy ledger — never from harness self-reporting — and are aggregated over **successful runs only** (every programmatic check passes and the judge score meets the task threshold); failures count against the success rate but earn no efficiency credit. Cost is computed from the pinned price table in `bench/src/price.rs` and shown next to raw token counts because cache accounting differences make tokens alone misleading. `n/a` = model missing from the price table; `—` = no successful runs.

## Headline (all tasks, medians over successful runs)

| Harness | Success | Cost USD | Uncached in | Cache read | Cache write | Out | Model calls | Tool calls | Wall ms |
|---|---|---|---|---|---|---|---|---|---|
| gantry | 4/4 (100%) | 0.0168 | 1390 | 500 | 250 | 685 | 1.5 | 1 | 8000 |
| claude-code | 3/4 (75%) | n/a | 5000 | 0 | 0 | 1200 | 1 | 1 | 25000 |

## Per-task results (median [min–max])

### explore-architecture

| Harness | Success | Cost USD | Uncached in | Cache read | Cache write | Out | Model calls | Tool calls | Wall ms |
|---|---|---|---|---|---|---|---|---|---|
| gantry | 3/3 (100%) | 0.0202 [0.0135–0.0275] | 1500 [1280–2000] | 1000 [0–3000] | 500 [0–3000] | 770 [600–800] | 2 [1–3] | 1 [0–3] | 9000 [7000–11000] |
| claude-code | 2/3 (67%) | 0.0547 [0.0360–0.0735] | 5500 [5000–6000] | 10000 [0–20000] | 4000 [0–8000] | 1350 [1200–1500] | 1.5 [1–2] | 2 [1–3] | 27500 [25000–30000] |

### targeted-edit

| Harness | Success | Cost USD | Uncached in | Cache read | Cache write | Out | Model calls | Tool calls | Wall ms |
|---|---|---|---|---|---|---|---|---|---|
| gantry | 1/1 (100%) | 0.0060 [0.0060–0.0060] | 1000 [1000–1000] | 0 [0–0] | 0 [0–0] | 200 [200–200] | 1 [1–1] | 1 [1–1] | 5000 [5000–5000] |
| claude-code | 1/1 (100%) | n/a | 2000 [2000–2000] | 0 [0–0] | 0 [0–0] | 400 [400–400] | 1 [1–1] | 1 [1–1] | 8000 [8000–8000] |

## Transparency

- Untracked traffic (non-`/v1/messages` requests, forwarded unparsed): gantry: 0 req / 0 B; claude-code: 3 req / 5120 B
- Judge bookkeeping: 5 of 7 graded runs carry a judge score; judge calls bypass the recording proxy and are excluded from every metric above.

---

- Harness versions: claude-code 2.0.55; gantry 0.4.0+abc1234
- gantry SHA: abc1234def
- Model: claude-experimental-9000, claude-sonnet-4-5-20250929
