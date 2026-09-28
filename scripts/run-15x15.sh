#!/usr/bin/env bash
# Run the British-style 15x15 crossword grid count on a large machine.
#
# Usage:  scripts/run-15x15.sh <command>
#
#   check      machine resources, build, fast test suite
#   smoke      13x13 in disk mode; must reproduce Keith's 162,468,835,136
#   scaling    13x13 at 1/8, 1/4, 1/2 and all cores (is it worth more cores?)
#   rehearse   15x15 on a 1/1024 sample through the last row (NOT a count):
#              exercises every 15x15 code path and measures per-row cost
#   run        the real 15x15 count, in the background; rerun to resume
#   status     progress of a running / interrupted run
#   verify     second full run with a different memory budget and batch size;
#              must give the same total and identical per-shard partial sums
#   export     bundle logs, results and machine info into one small .tar.gz
#   all        check, smoke, scaling, rehearse, then start run
#
# Settings (environment variables):
#   DATA_DIR     where rows live on disk; needs ~600 GB free for 15x15
#                (default: ./crossword-data). Put it on local NVMe.
#   RESULTS_DIR  logs and small result files (default: ./crossword-results)
#   N            grid size (default 15)
#   THREADS      worker threads (default: all cores)
#   BUDGET       hash-map entries before spilling (default: from free RAM)
#   BATCH        input shards loaded at once (default: from free RAM)
#   FORCE=1      start even if the resource checks fail
#
# The run is resumable: if the machine or process dies, run `run` again with
# the same DATA_DIR. At most the work since the last checkpoint is lost
# (every spill, and at least every 30 minutes).

set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA_DIR="${DATA_DIR:-$PWD/crossword-data}"
RESULTS_DIR="${RESULTS_DIR:-$PWD/crossword-results}"
N="${N:-15}"
THREADS="${THREADS:-$(nproc)}"
BIN="$REPO/target/release/bench"
LOG="$RESULTS_DIR/run-${N}.log"
PIDFILE="$RESULTS_DIR/run-${N}.pid"
KEITH_13=162468835136

mkdir -p "$RESULTS_DIR"

say() { printf '[%(%F %T)T] %s\n' -1 "$*" | tee -a "$RESULTS_DIR/script.log"; }
die() { say "ERROR: $*"; exit 1; }

# --- resources ---------------------------------------------------------------

mem_avail_gb() { awk '/MemAvailable/ {printf "%d", $2/1048576}' /proc/meminfo; }
disk_free_gb() { mkdir -p "$1"; df -BG --output=avail "$1" | tail -1 | tr -dc 0-9; }

# Hash-map entries measured at 60-90 bytes each (RSS / budget on 13x13 and
# 15x15 runs); budget ~45% of free RAM at 100 bytes/entry. The rest covers the
# overshoot of one batch (the budget is checked between batches), sort buffers
# during spills and the loaded input shards.
auto_budget() { echo "${BUDGET:-$(( $(mem_avail_gb) * 1073741824 * 45 / 100 / 100 ))}"; }
# One input shard per batch at 15x15: a row-4 shard alone yields ~10^8 new
# row-5 states (~9 GB). Smaller sizes can afford bigger batches.
auto_batch() {
    if [[ -n "${BATCH:-}" ]]; then echo "$BATCH"; return; fi
    if (( N >= 15 )); then echo 1; else echo 8; fi
}

machine_info() {
    echo "date:      $(date -Is)"
    echo "host:      $(hostname)"
    echo "cpu:       $(lscpu 2>/dev/null | awk -F: '/Model name/ {gsub(/^ +/,"",$2); print $2; exit}')"
    echo "cores:     $(nproc) (using THREADS=$THREADS)"
    echo "mem avail: $(mem_avail_gb) GB"
    echo "disk free: $(disk_free_gb "$DATA_DIR") GB at $DATA_DIR"
    echo "rustc:     $(rustc --version 2>/dev/null || echo missing)"
    echo "commit:    $(git -C "$REPO" rev-parse HEAD 2>/dev/null) $(git -C "$REPO" status --short | wc -l | tr -d ' ') local changes"
}

# Environment for every counting run.
run_env() {
    echo "RAYON_NUM_THREADS=$THREADS BRITISH_DISK_BUDGET=$(auto_budget) BRITISH_DISK_BATCH=$(auto_batch) DP_STATS=1"
}

build() {
    command -v cargo >/dev/null || die "cargo not found; install Rust: curl https://sh.rustup.rs -sSf | sh"
    local v; v=$(rustc --version | awk '{print $2}')
    [[ "$(printf '%s\n' 1.87.0 "$v" | sort -V | head -1)" == 1.87.0 ]] ||
        die "rustc $v is too old (need >= 1.87); run: rustup update stable"
    command -v cc >/dev/null || die "no C linker (cc); install: sudo apt install -y build-essential"
    say "building (release)"
    (cd "$REPO" && cargo build --release --quiet)
}

