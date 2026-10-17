'use strict';
/* ITER-077 (R4): video source — frame mapping, priming list, LRU, the
   rust-backend adapter (fake IPC), and the index.html / studio contracts.
   Rides along with `npm test` via frontend-contracts.test.js. */
const assert = require('assert');
const fs = require('fs');
const path = require('path');
const V = require('../src/lib/glyph-video-source.js');

if (typeof global.ImageData === 'undefined') {
  global.ImageData = function ImageData(data, w, h) { this.data = data; this.width = w; this.height = h; };
}

/* frameIndexAt: floor(t * fps), wrapped; exact at frame boundaries. */
assert.strictEqual(V.frameIndexAt(0, 25, 12), 0);
assert.strictEqual(V.frameIndexAt(40, 25, 12), 1, 'exactly one frame period in');
assert.strictEqual(V.frameIndexAt(39.9, 25, 12), 0);
assert.strictEqual(V.frameIndexAt(480, 25, 12), 0, 'wraps at the clip length');
assert.strictEqual(V.frameIndexAt(1000 / 30 * 7, 30, 100), 7, 'float t of an integer frame lands on it');
assert.strictEqual(V.frameIndexAt(-40, 25, 12), 11, 'negative t wraps backwards');
assert.strictEqual(V.frameIndexAt(100, 0, 12), 0, 'no fps → frame 0');

/* primeTimes mirrors primeTemporalState's wrap + frame 0. */
const pt = V.primeTimes(12, 11, 25);
assert.strictEqual(pt.length, 12);
assert.deepStrictEqual(pt.map((t) => V.frameIndexAt(t, 25, 12)), [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 0]);
assert.deepStrictEqual(V.primeTimes(3, 11, 25).map((t) => Math.round(t * 25 / 1000)).slice(0, 3), [1, 2, 0], 'short loops wrap');
assert(V.CACHE >= 12, 'the cache holds every priming frame + frame 0');

/* LRU */
const l = new V.Lru(2);
l.set(1, 'a'); l.set(2, 'b'); l.get(1); l.set(3, 'c');
assert.deepStrictEqual([l.has(1), l.has(2), l.has(3)], [true, false, true], 'least-recently used goes first');
assert(V.isVideoPath('/x/Clip.MOV') && V.isVideoPath('a.webm') && !V.isVideoPath('a.gif'));
assert.deepStrictEqual(V.DECODERS, ['auto', 'native', 'webview']);

/* Adapter over the rust backend with a fake IPC: frames are fetched by
   index, painted only when cached, and every repaint bumps index. */
