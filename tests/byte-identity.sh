#!/bin/bash
# tests/byte-identity.sh — headless output must be a pure function of
# (source, config, seed).  v0.2 Phase 1 gate (ITER-046, research F23).
#
# Usage: BIN=<binary> bash tests/byte-identity.sh [scratch-dir]
#
# Renders the same job
#   1. twice headless,
#   2. once with --show-window (rAF runs at 60 Hz instead of the ~5 Hz the
#      off-screen window gets — before ITER-046 that alone changed the
#      per-cell EMA warm-up and therefore the pixels of the first ~15 frames),
#   3. as a 2-job batch of identical jobs (job 2 used to inherit job 1's EMA
#      and CRT state),
# for a still source (tests/sources/cream-paper.png) and for an animated one
# (a 12-frame GIF cut from the same PNG by ffmpeg, so no binary fixture is
# checked in), decodes every output to raw RGBA with ffmpeg and requires the
# pixel streams to be identical.  Pixels, not container bytes: WebKit's PNG
# encoder and the GIF/MP4 encoders are not what this gate is about.
#
# Exit 0 when every pairing matches, 1 on any mismatch (the differing frame
# indices are printed from `ffmpeg -f framemd5`), 2 on tooling errors.

set -u -o pipefail   # pipefail (ITER-049): an ffmpeg decode failure must not hash as "" == ""
cd "$(dirname "$0")/.."

BIN="${BIN:-$HOME/glyph-grid-studio/src-tauri/target/release/glyph-grid-studio}"
SCRATCH="${1:-tests/scratch/byte-identity}"
STILL="$PWD/tests/sources/cream-paper.png"
TIMEOUT="${TEST_TIMEOUT:-180}"

[[ -x "$BIN" ]] || { echo "byte-identity: binary not found at $BIN" >&2; exit 2; }
[[ -f "$STILL" ]] || { echo "byte-identity: missing $STILL" >&2; exit 2; }
command -v ffmpeg >/dev/null || { echo "byte-identity: ffmpeg not on PATH" >&2; exit 2; }
command -v ffprobe >/dev/null || { echo "byte-identity: ffprobe not on PATH" >&2; exit 2; }

