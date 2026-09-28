//! Cell-by-cell ("broken profile") transfer for the British DP (experiment E1).
//!
//! Instead of placing a whole row at once, row `i` is placed one cell at a
//! time, merging equal states after every cell. Mid-row, column `c < j` holds
//! the new row's cell and column `c ≥ j` still holds row `i − 1`'s. Extra state:
//!   * the **pending** column `j − 1`: its new cell's checked status needs its
//!     right neighbour, so its old run statistic is kept until cell `j` lands;
//!   * `hw`: the statistic of row `i − 1`'s horizontal word in progress (its
//!     cells' checked status is known once the cell below is placed), using the
//!     same `(len, trail, d)` sufficient statistic as vertical words;
//!   * `hr`: row `i`'s current horizontal run length (length 2 is forbidden).
//!
//! At the end of a row the state is exactly a row-DP frontier, so both methods
//! must produce the same state sets row by row; a unit test checks this.

use std::sync::Mutex;

use ahash::AHashMap;
use rayon::prelude::*;

use super::{
    col_d, col_label, col_len, col_trail, pack_col, shard_of, vrun_close_ok, Ctx, Key, Packed,
    DBIAS, FLUSH, MAXN, SHARDS, SLOTS,
};

/// Mid-row state (wide form).
#[derive(Clone, Copy)]
struct CState {
    /// per column: `pack_col` value; the pending column holds `label | PEND`
    col: [u16; MAXN],
    /// old run statistic (low byte) of the pending column, when it is white
    pend: u16,
    pend_white: bool,
    /// row i−1 horizontal word in progress (low byte of `pack_col`, 0 = none)
    hw: u16,
    /// row i horizontal run length, capped at 3
    hr: u8,
    flags: u16,
}

/// Low byte marking a pending (white, not yet finalized) new cell.
const PEND: u16 = 0b11; // len 3, trail 0, d −DBIAS: never a real statistic

type CKey = ([u64; 2], u32);
type CMap = AHashMap<CKey, u64>;

impl CState {
    fn from_row(ctx: &Ctx, st: &Key) -> CState {
        let mut col = [0u16; MAXN];
        col[..ctx.n].copy_from_slice(&st[..ctx.n]);
        CState {
            col,
            pend: 0,
            pend_white: false,
            hw: 0,
            hr: 0,
            flags: st[ctx.n],
        }
    }

    fn pack(&self, ctx: &Ctx) -> CKey {
        let mut k = 0u128;
        for j in 0..ctx.n {
            let c = self.col[j];
            if col_len(c) > 0 {
                let low = c & 0xff;
                let code = if low == PEND {
                    31
                } else {
                    ctx.code[low as usize] as u128
                };
                debug_assert!(code != 0);
                k |= (code | (((col_label(c) - 1) as u128) << 5)) << (8 * j);
            }
        }
        let pend = if self.pend_white {
            ctx.code[self.pend as usize] as u32
        } else {
            0
        };
        let extra = pend
            | (self.hw as u32) << 5
            | (self.hr as u32) << 13
            | (self.flags as u32) << 15
            | (self.pend_white as u32) << 17;
        ([k as u64, (k >> 64) as u64], extra)
    }

    fn unpack(ctx: &Ctx, key: &CKey) -> CState {
        let k = key.0[0] as u128 | ((key.0[1] as u128) << 64);
        let mut col = [0u16; MAXN];
        for (j, c) in col.iter_mut().enumerate().take(ctx.n) {
            let b = (k >> (8 * j)) as u8;
            let code = (b & 0x1f) as usize;
            if code != 0 {
                let low = if code == 31 { PEND } else { ctx.stat[code] };
                *c = low | ((((b >> 5) + 1) as u16) << 8);
            }
        }
        let e = key.1;
        let pc = (e & 0x1f) as usize;
        CState {
            col,
            pend: if pc != 0 { ctx.stat[pc] } else { 0 },
            pend_white: (e >> 17) & 1 == 1,
            hw: ((e >> 5) & 0xff) as u16,
            hr: ((e >> 13) & 0b11) as u8,
            flags: ((e >> 15) & 0b11) as u16,
        }
    }
}

