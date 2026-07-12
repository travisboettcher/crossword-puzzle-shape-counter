//! Counting valid n×n American- and British-style crossword grids.
//!
//! See the module docs in [`rules`] for the exact rule set (from Michael Keith's
//! G4G16 paper). The public entry points return the total number of valid grids
//! for a given odd size `n`.

pub mod brute;
pub mod dp;
pub mod grid;
pub mod rules;

pub use rules::Style;

/// Count valid American-style n×n grids (OEIS A323839).
///
/// Currently backed by the brute-force oracle (intended for n ≤ 9); the
/// transfer-matrix DP replaces this in a later milestone.
pub fn count_american(n: usize) -> u128 {
    brute::count(n, Style::American)
}

/// Count valid British-style n×n grids (Keith, G4G16).
///
/// Currently backed by the brute-force oracle (intended for n ≤ 9).
pub fn count_british(n: usize) -> u128 {
    brute::count(n, Style::British)
}
