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

use ahash::AHashMap;
use rayon::prelude::*;

use crate::rules::{word_checks_ok, Style};

const MAXN: usize = 15;
const SLOTS: usize = MAXN + 1;
const DBIAS: i8 = 4;

type Key = [u16; SLOTS];
type Map = AHashMap<Key, u128>;

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

/// Place row `w` (row `i+1`) onto frontier `key` (row `i`).
fn step(key: &Key, w: u32, n: usize) -> Option<Key> {
    let old = key;
    let white = |j: usize| (w >> j) & 1 == 1;

    // Validate row i's horizontal words (its lower neighbour is now known).
    let mut row_i_mask: u32 = 0;
    let mut checked_v = [false; MAXN];
    for j in 0..n {
        if col_len(old[j]) > 0 {
            row_i_mask |= 1 << j;
            checked_v[j] = col_len(old[j]) >= 2 || white(j);
        }
    }
    if !hrow_ok(row_i_mask, n, &checked_v) {
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
                if l == 2 || (l == 1 && col_d(st[j]) == -1) {
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
/// mirrored bottom half, validating the center's words and connectivity.
fn glue_ok(key: &Key, c: u32, n: usize) -> bool {
    let old = key;
    let cw = |j: usize| (c >> j) & 1 == 1;

    // Row h-1's horizontal words (its lower neighbour is the center row).
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

    // Center row's horizontal words. A center cell is checked iff it has a white
    // vertical neighbour: row h-1 (col j) or row h+1 = reverse(row h-1) (col n-1-j).
    let mut checked_c = [false; MAXN];
    for j in 0..n {
        if cw(j) {
            checked_c[j] = col_len(old[j]) > 0 || col_len(old[n - 1 - j]) > 0;
        }
    }
    if !hrow_ok(c, n, &checked_c) {
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
