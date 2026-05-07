# Optimization & Responsiveness Plan

User-reported symptoms (2026-05-07):
1. **"Slow switching between settings"** — config changes don't reflect in the canvas immediately
2. **"Image freezes when exporting GIF"** — canvas stops animating during recording
3. **"Settings don't apply when toggling"** — some config toggles seem ignored
4. **"Feels alpha/beta"** — overall roughness, not one-off quirks

This plan separates the three symptoms because they have **different root causes and different fixes.** Bundling them as "the app is slow" hides which work matters.

---

## Phase 0 — Instrument first (Karpathy: look at the data)

Before optimizing, **measure**. The current status bar shows total `sample Xms` per frame, but doesn't break down where the time goes. Add:

### A. Per-stage timing in the render loop

Wrap each major stage with `performance.mark` / `performance.measure`:

```
[draw start]
  ├─ scene render (fills `src` graphics)         → t_scene
  ├─ linearizeToLuminance (canvas → lumBuf)      → t_lum
  ├─ XDoG prefilter (if enabled)                 → t_xdog
  ├─ downsampleToCells (lumBuf → cellSignal)     → t_downsample
  ├─ EMA + breathing                             → t_ema
  ├─ dither apply                                → t_dither
  ├─ shape-vector NN (if shape-edge-aware)       → t_shape
  ├─ glyph drawText loop (cells → canvas)        → t_drawText
  ├─ postprocess chain                           → t_postproc
  └─ canvas.toDataURL (during record only)       → t_capture
```

Surface as a debug panel folder (`Perf`) showing rolling average over last 30 frames. Gated behind `CONFIG.studio.showPerf = true` (default off; on for this work).

### B. Switch-latency timer

Hook every Tweakpane `addInput` `.on('change', ...)` to record a `change → first-frame-with-new-value` latency. Log to a ring buffer accessible via `__lastSwitchLatencies()` in dev console.

### C. Recording profile

Add per-frame timing during `handleRecordFrame`:
- `t_capture` (canvas.toDataURL) ← suspected biggest win
- `t_zip` (JSZip add)
- `t_status` (DOM update)

### Effort
~2–3 hours of instrumentation. Pays back via every subsequent phase having real numbers instead of guesses.

---

## Phase 1 — GIF export freeze (highest-confidence fix)

### Root cause (verified by source reading)

`src/index.html:1727`:
```js
const dataUrl = drawingContext.canvas.toDataURL('image/png');
```

This runs **synchronously**, on the main thread, **per frame during recording**. Empirically ~100–300ms per call on a 1024×1024 canvas. For a 24-frame export: 2.4–7.2s of blocked main thread → animation visibly freezes → user sees a stuck canvas.

### Fix

Replace with async `canvas.toBlob()` (or `canvas.convertToBlob()` on OffscreenCanvas):

```js
function captureFrameAsync() {
  return new Promise((resolve, reject) => {
    drawingContext.canvas.toBlob((blob) => {
      if (!blob) return reject(new Error('toBlob returned null'));
      const reader = new FileReader();
      reader.onloadend = () => {
        const dataUrl = reader.result;
        resolve(dataUrl.split(',')[1]); // base64
      };
      reader.readAsDataURL(blob);
    }, 'image/png');
  });
}
```

Modify `handleRecordFrame` to await this. The draw loop continues during the encode → canvas keeps animating → no freeze. Bonus: status text updates between frames.

### Expected gain

Eliminates the freeze entirely. Frame-capture work moves off the main thread (browser uses worker pool internally for `toBlob`). The export takes the same wall-clock time, but the UI stays responsive.

### Risk

`toBlob` is async; the existing recording state machine assumes synchronous capture. Need to gate the next frame's `handleRecordFrame` call on the previous one's blob arriving. Otherwise frames could capture out of order or pile up.

### Effort
~3–4 hours including state-machine adjustment.

---

## Phase 2 — "Settings don't apply when toggling"

