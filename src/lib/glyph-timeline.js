/* glyph-timeline.js — R2 (ITER-075): keyframe tracks on any CONFIG path.
 *
 *   CONFIG.timeline = {
 *     enabled: true,
 *     tracks: {
 *       'studio.breathing.gainSwing': [{ u: 0, v: 0.1 }, { u: 0.5, v: 0.6, ease: 'inOutSine' }],
 *       'palette':                    [{ u: 0, v: 'cream-paper' }, { u: 1, v: 'spice' }],
 *       'grid.cols':                  [{ u: 0, v: 60 }, { u: 0.4, v: 240 }],
 *     },
 *   }
 *
 * u is loop position in [0, 1] (t / animation.duration, the dispersal
 * convention).  Before the first key a track holds the first value, after
 * the last it holds the last.  A key's `ease` shapes the segment ARRIVING at
 * it (default 'linear'); 'step' holds the previous value until u reaches the
 * key.  Numeric values lerp; the 'palette' track blends the two palettes'
 * bg + inks in OKLab (glyph-palette-morph.js) into a synthetic palette;
 * structural keys (STRUCTURAL) and every non-numeric value are always step.
 *
 * Pure: no p5, no DOM.  The host (index.html draw()) calls frameConfig()
 * once per frame and points the renderer at the returned shadow CONFIG —
 * the Tweakpane-bound base object is never written.
 *
 * API:
 *   GlyphGrid.timeline.ease(name, k)                 → eased k
 *   GlyphGrid.timeline.evaluateTrack(path, keys, u)  → value | { from, to, k } (palette)
 *   GlyphGrid.timeline.evaluate(tracks, u)           → { path: value }
 *   GlyphGrid.timeline.blendPalettes(A, B, k)        → { bg, inks } (hex)
 *   GlyphGrid.timeline.frameConfig(base, u, palettes) → { config, palette }
 *   GlyphGrid.timeline.active(config)                → bool
 */

