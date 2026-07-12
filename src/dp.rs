//! Transfer-matrix / frontier DP that counts valid grids by processing the grid
//! one row at a time, merging partial grids that share a frontier.
//!
//! This module currently implements the **non-symmetric** American count (M1):
//! it counts grids obeying every rule *except* 180° symmetry, and exists to
//! validate the connectivity + vertical-run machinery against
//! [`crate::brute::count_nosym`] before the symmetry folding (M2) is added.
//!
//! ## Frontier state
//! After placing rows `0..=i`, the frontier is row `i`. For each column `j` we
//! store:
//!   * `vcap` ∈ {0,1,2,3}: the length (capped at 3) of the vertical white run
//!     ending at `(i, j)`; 0 means `(i, j)` is black.
//!   * `label`: a connectivity id. Two white columns share a label iff their
//!     frontier cells are joined through already-placed cells. Labels are
//!     canonicalized (restricted-growth) so equivalent frontiers hash equal.
//! Plus two flags `e0`, `eN` recording whether column 0 / column n-1 has ever
//! held a white cell (Rule 4).
//!
//! ## Connectivity invariant
//! In a single connected region no component is ever fully sealed before the
//! bottom of the grid, so an interior transition that would make an old
//! component vanish is rejected. The final "seal" (an implicit all-black row
//! past the bottom) accepts iff exactly one component remains.

use std::collections::HashMap;

use crate::rules::Style;

/// Column byte layout: bits 0..2 = `vcap` (0..3), bits 2.. = connectivity label.
#[inline]
fn make_col(label: u8, vcap: u8) -> u8 {
    (label << 2) | vcap
}
#[inline]
fn col_vcap(b: u8) -> u8 {
    b & 0b11
}
#[inline]
fn col_label(b: u8) -> u8 {
    b >> 2
}

/// A frontier state: `n` column bytes followed by one flag byte (`e0 | eN<<1`).
type State = Vec<u8>;

/// Rows (as white bit masks) whose horizontal runs are legal for `style`.
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
                Style::American => {
                    if len < 3 {
                        return false;
                    }
                }
                Style::British => {
                    if len == 2 {
                        return false;
                    }
                }
            }
        } else {
            j += 1;
        }
    }
    true
}

/// Advance the frontier by placing a new row with white mask `w`.
/// Returns the new state, or `None` if the transition is invalid (an illegal
/// vertical run closes, or a component would be sealed off mid-grid).
fn step(old: &State, w: u32, n: usize, style: Style) -> Option<State> {
    debug_assert_eq!(old.len(), n + 1);

    // 1. Vertical-run closures: columns white above, black now.
    for j in 0..n {
        let vc = col_vcap(old[j]);
        let now_white = (w >> j) & 1 == 1;
        if vc > 0 && !now_white {
            // Run of capped length `vc` closes here.
            if !vrun_close_ok(style, vc) {
                return None;
            }
        }
    }

    // 2. Union-find over the new row's white columns.
    let mut uf = Uf::new(n);
    let white = |j: usize| (w >> j) & 1 == 1;
    // horizontal adjacency within the new row
    for j in 0..n.saturating_sub(1) {
        if white(j) && white(j + 1) {
            uf.union(j, j + 1);
        }
    }
    // shared old component (cells sitting below the same old component)
    let mut first_below: HashMap<u8, usize> = HashMap::new();
    for j in 0..n {
        if white(j) && col_vcap(old[j]) > 0 {
            let c = col_label(old[j]);
            match first_below.get(&c) {
                Some(&j0) => uf.union(j0, j),
                None => {
                    first_below.insert(c, j);
                }
            }
        }
    }

    // 3. Which old components continue? Any old component with no cell below it
    //    is sealed — reject (interior death breaks single-connectivity).
    let mut old_comps: Vec<u8> = Vec::new();
    for j in 0..n {
        if col_vcap(old[j]) > 0 {
            let c = col_label(old[j]);
            if !old_comps.contains(&c) {
                old_comps.push(c);
            }
        }
    }
    for &c in &old_comps {
        if !first_below.contains_key(&c) {
            return None; // component sealed mid-grid
        }
    }

    // 4. Build the new canonical frontier.
    let mut new_state = vec![0u8; n + 1];
    let mut root_to_label: HashMap<usize, u8> = HashMap::new();
    let mut next_label: u8 = 1;
    for j in 0..n {
        if white(j) {
            let r = uf.find(j);
            let label = *root_to_label.entry(r).or_insert_with(|| {
                let l = next_label;
                next_label += 1;
                l
            });
            let vc = col_vcap(old[j]);
            let new_vc = if vc > 0 { (vc + 1).min(3) } else { 1 };
            new_state[j] = make_col(label, new_vc);
        } else {
            new_state[j] = 0;
        }
    }

    // 5. Rule-4 edge-column flags.
    let old_flags = old[n];
    let mut e0 = old_flags & 1;
    let mut en = (old_flags >> 1) & 1;
    if white(0) {
        e0 = 1;
    }
    if white(n - 1) {
        en = 1;
    }
    new_state[n] = e0 | (en << 1);

    Some(new_state)
}

