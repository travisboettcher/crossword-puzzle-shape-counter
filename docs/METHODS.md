# Counting British-style crossword grids: methods

This document describes how `crossword-grids` counts valid n×n British-style
(cryptic) crossword grids, and argues why each step preserves the count. It is
written to be checked: every shortcut the program takes is stated together with
the reason it cannot change the answer, and with the test that checks it.

The American-style counter (`src/dp.rs`) uses the same framework with simpler
word rules; it reproduces OEIS A323839 through 15×15 and is not discussed
further here.

## 1. What is counted

A **grid** is an n×n array of black and white cells, n odd. A **word** is a
maximal horizontal or vertical run of white cells of length ≥ 3. A white cell is
**checked** if it belongs to a word in both directions.

A grid is **valid** (Michael Keith, *How many n×n British-style crossword grids
are there?*, G4G16, 2026) if:

1. it is n×n with n odd;
2. it has 180° rotational symmetry;
3. every maximal white run has length 1 or ≥ 3 (no 2-letter words);
4. every outer row and column (top, bottom, left, right) contains a white cell;
5. the white cells form one 4-connected region;
6. every word of length k has exactly ⌈k/2⌉ checked letters;
7. no word has 3 or more consecutive unchecked letters;
8. no word starts or ends with 2 consecutive unchecked letters.

**The count is Keith's `#Total`:** the number of distinct valid grids as arrays.
A grid and its mirror image, or its 90° rotation, count separately when they
differ. (Keith's "primitive" count, up to the square's symmetries, is smaller
by roughly 4× and is not computed here.)

`src/rules.rs` (`word_checks_ok`) is the single definition of Rules 6–8.
`src/grid.rs` checks a whole grid against all eight rules, and the brute-force
enumerator `src/brute.rs` uses it as the reference oracle (British: n = 5, 7;
American: n ≤ 9).

## 2. Two facts the method relies on

**Fact A: a white cell is checked iff it has a white neighbour across it.** Take
a white cell and its vertical run. By Rule 3 that run has length 1 or ≥ 3. If
the cell has a white vertical neighbour, the run has length ≥ 2, hence ≥ 3, so
the cell is in a vertical word. If it has none, the run has length 1 and the
cell is in no vertical word. The same holds horizontally. So "checked in its
horizontal word" means exactly "has a white cell directly above or below", and
vice versa. Every checked/unchecked status is therefore a *local* property of
the cell's neighbours, known as soon as they are placed. (Keith uses the same
observation.)

**Fact B: Rules 3 and 6–8 depend on a word only through a small summary.** Read
a word's letters in order as checked (c) or unchecked (u). Let `len` be its
length capped at 3, `d` = #c − #u, and `trail` the number of trailing u's. Then:

* Rule 6: #c = ⌈k/2⌉ ⇔ d = 2⌈k/2⌉ − k ⇔ d ∈ {0, 1} (the parity of d is that of k).
* Rule 7: reject the moment `trail` would reach 3.
* Rule 8 (start): reject when the second letter is u and the first was u, i.e.
  when `len` = 1 and `trail` = 1 before adding a u.
* Rule 8 (end): at the word's end, require `trail` < 2.
* Rule 3: at the end, `len` must not be 2.

Every rule is checked either as a letter is added (from `(len, trail, d)` and
the new letter) or when the word ends (from `(len, trail, d)` alone). So two
partial words with the same `(len, trail, d)` have exactly the same set of
valid continuations: the summary is sufficient. `vrun_close_ok` and the growth
code in `step` implement this. Rules 6–8 are symmetric under reversing a word,
which matters for the symmetry arguments below.

## 3. The transfer-matrix (frontier) DP

The grid is built one row at a time, top to bottom. After placing rows 0…i,
everything the remaining rows can interact with is captured by the
**frontier state**:

* per column j: the summary `(len, trail, d)` of the vertical run ending in
  row i (len = 0 if the cell in row i is black), which by Fact B is all the
  future needs to know about that column's open vertical word;
* per white cell of row i: a **component label**. Two cells share a label iff
  they are joined by a white path through rows 0…i. Labels are renumbered in
  order of first appearance, so equal partitions get equal labels. Because
  the placed region is planar and every path lies above row i, the components
  meet row i in a non-crossing pattern;
* two **edge flags**: whether column 0 / column n−1 has had a white cell yet
  (Rule 4).

Partial grids with the same frontier state have exactly the same set of valid
completions, so they are merged, with a count of how many partial grids each
state stands for. At 13×13, a stored state at row 4 stands for about 100
partial grids on average; this merging is what makes the count feasible.

