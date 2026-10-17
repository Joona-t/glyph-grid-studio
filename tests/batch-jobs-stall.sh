#!/bin/bash
# tests/batch-jobs-stall.sh — a quiet `batch --jobs N` parent must not trip
# its own stall guard (ITER-072).
#
# Usage: BIN=<binary> bash tests/batch-jobs-stall.sh [scratch-dir]
#
# The `--jobs` parent's watchdog only sees liveness from its children's
# stderr lines.  Without `--progress` a child used to print nothing per
# frame, so one long job made the parent exit 3 ("stalled") after
# STALL_SECS.  This runs a 2-job `--jobs 2` batch WITHOUT --progress with
# the stall window shrunk to $STALL s (GGS_TEST_STALL_SECS, test-only; the
# parent strips it from its children) and frames enough that each job runs
# well past that window.  It must exit 0 with both outputs written, and the
# parent's stderr must carry no progress NDJSON (relayed only under
# --progress).
#
# Exit 0 on pass, 1 on failure, 2 on tooling errors.

set -u -o pipefail
cd "$(dirname "$0")/.."

BIN="${BIN:-$HOME/glyph-grid-studio/src-tauri/target/release/glyph-grid-studio}"
SCRATCH="${1:-tests/scratch/batch-jobs-stall}"
STILL="$PWD/tests/sources/cream-paper.png"
TIMEOUT="${TEST_TIMEOUT:-180}"
STALL="${STALL_SECS:-3}"
FRAMES=96

[[ -x "$BIN" ]] || { echo "batch-jobs-stall: binary not found at $BIN" >&2; exit 2; }

mkdir -p "$SCRATCH"
SCRATCH="$(cd "$SCRATCH" && pwd -P)"
rm -f "$SCRATCH"/*.gif "$SCRATCH"/*.json "$SCRATCH"/*.txt

cat > "$SCRATCH/m.json" <<EOF
{"in": "$STILL", "frames": $FRAMES, "jobs": [
  {"name": "s1", "out": "$SCRATCH/s1.gif", "config": {}},
  {"name": "s2", "out": "$SCRATCH/s2.gif", "config": {"palette": "spice"}}
]}
EOF

echo "=== batch --jobs 2, no --progress, stall guard ${STALL} s, $FRAMES frames/job ==="
t0=$(date +%s)
GGS_TEST_STALL_SECS="$STALL" timeout "$TIMEOUT" "$BIN" batch --manifest "$SCRATCH/m.json" --jobs 2 \
  >"$SCRATCH/log.txt" 2>&1; RC=$?
el=$(( $(date +%s) - t0 ))

PASS=0; FAIL=0
pass() { echo "  ✓ $1"; PASS=$((PASS+1)); }
fail() { echo "  ✗ $1: $2"; FAIL=$((FAIL+1)); }

grep -q "across 2 processes" "$SCRATCH/log.txt" || fail "fan-out" "no 'across 2 processes' line"
if [[ $RC -eq 0 ]]; then pass "exit 0 after ${el} s (> ${STALL} s stall window)"
else fail "exit" "exit $RC after ${el} s: $(tail -1 "$SCRATCH/log.txt")"; fi
[[ $el -gt $STALL ]] || fail "duration" "batch took ${el} s, not longer than the ${STALL} s window — test proves nothing"
[[ -s "$SCRATCH/s1.gif" && -s "$SCRATCH/s2.gif" ]] && pass "both outputs written" \
  || fail "outputs" "missing s1.gif / s2.gif"
if grep -q '"type":"progress"' "$SCRATCH/log.txt"; then
  fail "progress relay" "parent without --progress relayed progress NDJSON"
else
  pass "no progress NDJSON relayed without --progress"
fi

echo "  batch-jobs-stall: PASS $PASS FAIL $FAIL"
[[ "$FAIL" == "0" ]] && exit 0 || exit 1
