//! CLI: count valid crossword grids of a given style and size.
//!
//! Usage:
//!   count --style american --n 9
//!   count --style british  --n 7

use std::time::Instant;

use crossword_grids::{brute, Style};

fn main() {
    let mut style = Style::American;
    let mut n: usize = 9;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--style" | "-s" => {
                let v = args.next().expect("--style needs a value");
                style = match v.as_str() {
                    "american" | "a" => Style::American,
                    "british" | "b" => Style::British,
                    other => panic!("unknown style: {other}"),
                };
            }
            "--n" | "-n" => {
                n = args
                    .next()
                    .expect("--n needs a value")
                    .parse()
                    .expect("n must be a positive integer");
            }
            "--help" | "-h" => {
                eprintln!("usage: count --style <american|british> --n <odd size>");
                return;
            }
            other => panic!("unknown argument: {other}"),
        }
    }

    let start = Instant::now();
    // For now the CLI drives the brute-force oracle; the DP will be wired in later.
    let total = brute::count(n, style);
    let elapsed = start.elapsed();
    println!(
        "{} {}x{}: {} valid grids  ({:.3?})",
        style.as_str(),
        n,
        n,
        total,
        elapsed
    );
}
