# GUI Test Plan — comprehensive feature verification

Manual GUI tests via computer-use, run after each significant feature batch. Designed to catch regressions in the visual studio that automated tests can't see (focus issues, slider scrub freeze, drag-drop, dialog behavior).

## When to run

- After every Tauri rebuild that touches `src/index.html` or `src/lib/glyph-studio.js`
- After every new postprocess feature
- Before flipping repo to public
- After Apple Developer signing setup (to confirm signing didn't break anything)

## Test groups

### G1 — Empty state (5 tests)

| ID | Action | Pass criterion |
|---|---|---|
| G1.1 | Launch app fresh | Window opens within 3s; "glyph studio" title shown in panel |
| G1.2 | Without loading anything, observe canvas | "Drop an image to start" + "…or click Pick image…" + format hints visible |
| G1.3 | Observe status bar | Reads "no source — drag an image…" |
| G1.4 | All panel folders are present and collapsable | Image, Grid, Mapping, Dither, Color, Selection, Breathing, Postprocess, Animation, Presets, Export |
| G1.5 | Status bar empty-state stays during slider tweaks | No NaN, no crash |

### G2 — Image loading (4 tests)

| ID | Action | Pass criterion |
|---|---|---|
| G2.1 | Click "Pick image…" | Native file picker opens |
| G2.2 | Cmd+Shift+G → type `~/Downloads/sparky.png` → Return → Return | Image loads, scene renders within 3s |
| G2.3 | While image is loaded, drag-drop a different image (Finder → window) | Image swaps, scene re-renders |
| G2.4 | After image load, status bar shows scene/palette/grid metrics | Format: `studio | <palette> | <strategy>/<colorMode> | <cols>×<rows> | sample <ms>ms | t=<s>s | compat=v2` |

### G3 — All slider categories (10 tests)

For each, drag the slider through its full range. Pass criterion: canvas updates smoothly, no freeze, no NaN, no crash.

| ID | Slider | Range to scrub |
|---|---|---|
| G3.1 | Grid → cols | 60 → 400 (rapid) |
| G3.2 | Grid → rows | 40 → 300 (rapid) |
| G3.3 | Grid → size | 3 → 14 |
| G3.4 | Mapping → brightnessGamma | 0.2 → 2.5 |
| G3.5 | Breathing → emaAlpha | 0 → 1 |
| G3.6 | Breathing → gainSwing | 0 → 0.6 |
| G3.7 | Breathing → jitter | 0 → 0.2 |
| G3.8 | Breathing → pulseHz | 0.1 → 3 |
| G3.9 | Postprocess → kawaii → intensity | 0 → 1.5 (after enabling kawaii) |
| G3.10 | Postprocess → kawaii → heartCount | 0 → 60 |

### G4 — Dropdowns (8 tests)

For each, open the dropdown, click each option in sequence. Pass criterion: canvas updates within 1s of selection.

| ID | Dropdown | Cycle |
|---|---|---|
| G4.1 | Color → palette | All 12 (incl. kawaii-pink) |
| G4.2 | Color → colorMode | preserve / monochrome / duotone / gradient |
| G4.3 | Mapping → ramp | All 9 |
| G4.4 | Mapping → samplingStrategy | average / nearest / edge-weighted |
| G4.5 | Dither → mode | All 9 |
| G4.6 | Selection → selectionMode | All 4 |
| G4.7 | Selection → glyphSet | All 7 (null + 6 named) |
| G4.8 | Postprocess → each toggle | On/off cycle through all 9 stages |

### G5 — Export buttons (5 tests)

| ID | Action | Pass criterion |
|---|---|---|
| G5.1 | Snapshot PNG → save dialog → save to ~/Downloads/test.png | File written; valid PNG; canvas dimensions |
| G5.2 | Export GIF → save dialog → save | File written; valid GIF89a; 24 frames default |
| G5.3 | Export JSON → save dialog → save | File written; valid JSON with full CONFIG |
| G5.4 | Import JSON → pick the JSON from G5.3 | Settings restored; canvas updates |
| G5.5 | Copy share URL | Clipboard contains `?p=...` URL |

### G6 — Preset save/load (3 tests)

| ID | Action | Pass criterion |
|---|---|---|
| G6.1 | Save current with name "test-1" → tweak palette → Load test-1 | Settings revert to saved state |
| G6.2 | Type new name → Save current | New entry appears in Load dropdown |
| G6.3 | Load multiple presets in sequence | Each loads cleanly; no stale state leak |

### G7 — Kawaii feature (5 tests)

These verify the new kawaii postprocess works end-to-end.

| ID | Action | Pass criterion |
|---|---|---|
| G7.1 | Postprocess → kawaii → enable | Hearts + sparkles + twinkles visible on canvas |
| G7.2 | Tweak intensity slider 0 → 1 | Particles fade in/out smoothly |
| G7.3 | Tweak heartCount 0 → 60 | Heart density increases linearly |
| G7.4 | Tweak hue R/G/B sliders | Particle color shifts (e.g., RGB(0,0,255) → blue particles) |
| G7.5 | Switch palette to kawaii-pink | Background and inks shift to pink gradient; kawaii overlay still visible on top |

### G8 — Stress / regression (3 tests)

| ID | Action | Pass criterion |
|---|---|---|
| G8.1 | Enable ALL postprocess stages simultaneously (kitchen sink) | Canvas renders without crash; some visual chaos expected |
| G8.2 | Cols=400 + rows=300 + size=3 (max grid density) | Renders without crash; sample-time may be high (~500ms+) |
| G8.3 | Switch palette and dither during slider scrub | No NaN, no crash |

## Tally

8 groups × ~5 tests = ~43 manual tests total. Realistic time to run all: 30 minutes via computer-use, or 10 minutes if a human does it directly.

## Pass/fail recording

Each run writes results to `tests/GUI-RESULTS-<date>.md` with the same pass/fail format as `RESULTS-V0.1.md`.

## Failure protocol

Same as automated test suite:
1. Capture screenshot + console output (Cmd+Opt+I → Console)
2. Diagnose root cause from source
3. Fix in source
4. `cargo tauri build` → `ditto` → relaunch
5. Re-run failing test
6. Log in `BUGS_AND_ITERATIONS.md`
