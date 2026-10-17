'use strict';
/* ITER-074: unit tests for src/lib/glyph-text-export.js (pure, no p5) and
   the CR-8 compat warning.  Fixtures are deterministic GlyphFrame snapshots.
   Run: node tests/glyph-text-export.test.js (also required by
   frontend-contracts.test.js, so `npm test` covers it). */

const assert = require('assert');
const vm = require('vm');
const T = require('../src/lib/glyph-text-export.js');
const compat = require('../src/lib/glyph-compat.js');

/* cols x rows snapshot: cell i gets cp from pick(i), fg rgb from ink(i),
   alpha from al(i). */
function snap(cols, rows, pick, ink, al, bg) {
  const n = cols * rows;
  const s = {
    cols, rows, cp: new Uint32Array(n), fg: new Uint8Array(n * 3), alpha: new Uint8Array(n),
    bg: bg || '#102030',
  };
  for (let i = 0; i < n; i++) {
    s.cp[i] = pick(i);
    s.fg.set(ink(i), i * 3);
    s.alpha[i] = al ? al(i) : 255;
  }
  return s;
}
const ESC = '\x1b[';
const OCT = 0x1CD00;
const count = (str, sub) => str.split(sub).length - 1;

/* A 6x3 frame: row 0 "AB  CD" in one colour, row 1 octant + '<' + blanks in
   two colours, row 2 all blank. */
const A = snap(6, 3, (i) => [0x41, 0x42, 0, 0x20, 0x43, 0x44, OCT, 0x3c, 0, 0, OCT + 1, 0, 0, 0, 0, 0, 0, 0][i],
  (i) => (i < 6 ? [200, 100, 50] : i < 8 ? [10, 20, 30] : [250, 250, 250]));

/* snapFromIr copies (grow-only buffers are longer than cols*rows) */
{
  const ir = { cols: 2, rows: 1, cp: new Uint32Array([0x41, 0x42, 0x43]), fg: new Uint8Array(9).fill(7),
    alpha: new Uint8Array([255, 128, 9]), bgHex: '#ABCDEF' };
  const s = T.snapFromIr(ir);
  assert.deepStrictEqual([s.cols, s.rows, s.cp.length, s.fg.length, s.alpha.length, s.bg], [2, 1, 2, 6, 2, '#ABCDEF']);
  ir.cp[0] = 0x5a;
  assert.strictEqual(s.cp[0], 0x41, 'snapshot is a copy, not a view');
}

/* TXT: exact rectangle, blanks as spaces, astral codepoints intact. */
{
  const txt = T.toTxt(A);
  assert.strictEqual(txt, 'AB  CD\n' + String.fromCodePoint(OCT) + '<  ' + String.fromCodePoint(OCT + 1) + ' \n      \n');
  const rows = txt.split('\n');
  assert.strictEqual(rows.pop(), '', 'trailing newline');
  assert.strictEqual(rows.length, A.rows);
  for (const r of rows) assert.strictEqual(Array.from(r).length, A.cols, 'every row is cols codepoints');
  const control = snap(2, 1, (i) => [0x07, 0x9b][i], () => [0, 0, 0]);
  assert.strictEqual(T.toTxt(control), '  \n', 'control characters never reach the file');
  const clear = snap(2, 1, () => 0x41, () => [0, 0, 0], () => 0);
  assert.strictEqual(T.toTxt(clear), '  \n', 'alpha 0 cells are blank');
}