### Root cause hypotheses (need Phase 0 data to confirm)

A. **`glyphSet` change triggers async font reload.** Until the WOFF2 promise resolves (~50–500ms cold), the renderer keeps using the old glyph atlas. User toggles → sees no visual change → toggles back → same problem.

B. **`prefilter.mode` change doesn't invalidate the XDoG cache.** First frame after toggle may still use stale prefilter output.

C. **`postprocess.<stage>.enabled` toggling toggles the gate but the stage's internal state (e.g., phosphorDecay buffer) stays from previous frames.** First few frames after enable look weird until state warms up.

D. **Tweakpane updates the bound CONFIG value, but if a slider is mid-drag, the mid-drag value gets overwritten by the next tick.** Visible only on rapid scrubbing.

E. **`samplingStrategy` change has no live effect for some pipeline paths** — needs source-code audit.

### Investigation

Add a "toggle audit" mode: a `__configChange(key, value)` hook that logs:
- Which key changed
- What the renderer reads on the next frame
- Whether the next frame's output is byte-identical to the previous (i.e., did anything actually change?)

Run it for every CONFIG key. Identify the ones where output ≡ previous → those are the broken toggles.

### Fix path

For each broken toggle:
- **A (glyphSet):** show a "loading font…" indicator until promise resolves. Maybe pre-warm font on app start (load all 6 atlases in setup).
- **B (prefilter):** clear `__xdogCache` on prefilter config change.
- **C (postprocess state):** reset stage state on `enabled: false → true` transition.
- **D (mid-drag):** debounce slider events 50ms so rapid scrubs land on a single final value.
- **E (samplingStrategy):** audit pipeline path; ensure sampling code reads from CONFIG live.

### Effort
~6–10 hours, depends on how many keys are broken.

---

## Phase 3 — "Slow switching between settings"

### Root cause hypotheses

A. **Heavy work fires on every slider tick instead of after slider settles.** Cols/rows reallocate buffers; glyphSet reloads font; image-replace re-runs preprocess. If the user drags a slider through 50 intermediate values, the heavy work fires 50 times.

B. **Render loop is slow → next-frame-with-new-config arrives ≥1 frame late.** Sample time at 240×120 is ~250ms (per status bar), so the user perceives a 250ms+ delay between click and visual response. At 400×300 fine grid, 600ms+.

C. **Tweakpane refresh on every change.** The whole panel repaints when any binding mutates. With 50+ controls, this isn't free.

### Investigation

Phase 0's switch-latency timer surfaces actual numbers. Anything > 100ms is a candidate; anything > 300ms is "user can clearly perceive lag."

### Fix path

For (A) heavy-on-every-tick:
- Wrap `cols`/`rows`/`glyphSet`/`font.size` change handlers in a 120ms debounce. Light keys (palette/dither.mode) stay live.

For (B) inherently slow render:
- This is the hardest one. Per-frame work has hard floor. Options:
  - Lower default cell count from 240×120 to 200×100 (saves ~33% of per-cell work)
  - Cache the source-image preprocess at render-time across config changes that don't affect it (palette change shouldn't invalidate sourceProcessed; dither change shouldn't either)
  - Use `Uint8ClampedArray` instead of `Float32Array` for `lumBuf` where feasible (cuts memory pressure)
  - Skip the v2 advanced pipeline when the config doesn't actually need it (e.g., `selectionMode: 'brightness'` + no postprocess + no XDoG = use the fast v1 path)

For (C) Tweakpane refresh:
- Probably negligible vs render time. Skip until profiled.

### Expected gain

After (A): rapid scrubbing feels instant (drag → no work → release → 1 heavy work).
After (B): per-frame time drops 30–50% on default settings; switch latency proportional.

### Effort
(A) ~4 hours. (B) ~10–15 hours, requires careful pipeline analysis. (C) skipped.

---

## Phase 4 — Render pipeline microbenchmarks (after Phase 0)

Once we have per-stage timing, target the top 3 hotspots. Likely candidates by source reading:

