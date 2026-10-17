/* glyph-video-source.js — R4 (ITER-077): video + camera sources.

   A video becomes an ordinary source image for the studio: a p5.Graphics
   (made by index.html) tagged `__ggsVideo`, repainted per frame by the
   'video' FrameSource adapter built here.  Two backends feed it:

     rust     — H.264 MP4/MOV: `video_open` / `video_frame` (src-tauri
                video.rs, openh264) return frame N as raw RGBA.  Frame-exact
                and deterministic; the headless default.
     element  — anything else (HEVC, VP9/WebM, or decode:'webview'): a hidden
                <video muted playsinline> on the asset-protocol URL.  Live
                preview plays it and repaints on requestVideoFrameCallback;
                while recording, each frame is a seek (currentTime = (i+½)/fps,
                await 'seeked') so exports do not depend on rAF timing.
     camera   — getUserMedia, GUI only, live samples (not deterministic).

   Frames are addressed by index: i = floor(t · fps) mod frames (VFR clips
   are treated as CFR at their average rate).  Decoded frames live in a
   small LRU (CACHE frames) as ImageData; the adapter's frameAt() is
   synchronous and only paints what is cached — index.html gates recorded
   draws on ready() and awaits load() / prefetch() first.  Pure helpers
   (frameIndexAt, primeTimes, Lru) are node-testable. */
