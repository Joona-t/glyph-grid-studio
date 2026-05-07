# Test Plan — v0.1.0 implementation verification

Validates everything we've shipped to date: GUI, CLI (T1), MCP server (T3), pre-public cleanup, and the slider-scrub freeze bug fix. Designed to be re-runnable on every commit and during pre-release verification.

## Why this exists

Three implementations stacked on top of each other (GUI → CLI → MCP) plus a code-cleanup pass plus a bug fix. Without a comprehensive test, we can't confidently say "this works" — we just have a series of "this worked once, on my machine, last week" anecdotes. The test suite turns those into reproducible evidence.

## Coverage matrix

| Phase | What | How | Time |
|---|---|---|---|
| **A** | Static source-grep | Bash one-liners, fast | 10 s |
| **B** | CLI surface (help / catalog / render / error paths) | `tests/cli.sh` | ~3 min |
| **C** | MCP protocol (handshake / tools/list / tool calls) | `tests/mcp.py` | ~2 min |
| **D** | Cross-mode parity (CLI vs MCP same output) | `tests/parity.sh` | ~1 min |
| **E** | GUI manual checks (scrub freeze, drag-drop) | computer-use | ~3 min |
| **F** | Visual / size regression | bash + python | ~30 s |

Total: ~10 minutes of automated tests + ~3 minutes of manual GUI verification.

## Outputs

- `tests/RESULTS-V0.1.md` — written at the end with PASS/FAIL per test
- `tests/scratch/` — temporary GIFs from test runs, cleaned between sessions
- Any bugs found get logged to `BUGS_AND_ITERATIONS.md` and fixed before declaring v0.1 ready

---

## Phase A — Static source-grep (10 s)

Goal: prove the cleanup pass actually removed all the personal-context strings + hardcoded paths.

| ID | Test | Pass criterion |
|---|---|---|
| A1 | `grep -irn "/Users/darkfire/" src/ src-tauri/src/` returns nothing | No hits |
| A2 | `grep -irn "eron\|claire\|claymore" src/ src-tauri/src/` returns nothing | No hits |
| A3 | `grep -irn "TODO\|FIXME\|XXX" src/ src-tauri/src/` is empty *or* only contains acknowledged items | No surprise TODOs |
| A4 | `Cargo.toml` has non-empty `description`, `repository`, `authors` | All three present |
| A5 | Files exist: `src-tauri/src/mcp.rs`, `docs/mcp.md`, `Casks/glyph-grid-studio.rb`, `BUGS_AND_ITERATIONS.md`, `PRE-PUBLIC-CHECKLIST.md`, `PUBLIC-LAUNCH-PLAN.md`, `AGENT-INTEGRATION-PLAN.md` | All exist |
| A6 | `src/assets/eron.png` does NOT exist | Missing |

## Phase B — CLI smoke tests (~3 min)

Goal: every CLI surface works; happy paths produce valid GIFs; error paths exit non-zero.

| ID | Test | Pass criterion |
|---|---|---|
| B1 | `glyph-grid-studio --help` lists studio/render/catalog/mcp subcommands | All four present |
| B2 | `glyph-grid-studio render --help` lists all 14 flags | --in, --out, --frames, --palette, --color-mode, --ramp, --dither, --selection-mode, --glyph-set, --sampling-strategy, --postprocess, --cols, --rows, --preset, --show-window all present |
| B3 | `glyph-grid-studio catalog \| jq` returns valid JSON with 8 expected keys | palettes/color_modes/ramps/dithers/selection_modes/glyph_sets/sampling_strategies/postprocess_stages all present, each non-empty array |
| B4 | Minimal render: `--in Thor.png --out tmp/min.gif` (defaults for everything else) | Exits 0, GIF89a 1024×504, 24 frames, 1–15 MB |
| B5 | Full-flag render: every CLI flag set | Exits 0, GIF89a, palette/dither visible in output |
| B6 | Multiple postprocess: `--postprocess crtBeam --postprocess vignette` | Exits 0, both stages visible |
| B7 | Error: `--in /nonexistent` | Exits non-zero, no output GIF |
| B8 | Error: `--in dir/  --out /readonly/path/foo.gif` (cannot write) | Exits non-zero |
| B9 | `--show-window` opens visible window during render | Window visible (manual check) |

