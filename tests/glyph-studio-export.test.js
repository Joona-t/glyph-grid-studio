'use strict';

const assert = require('assert');
const fs = require('fs');
const vm = require('vm');

function loadStudio(invokeImpl) {
  let zipConstructs = 0;
  const clicked = [];
  const buttons = new Map();
  const monitors = new Map();
  function element(tag) {
    return {
      tagName: tag,
      style: {},
      parentNode: { removeChild() {} },
      setAttribute() {},
      appendChild() {},
      remove() {},
      click() { clicked.push(this.download || tag); },
    };
  }
  const storage = new Map();
  const window = {
    __TAURI__: {
      core: { invoke: invokeImpl },
    },
    JSZip: function JSZip() {
      zipConstructs++;
      this.file = function () {};
      this.generateAsync = function () { return Promise.resolve({ zip: true }); };
    },
    addEventListener() {},
  };
  function Binding(key) {
    this.key = key;
    this.handlers = {};
    this.element = element('binding');
    this.disabled = false;
  }
  Binding.prototype.on = function (name, fn) { this.handlers[name] = fn; return this; };
  Binding.prototype.dispose = function () {};
  Binding.prototype.fire = function (name, value) {
    if (this.handlers[name]) this.handlers[name]({ value, target: this });
  };
  function Folder() { this.children = []; this.handlers = {}; }
  Folder.prototype.addFolder = function () { const child = new Folder(); this.children.push(child); return child; };
  Folder.prototype.addInput = function (_obj, key) { const api = new Binding(key); this.children.push(api); return api; };
  Folder.prototype.addMonitor = function (_obj, key) {
    const api = new Binding(key);
    this.children.push(api);
    monitors.set(key, _obj);
    return api;
  };
  Folder.prototype.addButton = function (options) {
    const api = new Binding(options.title);
    this.children.push(api);
    buttons.set(options.title, api);
    return api;
  };
  Folder.prototype.on = function (name, fn) { this.handlers[name] = fn; return this; };
  function Pane() { Folder.call(this); }
  Pane.prototype = Object.create(Folder.prototype);
  Pane.prototype.constructor = Pane;
  Pane.prototype.refresh = function () {};
  window.Tweakpane = { Pane };
  function testTimeout(fn, ms) {
    const timer = setTimeout(fn, ms);
    if (ms > 250 && timer.unref) timer.unref();
    return timer;
  }
  function testInterval(fn, ms) {
    const timer = setInterval(fn, ms);
    if (timer.unref) timer.unref();
    return timer;
  }
  const context = {
    window,
    document: {
      head: { appendChild() {} },
      body: { appendChild() {} },
      createElement: element,
      getElementById() { return null; },
      querySelector(selector) { return selector === 'canvas' ? { width: 10, height: 10 } : null; },
    },
    localStorage: {
      get length() { return storage.size; },
      key(index) { return Array.from(storage.keys())[index] || null; },
      getItem(key) { return storage.has(key) ? storage.get(key) : null; },
      setItem(key, value) { storage.set(key, String(value)); },
    },
    URL: {
      createObjectURL() { return 'blob:test'; },
      revokeObjectURL() {},
    },
    Blob,
    Promise,
    Date,
    JSON,
    Math,
    console,
    setTimeout: testTimeout,
    clearTimeout,
    setInterval: testInterval,
    clearInterval,
    requestAnimationFrame(fn) { fn(); },
    navigator: {},
    location: { origin: 'https://example.test', pathname: '/', search: '' },
  };
  window.URL = context.URL;
  window.document = context.document;
  vm.runInNewContext(
    fs.readFileSync(require.resolve('../src/lib/glyph-studio.js'), 'utf8'),
    context,
    { filename: 'glyph-studio.js' },
  );
  return {
    studio: window.GlyphStudio,
    clicked,
    buttons,
    monitors,
    window,
    zipConstructs: () => zipConstructs,
  };
}

function recorder(frames, helpers) {
  let options;
  return {
    hook: {
      beginRecord(value) {
        options = value;
        value.onFinish(null, helpers || { getFrames: () => frames });
      },
      getRecState() { return null; },
      getConfig() { return { animation: { fps: 24 } }; },
    },
    options: () => options,
  };
}