(function () {
  'use strict';

  const root = (typeof window !== 'undefined') ? window
             : (typeof globalThis !== 'undefined') ? globalThis
             : this;

  /* Order is the schema enum (scripts/gen-schema.js reads it). */
  const EASES = Object.freeze(['linear', 'inOutSine', 'step']);
  /* Keys that reallocate grid buffers or swap the selection machinery: a
     fractional value means nothing, so they never interpolate. */
  const STRUCTURAL = Object.freeze(['grid.cols', 'grid.rows', 'glyphSet', 'selectionMode', 'dither.mode']);
  const PALETTE_PATH = 'palette';

  function morph() {
    if (root.GlyphGrid && root.GlyphGrid.paletteMorph) return root.GlyphGrid.paletteMorph;
    if (typeof require === 'function') return require('./glyph-palette-morph.js');
    throw new Error('glyph-timeline: glyph-palette-morph.js is not loaded');
  }

  function ease(name, k) {
    k = k <= 0 ? 0 : (k >= 1 ? 1 : k);
    switch (name) {
      case 'step': return k >= 1 ? 1 : 0;
      case 'inOutSine': return 0.5 - 0.5 * Math.cos(Math.PI * k);
      default: return k;   /* linear */
    }
  }

  function sortedKeys(keys) {
    if (!Array.isArray(keys)) return [];
    const out = [];
    for (let i = 0; i < keys.length; i++) {
      const kf = keys[i];
      if (kf && typeof kf.u === 'number' && isFinite(kf.u) && 'v' in kf) out.push(kf);
    }
    /* Stable by u: equal-u keys keep authoring order (the later one wins
       once u reaches them — an instant jump). */
    return out.map(function (kf, i) { return [kf, i]; })
      .sort(function (a, b) { return (a[0].u - b[0].u) || (a[1] - b[1]); })
      .map(function (p) { return p[0]; });
  }

  /* One track at u.  Returns the value; for the palette track a pair
     { from, to, k } when strictly between two different palettes. */
  function evaluateTrack(path, keys, u) {
    const ks = sortedKeys(keys);
    if (!ks.length) return undefined;
    if (u <= ks[0].u) return ks[0].v;
    const last = ks[ks.length - 1];
    if (u >= last.u) return last.v;
    let i = 0;
    while (i < ks.length - 2 && u >= ks[i + 1].u) i++;
    const a = ks[i], b = ks[i + 1];
    const span = b.u - a.u;
    const raw = span > 0 ? (u - a.u) / span : 1;
    const structural = STRUCTURAL.indexOf(path) >= 0;
    const name = structural ? 'step' : (b.ease || 'linear');
    const k = ease(name, raw);
    if (k <= 0) return a.v;
    if (k >= 1) return b.v;
    if (typeof a.v === 'number' && typeof b.v === 'number') return a.v + (b.v - a.v) * k;
    if (path === PALETTE_PATH && typeof a.v === 'string' && typeof b.v === 'string' && a.v !== b.v) {
      return { from: a.v, to: b.v, k: k };
    }
    return a.v;   /* booleans, strings, objects: step */
  }

  function evaluate(tracks, u) {
    const out = {};
    if (!tracks || typeof tracks !== 'object') return out;
    for (const path in tracks) {
      const v = evaluateTrack(path, tracks[path], u);
      if (v !== undefined) out[path] = v;
    }
    return out;
  }

  /* bg and inks blended rank-paired in OKLab; the shorter ink list is
     padded with its last ink (paletteMorph.blend's EC-8 rule). */
  function blendPalettes(A, B, k) {
    const m = morph();
    const bg = m.blend(m.parse([A.bg]), m.parse([B.bg]), k)[0];
    const inks = m.blend(m.parse(A.inks), m.parse(B.inks), k);
    return { bg: m.toHex(bg.srgb), inks: inks.map(function (e) { return m.toHex(e.srgb); }) };
  }

  function active(config) {
    const tl = config && config.timeline;
    if (!tl || !tl.enabled || !tl.tracks || typeof tl.tracks !== 'object') return false;
    for (const _p in tl.tracks) return true;
    return false;
  }

  /* Copy-on-write along each tracked path: the shadow shares every
     untouched sub-object with `base` (so per-frame side effects on them —
     the XDoG sigma sweep — land where they always did), and clones only
     the objects on a tracked path.  `palette` in the result is a
     synthetic { bg, inks } the host must register under config.palette's
     name, or null when no blend is in progress. */
  function frameConfig(base, u, palettes) {
    const values = evaluate(base.timeline && base.timeline.tracks, u);
    const out = Object.assign({}, base);
    const cloned = new Set();
    let palette = null;
    for (const path in values) {
      let v = values[path];
      if (path === PALETTE_PATH) {
        if (v && typeof v === 'object') {
          const A = palettes && palettes[v.from], B = palettes && palettes[v.to];
          if (!A || !B) continue;            /* unknown name: hold the base palette */
          palette = blendPalettes(A, B, v.k);
          v = '__timeline:' + palette.bg + ':' + palette.inks.join(',');
        } else if (typeof v !== 'string' || !(palettes && palettes[v])) {
          continue;
        }
      }
      const keys = path.split('.');
      let node = out;
      let prefix = '';
      for (let i = 0; i < keys.length - 1; i++) {
        prefix += (i ? '.' : '') + keys[i];
        const cur = node[keys[i]];
        if (!cloned.has(prefix)) {
          node[keys[i]] = (cur && typeof cur === 'object' && !Array.isArray(cur)) ? Object.assign({}, cur) : {};
          cloned.add(prefix);
        }
        node = node[keys[i]];
      }
      node[keys[keys.length - 1]] = v;
    }
    return { config: out, palette: palette };
  }

  const api = Object.freeze({
    EASES: EASES,
    STRUCTURAL: STRUCTURAL,
    ease: ease,
    evaluateTrack: evaluateTrack,
    evaluate: evaluate,
    blendPalettes: blendPalettes,
    frameConfig: frameConfig,
    active: active,
  });

  root.GlyphGrid = root.GlyphGrid || {};
  root.GlyphGrid.timeline = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})();
