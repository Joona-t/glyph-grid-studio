/* glyph-primitives.js — Wave 1 minimal primitive set.
 *
 * Wraps existing libs + a synthetic source into registered primitives for the
 * runtime. This file is the bridge from "scattered helper functions" to
 * "axis-classified, composable primitives". Wave 2+ will add the full set.
 *
 * Minimum proof-of-life set:
 *   source.noise           — synthetic seeded noise field (no p5 needed)
 *   source.from-scene      — legacy p5 scene adapter (requires GlyphGrid.v1.*)
 *   transform.xdog         — wraps glyph-xdog.js
 *   sampling.uniform-grid  — mean-per-cell downsample
 *   selection.brightness   — pick ramp glyph by cell luminance
 *   color.preserve         — per-cell RGB pass-through
 *   output.canvas          — paint cells to a 2D canvas (requires GlyphGrid.v1.*)
 *   output.void            — no-op terminal (harness use)
 *
 * Signatures (see runtime doc): each primitive is (signal, ctx, stage) → signal.
 * signal shape depends on axis:
 *   after source        → Field { w, h, channels, buf, _poolTag? }
 *   after transform     → Field
 *   after sampling      → CellSignal { cols, rows, channels, buf }
 *   after selection     → { cellSignal, glyphs: Uint16Array, ramp?, atlas? }
 *   after color         → { imageData | canvas painted, glyphRender: {...} }
 *   after postProcess   → same shape as color
 *   after output        → whatever the output returns (Promise, void, etc.)
 */