async function settle() {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

async function nativeSuccessSkipsZip() {
  const calls = [];
  const env = loadStudio((cmd, args) => {
    calls.push({ cmd, args });
    if (cmd === 'kv_load_all') return Promise.resolve('{}');
    if (cmd === 'save_gif_real') return Promise.resolve('/tmp/out.gif');
    return Promise.reject(new Error('unexpected command ' + cmd));
  });
  let fallbackCalls = 0;
  const rec = recorder(['aaa', 'bbb'], {
    getFrames: () => ['aaa', 'bbb'],
    getFallbackZip() { fallbackCalls++; return Promise.resolve({ zip: true }); },
  });

  env.studio.recordGIF(rec.hook, 2, null, null, 40, null, null, null);
  await settle();

  assert.strictEqual(rec.options().collectFrames, true);
  assert.strictEqual(rec.options().collectZip, false);
  assert.strictEqual(fallbackCalls, 0, 'fallback ZIP must stay lazy on native success');
  assert.strictEqual(env.zipConstructs(), 0, 'JSZip must not be instantiated on native success');
  const save = calls.find((call) => call.cmd === 'save_gif_real');
  assert(save, 'native GIF encoder should be invoked');
  assert.deepStrictEqual(Array.from(save.args.frames, (frame) => frame.b64), ['aaa', 'bbb']);
}

async function nativeFailureBuildsFallbackOnce() {
  const env = loadStudio((cmd) => {
    if (cmd === 'kv_load_all') return Promise.resolve('{}');
    if (cmd === 'save_gif_real') return Promise.reject('encoder unavailable');
    return Promise.reject(new Error('unexpected command ' + cmd));
  });
  let fallbackCalls = 0;
  const rec = recorder(['aaa'], {
    getFrames: () => ['aaa'],
    getFallbackZip() { fallbackCalls++; return Promise.resolve({ zip: true }); },
  });

  env.studio.recordGIF(rec.hook, 1, null, null, 40, null, null, null);
  await settle();

  assert.strictEqual(fallbackCalls, 1, 'native failure should build exactly one fallback ZIP');
  assert.strictEqual(env.clicked.length, 1, 'fallback ZIP should be downloaded');
}

async function stagedUiFlowStreamsWithoutRetainingFrames() {
  const calls = [];
  const env = loadStudio((cmd, args) => {
    calls.push({ cmd, args });
    if (cmd === 'kv_load_all') return Promise.resolve('{}');
    if (cmd === 'choose_export_path') return Promise.resolve('/tmp/staged.gif');
    if (cmd === 'begin_export_capture') return Promise.resolve();
    if (cmd === 'push_export_frame') return Promise.resolve();
    if (cmd === 'finish_gif_export') return Promise.resolve('/tmp/staged.gif');
    return Promise.reject(new Error('unexpected command ' + cmd));
  });
  let recState = null;
  let beginOptions = null;
  const hook = {
    beginRecord(options) {
      beginOptions = options;
      recState = { frameIdx: 0, done: false };
      Promise.resolve().then(async () => {
        for (let index = 0; index < 2; index++) {
          await options.onFrame({ jobId: options.jobId, index, b64: 'frame-' + index, total: 2 });
          recState.frameIdx++;
          if (options.onProgress) options.onProgress(recState.frameIdx, 2, options.jobId);
        }
        recState.done = true;
        options.onFinish(null, { getFrames: () => [] });
        recState = null;
      });
    },
    cancelRecord() { recState = null; return true; },
    getRecState() { return recState; },
    getConfig() { return { animation: { fps: 25 } }; },
  };
  env.window.__glyphGridTest = hook;
  env.studio.init({
    config: {
      studio: { enabled: true },
      animation: { fps: 25, duration: 0.08, loop: true },
      grid: { cols: 80, rows: 40 },
      font: { family: 'monospace', size: 8 },
      ramp: 'gradient',
      brightnessGamma: 1,
      samplingStrategy: 'average',
      colorMode: 'monochrome',
      palette: 'cream-paper',
      selectionMode: 'brightness',
      glyphSet: null,
    },
    testHook: hook,
  });
  const exportButton = env.buttons.get('Export GIF');
  assert(exportButton, 'Export GIF button should exist in native UI');
  exportButton.fire('click');
  await settle();
  await settle();

  assert.strictEqual(beginOptions.collectFrames, false);
  assert.strictEqual(beginOptions.collectZip, false);
  assert.strictEqual(typeof beginOptions.onFrame, 'function');
  assert.strictEqual(calls.filter((call) => call.cmd === 'push_export_frame').length, 2);
  assert(calls.some((call) => call.cmd === 'finish_gif_export'));
  assert(!calls.some((call) => call.cmd === 'save_gif_to_path'));
  assert.strictEqual(env.zipConstructs(), 0);
}

async function cancelButtonStopsCorrelatedCaptureAndNativeJob() {
  const calls = [];
  const env = loadStudio((cmd, args) => {
    calls.push({ cmd, args });
    if (cmd === 'kv_load_all') return Promise.resolve('{}');
    if (cmd === 'choose_export_path') return Promise.resolve('/tmp/cancel.gif');
    if (cmd === 'begin_export_capture') return Promise.resolve();
    if (cmd === 'cancel_export') return Promise.resolve(true);
    return Promise.reject(new Error('unexpected command ' + cmd));
  });
  let beginOptions;
  let cancelledJob;
  const hook = {
    beginRecord(options) { beginOptions = options; },
    cancelRecord(jobId) { cancelledJob = jobId; return true; },
    getRecState() { return beginOptions ? { frameIdx: 0, done: false } : null; },
    getConfig() { return { animation: { fps: 25 } }; },
  };
  env.window.__glyphGridTest = hook;
  env.studio.init({
    config: {
      studio: { enabled: true }, animation: { fps: 25, duration: 0.08, loop: true },
      grid: { cols: 80, rows: 40 }, font: { family: 'monospace', size: 8 },
      ramp: 'gradient', brightnessGamma: 1, samplingStrategy: 'average',
      colorMode: 'monochrome', palette: 'cream-paper', selectionMode: 'brightness', glyphSet: null,
    },
    testHook: hook,
  });
  env.buttons.get('Export GIF').fire('click');
  await settle();
  assert(beginOptions, 'capture should start after destination and native staging succeed');
  env.buttons.get('Cancel export').fire('click');
  await settle();

  assert.strictEqual(cancelledJob, beginOptions.jobId);
  const nativeCancel = calls.find((call) => call.cmd === 'cancel_export');
  assert(nativeCancel, 'Cancel export should signal the native job');
  assert.strictEqual(nativeCancel.args.jobId, beginOptions.jobId);
}

async function cancelBeforeBeginCleansLateNativeCapture() {
  const calls = [];
  let resolveBegin;
  const beginPending = new Promise((resolve) => { resolveBegin = resolve; });
  let cancelCount = 0;
  const env = loadStudio((cmd, args) => {
    calls.push({ cmd, args });
    if (cmd === 'kv_load_all') return Promise.resolve('{}');
    if (cmd === 'choose_export_path') return Promise.resolve('/tmp/race.gif');
    if (cmd === 'begin_export_capture') return beginPending;
    if (cmd === 'cancel_export') return Promise.resolve(++cancelCount > 1);
    return Promise.reject(new Error('unexpected command ' + cmd));
  });
  let beganRecording = false;
  const hook = {
    beginRecord() { beganRecording = true; },
    cancelRecord() { return false; },
    getRecState() { return null; },
    getConfig() { return { animation: { fps: 25 } }; },
  };
  env.window.__glyphGridTest = hook;
  env.studio.init({
    config: {
      studio: { enabled: true }, animation: { fps: 25, duration: 0.08, loop: true },
      grid: { cols: 80, rows: 40 }, font: { family: 'monospace', size: 8 },
      ramp: 'gradient', brightnessGamma: 1, samplingStrategy: 'average',
      colorMode: 'monochrome', palette: 'cream-paper', selectionMode: 'brightness', glyphSet: null,
    },
    testHook: hook,
  });
  env.buttons.get('Export GIF').fire('click');
  await settle();
  assert(calls.some((call) => call.cmd === 'begin_export_capture'));
  env.buttons.get('Cancel export').fire('click');
  await settle();
  assert.strictEqual(cancelCount, 1, 'first cancel may race before native staging exists');
  resolveBegin();
  await settle();
  await settle();

  assert.strictEqual(cancelCount, 2, 'late begin completion must trigger immediate cleanup cancel');
  assert.strictEqual(beganRecording, false, 'cancelled initialization must never start frame capture');
}

async function cancelledSaveDialogSettlesVisiblePhase() {
  for (const cancelMode of ['empty', 'rejected']) {
    const env = loadStudio((cmd) => {
      if (cmd === 'kv_load_all') return Promise.resolve('{}');
      if (cmd === 'choose_export_path') {
        return cancelMode === 'empty' ? Promise.resolve(null) : Promise.reject('cancelled');
      }
      return Promise.reject(new Error('unexpected command ' + cmd));
    });
    const hook = {
      beginRecord() { throw new Error('capture must not start after save-dialog cancellation'); },
      getRecState() { return null; },
      getConfig() { return { animation: { fps: 25 } }; },
    };
    env.window.__glyphGridTest = hook;
    env.studio.init({
      config: {
        studio: { enabled: true }, animation: { fps: 25, duration: 0.08, loop: true },
        grid: { cols: 80, rows: 40 }, font: { family: 'monospace', size: 8 },
        ramp: 'gradient', brightnessGamma: 1, samplingStrategy: 'average',
        colorMode: 'monochrome', palette: 'cream-paper', selectionMode: 'brightness', glyphSet: null,
      },
      testHook: hook,
    });
    env.buttons.get('Export GIF').fire('click');
    await settle();
    assert.strictEqual(env.monitors.get('phase').phase, 'cancelled',
      'save-dialog cancellation must not leave the export monitor at choose destination');
    assert.strictEqual(env.buttons.get('Cancel export').disabled, true);
  }
}

function twitterFitCopyDoesNotPromiseSuccess() {
  const source = fs.readFileSync(require.resolve('../src/lib/glyph-studio.js'), 'utf8');
  assert(!/GUARANTEES output|GUARANTEE < 15 MB/.test(source));
  assert(source.includes('Targets 15 MB'));
  assert(source.includes('The 15 MB target was not met'));
}

(async function run() {
  await nativeSuccessSkipsZip();
  await nativeFailureBuildsFallbackOnce();
  await stagedUiFlowStreamsWithoutRetainingFrames();
  await cancelButtonStopsCorrelatedCaptureAndNativeJob();
  await cancelBeforeBeginCleansLateNativeCapture();
  await cancelledSaveDialogSettlesVisiblePhase();
  twitterFitCopyDoesNotPromiseSuccess();
  console.log('glyph-studio export tests: ok');
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
