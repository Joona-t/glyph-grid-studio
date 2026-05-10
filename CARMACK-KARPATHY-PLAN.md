# CARMACK + KARPATHY OPTIMIZATION PLAN

The single source-of-truth document for v0.1 production-grade optimization. Consolidates the original `OPTIMIZATION-PLAN.md` (5 phases by symptom) and `OPTIMIZATION-AUDIT-CARMACK.md` (10 findings by hot-path). One plan, executed in dependency order, with measurement before and after every change.

**The two lenses, distilled:**

| Carmack | Karpathy |
|---|---|
| Mechanical efficiency. Allocations are sins. Hot loops are sacred. Eliminate redundancy. Pre-compute > runtime compute. | Look at the data. Build the instrument. Profile before optimizing. Each fix is an experiment; record the gradient. |
| **What:** the right code | **How:** the right method |
| `.plan` file disciplined | Karpathy "lr=3e-4, batch=32, here's why" disciplined |

These lenses agree more than they disagree. Carmack tells us *what to fix*; Karpathy tells us *how to know we fixed it.* Production-quality demands both.

---

## Status

| Step | What | Effort | Result |
|---|---|---|---|
| ✅ Phase 0 | Build instrumentation (Perf panel + switch-latency tag) | 3 h | Real numbers per stage |
| ✅ Phase 0.5 | Disable expensive postprocess defaults (bloom + halation off) | 5 min | **408 ms → 151 ms (2.5×)** |
| ✅ F1 | glyphSet switch freeze (remove redundant `clearCache` + load) | 5 min | **1–6 s → 174 ms (6–35×)** |
| 🔄 F7 | Debounce heavy sliders (cols/rows/font.size) | 2 h | TBD |
| ✅ F4 | Tighter postprocess gate (skip get/putImageData if all stages no-op) | 1 h | shipped 2026-05-10 (`src/index.html:2898-2925`); next loop cycle measures real gain — expected ~5–15 ms/frame on configs with stages enabled but intensity 0 |
| 🔄 F2A | drawText skip empty cells | 2 h | TBD |
| ✅ F8 | Branchless clamp in EMA loop (manual ship — loop hadn't reached it) | 30 min | shipped 2026-05-10 (`src/index.html:1612-1619`); next loop cycle measures real gain |
| 🔄 F3+F5+F10 | Zero-alloc hot path (persistent buffers) | 3 h | TBD |
| 🔄 F6 | Pre-warm shape atlases at startup | 1 h | TBD |
| 🔄 Phase 1 | GIF export freeze (toDataURL → toBlob async) | 3 h | TBD |
| 🔄 F9 | Aspect-aware grid (cols/rows scale on canvas resize) | 1 h | quality, not perf |

Cumulative target: **~70 ms/frame at default settings** (~14 fps live tuning) + **GIF export without UI freeze**.

---

## Methodology — Karpathy's discipline

Every change in this plan follows the same loop:

1. **Hypothesize** — what we expect to gain (in ms/frame or other measurable)
2. **Measure before** — capture Perf panel reading at default config, write to `PERF-BEFORE.txt`
3. **Implement** — minimal change, no scope creep
4. **Measure after** — capture Perf panel reading at same config, write to `PERF-AFTER.txt`
5. **Compare** — actual gain vs hypothesized; if 2× off, investigate why
6. **Commit** — log the actual gain in commit message; don't claim what we didn't measure

This costs about 5 extra minutes per fix. It's worth every second because:
- Catches "fix" that didn't actually fix anything (we've all done this)
- Catches "fix" that fixed one config but regressed another
- Builds a paper trail useful for future you / future AIs

---

## The fixes, in dependency order

### F7 — Debounce heavy sliders (NEXT)

**Where:** `src/lib/glyph-studio.js` `addInput` calls for `cols`, `rows`, `font.size`, and the `glyphSet` change handler

**Hypothesis:** Slider drag fires 30-60 onChange events/sec. Heavy keys (cols/rows/font.size) trigger buffer reallocations + scene cache clears each tick. A 1-second drag = 30-60 reallocs. Debouncing to fire only after slider settles (120 ms timeout) → 30-60 reallocs becomes 1.

**Expected gain:** Slider scrubbing UX feels instant. Frame time during scrub becomes consistent (no spikes from mid-drag reallocations).

**Implementation sketch:**

```js
function debouncedChange(handler, ms) {
  let t = null;
  return function (e) {
    if (t) clearTimeout(t);
    t = setTimeout(function () { t = null; handler(e); }, ms || 120);
  };
}

// at addInput call:
fGrid.addInput(config.grid, 'cols', { min: 60, max: 400, step: 5 })
  .on('change', debouncedChange(function () {
    /* heavy work here, only fires after settle */
  }, 120));
```

