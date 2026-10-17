'use strict';
/* ITER-075: unit tests for src/lib/glyph-timeline.js (pure, no p5).
   Run: node tests/glyph-timeline.test.js (also required by
   frontend-contracts.test.js, so `npm test` covers it). */

const assert = require('assert');
const fs = require('fs');
const path = require('path');
const TL = require('../src/lib/glyph-timeline.js');
const morph = require('../src/lib/glyph-palette-morph.js');

const near = (a, b, eps, msg) => assert(Math.abs(a - b) <= (eps || 1e-9), msg + ': ' + a + ' vs ' + b);

/* easing */
assert.deepStrictEqual(TL.EASES, ['linear', 'inOutSine', 'step']);
near(TL.ease('linear', 0.25), 0.25, 0, 'linear');
near(TL.ease('inOutSine', 0), 0, 1e-12, 'inOutSine(0)');
near(TL.ease('inOutSine', 0.5), 0.5, 1e-12, 'inOutSine(0.5)');
near(TL.ease('inOutSine', 1), 1, 1e-12, 'inOutSine(1)');
assert(TL.ease('inOutSine', 0.25) < 0.25, 'inOutSine is slow out of the start');
assert.strictEqual(TL.ease('step', 0.999), 0, 'step holds until the key');
assert.strictEqual(TL.ease('step', 1), 1);
near(TL.ease('linear', 2), 1, 0, 'k clamps');
near(TL.ease('nope', 0.3), 0.3, 0, 'unknown ease = linear');

/* numeric lerp, holds outside the keys, unsorted input */
const gain = [{ u: 0.5, v: 0.6, ease: 'inOutSine' }, { u: 0.1, v: 0.1 }];
near(TL.evaluateTrack('studio.breathing.gainSwing', gain, 0), 0.1, 0, 'holds first before u0');
near(TL.evaluateTrack('studio.breathing.gainSwing', gain, 0.9), 0.6, 0, 'holds last after uN');
near(TL.evaluateTrack('studio.breathing.gainSwing', gain, 0.3), 0.35, 1e-12, 'inOutSine midpoint');
near(TL.evaluateTrack('x', [{ u: 0, v: 0 }, { u: 1, v: 10 }], 0.25), 2.5, 1e-12, 'linear default');
near(TL.evaluateTrack('x', [{ u: 0, v: 0 }, { u: 0.5, v: 10 }, { u: 1, v: 0 }], 0.75), 5, 1e-12, '3 keys, 2nd segment');
assert.strictEqual(TL.evaluateTrack('x', [{ u: 0, v: 0 }, { u: 0.5, v: 10, ease: 'step' }], 0.49), 0, 'step holds');
assert.strictEqual(TL.evaluateTrack('x', [{ u: 0, v: 0 }, { u: 0.5, v: 10, ease: 'step' }], 0.5), 10, 'step lands');
assert.strictEqual(TL.evaluateTrack('x', [], 0.5), undefined, 'empty track');
assert.strictEqual(TL.evaluateTrack('x', [{ u: 'a', v: 1 }], 0.5), undefined, 'malformed keys dropped');

/* structural keys are always step, whatever the ease says */
for (const p of TL.STRUCTURAL) {
  const keys = [{ u: 0, v: p.startsWith('grid') ? 60 : 'a' }, { u: 0.4, v: p.startsWith('grid') ? 240 : 'b', ease: 'linear' }];
  assert.strictEqual(TL.evaluateTrack(p, keys, 0.39), keys[0].v, p + ' holds before the key');
  assert.strictEqual(TL.evaluateTrack(p, keys, 0.4), keys[1].v, p + ' steps at the key');
}
assert.deepStrictEqual(TL.STRUCTURAL, ['grid.cols', 'grid.rows', 'glyphSet', 'selectionMode', 'dither.mode']);
/* non-numeric values step too */
assert.strictEqual(TL.evaluateTrack('invertSignal', [{ u: 0, v: false }, { u: 1, v: true }], 0.9), false);

/* palette: names at the ends, a { from, to, k } pair between */
const pal = [{ u: 0, v: 'cream-paper' }, { u: 1, v: 'spice' }];
assert.strictEqual(TL.evaluateTrack('palette', pal, 0), 'cream-paper');
assert.strictEqual(TL.evaluateTrack('palette', pal, 1), 'spice');
assert.deepStrictEqual(TL.evaluateTrack('palette', pal, 0.5), { from: 'cream-paper', to: 'spice', k: 0.5 });

