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
];

for (const [name, pattern] of contracts) {
  assert(pattern.test(html), `frontend contract missing: ${name}`);
}

/* Negative contracts: the wall-clock sleeps ITER-047 removed must not come back. */
const banned = [
  ['headless start-up sleep', /setTimeout\(tryHeadlessRender,\s*\d+\)/],
  ['post-load / post-config settle sleeps', /await new Promise\(function \(r\) \{ setTimeout\(r, (300|200|1500)\); \}\);/],
  ['batch settle timer', /opts\.settleMs/],
];
for (const [name, pattern] of banned) {
  assert(!pattern.test(html), `frontend contract violated: ${name} is back`);
}

console.log(`frontend contracts: ok (${inlineScripts.length} inline script, ${contracts.length} invariants)`);
