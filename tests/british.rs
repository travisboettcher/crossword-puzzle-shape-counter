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
#[ignore = "slow: ~10 s, ~7M frontier states"]
fn british_paper_count_11() {
    assert_eq!(british::count(11, Style::British), 60_384_181);
}

/// Splitting the last two rows into passes must not change the count.
#[test]
fn british_passes_do_not_change_count() {
    for (rp, p) in [(2, 3), (3, 1), (1, 4)] {
        assert_eq!(british::count_with_passes(9, rp, p), 68_956);
    }
}

#[test]
#[ignore = "slow: ~2 h on 4 cores, ~12 GB peak; 24M/191M/672M frontier states"]
fn british_paper_count_13() {
    // Four passes over the last row keep the peak under 12 GB.
    assert_eq!(british::count_with_passes(13, 1, 4), 162_468_835_136);
}
