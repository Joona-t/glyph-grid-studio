'use strict';
/* ITER-076: unit tests for src/lib/glyph-text-source.js (parsers, layout,
   composite, stroke estimate).  Run: node tests/glyph-text-source.test.js
   (also required by frontend-contracts.test.js, so `npm test` covers it). */

const assert = require('assert');
const TX = require('../src/lib/glyph-text-source.js');

const near = (a, b, eps, msg) => assert(Math.abs(a - b) <= (eps || 1e-9), msg + ': ' + a + ' vs ' + b);

/* enums the schema reads */
assert.deepStrictEqual(TX.ANIMS, ['none', 'typewriter', 'rise', 'fade', 'scale']);
assert.deepStrictEqual(TX.BLENDS, ['replace', 'multiply', 'screen', 'mask']);

/* ---- SRT ---- */
const srt = '﻿1\r\n00:00:01,000 --> 00:00:02,500\r\n<i>Hello</i>\r\nworld\r\n\r\n'
  + '2\n00:00:03.25 --> 00:00:04,000 X1:10 X2:20\n{\\an8}Second\n\n'
  + '3\n00:00:05,000 --> 00:00:06,000\n   \n\n'          // empty body: dropped
  + 'garbage block\n\n'
  + '00:01:00,000 --> 00:01:01,000\nno index\n';
const cues = TX.parseSRT(srt);
assert.deepStrictEqual(cues, [
  { text: 'Hello\nworld', start: 1, end: 2.5 },
  { text: 'Second', start: 3.25, end: 4 },
  { text: 'no index', start: 60, end: 61 },
]);
assert.throws(() => TX.parseSRT('nothing here'), /no cues/);
assert.strictEqual(TX.lastEnd(cues), 61);
assert.strictEqual(TX.lastEnd([{ text: 'a', start: 0 }]), 0, 'open-ended lines do not set a duration');

/* ---- LRC ---- */
const lrc = '[ar:Someone]\n[offset:+500]\n[00:01.00]first\n[00:03.50][00:09.00]chorus <00:03.80>word\n'
  + '[00:05.00]\n[00:06:25]colon centis\n';
const lc = TX.parseLRC(lrc);
assert.deepStrictEqual(lc, [
  { text: 'first', start: 0.5, end: 3 },
  { text: 'chorus word', start: 3, end: 4.5 },          // ends at the empty [00:05] row
  { text: 'colon centis', start: 5.75, end: 8.5 },
  { text: 'chorus word', start: 8.5, end: 12.5 },        // last cue: +4 s hold
]);
near(TX.parseLRC('[length: 0:10]\n[00:02.00]x')[0].end, 10, 1e-9, '[length:] ends the last cue');
assert.throws(() => TX.parseLRC('[ar:x]\nplain'), /no timed/);
assert.strictEqual(TX.parseCues('[00:01.00]a', 'song.lrc')[0].text, 'a');
assert.strictEqual(TX.parseCues('1\n00:00:01,000 --> 00:00:02,000\nb', '')[0].text, 'b', 'sniff SRT by -->');

/* ---- layout ---- */
const ts = { w: 1000, h: 500, size: 100, lines: [{ text: 'GLYPH', start: 0, anim: 'typewriter', easeIn: 1 }] };
const at = (t, s) => TX.layout(s || ts, t, 1000, 500);
assert.deepStrictEqual(at(0), [], 'typewriter frame 0 is empty');
assert.strictEqual(at(0.45)[0].text, 'GL', 'floor(5 × 0.45) chars');
assert.strictEqual(at(0.45)[0].full, 'GLYPH');
assert.strictEqual(at(5)[0].text, 'GLYPH');
assert.deepStrictEqual(at(5), at(9), 'settled title: identical state key');
assert.strictEqual(at(5)[0].x, 500); assert.strictEqual(at(5)[0].y, 250);
assert(/^700 100px "LoveSparkGlyphGrid"/.test(at(5)[0].font), 'bundled alias + block defaults: ' + at(5)[0].font);
/* size scales with canvas height vs ts.h (composite canvases) */
assert.strictEqual(TX.layout(ts, 5, 2000, 1000)[0].size, 200);
/* start/end window, fade envelope, quantisation */
const fade = { h: 500, lines: [{ text: 'x', start: 1, end: 3, anim: 'fade', easeIn: 1, easeOut: 1 }] };
assert.deepStrictEqual(at(0.99, fade), []);
assert.deepStrictEqual(at(3, fade), [], 'end is exclusive');
near(at(2, fade)[0].alpha, 1, 0, 'fully in between envelopes');
assert(at(1.5, fade)[0].alpha > 0.8 && at(1.5, fade)[0].alpha < 1, 'easeOutCubic in');
near(at(2.5, fade)[0].alpha, 0.502, 0.002, 'linear out');
assert.deepStrictEqual(at(1.5, fade), at(1.5 + 1 / 1024, fade), 'sub-1/255 progress steps share one state');
/* rise / scale / none */
const rise = { h: 500, lines: [{ text: 'r', anim: 'rise', size: 100, easeIn: 1 }] };
assert(at(0.1, rise)[0].y > 250 && at(5, rise)[0].y === 250, 'rise comes up from below');
const sc = { h: 500, lines: [{ text: 's', anim: 'scale', easeIn: 1 }] };
assert(at(0.1, sc)[0].scale < 1 && at(5, sc)[0].scale === 1, 'scale grows to 1');
const none = { h: 500, lines: [{ text: 'n' }] };
assert.strictEqual(at(0, none)[0].alpha, 1, 'none: hard cut');
assert.strictEqual(TX.isDynamic(none), false, 'static title');
assert.strictEqual(TX.isDynamic(ts), true);
assert.strictEqual(TX.isDynamic({ lines: [{ text: 'a', end: 2 }] }), true, 'a cue that ends is dynamic');
/* unknown anim/align fall back; system fonts get a sans fallback */
const odd = TX.layout({ h: 10, lines: [{ text: 'o', anim: 'bounce', align: 'up', font: 'Georgia', weight: 400, size: 10 }] }, 0, 10, 10)[0];
assert.strictEqual(odd.align, 'center');
assert.strictEqual(odd.font, '400 10px "Georgia", system-ui, -apple-system, sans-serif');

