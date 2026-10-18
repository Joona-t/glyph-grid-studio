#!/bin/bash
# Comprehensive v0.1 test runner. Runs phases A-D + F (E is manual GUI).
# Outputs PASS/FAIL per test; writes RESULTS-V0.1.md at the end.
#
# Run: bash tests/run-all.sh
# Re-run after a rebuild: cargo tauri build && bash tests/run-all.sh

set -u
cd "$(dirname "$0")/.."  # repo root

# BIN overridable via env (audit 2026-06-10) — lets worktrees/CI point the
# gate at their own build instead of the hardcoded primary-tree bundle.
BIN="${BIN:-$HOME/glyph-grid-studio/src-tauri/target/release/bundle/macos/Glyph Grid Studio.app/Contents/MacOS/glyph-grid-studio}"
TEST_IMG="${TEST_IMG:-$HOME/Downloads/Thor.png}"

# Per-invocation timeout (audit 2026-06-10): a single WKWebView first-launch
# flake used to hang the whole gate forever (35 min observed). Any render
# that needs >3 min is a failure, not a wait.
bin_t() { timeout "${TEST_TIMEOUT:-180}" "$BIN" "$@"; }
SCRATCH="tests/scratch"
RESULTS="tests/RESULTS-V0.1.md"

mkdir -p "$SCRATCH"
rm -f "$SCRATCH"/*.gif "$SCRATCH"/*.json "$SCRATCH"/*.ndjson "$SCRATCH"/*.txt

PASS=0
FAIL=0
FAIL_NAMES=()

pass()   { echo "  ✓ $1"; PASS=$((PASS+1)); }
fail()   { echo "  ✗ $1: $2"; FAIL=$((FAIL+1)); FAIL_NAMES+=("$1: $2"); }
section(){ echo; echo "=== $1 ==="; }

# Sanity: binary + test image exist
[[ -x "$BIN" ]] || { echo "ERROR: binary not found at $BIN — run cargo tauri build first"; exit 2; }
[[ -f "$TEST_IMG" ]] || { echo "ERROR: test image not found at $TEST_IMG"; exit 2; }

# ---------- Phase A: Static source-grep ----------
section "Phase A — Static source"

# A1: no hardcoded home paths in source (catches any /Users/<name>/ leak)
if ! grep -rqIE "/Users/[^/]+/" src/ src-tauri/src/ 2>/dev/null; then
  pass "A1 no hardcoded home paths in source"
else
  fail "A1 no hardcoded home paths in source" "$(grep -rInE '/Users/[^/]+/' src/ src-tauri/src/ 2>/dev/null | head -2)"
fi

# A2: no eron/claire/claymore strings
if ! grep -irqI "eron\|claire\|claymore" src/ src-tauri/src/ 2>/dev/null; then
  pass "A2 no personal-context names in source"
else
  fail "A2 no personal-context names in source" "$(grep -irIn 'eron\|claire\|claymore' src/ src-tauri/src/ 2>/dev/null | head -2)"
fi

# A3: no surprise TODOs
TODO_COUNT=$(grep -rIn "TODO\|FIXME\|XXX" src/ src-tauri/src/ 2>/dev/null | wc -l | tr -d ' ')
if [[ "$TODO_COUNT" == "0" ]]; then
  pass "A3 no TODOs/FIXMEs in source"
else
  pass "A3 TODOs present ($TODO_COUNT) — review acceptable"
fi

# A4: Cargo.toml metadata
META_OK=true
grep -q '^description = "[^"]\+"' src-tauri/Cargo.toml || META_OK=false
grep -q '^repository = "[^"]\+"' src-tauri/Cargo.toml || META_OK=false
grep -q '^authors = \[' src-tauri/Cargo.toml || META_OK=false
if $META_OK; then pass "A4 Cargo.toml metadata complete"; else fail "A4 Cargo.toml metadata complete" "missing field"; fi

# A5: required public files exist (internal planning docs live in docs/internal/, not the public tree)
ALL_FILES=true
for f in src-tauri/src/mcp.rs docs/mcp.md Casks/glyph-grid-studio.rb \
         README.md LICENSE USER-GUIDE.md RELEASING.md \
         BUGS_AND_ITERATIONS.md docs/index.html; do
  if [[ ! -f "$f" ]]; then
    fail "A5 expected file exists" "$f missing"
    ALL_FILES=false
  fi
done
$ALL_FILES && pass "A5 all expected files exist"

# A6: eron.png removed
if [[ ! -e "src/assets/eron.png" ]]; then
  pass "A6 src/assets/eron.png removed"
else
  fail "A6 src/assets/eron.png removed" "still present"
fi

# A7 (audit 2026-06-10): version sync — Cargo.toml / tauri.conf.json /
# package.json must agree. Drift between them is now a test failure.
V_CARGO=$(grep -m1 '^version' src-tauri/Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
V_TAURI=$(python3 -c "import json;print(json.load(open('src-tauri/tauri.conf.json'))['version'])")
V_NPM=$(python3 -c "import json;print(json.load(open('package.json'))['version'])")
if [[ -n "$V_CARGO" && "$V_CARGO" == "$V_TAURI" && "$V_TAURI" == "$V_NPM" ]]; then
  pass "A7 version sync ($V_CARGO across Cargo.toml/tauri.conf.json/package.json)"
else
  fail "A7 version sync" "cargo=$V_CARGO tauri=$V_TAURI npm=$V_NPM"
fi

# A8 (v0.1.9): parse the inline app and exercise the staged native-export,
# lazy-ZIP, cancellation, resource-limit, and scheduling contracts.
if npm test >/dev/null 2>&1; then
  pass "A8 frontend performance/export regression tests"
else
  fail "A8 frontend performance/export regression tests" "npm test failed"
fi

# A9 (R7, ITER-056): the preset schema is generated from the live CONFIG —
# a CONFIG / option-table change without a regen is a gate failure.
if A9_OUT=$(node scripts/gen-schema.js --check 2>&1); then
  pass "A9 $A9_OUT"
else
  fail "A9 preset schema up to date" "$A9_OUT"
fi

# ---------- Phase B: CLI ----------
section "Phase B — CLI surface"

# B1: --help lists ALL FIVE subcommands (audit 2026-06-10: previously only
# checked 4 — batch could vanish from the CLI without failing the gate).
HELP=$("$BIN" --help 2>&1)
B1_OK=1
for sub in studio render batch catalog validate mcp; do
  echo "$HELP" | grep -q "$sub" || { B1_OK=0; B1_MISSING="$sub"; }
done
if [[ $B1_OK -eq 1 ]]; then
  pass "B1 --help lists all 6 subcommands (studio/render/batch/catalog/validate/mcp)"
else
  fail "B1 --help lists all 6 subcommands" "missing: $B1_MISSING"
fi

# B2: render --help lists all 14 flags
RENDER_HELP=$("$BIN" render --help 2>&1)
B2_OK=true
for flag in --in --out --frames --palette --color-mode --ramp --dither \
            --selection-mode --glyph-set --sampling-strategy --postprocess \
            --cols --rows --preset --show-window --dry-run --set; do
  if ! echo "$RENDER_HELP" | grep -q -- "$flag"; then
    fail "B2 render --help has $flag" "missing"
    B2_OK=false
  fi
done
$B2_OK && pass "B2 render --help lists all 17 flags"

# B3: catalog returns valid JSON with 8 keys
CATALOG=$("$BIN" catalog 2>&1)
if echo "$CATALOG" | python3 -c "
import sys, json
data = json.load(sys.stdin)
needed = {'palettes','color_modes','ramps','dithers','selection_modes','glyph_sets','sampling_strategies','postprocess_stages'}
missing = needed - set(data.keys())
empty = [k for k in needed if k in data and not data[k]]
if missing: print('MISSING:', missing); sys.exit(1)
if empty: print('EMPTY:', empty); sys.exit(1)
print('ok', len(data['palettes']), 'palettes,', len(data['ramps']), 'ramps')
" 2>&1; then
  pass "B3 catalog returns valid JSON with 8 keys"
else
  fail "B3 catalog returns valid JSON with 8 keys" "$CATALOG"
fi

# B4: minimal render
B4_OUT="$SCRATCH/b4_minimal.gif"
rm -f "$B4_OUT"
if bin_t render --in "$TEST_IMG" --out "$B4_OUT" --frames 6 >/dev/null 2>&1 && [[ -f "$B4_OUT" ]]; then
  if file "$B4_OUT" | grep -q "GIF image data, version 89a"; then
    SIZE=$(stat -f%z "$B4_OUT")
    if [[ $SIZE -gt 200000 && $SIZE -lt 15000000 ]]; then
      pass "B4 minimal render (size: $((SIZE/1024)) KB)"
    else
      fail "B4 minimal render" "size $SIZE out of range"
    fi
  else
    fail "B4 minimal render" "not GIF89a"
  fi
else
  fail "B4 minimal render" "command failed or no output"
fi

# B5: full-flag render
B5_OUT="$SCRATCH/b5_full.gif"
rm -f "$B5_OUT"
if bin_t render \
    --in "$TEST_IMG" --out "$B5_OUT" --frames 6 \
    --palette spice --color-mode gradient --ramp blockAscend \
    --dither stbn --selection-mode shape-edge-aware --glyph-set octant \
    --sampling-strategy average \
    --postprocess vignette --postprocess crtBeam \
    --cols 200 --rows 100 \
    >/dev/null 2>&1 && [[ -f "$B5_OUT" ]]; then
  pass "B5 full-flag render ($((($(stat -f%z "$B5_OUT")/1024))) KB)"
else
  fail "B5 full-flag render" "command failed or no output"
fi

# B7: error path - bad input
if bin_t render --in /tmp/__nonexistent_file__.png --out "$SCRATCH/b7.gif" --frames 6 >/dev/null 2>&1; then
  fail "B7 bad input fails non-zero" "exited zero with bad input"
else
  pass "B7 bad input exits non-zero"
fi

# B13-B16 (R7, ITER-056): schema validation fails fast with JSON paths,
# before Tauri starts — none of these may launch a render.
B13_GOOD="$SCRATCH/b13-good.json"; B13_BAD="$SCRATCH/b13-bad.json"
echo '{"$schema":"x","version":1,"palette":"spice","dither":{"mode":"stbn"},"postprocess":{"vignette":{"enabled":true,"strength":0.4}}}' > "$B13_GOOD"
echo '{"palette":"cream","dither":{"mode":"STBN"}}' > "$B13_BAD"
B13_ERR=$("$BIN" validate "$B13_BAD" 2>&1); B13_RC=$?
if "$BIN" validate "$B13_GOOD" >/dev/null 2>&1 && [[ $B13_RC -eq 2 ]] \
   && echo "$B13_ERR" | grep -q "/palette:" && echo "$B13_ERR" | grep -q "/dither/mode:"; then
  pass "B13 validate: good preset exit 0, bad enums exit 2 with JSON paths"
else
  fail "B13 validate subcommand" "rc=$B13_RC $(echo "$B13_ERR" | head -2 | tr '\n' ' ')"
fi

B14_T0=$(date +%s)
B14_ERR=$(bin_t render --in "$TEST_IMG" --out "$SCRATCH/b14.gif" --frames 2 --preset /tmp/__nonexistent_preset__.json 2>&1); B14_RC=$?
B14_S=$(( $(date +%s) - B14_T0 ))
if [[ $B14_RC -eq 2 && ! -f "$SCRATCH/b14.gif" && $B14_S -lt 10 ]] && echo "$B14_ERR" | grep -q "cannot read --preset"; then
  pass "B14 unreadable --preset exits 2 in ${B14_S}s (no silent defaults)"
else
  fail "B14 unreadable --preset exits 2" "rc=$B14_RC ${B14_S}s $(echo "$B14_ERR" | tail -1)"
fi

B15_JSON=$("$BIN" render --in "$TEST_IMG" --out "$SCRATCH/b15.gif" --preset "$B13_GOOD" --palette phosphor --dry-run 2>/dev/null)
B15_RC=$?
if [[ $B15_RC -eq 0 && ! -f "$SCRATCH/b15.gif" ]] && echo "$B15_JSON" | python3 -c "
import sys, json
c = json.load(sys.stdin)['config']
assert c['palette'] == 'phosphor', c          # flag beats preset
assert c['dither']['mode'] == 'stbn', c       # preset survives
assert '\$schema' not in c and 'version' not in c, c
" 2>&1; then
  pass "B15 render --dry-run prints the resolved config without rendering"
else
  fail "B15 render --dry-run" "rc=$B15_RC $(echo "$B15_JSON" | head -3 | tr '\n' ' ')"
fi

B16_MANIFEST="$SCRATCH/b16-manifest.json"
B16_ROOT="$(pwd -P)"
cat > "$B16_MANIFEST" <<EOF
{"in": "$B16_ROOT/tests/sources/cream-paper.png", "frames": 2, "jobs": [
  {"name": "ok", "out": "$B16_ROOT/$SCRATCH/b16a.gif", "config": {"palette": "spice"}},
  {"name": "typo", "out": "$B16_ROOT/$SCRATCH/b16b.gif", "config": {"selectionMode": "shapes"}}]}
EOF
B16_ERR=$(bin_t batch --manifest "$B16_MANIFEST" 2>&1); B16_RC=$?
if [[ $B16_RC -eq 2 && ! -f "$SCRATCH/b16a.gif" ]] && echo "$B16_ERR" | grep -q '"typo") config /selectionMode:'; then
  pass "B16 batch refuses a manifest with an invalid job config before rendering"
else
  fail "B16 batch job config validation" "rc=$B16_RC $(echo "$B16_ERR" | tail -1)"
fi

# B17-B18 (F24, ITER-057): one merger — flags and --set layer onto the
# preset (nested params survive), --frames auto, per-job frames + border.
B17_JSON=$("$BIN" render --in "$TEST_IMG" --out "$SCRATCH/b17.gif" --preset "$B13_GOOD" \
  --postprocess vignette --cols 90 --set seed=7 --set 'grid.rows=45' --frames auto --dry-run 2>/dev/null)
B17_RC=$?
if [[ $B17_RC -eq 0 ]] && echo "$B17_JSON" | python3 -c "
import sys, json
d = json.load(sys.stdin); c = d['config']
assert d['frames'] == 'auto', d['frames']
assert c['postprocess']['vignette'] == {'enabled': True, 'strength': 0.4}, c['postprocess']   # strength survives
assert c['postprocess']['bloom'] == {'enabled': False}, c['postprocess']                     # listed-stages-only
assert c['grid'] == {'cols': 90, 'rows': 45}, c['grid']
assert c['seed'] == 7 and c['dither']['mode'] == 'stbn', c
" 2>&1; then
  pass "B17 flags + --set deep-merge onto the preset; --frames auto"
else
  fail "B17 flags/--set layering" "rc=$B17_RC $(echo "$B17_JSON" | head -2 | tr '\n' ' ')"
fi

B18_ROOT="$(pwd -P)"
B18_MANIFEST="$SCRATCH/b18-manifest.json"
cat > "$B18_MANIFEST" <<EOF
{"in": "$B18_ROOT/tests/sources/cream-paper.png", "frames": 3, "jobs": [
  {"name": "plain",  "out": "$B18_ROOT/$SCRATCH/b18a.gif", "config": {"palette": "spice"}},
  {"name": "border", "out": "$B18_ROOT/$SCRATCH/b18b.gif", "border": {"width": 16}, "config": {"palette": "spice"}},
  {"name": "auto",   "out": "$B18_ROOT/$SCRATCH/b18c.gif", "frames": "auto",
   "config": {"animation": {"duration": 0.2, "fps": 25}}}]}
EOF
probe_wh_n() { ffprobe -v error -count_frames -select_streams v:0 -show_entries stream=width,height,nb_read_frames -of csv=p=0 "$1" 2>/dev/null | tr -d '[:space:]'; }
if bin_t batch --manifest "$B18_MANIFEST" >/dev/null 2>"$SCRATCH/b18-stderr.txt"; then
  A=$(probe_wh_n "$SCRATCH/b18a.gif"); Bx=$(probe_wh_n "$SCRATCH/b18b.gif"); C=$(probe_wh_n "$SCRATCH/b18c.gif")
  if python3 -c "
a, b, c = [tuple(map(int, s.split(','))) for s in '$A $Bx $C'.split()]
assert a[2] == 3 and b[2] == 3, (a, b)
assert (b[0], b[1]) == (a[0] + 32, a[1] + 32), (a, b)   # 16 px border on each side
assert c[2] == 5, c                                     # 0.2 s x 25 fps
" 2>&1; then
    pass "B18 batch per-job border (+32 px) and frames:auto (5) ($A / $Bx / $C)"
  else
    fail "B18 batch per-job frames/border" "a=$A b=$Bx c=$C"
  fi
else
  fail "B18 batch per-job frames/border" "batch failed: $(tail -1 "$SCRATCH/b18-stderr.txt")"
fi

# B19 (F25, ITER-059): --timeout bounds a render — exit 3, no output, no
# stray .part, well before the render could have finished (~20 s).
B19_T0=$(date +%s)
B19_ERR=$(bin_t render --in "$TEST_IMG" --out "$SCRATCH/b19.gif" --frames 120 --timeout 2 2>&1); B19_RC=$?
B19_S=$(( $(date +%s) - B19_T0 ))
B19_PARTS=$(ls -a "$SCRATCH" | grep -c '^\.b19\.gif\..*\.part$')
if [[ $B19_RC -eq 3 && ! -f "$SCRATCH/b19.gif" && $B19_PARTS -eq 0 && $B19_S -le 8 ]] && echo "$B19_ERR" | grep -q "timed out"; then
  pass "B19 render --timeout 2 exits 3 in ${B19_S}s, no output / .part left"
else
  fail "B19 render --timeout exits 3" "rc=$B19_RC ${B19_S}s parts=$B19_PARTS $(echo "$B19_ERR" | tail -1)"
fi

# B20 (F26, ITER-060): --progress = NDJSON on stderr, every capture frame
# of every job, named by the manifest job.
B20_ROOT="$(pwd -P)"
B20_MANIFEST="$SCRATCH/b20-manifest.json"
cat > "$B20_MANIFEST" <<EOF
{"in": "$B20_ROOT/tests/sources/cream-paper.png", "frames": 3, "jobs": [
  {"name": "p1", "out": "$B20_ROOT/$SCRATCH/b20a.gif"},
  {"name": "p2", "out": "$B20_ROOT/$SCRATCH/b20b.mp4", "frames": 2}]}
EOF
if bin_t batch --manifest "$B20_MANIFEST" --progress >/dev/null 2>"$SCRATCH/b20-stderr.txt" && python3 -c "
import json
lines = [json.loads(l) for l in open('$SCRATCH/b20-stderr.txt') if l.startswith('{\"type\":\"progress\"')]
cap = [(d['job'], d['frame'], d['total']) for d in lines if d['phase'] == 'capture']
assert cap == [('p1', 1, 3), ('p1', 2, 3), ('p1', 3, 3), ('p2', 1, 2), ('p2', 2, 2)], cap
assert any(d['phase'] != 'capture' for d in lines), 'no encode progress'
print(len(lines))
" > "$SCRATCH/b20-n.txt" 2>&1; then
  pass "B20 batch --progress: NDJSON capture lines per job + encode phases ($(cat "$SCRATCH/b20-n.txt") lines)"
else
  fail "B20 --progress NDJSON" "$(tail -1 "$SCRATCH/b20-n.txt" 2>/dev/null) $(tail -1 "$SCRATCH/b20-stderr.txt")"
fi

# B21 (F26, ITER-060): SIGINT mid-render exits 130 within ~1 s, leaves no
# output and no .part file.
"$BIN" render --in "$TEST_IMG" --out "$SCRATCH/b21.gif" --frames 120 --progress 2>"$SCRATCH/b21-stderr.txt" &
B21_PID=$!
for _ in $(seq 1 150); do grep -q '"phase":"capture"' "$SCRATCH/b21-stderr.txt" 2>/dev/null && break; sleep 0.2; done
kill -INT $B21_PID; B21_T0=$(date +%s)
wait $B21_PID; B21_RC=$?
B21_S=$(( $(date +%s) - B21_T0 ))
B21_PARTS=$(ls -a "$SCRATCH" | grep -c '^\.b21\.gif\..*\.part$')
if [[ $B21_RC -eq 130 && ! -f "$SCRATCH/b21.gif" && $B21_PARTS -eq 0 && $B21_S -le 3 ]]; then
  pass "B21 SIGINT mid-capture exits 130 in ${B21_S}s, no output / .part left"
else
  fail "B21 SIGINT exits 130" "rc=$B21_RC ${B21_S}s parts=$B21_PARTS $(tail -1 "$SCRATCH/b21-stderr.txt")"
fi

# B22 (F27, ITER-061): the manifest schema — a typo'd job key and an
# unknown version both exit 2 before Tauri starts (budget 100 ms, warm).
B22_ROOT="$(pwd -P)"
echo "{\"version\":1,\"in\":\"$B22_ROOT/tests/sources/cream-paper.png\",\"frames\":2,\"jobs\":[{\"name\":\"a\",\"out\":\"$B22_ROOT/$SCRATCH/b22.gif\",\"capwidth\":720}]}" > "$SCRATCH/b22-typo.json"
echo '{"version":2,"in":"x.png","jobs":[]}' > "$SCRATCH/b22-v2.json"
"$BIN" batch --manifest "$SCRATCH/b22-typo.json" >/dev/null 2>&1   # warm the binary
B22_OUT=$(python3 - "$BIN" "$SCRATCH" <<'PY'
import subprocess, sys, time
bin_, scratch = sys.argv[1], sys.argv[2]
best = 1e9
for _ in range(3):
    t = time.time()
    r = subprocess.run([bin_, "batch", "--manifest", scratch + "/b22-typo.json"], capture_output=True, text=True)
    best = min(best, (time.time() - t) * 1000)
v = subprocess.run([bin_, "batch", "--manifest", scratch + "/b22-v2.json"], capture_output=True, text=True)
okk = (r.returncode == 2 and "/jobs/0" in r.stderr and "capwidth" in r.stderr
       and v.returncode == 2 and "/version" in v.stderr and best < 100)
print("%s %.0f" % ("ok" if okk else "bad", best), r.returncode, v.returncode, r.stderr.strip().splitlines()[:1])
PY
)
if [[ "$B22_OUT" == ok* && ! -f "$SCRATCH/b22.gif" ]]; then
  pass "B22 manifest typo / version 2 exit 2 before launch ($(echo "$B22_OUT" | cut -d' ' -f2) ms)"
else
  fail "B22 manifest schema fails fast" "$B22_OUT"
fi

# B23 (F27, ITER-061): --resume skips jobs whose output is a valid file of
# the requested frame count and re-renders a truncated one.
B23_MANIFEST="$SCRATCH/b23-manifest.json"
cat > "$B23_MANIFEST" <<EOF
{"version": 1, "in": "$B22_ROOT/tests/sources/cream-paper.png", "frames": 2, "jobs": [
  {"name": "keep", "out": "$B22_ROOT/$SCRATCH/b23a.gif"},
  {"name": "redo", "out": "$B22_ROOT/$SCRATCH/b23b.mp4"},
  {"name": "more", "out": "$B22_ROOT/$SCRATCH/b23c.gif", "frames": 3}]}
EOF
B23_ERR=""
if bin_t batch --manifest "$B23_MANIFEST" >/dev/null 2>"$SCRATCH/b23-first.txt"; then
  B23_A=$(stat -f %m "$SCRATCH/b23a.gif")
  head -c 2000 "$SCRATCH/b23b.mp4" > "$SCRATCH/b23-t" && mv "$SCRATCH/b23-t" "$SCRATCH/b23b.mp4"
  # b23c: replace with the 2-frame b23a — wrong frame count, must re-render.
  cp "$SCRATCH/b23a.gif" "$SCRATCH/b23c.gif"
  sleep 1
  bin_t batch --manifest "$B23_MANIFEST" --resume >/dev/null 2>"$SCRATCH/b23-resume.txt"; B23_RC=$?
  N_B=$(probe_wh_n "$SCRATCH/b23b.mp4" | cut -d, -f3); N_C=$(probe_wh_n "$SCRATCH/b23c.gif" | cut -d, -f3)
  if [[ $B23_RC -eq 0 && "$(stat -f %m "$SCRATCH/b23a.gif")" == "$B23_A" && "$N_B" == 2 && "$N_C" == 3 ]] \
     && grep -q 'skip "keep"' "$SCRATCH/b23-resume.txt" && grep -q "skipped 1 of 3" "$SCRATCH/b23-resume.txt"; then
    bin_t batch --manifest "$B23_MANIFEST" --resume >/dev/null 2>"$SCRATCH/b23-noop.txt"; B23_RC2=$?
    if [[ $B23_RC2 -eq 0 ]] && grep -q "skipped 3 of 3" "$SCRATCH/b23-noop.txt" && grep -q "nothing to do" "$SCRATCH/b23-noop.txt"; then
      pass "B23 --resume skips the valid job, re-renders truncated + wrong-count ones; all-done = exit 0, no launch"
    else
      fail "B23 --resume all-done" "rc=$B23_RC2 $(tail -1 "$SCRATCH/b23-noop.txt")"
    fi
  else
    fail "B23 --resume" "rc=$B23_RC b=$N_B c=$N_C $(grep resume "$SCRATCH/b23-resume.txt" | tr '\n' ' ')"
  fi
else
  fail "B23 --resume" "first batch failed: $(tail -1 "$SCRATCH/b23-first.txt")"
fi

# B24 (ITER-062): a still (solid-colour, identical-frame) source finishes
# as a GIF with FEWER frames than requested (gifski merges identical
# frames) — --resume must still skip it: via the <out>.ggs.json sidecar,
# and via the GIF frames <= requested fallback when the sidecar is gone.
# A config change (sidecar hash mismatch) re-renders.
ffmpeg -v error -y -f lavfi -i color=c=0xE8D4B8:s=64x64:r=10 -frames:v 6 "$SCRATCH/b24-solid.gif"
rm -f "$SCRATCH/b24.gif" "$SCRATCH/b24.gif.ggs.json"
b24_manifest() {
  echo "{\"version\":1,\"in\":\"$B22_ROOT/$SCRATCH/b24-solid.gif\",\"frames\":6,\"jobs\":[{\"name\":\"still\",\"out\":\"$B22_ROOT/$SCRATCH/b24.gif\",\"config\":{\"dither\":{\"mode\":\"none\"},\"palette\":\"$1\"}}]}" > "$SCRATCH/b24-$1.json"
}
b24_manifest spice; b24_manifest phosphor
if bin_t batch --manifest "$SCRATCH/b24-spice.json" >/dev/null 2>"$SCRATCH/b24-first.txt"; then
  B24_N=$(probe_wh_n "$SCRATCH/b24.gif" | cut -d, -f3)
  B24_SIDE=$(python3 -c "import json;d=json.load(open('$SCRATCH/b24.gif.ggs.json'));print(d['version'],d['frames'],len(d['config_sha256']))" 2>&1)
  B24_M=$(stat -f %m "$SCRATCH/b24.gif"); sleep 1
  bin_t batch --manifest "$SCRATCH/b24-spice.json" --resume >/dev/null 2>"$SCRATCH/b24-r1.txt"; B24_R1=$?
  B24_M1=$(stat -f %m "$SCRATCH/b24.gif")
  bin_t batch --manifest "$SCRATCH/b24-phosphor.json" --resume >/dev/null 2>"$SCRATCH/b24-r2.txt"; B24_R2=$?
  rm -f "$SCRATCH/b24.gif.ggs.json"
  bin_t batch --manifest "$SCRATCH/b24-phosphor.json" --resume >/dev/null 2>"$SCRATCH/b24-r3.txt"; B24_R3=$?
  if [[ "$B24_N" -lt 6 && "$B24_SIDE" == "1 6 64" && $B24_R1 -eq 0 && "$B24_M1" == "$B24_M" \
        && $B24_R2 -eq 0 && $B24_R3 -eq 0 ]] \
     && grep -q 'skip "still".*sidecar' "$SCRATCH/b24-r1.txt" && grep -q "nothing to do" "$SCRATCH/b24-r1.txt" \
     && grep -q "skipped 0 of 1" "$SCRATCH/b24-r2.txt" \
     && grep -q "of 6 frames, no sidecar" "$SCRATCH/b24-r3.txt" && grep -q "skipped 1 of 1" "$SCRATCH/b24-r3.txt"; then
    pass "B24 --resume: still source ($B24_N/6 frames) skipped via sidecar + GIF <= fallback; config change re-renders"
  else
    fail "B24 --resume still source" "n=$B24_N side='$B24_SIDE' r=$B24_R1/$B24_R2/$B24_R3 m=$B24_M/$B24_M1 $(grep resume "$SCRATCH"/b24-r*.txt | tr '\n' ' ')"
  fi
else
  fail "B24 --resume still source" "first batch failed: $(tail -1 "$SCRATCH/b24-first.txt")"
fi

# B25 (ITER-062): `validate` rejects exactly what `batch` rejects — a
# non-array `jobs` (it used to be taken for a preset and pass), a bad
# per-job `frames`, a bad per-job `border`.  Both exit 2, nothing renders.
B25_SRC="$B22_ROOT/tests/sources/cream-paper.png"
echo '{"jobs":{}}' > "$SCRATCH/b25-obj.json"
echo "{\"in\":\"$B25_SRC\",\"jobs\":null}" > "$SCRATCH/b25-null.json"
echo "{\"in\":\"$B25_SRC\",\"jobs\":[{\"out\":\"$B22_ROOT/$SCRATCH/b25.gif\",\"frames\":0}]}" > "$SCRATCH/b25-frames.json"
echo "{\"in\":\"$B25_SRC\",\"jobs\":[{\"out\":\"$B22_ROOT/$SCRATCH/b25.gif\",\"border\":{\"width\":\"wide\"}}]}" > "$SCRATCH/b25-border.json"
B25_BAD=""
for c in obj null frames border; do
  "$BIN" validate "$SCRATCH/b25-$c.json" >/dev/null 2>&1; V=$?
  bin_t batch --manifest "$SCRATCH/b25-$c.json" >/dev/null 2>&1; Bt=$?
  [[ $V -eq 2 && $Bt -eq 2 ]] || B25_BAD="$B25_BAD $c(validate=$V batch=$Bt)"
done
if [[ -z "$B25_BAD" && ! -f "$SCRATCH/b25.gif" ]]; then
  pass "B25 validate == batch on jobs:{} / jobs:null / job frames:0 / job border.width:\"wide\" (all exit 2)"
else
  fail "B25 validate/batch parity" "$B25_BAD"
fi

# B26 (ITER-068): closed nested blocks — a typo'd key inside dither /
# a postprocess stage fails fast (it used to validate and render defaults).
echo '{"dither":{"moed":"stbn"}}' > "$SCRATCH/b26-dither.json"
echo "{\"in\":\"$B25_SRC\",\"jobs\":[{\"out\":\"$B22_ROOT/$SCRATCH/b26.gif\",\"config\":{\"postprocess\":{\"vignette\":{\"enabled\":true,\"strenght\":0.4}}}}]}" > "$SCRATCH/b26-stage.json"
B26_V=$("$BIN" validate "$SCRATCH/b26-dither.json" 2>&1); B26_VRC=$?
B26_R=$(bin_t render --in "$TEST_IMG" --out "$SCRATCH/b26r.gif" --frames 2 --preset "$SCRATCH/b26-dither.json" 2>&1); B26_RRC=$?
B26_B=$(bin_t batch --manifest "$SCRATCH/b26-stage.json" 2>&1); B26_BRC=$?
if [[ $B26_VRC -eq 2 && $B26_RRC -eq 2 && $B26_BRC -eq 2 && ! -f "$SCRATCH/b26r.gif" && ! -f "$SCRATCH/b26.gif" ]] \
   && echo "$B26_V" | grep -q "/dither: .*moed" && echo "$B26_B" | grep -q "/postprocess/vignette: .*strenght"; then
  pass "B26 typo inside dither / a postprocess stage exits 2 (validate, render --preset, batch)"
else
  fail "B26 closed nested blocks" "v=$B26_VRC r=$B26_RRC b=$B26_BRC $(echo "$B26_V" | head -1)"
fi

# B8-B11 (audit 2026-06-10): the batch CLI, multi-source manifests,
# PERF_JOB emission, MP4 export, and the twitter-fit shrink ladder all
# shipped with ZERO automated coverage. One multi-job batch session
# covers the first four; a second tiny batch exercises the ladder.
REPO_ABS="$(pwd -P)"
B8_MANIFEST="$SCRATCH/b8-manifest.json"
cat > "$B8_MANIFEST" <<EOF
{
  "in": "$REPO_ABS/tests/sources/cream-paper.png",
  "frames": 4,
  "perf": true,
  "jobs": [
    {"name": "b8a-gif", "in": "$REPO_ABS/tests/sources/cream-paper.png",
     "out": "$REPO_ABS/$SCRATCH/b8a.gif", "format": "gif", "config": {}},
    {"name": "b8b-mp4", "in": "$REPO_ABS/tests/sources/synthetic-noise.png",
     "out": "$REPO_ABS/$SCRATCH/b8b.mp4", "format": "mp4", "config": {}},
    {"name": "b8c-mp4-archive", "in": "$REPO_ABS/tests/sources/synthetic-noise.png",
     "out": "$REPO_ABS/$SCRATCH/b8c.mp4", "format": "mp4", "mp4Quality": "archive", "config": {}}
  ]
}
EOF
B8_STDERR="$SCRATCH/b8-stderr.txt"
# v0.2 Phase 0 (ITER-045): the batch runs under tests/perf-sample.sh so the
# WebKit XPC CPU (WebContent/GPU) is captured alongside PERF_JOB — `time`
# never saw those processes. Same timeout as bin_t, applied by the sampler.
B8_PERF="$SCRATCH/b8-perf.ndjson"
if PERF_TIMEOUT="${TEST_TIMEOUT:-180}" bash tests/perf-sample.sh "$B8_PERF" -- \
     "$BIN" batch --manifest "$B8_MANIFEST" 2>"$B8_STDERR" >"$SCRATCH/b8-stdout.txt" \
   && [[ -f "$SCRATCH/b8a.gif" && -f "$SCRATCH/b8b.mp4" && -f "$SCRATCH/b8c.mp4" ]]; then
  pass "B8 batch CLI: 3-job multi-source manifest renders every output"
else
  fail "B8 batch CLI multi-source manifest" "exit=$? a=$([[ -f $SCRATCH/b8a.gif ]] && echo y || echo n) b=$([[ -f $SCRATCH/b8b.mp4 ]] && echo y || echo n) $(tail -2 "$B8_STDERR" | tr '\n' ' ')"
fi

# B9: MP4 output is a real ISO-BMFF file (ftyp box at offset 4)
if [[ -f "$SCRATCH/b8b.mp4" ]] && head -c 12 "$SCRATCH/b8b.mp4" | tail -c 8 | grep -q "ftyp"; then
  pass "B9 MP4 export produces ISO-BMFF (ftyp) container"
else
  fail "B9 MP4 export ftyp magic" "header: $(head -c 12 "$SCRATCH/b8b.mp4" 2>/dev/null | xxd -p 2>/dev/null | head -1)"
fi

# B28 (ITER-050): MP4 rate control. Default (upload) uses bitrate RC and must
# come out smaller than the fixed-QP archive job on the same high-entropy
# source; both must carry a 1/fps track timescale (mp4 0.14 used to leave the
# mdhd at 1000 → 1 ms frames); a typo'd mp4Quality is refused up front.
B28_TB_B=$(ffprobe -v error -select_streams v:0 -show_entries stream=time_base,nb_frames -of csv=p=0 "$SCRATCH/b8b.mp4" 2>/dev/null)
B28_TB_C=$(ffprobe -v error -select_streams v:0 -show_entries stream=time_base,nb_frames -of csv=p=0 "$SCRATCH/b8c.mp4" 2>/dev/null)
B28_SZ_B=$(stat -f%z "$SCRATCH/b8b.mp4" 2>/dev/null || echo 0)
B28_SZ_C=$(stat -f%z "$SCRATCH/b8c.mp4" 2>/dev/null || echo 0)
B28_BAD="$SCRATCH/b28-bad-manifest.json"
cat > "$B28_BAD" <<EOF
{"in": "$REPO_ABS/tests/sources/cream-paper.png", "frames": 2,
 "jobs": [{"name": "b28-typo", "out": "$REPO_ABS/$SCRATCH/b28.mp4", "mp4Quality": "lossless", "config": {}}]}
EOF
bin_t batch --manifest "$B28_BAD" >/dev/null 2>"$SCRATCH/b28-stderr.txt"; B28_BAD_EXIT=$?
if [[ "$B28_TB_B" =~ ^1/[0-9]+,4$ && "$B28_TB_B" != "1/1000,4" && "$B28_TB_C" == "$B28_TB_B" ]] \
   && [[ "$B28_SZ_B" -gt 0 && "$B28_SZ_B" -lt "$B28_SZ_C" ]] \
   && [[ "$B28_BAD_EXIT" -eq 2 ]] && grep -q "unknown mp4 quality" "$SCRATCH/b28-stderr.txt"; then
  pass "B28 MP4 upload RC < archive ($((B28_SZ_B/1024)) KB vs $((B28_SZ_C/1024)) KB), timescale $B28_TB_B, typo refused"
else
  fail "B28 MP4 quality modes" "upload='$B28_TB_B' ${B28_SZ_B}B archive='$B28_TB_C' ${B28_SZ_C}B typo-exit=$B28_BAD_EXIT"
fi

# B10: PERF_JOB NDJSON emitted when manifest sets perf:true, AND the sampler
# wrote a summary with the XPC columns (ITER-045). A headless perf number
# without webcontent/gpu CPU is a gate failure from here on.
B10_XPC=$(python3 - "$B8_PERF" <<'PY' 2>/dev/null
import json, sys
s = None
for line in open(sys.argv[1]):
    try: d = json.loads(line)
    except ValueError: continue
    if d.get('type') == 'summary': s = d
ok = s and s['wall_s'] > 0 and all(isinstance(s[k], (int, float)) for k in ('app_cpu_s', 'webcontent_cpu_s', 'gpu_cpu_s')) and s['pids']['webcontent'] and s['pids']['gpu']
print(f"wall {s['wall_s']}s app {s['app_cpu_s']}s webcontent {s['webcontent_cpu_s']}s gpu {s['gpu_cpu_s']}s" if ok else "")
PY
)
if grep -q "PERF_JOB {" "$B8_STDERR" && [[ -n "$B10_XPC" ]]; then
  pass "B10 PERF_JOB NDJSON + XPC sampler summary ($B10_XPC)"
else
  fail "B10 PERF_JOB NDJSON + XPC sampler summary" "PERF_JOB=$(grep -c 'PERF_JOB {' "$B8_STDERR") xpc='$B10_XPC'"
fi

# B11: an impossible twitter-fit target must fail closed instead of returning
# an oversized file while claiming success. B12 then proves the same ladder
# succeeds for a feasible hard target.
B11_MANIFEST="$SCRATCH/b11-manifest.json"
cat > "$B11_MANIFEST" <<EOF
{
  "in": "$REPO_ABS/tests/sources/cream-paper.png",
  "frames": 4,
  "jobs": [
    {"name": "b11-fit", "out": "$REPO_ABS/$SCRATCH/b11.gif", "format": "gif",
     "capWidth": 720, "targetMaxBytes": 200000, "config": {}}
  ]
}
EOF
B11_STDERR="$SCRATCH/b11-stderr.txt"
if bin_t batch --manifest "$B11_MANIFEST" 2>"$B11_STDERR" >/dev/null; then
  B11_EXIT=0
else
  B11_EXIT=$?
fi
if [[ "$B11_EXIT" -ne 0 && ! -f "$SCRATCH/b11.gif" ]] \
   && grep -q "shrink ladder" "$B11_STDERR" \
   && grep -q "cannot meet target size" "$B11_STDERR"; then
  pass "B11 impossible twitter-fit target fails closed"
else
  fail "B11 impossible twitter-fit target fails closed" "exit=$B11_EXIT output=$([[ -f $SCRATCH/b11.gif ]] && echo yes || echo no)"
fi

B12_MANIFEST="$SCRATCH/b12-manifest.json"
cat > "$B12_MANIFEST" <<EOF
{
  "in": "$REPO_ABS/tests/sources/cream-paper.png",
  "frames": 4,
  "jobs": [
    {"name": "b12-fit", "out": "$REPO_ABS/$SCRATCH/b12.gif", "format": "gif",
     "capWidth": 720, "targetMaxBytes": 700000, "config": {}}
  ]
}
EOF
B12_STDERR="$SCRATCH/b12-stderr.txt"
if bin_t batch --manifest "$B12_MANIFEST" 2>"$B12_STDERR" >/dev/null \
   && [[ -f "$SCRATCH/b12.gif" ]]; then
  B12_SIZE=$(stat -f%z "$SCRATCH/b12.gif")
  B12_W=$(python3 -c "import struct;d=open('$SCRATCH/b12.gif','rb').read(10);print(struct.unpack('<H',d[6:8])[0])")
  if [[ "$B12_SIZE" -le 700000 && "$B12_W" -le 480 ]] \
     && grep -q "shrink ladder" "$B12_STDERR"; then
    pass "B12 feasible twitter-fit target succeeds (${B12_W}px, $((B12_SIZE/1024)) KB)"
  else
    fail "B12 feasible twitter-fit target succeeds" "width=$B12_W size=$B12_SIZE"
  fi
else
  fail "B12 feasible twitter-fit target succeeds" "command failed or no output"
fi

# B27 (x2, ITER-064, F18 step 2; BUGS backlog #1): over-limit sources are
# rejected from the Rust header probe BEFORE p5 decodes a single frame —
# the message proves which guard fired (the post-decode guard says
# "Image rejected", the probe says "rejected before decode").
for fx in "limit-481-frames.gif:481 frames" "limit-16385-wide.png:exceed 16384 px"; do
  FX_FILE="${fx%%:*}"; FX_WANT="${fx#*:}"
  FX_ERR=$(bin_t render --in "$PWD/tests/sources/$FX_FILE" --out "$SCRATCH/b27-$FX_FILE.gif" --frames 2 2>&1 >/dev/null)
  FX_CODE=$?
  if [[ $FX_CODE -ne 0 ]] && grep -q "rejected before decode: .*$FX_WANT" <<<"$FX_ERR" && [[ ! -f "$SCRATCH/b27-$FX_FILE.gif" ]]; then
    pass "B27 $FX_FILE rejected pre-decode ($FX_WANT)"
  else
    fail "B27 $FX_FILE rejected pre-decode" "exit=$FX_CODE stderr: $(grep 'glyph-grid-studio\[js\]' <<<"$FX_ERR" | tail -2)"
  fi
done


# ---------- Phase C: MCP ----------
section "Phase C — MCP protocol"

if BIN="$BIN" SCRATCH="$SCRATCH" TEST_IMG="$TEST_IMG" python3 tests/mcp.py 2>&1; then
  pass "C1-C7 MCP protocol all checks (see python output above)"
else
  fail "C1-C7 MCP protocol" "see python output above"
fi

# ---------- Phase D: Cross-mode parity ----------
section "Phase D — CLI vs MCP parity"

# Already wrote B5 (CLI) and C5 (MCP, in mcp.py output as scratch/c5.gif).
if [[ -f "$SCRATCH/b5_full.gif" && -f "$SCRATCH/c5_mcp_render.gif" ]]; then
  CLI_SIZE=$(stat -f%z "$SCRATCH/b5_full.gif")
  MCP_SIZE=$(stat -f%z "$SCRATCH/c5_mcp_render.gif")
  RATIO=$(python3 -c "print(min($CLI_SIZE,$MCP_SIZE)/max($CLI_SIZE,$MCP_SIZE))")
  if python3 -c "exit(0 if $RATIO > 0.85 else 1)"; then
    pass "D1 CLI/MCP size parity (ratio $RATIO, CLI=$((CLI_SIZE/1024))KB MCP=$((MCP_SIZE/1024))KB)"
  else
    fail "D1 CLI/MCP size parity" "ratio $RATIO too low"
  fi
  # D2 dimensions
  CLI_DIMS=$(file "$SCRATCH/b5_full.gif" | grep -oE '[0-9]+ x [0-9]+')
  MCP_DIMS=$(file "$SCRATCH/c5_mcp_render.gif" | grep -oE '[0-9]+ x [0-9]+')
  [[ "$CLI_DIMS" == "$MCP_DIMS" ]] && pass "D2 dimensions match ($CLI_DIMS)" || fail "D2 dimensions match" "$CLI_DIMS vs $MCP_DIMS"
else
  fail "D1/D2 cross-mode parity" "missing one of: scratch/b5_full.gif scratch/c5_mcp_render.gif"
fi

# ---------- Phase F: Regression ----------
section "Phase F — Regression"

# F3: no GIF in scratch over 15MB
OVERSIZED=$(find "$SCRATCH" -name "*.gif" -size +15M 2>/dev/null | wc -l | tr -d ' ')
if [[ "$OVERSIZED" == "0" ]]; then
  pass "F3 no test GIF over 15 MB"
else
  fail "F3 no test GIF over 15 MB" "$OVERSIZED file(s) over"
fi

# F4: all GIFs validate
ALL_VALID=true
for g in "$SCRATCH"/*.gif; do
  if ! file "$g" 2>/dev/null | grep -q "GIF image data, version 89a"; then
    ALL_VALID=false
    fail "F4 GIF $g valid" "not GIF89a"
  fi
done
$ALL_VALID && pass "F4 all test GIFs validate as GIF89a"

# ---------- Phase G: Determinism (v0.2 Phase 1, ITER-046) ----------
section "Phase G — Byte-identity"

# G1-G7: same job twice headless, once --show-window, 2-job batch of twins,
# still + animated source, GIF + MP4 — decoded RGBA must match exactly.
if BIN="$BIN" TEST_TIMEOUT="${TEST_TIMEOUT:-180}" bash tests/byte-identity.sh "$SCRATCH/byte-identity" 2>&1; then
  pass "G1-G7 byte-identity: headless output is a pure function of (source, config, seed)"
else
  fail "G1-G7 byte-identity" "see byte-identity output above"
fi

# G10 (ITER-055): `batch --jobs 4` fans the manifest out over child processes;
# every job's output file must be md5-identical to the serial batch.
if BIN="$BIN" TEST_TIMEOUT="${TEST_TIMEOUT:-180}" bash tests/batch-jobs-identity.sh "$SCRATCH/batch-jobs" 2>&1; then
  pass "G10 batch --jobs 4 == --jobs 1, md5 per job"
else
  fail "G10 batch --jobs identity" "see batch-jobs output above"
fi

# G11 (ITER-072): a quiet `--jobs 2` parent must not trip its own stall guard.
BIN="$BIN" TEST_TIMEOUT="${TEST_TIMEOUT:-180}" bash tests/batch-jobs-stall.sh "$SCRATCH/batch-jobs-stall" 2>&1 && pass "G11 quiet batch --jobs 2 outlives a 3 s stall window" || fail "G11 batch --jobs stall guard" "see batch-jobs-stall output above"

# H1 (ITER-074): terminal-native export — txt/ansi/cast/html from CLI + batch.
section "Phase H — Terminal export"; BIN="$BIN" TEST_TIMEOUT="${TEST_TIMEOUT:-180}" bash tests/text-export.sh "$SCRATCH/text-export" 2>&1 && pass "H1 terminal export: txt/ansi/cast/html via CLI + batch (T1-T8)" || fail "H1 terminal export" "see text-export output above"
section "Phase I — Timeline"; BIN="$BIN" TEST_TIMEOUT="${TEST_TIMEOUT:-180}" bash tests/timeline.sh "$SCRATCH/timeline" 2>&1 && pass "I1 timeline: palette morph, structural steps, determinism, JSON/CLI parity (L1-L5)" || fail "I1 timeline" "see timeline output above"
section "Phase J — Typography"; BIN="$BIN" TEST_TIMEOUT="${TEST_TIMEOUT:-180}" bash tests/text-source.sh "$SCRATCH/text-source" 2>&1 && pass "J1 text source: --text/--srt/composite/preset parity/errors/stroke warn (X1-X7)" || fail "J1 text source" "see text-source output above"
section "Phase K — Video"; BIN="$BIN" TEST_TIMEOUT="${TEST_TIMEOUT:-180}" bash tests/video-source.sh "$SCRATCH/video-source" 2>&1 && pass "K1 video source: Rust H.264 / WebView fallback / determinism / limits / batch (V1-V8)" || fail "K1 video source" "see video-source output above"

# ---------- Summary ----------
section "Summary"
echo "  PASS: $PASS"
echo "  FAIL: $FAIL"

# ---------- Write RESULTS-V0.1.md ----------
{
  echo "# Test Results — v0.1.0 (run $(date '+%Y-%m-%d %H:%M:%S'))"
  echo ""
  echo "**$PASS passed · $FAIL failed**"
  echo ""
  if [[ "$FAIL" == "0" ]]; then
    echo "## ✅ READY FOR v0.1"
  else
    echo "## ❌ NOT READY — failures below"
    echo ""
    for f in "${FAIL_NAMES[@]}"; do
      echo "- $f"
    done
  fi
  echo ""
  echo "## Per-phase breakdown"
  echo ""
  echo "Run \`tests/run-all.sh\` for the live output. Phase E (manual GUI) requires a separate computer-use session."
  echo ""
  echo "## Outputs"
  ls -lh "$SCRATCH" | tail -n +2
} > "$RESULTS"

echo
echo "Wrote $RESULTS"
[[ "$FAIL" == "0" ]] && exit 0 || exit 1
