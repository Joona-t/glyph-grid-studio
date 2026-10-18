#!/bin/bash
# tests/text-export.sh — terminal-native export (ITER-074, research R2).
#
# Usage: BIN=<binary> bash tests/text-export.sh [scratch-dir]
#
# CLI `render --format txt|ansi|cast|html` and a batch manifest with text
# jobs, on the bundled cream-paper still:
#   T1 txt is non-empty and exactly rows x cols characters
#   T2 ansi carries 24-bit SGR escapes, one line per grid row
#   T3 cast is asciinema v2: every line parses as JSON, header + events
#   T4 html is one file with the bundled WOFF2 inlined as a data URI
#   T5 octant under ansi warns CR-8 on stderr (and still writes)
#   T6 an unknown --format exits 2 before launching anything
#   T7 batch: txt/cast jobs written, with their --resume sidecars
#   T8 txt is deterministic: two renders are byte-identical
# Prints "text-export: N passed, M failed"; exit 0 only when all pass.

set -u -o pipefail
cd "$(dirname "$0")/.."

BIN="${BIN:-$HOME/glyph-grid-studio/src-tauri/target/release/glyph-grid-studio}"
SCRATCH="${1:-tests/scratch/text-export}"
STILL="$PWD/tests/sources/cream-paper.png"
TIMEOUT="${TEST_TIMEOUT:-180}"

[[ -x "$BIN" ]] || { echo "text-export: binary not found at $BIN" >&2; exit 2; }

