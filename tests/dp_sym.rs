//! M2 validation: the folded (symmetric) DP must reproduce the published
//! American counts (OEIS A323839) and agree with the brute-force oracle.

use crossword_grids::{brute, dp, Style};

#[test]
fn folded_matches_brute_5_7() {
    assert_eq!(
        dp::count_sym(5, Style::American),
        brute::count(5, Style::American)
    );
    assert_eq!(
        dp::count_sym(7, Style::American),
        brute::count(7, Style::American)
    );
}

#[test]
fn american_anchor_5_to_11() {
    assert_eq!(dp::count_sym(5, Style::American), 12);
    assert_eq!(dp::count_sym(7, Style::American), 312);
    assert_eq!(dp::count_sym(9, Style::American), 31_187);
    assert_eq!(dp::count_sym(11, Style::American), 17_438_702);
}

#[test]
#[ignore = "slow: ~25s"]
fn american_anchor_13() {
    assert_eq!(dp::count_sym(13, Style::American), 40_575_832_476);
}

#[test]
#[ignore = "slow: minutes"]
fn american_anchor_15() {
    assert_eq!(dp::count_sym(15, Style::American), 404_139_015_237_875);
}
