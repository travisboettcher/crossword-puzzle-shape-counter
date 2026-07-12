//! M1 cross-check: the non-symmetric connectivity DP must agree with the
//! non-symmetric brute force. This validates the connectivity + vertical-run
//! machinery independently of the symmetry folding.

use crossword_grids::{brute, dp, Style};

#[test]
fn american_nosym_matches_brute_n5() {
    assert_eq!(
        dp::count_nosym(5, Style::American),
        brute::count_nosym(5, Style::American)
    );
}

#[test]
fn american_nosym_matches_brute_n7() {
    assert_eq!(
        dp::count_nosym(7, Style::American),
        brute::count_nosym(7, Style::American)
    );
}
