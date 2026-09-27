# Crossword grid counter

Counts the number of **valid `n×n` crossword grids** — the black/white square
patterns, not the filled letters — for both American and British (cryptic)
styles, using a transfer-matrix / frontier dynamic program that folds in the
180° symmetry and the single-connected-region constraint.

It reproduces the published counts:

* **American** — OEIS [A323839](https://oeis.org/A323839) (Jim Ferry, 2019).
* **British** — Michael Keith, *How many n×n British-style crossword grids are
  there?*, Gathering 4 Gardner 16 (2026).

## Rules (from Keith's paper; odd `n`)

**Common (both styles).** (1) `n×n`, `n` odd. (2) 180° rotational symmetry.
(3) every word has ≥ 3 letters. (4) every outer-edge row and column has a white
square. (5) all white squares form one 4-connected region.

**American** additionally requires every letter to be *checked* (crossed by a
word in both directions) — equivalently, every maximal white run, horizontal and
vertical, has length ≥ 3.

**British** instead requires, for each word of length `k`: exactly `⌈k/2⌉`
checked letters (Rule 6); no 3 consecutive unchecked letters (Rule 7); and no
*pair* of adjacent unchecked letters at a word's start or end (Rule 8). A cell is
*checked* iff it is part of a word in both directions.

## Method

> **New here?** [`docs/walkthrough.html`](docs/walkthrough.html) is an interactive,
> step-through visualization of everything below — open it in a browser to watch the
> frontier sweep down a grid, merge, fold, and glue at the center.

The grid is built one row at a time. Two partial grids that look identical along
the current **frontier** (the last row placed) are interchangeable for every
future decision, so they are merged and a count is carried — collapsing an
astronomical number of grids into a DP over a manageable set of frontier states.

The frontier records, per column, the capped vertical-run length and a
canonical **connectivity label** (which frontier cells are already joined through
placed cells). A single connected region never seals a component off early, so a
transition that would orphan a component is rejected.

**Symmetry folding.** Only the top half is built. At the center row (a
palindrome) each surviving top-half frontier is glued to its own 180° mirror:
top components, center cells, and mirror components are merged and required to
form one region, and the vertical runs that cross or close at the center are
checked.

**British word rules.** Following Keith, because length-2 runs are forbidden
outright a cell is checked iff it merely has a white perpendicular neighbour.
Horizontal words are validated one row late (once a row's lower neighbour is
known); each vertical run carries the checked bit of each of its cells and is
validated at closure. See `src/british.rs`.

### Validation ladder

Every layer is independent, so agreement is strong evidence of correctness:

1. A straightforward full-grid validator (`src/grid.rs`) drives a brute-force
   enumerator (`src/brute.rs`) — the trusted oracle for `n ≤ 9`.
2. A non-symmetric connectivity DP is cross-checked against a non-symmetric
   brute force, isolating the connectivity machinery from the symmetry folding.
3. The folded DP is checked against the brute oracle (`n = 5, 7, 9`) and against
   the published counts.

## Results

`#Total` = number of valid grids (each counted once). ✓ = matches the published
value. Timings are wall-clock on 4 cores.

| n | American (A323839) | this code | British (Keith) | this code |
|---|---|---|---|---|
| 5 | 12 | ✓ | 17 | ✓ |
| 7 | 312 | ✓ | 650 | ✓ |
| 9 | 31,187 | ✓ | 68,956 | ✓ |
| 11 | 17,438,702 | ✓ | 60,384,181 | ✓ (~5 s) |
| 13 | 40,575,832,476 | ✓ (~15 s) | 162,468,835,136 | ✓ (~18 min, 7 GB) |
| 15 | 404,139,015,237,875 | ✓ (~14 min) | *open problem* | — |

American reproduces A323839 through 15×15. British reproduces Keith's `#Total`
through 13×13.

### British 13×13 — how it fits (and how fast)

The British frontier grows steeply. The top half of a 13×13 grid passes through
1.6K → 119K → 1.6M → 24M → 191M states (rows 0–4). Its last row (672M distinct
states) is never stored: each successor is glued to the center as soon as it is
generated. The run takes **18 min on 4 cores with a 7.3 GB peak**. The first
version of this DP ran out of memory past ~16 GB; the first working version
took 1 h 59 min and 11.9 GB.

* **Packed states.** A frontier is stored in 16 bytes: per column, a 5-bit code
  for the vertical-run statistic plus a 3-bit component label.
* **Mirror merging.** Reflecting a partial grid left-to-right preserves validity
  and commutes with every transition, and the palindromic center rows glue to a
  state and its mirror equally often. So each mirror pair is stored once, which
  halves both the states and the work.
* **Dead-run pruning.** A vertical run whose statistic cannot be completed in
  the cells left in its column is cut immediately.
* **Exact bitmask prefilter.** Per state, a few masks (columns forced white, runs
  that die if the new cell is checked or unchecked, component columns, and a
  row-validity lookup table) decide every rule for a candidate row. Only rows
  that contain every forced column are enumerated. The successor is then built
  with bitmask component merging (`step_fast`), not a union-find.
* **Bitmask gluing** (`GluePre`) does the same for the center row. Its
  per-column crossing checks come from a 64 KB lookup table, and the
  pure-bitwise tests run as one branch-free pass over all center rows
  (auto-vectorizable) before the table lookups and connectivity check.
  Building with `-C target-cpu=native` (AVX2/AVX-512) measured no further
  gain; the workload is dominated by branchy, data-dependent work and random
  memory access, not wide arithmetic.
* **One sharded concurrent map.** Successors merge into 1024 mutex-guarded
  shards, so each state is held once instead of once per thread.
* **Fused last row** (`BRITISH_PASSES=0`, the default from 13×13 up). Gluing
  each successor directly costs more glue calls than merging first, but it
  avoids storing ~670M states or rebuilding the row in several passes.
  `BRITISH_PASSES=k` builds and glues the last row in `k` passes instead.

The fast paths are checked against the straightforward all-rules `step` and
`glue_ok` on every state and every candidate row through 9×9 (unit test in
`src/british.rs`).

Timing breakdown before the step/glue rewrite (11×11, last row): enumerating
and prefiltering candidates 9%, building successors 51%, packing 30%, and
hash-map inserts 8%.

**15×15 (open).** Rows grew ×18–28 from 11×11 to 13×13 at the same row
index, which projects roughly 0.5B / 6B / 25B states for 15×15 rows 3–5 and
~100B successors into the last row. CPU cost per input state for the last
row rose from ~9 µs (11×11) to ~29 µs (13×13); at a projected ~90 µs, 15×15 is
roughly 800 CPU-hours (about a week on 4 cores, about half a day on 64). That is
a rough extrapolation from two data points. Row 5 alone (~600 GB packed) also
needs disk-sharded storage. Candidates for real speedups:
a cell-by-cell (broken-profile) transfer instead of row-by-row, merging run
statistics with identical futures, and a many-core machine (the transfer is
embarrassingly parallel).

## Usage

```sh
# Count via the DP (American uses the folded transfer matrix; British likewise)
cargo run --release --bin count -- --style american --n 13
cargo run --release --bin count -- --style british  --n 9
cargo run --release --bin count -- --style british --n 13   # ~18 min

# Timing / frontier statistics
DP_STATS=1 cargo run --release --bin bench -- american 13
DP_STATS=1 cargo run --release --bin bench -- british 13
```

## Tests

```sh
cargo test --release              # fast suite (n ≤ 11)
cargo test --release -- --ignored # slow anchors (n = 13, 15; British 11, 13)
```

## Layout

| file | purpose |
|---|---|
| `src/rules.rs` | the rule set; `word_checks_ok` (single source of truth for Rules 6–8) |
| `src/grid.rs` | grid type + full validator (brute-force oracle) |
| `src/brute.rs` | reference enumerators (symmetric and non-symmetric) |
| `src/dp.rs` | American folded transfer-matrix DP + non-symmetric DP |
| `src/british.rs` | British folded DP with the checked-letter word rules |
| `src/bin/count.rs` | CLI |

## References

- OEIS [A323839](https://oeis.org/A323839) — American counts (Jim Ferry, 2019).
- Michael Keith, *How many n×n British-style crossword grids are there?*, G4G16
  (2026) — British counts to 13×13; 15×15 is open.
