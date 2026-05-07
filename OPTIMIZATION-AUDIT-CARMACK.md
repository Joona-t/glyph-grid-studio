# Carmack-lens audit — Glyph Grid Studio

A from-the-hot-path-outward audit of every place in the render pipeline that **allocates per frame, awaits per frame, branches per frame, or does redundant work.** Lessons-learned from `.plan` files of John Carmack circa 1996–2014: profile mercilessly, eliminate redundancy, pre-compute what you can, question every allocation.

This doc complements `OPTIMIZATION-PLAN.md` and `tests/PERF-BASELINE.md`. Where the plan said "what to do," this audit says **why each item is on the list, in what order to fix it, and what the leverage is.**

Built on top of Phase 0/0.5 (instrumentation + bad-defaults fix, already shipped commit `9ba6db5`, 2.5× speedup confirmed).

---

## Top-10 findings, ordered by leverage (effort vs gain)

### F1 — `glyphSet` switch freezes for 1–6 seconds (USER-REPORTED)

**Where:** `src/lib/glyph-studio.js:472–484`

**What:**
```js
.on('change', function (e) {
  config.glyphSet = e.value === 'null' ? null : e.value;
  if (window.GlyphGrid && window.GlyphGrid.fonts && window.GlyphGrid.fonts.load) {
    window.GlyphGrid.fonts.clearCache && window.GlyphGrid.fonts.clearCache();  // ← problem
    window.GlyphGrid.fonts.load({ ... });                                       // ← awaits 3 font promises
  }
});
```

**Why it freezes:** `clearCache()` invalidates the descriptor; `load()` then awaits three `waitForFace` calls (3s + 1.5s + 1.5s timeouts each). Even though the fonts are *already loaded*, the path through resolved-promise-chaining can take 50-300ms; on cold cache or flake, much longer. The Tweakpane onChange handler doesn't return until the load promise settles, so the UI feels stuck.

**Why it shouldn't be needed:** The fonts are the same WOFF2 files for every glyphSet — switching from octant to sextant doesn't change which fonts load. Only the *requestedSet metadata* changes. The Stage 2A `trustRequested` flag already handles user intent without needing to re-probe availability.

**Fix (5 minutes):** Remove the `clearCache()` call entirely. The cache key includes `requestedSet`, so a different glyphSet naturally produces a cache miss → `load()` re-runs but `injectFaces()` is idempotent (early-out on existing `<style>` element) and `waitForFace` resolves quickly when fonts are already in `document.fonts`.

**Expected gain:** glyphSet switch goes from 1–6s → <50ms. **Eliminates the user's freeze complaint.**

---

### F2 — `drawText` calls dominate frame time (138ms/frame after Phase 0.5)

**Where:** `drawBrightnessGrid` in `src/index.html` ~line 1530

**What:** 28,800 calls per frame to `g.text(char, x, y)` at 240×120 grid. Each canvas2D `fillText` is ~4-5μs on this hardware. Total ~130ms.

**Carmack lens:** "Question every call." Why are we calling fillText 28K times when many cells render the same character?

**Fix tier A (cheap, ~30% gain):** Skip cells whose char is `' '` (space). The default `gradient` ramp has 2 leading spaces of total length 23 — about 9% of cells map to space at low brightness. Higher gamma settings push more cells into the dark = more space cells. Skip them.

**Fix tier B (medium, ~2× gain):** Group cells by char + color. Single `setTransform` per group. A typical 240×120 grid uses ~20 distinct codepoints — that's 20 groups instead of 28,800 calls. Need to keep order stable (no z-fighting) but glyphs don't overlap so order doesn't matter.

**Fix tier C (big, ~5–10× gain, day-of work):** Pre-render all glyphs into a sprite atlas (offscreen canvas), then for each cell call `drawImage(atlas, srcX, srcY, w, h, dstX, dstY, w, h)`. drawImage is dramatically faster than fillText because it's a memcpy not a typeset.

**Recommendation:** A first, then C if A wasn't enough. Skip B (its complexity doesn't beat C's pre-render approach).

**Expected gain:** A → 138ms → ~95ms. C → 138ms → ~25ms.

---

### F3 — Per-frame heap allocations in dither pipeline

**Where:** `src/lib/glyph-dither.js` lines 91, 105, 126, 165, 228, 272

**What:** Every dither mode allocates `new Uint8Array(signal.length)` per call. At 240×120 grid = 28,800 bytes/frame. Across multiple framerate-affected paths, this is ~30-60 KB allocated and GC'd per frame. JS GC is generational so young-gen is fast, but it's still pressure that shouldn't be there.

**Carmack lens:** "Allocate at startup, reuse forever."

