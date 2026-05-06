/* glyph-compat.js — SKILL_VERSION + CONFIG.compat gate.

   The v1→v2 upgrade adds eleven new pipeline stages. Each one costs cycles
   and can, in subtle ways, perturb pixels. The eight frozen portfolio
   pieces were shot against a specific pipeline; reopening one with the new
   template must produce byte-identical output at frame 0 and ΔE94 < 1 over
   the loop. Without a single switch that says "old pipeline only", every
   new stage would need its own default-off guard and every test would need
   to check the union.

   CONFIG.compat is that single switch. When "v1", every stage-gate returns
   false before the stage runs. When "v2", stages consult their own fields.
   The old defaults (samplingStrategy: 'average', colorMode: 'preserve', etc.)
   are preserved exactly; the new fields are additive.

   This file does not execute any stage. It is policy, not mechanism. */

(function () {
  'use strict';

  const SKILL_VERSION = '2.0.0';
  const COMPAT_VERSIONS = Object.freeze(['v1', 'v2']);
  const DEFAULT_COMPAT = 'v2';

  function normalizeCompat(config) {
    const raw = config && config.compat;
    if (raw === 'v1' || raw === 'v2') return raw;
    return DEFAULT_COMPAT;
  }

  function isV1(config) { return normalizeCompat(config) === 'v1'; }
  function isV2(config) { return normalizeCompat(config) === 'v2'; }

  /* Stage gates — each returns true only if v2 AND the stage's own field
     is enabled. A stage that short-circuits under v1 must consume no random
     state, no palette state, no side effects. Determinism depends on it. */

  function gateShapeSelection(config) {
    if (isV1(config)) return false;
    const mode = config.selectionMode;
    return mode === 'shape' || mode === 'shape-edge-aware';
  }

  function gateEdgeDirectional(config) {
    if (isV1(config)) return false;
    const mode = config.selectionMode;
    return mode === 'edge-directional' || mode === 'shape-edge-aware';
  }

  function gateDither(config) {
    if (isV1(config)) return false;
    const d = config.dither;
    if (!d || d === 'none' || d.mode === 'none') return false;
    return true;
  }

  function gatePrefilter(config) {
    if (isV1(config)) return false;
    const p = config.prefilter;
    if (!p || p === 'none' || p.mode === 'none') return false;
    return true;
  }

  function gatePostprocess(config) {
    if (isV1(config)) return false;
    const p = config.postprocess;
    if (!p) return false;
    if (p.crt && p.crt.enabled) return true;
    if (p.bloom && p.bloom.enabled) return true;
    if (p.scanlines && p.scanlines.enabled) return true;
    if (p.chromaticAberration && p.chromaticAberration.enabled) return true;
    if (p.phosphorDecay && p.phosphorDecay.enabled) return true;
    if (p.halation && p.halation.enabled) return true;
    if (p.vignette && p.vignette.enabled) return true;
    if (p.barrel && p.barrel.enabled) return true;
    if (p.godRays && p.godRays.enabled) return true;
    if (p.letterbox && p.letterbox.enabled) return true;
    return false;
  }

  function gatePaletteMorph(config) {
    if (isV1(config)) return false;
    return !!(config.paletteMorph && config.paletteMorph.enabled);
  }

  /* v2.1 — scene provides a depth p5.Graphics via `{ depth }` return.
     The engine downsamples to cellDepth and exposes it to fog / rays. */
  function gateDepth(config) {
    if (isV1(config)) return false;
    return !!(config.depth && config.depth.enabled);
  }

  /* v2.1 — per-cell color lerp toward a fog colour by cellDepth. */
  function gateDepthFog(config) {
    if (isV1(config)) return false;
    if (!gateDepth(config)) return false;
    const pp = config.postprocess;
    return !!(pp && pp.depthFog && pp.depthFog.enabled);
  }

  /* v2.1 — scene author supplies rectangles to re-sample at finer grid
     density so tiny features (figures, faces) survive 8x8 averaging. */
  function gateSalientROIs(config) {
    if (isV1(config)) return false;
    const rois = config.grid && config.grid.salientROIs;
    return Array.isArray(rois) && rois.length > 0;
  }

  /* Selection mode resolution — when unspecified under v2, default to
     the classic "brightness" path (one-to-one with v1 behavior). */
  function resolveSelectionMode(config) {
    if (isV1(config)) return 'brightness';
    return config.selectionMode || 'brightness';
  }

  /* Validators — called at setup() to fail loudly on known bad combos.
     Each returns an array of { level: 'warn' | 'error', code, message }. */
  function validate(config) {
    const issues = [];
    if (isV1(config)) return issues;

    const mode = resolveSelectionMode(config);
    const dith = config.dither && (config.dither.mode || config.dither);
    const pref = config.prefilter && (config.prefilter.mode || config.prefilter);

    if (dith && mode !== 'brightness' && dith !== 'source-prefilter') {
      issues.push({
        level: 'warn',
        code: 'CR-6',
        message: 'Dithering applies only when selectionMode is "brightness". ' +
                 'Under "' + mode + '" mode, dither is ignored. Use dither.mode ' +
                 '"source-prefilter" to apply dither before shape encoding.',
      });
    }

    if (pref && (pref === 'xdog' || (typeof pref === 'string' && pref.indexOf('xdog') === 0))
        && mode === 'shape') {
      issues.push({
        level: 'warn',
        code: 'CR-7',
        message: 'XDoG output is near-binary; shape-vector selection degenerates. ' +
                 'Recommend selectionMode: "shape-edge-aware" when prefilter is XDoG.',
      });
    }

    return issues;
  }

  const api = Object.freeze({
    SKILL_VERSION: SKILL_VERSION,
    COMPAT_VERSIONS: COMPAT_VERSIONS,
    DEFAULT_COMPAT: DEFAULT_COMPAT,
    normalizeCompat: normalizeCompat,
    isV1: isV1,
    isV2: isV2,
    gateShapeSelection: gateShapeSelection,
    gateEdgeDirectional: gateEdgeDirectional,
    gateDither: gateDither,
    gatePrefilter: gatePrefilter,
    gatePostprocess: gatePostprocess,
    gatePaletteMorph: gatePaletteMorph,
    gateDepth: gateDepth,
    gateDepthFog: gateDepthFog,
    gateSalientROIs: gateSalientROIs,
    resolveSelectionMode: resolveSelectionMode,
    validate: validate,
  });

  const root = (typeof window !== 'undefined') ? window
             : (typeof globalThis !== 'undefined') ? globalThis
             : this;
  root.GlyphGrid = root.GlyphGrid || {};
  root.GlyphGrid.compat = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})();
