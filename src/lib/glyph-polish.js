/* glyph-polish.js — cross-wave polish primitives.
 *
 * Additional colors, selections, and topology-aware output for richer
 * aesthetic coverage across the system.
 */

(function () {
  'use strict';

  if (!window.GlyphGrid || !window.GlyphGrid.runtime) return;
  const rt = window.GlyphGrid.runtime;

  /* ====================================================================
     Palettes (subset of legacy palettes, exposed here for pipeline use).
     Users can override via opts.palette = [{hex}, ...] or a named key.
     ==================================================================== */

  const PALETTES = {
    lovespark:    ['#FFB7C5', '#E91E8C', '#7FFFD4', '#FFFFFF'],
    phosphor:     ['#001100', '#33FF66', '#88FFAA'],
    amber:        ['#1A0D00', '#FFB000', '#FFD27F'],
    duneCinema:   ['#2A0E01', '#8A4B12', '#D28448', '#F4D29A', '#FFF4D9'],
    synthwave:    ['#1B0B3C', '#D726A8', '#FF6AD5', '#8E44FF', '#00F0FF'],
    bauhaus:      ['#F2EBDA', '#D62828', '#1D3557', '#F4C430'],
    ansi16:       ['#000','#800','#080','#880','#008','#808','#088','#ccc',
                   '#888','#f00','#0f0','#ff0','#00f','#f0f','#0ff','#fff'],
    'solar-flare':['#0A0020', '#3D1A47', '#C73E1D', '#F4A261', '#FCDC8F'],
  };

  function hexToRgb(hex) {
    const m = hex.replace('#', '');
    const full = m.length === 3
      ? m.split('').map(function (c) { return c + c; }).join('')
      : m;
    return [
      parseInt(full.slice(0, 2), 16),
      parseInt(full.slice(2, 4), 16),
      parseInt(full.slice(4, 6), 16),
    ];
  }

  function resolvePalette(arg) {
    if (Array.isArray(arg)) return arg.map(hexToRgb);
    if (typeof arg === 'string' && arg in PALETTES) return PALETTES[arg].map(hexToRgb);
    return PALETTES.lovespark.map(hexToRgb);
  }

  /* ====================================================================
     color.ramp-palette — brightness → palette-indexed color.
     Brightest 10% maps to the lightest palette color; dark → dark.
     ==================================================================== */

  rt.register('color', 'ramp-palette', function (sel, ctx, stage) {
    const cs = sel.cellSignal;
    const opts = stage.opts || {};
    const pal = resolvePalette(opts.palette || ctx.config.palette || 'lovespark');
    const last = pal.length - 1;
    const N = cs.cols * cs.rows;
    const rgb = new Uint8Array(N * 3);
    const lum = cs.buf.lum;
    const gamma = (opts.gamma != null) ? opts.gamma : 1.0;
    for (let i = 0; i < N; i++) {
      let v = lum[i]; if (v < 0) v = 0; else if (v > 1) v = 1;
      if (gamma !== 1.0) v = Math.pow(v, gamma);
      const idx = Math.min(last, Math.max(0, Math.round(v * last)));
      const c = pal[idx];
      const k = i * 3;
      rgb[k] = c[0]; rgb[k + 1] = c[1]; rgb[k + 2] = c[2];
    }
    return Object.assign({}, sel, { rgb: rgb });
  }, { label: 'Brightness → palette-indexed color' });

  /* ====================================================================
     color.palette-morph — time-animated palette interpolation across two
     palette sets. Colors lerp between A and B based on sin(ctx.t / period).
     ==================================================================== */

  rt.register('color', 'palette-morph', function (sel, ctx, stage) {
    const cs = sel.cellSignal;
    const opts = stage.opts || {};
    const palA = resolvePalette(opts.paletteA || opts.a || 'lovespark');
    const palB = resolvePalette(opts.paletteB || opts.b || 'phosphor');
    const period = opts.period || 4.0;
    const u = 0.5 + 0.5 * Math.sin((ctx.t / period) * Math.PI * 2);
    const n = Math.min(palA.length, palB.length);
    const N = cs.cols * cs.rows;
    const rgb = new Uint8Array(N * 3);
    const lum = cs.buf.lum;
    const last = n - 1;
    for (let i = 0; i < N; i++) {
      let v = lum[i]; if (v < 0) v = 0; else if (v > 1) v = 1;
      const idx = Math.min(last, Math.max(0, Math.round(v * last)));
      const cA = palA[idx], cB = palB[idx];
      const k = i * 3;
      rgb[k]     = cA[0] + (cB[0] - cA[0]) * u;
      rgb[k + 1] = cA[1] + (cB[1] - cA[1]) * u;
      rgb[k + 2] = cA[2] + (cB[2] - cA[2]) * u;
    }
    return Object.assign({}, sel, { rgb: rgb });
  }, { label: 'Time-animated palette morph between two palettes' });

  /* ====================================================================
     color.gradient-stops — OKLab-smooth gradient mapped by lum.
     Accepts opts.stops = [[0.0, '#000'], [0.5, '#F00'], [1.0, '#FF0']].
     Simple RGB lerp for now (OKLab would require conversion — skip unless
     the existing glyph-palette-morph module is in scope).
     ==================================================================== */

  rt.register('color', 'gradient-stops', function (sel, ctx, stage) {
    const cs = sel.cellSignal;
    const opts = stage.opts || {};
    const stops = (opts.stops || [[0, '#000'], [1, '#fff']]).map(function (s) {
      return [s[0], hexToRgb(s[1])];
    }).sort(function (a, b) { return a[0] - b[0]; });
    const N = cs.cols * cs.rows;
    const rgb = new Uint8Array(N * 3);
    const lum = cs.buf.lum;
    for (let i = 0; i < N; i++) {
      let v = lum[i]; if (v < 0) v = 0; else if (v > 1) v = 1;
      /* Find adjacent stops. */
      let aI = 0;
      for (let s = 0; s < stops.length - 1; s++) { if (v >= stops[s][0]) aI = s; }
      const [t0, c0] = stops[aI];
      const [t1, c1] = stops[Math.min(aI + 1, stops.length - 1)];
      const u = t1 > t0 ? (v - t0) / (t1 - t0) : 0;
      const k = i * 3;
      rgb[k]     = c0[0] + (c1[0] - c0[0]) * u;
      rgb[k + 1] = c0[1] + (c1[1] - c0[1]) * u;
      rgb[k + 2] = c0[2] + (c1[2] - c0[2]) * u;
    }
    return Object.assign({}, sel, { rgb: rgb });
  }, { label: 'Gradient-stops colorization' });

  /* ====================================================================
     output.topology-aware-canvas — paint glyphs at their declared (cellX,
     cellY) positions (non-uniform grids use this; uniform grids fall back
     to the same code path).
     ==================================================================== */

  rt.register('output', 'topology-aware-canvas', function (colored, ctx, stage) {
    const canvas = ctx.canvas
      || (window.GlyphGrid.v1 && window.GlyphGrid.v1.outputCanvas && window.GlyphGrid.v1.outputCanvas())
      || document.querySelector('canvas');
    if (!canvas) throw new window.GlyphGrid.GlyphGridError('STAGE_MISSING_HOST', 'topology-aware-canvas', 'output', { message: 'No canvas.' });
    const cs = colored.cellSignal;
    const opts = stage.opts || {};
    const c2d = canvas.getContext('2d');
    if (opts.clear !== false) {
      c2d.fillStyle = opts.bg || '#000';
      c2d.fillRect(0, 0, canvas.width, canvas.height);
    }
    /* Determine glyph size from topology. */
    const defaultSize = Math.max(4, Math.floor(canvas.height / (cs.rows || 40) * 1.1));
    const fontPx = opts.fontSize || defaultSize;
    c2d.font = fontPx + 'px ' + (opts.fontFamily || 'monospace');
    c2d.textAlign = 'center';
    c2d.textBaseline = 'middle';
    const N = cs.cols * cs.rows;
    const ramp = colored.ramp;
    const rgb = colored.rgb;
    const glyphs = colored.glyphs;
    const useCellXY = cs.cellX && cs.cellY;
    const cellW = canvas.width / cs.cols;
    const cellH = canvas.height / (cs.rows || 1);
    /* If the topology reports its source-field dims, scale cellX/cellY from
       source-field space to canvas space. Otherwise assume 1:1. */
    const srcW = cs.sourceW || canvas.width;
    const srcH = cs.sourceH || canvas.height;
    const sx = canvas.width / srcW;
    const sy = canvas.height / srcH;
    for (let i = 0; i < N; i++) {
      const gidx = glyphs[i];
      const ch = ramp ? ramp[gidx] : null;
      if (!ch || ch === ' ') continue;
      const k = i * 3;
      c2d.fillStyle = 'rgb(' + rgb[k] + ',' + rgb[k + 1] + ',' + rgb[k + 2] + ')';
      let x, y;
      if (useCellXY) {
        x = cs.cellX[i] * sx; y = cs.cellY[i] * sy;
      } else {
        const cy = (i / cs.cols) | 0;
        const cx = i - cy * cs.cols;
        x = (cx + 0.5) * cellW; y = (cy + 0.5) * cellH;
      }
      c2d.fillText(ch, x, y);
    }
    return colored;
  }, { label: 'Topology-aware glyph painter (honors cellX/cellY)' });

  /* ====================================================================
     selection.dither — threshold with Bayer matrix (per-cell, like dither
     on the image but on a cell grid). Good cheap ordered-look primitive.
     ==================================================================== */

  const BAYER4 = new Float32Array([
    0, 8, 2, 10,
    12, 4, 14, 6,
    3, 11, 1, 9,
    15, 7, 13, 5,
  ]).map(function (v) { return v / 16; });

  rt.register('selection', 'dither-brightness', function (cs, ctx, stage) {
    rt.assert.cellSignal(cs, ['lum'], 'dither-brightness', 'selection');
    const opts = stage.opts || {};
    const rampStr = opts.ramp || '  ..::--==++**##%%@@';
    const ramp = Array.from(rampStr);
    const last = ramp.length - 1;
    const lum = cs.buf.lum;
    const N = cs.cols * cs.rows;
    const glyphs = new Uint16Array(N);
    for (let i = 0; i < N; i++) {
      const cy = (i / cs.cols) | 0;
      const cx = i - cy * cs.cols;
      const bayer = BAYER4[(cy & 3) * 4 + (cx & 3)];
      const v = Math.max(0, Math.min(1, lum[i] + (bayer - 0.5) * 0.1));
      glyphs[i] = Math.round(v * last);
    }
    return { cellSignal: cs, glyphs: glyphs, ramp: ramp, atlas: null };
  }, { label: 'Bayer-dithered brightness selection', requires: ['lum'] });

  /* ====================================================================
     transform.invert — invert the lum channel. Trivial but useful.
     ==================================================================== */

  rt.register('transform', 'invert', function (field, ctx, stage) {
    rt.assert.field(field, ['lum'], 'invert', 'transform');
    const lum = field.buf.lum;
    for (let i = 0; i < lum.length; i++) lum[i] = 1 - lum[i];
    if (field.channels.has('r')) {
      const r = field.buf.r, g = field.buf.g, b = field.buf.b;
      for (let i = 0; i < r.length; i++) {
        r[i] = 1 - r[i]; g[i] = 1 - g[i]; b[i] = 1 - b[i];
      }
    }
    return field;
  }, {
    label: 'Invert luminance (and RGB if present)',
    requires: ['lum'], produces: ['lum'], mutatesInput: true,
  });

  /* ====================================================================
     transform.gamma — power-curve on lum.
     ==================================================================== */

  rt.register('transform', 'gamma', function (field, ctx, stage) {
    rt.assert.field(field, ['lum'], 'gamma', 'transform');
    const gamma = (stage.opts && stage.opts.value) || 2.2;
    const lum = field.buf.lum;
    for (let i = 0; i < lum.length; i++) {
      lum[i] = Math.pow(Math.max(0, lum[i]), gamma);
    }
    return field;
  }, {
    label: 'Gamma (power) curve on lum',
    requires: ['lum'], produces: ['lum'], mutatesInput: true,
  });

  /* Expose PALETTES for piece authors. */
  window.GlyphGrid.palettes = Object.freeze(PALETTES);

})();
