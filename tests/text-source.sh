#!/bin/bash
# tests/text-source.sh — kinetic typography (ITER-076, plan 5.3 / research R3).
#
# Usage: BIN=<binary> bash tests/text-source.sh [scratch-dir]
#
#   X1 render --text "GLYPH" --text-anim typewriter --frames 12 (no --in) →
#      non-empty 12-frame GIF whose frame 0 differs from frame 11
#   X2 the same render twice → per-frame pixel-identical (priming included)
#   X3 --srt with 2 s of cues + --frames auto → the loop is the last cue
#      (round(2 s × effFps 33.3) = 67 frames)
#   X4 --in + --text-blend screen composites (differs from the image alone
#      and from text-only replace)
#   X5 a preset with textSource (JSON form) renders the same pixels as the
#      CLI flags; --dry-run shows textSource + textImport, no window
#   X6 usage errors exit 2 before launch: no --in and no text; bad anim
#      (schema path); unreadable --srt
#   X7 a stem under one glyph cell warns on stderr ("text: warn")
# Prints "text-source: N passed, M failed"; exit 0 only when all pass.

set -u -o pipefail
cd "$(dirname "$0")/.."

BIN="${BIN:-$HOME/glyph-grid-studio/src-tauri/target/release/glyph-grid-studio}"
SCRATCH="${1:-tests/scratch/text-source}"
STILL="$PWD/tests/sources/cream-paper.png"
TIMEOUT="${TEST_TIMEOUT:-180}"

[[ -x "$BIN" ]] || { echo "text-source: binary not found at $BIN" >&2; exit 2; }

