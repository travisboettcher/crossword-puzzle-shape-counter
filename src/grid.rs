//! A materialized grid plus a straightforward, obviously-correct full validator.
//!
//! The validator is deliberately simple (it walks the whole grid and checks each
//! rule directly). It is the ground-truth oracle: the brute-force enumerator and,
//! later, the transfer-matrix DP are cross-checked against counts produced by
//! running this validator over candidate grids.

use crate::rules::{word_checks_ok, Style};

/// An n×n grid. `cells[i * n + j]` is `true` for a white square, `false` for black.
#[derive(Clone, PartialEq, Eq)]
pub struct Grid {
    pub n: usize,
    pub cells: Vec<bool>,
}

impl Grid {
    pub fn new(n: usize) -> Self {
        Grid {
            n,
            cells: vec![false; n * n],
        }
    }

    #[inline]
    pub fn white(&self, i: usize, j: usize) -> bool {
        self.cells[i * self.n + j]
    }

    #[inline]
    pub fn set(&mut self, i: usize, j: usize, white: bool) {
        self.cells[i * self.n + j] = white;
    }

    /// Full validity check against all rules for `style`.
    pub fn is_valid(&self, style: Style) -> bool {
        self.is_valid_inner(style, true)
    }

    /// Validity check with Rule 2 (180° symmetry) dropped — used to validate the
    /// non-symmetric DP (M1) independently of the symmetry folding (M2).
    pub fn is_valid_nosym(&self, style: Style) -> bool {
        self.is_valid_inner(style, false)
    }

    fn is_valid_inner(&self, style: Style, require_symmetry: bool) -> bool {
        let n = self.n;

        // Rule 1: n odd. (Sizes below 5 are degenerate.)
        if n < 5 || n % 2 == 0 {
            return false;
        }

        // Rule 2: 180° rotational symmetry.
        if require_symmetry {
            for i in 0..n {
                for j in 0..n {
                    if self.white(i, j) != self.white(n - 1 - i, n - 1 - j) {
                        return false;
                    }
                }
            }
        }

        // Rule 4: each outer-edge line (top/bottom row, left/right column) has a
        // white square, so the white region touches all four edges.
        let row_has_white = |i: usize| (0..n).any(|j| self.white(i, j));
        let col_has_white = |j: usize| (0..n).any(|i| self.white(i, j));
        if !row_has_white(0) || !row_has_white(n - 1) || !col_has_white(0) || !col_has_white(n - 1)
        {
            return false;
        }

        // Precompute, for every cell, the length of the maximal horizontal and
        // vertical white run through it (0 for black cells).
        let mut hrun = vec![0u16; n * n];
        let mut vrun = vec![0u16; n * n];
        for i in 0..n {
            let mut j = 0;
            while j < n {
                if self.white(i, j) {
                    let start = j;
                    while j < n && self.white(i, j) {
                        j += 1;
                    }
                    let len = (j - start) as u16;
                    for c in start..j {
                        hrun[i * n + c] = len;
                    }
                } else {
                    j += 1;
                }
            }
        }
        for j in 0..n {
            let mut i = 0;
            while i < n {
                if self.white(i, j) {
                    let start = i;
                    while i < n && self.white(i, j) {
                        i += 1;
                    }
                    let len = (i - start) as u16;
                    for r in start..i {
                        vrun[r * n + j] = len;
                    }
                } else {
                    i += 1;
                }
            }
        }

        // A cell is "checked" iff it is part of a word (run >= 3) in both directions.
        let checked = |i: usize, j: usize| hrun[i * n + j] >= 3 && vrun[i * n + j] >= 3;

        // Rule 3 + style word rules, horizontal direction.
        for i in 0..n {
            let mut j = 0;
            while j < n {
                if self.white(i, j) {
                    let start = j;
                    while j < n && self.white(i, j) {
                        j += 1;
                    }
                    let len = j - start;
                    if !self.run_ok(style, len, i, start, true, &checked) {
                        return false;
                    }
                } else {
                    j += 1;
                }
            }
        }
        // Vertical direction.
        for j in 0..n {
            let mut i = 0;
            while i < n {
                if self.white(i, j) {
                    let start = i;
                    while i < n && self.white(i, j) {
                        i += 1;
                    }
                    let len = i - start;
                    if !self.run_ok(style, len, start, j, false, &checked) {
                        return false;
                    }
                } else {
                    i += 1;
                }
            }
        }

        // Rule 5: the white squares form a single 4-connected region.
        if !self.white_is_connected() {
            return false;
        }

        true
    }

