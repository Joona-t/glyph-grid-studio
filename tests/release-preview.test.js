'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
const html = fs.readFileSync(path.join(__dirname, '../src/index.html'), 'utf8');
const studio = fs.readFileSync(path.join(__dirname, '../src/lib/glyph-studio.js'), 'utf8');

function section(start, end) {
  const a = html.indexOf(start);
  const b = html.indexOf(end, a + start.length);
  assert(a >= 0 && b > a, `missing section: ${start}`);
  return html.slice(a, b);
}

const tasks = new Map();
let sequence = 0;
const calls = { loop: 0, redraw: 0, font: 0, fps: [] };
const context = vm.createContext({
  window: {}, CONFIG: { animation: { fps: 30, duration: 6, loop: false } },
  recState: null, __renderGeneration: 0, __renderResumeTimer: 0,
  __renderRaf: 0, __lastScheduledFps: 0, __sourcePreprocessScratch: null,
  frameRate: n => calls.fps.push(n),
  noLoop: () => {}, isLooping: () => false,
  loop: () => { calls.loop++; }, redraw: () => { calls.redraw++; },
  syncActiveFont: () => { calls.font++; },
  setTimeout: fn => { tasks.set(++sequence, fn); return sequence; },
  clearTimeout: id => tasks.delete(id),
  requestAnimationFrame: fn => { tasks.set(++sequence, fn); return sequence; },
  cancelAnimationFrame: id => tasks.delete(id),
});
vm.runInContext(section('  function syncAnimationFrameRate()', '  /* ---- per-frame scratch buffers'), context);
const flush = () => { const pending = [...tasks.values()]; tasks.clear(); pending.forEach(fn => fn()); };

// Cheap changes coalesce; structural changes replace the cheap callback.
context.scheduleConfigRender('palette');
context.scheduleConfigRender('palette');
assert.equal(tasks.size, 1);
context.scheduleConfigRender('grid.cols');
context.scheduleConfigRender('font.size');
assert.equal(tasks.size, 1);
flush();
assert.equal(calls.redraw, 1);
assert.equal(calls.loop, 0, 'paused changes must not restart playback');
context.CONFIG.animation.loop = true;
context.scheduleConfigRender('palette'); flush();
assert.equal(calls.loop, 1);
context.recState = {};
context.scheduleConfigRender('font.size');
assert.equal(tasks.size, 0, 'capture exclusively owns rendering');
context.recState = null;
context.CONFIG.animation.fps = 60;
context.scheduleConfigRender('fps'); flush();
assert.equal(calls.fps.at(-1), 60);

const scratch = context.ensureSourceScratch(100, 80);
const planes = Object.values(scratch).filter(v => ArrayBuffer.isView(v));
assert.equal(planes.length, 2);
assert.equal(planes.reduce((sum, v) => sum + v.byteLength, 0), 100 * 80 * 8);
scratch.blur.fill(9);
assert.equal(context.ensureSourceScratch(100, 80), scratch);
assert(scratch.blur.every(v => v === 0));
assert.notEqual(context.ensureSourceScratch(80, 100), scratch);
assert.match(html, /const lum2 = lum;/);
assert.match(html, /const blur2 = blur;/);
assert.match(html, /const lumChip = lum;/);
assert.match(html, /const blurChip = blur;/);

vm.runInContext(section('  let _previewTime = 0;', "  let _fontLoadKey = ''"), context);
context.window.__requestRender = () => context.scheduleConfigRender('preview');
assert.equal(context.window.__seekPreview(0), true);
assert.equal(context.window.__previewState().frame, 0);
assert.equal(context.window.__seekPreview(1), true);
assert.equal(context.window.__previewState().frame, 359);
assert.equal(context.window.__previewState().playing, false);
assert.equal(context.window.__seekPreview(NaN), false);
context.recState = {};
assert.equal(context.window.__seekPreview(0), false);
context.recState = null;
tasks.clear();
context.CONFIG.animation.fps = 30;
context.CONFIG.animation.loop = true;
context.cellSignalEma = null;
context._stabIdx = context._stabPend = context._stabPendN = context.crtState = null;
vm.runInContext('_previewFrame = 0; _previewHasFrame = false;', context);
const tick = section('    if (!recState) {\n      const total = Math.max(1, Math.round(CONFIG.animation.duration', '    const t = recState ?');
const frames = [];
for (let i = 0; i <= 360; i++) {
  vm.runInContext(tick, context);
  frames.push(context.window.__previewState().frame);
}
assert.deepEqual(frames.slice(0, 180), Array.from({ length: 180 }, (_, i) => i));
assert.deepEqual(frames.slice(180, 360), frames.slice(0, 180));
assert.equal(frames[360], 0);

// Prevent accidental replacement of the public version's buffered binding.
assert.match(studio, /addStructuralInput\(fSel, config, 'glyphSet'/);
assert.match(studio, /structuralStates\[i\]\.binding === \(ev && ev\.target\)/);
for (const [file, text] of [['index.html', html], ['glyph-studio.js', studio]]) {
  assert(!/^(<<<<<<< |=======\s*$|>>>>>>> )/m.test(text), `unresolved merge in ${file}`);
}
console.log('combined preview regressions: PASS; 2 planes = 8 bytes/working pixel; 361 ticks, 2 exact 180-frame cycles; scheduler/pause/capture/seek checked');
