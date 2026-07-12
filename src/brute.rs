//! Brute-force reference enumerator — the independent oracle.
//!
//! It enumerates candidate grids by filling the top half row by row (the bottom
//! half is fixed by 180° symmetry), prunes with *necessary* conditions only, and
//! then runs the full [`Grid::is_valid`] check at each leaf. Because correctness
//! rests entirely on the leaf validator (the pruning can only ever remove grids
//! that are already provably invalid), this shares no logic with the DP and makes
//! a trustworthy cross-check for small `n` (≤ 9).

use crate::grid::Grid;
use crate::rules::Style;

/// Count valid n×n grids of the given style by brute force. Intended for n ≤ 9.
pub fn count(n: usize, style: Style) -> u128 {
    assert!(n >= 5 && n % 2 == 1, "n must be odd and >= 5");
    assert!(n <= 31, "brute force uses u32 row masks");
    let h = (n - 1) / 2; // center row index
    let full: u32 = if n == 32 { u32::MAX } else { (1u32 << n) - 1 };

    // Candidate rows satisfying the per-row horizontal necessary condition.
    let mut rows: Vec<u32> = Vec::new();
    for mask in 0..=full {
        if row_ok_horizontal(mask, n, style) {
            rows.push(mask);
        }
    }
    let center_rows: Vec<u32> = rows
        .iter()
        .copied()
        .filter(|&m| is_palindrome(m, n))
        .collect();

    let mut top = vec![0u32; h + 1];
    let mut count = 0u128;
    recurse(0, h, n, style, &rows, &center_rows, &mut top, &mut count);
    count
}

/// Count valid n×n grids of the given style by brute force, **dropping the 180°
/// symmetry rule**. Enumerates all `n` rows freely. Intended for very small `n`
/// (≤ 7) as an independent check on the non-symmetric DP.
pub fn count_nosym(n: usize, style: Style) -> u128 {
    assert!(n >= 5 && n % 2 == 1);
    assert!(n <= 31);
    let full: u32 = (1u32 << n) - 1;
    let mut rows: Vec<u32> = Vec::new();
    for mask in 0..=full {
        if row_ok_horizontal(mask, n, style) {
            rows.push(mask);
        }
    }
    let mut all = vec![0u32; n];
    let mut count = 0u128;
    recurse_nosym(0, n, style, &rows, &mut all, &mut count);
    count
}

fn recurse_nosym(
    depth: usize,
    n: usize,
    style: Style,
    rows: &[u32],
    all: &mut Vec<u32>,
    count: &mut u128,
) {
    for &mask in rows {
        all[depth] = mask;
        if !closed_vruns_ok(all, depth, n, style) {
            continue;
        }
        if depth == n - 1 {
            let mut g = Grid::new(n);
            for i in 0..n {
                for j in 0..n {
                    g.set(i, j, (all[i] >> j) & 1 == 1);
                }
            }
            if g.is_valid_nosym(style) {
                *count += 1;
            }
        } else {
            recurse_nosym(depth + 1, n, style, rows, all, count);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn recurse(
    depth: usize,
    h: usize,
    n: usize,
    style: Style,
    rows: &[u32],
    center_rows: &[u32],
    top: &mut [u32],
    count: &mut u128,
) {
    let candidates = if depth == h { center_rows } else { rows };
    for &mask in candidates {
        top[depth] = mask;
        // Necessary-condition prune: any vertical run already *closed* within the
        // placed rows must have a legal length.
        if !closed_vruns_ok(top, depth, n, style) {
            continue;
        }
        if depth == h {
            let grid = materialize(top, n);
            if grid.is_valid(style) {
                *count += 1;
            }
        } else {
            recurse(depth + 1, h, n, style, rows, center_rows, top, count);
        }
    }
}

/// Build the full grid from the chosen top-half rows using 180° symmetry.
fn materialize(top: &[u32], n: usize) -> Grid {
    let h = (n - 1) / 2;
    let mut g = Grid::new(n);
    for i in 0..=h {
        for j in 0..n {
            g.set(i, j, (top[i] >> j) & 1 == 1);
        }
    }
    for i in (h + 1)..n {
        for j in 0..n {
            // cell(i,j) == cell(n-1-i, n-1-j)
            let src_row = top[n - 1 - i];
            let bit = (src_row >> (n - 1 - j)) & 1 == 1;
            g.set(i, j, bit);
        }
    }
    g
}

/// Does this row satisfy the per-row horizontal necessary condition?
/// (A necessary condition on any valid grid, since horizontal words live in one
/// row.) American: every maximal white run ≥ 3. British: no run of length 2.
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

/// Check vertical runs that have already closed within rows `0..=cur`.
fn closed_vruns_ok(top: &[u32], cur: usize, n: usize, style: Style) -> bool {
    for j in 0..n {
        let mut i = 0;
        while i <= cur {
            if (top[i] >> j) & 1 == 1 {
                let start = i;
                while i <= cur && (top[i] >> j) & 1 == 1 {
                    i += 1;
                }
                // Closed iff terminated by a placed black cell (i <= cur).
                if i <= cur {
                    let len = i - start;
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
                }
            } else {
                i += 1;
            }
        }
    }
    true
}

fn is_palindrome(mask: u32, n: usize) -> bool {
    for j in 0..n {
        let a = (mask >> j) & 1;
        let b = (mask >> (n - 1 - j)) & 1;
        if a != b {
            return false;
        }
    }
    true
}