1. **`linearizeToLuminance`** — 1.5M float multiply+add per frame at 1024×1024. Could move to a worker, or use SIMD WebAssembly, or precompute the linear-sRGB lookup table (256 entries → array lookup instead of `Math.pow`).

2. **`downsampleToCells`** — for 240×120 grid + 1024×504 canvas, ~120K cells × ~17 pixels per cell = 2M ops. Vectorizable.

3. **Glyph drawText loop** — `g.text(char, x, y)` is canvas2D-slow when called 28K times. Could batch by character (group cells with same codepoint, draw once with translation).

4. **Postprocess chain** — each stage reads + writes the full canvas (~2MB). At 4 stages enabled = 16MB of bus traffic per frame. OffscreenCanvas + worker is the right answer eventually.

These are guesses. Phase 0 will reveal which is actually expensive.

---

## Phase 5 — UX polish (small but high-impact)

Already-known annoyances that aren't strict perf:

- **No loading state during font load.** Add a "loading font…" tag in the status bar.
- **No "recording in progress" affordance during Export GIF.** Disable interactive controls; show progress bar.
- **No undo for accidental palette/preset changes.** Add a single-step undo via Cmd+Z.
- **Dropdown opens slowly** (Tweakpane v3 issue — may be fine in v4 ESM).
- **Status bar message can scroll off when slider scrubbing.** Add a small "applying…" overlay.

These are individually small; together they account for a lot of the "feels alpha/beta" perception.

---

## Implementation order (recommended)

1. **Phase 0 instrumentation** (~3h) — required before targeting fixes
2. **Phase 1 (GIF export freeze)** (~3h) — single biggest visible win
3. **Phase 2 (toggle correctness)** (~6–10h after audit) — fixes correctness, not just speed
4. **Phase 3A (debounce heavy sliders)** (~4h) — biggest perceived-lag win
5. **Phase 5 (UX polish)** (~3h) — cumulative
6. **Phase 4 (microbenchmarks)** — only if Phase 0 shows hot spots that matter and we still want more
7. **Phase 3B (pipeline rewrite)** — defer; only if still felt as slow after Phases 1-5

Total Phases 1-5: ~20 hours of focused work. Stops at "feels production-ready" without the deep WebGPU rewrite that would consume weeks.

---

## Verification per phase

| Phase | Pass criterion |
|---|---|
| 0 | `__perfReport()` returns valid breakdown; switch-latency log writes |
| 1 | During Export GIF, canvas continues animating; no visible freeze |
| 2 | Every CONFIG toggle audit shows a different output frame (no broken toggles) |
| 3A | Slider scrub: 50 ticks → 1 reallocation in dev console log (debounced) |
| 3B | Per-frame time at default settings ≤ 150ms (currently ~250ms) |
| 4 | Top hotspot's `t_X` ms cut in half |
| 5 | Each polish item visible in app (loading state, recording lock, etc.) |

---

## Out of scope for this plan

- **WebGPU pipeline rewrite** — months of work, separate project
- **p5.js → bare canvas2D migration** — huge breaking change for low payoff at current scale
- **Service Worker caching** — premature; profile first
- **Rust render path** — would be fast but kills creative-coding ergonomics; defer indefinitely
- **WebAssembly SIMD** — only if Phase 4 proves linearizeToLuminance is the bottleneck

---

## Research output

Phase 0 produces a `tests/PERF-BASELINE.md` document containing:
- Current per-stage timing breakdown at 3 representative configs
- Switch latency for every Tweakpane control
- Recording per-frame breakdown
- 3 specific hotspots with ms numbers attached

Subsequent phases reference this baseline. After all phases done, a `tests/PERF-AFTER.md` documents the new numbers — concrete proof of improvement.

This is what "research-driven" optimization looks like: don't guess, measure, then fix the proven hotspots.

---

## Specific user-input questions before implementation

Three things that would let me prioritize correctly. None block writing the plan; they refine the order.
