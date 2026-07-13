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
| 11 | 17,438,702 | ✓ | 60,384,181 | ✓ (~73 s) |
| 13 | 40,575,832,476 | ✓ (~15 s) | 162,468,835,136 | ⚠ memory-bound (see below) |
| 15 | 404,139,015,237,875 | ✓ (~14 min) | *open problem* | — |

American reproduces A323839 through 15×15. British reproduces Keith's `#Total`
through 11×11.

### British 13×13 — memory-bound here

The same British DP is correct at 13×13 (Keith's value is 162,468,835,136), but
it exceeds the ~15 GB of RAM in this environment. The frontier grows steeply:
row 3 alone reaches ~49M states, and the two remaining top-half rows climb into
the hundreds of millions, OOM-ing past ~16 GB. Reaching 13×13 (and the open
15×15) would need a lower-memory state encoding or an external-memory / sharded
transfer step — a natural next step, not a correctness gap.

## Usage

```sh
# Count via the DP (American uses the folded transfer matrix; British likewise)
cargo run --release --bin count -- --style american --n 13
cargo run --release --bin count -- --style british  --n 9

# Timing / frontier statistics
DP_STATS=1 cargo run --release --bin bench -- american 13
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
