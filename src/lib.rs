//! Counting valid n×n American- and British-style crossword grids.
//!
//! See the module docs in [`rules`] for the exact rule set (from Michael Keith's
//! G4G16 paper). The public entry points return the total number of valid grids
//! for a given odd size `n`.

pub mod british;
pub mod brute;
pub mod dp;
pub mod grid;
pub mod rules;

pub use rules::Style;

/// Count valid American-style n×n grids (OEIS A323839) via the transfer-matrix
/// DP. Validated against the published counts through 13×13.
pub fn count_american(n: usize) -> u128 {
    dp::count_sym(n, Style::American)
}

/// Count valid British-style n×n grids (Keith, G4G16) via the folded DP.
/// Validated against the brute-force oracle (n ≤ 9) and Keith's published
/// `#Total` counts.
pub fn count_british(n: usize) -> u128 {
    british::count(n, Style::British)
}
