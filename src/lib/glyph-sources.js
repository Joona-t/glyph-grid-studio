/* glyph-sources.js — Wave 4 source primitives.
 *
 * Unlocks entire new categories of glyph-grid pieces:
 *   source.feedback      — previous-frame-as-input (reaction-diffusion, slime)
 *   source.audio-fft     — live Web Audio FFT as Field
 *   source.camera        — webcam frames
 *   source.video         — <video> element frames
 *   source.data-stream   — fetch JSON / text / binary, reshape to field
 *   source.canvas-ref    — sample pixels from any on-page canvas
 *   source.simulation    — N-body / reaction-diffusion / CA step per frame
 *
 * All sources honor the Field contract and the determinism rules of the
 * runtime (hash-seeded only where noise is used; live data streams are
 * inherently non-deterministic — that's expected).
 */

(function () {
  'use strict';

  if (!window.GlyphGrid || !window.GlyphGrid.runtime) {
    console.warn('[glyph-sources] runtime not loaded.');
    return;
  }
  const rt = window.GlyphGrid.runtime;

  /* ====================================================================
     source.feedback — previous frame becomes this frame's input.

     Maintains a ping-pong of Float32Arrays per pipeline stage instance.
     The scene author combines the previous frame with a perturbation to
     drive reaction-diffusion, slime-mold, or persistent-state visuals.

     Opts:
       w, h:       field dimensions
       seed:       initial fill seed (0..1 scalar, default 0.5)
       decay:      per-frame multiplier (default 0.96)
       inject:     (field, ctx) => void — optional perturbation callback
                   (receives the decayed previous frame, may mutate it)
     ==================================================================== */

  const feedbackStates = new WeakMap();

  rt.register('source', 'feedback', function (_in, ctx, stage) {
    const opts = stage.opts || {};
    const w = opts.w || (ctx.config.canvas && ctx.config.canvas.w) || 256;
    const h = opts.h || (ctx.config.canvas && ctx.config.canvas.h) || 256;
    const decay = (opts.decay != null) ? opts.decay : 0.96;
    let st = feedbackStates.get(stage);
    if (!st || st.w !== w || st.h !== h) {
      const seed = (opts.seed != null) ? opts.seed : 0.5;
      const buf = new Float32Array(w * h);
      buf.fill(seed);
      st = { w: w, h: h, buf: buf, frame: 0 };
      feedbackStates.set(stage, st);
    }
    const b = st.buf;
    /* Decay previous frame. */
    for (let i = 0; i < b.length; i++) b[i] *= decay;
    /* Optional per-frame injection. */
    if (typeof opts.inject === 'function') {
      opts.inject({ w: w, h: h, buf: b, frame: st.frame }, ctx);
    } else if (!opts.inject && st.frame === 0) {
      /* Default inject: seed some hash-noise so the loop doesn't stay flat. */
      const hash = rt.hash32;
      for (let y = 0; y < h; y++) {
        for (let x = 0; x < w; x++) {
          b[y * w + x] = hash(ctx.seed, 0, x, y) / 4294967296;
        }
      }
    }
    st.frame += 1;
    /* Expose as a Field with a 'lum' channel pointing at the persistent buf. */
    return { w: w, h: h, channels: new Set(['lum']), buf: { lum: b } };
  }, {
    label: 'Persistent-state feedback source (ping-pong Float32)',
    produces: ['lum'],
  });

  /* ====================================================================
     source.audio-fft — live Web Audio FFT as a Field row.

     Opts:
       stream:   MediaStream | HTMLAudioElement | HTMLVideoElement
                 (default: navigator.mediaDevices.getUserMedia({audio:true}))
       fftSize:  power of 2 (default 1024)
       w, h:     output field size (defaults to canvas size)
       mode:     'spectrum-bars' (default) | 'spectrum-gradient' | 'waveform'
     ==================================================================== */

  const audioStates = new WeakMap();

  async function ensureAudioAnalyser(stage, opts) {
    let st = audioStates.get(stage);
    if (st) return st;
    if (typeof AudioContext === 'undefined' && typeof webkitAudioContext === 'undefined') {
      throw new window.GlyphGrid.GlyphGridError('STAGE_MISSING_HOST', 'audio-fft', 'source', {
        message: 'Web Audio API not available.',
      });
    }
    const AC = window.AudioContext || window.webkitAudioContext;
    const audioCtx = new AC();
    let source;
    if (opts.stream instanceof MediaStream) {
      source = audioCtx.createMediaStreamSource(opts.stream);
    } else if (opts.stream && typeof opts.stream.captureStream === 'function') {
      source = audioCtx.createMediaStreamSource(opts.stream.captureStream());
    } else if (opts.stream && opts.stream.play) {
      source = audioCtx.createMediaElementSource(opts.stream);
    } else {
      /* Default: microphone. */
      const mic = await navigator.mediaDevices.getUserMedia({ audio: true });
      source = audioCtx.createMediaStreamSource(mic);
    }
    const analyser = audioCtx.createAnalyser();
    analyser.fftSize = opts.fftSize || 1024;
    source.connect(analyser);
    const bins = new Uint8Array(analyser.frequencyBinCount);
    const wave = new Uint8Array(analyser.frequencyBinCount);
    st = { audioCtx: audioCtx, analyser: analyser, bins: bins, wave: wave };
    audioStates.set(stage, st);
    return st;
  }

  rt.register('source', 'audio-fft', async function (_in, ctx, stage) {
    const opts = stage.opts || {};
    const w = opts.w || (ctx.config.canvas && ctx.config.canvas.w) || 256;
    const h = opts.h || (ctx.config.canvas && ctx.config.canvas.h) || 256;
    const mode = opts.mode || 'spectrum-bars';
    const st = await ensureAudioAnalyser(stage, opts);
    if (mode === 'waveform') st.analyser.getByteTimeDomainData(st.wave);
    else st.analyser.getByteFrequencyData(st.bins);

    const field = ctx.pool.acquireField('gg:source-audio', w, h);
    const lum = field.buf.lum;
    const data = (mode === 'waveform') ? st.wave : st.bins;
    const N = data.length;
    if (mode === 'spectrum-bars') {
      /* Map x to frequency bin, y < bar-height → full; else 0. */
      for (let x = 0; x < w; x++) {
        const binIdx = Math.floor((x / w) * N);
        const barH = Math.floor((data[binIdx] / 255) * h);
        for (let y = 0; y < h; y++) {
          lum[y * w + x] = (y > h - barH) ? 1.0 : 0.0;
        }
      }
    } else if (mode === 'spectrum-gradient') {
      /* Vertical gradient from intensity at that column. */
      for (let x = 0; x < w; x++) {
        const binIdx = Math.floor((x / w) * N);
        const intensity = data[binIdx] / 255;
        for (let y = 0; y < h; y++) {
          lum[y * w + x] = intensity * (1 - y / h);
        }
      }
    } else { /* waveform */
      for (let x = 0; x < w; x++) {
        const binIdx = Math.floor((x / w) * N);
        const v = (data[binIdx] - 128) / 128;  /* -1..1 */
        const yMid = h / 2 + v * (h / 2);
        for (let y = 0; y < h; y++) {
          lum[y * w + x] = Math.max(0, 1 - Math.abs(y - yMid) * 0.15);
        }
      }
    }
    return field;
  }, {
    label: 'Live audio FFT or waveform as field',
    produces: ['lum'],
    scratch: { fields: [{ tag: 'gg:source-audio', channels: ['lum'] }] },
  });

  /* ====================================================================
     source.camera — webcam frames.

     Opts:
       w, h:            output field dimensions
       constraints:     getUserMedia video constraints (default { video: true })
       mirror:          horizontally flip (default true — matches user expectation)
     ==================================================================== */

  const cameraStates = new WeakMap();

  async function ensureCamera(stage, opts) {
    let st = cameraStates.get(stage);
    if (st) return st;
    const video = document.createElement('video');
    video.autoplay = true;
    video.muted = true;
    video.playsInline = true;
    const stream = await navigator.mediaDevices.getUserMedia(opts.constraints || { video: true });
    video.srcObject = stream;
    await new Promise(function (r) { video.onloadedmetadata = r; });
    await video.play();
    const off = document.createElement('canvas');
    const octx = off.getContext('2d', { willReadFrequently: true });
    st = { video: video, canvas: off, ctx: octx, stream: stream };
    cameraStates.set(stage, st);
    return st;
  }

  rt.register('source', 'camera', async function (_in, ctx, stage) {
    const opts = stage.opts || {};
    const w = opts.w || (ctx.config.canvas && ctx.config.canvas.w) || 320;
    const h = opts.h || (ctx.config.canvas && ctx.config.canvas.h) || 240;
    const mirror = (opts.mirror !== false);
    const st = await ensureCamera(stage, opts);
    st.canvas.width = w; st.canvas.height = h;
    st.ctx.save();
    if (mirror) { st.ctx.translate(w, 0); st.ctx.scale(-1, 1); }
    st.ctx.drawImage(st.video, 0, 0, w, h);
    st.ctx.restore();
    const img = st.ctx.getImageData(0, 0, w, h);
    const field = ctx.pool.acquireField('gg:source-camera', w, h);
    const lum = field.buf.lum, r = field.buf.r, g = field.buf.g, b = field.buf.b;
    const N = w * h;
    for (let i = 0; i < N; i++) {
      const p = i * 4;
      r[i] = img.data[p] / 255;
      g[i] = img.data[p + 1] / 255;
      b[i] = img.data[p + 2] / 255;
      lum[i] = 0.2126 * r[i] + 0.7152 * g[i] + 0.0722 * b[i];
    }
    return field;
  }, {
    label: 'Webcam live feed as field',
    produces: ['lum', 'r', 'g', 'b'],
    scratch: { fields: [{ tag: 'gg:source-camera', channels: ['lum', 'r', 'g', 'b'] }] },
  });

  /* ====================================================================
     source.video — any <video> element (src URL or provided element).
     Shares the same pixel-extraction pattern as source.camera.
     ==================================================================== */

  const videoStates = new WeakMap();

  async function ensureVideo(stage, opts) {
    let st = videoStates.get(stage);
    if (st) return st;
    let video;
    if (opts.element instanceof HTMLVideoElement) {
      video = opts.element;
    } else {
      video = document.createElement('video');
      video.autoplay = true;
      video.muted = true;
      video.loop = !!opts.loop;
      video.playsInline = true;
      video.crossOrigin = opts.crossOrigin || 'anonymous';
      if (opts.src) video.src = opts.src;
      await video.play().catch(function () { /* autoplay may be blocked */ });
    }
    const off = document.createElement('canvas');
    const octx = off.getContext('2d', { willReadFrequently: true });
    st = { video: video, canvas: off, ctx: octx };
    videoStates.set(stage, st);
    return st;
  }

  rt.register('source', 'video', async function (_in, ctx, stage) {
    const opts = stage.opts || {};
    const w = opts.w || (ctx.config.canvas && ctx.config.canvas.w) || 320;
    const h = opts.h || (ctx.config.canvas && ctx.config.canvas.h) || 240;
    const st = await ensureVideo(stage, opts);
    st.canvas.width = w; st.canvas.height = h;
    st.ctx.drawImage(st.video, 0, 0, w, h);
    const img = st.ctx.getImageData(0, 0, w, h);
    const field = ctx.pool.acquireField('gg:source-video', w, h);
    const lum = field.buf.lum, r = field.buf.r, g = field.buf.g, b = field.buf.b;
    const N = w * h;
    for (let i = 0; i < N; i++) {
      const p = i * 4;
      r[i] = img.data[p] / 255;
      g[i] = img.data[p + 1] / 255;
      b[i] = img.data[p + 2] / 255;
      lum[i] = 0.2126 * r[i] + 0.7152 * g[i] + 0.0722 * b[i];
    }
    return field;
  }, {
    label: 'Video element / URL frames as field',
    produces: ['lum', 'r', 'g', 'b'],
    scratch: { fields: [{ tag: 'gg:source-video', channels: ['lum', 'r', 'g', 'b'] }] },
  });

  /* ====================================================================
     source.canvas-ref — sample pixels from any on-page canvas.

     Opts:
       canvas: HTMLCanvasElement | CanvasRenderingContext2D | selector string
       w, h:   output field dimensions (canvas is resampled)
     ==================================================================== */

  rt.register('source', 'canvas-ref', function (_in, ctx, stage) {
    const opts = stage.opts || {};
    let src = opts.canvas;
    if (typeof src === 'string') src = document.querySelector(src);
    if (src && src.canvas) src = src.canvas; /* accept a 2D context */
    if (!src || !src.getContext) {
      throw new window.GlyphGrid.GlyphGridError('STAGE_BAD_OPTS', 'canvas-ref', 'source', {
        message: 'canvas-ref opts.canvas must be an HTMLCanvasElement, context, or selector.',
      });
    }
    const w = opts.w || src.width;
    const h = opts.h || src.height;
    /* Draw source canvas into our scratch offscreen at target dims, read pixels. */
    if (!ctx._canvasRefScratch) ctx._canvasRefScratch = document.createElement('canvas');
    const off = ctx._canvasRefScratch;
    off.width = w; off.height = h;
    const octx = off.getContext('2d', { willReadFrequently: true });
    octx.drawImage(src, 0, 0, w, h);
    const img = octx.getImageData(0, 0, w, h);
    const field = ctx.pool.acquireField('gg:source-canvas-ref', w, h);
    const lum = field.buf.lum, r = field.buf.r, g = field.buf.g, b = field.buf.b;
    const N = w * h;
    for (let i = 0; i < N; i++) {
      const p = i * 4;
      r[i] = img.data[p] / 255;
      g[i] = img.data[p + 1] / 255;
      b[i] = img.data[p + 2] / 255;
      lum[i] = 0.2126 * r[i] + 0.7152 * g[i] + 0.0722 * b[i];
    }
    return field;
  }, {
    label: 'Sample from another canvas element',
    produces: ['lum', 'r', 'g', 'b'],
    scratch: { fields: [{ tag: 'gg:source-canvas-ref', channels: ['lum', 'r', 'g', 'b'] }] },
  });

  /* ====================================================================
     source.reaction-diffusion — Gray-Scott model, one step per frame.

     Persistent state (A, B Float32Arrays). Produces rich organic patterns.
     Deterministic per seed.

     Opts:
       w, h:    grid dimensions (default 200×200)
       feed:    0.037 (typical 0.02..0.08)
       kill:    0.06  (typical 0.045..0.07)
       dA:      1.0    diffusion rate A
       dB:      0.5    diffusion rate B
       iterPerFrame: 5 (integration sub-steps per draw call)
     ==================================================================== */

  const rdStates = new WeakMap();

  rt.register('source', 'reaction-diffusion', function (_in, ctx, stage) {
    const opts = stage.opts || {};
    const w = opts.w || 200;
    const h = opts.h || 200;
    let st = rdStates.get(stage);
    if (!st || st.w !== w || st.h !== h) {
      const A = new Float32Array(w * h); A.fill(1.0);
      const B = new Float32Array(w * h);
      /* Seed with a small disk of B in the middle, and hash-noise scattered. */
      const hash = rt.hash32;
      const cx = w / 2, cy = h / 2, r0 = Math.min(w, h) * 0.05;
      for (let y = 0; y < h; y++) {
        for (let x = 0; x < w; x++) {
          const dx = x - cx, dy = y - cy;
          if (dx * dx + dy * dy < r0 * r0) B[y * w + x] = 1.0;
          if (hash(ctx.seed, 0, x, y) % 1000 < 5) B[y * w + x] = 1.0;
        }
      }
      st = { w: w, h: h, A: A, B: B, Anext: new Float32Array(w * h), Bnext: new Float32Array(w * h) };
      rdStates.set(stage, st);
    }
    const feed = opts.feed || 0.037;
    const kill = opts.kill || 0.06;
    const dA = opts.dA || 1.0, dB = opts.dB || 0.5;
    const iters = opts.iterPerFrame || 5;
    const A = st.A, B = st.B, An = st.Anext, Bn = st.Bnext;

    for (let it = 0; it < iters; it++) {
      for (let y = 1; y < h - 1; y++) {
        for (let x = 1; x < w - 1; x++) {
          const i = y * w + x;
          /* 9-point Laplacian (center -1, edges 0.2, corners 0.05). */
          const lapA =
            -A[i] +
            0.2 * (A[i - 1] + A[i + 1] + A[i - w] + A[i + w]) +
            0.05 * (A[i - 1 - w] + A[i + 1 - w] + A[i - 1 + w] + A[i + 1 + w]);
          const lapB =
            -B[i] +
            0.2 * (B[i - 1] + B[i + 1] + B[i - w] + B[i + w]) +
            0.05 * (B[i - 1 - w] + B[i + 1 - w] + B[i - 1 + w] + B[i + 1 + w]);
          const ab2 = A[i] * B[i] * B[i];
          An[i] = A[i] + (dA * lapA - ab2 + feed * (1 - A[i]));
          Bn[i] = B[i] + (dB * lapB + ab2 - (kill + feed) * B[i]);
          if (An[i] < 0) An[i] = 0; else if (An[i] > 1) An[i] = 1;
          if (Bn[i] < 0) Bn[i] = 0; else if (Bn[i] > 1) Bn[i] = 1;
        }
      }
      /* Swap. */
      const tA = st.A; st.A = st.Anext; st.Anext = tA;
      const tB = st.B; st.B = st.Bnext; st.Bnext = tB;
    }

    /* Render B channel as 'lum'. */
    const field = ctx.pool.acquireField('gg:source-rd', w, h);
    const lum = field.buf.lum;
    const Bout = st.B;
    for (let i = 0; i < w * h; i++) lum[i] = Bout[i];
    return field;
  }, {
    label: 'Gray-Scott reaction-diffusion simulation',
    produces: ['lum'],
    scratch: { fields: [{ tag: 'gg:source-rd', channels: ['lum'] }] },
  });

  /* ====================================================================
     source.cellular-automaton — rule-based CA (1D elementary or 2D Life).

     Opts:
       w, h:    grid dims
       rule:    'life' | 0..255 (1D elementary)
       init:    'random' | 'centered' | Array<0|1>
       stepsPerFrame: 1
     ==================================================================== */

  const caStates = new WeakMap();

  rt.register('source', 'cellular-automaton', function (_in, ctx, stage) {
    const opts = stage.opts || {};
    const w = opts.w || 128;
    const h = opts.h || 128;
    const rule = (opts.rule === undefined) ? 'life' : opts.rule;
    let st = caStates.get(stage);
    if (!st || st.w !== w || st.h !== h || st.rule !== rule) {
      const cur = new Uint8Array(w * h);
      const nxt = new Uint8Array(w * h);
      const hash = rt.hash32;
      if (opts.init === 'centered') {
        cur[Math.floor(h / 2) * w + Math.floor(w / 2)] = 1;
      } else {
        /* random */
        for (let i = 0; i < cur.length; i++) {
          cur[i] = (hash(ctx.seed, 0, i, 0) % 100) < 35 ? 1 : 0;
        }
      }
      st = { w: w, h: h, rule: rule, cur: cur, nxt: nxt };
      caStates.set(stage, st);
    }
    const steps = opts.stepsPerFrame || 1;
    for (let s = 0; s < steps; s++) {
      if (rule === 'life') {
        const a = st.cur, b = st.nxt;
        for (let y = 0; y < h; y++) {
          const ym = (y - 1 + h) % h, yp = (y + 1) % h;
          for (let x = 0; x < w; x++) {
            const xm = (x - 1 + w) % w, xp = (x + 1) % w;
            const n = a[ym * w + xm] + a[ym * w + x] + a[ym * w + xp]
                    + a[y  * w + xm]                 + a[y  * w + xp]
                    + a[yp * w + xm] + a[yp * w + x] + a[yp * w + xp];
            const alive = a[y * w + x];
            b[y * w + x] = (alive ? (n === 2 || n === 3) : (n === 3)) ? 1 : 0;
          }
        }
      } else {
        /* 1D elementary, scrolling downward. Row 0 uses the rule bits against
           three cells sampled from the previous row. Initial row from cur[0..w-1]. */
        const r = rule | 0;
        const a = st.cur, b = st.nxt;
        /* Shift rows down by 1 into b. */
        for (let y = h - 1; y > 0; y--) {
          for (let x = 0; x < w; x++) b[y * w + x] = a[(y - 1) * w + x];
        }
        /* New top row from previous top row + rule. */
        for (let x = 0; x < w; x++) {
          const l = a[(x - 1 + w) % w];
          const c = a[x];
          const rr = a[(x + 1) % w];
          const bits = (l << 2) | (c << 1) | rr;
          b[x] = (r >> bits) & 1;
        }
      }
      const tmp = st.cur; st.cur = st.nxt; st.nxt = tmp;
    }
    const field = ctx.pool.acquireField('gg:source-ca', w, h);
    const lum = field.buf.lum;
    for (let i = 0; i < w * h; i++) lum[i] = st.cur[i] ? 1.0 : 0.0;
    return field;
  }, {
    label: 'Cellular automaton (Life or 1D elementary)',
    produces: ['lum'],
    scratch: { fields: [{ tag: 'gg:source-ca', channels: ['lum'] }] },
  });

  /* ====================================================================
     source.flow-field — Perlin-like flow with integrated trails.

     Advects N particles through a curl-noise field, writes trail alpha
     into 'lum'. Deterministic from seed.

     Opts:
       w, h:          field dims (default 256)
       particles:     particle count (default 800)
       steps:         integration steps per particle (default 40)
       scale:         field spatial scale (default 1/60)
       tDrift:        flow-time multiplier (default 0.05)
     ==================================================================== */

  const flowStates = new WeakMap();

  rt.register('source', 'flow-field', function (_in, ctx, stage) {
    const opts = stage.opts || {};
    const w = opts.w || 256;
    const h = opts.h || 256;
    const count = opts.particles || 800;
    const steps = opts.steps || 40;
    const scale = opts.scale || (1 / 60);
    const tDrift = opts.tDrift || 0.05;

    let st = flowStates.get(stage);
    if (!st || st.w !== w || st.h !== h) {
      st = { w: w, h: h, lum: new Float32Array(w * h) };
      flowStates.set(stage, st);
    }
    const lum = st.lum;
    lum.fill(0);

    /* Simple value-noise with hash smoothing. */
    const hash = rt.hash32;
    const seed = ctx.seed;
    const t = ctx.t * tDrift;
    function ang(px, py) {
      const x0 = Math.floor(px), y0 = Math.floor(py);
      const n = hash(seed, 0, x0, y0) / 4294967296;
      const tx = px - x0, ty = py - y0;
      return (n + tx * 0.1 + ty * 0.07 + t) * Math.PI * 2;
    }

    for (let p = 0; p < count; p++) {
      const sHash = hash(seed, 1, p, 0);
      let x = (sHash % 10000) / 10000 * w;
      let y = ((sHash >>> 10) % 10000) / 10000 * h;
      for (let s = 0; s < steps; s++) {
        const a = ang(x * scale, y * scale);
        x += Math.cos(a); y += Math.sin(a);
        if (x < 0 || x >= w || y < 0 || y >= h) break;
        const i = ((y | 0) * w + (x | 0));
        lum[i] = Math.min(1, lum[i] + 0.12);
      }
    }

    return { w: w, h: h, channels: new Set(['lum']), buf: { lum: lum } };
  }, {
    label: 'Curl-noise flow field with integrated particle trails',
    produces: ['lum'],
  });

})();
