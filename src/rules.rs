//! The rules that define a *valid* crossword grid, following Michael Keith's
//! G4G16 paper "How many n×n British-style crossword grids are there?".
//!
//! Common rules (American & British), for odd `n`:
//!   1. n×n square, n odd.
//!   2. 180° rotational symmetry.
//!   3. Every word has at least 3 letters.
//!   4. Every outer-edge row and column has at least one white square.
//!   5. All white squares form a single 4-connected region.
//!
//! American adds:
//!   6. Every letter in every word is *checked*.
//!
//! British adds:
//!   6. Each word of length k has exactly ⌈k/2⌉ checked letters.
//!   7. Three or more adjacent unchecked letters are forbidden.
//!   8. Two adjacent unchecked letters are allowed, but not at the start or end
//!      of a word.
//!
//! A cell is *checked* iff it belongs to a word (a maximal white run of length
//! ≥ 3) in **both** the across and down directions. A "word" is a maximal white
//! run of length ≥ 3; Rule 3 bans length-2 runs outright.

/// Which style's word rules to apply. The five common rules are shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Style {
    American,
    British,
}

impl Style {
    pub fn as_str(self) -> &'static str {
        match self {
            Style::American => "american",
            Style::British => "british",
        }
    }
}

/// Validate a single word (a maximal white run of length `k >= 3`) against the
/// per-word rules of `style`, given, for each cell of the word in order, whether
/// that cell is *checked* (i.e. crossed by a word in the perpendicular
/// direction).
///
/// This is the single source of truth for Rules 6–8; both the brute-force
/// validator and the transfer-matrix DP consult it (directly or via the
/// precomputed lookup tables derived from it).
pub fn word_checks_ok(style: Style, checked: &[bool]) -> bool {
    let k = checked.len();
    debug_assert!(k >= 3, "words have length >= 3");
    match style {
        // American Rule 6: every letter checked.
        Style::American => checked.iter().all(|&c| c),
        Style::British => {
            // Rule 6: exactly ⌈k/2⌉ checked letters.
            let checked_count = checked.iter().filter(|&&c| c).count();
            if checked_count != k.div_ceil(2) {
                return false;
            }
            // Rule 8: a *pair* of adjacent unchecked letters may not sit at the
            // start or end of the word. (A single unchecked letter at an end is
            // permitted — the rule constrains pairs, not lone unchecked cells.)
            if !checked[0] && !checked[1] {
                return false;
            }
            if !checked[k - 1] && !checked[k - 2] {
                return false;
            }
            // Rule 7: no run of 3+ consecutive unchecked letters.
            let mut consec_unchecked = 0usize;
            for &c in checked {
                if c {
                    consec_unchecked = 0;
                } else {
                    consec_unchecked += 1;
                    if consec_unchecked >= 3 {
                        return false;
                    }
                }
            }
            true
        }
    }
}