/// Is it legal for a vertical run of capped length `vc` (1, 2, or 3=">=3") to
/// close now? For American every run must be ≥ 3.
fn vrun_close_ok(style: Style, vc: u8) -> bool {
    match style {
        Style::American => vc >= 3,
        // British handling arrives with M4; length-2 is always illegal.
        Style::British => vc != 2,
    }
}

/// Accept the frontier as a completed grid: seal with an implicit all-black row.
fn terminal_ok(st: &State, n: usize, style: Style) -> bool {
    // All remaining vertical runs close now.
    let mut comps: Vec<u8> = Vec::new();
    for j in 0..n {
        let vc = col_vcap(st[j]);
        if vc > 0 {
            if !vrun_close_ok(style, vc) {
                return false;
            }
            let c = col_label(st[j]);
            if !comps.contains(&c) {
                comps.push(c);
            }
        }
    }
    // Exactly one connected component, and both edge columns were used.
    let flags = st[n];
    let e0 = flags & 1 == 1;
    let en = (flags >> 1) & 1 == 1;
    comps.len() == 1 && e0 && en
}

/// Count valid n×n grids of `style`, **without** the 180° symmetry rule (M1).
pub fn count_nosym(n: usize, style: Style) -> u128 {
    assert!(n >= 5 && n % 2 == 1);
    let rows = allowed_rows(n, style);

    let mut map: HashMap<State, u128> = HashMap::new();
    map.insert(vec![0u8; n + 1], 1);

    for i in 0..n {
        let mut next: HashMap<State, u128> = HashMap::new();
        for (st, &cnt) in &map {
            for &w in &rows {
                // Rule 4: top and bottom rows must contain a white square.
                if (i == 0 || i == n - 1) && w == 0 {
                    continue;
                }
                if let Some(ns) = step(st, w, n, style) {
                    *next.entry(ns).or_insert(0) += cnt;
                }
            }
        }
        map = next;
    }

    map.iter()
        .filter(|(st, _)| terminal_ok(st, n, style))
        .map(|(_, &c)| c)
        .sum()
}

/// A tiny union-find over `0..n`.
struct Uf {
    parent: Vec<usize>,
}
impl Uf {
    fn new(n: usize) -> Self {
        Uf {
            parent: (0..n).collect(),
        }
    }
    fn find(&mut self, x: usize) -> usize {
        let mut r = x;
        while self.parent[r] != r {
            r = self.parent[r];
        }
        // path compression
        let mut c = x;
        while self.parent[c] != r {
            let next = self.parent[c];
            self.parent[c] = r;
            c = next;
        }
        r
    }
    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.parent[ra] = rb;
        }
    }
}
