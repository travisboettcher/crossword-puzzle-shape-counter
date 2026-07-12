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
//!     `i+1` is placed we know both vertical neighbours of every cell in row `i`.
//!   * **Vertical** words are validated at their closure by carrying, per open
//!     column run, the sequence of checked bits (each cell's "has a horizontal
//!     neighbour"). Validation reuses [`word_checks_ok`].
//!
//! ## Frontier state (per column, packed into a u16)
//!   * `label` (bits 12..16): connectivity id, canonicalized (0 = black).
//!   * `len`   (bits 8..12): length of the open vertical white run (0 = black),
//!     capped by the half-height `h ≤ 7`.
//!   * `bits`  (bits 0..8): checked bit of each cell of the run; bit 0 is the
//!     topmost cell, bit `len-1` the frontier cell.
//! Plus a flag u16 (`e0 | eN<<1`) for the Rule-4 edge columns.

use ahash::AHashMap;
use rayon::prelude::*;

use crate::rules::{word_checks_ok, Style};

const MAXN: usize = 15;
const SLOTS: usize = MAXN + 1;

type Key = [u16; SLOTS];
type Map = AHashMap<Key, u128>;

#[inline]
fn pack_col(label: u8, len: u8, bits: u8) -> u16 {
    ((label as u16) << 12) | ((len as u16) << 8) | bits as u16
}
#[inline]
fn col_label(c: u16) -> u8 {
    (c >> 12) as u8
}
#[inline]
fn col_len(c: u16) -> u8 {
    ((c >> 8) & 0xf) as u8
}
#[inline]
fn col_bits(c: u16) -> u8 {
    (c & 0xff) as u8
}

/// Expand the stored run into its checked-bit sequence (top cell first).
#[inline]
fn run_pattern(c: u16, out: &mut [bool; 16]) -> usize {
    let len = col_len(c) as usize;
    let bits = col_bits(c);
    for (t, o) in out.iter_mut().enumerate().take(len) {
        *o = (bits >> t) & 1 == 1;
    }
    len
}

/// Validate one completed vertical run against Rule 3 + British Rules 6–8.
/// `pat` holds the checked bit of each cell (top to bottom).
fn vrun_word_ok(pat: &[bool]) -> bool {
    match pat.len() {
        0 => true,
        1 => pat[0], // a lone cell must be checked by its horizontal word
        2 => false,  // length-2 word forbidden (Rule 3)
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
            if !vrun_word_ok(&pat) {
                return false;
            }
        } else {
            j += 1;
        }
    }
    true
}

