# 15×15 British grid count — run record

Method: [`docs/METHODS.md`](../../docs/METHODS.md). Template:
[`../TEMPLATE.md`](../TEMPLATE.md).

## Result

**#Total = 2,393,670,267,515,481** valid 15×15 British-style grids
(≈ 2.39 × 10¹⁵).

> **Status: single run, verification pending.** Not to be quoted as final until
> the `verify` run (different merge order, overflow-checked build) reproduces
> the total and all 1024 per-shard partial sums.

Counting convention: every valid 180°-symmetric grid counted once as an array;
mirror images and rotations count separately when they differ (Keith's
`#Total`, not the primitive count).

Context: 13×13 = 162,468,835,136 (Keith; reproduced), so 15×15 / 13×13 ≈ 14,733.
Earlier ratios: 7→9 ×106, 9→11 ×876, 11→13 ×2,690.

## Verification

| check | outcome |
|---|---|
| `smoke` on the same machine (13×13 = 162,468,835,136) | PASS (126 s) |
| `RESULT` equals the sum of the 1024 partial sums | PASS |
| per-shard partial sums | all 1024 present and non-zero; 2.19–2.52 × 10¹², mean 2.34 × 10¹², spread 2.2% (shards are hash-assigned, so a damaged shard would likely stand out) |
| `verify`: second full run (half budget, overflow checks on; row 4 and row 5 state counts already match exactly) | **in progress** |
| sha256 of `glue-partial-sums.txt` | `de4289d15d72a03a55807696d3de8624cd873ea167835b2fab4fa224e7cf7c99` |

## Provenance

| | |
|---|---|
| code | `travisboettcher/crossword-puzzle-shape-counter`, commit `4ddb942` (branch `claude/crossword-puzzle-shape-j0v5v1`); the one "local change" is the untracked results directory |
| toolchain | rustc 1.99.0 (b940084d7 2026-09-28) |
| machine | Vultr, AMD EPYC Turin, 32 threads (≈16 cores + SMT), 244 GB RAM available, 1.7 TB free disk |
| settings | disk mode, `RAYON_NUM_THREADS=32 BRITISH_DISK_BUDGET=1178968522 BRITISH_DISK_BATCH=1` |
| started / finished | 2026-10-01 14:46:42 / 2026-10-02 02:03:29 UTC (40,607 s = 11 h 17 min), no interruptions |

## Frontier sizes and timings

States after left–right mirror merging (from `run-15.log`).

| row | states | on disk | finished at |
|---|---|---|---|
| 0 | 5,451 | 0.1 MB | 0.06 s |
| 1 | 864,641 | 13 MB | 0.24 s |
| 2 | 17,782,460 | 267 MB | 1.2 s |
| 3 | 386,108,733 | 5.8 GB | 29 s |
| 4 | 4,485,359,212 | 68 GB | 1,969 s (32.8 min) |
| 5 | 16,610,709,547 | 262 GB | 26,004 s (7.2 h) |
| 6 (last row, glued as generated; not stored) | — | — | 40,606 s (11.3 h) |

154 spills (compactions) in total. Peak disk and RAM were not recorded; the
largest stored row is 262 GB, built while row 4 (68 GB) was still on disk.

## Files

| file | contents |
|---|---|
| `RESULT` | the total |
| `CONFIG` | `n=15 shards=1024 format=1` (no sampling) |
| `glue-partial-sums.txt` | last-row partial sum for each of the 1024 input shards |
| `run-15.log` | timestamped per-row progress, with machine details |
| `machine.txt`, `script.log` | machine/toolchain details; the script's own log |
| `smoke-13.log`, `rehearsal-15.log` | pre-flight checks (the rehearsal is a sample, not a count) |
