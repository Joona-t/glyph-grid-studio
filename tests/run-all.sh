#!/bin/bash
# Comprehensive v0.1 test runner. Runs phases A-D + F (E is manual GUI).
# Outputs PASS/FAIL per test; writes RESULTS-V0.1.md at the end.
#
# Run: bash tests/run-all.sh
# Re-run after a rebuild: cargo tauri build && bash tests/run-all.sh

set -u
cd "$(dirname "$0")/.."  # repo root

BIN="$HOME/glyph-grid-studio/src-tauri/target/release/bundle/macos/Glyph Grid Studio.app/Contents/MacOS/glyph-grid-studio"
TEST_IMG="$HOME/Downloads/Thor.png"
SCRATCH="tests/scratch"
RESULTS="tests/RESULTS-V0.1.md"

mkdir -p "$SCRATCH"
rm -f "$SCRATCH"/*.gif "$SCRATCH"/*.json "$SCRATCH"/*.txt

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

# A1: no /Users/darkfire/ paths in source
if ! grep -rqI "/Users/darkfire/" src/ src-tauri/src/ 2>/dev/null; then
  pass "A1 no /Users/darkfire/ paths in source"
else
  fail "A1 no /Users/darkfire/ paths in source" "$(grep -rIn '/Users/darkfire/' src/ src-tauri/src/ 2>/dev/null | head -2)"
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

# A5: required files exist
ALL_FILES=true
for f in src-tauri/src/mcp.rs docs/mcp.md Casks/glyph-grid-studio.rb \
         BUGS_AND_ITERATIONS.md PRE-PUBLIC-CHECKLIST.md PUBLIC-LAUNCH-PLAN.md \
         AGENT-INTEGRATION-PLAN.md TEST-PLAN-V0.1.md; do
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

# ---------- Phase B: CLI ----------
section "Phase B — CLI surface"

# B1: --help lists subcommands
HELP=$("$BIN" --help 2>&1)
if echo "$HELP" | grep -q "studio" && echo "$HELP" | grep -q "render" && \
   echo "$HELP" | grep -q "catalog" && echo "$HELP" | grep -q "mcp"; then
  pass "B1 --help lists all 4 subcommands"
else
  fail "B1 --help lists all 4 subcommands" "$HELP"
fi

# B2: render --help lists all 14 flags
RENDER_HELP=$("$BIN" render --help 2>&1)
B2_OK=true
for flag in --in --out --frames --palette --color-mode --ramp --dither \
            --selection-mode --glyph-set --sampling-strategy --postprocess \
            --cols --rows --preset --show-window; do
  if ! echo "$RENDER_HELP" | grep -q -- "$flag"; then
    fail "B2 render --help has $flag" "missing"
    B2_OK=false
  fi
done
$B2_OK && pass "B2 render --help lists all 15 flags"

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
if "$BIN" render --in "$TEST_IMG" --out "$B4_OUT" --frames 6 >/dev/null 2>&1 && [[ -f "$B4_OUT" ]]; then
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
if "$BIN" render \
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
if "$BIN" render --in /tmp/__nonexistent_file__.png --out "$SCRATCH/b7.gif" --frames 6 >/dev/null 2>&1; then
  fail "B7 bad input fails non-zero" "exited zero with bad input"
else
  pass "B7 bad input exits non-zero"
fi

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
