#!/bin/bash
# tests/video-source.sh — video sources (ITER-077, plan 5.4 / research R4).
#
# Usage: BIN=<binary> bash tests/video-source.sh [scratch-dir]
#
#   V1 render --in tests/sources/tiny.mp4 --frames 12 (Rust openh264) →
#      12-frame GIF whose first and last frames differ
#   V2 the same render twice → md5-identical files (byte-identity)
#   V3 --video-decode webview (the <video> seek path) → 12 frames, two runs
#      pixel-identical, same picture as the Rust path (PSNR >= 30 dB)
#   V4 a B-frame High-profile clip decodes in Rust and renders
#   V5 HEVC (generated with libx265 when ffmpeg has it): auto falls back to
#      the WebView with a stderr note; native exits non-zero, no output
#   V6 --frames auto = clip length (12 / 25 fps = 0.48 s) x effFps 33.3 = 16
#   V7 limits before decode: a 481-frame clip is rejected by the header
#      probe; a typo'd --video-decode exits 2 with the schema path
#   V8 batch: one clip, two jobs with different video.decode → both render
# Prints "video-source: N passed, M failed"; exit 0 only when all pass.

set -u -o pipefail
cd "$(dirname "$0")/.."

BIN="${BIN:-$PWD/src-tauri/target/release/glyph-grid-studio}"
SCRATCH="${1:-tests/scratch/video-source}"
TINY="$PWD/tests/sources/tiny.mp4"
TINYB="$PWD/tests/sources/tiny-bframes.mp4"
TIMEOUT="${TEST_TIMEOUT:-180}"

[[ -x "$BIN" ]] || { echo "video-source: binary not found at $BIN" >&2; exit 2; }

