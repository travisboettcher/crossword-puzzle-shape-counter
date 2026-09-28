//! British-style folded DP (M4).
//!
//! British grids share the connectivity + symmetry-folding machinery with the
//! American DP, but their word rules (Keith's Rules 6–8) are far more involved:
//! a cell is *checked* iff it is crossed by a perpendicular word, and each word
//! of length k must have exactly ⌈k/2⌉ checked cells (plus the consecutive-
//! unchecked constraints).
//!
//! We use Keith's observation that, because length-2 runs are forbidden outright
//! (horizontal runs by [`allowed_rows`], vertical runs at closure), a cell is
//! checked iff it merely has a white perpendicular neighbour. So:
//!   * **Horizontal** words of row `i` are validated one step late — when row
//!     `i+1` is placed we know both vertical neighbours of every cell in row `i`,
//!     and the full row pattern is known, so [`word_checks_ok`] applies directly.
//!   * **Vertical** words are validated at their closure. Rather than store each
//!     open run's full checked-bit pattern (which explodes the state count), we
//!     carry only its **minimal sufficient statistic**: the capped length, the
//!     running balance `d = #checked − #unchecked`, and the trailing count of
//!     consecutive unchecked cells. Rules 7 (no 3 unchecked) and the Rule-8 start
//!     constraint are enforced during growth; Rule 6 (`d ∈ {0,1}`) and the Rule-8
//!     end constraint (`trailing < 2`) are checked at closure. This is exact —
//!     two runs with the same statistic have identical future validity — and
//!     collapses the state space enough for 11×11 and 13×13 to fit in memory.
//!
//! ## Frontier state (per column, packed into a u16)
//!   * `len`   (bits 0..2): capped run length (0 = black, else 1, 2, or 3=">=3").
//!   * `trail` (bits 2..4): trailing consecutive-unchecked count (0, 1, 2).
//!   * `d`     (bits 4..8): `#checked − #unchecked`, stored as `d + DBIAS`.
//!   * `label` (bits 8..12): connectivity id, canonicalized.
//! Plus a flag u16 (`e0 | eN<<1`) for the Rule-4 edge columns.
//!
//! ## Scaling to 13×13
//! Stored frontiers are packed to 16 bytes and merged with their left–right
//! mirror ([`Ctx::pack_canon`]); runs that can no longer be
//! completed are pruned ([`feasible`]); most candidate rows are rejected by
//! bitmask tests ([`Prefilter`]), which then build successors with bitmask
//! component merging ([`Prefilter::step_fast`]); successors are merged into one
//! sharded concurrent map; and the last row is glued to the center as it is
//! generated ([`advance_glue`], with bitmask gluing in [`GluePre`]) instead of
//! being stored. The slower all-rules [`step`] and [`glue_ok`] are kept as
//! references, and a unit test checks the fast paths against them.
//! 13×13 peaks at ~24M / 191M / ~670M states in rows 3 / 4 / 5.

use std::sync::Mutex;

use ahash::AHashMap;
use rayon::prelude::*;

use crate::rules::{word_checks_ok, Style};

mod cell;
mod disk;

const MAXN: usize = 15;
const SLOTS: usize = MAXN + 1;
const DBIAS: i8 = 4;

/// Working ("wide") frontier: one `pack_col` u16 per column plus the flag slot.
type Key = [u16; SLOTS];

/// Stored ("packed") frontier: 8 bits per column — a 5-bit code for the
/// `(len, trail, d)` statistic (0 = black) and a 3-bit `label − 1` (at most
/// ⌈15/2⌉ = 8 components fit in a row) — with the two edge flags in bits
/// 120..122. 16 bytes instead of 32, so a map entry is 24 bytes instead of 48.
type Packed = [u64; 2];
type Map = AHashMap<Packed, u64>;

const STAT_BITS: u32 = 5;
const MAX_CODES: usize = 1 << STAT_BITS;
/// Output shards of the concurrent frontier map (see [`advance`]).
const SHARDS: usize = 1024;
/// Per-thread, per-shard buffer length before a flush into the shared shard.
const FLUSH: usize = 256;

/// Per-`n` tables: the statistic code book and the dead-run prune.
struct Ctx {
    n: usize,
    /// `(stat & 0xff)` → 5-bit code (0 = unreachable/black).
    code: [u8; 256],
    /// code → `stat` (the low 8 bits of a `pack_col` value).
    stat: [u16; MAX_CODES],
    /// `feas[len][trail][d + DBIAS][rem]`: can a vertical run with this
    /// statistic still be completed validly using at most `rem` more cells?
    feas: Vec<bool>,
    /// Bitset over `(mask << n) | checked`: are all horizontal words of the row
    /// with white mask `mask` valid, given which of its cells are checked?
    /// Replaces a per-word allocation in the innermost loop.
    row_ok: Vec<u64>,
    /// `glue_tab[(top & 0xff) << 8 | (mirror & 0xff)]`, indexed by the run
    /// statistics of a top column and its mirror column: bit 0 = the crossing
    /// vertical word fails with a checked center cell, bit 1 = fails with an
    /// unchecked one, bit 2 = the top run cannot close (center must be white).
    glue_tab: Vec<u8>,
    /// `canon[row][stat & 0xff]`: a representative of the run statistics that
    /// behave identically from grid row `row` on (see [`Ctx::build_canon`]).
    canon: Vec<[u8; 256]>,
}

const FEAS_R: usize = MAXN + 1;

#[inline]
fn feas_idx(len: u8, trail: u8, d: i8, rem: usize) -> usize {
    (((len as usize) * 3 + trail as usize) * 16 + (d + DBIAS) as usize) * FEAS_R + rem
}

/// Whether the run statistic `(len, trail, d)` has *any* completion of at most
/// `rem` cells (each checked or unchecked, subject to the growth rules) that
/// closes validly. This is a superset of the real futures, so pruning states
/// for which it is false removes only dead states and the count is unchanged.
fn feasible(len: u8, trail: u8, d: i8, rem: usize) -> bool {
    if vrun_close_ok(len, trail, d) {
        return true;
    }
    if rem == 0 {
        return false;
    }
    let nl = (len + 1).min(3);
    if (d + 1 + DBIAS) < 16 && feasible(nl, 0, d + 1, rem - 1) {
        return true;
    }
    let nt = trail + 1;
    nt < 3 && !(len == 1 && trail == 1) && d - 1 + DBIAS >= 0 && feasible(nl, nt, d - 1, rem - 1)
}

