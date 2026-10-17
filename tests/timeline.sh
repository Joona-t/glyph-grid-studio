#!/bin/bash
# tests/timeline.sh — keyframe timeline (ITER-075, plan 5.2 / research R1).
#
# Usage: BIN=<binary> bash tests/timeline.sh [scratch-dir]
#
# Headless renders of the bundled cream-paper still, 12 frames (1 s @ 12 fps,
# --frames auto), MP4 archive; comparisons are on decoded RGB pixels:
#   L1 a 2-keyframe palette morph (cream-paper → spice): first and last
#      frames differ in mean colour, and the middle frame lies between
#   L2 structural + numeric + palette tracks (grid.cols, glyphSet,
#      selectionMode, gainSwing): exit 0, 12 frames, and two runs are
#      pixel-identical (evaluated during priming too → deterministic)
#   L3 timeline present but enabled:false is pixel-identical to no timeline
#   L4 the JSON form (--preset) and a batch manifest job config render the
#      same pixels as the CLI --timeline strings
#   L5 a malformed --timeline exits 2 before launching; a bad ease is a
#      schema error at its JSON path
# Prints "timeline: N passed, M failed"; exit 0 only when all pass.

set -u -o pipefail
cd "$(dirname "$0")/.."

BIN="${BIN:-$HOME/glyph-grid-studio/src-tauri/target/release/glyph-grid-studio}"
SCRATCH="${1:-tests/scratch/timeline}"
STILL="$PWD/tests/sources/cream-paper.png"
TIMEOUT="${TEST_TIMEOUT:-180}"

[[ -x "$BIN" ]] || { echo "timeline: binary not found at $BIN" >&2; exit 2; }