# Run one counting job in the foreground, timestamping its log lines.
count() { # dir extra-env...
    local dir="$1"; shift
    env $(run_env) "$@" BRITISH_DISK_DIR="$dir" "$BIN" british "$N_RUN" 2>&1 |
        while IFS= read -r line; do printf '[%(%F %T)T] %s\n' -1 "$line"; done
}

# --- commands ----------------------------------------------------------------

cmd_check() {
    machine_info | tee "$RESULTS_DIR/machine.txt"
    local ok=1
    (( $(nproc) >= 16 )) || { say "WARN: fewer than 16 cores; 15x15 will take days"; ok=0; }
    (( $(mem_avail_gb) >= 64 )) || { say "WARN: under 64 GB free RAM; more RAM = fewer disk rewrites"; ok=0; }
    if [[ "$N" == 15 ]]; then
        (( $(disk_free_gb "$DATA_DIR") >= 600 )) || { say "WARN: under 600 GB free at DATA_DIR (peak ~475 GB)"; ok=0; }
    fi
    say "auto settings: budget=$(auto_budget) entries, batch=$(auto_batch) shards"
    build
    say "fast test suite"
    (cd "$REPO" && cargo test --release --quiet 2>&1 | grep -E "test result|FAILED|panicked") | tee -a "$RESULTS_DIR/script.log"
    (( ok )) || [[ "${FORCE:-}" == 1 ]] || die "resource checks failed (set FORCE=1 to continue anyway)"
    say "check OK"
}

cmd_smoke() {
    build
    say "smoke: 13x13 in disk mode (expect $KEITH_13)"
    local out
    out=$(N_RUN=13 count "$DATA_DIR-smoke" BRITISH_DISK_FRESH=1 | tee "$RESULTS_DIR/smoke-13.log")
    rm -rf "$DATA_DIR-smoke"
    grep -q "british n=13: $KEITH_13 " <<<"$out" || die "smoke FAILED: $(grep 'british n=' <<<"$out")"
    say "smoke PASS: $(grep 'british n=' <<<"$out" | sed 's/.*\] //')"
}

cmd_scaling() {
    build
    local out="$RESULTS_DIR/scaling-13.txt" t
    echo "threads  seconds  (13x13, disk mode)" > "$out"
    for t in $(( THREADS / 8 )) $(( THREADS / 4 )) $(( THREADS / 2 )) "$THREADS"; do
        (( t >= 1 )) || continue
        say "scaling: 13x13 with $t threads"
        local s=$SECONDS
        THREADS=$t N_RUN=13 count "$DATA_DIR-scaling" BRITISH_DISK_FRESH=1 > "$RESULTS_DIR/scaling-13-t$t.log"
        rm -rf "$DATA_DIR-scaling"
        grep -q "british n=13: $KEITH_13 " "$RESULTS_DIR/scaling-13-t$t.log" || die "wrong 13x13 count at $t threads"
        printf '%7d  %7d\n' "$t" $(( SECONDS - s )) | tee -a "$out"
    done
    say "scaling results in $out (ideal: seconds halve as threads double)"
}

cmd_rehearse() {
    build
    # 15x15 stores rows 0..5 and glues row 6: read 1/1024 of the input when
    # building row 4, generate all of row 5 from it but store 1/1024, then
    # glue those. Per-state costs of rows 5 and 6 come out of the log.
    local h=$(( (N - 1) / 2 ))
    local r_in=$(( h - 3 )) r_out=$(( h - 2 ))
    say "rehearsal: ${N}x${N}, 1/1024 of row-$r_in input, 1/1024 of row-$r_out output (NOT a count)"
    N_RUN=$N count "$DATA_DIR-rehearsal" BRITISH_DISK_FRESH=1 \
        BRITISH_SAMPLE=$r_in:1024 BRITISH_SAMPLE_OUT=$r_out:1024 \
        | tee "$RESULTS_DIR/rehearsal-${N}.log"
    rm -rf "$DATA_DIR-rehearsal"
    say "rehearsal finished; per-row timings in $RESULTS_DIR/rehearsal-${N}.log"
}