**Placing row i+1** (white mask w) onto a frontier (`step` in
`src/british.rs`):

1. Row w must have no white run of length 2 (Rule 3, horizontal).
2. Row i's horizontal words can now be judged, because by Fact A each of their
   cells is checked iff it has a white cell above (`len` ≥ 2) or below
   (`w` white there). They are validated with `word_checks_ok`. Row i+1's words
   are judged one step later.
3. Every vertical run that ends (white in row i, black in w) must pass the
   end-of-word test of Fact B.
4. Every continuing or new vertical run is extended by one letter, checked iff
   the new cell has a white horizontal neighbour in w (Fact A), with the
   during-growth tests of Fact B.
5. Components: cells of w merge with horizontal neighbours and with the
   components above them. A component of row i with no white cell below it in
   w would be sealed off from everything later, so the state is rejected; this
   is exact for a single connected region (a sealed component plus anything
   placed later gives ≥ 2 regions; and its 180° image gives another copy).
6. Row 0 must contain a white cell (Rule 4, top row).

## 4. Using the 180° symmetry: build half, glue at the center

With h = (n−1)/2, a 180°-symmetric grid is determined by rows 0…h−1 and the
center row h. Row n−1−i is row i reversed, and the center row must equal its
own reversal (a palindrome). So the DP builds only rows 0…h−1, then, for each
frontier state s and each palindromic allowed center row c, decides whether
the full grid is valid (`glue_ok`). The count is

  **Total = Σ over states s after row h−1 of N(s) · #{valid centers c for s}.**

Why the glue check is complete:

* **Words entirely above the center** were validated by the DP. **Words
  entirely below** are 180° images of those, and rotation maps words to words
  and checked letters to checked letters while reversing letter order. Rules
  3 and 6–8 are invariant under reversal, so they hold too.
* **Row h−1's horizontal words** need their lower neighbour, the center row:
  checked at the glue. Row h+1's words are their images.
* **The center row's horizontal words:** a center cell in column j is checked
  iff row h−1 is white at j or row h+1 is white at j; row h+1 at j is row h−1 at
  n−1−j.
* **Vertical words through the center** in column j are: the open run of column
  j above (its summary), the center cell, and the run below. The part below is
  the rotation of column n−1−j's open run, read in reverse, so it is described
  by column n−1−j's summary with start and end swapped. `vrun_cross_ok`
  combines the two summaries and the center cell exactly as Fact B requires,
  including the Rule-8 conditions at both ends. A column whose top run cannot
  end (Fact B) forces the center cell white.
* **Connectivity:** the top components, the center cells and the bottom
  components (mirror images of the top ones) must form one connected graph.
  No component was sealed off inside the top half (rejected in §3), so this
  graph is the whole grid's white region.
* **Rule 4:** the top row has a white cell (§3), so the bottom row does too. The
  left column has a white cell iff column 0 had one above the center, the center
  row has one at column 0, or column n−1 had one above the center (its image is
  column 0 below). The right column gives the same condition by symmetry.

## 5. Merges and prunings that shrink the state space

Each of these replaces the state set by a smaller one with the same total.

**5.1 Left–right mirror merging** (`Ctx::pack_canon`). Let R reflect a partial
grid left to right. R maps valid grids to valid grids: the rules are invariant
under reflection, and R commutes with the 180° rotation. On frontier states R
reverses the columns and renumbers labels. Every transition commutes with R:
the successors of R(s) are the reflections of the successors of s, with the
same multiplicities. The center rows are palindromes (R(c) = c), so the glue
count of R(s) equals that of s. Hence the number of completions F satisfies
F(R(s)) = F(s), and replacing each state by the lexicographically smaller of
its packing and its mirror's packing, summing their counts, leaves
Σ N(s)·F(s) unchanged. Measured effect: 13×13 row 3 drops from 49M to 24.5M
states.

**5.2 Dead-run pruning** (`feasible`). A vertical run whose summary cannot be
ended validly within the cells left in its column (allowing every possible
sequence of checked/unchecked letters, a superset of what can really happen)
has no valid completion. Dropping it removes only states with F = 0.