mkdir -p "$SCRATCH"
SCRATCH="$(cd "$SCRATCH" && pwd -P)"
rm -f "$SCRATCH"/*.mp4 "$SCRATCH"/*.json "$SCRATCH"/*.log

P=0; F=0
ok()  { echo "  ✓ $1"; P=$((P+1)); }
bad() { echo "  ✗ $1: $2"; F=$((F+1)); }
BASE=(--mp4-quality archive --frames auto --set animation.duration=1 --set animation.fps=12 --cols 96 --rows 48)
render() { timeout "$TIMEOUT" "$BIN" render --in "$STILL" "${BASE[@]}" "$@"; }
pixmd5() { ffmpeg -v error -i "$1" -pix_fmt rgb24 -f md5 - 2>/dev/null; }
nframes() { ffprobe -v error -count_frames -select_streams v:0 -show_entries stream=nb_read_frames -of csv=p=0 "$1" 2>/dev/null; }
# mean R,G,B of frame $2
meanrgb() {
  ffmpeg -v error -i "$1" -vf "select=eq(n\,$2)" -frames:v 1 -f rawvideo -pix_fmt rgb24 - 2>/dev/null | python3 -c '
import sys
b = sys.stdin.buffer.read(); n = len(b) // 3
print(" ".join("%.1f" % (sum(b[i::3]) / max(1, n)) for i in range(3)))'
}

MORPH=(--timeline "palette=0:(cream-paper),1:(spice)")

# L1
render --out "$SCRATCH/l1.mp4" "${MORPH[@]}" 2>"$SCRATCH/l1.log"
L1N=$(nframes "$SCRATCH/l1.mp4")
L1=$(python3 - "$(meanrgb "$SCRATCH/l1.mp4" 0)" "$(meanrgb "$SCRATCH/l1.mp4" 6)" "$(meanrgb "$SCRATCH/l1.mp4" $(( ${L1N:-1} - 1 )))" <<'EOF'
import sys
try:
    a, m, z = ([float(x) for x in s.split()] for s in sys.argv[1:4])
except Exception:
    print("could not decode frames"); sys.exit(1)
d = sum(abs(x - y) for x, y in zip(a, z))
mid = all(min(x, y) - 1 <= v <= max(x, y) + 1 for x, y, v in zip(a, z, m))
msg = "first %s, mid %s, last %s (|d| %.1f)" % (a, m, z, d)
if d < 30: print("first/last too close: " + msg); sys.exit(1)
if not mid: print("middle frame not between: " + msg); sys.exit(1)
print(msg)
EOF
) && ok "L1 palette morph: $L1" || bad "L1 palette morph mean colour" "$L1 | $(tail -2 "$SCRATCH/l1.log")"

# L2
STRUCT=(--timeline "grid.cols=0:(64),0.5:(128)"
        --timeline "glyphSet=0:(ascii),0.25:(braille)"
        --timeline "selectionMode=0:(brightness),0.75:(shape)"
        --timeline "studio.breathing.gainSwing=0:(0.1),0.5:(0.6):inOutSine"
        "${MORPH[@]}")
render --out "$SCRATCH/l2a.mp4" "${STRUCT[@]}" 2>"$SCRATCH/l2a.log"; C1=$?
render --out "$SCRATCH/l2b.mp4" "${STRUCT[@]}" 2>"$SCRATCH/l2b.log"; C2=$?
N2=$(nframes "$SCRATCH/l2a.mp4")
M2A=$(pixmd5 "$SCRATCH/l2a.mp4"); M2B=$(pixmd5 "$SCRATCH/l2b.mp4")
if [[ "$C1$C2" == "00" && "${N2:-0}" -ge 12 && -n "$M2A" && "$M2A" == "$M2B" ]]; then
  ok "L2 structural tracks render $N2 frames, two runs pixel-identical"
else
  bad "L2 structural timeline" "exit=$C1/$C2 frames=$N2 md5 ${M2A:-none} vs ${M2B:-none} | $(tail -2 "$SCRATCH/l2a.log")"
fi

# L3
render --out "$SCRATCH/l3a.mp4" 2>"$SCRATCH/l3a.log"
cat > "$SCRATCH/off.json" <<'EOF'
{"timeline": {"enabled": false, "tracks": {"palette": [{"u": 0, "v": "cream-paper"}, {"u": 1, "v": "spice"}]}}}
EOF
render --out "$SCRATCH/l3b.mp4" --preset "$SCRATCH/off.json" 2>"$SCRATCH/l3b.log"
M3A=$(pixmd5 "$SCRATCH/l3a.mp4"); M3B=$(pixmd5 "$SCRATCH/l3b.mp4")
if [[ -n "$M3A" && "$M3A" == "$M3B" ]]; then
  ok "L3 enabled:false timeline is pixel-identical to none"
else
  bad "L3 disabled timeline" "md5 ${M3A:-none} vs ${M3B:-none}"
fi

# L4
cat > "$SCRATCH/on.json" <<'EOF'
{"timeline": {"enabled": true, "tracks": {"palette": [{"u": 0, "v": "cream-paper"}, {"u": 1, "v": "spice"}]}}}
EOF
render --out "$SCRATCH/l4p.mp4" --preset "$SCRATCH/on.json" 2>"$SCRATCH/l4p.log"
cat > "$SCRATCH/m.json" <<EOF
{"in": "$STILL", "frames": "auto", "jobs": [
  {"name": "tl", "out": "$SCRATCH/l4b.mp4", "mp4Quality": "archive", "config": {
    "animation": {"duration": 1, "fps": 12}, "grid": {"cols": 96, "rows": 48},
    "timeline": {"enabled": true, "tracks": {"palette": [{"u": 0, "v": "cream-paper"}, {"u": 1, "v": "spice"}]}}}}
]}
EOF
timeout "$TIMEOUT" "$BIN" batch --manifest "$SCRATCH/m.json" 2>"$SCRATCH/l4b.log"
M1=$(pixmd5 "$SCRATCH/l1.mp4"); M4P=$(pixmd5 "$SCRATCH/l4p.mp4"); M4B=$(pixmd5 "$SCRATCH/l4b.mp4")
if [[ -n "$M1" && "$M1" == "$M4P" && "$M1" == "$M4B" ]]; then
  ok "L4 preset JSON + batch manifest timeline == CLI --timeline (pixels)"
else
  bad "L4 JSON form parity" "cli ${M1:-none} preset ${M4P:-none} batch ${M4B:-none} | $(tail -2 "$SCRATCH/l4b.log")"
fi

# L5
OUT5=$(timeout 30 "$BIN" render --in "$STILL" --out "$SCRATCH/l5.mp4" --timeline "palette=0:(spice" 2>&1); C5=$?
OUT5B=$(timeout 30 "$BIN" render --in "$STILL" --out "$SCRATCH/l5.mp4" --dry-run --timeline "seed=0:(1):bouncy" 2>&1); C5B=$?
if [[ "$C5" == "2" && "$C5B" == "2" && ! -e "$SCRATCH/l5.mp4" ]] \
   && echo "$OUT5" | grep -q -- "--timeline" && echo "$OUT5B" | grep -q "timeline/tracks/seed/0/ease"; then
  ok "L5 malformed --timeline / bad ease exit 2 before launching"
else
  bad "L5 timeline errors" "exit=$C5/$C5B | $(echo "$OUT5" | tail -1) | $(echo "$OUT5B" | tail -1)"
fi

echo "timeline: $P passed, $F failed"
[[ $F -eq 0 ]]