(async function () {
  const W = 4, H = 2;
  const calls = [];
  const invoke = function (cmd, args) {
    calls.push([cmd, args.index]);
    if (cmd !== 'video_frame') throw new Error(cmd);
    const b = new Uint8Array(W * H * 4).fill(args.index);
    return Promise.resolve(b.buffer);
  };
  const vs = {
    kind: 'video', backend: 'rust', camera: false, mirror: false, handle: 7,
    fps: 25, frames: 12, width: W, height: H, cache: new V.Lru(V.CACHE),
    chain: Promise.resolve(), epoch: 1, painted: 0, presented: 0, livePainted: -1, disposed: false,
  };
  const painted = [];
  const g = { width: W, height: H, drawingContext: { putImageData: function (img) { painted.push(img.data[0]); } } };
  let rec = true;
  const a = V.adapter(g, vs, 1, { invoke: invoke, recording: function () { return rec; } });
  assert.strictEqual(a.kind, 'video');
  assert.strictEqual(a.durationMs, 480);
  assert.strictEqual(a.index, 1, 'index starts at the painted epoch');
  assert.strictEqual(a.ready(80), false);
  a.frameAt(80);
  assert.deepStrictEqual(painted, [], 'recording: an uncached frame is never painted');
  await a.prefetch([80, 120, 80]);
  assert.deepStrictEqual(calls, [['video_frame', 2], ['video_frame', 3]], 'one fetch per frame, deduped');
  assert(a.ready(80) && a.ready(120));
  a.frameAt(80); a.frameAt(80); a.frameAt(120);
  assert.deepStrictEqual(painted, [2, 3], 'repaint only on a frame change');
  assert.strictEqual(a.index, 3, 'each repaint bumps the epoch');
  /* Live (not recording): a miss kicks a fetch, keeps the old frame. */
  rec = false;
  a.frameAt(400);
  await vs.chain;
  assert(vs.cache.has(10), 'live miss prefetched frame 10');
  a.frameAt(400);
  assert.strictEqual(painted[painted.length - 1], 10);
  /* Byte-count guard. */
  const bad = Object.assign({}, vs, { cache: new V.Lru(4), chain: Promise.resolve(), pending: null, width: 5 });
  await assert.rejects(V.load(bad, 0, invoke), /want 40/);
  V.dispose(vs);
  await assert.rejects(V.load(vs, 5, invoke), /replaced/);

  /* index.html + studio contracts. */
  const html = fs.readFileSync(path.join(__dirname, '../src/index.html'), 'utf8');
  assert(html.indexOf('<script src="lib/glyph-video-source.js"></script>') > html.indexOf('<script src="lib/glyph-text-source.js"></script>'),
    'glyph-video-source.js is loaded with the other libs');
  assert(/if \(img && img\.__ggsVideo\) return GlyphGrid\.video\.adapter\(/.test(html), 'videos go through the FrameSource adapter');
  assert(/recState && recState\._srcWait && !__primeOnly\) return;/.test(html), 'drawFrame parks while a capture waits on a decode');
  assert(/!_vsrc\.ready\(t \* 1000\)\) \{\s*awaitVideoFrame\(/.test(html), 'captured draws gate on the decoded frame');
  assert(/const _prefetch = prefetchCaptureFrames\(total\);[\s\S]{0,700}primeTemporalState\(total\);/.test(html),
    'beginRecord decodes the priming frames before it primes');
  assert(/isVideo\) return _cliLoadVideo\(/.test(html), 'the CLI loader routes clips to the video source');
  assert(/video: \{ decode: 'auto', mirror: true \},/.test(html), 'CONFIG.video defaults (no camera: ITER-078)');
  /* ITER-078 (privacy): the webcam switch is runtime-only state. */
  assert(!/CONFIG(_BASE)?\.video\.camera/.test(html), 'nothing reads or writes CONFIG.video.camera');
  assert(/const CAMERA = \{ camera: false \};/.test(html) && /const want = !!CAMERA\.camera;/.test(html),
    'the camera kick reads the runtime CAMERA only');
  assert.strictEqual((html.match(/CAMERA\.camera = (?!false)/g) || []).length, 0, 'page code only ever switches the camera off');
  assert(/cameraState: CAMERA,/.test(html), 'the GUI toggle is bound to CAMERA');
  assert((html.match(/openCamera\(/g) || []).length === 1, 'one getUserMedia entry point (the kick)');
  assert(/deepMerge\(CONFIG, window\.GlyphStudio\.stripRuntimeOnly\(overrides/.test(html)
    && /deepMerge\(CONFIG, window\.GlyphStudio\.stripRuntimeOnly\(cfg\)\)/.test(html),
    'setConfig and the batch applyConfig strip video.camera');
  const schema = JSON.parse(fs.readFileSync(path.join(__dirname, '../src/glyph-grid.schema.json'), 'utf8'));
  assert.deepStrictEqual(schema.properties.video.properties.decode.enum, V.DECODERS, 'schema video.decode enum');
  assert.strictEqual(schema.properties.video.additionalProperties, false, 'video block is closed');
  const studio = fs.readFileSync(path.join(__dirname, '../src/lib/glyph-studio.js'), 'utf8');
  assert(/'video',\s*\/\*[^*]*\*\/\s*'timeline'/.test(studio), 'video is in the preset whitelist');
  assert(/if \(out\.video\) delete out\.video\.camera;/.test(studio), 'presets never carry camera:true');
  assert(/probe\.kind === 'video'[\s\S]{0,1500}window\.__ggsOpenVideo\(path, probe\)/.test(studio), 'GUI imports route clips to the video source');
  console.log('glyph-video-source: ok');
})().catch(function (e) { console.error(e); process.exit(1); });
