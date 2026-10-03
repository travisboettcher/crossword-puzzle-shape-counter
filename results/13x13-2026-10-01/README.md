# 13×13 British grid count — run record (server smoke test)

Filled-in example of [`../TEMPLATE.md`](../TEMPLATE.md): the `smoke` step of
`scripts/run-15x15.sh` on the machine later used for 15×15.
Method: [`docs/METHODS.md`](../../docs/METHODS.md).

## Result

**#Total = 162,468,835,136** valid 13×13 British-style grids, equal to Michael
Keith's published value (G4G16, 2026).

Counting convention: every valid 180°-symmetric grid counted once as an array
(Keith's `#Total`).

## Verification

| check | outcome |
|---|---|
| matches the published value | PASS |
| `verify` | not run (the published value is the check) |

## Provenance

| | |
|---|---|
| code | commit `4ddb942` (branch `claude/crossword-puzzle-shape-j0v5v1`); the one "local change" in `machine.txt` is the untracked results directory |
| toolchain | rustc 1.99.0 (b940084d7 2026-09-28) |
| machine | Vultr, AMD EPYC Turin, 32 threads (≈16 cores + SMT), 244 GB RAM available, 1.7 TB free disk |
| settings | disk mode, `THREADS=32`, budget 1,178,968,522 entries, 1 input shard per batch |
| started / finished | 2026-10-01 14:29:33 / 14:31:39 UTC (126 s) |

## Frontier sizes and timings

| row | states | finished at |
|---|---|---|
| 0 | 1,583 | 0.05 s |
| 1 | 119,485 | 0.15 s |
| 2 | 1,564,046 | 0.94 s |
| 3 | 22,125,681 | 5.1 s |
| 4 | 171,394,918 | 50.2 s |
| 5 (last row, glued as generated) | — | 126.3 s |

## Files

`smoke-13.log` (timestamped progress), `machine.txt`.
