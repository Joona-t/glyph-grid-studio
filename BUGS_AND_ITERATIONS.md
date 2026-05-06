# Bugs and Iterations — Glyph Grid Studio

Running log of every defect found, every iteration that landed, and the why behind each. Newest at top.

---

## 2026-05-06 — Comprehensive feature test (469-GIF Cartesian)

Drove the full feature surface against a single anime portrait (Claymore — Clare). Generated 469 GIFs across 9 phases:

| Phase | What | GIFs |
|---|---|---|
| 1 | Palette × ColorMode × Dither (Cartesian, 11×4×9) | 396 |
| 2 | SelectionMode × GlyphSet (Cartesian, 4×7) | 28 |
| 3 | Ramp coverage | 9 |
| 4 | Postprocess solo + combos + kitchen sink | 13 |
| 5 | SamplingStrategy | 3 |
| 6 | Breathing extremes | 8 |
| 7 | Grid density | 3 |
| 8 | Animation duration × fps | 4 |
| 9 | Showcase combos | 5 |
| **Total** | | **469** |

All outputs validate as GIF89a 1024×504. Total disk ~2.4 GB. Three Phase-9 showcase GIFs slightly exceed X.com's 15 MB cap (18 MB, 20 MB, 15.4 MB) due to 48-frame loops; trim to 24 frames for upload.

Driver innovation that landed in this pass:
- New Tauri command `save_gif_to_path` — writes GIF89a directly to absolute path with no save dialog
- `runStudioPhase(N)` / `runAllStudioPhases()` JS drivers baked into the build
- `devtools` Cargo feature added so Cmd+Opt+I works in release builds

---

## BUG-001 — Batch GIF export stalls after first job

**Found:** 2026-05-06, Phase 1 of comprehensive test
**Symptom:** `runStudioPhase(1)` processed 1 of 396 jobs, console showed `batch: 1/396` then went silent indefinitely. Disk count stuck at 1.

**Root cause:** After `finishRecording` fires `onFinish` for the batch's first job, the next `draw()` iteration calls `noLoop()` because `recState.done === true`. Subsequent `beginRecord` calls in the batch driver create new `recState` with `done: false`, but never re-engage `loop()`, so p5's draw cycle stays paused. The `handleRecordFrame` is never called for jobs 2..N, so no frames get captured.

**Fix:** Added `try { loop(); } catch (e) {} ` at the end of `beginRecord` in `src/index.html`'s `window.__glyphGridTest`. Always re-engages the draw loop when a new recording starts. The batch driver also calls `loop()` defensively before each `setTimeout`.

**Verification:** Re-ran `runAllStudioPhases()` after fix, watched 469/469 progress through to completion without stalls.

---

## Pre-history (skill → app migration)

Earlier this project lived as a Claude skill (`/glyph-grid`) — a single-file p5.js renderer scaffolded into per-piece HTML files. Two pieces shipped that way: `eron-chip-inspection/` and `cloud-reach/`.

The migration to a proper desktop app (this repo) was driven by feedback that the iteration loop was too slow (edit code → reload → screenshot → repeat) and that there was no visual surface for the per-frame CONFIG mutations the v2 pipeline already supports via `__glyphGridTest.setConfig()`. The Tauri shell + Tweakpane studio panel addresses both: live slider-driven tuning, plus a native build that doesn't require the user to clone the skill repo.

Three-stage upgrade plan (per `~/.claude/plans/more-detail-on-hte-unified-gem.md`):
- **Stage 1 — Studio UI** (Tweakpane bindings + drag-drop + presets + export): COMPLETE
- **Stage 2 — Quality lifts** (octant cascade fix, STBN dither, OKLCH palette, k-d tree NN): COMPLETE
- **Stage 3 — Bleeding-edge** (CRT-beam shader, LLM-driven preset gen): 3A done, 3B deferred (rule #10: no paid LLM API)

---

## Known limitations

- Drag-drop from Finder is wired through `tauri://drag-drop` events when `dragDropEnabled: true`. Confirmed event listeners register on launch (console: `glyph-studio: registering Tauri drag-drop listeners`) but a full smoke test from Finder is pending — Finder access prompt timed out at the test session start.
- Canvas freezes momentarily when scrubbing certain sliders (cols/rows re-allocates buffers; glyphSet flip reloads the font cascade). Debouncing the heavy paths is queued — not yet implemented.
- p5 `loadImage` only loads the first frame of animated GIF inputs by design. Animated source video would require a different pipeline.

---

## Iteration tracking format

Future entries should include:
- **Date** + short title
- **Symptom** — what was observed
- **Diagnosis** — root cause from source reading
- **Fix** — source change made (file path, line number)
- **Verification** — how it was confirmed fixed