impl Ctx {
    fn new(n: usize) -> Ctx {
        let mut feas = vec![false; 4 * 3 * 16 * FEAS_R];
        for len in 1..=3u8 {
            for trail in 0..3u8 {
                for d in -DBIAS..(16 - DBIAS) {
                    for rem in 0..FEAS_R {
                        feas[feas_idx(len, trail, d, rem)] = feasible(len, trail, d, rem);
                    }
                }
            }
        }
        // Enumerate every statistic a top-half run can reach (grown by the same
        // rules as `step`, starting at row 0 for the most lenient prune) and give
        // each a code.
        let h = (n - 1) / 2;
        let mut seen = std::collections::BTreeSet::new();
        let mut frontier = vec![(1u8, 0u8, 1i8), (1u8, 1u8, -1i8)];
        for rows in 1..=h {
            let mut next = Vec::new();
            for &(l, t, d) in &frontier {
                if !feas[feas_idx(l, t, d, n - rows)] {
                    continue;
                }
                if seen.insert((l, t, d)) || rows < h {
                    let nl = (l + 1).min(3);
                    next.push((nl, 0, d + 1));
                    if t + 1 < 3 && !(l == 1 && t == 1) {
                        next.push((nl, t + 1, d - 1));
                    }
                }
            }
            next.sort();
            next.dedup();
            frontier = next;
        }
        assert!(
            seen.len() < MAX_CODES,
            "too many run statistics: {}",
            seen.len()
        );
        let mut code = [0u8; 256];
        let mut stat = [0u16; MAX_CODES];
        for (i, &(l, t, d)) in seen.iter().enumerate() {
            let s = pack_col(0, l, t, d);
            code[s as usize] = (i + 1) as u8;
            stat[i + 1] = s;
        }
        let rows = allowed_rows(n);
        let mut row_ok = vec![0u64; (1usize << (2 * n)) / 64];
        for &mask in &rows {
            // every subset `checked` of `mask`
            let mut sub = mask;
            loop {
                let mut cv = [false; MAXN];
                for (j, c) in cv.iter_mut().enumerate().take(n) {
                    *c = (sub >> j) & 1 == 1;
                }
                if hrow_ok(mask, n, &cv) {
                    let i = ((mask as usize) << n) | sub as usize;
                    row_ok[i / 64] |= 1 << (i % 64);
                }
                if sub == 0 {
                    break;
                }
                sub = (sub - 1) & mask;
            }
        }
        let mut glue_tab = vec![0u8; 1 << 16];
        for (i, e) in glue_tab.iter_mut().enumerate() {
            let (c, m) = ((i >> 8) as u16, (i & 0xff) as u16);
            let cross = |cc| {
                vrun_cross_ok(
                    col_len(c),
                    col_trail(c),
                    col_d(c),
                    col_len(m),
                    col_trail(m),
                    col_d(m),
                    cc,
                )
            };
            *e = (!cross(true)) as u8
                | ((!cross(false)) as u8) << 1
                | ((col_len(c) > 0 && !vrun_close_ok(col_len(c), col_trail(c), col_d(c))) as u8)
                    << 2;
        }
        let mut ctx = Ctx {
            n,
            code,
            stat,
            feas,
            row_ok,
            glue_tab,
            canon: Vec::new(),
        };
        ctx.canon = ctx.build_canon();
        ctx
    }

    /// Stats reachable by a run (low bytes of `pack_col`), including 0 = black.
    fn stat_list(&self) -> Vec<u16> {
        let mut v = vec![0u16];
        v.extend((1..MAX_CODES).map(|k| self.stat[k]).filter(|&s| s != 0));
        v
    }

    /// Extend run statistic `s` (placed on grid row `row`) by one white cell on
    /// row `row + 1`, checked or not; `None` if that is illegal or the run could
    /// no longer finish. Mirrors [`Prefilter::new`] exactly.
    fn extend(&self, s: u16, checked: bool, row: usize) -> Option<u16> {
        let rem = self.n - 1 - (row + 1);
        let (l, t, d) = (col_len(s), col_trail(s), col_d(s));
        let (nl, nt, nd) = if l == 0 {
            if checked {
                (1, 0, 1)
            } else {
                (1, 1, -1)
            }
        } else if checked {
            ((l + 1).min(3), 0, d + 1)
        } else {
            if t + 1 >= 3 || (l == 1 && t == 1) || d - 1 + DBIAS < 0 {
                return None;
            }
            ((l + 1).min(3), t + 1, d - 1)
        };
        if d + 1 + DBIAS >= 16 || !self.alive(nl, nt, nd, rem) {
            return None;
        }
        Some(pack_col(0, nl, nt, nd))
    }

    /// Bounded-horizon bisimulation over run statistics. Two stats at row `r`
    /// are equivalent iff they agree on everything the DP ever asks of them
    /// from row `r` on: white/black, "checked from above" (len ≥ 2, used by the
    /// row's horizontal words), whether the run may close, and — recursively —
    /// the classes of their checked / unchecked extensions; at the last
    /// top-half row, their full glue behaviour in both the top and the mirror
    /// role. Replacing a stat by its class representative therefore never
    /// changes a count, but lets more frontiers merge.
    fn build_canon(&self) -> Vec<[u8; 256]> {
        let h = (self.n - 1) / 2;
        let stats = self.stat_list();
        let mut canon = vec![[0u8; 256]; h];
        let mut class: Vec<AHashMap<u16, usize>> = vec![AHashMap::new(); h];
        for r in (0..h).rev() {
            let mut ids: AHashMap<Vec<u32>, (usize, u16)> = AHashMap::new();
            for &s in &stats {
                let l = col_len(s);
                let mut key = vec![
                    (l > 0) as u32,
                    (l >= 2) as u32,
                    (l > 0 && !vrun_close_ok(l, col_trail(s), col_d(s))) as u32,
                ];
                if r + 1 == h {
                    for &m in &stats {
                        key.push(self.glue_tab[(s as usize) << 8 | m as usize] as u32);
                        key.push(self.glue_tab[(m as usize) << 8 | s as usize] as u32);
                    }
                } else {
                    for checked in [true, false] {
                        key.push(match self.extend(s, checked, r) {
                            // a stat outside the code book only arises from a
                            // stat that cannot occur on this row; keep it apart
                            Some(x) => class[r + 1].get(&x).map_or(u32::MAX, |&c| 1 + c as u32),
                            None => 0,
                        });
                    }
                }
                let next = ids.len();
                let (id, rep) = *ids.entry(key).or_insert((next, s));
                class[r].insert(s, id);
                canon[r][s as usize] = rep as u8;
            }
            if std::env::var("CANON_STATS").is_ok() {
                for &x in &stats {
                    if canon[r][x as usize] as u16 != x {
                        eprintln!(
                            "    row {r}: (len {}, trail {}, d {}) ~ (len {}, trail {}, d {})",
                            col_len(x),
                            col_trail(x),
                            col_d(x),
                            col_len(canon[r][x as usize] as u16),
                            col_trail(canon[r][x as usize] as u16),
                            col_d(canon[r][x as usize] as u16)
                        );
                    }
                }
                eprintln!(
                    "  canon row {r}: {} stats -> {} classes",
                    stats.len(),
                    ids.len()
                );
            }
        }
        canon
    }

