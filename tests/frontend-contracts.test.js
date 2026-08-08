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
];

for (const [name, pattern] of contracts) {
  assert(pattern.test(html), `frontend contract missing: ${name}`);
}

console.log(`frontend contracts: ok (${inlineScripts.length} inline script, ${contracts.length} invariants)`);
