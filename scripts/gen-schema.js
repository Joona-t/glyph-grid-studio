#!/usr/bin/env node
/* scripts/gen-schema.js — R7 (ITER-056): the preset schema is generated,
   never hand-edited.

   Reads the LIVE renderer defaults and option tables and writes
   src/glyph-grid.schema.json (JSON Schema 2020-12):
     - CONFIG / RAMPS / PALETTES / SCENES — evaluated from src/index.html's
       inline script in a vm sandbox (same loader idea as
       tests/frontend-contracts.test.js; only the literal tables are run,
       not p5 setup),
     - dither modes — the dispatch in src/lib/glyph-dither.js apply(),
     - glyph sets — src/glyph-sets/*.json,
     - selection / sampling / colour / renderer / prefilter options and the
       postprocess stage list — the Tweakpane panel in src/lib/glyph-studio.js,
     - compat versions — src/lib/glyph-compat.js COMPAT_VERSIONS,
     - preset format version + $id — glyph-studio.js PRESET_SCHEMA_VERSION.
   Where two sources list the same enum (panel vs dither dispatcher, panel
   vs glyph-sets/), they must agree or generation fails: that drift is the
   bug this file exists to catch.

   Usage: node scripts/gen-schema.js           # (re)write the schema
          node scripts/gen-schema.js --check   # exit 1 if it is stale (run-all A9)
   No dependencies. */
'use strict';

const fs = require('fs');
const path = require('path');
const vm = require('vm');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'src', 'glyph-grid.schema.json');
const read = (rel) => fs.readFileSync(path.join(ROOT, rel), 'utf8');

function fail(msg) {
  console.error('gen-schema: ' + msg);
  process.exit(2);
}

/* Balanced-brace slice starting at the `{` (or `[`) at/after `from`.
   Skips string literals and comments so braces inside them don't count. */
