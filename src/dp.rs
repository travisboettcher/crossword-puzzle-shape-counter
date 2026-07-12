//! Transfer-matrix / frontier DP that counts valid grids row by row, merging
//! partial grids that share a frontier.
//!
//! ## Frontier state
//! After placing a set of rows, the frontier is the last row placed. For each
//! column `j` we store:
//!   * `vcap` ∈ {0,1,2,3}: length (capped at 3) of the vertical white run ending
//!     at the frontier in column `j`; 0 means the frontier cell is black.
//!   * `label`: a connectivity id. Two white columns share a label iff their
//!     frontier cells are joined through already-placed cells. Labels are
//!     canonicalized (restricted-growth) so equivalent frontiers coincide.
//! Plus flags `e0`, `eN` recording whether column 0 / column n-1 has ever held a
//! white cell (Rule 4).
//!
//! The whole state is packed into a single `u128`: 6 bits per column
//! (`label << 2 | vcap`) plus 2 flag bits, which fits every `n ≤ 21`.
//!
//! ## Connectivity invariant
//! In a single connected region no component is sealed off before the region is
//! complete, so a transition that makes an old component vanish is rejected.
//! * Non-symmetric count ([`count_nosym`]): the final all-black seal accepts iff
//!   exactly one component remains.
//! * Symmetric count ([`count_sym`]): the strict top half forbids every
//!   component death (a component sealed in the top half has its mirror sealed in
//!   the bottom half → two pieces), then the center row is glued to the mirrored
//!   bottom frontier.

use ahash::AHashMap;
use rayon::prelude::*;

use crate::rules::Style;

const MAXN: usize = 21;
const BITS_PER_COL: u32 = 6;

type Map = AHashMap<u128, u128>;

// --- state packing ---------------------------------------------------------

#[inline]
fn decode(key: u128, n: usize, vcap: &mut [u8; MAXN], label: &mut [u8; MAXN]) -> u8 {
    for (j, (vc, lb)) in vcap.iter_mut().zip(label.iter_mut()).enumerate().take(n) {
        let b = ((key >> (BITS_PER_COL * j as u32)) & 0x3f) as u8;
        *vc = b & 0b11;
        *lb = b >> 2;
    }
    ((key >> (BITS_PER_COL * n as u32)) & 0b11) as u8
}

#[inline]
fn encode(vcap: &[u8; MAXN], label: &[u8; MAXN], n: usize, flags: u8) -> u128 {
    let mut key: u128 = 0;
    for j in 0..n {
        let b = ((label[j] << 2) | vcap[j]) as u128;
        key |= b << (BITS_PER_COL * j as u32);
    }
    key | ((flags as u128) << (BITS_PER_COL * n as u32))
}

// --- row / run helpers -----------------------------------------------------

fn allowed_rows(n: usize, style: Style) -> Vec<u32> {
    let full: u32 = (1u32 << n) - 1;
    (0..=full)
        .filter(|&m| row_ok_horizontal(m, n, style))
        .collect()
}

/// American: every maximal white run ≥ 3. British: no run of length 2.
fn row_ok_horizontal(mask: u32, n: usize, style: Style) -> bool {
    let mut j = 0;
    while j < n {
        if (mask >> j) & 1 == 1 {
            let start = j;
            while j < n && (mask >> j) & 1 == 1 {
                j += 1;
            }
            let len = j - start;
            match style {
                Style::American if len < 3 => return false,
                Style::British if len == 2 => return false,
                _ => {}
            }
        } else {
            j += 1;
        }
    }
    true
}

/// May a vertical run of capped length `vc` close now? American: needs ≥ 3.
fn vrun_close_ok(style: Style, vc: u8) -> bool {
    match style {
        Style::American => vc >= 3,
        Style::British => vc != 2, // refined in M4
    }
}

/// A vertical run crossing the center: capped length `a` above, `b` below, plus
/// the center cell. Caps only merge lengths ≥ 3 (always legal), so this decides
/// correctly.
fn cross_run_ok(style: Style, a: u8, b: u8) -> bool {
    match style {
        Style::American => a >= 3 || b >= 3 || a + b >= 2, // total a + 1 + b ≥ 3
        Style::British => a + 1 + b != 2,                  // refined in M4
    }
}

// --- union-find on a stack array ------------------------------------------

