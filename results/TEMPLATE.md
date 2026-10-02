# <N>×<N> British grid count — run record

Copy this file to `results/<N>x<N>-<date>/README.md` and fill it in from the
`scripts/run-15x15.sh export` bundle, whose files go in the same directory.
Method: [`docs/METHODS.md`](../docs/METHODS.md).

## Result

**#Total = <count>** valid <N>×<N> British-style grids.

Counting convention: every valid 180°-symmetric grid counted once as an array;
mirror images and rotations count separately when they differ (Keith's
`#Total`, not the primitive count).

## Verification

| check | outcome |
|---|---|
| `smoke` on the same machine (13×13 = 162,468,835,136) | <PASS/FAIL> |
| `verify`: second full run, budget <…>, batch <…>, overflow checks on | <PASS: same total and all 1024 partial sums / FAIL / not run> |
| per-shard partial sums | `glue-partial-sums.txt` (1024 lines; sha256 `<…>`) |

## Provenance

| | |
|---|---|
| code | `travisboettcher/crossword-puzzle-shape-counter`, commit `<hash>` (branch `<branch>`), <0> local changes |
| toolchain | `<rustc --version>` |
| machine | <provider / plan>, <CPU>, <cores> threads, <RAM>, <disk> |
| settings | `THREADS=<…> BRITISH_DISK_BUDGET=<…> BRITISH_DISK_BATCH=<…>` |
| started / finished | <UTC> / <UTC> (<wall time>), <interruptions/resumes> |

## Frontier sizes and timings

From the run log (`run-<N>.log`); states are after left–right mirror merging.

| row | states | finished at (wall) |
|---|---|---|
| 0 | | |
| 1 | | |
| … | | |
| <h−2> | | |
| <h−1> (last row, glued as generated; not stored) | — | |

Peak disk: <…> GB. Peak RAM: <…> GB.

## Files

| file | contents |
|---|---|
| `RESULT` | the total |
| `CONFIG` | grid size, shard count, record format (and sampling, if any) |
| `glue-partial-sums.txt` | the last-row partial sum for each of the 1024 input shards |
| `run-<N>.log` | timestamped per-row progress |
| `verify-<N>.log`, `verify-<N>.txt` | the verification run and its verdict |
| `machine.txt` | machine and toolchain details |
| `smoke-13.log`, `rehearsal-<N>.log` | pre-flight checks |

## Notes

<anything unusual: interruptions, resumes, warnings>