function sliceBalanced(src, from) {
  const open = src.slice(from).search(/[{[]/);
  if (open < 0) fail('no literal after offset ' + from);
  let i = from + open;
  const start = i;
  const stack = [];
  for (; i < src.length; i++) {
    const c = src[i];
    if (c === '/' && src[i + 1] === '/') { i = src.indexOf('\n', i); continue; }
    if (c === '/' && src[i + 1] === '*') { i = src.indexOf('*/', i + 2) + 1; continue; }
    if (c === '\'' || c === '"' || c === '`') {
      for (i++; i < src.length && src[i] !== c; i++) if (src[i] === '\\') i++;
      continue;
    }
    if (c === '{' || c === '[') stack.push(c);
    else if (c === '}' || c === ']') {
      stack.pop();
      if (!stack.length) return src.slice(start, i + 1);
    }
  }
  fail('unbalanced literal at offset ' + start);
}

function evalLiteral(text, label) {
  try { return vm.runInNewContext('(' + text + ')', {}, { filename: label }); }
  catch (e) { fail('cannot evaluate ' + label + ': ' + e.message); }
}

function literalAfter(src, anchor, label) {
  const at = src.indexOf(anchor);
  if (at < 0) fail('anchor not found for ' + label + ': ' + anchor);
  return evalLiteral(sliceBalanced(src, at + anchor.length), label);
}

/* ---- 1. index.html inline tables ---- */
const html = read('src/index.html');
const inline = Array.from(html.matchAll(/<script(?![^>]*\bsrc=)[^>]*>([\s\S]*?)<\/script>/gi))
  .map((m) => m[1]).find((s) => s.includes('const CONFIG = {'));
if (!inline) fail('src/index.html has no inline script defining CONFIG');
const CONFIG = literalAfter(inline, 'const CONFIG =', 'CONFIG');
const RAMPS = literalAfter(inline, 'const RAMPS =', 'RAMPS');
const PALETTES = literalAfter(inline, 'const PALETTES =', 'PALETTES');
const SCENES = literalAfter(inline, 'const SCENES =', 'SCENES');   // methods are defined, never run

/* ---- 2. dither dispatcher ---- */
const ditherSrc = read('src/lib/glyph-dither.js');
const applyAt = ditherSrc.indexOf('function apply(');
if (applyAt < 0) fail('glyph-dither.js apply() not found');
const applyBody = sliceBalanced(ditherSrc, ditherSrc.indexOf(')', applyAt));
const DITHER_MODES = [...new Set(Array.from(applyBody.matchAll(/mode === '([^']+)'/g), (m) => m[1]))];

/* ---- 3. glyph sets on disk ---- */
const GLYPH_SETS = fs.readdirSync(path.join(ROOT, 'src', 'glyph-sets'))
  .filter((f) => f.endsWith('.json')).map((f) => f.slice(0, -5)).sort();

/* ---- 4. panel option tables ---- */
const studioSrc = read('src/lib/glyph-studio.js');
function panelOptions(anchor, label) {
  const at = studioSrc.indexOf(anchor);
  if (at < 0) fail('panel anchor not found for ' + label + ': ' + anchor);
  const optAt = studioSrc.indexOf('options:', at);
  return Object.values(evalLiteral(sliceBalanced(studioSrc, optAt), label + ' options'));
}
const SELECTION_MODES = panelOptions("config, 'selectionMode', {", 'selectionMode');
const SAMPLING = panelOptions("config, 'samplingStrategy', {", 'samplingStrategy');
const COLOR_MODES = panelOptions("config, 'colorMode', {", 'colorMode');
const RENDERERS = panelOptions("config, 'renderer', {", 'renderer');
const PREFILTER_MODES = panelOptions("config.prefilter, 'mode', {", 'prefilter.mode');
const PANEL_DITHERS = panelOptions("config.dither, 'mode', {", 'dither.mode');
const PANEL_GLYPH_SETS = panelOptions("config, 'glyphSet', {", 'glyphSet').filter((v) => v !== 'null');
const stagesMatch = studioSrc.match(/\[([^\]]+)\]\.forEach\(function \(key\) \{\s*if \(!config\.postprocess\[key\]\) return;/);
if (!stagesMatch) fail('postprocess stage list not found in glyph-studio.js');
const STAGES = evalLiteral('[' + stagesMatch[1] + ']', 'postprocess stages');
/* Stage blocks the panel creates when the scene didn't declare them. */
const PANEL_STAGE_DEFAULTS = {};
for (const m of studioSrc.matchAll(/config\.postprocess\.(\w+) = \{/g)) {
  PANEL_STAGE_DEFAULTS[m[1]] = evalLiteral(sliceBalanced(studioSrc, m.index + m[0].length - 1), m[1] + ' defaults');
}

const compatSrc = read('src/lib/glyph-compat.js');
const COMPAT = literalAfter(compatSrc, 'COMPAT_VERSIONS = Object.freeze(', 'COMPAT_VERSIONS');
const verMatch = studioSrc.match(/var PRESET_SCHEMA_VERSION = (\d+);/);
const idMatch = studioSrc.match(/var PRESET_SCHEMA_ID = '([^']+)' \+\s*PRESET_SCHEMA_VERSION \+ '([^']+)';/);
if (!verMatch || !idMatch) fail('PRESET_SCHEMA_VERSION / PRESET_SCHEMA_ID not found in glyph-studio.js');
const VERSION = Number(verMatch[1]);
const SCHEMA_ID = idMatch[1] + VERSION + idMatch[2];

/* ---- drift checks: two sources, one truth ---- */
function sameSet(a, b, what) {
  const sa = [...a].sort().join(','), sb = [...b].sort().join(',');
  if (sa !== sb) fail(what + ' disagree:\n  ' + sa + '\n  ' + sb);
}
sameSet(DITHER_MODES, PANEL_DITHERS, 'dither modes (glyph-dither.js apply vs panel)');
sameSet(GLYPH_SETS, PANEL_GLYPH_SETS, 'glyph sets (src/glyph-sets/ vs panel)');
for (const k of ['palette', 'ramp', 'scene']) {
  const table = { palette: PALETTES, ramp: RAMPS, scene: SCENES }[k];
  if (!(CONFIG[k] in table)) fail('CONFIG.' + k + ' default "' + CONFIG[k] + '" is not in its table');
}

/* ---- schema construction ---- */
const DESCRIPTIONS = {
  compat: 'Pipeline generation switch. "v1" disables every v2 stage gate.',
  renderer: 'Render substrate. webgl is opt-in and falls back to cpu.',
  canvas: 'Canvas size in px before fit-to-image.',
  grid: 'Glyph grid dimensions (cells).',
  font: 'Glyph font family and pixel size.',
  ramp: 'Density ramp used by brightness selection.',
  brightnessGamma: 'Curve on source brightness; <0.5 = high-contrast stipple.',
  bgThreshold: 'Source luminance above this renders as palette bg. 0 = off.',
  invertSignal: 'Flip bright/dark so dark detail becomes ink.',
  samplingStrategy: 'How a cell reads its source pixels.',
  colorMode: 'How the palette maps to cells.',
  palette: 'Background colour + ink stops.',
  scene: 'Built-in scene (studio renders the loaded source).',
  animation: 'Loop timing. Frames for `--frames auto` derive from duration x fps.',
  seed: 'Deterministic seed for noise, dither phase and jitter.',
  record: 'Internal recording flag.',
  glyphSet: 'Shape atlas for shape selection modes; null = ramp only.',
  selectionMode: 'How glyphs are picked per cell.',
  dither: 'Quantization noise pattern (brightness selection only).',
  prefilter: 'Source-wide preprocessing.',
  postprocess: 'Per-stage post effects; each stage has `enabled` plus its params.',
  depth: 'Depth-map fog zones.',
  paletteMorph: 'OKLab palette interpolation over time; null = off.',
  temporalStability: 'Anti-flicker Schmitt trigger on the ramp index.',
  flow: 'ETF tuning for selectionMode "flow".',
  recording: 'Recording backend.',
  dispersal: 'Stardust drift-and-fade effect.',
  studio: 'Studio panel, fit-to-image and breathing (EMA) constants.',
};
const ENUMS = {
  compat: COMPAT,
  renderer: RENDERERS,
  ramp: Object.keys(RAMPS),
  samplingStrategy: SAMPLING,
  colorMode: COLOR_MODES,
  palette: Object.keys(PALETTES),
  scene: Object.keys(SCENES),
  selectionMode: SELECTION_MODES,
  'dither.mode': DITHER_MODES,
  'prefilter.mode': PREFILTER_MODES,
};

function node(value, dotted) {
  const out = {};
  if (ENUMS[dotted]) {
    out.type = 'string';
    out.enum = ENUMS[dotted];
    out.default = value;
  } else if (value === null) {
    out.type = ['object', 'null'];
    out.default = null;
  } else if (Array.isArray(value)) {
    out.type = 'array';
    out.default = value;
  } else if (typeof value === 'object') {
    out.type = 'object';
    out.properties = {};
    for (const k of Object.keys(value)) out.properties[k] = node(value[k], dotted ? dotted + '.' + k : k);
    out.additionalProperties = true;
  } else {
    out.type = typeof value === 'number' ? 'number' : typeof value;
    out.default = value;
  }
  return out;
}

const properties = {
  $schema: { type: 'string', description: 'Schema this preset was written against.' },
  version: { type: 'integer', minimum: 1, description: 'Preset format version.', default: VERSION },
};
for (const k of Object.keys(CONFIG)) {
  let p = node(CONFIG[k], k);
  if (k === 'glyphSet') {
    p = { type: ['string', 'null'], enum: [null].concat(GLYPH_SETS), default: CONFIG.glyphSet };
  }
  if (k === 'postprocess') {
    const stages = {};
    for (const s of STAGES) {
      const defaults = Object.assign({ enabled: false }, PANEL_STAGE_DEFAULTS[s] || {}, CONFIG.postprocess[s] || {});
      stages[s] = node(defaults, 'postprocess.' + s);
    }
    p = { type: 'object', properties: stages, additionalProperties: true };
  }
  if (DESCRIPTIONS[k]) p = Object.assign({ description: DESCRIPTIONS[k] }, p);
  properties[k] = p;
}
const missing = Object.keys(CONFIG).filter((k) => !DESCRIPTIONS[k]);
if (missing.length) fail('CONFIG keys without a description in gen-schema.js: ' + missing.join(', '));

const schema = {
  $schema: 'https://json-schema.org/draft/2020-12/schema',
  $id: SCHEMA_ID,
  title: 'Glyph Grid Studio preset',
  description: 'A preset is a partial CONFIG: every key is optional and deep-merges onto the renderer defaults. ' +
    'Generated by scripts/gen-schema.js from the live CONFIG — do not edit by hand.',
  type: 'object',
  properties,
  additionalProperties: true,
  'x-unknown-key-policy': 'warn',
  'x-preset-version': VERSION,
};
const text = JSON.stringify(schema, null, 2) + '\n';

if (process.argv.includes('--check')) {
  const current = fs.existsSync(OUT) ? fs.readFileSync(OUT, 'utf8') : '';
  if (current !== text) {
    console.error('gen-schema: src/glyph-grid.schema.json is stale — run `node scripts/gen-schema.js` and commit it');
    process.exit(1);
  }
  console.log('gen-schema: schema up to date (' + Object.keys(CONFIG).length + ' CONFIG keys)');
} else {
  fs.writeFileSync(OUT, text);
  console.log('gen-schema: wrote ' + path.relative(ROOT, OUT) + ' (' + Object.keys(CONFIG).length +
    ' CONFIG keys, ' + Object.keys(PALETTES).length + ' palettes, ' + DITHER_MODES.length + ' dithers, ' +
    STAGES.length + ' postprocess stages)');
}
