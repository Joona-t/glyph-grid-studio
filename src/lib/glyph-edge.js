/* glyph-edge.js — cheap edge-directional selection (Sobel on cell grid).
 *
 * This is the alternative to shape-vector selection. It costs a single
 * 3×3 Sobel pass over the cell-resolution signal and maps (magnitude,
 * direction) onto a small pre-chosen alphabet of directional glyphs.
 *
 * Gradient convention (standard image processing):
 *   Gx = horizontal gradient (pos right)
 *   Gy = vertical gradient   (pos down)
 *   angle = atan2(Gy, Gx)    ∈ [-π, π]
 *
 * Direction binning: 8 bins at 45° intervals. Bin 0 is horizontal (→),
 * bin 2 is vertical-down (↓), etc. Bin labels map to codepoints via the
 * default alphabet, overridable via opts.
 *
 * Intended path: for each cell, if gradient magnitude > threshold, draw
 * a directional glyph; else fall back to brightness-selected glyph. The
 * caller wires the fallback — this lib just emits (mag, dir) per cell and
 * a glyph index when a directional alphabet is provided.
 *
 * IMPORTANT per CR-1: the caller must pass a LINEARIZED luminance signal
 * (proper sRGB EOTF + Rec.709 weights) for the Sobel to be physically
 * meaningful. This lib doesn't linearize.
 *
 * Per IM-3: run Sobel on the cell-resolution signal, NOT full-res. Full-res
 * gradients pick up sub-cell aliasing and misdirect the selection.
 */

