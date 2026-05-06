/* glyph-sampling.js — Wave 5 sampling topologies.
 *
 * Beyond the uniform rectangular grid: these topologies unlock distinct
 * artistic idioms. Each topology produces a CellSignal whose cells may be
 * non-rectangular; the sampling fn also populates cellX/cellY/cellW/cellH
 * metadata so downstream selection + color + output primitives know where
 * to paint.
 *
 * Registered:
 *   sampling.radial           concentric rings + angular spokes
 *   sampling.log-polar        radial with log-scaled rings (tunnel aesthetic)
 *   sampling.phyllotaxis      golden-angle spiral point distribution
 *   sampling.hexagonal        hex-packed cells
 *   sampling.voronoi          Voronoi tiling around N seeded sites
 *   sampling.adaptive         quadtree refinement where XDoG edge density is high
 */

(function () {
  'use strict';

  if (!window.GlyphGrid || !window.GlyphGrid.runtime) {
    console.warn('[glyph-sampling] runtime not loaded.');
    return;
  }
  const rt = window.GlyphGrid.runtime;

  /* Helper: copy mean RGB from a field into a cell buffer given rect. */
  function sampleRect(field, buf, i, x0, y0, x1, y1) {
    const w = field.w, h = field.h;
    x0 = Math.max(0, Math.floor(x0)); y0 = Math.max(0, Math.floor(y0));
    x1 = Math.min(w, Math.ceil(x1));  y1 = Math.min(h, Math.ceil(y1));
    const lum = field.buf.lum;
    const fr = field.buf.r, fg = field.buf.g, fb = field.buf.b;
    const wantRgb = !!fr;
    let sL = 0, sR = 0, sG = 0, sB = 0, n = 0;
    for (let y = y0; y < y1; y++) {
      const row = y * w;
      for (let x = x0; x < x1; x++) {
        const k = row + x;
        sL += lum[k];
        if (wantRgb) { sR += fr[k]; sG += fg[k]; sB += fb[k]; }
        n++;
      }
    }
    const inv = n ? 1 / n : 0;
    buf.lum[i] = sL * inv;
    if (wantRgb) {
      buf.r[i] = sR * inv;
      buf.g[i] = sG * inv;
      buf.b[i] = sB * inv;
    }
  }

  /* ====================================================================
     sampling.radial — rings × spokes grid.
     ==================================================================== */

  rt.register('sampling', 'radial', function (field, ctx, stage) {
    rt.assert.field(field, ['lum'], 'radial', 'sampling');
    const opts = stage.opts || {};
    const rings = opts.rings || 32;
    const spokes = opts.spokes || 96;
    const w = field.w, h = field.h;
    const cx = w / 2, cy = h / 2;
    const maxR = Math.min(w, h) * (opts.maxRadius || 0.48);
    const innerR = maxR * (opts.innerRadius || 0.0);
    const count = rings * spokes;
    const wantRgb = field.channels.has('r');

    const chans = wantRgb ? ['lum', 'r', 'g', 'b'] : ['lum'];
    const cs = ctx.pool.acquireCellSignal('gg:sampling-radial', spokes, rings);
    const cellX = new Float32Array(count);
    const cellY = new Float32Array(count);
    const cellR = new Float32Array(count);    /* radius of cell center */
    const cellA = new Float32Array(count);    /* angle of cell center */

    for (let r = 0; r < rings; r++) {
      const rInner = innerR + (r / rings) * (maxR - innerR);
      const rOuter = innerR + ((r + 1) / rings) * (maxR - innerR);
      const rMid = 0.5 * (rInner + rOuter);
      for (let s = 0; s < spokes; s++) {
        const a0 = (s / spokes) * Math.PI * 2;
        const a1 = ((s + 1) / spokes) * Math.PI * 2;
        const aMid = 0.5 * (a0 + a1);
        const i = r * spokes + s;
        cellX[i] = cx + Math.cos(aMid) * rMid;
        cellY[i] = cy + Math.sin(aMid) * rMid;
        cellR[i] = rMid; cellA[i] = aMid;
        /* Sample a small rect around the cell center — not perfectly area-matched
           but perceptually close at typical resolutions. */
        const rStep = (rOuter - rInner);
        const aStep = (a1 - a0);
        const boxHalf = Math.max(1, rStep * 0.5);
        const ax = cellX[i], ay = cellY[i];
        sampleRect(field, cs.buf, i,
          ax - boxHalf, ay - boxHalf,
          ax + boxHalf, ay + boxHalf);
        void aStep;
      }
    }
    return {
      cols: spokes, rows: rings,
      channels: new Set(chans), buf: cs.buf,
      sourceW: w, sourceH: h,
      cellX: cellX, cellY: cellY,
      topology: 'radial', radius: cellR, angle: cellA, center: [cx, cy],
    };
  }, {
    label: 'Radial rings × spokes sampling',
    requires: ['lum'],
    produces: ['lum', 'r', 'g', 'b'],
    scratch: { cellSignals: [{ tag: 'gg:sampling-radial', channels: ['lum', 'r', 'g', 'b'] }] },
  });

  /* ====================================================================
     sampling.log-polar — radial with log-scaled rings; tunnel aesthetic.
     ==================================================================== */

  rt.register('sampling', 'log-polar', function (field, ctx, stage) {
    rt.assert.field(field, ['lum'], 'log-polar', 'sampling');
    const opts = stage.opts || {};
    const rings = opts.rings || 40;
    const spokes = opts.spokes || 120;
    const w = field.w, h = field.h;
    const cx = w / 2, cy = h / 2;
    const maxR = Math.min(w, h) * (opts.maxRadius || 0.48);
    const minR = maxR * (opts.minRadius || 0.02);
    const count = rings * spokes;
    const wantRgb = field.channels.has('r');
    const chans = wantRgb ? ['lum', 'r', 'g', 'b'] : ['lum'];
    const cs = ctx.pool.acquireCellSignal('gg:sampling-logpolar', spokes, rings);
    const cellX = new Float32Array(count);
    const cellY = new Float32Array(count);
    const lnMin = Math.log(minR), lnMax = Math.log(maxR);
    for (let r = 0; r < rings; r++) {
      const ringR = Math.exp(lnMin + (lnMax - lnMin) * (r / (rings - 1)));
      for (let s = 0; s < spokes; s++) {
        const a = (s / spokes) * Math.PI * 2;
        const i = r * spokes + s;
        cellX[i] = cx + Math.cos(a) * ringR;
        cellY[i] = cy + Math.sin(a) * ringR;
        const boxHalf = Math.max(1, ringR * 0.03);
        sampleRect(field, cs.buf, i,
          cellX[i] - boxHalf, cellY[i] - boxHalf,
          cellX[i] + boxHalf, cellY[i] + boxHalf);
      }
    }
    return {
      cols: spokes, rows: rings,
      channels: new Set(chans), buf: cs.buf,
      sourceW: w, sourceH: h,
      cellX: cellX, cellY: cellY,
      topology: 'log-polar', center: [cx, cy],
    };
  }, {
    label: 'Log-polar sampling — logarithmic rings for tunnel/vortex aesthetic',
    requires: ['lum'],
    produces: ['lum', 'r', 'g', 'b'],
    scratch: { cellSignals: [{ tag: 'gg:sampling-logpolar', channels: ['lum', 'r', 'g', 'b'] }] },
  });

  /* ====================================================================
     sampling.phyllotaxis — golden-angle spiral point distribution.
     Natural / plant-like aesthetic. Cells are small squares around each
     spiral point.
     ==================================================================== */

  rt.register('sampling', 'phyllotaxis', function (field, ctx, stage) {
    rt.assert.field(field, ['lum'], 'phyllotaxis', 'sampling');
    const opts = stage.opts || {};
    const n = opts.count || 1200;
    const c = opts.c || 1.2;                /* spacing constant */
    const w = field.w, h = field.h;
    const cx = w / 2, cy = h / 2;
    const goldenAngle = Math.PI * (3 - Math.sqrt(5));  /* ≈ 137.5° in radians */
    const maxR = Math.min(w, h) * (opts.maxRadius || 0.48);
    const wantRgb = field.channels.has('r');
    const chans = wantRgb ? ['lum', 'r', 'g', 'b'] : ['lum'];
    const cs = ctx.pool.acquireCellSignal('gg:sampling-phyllo', n, 1);
    const cellX = new Float32Array(n);
    const cellY = new Float32Array(n);
    /* Pick scale so that the Nth point is at maxR. */
    const scale = maxR / (c * Math.sqrt(n));
    for (let i = 0; i < n; i++) {
      const r = c * scale * Math.sqrt(i + 1);
      const a = i * goldenAngle;
      cellX[i] = cx + Math.cos(a) * r;
      cellY[i] = cy + Math.sin(a) * r;
      const boxHalf = Math.max(1, scale * 0.75);
      sampleRect(field, cs.buf, i,
        cellX[i] - boxHalf, cellY[i] - boxHalf,
        cellX[i] + boxHalf, cellY[i] + boxHalf);
    }
    return {
      cols: n, rows: 1,
      channels: new Set(chans), buf: cs.buf,
      sourceW: w, sourceH: h,
      cellX: cellX, cellY: cellY,
      topology: 'phyllotaxis', pointSize: scale,
    };
  }, {
    label: 'Golden-angle spiral (Vogel phyllotaxis) point distribution',
    requires: ['lum'],
    produces: ['lum', 'r', 'g', 'b'],
    scratch: { cellSignals: [{ tag: 'gg:sampling-phyllo', channels: ['lum', 'r', 'g', 'b'] }] },
  });

  /* ====================================================================
     sampling.hexagonal — hex-packed cells.
     Rows alternate between offset starts; natural organic aesthetic.
     ==================================================================== */

  rt.register('sampling', 'hexagonal', function (field, ctx, stage) {
    rt.assert.field(field, ['lum'], 'hexagonal', 'sampling');
    const opts = stage.opts || {};
    const cols = opts.cols || 80;
    const w = field.w, h = field.h;
    const cellW = w / cols;
    const cellH = cellW * 0.866025;        /* sqrt(3)/2 */
    const rows = Math.ceil(h / cellH);
    const count = cols * rows;
    const wantRgb = field.channels.has('r');
    const chans = wantRgb ? ['lum', 'r', 'g', 'b'] : ['lum'];
    const cs = ctx.pool.acquireCellSignal('gg:sampling-hex', cols, rows);
    const cellX = new Float32Array(count);
    const cellY = new Float32Array(count);
    for (let y = 0; y < rows; y++) {
      const yCenter = (y + 0.5) * cellH;
      const xOffset = (y & 1) ? cellW * 0.5 : 0;
      for (let x = 0; x < cols; x++) {
        const xCenter = x * cellW + xOffset + cellW * 0.5;
        const i = y * cols + x;
        cellX[i] = xCenter; cellY[i] = yCenter;
        sampleRect(field, cs.buf, i,
          xCenter - cellW * 0.5, yCenter - cellH * 0.5,
          xCenter + cellW * 0.5, yCenter + cellH * 0.5);
      }
    }
    return {
      cols: cols, rows: rows,
      channels: new Set(chans), buf: cs.buf,
      sourceW: w, sourceH: h,
      cellX: cellX, cellY: cellY,
      topology: 'hex', cellW: cellW, cellH: cellH,
    };
  }, {
    label: 'Hex-packed tiling',
    requires: ['lum'],
    produces: ['lum', 'r', 'g', 'b'],
    scratch: { cellSignals: [{ tag: 'gg:sampling-hex', channels: ['lum', 'r', 'g', 'b'] }] },
  });

  /* ====================================================================
     sampling.voronoi — Voronoi around N seeded sites.

     Brute-force nearest-site assignment. For scenes using few sites
     (< 2000), this is plenty fast. For more, upgrade later.

     Opts:
       sites: count (default 300) — seeded via config.seed
       sites points: optional array of [x, y] normalized coords to replace
                     the deterministic seeding
     ==================================================================== */

  rt.register('sampling', 'voronoi', function (field, ctx, stage) {
    rt.assert.field(field, ['lum'], 'voronoi', 'sampling');
    const opts = stage.opts || {};
    const w = field.w, h = field.h;
    let siteXs, siteYs;
    const wantRgb = field.channels.has('r');
    if (Array.isArray(opts.points)) {
      const N = opts.points.length;
      siteXs = new Float32Array(N);
      siteYs = new Float32Array(N);
      for (let i = 0; i < N; i++) {
        siteXs[i] = opts.points[i][0] * w;
        siteYs[i] = opts.points[i][1] * h;
      }
    } else {
      const N = opts.sites || 300;
      siteXs = new Float32Array(N);
      siteYs = new Float32Array(N);
      const hash = rt.hash32;
      for (let i = 0; i < N; i++) {
        siteXs[i] = ((hash(ctx.seed, 101, i, 0) % 10000) / 10000) * w;
        siteYs[i] = ((hash(ctx.seed, 102, i, 0) % 10000) / 10000) * h;
      }
    }
    const N = siteXs.length;
    /* Accumulate mean signal per site via single raster pass over the field. */
    const chans = wantRgb ? ['lum', 'r', 'g', 'b'] : ['lum'];
    const cs = ctx.pool.acquireCellSignal('gg:sampling-voronoi', N, 1);
    const lum = cs.buf.lum;
    const Rb = wantRgb ? cs.buf.r : null;
    const Gb = wantRgb ? cs.buf.g : null;
    const Bb = wantRgb ? cs.buf.b : null;
    const counts = new Uint32Array(N);
    lum.fill(0);
    if (wantRgb) { Rb.fill(0); Gb.fill(0); Bb.fill(0); }
    const fLum = field.buf.lum;
    const fR = wantRgb ? field.buf.r : null;
    const fG = wantRgb ? field.buf.g : null;
    const fB = wantRgb ? field.buf.b : null;
    /* Coarse stride to keep cost linear in sites×pixels-sampled. */
    const stride = opts.stride || Math.max(1, Math.floor(Math.sqrt(w * h / 8000)));
    for (let y = 0; y < h; y += stride) {
      for (let x = 0; x < w; x += stride) {
        let bestI = 0, bestD = Infinity;
        for (let s = 0; s < N; s++) {
          const dx = x - siteXs[s], dy = y - siteYs[s];
          const d = dx * dx + dy * dy;
          if (d < bestD) { bestD = d; bestI = s; }
        }
        const i = y * w + x;
        lum[bestI] += fLum[i];
        if (wantRgb) { Rb[bestI] += fR[i]; Gb[bestI] += fG[i]; Bb[bestI] += fB[i]; }
        counts[bestI] += 1;
      }
    }
    for (let s = 0; s < N; s++) {
      const c = counts[s]; if (!c) continue;
      const inv = 1 / c;
      lum[s] *= inv;
      if (wantRgb) { Rb[s] *= inv; Gb[s] *= inv; Bb[s] *= inv; }
    }
    return {
      cols: N, rows: 1,
      channels: new Set(chans), buf: cs.buf,
      sourceW: w, sourceH: h,
      cellX: siteXs, cellY: siteYs,
      topology: 'voronoi',
    };
  }, {
    label: 'Voronoi tiling — nearest-site assignment',
    requires: ['lum'],
    produces: ['lum', 'r', 'g', 'b'],
    scratch: { cellSignals: [{ tag: 'gg:sampling-voronoi', channels: ['lum', 'r', 'g', 'b'] }] },
  });

})();
