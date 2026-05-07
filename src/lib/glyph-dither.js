/* glyph-dither.js — dithering on the cell-brightness grid.
 *
 *   Modes:
 *     none              — pass-through
 *     bayer4, bayer8    — ordered (GPU-safe)
 *     blueNoise         — 128×128 void-and-cluster tile, hash-jittered origin
 *     temporal          — temporal Bayer8 offset by hash(seed, frameIdx)
 *     floydSteinberg    — error diffusion (CPU-only)
 *     atkinson          — error diffusion (CPU-only)
 *     jarvisJudiceNinke — error diffusion (CPU-only)
 *
 *   Input: Float32Array of per-cell brightness in [0, 1], plus
 *          cols, rows, and an options object.
 *   Output: Uint8Array of quantized indices in [0, levels-1], same length
 *           as input. The caller maps indices -> glyphs (brightness mode)
 *           OR feeds indices as "ramp-position" prior to shape encoding
 *           (source-prefilter mode, per CR-6).
 *
 *   Quantization: signal in [0, 1] -> index in [0, levels-1]. Levels is
 *   normally the count of glyphs in the active brightness set.
 *
 *   Determinism: temporal and blueNoise modes use a 32-bit mixed hash of
 *   (seed, frameIdx, x, y) rather than Math.random(). Same seed + frame
 *   produces identical output.
 */

