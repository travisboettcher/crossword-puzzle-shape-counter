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

/// Input shards per batch (default; `BRITISH_DISK_BATCH` overrides). The memory
/// budget is only checked between batches, so one batch's new states can
/// overshoot it: keep batches small when a row is large. At 15×15, one row-4
/// input shard yields ~10^8 new row-5 states (~9 GB of maps).
const BATCH: usize = 8;

/// Input states per parallel work item.
const CHUNK: usize = 16384;

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
    let f = w.into_inner().expect("flush merge output");
    f.sync_all().expect("sync merge output");
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

// --- checkpointing ----------------------------------------------------------
//
// Layout under the data directory:
//   CONFIG             n / shard count / record format; resume refuses a mismatch
//   row-init/, rowI/   one directory per finished (or in-progress) row
//     sNNNN.bin        final shard files (sorted, merged)
//     rNNNN.bin        run files while the row is being built
//     tNNNN.bin        next run files during a spill
//     SPILL            spill written, renames t→r in progress (redo them)
//     CKPT             last completed checkpoint: input batches merged so far
//     DONE             row complete (state count)
//   glue/gNNNN         partial sum of the fused last row for input shard NNNN
//   RESULT             the final count
//
// Every marker is written to a temp file, synced and renamed, so a crash at
// any point leaves either the previous checkpoint or the next one.

/// Rehearsal sampling (`BRITISH_SAMPLE=row:k`): when building grid row `row`,
/// read only every `k`-th input shard. Everything downstream is exercised on
/// real states at full width, but the result is **not** a count.
fn sample() -> Option<(usize, usize)> {
    let v = std::env::var("BRITISH_SAMPLE").ok()?;
    let (r, k) = v.split_once(':').expect("BRITISH_SAMPLE=row:k");
    Some((r.parse().expect("sample row"), r_k(k)))
}

fn r_k(k: &str) -> usize {
    k.parse::<usize>().expect("sample k").max(1)
}

/// Output sampling (`BRITISH_SAMPLE_OUT=row:k`): when building `row`, generate
/// every successor but store only those in every `k`-th output shard. Measures
/// the full generation cost of a row while storing (and later gluing) a
/// sample of it. Also **not** a count.
fn sample_out() -> Option<(usize, usize)> {
    let v = std::env::var("BRITISH_SAMPLE_OUT").ok()?;
    let (r, k) = v.split_once(':').expect("BRITISH_SAMPLE_OUT=row:k");
    Some((r.parse().expect("sample row"), r_k(k)))
}

fn keep_input(row: usize, s: usize) -> bool {
    match sample() {
        Some((r, k)) if r == row => s.is_multiple_of(k),
        _ => true,
    }
}

/// Test hook: panic at a named point to simulate a crash.
static CRASH_AT: Mutex<Option<String>> = Mutex::new(None);

fn crash_point(tag: &str) {
    let hit = CRASH_AT.lock().unwrap().as_deref() == Some(tag)
        || std::env::var("BRITISH_DISK_CRASH_AT").as_deref() == Ok(tag);
    if hit {
        *CRASH_AT.lock().unwrap() = None;
        panic!("injected crash at {tag}");
    }
}

#[cfg(test)]
pub(super) fn set_crash(tag: Option<&str>) {
    *CRASH_AT.lock().unwrap() = tag.map(str::to_owned);
}

fn write_atomic(path: &Path, text: &str) {
    let tmp = path.with_extension("tmp");
    let mut f = File::create(&tmp).expect("create marker");
    f.write_all(text.as_bytes()).expect("write marker");
    f.sync_all().expect("sync marker");
    fs::rename(&tmp, path).expect("rename marker");
}

/// Checkpoint: input batches fully merged into the run files, the batch
/// length they were cut with, and each shard's run-file record count.
struct Ckpt {
    batches: usize,
    batch_len: usize,
    counts: Vec<usize>,
}

impl Ckpt {
    fn save(&self, path: &Path) {
        let counts: Vec<String> = self.counts.iter().map(|c| c.to_string()).collect();
        write_atomic(
            path,
            &format!(
                "{} {}\n{}\n",
                self.batches,
                self.batch_len,
                counts.join(" ")
            ),
        );
    }
    fn load(path: &Path) -> Option<Ckpt> {
        let text = fs::read_to_string(path).ok()?;
        let mut lines = text.lines();
        let mut head = lines.next()?.split_whitespace().map(|x| x.parse::<usize>());
        let batches = head.next()?.ok()?;
        let batch_len = head.next()?.ok()?;
        let counts: Vec<usize> = lines
            .next()?
            .split_whitespace()
            .map(|x| x.parse().expect("bad checkpoint"))
            .collect();
        assert_eq!(counts.len(), SHARDS, "checkpoint shard count");
        Some(Ckpt {
            batches,
            batch_len,
            counts,
        })
    }
}