mkdir -p "$SCRATCH"
SCRATCH="$(cd "$SCRATCH" && pwd -P)"
rm -f "$SCRATCH"/*.txt "$SCRATCH"/*.ans "$SCRATCH"/*.cast "$SCRATCH"/*.html "$SCRATCH"/*.json "$SCRATCH"/*.log

P=0; F=0
ok()  { echo "  ✓ $1"; P=$((P+1)); }
bad() { echo "  ✗ $1: $2"; F=$((F+1)); }
render() { timeout "$TIMEOUT" "$BIN" render --in "$STILL" "$@"; }

COLS=96; ROWS=48
GRID=(--cols "$COLS" --rows "$ROWS")

# T1
render --out "$SCRATCH/t1.txt" "${GRID[@]}" --palette cream-paper --color-mode monochrome 2>"$SCRATCH/t1.log"
T1=$(python3 - "$SCRATCH/t1.txt" "$COLS" "$ROWS" <<'EOF'
import sys
p, cols, rows = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
try:
    body = open(p, encoding="utf-8").read()
except Exception as e:
    print("unreadable: %s" % e); sys.exit(1)
lines = body.split("\n")
if lines[-1] != "": print("no trailing newline"); sys.exit(1)
lines = lines[:-1]
inked = sum(1 for l in lines for c in l if c != " ")
if len(lines) != rows: print("%d rows, want %d" % (len(lines), rows)); sys.exit(1)
bad = [i for i, l in enumerate(lines) if len(l) != cols]
if bad: print("row %d has %d chars, want %d" % (bad[0], len(lines[bad[0]]), cols)); sys.exit(1)
if inked == 0: print("all blank"); sys.exit(1)
print("%dx%d, %d inked cells" % (cols, rows, inked))
EOF
) && ok "T1 txt is $T1" || bad "T1 txt rows x cols" "$T1 | $(tail -2 "$SCRATCH/t1.log")"

# T2
render --out "$SCRATCH/t2.ans" "${GRID[@]}" 2>"$SCRATCH/t2.log"
T2=$(python3 - "$SCRATCH/t2.ans" "$ROWS" <<'EOF'
import sys
try:
    body = open(sys.argv[1], encoding="utf-8").read()
except Exception as e:
    print("unreadable: %s" % e); sys.exit(1)
rows = body.split("\n")[:-1]
if len(rows) != int(sys.argv[2]): print("%d lines" % len(rows)); sys.exit(1)
if not all(r.startswith("\x1b[48;2;") and r.endswith("\x1b[0m") for r in rows): print("row framing"); sys.exit(1)
n = body.count("\x1b[38;2;")
if n == 0: print("no 24-bit fg escapes"); sys.exit(1)
print("%d rows, %d fg escapes, %d KB" % (len(rows), n, len(body.encode()) // 1024))
EOF
) && ok "T2 ansi $T2" || bad "T2 ansi" "$T2 | $(tail -2 "$SCRATCH/t2.log")"

# T3
render --out "$SCRATCH/t3.cast" "${GRID[@]}" --frames 6 2>"$SCRATCH/t3.log"
T3=$(python3 - "$SCRATCH/t3.cast" "$COLS" "$ROWS" <<'EOF'
import json, sys
try:
    lines = open(sys.argv[1], encoding="utf-8").read().splitlines()
    docs = [json.loads(l) for l in lines]
except Exception as e:
    print("not JSON lines: %s" % e); sys.exit(1)
h, ev = docs[0], docs[1:]
if h.get("version") != 2 or h.get("width") != int(sys.argv[2]) or h.get("height") != int(sys.argv[3]):
    print("header %r" % h); sys.exit(1)
if not ev or not all(isinstance(e, list) and len(e) == 3 and e[1] == "o" for e in ev): print("bad events"); sys.exit(1)
if "\x1b[2J" not in ev[0][2]: print("frame 0 does not clear"); sys.exit(1)
ts = [e[0] for e in ev]
if ts != sorted(ts): print("times not monotonic"); sys.exit(1)
print("v2 %sx%s, %d events, %.3fs" % (h["width"], h["height"], len(ev), ts[-1]))
EOF
) && ok "T3 cast $T3" || bad "T3 cast" "$T3 | $(tail -2 "$SCRATCH/t3.log")"

# T4
render --out "$SCRATCH/t4.html" "${GRID[@]}" --frames 3 2>"$SCRATCH/t4.log"
T4=$(python3 - "$SCRATCH/t4.html" "$PWD/src/fonts/cascadia-mono-subset.woff2" <<'EOF'
import base64, re, sys
try:
    body = open(sys.argv[1], encoding="utf-8").read()
except Exception as e:
    print("unreadable: %s" % e); sys.exit(1)
m = re.search(r"data:font/woff2;base64,([A-Za-z0-9+/=]+)", body)
if not m: print("no font data URI"); sys.exit(1)
if base64.b64decode(m.group(1)) != open(sys.argv[2], "rb").read(): print("inlined font != bundled WOFF2"); sys.exit(1)
if 'id="frames"' not in body or "prefers-reduced-motion" not in body: print("no stepper"); sys.exit(1)
if re.search(r'<(?:script|link)[^>]+(?:src|href)=', body): print("external reference"); sys.exit(1)
print("self-contained, font inlined (%d KB)" % (len(body.encode()) // 1024))
EOF
) && ok "T4 html $T4" || bad "T4 html" "$T4 | $(tail -2 "$SCRATCH/t4.log")"

# T5
render --out "$SCRATCH/t5.ans" "${GRID[@]}" --selection-mode shape --glyph-set octant 2>"$SCRATCH/t5.log"
if grep -q "CR-8" "$SCRATCH/t5.log" && [[ -s "$SCRATCH/t5.ans" ]]; then
  ok "T5 octant under ansi warns CR-8 and still writes"
else
  bad "T5 CR-8 warning" "$(grep -c CR-8 "$SCRATCH/t5.log") CR-8 lines, file $( [[ -s "$SCRATCH/t5.ans" ]] && echo present || echo missing)"
fi

# T6
render --out "$SCRATCH/t6.txt" --format webm >/dev/null 2>"$SCRATCH/t6.log"
T6_CODE=$?
if [[ "$T6_CODE" == "2" ]] && grep -q "unknown output format" "$SCRATCH/t6.log" && [[ ! -e "$SCRATCH/t6.txt" ]]; then
  ok "T6 unknown --format exits 2"
else
  bad "T6 unknown --format" "exit=$T6_CODE $(tail -1 "$SCRATCH/t6.log")"
fi

# T7
cat > "$SCRATCH/m.json" <<EOF
{"in": "$STILL", "frames": 4, "jobs": [
  {"name": "bt", "out": "$SCRATCH/b1.txt", "config": {"grid": {"cols": $COLS, "rows": $ROWS}}},
  {"name": "bc", "out": "$SCRATCH/b2.cast", "format": "cast", "config": {"palette": "spice", "grid": {"cols": $COLS, "rows": $ROWS}}}
]}
EOF
timeout "$TIMEOUT" "$BIN" batch --manifest "$SCRATCH/m.json" 2>"$SCRATCH/t7.log"
T7_CODE=$?
if [[ "$T7_CODE" == "0" && -s "$SCRATCH/b1.txt" && -s "$SCRATCH/b2.cast" \
      && -f "$SCRATCH/b1.txt.ggs.json" && -f "$SCRATCH/b2.cast.ggs.json" ]]; then
  ok "T7 batch writes txt + cast jobs with resume sidecars"
else
  bad "T7 batch text jobs" "exit=$T7_CODE $(tail -2 "$SCRATCH/t7.log")"
fi

# T8
render --out "$SCRATCH/t8.txt" "${GRID[@]}" --palette cream-paper --color-mode monochrome 2>/dev/null
if [[ -s "$SCRATCH/t8.txt" ]] && cmp -s "$SCRATCH/t1.txt" "$SCRATCH/t8.txt"; then
  ok "T8 txt deterministic ($(md5 -q "$SCRATCH/t8.txt"))"
else
  bad "T8 txt deterministic" "t1 vs t8 differ"
fi

echo "  text-export: $P passed, $F failed"
[[ "$F" == "0" ]]