**Fix:** Each dither function accepts an optional `out` Uint8Array parameter; caller passes a persistent buffer. Single allocation in setup(), reused per frame.

**Expected gain:** Likely small (~1-3ms/frame) but eliminates GC stutter that may show up as random janky frames during long sessions.

---

### F4 — Postprocess `getImageData/putImageData` per frame

**Where:** `applyPostprocess` in `src/index.html` ~line 1622

**What:** Every frame, even with no postprocess enabled, calls:
```js
const imgData = ctx.getImageData(0, 0, w, h);
GlyphGrid.crt.applyChain(imgData, ...);
ctx.putImageData(imgData, 0, 0);
```

That's ~2 MB read + 2 MB write per frame regardless of whether anything inside `applyChain` actually does work. At 4 stages enabled = 16 MB of bus traffic/frame.

**Carmack lens:** "Don't read what you don't need."

**Fix:** Short-circuit `applyPostprocess` if no stage is enabled. Existing `gatePostprocess(CONFIG)` does this at the gate, but the gate may already be true if any stage with `enabled:true` exists — even if that stage's parameters mean it's a no-op (e.g. `vignette.strength=0`).

Tighter gate: check each stage individually for "would change pixels." Skip the get/put entirely if every stage would be a no-op.

**Expected gain:** ~5-15ms/frame on configs that have one stage barely enabled. Minor but cumulative.

---

### F5 — `cellIndex = new Uint8Array(out)` returned by every dither.apply

**Where:** `glyph-dither.js` `apply()` returns a freshly-allocated array

**What:** Same root cause as F3 but at the API level. Every call site assigns `cellIndex = ...` then immediately reads from it. Could be a persistent buffer.

**Fix:** dither.apply takes an `out` argument (or store the persistent buffer on a state object passed in).

**Expected gain:** Couples with F3.

---

### F6 — Async shape-vector atlas load on first shape-edge-aware use

**Where:** `glyph-shape-index.js` (atlas loading) + `index.html` `drawGlyphGridV2` lazy load

**What:** First time the user switches to `selectionMode: 'shape-edge-aware'` with a non-trivial glyphSet, the renderer calls `fetch(glyph-sets/octant.json)` async. While loading, the renderer falls back to ASCII (or worse, shows nothing). Net: 200–500ms of "broken" output the first time.

**Carmack lens:** "Pre-load everything you can predict you'll need."

**Fix:** During app setup(), kick off all 6 atlas fetches in parallel. They'll be cached in `window[CACHE_KEY]` by the time the user touches the dropdown.

**Expected gain:** First-time shape-edge-aware switch: 200-500ms freeze → instant.

---

### F7 — Tweakpane fires onChange on every drag tick (no debounce)

**Where:** Every `addInput(...).on('change', ...)` chain

**What:** Dragging a slider at 60Hz fires up to 60 onChange events per second. For light keys (palette), each fires a config write + next-frame redraw — fine. For heavy keys (cols/rows/glyphSet/font.size), each fires expensive work. A 1-second drag through cols 60→400 fires ~60 buffer reallocations.

**Carmack lens:** "Don't do work you'll redo."

**Fix:** Wrap heavy onChange handlers in a 120ms debounce. Stash the latest value; do the work after slider settles. Lightweight: implement once in `glyph-studio.js`'s wrapped addInput helper.

**Expected gain:** Slider scrub from 60 reallocations/sec → 1 reallocation. UX feel: instant.

---

### F8 — Multiple `Math.*` per cell in EMA loop (cheap but visible at high density)

**Where:** `drawGlyphGridV2` ~line 1453-1458

**What:** 
```js
for (let i = 0; i < cellSignal.length; i++) {
  cellSignalEma[i] = (1 - _emaA) * cellSignalEma[i] + _emaA * cellSignal[i];
  const h = ((i * 2246822519) ^ _seed) >>> 0;
  const jitter = ((h & 0xff) - 128) * _jScale;
  cellSignal[i] = Math.max(0, Math.min(1, cellSignalEma[i] * _breatheGain + jitter));
}
```

The `Math.max(0, Math.min(1, x))` is a clamp. Two function calls per cell × 28,800 cells = 57,600 Math calls/frame.

**Carmack lens:** "Branchless > branched. Inline > called."

**Fix:** Replace `Math.max(0, Math.min(1, x))` with branchless: `(x < 0) ? 0 : (x > 1) ? 1 : x` — ternaries inline, no function call overhead.

**Expected gain:** ~1ms/frame on low-end hardware. Ignorable in normal use, but adds up at 400×300 grid.

---

### F9 — Default font.size and grid don't respect canvas aspect after auto-fit

**Where:** Implicit, observed during BUG-003 fix verification