mkdir -p "$SCRATCH"
SCRATCH="$(cd "$SCRATCH" && pwd -P)"
rm -f "$SCRATCH"/*.gif "$SCRATCH"/*.json "$SCRATCH"/*.log "$SCRATCH"/*.mp4

P=0; F=0
ok()  { echo "  ✓ $1"; P=$((P+1)); }
bad() { echo "  ✗ $1: $2"; F=$((F+1)); }
render() { timeout "$TIMEOUT" "$BIN" render --cols 120 --rows 60 "$@"; }
nframes() { ffprobe -v error -count_frames -select_streams v:0 -show_entries stream=nb_read_frames -of csv=p=0 "$1" 2>/dev/null; }
framemd5() { ffmpeg -v error -i "$1" -vf "select=eq(n\,$2)" -frames:v 1 -f rawvideo -pix_fmt rgb24 - 2>/dev/null | md5 -q 2>/dev/null || true; }
allmd5() { ffmpeg -v error -i "$1" -pix_fmt rgb24 -f framemd5 - 2>/dev/null | grep -v '^#'; }

# V1
render --in "$TINY" --out "$SCRATCH/v1.gif" --frames 12 2>"$SCRATCH/v1.log"; V1RC=$?
V1N=$(nframes "$SCRATCH/v1.gif"); V1A=$(framemd5 "$SCRATCH/v1.gif" 0); V1Z=$(framemd5 "$SCRATCH/v1.gif" 11)
if [[ $V1RC -eq 0 && "$V1N" == "12" && -n "$V1A" && "$V1A" != "$V1Z" ]] && grep -q "via Rust openh264" "$SCRATCH/v1.log"; then
  ok "V1 tiny.mp4 via Rust openh264: 12-frame GIF, frame 0 != frame 11"
else
  bad "V1 tiny.mp4 render" "rc=$V1RC frames=$V1N f0=$V1A f11=$V1Z $(grep -E 'video|rror' "$SCRATCH/v1.log" | tail -2 | tr '\n' ' ')"
fi

# V2
render --in "$TINY" --out "$SCRATCH/v2.gif" --frames 12 2>/dev/null
M1=$(md5 -q "$SCRATCH/v1.gif" 2>/dev/null); M2=$(md5 -q "$SCRATCH/v2.gif" 2>/dev/null)
if [[ -n "$M1" && "$M1" == "$M2" ]]; then
  ok "V2 two renders of the mp4 are md5-identical (${M1:0:8})"
else
  bad "V2 mp4 byte-identity" "$M1 vs $M2"
fi

# V3
render --in "$TINY" --out "$SCRATCH/v3a.gif" --frames 12 --video-decode webview 2>"$SCRATCH/v3.log"; V3RC=$?
render --in "$TINY" --out "$SCRATCH/v3b.gif" --frames 12 --video-decode webview 2>/dev/null
V3PSNR=$(ffmpeg -i "$SCRATCH/v1.gif" -i "$SCRATCH/v3a.gif" -lavfi psnr -f null - 2>&1 | grep -o 'average:[0-9.inf]*' | cut -d: -f2)
V3A=$(allmd5 "$SCRATCH/v3a.gif")
if [[ $V3RC -eq 0 && "$(nframes "$SCRATCH/v3a.gif")" == "12" && "$V3A" == "$(allmd5 "$SCRATCH/v3b.gif")" ]] \
   && grep -q "via WebView <video>" "$SCRATCH/v3.log" && python3 -c "import sys; v='$V3PSNR'; sys.exit(0 if v and (v=='inf' or float(v) >= 30) else 1)"; then
  ok "V3 --video-decode webview: 12 frames, 2 runs pixel-identical, PSNR vs Rust ${V3PSNR} dB"
else
  bad "V3 webview seek path" "rc=$V3RC psnr=$V3PSNR $(grep -E 'video|rror' "$SCRATCH/v3.log" | tail -2 | tr '\n' ' ')"
fi

# V4
render --in "$TINYB" --out "$SCRATCH/v4.gif" --frames 12 --video-decode native 2>"$SCRATCH/v4.log"; V4RC=$?
if [[ $V4RC -eq 0 && "$(nframes "$SCRATCH/v4.gif")" == "12" && "$(framemd5 "$SCRATCH/v4.gif" 0)" != "$(framemd5 "$SCRATCH/v4.gif" 11)" ]]; then
  ok "V4 High-profile B-frame clip decodes in Rust (native)"
else
  bad "V4 B-frame clip" "rc=$V4RC $(tail -1 "$SCRATCH/v4.log")"
fi

# V5
if ffmpeg -v error -y -f lavfi -i "testsrc2=s=176x100:r=30" -frames:v 10 -c:v libx265 -tag:v hvc1 \
     -x265-params log-level=none "$SCRATCH/hevc.mp4" 2>/dev/null && [[ -s "$SCRATCH/hevc.mp4" ]]; then
  render --in "$SCRATCH/hevc.mp4" --out "$SCRATCH/v5a.gif" --frames 6 2>"$SCRATCH/v5a.log"; V5A=$?
  render --in "$SCRATCH/hevc.mp4" --out "$SCRATCH/v5b.gif" --frames 6 --video-decode native 2>"$SCRATCH/v5b.log"; V5B=$?
  if [[ $V5A -eq 0 && "$(nframes "$SCRATCH/v5a.gif")" == "6" && $V5B -ne 0 && ! -f "$SCRATCH/v5b.gif" ]] \
     && grep -q "decoding through the WebView" "$SCRATCH/v5a.log" && grep -q 'video.decode is "native"' "$SCRATCH/v5b.log"; then
    ok "V5 HEVC: auto falls back to the WebView (noted on stderr); native exits $V5B"
  else
    bad "V5 HEVC fallback" "auto=$V5A native=$V5B $(grep -E 'video|rror' "$SCRATCH/v5a.log" "$SCRATCH/v5b.log" | tail -2 | tr '\n' ' ')"
  fi
else
  ok "V5 HEVC fallback (skipped: ffmpeg has no libx265)"
fi

# V6
render --in "$TINY" --out "$SCRATCH/v6.gif" --frames auto 2>/dev/null; V6RC=$?
V6N=$(nframes "$SCRATCH/v6.gif")
if [[ $V6RC -eq 0 && "$V6N" == "16" ]]; then
  ok "V6 --frames auto = 0.48 s clip x 33.3 fps = 16 frames"
else
  bad "V6 --frames auto" "rc=$V6RC frames=$V6N"
fi

# V7
ffmpeg -v error -y -f lavfi -i "color=c=gray:s=16x16:r=60" -frames:v 481 -c:v libx264 -profile:v baseline "$SCRATCH/long.mp4" 2>/dev/null
V7ERR=$(render --in "$SCRATCH/long.mp4" --out "$SCRATCH/v7.gif" --frames 2 2>&1); V7RC=$?
V7B=$("$BIN" render --in "$TINY" --out "$SCRATCH/v7b.gif" --video-decode ffmpeg 2>&1); V7BRC=$?
if [[ $V7RC -ne 0 && ! -f "$SCRATCH/v7.gif" && $V7BRC -eq 2 ]] \
   && grep -q "rejected before decode: animation has 481 frames" <<<"$V7ERR" && grep -q "/video/decode" <<<"$V7B"; then
  ok "V7 481-frame clip rejected by the header probe; bad --video-decode exits 2 at /video/decode"
else
  bad "V7 limits / flag validation" "rc=$V7RC/$V7BRC $(grep -E 'reject|rror' <<<"$V7ERR" | tail -1) | $(tail -1 <<<"$V7B")"
fi

# V8
cat > "$SCRATCH/v8.json" <<EOF
{"version": 1, "in": "$TINY", "frames": 4, "jobs": [
  {"name": "rust", "out": "$SCRATCH/v8a.gif"},
  {"name": "webview", "out": "$SCRATCH/v8b.gif", "config": {"video": {"decode": "webview"}}}]}
EOF
timeout "$TIMEOUT" "$BIN" batch --manifest "$SCRATCH/v8.json" 2>"$SCRATCH/v8.log"; V8RC=$?
if [[ $V8RC -eq 0 && "$(nframes "$SCRATCH/v8a.gif")" == "4" && "$(nframes "$SCRATCH/v8b.gif")" == "4" ]] \
   && grep -q "via Rust openh264" "$SCRATCH/v8.log" && grep -q "via WebView <video>" "$SCRATCH/v8.log"; then
  ok "V8 batch: per-job video.decode re-opens the clip (Rust, then WebView)"
else
  bad "V8 batch decoder swap" "rc=$V8RC $(grep -E 'video|rror|batch' "$SCRATCH/v8.log" | tail -3 | tr '\n' ' ')"
fi

echo "video-source: $P passed, $F failed"
[[ $F -eq 0 ]]