mkdir -p "$SCRATCH"
SCRATCH="$(cd "$SCRATCH" && pwd -P)"
rm -f "$SCRATCH"/*.gif "$SCRATCH"/*.json "$SCRATCH"/*.log "$SCRATCH"/*.srt

P=0; F=0
ok()  { echo "  ✓ $1"; P=$((P+1)); }
bad() { echo "  ✗ $1: $2"; F=$((F+1)); }
render() { timeout "$TIMEOUT" "$BIN" render --cols 120 --rows 60 "$@"; }
nframes() { ffprobe -v error -count_frames -select_streams v:0 -show_entries stream=nb_read_frames -of csv=p=0 "$1" 2>/dev/null; }
# md5 of decoded RGB frame $2
framemd5() { ffmpeg -v error -i "$1" -vf "select=eq(n\,$2)" -frames:v 1 -f rawvideo -pix_fmt rgb24 - 2>/dev/null | md5 -q 2>/dev/null || true; }
allmd5() { ffmpeg -v error -i "$1" -pix_fmt rgb24 -f framemd5 - 2>/dev/null | grep -v '^#'; }

TW=(--text GLYPH --text-anim typewriter --frames 12)

# X1
render "${TW[@]}" --out "$SCRATCH/x1.gif" 2>"$SCRATCH/x1.log"; X1RC=$?
X1N=$(nframes "$SCRATCH/x1.gif"); X1A=$(framemd5 "$SCRATCH/x1.gif" 0); X1Z=$(framemd5 "$SCRATCH/x1.gif" 11)
if [[ $X1RC -eq 0 && -s "$SCRATCH/x1.gif" && "$X1N" == "12" && -n "$X1A" && "$X1A" != "$X1Z" ]]; then
  ok "X1 --text typewriter, no --in: 12-frame GIF ($(wc -c <"$SCRATCH/x1.gif" | tr -d ' ') B), frame 0 != frame 11"
else
  bad "X1 --text typewriter" "rc=$X1RC frames=$X1N f0=$X1A f11=$X1Z $(tail -1 "$SCRATCH/x1.log")"
fi

# X2
render "${TW[@]}" --out "$SCRATCH/x2.gif" 2>/dev/null
X2A=$(allmd5 "$SCRATCH/x1.gif")
if [[ -s "$SCRATCH/x2.gif" && $(echo "$X2A" | grep -c .) -eq 12 && "$X2A" == "$(allmd5 "$SCRATCH/x2.gif")" ]]; then
  ok "X2 two runs pixel-identical (12/12 frames)"
else
  bad "X2 determinism" "frame md5s differ"
fi

# X3
printf '1\n00:00:00,000 --> 00:00:01,000\nDROP\n\n2\n00:00:01,000 --> 00:00:02,000\nTHE\nBEAT\n' > "$SCRATCH/x3.srt"
render --srt "$SCRATCH/x3.srt" --text-anim rise --frames auto --out "$SCRATCH/x3.gif" 2>"$SCRATCH/x3.log"; X3RC=$?
X3N=$(nframes "$SCRATCH/x3.gif")
X3A=$(framemd5 "$SCRATCH/x3.gif" 20); X3B=$(framemd5 "$SCRATCH/x3.gif" 55)
if [[ $X3RC -eq 0 && "$X3N" == "67" && "$X3A" != "$X3B" ]]; then
  ok "X3 --srt: loop = last cue end (2 s → 67 frames), cue 1 != cue 2"
else
  bad "X3 --srt" "rc=$X3RC frames=$X3N $(tail -1 "$SCRATCH/x3.log")"
fi

# X4
render --in "$STILL" --frames 2 --out "$SCRATCH/x4-img.gif" 2>/dev/null
render --in "$STILL" --text GLYPH --text-blend screen --frames 2 --out "$SCRATCH/x4-scr.gif" 2>/dev/null
render --text GLYPH --frames 2 --out "$SCRATCH/x4-rep.gif" 2>/dev/null
X4I=$(framemd5 "$SCRATCH/x4-img.gif" 1); X4S=$(framemd5 "$SCRATCH/x4-scr.gif" 1); X4R=$(framemd5 "$SCRATCH/x4-rep.gif" 1)
if [[ -n "$X4S" && "$X4S" != "$X4I" && "$X4S" != "$X4R" ]]; then
  ok "X4 --in + --text-blend screen composites (differs from image-only and text-only)"
else
  bad "X4 composite" "img=$X4I screen=$X4S replace=$X4R"
fi

# X5
cat > "$SCRATCH/x5.json" <<'EOF'
{"textSource":{"enabled":true,"anim":"typewriter","lines":[{"text":"GLYPH","start":0}]}}
EOF
render --preset "$SCRATCH/x5.json" --frames 12 --out "$SCRATCH/x5.gif" 2>/dev/null
DRY=$("$BIN" render --text HI --srt "$SCRATCH/x3.srt" --out "$SCRATCH/never.gif" --dry-run 2>/dev/null)
if [[ "$(allmd5 "$SCRATCH/x1.gif")" == "$(allmd5 "$SCRATCH/x5.gif")" ]] \
   && echo "$DRY" | python3 -c 'import json,sys; j=json.load(sys.stdin); assert j["config"]["textSource"]["enabled"] and j["inPath"] is None and "DROP" in j["textImport"]["body"]' 2>/dev/null \
   && [[ ! -f "$SCRATCH/never.gif" ]]; then
  ok "X5 preset JSON form == CLI flags (pixels); --dry-run carries textSource + textImport"
else
  bad "X5 preset / dry-run parity" "$(echo "$DRY" | head -c 200)"
fi

# X6
X6_T0=$(date +%s)
"$BIN" render --out "$SCRATCH/x6a.gif" >/dev/null 2>"$SCRATCH/x6a.log"; X6A=$?
"$BIN" render --text HI --text-anim bounce --out "$SCRATCH/x6b.gif" >/dev/null 2>"$SCRATCH/x6b.log"; X6B=$?
"$BIN" render --srt /tmp/__nonexistent__.srt --out "$SCRATCH/x6c.gif" >/dev/null 2>"$SCRATCH/x6c.log"; X6C=$?
X6_S=$(( $(date +%s) - X6_T0 ))
if [[ $X6A -eq 2 && $X6B -eq 2 && $X6C -eq 2 && $X6_S -lt 10 ]] \
   && grep -q "no source" "$SCRATCH/x6a.log" && grep -q "config/textSource/anim" "$SCRATCH/x6b.log" \
   && grep -q "cannot read --srt" "$SCRATCH/x6c.log"; then
  ok "X6 no source / bad anim / unreadable --srt → exit 2 in ${X6_S}s"
else
  bad "X6 usage errors" "rc=$X6A/$X6B/$X6C ${X6_S}s"
fi

# X7
render --text tiny --text-size 10 --frames 2 --out "$SCRATCH/x7.gif" 2>"$SCRATCH/x7.log"
if grep -q "text: warn: smallest stroke" "$SCRATCH/x7.log" && ! grep -q "text: warn" "$SCRATCH/x1.log"; then
  ok "X7 sub-cell stroke warns on stderr (and a 160 px title does not)"
else
  bad "X7 stroke warning" "$(grep -i warn "$SCRATCH/x7.log" | head -1)"
fi

echo "text-source: $P passed, $F failed"
[[ $F -eq 0 ]]
