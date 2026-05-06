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

  const api = Object.freeze({
    SOBEL_X: SOBEL_X,
    SOBEL_Y: SOBEL_Y,
    DEFAULT_ALPHABET: DEFAULT_ALPHABET,
    sobel: sobel,
    binDirection: binDirection,
    emitDirectional: emitDirectional,
    selectGrid: selectGrid,
  });

  const root = (typeof window !== 'undefined') ? window
             : (typeof globalThis !== 'undefined') ? globalThis
             : this;
  root.GlyphGrid = root.GlyphGrid || {};
  root.GlyphGrid.edge = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})();