/// Stream `input` through one transfer step into the on-disk row `out`,
/// resuming from `out`'s last checkpoint if there is one. Returns the row's
/// state count.
#[allow(clippy::too_many_arguments)]
fn advance_disk(
    ctx: &Ctx,
    input: &RowDir,
    out: &RowDir,
    rows: &[u32],
    row: usize,
    budget: usize,
    ckpt_every: std::time::Duration,
    spills: &AtomicUsize,
) -> usize {
    let n = ctx.n;
    let done = out.dir.join("DONE");
    if let Ok(t) = fs::read_to_string(&done) {
        return t.trim().parse().expect("bad DONE");
    }
    fs::create_dir_all(&out.dir).expect("create row dir");
    let run = |s: usize| out.dir.join(format!("r{s:04}.bin"));
    let tmp = |s: usize| out.dir.join(format!("t{s:04}.bin"));
    let (spill_mark, ckpt_mark) = (out.dir.join("SPILL"), out.dir.join("CKPT"));

    // Recover: finish an interrupted spill's renames, or drop a partial one.
    if let Some(c) = Ckpt::load(&spill_mark) {
        for s in 0..SHARDS {
            if tmp(s).exists() {
                fs::rename(tmp(s), run(s)).expect("redo rename");
            }
        }
        c.save(&ckpt_mark);
        let _ = fs::remove_file(&spill_mark);
    } else {
        for s in 0..SHARDS {
            let _ = fs::remove_file(tmp(s));
        }
    }
    let env_batch = std::env::var("BRITISH_DISK_BATCH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(BATCH)
        .max(1);
    let mut ck = Ckpt::load(&ckpt_mark).unwrap_or(Ckpt {
        batches: 0,
        batch_len: env_batch,
        counts: vec![0; SHARDS],
    });
    let shard_ids: Vec<usize> = (0..SHARDS).collect();
    let batches: Vec<&[usize]> = shard_ids.chunks(ck.batch_len).collect();
    if ck.batches > 0 && std::env::var("DP_STATS").is_ok() {
        eprintln!(
            "  row {row}: resuming at input batch {}/{}",
            ck.batches,
            batches.len()
        );
    }

    let allowed = super::allowed_set(n, rows);
    let keep_k = match sample_out() {
        Some((r, k)) if r == row => k,
        _ => 1,
    };
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
    // Compaction + checkpoint: merge every shard's map into a new run file,
    // mark the spill, swap the files in, record the checkpoint.
    let spill = |ck: &mut Ckpt, batches_done: usize| {
        let counts: Vec<usize> = maps
            .par_iter()
            .enumerate()
            .map(|(s, m)| {
                let mut m = m.lock().unwrap();
                if m.is_empty() {
                    return ck.counts[s];
                }
                let v = sorted(&mut m);
                drop(m);
                merge_to(&run(s), &v, &tmp(s), n)
            })
            .collect();
        spills.fetch_add(1, Ordering::Relaxed);
        crash_point(&format!("spill-written:{row}:{batches_done}"));
        let next = Ckpt {
            batches: batches_done,
            batch_len: ck.batch_len,
            counts,
        };
        next.save(&spill_mark);
        for s in 0..SHARDS {
            if s == SHARDS / 2 {
                crash_point(&format!("spill-renaming:{row}:{batches_done}"));
            }
            if tmp(s).exists() {
                fs::rename(tmp(s), run(s)).expect("replace run file");
            }
        }
        next.save(&ckpt_mark);
        let _ = fs::remove_file(&spill_mark);
        live.store(0, Ordering::Relaxed);
        *ck = next;
    };

    let mut last = std::time::Instant::now();
    for (b, batch) in batches.iter().enumerate().skip(ck.batches) {
        let parts: Vec<Vec<(Packed, u64)>> = batch
            .par_iter()
            .map(|&s| {
                if keep_input(row, s) {
                    input.load(s, n)
                } else {
                    Vec::new()
                }
            })
            .collect();
        // Parallelise within shards too, so a batch of one shard still uses
        // every core (small batches bound how far a batch can overshoot the
        // memory budget, which is only checked between batches).
        let chunks: Vec<&[(Packed, u64)]> = parts.iter().flat_map(|p| p.chunks(CHUNK)).collect();
        chunks.par_iter().for_each_init(
            || vec![Vec::<(Packed, u64)>::new(); SHARDS],
            |bufs, part| {
                for (p, cnt) in part.iter() {
                    successors(ctx, &ctx.unpack(p), row, &allowed, |ns| {
                        let pk = ctx.pack_canon(&ns);
                        let s = shard_of(&pk);
                        if keep_k > 1 && !s.is_multiple_of(keep_k) {
                            return;
                        }
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
        crash_point(&format!("batch:{row}:{b}"));
        if live.load(Ordering::Relaxed) > budget || last.elapsed() >= ckpt_every {
            spill(&mut ck, b + 1);
            last = std::time::Instant::now();
        }
    }
    // Final checkpoint (all batches merged), then the run files become the
    // row's shard files. Renames are idempotent, so this is safe to redo.
    if ck.batches < batches.len() || live.load(Ordering::Relaxed) > 0 {
        spill(&mut ck, batches.len());
    }
    for s in 0..SHARDS {
        if s == SHARDS / 2 {
            crash_point(&format!("finalizing:{row}"));
        }
        if run(s).exists() {
            fs::rename(run(s), out.shard(s)).expect("finalize shard");
        }
    }
    let total: usize = ck.counts.iter().sum();
    write_atomic(&done, &format!("{total}\n"));
    let _ = fs::remove_file(&ckpt_mark);
    total
}

/// Count British grids with every row stored on disk under `dir`.
///
/// Resumes automatically from whatever `dir` holds (finished rows, the last
/// checkpoint of a row in progress, finished last-row shards) as long as its
/// `CONFIG` matches; `fresh` wipes it first. A crash loses at most the work
/// since the last checkpoint (every spill, and at least every `ckpt_every`).
pub(super) fn count(
    ctx: &Ctx,
    rows: &[u32],
    dir: &Path,
    budget: usize,
    ckpt_every: std::time::Duration,
    fresh: bool,
    stats: bool,
) -> u128 {
    let n = ctx.n;
    let h = (n - 1) / 2;
    let t0 = std::time::Instant::now();
    let config = match sample() {
        None => format!("n={n} shards={SHARDS} format=1\n"),
        Some((r, k)) => format!("n={n} shards={SHARDS} format=1 SAMPLE row={r} keep=1/{k}\n"),
    };
    let config = match sample_out() {
        None => config,
        Some((r, k)) => format!("{} SAMPLE_OUT row={r} keep=1/{k}\n", config.trim_end()),
    };
    if (sample().is_some() || sample_out().is_some()) && stats {
        eprintln!("  REHEARSAL (BRITISH_SAMPLE): the result below is NOT a grid count");
    }
    if fresh {
        let _ = fs::remove_dir_all(dir);
    }
    match fs::read_to_string(dir.join("CONFIG")) {
        Ok(c) if c == config => {
            if stats {
                eprintln!("  resuming from {}", dir.display());
            }
        }
        Ok(c) => panic!(
            "{} holds a different run ({}); set BRITISH_DISK_FRESH=1 to discard it",
            dir.display(),
            c.trim()
        ),
        Err(_) => {
            let _ = fs::remove_dir_all(dir);
            fs::create_dir_all(dir).expect("create data dir");
            write_atomic(&dir.join("CONFIG"), &config);
        }
    }
    if let Ok(r) = fs::read_to_string(dir.join("RESULT")) {
        return r.trim().parse().expect("bad RESULT");
    }
    let spills = AtomicUsize::new(0);
    let row_dir = |i: Option<usize>| RowDir {
        dir: dir.join(match i {
            Some(i) => format!("row{i}"),
            None => "row-init".to_string(),
        }),
    };
    // Start after the latest finished row (earlier rows are already deleted).
    let first = (0..h - 1)
        .rev()
        .find(|&i| row_dir(Some(i)).dir.join("DONE").exists())
        .map_or(0, |i| i + 1);
    let mut cur = row_dir(first.checked_sub(1));
    if first == 0 && !cur.dir.join("DONE").exists() {
        fs::create_dir_all(&cur.dir).expect("create dir");
        write_all(&cur.shard(0), &[([0, 0], 1)], n);
        write_atomic(&cur.dir.join("DONE"), "1\n");
    }
    for i in 0..first.saturating_sub(1) {
        let _ = fs::remove_dir_all(row_dir(Some(i)).dir); // leftovers of a crash
    }
    for i in first..h - 1 {
        let next = row_dir(Some(i));
        let states = advance_disk(ctx, &cur, &next, rows, i, budget, ckpt_every, &spills);
        crash_point(&format!("row-done:{i}"));
        let _ = fs::remove_dir_all(&cur.dir);
        if stats {
            eprintln!(
                "  row {i}: {states} states, {:.1} MB on disk, {} spills so far ({:.2?})",
                next.bytes() as f64 / 1e6,
                spills.load(Ordering::Relaxed),
                t0.elapsed()
            );
        }
        cur = next;
    }
    // Fused last row, one input shard at a time; each shard's partial sum is
    // saved so a restart skips it.
    let centers: Vec<u32> = rows
        .iter()
        .copied()
        .filter(|&m| is_palindrome(m, n))
        .collect();
    let allowed = super::allowed_set(n, rows);
    let glue = dir.join("glue");
    fs::create_dir_all(&glue).expect("create glue dir");
    let total: u128 = (0..SHARDS)
        .into_par_iter()
        .map(|s| {
            let g = glue.join(format!("g{s:04}"));
            if let Ok(t) = fs::read_to_string(&g) {
                return t.trim().parse::<u128>().expect("bad glue sum");
            }
            let sum = advance_glue_part(ctx, &cur.load(s, n), h - 1, &allowed, &centers);
            write_atomic(&g, &format!("{sum}\n"));
            crash_point(&format!("glue:{s}"));
            sum
        })
        .sum();
    write_atomic(&dir.join("RESULT"), &format!("{total}\n"));
    let _ = fs::remove_dir_all(&cur.dir);
    if stats {
        eprintln!("  row {} fused with glue ({:.2?})", h - 1, t0.elapsed());
    }
    total
}