/* ANSI: bg once per row, fg only on change (rle), reset per row. */
{
  const ansi = T.toAnsi(A, { truecolor: true, rle: true });
  const rows = ansi.split('\n');
  assert.strictEqual(rows.pop(), '');
  assert.strictEqual(rows.length, 3);
  for (const r of rows) {
    assert(r.startsWith(ESC + '48;2;16;32;48m'), 'row starts with the palette bg');
    assert(r.endsWith(ESC + '0m'), 'row ends with a reset');
    assert.strictEqual(count(r, ESC + '48;'), 1, 'bg set once per row');
  }
  /* row 0: one colour across a blank gap -> one fg escape */
  assert.strictEqual(count(rows[0], ESC + '38;2;'), 1, 'blanks do not break an rle run');
  assert.strictEqual(rows[0], ESC + '48;2;16;32;48m' + ESC + '38;2;200;100;50mAB  CD' + ESC + '0m');
  /* row 1: two colours */
  assert.strictEqual(count(rows[1], ESC + '38;2;'), 2);
  assert.strictEqual(count(rows[2], ESC + '38;2;'), 0, 'blank row has no fg escapes');
  const noRle = T.toAnsi(A, { truecolor: true, rle: false });
  assert.strictEqual(count(noRle, ESC + '38;2;'), 7, 'rle:false = one escape per inked cell');
  const c256 = T.toAnsi(A, { truecolor: false });
  assert(!c256.includes('38;2;') && c256.includes(ESC + '38;5;'), 'truecolor:false uses xterm-256');
  /* alpha composites over bg */
  const half = snap(1, 1, () => 0x41, () => [255, 255, 255], () => 128, '#000000');
  assert(T.toAnsi(half).includes(ESC + '38;2;128;128;128mA'), 'alpha 128 of white over black = 128 grey');
  assert.strictEqual(T.toAnsi(A), T.toAnsi(A), 'deterministic');
}

/* .cast: asciinema v2 header + events, clear-screen prefix, row diff. */
{
  const B = snap(6, 3, (i) => A.cp[i], (i) => (i < 6 ? [200, 100, 50] : [1, 2, 3]));
  const cast = T.toCastV2([A, A, B], 12, { width: 6, height: 3, title: 't' });
  const lines = cast.trimEnd().split('\n').map((l) => JSON.parse(l));
  const header = lines[0];
  assert.deepStrictEqual(header, { version: 2, width: 6, height: 3, env: { TERM: 'xterm-256color' }, title: 't' });
  assert(!('timestamp' in header), 'no wall-clock timestamp: output is deterministic');
  const ev = lines.slice(1);
  for (const e of ev) {
    assert(Array.isArray(e) && e.length === 3 && typeof e[0] === 'number' && e[1] === 'o' && typeof e[2] === 'string');
  }
  assert(ev[0][2].startsWith(ESC + '?25l' + ESC + '2J'), 'frame 0 clears the screen');
  assert.strictEqual(ev[0][0], 0);
  assert.strictEqual(count(ev[0][2], ';1H'), 3, 'frame 0 draws every row');
  /* frame 1 == frame 0 -> no event; frame 2 recolours row 1 only (row 2 is
     blank, so its ink colour is invisible and it is not redrawn) */
  assert.strictEqual(ev.length, 3, 'unchanged frame emits nothing');
  assert.strictEqual(ev[1][0], +(2 / 12).toFixed(6));
  assert(!ev[1][2].includes(ESC + '1;1H') && ev[1][2].includes(ESC + '2;1H') && !ev[1][2].includes(ESC + '3;1H'),
    'only changed rows are redrawn');
  assert.strictEqual(ev[2][0], +(3 / 12).toFixed(6), 'final event at n/fps');
  assert(ev[2][2].includes(ESC + '?25h'), 'final event restores the cursor');
  for (let i = 1; i < ev.length; i++) assert(ev[i][0] > ev[i - 1][0], 'event times increase');
}

