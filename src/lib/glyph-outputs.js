/* glyph-outputs.js — Wave 2 output primitives.
 *
 * Registers:
 *   output.frame-zip    — capture per-frame PNGs into a JSZip → download
 *   output.svg          — emit one-frame SVG <text>-per-cell, trigger download
 *   output.gif-encoded  — in-browser animated GIF via gifenc (lazy CDN load)
 *   output.mp4          — canvas.captureStream via MediaRecorder
 *   output.png-frame    — save current canvas as a single PNG
 *
 * All outputs are TERMINAL — they return { ok: true, kind: '...' } or a
 * Promise of it. They do not mutate the signal.
 *
 * Lazy-load policy: heavy libs (gifenc) only fetched on first use.
 * JSZip + fflate are already loaded by render.html globally.
 */

(function () {
  'use strict';

  if (!window.GlyphGrid || !window.GlyphGrid.runtime) {
    console.warn('[glyph-outputs] runtime not loaded; skipping registrations.');
    return;
  }
  const rt = window.GlyphGrid.runtime;

  /* ====================================================================
     Helpers
     ==================================================================== */

  function downloadBlob(blob, filename) {
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(function () { URL.revokeObjectURL(url); }, 2000);
  }

  function timestamp() {
    return new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19);
  }

  function loadScriptOnce(url) {
    /* Lazy CDN <script> loader — idempotent. */
    if (loadScriptOnce._cache && loadScriptOnce._cache[url]) return loadScriptOnce._cache[url];
    loadScriptOnce._cache = loadScriptOnce._cache || {};
    const p = new Promise(function (resolve, reject) {
      const s = document.createElement('script');
      s.src = url;
      s.crossOrigin = 'anonymous';
      s.onload = function () { resolve(); };
      s.onerror = function () { reject(new Error('Failed to load ' + url)); };
      document.head.appendChild(s);
    });
    loadScriptOnce._cache[url] = p;
    return p;
  }

  /* ====================================================================
     output.png-frame — single-frame PNG download.
     ==================================================================== */

  rt.register('output', 'png-frame', async function (_in, ctx, stage) {
    const canvas = ctx.canvas
      || (window.GlyphGrid.v1 && window.GlyphGrid.v1.outputCanvas && window.GlyphGrid.v1.outputCanvas())
      || document.querySelector('canvas');
    if (!canvas) throw new window.GlyphGrid.GlyphGridError('STAGE_MISSING_HOST', 'png-frame', 'output', {
      message: 'No canvas available.',
    });
    const name = (stage.opts && stage.opts.filename) || ('glyph-grid-' + timestamp() + '.png');
    const blob = await new Promise(function (r) { canvas.toBlob(r, 'image/png'); });
    if (!blob) throw new window.GlyphGrid.GlyphGridError('OUTPUT_FAILED', 'png-frame', 'output', {
      message: 'canvas.toBlob returned null.',
    });
    if (stage.opts && stage.opts.autoDownload !== false) downloadBlob(blob, name);
    return { ok: true, kind: 'png-frame', filename: name, bytes: blob.size, blob: blob };
  }, { label: 'Save current canvas frame as PNG' });

  /* ====================================================================
     output.frame-zip — capture N frames of PNGs into a JSZip.

     State lives across frames. On first call we initialize; we capture
     current frame; after the Nth frame we assemble + download the zip.

     Requires JSZip (already on window from render.html).
     ==================================================================== */

  /* Per-pipeline state is keyed by stage identity so two pieces can run
     side-by-side without interference. Keyed by `stage` object ref. */
  const zipStates = new WeakMap();

  rt.register('output', 'frame-zip', async function (_in, ctx, stage) {
    if (typeof JSZip === 'undefined') {
      throw new window.GlyphGrid.GlyphGridError('STAGE_MISSING_HOST', 'frame-zip', 'output', {
        message: 'JSZip not loaded. Ensure the JSZip CDN script is present.',
      });
    }
    const opts = stage.opts || {};
    const total = opts.total
      || (ctx.config.animation && Math.round(ctx.config.animation.duration * ctx.config.animation.fps))
      || 60;
    let st = zipStates.get(stage);
    if (!st) {
      st = { zip: new JSZip(), captured: 0, total: total, done: false };
      zipStates.set(stage, st);
    }
    if (st.done) return { ok: true, kind: 'frame-zip', captured: st.captured, done: true };

    const canvas = ctx.canvas
      || (window.GlyphGrid.v1 && window.GlyphGrid.v1.outputCanvas && window.GlyphGrid.v1.outputCanvas())
      || document.querySelector('canvas');
    if (!canvas) throw new window.GlyphGrid.GlyphGridError('STAGE_MISSING_HOST', 'frame-zip', 'output', {
      message: 'No canvas available.',
    });

    /* toBlob is faster than toDataURL for PNG; encode in parallel where we
       can. We still need to resolve before the next frame's capture to keep
       frame ordering deterministic, so we await. */
    const blob = await new Promise(function (r) { canvas.toBlob(r, 'image/png'); });
    const idx = st.captured;
    const fname = 'frame_' + String(idx).padStart(4, '0') + '.png';
    st.zip.file(fname, blob);
    st.captured += 1;

    if (st.captured >= st.total && !st.done) {
      st.done = true;
      const blobZip = await st.zip.generateAsync({ type: 'blob' });
      const name = (opts.filename || 'glyph-grid-frames') + '_' + timestamp() + '.zip';
      if (opts.autoDownload !== false) downloadBlob(blobZip, name);
      return { ok: true, kind: 'frame-zip', captured: st.captured, done: true, filename: name, bytes: blobZip.size };
    }
    return { ok: true, kind: 'frame-zip', captured: st.captured, done: false };
  }, { label: 'Capture N canvas frames into a JSZip of PNGs' });

  /* ====================================================================
     output.svg — emit one-frame SVG as <text>-per-cell, trigger download.

     This primitive assumes it runs AFTER a color stage (signal has
     `.cellSignal`, `.glyphs`, `.ramp` or `.atlas`, `.rgb`). Draws no
     glyphs to a raster canvas; produces a pure SVG string instead.
     ==================================================================== */

  function svgEscape(s) {
    return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  }

  rt.register('output', 'svg', async function (colored, ctx, stage) {
    const opts = stage.opts || {};
    if (!colored || !colored.cellSignal || !colored.glyphs || !colored.rgb) {
      throw new window.GlyphGrid.GlyphGridError('TYPE_MISMATCH', 'svg', 'output', {
        message: 'output.svg expects {cellSignal, glyphs, rgb} from an upstream color stage.',
      });
    }
    const cs = colored.cellSignal;
    const ramp = colored.ramp || [];
    const w = opts.width || (ctx.config.canvas && ctx.config.canvas.w) || 640;
    const h = opts.height || (ctx.config.canvas && ctx.config.canvas.h) || 640;
    const cellW = w / cs.cols;
    const cellH = h / cs.rows;
    const fontPx = opts.fontSize || Math.max(4, Math.floor(cellH));
    const bg = opts.bg || '#000000';
    const fontFamily = opts.fontFamily || 'monospace';

    const out = [];
    out.push('<?xml version="1.0" encoding="UTF-8"?>');
    out.push('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ' + w + ' ' + h + '" width="' + w + '" height="' + h + '">');
    out.push('<rect width="100%" height="100%" fill="' + bg + '"/>');
    out.push('<g font-family="' + fontFamily + '" font-size="' + fontPx + '" text-anchor="middle" dominant-baseline="central">');

    const rgb = colored.rgb;
    const glyphs = colored.glyphs;
    for (let cy = 0; cy < cs.rows; cy++) {
      const y = (cy + 0.5) * cellH;
      for (let cx = 0; cx < cs.cols; cx++) {
        const i = cy * cs.cols + cx;
        const gidx = glyphs[i];
        const ch = ramp[gidx];
        if (!ch || ch === ' ') continue;
        const k = i * 3;
        const color = 'rgb(' + rgb[k] + ',' + rgb[k + 1] + ',' + rgb[k + 2] + ')';
        out.push('<text x="' + ((cx + 0.5) * cellW).toFixed(1) +
                 '" y="' + y.toFixed(1) +
                 '" fill="' + color + '">' + svgEscape(ch) + '</text>');
      }
    }
    out.push('</g></svg>');

    const svg = out.join('\n');
    const blob = new Blob([svg], { type: 'image/svg+xml' });
    const name = (opts.filename || 'glyph-grid') + '_' + timestamp() + '.svg';
    if (opts.autoDownload !== false) downloadBlob(blob, name);
    return { ok: true, kind: 'svg', filename: name, bytes: svg.length, svg: svg };
  }, { label: 'Emit one-frame SVG (one <text> per cell)' });

  /* ====================================================================
     output.gif-encoded — animated GIF via gifenc (lazy CDN load).

     State aggregates frames across draw calls; once `total` frames have
     been captured, it encodes and triggers download.
     ==================================================================== */

  const GIFENC_URL = 'https://unpkg.com/gifenc@1.0.3/dist/gifenc.esm.js';
  const gifStates = new WeakMap();

  async function ensureGifenc() {
    if (window.gifenc) return window.gifenc;
    /* gifenc ships as ESM. Use dynamic import() since plain <script> won't
       expose module exports. All modern browsers support this via Blob URL. */
    const mod = await import(GIFENC_URL);
    window.gifenc = mod;
    return mod;
  }

  rt.register('output', 'gif-encoded', async function (_in, ctx, stage) {
    const gifenc = await ensureGifenc();
    const opts = stage.opts || {};
    const total = opts.total
      || (ctx.config.animation && Math.round(ctx.config.animation.duration * ctx.config.animation.fps))
      || 60;
    const fps = opts.fps || (ctx.config.animation && ctx.config.animation.fps) || 30;
    let st = gifStates.get(stage);
    if (!st) {
      st = { frames: [], total: total, fps: fps, done: false };
      gifStates.set(stage, st);
    }
    if (st.done) return { ok: true, kind: 'gif-encoded', done: true };

    const canvas = ctx.canvas
      || (window.GlyphGrid.v1 && window.GlyphGrid.v1.outputCanvas && window.GlyphGrid.v1.outputCanvas())
      || document.querySelector('canvas');
    if (!canvas) throw new window.GlyphGrid.GlyphGridError('STAGE_MISSING_HOST', 'gif-encoded', 'output', {
      message: 'No canvas available.',
    });

    /* Capture RGBA bytes. */
    const c2d = canvas.getContext('2d', { willReadFrequently: true });
    const imgData = c2d.getImageData(0, 0, canvas.width, canvas.height);
    st.frames.push({ w: canvas.width, h: canvas.height, data: imgData.data });

    if (st.frames.length < st.total) {
      return { ok: true, kind: 'gif-encoded', captured: st.frames.length, done: false };
    }

    /* Encode. */
    st.done = true;
    const gif = gifenc.GIFEncoder();
    const delay = Math.round(1000 / st.fps);
    for (const f of st.frames) {
      const palette = gifenc.quantize(f.data, 256);
      const index = gifenc.applyPalette(f.data, palette);
      gif.writeFrame(index, f.w, f.h, { palette: palette, delay: delay });
    }
    gif.finish();
    const bytes = gif.bytes();
    const blob = new Blob([bytes], { type: 'image/gif' });
    const name = (opts.filename || 'glyph-grid') + '_' + timestamp() + '.gif';
    if (opts.autoDownload !== false) downloadBlob(blob, name);
    /* Free frame memory. */
    st.frames = null;
    return { ok: true, kind: 'gif-encoded', done: true, filename: name, bytes: blob.size };
  }, { label: 'Animated GIF via gifenc (lazy-loaded)' });

  /* ====================================================================
     output.mp4 — MediaRecorder capture of canvas.captureStream.

     Starts on first call; continues for N frames (or T seconds) then stops
     and downloads. Tab-hidden will throttle rAF and break frame timing —
     that's a MediaRecorder limitation, not ours.
     ==================================================================== */

  const mp4States = new WeakMap();

  function pickMp4MimeType() {
    const candidates = [
      'video/mp4;codecs=avc1.42E01E',
      'video/mp4',
      'video/webm;codecs=vp9',
      'video/webm;codecs=vp8',
      'video/webm',
    ];
    for (const mt of candidates) {
      if (typeof MediaRecorder !== 'undefined' && MediaRecorder.isTypeSupported && MediaRecorder.isTypeSupported(mt)) return mt;
    }
    return null;
  }

  rt.register('output', 'mp4', function (_in, ctx, stage) {
    if (typeof MediaRecorder === 'undefined') {
      throw new window.GlyphGrid.GlyphGridError('STAGE_MISSING_HOST', 'mp4', 'output', {
        message: 'MediaRecorder not available in this browser.',
      });
    }
    const opts = stage.opts || {};
    const total = opts.total
      || (ctx.config.animation && Math.round(ctx.config.animation.duration * ctx.config.animation.fps))
      || 60;
    const fps = opts.fps || (ctx.config.animation && ctx.config.animation.fps) || 30;
    let st = mp4States.get(stage);
    if (!st) {
      const canvas = ctx.canvas
        || (window.GlyphGrid.v1 && window.GlyphGrid.v1.outputCanvas && window.GlyphGrid.v1.outputCanvas())
        || document.querySelector('canvas');
      if (!canvas) throw new window.GlyphGrid.GlyphGridError('STAGE_MISSING_HOST', 'mp4', 'output', {
        message: 'No canvas available.',
      });
      const mime = pickMp4MimeType();
      if (!mime) throw new window.GlyphGrid.GlyphGridError('OUTPUT_FAILED', 'mp4', 'output', {
        message: 'No MediaRecorder codec supported (tried mp4/webm).',
      });
      const stream = canvas.captureStream(fps);
      const rec = new MediaRecorder(stream, { mimeType: mime, videoBitsPerSecond: opts.bitrate || 8_000_000 });
      const chunks = [];
      rec.ondataavailable = function (e) { if (e.data && e.data.size) chunks.push(e.data); };
      const ext = mime.indexOf('mp4') === 0 ? 'mp4' : 'webm';
      st = { rec: rec, chunks: chunks, mime: mime, ext: ext, count: 0, total: total, stopping: false, done: false };
      mp4States.set(stage, st);
      rec.start();
    }
    if (st.done) return { ok: true, kind: 'mp4', done: true };

    st.count += 1;
    if (st.count >= st.total && !st.stopping) {
      st.stopping = true;
      const stopPromise = new Promise(function (resolve) {
        st.rec.onstop = function () {
          const blob = new Blob(st.chunks, { type: st.mime });
          const name = (opts.filename || 'glyph-grid') + '_' + timestamp() + '.' + st.ext;
          if (opts.autoDownload !== false) downloadBlob(blob, name);
          st.done = true;
          resolve({ ok: true, kind: 'mp4', done: true, filename: name, bytes: blob.size, mime: st.mime });
        };
      });
      setTimeout(function () { try { st.rec.stop(); } catch (_) {} }, 100);
      return stopPromise;
    }
    return { ok: true, kind: 'mp4', count: st.count, done: false };
  }, { label: 'Video capture via MediaRecorder (MP4/WebM depending on browser)' });

})();
