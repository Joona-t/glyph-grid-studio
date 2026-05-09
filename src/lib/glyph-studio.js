/* glyph-studio.js — interactive parameter tuning for /glyph-grid pieces.

   Wires Tweakpane to CONFIG so every slider live-mutates the running
   render — no reload, no code edit. Adds drag-drop image upload, preset
   save/load (localStorage + JSON export/import + URL sharing), and
   a recording trigger that drives __glyphGridTest.beginRecord and
   downloads the resulting ZIP for export-gif.sh.

   Opt-in per piece: set `CONFIG.studio = { enabled: true, … }`. Pieces
   that don't set this are unaffected (the lib early-returns on init).

   Public API:
     window.GlyphStudio.init(opts)
       opts: {
         config:   the piece's CONFIG object (mutated in-place by sliders)
         imageRef: { name, set(img) } — handle to scene image so drop-image
                   can swap the source. Optional; if missing, drag-drop disabled.
         testHook: window.__glyphGridTest                  — required for record
         palettes: PALETTES                                — for the palette dropdown
         ramps:    RAMPS                                   — for the ramp dropdown
         sceneCacheKeys: ['__sourceProcessed', '__sourceDepth', …]
                   keys inside the SCENES[name] container that hold
                   cached p5.Graphics buffers. Cleared on image swap.
       }
*/