/* HTML: inlined font, frame 0 in markup, escaped text, working stepper. */
{
  const B = snap(6, 3, (i) => (i === 0 ? 0x5a : A.cp[i]), (i) => [9, 9, 9]);
  const font = Buffer.from('fake-woff2').toString('base64');
  const html = T.toHtml([A, B], { fps: 10, fontBase64: font, title: 'a<b' });
  assert(html.startsWith('<!doctype html>'));
  assert(html.includes("src:url(data:font/woff2;base64," + font + ")"), 'font inlined as a data URI');
  assert(html.includes('<title>a&lt;b</title>'));
  assert(html.includes('&lt;') && !/<pre[^>]*>[^]*<(?!\/?span)[^]*<\/pre>/.test(html.replace(/<span[^>]*>|<\/span>/g, '')),
    'cell text is escaped');
  assert(html.includes('prefers-reduced-motion: reduce'), 'stepper honours reduced motion');
  const one = T.toHtml([A], { fontBase64: font });
  assert(!one.includes('<script'), 'a single frame needs no script');
  assert(!T.toHtml([A], { inlineFont: false, fontBase64: font }).includes('@font-face'));

  /* run the player against a stub DOM */
  const scripts = Array.from(html.matchAll(/<script(?: type="application\/json" id="frames")?>([\s\S]*?)<\/script>/g));
  const dataJson = scripts[0][1], player = scripts[1][1];
  const els = {};
  const el = (id) => (els[id] = els[id] || {
    id, textContent: id === 'frames' ? dataJson : '', innerHTML: '',
    attrs: { 'aria-label': 'Glyph grid, 6 by 3 characters, frame 1 of 2' },
    getAttribute(k) { return this.attrs[k]; }, setAttribute(k, v) { this.attrs[k] = v; },
  });
  for (const reduce of [true, false]) {
    for (const k of Object.keys(els)) delete els[k];
    const timers = [];
    vm.runInNewContext(player, {
      document: { getElementById: el },
      window: { matchMedia: () => ({ matches: reduce }) },
      matchMedia: () => ({ matches: reduce }),
      setInterval: (fn) => { timers.push(fn); return timers.length; },
      clearInterval: () => {},
    });
    assert.strictEqual(timers.length, reduce ? 0 : 1, reduce ? 'no autoplay under reduced motion' : 'autoplays');
    assert.strictEqual(els.play.textContent, reduce ? 'Play' : 'Pause');
    els.step.onclick();
    const shown = els.g.innerHTML.replace(/<span[^>]*>|<\/span>/g, '')
      .replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&amp;/g, '&');
    assert.strictEqual(shown + '\n', T.toTxt(B), 'Step shows frame 2 with the right text');
    assert(els.g.innerHTML.includes('color:#090909'), 'frame 2 colours');
    assert.strictEqual(els.n.textContent, '2 / 2');
    assert.strictEqual(els.g.attrs['aria-label'], 'Glyph grid, 6 by 3 characters, frame 2 of 2');
  }
}

/* build() dispatch */
{
  assert.deepStrictEqual(T.FORMATS, ['txt', 'ansi', 'cast', 'html']);
  assert.strictEqual(T.build('txt', [A, A]), T.toTxt(A), 'txt is frame 0');
  assert.strictEqual(T.build('ansi', [A]), T.toAnsi(A, { truecolor: true, rle: true }));
  assert(T.build('cast', [A], { fps: 24 }).startsWith('{"version":2,"width":6,"height":3'));
  assert.throws(() => T.build('webm', [A]), /unknown text export format/);
}

/* CR-8: octant / sextant under ansi / cast warn; nothing else does. */
{
  const cr8 = (cfg, format) => compat.validate(cfg, { format }).filter((i) => i.code === 'CR-8');
  assert.strictEqual(cr8({ compat: 'v2', glyphSet: 'octant' }, 'ansi').length, 1);
  assert.strictEqual(cr8({ compat: 'v2', glyphSet: 'sextant' }, 'cast').length, 1);
  assert.strictEqual(cr8({ compat: 'v2', glyphSet: 'octant' }, 'html').length, 0, 'html inlines the font');
  assert.strictEqual(cr8({ compat: 'v2', glyphSet: 'octant' }, 'txt').length, 0);
  assert.strictEqual(cr8({ compat: 'v2', glyphSet: 'blockElements' }, 'ansi').length, 0);
  assert.strictEqual(cr8({ compat: 'v2', glyphSet: 'octant' }).length, 0, 'no format, no warning');
  assert.strictEqual(compat.validate({ compat: 'v2', glyphSet: 'octant' }).length, 0, 'old call shape unchanged');
}

console.log('glyph-text-export: ok');
