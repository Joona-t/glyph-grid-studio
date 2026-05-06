/* glyph-record.js — streaming recording.

   Old path (legacy under v1): sync canvas.toDataURL + JSZip in-memory.
   Memory grows ~1–2 MB/frame at 1080² → OOM past ~8 s recordings.

   New path (v2 default):
     1. File System Access API (Chromium/Edge) — pick a directory; frames
        are written PNG-by-PNG directly to disk. No memory growth.
     2. Worker-based ZIP (fflate) — if no File System Access, stream frames
        into an fflate AsyncZipDeflate in a Web Worker; browser flushes
        chunks incrementally; main thread stays responsive.
     3. Legacy fallback — same as old path, documented size cap.

   `recorder.begin({ total, fps })` returns a promise that resolves to a
   recorder instance with:
     capture(canvas)  — push the current frame
     finish()         — close and return the handle (directory or blob)

   Deterministic output requires the caller to drive frame timing. This
   module does not own the animation loop.

   We do NOT require the Worker file to exist; if fflate + Worker is not
   available the fallback chain is:
     FileSystem writable → fflate-in-worker → fflate-main → JSZip-legacy
   The legacy path always works; the upper tiers are optimizations. */

(function () {
  'use strict';

  /* ---------- capability probes ---------- */

  function hasFileSystemWritable() {
    return typeof window !== 'undefined'
           && typeof window.showDirectoryPicker === 'function';
  }

  function hasWorker() {
    return typeof Worker !== 'undefined';
  }

  function hasFflate() {
    return typeof window !== 'undefined'
           && typeof window.fflate !== 'undefined';
  }

  function hasJSZip() {
    return typeof window !== 'undefined'
           && typeof window.JSZip !== 'undefined';
  }

  /* ---------- canvas → Blob (PNG) ---------- */

  function canvasToPngBlob(canvas) {
    return new Promise(function (resolve, reject) {
      if (typeof canvas.convertToBlob === 'function') {
        canvas.convertToBlob({ type: 'image/png' }).then(resolve, reject);
      } else if (typeof canvas.toBlob === 'function') {
        canvas.toBlob(function (b) {
          if (b) resolve(b); else reject(new Error('toBlob returned null'));
        }, 'image/png');
      } else {
        try {
          const url = canvas.toDataURL('image/png');
          fetch(url).then(function (r) { return r.blob(); }).then(resolve, reject);
        } catch (e) { reject(e); }
      }
    });
  }

  function blobToUint8(blob) {
    if (blob.arrayBuffer) return blob.arrayBuffer().then(function (b) { return new Uint8Array(b); });
    return new Promise(function (resolve, reject) {
      const r = new FileReader();
      r.onload = function () { resolve(new Uint8Array(r.result)); };
      r.onerror = function () { reject(r.error); };
      r.readAsArrayBuffer(blob);
    });
  }

  /* ---------- mode 1: File System Access writable directory ---------- */

  function beginFileSystemRecorder(opts) {
    return window.showDirectoryPicker({
      id: 'glyph-grid-record',
      mode: 'readwrite',
      startIn: 'downloads',
    }).then(function (dirHandle) {
      let frameIdx = 0;
      return {
        mode: 'filesystem',
        capture: function (canvas) {
          return canvasToPngBlob(canvas).then(function (blob) {
            const name = 'frame_' + String(frameIdx + 1).padStart(5, '0') + '.png';
            frameIdx++;
            return dirHandle.getFileHandle(name, { create: true })
              .then(function (fh) { return fh.createWritable(); })
              .then(function (w) { return w.write(blob).then(function () { return w.close(); }); });
          });
        },
        finish: function () {
          return Promise.resolve({ mode: 'filesystem', directory: dirHandle, frames: frameIdx });
        },
        framesCaptured: function () { return frameIdx; },
      };
    });
  }

  /* ---------- mode 2: fflate-in-main (streaming ZIP) ---------- */

  function beginFflateRecorder(opts) {
    const zip = new window.fflate.Zip();
    const chunks = [];
    zip.ondata = function (err, data, final) {
      if (err) throw err;
      chunks.push(data);
    };
    let frameIdx = 0;
    return Promise.resolve({
      mode: 'fflate',
      capture: function (canvas) {
        return canvasToPngBlob(canvas).then(blobToUint8).then(function (u8) {
          const name = 'frame_' + String(frameIdx + 1).padStart(5, '0') + '.png';
          frameIdx++;
          const file = new window.fflate.ZipPassThrough(name);
          zip.add(file);
          file.push(u8, true);
        });
      },
      finish: function () {
        zip.end();
        const blob = new Blob(chunks, { type: 'application/zip' });
        return { mode: 'fflate', blob: blob, frames: frameIdx };
      },
      framesCaptured: function () { return frameIdx; },
    });
  }

  /* ---------- mode 3: JSZip legacy fallback ---------- */

  function beginJSZipRecorder(opts) {
    if (!hasJSZip()) {
      return Promise.reject(new Error('No recording backend available (JSZip missing)'));
    }
    const zip = new window.JSZip();
    let frameIdx = 0;
    return Promise.resolve({
      mode: 'jszip',
      capture: function (canvas) {
        return canvasToPngBlob(canvas).then(blobToUint8).then(function (u8) {
          const name = 'frame_' + String(frameIdx + 1).padStart(5, '0') + '.png';
          frameIdx++;
          zip.file(name, u8);
        });
      },
      finish: function () {
        return zip.generateAsync({ type: 'blob' }).then(function (blob) {
          return { mode: 'jszip', blob: blob, frames: frameIdx };
        });
      },
      framesCaptured: function () { return frameIdx; },
    });
  }

  /* ---------- dispatcher ---------- */

  /* Prefer the strongest backend available; caller may force via opts.mode. */
  function begin(opts) {
    opts = opts || {};
    const prefer = opts.mode || 'auto';
    if (prefer === 'filesystem' || (prefer === 'auto' && hasFileSystemWritable())) {
      return beginFileSystemRecorder(opts).catch(function (e) {
        /* user cancelled picker or API threw — fall through */
        if (console && console.warn) console.warn('File System Access unavailable: ' + (e && e.message));
        return beginNonFS(opts);
      });
    }
    return beginNonFS(opts);
  }

  function beginNonFS(opts) {
    if (hasFflate()) return beginFflateRecorder(opts);
    return beginJSZipRecorder(opts);
  }

  /* Post-finish helper: save a Blob to downloads with a filename. */
  function downloadBlob(blob, filename) {
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    a.remove();
    URL.revokeObjectURL(url);
  }

  const api = Object.freeze({
    hasFileSystemWritable: hasFileSystemWritable,
    hasWorker: hasWorker,
    hasFflate: hasFflate,
    hasJSZip: hasJSZip,
    begin: begin,
    downloadBlob: downloadBlob,
  });

  const root = (typeof window !== 'undefined') ? window
             : (typeof globalThis !== 'undefined') ? globalThis
             : this;
  root.GlyphGrid = root.GlyphGrid || {};
  root.GlyphGrid.record = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})();