**What:** When user loads a portrait image and the canvas auto-fits to e.g. 512×1024, the grid stays at 240×120. Cell pixel dims become (512/240) × (1024/120) = 2.13 × 8.5 — extremely non-square. Render still works but cells are weirdly stretched.

**Carmack lens:** "Aspect-aware tooling."

**Fix:** When auto-fitting canvas, also adjust cols/rows proportionally to keep cell-pixel-aspect ≈ 1:1. (Or expose this as a "match cell aspect" toggle.)

**Expected gain:** Quality not perf. Output looks much better on portrait sources.

---

### F10 — `Object.assign` on hot path

**Where:** `drawGlyphGridV2` ~line 1481

**What:** `const ditherOpts = Object.assign({}, CONFIG.dither, { levels, seed, frameIdx });` — runs every frame in the dither path. Allocates a new object each time.

**Fix:** Mutate a persistent `ditherOpts` object instead of creating a new one. Single allocation in setup, reuse per frame.

**Expected gain:** 0.1–0.5ms/frame depending on JIT.

---

## Implementation order (Carmack rule: fix the worst first)

| Order | Finding | Effort | Gain |
|---|---|---|---|
| 1 | F1 glyphSet freeze | 5 min | 1–6s → <50ms (user-reported, largest perceived win) |
| 2 | F7 debounce heavy sliders | 2 hours | 60 reallocs/sec → 1 (UX feels instant) |
| 3 | F2A drawText skip empty cells | 2 hours | 138ms → ~95ms (~30%) |
| 4 | F6 pre-warm shape atlases | 1 hour | 200–500ms freeze → 0 on first use |
| 5 | F4 tighter postprocess gate | 1 hour | 5–15ms/frame on lightly-used pp |
| 6 | F3+F5+F10 zero allocations | 3 hours | 1–3ms/frame, smoother feel |
| 7 | F8 branchless clamp | 30 min | 1ms/frame (tiny) |
| 8 | F2C drawText sprite atlas | 1 day | 138ms → ~25ms (5×) — biggest remaining win |
| 9 | F9 aspect-aware grid | 1 hour | quality fix, not perf |

**Time investment vs. payoff:**
- First 4 items: ~5 hours → app feels production-quality
- Add item 8: ~1.5 days → 60+ fps live tuning at default settings

---

## Carmack patterns observed (transferable to future projects)

1. **Pre-compute > runtime compute.** Font atlas glyphs, shape vectors, dither tables — all baked at startup or build time, looked up at runtime.
2. **Allocate once, reuse forever.** Buffers sized at setup, mutated in place. New typed-array per frame is a smell.
3. **Async work belongs in setup, not user-input handlers.** F1 is the canonical example: a 1-line synchronous-feeling click triggers 6 seconds of awaits.
4. **Idempotent re-init.** F1's `injectFaces` is already idempotent — that pattern saved us. Never lost the lesson.
5. **Tighten gates.** F4's "if any stage is even slightly on" gate is too loose. "If any stage would actually change a pixel" is the right gate.
6. **Branchless > branched on hot paths.** F8 is small-leverage but pure form.
7. **Profile before optimizing.** Phase 0 instrumentation found a 2.5× win that no amount of micro-tuning would've matched. Repeat the discipline before each optimization phase.

---

## What NOT to do (Carmack lens warnings)

- **Don't rewrite the renderer in WebGL until F1–F8 land.** Most of the perceived slowness is fixable with config + small JS changes. WebGL is a week+ rewrite for ~5× speedup; the small fixes are ~5× speedup combined with much less risk.
- **Don't move to Workers prematurely.** Worker boundary costs (postMessage, ImageBitmap transfer) eat the gains until per-stage cost > 50ms.
- **Don't add Service Worker caching until profiling shows network is the bottleneck.** Currently it isn't.
- **Don't refactor until the audit findings land.** The current code is messy in places but it works; rearrange after the bugs are fixed.

---

## Phase 0.5 retro

- **Found by Phase 0 instrumentation:** bloom + halation enabled by default, costing 251ms/frame
- **Fix:** 30-second config edit
- **Result:** 382ms → 151ms (2.5×)
- **What we'd have done without instrumentation:** chased symptoms (debounce sliders, throttle Tweakpane) for hours and missed the actual win

This is the lesson: **measure first, fix second, optimize third.** The order matters. Skipping measurement is what makes "the app feels slow" turn into a week of wasted effort.

---

## Status

This audit is the source-of-truth for the next 5–8 hours of optimization work. After F1–F4 land, re-measure and update `tests/PERF-BASELINE.md` with new numbers. Each finding has a clear before/after measurement protocol.

The instrument (Perf folder + dev console `__perfReport()`) stays in the build. Future regressions are caught by running through the same stages.