cmd_run() {
    if [[ -f "$PIDFILE" ]] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
        die "already running (pid $(cat "$PIDFILE")); see: $0 status"
    fi
    build
    [[ -f "$DATA_DIR/CONFIG" ]] && say "resuming from $DATA_DIR ($(tr -d '\n' < "$DATA_DIR/CONFIG"))"
    say "starting ${N}x${N}: $(run_env) DATA_DIR=$DATA_DIR"
    machine_info >> "$LOG"
    # setsid + nohup: survives the SSH session ending
    N_RUN=$N setsid nohup bash -c "$(declare -f count run_env auto_budget auto_batch mem_avail_gb); \
        N_RUN=$N BIN='$BIN' THREADS=$THREADS BUDGET='${BUDGET:-}' BATCH='${BATCH:-}' \
        count '$DATA_DIR'" >> "$LOG" 2>&1 < /dev/null &
    echo $! > "$PIDFILE"
    say "running in the background (pid $!). Follow: tail -f $LOG   Progress: $0 status"
}

cmd_status() {
    if [[ -f "$DATA_DIR/RESULT" ]]; then
        echo "state:   FINISHED"
    elif [[ -f "$PIDFILE" ]] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
        echo "state:   RUNNING (pid $(cat "$PIDFILE"))"
    else
        echo "state:   not running (rerun '$0 run' to resume)"
    fi
    [[ -f "$DATA_DIR/CONFIG" ]] && echo "config:  $(tr -d '\n' < "$DATA_DIR/CONFIG")"
    local d
    for d in "$DATA_DIR"/row*; do
        [[ -d "$d" ]] || continue
        if [[ -f "$d/DONE" ]]; then echo "$(basename "$d"):    done, $(cat "$d/DONE") states"
        elif [[ -f "$d/CKPT" ]]; then echo "$(basename "$d"):    in progress, checkpoint at input batch $(head -1 "$d/CKPT" | cut -d' ' -f1)"
        else echo "$(basename "$d"):    in progress, no checkpoint yet"; fi
    done
    [[ -d "$DATA_DIR/glue" ]] && echo "last row: $(ls "$DATA_DIR/glue" | grep -c '^g[0-9]*$') / 1024 shards summed"
    [[ -f "$DATA_DIR/RESULT" ]] && echo "RESULT:  $(cat "$DATA_DIR/RESULT")"
    echo "disk:    $(du -sh "$DATA_DIR" 2>/dev/null | cut -f1) used, $(disk_free_gb "$DATA_DIR") GB free"
    echo "--- last log lines"
    tail -5 "$LOG" 2>/dev/null || true
}

cmd_verify() {
    [[ -f "$DATA_DIR/RESULT" ]] || die "no finished run in $DATA_DIR to verify against"
    build
    local vdir="$DATA_DIR-verify" budget batch
    budget=$(( $(auto_budget) / 2 )); batch=$(( $(auto_batch) > 1 ? $(auto_batch) / 2 : 1 ))
    say "verify: second ${N}x${N} run with budget=$budget batch=$batch (different merge order)"
    BUDGET=$budget BATCH=$batch N_RUN=$N count "$vdir" >> "$RESULTS_DIR/verify-${N}.log"
    local a b
    a=$(cat "$DATA_DIR/RESULT"); b=$(cat "$vdir/RESULT")
    if [[ "$a" == "$b" ]] && diff -rq "$DATA_DIR/glue" "$vdir/glue" >/dev/null; then
        say "verify PASS: $a, all 1024 per-shard partial sums identical" | tee "$RESULTS_DIR/verify-${N}.txt"
    else
        say "verify FAILED: $a vs $b" | tee "$RESULTS_DIR/verify-${N}.txt"
        diff -r "$DATA_DIR/glue" "$vdir/glue" | head -20 | tee -a "$RESULTS_DIR/verify-${N}.txt"
        exit 1
    fi
}

cmd_export() {
    local stage="$RESULTS_DIR/export" out
    rm -rf "$stage"; mkdir -p "$stage"
    machine_info > "$stage/machine.txt"
    cp "$RESULTS_DIR"/*.log "$RESULTS_DIR"/*.txt "$stage/" 2>/dev/null || true
    for f in CONFIG RESULT; do [[ -f "$DATA_DIR/$f" ]] && cp "$DATA_DIR/$f" "$stage/$f"; done
    if [[ -d "$DATA_DIR/glue" ]]; then
        (cd "$DATA_DIR/glue" && for g in g[0-9]*; do [[ "$g" == *.tmp ]] || echo "$g $(cat "$g")"; done) \
            > "$stage/glue-partial-sums.txt"
    fi
    out="$RESULTS_DIR/crossword-${N}x${N}-$(date +%Y%m%d-%H%M).tar.gz"
    tar -czf "$out" -C "$stage" .
    rm -rf "$stage"
    say "exported $(du -h "$out" | cut -f1): $out"
}

cmd_all() { cmd_check; cmd_smoke; cmd_scaling; cmd_rehearse; cmd_run; }

case "${1:-}" in
    check|smoke|scaling|rehearse|run|status|verify|export|all) "cmd_$1" ;;
    *) sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
