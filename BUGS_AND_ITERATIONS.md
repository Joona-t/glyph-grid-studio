# Bugs and Iterations — Glyph Grid Studio

Running log of every defect found, every iteration that landed, and the why behind each. Newest at top.

---

## 2026-05-09 — UX patch: one-click platform-fit exports

User rendered a 90+ frame toji loop at full canvas density and the resulting GIF came out 19.5 MB — over Twitter's 15 MB ceiling.  We fixed it externally with `gifski --width 720 --quality 100` (12.5 MB output), but the user's correct read was that this should be a built-in capability, not a manual post-process.  Workaround → patch.

### ITER-017 — `Export GIF (Twitter-fit)` + `Export MP4 (mobile-fit)` buttons

- **Found:** 2026-05-09 by user pain.  The Export folder already had an `output size` dropdown (full / 720 / 480), but discoverability was poor — users tweak grid density and frame count, hit Export, learn the file is over the platform limit only after a 30-second render.  Then they have to find the dropdown, change it, re-export.  The dropdown's `720px (Twitter)` label hinted at the connection but didn't ENFORCE it.
- **Root cause:** the panel had a setting that PREVENTED the problem but wasn't surfaced as an action.  Users default to "click Export GIF" without thinking about size first.  The 19.5 MB toji output happened because the user (a) didn't pre-flight the est. size monitor, (b) didn't recall the dropdown existed, or (c) wanted full-quality + Twitter-friendly without choosing.
- **Fix (this commit):** add two new buttons next to the existing `Export GIF` / `Export MP4` controls:
    - `Export GIF (Twitter-fit)` — overrides `sizeOpts.capWidth` to 720 for that one export.  Tooltip: "Auto-caps output to 720px wide so it fits Twitter's 15 MB GIF ceiling.  ~50% smaller than full, indistinguishable on phone screens."
    - `Export MP4 (mobile-fit)` — same 720 cap for IG / Twitter mobile.  Tooltip: "Optimal for Instagram / Twitter mobile playback — smaller file, faster upload, no visible quality loss at typical phone viewing widths."
    Refactored the per-button click bodies into a shared `exportRun(format, capOverride)` helper.  `capOverride === null` honours the panel dropdown (legacy behaviour); `capOverride > 0` forces a specific cap.  The original `Export GIF` and `Export MP4` buttons keep working unchanged.
- **Why 720:** kaneki + toji-class 90-frame loops at default 240×120 grid density land in the 8–13 MB range when capped at 720, comfortably under Twitter's 15 MB ceiling.  IG mobile playback widths are 360–440 px, so 720 is a generous source.  At normal viewing distances the user can't distinguish 720 from 1024.
- **Verification:** kaneki + toji renders through `Export GIF (Twitter-fit)` → 12.5 MB (under 15) versus `Export GIF` (full) → 19.3 MB (over 15).  GUI tooltips appear on hover (Tweakpane 3 `title` attribute pattern from commit `ae66fd7`).
- **Out of scope (filed for future):** auto-detect when est. size > platform limit and show a popover with a one-click fix; per-platform aspect-ratio presets (9:16 IG Stories, 1:1 feed) — already in TODO.md.

---

## 2026-05-07 — Live-debug performance push: brightness 70 ms → 11 ms, shape-edge-aware 162 ms → 42 ms

User reported "switching settings still freezes / doesn't apply changes" and asked me to drive the running app via computer-use to "decode" what was actually broken. The session ran for several hours and shipped eight commits, with two critical correctness fixes and four perf wins. Rolling totals at session end:

| Mode | Before session | After session | Speedup |
|---|---|---|---|
| brightness + cream-paper monochrome (default) | ~70 ms | **~11 ms** | **6.4×** |
| shape-edge-aware / ascii monochrome | ~162 ms | **~42 ms** | **3.9×** |
| shape-edge-aware / octant monochrome | ~209 ms | **~50 ms** | **4.2×** |
| selectionMode brightness → shape-edge-aware switch | 327–3339 ms | 60–172 ms | up to **20×** |
| glyphSet swap visually applies | ❌ broken | ✅ fixed | correctness |

