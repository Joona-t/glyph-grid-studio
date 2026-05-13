/* glyph-crt.js — CRT post-process chain (CPU path).
 *
 * Ordered chain applied AFTER glyph draw, BEFORE record:
 *
 *   1. phosphor decay — blend previous frame by decayFactor (stateful)
 *   2. halation       — extract bright threshold, blur, add
 *   3. bloom          — same but broader, additive
 *   4. scanlines      — multiplicative darken of alternating rows
 *   5. chromatic aberration — clamped per-channel x offset (CR-9)
 *   6. barrel distortion  — light radial warp (sample with bilerp)
 *   7. vignette       — radial darken at edges
 *
 * Each stage honors its own CONFIG.postprocess.<stage>.enabled switch.
 *
 * State (phosphor decay) is stored in a persistent Float32 RGB buffer. The
 * caller must warm up N frames before recording (CR-8) — this lib exposes
 * prerollFrames(n, canvas) that runs N phosphor-decay iterations without
 * committing to the screen.
 *
 * Reduced motion: when opts.prefersReducedMotion is true, animated stages
 * (scanline subpixel shift, phosphor decay, chromatic wobble) become static.
 *
 * All stages take and return ImageData (8-bit RGBA). Internal math uses
 * linear-light where needed (halation/bloom blur; linear in linear out).
 */