/* ---- drawOps: anchored typewriter rows, stub ctx ---- */
{
  const calls = [];
  const ctx = {
    save() {}, restore() {}, translate(x, y) { calls.push(['t', x, y]); }, scale() {},
    measureText(s) { return { width: s.length * 10 }; },
    fillText(s, x, y) { calls.push(['f', s, x, y, this.textAlign]); },
  };
  TX.drawOps(ctx, at(0.45), '#fff');
  assert.deepStrictEqual(calls[1], ['f', 'GL', -25, 0, 'left'], 'partial row starts where the centred full row starts');
  calls.length = 0;
  TX.drawOps(ctx, TX.layout({ h: 100, size: 10, lines: [{ text: 'a\nbb' }] }, 0, 100, 100), '#fff');
  assert.deepStrictEqual(calls.slice(1), [['f', 'a', 0, -5.75, 'center'], ['f', 'bb', 0, 5.75, 'center']], 'stacked rows');
}

/* ---- composite in linear light ---- */
{
  const mk = (r, g, b, a) => new Uint8ClampedArray([r, g, b, a]);
  /* full coverage, white fg: multiply leaves base, screen → white, mask → base */
  assert.deepStrictEqual(Array.from(TX.compositeLinear(mk(128, 64, 32, 255), mk(0, 0, 0, 255), 'multiply', [255, 255, 255], [0, 0, 0])), [128, 64, 32, 255]);
  assert.deepStrictEqual(Array.from(TX.compositeLinear(mk(128, 64, 32, 255), mk(0, 0, 0, 255), 'screen', [255, 255, 255], [0, 0, 0])), [255, 255, 255, 255]);
  assert.deepStrictEqual(Array.from(TX.compositeLinear(mk(128, 64, 32, 255), mk(0, 0, 0, 0), 'mask', [255, 255, 255], [10, 20, 30])), [10, 20, 30, 255], 'mask outside letters = bg');
  /* half coverage of black over white is the LINEAR midpoint (188), not 128 */
  const half = TX.compositeLinear(mk(255, 255, 255, 255), mk(0, 0, 0, 128), 'multiply', [0, 0, 0], [0, 0, 0]);
  assert(Math.abs(half[0] - 188) <= 1, 'linear-light edge: ' + half[0]);
  assert.deepStrictEqual(TX.hexRgb('#0f8'), [0, 255, 136]);
}

/* ---- stroke estimate ---- */
{
  /* 160 px bold at h=512 on a 1024-wide canvas with 240 cols: 22.4 px stem / 4.27 px cell */
  const c = TX.minStrokeCells({ h: 512, lines: [{ text: 'A' }] }, 1024, 512, 1024, 240);
  near(c, 160 * 0.14 / (1024 / 240), 1e-9, 'stroke cells');
  assert(TX.minStrokeCells({ h: 512, lines: [{ text: 'A', size: 12, weight: 400 }] }, 1024, 512, 1024, 240) < TX.MIN_STROKE_CELLS);
  assert.strictEqual(TX.minStrokeCells({ lines: [] }, 1, 1, 1, 1), Infinity);
}

console.log('glyph-text-source: ok');