(function () {
  'use strict';

  if (!window.GlyphGrid || !window.GlyphGrid.runtime) {
    console.warn('[glyph-primitives] glyph-runtime.js must load first; skipping registrations.');
    return;
  }
  const rt = window.GlyphGrid.runtime;

  /* ====================================================================
     source.noise — hash-seeded per-pixel noise. No dependencies.
     ==================================================================== */

  rt.register('source', 'noise', function (_in, ctx, stage) {
    const w = (stage.opts && stage.opts.w) || ctx.config.canvas && ctx.config.canvas.w || 256;
    const h = (stage.opts && stage.opts.h) || ctx.config.canvas && ctx.config.canvas.h || 256;
    const scale = (stage.opts && stage.opts.scale) || 1.0;
    const field = ctx.pool.acquireField('gg:source-noise-lum', w, h);
    const lum = field.buf.lum;
    const hash = rt.hash32;
    const s = ctx.seed;
    const fi = ctx.frameIdx;
    for (let y = 0; y < h; y++) {
      for (let x = 0; x < w; x++) {
        /* Two octaves of hash-based noise for some texture. */
        const h1 = hash(s, fi, (x * scale) | 0, (y * scale) | 0) / 4294967296;
        const h2 = hash(s, fi ^ 0x7f4a, (x * 2 * scale) | 0, (y * 2 * scale) | 0) / 4294967296;
        lum[y * w + x] = 0.6 * h1 + 0.4 * h2;
      }
    }
    return field;
  }, {
    label: 'Seeded 2-octave hash noise',
    produces: ['lum'],
    scratch: { fields: [{ tag: 'gg:source-noise-lum', channels: ['lum'] }] },
  });

  /* ====================================================================
     source.from-scene — legacy p5 scene adapter.

     Requires window.GlyphGrid.v1 to be populated by render.html setup with:
       - srcGraphics:  () => p5.Graphics          (the scene buffer)
       - loadPixels:   (g) => void                (calls g.loadPixels())
       - linearizeToLuminance: (rgbaBytes, dst, w, h) => void
     ==================================================================== */

  rt.register('source', 'from-scene', function (_in, ctx, stage) {
    const v1 = window.GlyphGrid && window.GlyphGrid.v1;
    if (!v1) {
      throw new window.GlyphGrid.GlyphGridError('STAGE_MISSING_HOST', 'from-scene', 'source', {
        message: 'GlyphGrid.v1 helpers not found. This primitive only runs inside render.html.',
      });
    }
    const scene = stage.opts && stage.opts.scene;
    if (typeof scene !== 'function') {
      throw new window.GlyphGrid.GlyphGridError('STAGE_BAD_OPTS', 'from-scene', 'source', {
        message: 'Expected opts.scene to be a function (scene fn).',
      });
    }
    const g = v1.srcGraphics();
    if (!g || typeof g.loadPixels !== 'function') {
      throw new window.GlyphGrid.GlyphGridError('STAGE_MISSING_HOST', 'from-scene', 'source', {
        message: 'GlyphGrid.v1.srcGraphics() did not return a p5.Graphics.',
      });
    }
    scene(g, ctx.t, ctx.config);
    g.loadPixels();
    const w = g.width, h = g.height;
    const field = ctx.pool.acquireField('gg:source-from-scene', w, h);
    const lum = field.buf.lum, r = field.buf.r, g_ = field.buf.g, b = field.buf.b;
    const px = g.pixels;
    if (v1.linearizeToLuminance) v1.linearizeToLuminance(px, lum, w, h);
    /* Populate r/g/b channels directly (linear sRGB? Leave as-is; caller can
       linearize if needed — v1 ColorRule code uses these as sRGB [0,1]). */
    const N = w * h;
    for (let i = 0; i < N; i++) {
      const p = i * 4;
      r[i]  = px[p]     / 255;
      g_[i] = px[p + 1] / 255;
      b[i]  = px[p + 2] / 255;
      if (!v1.linearizeToLuminance) {
        /* Fallback Rec.709 sRGB luminance if host didn't provide one. */
        lum[i] = 0.2126 * r[i] + 0.7152 * g_[i] + 0.0722 * b[i];
      }
    }
    return field;
  }, {
    label: 'Legacy p5 scene adapter',
    produces: ['lum', 'r', 'g', 'b'],
    scratch: { fields: [{ tag: 'gg:source-from-scene', channels: ['lum', 'r', 'g', 'b'] }] },
  });

  /* ====================================================================
     transform.xdog — wraps glyph-xdog.
     ==================================================================== */

  rt.register('transform', 'xdog', function (field, ctx, stage) {
    window.GlyphGrid.runtime.assert.field(field, ['lum'], 'xdog', 'transform');
    if (!window.GlyphGrid.xdog || typeof window.GlyphGrid.xdog.xdog !== 'function') {
      throw new window.GlyphGrid.GlyphGridError('STAGE_MISSING_HOST', 'xdog', 'transform', {
        message: 'GlyphGrid.xdog.xdog() not loaded. Ensure glyph-xdog.js <script> tag precedes this primitive registration.',
      });
    }
    const opts = Object.assign({ mode: 'xdog', sigma: 1.2, k: 1.6, tau: 0.98, phi: 40, epsilon: 0.003 }, stage.opts || {});
    const out = window.GlyphGrid.xdog.xdog(field.buf.lum, field.w, field.h, opts);
    /* xdog.xdog returns a Float32Array; wrap back into a Field (mutating in place OK). */
    field.buf.lum.set(out);
    return field;
  }, {
    label: 'XDoG prefilter (edge-preserving near-binary)',
    requires: ['lum'],
    produces: ['lum'],
    mutatesInput: true,
  });

  /* ====================================================================
     sampling.uniform-grid — mean luminance and mean RGB per cell.
     ==================================================================== */

  rt.register('sampling', 'uniform-grid', function (field, ctx, stage) {
    window.GlyphGrid.runtime.assert.field(field, ['lum'], 'uniform-grid', 'sampling');
    const cols = (stage.opts && stage.opts.cols) || (ctx.config.grid && ctx.config.grid.cols) || 80;
    const rows = (stage.opts && stage.opts.rows) || (ctx.config.grid && ctx.config.grid.rows) || 40;
    const wantRgb = field.channels.has('r');
    const chans = wantRgb ? ['lum', 'r', 'g', 'b'] : ['lum'];
    const cs = ctx.pool.acquireCellSignal('gg:sampling-uniform', cols, rows);
    const w = field.w, h = field.h;
    const cellW = w / cols, cellH = h / rows;
    const lum = field.buf.lum;
    const fr = wantRgb ? field.buf.r : null;
    const fg = wantRgb ? field.buf.g : null;
    const fb = wantRgb ? field.buf.b : null;
    const olum = cs.buf.lum;
    const or = wantRgb ? cs.buf.r : null;
    const og = wantRgb ? cs.buf.g : null;
    const ob = wantRgb ? cs.buf.b : null;
    for (let cy = 0; cy < rows; cy++) {
      const y0 = Math.floor(cy * cellH);
      const y1 = Math.min(h, Math.ceil((cy + 1) * cellH));
      for (let cx = 0; cx < cols; cx++) {
        const x0 = Math.floor(cx * cellW);
        const x1 = Math.min(w, Math.ceil((cx + 1) * cellW));
        let sL = 0, sR = 0, sG = 0, sB = 0, n = 0;
        for (let y = y0; y < y1; y++) {
          const row = y * w;
          for (let x = x0; x < x1; x++) {
            const i = row + x;
            sL += lum[i];
            if (wantRgb) { sR += fr[i]; sG += fg[i]; sB += fb[i]; }
            n++;
          }
        }
        const inv = n ? (1 / n) : 0;
        const ci = cy * cols + cx;
        olum[ci] = sL * inv;
        if (wantRgb) { or[ci] = sR * inv; og[ci] = sG * inv; ob[ci] = sB * inv; }
      }
    }
    /* Return a CellSignal with only the channels we populated. */
    const outChannels = new Set(chans);
    return { cols: cols, rows: rows, channels: outChannels, buf: cs.buf, _poolTag: cs._poolTag };
  }, {
    label: 'Uniform-grid mean downsample',
    requires: ['lum'],
    produces: ['lum', 'r', 'g', 'b'],
    scratch: { cellSignals: [{ tag: 'gg:sampling-uniform', channels: ['lum', 'r', 'g', 'b'] }] },
  });

  /* ====================================================================
     selection.brightness — ramp-based brightness selection.

     opts.ramp = array of strings/glyphs OR a ramp-name string.
     Default ramp: ' .:-=+*#%@' (10-step dense-light to dense-dark).
     ==================================================================== */

  const DEFAULT_RAMPS = {
    classic: ' .·:-=+*#%@',
    dense:   ' .,:;i1tfLCG08@',
    sparse:  ' .:+#@',
    unicode: ' ░▒▓█',
  };

  rt.register('selection', 'brightness', function (cs, ctx, stage) {
    window.GlyphGrid.runtime.assert.cellSignal(cs, ['lum'], 'brightness', 'selection');
    let rampSrc = (stage.opts && stage.opts.ramp) || 'classic';
    if (typeof rampSrc === 'string' && rampSrc in DEFAULT_RAMPS) rampSrc = DEFAULT_RAMPS[rampSrc];
    const ramp = (typeof rampSrc === 'string') ? Array.from(rampSrc) : rampSrc;
    const gamma = (stage.opts && stage.opts.gamma != null) ? stage.opts.gamma : 1.0;
    const N = cs.cols * cs.rows;
    const glyphs = new Uint16Array(N);
    const lum = cs.buf.lum;
    const last = ramp.length - 1;
    for (let i = 0; i < N; i++) {
      let v = lum[i];
      if (v < 0) v = 0; else if (v > 1) v = 1;
      if (gamma !== 1.0) v = Math.pow(v, gamma);
      glyphs[i] = Math.min(last, Math.max(0, Math.round(v * last)));
    }
    return { cellSignal: cs, glyphs: glyphs, ramp: ramp, atlas: null };
  }, {
    label: 'Brightness → ramp glyph selection',
    requires: ['lum'],
  });

  /* ====================================================================
     color.preserve — per-cell RGB pass-through (if RGB channels present).
     Otherwise emits a default amber.
     ==================================================================== */

  rt.register('color', 'preserve', function (sel, ctx, stage) {
    const cs = sel.cellSignal;
    const hasRgb = cs.channels.has('r');
    const N = cs.cols * cs.rows;
    const rgb = new Uint8Array(N * 3);
    if (hasRgb) {
      const fr = cs.buf.r, fg = cs.buf.g, fb = cs.buf.b;
      for (let i = 0; i < N; i++) {
        const k = i * 3;
        rgb[k]     = Math.max(0, Math.min(255, Math.round(fr[i] * 255)));
        rgb[k + 1] = Math.max(0, Math.min(255, Math.round(fg[i] * 255)));
        rgb[k + 2] = Math.max(0, Math.min(255, Math.round(fb[i] * 255)));
      }
    } else {
      /* Default amber (LoveSpark duneCinema-ish) scaled by brightness. */
      const lum = cs.buf.lum;
      const rC = 0xF4, gC = 0xD2, bC = 0x9A;
      for (let i = 0; i < N; i++) {
        const v = Math.max(0, Math.min(1, lum[i]));
        const k = i * 3;
        rgb[k]     = Math.round(rC * v);
        rgb[k + 1] = Math.round(gC * v);
        rgb[k + 2] = Math.round(bC * v);
      }
    }
    return Object.assign({}, sel, { rgb: rgb });
  }, {
    label: 'Preserve source per-cell RGB (or amber fallback)',
  });

  /* ====================================================================
     output.canvas — paint cells onto a 2D canvas.

     Requires:
       ctx.canvas (HTMLCanvasElement or OffscreenCanvas)
       — OR GlyphGrid.v1.outputCanvas() returning one.
     ==================================================================== */

  rt.register('output', 'canvas', function (colored, ctx, stage) {
    const canvas = ctx.canvas
      || (window.GlyphGrid && window.GlyphGrid.v1 && window.GlyphGrid.v1.outputCanvas && window.GlyphGrid.v1.outputCanvas());
    if (!canvas) {
      throw new window.GlyphGrid.GlyphGridError('STAGE_MISSING_HOST', 'canvas', 'output', {
        message: 'output.canvas needs ctx.canvas or GlyphGrid.v1.outputCanvas().',
      });
    }
    const c2d = canvas.getContext('2d');
    const cs = colored.cellSignal;
    const cellW = canvas.width / cs.cols;
    const cellH = canvas.height / cs.rows;
    const fontPx = (stage.opts && stage.opts.fontSize) || Math.max(4, Math.floor(cellH));
    c2d.save();
    if ((stage.opts && stage.opts.clear) !== false) {
      c2d.fillStyle = (stage.opts && stage.opts.bg) || '#000';
      c2d.fillRect(0, 0, canvas.width, canvas.height);
    }
    c2d.font = fontPx + 'px monospace';
    c2d.textAlign = 'center';
    c2d.textBaseline = 'middle';
    const rgb = colored.rgb;
    const glyphs = colored.glyphs;
    const ramp = colored.ramp;
    for (let cy = 0; cy < cs.rows; cy++) {
      const y = (cy + 0.5) * cellH;
      for (let cx = 0; cx < cs.cols; cx++) {
        const i = cy * cs.cols + cx;
        const k = i * 3;
        c2d.fillStyle = 'rgb(' + rgb[k] + ',' + rgb[k + 1] + ',' + rgb[k + 2] + ')';
        c2d.fillText(ramp[glyphs[i]], (cx + 0.5) * cellW, y);
      }
    }
    c2d.restore();
    return colored;
  }, {
    label: 'Paint cells to a 2D canvas',
  });

  /* ====================================================================
     output.void — no-op terminal for harness tests.
     Returns a summary of the signal so tests can assert on it.
     ==================================================================== */

  rt.register('output', 'void', function (signal, ctx, stage) {
    const summary = { ok: true, t: ctx.t, frameIdx: ctx.frameIdx };
    if (signal && signal.cellSignal) {
      summary.cellSignal = {
        cols: signal.cellSignal.cols,
        rows: signal.cellSignal.rows,
        channels: Array.from(signal.cellSignal.channels),
      };
      if (signal.glyphs) {
        /* Running min/max/mean of glyph indices. */
        let lo = Infinity, hi = -Infinity, sum = 0;
        for (let i = 0; i < signal.glyphs.length; i++) {
          const v = signal.glyphs[i];
          if (v < lo) lo = v; if (v > hi) hi = v; sum += v;
        }
        summary.glyphs = { min: lo, max: hi, mean: sum / signal.glyphs.length };
      }
    } else if (signal && typeof signal.w === 'number') {
      summary.field = { w: signal.w, h: signal.h, channels: Array.from(signal.channels) };
    }
    return summary;
  }, { label: 'No-op terminal (harness)' });

})();