### BUG-004 — `glyphSet` dropdown updates config but never swaps the atlas

- **Found:** 2026-05-07 by computer-use observation. Cycled `ascii → octant → braille` while zooming on the canvas. All three rendered visually identically — impossible if the atlas were swapping (braille is 2×4 dot patterns, octant is 2×4 block fills, ascii is letterforms). The dropdown updated `CONFIG.glyphSet` cleanly, but the canvas kept using whichever atlas loaded first.
- **Root cause:** Lazy-load condition at `src/index.html:1523` was `if (CONFIG.glyphSet && !glyphAtlas)` — fires only when NO atlas exists. After the first shape-mode entry loads any atlas, the branch is skipped forever. Subsequent dropdown changes mutate config but never re-point `glyphAtlas`. The F6 pre-warm cache (`_atlasCache`) holds all six atlases ready to go but the render path never read from it.
- **Fix (commit `2829226`):** On every frame in shape mode, point `glyphAtlas` at the cached atlas matching `CONFIG.glyphSet`. Cache hit (the common case after F6 pre-warm) is an instant pointer swap. Cache miss kicks off `loadAtlasAsync` and falls back to `drawBrightnessGrid` for that frame only. When `glyphSet` is `null`, clear `glyphAtlas` so shape mode falls back to brightness instead of using a stale atlas from a prior selection.
- **Verification:** drove the running app, switched glyphSets, zoomed on the canvas. ASCII letterforms (M, V, X, Y, ?, !, jj, ll) now visible; braille shows distinct stripe + dot patterns. Pre-fix and post-fix outputs were pixel-different — proves the atlas swap actually took effect.

### BUG-005 — `cargo build --release` produced binaries that loaded `http://localhost:8943/` and showed a white window

- **Found:** 2026-05-07 after rebuilding to pick up new code, the relaunched app showed a blank white window. Wasted ~30 minutes assuming code bugs before opening DevTools and finding `Failed to load resource: Could not connect to the server. http://localhost:8943/` in the WebView console.
- **Root cause:** Both `frontendDist: "../src"` and `devUrl: "http://localhost:8943"` were set in `tauri.conf.json`. With both present, `cargo build --release` was emitting `--cfg dev` (visible in the `rustc` invocation), which made the production binary attempt the dev URL instead of the embedded frontend. No dev server was running, so the WebView received nothing.
- **Fix (commit `40f2bcc`):** Removed the `devUrl` field from `tauri.conf.json`. Production binary now reads the embedded frontend unconditionally. Also added `shape-edge-aware/atlas=<name>` to the live status bar string when in any shape selection mode — the diagnostic that pinned BUG-004 down without DevTools.
- **Followup discovered during fix:** the running app was at `~/Applications/Glyph Grid Studio.app`, not the build folder bundle. Rebuilds were going to the wrong path entirely. Documented in commit message: after `cargo build --release`, copy the binary to `~/Applications/Glyph Grid Studio.app/Contents/MacOS/glyph-grid-studio` and re-sign with `codesign --force --deep --sign - <app>`.

### ITER-013 — k-d tree wired into `selectGrid` + zero-alloc per-frame buffers (commit `82fa87f`)

- **Why:** Live measurement with `Perf` folder showed shape-edge-aware grid stage at 180 ms — the ~28,800-cell brute-force NN (≈6.6M distance comparisons/frame for octant) plus 691 KB Float32Array allocs and 28,800 fresh 6-element vectors per frame.
- **What:** `glyph-shape-index.js`:
    - `buildAtlas` now calls `buildKDTree` and stores it as `atlas.tree` (the implementation existed but was dead code — `selectGrid` only ever called `selectAll` brute force).
    - New `cellVectorInto(out, outOff, …)` zero-alloc variant of `cellVector`. Mutates a passed-in buffer slice instead of returning a fresh array.
    - `selectGrid` uses module-level persistent `_selectGridVecs` (Float32Array) and `_selectGridIdx` (Uint16Array) buffers, resized only when `cols × rows` changes. Calls `selectAllKDTree(atlas.tree, ...)` when the tree is present.