/* OKLab blend: endpoints exact, midpoint is the OKLab average (not sRGB) */
const PALETTES = {
  'cream-paper': { bg: '#E8DDC8', inks: ['#0E0805'] },
  spice: { bg: '#0A0302', inks: ['#FFB347', '#E85D1A', '#FFE4A8', '#9C4A16'] },
  mono: { bg: '#000000', inks: ['#FFFFFF'] },
};
const b0 = TL.blendPalettes(PALETTES['cream-paper'], PALETTES.spice, 0);
assert.strictEqual(b0.bg, '#e8ddc8');
assert.deepStrictEqual(b0.inks, ['#0e0805', '#0e0805', '#0e0805', '#0e0805'], 'shorter palette rank-pads');
const b1 = TL.blendPalettes(PALETTES['cream-paper'], PALETTES.spice, 1);
assert.deepStrictEqual([b1.bg].concat(b1.inks), ['#0a0302', '#ffb347', '#e85d1a', '#ffe4a8', '#9c4a16']);
const bm = TL.blendPalettes(PALETTES.mono, PALETTES.mono, 0.5);
assert.deepStrictEqual(bm, { bg: '#000000', inks: ['#ffffff'] }, 'same palette blends to itself');
const half = TL.blendPalettes({ bg: '#000000', inks: ['#000000'] }, { bg: '#ffffff', inks: ['#ffffff'] }, 0.5);
const L = morph.parse([half.bg])[0].oklab[0];
near(L, 0.5, 0.01, 'black→white midpoint is OKLab L=0.5');
assert.notStrictEqual(half.bg, '#808080', 'not the sRGB midpoint');

/* frameConfig: copy-on-write, base never written */
const base = {
  palette: 'cream-paper',
  grid: { cols: 60, rows: 30 },
  studio: { enabled: true, breathing: { gainSwing: 0.3, jitter: 0.05 } },
  postprocess: { vignette: { enabled: true } },
  timeline: {
    enabled: true,
    tracks: {
      'studio.breathing.gainSwing': [{ u: 0, v: 0 }, { u: 1, v: 1 }],
      'grid.cols': [{ u: 0, v: 60 }, { u: 0.5, v: 120 }],
      palette: pal,
      'new.block.key': [{ u: 0, v: 7 }],
    },
  },
};
const snapshot = JSON.stringify(base);
const fr = TL.frameConfig(base, 0.75, PALETTES);
assert.strictEqual(JSON.stringify(base), snapshot, 'base untouched');
near(fr.config.studio.breathing.gainSwing, 0.75, 1e-12, 'numeric track applied');
assert.strictEqual(fr.config.studio.breathing.jitter, 0.05, 'sibling keys kept');
assert.strictEqual(fr.config.studio.enabled, true);
assert.notStrictEqual(fr.config.studio, base.studio, 'tracked path cloned');
assert.strictEqual(fr.config.grid.cols, 120, 'structural step applied');
assert.strictEqual(fr.config.grid.rows, 30);
assert.strictEqual(fr.config.postprocess, base.postprocess, 'untouched blocks are shared');
assert.strictEqual(fr.config.new.block.key, 7, 'missing parents are created');
assert(fr.palette && /^__timeline:/.test(fr.config.palette), 'synthetic palette name');
assert.deepStrictEqual(fr.palette, TL.blendPalettes(PALETTES['cream-paper'], PALETTES.spice, 0.75));
assert.strictEqual(fr.config.palette, '__timeline:' + fr.palette.bg + ':' + fr.palette.inks.join(','),
  'name encodes the colours (cache keys follow the blend)');
/* deterministic */
assert.deepStrictEqual(TL.frameConfig(base, 0.75, PALETTES), fr);
/* palette end keys resolve to the real name, no synthetic */
const fe = TL.frameConfig(base, 1, PALETTES);
assert.strictEqual(fe.config.palette, 'spice');
assert.strictEqual(fe.palette, null);
/* unknown palette name holds the base palette */
const bad = TL.frameConfig({ palette: 'mono', timeline: { enabled: true, tracks: { palette: [{ u: 0, v: 'nope' }, { u: 1, v: 'spice' }] } } }, 0.5, PALETTES);
assert.strictEqual(bad.config.palette, 'mono');
assert.strictEqual(bad.palette, null);

/* active() */
assert.strictEqual(TL.active(base), true);
assert.strictEqual(TL.active({ timeline: { enabled: false, tracks: base.timeline.tracks } }), false);
assert.strictEqual(TL.active({ timeline: { enabled: true, tracks: {} } }), false, 'no tracks = off');
assert.strictEqual(TL.active({}), false);

/* the schema's ease enum is this module's (gen-schema reads it) */
const schema = JSON.parse(fs.readFileSync(path.join(__dirname, '..', 'src', 'glyph-grid.schema.json'), 'utf8'));
assert.deepStrictEqual(schema.properties.timeline.properties.tracks.additionalProperties.items.properties.ease.enum, TL.EASES);

console.log('glyph-timeline: ok');