/// Extend a word statistic by one checked/unchecked cell, or `None` if the
/// growth rules (Rule 7, Rule-8 start) or the `rem`-cell feasibility forbid it.
#[inline]
fn grow(ctx: &Ctx, s: u16, checked: bool, rem: usize) -> Option<u16> {
    let (l, t, d) = (col_len(s), col_trail(s), col_d(s));
    let (nl, nt, nd) = if l == 0 {
        if checked {
            (1, 0, 1)
        } else {
            (1, 1, -1)
        }
    } else if checked {
        ((l + 1).min(3), 0, d + 1)
    } else {
        if t + 1 >= 3 || (l == 1 && t == 1) {
            return None;
        }
        ((l + 1).min(3), t + 1, d - 1)
    };
    if nd + DBIAS < 0 || nd + DBIAS >= 16 || !ctx.alive(nl, nt, nd, rem) {
        return None;
    }
    Some(pack_col(0, nl, nt, nd))
}

/// Relabel components in first-appearance order (labels in bits 8..12).
#[inline]
fn relabel(col: &mut [u16; MAXN], n: usize) {
    let mut map = [0u8; 32];
    let mut next = 0u8;
    for c in col.iter_mut().take(n) {
        if col_len(*c) > 0 {
            let l = col_label(*c) as usize;
            if map[l] == 0 {
                next += 1;
                map[l] = next;
            }
            *c = (*c & 0xff) | ((map[l] as u16) << 8);
        }
    }
}

/// Finalize the pending column `p` (its right neighbour is now known).
#[inline]
fn finalize(ctx: &Ctx, s: &mut CState, p: usize, right_white: bool, row: usize) -> bool {
    if !s.pend_white {
        return true;
    }
    let left_white = p > 0 && col_len(s.col[p - 1]) > 0;
    let checked = left_white || right_white;
    match grow(ctx, s.pend, checked, ctx.n - 1 - row) {
        None => false,
        Some(st) => {
            let st = ctx.canon[row][st as usize] as u16;
            s.col[p] = (s.col[p] & 0xff00) | st;
            s.pend_white = false;
            s.pend = 0;
            true
        }
    }
}

/// Place cell `(row, j)`, white or black.
fn place(ctx: &Ctx, st: &CState, row: usize, j: usize, white: bool) -> Option<CState> {
    let n = ctx.n;
    let mut s = *st;
    let old = st.col[j];
    let up_white = col_len(old) > 0;

    // Row above: its cell (row−1, j) is now fully known.
    if up_white {
        let checked = col_len(old) >= 2 || white;
        s.hw = grow(ctx, s.hw, checked, n - 1 - j)?;
    } else if s.hw != 0 {
        if !vrun_close_ok(col_len(s.hw), col_trail(s.hw), col_d(s.hw)) {
            return None;
        }
        s.hw = 0;
    }
    // Vertical run closing here.
    if up_white && !white && !vrun_close_ok(col_len(old), col_trail(old), col_d(old)) {
        return None;
    }
    // The pending column j−1 learns its right neighbour.
    if j > 0 && !finalize(ctx, &mut s, j - 1, white, row) {
        return None;
    }
    // Row i horizontal run: no length-2 words.
    if white {
        s.hr = (s.hr + 1).min(3);
    } else {
        if s.hr == 2 {
            return None;
        }
        s.hr = 0;
    }
    // Connectivity.
    if white {
        let fresh = 15u16; // unused 4-bit label, relabelled below
        let mut lab = fresh;
        let left = if j > 0 && col_len(s.col[j - 1]) > 0 {
            Some(col_label(s.col[j - 1]) as u16)
        } else {
            None
        };
        let up = if up_white {
            Some(col_label(old) as u16)
        } else {
            None
        };
        match (left, up) {
            (Some(a), Some(b)) => {
                lab = a;
                if a != b {
                    for c in s.col.iter_mut().take(n) {
                        if col_len(*c) > 0 && col_label(*c) as u16 == b {
                            *c = (*c & 0xff) | (a << 8);
                        }
                    }
                }
            }
            (Some(a), None) | (None, Some(a)) => lab = a,
            (None, None) => {}
        }
        s.col[j] = PEND | (lab << 8);
        s.pend_white = true;
        s.pend = old & 0xff;
    } else {
        if up_white {
            let l = col_label(old);
            let elsewhere =
                (0..n).any(|c| c != j && col_len(s.col[c]) > 0 && col_label(s.col[c]) == l);
            if !elsewhere {
                return None; // component sealed off
            }
        }
        s.col[j] = 0;
        s.pend_white = false;
        s.pend = 0;
    }
    relabel(&mut s.col, n);
    if white && j == 0 {
        s.flags |= 1;
    }
    if white && j == n - 1 {
        s.flags |= 2;
    }
    Some(s)
}

