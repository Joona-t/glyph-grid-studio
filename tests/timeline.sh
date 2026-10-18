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
#   L6 (ITER-085) braille shape selection on the still breathes: preserve +
#      shape and duotone + shape-edge-aware give more than one distinct frame
#      (the selection used to ignore breathing — one frame, every frame)
#   L7 (ITER-086) a brightnessGamma track under colorMode preserve with a
#      dither renders exactly like a constant gamma, and validate warns; with
#      dither none the track changes the frames and validate stays quiet
#   L8 (ITER-088) dispersal with animation.loop false, duration 7, fps 15,
#      105 frames (fps rounds to 70 ms, so frames 100-104 run past the
#      duration): the first frame has ink, the last frame is blank paper
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
rm -f "$SCRATCH"/*.mp4 "$SCRATCH"/*.gif "$SCRATCH"/*.json "$SCRATCH"/*.log

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

# L6
distinct() { ffmpeg -v error -i "$1" -f framemd5 - 2>/dev/null | grep -v '^#' | awk -F, '{print $NF}' | sort -u | wc -l | tr -d ' '; }
L6=""
for sel in "preserve shape" "duotone shape-edge-aware"; do
  set -- $sel
  render --out "$SCRATCH/l6-$1.gif" --palette cream-paper --color-mode "$1" --glyph-set braille \
    --selection-mode "$2" --dither temporal 2>"$SCRATCH/l6-$1.log"
  L6="$L6 $1/$2=$(distinct "$SCRATCH/l6-$1.gif")"
done
if ! echo "$L6" | grep -qE "=(0|1)( |$)"; then
  ok "L6 braille shape selection breathes on a still:$L6 distinct frames"
else
  bad "L6 shape selection frozen" "$L6 distinct frames | $(tail -1 "$SCRATCH/l6-preserve.log")"
fi

# L7
for d in temporal none; do
  render --out "$SCRATCH/l7-$d-const.gif" --color-mode preserve --dither "$d" --set brightnessGamma=0.4 2>/dev/null
  render --out "$SCRATCH/l7-$d-track.gif" --color-mode preserve --dither "$d" --set brightnessGamma=0.4 \
    --timeline "brightnessGamma=0:(0.4),1:(2.5)" 2>/dev/null
  printf '{"colorMode":"preserve","dither":{"mode":"%s"},"timeline":{"enabled":true,"tracks":{"brightnessGamma":[{"u":0,"v":0.4},{"u":1,"v":2.5}]}}}' "$d" > "$SCRATCH/l7-$d.json"
  timeout 30 "$BIN" validate "$SCRATCH/l7-$d.json" > "$SCRATCH/l7-$d.validate.log" 2>&1
done
M7TC=$(pixmd5 "$SCRATCH/l7-temporal-const.gif"); M7TT=$(pixmd5 "$SCRATCH/l7-temporal-track.gif")
M7NC=$(pixmd5 "$SCRATCH/l7-none-const.gif"); M7NT=$(pixmd5 "$SCRATCH/l7-none-track.gif")
if [[ -n "$M7TC" && "$M7TC" == "$M7TT" && -n "$M7NC" && "$M7NC" != "$M7NT" ]] \
   && grep -q "brightnessGamma: has no effect under colorMode" "$SCRATCH/l7-temporal.validate.log" \
   && ! grep -q "brightnessGamma" "$SCRATCH/l7-none.validate.log"; then
  ok "L7 preserve ignores a gamma track under a dither, and validate says so"
else
  bad "L7 preserve gamma warning" "temporal ${M7TC:-none}/${M7TT:-none} none ${M7NC:-none}/${M7NT:-none} | $(cat "$SCRATCH/l7-temporal.validate.log" | tail -2)"
fi

# L8
timeout "$TIMEOUT" "$BIN" render --in "$STILL" --out "$SCRATCH/l8.gif" --frames 105 --cols 96 --rows 48 \
  --palette phosphor --color-mode monochrome --ramp blockAscend --dither bayer4 \
  --set 'animation={"duration":7,"fps":15,"loop":false}' \
  --set 'dispersal={"enabled":true,"startT":0.76,"endT":0.97,"intensity":0.5,"upwardBias":0.8,"swayAmount":0.6,"rippleAmt":0.25}' \
  2>"$SCRATCH/l8.log"
L8WH=$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height -of csv=p=0:s=x "$SCRATCH/l8.gif" 2>/dev/null)
L8=$(ffmpeg -v error -i "$SCRATCH/l8.gif" -f rawvideo -pix_fmt rgb24 - 2>/dev/null | python3 -c '
import sys
W, H = (int(x) for x in sys.argv[1].split("x"))
b = sys.stdin.buffer.read(); fr = len(b) // (W * H * 3)
def ink(k):
    o = k * W * H * 3; bg = b[o:o + 3]
    return sum(1 for i in range(o, o + W * H * 3, 3) if abs(b[i] - bg[0]) + abs(b[i + 1] - bg[1]) + abs(b[i + 2] - bg[2]) > 60)
print(ink(0), ink(fr - 1))' "${L8WH:-0x0}" 2>/dev/null)
set -- $L8
if [[ "${1:-0}" -gt 1000 && "${2:-1}" == "0" ]]; then
  ok "L8 dispersal holds after endT with loop off (ink first/last frame: $L8)"
else
  bad "L8 dispersal hold" "ink first/last frame: ${L8:-none} | $(tail -1 "$SCRATCH/l8.log")"
fi

echo "timeline: $P passed, $F failed"
[[ $F -eq 0 ]]