    /// Table lookup for [`hrow_ok`]; `mask` must be an allowed row.
    #[inline]
    fn row_valid(&self, mask: u32, checked: u32) -> bool {
        let i = ((mask as usize) << self.n) | (checked & mask) as usize;
        (self.row_ok[i / 64] >> (i % 64)) & 1 == 1
    }

    #[inline]
    fn alive(&self, len: u8, trail: u8, d: i8, rem: usize) -> bool {
        self.feas[feas_idx(len, trail, d, rem)]
    }

    /// The smaller packing of `w` and its left–right mirror. Mirroring a
    /// partial grid preserves validity and maps successors to successors, and
    /// the (palindromic) center rows glue to a state and its mirror equally
    /// often, so each mirror pair can be stored once with the summed count.
    #[inline]
    fn pack_canon(&self, w: &Key) -> Packed {
        // One sweep from the right packs `w` at column j and its mirror at
        // column n−1−j, relabelling the mirror by first appearance.
        let n = self.n;
        let (mut a, mut b) = (0u128, 0u128);
        let mut relabel = [0u8; MAXN + 1];
        let mut next = 0u8;
        for (i, &c) in w[..n].iter().rev().enumerate() {
            if col_len(c) > 0 {
                let code = self.code[(c & 0xff) as usize] as u128;
                let l = col_label(c) as usize;
                if relabel[l] == 0 {
                    next += 1;
                    relabel[l] = next;
                }
                a |= (code | (((l - 1) as u128) << STAT_BITS)) << (8 * (n - 1 - i));
                b |= (code | (((relabel[l] - 1) as u128) << STAT_BITS)) << (8 * i);
            }
        }
        let f = w[n] as u128;
        a |= f << 120;
        b |= (((f & 1) << 1) | ((f >> 1) & 1)) << 120;
        let k = a.min(b);
        [k as u64, (k >> 64) as u64]
    }

    #[inline]
    fn unpack(&self, p: &Packed) -> Key {
        let k = p[0] as u128 | ((p[1] as u128) << 64);
        let mut w = [0u16; SLOTS];
        for (j, c) in w.iter_mut().enumerate().take(self.n) {
            let b = (k >> (8 * j)) as u8;
            let code = (b as usize) & (MAX_CODES - 1);
            if code != 0 {
                *c = self.stat[code] | ((((b >> STAT_BITS) + 1) as u16) << 8);
            }
        }
        w[self.n] = ((k >> 120) & 0b11) as u16;
        w
    }
}

