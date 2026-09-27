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
//! Stored frontiers are packed to 16 bytes ([`Ctx::pack`]) and merged with
//! their left–right mirror ([`Ctx::pack_canon`]); runs that can no longer be
//! completed are pruned ([`feasible`]); most candidate rows are rejected by
//! bitmask tests ([`Prefilter`]) before the union-find; successors are merged
//! into one sharded concurrent map; and the last rows can be built in passes.
//! 13×13 peaks at ~24M / 191M / ~670M states in rows 3 / 4 / 5.

use std::sync::Mutex;

use ahash::AHashMap;
use rayon::prelude::*;

use crate::rules::{word_checks_ok, Style};

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
        Ctx {
            n,
            code,
            stat,
            feas,
            row_ok,
        }
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

    #[inline]
    fn pack(&self, w: &Key) -> Packed {
        let mut k = 0u128;
        for (j, &c) in w.iter().enumerate().take(self.n) {
            if col_len(c) > 0 {
                let code = self.code[(c & 0xff) as usize];
                debug_assert!(code != 0, "unreachable run statistic");
                let b = code as u128 | (((col_label(c) - 1) as u128) << STAT_BITS);
                k |= b << (8 * j);
            }
        }
        k |= (w[self.n] as u128) << 120;
        [k as u64, (k >> 64) as u64]
    }

    /// The smaller packing of `w` and its left–right mirror. Mirroring a
    /// partial grid preserves validity and maps successors to successors, and
    /// the (palindromic) center rows glue to a state and its mirror equally
    /// often, so each mirror pair can be stored once with the summed count.
    #[inline]
    fn pack_canon(&self, w: &Key) -> Packed {
        let n = self.n;
        let mut m = [0u16; SLOTS];
        let mut relabel = [0u8; MAXN + 1];
        let mut next = 1u8;
        for j in 0..n {
            let c = w[n - 1 - j];
            if col_len(c) > 0 {
                let l = col_label(c) as usize;
                if relabel[l] == 0 {
                    relabel[l] = next;
                    next += 1;
                }
                m[j] = (c & 0xff) | ((relabel[l] as u16) << 8);
            }
        }
        let f = w[n];
        m[n] = ((f & 1) << 1) | ((f >> 1) & 1);
        let (a, b) = (self.pack(w), self.pack(&m));
        if (a[1], a[0]) <= (b[1], b[0]) {
            a
        } else {
            b
        }
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

/// Place row `w` (grid row `row`) onto frontier `key` (row `row − 1`).
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
        };
        for (j, &c) in st.iter().enumerate().take(n) {
            let (l, t, d) = (col_len(c), col_trail(c), col_d(c));
            let (alive_c, alive_u) = if l == 0 {
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
    let n = ctx.n;
    let full: u32 = (1u32 << n) - 1;
    let mut allowed = vec![false; 1 << n];
    for &w in rows {
        allowed[w as usize] = true;
    }
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
                let st = ctx.unpack(p);
                let forced = forced_cols(&st, n);
                let pre = Prefilter::new(ctx, &st, row);
                // Visit only the rows containing every forced column: walk the
                // submasks of the free columns, filtered by the allowed-row set.
                let free = full & !forced;
                let mut sub = free;
                loop {
                    let w = forced | sub;
                    if allowed[w as usize] && !(row == 0 && w == 0) && pre.pass(ctx, w, full) {
                        if let Some(ns) = step(ctx, &st, w, row) {
                            let pk = ctx.pack_canon(&ns);
                            let s = shard_of(&pk);
                            if keep(s) {
                                bufs[s].push((pk, *cnt));
                                if bufs[s].len() >= FLUSH {
                                    flush(&mut bufs[s], s);
                                }
                            }
                        }
                    }
                    if sub == 0 {
                        break;
                    }
                    sub = (sub - 1) & free;
                }
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
/// Memory knobs (both default to 1):
/// * `BRITISH_PASSES=k` builds the last top-half row in `k` passes, each keeping
///   1/k of its shards and gluing them before moving on — k× the work of that
///   transfer step for 1/k of its peak memory.
/// * `BRITISH_ROW_PASSES=k` does the same for the second-to-last row, whose
///   shards are flattened after each pass, so only 1/k of it is ever held as
///   hash maps at once.
pub fn count(n: usize, style: Style) -> u128 {
    assert_eq!(style, Style::British);
    let env = |k: &str| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1)
    };
    count_with_passes(n, env("BRITISH_ROW_PASSES"), env("BRITISH_PASSES"))
}

/// [`count`] with explicit pass counts for the second-to-last (`row_passes`)
/// and last (`passes`) top-half rows. The result does not depend on them.
pub fn count_with_passes(n: usize, row_passes: usize, passes: usize) -> u128 {
    assert!(n >= 5 && n % 2 == 1 && n <= MAXN);
    let (row_passes, passes) = (row_passes.max(1), passes.max(1));
    let h = (n - 1) / 2;
    let rows = allowed_rows(n);
    let ctx = Ctx::new(n);

    let instrument = std::env::var("DP_STATS").is_ok();
    let t0 = std::time::Instant::now();
    let total_len = |p: &[Vec<(Packed, u64)>]| p.iter().map(Vec::len).sum::<usize>();
    let mut parts: Vec<Vec<(Packed, u64)>> = vec![vec![([0, 0], 1)]];
    for i in 0..h - 1 {
        let k = if i + 2 == h { row_passes } else { 1 };
        let mut next: Vec<Vec<(Packed, u64)>> = Vec::new();
        for pass in 0..k {
            let keep = move |s: usize| s % k == pass;
            next.extend(into_parts(advance(&ctx, &parts, &rows, i, &keep)));
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
                    for &c in &centers {
                        if glue_ok(&ctx, &st, c) {
                            local += *cnt as u128;
                        }
                    }
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