(function () {
  'use strict';

  const SOBEL_X = [-1, 0, 1, -2, 0, 2, -1, 0, 1];
  const SOBEL_Y = [-1, -2, -1, 0, 0, 0, 1, 2, 1];

  /* Default directional alphabet — 8 glyphs + "no edge" fallback.
     Uses Box Drawing codepoints; caller can override with octants/arrows. */
  const DEFAULT_ALPHABET = [
    0x2500, // ─  0 horizontal →
    0x2571, // ╱  1 NE
    0x2502, // │  2 vertical ↓
    0x2572, // ╲  3 SE
    0x2500, // ─  4 horizontal ← (same shape, different direction)
    0x2571, // ╱  5 SW (same shape)
    0x2502, // │  6 vertical ↑
    0x2572, // ╲  7 NW
  ];

  /* Compute Sobel gradients on a cell-resolution signal.
     Input:  Float32Array length cols*rows in [0, 1].
     Output: { mag: Float32Array(cols*rows), dir: Float32Array(cols*rows) }
       mag  — gradient magnitude (not normalized)
       dir  — atan2(Gy, Gx) in radians ∈ [-π, π] */
  function sobel(signal, cols, rows) {
    const mag = new Float32Array(cols * rows);
    const dir = new Float32Array(cols * rows);
    for (let y = 0; y < rows; y++) {
      for (let x = 0; x < cols; x++) {
        let gx = 0, gy = 0;
        for (let ky = -1; ky <= 1; ky++) {
          const yy = y + ky;
          if (yy < 0 || yy >= rows) continue;
          const row = yy * cols;
          for (let kx = -1; kx <= 1; kx++) {
            const xx = x + kx;
            if (xx < 0 || xx >= cols) continue;
            const idx = (ky + 1) * 3 + (kx + 1);
            const v = signal[row + xx];
            gx += v * SOBEL_X[idx];
            gy += v * SOBEL_Y[idx];
          }
        }
        const m = Math.hypot(gx, gy);
        mag[y * cols + x] = m;
        dir[y * cols + x] = m > 1e-6 ? Math.atan2(gy, gx) : 0;
      }
    }
    return { mag: mag, dir: dir };
  }

  /* Bin a direction (radians ∈ [-π, π]) into one of 8 compass bins (0..7).
     Bin 0 centered on 0° (east), bin 2 on 90° (south-equivalent, +Gy down). */
  function binDirection(rad) {
    const twoPi = Math.PI * 2;
    let a = rad;
    while (a < 0) a += twoPi;
    /* shift so bin 0 covers [-22.5°, +22.5°] */
    const shifted = (a + Math.PI / 8) % twoPi;
    return Math.floor(shifted / (Math.PI / 4)) & 7;
  }

  /* Produce a Uint16Array of cp per cell given mag/dir and a threshold.
     Cells below threshold emit `fallbackCp` (caller passes 0x20 space or
     a brightness-selected codepoint if coupling the two). */
  function emitDirectional(mag, dir, cols, rows, opts) {
    opts = opts || {};
    const threshold = opts.threshold == null ? 0.15 : opts.threshold;
    const alphabet = opts.alphabet || DEFAULT_ALPHABET;
    const fallbackCp = opts.fallbackCp == null ? 0x20 : opts.fallbackCp;
    const out = new Uint16Array(cols * rows);
    for (let i = 0; i < out.length; i++) {
      if (mag[i] < threshold) {
        out[i] = fallbackCp;
      } else {
        out[i] = alphabet[binDirection(dir[i])];
      }
    }
    return out;
  }

  /* Convenience: single call from signal -> directional cp grid. */
  function selectGrid(signal, cols, rows, opts) {
    const g = sobel(signal, cols, rows);
    return emitDirectional(g.mag, g.dir, cols, rows, opts);
  }

  /* ── Edge-tangent flow field (v0.1.6, SOTA feature 3) ────────────────
     Structure-tensor ETF in the lineage of Kang/Lee/Chui's flow-based
     abstraction: per-cell gradients → outer-product tensor (Jxx Jxy Jyy)
     → N passes of 3×3 box smoothing ON THE TENSOR (immune to the angle-
     wraparound artifacts of smoothing raw orientations) → eigen-analysis:
       gradient dir  = ½·atan2(2Jxy, Jxx−Jyy)
       tangent       = gradient + π/2   (the direction strokes FLOW)
       coherence     = (λ1−λ2)/(λ1+λ2)  ∈ [0,1] — how oriented the
                       neighbourhood is (1 = clean edge, 0 = isotropic)
       energy        = λ1+λ2            — overall gradient strength
     All buffers persistent (grow-only); zero per-frame allocations after
     the first call at a given grid size. */
  let _ffKey = '';
  let _ffGx = null, _ffGy = null;
  let _ffJxx = null, _ffJxy = null, _ffJyy = null, _ffTmp = null;
  let _ffTangent = null, _ffCoherence = null, _ffEnergy = null;

  function _ffEnsure(cols, rows) {
    const key = cols + 'x' + rows;
    if (_ffKey === key) return;
    const n = cols * rows;
    _ffGx = new Float32Array(n);   _ffGy = new Float32Array(n);
    _ffJxx = new Float32Array(n);  _ffJxy = new Float32Array(n);
    _ffJyy = new Float32Array(n);  _ffTmp = new Float32Array(n);
    _ffTangent = new Float32Array(n);
    _ffCoherence = new Float32Array(n);
    _ffEnergy = new Float32Array(n);
    _ffKey = key;
  }

  function _boxBlur3(buf, tmp, cols, rows) {
    /* one 3×3 box pass, edge-clamped, in place via tmp */
    for (let y = 0; y < rows; y++) {
      const row = y * cols;
      const up = (y > 0 ? y - 1 : 0) * cols;
      const dn = (y < rows - 1 ? y + 1 : rows - 1) * cols;
      for (let x = 0; x < cols; x++) {
        const l = x > 0 ? x - 1 : 0;
        const r = x < cols - 1 ? x + 1 : cols - 1;
        tmp[row + x] = (
          buf[up + l] + buf[up + x] + buf[up + r] +
          buf[row + l] + buf[row + x] + buf[row + r] +
          buf[dn + l] + buf[dn + x] + buf[dn + r]
        ) / 9;
      }
    }
    buf.set(tmp);
  }

  /* Compute the flow field for a cell-resolution signal.
     opts.smoothPasses (default 2) — tensor smoothing iterations.
     Returns persistent { tangent, coherence, energy } Float32Arrays
     (valid until the next flowField call). */
  function flowField(signal, cols, rows, opts) {
    opts = opts || {};
    const passes = opts.smoothPasses == null ? 2 : (opts.smoothPasses | 0);
    _ffEnsure(cols, rows);
    const n = cols * rows;

    /* Sobel into persistent gx/gy. */
    for (let y = 0; y < rows; y++) {
      for (let x = 0; x < cols; x++) {
        let gx = 0, gy = 0;
        for (let ky = -1; ky <= 1; ky++) {
          const yy = y + ky;
          if (yy < 0 || yy >= rows) continue;
          const row = yy * cols;
          for (let kx = -1; kx <= 1; kx++) {
            const xx = x + kx;
            if (xx < 0 || xx >= cols) continue;
            const idx = (ky + 1) * 3 + (kx + 1);
            const v = signal[row + xx];
            gx += v * SOBEL_X[idx];
            gy += v * SOBEL_Y[idx];
          }
        }
        const i = y * cols + x;
        _ffGx[i] = gx;
        _ffGy[i] = gy;
      }
    }

    /* Structure tensor + smoothing. */
    for (let i = 0; i < n; i++) {
      const gx = _ffGx[i], gy = _ffGy[i];
      _ffJxx[i] = gx * gx;
      _ffJxy[i] = gx * gy;
      _ffJyy[i] = gy * gy;
    }
    for (let p = 0; p < passes; p++) {
      _boxBlur3(_ffJxx, _ffTmp, cols, rows);
      _boxBlur3(_ffJxy, _ffTmp, cols, rows);
      _boxBlur3(_ffJyy, _ffTmp, cols, rows);
    }

    /* Eigen-analysis. */
    for (let i = 0; i < n; i++) {
      const jxx = _ffJxx[i], jxy = _ffJxy[i], jyy = _ffJyy[i];
      const tr = jxx + jyy;
      const det = Math.sqrt((jxx - jyy) * (jxx - jyy) + 4 * jxy * jxy);
      const l1 = (tr + det) * 0.5;
      const l2 = (tr - det) * 0.5;
      _ffEnergy[i] = tr;
      _ffCoherence[i] = tr > 1e-9 ? (l1 - l2) / (l1 + l2 + 1e-9) : 0;
      /* dominant gradient orientation; tangent is +90°. */
      const gradAngle = 0.5 * Math.atan2(2 * jxy, jxx - jyy);
      _ffTangent[i] = gradAngle + Math.PI / 2;
    }
    return { tangent: _ffTangent, coherence: _ffCoherence, energy: _ffEnergy };
  }

  /* Tangent (orientation, period π) → one of 4 stroke glyphs.
     Bins centred on 0° ─, 45° ╱, 90° │, 135° ╲. */
  const FLOW_ALPHABET = [0x2500, 0x2571, 0x2502, 0x2572];
  function flowGlyph(tangentRad) {
    let a = tangentRad % Math.PI;
    if (a < 0) a += Math.PI;
    const bin = Math.floor(((a + Math.PI / 8) % Math.PI) / (Math.PI / 4)) & 3;
    return FLOW_ALPHABET[bin];
  }

  const api = Object.freeze({
    SOBEL_X: SOBEL_X,
    SOBEL_Y: SOBEL_Y,
    DEFAULT_ALPHABET: DEFAULT_ALPHABET,
    FLOW_ALPHABET: FLOW_ALPHABET,
    sobel: sobel,
    binDirection: binDirection,
    emitDirectional: emitDirectional,
    selectGrid: selectGrid,
    flowField: flowField,
    flowGlyph: flowGlyph,
  });

  const root = (typeof window !== 'undefined') ? window
             : (typeof globalThis !== 'undefined') ? globalThis
             : this;
  root.GlyphGrid = root.GlyphGrid || {};
  root.GlyphGrid.edge = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})();