#[inline]
fn uf_find(parent: &mut [usize; MAXN], x: usize) -> usize {
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
fn uf_union(parent: &mut [usize; MAXN], a: usize, b: usize) {
    let ra = uf_find(parent, a);
    let rb = uf_find(parent, b);
    if ra != rb {
        parent[ra] = rb;
    }
}

// --- transition ------------------------------------------------------------

/// Columns that a valid next row is *forced* to keep white: closing them now
/// would create an illegal short vertical run. American: capped length 1 or 2.
#[inline]
fn forced_mask(vcap: &[u8; MAXN], n: usize, style: Style) -> u32 {
    let mut f = 0u32;
    match style {
        Style::American => {
            for j in 0..n {
                if vcap[j] == 1 || vcap[j] == 2 {
                    f |= 1 << j;
                }
            }
        }
        Style::British => {
            for j in 0..n {
                if vcap[j] == 2 {
                    f |= 1 << j;
                }
            }
        }
    }
    f
}

/// The transition body, given the already-decoded frontier.
#[inline]
fn step_decoded(
    vcap: &[u8; MAXN],
    label: &[u8; MAXN],
    flags: u8,
    w: u32,
    n: usize,
    style: Style,
) -> Option<u128> {
    let white = |j: usize| (w >> j) & 1 == 1;

    // 1. Vertical-run closures (white above, black now).
    for j in 0..n {
        if vcap[j] > 0 && !white(j) && !vrun_close_ok(style, vcap[j]) {
            return None;
        }
    }

    // 2. Union-find over the new row's white columns.
    let mut parent = [0usize; MAXN];
    for (j, p) in parent.iter_mut().enumerate().take(n) {
        *p = j;
    }
    for j in 0..n.saturating_sub(1) {
        if white(j) && white(j + 1) {
            uf_union(&mut parent, j, j + 1);
        }
    }
    // cells sitting below the same old component
    let mut first_below = [usize::MAX; MAXN]; // indexed by old label
    for j in 0..n {
        if white(j) && vcap[j] > 0 {
            let c = label[j] as usize;
            if first_below[c] == usize::MAX {
                first_below[c] = j;
            } else {
                uf_union(&mut parent, first_below[c], j);
            }
        }
    }

    // 3. Reject if any old component has no continuation (sealed mid-grid).
    for j in 0..n {
        if vcap[j] > 0 && first_below[label[j] as usize] == usize::MAX {
            return None;
        }
    }

    // 4. Canonical new frontier.
    let mut nvcap = [0u8; MAXN];
    let mut nlabel = [0u8; MAXN];
    let mut root_label = [0u8; MAXN]; // root column -> new label (0 = unassigned)
    let mut next_label: u8 = 1;
    for j in 0..n {
        if white(j) {
            let r = uf_find(&mut parent, j);
            if root_label[r] == 0 {
                root_label[r] = next_label;
                next_label += 1;
            }
            nlabel[j] = root_label[r];
            nvcap[j] = if vcap[j] > 0 { (vcap[j] + 1).min(3) } else { 1 };
        }
    }

    let mut nflags = flags;
    if white(0) {
        nflags |= 1;
    }
    if white(n - 1) {
        nflags |= 2;
    }
    Some(encode(&nvcap, &nlabel, n, nflags))
}

/// Terminal acceptance for the non-symmetric count: seal with an all-black row.
fn terminal_ok(key: u128, n: usize, style: Style) -> bool {
    let mut vcap = [0u8; MAXN];
    let mut label = [0u8; MAXN];
    let flags = decode(key, n, &mut vcap, &mut label);
    let mut seen = [false; MAXN];
    let mut comps = 0usize;
    for j in 0..n {
        if vcap[j] > 0 {
            if !vrun_close_ok(style, vcap[j]) {
                return false;
            }
            let c = label[j] as usize;
            if !seen[c] {
                seen[c] = true;
                comps += 1;
            }
        }
    }
    comps == 1 && (flags & 1 == 1) && (flags & 2 == 2)
}

// --- parallel row transition ----------------------------------------------

fn advance(map: Map, rows: &[u32], n: usize, style: Style, first_row: bool) -> Map {
    let entries: Vec<(u128, u128)> = map.into_iter().collect();
    entries
        .par_iter()
        .fold(AHashMap::new, |mut acc: Map, &(st, cnt)| {
            let mut vcap = [0u8; MAXN];
            let mut label = [0u8; MAXN];
            let flags = decode(st, n, &mut vcap, &mut label);
            let forced = forced_mask(&vcap, n, style);
            for &w in rows {
                if first_row && w == 0 {
                    continue; // Rule 4: top row has a white square
                }
                if w & forced != forced {
                    continue; // a run that must continue would be closed
                }
                if let Some(ns) = step_decoded(&vcap, &label, flags, w, n, style) {
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

/// Count valid n×n grids of `style`, **without** the 180° symmetry rule (M1).
pub fn count_nosym(n: usize, style: Style) -> u128 {
    assert!(n >= 5 && n % 2 == 1 && n <= MAXN);
    let rows = allowed_rows(n, style);
    let mut map: Map = AHashMap::new();
    map.insert(0, 1);
    for i in 0..n {
        let last = i == n - 1;
        // Rule 4: bottom row must also contain a white square.
        let rows_i: Vec<u32> = if last {
            rows.iter().copied().filter(|&w| w != 0).collect()
        } else {
            rows.clone()
        };
        map = advance(map, &rows_i, n, style, i == 0);
    }
    let entries: Vec<(u128, u128)> = map.into_iter().collect();
    entries
        .par_iter()
        .filter(|&&(st, _)| terminal_ok(st, n, style))
        .map(|&(_, c)| c)
        .sum()
}

// --- M2: symmetry folding + center gluing ---------------------------------

/// Count valid 180°-symmetric n×n grids of `style` (the published quantity).
pub fn count_sym(n: usize, style: Style) -> u128 {
    assert!(n >= 5 && n % 2 == 1 && n <= MAXN);
    let h = (n - 1) / 2;
    let rows = allowed_rows(n, style);

    // Top half: rows 0..=h-1.
    let instrument = std::env::var("DP_STATS").is_ok();
    let t0 = std::time::Instant::now();
    let mut map: Map = AHashMap::new();
    map.insert(0, 1);
    for i in 0..h {
        map = advance(map, &rows, n, style, i == 0);
        if instrument {
            eprintln!("  row {i}: {} states ({:.2?})", map.len(), t0.elapsed());
        }
    }

    // Center gluing (parallel over surviving top-half frontiers).
    let centers: Vec<u32> = rows
        .iter()
        .copied()
        .filter(|&m| is_palindrome(m, n))
        .collect();
    if instrument {
        eprintln!(
            "  top-half done: {} states, {} centers, {} rows ({:.2?})",
            map.len(),
            centers.len(),
            rows.len(),
            t0.elapsed()
        );
    }
    let entries: Vec<(u128, u128)> = map.into_iter().collect();
    entries
        .par_iter()
        .map(|&(st, cnt)| {
            let mut local = 0u128;
            for &c in &centers {
                if glue_ok(st, c, n, style) {
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

/// Glue a top-half frontier `key` to center row `c` (a palindrome) and its
/// mirrored bottom half. Returns whether the resulting full grid is valid.
fn glue_ok(key: u128, c: u32, n: usize, style: Style) -> bool {
    let mut vcap = [0u8; MAXN];
    let mut label = [0u8; MAXN];
    let flags = decode(key, n, &mut vcap, &mut label);
    let cw = |j: usize| (c >> j) & 1 == 1;

    // 1. Vertical-run rules around the center.
    for j in 0..n {
        if cw(j) {
            if !cross_run_ok(style, vcap[j], vcap[n - 1 - j]) {
                return false;
            }
        } else if vcap[j] > 0 && !vrun_close_ok(style, vcap[j]) {
            return false;
        }
    }

    // 2. Rule 4 (edge columns).
    let used0 = flags & 1 == 1;
    let usedn = flags & 2 == 2;
    if !(used0 || usedn || cw(0)) {
        return false;
    }

    // 3. Connectivity: glue top classes, center cells, mirrored bottom classes.
    let m = (0..n).map(|j| label[j]).max().unwrap_or(0) as usize; // labels 1..=m
    if m == 0 {
        return false;
    }
    // Node layout: top class k -> k-1; bottom class k -> m+(k-1); center j -> 2m+j.
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
            if vcap[j] > 0 {
                union3(&mut parent, cen_node(j), top_node(label[j]));
            }
            if vcap[n - 1 - j] > 0 {
                union3(&mut parent, cen_node(j), bot_node(label[n - 1 - j]));
            }
        }
    }

    // All top classes, bottom classes, and center white cells must coincide.
    let mut root: Option<usize> = None;
    let same = |node: usize, parent: &mut [usize; MAXN * 3], root: &mut Option<usize>| {
        let r = find3(parent, node);
        match *root {
            None => {
                *root = Some(r);
                true
            }
            Some(rr) => rr == r,
        }
    };
    for k in 1..=m as u8 {
        if !same(top_node(k), &mut parent, &mut root) {
            return false;
        }
        if !same(bot_node(k), &mut parent, &mut root) {
            return false;
        }
    }
    for j in 0..n {
        if cw(j) && !same(cen_node(j), &mut parent, &mut root) {
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
    let ra = find3(parent, a);
    let rb = find3(parent, b);
    if ra != rb {
        parent[ra] = rb;
    }
}
