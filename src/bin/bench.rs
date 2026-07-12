use crossword_grids::{dp, Style};
use std::time::Instant;
fn main() {
    let mut a = std::env::args().skip(1);
    let style = match a.next().as_deref() {
        Some("british") => Style::British,
        _ => Style::American,
    };
    let n: usize = a.next().unwrap().parse().unwrap();
    let t = Instant::now();
    let got = dp::count_sym(n, style);
    println!("{} n={n}: {got} ({:.2?})", style.as_str(), t.elapsed());
}