**5.3 Merging summaries with identical futures** (`Ctx::build_canon`). Two
summaries are equivalent at row r if everything the DP will ever ask of them
from row r on gives the same answer: white or black, `len` ≥ 2 (used by the
current row's horizontal words), whether the run may end here, the
equivalence classes of their checked and unchecked extensions at row r+1, and
on the last top-half row all glue outcomes in both the upper and the mirrored
role. This is computed exactly, bottom-up from row h−1, by partition
refinement (a bounded-horizon bisimulation). Equivalent summaries trigger
identical decisions in every future step, so storing each new cell's summary
as its class representative cannot change any count. One merge occurs before
the last row: (len ≥ 3, trail 0, d 2) ~ (len 2, trail 0, d 2). It removes about
10% of states (13×13 row 4: 191M → 171M).

## 6. Fast paths and the reference implementations

The program keeps straightforward reference versions of the two core
operations and replaces them in the hot loop with faster equivalents:

| reference | fast path | what changes |
|---|---|---|
| `step` (all of §3, with a union-find) | `Prefilter::pass` + `Prefilter::step_fast` | per-state bitmasks decide every rejection in §3; components merged as bitmasks; only rows containing every forced-white column are enumerated |
| `glue_ok` (all of §4, union-find) | `GluePre::ok` / `glue_count` | per-column crossing outcomes from a 64 KB table; bitwise filtering of all center rows before the connectivity check |

The unit test `fast_step_matches_reference` runs the full DP for n = 5, 7, 9
and checks, for **every** frontier state and every candidate row, that the
fast path produces exactly the reference successors (after §5.3's
representatives). It also checks, for every last-row state and every center
row, that the fast glue agrees with `glue_ok` and that `glue_count` equals the
reference count.

## 7. Storage, ordering and determinism

Counts are combined only by addition, so the result does not depend on the
order in which partial grids are generated or merged. The program uses this
freely:

* **Sharding.** States are split into 1024 shards by a hash of their packed
  form; each shard is merged independently.
* **Disk mode** (`src/british/disk.rs`). Each finished row is stored as 1024
  sorted shard files. A row is built by streaming input shards through hash
  maps that, when a memory budget is exceeded, merge their contents into each
  shard's single sorted run file. At the end of the row, the run files become
  the row's shard files. Records use a lossless 7-bit-per-column encoding
  (5-bit summary code and a 2-bit non-crossing component code) plus a
  variable-length count; a test checks that every 9×9 frontier round-trips
  exactly.
* **The last row is never stored.** Each row-(h−1) successor is glued as soon
  as it is generated (`advance_glue_part`). The partial sum for each input
  shard is saved, and the total is their sum.
* **Checkpoints.** Interrupted runs resume from the last checkpoint; spills are
  write → mark → rename, so a crash leaves either the old or the new state.
  Tests inject crashes at six points and require the exact count after
  resuming.

**Integer widths.** Per-state counts are 64-bit; partial sums and the total are
128-bit. Rust release builds do **not** trap on overflow by default. The
largest per-state count is far below 2⁶⁴ (≈1.8·10¹⁹): the whole 13×13 total is
1.6·10¹¹, and 15×15 totals are projected around 10¹⁴–10¹⁶. To make this a
checked fact rather than an estimate, the verification run (§8) is built with
`overflow-checks = true`, so any overflow aborts it.

## 8. Validation

| check | what it establishes |
|---|---|
| Brute-force enumeration of every symmetric grid (British n = 5, 7) against the folded DP | the DP, the fold and the glue are exact on every grid at small sizes; the brute counts also equal Keith's |
| Keith's published `#Total` for 5, 7, 9, 11, 13 (17; 650; 68,956; 60,384,181; 162,468,835,136) | the full pipeline reproduces independent results up to 13×13 |
| American counterparts: brute force (n ≤ 9), A323839 through 15×15, and a non-symmetric DP against non-symmetric brute force | the shared connectivity and folding machinery, independently of the British word rules |
| `fast_step_matches_reference` | fast paths ≡ reference, state by state (n ≤ 9) |
| `cell_matches_row` | an independent cell-by-cell transfer reproduces the row DP's state sets and counts exactly after every row (n ≤ 11) |
| `disk_mode_counts` | disk mode gives the right counts with constant spilling, and after injected crashes at every risky point |
| Passes / batch sizes / budgets | identical counts under different merge orders |
| 15×15 `verify` run | a second full run with a different memory budget and batch size (different merge order), built with overflow checks, must reproduce the total **and all 1024 per-shard partial sums** |

**What cannot be checked:** there is no published 15×15 British count to compare
against. The evidence for a new 15×15 number is: the same code is exact on
every smaller size where the answer is known; the size-independent equivalence
tests; and the reproducibility of the 15×15 result under a different merge
order. An independent re-implementation would be the next level of assurance.

## 9. Reproducing a run

See "Running 15×15" in the README and `scripts/run-15x15.sh`. Each finished
run is recorded under `results/` (commit, toolchain, machine, settings,
per-row state counts, timings, the per-shard partial sums and the
verification outcome); `results/TEMPLATE.md` lists what a record contains.