The render loop's auto-realloc-on-mismatch check still runs each frame, so the canvas updates immediately — the debounce just prevents *redundant* reallocations during the scrub.

---

### F4 — Tighter postprocess gate

**Where:** `src/index.html:1622` `applyPostprocess()`

**Hypothesis:** `getImageData` + `putImageData` cost ~5 ms/frame even when no postprocess stage actually changes pixels. The current `gatePostprocess()` returns true if any stage's `enabled === true`, but a stage with `enabled: true` and `strength: 0` still passes the gate.

**Expected gain:** 5-15 ms/frame for configs with one nominally-enabled-but-no-op stage.

**Implementation sketch:**

```js
function isPostprocessActive(pp) {
  if (!pp) return false;
  // stage is "active" only if its parameters would actually change pixels
  if (pp.vignette?.enabled && pp.vignette.strength > 0.001) return true;
  if (pp.bloom?.enabled && pp.bloom.intensity > 0.001 && pp.bloom.radius > 0) return true;
  // ... and so on for each stage
  return false;
}

function applyPostprocess() {
  if (!isPostprocessActive(CONFIG.postprocess)) return;  // skip get/putImageData entirely
  // ... existing code
}
```

---

### F2A — drawText skip empty cells

**Where:** `drawBrightnessGrid` in `src/index.html` ~line 1530

**Hypothesis:** ~9-30% of cells render `' '` (space) at low brightness. `g.text(' ', x, y)` does measureText + fillText for what amounts to no-op. Skipping these saves the call entirely.

**Expected gain:** Grid stage from 138 ms → ~95 ms (~30%). Total frame time ~151 ms → ~108 ms.

**Implementation sketch:** add `if (char === ' ') continue;` at the top of the per-cell loop body. Possibly also skip cells where the resolved alpha is below a threshold (in monochrome mode: low-alpha space looks identical to background).

---

### F8 — Branchless clamp in EMA loop

**Where:** `drawGlyphGridV2` `~line 1457`

**Hypothesis:** `Math.max(0, Math.min(1, x))` does 2 function calls per cell × 28,800 cells = 57,600 Math calls/frame. Replacing with ternary `(x < 0) ? 0 : (x > 1) ? 1 : x` inlines the comparison.

**Expected gain:** 0.5-1 ms/frame. Tiny but pure form, easy fix.

---

### F3 + F5 + F10 — Zero-alloc hot path

**Where:** `glyph-dither.js` (`new Uint8Array(signal.length)` per frame in 6 modes), `drawGlyphGridV2` (`Object.assign({}, ...)` per frame for ditherOpts)

**Hypothesis:** Per-frame heap allocations cause GC pressure that shows up as occasional janky frames. At 240×120 grid: ~30-60 KB allocated/frame across all dither modes + opts. JS GC is generational so young-gen is fast, but cumulative pressure is real.

**Expected gain:** 1-3 ms/frame *on average*; eliminates random spikes. The variance reduction matters as much as the average.

**Implementation sketch:**

```js
// In glyph-dither.js, accept persistent out buffer:
function applyBayer(signal, cols, rows, levels, mat, size, out) {
  if (!out || out.length !== signal.length) out = new Uint8Array(signal.length);
  // ... mutate out in place
  return out;
}

// In drawGlyphGridV2: persistent ditherOpts object:
let _ditherOpts = { mode: '', asSourcePrefilter: true, levels: 16, seed: 0, frameIdx: 0 };
function frame() {
  _ditherOpts.mode = CONFIG.dither.mode;
  _ditherOpts.levels = levels;
  _ditherOpts.seed = CONFIG.seed;
  _ditherOpts.frameIdx = frameIdx;
  cellIndex = GlyphGrid.dither.apply(cellSignal, cols, rows, _ditherOpts, _ditherOut);
}
```

---

### F6 — Pre-warm shape atlases at startup

**Where:** `src/index.html` `setup()` function

**Hypothesis:** First time user switches to `selectionMode: 'shape-edge-aware'` with a non-trivial glyphSet, the renderer fetches `glyph-sets/octant.json` async. While loading, the renderer falls back. Pre-warming all 6 atlases at app launch eliminates this first-use freeze.

**Expected gain:** 200-500 ms freeze on first shape-edge-aware switch → 0. Doesn't change steady-state perf.

**Implementation sketch:** in `setup()`, kick off `fetch('glyph-sets/' + name + '.json')` for all 6 sets. Cache the parsed JSONs on a global. The shape-vector load path checks this cache first.

---

### Phase 1 — GIF export freeze