(function (root) {
  'use strict';

  var DECODERS = ['auto', 'native', 'webview'];
  /* JS-side decoded-frame cache.  Must hold the PRIME_FRAMES (11) priming
     frames + frame 0 at once (index.html prefetches them before priming).
     Also the multiplier SOURCE_LIMITS.maxDecodedPixels applies to a video. */
  var CACHE = 16;
  var VIDEO_EXT = /\.(mp4|m4v|mov|webm|mkv)$/i;
  var SEEK_TIMEOUT_MS = 10000;
  var PRESENT_GRACE_MS = 1000;
  var META_TIMEOUT_MS = 20000;

  function frameIndexAt(tMs, fps, frames) {
    if (!(frames > 0) || !(fps > 0)) return 0;
    var i = Math.floor((Number(tMs) || 0) * fps / 1000 + 1e-6);
    return ((i % frames) + frames) % frames;
  }

  /* The times (ms) a capture of `total` frames at `outFps` touches before
     its first draw: the PRIME priming passes over the loop tail (the same
     wrap as primeTemporalState) and frame 0. */
  function primeTimes(total, prime, outFps) {
    var n = Math.max(1, total | 0);
    var out = [];
    for (var i = 0; i < prime; i++) out.push(((((n - prime + i) % n) + n) % n) / outFps * 1000);
    out.push(0);
    return out;
  }

  function Lru(cap) { this.cap = cap; this.map = new Map(); }
  Lru.prototype.get = function (k) {
    if (!this.map.has(k)) return undefined;
    var v = this.map.get(k);
    this.map.delete(k);
    this.map.set(k, v);
    return v;
  };
  Lru.prototype.has = function (k) { return this.map.has(k); };
  Lru.prototype.set = function (k, v) {
    if (this.map.has(k)) this.map.delete(k);
    this.map.set(k, v);
    while (this.map.size > this.cap) this.map.delete(this.map.keys().next().value);
  };
  Lru.prototype.clear = function () { this.map.clear(); };

  function isVideoPath(p) { return VIDEO_EXT.test(String(p || '')); }

  function withTimeout(promise, ms, what) {
    var timer;
    return Promise.race([
      promise,
      new Promise(function (_, reject) {
        timer = setTimeout(function () { reject(new Error(what + ' timed out after ' + ms + ' ms')); }, ms);
      }),
    ]).then(function (v) { clearTimeout(timer); return v; }, function (e) { clearTimeout(timer); throw e; });
  }

  function once(el, ok, bad) {
    return new Promise(function (resolve, reject) {
      function done() { el.removeEventListener(ok, done); el.removeEventListener(bad, fail); resolve(); }
      function fail() {
        el.removeEventListener(ok, done); el.removeEventListener(bad, fail);
        var err = el.error;
        reject(new Error('video element error' + (err ? ' ' + err.code + (err.message ? ': ' + err.message : '') : '')));
      }
      el.addEventListener(ok, done);
      el.addEventListener(bad, fail);
    });
  }

  function makeVideoElement() {
    var v = document.createElement('video');
    v.muted = true;
    v.defaultMuted = true;
    v.playsInline = true;
    v.setAttribute('playsinline', '');
    v.setAttribute('muted', '');
    v.loop = true;
    v.preload = 'auto';
    v.crossOrigin = 'anonymous';
    v.style.cssText = 'position:fixed;left:-10000px;top:0;width:2px;height:2px;opacity:0;pointer-events:none;';
    document.body.appendChild(v);
    return v;
  }

  /* Count presented frames for the live preview (index bump = repaint). */
  function watchPresented(vs) {
    var v = vs.video;
    if (!v) return;
    if (typeof v.requestVideoFrameCallback === 'function') {
      var tick = function () {
        if (vs.disposed) return;
        vs.presented++;
        if (vs.onPresent) { try { vs.onPresent(); } catch (_) {} }
        v.requestVideoFrameCallback(tick);
      };
      v.requestVideoFrameCallback(tick);
    } else {
      v.addEventListener('timeupdate', function () { vs.presented++; });
    }
  }

  /* Open a clip.  opts: { path, url, probe, decode, invoke, log }.
     Resolves to the video state `vs` (backend + metadata + cache); the
     caller builds the p5.Graphics at vs.width × vs.height and calls
     paintFirst(). */
  function open(opts) {
    var decode = DECODERS.indexOf(opts.decode) >= 0 ? opts.decode : 'auto';
    var probe = opts.probe || {};
    var vs = {
      path: opts.path, kind: 'video', backend: null, camera: false, mirror: false,
      fps: 0, frames: 0, width: 0, height: 0, codec: probe.codec || '',
      cache: new Lru(CACHE), chain: Promise.resolve(), epoch: 0, painted: null,
      presented: 0, livePainted: -1, video: null, handle: 0, disposed: false,
      fallbackReason: '',
    };
    var tryRust = decode !== 'webview' && typeof opts.invoke === 'function';
    var start = tryRust
      ? Promise.resolve(opts.invoke('video_open', { path: opts.path })).then(function (info) {
          if (info && info.supported) {
            vs.backend = 'rust';
            vs.handle = info.handle;
            vs.fps = info.fps; vs.frames = info.frames;
            vs.width = info.width; vs.height = info.height; vs.codec = info.codec;
            return vs;
          }
          var why = (info && info.reason) || 'not decodable in Rust';
          if (decode === 'native') throw new Error('video: ' + why + ' (video.decode is "native"; use "auto" or "webview")');
          vs.fallbackReason = why;
          if (opts.log) opts.log('video: ' + why + '; decoding through the WebView <video> instead (seek per frame)');
          return null;
        })
      : Promise.resolve(null);
    return start.then(function (done) {
      if (done) {
        /* GUI: the Rust decoder serves exports; a hidden <video> still
           drives the smooth live preview (rVFC).  Headless: no element. */
        if (opts.preview && opts.url) {
          var pv = makeVideoElement();
          pv.src = opts.url;
          vs.video = pv;
          watchPresented(vs);
        }
        return vs;
      }
      if (!opts.url) throw new Error('video: no URL for the WebView decoder');
      var v = makeVideoElement();
      vs.backend = 'element';
      vs.video = v;
      var meta = once(v, 'loadeddata', 'error');
      v.src = opts.url;
      try { v.load(); } catch (_) {}
      return withTimeout(meta, META_TIMEOUT_MS, 'video metadata').then(function () {
        vs.width = v.videoWidth; vs.height = v.videoHeight;
        vs.fps = (probe.fps > 0) ? probe.fps : 30;
        vs.frames = (probe.probed && probe.frames > 0) ? probe.frames
          : Math.max(1, Math.round((isFinite(v.duration) ? v.duration : 0) * vs.fps));
        if (!vs.width || !vs.height) throw new Error('video: the WebView could not decode this clip (no picture size)');
        watchPresented(vs);
        return vs;
      }, function (e) {
        dispose(vs);
        throw e;
      });
    });
  }

  function openCamera(opts) {
    var md = typeof navigator !== 'undefined' && navigator.mediaDevices;
    if (!md || typeof md.getUserMedia !== 'function') {
      return Promise.reject(new Error('camera: getUserMedia is not available in this WebView'));
    }
    return md.getUserMedia({ video: { width: { ideal: 1280 }, height: { ideal: 720 } }, audio: false }).then(function (stream) {
      var v = makeVideoElement();
      v.loop = false;
      v.srcObject = stream;
      var vs = {
        path: null, kind: 'video', backend: 'element', camera: true, mirror: !!(opts && opts.mirror),
        fps: 30, frames: 1, width: 0, height: 0, codec: 'camera',
        cache: new Lru(1), chain: Promise.resolve(), epoch: 0, painted: null,
        presented: 0, livePainted: -1, video: v, stream: stream, handle: 0, disposed: false,
      };
      var ready = once(v, 'loadeddata', 'error');
      var p = v.play();
      if (p && p.catch) p.catch(function () {});
      return withTimeout(ready, META_TIMEOUT_MS, 'camera start').then(function () {
        vs.width = v.videoWidth; vs.height = v.videoHeight;
        watchPresented(vs);
        return vs;
      });
    });
  }

  function dispose(vs) {
    if (!vs || vs.disposed) return;
    vs.disposed = true;
    vs.cache.clear();
    if (vs.stream) { try { vs.stream.getTracks().forEach(function (t) { t.stop(); }); } catch (_) {} }
    if (vs.video) {
      try { vs.video.pause(); } catch (_) {}
      try { vs.video.removeAttribute('src'); vs.video.srcObject = null; vs.video.load(); } catch (_) {}
      try { vs.video.remove(); } catch (_) {}
      vs.video = null;
    }
  }

  /* Decode frame i into the cache (serialised: one decoder cursor / one
     <video> element per clip). */
  function load(vs, i, invoke) {
    if (vs.cache.has(i)) return Promise.resolve(vs.cache.get(i));
    if (!vs.pending) vs.pending = new Map();
    if (vs.pending.has(i)) return vs.pending.get(i);
    var p = vs.chain.then(function () {
      if (vs.disposed) throw new Error('video source was replaced');
      if (vs.cache.has(i)) return vs.cache.get(i);
      if (vs.backend === 'rust') {
        return Promise.resolve(invoke('video_frame', { handle: vs.handle, index: i })).then(function (buf) {
          var bytes = buf instanceof ArrayBuffer ? new Uint8ClampedArray(buf)
            : (buf && buf.buffer instanceof ArrayBuffer) ? new Uint8ClampedArray(buf.buffer, buf.byteOffset, buf.byteLength)
            : new Uint8ClampedArray(buf || []);
          if (bytes.length !== vs.width * vs.height * 4) {
            throw new Error('video_frame ' + i + ': got ' + bytes.length + ' bytes, want ' + (vs.width * vs.height * 4));
          }
          var img = new ImageData(bytes, vs.width, vs.height);
          vs.cache.set(i, img);
          return img;
        });
      }
      return seekGrab(vs, i);
    });
    vs.chain = p.catch(function () {});
    vs.pending.set(i, p);
    var clear = function () { vs.pending.delete(i); };
    p.then(clear, clear);
    return p;
  }

  function presentedAt(v, i, fps) {
    if (typeof v.requestVideoFrameCallback !== 'function') return Promise.resolve();
    return new Promise(function (resolve) {
      var tick = function (_now, meta) {
        /* round, not floor: WebM stores ms timestamps (frame 4 @ 30 fps
           = 0.133 s, i.e. 3.99 frames). */
        var at = meta && typeof meta.mediaTime === 'number' ? Math.round(meta.mediaTime * fps) : i;
        if (at === i) resolve();
        else v.requestVideoFrameCallback(tick);
      };
      v.requestVideoFrameCallback(tick);
    });
  }

  function seekGrab(vs, i) {
    var v = vs.video;
    if (!v) return Promise.reject(new Error('video element gone'));
    try { v.pause(); } catch (_) {}
    var target = Math.min((i + 0.5) / vs.fps, Math.max(0, (v.duration || 0) - 1e-3));
    /* 'seeked' alone is not enough on WebKit: it can fire while the element
       still shows the previous picture (measured: exports repeated stale
       frames).  Also wait until a requestVideoFrameCallback reports a
       presented frame whose mediaTime is the target frame. */
    var presented = presentedAt(v, i, vs.fps);
    var seeked = once(v, 'seeked', 'error');
    v.currentTime = target;
    /* A paused element presents one picture per seek; if its mediaTime
       never maps to i (VFR, odd timestamps) settle PRESENT_GRACE_MS after
       'seeked' rather than hang. */
    var settled = seeked.then(function () {
      return Promise.race([presented, new Promise(function (r) { setTimeout(r, PRESENT_GRACE_MS); })]);
    });
    return withTimeout(settled, SEEK_TIMEOUT_MS, 'video seek to frame ' + i).then(function () {
      if (!vs.scratch) {
        vs.scratch = document.createElement('canvas');
        vs.scratch.width = vs.width; vs.scratch.height = vs.height;
      }
      var ctx = vs.scratch.getContext('2d', { willReadFrequently: true });
      ctx.drawImage(v, 0, 0, vs.width, vs.height);
      var img = ctx.getImageData(0, 0, vs.width, vs.height);
      vs.cache.set(i, img);
      return img;
    });
  }

  /* The FrameSource adapter.  g: the p5.Graphics (sourceImg).  hooks:
     { invoke, recording() → bool }.  index is the content epoch: it bumps
     on every repaint, so draw()'s __sourceFrameIdx memo busting works. */
  function adapter(g, vs, revision, hooks) {
    function paintData(img) {
      g.drawingContext.putImageData(img, 0, 0);
    }
    function paintLive() {
      var v = vs.video;
      if (!v || v.readyState < 2) return false;
      var ctx = g.drawingContext;
      ctx.save();
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      if (vs.mirror) { ctx.translate(g.width, 0); ctx.scale(-1, 1); }
      ctx.drawImage(v, 0, 0, g.width, g.height);
      ctx.restore();
      return true;
    }
    return {
      kind: 'video', img: g, revision: revision, video: vs,
      durationMs: vs.camera ? 0 : vs.frames / vs.fps * 1000,
      index: vs.epoch > 0 ? vs.epoch : -1,
      frameIdx: function (tMs) { return frameIndexAt(tMs, vs.fps, vs.frames); },
      ready: function (tMs) { return vs.camera || vs.cache.has(this.frameIdx(tMs)); },
      load: function (tMs) { return vs.camera ? Promise.resolve() : load(vs, this.frameIdx(tMs), hooks.invoke); },
      prefetch: function (times) {
        var self = this;
        return times.reduce(function (p, t) { return p.then(function () { return self.load(t); }); }, Promise.resolve());
      },
      frameAt: function (tMs) {
        var recording = hooks.recording();
        var v = vs.video;
        if (v && !vs.disposed) {
          /* Live preview plays the element; a recording owns the frames
             (Rust decode, or seeks on this same element). */
          if (recording && !vs.camera) {
            if (!v.paused) { try { v.pause(); } catch (_) {} }
          } else {
            if (v.paused) {
              var pr = v.play();
              if (pr && pr.catch) pr.catch(function () {});
            }
            if (vs.presented !== vs.livePainted && paintLive()) {
              vs.livePainted = vs.presented;
              vs.painted = 'live';
              this.index = ++vs.epoch;
            }
            /* Not playable yet → show decoded frames meanwhile. */
            if (vs.camera || vs.painted === 'live') return g;
          }
        }
        var i = this.frameIdx(tMs);
        var f = vs.cache.get(i);
        if (f) {
          if (vs.painted !== i) { paintData(f); vs.painted = i; this.index = ++vs.epoch; }
        } else if (!recording && !vs.disposed) {
          /* Live (headless pre-capture) — fetch it for a later draw. */
          load(vs, i, hooks.invoke).catch(function () {});
        }
        return g;
      },
    };
  }

  var api = {
    DECODERS: DECODERS, CACHE: CACHE, VIDEO_EXT: VIDEO_EXT,
    frameIndexAt: frameIndexAt, primeTimes: primeTimes, Lru: Lru, isVideoPath: isVideoPath,
    open: open, openCamera: openCamera, dispose: dispose, load: load, adapter: adapter,
  };
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  if (root) {
    root.GlyphGrid = root.GlyphGrid || {};
    root.GlyphGrid.video = api;
  }
})(typeof window !== 'undefined' ? window : null);