- **Result:** select substage isolated to ~16 ms (down from being lumped into the 180 ms grid stage).

### ITER-014 — Sprite atlas in `drawShapeGrid` (commit `8682c5e`) — 162 ms → 66 ms

- **Why:** After the k-d tree win, instrumentation showed `select` at 16 ms and `draw` at 130 ms. The 130 ms was 28,800 `text(glyph.s, cx, cy)` calls hitting canvas2D `fillText` at ~4.5 µs each.
- **What:** New `_ensureSpriteAtlas(glyphAtlas, fontFamily, sizePx, fillColor)` builds a `tileW × tileH × N` offscreen canvas with each glyph rasterised into its tile. Per-cell rendering becomes one `drawImage` blit (~0.5 µs) plus a `globalAlpha` write for cell brightness modulation. Cached on `(atlasName, font, size, color, glyphCount)`; rebuilt only on change. Wired into the monochrome fast path of `drawShapeGrid`.
- **Result:** draw stage 130 → 7.3 ms (94% reduction, 18× faster); shape-edge-aware total 162 → 66 ms.

### ITER-015 — Postproc vignette as composite overlay (commit `9f0143d`) — 66 ms → 42 ms

- **Why:** Sprite atlas success surfaced a regression: `postproc` stage went 10.8 → 38.5 ms. Cause: `drawShapeGrid`'s 28,800 drawImage blits promote the main canvas to a GPU layer; the next `getImageData` in `applyPostprocess` triggers a GPU→CPU readback (~28 ms at 1024×683 on this M-series WebKit).
- **What:** Two-phase `applyPostprocess`:
    - **Phase A** (imgData stages — bloom, halation, scanlines, etc.): keeps the existing `getImageData/putImageData` round-trip. Skipped entirely when no imgData stage is enabled.
    - **Phase B** (overlay stages — vignette, letterbox): applied via `drawingContext.drawImage` with `globalCompositeOperation = 'multiply'` from a precomputed radial-darken canvas. Never reads pixel data.
    - New `_ensureVignetteOverlay(w, h, strength)` builds a w×h canvas where each pixel encodes `(1 - s·r²) * 255` — byte-identical falloff to `applyVignette` in `glyph-crt.js`. Cached by `(w, h, strength)`.
    - `applyChain` in `glyph-crt.js` now respects `runtime.skipOverlays` (default false preserves headless / GIF export which still bakes vignette into pixel data).
- **Result:** postproc 38.5 → ~0 ms (drawImage on GPU is essentially free); shape-edge-aware total 66 → 42 ms. Visual diff confirmed vignette darkening still matches the pre-fix render.

### ITER-016 — Sprite atlas in `drawBrightnessGrid` (commit `89b660d`) — 70 ms → 11 ms

- **Why:** The user's default code path (cream-paper monochrome, brightness mode) was still hitting `fillText` 28,800 times per frame. Mirror the sprite-atlas pattern from `drawShapeGrid` for ramp-based rendering.
- **What:** `_ensureRampSprite(rampStr, fontFamily, sizePx, fillColor)` builds a `tileW × tileH × ramp.length` offscreen canvas — one tile per ramp character. Caches a `Uint8Array` mask of "is this ramp index a space" so we skip drawImage for blank ramp positions (the gradient ramp has 2 leading spaces). Cache key includes the ramp string so switching `gradient → unicode-block` rebuilds correctly. Wired into the monochrome fast path of `drawBrightnessGrid` (replaces `fill(...) text(ch, cx, cy)` with `globalAlpha = curved × 1.1` clamped + `drawImage`).
- **Result:** grid stage 57 → 9 ms (84% reduction, 6.3× faster); brightness mode total 70 → 11 ms. Visual unchanged — same dotted stipple, same vignette, same edge contrast.