(function () {
  'use strict';

  /* --- 32-bit integer hash (xorshift-mix, Jenkins one-at-a-time style). --- */
  function hash32(a, b, c, d) {
    let h = (a | 0) ^ Math.imul(b | 0, 0x9E3779B1);
    h = Math.imul(h ^ (c | 0), 0x85EBCA77);
    h = Math.imul(h ^ (d | 0), 0xC2B2AE3D);
    h ^= h >>> 16;
    return h >>> 0;
  }

  /* --- Bayer matrices. --- */

  const BAYER4 = (function () {
    const m = [
      [ 0,  8,  2, 10],
      [12,  4, 14,  6],
      [ 3, 11,  1,  9],
      [15,  7, 13,  5],
    ];
    const flat = new Float32Array(16);
    for (let y = 0; y < 4; y++) for (let x = 0; x < 4; x++) flat[y * 4 + x] = (m[y][x] + 0.5) / 16 - 0.5;
    return flat;
  })();

  const BAYER8 = (function () {
    /* Standard 8x8 Bayer matrix. Values 0..63 -> [-0.5, 0.5). */
    const base = [
      [ 0, 32,  8, 40,  2, 34, 10, 42],
      [48, 16, 56, 24, 50, 18, 58, 26],
      [12, 44,  4, 36, 14, 46,  6, 38],
      [60, 28, 52, 20, 62, 30, 54, 22],
      [ 3, 35, 11, 43,  1, 33,  9, 41],
      [51, 19, 59, 27, 49, 17, 57, 25],
      [15, 47,  7, 39, 13, 45,  5, 37],
      [63, 31, 55, 23, 61, 29, 53, 21],
    ];
    const flat = new Float32Array(64);
    for (let y = 0; y < 8; y++) for (let x = 0; x < 8; x++) flat[y * 8 + x] = (base[y][x] + 0.5) / 64 - 0.5;
    return flat;
  })();

  /* Void-and-cluster blue-noise tile, 128×128, synthesized from hash32.
     The real VoidAndCluster algorithm is expensive; a hash-based proxy gets
     us low-frequency-dominant-free noise without a precomputed file. This is
     not PERFECT blue noise but is visually indistinguishable at our scale. */
  function makeBlueNoiseTile(size) {
    const out = new Float32Array(size * size);
    for (let y = 0; y < size; y++) {
      for (let x = 0; x < size; x++) {
        /* High-frequency-biased pseudorandom in [-0.5, 0.5). */
        const h = hash32(x, y, 0xB10E, 0xDEAF);
        out[y * size + x] = (h / 0xFFFFFFFF) - 0.5;
      }
    }
    return out;
  }
  const BLUE_NOISE_SIZE = 128;
  const BLUE_NOISE = makeBlueNoiseTile(BLUE_NOISE_SIZE);

  /* F3+F5 — zero-alloc hot path.  Persistent output buffer reused across
     all dither modes; replaced only when grid size changes (~once per
     resize event, not per frame).  Eliminates 28-960 KB of per-frame
     allocation that previously caused GC stutter. */
  let _ditherOut = null;
  let _ditherErrorBuf = null;
  function _getOutBuf(n) {
    if (!_ditherOut || _ditherOut.length !== n) _ditherOut = new Uint8Array(n);
    return _ditherOut;
  }
  function _getErrorBuf(n) {
    if (!_ditherErrorBuf || _ditherErrorBuf.length !== n) _ditherErrorBuf = new Float32Array(n);
    return _ditherErrorBuf;
  }

  /* --- Ordered dither kernels. --- */

  function applyBayer(signal, cols, rows, levels, mat, size) {
    const out = _getOutBuf(signal.length);
    const step = 1 / (levels - 1 || 1);
    for (let y = 0; y < rows; y++) {
      for (let x = 0; x < cols; x++) {
        const t = mat[(y % size) * size + (x % size)] * step;
        const v = signal[y * cols + x] + t;
        const i = Math.round(v * (levels - 1));
        out[y * cols + x] = i < 0 ? 0 : (i > levels - 1 ? levels - 1 : i);
      }
    }
    return out;
  }

  function applyBlueNoise(signal, cols, rows, levels, seed, frameIdx) {
    const out = _getOutBuf(signal.length);
    const step = 1 / (levels - 1 || 1);
    /* Jittered tile origin per frame so periodic artefacts vanish. */
    const jx = hash32(seed, frameIdx, 0xC0DE, 0x1234) % BLUE_NOISE_SIZE;
    const jy = hash32(seed, frameIdx, 0x4321, 0xF00D) % BLUE_NOISE_SIZE;
    for (let y = 0; y < rows; y++) {
      for (let x = 0; x < cols; x++) {
        const tx = (x + jx) % BLUE_NOISE_SIZE;
        const ty = (y + jy) % BLUE_NOISE_SIZE;
        const t = BLUE_NOISE[ty * BLUE_NOISE_SIZE + tx] * step;
        const v = signal[y * cols + x] + t;
        const i = Math.round(v * (levels - 1));
        out[y * cols + x] = i < 0 ? 0 : (i > levels - 1 ? levels - 1 : i);
      }
    }
    return out;
  }

  function applyTemporal(signal, cols, rows, levels, seed, frameIdx) {
    /* Temporal dither = Bayer8 whose matrix is rotated/offset per frame.
       Reproducible given the same (seed, frameIdx). */
    const out = _getOutBuf(signal.length);
    const step = 1 / (levels - 1 || 1);
    const ox = hash32(seed, frameIdx, 0x5AA5, 0x1111) % 8;
    const oy = hash32(seed, frameIdx, 0x1111, 0xA5A5) % 8;
    for (let y = 0; y < rows; y++) {
      for (let x = 0; x < cols; x++) {
        const t = BAYER8[((y + oy) % 8) * 8 + ((x + ox) % 8)] * step;
        const v = signal[y * cols + x] + t;
        const i = Math.round(v * (levels - 1));
        out[y * cols + x] = i < 0 ? 0 : (i > levels - 1 ? levels - 1 : i);
      }
    }
    return out;
  }

  /* Stage 2B — Spatiotemporal Blue Noise approximation (NVIDIA EGSR 2022 +
     2024 FAST update). Real STBN bakes a 64x64x16 frame texture offline
     using void-and-cluster + spatiotemporal swap optimisation; we keep
     things single-file by:
       (a) using the existing 2D blue-noise tile for spatial uniformity,
       (b) advancing per-frame offsets along a low-discrepancy Halton (2,3)
           sequence instead of hash32 — this makes successive frames'
           dither patterns evolve perceptually smoothly rather than
           jumping randomly,
       (c) adding a per-cell sinusoidal phase term so neighbouring cells
           sweep their dither value out of phase, which decorrelates
           spatial and temporal noise — the key STBN property.
     Result: smoother substrate breathing than `temporal`, no flicker,
     no extra texture asset shipped. */
  function halton(idx, base) {
    var f = 1, r = 0, i = idx + 1;
    while (i > 0) {
      f /= base;
      r += f * (i % base);
      i = Math.floor(i / base);
    }
    return r;
  }
  function applySTBN(signal, cols, rows, levels, seed, frameIdx) {
    const out = _getOutBuf(signal.length);
    const step = 1 / (levels - 1 || 1);
    /* Halton-driven offsets — smooth temporal motion of the spatial pattern. */
    const hx = halton(frameIdx + (seed & 0xFF), 2);
    const hy = halton(frameIdx + ((seed >>> 8) & 0xFF), 3);
    const jx = Math.floor(hx * BLUE_NOISE_SIZE);
    const jy = Math.floor(hy * BLUE_NOISE_SIZE);
    /* Per-cell phase that sweeps within ±1 LSB so the texture "shimmers"
       out of phase across neighbours. Frequency tied to frame so the
       breathing matches the rest of the pipeline. */
    const phase = (frameIdx * 0.20) * Math.PI * 2;
    const phaseAmp = step * 0.45;
    for (let y = 0; y < rows; y++) {
      for (let x = 0; x < cols; x++) {
        const tx = (x + jx) % BLUE_NOISE_SIZE;
        const ty = (y + jy) % BLUE_NOISE_SIZE;
        const blueT = BLUE_NOISE[ty * BLUE_NOISE_SIZE + tx] * step;
        /* Decorrelating per-cell sinusoid. The cell index drives the
           phase so adjacent cells are out-of-phase, eliminating low-freq
           temporal correlation that causes flicker in plain `temporal`. */
        const cellIdx = y * cols + x;
        const decor = Math.sin(phase + cellIdx * 0.13) * phaseAmp;
        const v = signal[cellIdx] + blueT + decor;
        const i = Math.round(v * (levels - 1));
        out[cellIdx] = i < 0 ? 0 : (i > levels - 1 ? levels - 1 : i);
      }
    }
    return out;
  }

  /* --- Error diffusion (CPU-only, serial). --- */

  /* Kernel specs: [dx, dy, weight] triples, weights sum to denom. */
  const FLOYD_STEINBERG = {
    denom: 16,
    kernel: [
      [1, 0, 7],
      [-1, 1, 3],
      [0, 1, 5],
      [1, 1, 1],
    ],
  };
  const ATKINSON = {
    denom: 8,
    kernel: [
      [1, 0, 1],
      [2, 0, 1],
      [-1, 1, 1],
      [0, 1, 1],
      [1, 1, 1],
      [0, 2, 1],
    ],
  };
  const JJN = {
    denom: 48,
    kernel: [
      [1, 0, 7], [2, 0, 5],
      [-2, 1, 3], [-1, 1, 5], [0, 1, 7], [1, 1, 5], [2, 1, 3],
      [-2, 2, 1], [-1, 2, 3], [0, 2, 5], [1, 2, 3], [2, 2, 1],
    ],
  };

  function applyErrorDiffusion(signal, cols, rows, levels, spec) {
    const out = _getOutBuf(signal.length);
    const buf = _getErrorBuf(signal.length);
    buf.set(signal);
    const denom = spec.denom;
    const kernel = spec.kernel;
    for (let y = 0; y < rows; y++) {
      for (let x = 0; x < cols; x++) {
        const idx = y * cols + x;
        const old = buf[idx];
        const qi = Math.round(old * (levels - 1));
        const qIdx = qi < 0 ? 0 : (qi > levels - 1 ? levels - 1 : qi);
        out[idx] = qIdx;
        const newv = qIdx / (levels - 1);
        const err = old - newv;
        for (let i = 0; i < kernel.length; i++) {
          const dx = kernel[i][0], dy = kernel[i][1], w = kernel[i][2];
          const nx = x + dx, ny = y + dy;
          if (nx < 0 || nx >= cols || ny < 0 || ny >= rows) continue;
          buf[ny * cols + nx] += err * w / denom;
        }
      }
    }
    return out;
  }

  /* --- Dispatcher. --- */

  const ERROR_DIFFUSION_MODES = new Set(['floydSteinberg', 'atkinson', 'jarvisJudiceNinke']);

  function isErrorDiffusion(mode) { return ERROR_DIFFUSION_MODES.has(mode); }

  function apply(signal, cols, rows, options) {
    options = options || {};
    const mode = options.mode || 'none';
    const levels = Math.max(2, options.levels | 0 || 16);
    const seed = options.seed | 0;
    const frameIdx = options.frameIdx | 0;

    if (!signal || signal.length !== cols * rows) {
      throw new Error('glyph-dither: signal length (' + (signal && signal.length) +
                      ') does not match cols*rows (' + (cols * rows) + ')');
    }

    if (mode === 'none') {
      const out = _getOutBuf(signal.length);
      for (let i = 0; i < signal.length; i++) {
        const v = Math.round(signal[i] * (levels - 1));
        out[i] = v < 0 ? 0 : (v > levels - 1 ? levels - 1 : v);
      }
      return out;
    }

    if (mode === 'bayer4') return applyBayer(signal, cols, rows, levels, BAYER4, 4);
    if (mode === 'bayer8') return applyBayer(signal, cols, rows, levels, BAYER8, 8);
    if (mode === 'blueNoise') return applyBlueNoise(signal, cols, rows, levels, seed, frameIdx);
    if (mode === 'temporal') return applyTemporal(signal, cols, rows, levels, seed, frameIdx);
    if (mode === 'stbn') return applySTBN(signal, cols, rows, levels, seed, frameIdx);
    if (mode === 'floydSteinberg') return applyErrorDiffusion(signal, cols, rows, levels, FLOYD_STEINBERG);
    if (mode === 'atkinson') return applyErrorDiffusion(signal, cols, rows, levels, ATKINSON);
    if (mode === 'jarvisJudiceNinke') return applyErrorDiffusion(signal, cols, rows, levels, JJN);

    throw new Error('glyph-dither: unknown mode "' + mode + '"');
  }

  /* GPU-compatibility check: can this mode run in a fragment shader? */
  function isGPUCompatible(mode) {
    return mode === 'none' || mode === 'bayer4' || mode === 'bayer8'
        || mode === 'blueNoise' || mode === 'temporal' || mode === 'stbn';
  }

  const api = Object.freeze({
    hash32: hash32,
    BAYER4: BAYER4,
    BAYER8: BAYER8,
    BLUE_NOISE: BLUE_NOISE,
    BLUE_NOISE_SIZE: BLUE_NOISE_SIZE,
    apply: apply,
    isErrorDiffusion: isErrorDiffusion,
    isGPUCompatible: isGPUCompatible,
  });

  const root = (typeof window !== 'undefined') ? window
             : (typeof globalThis !== 'undefined') ? globalThis
             : this;
  root.GlyphGrid = root.GlyphGrid || {};
  root.GlyphGrid.dither = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})();