mkdir -p "$SCRATCH"
SCRATCH="$(cd "$SCRATCH" && pwd -P)"
rm -f "$SCRATCH"/*.gif "$SCRATCH"/*.mp4 "$SCRATCH"/*.json "$SCRATCH"/*.txt

PASS=0; FAIL=0
pass() { echo "  ✓ $1"; PASS=$((PASS+1)); }
fail() { echo "  ✗ $1: $2"; FAIL=$((FAIL+1)); }

render() {  # render <in> <out> <frames> [extra args...]
  local in="$1" out="$2" frames="$3"; shift 3
  timeout "$TIMEOUT" "$BIN" render --in "$in" --out "$out" --frames "$frames" "$@" \
    >"$out.log.txt" 2>&1 || { echo "    render failed: $out ($(tail -1 "$out.log.txt"))"; return 1; }
  [[ -f "$out" ]]
}

# ITER-049: a decode failure returns non-zero (pipefail) instead of an empty
# md5 that would compare equal to another empty md5.
pixmd5() { ffmpeg -nostdin -v error -i "$1" -f rawvideo -pix_fmt rgba - 2>/dev/null | md5 -q; }
# Decoded frame count, so an 8-frame request that produced a 1-frame file
# cannot pass by being identically truncated twice.
frame_count() {
  ffprobe -v error -count_frames -select_streams v:0 -show_entries stream=nb_read_frames \
    -of csv=p=0 "$1" 2>/dev/null | head -1 | tr -d '[:space:],'
}

# Prints the indices of frames whose RGBA differs between two files.
diff_frames() {
  local a b
  a=$(ffmpeg -nostdin -v error -i "$1" -f framemd5 - 2>/dev/null | grep -v '^#' | awk -F', *' '{print $6}')
  b=$(ffmpeg -nostdin -v error -i "$2" -f framemd5 - 2>/dev/null | grep -v '^#' | awk -F', *' '{print $6}')
  paste <(echo "$a") <(echo "$b") | awk '$1 != $2 {printf "%d ", NR-1}'
}

check_identical() {  # check_identical <label> <file-a> <file-b> <expected-frames>
  local label="$1" a="$2" b="$3" want="$4"
  [[ -f "$a" && -f "$b" ]] || { fail "$label" "missing output"; return; }
  local ma mb na nb
  ma=$(pixmd5 "$a") || { fail "$label" "ffmpeg could not decode $a"; return; }
  mb=$(pixmd5 "$b") || { fail "$label" "ffmpeg could not decode $b"; return; }
  na=$(frame_count "$a"); nb=$(frame_count "$b")
  if [[ "$na" != "$want" || "$nb" != "$want" ]]; then
    fail "$label" "decoded frames ${na:-?} / ${nb:-?}, requested $want"; return
  fi
  if [[ -n "$ma" && "$ma" == "$mb" ]]; then
    pass "$label (rgba md5 $ma, $na frames)"
  else
    fail "$label" "frames differ: $(diff_frames "$a" "$b")"
  fi
}

# Animated fixture: 12 frames, 360×240, a drifting crop of the still so the
# source frame selection + loop-tail priming paths are exercised.
ANIM="$SCRATCH/anim-src.gif"
if ! ffmpeg -nostdin -hide_banner -loglevel error -y -loop 1 -i "$STILL" \
     -vf "crop=360:240:'mod(t*120,120)':'mod(t*60,60)',fps=10" -t 1.2 "$ANIM"; then
  echo "byte-identity: could not build the animated fixture" >&2; exit 2
fi

echo "=== Byte-identity — still source (8 frames) ==="
render "$STILL" "$SCRATCH/still-a.gif" 8 \
  && render "$STILL" "$SCRATCH/still-b.gif" 8 \
  && render "$STILL" "$SCRATCH/still-win.gif" 8 --show-window
check_identical "G1 still: headless run A == run B" "$SCRATCH/still-a.gif" "$SCRATCH/still-b.gif" 8
check_identical "G2 still: headless == --show-window" "$SCRATCH/still-a.gif" "$SCRATCH/still-win.gif" 8

echo "=== Byte-identity — animated source (12 frames) ==="
render "$ANIM" "$SCRATCH/anim-a.gif" 12 \
  && render "$ANIM" "$SCRATCH/anim-b.gif" 12 \
  && render "$ANIM" "$SCRATCH/anim-win.gif" 12 --show-window
check_identical "G3 animated: headless run A == run B" "$SCRATCH/anim-a.gif" "$SCRATCH/anim-b.gif" 12
check_identical "G4 animated: headless == --show-window" "$SCRATCH/anim-a.gif" "$SCRATCH/anim-win.gif" 12

render "$ANIM" "$SCRATCH/anim-a.mp4" 12 --format mp4 \
  && render "$ANIM" "$SCRATCH/anim-b.mp4" 12 --format mp4
check_identical "G5 animated MP4: run A == run B" "$SCRATCH/anim-a.mp4" "$SCRATCH/anim-b.mp4" 12

echo "=== Byte-identity — batch of identical jobs ==="
# twin-1/twin-2: phosphorDecay carries persistent per-pixel state (and needs
# a `decay` value to pass _hasImgDataStages), so job 2 used to start from
# job 1's last frame.  twin-3 uses the exact config shape `render --palette
# --dither` builds, so it can be compared against a standalone process.
cat > "$SCRATCH/batch.json" <<EOF
{"in": "$ANIM", "frames": 12, "jobs": [
  {"name": "twin-1", "out": "$SCRATCH/batch-1.gif", "format": "gif",
   "config": {"palette": "spice", "postprocess": {"phosphorDecay": {"enabled": true, "decay": 0.85}}}},
  {"name": "twin-2", "out": "$SCRATCH/batch-2.gif", "format": "gif",
   "config": {"palette": "spice", "postprocess": {"phosphorDecay": {"enabled": true, "decay": 0.85}}}},
  {"name": "twin-3", "out": "$SCRATCH/batch-3.gif", "format": "gif",
   "config": {"palette": "spice", "dither": {"mode": "stbn", "asSourcePrefilter": true, "levels": 16}}}
]}
EOF
if ! timeout "$TIMEOUT" "$BIN" batch --manifest "$SCRATCH/batch.json" >"$SCRATCH/batch.log.txt" 2>&1; then
  echo "    batch failed: $(tail -1 "$SCRATCH/batch.log.txt")"
fi
check_identical "G6 batch: job 1 == job 2 (phosphorDecay on)" "$SCRATCH/batch-1.gif" "$SCRATCH/batch-2.gif" 12
# One process per job vs one process for all jobs is not allowed to matter.
render "$ANIM" "$SCRATCH/anim-spice.gif" 12 --palette spice --dither stbn
check_identical "G7 batch job 3 == standalone render (same config)" "$SCRATCH/batch-3.gif" "$SCRATCH/anim-spice.gif" 12

echo "  byte-identity: PASS $PASS FAIL $FAIL"
[[ "$FAIL" == "0" ]] && exit 0 || exit 1
