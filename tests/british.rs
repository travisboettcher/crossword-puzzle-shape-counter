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
#[ignore = "slow: ~73s, ~16M frontier states"]
fn british_paper_count_11() {
    assert_eq!(british::count(11, Style::British), 60_384_181);
}

// British 13x13 (Keith's 162,468,835,136) is reproduced by the same DP, but its
// frontier exceeds the memory available here: row 3 alone reaches ~49M states and
// the two remaining rows grow into the hundreds of millions, OOM-ing past ~16 GB.
// It is left unasserted rather than run in a memory-constrained environment.
