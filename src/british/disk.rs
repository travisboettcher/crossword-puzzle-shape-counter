//! External-memory British DP (experiments E4 + E6).
//!
//! Each finished row lives on disk as `SHARDS` files, each holding that
//! shard's states sorted and merged. The next row is built by streaming input
//! shards in batches into sharded hash maps; whenever the maps exceed `budget`
//! entries, every shard is sorted and merged into its single run file
//! ("spill" with compaction: the run file always holds each distinct state
//! once). At the end of the row each run file is merged with what is still in
//! memory into the shard's final file. Peak memory is about `budget` map entries plus one input batch,
//! regardless of row size; disk holds the input row, the output row and the
//! spilled runs.
//!
//! Records use the compact encoding of experiment E6: per column a 5-bit run
//! statistic code and a 2-bit non-crossing connectivity code (singleton /
//! opens / continues / closes a component), then the two edge flags, rounded
//! up to whole bytes (12 bytes at n = 13, 14 at n = 15), followed by the count
//! as a LEB128 varint.

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use rayon::prelude::*;

use super::{
    advance_glue_part, is_palindrome, shard_of, successors, Ctx, Map, Packed, FLUSH, SHARDS,
    STAT_BITS,
};

/// Input shards loaded into memory at once while building a row (default;
/// `BRITISH_DISK_BATCH` overrides). Each loaded state takes 24 bytes, so at
/// 15×15 row 5 (~25M states per shard) 64 shards would need ~38 GB.
const BATCH: usize = 64;

// --- compact record encoding (E6) -------------------------------------------

fn key_bytes(n: usize) -> usize {
    (7 * n + 2).div_ceil(8)
}

/// Packed (5-bit code + 3-bit label per column) → 7 bits per column.
pub(super) fn compact(p: &Packed, n: usize) -> u128 {
    let k = p[0] as u128 | ((p[1] as u128) << 64);
    let mut first = [usize::MAX; 16];
    let mut last = [0usize; 16];
    for j in 0..n {
        let b = (k >> (8 * j)) as u8;
        if b & 0x1f != 0 {
            let l = (b >> STAT_BITS) as usize;
            if first[l] == usize::MAX {
                first[l] = j;
            }
            last[l] = j;
        }
    }
    let mut out = 0u128;
    for j in 0..n {
        let b = (k >> (8 * j)) as u8;
        let code = (b & 0x1f) as u128;
        if code != 0 {
            let l = (b >> STAT_BITS) as usize;
            let conn = match (first[l] == j, last[l] == j) {
                (true, true) => 0,   // singleton
                (true, false) => 1,  // opens
                (false, false) => 2, // continues
                (false, true) => 3,  // closes
            };
            out |= (code | (conn << 5)) << (7 * j);
        }
    }
    out | (((k >> 120) & 0b11) << (7 * n))
}

/// Inverse of [`compact`]; labels come back in first-appearance order, which
/// is the canonical labelling, because frontier components never cross.
pub(super) fn expand(c: u128, n: usize) -> Packed {
    let mut k = 0u128;
    let mut stack = [0u8; 16];
    let mut sp = 0usize;
    let mut next = 0u8;
    for j in 0..n {
        let b = (c >> (7 * j)) as u8 & 0x7f;
        let code = b & 0x1f;
        if code == 0 {
            continue;
        }
        let label = match b >> 5 {
            0 => {
                next += 1;
                next - 1
            }
            1 => {
                next += 1;
                stack[sp] = next - 1;
                sp += 1;
                next - 1
            }
            2 => stack[sp - 1],
            _ => {
                sp -= 1;
                stack[sp]
            }
        };
        k |= ((code | (label << STAT_BITS)) as u128) << (8 * j);
    }
    k |= ((c >> (7 * n)) & 0b11) << 120;
    [k as u64, (k >> 64) as u64]
}

fn write_rec(w: &mut impl Write, p: &Packed, cnt: u64, n: usize) -> std::io::Result<()> {
    let c = compact(p, n);
    w.write_all(&c.to_le_bytes()[..key_bytes(n)])?;
    let mut v = cnt;
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            w.write_all(&[byte])?;
            return Ok(());
        }
        w.write_all(&[byte | 0x80])?;
    }
}

/// Streaming reader over a record file (empty if the file does not exist).
struct RecReader {
    r: Option<BufReader<File>>,
    n: usize,
}