(function () {
  'use strict';

  /* ----------------------------------------------------------------
   * 1.  Tweakpane availability check + CDN-load fallback
   * ---------------------------------------------------------------- */
  function ensureTweakpane(cb) {
    /* Tweakpane 3.x ships UMD (works with <script>); 4.x is ESM-only.
       We use 3.1.10 to stay browser-script-friendly without a build
       step. API differences: v3 uses addInput, v4 uses addBinding;
       this lib uses addInput throughout. */
    if (window.Tweakpane && window.Tweakpane.Pane) { cb(window.Tweakpane.Pane); return; }
    var s = document.createElement('script');
    s.src = 'https://cdn.jsdelivr.net/npm/tweakpane@3.1.10/dist/tweakpane.min.js';
    s.onload = function () {
      if (window.Tweakpane && window.Tweakpane.Pane) cb(window.Tweakpane.Pane);
      else console.warn('glyph-studio: Tweakpane loaded but no Pane export found');
    };
    s.onerror = function () { console.warn('glyph-studio: Tweakpane CDN unreachable'); };
    document.head.appendChild(s);
  }

  /* ----------------------------------------------------------------
   * 2.  Preset save / load helpers
   * ---------------------------------------------------------------- */
  var PRESET_PREFIX = 'glyph-studio:';
  function presetKey(n) { return PRESET_PREFIX + n; }

  function listPresets() {
    var out = [];
    for (var i = 0; i < localStorage.length; i++) {
      var k = localStorage.key(i);
      if (k && k.indexOf(PRESET_PREFIX) === 0) out.push(k.slice(PRESET_PREFIX.length));
    }
    return out.sort();
  }
  function savePreset(name, snap) {
    if (!name) return false;
    try { localStorage.setItem(presetKey(name), JSON.stringify(snap)); return true; }
    catch (e) { console.warn('glyph-studio: localStorage full?', e); return false; }
  }
  function loadPreset(name) {
    var raw = localStorage.getItem(presetKey(name));
    if (!raw) return null;
    try { return JSON.parse(raw); } catch (e) { return null; }
  }

  function snapshotConfig(config) {
    var keys = ['canvas', 'grid', 'font', 'ramp', 'brightnessGamma',
      'samplingStrategy', 'colorMode', 'palette', 'glyphSet',
      'selectionMode', 'dither', 'prefilter', 'postprocess',
      'depth', 'paletteMorph', 'animation', 'seed', 'studio'];
    var out = {};
    for (var i = 0; i < keys.length; i++) {
      var k = keys[i];
      if (config[k] !== undefined) out[k] = JSON.parse(JSON.stringify(config[k]));
    }
    return out;
  }
  function applyPreset(config, preset) {
    function deepMerge(t, s) {
      for (var k in s) {
        if (s[k] !== null && typeof s[k] === 'object' && !Array.isArray(s[k])) {
          if (!t[k] || typeof t[k] !== 'object') t[k] = {};
          deepMerge(t[k], s[k]);
        } else { t[k] = s[k]; }
      }
    }
    deepMerge(config, preset);
  }
  function exportJSON(snap, filename) {
    var blob = new Blob([JSON.stringify(snap, null, 2)], { type: 'application/json' });
    var url = URL.createObjectURL(blob);
    var a = document.createElement('a');
    a.href = url; a.download = filename || 'glyph-preset.json';
    document.body.appendChild(a); a.click(); a.remove();
    URL.revokeObjectURL(url);
  }
  function importJSON(file, cb) {
    var r = new FileReader();
    r.onload = function () {
      try { cb(JSON.parse(r.result)); }
      catch (e) { console.warn('glyph-studio: invalid preset JSON', e); }
    };
    r.readAsText(file);
  }
  function shareURL(snap) {
    var enc = btoa(unescape(encodeURIComponent(JSON.stringify(snap))));
    return location.origin + location.pathname + '?p=' + enc;
  }
  function presetFromURL() {
    var m = location.search.match(/[?&]p=([^&]+)/);
    if (!m) return null;
    try { return JSON.parse(decodeURIComponent(escape(atob(m[1])))); }
    catch (e) { return null; }
  }

  /* ----------------------------------------------------------------
   * 3.  Drag-drop image upload zone
   * ---------------------------------------------------------------- */
  function setupImageDrop(imageRef, sceneCacheKeys, onSwap, config) {
    if (!imageRef) return;
    var overlay = document.createElement('div');
    overlay.style.cssText =
      'position:fixed;top:0;left:0;right:0;bottom:0;z-index:99;' +
      'pointer-events:none;background:transparent;' +
      'border:2px dashed transparent;transition:border-color 80ms;';
    document.body.appendChild(overlay);
    var dragCount = 0;
    function over(e) {
      e.preventDefault(); dragCount++;
      overlay.style.borderColor = 'rgba(255,180,80,0.85)';
      overlay.style.pointerEvents = 'auto';
    }
    function leave() {
      dragCount = Math.max(0, dragCount - 1);
      if (dragCount === 0) {
        overlay.style.borderColor = 'transparent';
        overlay.style.pointerEvents = 'none';
      }
    }
    function drop(e) {
      e.preventDefault();
      dragCount = 0;
      overlay.style.borderColor = 'transparent';
      overlay.style.pointerEvents = 'none';
      var file = e.dataTransfer && e.dataTransfer.files && e.dataTransfer.files[0];
      if (!file || !/^image\//.test(file.type)) return;
      var url = URL.createObjectURL(file);
      window.loadImage(url, function (img) {
        imageRef.set(img);
        if (sceneCacheKeys && window.SCENES) {
          for (var i = 0; i < sceneCacheKeys.length; i++) {
            for (var name in window.SCENES) {
              if (window.SCENES[name][sceneCacheKeys[i]]) delete window.SCENES[name][sceneCacheKeys[i]];
            }
          }
        }
        if (onSwap) onSwap(img);
        URL.revokeObjectURL(url);
      }, function () {
        console.warn('glyph-studio: failed to decode dropped image');
        URL.revokeObjectURL(url);
      });
    }
    window.addEventListener('dragenter', over);
    window.addEventListener('dragover', function (e) { e.preventDefault(); });
    window.addEventListener('dragleave', leave);
    window.addEventListener('drop', drop);

    /* Tauri 2: when `dragDropEnabled` is true on the window, the OS swallows
       the DOM drag events and we never see them. Listen to Tauri's native
       drop event as a fallback so the studio works in either config.
       The event delivers absolute file paths from Finder. */
    function clearSceneCaches() {
      if (sceneCacheKeys && window.SCENES) {
        for (var i = 0; i < sceneCacheKeys.length; i++) {
          for (var name in window.SCENES) {
            if (window.SCENES[name][sceneCacheKeys[i]]) {
              delete window.SCENES[name][sceneCacheKeys[i]];
            }
          }
        }
      }
    }
    function loadFromFilePath(absPath) {
      if (!(window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke)) {
        console.warn('glyph-studio: Tauri core unavailable, cannot read file');
        return;
      }
      window.__TAURI__.core.invoke('read_image_file', { path: absPath })
        .then(function (dataUrl) {
          window.loadImage(dataUrl, function (img) {
            imageRef.set(img);
            clearSceneCaches();
            if (onSwap) onSwap(img);
            try {
              if (typeof window.__glyphAddRecentSource === 'function') {
                window.__glyphAddRecentSource(absPath);
              }
              if (typeof window.__glyphMaybeSuggestInvert === 'function' && config) {
                window.__glyphMaybeSuggestInvert(img, config);
              }
            } catch (e) {}
          }, function (err) { console.warn('glyph-studio: image decode failed', err); });
        })
        .catch(function (e) { console.warn('glyph-studio: read_image_file failed:', e); });
    }
    if (window.__TAURI__ && window.__TAURI__.event && window.__TAURI__.event.listen) {
      var enterEvents = ['tauri://drag-enter', 'tauri://file-drop-hover', 'tauri://drag-over'];
      var leaveEvents = ['tauri://drag-leave', 'tauri://file-drop-cancelled', 'tauri://drag-cancelled'];
      var dropEvents  = ['tauri://drag-drop',  'tauri://file-drop'];
      enterEvents.forEach(function (name) {
        window.__TAURI__.event.listen(name, function () {
          overlay.style.borderColor = 'rgba(255,180,80,0.85)';
          overlay.style.pointerEvents = 'auto';
        });
      });
      leaveEvents.forEach(function (name) {
        window.__TAURI__.event.listen(name, function () {
          overlay.style.borderColor = 'transparent';
          overlay.style.pointerEvents = 'none';
        });
      });
      dropEvents.forEach(function (name) {
        window.__TAURI__.event.listen(name, function (evt) {
          overlay.style.borderColor = 'transparent';
          overlay.style.pointerEvents = 'none';
          /* v2 payload: { paths: [...], position: {x,y} }  v1: array of paths */
          var p = evt && evt.payload;
          var paths = [];
          if (p && Array.isArray(p)) paths = p;
          else if (p && Array.isArray(p.paths)) paths = p.paths;
          else if (typeof p === 'string') paths = [p];
          if (paths.length) loadFromFilePath(paths[0]);
          else console.warn('glyph-studio: drop event with no paths in payload', p);
        });
      });
    }
  }

  /* ----------------------------------------------------------------
   * 4.  Recording + snapshot
   * ---------------------------------------------------------------- */
  /* Native (Tauri) path: collect frame base64 strings during the recording
     loop, then hand them to the Rust gif muxer.  Browser path: keep the old
     ZIP behaviour as a fallback so the studio works outside Tauri.
     `delayMs` defaults to the per-frame duration implied by CONFIG.animation. */
  function recordGIF(testHook, total, onProgress, onDone, delayMs, capWidth, border) {
    if (!testHook) return;
    var inTauri = !!(window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke);

    testHook.beginRecord({
      total: total || 24,
      collectFrames: inTauri,
      onFinish: function (blob) {
        if (onDone) onDone(blob);
        if (inTauri) {
          var rs = testHook.getRecState();
          var frames = rs && rs.framesB64 ? rs.framesB64 : [];
          if (!frames.length) {
            console.warn('glyph-studio: no frames collected, falling back to ZIP');
            saveBlobBrowser(blob, 'glyph-frames_' + stamp() + '.zip');
            return;
          }
          var fps = (window.__glyphGridTest &&
                     window.__glyphGridTest.getConfig &&
                     window.__glyphGridTest.getConfig().animation &&
                     window.__glyphGridTest.getConfig().animation.fps) || 24;
          var dly = delayMs || Math.round(1000 / fps);
          var payloadFrames = frames.map(function (b64) { return { b64: b64 }; });
          window.__TAURI__.core.invoke('save_gif_real', {
            frames: payloadFrames,
            delayMs: dly,
            capWidth: (typeof capWidth === 'number' && capWidth > 0) ? capWidth : null,
            border: (border && border.enabled) ? border : null,
          })
            .then(function (p) {
              if (p) console.log('glyph-studio: saved GIF to', p);
            })
            .catch(function (e) {
              if (e !== 'cancelled') {
                console.warn('save_gif_real failed:', e, '— falling back to ZIP');
                saveBlobBrowser(blob, 'glyph-frames_' + stamp() + '.zip');
              }
            });
        } else {
          saveBlobBrowser(blob, 'glyph-frames_' + stamp() + '.zip');
        }
      },
    });
    if (onProgress) {
      var lastIdx = 0;
      var t = setInterval(function () {
        var rs = testHook.getRecState();
        if (!rs) { clearInterval(t); return; }
        if (rs.frameIdx !== lastIdx) { lastIdx = rs.frameIdx; onProgress(lastIdx, total || 24); }
        if (rs.done) clearInterval(t);
      }, 200);
    }
  }

  /* MP4/H.264 path — for Instagram (Reels / Stories / feed posts) which
     strips uploaded GIFs.  Mirrors recordGIF: collect frames during
     playback, hand them to the Rust openh264 + mp4 muxer.  Outside Tauri
     this falls back to a ZIP of frames, same as the GIF path. */
  function recordMP4(testHook, total, onProgress, onDone, fps, capWidth, border) {
    if (!testHook) return;
    var inTauri = !!(window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke);

    testHook.beginRecord({
      total: total || 24,
      collectFrames: inTauri,
      onFinish: function (blob) {
        if (onDone) onDone(blob);
        if (inTauri) {
          var rs = testHook.getRecState();
          var frames = rs && rs.framesB64 ? rs.framesB64 : [];
          if (!frames.length) {
            console.warn('glyph-studio: no frames collected, falling back to ZIP');
            saveBlobBrowser(blob, 'glyph-frames_' + stamp() + '.zip');
            return;
          }
          var resolvedFps = (typeof fps === 'number' && fps > 0)
            ? Math.round(fps)
            : ((window.__glyphGridTest &&
                window.__glyphGridTest.getConfig &&
                window.__glyphGridTest.getConfig().animation &&
                window.__glyphGridTest.getConfig().animation.fps) || 30);
          var payloadFrames = frames.map(function (b64) { return { b64: b64 }; });
          window.__TAURI__.core.invoke('save_mp4_real', {
            frames: payloadFrames,
            fps: resolvedFps,
            capWidth: (typeof capWidth === 'number' && capWidth > 0) ? capWidth : null,
            border: (border && border.enabled) ? border : null,
          })
            .then(function (p) {
              if (p) console.log('glyph-studio: saved MP4 to', p);
            })
            .catch(function (e) {
              if (e !== 'cancelled') {
                console.warn('save_mp4_real failed:', e, '— falling back to ZIP');
                saveBlobBrowser(blob, 'glyph-frames_' + stamp() + '.zip');
              }
            });
        } else {
          saveBlobBrowser(blob, 'glyph-frames_' + stamp() + '.zip');
        }
      },
    });
    if (onProgress) {
      var lastIdx = 0;
      var t = setInterval(function () {
        var rs = testHook.getRecState();
        if (!rs) { clearInterval(t); return; }
        if (rs.frameIdx !== lastIdx) { lastIdx = rs.frameIdx; onProgress(lastIdx, total || 24); }
        if (rs.done) clearInterval(t);
      }, 200);
    }
  }
  function stamp() { return new Date().toISOString().replace(/[:.]/g, '-'); }
  function saveBlobBrowser(blob, filename) {
    var url = URL.createObjectURL(blob);
    var a = document.createElement('a');
    a.href = url; a.download = filename;
    document.body.appendChild(a); a.click(); a.remove();
    URL.revokeObjectURL(url);
  }
  function snapshotPNG(canvas) {
    if (!canvas) return;
    var url = canvas.toDataURL('image/png');
    /* In the Tauri app, prefer the native save dialog. */
    if (window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke) {
      window.__TAURI__.core.invoke('save_png', { dataUrl: url })
        .then(function (p) { if (p) console.log('glyph-studio: saved PNG to', p); })
        .catch(function (e) { if (e !== 'cancelled') console.warn('save_png:', e); });
      return;
    }
    var a = document.createElement('a');
    a.href = url;
    var stamp = new Date().toISOString().replace(/[:.]/g, '-');
    a.download = 'glyph-frame_' + stamp + '.png';
    document.body.appendChild(a); a.click(); a.remove();
  }

  /* Native preset save/load (Tauri only) — falls back to browser flow when not in Tauri. */
  function savePresetNative(snap) {
    if (window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke) {
      window.__TAURI__.core.invoke('save_preset_json', { json: JSON.stringify(snap, null, 2) })
        .then(function (p) { if (p) console.log('glyph-studio: saved preset to', p); })
        .catch(function (e) { if (e !== 'cancelled') console.warn('save_preset:', e); });
      return true;
    }
    return false;
  }
  function loadPresetNative(applyCb) {
    if (window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke) {
      window.__TAURI__.core.invoke('load_preset_json')
        .then(function (txt) {
          try { applyCb(JSON.parse(txt)); }
          catch (e) { console.warn('glyph-studio: invalid preset JSON', e); }
        })
        .catch(function (e) { if (e !== 'cancelled') console.warn('load_preset:', e); });
      return true;
    }
    return false;
  }
  function pickImageNative(loadIntoP5) {
    if (window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke) {
      /* Use the path-aware variant so we can record the load in
         localStorage's recent-sources list.  Fall back to plain
         `pick_image` if the new command isn't registered (older builds). */
      window.__TAURI__.core.invoke('pick_image_with_path')
        .then(function (result) {
          if (result && result.dataUrl && window.loadImage) {
            window.loadImage(result.dataUrl, loadIntoP5);
            try { addRecentSource(result.path); } catch (e) {}
          }
        })
        .catch(function (e) {
          if (e === 'cancelled') return;
          /* Older build — try the legacy pick_image.  No path tracking. */
          window.__TAURI__.core.invoke('pick_image')
            .then(function (dataUrl) {
              if (window.loadImage) window.loadImage(dataUrl, loadIntoP5);
            })
            .catch(function (e2) { if (e2 !== 'cancelled') console.warn('pick_image:', e2); });
        });
      return true;
    }
    return false;
  }

  /* Recent-sources persistence: last 5 image paths the user opened.
     Stored in localStorage as JSON `[{path, name, ts}, ...]`.  Updated
     by both the file picker and the drag-drop hook (see loadFromFilePath
     in setupImageDrop).  Display name = basename(path). */
  var RECENT_KEY = 'glyphgrid:recentSources';
  function basename(p) {
    if (!p) return '';
    var s = String(p).split(/[/\\]/);
    return s[s.length - 1] || s[s.length - 2] || p;
  }
  function getRecentSources() {
    try {
      var raw = localStorage.getItem(RECENT_KEY);
      var arr = raw ? JSON.parse(raw) : [];
      return Array.isArray(arr) ? arr : [];
    } catch (e) { return []; }
  }
  function addRecentSource(path) {
    if (!path) return;
    var list = getRecentSources();
    list = list.filter(function (r) { return r.path !== path; });
    list.unshift({ path: path, name: basename(path), ts: Date.now() });
    list = list.slice(0, 5);
    try { localStorage.setItem(RECENT_KEY, JSON.stringify(list)); } catch (e) {}
  }
  /* Expose to setupImageDrop so the drag-drop path also tracks recents. */
  window.__glyphAddRecentSource = addRecentSource;

  /* Toast notification — small banner near the top of the window for
     non-blocking suggestions (e.g. "try invertSignal").  Click to
     dismiss; auto-fades after `ttlMs`.  Cream-on-charcoal palette to
     match the app aesthetic. */
  function showToast(msg, ttlMs) {
    var existing = document.getElementById('glyph-toast');
    if (existing) existing.parentNode.removeChild(existing);
    var t = document.createElement('div');
    t.id = 'glyph-toast';
    t.textContent = msg;
    t.style.cssText =
      'position:fixed; top:14px; left:50%; transform:translateX(-50%); ' +
      'background:rgba(20,18,16,0.93); color:#E8DDC8; ' +
      'padding:10px 18px; border-radius:6px; ' +
      'font:12px ui-monospace, "SF Mono", Menlo, monospace; ' +
      'z-index:9999; box-shadow:0 2px 10px rgba(0,0,0,0.35); ' +
      'cursor:pointer; opacity:0; transition:opacity 0.3s ease; ' +
      'max-width:520px; line-height:1.5;';
    t.title = 'click to dismiss';
    t.onclick = function () {
      if (t.parentNode) t.parentNode.removeChild(t);
    };
    document.body.appendChild(t);
    requestAnimationFrame(function () { t.style.opacity = '1'; });
    setTimeout(function () {
      t.style.opacity = '0';
      setTimeout(function () {
        if (t.parentNode) t.parentNode.removeChild(t);
      }, 300);
    }, ttlMs || 9000);
  }
  window.__glyphToast = showToast;

  /* Auto-suggest helper — when a freshly-loaded image is mostly bright
     (mean luminance > 60%) AND invertSignal is currently OFF, surface a
     toast suggesting the sketch-mode setting.  No auto-apply — the user
     might want the "filter" look intentionally.  Sampled every 16th pixel
     for speed (~1 ms on a 1024×574 frame). */
  function maybeSuggestInvert(img, configRef) {
    if (!img || !img.loadPixels) return;
    if (!configRef || configRef.invertSignal) return;
    try {
      img.loadPixels();
      var pixels = img.pixels;
      if (!pixels || pixels.length < 64) return;
      var sum = 0, n = 0;
      for (var i = 0; i < pixels.length; i += 64) {
        sum += 0.2126 * pixels[i] + 0.7152 * pixels[i + 1] + 0.0722 * pixels[i + 2];
        n++;
      }
      if (n === 0) return;
      var meanPct = Math.round(sum / n / 255 * 100);
      if (meanPct > 60) {
        showToast(
          '💡 Bright source detected (' + meanPct + '% mean luminance). ' +
          'Try toggling `invert signal` in Mapping for the sketch / ink-on-paper look.',
          10000
        );
      }
    } catch (e) {
      /* loadPixels may fail mid-load; harmless. */
    }
  }
  window.__glyphMaybeSuggestInvert = maybeSuggestInvert;

  /* ----------------------------------------------------------------
   * 5.  Build Tweakpane panel
   * ---------------------------------------------------------------- */
  function buildPane(Pane, opts) {
    var config = opts.config;
    var pane = new Pane({ title: opts.title || 'glyph-studio', container: opts.container });

    /* Tooltip helper — sets a native-browser `title` on the binding's
       row element so hover-text appears for confusing settings.  Tweakpane
       3.1 doesn't expose a `description` option; the row element is
       reachable via the BindingApi's `.element` accessor.  No-op if the
       binding API shape differs. */
    function tip(binding, text) {
      try {
        if (binding && text && binding.element) binding.element.setAttribute('title', text);
      } catch (e) {}
      return binding;
    }

    /* Phase 0 perf instrumentation — attach a single pane-level change
       listener that records every binding mutation for switch-latency
       tracking.  Tweakpane fires this for every addInput in any folder. */
    if (window.__markChange) {
      try {
        pane.on('change', function (ev) {
          var key = (ev && ev.target && ev.target.key) || 'unknown';
          window.__markChange(key);
        });
      } catch (e) { /* old Tweakpane build may not expose pane.on */ }
    }

    /* Help — opens USER-GUIDE.md in the user's default markdown / text app
       via tauri-plugin-shell.  Sits at the top of the panel for
       discoverability.  No-op outside Tauri (browser preview). */
    if (window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke) {
      pane.addButton({ title: '? User Guide' }).on('click', function () {
        window.__TAURI__.core.invoke('open_user_guide').catch(function (e) {
          console.warn('open_user_guide failed:', e);
        });
      });
    }

    var fImg = pane.addFolder({ title: 'Image' });
    fImg.addButton({ title: 'drag & drop an image anywhere' });
    /* Native picker — Tauri only. Falls back to a hidden <input type=file>
       when running in plain browser. */
    fImg.addButton({ title: 'Pick image…' }).on('click', function () {
      var swap = function (img) {
        if (opts.imageRef && opts.imageRef.set) opts.imageRef.set(img);
        if (opts.sceneCacheKeys && window.SCENES) {
          for (var i = 0; i < opts.sceneCacheKeys.length; i++) {
            for (var name in window.SCENES) {
              if (window.SCENES[name][opts.sceneCacheKeys[i]]) delete window.SCENES[name][opts.sceneCacheKeys[i]];
            }
          }
        }
        /* Refresh slider displays after imageRef.set's auto-tune of
           CONFIG.animation.duration (animated-source uploads). */
        if (opts.__refreshPane) opts.__refreshPane();
        try {
          if (typeof window.__glyphMaybeSuggestInvert === 'function') {
            window.__glyphMaybeSuggestInvert(img, config);
          }
        } catch (e) {}
      };
      if (pickImageNative(swap)) return;
      var inp = document.createElement('input');
      inp.type = 'file'; inp.accept = 'image/*';
      inp.onchange = function () {
        if (inp.files && inp.files[0]) {
          var u = URL.createObjectURL(inp.files[0]);
          window.loadImage(u, function (img) { swap(img); URL.revokeObjectURL(u); });
        }
      };
      inp.click();
    });
    /* Clear image — wipe sourceImg + scene caches and revert to the
       "Drop an image to start" placeholder.  draw() at index.html:1041
       early-returns when sourceImg is null and re-renders the placeholder.
       Disabled while a recording is in progress so we don't kill an
       in-flight export. */
    fImg.addButton({ title: 'Clear image' }).on('click', function () {
      var rs = opts.testHook && opts.testHook.getRecState && opts.testHook.getRecState();
      if (rs && !rs.done) return;
      if (opts.imageRef && opts.imageRef.set) opts.imageRef.set(null);
      if (opts.sceneCacheKeys && window.SCENES) {
        for (var i = 0; i < opts.sceneCacheKeys.length; i++) {
          for (var name in window.SCENES) {
            if (window.SCENES[name][opts.sceneCacheKeys[i]]) delete window.SCENES[name][opts.sceneCacheKeys[i]];
          }
        }
      }
    });

    /* Recent sources dropdown — last 5 image paths persisted in
       localStorage (key `glyphgrid:recentSources`).  Selecting a recent
       re-loads via the read_image_file Tauri command (Tauri only).
       Rebuilt every time the dropdown opens so freshly-loaded images
       appear without a panel reload.  Browser preview: shows nothing. */
    var inTauriForRecent = !!(window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke);
    if (inTauriForRecent) {
      var recentBox = { sel: '' };
      function rebuildRecentOptions() {
        var list = getRecentSources();
        var opt = { '(none)': '' };
        for (var i = 0; i < list.length; i++) opt[list[i].name] = list[i].path;
        return opt;
      }
      var _recentBinding = fImg.addInput(recentBox, 'sel', {
        label: 'Recent', options: rebuildRecentOptions(),
      });
      tip(_recentBinding, 'Last 5 images you opened. Pick one to re-load. Cleared by clearing browser localStorage.');
      _recentBinding.on('change', function (e) {
        if (!e.value) return;
        window.__TAURI__.core.invoke('read_image_file', { path: e.value })
          .then(function (dataUrl) {
            if (!window.loadImage) return;
            window.loadImage(dataUrl, function (img) {
              if (opts.imageRef && opts.imageRef.set) opts.imageRef.set(img);
              if (opts.sceneCacheKeys && window.SCENES) {
                for (var i = 0; i < opts.sceneCacheKeys.length; i++) {
                  for (var name in window.SCENES) {
                    if (window.SCENES[name][opts.sceneCacheKeys[i]]) delete window.SCENES[name][opts.sceneCacheKeys[i]];
                  }
                }
              }
              try { addRecentSource(e.value); } catch (_) {}
              try {
                if (typeof window.__glyphMaybeSuggestInvert === 'function') {
                  window.__glyphMaybeSuggestInvert(img, config);
                }
              } catch (_) {}
              if (opts.__refreshPane) opts.__refreshPane();
            });
          })
          .catch(function (err) {
            console.warn('Recent sources: load failed for', e.value, err);
          });
        recentBox.sel = '';
      });
    }

    var fGrid = pane.addFolder({ title: 'Grid' });
    if (!config.grid) config.grid = { cols: 240, rows: 120 };
    tip(fGrid.addInput(config.grid, 'cols', { min: 60, max: 400, step: 5 }),
        'Glyph columns. More = finer detail, smaller cells, slower render. cols × rows = total cell count.');
    tip(fGrid.addInput(config.grid, 'rows', { min: 40, max: 300, step: 5 }),
        'Glyph rows. Pair with cols. Default 240×120 ≈ 28,800 cells.');
    if (!config.font) config.font = { family: 'monospace', size: 8 };
    tip(fGrid.addInput(config.font, 'size', { min: 3, max: 14, step: 1 }),
        'Glyph pixel size. Tune until characters bleed into neighbours and the grid pattern dissolves.');

    var fMap = pane.addFolder({ title: 'Mapping' });
    if (opts.ramps) {
      var rampOpts = {};
      Object.keys(opts.ramps).forEach(function (k) { rampOpts[k] = k; });
      tip(fMap.addInput(config, 'ramp', { options: rampOpts }),
          'Character set ordered by visual density. `gradient` is default. Try `blockShaded` for solid texture.');
    }
    tip(fMap.addInput(config, 'brightnessGamma', { min: 0.2, max: 2.5, step: 0.05 }),
        'Curve applied to source brightness. <0.5 = high-contrast stipple. >1.0 = lifts mid-tones, more cream.');
    if (config.invertSignal === undefined) config.invertSignal = false;
    tip(fMap.addInput(config, 'invertSignal', { label: 'invert signal' }),
        'Flip bright↔dark. ON = dark source becomes drawn ink, bright becomes cream paper. Use for white-bg sources.');

    if (config.bgThreshold === undefined) config.bgThreshold = 0;
    tip(fMap.addInput(config, 'bgThreshold', { min: 0, max: 1, step: 0.05, label: 'bg threshold' }),
        'Source pixels brighter than this are forced to palette bg (cream). Carves negative space. 0 = off.');
    tip(fMap.addInput(config, 'samplingStrategy', {
      options: { average: 'average', nearest: 'nearest', 'edge-weighted': 'edge-weighted' },
    }), 'How each cell reads source pixels. `average` for photos, `nearest` for sprite art, `edge-weighted` preserves thin lines.');

    /* Dither folder — Stage 2B exposes the new STBN mode alongside the
       existing modes. STBN gives smoother substrate breathing than the
       hash-based `temporal` mode without flicker. */
    if (config.dither) {
      var fDth = pane.addFolder({ title: 'Dither', expanded: false });
      tip(fDth.addInput(config.dither, 'mode', {
        options: {
          none: 'none',
          bayer4: 'bayer4',
          bayer8: 'bayer8',
          blueNoise: 'blueNoise',
          temporal: 'temporal',
          stbn: 'stbn (smooth temporal blue noise)',
          floydSteinberg: 'floydSteinberg',
          atkinson: 'atkinson',
          jarvisJudiceNinke: 'jarvisJudiceNinke',
        },
      }), 'Quantization noise pattern. `temporal` (default) animates the grid each frame. Only applies in brightness selection mode.');
      if (typeof config.dither.levels === 'number') {
        tip(fDth.addInput(config.dither, 'levels', { min: 2, max: 32, step: 1 }),
            'Number of brightness bands. Lower = chunkier, higher = more glyph variety.');
      }
      if (typeof config.dither.asSourcePrefilter === 'boolean') {
        tip(fDth.addInput(config.dither, 'asSourcePrefilter'),
            'Run dither over the source image before cell signal computation. Only applies for `temporal` mode.');
      }
    }

    var fCol = pane.addFolder({ title: 'Color' });
    if (opts.palettes) {
      var palOpts = {};
      Object.keys(opts.palettes).forEach(function (k) { palOpts[k] = k; });
      tip(fCol.addInput(config, 'palette', { options: palOpts }),
          'Background colour + ink stops. `cream-paper` is the signature look. Pairs with `monochrome` colorMode.');
    }
    tip(fCol.addInput(config, 'colorMode', {
      options: { preserve: 'preserve', monochrome: 'monochrome', duotone: 'duotone', gradient: 'gradient' },
    }), 'How palette maps to cells. `monochrome` = single ink with alpha. `gradient` = OKLab interp across all stops.');

    var fSel = pane.addFolder({ title: 'Selection (advanced)', expanded: false });
    tip(fSel.addInput(config, 'selectionMode', {
      options: {
        brightness: 'brightness', shape: 'shape',
        'shape-edge-aware': 'shape-edge-aware', 'edge-directional': 'edge-directional',
      },
    }), 'How glyphs are picked per cell. `brightness` = fast ramp lookup. `shape-edge-aware` = best aesthetic, ~4× slower.');
    var glyphSetVal = { v: config.glyphSet || 'null' };
    var _gsBinding = fSel.addInput(glyphSetVal, 'v', {
      label: 'glyphSet',
      options: {
        none: 'null', ascii: 'ascii', asciiDense: 'asciiDense',
        blockElements: 'blockElements', braille: 'braille',
        sextant: 'sextant', octant: 'octant',
      },
    });
    tip(_gsBinding, 'Atlas used by shape modes. `octant` (Unicode 16) gives the densest texture; `ascii` is widely compatible.');
    _gsBinding.on('change', function (e) {
      config.glyphSet = e.value === 'null' ? null : e.value;
      /* Stage 2A trustRequested: the renderer reads config.glyphSet
         live + uses the bundled fonts via the always-loaded cssStack;
         no async font reload needed on switch. The previous version
         called clearCache + load() which awaited 3 waitForFace promises
         (3s + 1.5s + 1.5s timeouts) and made the UI freeze for 1–6s
         on every glyphSet switch. That was the wrong path — the fonts
         are physically the same WOFF2 files for every glyphSet; only
         the requestedSet metadata changes. Re-issue load() WITHOUT
         clearing the cache so subsequent calls hit the descriptor cache
         (or do a fast availability re-probe + return). The renderer's
         next frame picks up config.glyphSet automatically. */
      if (window.GlyphGrid && window.GlyphGrid.fonts && window.GlyphGrid.fonts.load) {
        window.GlyphGrid.fonts.load({
          sizePx: config.font.size,
          glyphSet: config.glyphSet || 'ascii',
          trustRequested: !!config.glyphSet,
        });
      }
    });

    if (config.studio && config.studio.breathing) {
      var fBr = pane.addFolder({ title: 'Breathing' });
      tip(fBr.addInput(config.studio.breathing, 'emaAlpha', { min: 0, max: 1, step: 0.01 }),
          'EMA smoothing factor. 0 = flicker, 1 = frozen. 0.35 = relaxed breathing.');
      tip(fBr.addInput(config.studio.breathing, 'gainSwing', { min: 0, max: 0.6, step: 0.01, label: 'gain swing' }),
          'Per-frame brightness pulse magnitude. 0 = no pulse, 0.5 = strong heartbeat.');
      tip(fBr.addInput(config.studio.breathing, 'jitter', { min: 0, max: 0.2, step: 0.005 }),
          'Per-cell random offset each frame. Higher = more "live" texture.');
      tip(fBr.addInput(config.studio.breathing, 'pulseHz', { min: 0.1, max: 3, step: 0.05 }),
          'Gain swing rate in Hz. 0.7 = once per 1.4s. Slow = meditative, fast = nervous.');
    }

    /* Dispersal — stardust effect.  Each inked cell drifts off in a seeded
       random direction (upward-biased) and fades out across a tunable
       startT → endT window.  Cream-paper bg is naturally exempt (cells
       with space-glyphs render nothing anyway).  Read at draw-time per
       index.html ITER-018. */
    if (!config.dispersal) {
      config.dispersal = {
        enabled: false, startT: 0.5, endT: 0.95,
        intensity: 0.4, upwardBias: 0.7, swayAmount: 0.5, rippleAmt: 0.2,
      };
    }
    var fDis = pane.addFolder({ title: 'Dispersal', expanded: false });
    tip(fDis.addInput(config.dispersal, 'enabled'),
        'Stardust drift effect. Inked cells fly off and fade between startT and endT (fractions of animation duration). End state: blank cream paper.');
    tip(fDis.addInput(config.dispersal, 'startT', { min: 0, max: 1, step: 0.01, label: 'start T' }),
        'When the drift begins, as a fraction of animation.duration. 0 = from frame 0, 0.5 = halfway through.');
    tip(fDis.addInput(config.dispersal, 'endT', { min: 0, max: 1, step: 0.01, label: 'end T' }),
        'When fully dispersed (alpha = 0). Set ≤ 1.0 so the last frames are blank cream.');
    tip(fDis.addInput(config.dispersal, 'intensity', { min: 0, max: 2, step: 0.05 }),
        'Max drift distance as a fraction of canvas height. 0.4 ≈ 40% of canvas. Higher = particles fly further off-screen.');
    tip(fDis.addInput(config.dispersal, 'upwardBias', { min: 0, max: 1, step: 0.05, label: 'upward bias' }),
        '0 = random direction, 1 = straight up. 0.7 reads as "lifting away".');
    tip(fDis.addInput(config.dispersal, 'swayAmount', { min: 0, max: 2, step: 0.05, label: 'sway' }),
        'Horizontal sine wobble during drift. Subtle organic feel; 0 disables.');
    tip(fDis.addInput(config.dispersal, 'rippleAmt', { min: 0, max: 0.5, step: 0.01, label: 'ripple' }),
        'Per-cell start-time jitter. 0 = all cells start dispersing at startT. 0.3 = 30% of cells start later, creating a ripple.');

    if (config.postprocess) {
      /* Stage 3A: ensure crtBeam config block exists so the slider appears
         even if the scene didn't pre-declare it. */
      if (!config.postprocess.crtBeam) {
        config.postprocess.crtBeam = {
          enabled: false, intensity: 0.45, beamWidth: 0.18,
          speed: 1.0, slotMask: true, slotStrength: 0.20,
        };
      }
      /* Kawaii overlay — soft pink hearts + sparkles. Default off so the
         studio doesn't surprise users; enable per-piece via slider. */
      if (!config.postprocess.kawaii) {
        config.postprocess.kawaii = {
          enabled: false, intensity: 0.85,
          heartCount: 12, sparkleCount: 28, twinkleCount: 60,
          speed: 1.0,
          hueR: 255, hueG: 105, hueB: 180,
        };
      }
      var fPP = pane.addFolder({ title: 'Postprocess', expanded: false });
      ['vignette', 'bloom', 'halation', 'scanlines', 'chromaticAberration', 'phosphorDecay', 'depthFog', 'crtBeam', 'kawaii'].forEach(function (key) {
        if (!config.postprocess[key]) return;
        var sub = fPP.addFolder({ title: key, expanded: false });
        var P = config.postprocess[key];
        if ('enabled' in P) sub.addInput(P, 'enabled');
        if ('strength' in P) sub.addInput(P, 'strength', { min: 0, max: 1.5, step: 0.05 });
        if ('intensity' in P) sub.addInput(P, 'intensity', { min: 0, max: 1.5, step: 0.05 });
        if ('radius' in P) sub.addInput(P, 'radius', { min: 0.5, max: 8, step: 0.5 });
        if ('amount' in P) sub.addInput(P, 'amount', { min: 0, max: 5, step: 0.1 });
        if ('decay' in P) sub.addInput(P, 'decay', { min: 0, max: 1, step: 0.01 });
        if ('period' in P) sub.addInput(P, 'period', { min: 1, max: 8, step: 1 });
        if ('beamWidth' in P) sub.addInput(P, 'beamWidth', { min: 0.05, max: 0.6, step: 0.01 });
        if ('speed' in P) sub.addInput(P, 'speed', { min: 0.1, max: 4, step: 0.05 });
        if ('slotMask' in P) sub.addInput(P, 'slotMask');
        if ('slotStrength' in P) sub.addInput(P, 'slotStrength', { min: 0, max: 0.6, step: 0.01 });
        // Kawaii-specific sliders
        if ('heartCount' in P) sub.addInput(P, 'heartCount', { min: 0, max: 60, step: 1 });
        if ('sparkleCount' in P) sub.addInput(P, 'sparkleCount', { min: 0, max: 120, step: 1 });
        if ('twinkleCount' in P) sub.addInput(P, 'twinkleCount', { min: 0, max: 200, step: 1 });
        if ('hueR' in P) sub.addInput(P, 'hueR', { min: 0, max: 255, step: 1, label: 'hue R' });
        if ('hueG' in P) sub.addInput(P, 'hueG', { min: 0, max: 255, step: 1, label: 'hue G' });
        if ('hueB' in P) sub.addInput(P, 'hueB', { min: 0, max: 255, step: 1, label: 'hue B' });
      });
    }

    var fAnim = pane.addFolder({ title: 'Animation', expanded: false });
    if (!config.animation) config.animation = { fps: 30, duration: 6, loop: true };
    /* Slider cap raised from 20 → 60 s so animated-source uploads with
       longer loops (10+ s anime GIFs) don't get visually clamped.
       Programmatic duration values beyond the slider max still work.
       Step lowered from 0.5 → 0.1 so animated-GIF auto-tune can preserve
       sub-half-second loops without rounding (e.g. kaneki.gif 2.70 s
       was getting snapped to 2.50 s by Tweakpane's step constraint
       during pane.refresh, costing 6 frames of source motion). */
    fAnim.addInput(config.animation, 'duration', { min: 0.5, max: 60, step: 0.1 });
    fAnim.addInput(config.animation, 'fps', { min: 12, max: 60, step: 6 });
    fAnim.addInput(config.animation, 'loop');

    /* Perf — Phase 0 instrumentation. Live rolling averages of per-stage
       frame time + last switch latency. Click "Report (console)" to
       dump full averages to dev console. Click "Clear" to reset the
       ring buffer. */
    if (window.__perfReport) {
      var fPerf = pane.addFolder({ title: 'Perf', expanded: false });
      var perfState = { total: 0, fps: 0, scene: 0, lum: 0, downsample: 0, ema: 0, dither: 0, grid: 0, select: 0, draw: 0, postproc: 0, lastSwitch: '—' };
      fPerf.addMonitor(perfState, 'total', { label: 'total ms' });
      fPerf.addMonitor(perfState, 'fps', { label: 'fps' });
      fPerf.addMonitor(perfState, 'scene', { label: 'scene ms' });
      fPerf.addMonitor(perfState, 'lum', { label: 'lum ms' });
      fPerf.addMonitor(perfState, 'downsample', { label: 'downsample ms' });
      fPerf.addMonitor(perfState, 'ema', { label: 'ema ms' });
      fPerf.addMonitor(perfState, 'dither', { label: 'dither ms' });
      fPerf.addMonitor(perfState, 'grid', { label: 'grid ms' });
      fPerf.addMonitor(perfState, 'select', { label: '  select ms' });
      fPerf.addMonitor(perfState, 'draw', { label: '  draw ms' });
      fPerf.addMonitor(perfState, 'postproc', { label: 'postproc ms' });
      fPerf.addMonitor(perfState, 'lastSwitch', { label: 'last switch' });
      fPerf.addButton({ title: 'Report (console)' }).on('click', function () { window.__perfReport(); });
      fPerf.addButton({ title: 'Clear' }).on('click', function () { window.__perfClear(); });

      // Update perf state every 500ms with rolling-30-frame averages
      setInterval(function () {
        var ring = window.__perfRing && window.__perfRing();
        if (!ring || !ring.length) return;
        var sum = { total: 0, scene: 0, _lum: 0, _downsample: 0, _ema: 0, _dither: 0, grid: 0, _select: 0, _draw: 0, postprocess: 0 };
        for (var i = 0; i < ring.length; i++) {
          sum.total += ring[i].total;
          sum.scene += ring[i].stages.scene || 0;
          sum._lum += ring[i].stages._lum || 0;
          sum._downsample += ring[i].stages._downsample || 0;
          sum._ema += ring[i].stages._ema || 0;
          sum._dither += ring[i].stages._dither || 0;
          sum.grid += ring[i].stages.grid || 0;
          sum._select += ring[i].stages._select || 0;
          sum._draw += ring[i].stages._draw || 0;
          sum.postprocess += ring[i].stages.postprocess || 0;
        }
        var n = ring.length;
        perfState.total = +(sum.total / n).toFixed(1);
        perfState.fps = perfState.total > 0 ? +(1000 / perfState.total).toFixed(1) : 0;
        perfState.scene = +(sum.scene / n).toFixed(1);
        perfState.lum = +(sum._lum / n).toFixed(1);
        perfState.downsample = +(sum._downsample / n).toFixed(1);
        perfState.ema = +(sum._ema / n).toFixed(1);
        perfState.dither = +(sum._dither / n).toFixed(1);
        perfState.grid = +(sum.grid / n).toFixed(1);
        perfState.select = +(sum._select / n).toFixed(1);
        perfState.draw = +(sum._draw / n).toFixed(1);
        perfState.postproc = +(sum.postprocess / n).toFixed(1);
      }, 500);
    }

    /* Presets */
    var fPre = pane.addFolder({ title: 'Presets' });
    var presetState = { name: '' };
    fPre.addInput(presetState, 'name', { label: 'Save as' });
    fPre.addButton({ title: 'Save current' }).on('click', function () {
      if (!presetState.name) return;
      savePreset(presetState.name, snapshotConfig(config));
      console.log('glyph-studio: saved preset', presetState.name);
    });
    var slot = { sel: '' };
    function rebuildLoad() {
      var pres = listPresets();
      var o = { '— select —': '' };
      pres.forEach(function (n) { o[n] = n; });
      return o;
    }
    fPre.addInput(slot, 'sel', { label: 'Load', options: rebuildLoad() })
      .on('change', function (e) {
        if (!e.value) return;
        var p = loadPreset(e.value);
        if (p) { applyPreset(config, p); pane.refresh(); }
      });
    fPre.addButton({ title: 'Export JSON' }).on('click', function () {
      var snap = snapshotConfig(config);
      if (savePresetNative(snap)) return;
      exportJSON(snap, (presetState.name || 'glyph-preset') + '.json');
    });
    fPre.addButton({ title: 'Import JSON' }).on('click', function () {
      if (loadPresetNative(function (p) { applyPreset(config, p); pane.refresh(); })) return;
      var inp = document.createElement('input');
      inp.type = 'file'; inp.accept = 'application/json,.json';
      inp.onchange = function () {
        if (inp.files && inp.files[0]) importJSON(inp.files[0], function (p) {
          applyPreset(config, p); pane.refresh();
        });
      };
      inp.click();
    });
    fPre.addButton({ title: 'Copy share URL' }).on('click', function () {
      var url = shareURL(snapshotConfig(config));
      if (navigator.clipboard) navigator.clipboard.writeText(url);
      else prompt('Share URL', url);
    });

    var fExp = pane.addFolder({ title: 'Export' });
    fExp.addButton({ title: 'Snapshot PNG' }).on('click', function () {
      snapshotPNG(document.querySelector('canvas'));
    });
    /* Compute the frame count + per-frame delay we'll actually pass to
       the GIF encoder.  The GIF format stores delays in centiseconds
       (10 ms units), so e.g. 30 fps wants 33.33 ms but the encoder
       rounds to 30 ms.  Effective fps = 1000 / delayMs, NOT nominal
       fps — for total output duration to match the user's chosen
       duration we must derive frame count from effective fps. */
    function exportPlan() {
      var fps = (config.animation && config.animation.fps) || 30;
      var dur = (config.animation && config.animation.duration) || 6;
      var delayMs = Math.max(20, Math.round(1000 / fps / 10) * 10);
      var effFps = 1000 / delayMs;
      var n = Math.max(2, Math.round(dur * effFps));
      return { frames: n, delayMs: delayMs, dur: n * delayMs / 1000, fps: fps, effFps: effFps };
    }

    /* Live readout: frames + EXACT output duration + estimated file size
       (GIF and MP4) before clicking Export.  Size estimate uses bytes-per-
       pixel heuristic seeded from kaneki measurements:
         GIF: 0.18 B/px (cream-paper monochrome) / 0.30 (multi-colour)
         MP4: 0.075 B/px (mono) / 0.10 (multi-colour)
       Heavy postproc (bloom + halation) adds ~30% entropy.
       Twitter limits: GIF 15 MB, MP4 ~512 MB.  The ✗ tag flags GIF > 15. */
    var expState = { frames: 0, length: '—', estGif: '—', estMp4: '—' };
    fExp.addMonitor(expState, 'frames', { label: 'frames', interval: 200 });
    fExp.addMonitor(expState, 'length', { label: 'length', interval: 200 });
    fExp.addMonitor(expState, 'estGif', { label: 'est. gif', interval: 200 });
    fExp.addMonitor(expState, 'estMp4', { label: 'est. mp4', interval: 200 });
    function _estimateBytes(p, capW, gif) {
      var canv = document.querySelector('canvas');
      var srcW = (canv && canv.width)  || 1024;
      var srcH = (canv && canv.height) || 504;
      var w = (capW > 0 && capW < srcW) ? capW : srcW;
      var h = (capW > 0 && capW < srcW) ? Math.round(capW * srcH / srcW) : srcH;
      var px = p.frames * w * h;
      var isMono = (config.colorMode === 'monochrome');
      var pp = config.postprocess || {};
      var heavyPP = !!((pp.bloom && pp.bloom.enabled) || (pp.halation && pp.halation.enabled));
      var bpp = gif ? (isMono ? 0.18 : 0.30) : (isMono ? 0.075 : 0.10);
      if (heavyPP) bpp *= 1.3;
      return px * bpp;
    }
    function _formatMB(bytes) {
      var mb = bytes / 1024 / 1024;
      return (mb < 1) ? (Math.round(bytes / 1024) + ' KB') : (mb.toFixed(1) + ' MB');
    }
    setInterval(function () {
      var p = exportPlan();
      var capW = (sizeOpts && sizeOpts.capWidth) || 0;
      expState.frames = p.frames;
      expState.length = p.dur.toFixed(2) + 's (' + p.delayMs + 'ms/frame)';
      var gifB = _estimateBytes(p, capW, true);
      var mp4B = _estimateBytes(p, capW, false);
      expState.estGif = _formatMB(gifB) + (gifB > 15 * 1024 * 1024 ? ' ✗ Twitter' : '');
      expState.estMp4 = _formatMB(mp4B);
    }, 200);

    /* Output size dropdown — caps width before encoding for smaller files.
       Twitter's GIF post limit is 15 MB on web/iOS (5 MB on Android); a
       1024×683 glyph render of an animated source can exceed 30 MB at
       full size.  720 px wide ≈ 50% of the pixel count → ≈ 50% smaller
       file with no perceptible difference at typical Twitter mobile
       display widths.  480 px is for very long loops or smaller targets.
       0 = no cap (full quality, original render dimensions). */
    var sizeOpts = { capWidth: 0 };
    tip(fExp.addInput(sizeOpts, 'capWidth', {
      label: 'output size',
      options: { 'full (no cap)': 0, '720px (Twitter)': 720, '480px (small)': 480 },
    }), 'Cap output width before encoding. Smaller = smaller file, no quality loss for mobile viewing.');

    /* Bordered export — palette-aware matte added to the encoded frame.
       Frames out at (w + 2*borderWidth) × (h + 2*borderWidth), rounded to
       even for yuv420p compatibility.  v0.1.1 ships `solid` only; deckle /
       passepartout / inner-line styles are queued for v0.1.2 (TODO.md). */
    var borderOpts = { enabled: false, width: 32, color: 'auto' };
    var fBorder = fExp.addFolder({ title: 'Border', expanded: false });
    tip(fBorder.addInput(borderOpts, 'enabled'),
        'Add a palette-aware matte band around the encoded frame (passepartout look).');
    tip(fBorder.addInput(borderOpts, 'width', { min: 0, max: 128, step: 4 }),
        'Border thickness in pixels. Same on all 4 sides. Rounded to even for video compatibility.');
    tip(fBorder.addInput(borderOpts, 'color', {
      options: { 'palette bg (auto)': 'auto', 'cream': '#E8DDC8', 'charcoal': '#0E0805', 'white': '#FFFFFF', 'black': '#000000' },
    }), 'Matte colour. `auto` picks the current palette\'s bg colour for a seamless print-on-paper feel.');

    var inTauri = !!(window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke);
    /* Export GIF — derives frame count from effective fps so the output
       duration matches the user's Animation › duration setting (within
       the GIF format's 10 ms quantization).  Encoder is gifski (shared
       palette across frames + error-diffusion dithering at quality 100,
       lossless for our limited-palette glyph art).  Optional cap-width
       resize is applied by gifski itself before quantisation.

       Loop-clean fix: override CONFIG.animation.fps to the effective
       fps (1000 / delayMs) for the duration of the recording.  Without
       this override, the studio's animation timeline advances at the
       slider fps (e.g. 30) but the output GIF plays at the cs-quantised
       rate (e.g. 33.33 if delay rounds to 30 ms).  The mismatch makes
       the last few captured frames overshoot the source loop, then the
       output GIF wraps and replays the same source frames it just
       showed — that's the stutter.  Forcing the studio to render at
       the same effective rate the GIF will play at means each captured
       frame represents exactly one delayMs of animation time, and the
       last captured frame lands precisely at duration - delayMs. */
    /* Resolve border colour: 'auto' uses the current palette bg.  Returns
       a fully-resolved BorderConfig or null when disabled. */
    function resolveBorder() {
      if (!borderOpts.enabled || !borderOpts.width) return null;
      var color = borderOpts.color;
      if (color === 'auto' && opts.palettes && config.palette) {
        var pal = opts.palettes[config.palette];
        if (pal && pal.bg) color = pal.bg;
      }
      if (!color || color === 'auto') color = '#E8DDC8';
      var ink = '#0E0805';
      if (opts.palettes && config.palette) {
        var p2 = opts.palettes[config.palette];
        if (p2 && p2.inks && p2.inks[0]) ink = p2.inks[0];
      }
      return {
        enabled: true,
        width: Math.round(borderOpts.width),
        style: 'solid',
        bgColor: color,
        inkColor: ink,
      };
    }

    /* Helper — encapsulates the fps-override + recordGIF / recordMP4 dance.
       Usage:
         exportRun('gif',  720)  — Twitter-fit GIF
         exportRun('gif',  null) — uses sizeOpts.capWidth (manual)
         exportRun('mp4',  720)  — mobile-fit MP4
       `capOverride === null` means "honour the panel dropdown".
       `capOverride > 0` means "force this exact capWidth, ignore dropdown". */
    function exportRun(format, capOverride) {
      var p = exportPlan();
      var capW = (capOverride !== null && capOverride !== undefined)
        ? capOverride
        : (sizeOpts.capWidth || 0);
      var savedFps = config.animation.fps;
      config.animation.fps = p.effFps;
      var border = resolveBorder();
      var onProgress = function (i, total) { console.log('  ', i, '/', total); };
      var onDone = function () {
        config.animation.fps = savedFps;
        try { pane.refresh(); } catch (e) {}
        console.log('glyph-studio: recording finished, fps restored to ' + savedFps);
      };
      if (format === 'mp4') {
        var encFps = Math.round(p.effFps);
        console.log('glyph-studio: recording MP4', p.frames, 'frames @', encFps, 'fps =', p.dur.toFixed(2), 's' + (capW > 0 ? ' (capped at ' + capW + 'px wide)' : '') + '; studio fps ' + savedFps + ' → ' + p.effFps.toFixed(2));
        recordMP4(opts.testHook, p.frames, onProgress, onDone, encFps, capW > 0 ? capW : null, border);
      } else {
        console.log('glyph-studio: recording', p.frames, 'frames at', p.delayMs, 'ms/frame =', p.dur.toFixed(2), 's' + (capW > 0 ? ' (capped at ' + capW + 'px wide)' : '') + '; studio fps ' + savedFps + ' → ' + p.effFps.toFixed(2) + ' for clean loop');
        recordGIF(opts.testHook, p.frames, onProgress, onDone, p.delayMs, capW > 0 ? capW : null, border);
      }
    }

    fExp.addButton({ title: inTauri ? 'Export GIF' : 'Export GIF (ZIP fallback)' }).on('click', function () {
      exportRun('gif', null);
    });
    /* Export GIF (Twitter-fit) — caps to 720px regardless of dropdown.
       Solves the recurring "my GIF is over 15 MB" pain (default density +
       90+ frames at full canvas reliably blows past Twitter's GIF ceiling).
       At 720 wide the kaneki/toji-class loops land 8–13 MB.  See
       BUGS_AND_ITERATIONS.md ITER-017 for context. */
    var btnGifTw = fExp.addButton({ title: 'Export GIF (Twitter-fit)' });
    tip(btnGifTw, 'Auto-caps output to 720px wide so it fits Twitter\'s 15 MB GIF ceiling. ~50% smaller than full, indistinguishable on phone screens. Honours the panel dropdown for a manual size override.');
    btnGifTw.on('click', function () { exportRun('gif', 720); });

    /* Export MP4 — for Instagram (Reels / Stories / feed posts strip
       uploaded GIFs).  Same fps-override discipline as Export GIF for
       a clean loop wrap.  Uses effFps as the encoded fps so the studio
       frame timing matches the MP4's playback timestamps exactly. */
    fExp.addButton({ title: inTauri ? 'Export MP4' : 'Export MP4 (ZIP fallback)' }).on('click', function () {
      exportRun('mp4', null);
    });
    /* Export MP4 (mobile-fit) — caps to 720px.  IG / Twitter mobile
       playback width is 360–440 px so 720 is more than enough; smaller
       file uploads faster, identical viewer experience. */
    var btnMp4Mob = fExp.addButton({ title: 'Export MP4 (mobile-fit)' });
    tip(btnMp4Mob, 'Auto-caps to 720px wide. Optimal for Instagram / Twitter mobile playback — smaller file, faster upload, no visible quality loss at typical phone viewing widths.');
    btnMp4Mob.on('click', function () { exportRun('mp4', 720); });

    var urlPre = presetFromURL();
    if (urlPre) { applyPreset(config, urlPre); pane.refresh(); }

    return pane;
  }

  /* ----------------------------------------------------------------
   * 6.  Public init
   * ---------------------------------------------------------------- */
  function init(opts) {
    if (!opts || !opts.config) return null;
    if (!opts.config.studio || !opts.config.studio.enabled) return null;
    var apiOut = { pane: null, refresh: function () {} };
    ensureTweakpane(function (Pane) {
      var pane = buildPane(Pane, opts);
      apiOut.pane = pane;
      apiOut.refresh = function () { pane.refresh(); };
      /* When a new image lands (drag-drop OR pick-image button OR Tauri
         file-drop), the imageRef.set callback in index.html may auto-tune
         CONFIG.animation.duration to match an animated GIF's loop length.
         Tweakpane v3 does NOT auto-refresh displayed values when bound
         object properties mutate externally — pane.refresh() is required.
         Pass a refresh callback to setupImageDrop and the Pick-image
         button so the slider catches up after every image swap. */
      var refreshOnSwap = function () { try { pane.refresh(); } catch (e) {} };
      if (opts.imageRef) {
        setupImageDrop(opts.imageRef, opts.sceneCacheKeys, refreshOnSwap, opts.config);
      }
      /* Expose to buildPane's pick-image handler via opts so the synchronous
         path (Pick image button) refreshes too. */
      opts.__refreshPane = refreshOnSwap;
    });
    return apiOut;
  }

  window.GlyphStudio = Object.freeze({
    init: init,
    snapshotConfig: snapshotConfig,
    applyPreset: applyPreset,
    listPresets: listPresets,
    savePreset: savePreset,
    loadPreset: loadPreset,
    exportJSON: exportJSON,
    importJSON: importJSON,
    shareURL: shareURL,
    presetFromURL: presetFromURL,
    snapshotPNG: snapshotPNG,
    recordGIF: recordGIF,
  });
})();