    /// Check one maximal white run against Rule 3 and the style's word rules.
    /// `horizontal` selects the direction; `(i0, j0)` is the run's first cell.
    fn run_ok(
        &self,
        style: Style,
        len: usize,
        i0: usize,
        j0: usize,
        horizontal: bool,
        checked: &impl Fn(usize, usize) -> bool,
    ) -> bool {
        let n = self.n;
        // Rule 3: no 2-letter words (a length-2 maximal run is a forbidden word).
        if len == 2 {
            return false;
        }
        if len == 1 {
            // A lone white cell is not a word; it must be an (unchecked) letter of
            // a perpendicular word — otherwise it is uncrossed / isolated.
            // American forbids any uncrossed letter (Rule 6 ⇒ no length-1 runs).
            // British allows it iff the perpendicular run is a real word (>= 3).
            let (i, j) = (i0, j0);
            return match style {
                Style::American => false,
                Style::British => {
                    // The run's own direction has length 1, so this cell is
                    // unchecked in that direction; it is legal only if the
                    // perpendicular run is a real word (length >= 3).
                    let perp_len = if horizontal {
                        self.vertical_run_len(i, j)
                    } else {
                        self.horizontal_run_len(i, j)
                    };
                    perp_len >= 3
                }
            };
        }
        // len >= 3: a genuine word. Collect the checked-flag of each cell in order.
        let mut flags = Vec::with_capacity(len);
        if horizontal {
            for c in 0..len {
                flags.push(checked(i0, j0 + c));
            }
        } else {
            for c in 0..len {
                flags.push(checked(i0 + c, j0));
            }
        }
        debug_assert!(i0 < n && j0 < n);
        word_checks_ok(style, &flags)
    }

    fn horizontal_run_len(&self, i: usize, j: usize) -> usize {
        if !self.white(i, j) {
            return 0;
        }
        let mut a = j;
        while a > 0 && self.white(i, a - 1) {
            a -= 1;
        }
        let mut b = j;
        while b + 1 < self.n && self.white(i, b + 1) {
            b += 1;
        }
        b - a + 1
    }

    fn vertical_run_len(&self, i: usize, j: usize) -> usize {
        if !self.white(i, j) {
            return 0;
        }
        let mut a = i;
        while a > 0 && self.white(a - 1, j) {
            a -= 1;
        }
        let mut b = i;
        while b + 1 < self.n && self.white(b + 1, j) {
            b += 1;
        }
        b - a + 1
    }

    /// Rule 5: are all white cells a single 4-connected region?
    fn white_is_connected(&self) -> bool {
        let n = self.n;
        let mut start = None;
        let mut total_white = 0usize;
        for idx in 0..n * n {
            if self.cells[idx] {
                total_white += 1;
                if start.is_none() {
                    start = Some(idx);
                }
            }
        }
        let start = match start {
            Some(s) => s,
            None => return false, // no white cells at all is invalid (Rule 4 caught it too)
        };
        let mut seen = vec![false; n * n];
        let mut stack = vec![start];
        seen[start] = true;
        let mut count = 0usize;
        while let Some(idx) = stack.pop() {
            count += 1;
            let (i, j) = (idx / n, idx % n);
            let push = |i: usize, j: usize, seen: &mut Vec<bool>, stack: &mut Vec<usize>| {
                let k = i * n + j;
                if self.cells[k] && !seen[k] {
                    seen[k] = true;
                    stack.push(k);
                }
            };
            if i > 0 {
                push(i - 1, j, &mut seen, &mut stack);
            }
            if i + 1 < n {
                push(i + 1, j, &mut seen, &mut stack);
            }
            if j > 0 {
                push(i, j - 1, &mut seen, &mut stack);
            }
            if j + 1 < n {
                push(i, j + 1, &mut seen, &mut stack);
            }
        }
        count == total_white
    }

    /// Render as ASCII: `#` for black, `.` for white.
    pub fn pretty(&self) -> String {
        let n = self.n;
        let mut s = String::with_capacity(n * (n + 1));
        for i in 0..n {
            for j in 0..n {
                s.push(if self.white(i, j) { '.' } else { '#' });
            }
            s.push('\n');
        }
        s
    }
}