/// Close the row after its last cell; returns the row-DP frontier.
fn end_row(ctx: &Ctx, st: &CState, row: usize) -> Option<Key> {
    let n = ctx.n;
    let mut s = *st;
    if !finalize(ctx, &mut s, n - 1, false, row) {
        return None;
    }
    if s.hw != 0 && !vrun_close_ok(col_len(s.hw), col_trail(s.hw), col_d(s.hw)) {
        return None;
    }
    if s.hr == 2 {
        return None;
    }
    if (0..n).all(|c| col_len(s.col[c]) == 0) {
        return None; // an all-black row seals everything (or leaves row 0 empty)
    }
    let mut k = [0u16; SLOTS];
    k[..n].copy_from_slice(&s.col[..n]);
    k[n] = s.flags;
    Some(k)
}

fn merge_step<I, F>(input: I, f: F) -> Vec<Vec<(CKey, u64)>>
where
    I: IndexedParallelIterator,
    I::Item: IntoIterator<Item = (CKey, u64)>,
    F: Fn(&(CKey, u64), &mut dyn FnMut(CKey, u64)) + Sync,
{
    let shard = |k: &CKey| shard_of(&[k.0[0] ^ (k.1 as u64).rotate_left(17), k.0[1]]);
    let shards: Vec<Mutex<CMap>> = (0..SHARDS).map(|_| Mutex::new(CMap::default())).collect();
    input.for_each_init(
        || vec![Vec::<(CKey, u64)>::new(); SHARDS],
        |bufs, part| {
            for e in part {
                f(&e, &mut |k, v| {
                    let s = shard(&k);
                    bufs[s].push((k, v));
                    if bufs[s].len() >= FLUSH {
                        let mut m = shards[s].lock().unwrap();
                        for (k, v) in bufs[s].drain(..) {
                            *m.entry(k).or_insert(0) += v;
                        }
                    }
                });
            }
            for (s, b) in bufs.iter_mut().enumerate() {
                let mut m = shards[s].lock().unwrap();
                for (k, v) in b.drain(..) {
                    *m.entry(k).or_insert(0) += v;
                }
            }
        },
    );
    shards
        .into_iter()
        .map(|m| m.into_inner().unwrap().into_iter().collect())
        .collect()
}

/// Build grid row `row` cell by cell from row-DP frontiers; returns the new
/// row's frontiers (packed + mirror-canonical, merged) and the largest
/// mid-row layer.
pub(super) fn advance_cells(
    ctx: &Ctx,
    input: &[Vec<(Packed, u64)>],
    row: usize,
) -> (Vec<Vec<(Packed, u64)>>, usize) {
    let n = ctx.n;
    let mut layer: Vec<Vec<(CKey, u64)>> = input
        .par_iter()
        .map(|part| {
            part.iter()
                .map(|(p, c)| (CState::from_row(ctx, &ctx.unpack(p)).pack(ctx), *c))
                .collect()
        })
        .collect();
    let mut widest = 0usize;
    for j in 0..n {
        layer = merge_step(layer.into_par_iter(), |(k, c), emit| {
            let st = CState::unpack(ctx, k);
            for white in [false, true] {
                if let Some(ns) = place(ctx, &st, row, j, white) {
                    emit(ns.pack(ctx), *c);
                }
            }
        });
        widest = widest.max(layer.iter().map(Vec::len).sum());
    }
    // close the row and hand back row-DP frontiers
    let shards: Vec<Mutex<super::Map>> = (0..SHARDS)
        .map(|_| Mutex::new(super::Map::default()))
        .collect();
    layer.par_iter().for_each(|part| {
        let mut local: Vec<(Packed, u64)> = Vec::new();
        for (k, c) in part {
            if let Some(key) = end_row(ctx, &CState::unpack(ctx, k), row) {
                local.push((ctx.pack_canon(&key), *c));
            }
        }
        for (p, c) in local {
            *shards[shard_of(&p)].lock().unwrap().entry(p).or_insert(0) += c;
        }
    });
    let out = shards
        .into_iter()
        .map(|m| m.into_inner().unwrap().into_iter().collect())
        .collect();
    (out, widest)
}
