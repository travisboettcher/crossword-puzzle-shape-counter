//! M4 validation: the folded British DP must agree with the brute-force oracle
//! and reproduce Keith's published `#Total` counts.

use crossword_grids::{british, brute, Style};

#[test]
fn british_matches_brute_5_7() {
    assert_eq!(
        british::count(5, Style::British),
        brute::count(5, Style::British)
    );
    assert_eq!(
        british::count(7, Style::British),
        brute::count(7, Style::British)
    );
}

#[test]
fn british_paper_counts_5_to_9() {
    assert_eq!(british::count(5, Style::British), 17);
    assert_eq!(british::count(7, Style::British), 650);
    assert_eq!(british::count(9, Style::British), 68_956);
}

#[test]
#[ignore = "slow: minutes"]
fn british_paper_counts_11_13() {
    assert_eq!(british::count(11, Style::British), 60_384_181);
    assert_eq!(british::count(13, Style::British), 162_468_835_136);
}