impl RecReader {
    fn open(path: &Path, n: usize) -> RecReader {
        let r = File::open(path)
            .ok()
            .map(|f| BufReader::with_capacity(1 << 20, f));
        RecReader { r, n }
    }
}

impl Iterator for RecReader {
    type Item = (Packed, u64);
    fn next(&mut self) -> Option<(Packed, u64)> {
        let r = self.r.as_mut()?;
        let kb = key_bytes(self.n);
        let mut buf = [0u8; 16];
        if r.read_exact(&mut buf[..kb]).is_err() {
            return None;
        }
        let p = expand(u128::from_le_bytes(buf), self.n);
        let (mut cnt, mut shift) = (0u64, 0);
        loop {
            let mut b = [0u8; 1];
            r.read_exact(&mut b).expect("truncated record");
            cnt |= ((b[0] & 0x7f) as u64) << shift;
            shift += 7;
            if b[0] & 0x80 == 0 {
                return Some((p, cnt));
            }
        }
    }
}

fn read_all(path: &Path, n: usize, out: &mut Vec<(Packed, u64)>) {
    out.extend(RecReader::open(path, n));
}

/// Merge the sorted, reduced file `old` (may be missing) with the sorted,
/// reduced `mem`, summing equal keys, streaming into `out`. Returns the
/// number of records written. `old` is only read, never loaded whole.
fn merge_to(old: &Path, mem: &[(Packed, u64)], out: &Path, n: usize) -> usize {
    let f = File::create(out).expect("create merge output");
    let mut w = BufWriter::with_capacity(1 << 20, f);
    let mut a = RecReader::open(old, n).peekable();
    let mut b = mem.iter().copied().peekable();
    let mut written = 0usize;
    loop {
        let next = match (a.peek(), b.peek()) {
            (None, None) => break,
            (Some(_), None) => a.next(),
            (None, Some(_)) => b.next(),
            (Some(x), Some(y)) => {
                if x.0 < y.0 {
                    a.next()
                } else if y.0 < x.0 {
                    b.next()
                } else {
                    let (k, c1) = a.next().unwrap();
                    let (_, c2) = b.next().unwrap();
                    Some((k, c1 + c2))
                }
            }
        };
        let (k, c) = next.unwrap();
        write_rec(&mut w, &k, c, n).expect("write merge output");
        written += 1;
    }
    w.flush().expect("flush merge output");
    written
}

/// Drain a shard map into a key-sorted vector.
fn sorted(m: &mut Map) -> Vec<(Packed, u64)> {
    let mut v: Vec<(Packed, u64)> = m.drain().collect();
    m.shrink_to_fit();
    v.sort_unstable_by_key(|e| e.0);
    v
}

fn write_all(path: &Path, v: &[(Packed, u64)], n: usize) {
    let f = File::create(path).expect("create shard file");
    let mut w = BufWriter::with_capacity(1 << 20, f);
    for (p, c) in v {
        write_rec(&mut w, p, *c, n).expect("write shard file");
    }
    w.flush().expect("flush shard file");
}

// --- one row on disk ----------------------------------------------------------

struct RowDir {
    dir: PathBuf,
}

impl RowDir {
    fn shard(&self, s: usize) -> PathBuf {
        self.dir.join(format!("s{s:04}.bin"))
    }
    fn load(&self, s: usize, n: usize) -> Vec<(Packed, u64)> {
        let mut v = Vec::new();
        read_all(&self.shard(s), n, &mut v);
        v
    }
    fn bytes(&self) -> u64 {
        fs::read_dir(&self.dir)
            .map(|d| {
                d.flatten()
                    .filter_map(|e| e.metadata().ok())
                    .map(|m| m.len())
                    .sum()
            })
            .unwrap_or(0)
    }
}