## Phase C — MCP protocol tests (~2 min)

Goal: server speaks JSON-RPC 2.0 correctly; tools work end-to-end.

| ID | Test | Pass criterion |
|---|---|---|
| C1 | `initialize` request returns server capabilities + protocolVersion 2024-11-05 | Returns valid result |
| C2 | After init, `tools/list` returns exactly 2 tools | glyph_grid_render + glyph_grid_catalog |
| C3 | Each tool's inputSchema is valid JSON Schema with proper types/required fields | Schema parses; required = [in_path, out_path] for render; [] for catalog |
| C4 | `tools/call glyph_grid_catalog` returns the same JSON as `glyph-grid-studio catalog` | Byte-equal (modulo whitespace) |
| C5 | `tools/call glyph_grid_render` with minimal args writes a valid GIF | File written, 1–15 MB, GIF89a |
| C6 | `tools/call glyph_grid_render` with bad in_path returns error message | result.content text contains "failed" or "error" |
| C7 | Server handles two sequential renders without restart | Both succeed |

## Phase D — Cross-mode parity (~1 min)

Goal: the same render config invoked via CLI vs MCP produces equivalent output.

| ID | Test | Pass criterion |
|---|---|---|
| D1 | Render Thor with same config via CLI + via MCP | Both succeed; output sizes within 10% of each other |
| D2 | Both outputs are GIF89a 1024×504 | dimensions identical |
| D3 | Both have same frame count | identical |

(Note: NeuQuant palette quantization has small nondeterminism; byte-equality not expected.)

## Phase E — Manual GUI verification (~3 min)

Goal: the parts of the app that need an actual person at the keyboard.

| ID | Test | Pass criterion |
|---|---|---|
| E1 | App launches with empty-state placeholder | "Drop an image to start" text visible |
| E2 | Pick image loads cleanly | Canvas renders the chosen image |
| E3 | **Scrub freeze fix**: drag cols/rows slider rapidly | Breathing animation does NOT visibly reset/freeze; cells continue to pulse during scrub |
| E4 | **Drag-drop**: drag image from Finder onto window | Image loads, scene re-renders |
| E5 | Snapshot PNG button writes valid file | PNG appears in chosen location |
| E6 | Export GIF button writes valid file | GIF appears in chosen location |
| E7 | Preset save → reload → load roundtrip | Settings restored exactly |

Tests E3–E4 are the highest-priority because they validate fixes the user explicitly reported as broken.

## Phase F — Regression (~30 s)

Goal: catch silent quality drift between releases.

| ID | Test | Pass criterion |
|---|---|---|
| F1 | Render a known-config GIF; size within ±15% of v0.1.0-rc baseline | within tolerance |
| F2 | Render takes <60 s wall-clock | true |
| F3 | No GIF in any test phase exceeds 15 MB (X.com cap) | true |
| F4 | All test GIFs validate as GIF89a | true |

---

## Bug-fix protocol

Same as the 469-GIF test session:

1. Capture symptom + console output
2. Diagnose: read source, identify root cause
3. Fix in source
4. Rebuild: `cargo tauri build`
5. Reinstall: `ditto …`
6. Re-run the failing test
7. Log the bug + fix to `BUGS_AND_ITERATIONS.md`

## Verdict format

`tests/RESULTS-V0.1.md` ends with one of:

- ✅ **READY FOR v0.1** — all tests pass, no critical bugs found
- ⚠ **NEEDS PATCH** — non-critical bugs found and logged; safe to ship as v0.1.0 with bug list documented in release notes
- ❌ **NOT READY** — critical bugs found; v0.1 shipping is blocked
