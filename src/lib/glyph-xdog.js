/* glyph-xdog.js — Extended Difference of Gaussians prefilter.
 *
 * Winnemöller 2012; ramped XDoG for stylized edges.
 *
 * Pipeline per CR-5 (dynamic kernel radius):
 *   G(σ)   = separable Gaussian with kernel radius r = ceil(3σ)
 *   DoG    = G(σ) - τ · G(kσ)
 *   XDoG   = tanh(φ · (DoG - ε)) if DoG >= ε else 1         (Winnemöller)
 *
 * Supported σ range: 0.5 (5-tap) to 5.0 (31-tap). k defaults to 1.6
 * (Marr-Hildreth). Operates on a single-channel Float32Array luminance
 * in [0, 1]. Output is Float32Array in [0, 1] where 1 = keep, 0 = edge.
 *
 * Kernel weights are cached per σ — a piece uses few unique σ so the cache
 * stays small.
 */

(function () {
  'use strict';

  const CACHE = Object.create(null);

  function kernel1D(sigma) {
    const key = sigma.toFixed(4);
    if (CACHE[key]) return CACHE[key];
    const r = Math.ceil(3 * sigma);
    const n = 2 * r + 1;
    const w = new Float32Array(n);
    const inv2s2 = 1 / (2 * sigma * sigma);
    let sum = 0;
    for (let i = 0; i < n; i++) {
      const x = i - r;
      const v = Math.exp(-x * x * inv2s2);
      w[i] = v;
      sum += v;
    }
    for (let i = 0; i < n; i++) w[i] /= sum;
    const entry = { radius: r, weights: w };
    CACHE[key] = entry;
    return entry;
  }

  function gaussianBlur(src, dst, width, height, sigma, scratch) {
    if (sigma < 0.3) {
      dst.set(src);
      return dst;
    }
    const { radius, weights } = kernel1D(sigma);
    const tmp = scratch || new Float32Array(width * height);

    /* Horizontal pass. */
    for (let y = 0; y < height; y++) {
      const rowOff = y * width;
      for (let x = 0; x < width; x++) {
        let s = 0;
        for (let k = -radius; k <= radius; k++) {
          let xx = x + k;
          if (xx < 0) xx = -xx;
          else if (xx >= width) xx = 2 * width - xx - 2;
          s += src[rowOff + xx] * weights[k + radius];
        }
        tmp[rowOff + x] = s;
      }
    }
    /* Vertical pass. */
    for (let x = 0; x < width; x++) {
      for (let y = 0; y < height; y++) {
        let s = 0;
        for (let k = -radius; k <= radius; k++) {
          let yy = y + k;
          if (yy < 0) yy = -yy;
          else if (yy >= height) yy = 2 * height - yy - 2;
          s += tmp[yy * width + x] * weights[k + radius];
        }
        dst[y * width + x] = s;
      }
    }
    return dst;
  }

  /* XDoG: Winnemöller 2012 formula. */
  function xdog(src, width, height, opts) {
    opts = opts || {};
    const sigma = clamp(opts.sigma == null ? 1.0 : opts.sigma, 0.5, 5.0);
    const k = opts.k == null ? 1.6 : opts.k;
    const tau = opts.tau == null ? 0.98 : opts.tau;
    const phi = opts.phi == null ? 40 : opts.phi;
    const epsilon = opts.epsilon == null ? 0.003 : opts.epsilon;
    const invert = opts.invert !== false; /* default true: dark edges on light */

    const N = width * height;
    const g1 = new Float32Array(N);
    const g2 = new Float32Array(N);
    const tmp = new Float32Array(N);
    gaussianBlur(src, g1, width, height, sigma, tmp);
    gaussianBlur(src, g2, width, height, sigma * k, tmp);

    const out = new Float32Array(N);
    for (let i = 0; i < N; i++) {
      const d = g1[i] - tau * g2[i];
      let val;
      if (d >= epsilon) val = 1.0;
      else              val = 1.0 + Math.tanh(phi * (d - epsilon));
      /* Clamp to [0,1]. */
      if (val < 0) val = 0; else if (val > 1) val = 1;
      out[i] = invert ? val : 1 - val;
    }
    return out;
  }

  function clamp(v, lo, hi) { return v < lo ? lo : (v > hi ? hi : v); }

  const api = Object.freeze({
    kernel1D: kernel1D,
    gaussianBlur: gaussianBlur,
    xdog: xdog,
  });

  const root = (typeof window !== 'undefined') ? window
             : (typeof globalThis !== 'undefined') ? globalThis
             : this;
  root.GlyphGrid = root.GlyphGrid || {};
  root.GlyphGrid.xdog = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})();
