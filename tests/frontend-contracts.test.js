'use strict';

const assert = require('assert');
const fs = require('fs');
const vm = require('vm');

const html = fs.readFileSync(require.resolve('../src/index.html'), 'utf8');
const inlineScripts = Array.from(html.matchAll(/<script(?![^>]*\bsrc=)[^>]*>([\s\S]*?)<\/script>/gi));

assert(inlineScripts.length > 0, 'index.html must contain an inline application script');
for (const [index, match] of inlineScripts.entries()) {
  new vm.Script(match[1], { filename: `index-inline-${index + 1}.js` });
}

const contracts = [
  ['native ZIP allocation is conditional', /zip:\s*collectZip\s*\?\s*new JSZip\(\)\s*:\s*null/],
  ['recorder preserves supplied job IDs', /jobId:\s*opts\.jobId\s*\|\|\s*\(\+\+__recordJobSequence\)/],
  ['frame consumer participates in capture backpressure', /return state\.onFrame\(\{/],
  ['capture frame limit is explicit', /MAX_CAPTURE_FRAMES\s*=\s*600/],
  ['capture pixel budget is aligned with Rust', /MAX_CAPTURE_PIXELS\s*=\s*100\s*\*\s*1000\s*\*\s*1000/],
  ['source file limit is aligned with Rust', /maxFileBytes:\s*64\s*\*\s*1024\s*\*\s*1024/],
  ['source frame limit is explicit', /maxFrames:\s*480/],
  ['source decoded-pixel limit is explicit', /maxDecodedPixels:\s*100\s*\*\s*1000\s*\*\s*1000/],
  ['studio preprocessing uses working dimensions', /const W0 = W, H0 = H/],
  ['studio preprocessing reuses scratch planes', /function ensureSourceScratch\(w, h\)/],
  ['static cell-signal cache is present', /__sourceSignalCache\.cells = cellSignal\.slice\(\)/],
  ['atlas commits reject stale generations', /requestedGeneration === __renderGeneration/],
  ['p5 cadence follows the live FPS setting', /frameRate\(fps\)/],
  ['CLI accepts metadata-bearing native image payloads', /typeof loaded === ['"]object['"]\)[\s\S]{0,100}loaded\.dataUrl/],
  /* ITER-064 (F18): header probe gates the source before p5 decodes it; the
     webview streams the file via the asset protocol. */
  ['CLI validates the header probe before any loadImage', /invoke\('probe_image_file', \{ path: inPath \}\);[\s\S]{0,600}validateSourceMetadata\([\s\S]{0,1200}window\.loadImage\(sourceUrl,/],
  ['CLI loads native sources via convertFileSrc', /sourceUrl = core\.convertFileSrc\(inPath\);/],
  ['studio imageRef exposes the pre-decode validator', /validate: function \(img, extra\) \{ return validateSourceMetadata\(img, extra\); \}/],
  ['memory probe sums every audited cache (F20)', /GlyphGrid\.memProbe = function[\s\S]{0,6000}totalBytes: total/],
  ['memory probe covers the wave6 atlas map', /wave6\.atlasCacheStats\(\)/],
  /* v0.2 Phase 1 determinism (ITER-046/047) */
  /* ITER-049: priming must precede loop() — p5's loop() can draw synchronously. */
  ['beginRecord primes temporal state before loop() re-arms the draw', /primeTemporalState\(total\);[\s\S]{0,600}try \{ loop\(\); \} catch \(e\) \{\}\s*return recState\.jobId;/],
  ['perf switch stamp is re-taken after priming', /primeTemporalState\(total\);[\s\S]{0,400}if \(__pfLastChange\) __pfLastChange\.t = performance\.now\(\);/],
  ['stale prime diagnostics are cleared per job', /function primeTemporalState\(total\) \{\s*__lastPrime = null;/],
  ['setup() rejects __setupReady on throw', /catch \(e\) \{\s*if \(__setupReadyReject\) __setupReadyReject\(e\);\s*throw e;/],
  ['headless exits non-zero when setup() throws', /try \{\s*await __setupReady;\s*\} catch \(e\) \{[\s\S]{0,400}exit_with_status', \{ ok: false \}/],
  ['priming resets EMA, CRT state and stability hysteresis', /cellSignalEma\.fill\(0\.5\);\s*crtState = null;[\s\S]{0,80}_stabKey = '';/],
  ['throttled capture draws are no-ops (stray p5 rAF)', /if \(recState && recState\._capturing && !__primeOnly\) return;/],
  ['animated sources are paused so setFrame owns playback (F21)', /if \(typeof img\.pause === 'function'\) img\.pause\(\);/],
  ['headless start waits on readiness, not the clock', /await __setupReady;\s*\} catch \(e\) \{[\s\S]{0,400}\}\s*await __assetsReady\(\);/],
  /* ITER-048 (F17 part 1): reduced motion pins the frame-hashed seed + dither phase for stills */
  ['reduced motion pins frame index for still sources', /const _rmPinned = _rmStill && window\.__sourceFrameIdx === -1;\s*const _frameIdx = _rmPinned \? 0 :/],
  ['reduced motion pins the temporal dither phase', /_persistentDitherOpts\.frameIdx = _rmPinned \? 0 : frameIdx;/],
  /* ITER-062 (F17 part 2): static-frame detector */
  ['idle check runs after the live frame is painted, never while recording', /if \(recState\) \{\s*handleRecordFrame\(\);\s*\} else \{\s*idleCheck\(\);\s*\}/],
  ['idle park requires a settled EMA', /frameIsTimeDependent\(\) \|\| !\(__emaMaxDelta < IDLE_EMA_EPS\)/],
  ['a parked redraw that turns dynamic re-arms the loop', /__idleParked = false;\s*setTimeout\(function \(\) \{\s*if \(!recState && !__renderResumeTimer\) \{ try \{ loop\(\); \}/],
  ['EMA formula is unchanged by the idle probe', /cellSignalEma\[i\] = \(1 - _emaA\) \* cellSignalEma\[i\] \+ _emaA \* cellSignal\[i\];/],
  /* ITER-063 (F19): vendored deps + single font readiness promise */
  ['p5 is loaded from the vendored copy', /<script src="vendor\/p5-1\.11\.2\/p5\.min\.js"><\/script>/],
  ['Tweakpane is loaded from the vendored copy', /<script src="vendor\/tweakpane-3\.1\.10\/tweakpane\.min\.js"><\/script>/],
  ['JSZip is loaded from the vendored copy', /<script src="vendor\/jszip-3\.10\.1\/jszip\.min\.js"><\/script>/],
  ['fflate is loaded from the vendored copy', /<script src="vendor\/fflate-0\.8\.2\/index\.js"><\/script>/],
];

for (const [name, pattern] of contracts) {
  assert(pattern.test(html), `frontend contract missing: ${name}`);
}

/* Negative contracts: the wall-clock sleeps ITER-047 removed must not come back. */
const banned = [
  ['headless start-up sleep', /setTimeout\(tryHeadlessRender,\s*\d+\)/],
  ['post-load / post-config settle sleeps', /await new Promise\(function \(r\) \{ setTimeout\(r, (300|200|1500)\); \}\);/],
  ['batch settle timer', /opts\.settleMs/],
  ['remote <script>/<link> (offline boot, ITER-063)', /<(script|link)\b[^>]*\b(src|href)=["']?(https?:)?\/\//i],
];
for (const [name, pattern] of banned) {
  assert(!pattern.test(html), `frontend contract violated: ${name} is back`);
}

console.log(`frontend contracts: ok (${inlineScripts.length} inline script, ${contracts.length} invariants)`);

/* ITER-062: behavioural check of frameIsTimeDependent() against stubbed state.
   Unknown/dynamic must win; only a fully still config may report false. */
{
  const m = html.match(/const IDLE_STATIC_DITHER = [^\n]*\n[\s\S]*?\n  function frameIsTimeDependent\(\) \{[\s\S]*?\n  \}\n/);
  assert(m, 'frameIsTimeDependent() not found');
  const src = m[0].replace(/^\s*let __\w+ = [^\n]*\n/gm, '');
  const make = (over) => {
    const still = {
      scene: 'studio', glyphSet: null, prefilter: null, dispersal: { enabled: false },
      studio: { breathing: { gainSwing: 0, jitter: 0 } }, dither: { mode: 'none' },
      postprocess: {}, animation: { loop: true },
    };
    const ctx = Object.assign({
      CONFIG: Object.assign(still, over && over.CONFIG),
      recState: null, __primeOnly: false, __fontLoadPromise: null, fontDescriptor: null,
      sourceImg: { width: 10 }, _atlasInflight: {}, _atlasCache: {},
      window: { PIPELINE: null, __sourceFrameIdx: -1 },
      __prefersReducedMotion: () => false,
      usesAdvancedPipeline: () => true,
      _hasImgDataStages: () => false,
    }, over && over.ctx);
    ctx.GlyphGrid = { compat: { isV2: () => true, gateDither: (c) => c.dither.mode !== 'none',
      gatePostprocess: () => true } };
    ctx.window.GlyphGrid = ctx.GlyphGrid;
    vm.createContext(ctx);
    vm.runInContext(src + '\nthis.__f = frameIsTimeDependent;', ctx);
    return ctx.__f();
  };
  assert.strictEqual(make(), false, 'fully still config must be static');
  const dynamic = [
    ['default breathing', { CONFIG: { studio: { breathing: {} } } }],
    ['temporal dither', { CONFIG: { dither: { mode: 'temporal' } } }],
    ['stbn dither', { CONFIG: { dither: { mode: 'stbn' } } }],
    ['dispersal', { CONFIG: { dispersal: { enabled: true, startT: 0.2, endT: 0.9 } } }],
    ['xdog sigma sweep', { CONFIG: { prefilter: { mode: 'xdog' } } }],
    ['generative scene', { CONFIG: { scene: 'flowField' } }],
    ['animated source', { ctx: { window: { PIPELINE: null, __sourceFrameIdx: 3 } } }],
    ['pipeline', { ctx: { window: { PIPELINE: {}, __sourceFrameIdx: -1 } } }],
    ['recording', { ctx: { recState: { frameIdx: 0 } } }],
    ['priming', { ctx: { __primeOnly: true } }],
    ['no source', { ctx: { sourceImg: null } }],
    ['font pending', { ctx: { __fontLoadPromise: {}, fontDescriptor: null } }],
    ['atlas in flight', { ctx: { _atlasInflight: { octant: {} } } }],
    ['atlas missing', { CONFIG: { glyphSet: 'octant' } }],
    ['CRT pixel stage', { ctx: { _hasImgDataStages: () => true } }],
  ];
  for (const [name, over] of dynamic) {
    assert.strictEqual(make(over), true, `frameIsTimeDependent must be true for: ${name}`);
  }
  /* Reduced motion stills breathing + temporal dither for a still source. */
  assert.strictEqual(make({ CONFIG: { studio: { breathing: {} }, dither: { mode: 'temporal' } },
    ctx: { __prefersReducedMotion: () => true } }), false, 'reduced motion still source must be static');
  assert.strictEqual(make({ CONFIG: { dither: { mode: 'bayer8' } } }), false, 'ordered dither is static');
}

/* ITER-063: vendored files must match the pinned CDN hashes, ship a licence,
   and the font loader must wait on all faces in parallel. */
{
  const crypto = require('crypto');
  const path = require('path');
  const vendorDir = path.join(__dirname, '..', 'src', 'vendor');
  const sums = fs.readFileSync(path.join(vendorDir, 'SHA256SUMS'), 'utf8').trim().split('\n');
  assert.strictEqual(sums.length, 4, 'SHA256SUMS must pin exactly the four vendored files');
  for (const line of sums) {
    const [hash, rel] = line.split(/\s+/);
    const got = crypto.createHash('sha256').update(fs.readFileSync(path.join(vendorDir, rel))).digest('hex');
    assert.strictEqual(got, hash, `vendored file drifted from its pinned hash: ${rel}`);
    const dir = path.join(vendorDir, path.dirname(rel));
    assert(fs.readdirSync(dir).some((f) => /^LICENSE/i.test(f)), `missing LICENSE in ${dir}`);
    assert(html.includes(`<script src="vendor/${rel}"></script>`), `index.html does not load vendor/${rel}`);
  }
  const fontsJs = fs.readFileSync(path.join(__dirname, '..', 'src', 'lib', 'glyph-fonts.js'), 'utf8');
  assert(/return Promise\.all\(\[\s*waitForFace\(FONT_FAMILY, sizePx, 3000\),\s*waitForFace\(FONT_FAMILY \+ '-Fallback'/.test(fontsJs),
    'font faces must load behind one parallel readiness promise');
  assert(!/waitForFace\([^)]*\)\s*\.then\(function \(\) \{ return waitForFace/.test(fontsJs),
    'sequential font wait chain is back');
}

/* ITER-064 (F18): behavioural check of the studio's native loader. An
   over-limit probe must never reach loadImage; an in-limit one loads the
   asset-protocol URL; without convertFileSrc it falls back to inline data. */
{
  const path = require('path');
  const studioJs = fs.readFileSync(path.join(__dirname, '..', 'src', 'lib', 'glyph-studio.js'), 'utf8');
  const m = studioJs.match(/\n  function loadNativeImageLatest\([\s\S]*?\n  \}\n/);
  assert(m, 'loadNativeImageLatest() not found');
  const run = (probe, withConvert) => {
    const calls = { invoke: [], loaded: [], feedback: [] };
    const core = {
      invoke(cmd, args) {
        calls.invoke.push([cmd, args]);
        if (cmd === 'probe_image_file') return Promise.resolve(probe);
        if (cmd === 'read_image_file') return Promise.resolve({ dataUrl: 'data:image/gif;base64,AA==', sizeBytes: 1 });
        return Promise.reject(new Error('unexpected ' + cmd));
      },
    };
    if (withConvert) core.convertFileSrc = (p) => 'asset://localhost/' + encodeURIComponent(p);
    const ctx = {
      window: { __TAURI__: { core } },
      isCurrentSourceLoad: () => true,
      sourcePayload: (v) => ({ dataUrl: (v && v.dataUrl) || '' }),
      exportFeedback: (msg) => calls.feedback.push(msg),
      loadImageLatest: (url) => calls.loaded.push(url),
    };
    vm.createContext(ctx);
    vm.runInContext(m[0] + '\nthis.__f = loadNativeImageLatest;', ctx);
    const imageRef = {
      validate(img, extra) {
        const frames = img.numFrames();
        const reason = img.width > 16384 || img.height > 16384 ? 'logical dimensions exceed 16384 px'
          : frames > 480 ? 'animation has ' + frames + ' frames (limit 480)' : '';
        return { ok: !reason, reason, meta: { sizeBytes: extra.sizeBytes } };
      },
    };
    ctx.__f('/tmp/a b.gif', imageRef, 'a b.gif', 1, () => {});
    return new Promise((r) => setTimeout(r, 0)).then(() => calls);
  };
  Promise.all([
    run({ width: 1, height: 1, frames: 481, sizeBytes: 17830, probed: true }, true),
    run({ width: 16385, height: 1, frames: 1, sizeBytes: 97, probed: true }, true),
    run({ width: 462, height: 622, frames: 50, sizeBytes: 1, probed: true }, true),
    run({ width: 462, height: 622, frames: 50, sizeBytes: 1, probed: true }, false),
    run({ width: 0, height: 0, frames: 0, sizeBytes: 1, probed: false }, true),
  ]).then(([tooMany, tooWide, ok, noConvert, unprobed]) => {
    assert.deepStrictEqual(tooMany.loaded, [], '481-frame GIF must be rejected before loadImage');
    assert(/481 frames/.test(tooMany.feedback[0]), 'frame-limit rejection is reported');
    assert.deepStrictEqual(tooWide.loaded, [], '16385-px still must be rejected before loadImage');
    assert.deepStrictEqual(ok.loaded, ['asset://localhost/%2Ftmp%2Fa%20b.gif']);
    assert(!ok.invoke.some(([cmd]) => cmd === 'read_image_file'), 'asset path must not read the file over IPC');
    assert.deepStrictEqual(noConvert.loaded, ['data:image/gif;base64,AA==']);
    assert.strictEqual(JSON.stringify(noConvert.invoke[1]), JSON.stringify(['read_image_file', { path: '/tmp/a b.gif', inline: true }]));
    assert.strictEqual(unprobed.loaded.length, 1, 'unprobed formats fall back to the post-decode guard');
    console.log('native source loader: ok (5 cases)');
  }).catch((e) => { console.error(e); process.exit(1); });
}

/* ITER-065 (F7): the GL grid path must place, skip and weight glyphs exactly
   like the CPU monochrome sprite blit — and must run on its own arguments
   (pre-ITER-065 it read the caller's locals and threw on frame 1). */
{
  const m = html.match(/  let _glInst = null;[^\n]*\n[\s\S]*?\n  function drawGridWebGL\([\s\S]*?\n  \}\n/);
  assert(m, 'drawGridWebGL() not found');
  const gate = /drawGridWebGL\(cellIndex, signal, cols, rows, cellW, cellH,\s*palette, ramp, rampLast, _stabOnInt\)/;
  assert(gate.test(html), 'GL gate passes the stability flag explicitly');
  const sprite = { canvas: { width: 90, height: 12 }, tileW: 9, tileH: 12, halfW: 4.5, halfH: 6 };
  const seen = {};
  const lut = new Float32Array(256); for (let i = 0; i < 256; i++) lut[i] = i / 255;
  const ctx = {
    width: 1024, height: 504, CONFIG: { font: { size: 8 }, renderer: 'webgl' },
    _glOk: true, _glCanvas: null, _stabOn: false, _stabIdx: null, _gammaLut: lut,
    _rampSpriteSpaceMask: Uint8Array.from([1, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
    _ensureRampSprite: () => sprite, fontFamilyForActive: () => 'mono',
    _hexToRgb: () => [17, 34, 51], _stableIdx: (s, i, v, idx) => idx, _stableIdxInt: (i, idx) => idx,
    __pfAdd: () => {}, performance: { now: () => 0 }, console: { warn: (...a) => { seen.warn = a; } },
    drawingContext: { drawImage: (c) => { seen.composited = c; } },
    document: { createElement: () => ({ width: 0, height: 0 }) },
  };
  ctx.window = { GlyphGrid: { wave6: {
    makeSprite: () => ({}),
    drawSprite: (r, c, atlas, tw, th, inst, n, ink) => { Object.assign(seen, { atlas, tw, th, inst: Array.from(inst.subarray(0, n * 4)), n, ink }); },
  } } };
  vm.createContext(ctx);
  vm.runInContext(m[0].replace(/^  let _glInst = null;/, 'var _glInst = null;') + '\nthis.__f = drawGridWebGL;', ctx);
  const cols = 3, rows = 2, cellW = 1024 / 3, cellH = 504 / 2;
  const signal = Float32Array.from([0, 0.5, 1, 0.2, 0.9, 0.05]);
  assert.strictEqual(ctx.__f(null, signal, cols, rows, cellW, cellH, { inks: ['#112233'] }, ' .:-=+*#%@', 9, false), true, 'GL path renders');
  assert(!seen.warn, 'GL path must not throw: ' + (seen.warn && seen.warn[1]));
  /* CPU reference: same idx rule, space skip, origin snaps and alpha clamp. */
  const want = [];
  for (let r = 0; r < rows; r++) for (let c = 0; c < cols; c++) {
    const curved = lut[(signal[r * cols + c] * 255) | 0];
    const idx = Math.min(9, (curved * 9 + 0.0001) | 0);
    if (ctx._rampSpriteSpaceMask[idx]) continue;
    const a = Math.min(1, curved * 1.1);
    want.push(((((c * cellW) | 0) + cellW * 0.5) - 4.5) | 0, ((((r * cellH) | 0) + cellH * 0.5) - 6) | 0, idx, Math.fround(a));
  }
  assert.strictEqual(seen.n, want.length / 4, 'space glyphs are dropped from the instance list');
  assert.deepStrictEqual(seen.inst, want, 'instance origins/idx/alpha match the CPU sprite blit');
  assert.strictEqual(seen.atlas, sprite.canvas, 'GL samples the CPU ramp sprite atlas');
  assert.deepStrictEqual([seen.tw, seen.th], [9, 12], 'tile size is the sprite tile, not the cell');
  assert.strictEqual(seen.composited, ctx._glCanvas, 'GL result is composited onto the p5 canvas');
  console.log('GL sprite-equivalent instance fill: ok');
}

/* ITER-066 (F8 Phase A): the WebGL2 preprocess port must keep every tuned
   constant identical to the JS path it replaces, and only take animated
   frames behind the CONFIG.studio.gpuPreprocess switch. */
{
  const gl = fs.readFileSync(require.resolve('../src/lib/glyph-preprocess-gl.js'), 'utf8');
  const names = ['gainGlobal', 'midpoint', 'sharpenGlobal', 'sharpenFace', 'gainFace', 'fineSharpen',
    'sharpAxis', 'sharpChip', 'sharpHand', 'gainAxis', 'gainChip', 'gainHand', 'midpoint2'];
  const jsBody = html.slice(html.indexOf('1. base image preprocess'), html.indexOf('buf.updatePixels();'));
  assert(jsBody.length > 1000, 'JS preprocess body located');
  for (const n of names) {
    const re = new RegExp('(?:const (?:float )?|, )' + n + '\\s*=\\s*([0-9.]+)');
    const a = jsBody.match(re), b = gl.match(re);
    assert(a && b, `constant ${n} present in both preprocess paths`);
    assert.strictEqual(parseFloat(a[1]), parseFloat(b[1]), `constant ${n}: JS ${a[1]} vs GL ${b[1]}`);
  }
  assert(/<script src="lib\/glyph-preprocess-gl\.js"><\/script>/.test(html), 'GL preprocess module is loaded');
  assert(/_curFrame >= 0 && config\.studio && config\.studio\.gpuPreprocess/.test(html),
    'GL preprocess only runs for animated frames behind studio.gpuPreprocess');
  assert(/gpuPreprocess: true,/.test(html), 'studio.gpuPreprocess default is declared');
  console.log('GL preprocess constant parity: ok (' + names.length + ' constants)');
}
