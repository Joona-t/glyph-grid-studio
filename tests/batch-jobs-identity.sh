#!/bin/bash
# tests/batch-jobs-identity.sh — `batch --jobs N` must not change any output
# byte (ITER-055, research F4).
#
# Usage: BIN=<binary> bash tests/batch-jobs-identity.sh [scratch-dir]
#
# Runs one 5-job manifest twice: serially (default --jobs 1, one session)
# and fanned out with --jobs 4 (round-robin over child processes, so one
# child gets two consecutive jobs).  The jobs mix a still and an animated
# source, GIF and MP4, phosphorDecay state, and two jobs WITHOUT their own
# `in` that follow a job with one: serially those render the previously
# loaded source, and the fan-out has to pin them to it.  Every output file
# must be md5-identical between the two runs, with the requested frame count.
#
# Exit 0 when every job matches, 1 on any mismatch, 2 on tooling errors.

set -u -o pipefail
cd "$(dirname "$0")/.."

BIN="${BIN:-$HOME/glyph-grid-studio/src-tauri/target/release/glyph-grid-studio}"
SCRATCH="${1:-tests/scratch/batch-jobs}"
STILL="$PWD/tests/sources/cream-paper.png"
TIMEOUT="${TEST_TIMEOUT:-180}"
FRAMES=8

[[ -x "$BIN" ]] || { echo "batch-jobs: binary not found at $BIN" >&2; exit 2; }
command -v ffmpeg >/dev/null || { echo "batch-jobs: ffmpeg not on PATH" >&2; exit 2; }
command -v ffprobe >/dev/null || { echo "batch-jobs: ffprobe not on PATH" >&2; exit 2; }

mkdir -p "$SCRATCH/serial" "$SCRATCH/jobs4"
SCRATCH="$(cd "$SCRATCH" && pwd -P)"
rm -f "$SCRATCH"/serial/* "$SCRATCH"/jobs4/* "$SCRATCH"/*.json "$SCRATCH"/*.txt

ANIM="$SCRATCH/anim-src.gif"
if ! ffmpeg -nostdin -hide_banner -loglevel error -y -loop 1 -i "$STILL" \
     -vf "crop=360:240:'mod(t*120,120)':'mod(t*60,60)',fps=10" -t 1.2 "$ANIM"; then
  echo "batch-jobs: could not build the animated fixture" >&2; exit 2
fi

NAMES=(j1-still j2-anim-decay j3-inherit-anim j4-still-mp4 j5-inherit-still)
EXTS=(gif gif gif mp4 gif)

manifest() {  # manifest <out-dir>
  local d="$1"
  cat <<EOF
{"in": "$STILL", "frames": $FRAMES, "jobs": [
  {"name": "j1-still", "out": "$d/j1-still.gif", "config": {}},
  {"name": "j2-anim-decay", "in": "$ANIM", "out": "$d/j2-anim-decay.gif",
   "config": {"palette": "spice", "postprocess": {"phosphorDecay": {"enabled": true, "decay": 0.85}}}},
  {"name": "j3-inherit-anim", "out": "$d/j3-inherit-anim.gif",
   "config": {"dither": {"mode": "stbn", "asSourcePrefilter": true, "levels": 16}}},
  {"name": "j4-still-mp4", "in": "$STILL", "out": "$d/j4-still-mp4.mp4", "format": "mp4",
   "config": {"palette": "spice"}},
  {"name": "j5-inherit-still", "out": "$d/j5-inherit-still.gif",
   "config": {"postprocess": {"phosphorDecay": {"enabled": true, "decay": 0.85}}}}
]}
EOF
}
manifest "$SCRATCH/serial" > "$SCRATCH/serial.json"
manifest "$SCRATCH/jobs4" > "$SCRATCH/jobs4.json"

PASS=0; FAIL=0
pass() { echo "  ✓ $1"; PASS=$((PASS+1)); }
fail() { echo "  ✗ $1: $2"; FAIL=$((FAIL+1)); }

frame_count() {
  ffprobe -v error -count_frames -select_streams v:0 -show_entries stream=nb_read_frames \
    -of csv=p=0 "$1" 2>/dev/null | head -1 | tr -d '[:space:],'
}

echo "=== batch --jobs 1 vs --jobs 4 (${#NAMES[@]} jobs, $FRAMES frames) ==="
timeout "$TIMEOUT" "$BIN" batch --manifest "$SCRATCH/serial.json" \
  >"$SCRATCH/serial.log.txt" 2>&1 || echo "    serial batch failed: $(tail -1 "$SCRATCH/serial.log.txt")"
timeout "$TIMEOUT" "$BIN" batch --manifest "$SCRATCH/jobs4.json" --jobs 4 \
  >"$SCRATCH/jobs4.log.txt" 2>&1; J4=$?
[[ $J4 -eq 0 ]] || echo "    --jobs 4 batch exit $J4: $(tail -1 "$SCRATCH/jobs4.log.txt")"
grep -q "across [0-9]* processes" "$SCRATCH/jobs4.log.txt" \
  || fail "fan-out" "--jobs 4 did not fork children (no 'across N processes' line)"

for i in "${!NAMES[@]}"; do
  n="${NAMES[$i]}"; f="$n.${EXTS[$i]}"
  a="$SCRATCH/serial/$f"; b="$SCRATCH/jobs4/$f"
  [[ -f "$a" && -f "$b" ]] || { fail "$n" "missing output (serial $([[ -f $a ]] && echo y || echo n), jobs4 $([[ -f $b ]] && echo y || echo n))"; continue; }
  na=$(frame_count "$a"); nb=$(frame_count "$b")
  [[ "$na" == "$FRAMES" && "$nb" == "$FRAMES" ]] || { fail "$n" "decoded frames ${na:-?} / ${nb:-?}, requested $FRAMES"; continue; }
  ma=$(md5 -q "$a"); mb=$(md5 -q "$b")
  if [[ "$ma" == "$mb" ]]; then pass "$n (md5 $ma)"; else fail "$n" "md5 $ma != $mb"; fi
done

echo "  batch-jobs: PASS $PASS FAIL $FAIL"
[[ "$FAIL" == "0" && "$PASS" == "${#NAMES[@]}" ]] && exit 0 || exit 1
