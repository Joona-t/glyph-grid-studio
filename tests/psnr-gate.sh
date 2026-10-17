#!/bin/bash
# tests/psnr-gate.sh — output-equivalence gate (v0.2 Phase 0; F5/F7 both asked for it).
#
# Usage: tests/psnr-gate.sh <new.mp4|gif> <baseline.mp4|gif> [min_db]
#
# Wraps the ffmpeg PSNR comparison that was being typed by hand before every
# commit. Prints the average PSNR, the worst frame (index + dB), and exits:
#   0  average >= min_db (default 35)
#   1  average below threshold
#   2  usage / ffmpeg failure / frame-count mismatch
# Identical inputs give PSNR=inf and pass. The per-frame stats file is kept
# under $TMPDIR so a failing gate can be inspected (path printed on failure).

set -u

MIN_DB="${3:-35}"
NEW="${1:-}"; BASE="${2:-}"
if [[ -z "$NEW" || -z "$BASE" ]]; then
  echo "usage: $0 <new> <baseline> [min_db]" >&2; exit 2
fi
for f in "$NEW" "$BASE"; do
  [[ -f "$f" ]] || { echo "psnr-gate: missing file $f" >&2; exit 2; }
done
command -v ffmpeg >/dev/null || { echo "psnr-gate: ffmpeg not on PATH" >&2; exit 2; }

STATS="$(mktemp "${TMPDIR:-/tmp}/psnr-gate.XXXXXX")"
LOG="$(mktemp "${TMPDIR:-/tmp}/psnr-gate-log.XXXXXX")"

ffmpeg -nostdin -hide_banner -i "$NEW" -i "$BASE" \
  -lavfi "psnr=stats_file=$STATS" -f null - >"$LOG" 2>&1
rc=$?
if [[ $rc -ne 0 ]]; then
  echo "psnr-gate: ffmpeg failed (exit $rc):" >&2
  tail -5 "$LOG" >&2
  exit 2
fi

SUMMARY=$(grep -E 'PSNR .*average:' "$LOG" | tail -1)
[[ -n "$SUMMARY" ]] || { echo "psnr-gate: no PSNR summary in ffmpeg output" >&2; tail -5 "$LOG" >&2; exit 2; }

python3 - "$STATS" "$SUMMARY" "$MIN_DB" "$NEW" "$BASE" <<'PY'
import re, sys
stats, summary, min_db, new, base = sys.argv[1], sys.argv[2], float(sys.argv[3]), sys.argv[4], sys.argv[5]

def num(s):
    return float('inf') if s == 'inf' else float(s)

m = re.search(r'average:(\S+) min:(\S+) max:(\S+)', summary)
avg, mn, mx = (num(m.group(1)), num(m.group(2)), num(m.group(3))) if m else (float('nan'),) * 3

worst_n, worst_db, frames = None, float('inf'), 0
for line in open(stats):
    n = re.search(r'\bn:(\d+)', line)
    p = re.search(r'psnr_avg:(\S+)', line)
    if not (n and p):
        continue
    frames += 1
    v = num(p.group(1))
    if v < worst_db:
        worst_n, worst_db = int(n.group(1)), v

print(f"psnr-gate: {new} vs {base}")
print(f"  frames compared : {frames}")
print(f"  average         : {avg:.2f} dB  (per-frame min {mn:.2f}, max {mx:.2f})")
if worst_n is not None:
    print(f"  worst frame     : #{worst_n} @ {worst_db:.2f} dB")
print(f"  threshold       : >= {min_db:.2f} dB")
if avg != avg:  # nan
    print("  RESULT: ERROR (could not parse average)"); sys.exit(2)
if avg >= min_db:
    print("  RESULT: PASS"); sys.exit(0)
print(f"  RESULT: FAIL (stats: {stats})"); sys.exit(1)
PY
rc=$?
[[ $rc -eq 0 ]] && rm -f "$STATS"
rm -f "$LOG"
exit $rc