#[inline]
fn shard_of(p: &Packed) -> usize {
    let x = (p[0] ^ p[1].rotate_left(29)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (x >> 54) as usize % SHARDS
}

#[inline]
fn pack_col(label: u8, len: u8, trail: u8, d: i8) -> u16 {
    (len as u16) | ((trail as u16) << 2) | (((d + DBIAS) as u16) << 4) | ((label as u16) << 8)
}
#[inline]
fn col_len(c: u16) -> u8 {
    (c & 0b11) as u8
}
#[inline]
fn col_trail(c: u16) -> u8 {
    ((c >> 2) & 0b11) as u8
}
#[inline]
fn col_d(c: u16) -> i8 {
    (((c >> 4) & 0xf) as i8) - DBIAS
}
#[inline]
fn col_label(c: u16) -> u8 {
    ((c >> 8) & 0xf) as u8
}

/// Validate a completed **vertical** run from its sufficient statistic.
/// `len` is capped (1, 2, 3=">=3"), `trail` the trailing-unchecked count, `d`
/// the checked−unchecked balance. (Rules 7 and the Rule-8 start are enforced
/// during growth.)
#[inline]
fn vrun_close_ok(len: u8, trail: u8, d: i8) -> bool {
    match len {
        0 => true,
        1 => d == 1,                          // a lone cell must be checked
        2 => false,                           // length-2 word forbidden
        _ => (d == 0 || d == 1) && trail < 2, // Rule 6 + Rule-8 end
    }
}

/// Validate a **vertical** word that crosses the center: top run `(lj,tj,dj)`,
/// mirrored bottom run `(lm,tm,dm)`, and the center cell (checked = `cc`).
#[allow(clippy::too_many_arguments)]
fn vrun_cross_ok(lj: u8, tj: u8, dj: i8, lm: u8, tm: u8, dm: i8, cc: bool) -> bool {
    // Empty sides contribute nothing.
    let (tj, dj) = if lj > 0 { (tj, dj) } else { (0, 0) };
    let (tm, dm) = if lm > 0 { (tm, dm) } else { (0, 0) };

    if lj == 0 && lm == 0 {
        return cc; // word is the single center cell; it must be checked
    }
    if lj + lm == 1 {
        return false; // total length 2
    }

    // total length >= 3
    let d_total = dj as i32 + dm as i32 + if cc { 1 } else { -1 };
    if d_total != 0 && d_total != 1 {
        return false; // Rule 6
    }
    // Rule 7 across the center (only an issue when the center is unchecked).
    if !cc && (tj + 1 + tm) >= 3 {
        return false;
    }
    // Rule 8 start (top of the word).
    if lj == 0 {
        if !cc && tm >= 1 {
            return false; // center + first bottom cell both unchecked
        }
    } else if lj == 1 && dj == -1 && !cc {
        return false; // lone unchecked top cell + unchecked center
    }
    // Rule 8 end (bottom of the word).
    if lm == 0 {
        if !cc && tj >= 1 {
            return false;
        }
    } else if lm == 1 && dm == -1 && !cc {
        return false;
    }
    true
}

/// Validate a completed **horizontal** word from its full checked pattern.
#[inline]
fn hword_ok(pat: &[bool]) -> bool {
    match pat.len() {
        0 => true,
        1 => pat[0],
        2 => false,
        _ => word_checks_ok(Style::British, pat),
    }
}

/// Rows (white masks) with no length-2 horizontal run (Rule 3, British).
fn allowed_rows(n: usize) -> Vec<u32> {
    let full: u32 = (1u32 << n) - 1;
    (0..=full)
        .filter(|&m| {
            let mut j = 0;
            while j < n {
                if (m >> j) & 1 == 1 {
                    let s = j;
                    while j < n && (m >> j) & 1 == 1 {
                        j += 1;
                    }
                    if j - s == 2 {
                        return false;
                    }
                } else {
                    j += 1;
                }
            }
            true
        })
        .collect()
}

// --- union-find on a stack array ------------------------------------------

#[inline]
fn find(parent: &mut [usize; MAXN], x: usize) -> usize {
    let mut r = x;
    while parent[r] != r {
        r = parent[r];
    }
    let mut c = x;
    while parent[c] != r {
        let nxt = parent[c];
        parent[c] = r;
        c = nxt;
    }
    r
}
#[inline]
fn union(parent: &mut [usize; MAXN], a: usize, b: usize) {
    let (ra, rb) = (find(parent, a), find(parent, b));
    if ra != rb {
        parent[ra] = rb;
    }
}

/// Validate the horizontal words of a row given its white mask and, per column,
/// whether that cell is checked (has a white vertical neighbour).
fn hrow_ok(mask: u32, n: usize, checked_v: &[bool; MAXN]) -> bool {
    let mut j = 0;
    while j < n {
        if (mask >> j) & 1 == 1 {
            let s = j;
            while j < n && (mask >> j) & 1 == 1 {
                j += 1;
            }
            let pat: Vec<bool> = (s..j).map(|c| checked_v[c]).collect();
            if !hword_ok(&pat) {
                return false;
            }
        } else {
            j += 1;
        }
    }
    true
}

/// Reference transition: place row `w` (grid row `row`) onto frontier `key`
/// (row `row − 1`), checking every rule. The hot path uses [`Prefilter::pass`]
/// + [`step_fast`]; a unit test checks the two agree on every input.
#[cfg_attr(not(test), allow(dead_code))]
fn step(ctx: &Ctx, key: &Key, w: u32, row: usize) -> Option<Key> {
    let n = ctx.n;
    let rem = n - 1 - row; // cells left below this row in each column
    let old = key;
    let white = |j: usize| (w >> j) & 1 == 1;

    // Validate row i's horizontal words (its lower neighbour is now known).
    let mut row_i_mask: u32 = 0;
    let mut long_runs: u32 = 0;
    for (j, &c) in old.iter().enumerate().take(n) {
        if col_len(c) > 0 {
            row_i_mask |= 1 << j;
            if col_len(c) >= 2 {
                long_runs |= 1 << j;
            }
        }
    }
    if !ctx.row_valid(row_i_mask, long_runs | w) {
        return None;
    }

    // Vertical runs that close now (white above, black below).
    for j in 0..n {
        if col_len(old[j]) > 0
            && !white(j)
            && !vrun_close_ok(col_len(old[j]), col_trail(old[j]), col_d(old[j]))
        {
            return None;
        }
    }

    // Connectivity: union-find over the new row's white columns.
    let mut parent = [0usize; MAXN];
    for (j, p) in parent.iter_mut().enumerate().take(n) {
        *p = j;
    }
    for j in 0..n.saturating_sub(1) {
        if white(j) && white(j + 1) {
            union(&mut parent, j, j + 1);
        }
    }
    let mut first_below = [usize::MAX; MAXN];
    for j in 0..n {
        if white(j) && col_len(old[j]) > 0 {
            let c = col_label(old[j]) as usize;
            if first_below[c] == usize::MAX {
                first_below[c] = j;
            } else {
                union(&mut parent, first_below[c], j);
            }
        }
    }
    for j in 0..n {
        if col_len(old[j]) > 0 && first_below[col_label(old[j]) as usize] == usize::MAX {
            return None; // component sealed off mid-grid
        }
    }

    // Build the new frontier, updating each column's vertical-run statistic.
    let mut out = [0u16; SLOTS];
    let mut root_label = [0u8; MAXN];
    let mut next_label = 1u8;
    for j in 0..n {
        if white(j) {
            let r = find(&mut parent, j);
            if root_label[r] == 0 {
                root_label[r] = next_label;
                next_label += 1;
            }
            let label = root_label[r];
            // checked bit of this new cell = has a horizontal neighbour
            let ch = (j > 0 && white(j - 1)) || (j + 1 < n && white(j + 1));
            let (new_len, new_trail, new_d) = if col_len(old[j]) == 0 {
                // fresh run
                (1u8, if ch { 0 } else { 1 }, if ch { 1 } else { -1 })
            } else {
                let (ol, ot, od) = (col_len(old[j]), col_trail(old[j]), col_d(old[j]));
                if ch {
                    ((ol + 1).min(3), 0, od + 1)
                } else {
                    let nt = ot + 1;
                    if nt >= 3 {
                        return None; // Rule 7: three consecutive unchecked
                    }
                    if ol == 1 && ot == 1 {
                        return None; // Rule 8 start: unchecked pair at word start
                    }
                    ((ol + 1).min(3), nt, od - 1)
                }
            };
            if !ctx.alive(new_len, new_trail, new_d, rem) {
                return None; // this vertical word can no longer be completed
            }
            out[j] = pack_col(label, new_len, new_trail, new_d);
        }
    }
    let mut flags = old[n];
    if white(0) {
        flags |= 1;
    }
    if white(n - 1) {
        flags |= 2;
    }
    out[n] = flags;
    Some(out)
}

/// Columns forced to stay white: any open run that cannot legally close here
/// (a length-2 run, a lone unchecked cell, a longer run failing Rule 6/8).
#[inline]
fn forced_cols(st: &Key, n: usize) -> u32 {
    let mut forced = 0u32;
    for (j, &c) in st.iter().enumerate().take(n) {
        if col_len(c) > 0 && !vrun_close_ok(col_len(c), col_trail(c), col_d(c)) {
            forced |= 1 << j;
        }
    }
    forced
}

/// Per-state bitmasks that reject most candidate rows before [`step`] runs its
/// union-find. Every test here is one [`step`] also makes, so this is purely a
/// fast path and never changes which successors exist.
struct Prefilter {
    /// white cells of the frontier row (the row whose words are now validated)
    row_mask: u32,
    /// frontier cells already checked by the run above them (len ≥ 2)
    long_runs: u32,
    /// columns whose run dies if the new cell is checked / unchecked
    dead_checked: u32,
    dead_unchecked: u32,
    /// per component label, the frontier columns carrying it
    comps: [u32; MAXN],
    ncomps: usize,
    /// per column, the new cell's run statistic (`pack_col` low byte) when it
    /// is checked / unchecked (only meaningful when not dead)
    next_checked: [u16; MAXN],
    next_unchecked: [u16; MAXN],
    flags: u16,
}

impl Prefilter {
    fn new(ctx: &Ctx, st: &Key, row: usize) -> Prefilter {
        let n = ctx.n;
        let rem = n - 1 - row;
        let mut p = Prefilter {
            row_mask: 0,
            long_runs: 0,
            dead_checked: 0,
            dead_unchecked: 0,
            comps: [0; MAXN],
            ncomps: 0,
            next_checked: [0; MAXN],
            next_unchecked: [0; MAXN],
            flags: st[n],
        };
        for (j, &c) in st.iter().enumerate().take(n) {
            let (l, t, d) = (col_len(c), col_trail(c), col_d(c));
            let (alive_c, alive_u) = if l == 0 {
                p.next_checked[j] = pack_col(0, 1, 0, 1);
                p.next_unchecked[j] = pack_col(0, 1, 1, -1);
                (ctx.alive(1, 0, 1, rem), ctx.alive(1, 1, -1, rem))
            } else {
                p.row_mask |= 1 << j;
                if l >= 2 {
                    p.long_runs |= 1 << j;
                }
                let lab = col_label(c) as usize;
                p.comps[lab - 1] |= 1 << j;
                p.ncomps = p.ncomps.max(lab);
                let nl = (l + 1).min(3);
                p.next_checked[j] = pack_col(0, nl, 0, d + 1);
                if t < 2 && d > -DBIAS {
                    p.next_unchecked[j] = pack_col(0, nl, t + 1, d - 1);
                }
                (
                    ctx.alive(nl, 0, d + 1, rem),
                    t + 1 < 3 && !(l == 1 && t == 1) && ctx.alive(nl, t + 1, d - 1, rem),
                )
            };
            if !alive_c {
                p.dead_checked |= 1 << j;
            }
            if !alive_u {
                p.dead_unchecked |= 1 << j;
            }
            // store each new run statistic as its equivalence-class representative
            let cr = &ctx.canon[row];
            p.next_checked[j] = cr[p.next_checked[j] as usize] as u16;
            p.next_unchecked[j] = cr[p.next_unchecked[j] as usize] as u16;
        }
        p
    }

    #[inline]
    fn pass(&self, ctx: &Ctx, w: u32, full: u32) -> bool {
        let nb = ((w << 1) | (w >> 1)) & full;
        if w & nb & self.dead_checked != 0 || w & !nb & self.dead_unchecked != 0 {
            return false;
        }
        if !ctx.row_valid(self.row_mask, self.long_runs | w) {
            return false;
        }
        self.comps[..self.ncomps].iter().all(|&m| w & m != 0)
    }

    /// The successor for a row `w` that passed [`Prefilter::pass`] (and was
    /// drawn from the forced-column supersets): every rule is already
    /// satisfied, so only the new frontier is built. Components are merged as
    /// bitmasks: each horizontal segment of `w` starts as its own group, and
    /// every old component fuses the groups it touches.
    #[inline]
    fn step_fast(&self, w: u32, full: u32) -> Key {
        let mut groups = [0u32; MAXN];
        let mut ng = segments(w, &mut groups);
        for &m in &self.comps[..self.ncomps] {
            ng = fuse(&mut groups, ng, m & w);
        }
        // canonical labels: order groups by their lowest column
        groups[..ng].sort_unstable_by_key(|g| g.trailing_zeros());
        let nb = ((w << 1) | (w >> 1)) & full;
        let mut out = [0u16; SLOTS];
        for (li, &g) in groups[..ng].iter().enumerate() {
            let label = ((li + 1) as u16) << 8;
            let mut b = g;
            while b != 0 {
                let j = b.trailing_zeros() as usize;
                b &= b - 1;
                let stat = if nb >> j & 1 == 1 {
                    self.next_checked[j]
                } else {
                    self.next_unchecked[j]
                };
                out[j] = stat | label;
            }
        }
        let n = full.count_ones() as usize;
        out[n] = self.flags | (w & 1) as u16 | (((w >> (n - 1)) & 1) << 1) as u16;
        out
    }
}

fn allowed_set(n: usize, rows: &[u32]) -> Vec<bool> {
    let mut allowed = vec![false; 1 << n];
    for &w in rows {
        allowed[w as usize] = true;
    }
    allowed
}

/// Call `f` with every valid successor of `st` when grid row `row` is placed.
#[inline]
fn successors(ctx: &Ctx, st: &Key, row: usize, allowed: &[bool], mut f: impl FnMut(Key)) {
    let n = ctx.n;
    let full: u32 = (1u32 << n) - 1;
    let forced = forced_cols(st, n);
    let pre = Prefilter::new(ctx, st, row);
    // Visit only the rows containing every forced column: walk the submasks of
    // the free columns, filtered by the allowed-row set.
    let free = full & !forced;
    let mut sub = free;
    loop {
        let w = forced | sub;
        if allowed[w as usize] && !(row == 0 && w == 0) && pre.pass(ctx, w, full) {
            f(pre.step_fast(w, full));
        }
        if sub == 0 {
            break;
        }
        sub = (sub - 1) & free;
    }
}

/// Palindromic allowed rows: 45 / 84 / 157 for n = 11 / 13 / 15.
const MAX_CENTERS: usize = 256;

/// Number of center rows that complete the last top-half frontier `st` into a
/// valid grid. A column whose run cannot close must stay white in the center.
#[inline]
fn glue_count(ctx: &Ctx, st: &Key, centers: &[u32]) -> u64 {
    let g = GluePre::new(ctx, st);
    // Stage 1: the pure-bitwise tests, branch-free over all centers at once so
    // the compiler can vectorize them. Stage 2 runs only on the survivors.
    let edge = (!g.edge_seen) as u32;
    let mut bad = [0u32; MAX_CENTERS];
    let bad = &mut bad[..centers.len()];
    for (b, &c) in bad.iter_mut().zip(centers) {
        let nb = ((c << 1) | (c >> 1)) & g.full;
        *b = ((c & g.forced) ^ g.forced)
            | (c & nb & g.bad_checked)
            | (c & !nb & g.bad_unchecked)
            | (edge & !c);
    }
    bad.iter()
        .zip(centers)
        .filter(|&(&b, &c)| b == 0 && g.ok_rest(ctx, c))
        .count() as u64
}

/// Place the last top-half row and glue each successor to the center directly,
/// without storing (or merging) that row. Trades merging for zero memory: glue
/// runs once per successor instead of once per distinct state.
fn advance_glue(
    ctx: &Ctx,
    input: &[Vec<(Packed, u64)>],
    rows: &[u32],
    row: usize,
    centers: &[u32],
) -> u128 {
    let allowed = allowed_set(ctx.n, rows);
    input
        .par_iter()
        .map(|part| advance_glue_part(ctx, part, row, &allowed, centers))
        .sum()
}

/// [`advance_glue`] for one input part.
fn advance_glue_part(
    ctx: &Ctx,
    part: &[(Packed, u64)],
    row: usize,
    allowed: &[bool],
    centers: &[u32],
) -> u128 {
    let mut local = 0u128;
    for (p, cnt) in part {
        successors(ctx, &ctx.unpack(p), row, allowed, |ns| {
            local += *cnt as u128 * glue_count(ctx, &ns, centers) as u128;
        });
    }
    local
}

/// Split `w` into its maximal runs of ones; returns how many.
#[inline]
fn segments(w: u32, groups: &mut [u32; MAXN]) -> usize {
    let mut ng = 0;
    let mut rest = w;
    while rest != 0 {
        let lo = rest & rest.wrapping_neg();
        let above = (rest + lo) & !rest; // first zero above the run at `lo`
        let seg = above.wrapping_sub(lo) & rest;
        groups[ng] = seg;
        ng += 1;
        rest &= !seg;
    }
    ng
}

/// Merge every group intersecting `touch` into one; returns the new count.
#[inline]
fn fuse(groups: &mut [u32; MAXN], ng: usize, touch: u32) -> usize {
    let mut merged = 0u32;
    let mut k = 0;
    for i in 0..ng {
        let g = groups[i];
        if g & touch != 0 {
            merged |= g;
        } else {
            groups[k] = g;
            k += 1;
        }
    }
    if merged == 0 {
        return ng;
    }
    groups[k] = merged;
    k + 1
}

/// Per-state tables for gluing the last top-half frontier to a center row
/// and the mirrored bottom half; [`GluePre::ok`] agrees with [`glue_ok`] (see
/// the unit test) but works on bitmasks.
struct GluePre {
    n: usize,
    full: u32,
    /// columns whose top run cannot close (center must be white there)
    forced: u32,
    row_hm1: u32,
    long_runs: u32,
    /// row h−1 | row h+1 (= reversed row h−1): center cells with a vertical neighbour
    vert_nb: u32,
    /// center columns whose crossing vertical word fails if the center cell
    /// is checked / unchecked
    bad_checked: u32,
    bad_unchecked: u32,
    edge_seen: bool,
    comps: [u32; MAXN],
    ncomps: usize,
}

impl GluePre {
    fn new(ctx: &Ctx, st: &Key) -> GluePre {
        let n = ctx.n;
        let mut g = GluePre {
            n,
            full: (1u32 << n) - 1,
            forced: 0,
            row_hm1: 0,
            long_runs: 0,
            vert_nb: 0,
            bad_checked: 0,
            bad_unchecked: 0,
            edge_seen: st[n] & 3 != 0,
            comps: [0; MAXN],
            ncomps: 0,
        };
        for (j, &c) in st.iter().enumerate().take(n) {
            if col_len(c) > 0 {
                g.row_hm1 |= 1 << j;
                if col_len(c) >= 2 {
                    g.long_runs |= 1 << j;
                }
                let lab = col_label(c) as usize;
                g.comps[lab - 1] |= 1 << j;
                g.ncomps = g.ncomps.max(lab);
            }
            let t =
                ctx.glue_tab[((c & 0xff) as usize) << 8 | (st[n - 1 - j] & 0xff) as usize] as u32;
            g.bad_checked |= (t & 1) << j;
            g.bad_unchecked |= ((t >> 1) & 1) << j;
            g.forced |= ((t >> 2) & 1) << j;
        }
        g.vert_nb = g.row_hm1 | (g.row_hm1.reverse_bits() >> (32 - n));
        g
    }

    /// All glue rules for one center row (the unit test's per-center check;
    /// [`glue_count`] splits it into a vectorizable stage and [`Self::ok_rest`]).
    #[cfg_attr(not(test), allow(dead_code))]
    #[inline]
    fn ok(&self, ctx: &Ctx, c: u32) -> bool {
        if c & self.forced != self.forced || !(self.edge_seen || c & 1 == 1) {
            return false;
        }
        let nb = ((c << 1) | (c >> 1)) & self.full;
        if c & nb & self.bad_checked != 0 || c & !nb & self.bad_unchecked != 0 {
            return false;
        }
        self.ok_rest(ctx, c)
    }

    /// The table-lookup and connectivity half of [`GluePre::ok`].
    #[inline]
    fn ok_rest(&self, ctx: &Ctx, c: u32) -> bool {
        let n = self.n;
        if !ctx.row_valid(self.row_hm1, self.long_runs | c) || !ctx.row_valid(c, self.vert_nb) {
            return false;
        }
        // Connectivity: center segments, fused by each top component and by
        // its mirror image below, must end as one group touching every component.
        let mut groups = [0u32; MAXN];
        let mut ng = segments(c, &mut groups);
        if ng == 0 || self.ncomps == 0 {
            return false;
        }
        for &m in &self.comps[..self.ncomps] {
            let t = m & c;
            if t == 0 {
                return false; // a top component never reaches the center
            }
            // the top component and its mirror image below are distinct
            // cells: each fuses only the center segments it touches
            ng = fuse(&mut groups, ng, t);
            ng = fuse(&mut groups, ng, (m.reverse_bits() >> (32 - n)) & c);
        }
        ng == 1
    }
}

/// Place grid row `row` on every frontier in `input`, merging equal successors.
///
/// Only successors whose shard `s` satisfies `keep(s)` are kept, so the final
/// row can be built in several passes when it does not fit in memory at once.
///
/// Successors go into `SHARDS` mutex-guarded maps (via small per-thread
/// buffers), so each distinct state is stored exactly once — unlike a rayon
/// fold/reduce, whose per-thread partial maps can hold several copies of the
/// output at the peak.
fn advance(
    ctx: &Ctx,
    input: &[Vec<(Packed, u64)>],
    rows: &[u32],
    row: usize,
    keep: &(dyn Fn(usize) -> bool + Sync),
) -> Vec<Mutex<Map>> {
    let allowed = allowed_set(ctx.n, rows);
    let shards: Vec<Mutex<Map>> = (0..SHARDS).map(|_| Mutex::new(Map::default())).collect();
    let flush = |buf: &mut Vec<(Packed, u64)>, s: usize| {
        let mut m = shards[s].lock().unwrap();
        for (k, v) in buf.drain(..) {
            *m.entry(k).or_insert(0) += v;
        }
    };
    input.par_iter().for_each_init(
        || vec![Vec::<(Packed, u64)>::new(); SHARDS],
        |bufs, part| {
            for (p, cnt) in part {
                successors(ctx, &ctx.unpack(p), row, &allowed, |ns| {
                    let pk = ctx.pack_canon(&ns);
                    let s = shard_of(&pk);
                    if keep(s) {
                        bufs[s].push((pk, *cnt));
                        if bufs[s].len() >= FLUSH {
                            flush(&mut bufs[s], s);
                        }
                    }
                });
            }
            for (s, b) in bufs.iter_mut().enumerate() {
                if !b.is_empty() {
                    flush(b, s);
                }
            }
        },
    );
    shards
}

/// Sort `v` by key and sum the counts of equal keys, in place.
fn sort_reduce(v: &mut Vec<(Packed, u64)>) {
    v.sort_unstable_by_key(|e| e.0);
    let mut w = 0usize;
    for r in 0..v.len() {
        if w > 0 && v[w - 1].0 == v[r].0 {
            v[w - 1].1 += v[r].1;
        } else {
            v[w] = v[r];
            w += 1;
        }
    }
    v.truncate(w);
}

/// Flatten shard maps into plain vectors (24 bytes per state, no table
/// overhead), freeing each map as soon as it is copied.
fn into_parts(shards: Vec<Mutex<Map>>) -> Vec<Vec<(Packed, u64)>> {
    shards
        .into_iter()
        .map(|m| m.into_inner().unwrap().into_iter().collect())
        .collect()
}

/// Count valid 180°-symmetric British n×n grids (Keith's #Total).
///
/// Knobs:
/// * `BRITISH_PASSES=0` (the default for n ≥ 13) glues each last-row successor
///   as it is generated, so that row is never stored or merged. It glues more
///   often (once per successor, not per distinct state) but needs no memory
///   for that row and no repeated passes. 13×13: ~12.5 min, 7 GB.
/// * `BRITISH_PASSES=k ≥ 1` (default 1 below 13) builds the last row in `k`
///   passes, each keeping 1/k of its shards and gluing them before moving on —
///   k× the work of that transfer step for 1/k of its peak memory.
/// * `BRITISH_ROW_PASSES=k` (default 1) splits the second-to-last row, whose
///   shards are flattened after each pass, so only 1/k of it is ever held as
///   hash maps at once.
pub fn count(n: usize, style: Style) -> u128 {
    assert_eq!(style, Style::British);
    let env = |k: &str, default: usize| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    // Merging the last row wins while it fits in memory; fusing wins beyond.
    let passes = env("BRITISH_PASSES", if n >= 13 { 0 } else { 1 });
    count_with_passes(n, env("BRITISH_ROW_PASSES", 1), passes)
}

/// [`count`] with explicit pass counts for the second-to-last (`row_passes`)
/// and last (`passes`, 0 = fused with the glue) top-half rows. The result
/// does not depend on them.
pub fn count_with_passes(n: usize, row_passes: usize, passes: usize) -> u128 {
    assert!(n >= 5 && n % 2 == 1 && n <= MAXN);
    // passes == 0: glue the last row as it is generated (never stored)
    let fused = passes == 0;
    let (row_passes, passes) = (row_passes.max(1), passes.max(1));
    let h = (n - 1) / 2;
    let rows = allowed_rows(n);
    let ctx = Ctx::new(n);

    let instrument = std::env::var("DP_STATS").is_ok();
    if let Ok(dir) = std::env::var("BRITISH_DISK_DIR") {
        let budget = std::env::var("BRITISH_DISK_BUDGET")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(50_000_000);
        return disk::count(&ctx, &rows, std::path::Path::new(&dir), budget, instrument);
    }
    let t0 = std::time::Instant::now();
    let total_len = |p: &[Vec<(Packed, u64)>]| p.iter().map(Vec::len).sum::<usize>();
    let mut parts: Vec<Vec<(Packed, u64)>> = vec![vec![([0, 0], 1)]];
    for i in 0..h - 1 {
        let k = if i + 2 == h { row_passes } else { 1 };
        let mut next: Vec<Vec<(Packed, u64)>> = Vec::new();
        for pass in 0..k {
            let keep = move |s: usize| s % k == pass;
            if k == 1 && std::env::var("BRITISH_CELL").is_ok() {
                let (out, widest) = cell::advance_cells(&ctx, &parts, i);
                if instrument {
                    eprintln!("    row {i}: widest mid-row layer {widest}");
                }
                next.extend(out);
            } else {
                next.extend(into_parts(advance(&ctx, &parts, &rows, i, &keep)));
            }
            if instrument && k > 1 {
                eprintln!("  row {i} pass {pass}/{k} ({:.2?})", t0.elapsed());
            }
        }
        parts = next;
        if instrument {
            eprintln!(
                "  row {i}: {} states ({:.2?})",
                total_len(&parts),
                t0.elapsed()
            );
        }
    }

    let centers: Vec<u32> = rows
        .iter()
        .copied()
        .filter(|&m| is_palindrome(m, n))
        .collect();
    if fused {
        let total = advance_glue(&ctx, &parts, &rows, h - 1, &centers);
        if instrument {
            eprintln!("  row {} fused with glue ({:.2?})", h - 1, t0.elapsed());
        }
        return total;
    }
    let mut total = 0u128;
    for pass in 0..passes {
        let keep = move |s: usize| s % passes == pass;
        let last = advance(&ctx, &parts, &rows, h - 1, &keep);
        if instrument {
            let states: usize = last.iter().map(|m| m.lock().unwrap().len()).sum();
            eprintln!(
                "  row {} pass {pass}/{passes}: {states} states ({:.2?})",
                h - 1,
                t0.elapsed()
            );
        }
        let got: u128 = last
            .into_par_iter()
            .map(|m| {
                let m = m.into_inner().unwrap();
                let mut local = 0u128;
                for (p, cnt) in &m {
                    let st = ctx.unpack(p);
                    local += *cnt as u128 * glue_count(&ctx, &st, &centers) as u128;
                }
                local
            })
            .sum();
        total += got;
        if instrument {
            eprintln!(
                "  row {} pass {pass}/{passes} glued ({:.2?})",
                h - 1,
                t0.elapsed()
            );
        }
    }
    total
}

fn is_palindrome(mask: u32, n: usize) -> bool {
    (0..n).all(|j| (mask >> j) & 1 == (mask >> (n - 1 - j)) & 1)
}

/// Glue the top-half frontier `key` to the palindromic center row `c` and the
/// mirrored bottom half, validating the center's words and connectivity.
/// Reference for [`GluePre::ok`].
#[cfg_attr(not(test), allow(dead_code))]
fn glue_ok(ctx: &Ctx, key: &Key, c: u32) -> bool {
    let n = ctx.n;
    let old = key;
    let cw = |j: usize| (c >> j) & 1 == 1;

    // Row h-1's horizontal words (its lower neighbour is the center row).
    let mut row_hm1: u32 = 0;
    let mut long_runs: u32 = 0;
    for (j, &cj) in old.iter().enumerate().take(n) {
        if col_len(cj) > 0 {
            row_hm1 |= 1 << j;
            if col_len(cj) >= 2 {
                long_runs |= 1 << j;
            }
        }
    }
    if !ctx.row_valid(row_hm1, long_runs | c) {
        return false;
    }

    // Center row's horizontal words. A center cell is checked iff it has a white
    // vertical neighbour: row h-1 (col j) or row h+1 = reverse(row h-1) (col n-1-j).
    // row h+1 is row h-1 reversed
    let rev_hm1 = row_hm1.reverse_bits() >> (32 - n);
    if !ctx.row_valid(c, row_hm1 | rev_hm1) {
        return false;
    }

    // Vertical words at the center.
    for j in 0..n {
        if cw(j) {
            let center_checked = (j > 0 && cw(j - 1)) || (j + 1 < n && cw(j + 1));
            let m = old[n - 1 - j];
            if !vrun_cross_ok(
                col_len(old[j]),
                col_trail(old[j]),
                col_d(old[j]),
                col_len(m),
                col_trail(m),
                col_d(m),
                center_checked,
            ) {
                return false;
            }
        } else if col_len(old[j]) > 0
            && !vrun_close_ok(col_len(old[j]), col_trail(old[j]), col_d(old[j]))
        {
            return false;
        }
    }

    // Rule 4 edge columns.
    let flags = old[n];
    if !((flags & 1 == 1) || (flags & 2 == 2) || cw(0)) {
        return false;
    }

    // Connectivity gluing (identical to the American case).
    let m = (0..n).map(|j| col_label(old[j])).max().unwrap_or(0) as usize;
    if m == 0 {
        return false;
    }
    let size = 2 * m + n;
    let mut parent = [0usize; MAXN * 3];
    for (i, p) in parent.iter_mut().enumerate().take(size) {
        *p = i;
    }
    let top_node = |k: u8| (k as usize) - 1;
    let bot_node = |k: u8| m + (k as usize) - 1;
    let cen_node = |j: usize| 2 * m + j;
    for j in 0..n {
        if cw(j) {
            if j + 1 < n && cw(j + 1) {
                union3(&mut parent, cen_node(j), cen_node(j + 1));
            }
            if col_len(old[j]) > 0 {
                union3(&mut parent, cen_node(j), top_node(col_label(old[j])));
            }
            if col_len(old[n - 1 - j]) > 0 {
                union3(
                    &mut parent,
                    cen_node(j),
                    bot_node(col_label(old[n - 1 - j])),
                );
            }
        }
    }
    let mut root: Option<usize> = None;
    let mut ok = |node: usize, parent: &mut [usize; MAXN * 3]| -> bool {
        let r = find3(parent, node);
        match root {
            None => {
                root = Some(r);
                true
            }
            Some(rr) => rr == r,
        }
    };
    for k in 1..=m as u8 {
        if !ok(top_node(k), &mut parent) || !ok(bot_node(k), &mut parent) {
            return false;
        }
    }
    for j in 0..n {
        if cw(j) && !ok(cen_node(j), &mut parent) {
            return false;
        }
    }
    true
}

#[inline]
fn find3(parent: &mut [usize; MAXN * 3], x: usize) -> usize {
    let mut r = x;
    while parent[r] != r {
        r = parent[r];
    }
    let mut c = x;
    while parent[c] != r {
        let nxt = parent[c];
        parent[c] = r;
        c = nxt;
    }
    r
}
#[inline]
fn union3(parent: &mut [usize; MAXN * 3], a: usize, b: usize) {
    let (ra, rb) = (find3(parent, a), find3(parent, b));
    if ra != rb {
        parent[ra] = rb;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fast path (forced-superset enumeration + prefilter + `step_fast`)
    /// must produce exactly the successors of the reference `step`, for every
    /// frontier reached in a full 9×9 run.
    #[test]
    fn fast_step_matches_reference() {
        for n in [5usize, 7, 9] {
            let ctx = Ctx::new(n);
            let rows = allowed_rows(n);
            let allowed = allowed_set(n, &rows);
            let mut frontier: Vec<Key> = vec![[0u16; SLOTS]];
            for row in 0..(n - 1) / 2 {
                let mut next = std::collections::BTreeSet::new();
                for st in &frontier {
                    let mut fast = Vec::new();
                    successors(&ctx, st, row, &allowed, |ns| fast.push(ns));
                    let mut slow: Vec<Key> = rows
                        .iter()
                        .filter(|&&w| !(row == 0 && w == 0))
                        .filter_map(|&w| step(&ctx, st, w, row))
                        .map(|mut k| {
                            // the fast path stores class representatives
                            for c in k.iter_mut().take(n) {
                                let rep = ctx.canon[row][(*c & 0xff) as usize] as u16;
                                *c = (*c & 0xff00) | rep;
                            }
                            k
                        })
                        .collect();
                    fast.sort();
                    slow.sort();
                    assert_eq!(fast, slow, "n={n} row={row} state={st:?}");
                    next.extend(slow);
                }
                frontier = next.into_iter().collect();
            }
            // ...cell-by-cell transfer must reproduce each row exactly...
            // (checked separately below in `cell_matches_row`)
            // ...the compact on-disk encoding must round-trip every frontier...
            for st in &frontier {
                let p = ctx.pack_canon(st);
                assert_eq!(
                    disk::expand(disk::compact(&p, n), n),
                    p,
                    "n={n} state={st:?}"
                );
            }
            // ...and fast gluing must agree with the reference on every
            // last-row frontier and every palindromic center row.
            let centers: Vec<u32> = rows
                .iter()
                .copied()
                .filter(|&m| is_palindrome(m, n))
                .collect();
            for st in &frontier {
                let g = GluePre::new(&ctx, st);
                let expect = centers.iter().filter(|&&c| glue_ok(&ctx, st, c)).count() as u64;
                assert_eq!(glue_count(&ctx, st, &centers), expect, "n={n} state={st:?}");
                for &c in &centers {
                    assert_eq!(
                        g.ok(&ctx, c),
                        glue_ok(&ctx, st, c),
                        "n={n} c={c:b} state={st:?}"
                    );
                }
            }
        }
    }

    /// Cell-by-cell and row-by-row transfer produce identical frontier sets
    /// and counts after every row.
    #[test]
    fn cell_matches_row() {
        for n in [5usize, 7, 9, 11] {
            let ctx = Ctx::new(n);
            let rows = allowed_rows(n);
            let mut parts: Vec<Vec<(Packed, u64)>> = vec![vec![([0, 0], 1)]];
            for i in 0..(n - 1) / 2 - 1 {
                let by_row: std::collections::BTreeMap<_, _> =
                    into_parts(advance(&ctx, &parts, &rows, i, &|_| true))
                        .into_iter()
                        .flatten()
                        .collect();
                let (cells, _) = cell::advance_cells(&ctx, &parts, i);
                let by_cell: std::collections::BTreeMap<_, _> =
                    cells.into_iter().flatten().collect();
                assert_eq!(by_row.len(), by_cell.len(), "n={n} row={i}");
                assert!(by_row == by_cell, "n={n} row={i}: state sets differ");
                parts = vec![by_row.into_iter().collect()];
            }
        }
    }
}