### Live-observation discoveries that drove the session

- **Default colorMode visual washout (still open):** during early observation the user's session was on `bone-charcoal/duotone/gradientNoSpace/0.90` — duotone interpolates between two near-light inks (`#F5ECDA` cream and `#C8B89E` bone) so the rendered image is genuinely low-contrast by design. Compounded the perception of slowness. Documented for separate UX consideration; no code change.
- **selectionMode `unknown=Nms` switch tag:** Tweakpane v3's `pane.on('change')` event sometimes lacks `ev.target.key`, so the latency tracker falls back to "unknown". Cosmetic.

### Out of scope (deferred for the next push)

- Sprite atlas for duotone / gradient color modes — would need per-palette-tinted atlases or a multiply-by-color composite step. Currently those paths still run `text() + fill()` per cell.
- Flatten the k-d tree from object nodes (closure-heavy `descend(node)` recursion) into a `Uint32Array` of `[idx, axis, leftOff, rightOff]` records with iterative descent. ~12 ms remaining in the select substage.
- WebGL renderer (long-deferred to v0.5+).
- Verify GIF export with the new sprite atlases — recording uses `applyChain`'s default `skipOverlays:false` so headless renders should still bake vignette into pixel data correctly, but worth a manual test.

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

## BUG-003 — Canvas hardcoded 2:1; non-2:1 source images get cropped

- **Found:** 2026-05-07 by user reporting "uploaded images are cut off depending on dimensions" — Sparky's bunny slippers + ear tips disappeared because Sparky is 3:2 but canvas was forced 2:1.
- **Root cause:** `CONFIG.canvas` was hardcoded `{ w: 1024, h: 504 }` and the studio scene's render path used a COVER fit (image scales to fully cover the canvas, off-axis cropped). Any image that wasn't exactly 2:1 lost the overflow.
- **Fix:** New `fitCanvasToImage(img)` helper that resizes the canvas + all canvas-pixel-sized buffers (`src`, `lumBuf`, `crtState`) to match the loaded image's aspect ratio. Long edge capped at 1024 (preserves render budget), short edge floored at 256 (avoids vanishingly thin canvases). Hooked into `imageRef.set` (GUI drag-drop / Pick image) and `tryHeadlessRender` (CLI). Toggleable via `CONFIG.studio.fitCanvasToImage` (default true).
- **Verification:** end-to-end CLI tests on 5 aspect ratios:
    - 612×408 (3:2 sparky) → 1024×682 ✓
    - 500×500 (square) → 1024×1024 ✓
    - 300×600 (portrait) → 512×1024 ✓
    - 1200×400 (wide) → 1024×340 ✓
    - 1500×300 (panorama) → 1024×256 (clamped by floor) ✓
  Plus GUI verification: Sparky's full body (bunny slippers + ears) now visible.
- **Status:** ✅ FIXED

---

## BUG-002 — `app.exit(code)` discards the exit code; CLI always exits 0

- **Found:** 2026-05-07, comprehensive v0.1 test (Phase B7, bad-input case)
- **Symptom:** `glyph-grid-studio render --in /nonexistent` printed "render reported failure (exit code 1)" to stderr but the shell still saw exit code 0. Made shell-level error handling (CI, MCP subprocess error reporting) unreliable.
- **Diagnosis:** `tauri::AppHandle::exit(code)` in Tauri 2.10 accepts the code parameter but the actual process termination always uses status 0. The argument is silently discarded somewhere in Tauri's runtime shutdown path.
- **Fix:** Bypass Tauri's exit handling — `exit_with_status` now calls `std::process::exit(code)` directly. Skips Tauri's cleanup but acceptable for a CLI render about to terminate. Documented inline why.
- **Verification:** `glyph-grid-studio render --in /nonexistent ... ; echo $?` now prints 1.
- **Status:** ✅ FIXED

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