/// Place row `w` (row `i+1`) onto frontier `key` (row `i`).
/// First validates row `i`'s horizontal words (now that its lower neighbour is
/// known), then closes/extends vertical runs and updates connectivity.
fn step(key: &Key, w: u32, n: usize) -> Option<Key> {
    let old = key;
    let white = |j: usize| (w >> j) & 1 == 1;

    // --- validate row i's horizontal words -------------------------------
    // row i's white pattern is the set of columns with an open run.
    let mut row_i_mask: u32 = 0;
    let mut checked_v = [false; MAXN];
    for j in 0..n {
        if col_len(old[j]) > 0 {
            row_i_mask |= 1 << j;
            // checked in vertical direction: white above (run len >= 2) or below.
            checked_v[j] = col_len(old[j]) >= 2 || white(j);
        }
    }
    if !hrow_ok(row_i_mask, n, &checked_v) {
        return None;
    }

    // --- vertical runs: close (white->black) or extend (white below) ------
    let mut pat = [false; 16];
    for j in 0..n {
        if col_len(old[j]) > 0 && !white(j) {
            let len = run_pattern(old[j], &mut pat);
            if !vrun_word_ok(&pat[..len]) {
                return None;
            }
        }
    }

    // --- connectivity: union-find over new-row white columns -------------
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
    // reject any old component sealed off mid-grid
    for j in 0..n {
        if col_len(old[j]) > 0 && first_below[col_label(old[j]) as usize] == usize::MAX {
            return None;
        }
    }

    // --- build new frontier ----------------------------------------------
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
            // checked bit of this new cell (row i+1) = has horizontal neighbour
            let ch = (j > 0 && white(j - 1)) || (j + 1 < n && white(j + 1));
            let (old_len, old_bits) = (col_len(old[j]), col_bits(old[j]));
            let new_len = (old_len + 1).min(15);
            let new_bits = old_bits | ((ch as u8) << old_len);
            out[j] = pack_col(label, new_len, new_bits);
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

fn advance(map: Map, rows: &[u32], n: usize, first_row: bool) -> Map {
    let entries: Vec<(Key, u128)> = map.into_iter().collect();
    entries
        .par_iter()
        .fold(AHashMap::new, |mut acc: Map, &(st, cnt)| {
            // Columns forced to stay white: a length-2 run (can't close) or a
            // length-1 run whose cell is unchecked (closing would strand it).
            let mut forced = 0u32;
            for j in 0..n {
                let l = col_len(st[j]);
                if l == 2 || (l == 1 && col_bits(st[j]) & 1 == 0) {
                    forced |= 1 << j;
                }
            }
            for &w in rows {
                if first_row && w == 0 {
                    continue;
                }
                if w & forced != forced {
                    continue;
                }
                if let Some(ns) = step(&st, w, n) {
                    *acc.entry(ns).or_insert(0) += cnt;
                }
            }
            acc
        })
        .reduce(AHashMap::new, |mut a, b| {
            for (k, v) in b {
                *a.entry(k).or_insert(0) += v;
            }
            a
        })
}

/// Count valid 180°-symmetric British n×n grids (Keith's #Total).
pub fn count(n: usize, style: Style) -> u128 {
    assert_eq!(style, Style::British);
    assert!(n >= 5 && n % 2 == 1 && n <= MAXN);
    let h = (n - 1) / 2;
    let rows = allowed_rows(n);

    let instrument = std::env::var("DP_STATS").is_ok();
    let t0 = std::time::Instant::now();
    let mut map: Map = AHashMap::new();
    map.insert([0u16; SLOTS], 1);
    for i in 0..h {
        map = advance(map, &rows, n, i == 0);
        if instrument {
            eprintln!("  row {i}: {} states ({:.2?})", map.len(), t0.elapsed());
        }
    }

    let centers: Vec<u32> = rows
        .iter()
        .copied()
        .filter(|&m| is_palindrome(m, n))
        .collect();
    if instrument {
        eprintln!(
            "  top-half done: {} states, {} centers ({:.2?})",
            map.len(),
            centers.len(),
            t0.elapsed()
        );
    }
    let entries: Vec<(Key, u128)> = map.into_iter().collect();
    entries
        .par_iter()
        .map(|&(st, cnt)| {
            let mut local = 0u128;
            for &c in &centers {
                if glue_ok(&st, c, n) {
                    local += cnt;
                }
            }
            local
        })
        .sum()
}

fn is_palindrome(mask: u32, n: usize) -> bool {
    (0..n).all(|j| (mask >> j) & 1 == (mask >> (n - 1 - j)) & 1)
}

/// Glue the top-half frontier `key` to the palindromic center row `c` and the
/// mirrored bottom half, validating the center row's words and connectivity.
fn glue_ok(key: &Key, c: u32, n: usize) -> bool {
    let old = key;
    let cw = |j: usize| (c >> j) & 1 == 1;

    // --- row h-1's horizontal words --------------------------------------
    // The top-half loop validates each row when the next is placed, leaving the
    // last top row (h-1) for here: its lower neighbour is the center row.
    let mut row_hm1: u32 = 0;
    let mut checked_hm1 = [false; MAXN];
    for j in 0..n {
        if col_len(old[j]) > 0 {
            row_hm1 |= 1 << j;
            checked_hm1[j] = col_len(old[j]) >= 2 || cw(j);
        }
    }
    if !hrow_ok(row_hm1, n, &checked_hm1) {
        return false;
    }

    // --- center row's horizontal words -----------------------------------
    // center cell (h,j) checked iff it has a white vertical neighbour: row h-1
    // (open run in col j) or row h+1 = reverse(row h-1) (open run in col n-1-j).
    let mut checked_v = [false; MAXN];
    for j in 0..n {
        if cw(j) {
            checked_v[j] = col_len(old[j]) > 0 || col_len(old[n - 1 - j]) > 0;
        }
    }
    if !hrow_ok(c, n, &checked_v) {
        return false;
    }

    // --- vertical words at the center ------------------------------------
    let mut ptop = [false; 16];
    let mut pmir = [false; 16];
    for j in 0..n {
        if cw(j) {
            // Crossing word: top run (col j) ++ center ++ reverse(top run col n-1-j).
            let lt = run_pattern(old[j], &mut ptop);
            let lm = run_pattern(old[n - 1 - j], &mut pmir);
            let center_checked = (j > 0 && cw(j - 1)) || (j + 1 < n && cw(j + 1));
            let mut full: Vec<bool> = Vec::with_capacity(lt + 1 + lm);
            full.extend_from_slice(&ptop[..lt]);
            full.push(center_checked);
            for t in (0..lm).rev() {
                full.push(pmir[t]);
            }
            if !vrun_word_ok(&full) {
                return false;
            }
        } else if col_len(old[j]) > 0 {
            // Top run closes at the (black) center; the mirrored bottom run in
            // this column is validated when column n-1-j is processed.
            let len = run_pattern(old[j], &mut ptop);
            if !vrun_word_ok(&ptop[..len]) {
                return false;
            }
        }
    }

    // --- Rule 4 edge columns ---------------------------------------------
    let flags = old[n];
    if !((flags & 1 == 1) || (flags & 2 == 2) || cw(0)) {
        return false;
    }

    // --- connectivity gluing (identical to the American case) ------------
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
