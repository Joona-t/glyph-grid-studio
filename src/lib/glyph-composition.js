/* glyph-composition.js — Wave 3 composition primitives.
 *
 * Composition primitives sit in the pipeline and run OTHER pipelines as
 * sub-computations, merging their results. They operate at two levels:
 *   1) cell level — composition.mask dispatches cells between two sub-pipes
 *   2) pixel level — composition.blend & composition.overlay composite
 *      rendered canvases with pixel rules
 *   3) time level — composition.temporal-sequence picks which sub-pipeline
 *      runs based on `ctx.t`
 *
 * All composition primitives receive the current signal, run a side pipeline
 * as needed, and return a merged signal. Sub-pipelines are passed via
 * stage.opts.pipeline (or pipelines[]) and are lint'd at registration time
 * by the consuming piece.
 */

(function () {
  'use strict';

  if (!window.GlyphGrid || !window.GlyphGrid.runtime) {
    console.warn('[glyph-composition] runtime not loaded; skipping.');
    return;
  }
  const rt = window.GlyphGrid.runtime;

  /* ====================================================================
     Pixel-level blend rules (operate on ImageData.data byte arrays).
     ==================================================================== */

  const BLEND_RULES = {
    'alpha-over': function (a, b, out, opts) {
      /* Standard porter-duff A-over-B with per-pixel alpha from A. */
      const mix = (opts && opts.mix != null) ? opts.mix : 1.0;
      const N = a.length;
      for (let i = 0; i < N; i += 4) {
        const aA = (a[i + 3] / 255) * mix;
        const iA = 1 - aA;
        out[i]     = (a[i] * aA + b[i] * iA) | 0;
        out[i + 1] = (a[i + 1] * aA + b[i + 1] * iA) | 0;
        out[i + 2] = (a[i + 2] * aA + b[i + 2] * iA) | 0;
        out[i + 3] = Math.max(a[i + 3], b[i + 3]);
      }
    },
    'additive': function (a, b, out) {
      const N = a.length;
      for (let i = 0; i < N; i += 4) {
        out[i]     = Math.min(255, a[i] + b[i]);
        out[i + 1] = Math.min(255, a[i + 1] + b[i + 1]);
        out[i + 2] = Math.min(255, a[i + 2] + b[i + 2]);
        out[i + 3] = Math.max(a[i + 3], b[i + 3]);
      }
    },
    'multiply': function (a, b, out) {
      const N = a.length;
      for (let i = 0; i < N; i += 4) {
        out[i]     = ((a[i] * b[i]) / 255) | 0;
        out[i + 1] = ((a[i + 1] * b[i + 1]) / 255) | 0;
        out[i + 2] = ((a[i + 2] * b[i + 2]) / 255) | 0;
        out[i + 3] = Math.max(a[i + 3], b[i + 3]);
      }
    },
    'screen': function (a, b, out) {
      const N = a.length;
      for (let i = 0; i < N; i += 4) {
        out[i]     = 255 - (((255 - a[i])     * (255 - b[i]))     / 255 | 0);
        out[i + 1] = 255 - (((255 - a[i + 1]) * (255 - b[i + 1])) / 255 | 0);
        out[i + 2] = 255 - (((255 - a[i + 2]) * (255 - b[i + 2])) / 255 | 0);
        out[i + 3] = Math.max(a[i + 3], b[i + 3]);
      }
    },
    'difference': function (a, b, out) {
      const N = a.length;
      for (let i = 0; i < N; i += 4) {
        out[i]     = Math.abs(a[i]     - b[i]);
        out[i + 1] = Math.abs(a[i + 1] - b[i + 1]);
        out[i + 2] = Math.abs(a[i + 2] - b[i + 2]);
        out[i + 3] = Math.max(a[i + 3], b[i + 3]);
      }
    },
    'lighten': function (a, b, out) {
      const N = a.length;
      for (let i = 0; i < N; i += 4) {
        out[i]     = Math.max(a[i],     b[i]);
        out[i + 1] = Math.max(a[i + 1], b[i + 1]);
        out[i + 2] = Math.max(a[i + 2], b[i + 2]);
        out[i + 3] = Math.max(a[i + 3], b[i + 3]);
      }
    },
    'darken': function (a, b, out) {
      const N = a.length;
      for (let i = 0; i < N; i += 4) {
        out[i]     = Math.min(a[i],     b[i]);
        out[i + 1] = Math.min(a[i + 1], b[i + 1]);
        out[i + 2] = Math.min(a[i + 2], b[i + 2]);
        out[i + 3] = Math.max(a[i + 3], b[i + 3]);
      }
    },
  };

  function getCanvas(ctx) {
    return ctx.canvas
      || (window.GlyphGrid.v1 && window.GlyphGrid.v1.outputCanvas && window.GlyphGrid.v1.outputCanvas())
      || document.querySelector('canvas');
  }

  /* If the upstream signal is a colored-but-not-yet-painted cellSignal,
     paint it to the target canvas before compositing. This lets pieces
     write `...color(X).composition(Y).output('canvas')` without needing
     an intermediate output.canvas call. No-op if signal already painted
     (detected via opts.alreadyPainted flag or missing rgb/glyphs). */
  function ensurePainted(canvas, signal) {
    if (!canvas || !signal || signal.alreadyPainted) return;
    if (!signal.rgb || !signal.glyphs || !signal.cellSignal) return;
    const cs = signal.cellSignal;
    const c2d = canvas.getContext('2d');
    const cellW = canvas.width / cs.cols;
    const cellH = canvas.height / (cs.rows || 1);
    const defaultSize = Math.max(4, Math.floor(cellH));
    c2d.fillStyle = '#000';
    c2d.fillRect(0, 0, canvas.width, canvas.height);
    c2d.font = defaultSize + 'px monospace';
    c2d.textAlign = 'center'; c2d.textBaseline = 'middle';
    const useXY = cs.cellX && cs.cellY;
    const rgb = signal.rgb, glyphs = signal.glyphs, ramp = signal.ramp;
    for (let i = 0; i < glyphs.length; i++) {
      const ch = ramp ? ramp[glyphs[i]] : null;
      if (!ch || ch === ' ') continue;
      const k = i * 3;
      c2d.fillStyle = 'rgb(' + rgb[k] + ',' + rgb[k + 1] + ',' + rgb[k + 2] + ')';
      let x, y;
      if (useXY) { x = cs.cellX[i]; y = cs.cellY[i]; }
      else {
        const cy = (i / cs.cols) | 0;
        const cx = i - cy * cs.cols;
        x = (cx + 0.5) * cellW; y = (cy + 0.5) * cellH;
      }
      c2d.fillText(ch, x, y);
    }
    signal.alreadyPainted = true;
  }

  /* ====================================================================
     composition.blend — run a side pipeline, composite with current canvas.

     Opts:
       pipeline: Pipeline  — the side pipeline (required)
       rule: string        — one of BLEND_RULES keys (default 'additive')
       mix: number         — 0..1 opacity of side (some rules honor this)

     Reads the current canvas as imageData A, renders the side pipeline to a
     hidden offscreen canvas as B, blends into A, writes back to the canvas.
     ==================================================================== */

  rt.register('composition', 'blend', async function (signal, ctx, stage) {
    const opts = stage.opts || {};
    if (!opts.pipeline) {
      throw new window.GlyphGrid.GlyphGridError('STAGE_BAD_OPTS', 'blend', 'composition', {
        message: 'composition.blend requires opts.pipeline (the side Pipeline).',
      });
    }
    const rule = opts.rule || 'additive';
    const blendFn = BLEND_RULES[rule];
    if (!blendFn) {
      throw new window.GlyphGrid.GlyphGridError('STAGE_BAD_OPTS', 'blend', 'composition', {
        message: 'Unknown blend rule "' + rule + '". Valid: ' + Object.keys(BLEND_RULES).join(', '),
      });
    }

    const mainCanvas = getCanvas(ctx);
    if (!mainCanvas) throw new window.GlyphGrid.GlyphGridError('STAGE_MISSING_HOST', 'blend', 'composition', {
      message: 'No main canvas available.',
    });
    ensurePainted(mainCanvas, signal);
    const mainCtx2d = mainCanvas.getContext('2d', { willReadFrequently: true });
    const a = mainCtx2d.getImageData(0, 0, mainCanvas.width, mainCanvas.height);

    /* Run side pipeline to an offscreen canvas. */
    const side = document.createElement('canvas');
    side.width = mainCanvas.width;
    side.height = mainCanvas.height;
    const sideCtx = Object.assign({}, ctx, { canvas: side });
    await rt.run(opts.pipeline, sideCtx);
    const sideCtx2d = side.getContext('2d', { willReadFrequently: true });
    const b = sideCtx2d.getImageData(0, 0, side.width, side.height);

    /* Blend into a new buffer, write back. */
    const out = new Uint8ClampedArray(a.data.length);
    blendFn(a.data, b.data, out, opts);
    mainCtx2d.putImageData(new ImageData(out, mainCanvas.width, mainCanvas.height), 0, 0);

    return Object.assign({}, signal, { blended: { rule: rule } });
  }, { label: 'Composite a side pipeline into the current canvas' });

  /* ====================================================================
     composition.overlay — like blend but skip pixels where side alpha == 0.

     Simpler semantics: draw side on top where it has content.
     ==================================================================== */

  rt.register('composition', 'overlay', async function (signal, ctx, stage) {
    const opts = stage.opts || {};
    if (!opts.pipeline) {
      throw new window.GlyphGrid.GlyphGridError('STAGE_BAD_OPTS', 'overlay', 'composition', {
        message: 'composition.overlay requires opts.pipeline.',
      });
    }
    const mainCanvas = getCanvas(ctx);
    if (!mainCanvas) throw new window.GlyphGrid.GlyphGridError('STAGE_MISSING_HOST', 'overlay', 'composition', { message: 'No main canvas.' });
    ensurePainted(mainCanvas, signal);

    const side = document.createElement('canvas');
    side.width = mainCanvas.width; side.height = mainCanvas.height;
    await rt.run(opts.pipeline, Object.assign({}, ctx, { canvas: side }));

    /* Native draw with source-over composite operation (faster than pixel
       loop for opaque glyph output). */
    const m = mainCanvas.getContext('2d');
    m.save();
    m.globalAlpha = (opts.alpha != null) ? opts.alpha : 1.0;
    m.globalCompositeOperation = opts.compositeOperation || 'source-over';
    m.drawImage(side, 0, 0);
    m.restore();
    return Object.assign({}, signal, { overlaid: true });
  }, { label: 'Paint side pipeline on top of current canvas' });

  /* ====================================================================
     composition.mask — cell-level dispatch between two sub-pipelines.

     Opts:
       pipelineA: Pipeline — runs on cells where mask channel == 1
       pipelineB: Pipeline — runs on cells where mask == 0 (optional; if
                             absent, masked-out cells stay empty)
       maskFn: (col, row, cols, rows) => 0|1 — per-cell mask function
                                               (alternative to upstream mask)

     This is the simpler composition: we don't literally run two grid passes.
     Instead the consumer pipelines render separately and we merge by cell
     index based on the mask. Each sub-pipeline should terminate at output
     (e.g., canvas) so the merge can read their rendered outputs.
     ==================================================================== */

  rt.register('composition', 'mask', async function (signal, ctx, stage) {
    const opts = stage.opts || {};
    if (!opts.pipelineA && !opts.pipelineB) {
      throw new window.GlyphGrid.GlyphGridError('STAGE_BAD_OPTS', 'mask', 'composition', {
        message: 'composition.mask requires at least pipelineA.',
      });
    }
    const mainCanvas = getCanvas(ctx);
    if (!mainCanvas) throw new window.GlyphGrid.GlyphGridError('STAGE_MISSING_HOST', 'mask', 'composition', { message: 'No main canvas.' });
    const w = mainCanvas.width, h = mainCanvas.height;

    const aCanvas = document.createElement('canvas');
    aCanvas.width = w; aCanvas.height = h;
    if (opts.pipelineA) await rt.run(opts.pipelineA, Object.assign({}, ctx, { canvas: aCanvas }));

    let bCanvas = null;
    if (opts.pipelineB) {
      bCanvas = document.createElement('canvas');
      bCanvas.width = w; bCanvas.height = h;
      await rt.run(opts.pipelineB, Object.assign({}, ctx, { canvas: bCanvas }));
    }

    /* Build mask buffer. */
    const cols = (signal && signal.cellSignal && signal.cellSignal.cols) || (ctx.config.grid && ctx.config.grid.cols) || 80;
    const rows = (signal && signal.cellSignal && signal.cellSignal.rows) || (ctx.config.grid && ctx.config.grid.rows) || 80;
    const cellW = w / cols, cellH = h / rows;

    let maskFn = opts.maskFn;
    if (!maskFn && signal && signal.cellSignal && signal.cellSignal.channels.has('mask')) {
      const m = signal.cellSignal.buf.mask;
      maskFn = function (cx, cy) { return m[cy * cols + cx] > 0.5 ? 1 : 0; };
    }
    if (!maskFn) {
      /* Default: left-half A, right-half B. */
      maskFn = function (cx, _cy) { return cx < cols / 2 ? 1 : 0; };
    }

    /* Merge per-cell via drawImage rectangles — fast, avoids pixel loop. */
    const m2d = mainCanvas.getContext('2d');
    m2d.clearRect(0, 0, w, h);
    for (let cy = 0; cy < rows; cy++) {
      for (let cx = 0; cx < cols; cx++) {
        const src = maskFn(cx, cy, cols, rows) === 1 ? aCanvas : bCanvas;
        if (!src) continue;
        const x = cx * cellW, y = cy * cellH;
        m2d.drawImage(src,
          x, y, cellW, cellH,  /* src rect */
          x, y, cellW, cellH); /* dst rect */
      }
    }
    return Object.assign({}, signal, { masked: true });
  }, { label: 'Cell-level mask dispatch between two sub-pipelines' });

  /* ====================================================================
     composition.temporal-sequence — pick pipeline by ctx.t.

     Opts:
       keyframes: [{ t: number, pipeline: Pipeline }]
       crossfade: number (seconds, default 0) — linear blend between adjacent
                  pipelines when t is within crossfade of a boundary
     ==================================================================== */

  rt.register('composition', 'temporal-sequence', async function (signal, ctx, stage) {
    const opts = stage.opts || {};
    if (!opts.keyframes || !opts.keyframes.length) {
      throw new window.GlyphGrid.GlyphGridError('STAGE_BAD_OPTS', 'temporal-sequence', 'composition', {
        message: 'composition.temporal-sequence requires opts.keyframes = [{t, pipeline}].',
      });
    }
    const crossfade = opts.crossfade || 0;
    /* Find active keyframe(s) for ctx.t. */
    const kfs = opts.keyframes.slice().sort(function (a, b) { return a.t - b.t; });
    const t = ctx.t;
    let active = kfs[0];
    for (const kf of kfs) { if (kf.t <= t) active = kf; else break; }

    const mainCanvas = getCanvas(ctx);
    if (!mainCanvas) throw new window.GlyphGrid.GlyphGridError('STAGE_MISSING_HOST', 'temporal-sequence', 'composition', { message: 'No main canvas.' });
    const w = mainCanvas.width, h = mainCanvas.height;

    const aCanvas = document.createElement('canvas');
    aCanvas.width = w; aCanvas.height = h;
    await rt.run(active.pipeline, Object.assign({}, ctx, { canvas: aCanvas }));

    const m = mainCanvas.getContext('2d');
    m.clearRect(0, 0, w, h);
    m.drawImage(aCanvas, 0, 0);

    /* Optional crossfade with the next keyframe. */
    if (crossfade > 0) {
      const next = kfs.find(function (kf) { return kf.t > active.t; });
      if (next && (t > next.t - crossfade)) {
        const u = (t - (next.t - crossfade)) / crossfade;  /* 0..1 */
        const bCanvas = document.createElement('canvas');
        bCanvas.width = w; bCanvas.height = h;
        await rt.run(next.pipeline, Object.assign({}, ctx, { canvas: bCanvas }));
        m.save();
        m.globalAlpha = Math.max(0, Math.min(1, u));
        m.drawImage(bCanvas, 0, 0);
        m.restore();
      }
    }
    return Object.assign({}, signal, { temporal: { active: active.t } });
  }, { label: 'Run one of N sub-pipelines chosen by ctx.t, with optional crossfade' });

  /* ====================================================================
     Expose blend rules for user introspection.
     ==================================================================== */

  window.GlyphGrid.composition = Object.freeze({
    BLEND_RULES: Object.freeze(Object.assign({}, BLEND_RULES)),
    blendRules: function () { return Object.keys(BLEND_RULES); },
  });

})();
