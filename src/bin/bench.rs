use crossword_grids::{british, dp, Style};
use std::time::Instant;
fn main() {
    let mut a = std::env::args().skip(1);
    let style = match a.next().as_deref() {
        Some("british") => Style::British,
        _ => Style::American,
    };
    let n: usize = a.next().unwrap().parse().unwrap();
    let t = Instant::now();
    let got = match style {
        Style::American => dp::count_sym(n, style),
        Style::British => british::count(n, style),
    };
    println!("{} n={n}: {got} ({:.2?})", style.as_str(), t.elapsed());
}
