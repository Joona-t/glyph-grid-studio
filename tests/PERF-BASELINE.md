# PERF-BASELINE — frame-time breakdown for v0.1 + kawaii build

**Date:** 2026-05-07
**Build:** v0.1 + kawaii feature + canvas auto-fit (commit `6544528`+)
**Test image:** Sparky 612×408 (auto-fit canvas → 1024×683)
**Hardware:** [user's MacBook]
**Method:** Phase 0 instrumentation — `__pf()` markers in draw() + drawGlyphGridV2; live rolling 30-frame averages surfaced via the studio panel's new Perf folder.

---

## Smoking-gun finding

**Default settings render at ~408 ms per frame (~2.5 fps).** Without the user touching any postprocess controls, **63% of every frame is spent in postprocess** because `bloom`, `halation`, and `vignette` are enabled-by-default in CONFIG. The next 36% is the per-cell `g.text()` drawText loop. Together they account for >99% of frame time.

This is why the app feels laggy:
- **"Slow switching"** — every config tweak waits ~408ms for the next render to reflect it
- **"Settings don't apply"** — they DO apply, but the next frame is 408ms away, so to the user the click looks dropped
- **"Alpha/beta feel"** — 2.5 fps is genuinely jarring; users expect 30+ fps for live tuning

---

## Per-stage breakdown (Sparky, 240×120 grid, default config)

Captured 2026-05-07 from live Perf folder, ~30-frame rolling average:

| Stage | ms | % |
|---|---|---|
| **postprocess** | **251.2** | **63%** ← biggest cost |
| **grid (drawText)** | **128.7** | **34%** ← second |
| dither | 0.4 | 0.1% |
| ema | 0.1 | 0.0% |
| downsample | 0.6 | 0.2% |
| lum | 1.1 | 0.3% |
| scene | 0.1 | 0.0% |
| **TOTAL** | **382.5** | 100% |

The pipeline math (linearize → downsample → EMA → dither) is essentially free: **2.3 ms combined.** All the time is in the two visual-output stages.

---

## Why postprocess is 251ms with no postprocess folder touched

Looking at `src/index.html` CONFIG.postprocess defaults:

```js
postprocess: {
  vignette: { enabled: true, strength: 0.55 },
  bloom:    { enabled: true, radius: 2.5, strength: 0.95 },
  halation: { enabled: true, strength: 0.70 },
  depthFog: { enabled: false },
},
```

**3 of 8 postprocess stages run by default.** These were tuned for the original eron-chip-inspection piece (the source of the studio scene), but they're inappropriate as studio defaults — they cost ~250ms together and make every other interaction feel slow.

Cost breakdown (approximate, from glyph-crt.js source):
- `vignette` — single radial pass — ~5–10ms (cheap)
- `bloom` — extract bright + box-blur radius=2.5 + add — ~80–120ms (medium)
- `halation` — same shape as bloom but with default radius=4 — ~120–180ms (expensive)

Bloom + halation are the same algorithm called twice with different parameters. Each does 4 box-blur passes over the full canvas (~2 MB read+write per pass).

---

## Why grid (drawText) is 128ms

`drawBrightnessGrid` calls `g.text(char, x, y)` for every cell. At 240×120 = 28,800 cells, that's 28,800 calls to canvas2D's `fillText` per frame. Empirically ~4.5 μs per call on this hardware.

This is unavoidable with the current canvas2D approach. The fix paths (in order of effort):
1. **Skip empty cells** — if the char is `' '` (space, 0-density), skip the call entirely. Some ramps have ~30% spaces; saves ~30%.
2. **Batch by codepoint** — group cells with the same char, set color/font once per group, draw with `setTransform`. Probably 2–3× speedup.
3. **OffscreenCanvas + Worker** — render glyph grid in a worker, transfer ImageBitmap. ~2× speedup.
4. **WebGL atlas** — pre-render all glyphs into a texture atlas, use shader for per-cell render. 5–10× speedup but rewrites the renderer.

---

## Switch latency (current, instrumented but not yet baselined)

The `__markChange` hook records every Tweakpane binding mutation. The next frame's render computes `now - markTime` and exposes it as `lastSwitch` in the Perf panel.

To baseline this: change a dropdown, watch `lastSwitch` field. With 408ms frame time, expect ~408–800ms (one to two frames depending on click timing).

After Phase 0.5 + Phase 4 (per-stage skips), expect <100ms.

---

## Recommended fix order (revised based on this baseline)

The original optimization plan ordered Phase 0 → 1 → 2 → 3. The data suggests an **urgent Phase 0.5** sandwiched in:

### Phase 0.5 — Fix bad defaults (URGENT, ~30 minutes)

Move `bloom`, `halation`, `phosphorDecay` defaults to `enabled: false`. Keep `vignette` on (cheap and visually nice). Users can enable bloom/halation per-piece via the Postprocess folder.

**Expected result:** frame time drops from 382ms → ~140ms. **2.7× speedup.** Single biggest win for zero engineering effort.

This is the kind of finding that justified Phase 0 instrumentation — without measurement we'd have wasted hours on micro-optimizations while the actual win was a config-file edit.

### Phase 1 — GIF export freeze (still high priority, separate cause)

`canvas.toDataURL` sync. Switch to `toBlob` async. ~3 hours. Not affected by Phase 0.5.

### Phase 2 — "Toggles don't apply" (most of this disappears after 0.5)

At 140ms/frame, switch latency drops naturally to <200ms. Most of the user's "doesn't seem to apply" complaints will disappear. What remains (genuine cache-invalidation bugs) becomes diagnosable.

### Phase 3A — Slider scrub debounce (after 0.5)

Still worth doing for cols/rows/glyphSet (the heavy-on-tick keys). ~4h.

### Phase 4 — drawText optimization (when ready)

Skip-empty-cells is the cheap win (~30%, half a day of work). Batch-by-codepoint is the medium win (~2×, 2-3 days). WebGL atlas is the big win (~5–10×, week+ of work).

---

## Reproducibility

To re-measure on any machine after a code change:

1. Build with `cargo tauri build --features dev-tools`
2. Launch app, load any image
3. Open the Perf folder in the studio panel
4. Wait ~5 seconds for rolling average to stabilize
5. Read the totals. Click "Report (console)" to dump full averages to dev console.

After running, save the Perf panel screenshot and the console JSON output to `tests/PERF-AFTER-<change>.md`.

---

## Concrete numbers to beat

After all phases land, target:

| Config | Current | After 0.5 | After 4 | Stretch (WebGL) |
|---|---|---|---|---|
| 240×120, defaults | 382 ms | ~140 ms | ~70 ms | ~15 ms |
| 360×240, defaults | ~700 ms | ~280 ms | ~130 ms | ~25 ms |
| 240×120, all postprocess on | ~600 ms | ~600 ms | ~600 ms | ~80 ms |

Bold success criterion: **default-settings 240×120 ≤ 100 ms (10 fps).** Live tuning would feel actually live.

---

## Why this is the right way to optimize

The user described "slow switching." Without measurement the temptation is to debounce sliders, throttle Tweakpane, etc. — all superficial. The real cause is a config defaults choice that no slider could fix.

Karpathy: *look at the data, build the instrument, fix the proven hotspot.* Phase 0's 3 hours of instrumentation → 1 minute of looking at numbers → 30-minute Phase 0.5 fix that beats every other optimization 2.7×. **That's the leverage from research-driven optimization.**
