//! Brute-force oracle validation against the published reference counts.
//!
//! The fast suite covers n = 5 and 7 for both styles. The n = 9 cases are
//! correct but slower, so they are `#[ignore]`d by default; run them with
//! `cargo test -- --ignored`.

use crossword_grids::{brute, Style};

#[test]
fn american_5_and_7() {
    assert_eq!(brute::count(5, Style::American), 12);
    assert_eq!(brute::count(7, Style::American), 312);
}

#[test]
fn british_5_and_7() {
    assert_eq!(brute::count(5, Style::British), 17);
    assert_eq!(brute::count(7, Style::British), 650);
}

#[test]
#[ignore = "slow: brute force at n=9"]
fn american_9() {
    assert_eq!(brute::count(9, Style::American), 31_187);
}

#[test]
#[ignore = "slow: brute force at n=9"]
fn british_9() {
    assert_eq!(brute::count(9, Style::British), 68_956);
}