**Where:** `src/index.html:1727` `handleRecordFrame()`

**Hypothesis:** `canvas.toDataURL('image/png')` is sync and slow (~100-300 ms per call × 24-48 frames = 2.4-15 s of blocked main thread). Switching to `canvas.toBlob()` async lets the draw loop continue between captures.

**Expected gain:** UI stays responsive during recording. Total wall-clock for recording stays similar (the encode work has to happen somewhere) but no visible freeze.

**Implementation sketch:**

```js
function captureFrameAsync() {
  return new Promise((resolve, reject) => {
    drawingContext.canvas.toBlob((blob) => {
      if (!blob) return reject(new Error('toBlob returned null'));
      const reader = new FileReader();
      reader.onloadend = () => resolve(reader.result.split(',')[1]);
      reader.onerror = () => reject(reader.error);
      reader.readAsDataURL(blob);
    }, 'image/png');
  });
}

// recState gets a flag indicating capture-in-progress:
async function handleRecordFrame() {
  if (recState._capturing) return; // skip; previous capture not yet done
  recState._capturing = true;
  try {
    const base64 = await captureFrameAsync();
    recState.zip.file(name, base64, { base64: true });
    if (recState.framesB64) recState.framesB64.push(base64);
    recState.frameIdx++;
    // ... status update
  } finally {
    recState._capturing = false;
  }
}
```

The state machine needs to handle the "drop frame if previous still capturing" case gracefully. p5's draw loop continues to fire; it just sometimes re-records the same content. Acceptable for v0.1.

---

### F9 — Aspect-aware grid (quality fix)

**Where:** `src/index.html` `fitCanvasToImage()` from BUG-003 fix

**Hypothesis:** When auto-fitting canvas to a portrait image, the cell pixel aspect ratio gets distorted. Cols/rows should adapt proportionally.

**Expected gain:** Output looks better on portrait sources. No perf change.

**Implementation sketch:** when canvas is resized via `fitCanvasToImage`, also adjust `CONFIG.grid.cols` and `CONFIG.grid.rows` to keep cell pixel aspect close to 1:1 (or close to a target like the font's native aspect of ~0.55:1).

---

## Verification matrix

After all fixes land, re-baseline against:

| Config | Current | Target |
|---|---|---|
| Default 240×120, vignette only | 151 ms | ≤ 70 ms |
| 360×240 high-density, vignette only | ? | ≤ 200 ms |
| 240×120 + bloom + halation enabled | ? | ≤ 250 ms |
| Recording (24 frames, default config) | ~6 s frozen UI | UI stays animating |
| glyphSet switch | 174 ms ✓ | ≤ 200 ms |
| Slider scrub (cols 60→400 over 1s) | 60 reallocs | ≤ 5 reallocs |

Each row gets measured pre/post; results land in `tests/PERF-BASELINE.md`.

---

## Stretch goals (NOT in this plan)

These are larger and can wait until after the v0.1 polish lands:

- **F2C — drawText sprite atlas** (1 day, ~5× speedup on grid stage)
- **OffscreenCanvas + Worker for postprocess** (3 days, ~2-3× on multi-stage configs)
- **WebGL atlas renderer** (1+ week, ~10× and unlocks 60fps live)
- **WASM SIMD for linearizeToLuminance** (premature; per-pixel work is currently <1% of frame)

These are tracked in `OPTIMIZATION-AUDIT-CARMACK.md` § "What NOT to do."

---

## Production quality bar

The user explicitly called this out: *"We are making software for production level it must run smoothly and correctly."* That has two clauses:

**Smoothly** = no freezes, no jank, no surprises during normal use:
- ✅ Frame time consistent (no allocation-induced GC spikes — F3)
- ✅ No multi-second freezes on any UI action (F1 done; F6 next; Phase 1 next)
- ✅ Slider scrub doesn't hitch (F7 next)
- ✅ Recording doesn't freeze UI (Phase 1 next)

**Correctly** = behaviorally right, not just fast:
- ✅ Every config toggle visibly applies on next frame (Phase 0.5 + F1 done)
- ✅ No stale cache effects (F1 fix path verified)
- ✅ No silent failures during recording (Phase 1's state machine handles drop-frames explicitly)
- ✅ Output deterministic given config + seed (recording state machine)

These are the bars. The plan above hits both.

---

## Document genealogy

This plan supersedes:
- `OPTIMIZATION-PLAN.md` (5-phase by symptom, conceptually subsumed here)
- `OPTIMIZATION-AUDIT-CARMACK.md` (10 findings, all incorporated above)

Both prior docs are kept in the repo for git-history reference but should be read AS HISTORY, not as live work plans. This file is the live one.
