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
  /* ITER-057 (F24): one config merger for GUI / CLI / batch */
  ['CLI setConfig uses the shared deep merge', /setConfig: \(overrides\) => \{[\s\S]{0,400}window\.GlyphStudio\.deepMerge\(CONFIG, overrides \|\| \{\}\);/],
  ['batch applyConfig uses the shared deep merge', /function applyConfig\(cfg\) \{\s*if \(!cfg\) return;\s*window\.GlyphStudio\.deepMerge\(CONFIG, cfg\);/],
  ['frames "auto" follows the GUI export plan', /job\.frames === 'auto'\s*\? Math\.max\(2, Math\.round\(\(\(CONFIG\.animation && CONFIG\.animation\.duration\) \|\| 6\) \* effFps\)\)/],
  /* ITER-071: the GIF border rides in the ITER-053 gifStream settings, which finish repeats */
  ['per-job border reaches the MP4 encoder', /fps: Math\.round\(effFps\),[\s\S]{0,200}border: jobBorder,/],
  ['per-job border reaches the streamed GIF settings', /const gifStream = fmt === 'mp4' \? null : \{[\s\S]{0,300}border: jobBorder,/],
  ['batch jobs carry per-job frames and border', /frames:\s+j\.frames \|\| job\.frames \|\| 24,[\s\S]{0,120}border:\s+j\.border \|\| null,/],
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
  /* ITER-053 (F12): headless GIF streams into gifski during capture; finish repeats the same settings */
  ['headless GIF passes its encode settings to begin_export_capture', /tauri\('begin_export_capture', \{ jobId: jobId, total: frameTotal, gifStream: gifStream, label: job\.name \}\)/],
  ['finish_gif_export args are the streamed settings', /:\s*Object\.assign\(\{ jobId: jobId \}, gifStream\);/],
  /* ITER-074: text exports copy the GlyphFrame IR synchronously at capture
     time (the next draw overwrites it) and skip the pixel readback. */
  ['text exports snapshot the IR at capture time', /state\.captureIr \? GlyphGrid\.textExport\.snapFromIr\(glyphFrame\)/],
  ['irOnly capture skips the pixel readback', /if \(state\.irOnly\) \{\s*blobReady = Promise\.resolve\(null\);/],
  ['text-export module is loaded before the app script', /<script src="lib\/glyph-text-export\.js"><\/script>/],
];

for (const [name, pattern] of contracts) {
  assert(pattern.test(html), `frontend contract missing: ${name}`);
}

/* Negative contracts: the wall-clock sleeps ITER-047 removed must not come back. */
const banned = [
  ['one-level Object.assign config merge (F24)', /Object\.assign\(CONFIG\[k\], v\)/],
  ['headless start-up sleep', /setTimeout\(tryHeadlessRender,\s*\d+\)/],
  ['post-load / post-config settle sleeps', /await new Promise\(function \(r\) \{ setTimeout\(r, (300|200|1500)\); \}\);/],
  ['batch settle timer', /opts\.settleMs/],
  ['remote <script>/<link> (offline boot, ITER-063)', /<(script|link)\b[^>]*\b(src|href)=["']?(https?:)?\/\//i],
  /* ITER-054: the total capture pixel cap went away with the GIF path's decode-everything step. */
  ['total capture pixel cap', /MAX_CAPTURE_PIXELS|capture-pixel-limit/],
];
for (const [name, pattern] of banned) {
  assert(!pattern.test(html), `frontend contract violated: ${name} is back`);
}
/* ITER-069: the offline guarantee covers the runtime libs too — no CDN
   script fallback, no remote import(). Only the SVG namespace URI and the
   preset schema $id (ITER-056: an identifier, never fetched) may appear. */
{
  const libDir = require('path').join(__dirname, '../src/lib');
  for (const f of fs.readdirSync(libDir).filter((n) => n.endsWith('.js'))) {
    const src = fs.readFileSync(require('path').join(libDir, f), 'utf8');
    const hit = src.match(/https?:\/\/(?!www\.w3\.org\/2000\/svg|github\.com\/Joona-t\/glyph-grid-studio\/schema\/)[^\s'"`]+/);
    assert(!hit, `frontend contract violated: remote URL in src/lib/${f}: ${hit && hit[0]}`);
  }
  const studio = fs.readFileSync(require.resolve('../src/lib/glyph-studio.js'), 'utf8');
  assert(/s\.src = 'vendor\/tweakpane-3\.1\.10\/tweakpane\.min\.js'/.test(studio), 'Tweakpane fallback is the vendored file');
  /* ITER-069 (ITER-062 follow-up): panel font swaps go through the host hook
     the idle detector can see. */
  assert.strictEqual((studio.match(/swapFont\(config\);/g) || []).length, 2, 'both panel font swaps use swapFont()');
  assert(/window\.__glyphFontSwap\(opts\)/.test(studio), 'swapFont() prefers the host __glyphFontSwap hook');
  assert(/window\.__glyphFontSwap = function \(opts\) \{[\s\S]{0,200}__fontSwapPending\+\+;[\s\S]{0,300}__fontLoadPromise = [\s\S]{0,300}__fontSwapPending--;\s*\n\s*if \(!recState\) \{ try \{ loop\(\); \}/.test(html),
    '__glyphFontSwap tracks the swap as pending and re-arms the loop when it resolves');
}

/* ITER-069: headless determinism — CLI/MCP jobs default to the JS preprocess
   (only an explicit studio.gpuPreprocess:true from the preset/job opts in),
   and every job reports the path it ran. */
{
  const at = html.indexOf("console.log('CLI: render job received', job);");
  assert(at > 0, 'CLI job entry located');
  assert(/^\s*__headlessPreprocessDefault\(\);/.test(html.slice(at + 46, at + 120)), 'headless default applied on job receipt');
  const apply = html.indexOf('__glyphGridTest.setConfig(job.config)');
  const batch = html.indexOf('__glyphGridTest.runBatchExport(batchJobs');
  assert(at < apply && at < batch, 'headless default lands before the preset/manifest config is applied');
  const fm = html.match(/function __headlessPreprocessDefault\(\) \{[\s\S]*?\n  \}/);
  assert(fm, '__headlessPreprocessDefault() found');
  const c = { CONFIG: { studio: { gpuPreprocess: true, enabled: true } } };
  vm.createContext(c);
  vm.runInContext(fm[0] + '\n__headlessPreprocessDefault();', c);
  assert.strictEqual(c.CONFIG.studio.gpuPreprocess, false, 'headless default is the JS preprocess');
  assert(/' preprocess:' \+ _pp/.test(html), 'batch result line reports preprocess path');
  assert(/snap\.preprocess = _pp;/.test(html), 'PERF_JOB reports preprocess path');
  assert(/window\.__preprocessPath = _ppGL \? 'gl' : 'js';/.test(html), 'preprocess path recorded where it is chosen');
  assert(/'warn: GL preprocess latched the JS path: '/.test(html), 'GL latch warning is forwarded through cli_log');
  console.log('headless preprocess determinism: ok');
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
      recState: null, __primeOnly: false, __fontLoadPromise: null, fontDescriptor: null, __fontSwapPending: 0,
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
    ['studio font swap pending', { ctx: { __fontLoadPromise: {}, fontDescriptor: {}, __fontSwapPending: 1 } }],
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
    irPut: (i, cp, r, g, b, a) => { (seen.ir = seen.ir || []).push([i, cp, r, g, b]); },
    drawingContext: { drawImage: (c) => { seen.composited = c; } },
    document: { createElement: () => ({ width: 0, height: 0,
      addEventListener: (type, fn) => { seen.listeners = Object.assign(seen.listeners || {}, { [type]: fn }); } }) },
  };
  const glState = { lost: false, loseInDraw: false };
  const glStub = { isContextLost: () => glState.lost };
  ctx.window = { GlyphGrid: { wave6: {
    makeSprite: () => ({ gl: glStub }),
    drawSprite: (r, c, atlas, tw, th, inst, n, ink) => {
      Object.assign(seen, { atlas, tw, th, inst: Array.from(inst.subarray(0, n * 4)), n, ink });
      if (glState.loseInDraw) glState.lost = true;
    },
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
  /* ITER-073: every GL instance is mirrored into the GlyphFrame IR. */
  assert.strictEqual(seen.ir.length, seen.n, 'GL path writes one IR cell per instance');
  assert.deepStrictEqual(seen.ir.map((e) => e[1]), want.filter((_, k) => (k & 3) === 2).map((idx) => ' .:-=+*#%@'.charCodeAt(idx)),
    'GL IR codepoints are the ramp glyphs it instanced');
  assert(seen.ir.every((e) => e[2] === 17 && e[3] === 34 && e[4] === 51), 'GL IR ink is the palette ink');
  assert.deepStrictEqual(seen.inst, want, 'instance origins/idx/alpha match the CPU sprite blit');
  assert.strictEqual(seen.atlas, sprite.canvas, 'GL samples the CPU ramp sprite atlas');
  assert.deepStrictEqual([seen.tw, seen.th], [9, 12], 'tile size is the sprite tile, not the cell');
  assert.strictEqual(seen.composited, ctx._glCanvas, 'GL result is composited onto the p5 canvas');
  /* ITER-069: context loss latches the CPU sprite path — via the event, a
     pre-draw isContextLost(), or a loss during the draw (nothing composited). */
  assert(seen.listeners && typeof seen.listeners.webglcontextlost === 'function', 'webglcontextlost listener installed');
  seen.listeners.webglcontextlost();
  assert.strictEqual(ctx._glOk, false, 'webglcontextlost latches _glOk=false');
  const lostCases = [['lost before draw', { lost: true, loseInDraw: false }], ['lost during draw', { lost: false, loseInDraw: true }]];
  for (const [name, st] of lostCases) {
    ctx._glOk = true; Object.assign(glState, st); seen.composited = null;
    assert.strictEqual(ctx.__f(null, signal, cols, rows, cellW, cellH, { inks: ['#112233'] }, ' .:-=+*#%@', 9, false), false,
      `${name}: falls through to the CPU path`);
    assert.strictEqual(ctx._glOk, false, `${name}: latches _glOk=false`);
    assert.strictEqual(seen.composited, null, `${name}: nothing composited from the lost context`);
  }
  console.log('GL sprite-equivalent instance fill + context loss: ok');
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

/* ITER-067 (F10 Tier A): scanlines/kawaii-only stacks skip the readback,
   and their canvas-2D versions reproduce the RGBA kernels. */
{
  const crt = require('../src/lib/glyph-crt.js');
  /* kawaii: the fillRect blit on a fake source-over ctx vs the RGBA loop. */
  const w = 97, h = 61, opts = { intensity: 0.9, heartCount: 9, sparkleCount: 14, twinkleCount: 30 };
  for (const frameIdx of [0, 7, 33]) {
    const ref = new Uint8ClampedArray(w * h * 4);
    for (let i = 0; i < ref.length; i++) ref[i] = (i * 37) & 255;
    const out = Float64Array.from(ref);
    crt.applyChain({ width: w, height: h, data: ref }, null, { kawaii: Object.assign({ enabled: true }, opts) }, { frameIdx });
    let fill = [0, 0, 0], alpha = 1, rects = 0;
    const ctx = {
      set fillStyle(v) { fill = v.match(/\d+/g).map(Number); },
      set globalAlpha(v) { alpha = v; },
      fillRect(x, y, rw, rh) {
        assert(rw === 1 && rh === 1 && x >= 0 && y >= 0 && x < w && y < h, 'kawaii rects are in-bounds 1x1');
        const o = (y * w + x) * 4; rects++;
        for (let c = 0; c < 3; c++) out[o + c] = Math.round(out[o + c] * (1 - alpha) + fill[c] * alpha);
      },
    };
    crt.applyKawaiiCtx(ctx, w, h, opts, frameIdx);
    assert(rects > 0, 'kawaii ctx blit draws');
    let maxD = 0;
    for (let i = 0; i < ref.length; i++) if ((i & 3) !== 3) maxD = Math.max(maxD, Math.abs(ref[i] - out[i]));
    assert(maxD <= 1, `kawaii ctx blit within 1 LSB of the RGBA kernel (frame ${frameIdx}: ${maxD})`);
  }
  /* readback gate truth table + scanline mask rows. */
  const fnSrc = ['_hasImgDataStages', '_needsImgDataReadback', '_ensureScanlineOverlay'].map((n) => {
    const m = html.match(new RegExp('\\n  function ' + n + '\\([\\s\\S]*?\\n  \\}\\n'));
    assert(m, n + ' not found'); return m[0];
  }).join('\n');
  const rows = [];
  const sctx = { document: { createElement: () => ({ getContext: () => ({ fillRect: (x, y) => rows.push(y) }) }) } };
  vm.createContext(sctx);
  vm.runInContext('let _scanlineOverlay = null;\n' + fnSrc + '\nthis.n = _needsImgDataReadback; this.o = _ensureScanlineOverlay;', sctx);
  const on = (o) => Object.assign({ enabled: true }, o);
  assert.strictEqual(sctx.n({ vignette: on({ strength: 0.5 }) }), false, 'vignette only: no readback');
  assert.strictEqual(sctx.n({ scanlines: on({ intensity: 0.3 }) }), false, 'scanlines only: no readback');
  assert.strictEqual(sctx.n({ kawaii: on({ intensity: 0.8 }), scanlines: on({ intensity: 0.3 }) }), false, 'scanlines+kawaii: no readback');
  assert.strictEqual(sctx.n({ scanlines: on({ intensity: -0.3, strength: 0.4 }) }), true, 'brightening scanlines stay on the RGBA path');
  assert.strictEqual(sctx.n({ scanlines: on({ intensity: 0.3 }), bloom: on({ strength: 0.9 }) }), true, 'scanlines+bloom: readback');
  assert.strictEqual(sctx.n({ phosphorDecay: on({ decay: 0.8 }) }), true, 'phosphorDecay: readback');
  for (const [period, phase] of [[2, 0], [3, 1], [1, 0], [2, 0.5]]) {
    rows.length = 0;
    sctx.o(10, 9, period, phase);
    const ref = new Uint8ClampedArray(10 * 9 * 4).fill(200);
    crt.applyScanlines(ref, 10, 9, { intensity: 0.5, period, phase });
    const dark = []; for (let y = 0; y < 9; y++) if (ref[y * 40] !== 200) dark.push(y);
    assert.deepStrictEqual(rows, dark, `scanline mask rows match applyScanlines (period ${period}, phase ${phase})`);
  }
  console.log('postprocess Tier A overlays: ok');
}

/* ITER-073 (Phase 5.0): GlyphFrame IR.  Drive every drawer (v1, brightness
   CPU sprite/text + GL, shape, shape-edge-aware, edge-directional, flow) in
   a vm harness with p5 stubs and assert the IR holds exactly what was drawn:
   one cell per draw op, same codepoint, nothing for skipped cells.  Then
   captureGridSnapshot() must export from the IR in a non-brightness mode. */
{
  const grab = (re, what) => { const m = html.match(re); assert(m, what + ' not found'); return m[0]; };
  const fn = (name) => grab(new RegExp('\\n  function ' + name + '\\([\\s\\S]*?\\n  \\}\\n'), name + '()');
  const irBlock = grab(/  const glyphFrame = \{[\s\S]*?let _irR = 0, _irG = 0, _irB = 0, _irA = 1;\n/, 'GlyphFrame IR block');
  const flowBlock = grab(/  let _flowCps = null;[\s\S]*?\n  function drawFlowGrid\([\s\S]*?\n  \}\n/, 'drawFlowGrid()');
  const glBlock = grab(/  let _glInst = null;[^\n]*\n[\s\S]*?\n  function drawGridWebGL\([\s\S]*?\n  \}\n/, 'drawGridWebGL()')
    .replace(/^  let _glInst = null;/, 'var _glInst = null;');
  const snapSrc = grab(/    captureGridSnapshot\(\) \{[\s\S]*?\n    \},\n/, 'captureGridSnapshot()');
  const code = [irBlock, fn('applyCellFill'), fn('sampleCell'), fn('sampleCellInto'), fn('drawGlyphGrid'),
    fn('drawBrightnessGrid'), fn('drawShapeGrid'), fn('drawEdgeDirectionalGrid'), flowBlock, glBlock,
    'var __hook = {\n' + snapSrc + '};',
    'this.gf = () => glyphFrame; this.hook = __hook;'].join('\n');

  const cols = 4, rows = 2, cellW = 10, cellH = 10;
  const ramp = ' .:-=+*#%@';
  const hex = (h) => [1, 3, 5].map((k) => parseInt(h.slice(k, k + 2), 16));
  const mkColor = (lv) => ({ levels: lv.slice(0, 3).concat([255]),
    setAlpha(a) { this.levels = this.levels.slice(0, 3).concat([Math.round(a)]); } });
  const ops = [];
  const cellOf = (x, y) => Math.floor(y / cellH) * cols + Math.floor(x / cellW);
  const sprite = (cps) => ({ canvas: { cps }, tileW: 4, tileH: 4, halfW: 2, halfH: 2, steps: 16 });
  const octant = 0x1CD00;
  const atlas = { name: 't', glyphs: [{ cp: 0x41, s: 'A' }, { cp: octant, s: String.fromCodePoint(octant) }, { cp: 0x2588, s: '█' }] };
  const shapeIdx = Int32Array.from([0, 1, 2, 9, 1, 0, 2, 1]);       /* 9 → no glyph → skipped */
  const edgeTile = new Int16Array(0x10000).fill(-1); edgeTile[0x2500] = 0; edgeTile[0x2502] = 1;
  const gradLut = new Uint8Array(768); for (let k = 0; k < 256; k++) gradLut.set([k, 255 - k, 7], k * 3);
  const lut = new Float32Array(256); for (let k = 0; k < 256; k++) lut[k] = k / 255;
  const W = cols * cellW, H = rows * cellH;
  const pixels = new Uint8ClampedArray(W * H * 4);
  for (let y = 0; y < H; y++) for (let x = 0; x < W; x++) {
    const c = cellOf(x, y), o = (y * W + x) * 4;
    pixels.set(c === 0 ? [0xf0, 0xe8, 0xd8, 255] : [(c * 37) & 255, 20 + c * 25, 200 - c * 20, 255], o);   /* cell 0 = paper → skipped */
  }
  const src = { width: W, height: H, pixels };
  const ctx = {
    width: W, height: H, CENTER: 'center', frameCount: 1, recState: null, currentCellDepth: -1,
    MAX_RGB_DIST: Math.sqrt(3 * 255 * 255), src, lumBuf: new Float32Array(W * H),
    cellSignal: Float32Array.from([0, 0.1, 0.3, 0.5, 0.7, 0.9, 1, 0.6]), cellDepth: new Float32Array(8).fill(-1),
    PALETTES: { t: { bg: '#f0e8d8', inks: ['#112233', '#c04080'] } }, RAMPS: { r: ramp },
    CONFIG: { palette: 't', ramp: 'r', colorMode: 'monochrome', samplingStrategy: 'box', brightnessGamma: 1,
      font: { size: 8, family: 'mono' }, grid: { cols, rows }, renderer: 'cpu', seed: 1, flow: {} },
    color: (h) => mkColor(typeof h === 'string' ? hex(h) : [h, h, h]),
    red: (c) => c.levels[0], green: (c) => c.levels[1], blue: (c) => c.levels[2],
    lerpColor: (a, b, t) => mkColor(a.levels.map((v, k) => v + (b.levels[k] - v) * t)),
    noStroke() {}, textAlign() {}, textSize() {}, textFont() {}, fill() {},
    text: (ch, x, y) => ops.push([cellOf(x, y), ch.codePointAt(0)]),
    drawingContext: { globalAlpha: 1, drawImage: (cv, sx, sy, sw, sh, dx, dy) => {
      if (cv.cps) ops.push([cellOf(dx + 2, dy + 2), cv.cps[sx / 4]]);
    } },
    _gammaLut: lut, _buildGammaLut() {}, _gradColorLut: gradLut, _ensureGradColorLut() {},
    resolveGradientStops: () => [[0, 0, 0]], fontFamilyForActive: () => 'mono',
    __prefersReducedMotion: () => false, _needsImgDataReadback: () => false, _glOk: true,
    _stableIdx: (s, i, v, idx) => idx, _stableIdxInt: (i, idx) => idx, _ensureStabBuf() {},
    _stabDamped: 0, _stabSwapped: 0, _stabIdx: null, _stabOn: false, _sampleObj: { r: 0, g: 0, b: 0 },
    __pfAdd() {}, __pfFrame: null, __curCellKey: null, __curCellsFresh: false, __sourceSignalCache: null,
    performance: { now: () => 0 }, console,
    _ensureRampSprite: () => sprite(Array.from(ramp, (ch) => ch.charCodeAt(0))),
    _rampSpriteSpaceMask: Uint8Array.from(Array.from(ramp, (ch) => ch === ' ' ? 1 : 0)),
    _ensureRampDuotoneSprite: () => sprite(Array.from(ramp, (ch) => ch.charCodeAt(0))),
    _rampDuotoneSpaceMask: Uint8Array.from(Array.from(ramp, (ch) => ch === ' ' ? 1 : 0)),
    _ensureRampGradientSprite: () => sprite(Array.from(ramp, (ch) => ch.charCodeAt(0))),
    _rampGradientSpaceMask: Uint8Array.from(Array.from(ramp, (ch) => ch === ' ' ? 1 : 0)),
    glyphAtlas: atlas, _ensureSpriteAtlas: () => sprite(atlas.glyphs.map((g) => g.cp)),
    _ensureGlyphDuotoneSprite: () => sprite(atlas.glyphs.map((g) => g.cp)),
    _ensureGlyphGradientSprite: () => sprite(atlas.glyphs.map((g) => g.cp)),
    _edgeTile: edgeTile, _ensureEdgeSprite: () => sprite([0x2500, 0x2502]),
    _hexToRgb: hex, sourceImg: { width: W }, _glCanvas: null, __glDraws: 0,
    document: { createElement: () => ({ width: 0, height: 0, addEventListener() {} }) },
  };
  ctx.window = { GlyphGrid: {
    compat: { gateDepthFog: () => false },
    paletteMorph: { interpAtU: () => [1, 2, 3] },
    shapeIndex: { selectGrid: () => shapeIdx },
    edge: {
      selectGrid: () => Uint16Array.from([0x2500, 0x20, 0x2502, 0x41, 0x2500, 0x2502, 0x20, 0x42]),
      flowField: () => ({ coherence: Float32Array.from([1, 0, 1, 0, 1, 0, 1, 0]), energy: new Float32Array(8).fill(1),
        tangent: new Float32Array(8) }),
      flowGlyph: () => 0x2571,
    },
    wave6: { makeSprite: () => ({ gl: { isContextLost: () => false } }),
      drawSprite: (r, c, at, tw, th, inst, n) => {
        ctx.__glDraws++;
        for (let k = 0; k < n; k++) ops.push([cellOf(inst[k * 4] + 2, inst[k * 4 + 1] + 2), at.cps[inst[k * 4 + 2]]]);
      } },
  } };
  ctx.GlyphGrid = ctx.window.GlyphGrid;
  vm.createContext(ctx);
  vm.runInContext(code, ctx);

  const runs = [
    ['v1 mono', 'monochrome', () => ctx.drawGlyphGrid(src), 'brightness'],
    ['v1 preserve', 'preserve', () => ctx.drawGlyphGrid(src), 'brightness'],
    ['brightness GL', 'monochrome', () => { ctx.CONFIG.renderer = 'webgl'; ctx.drawBrightnessGrid(null, ctx.cellSignal, cols, rows, cellW, cellH); ctx.CONFIG.renderer = 'cpu'; }, 'brightness'],
    ...['monochrome', 'duotone', 'gradient', 'preserve'].flatMap((cm) => [
      ['brightness ' + cm, cm, () => ctx.drawBrightnessGrid(null, ctx.cellSignal, cols, rows, cellW, cellH), 'brightness'],
      ['shape ' + cm, cm, () => ctx.drawShapeGrid(src, cols, rows, cellW, cellH, 10, 10, false), 'shape'],
      ['shape-edge-aware ' + cm, cm, () => ctx.drawShapeGrid(src, cols, rows, cellW, cellH, 10, 10, true), 'shape-edge-aware'],
      ['edge-directional ' + cm, cm, () => ctx.drawEdgeDirectionalGrid(cols, rows, cellW, cellH), 'edge-directional'],
      ['flow ' + cm, cm, () => ctx.drawFlowGrid(cols, rows, cellW, cellH), 'flow'],
    ]),
  ];
  let seq = 0;
  for (const [name, cm, run, mode] of runs) {
    ctx.CONFIG.colorMode = cm;
    /* poison the IR so a drawer that forgets irBegin/irPut can't pass on stale data */
    const before = ctx.gf();
    if (before.cp.length) { before.cp.fill(0xDEAD); }
    ops.length = 0;
    run();
    const f = ctx.gf();
    assert.strictEqual(f.seq, ++seq, name + ': drawer called irBegin once');
    assert.deepStrictEqual([f.cols, f.rows, f.mode], [cols, rows, mode], name + ': IR shape + mode');
    assert.strictEqual(Object.prototype.toString.call(f.cp), '[object Uint32Array]', name + ': cp is Uint32');
    const drawn = new Map(ops);
    assert(drawn.size > 0 && drawn.size === ops.length && drawn.size < cols * rows, name + ': drew a strict subset of cells once each');
    const ir = new Map();
    for (let i = 0; i < cols * rows; i++) if (f.cp[i]) ir.set(i, f.cp[i]);
    assert.deepStrictEqual([...ir].sort((a, b) => a[0] - b[0]), [...drawn].sort((a, b) => a[0] - b[0]),
      name + ': IR cells == drawn cells, same codepoints');
    if (cm === 'monochrome') {
      for (const i of ir.keys()) assert.deepStrictEqual(Array.from(f.fg.subarray(i * 3, i * 3 + 3)), [0x11, 0x22, 0x33], name + ': mono ink in IR');
    }
    if (cm === 'gradient' && mode !== 'brightness') {
      const i = 4, sig = ctx.cellSignal[i], lo = ((sig * 255) | 0) * 3;
      if (ir.has(i)) assert.deepStrictEqual(Array.from(f.fg.subarray(12, 15)), Array.from(gradLut.subarray(lo, lo + 3)), name + ': gradient LUT ink');
    }
  }
  assert.strictEqual(ctx.__glDraws, 1, 'the GL run went through drawGridWebGL, not the CPU fallback');
  assert.strictEqual(ctx._glOk, true, 'GL path did not latch off');
  /* alpha: brightness mono cell 3 (signal 0.5) → min(1, 0.55) * 255 */
  ctx.CONFIG.colorMode = 'monochrome';
  ctx.drawBrightnessGrid(null, ctx.cellSignal, cols, rows, cellW, cellH);
  assert.strictEqual(ctx.gf().alpha[3], Math.round(0.5 * 1.1 * 255), 'IR alpha is the drawn opacity');
  assert.strictEqual(ctx.gf().alpha[6], 255, 'IR alpha clamps at 1');
  /* captureGridSnapshot reads the IR: shape mode exports (incl. an octant). */
  ctx.CONFIG.colorMode = 'duotone';
  ctx.drawShapeGrid(src, cols, rows, cellW, cellH, 10, 10, false);
  const snap = ctx.hook.captureGridSnapshot();
  assert(snap && snap.mode === 'shape', 'snapshot available in shape mode');
  assert.strictEqual(snap.cells.length, [...ctx.gf().cp.subarray(0, 8)].filter((v) => v).length, 'snapshot = IR cells');
  assert(snap.cells.some((c) => c.c === String.fromCodePoint(octant)), 'octant survives the IR (Uint32)');
  assert.strictEqual(snap.bg, '#f0e8d8', 'snapshot bg from IR');
  ctx.CONFIG.grid.cols = 5;
  assert.strictEqual(ctx.hook.captureGridSnapshot(), null, 'stale IR (grid resized) → null');
  ctx.CONFIG.grid.cols = cols;
  /* grow-only: a same-or-smaller grid reuses the buffers. */
  const cpBuf = ctx.gf().cp;
  ctx.drawEdgeDirectionalGrid(cols, rows, cellW, cellH);
  assert.strictEqual(ctx.gf().cp, cpBuf, 'IR buffers are reused across frames');
  console.log('GlyphFrame IR: ok (' + runs.length + ' drawer/colour-mode runs)');
}

/* ITER-073 (Phase 5.0): FrameSource adapter — still + GIF behind one seam,
   frame selection identical to the pre-adapter inline GIF branch. */
{
  const m = html.match(/  let __frameSource = null;[\s\S]*?\n  function frameSourceFor\([\s\S]*?\n  \}\n/);
  assert(m, 'FrameSource adapter not found');
  assert(/const _fsrc = frameSourceFor\(sourceImg\);\s*_fsrc\.frameAt\(t \* 1000\);\s*window\.__sourceFrameIdx = _fsrc\.index;/.test(html),
    'draw() routes the source through the adapter and publishes __sourceFrameIdx');
  const ctx = { __sourceRevision: 1 };
  vm.createContext(ctx);
  vm.runInContext(m[0] + '\nthis.make = makeFrameSource; this.forImg = frameSourceFor;', ctx);
  const delays = [100, 0, 50, 70];
  const setCalls = [];
  const gif = {
    gifProperties: { numFrames: 4, displayIndex: 0, frames: delays.map((d) => ({ delay: d })) },
    numFrames: () => 4,
    setFrame(i) { setCalls.push(i); this.gifProperties.displayIndex = i; },
  };
  const fs1 = ctx.forImg(gif);
  assert.strictEqual(fs1.kind, 'gif');
  assert.strictEqual(fs1.durationMs, 320, 'GIF loop length uses delay||100');
  /* legacy inline algorithm (pre-ITER-073 draw()) as the oracle */
  const legacy = (t) => {
    const total = delays.reduce((s, d) => s + (d || 100), 0);
    const loopT = (t * 1000) % total;
    let acc = 0, target = 3;
    for (let i = 0; i < 4; i++) { acc += (delays[i] || 100); if (loopT < acc) { target = i; break; } }
    return target;
  };
  let prev = gif.gifProperties.displayIndex, wantCalls = 0;
  for (let f = 0; f < 200; f++) {
    const t = f / 30;
    const want = legacy(t);
    assert.strictEqual(fs1.frameAt(t * 1000), gif, 'frameAt returns the source image');
    assert.strictEqual(fs1.index, want, 'GIF frame at t=' + t.toFixed(3));
    if (want !== prev) wantCalls++;
    prev = want;
  }
  assert.strictEqual(setCalls.length, wantCalls, 'setFrame only when the frame changes');
  assert.strictEqual(ctx.forImg(gif), fs1, 'adapter memoised per (img, revision)');
  ctx.__sourceRevision = 2;
  assert.notStrictEqual(ctx.forImg(gif), fs1, 'revision bump rebuilds the adapter');
  const still = { width: 10 };
  const fs2 = ctx.forImg(still);
  assert.deepStrictEqual([fs2.kind, fs2.durationMs, fs2.index], ['still', 0, -1], 'still adapter');
  assert.strictEqual(fs2.frameAt(1234), still, 'still frameAt returns the image');
  const one = { gifProperties: { numFrames: 1, frames: [{ delay: 100 }] }, numFrames: () => 1 };
  assert.strictEqual(ctx.make(one, 3).kind, 'still', 'single-frame GIF is a still');
  console.log('FrameSource adapter: ok');
}

/* ITER-074: the terminal-export unit tests ride along with `npm test`. */
require('./glyph-text-export.test.js');