/// Stream `input` through one transfer step into a new on-disk row.
fn advance_disk(
    ctx: &Ctx,
    input: &RowDir,
    out: RowDir,
    rows: &[u32],
    row: usize,
    budget: usize,
    spills: &AtomicUsize,
) -> (RowDir, usize) {
    let n = ctx.n;
    fs::create_dir_all(&out.dir).expect("create row dir");
    let allowed = super::allowed_set(n, rows);
    let run = |s: usize| out.dir.join(format!("r{s:04}.bin"));
    let maps: Vec<Mutex<Map>> = (0..SHARDS).map(|_| Mutex::new(Map::default())).collect();
    let live = AtomicUsize::new(0);
    let flush = |buf: &mut Vec<(Packed, u64)>, s: usize| {
        let mut m = maps[s].lock().unwrap();
        let before = m.len();
        for (k, v) in buf.drain(..) {
            *m.entry(k).or_insert(0) += v;
        }
        live.fetch_add(m.len() - before, Ordering::Relaxed);
    };
    let tmp = |s: usize| out.dir.join(format!("t{s:04}.bin"));
    // Compaction: each spill merges into the shard's single run file, so a
    // shard never holds more than one copy of the distinct states seen so far.
    let spill = || {
        maps.par_iter().enumerate().for_each(|(s, m)| {
            let mut m = m.lock().unwrap();
            if m.is_empty() {
                return;
            }
            let v = sorted(&mut m);
            drop(m);
            merge_to(&run(s), &v, &tmp(s), n);
            fs::rename(tmp(s), run(s)).expect("replace run file");
        });
        live.store(0, Ordering::Relaxed);
        spills.fetch_add(1, Ordering::Relaxed);
    };
    let batch_len = std::env::var("BRITISH_DISK_BATCH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(BATCH)
        .max(1);
    for batch in (0..SHARDS).collect::<Vec<_>>().chunks(batch_len) {
        let parts: Vec<Vec<(Packed, u64)>> = batch.par_iter().map(|&s| input.load(s, n)).collect();
        parts.par_iter().for_each_init(
            || vec![Vec::<(Packed, u64)>::new(); SHARDS],
            |bufs, part| {
                for (p, cnt) in part {
                    successors(ctx, &ctx.unpack(p), row, &allowed, |ns| {
                        let pk = ctx.pack_canon(&ns);
                        let s = shard_of(&pk);
                        bufs[s].push((pk, *cnt));
                        if bufs[s].len() >= FLUSH {
                            flush(&mut bufs[s], s);
                        }
                    });
                }
                for (s, b) in bufs.iter_mut().enumerate() {
                    if !b.is_empty() {
                        flush(b, s);
                    }
                }
            },
        );
        drop(parts);
        if live.load(Ordering::Relaxed) > budget {
            spill();
        }
    }
    // Final merge: each shard's run file with what is still in memory.
    let total = AtomicUsize::new(0);
    maps.into_par_iter().enumerate().for_each(|(s, m)| {
        let v = sorted(&mut m.into_inner().unwrap());
        total.fetch_add(merge_to(&run(s), &v, &out.shard(s), n), Ordering::Relaxed);
        let _ = fs::remove_file(run(s));
    });
    (out, total.into_inner())
}

/// Count British grids with every row stored on disk under `dir`.
pub(super) fn count(ctx: &Ctx, rows: &[u32], dir: &Path, budget: usize, stats: bool) -> u128 {
    let n = ctx.n;
    let h = (n - 1) / 2;
    let t0 = std::time::Instant::now();
    let spills = AtomicUsize::new(0);
    let row_dir = |i: usize| RowDir {
        dir: dir.join(format!("row{i}")),
    };
    let _ = fs::remove_dir_all(dir);
    let mut cur = row_dir(usize::MAX);
    fs::create_dir_all(&cur.dir).expect("create dir");
    write_all(&cur.shard(0), &[([0, 0], 1)], n);
    for i in 0..h - 1 {
        let (next, states) = advance_disk(ctx, &cur, row_dir(i), rows, i, budget, &spills);
        if stats {
            eprintln!(
                "  row {i}: {states} states, {:.1} MB on disk, {} spills so far ({:.2?})",
                next.bytes() as f64 / 1e6,
                spills.load(Ordering::Relaxed),
                t0.elapsed()
            );
        }
        let _ = fs::remove_dir_all(&cur.dir);
        cur = next;
    }
    let centers: Vec<u32> = rows
        .iter()
        .copied()
        .filter(|&m| is_palindrome(m, n))
        .collect();
    let allowed = super::allowed_set(n, rows);
    let total: u128 = (0..SHARDS)
        .into_par_iter()
        .map(|s| advance_glue_part(ctx, &cur.load(s, n), h - 1, &allowed, &centers))
        .sum();
    if stats {
        eprintln!("  row {} fused with glue ({:.2?})", h - 1, t0.elapsed());
    }
    let _ = fs::remove_dir_all(dir);
    total
}
