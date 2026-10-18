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
command -v ffprobe >/dev/null || { echo "psnr-gate: ffprobe not on PATH" >&2; exit 2; }

# ITER-049: the frame-count mismatch exit promised above was never
# implemented — ffmpeg's psnr filter silently compares the shorter stream.
# Count decoded frames on both sides first.
nframes() {
  ffprobe -v error -count_frames -select_streams v:0 -show_entries stream=nb_read_frames \
    -of csv=p=0 "$1" 2>/dev/null | head -1 | tr -d '[:space:],'
}
N_NEW=$(nframes "$NEW"); N_BASE=$(nframes "$BASE")
if [[ -z "$N_NEW" || -z "$N_BASE" ]]; then
  echo "psnr-gate: ffprobe could not count frames (new=${N_NEW:-?} base=${N_BASE:-?})" >&2; exit 2
fi
if [[ "$N_NEW" != "$N_BASE" ]]; then
  echo "psnr-gate: frame-count mismatch — $NEW has $N_NEW frames, $BASE has $N_BASE" >&2; exit 2
fi

STATS="$(mktemp "${TMPDIR:-/tmp}/psnr-gate.XXXXXX")"
LOG="$(mktemp "${TMPDIR:-/tmp}/psnr-gate-log.XXXXXX")"

# ITER-050: pair frames by INDEX, not timestamp.  The psnr filter's framesync
# matches on pts, so an MP4 written before the track-timescale fix (1 ms per
# frame) against one written after it (1/fps per frame) compared frame 0 with
# whatever the other stream held at that instant — 17 dB between two
# pixel-identical files.  Re-stamp both streams as pts = N in a common 1/1000
# timebase.
ffmpeg -nostdin -hide_banner -i "$NEW" -i "$BASE" \
  -lavfi "[0:v]settb=1/1000,setpts=N[a];[1:v]settb=1/1000,setpts=N[b];[a][b]psnr=stats_file=$STATS" \
  -f null - >"$LOG" 2>&1
rc=$?
if [[ $rc -ne 0 ]]; then
  echo "psnr-gate: ffmpeg failed (exit $rc):" >&2
  tail -5 "$LOG" >&2
  exit 2
fi

SUMMARY=$(grep -E 'PSNR .*average:' "$LOG" | tail -1)
[[ -n "$SUMMARY" ]] || { echo "psnr-gate: no PSNR summary in ffmpeg output" >&2; tail -5 "$LOG" >&2; exit 2; }

python3 - "$STATS" "$SUMMARY" "$MIN_DB" "$NEW" "$BASE" "$N_NEW" <<'PY'
import re, sys
stats, summary, min_db, new, base = sys.argv[1], sys.argv[2], float(sys.argv[3]), sys.argv[4], sys.argv[5]
n_probe = int(sys.argv[6])

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
print(f"  frames compared : {frames}  (ffprobe: {n_probe} in each)")
print(f"  average         : {avg:.2f} dB  (per-frame min {mn:.2f}, max {mx:.2f})")
if worst_n is not None:
    print(f"  worst frame     : #{worst_n} @ {worst_db:.2f} dB")
print(f"  threshold       : >= {min_db:.2f} dB")
if avg != avg:  # nan
    print("  RESULT: ERROR (could not parse average)"); sys.exit(2)
if frames != n_probe:
    print(f"  RESULT: ERROR (psnr compared {frames} frames, ffprobe counted {n_probe})"); sys.exit(2)
if avg >= min_db:
    print("  RESULT: PASS"); sys.exit(0)
print(f"  RESULT: FAIL (stats: {stats})"); sys.exit(1)
PY
rc=$?
[[ $rc -eq 0 ]] && rm -f "$STATS"
rm -f "$LOG"
exit $rc