(function () {
  'use strict';

  /* OPT-016 — persistent Float32 scratch buffers for boxBlurLinear + applyBloom.
     Grow-only; only re-allocate on canvas resize (rare). Single-threaded JS +
     synchronous applyChain guarantees no concurrent reuse. Bit-exact: buffers
     are fully overwritten before being read in every call path. */
  let _crtBlurOut = null;
  let _crtBlurTmp = null;
  let _bloomLinBuf = null;

  /* F9 (OPT-018) — 256-entry Float64 LUT for srgbToLinear.
   *
   * `srgbToLinear` runs ~6.2M times/frame on cfg-postproc-heavy (applyBloom
   * × 2 for halation + bloom, 6 calls/pixel × 516096 pixels). All call sites
   * pass an integer byte 0-255 (Uint8ClampedArray slot), so the input domain
   * is fully quantized to 256 values.
   *
   * Bit-exact verified: for every i in 0..255, _SRGB_TO_LINEAR_LUT[i] is
   * computed with the same IEEE 754 double-precision formula as the original
   * srgbToLinear(i). Float64Array stores doubles, so the LUT lookup returns
   * the exact same bit pattern as direct computation. 256/256 identical,
   * max diff = 0 — confirmed empirically with `node -e ...` in the
   * ship-cycle workflow.
   *
   * `linearToSrgb` (line 38) is NOT changed — its input is a sum of linear
   * floats, not quantized to 256 values.
   */
  const _SRGB_TO_LINEAR_LUT = (function () {
    const lut = new Float64Array(256);
    for (let i = 0; i < 256; i++) {
      const s = i / 255;
      lut[i] = s <= 0.04045 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
    }
    return lut;
  })();

  // HOT-PATH: ~6.2M calls/frame on cfg-postproc-heavy. LUT lookup eliminates
  // Math.pow entirely. Input is always a Uint8ClampedArray byte (0-255).
  function srgbToLinear(c) {
    return _SRGB_TO_LINEAR_LUT[c < 0 ? 0 : c > 255 ? 255 : (c | 0)];
  }

  /* OPT-100 — 1024-entry Uint8ClampedArray LUT for linearToSrgb.
   *
   * `linearToSrgb` runs ~9.2M times/frame on cfg-postproc-heavy across
   * applyBloom (halation + bloom: 3 calls/pixel × 2 stages), applyPhosphorDecay,
   * applyVignette, and applyGodRays. Input is a non-quantized linear float; the
   * output is always clamped to a 0..255 integer byte before being written back
   * into a Uint8ClampedArray. A 1024-bin precomputed table maps input bins
   * (linear in [0,1]) directly to output bytes, eliminating Math.pow from the
   * hot path. Quantization error is bounded by ~1/1024 of the linear input
   * range — sub-visual and SSIM-gated.
   *
   * Measured: -50.97ms/frame geomean across 5 configs (cfg-postproc-heavy
   * -67.28ms = -43%); SSIM 0.9876 vs 0.985 floor. See ITER-100 entry in
   * BUGS_AND_ITERATIONS.md.
   */
  const _LINEAR_TO_SRGB_LUT = (function () {
    const lut = new Uint8ClampedArray(1024);
    for (let i = 0; i < 1024; i++) {
      const lin = i / 1023;
      const v = lin <= 0.0031308 ? 12.92 * lin : 1.055 * Math.pow(lin, 1 / 2.4) - 0.055;
      lut[i] = Math.round(v * 255);
    }
    return lut;
  })();

  function linearToSrgb(c) {
    return _LINEAR_TO_SRGB_LUT[c < 0 ? 0 : c > 1 ? 1023 : (c * 1023) | 0];
  }

  function makeState(w, h) {
    return {
      width: w,
      height: h,
      phosphor: new Float32Array(w * h * 3),  /* linear RGB */
      phosphorInit: false,
    };
  }

  /* --- 1. Phosphor decay --- */

  function applyPhosphorDecay(rgba, state, opts) {
    const decayFactor = opts.decayFactor == null ? 0.85 : opts.decayFactor;
    const N = rgba.length / 4;
    if (!state.phosphorInit) {
      for (let i = 0; i < N; i++) {
        state.phosphor[i * 3]     = srgbToLinear(rgba[i * 4]);
        state.phosphor[i * 3 + 1] = srgbToLinear(rgba[i * 4 + 1]);
        state.phosphor[i * 3 + 2] = srgbToLinear(rgba[i * 4 + 2]);
      }
      state.phosphorInit = true;
      return rgba;
    }
    for (let i = 0; i < N; i++) {
      const oldR = state.phosphor[i * 3];
      const oldG = state.phosphor[i * 3 + 1];
      const oldB = state.phosphor[i * 3 + 2];
      const newR = srgbToLinear(rgba[i * 4]);
      const newG = srgbToLinear(rgba[i * 4 + 1]);
      const newB = srgbToLinear(rgba[i * 4 + 2]);
      /* Max blend preserves bright trails without darkening live content. */
      const blendR = Math.max(newR, oldR * decayFactor);
      const blendG = Math.max(newG, oldG * decayFactor);
      const blendB = Math.max(newB, oldB * decayFactor);
      state.phosphor[i * 3]     = blendR;
      state.phosphor[i * 3 + 1] = blendG;
      state.phosphor[i * 3 + 2] = blendB;
      rgba[i * 4]     = linearToSrgb(blendR);
      rgba[i * 4 + 1] = linearToSrgb(blendG);
      rgba[i * 4 + 2] = linearToSrgb(blendB);
    }
    return rgba;
  }

  /* --- Small box blur for bloom/halation (linear-light, separable). --- */

  function boxBlurLinear(src, w, h, radius) {
    const N = src.length;
    if (!_crtBlurOut || _crtBlurOut.length < N) {
      _crtBlurOut = new Float32Array(N);
      _crtBlurTmp = new Float32Array(N);
    }
    const out = _crtBlurOut, tmp = _crtBlurTmp;
    const window = radius * 2 + 1;
    /* Horizontal */
    for (let y = 0; y < h; y++) {
      const row = y * w * 3;
      for (let c = 0; c < 3; c++) {
        let sum = 0;
        for (let i = -radius; i <= radius; i++) {
          const xx = Math.max(0, Math.min(w - 1, i));
          sum += src[row + xx * 3 + c];
        }
        tmp[row + 0 * 3 + c] = sum / window;
        for (let x = 1; x < w; x++) {
          const addX = Math.min(w - 1, x + radius);
          const subX = Math.max(0, x - radius - 1);
          sum += src[row + addX * 3 + c] - src[row + subX * 3 + c];
          tmp[row + x * 3 + c] = sum / window;
        }
      }
    }
    /* Vertical */
    for (let x = 0; x < w; x++) {
      for (let c = 0; c < 3; c++) {
        let sum = 0;
        for (let i = -radius; i <= radius; i++) {
          const yy = Math.max(0, Math.min(h - 1, i));
          sum += tmp[yy * w * 3 + x * 3 + c];
        }
        out[0 * w * 3 + x * 3 + c] = sum / window;
        for (let y = 1; y < h; y++) {
          const addY = Math.min(h - 1, y + radius);
          const subY = Math.max(0, y - radius - 1);
          sum += tmp[addY * w * 3 + x * 3 + c] - tmp[subY * w * 3 + x * 3 + c];
          out[y * w * 3 + x * 3 + c] = sum / window;
        }
      }
    }
    return out;
  }

  function applyBloom(rgba, w, h, opts) {
    const threshold = opts.threshold == null ? 0.75 : opts.threshold;
    const intensity = opts.intensity == null ? 0.5 : opts.intensity;
    const radius = opts.radius == null ? 6 : opts.radius;
    const N = rgba.length / 4;
    if (!_bloomLinBuf || _bloomLinBuf.length < N * 3) _bloomLinBuf = new Float32Array(N * 3);
    const lin = _bloomLinBuf;
    for (let i = 0; i < N; i++) {
      let r = srgbToLinear(rgba[i * 4]);
      let g = srgbToLinear(rgba[i * 4 + 1]);
      let b = srgbToLinear(rgba[i * 4 + 2]);
      const luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
      const over = Math.max(0, luma - threshold);
      const scale = over > 0 ? over / Math.max(luma, 1e-4) : 0;
      lin[i * 3]     = r * scale;
      lin[i * 3 + 1] = g * scale;
      lin[i * 3 + 2] = b * scale;
    }
    const blurred = boxBlurLinear(lin, w, h, radius | 0);
    for (let i = 0; i < N; i++) {
      const r = srgbToLinear(rgba[i * 4])     + blurred[i * 3]     * intensity;
      const g = srgbToLinear(rgba[i * 4 + 1]) + blurred[i * 3 + 1] * intensity;
      const b = srgbToLinear(rgba[i * 4 + 2]) + blurred[i * 3 + 2] * intensity;
      rgba[i * 4]     = linearToSrgb(r);
      rgba[i * 4 + 1] = linearToSrgb(g);
      rgba[i * 4 + 2] = linearToSrgb(b);
    }
    return rgba;
  }

  /* --- Scanlines --- */

  function applyScanlines(rgba, w, h, opts) {
    const intensity = opts.intensity == null ? 0.25 : opts.intensity;
    const period = Math.max(1, opts.period | 0 || 2);
    const phase = opts.phase || 0;
    for (let y = 0; y < h; y++) {
      const darken = ((y + phase) % period === 0) ? 1 : (1 - intensity);
      for (let x = 0; x < w; x++) {
        const i = (y * w + x) * 4;
        rgba[i]     = rgba[i]     * darken;
        rgba[i + 1] = rgba[i + 1] * darken;
        rgba[i + 2] = rgba[i + 2] * darken;
      }
    }
    return rgba;
  }

  /* --- Chromatic aberration --- */

  function applyChromaticAberration(rgba, w, h, opts) {
    const maxOffset = opts.offset == null ? 0.5 : opts.offset;
    /* CR-9: clamp to <= 0.5 * cellSize in the caller's opts; caller should
       compute `offset` based on cellSize; we just obey. */
    const src = new Uint8ClampedArray(rgba);
    const dx = Math.min(Math.max(-5, maxOffset), 5);
    for (let y = 0; y < h; y++) {
      for (let x = 0; x < w; x++) {
        const iDst = (y * w + x) * 4;
        const xr = Math.max(0, Math.min(w - 1, Math.round(x - dx)));
        const xb = Math.max(0, Math.min(w - 1, Math.round(x + dx)));
        rgba[iDst]     = src[(y * w + xr) * 4];     /* R shifted left */
        rgba[iDst + 1] = src[(y * w + x) * 4 + 1];  /* G centered */
        rgba[iDst + 2] = src[(y * w + xb) * 4 + 2]; /* B shifted right */
      }
    }
    return rgba;
  }

  /* --- Barrel distortion --- */

  function applyBarrel(rgba, w, h, opts) {
    const strength = opts.strength == null ? 0.08 : opts.strength;
    const src = new Uint8ClampedArray(rgba);
    const cx = w / 2, cy = h / 2;
    const maxR2 = cx * cx + cy * cy;
    for (let y = 0; y < h; y++) {
      for (let x = 0; x < w; x++) {
        const ux = (x - cx);
        const uy = (y - cy);
        const r2 = (ux * ux + uy * uy) / maxR2;
        const f = 1 + strength * r2;
        const sx = cx + ux * f;
        const sy = cy + uy * f;
        /* Nearest-neighbor — fast, avoids ringing on glyph edges. */
        const ix = Math.max(0, Math.min(w - 1, sx | 0));
        const iy = Math.max(0, Math.min(h - 1, sy | 0));
        const iSrc = (iy * w + ix) * 4;
        const iDst = (y * w + x) * 4;
        rgba[iDst]     = src[iSrc];
        rgba[iDst + 1] = src[iSrc + 1];
        rgba[iDst + 2] = src[iSrc + 2];
      }
    }
    return rgba;
  }

  /* --- Vignette --- */

  function applyVignette(rgba, w, h, opts) {
    const strength = opts.strength == null ? 0.5 : opts.strength;
    const cx = w / 2, cy = h / 2;
    const maxR = Math.hypot(cx, cy);
    for (let y = 0; y < h; y++) {
      for (let x = 0; x < w; x++) {
        const r = Math.hypot(x - cx, y - cy) / maxR;
        const darken = 1 - strength * r * r;
        const i = (y * w + x) * 4;
        rgba[i]     = rgba[i]     * darken;
        rgba[i + 1] = rgba[i + 1] * darken;
        rgba[i + 2] = rgba[i + 2] * darken;
      }
    }
    return rgba;
  }

  /* --- God rays (v2.1) --- */

  /* Simple radial-blur accumulator.  Traces `steps` samples from each
     pixel back toward `lightPos` (normalized), accumulates linear
     luminance above `threshold`, and adds the result to the pixel.
     O(W*H*steps); ~20M ops at 800x800x32 — fine for record mode. */
  function applyGodRays(rgba, w, h, opts) {
    const steps = Math.max(4, Math.min(96, opts.steps == null ? 32 : opts.steps));
    const strength = opts.strength == null ? 0.35 : opts.strength;
    const threshold = opts.threshold == null ? 0.6 : opts.threshold;
    const decay = opts.decay == null ? 0.96 : opts.decay;
    const lightPos = opts.lightPos || [0.5, 0.05];
    const lx = lightPos[0] * w;
    const ly = lightPos[1] * h;
    const tintHex = opts.color || null;
    let tintR = 1, tintG = 1, tintB = 1;
    if (tintHex && typeof tintHex === 'string') {
      const m = tintHex.match(/#?([0-9a-fA-F]{2})([0-9a-fA-F]{2})([0-9a-fA-F]{2})/);
      if (m) {
        tintR = parseInt(m[1], 16) / 255;
        tintG = parseInt(m[2], 16) / 255;
        tintB = parseInt(m[3], 16) / 255;
      }
    }
    for (let y = 0; y < h; y++) {
      for (let x = 0; x < w; x++) {
        let accum = 0, weight = 1;
        const dx = (lx - x) / steps;
        const dy = (ly - y) / steps;
        for (let s = 1; s <= steps; s++) {
          const sx = (x + dx * s) | 0;
          const sy = (y + dy * s) | 0;
          if (sx < 0 || sx >= w || sy < 0 || sy >= h) break;
          const si = (sy * w + sx) * 4;
          const lr = srgbToLinear(rgba[si]);
          const lg = srgbToLinear(rgba[si + 1]);
          const lb = srgbToLinear(rgba[si + 2]);
          const lum = 0.2126 * lr + 0.7152 * lg + 0.0722 * lb;
          if (lum > threshold) accum += (lum - threshold) * weight;
          weight *= decay;
        }
        if (accum <= 0) continue;
        const add = (accum / steps) * strength;
        const i = (y * w + x) * 4;
        const dr = srgbToLinear(rgba[i])     + add * tintR;
        const dg = srgbToLinear(rgba[i + 1]) + add * tintG;
        const db = srgbToLinear(rgba[i + 2]) + add * tintB;
        rgba[i]     = linearToSrgb(dr);
        rgba[i + 1] = linearToSrgb(dg);
        rgba[i + 2] = linearToSrgb(db);
      }
    }
  }

  /* --- Letterbox (v2.1) --- */

  /* Fill top and bottom bars with the letterbox colour.  Always runs
     last — any other post-process stage that depends on full canvas
     content already finished. */
  function applyLetterbox(rgba, w, h, opts) {
    const topPx = Math.max(0, Math.floor(opts.topPx || 0));
    const botPx = Math.max(0, Math.floor(opts.bottomPx || 0));
    const hex = opts.color || '#000000';
    const m = hex.match(/#?([0-9a-fA-F]{2})([0-9a-fA-F]{2})([0-9a-fA-F]{2})/);
    const r = m ? parseInt(m[1], 16) : 0;
    const g = m ? parseInt(m[2], 16) : 0;
    const b = m ? parseInt(m[3], 16) : 0;
    for (let y = 0; y < Math.min(topPx, h); y++) {
      const base = y * w * 4;
      for (let x = 0; x < w; x++) {
        const i = base + x * 4;
        rgba[i] = r; rgba[i + 1] = g; rgba[i + 2] = b; rgba[i + 3] = 255;
      }
    }
    for (let y = Math.max(0, h - botPx); y < h; y++) {
      const base = y * w * 4;
      for (let x = 0; x < w; x++) {
        const i = base + x * 4;
        rgba[i] = r; rgba[i + 1] = g; rgba[i + 2] = b; rgba[i + 3] = 255;
      }
    }
  }

  /* --- Apply chain --- */

  function applyChain(imgData, state, postOpts, runtime) {
    postOpts = postOpts || {};
    runtime = runtime || {};
    const w = imgData.width, h = imgData.height;
    const d = imgData.data;
    const reduced = !!runtime.prefersReducedMotion;

    if (postOpts.phosphorDecay && postOpts.phosphorDecay.enabled && !reduced) {
      /* Lazy init: state may be null if phosphorDecay was enabled after setup().
         This was a crash (Cannot read 'phosphorInit' of null) pre-Wave-0. */
      if (!state) state = makeState(w, h);
      applyPhosphorDecay(d, state, postOpts.phosphorDecay);
    }
    if (postOpts.crtBeam && postOpts.crtBeam.enabled) {
      if (!state) state = makeState(w, h);
      if (!state.crtBeam) state.crtBeam = { beamY: 0 };
      state.crtBeam = applyCrtBeam(d, w, h, state.crtBeam, postOpts.crtBeam);
    }
    if (postOpts.halation && postOpts.halation.enabled) {
      applyBloom(d, w, h, Object.assign({ threshold: 0.6, intensity: 0.3, radius: 4 }, postOpts.halation));
    }
    if (postOpts.bloom && postOpts.bloom.enabled) {
      applyBloom(d, w, h, postOpts.bloom);
    }
    if (postOpts.godRays && postOpts.godRays.enabled) {
      applyGodRays(d, w, h, postOpts.godRays);
    }
    if (postOpts.scanlines && postOpts.scanlines.enabled) {
      const phase = reduced ? 0 : (postOpts.scanlines.phase || 0);
      applyScanlines(d, w, h, Object.assign({}, postOpts.scanlines, { phase: phase }));
    }
    if (postOpts.chromaticAberration && postOpts.chromaticAberration.enabled) {
      applyChromaticAberration(d, w, h, postOpts.chromaticAberration);
    }
    if (postOpts.barrel && postOpts.barrel.enabled) {
      applyBarrel(d, w, h, postOpts.barrel);
    }
    /* Vignette + letterbox are pure mask multiplies; the host can apply
       them as canvas2D composite overlays after putImageData (avoids the
       per-pixel JS multiply when those are the only enabled stages).
       Set runtime.skipOverlays = true to skip them here. */
    if (!runtime.skipOverlays) {
      if (postOpts.vignette && postOpts.vignette.enabled) {
        applyVignette(d, w, h, postOpts.vignette);
      }
      if (postOpts.letterbox && postOpts.letterbox.enabled) {
        applyLetterbox(d, w, h, postOpts.letterbox);
      }
    }
    /* Kawaii overlay — drawn LAST so the hearts + sparkles stay on top
       of the glyph grid. Frame-aware via runtime.frameIdx so they twinkle
       smoothly. */
    if (postOpts.kawaii && postOpts.kawaii.enabled) {
      applyKawaii(d, w, h, postOpts.kawaii, runtime.frameIdx | 0);
    }
    return imgData;
  }

  /* Stage 3A — CRT-beam (Lottes / Blur Busters 2024).
     Combines a rolling-raster beam + phosphor afterglow + slot-mask color
     pattern into one pass. Designed for the "monitor mode" look — gives
     each frame a faint bright band that sweeps the screen + an aperture-
     grille tint pattern that mimics RGB stripe phosphors at sub-pixel
     scale. State carries the rolling-beam phase between frames so the
     band moves smoothly. */
  function applyCrtBeam(rgba, w, h, state, opts) {
    const intensity = opts.intensity != null ? opts.intensity : 0.45;
    const beamW = (opts.beamWidth != null ? opts.beamWidth : 0.18) * h; // half-band in px
    const speed = opts.speed != null ? opts.speed : 1.0;
    const slotMask = opts.slotMask !== false; // default on
    const slotStrength = opts.slotStrength != null ? opts.slotStrength : 0.20;

    if (!state) state = { beamY: 0 };
    state.beamY = (state.beamY + speed * h * 0.04) % (h + beamW * 4);

    const beamCenter = state.beamY - beamW * 2;
    for (let y = 0; y < h; y++) {
      // Gaussian falloff from the beam center.
      const dy = (y - beamCenter) / beamW;
      const g = Math.exp(-dy * dy);
      const yMul = 1 + g * intensity;          // bright crest
      const yPenum = 1 - Math.max(0, dy) * 0.05 * intensity; // mild trail dimming
      const rowMul = yMul * Math.max(0.6, yPenum);
      const rowOff = y * w * 4;
      for (let x = 0; x < w; x++) {
        const i = rowOff + x * 4;
        let r = d_at(rgba, i)     / 255;
        let g_ = d_at(rgba, i + 1) / 255;
        let b = d_at(rgba, i + 2) / 255;
        if (slotMask) {
          // Aperture-grille triad: each sub-column biases one channel.
          const phase = x % 3;
          const rW = phase === 0 ? 1 + slotStrength : 1 - slotStrength * 0.5;
          const gW = phase === 1 ? 1 + slotStrength : 1 - slotStrength * 0.5;
          const bW = phase === 2 ? 1 + slotStrength : 1 - slotStrength * 0.5;
          r *= rW; g_ *= gW; b *= bW;
        }
        r *= rowMul; g_ *= rowMul; b *= rowMul;
        rgba[i]     = Math.max(0, Math.min(255, r * 255 | 0));
        rgba[i + 1] = Math.max(0, Math.min(255, g_ * 255 | 0));
        rgba[i + 2] = Math.max(0, Math.min(255, b * 255 | 0));
      }
    }
    return state;
  }
  function d_at(rgba, i) { return rgba[i]; }

  /* =========================================================================
     KAWAII OVERLAY
     A postprocess stage that scatters soft pink hearts ♥ and sparkles ✦ on
     top of the rendered glyph grid. Particles are deterministic per-cell
     (hashed by position + frameIdx) so they don't strobe between frames;
     a per-particle phase makes them twinkle smoothly. Heart count, sparkle
     count, hue, and twinkle speed are all tunable.

     Implementation: each particle is a tiny pre-baked bitmap pattern blitted
     directly into the canvas RGBA buffer. No font dependency; works with
     any source image and any palette. Drawn LAST in the postprocess chain
     so vignette + crtBeam don't darken the kawaii overlay.
     ========================================================================= */
  // 8x8 heart pattern (1 = filled)
  const KAWAII_HEART = new Uint8Array([
    0,1,1,0,0,1,1,0,
    1,1,1,1,1,1,1,1,
    1,1,1,1,1,1,1,1,
    1,1,1,1,1,1,1,1,
    0,1,1,1,1,1,1,0,
    0,0,1,1,1,1,0,0,
    0,0,0,1,1,0,0,0,
    0,0,0,0,1,0,0,0,
  ]);
  // 5x5 four-point sparkle (cross with center bright spot)
  const KAWAII_SPARKLE = new Uint8Array([
    0,0,1,0,0,
    0,0,2,0,0,
    1,2,3,2,1,
    0,0,2,0,0,
    0,0,1,0,0,
  ]);
  // 3x3 small twinkle (just a plus)
  const KAWAII_TWINKLE = new Uint8Array([
    0,1,0,
    1,2,1,
    0,1,0,
  ]);

  /* Lightweight integer hash used for deterministic particle placement. */
  function _kawaiiHash(a, b) {
    let h = (a | 0) ^ Math.imul(b | 0, 0x9e3779b1);
    h ^= h >>> 16;
    h = Math.imul(h, 0x7feb352d);
    h ^= h >>> 15;
    h = Math.imul(h, 0x846ca68b);
    h ^= h >>> 16;
    return h >>> 0;
  }

  /* Blit a kawaii pattern into rgba at (px, py) with given color + alpha.
     The pattern's nonzero cells get blended on top of the existing pixel. */
  function _kawaiiBlit(rgba, w, h, pattern, pw, ph, px, py, r, g, b, alpha) {
    const ox = (px - (pw >> 1)) | 0;
    const oy = (py - (ph >> 1)) | 0;
    for (let dy = 0; dy < ph; dy++) {
      const yy = oy + dy;
      if (yy < 0 || yy >= h) continue;
      for (let dx = 0; dx < pw; dx++) {
        const v = pattern[dy * pw + dx];
        if (v === 0) continue;
        const xx = ox + dx;
        if (xx < 0 || xx >= w) continue;
        // intensity 1..3 → opacity boost
        const a = alpha * (v / 3);
        const i = (yy * w + xx) * 4;
        rgba[i]     = Math.min(255, rgba[i]     * (1 - a) + r * a);
        rgba[i + 1] = Math.min(255, rgba[i + 1] * (1 - a) + g * a);
        rgba[i + 2] = Math.min(255, rgba[i + 2] * (1 - a) + b * a);
      }
    }
  }

  function applyKawaii(rgba, w, h, opts, frameIdx) {
    const intensity   = opts.intensity   != null ? opts.intensity   : 0.85;
    const heartCount  = opts.heartCount  != null ? opts.heartCount  : 12;
    const sparkleCount= opts.sparkleCount!= null ? opts.sparkleCount: 28;
    const twinkleCount= opts.twinkleCount!= null ? opts.twinkleCount: 60;
    const speed       = opts.speed       != null ? opts.speed       : 1.0;
    const hueR = opts.hueR != null ? opts.hueR : 255;
    const hueG = opts.hueG != null ? opts.hueG : 105;
    const hueB = opts.hueB != null ? opts.hueB : 180;

    /* Hearts — slow phase, big amplitude. Position drifts linearly so they
       glide gently across the canvas instead of jittering. */
    for (let i = 0; i < heartCount; i++) {
      const seedX = _kawaiiHash(i, 0xBEEF);
      const seedY = _kawaiiHash(i, 0xCAFE);
      // Drift slowly: position = seed + frameIdx * driftSpeed
      const dvx = ((_kawaiiHash(i, 0x1A1A) % 1000) / 1000 - 0.5) * 0.6;
      const dvy = ((_kawaiiHash(i, 0x2B2B) % 1000) / 1000 - 0.5) * 0.4;
      const px = ((seedX % w) + frameIdx * dvx + w * 1000) % w;
      const py = ((seedY % h) + frameIdx * dvy + h * 1000) % h;
      // Slow twinkle cycle, never fully off
      const phase = (frameIdx * speed * 0.06 + i * 0.5);
      const tw = Math.sin(phase) * 0.5 + 0.65;
      const a = Math.max(0, Math.min(1, intensity * tw));
      _kawaiiBlit(rgba, w, h, KAWAII_HEART, 8, 8, px | 0, py | 0, hueR, hueG, hueB, a);
    }

    /* Sparkles — medium count, faster twinkle. Use the brighter "4-point"
       pattern; lighter pink hue. */
    const spR = Math.min(255, hueR + 25);
    const spG = Math.min(255, hueG + 60);
    const spB = Math.min(255, hueB + 40);
    for (let i = 0; i < sparkleCount; i++) {
      const seedX = _kawaiiHash(i, 0xDEAD);
      const seedY = _kawaiiHash(i, 0xFACE);
      const px = seedX % w;
      const py = seedY % h;
      const phase = (frameIdx * speed * 0.18 + i * 0.7);
      const tw = Math.sin(phase) * 0.5 + 0.5;
      const a = Math.max(0, Math.min(1, intensity * tw * 0.9));
      _kawaiiBlit(rgba, w, h, KAWAII_SPARKLE, 5, 5, px | 0, py | 0, spR, spG, spB, a);
    }

    /* Tiny twinkles — many, fastest. White-ish, brief flashes. */
    const twR = 255, twG = 240, twB = 250;
    for (let i = 0; i < twinkleCount; i++) {
      const seedX = _kawaiiHash(i, 0xC0DE);
      const seedY = _kawaiiHash(i, 0xBABE);
      const px = seedX % w;
      const py = seedY % h;
      const phase = (frameIdx * speed * 0.32 + i * 1.1);
      const tw = Math.sin(phase) * 0.5 + 0.5;
      // Make them flicker — clamp at 0.4 floor for visibility
      const a = Math.max(0, Math.min(1, intensity * Math.pow(tw, 2.5)));
      if (a > 0.05) {
        _kawaiiBlit(rgba, w, h, KAWAII_TWINKLE, 3, 3, px | 0, py | 0, twR, twG, twB, a);
      }
    }
  }

  /* Pre-roll: advance stateful stages N frames before recording (CR-8). */
  function prerollFrames(n, capture, state, postOpts, runtime) {
    for (let i = 0; i < n; i++) {
      const img = capture(i);
      if (!img) return;
      applyChain(img, state, postOpts, runtime);
    }
  }

  const api = Object.freeze({
    makeState: makeState,
    applyPhosphorDecay: applyPhosphorDecay,
    applyBloom: applyBloom,
    applyScanlines: applyScanlines,
    applyChromaticAberration: applyChromaticAberration,
    applyBarrel: applyBarrel,
    applyVignette: applyVignette,
    applyGodRays: applyGodRays,
    applyLetterbox: applyLetterbox,
    applyChain: applyChain,
    prerollFrames: prerollFrames,
    boxBlurLinear: boxBlurLinear,
  });

  const root = (typeof window !== 'undefined') ? window
             : (typeof globalThis !== 'undefined') ? globalThis
             : this;
  root.GlyphGrid = root.GlyphGrid || {};
  root.GlyphGrid.crt = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})();
