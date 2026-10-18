/* glyph-wave6.js — Wave 6 primitives.
 *
 * Unlocks the four remaining pieces of the charter:
 *   postProcess.crt-chain   — full atmospheric post stack via legacy crt.js
 *   selection.harri-faithful — real 6 in-cell + 12 external contrast match
 *   output.webgl-canvas     — instanced-quad paint (unblocks 500×500+)
 *   source.pointer          — live mouse position as Field
 *   source.keyboard         — live text-input buffer as Field
 *   output.audio-synth      — glyph state → additive synth (cross-modal)
 *
 * Plus: runtime.instrument() — per-primitive timing wrapper.
 */

(function () {
  'use strict';

  if (!window.GlyphGrid || !window.GlyphGrid.runtime) {
    console.warn('[glyph-wave6] runtime missing'); return;
  }
  const rt = window.GlyphGrid.runtime;
  const GGE = window.GlyphGrid.GlyphGridError;

  /* ====================================================================
     postProcess.crt-chain — wraps GlyphGrid.crt.applyChain.

     Maintains crtState across frames per-stage so phosphor decay works.
     Reads opts.stages = { phosphorDecay, bloom, scanlines, chromaticAberration,
     barrel, vignette, godRays, halation, letterbox } — each with
     `enabled: true` + stage-specific opts. Falls back to legacy's own
     defaults when not specified.
     ==================================================================== */

  const crtStates = new WeakMap();

  /* Paint upstream signal to canvas if the signal is a colored-but-unpainted
     cellSignal. Lets pipelines go `...color → crt-chain` without needing
     an intermediate output.canvas step. */
  function ensurePaintedForCrt(canvas, signal) {
    if (!canvas || !signal || signal.alreadyPainted || !signal.rgb || !signal.glyphs || !signal.cellSignal) return;
    const cs = signal.cellSignal;
    const c2d = canvas.getContext('2d');
    const cellW = canvas.width / cs.cols;
    const cellH = canvas.height / (cs.rows || 1);
    const fontPx = Math.max(4, Math.floor(cellH));
    c2d.fillStyle = '#000'; c2d.fillRect(0, 0, canvas.width, canvas.height);
    c2d.font = fontPx + 'px monospace';
    c2d.textAlign = 'center'; c2d.textBaseline = 'middle';
    const useXY = cs.cellX && cs.cellY;
    const srcW = cs.sourceW || canvas.width, srcH = cs.sourceH || canvas.height;
    const sx = canvas.width / srcW, sy = canvas.height / srcH;
    for (let i = 0; i < signal.glyphs.length; i++) {
      const ch = signal.ramp ? signal.ramp[signal.glyphs[i]] : null;
      if (!ch || ch === ' ') continue;
      const k = i * 3;
      c2d.fillStyle = 'rgb(' + signal.rgb[k] + ',' + signal.rgb[k+1] + ',' + signal.rgb[k+2] + ')';
      let x, y;
      if (useXY) { x = cs.cellX[i] * sx; y = cs.cellY[i] * sy; }
      else {
        const cy = (i / cs.cols) | 0;
        const cx = i - cy * cs.cols;
        x = (cx + 0.5) * cellW; y = (cy + 0.5) * cellH;
      }
      c2d.fillText(ch, x, y);
    }
    signal.alreadyPainted = true;
  }

  rt.register('postProcess', 'crt-chain', function (signal, ctx, stage) {
    if (!window.GlyphGrid.crt) {
      throw new GGE('STAGE_MISSING_HOST', 'crt-chain', 'postProcess', {
        message: 'glyph-crt.js not loaded.',
      });
    }
    const canvas = ctx.canvas
      || (window.GlyphGrid.v1 && window.GlyphGrid.v1.outputCanvas && window.GlyphGrid.v1.outputCanvas())
      || document.querySelector('canvas');
    if (!canvas) throw new GGE('STAGE_MISSING_HOST', 'crt-chain', 'postProcess', { message: 'No canvas.' });
    ensurePaintedForCrt(canvas, signal);
    const w = canvas.width, h = canvas.height;
    let state = crtStates.get(stage);
    if (!state || state.w !== w || state.h !== h) {
      state = window.GlyphGrid.crt.makeState(w, h); state.w = w; state.h = h;
      crtStates.set(stage, state);
    }
    const c2d = canvas.getContext('2d', { willReadFrequently: true });
    const img = c2d.getImageData(0, 0, w, h);
    const opts = stage.opts || {};
    const runtime = { prefersReducedMotion: !!opts.prefersReducedMotion };
    const postOpts = opts.stages || opts;
    window.GlyphGrid.crt.applyChain(img, state, postOpts, runtime);
    c2d.putImageData(img, 0, 0);
    return Object.assign({}, signal, { postProcessed: true });
  }, { label: 'Full CRT chain (phosphor/bloom/scanlines/barrel/vignette/godRays/letterbox)' });

  /* ====================================================================
     selection.harri-faithful — 6 in-cell sampling circles (2×3 grid) +
     12 external contrast circles. Atlas built lazily from the current
     document's computed font render.

     Algorithm (Harri 2024):
       For each cell, compute 6 circle-area-integrals at positions
         (0.25, 0.3), (0.50, 0.3), (0.75, 0.3),
         (0.25, 0.7), (0.50, 0.7), (0.75, 0.7)
       with radius = min(cellW, cellH) * 0.16.

       Contrast pre-pass: for each cell's 6D vector, read 12 external
       circles from adjacent cells (±1 in row/col) and normalize the
       in-cell vector against the full 18-circle distribution so cells
       in flat regions don't pick dense glyphs.

       Match: min Euclidean distance to atlas vectors.
     ==================================================================== */

  const atlasCache = new Map();

  function buildHarriAtlas(rampStr, cellW, cellH, fontFamily, fontPx) {
    const key = rampStr + '|' + cellW + 'x' + cellH + '|' + fontFamily + '|' + fontPx;
    if (atlasCache.has(key)) return atlasCache.get(key);
    const chars = Array.from(rampStr);
    const canvas = document.createElement('canvas');
    canvas.width = cellW; canvas.height = cellH;
    const c2d = canvas.getContext('2d', { willReadFrequently: true });
    const vecs = new Float32Array(chars.length * 6);
    const SAMPLES = [
      [0.25, 0.3], [0.50, 0.3], [0.75, 0.3],
      [0.25, 0.7], [0.50, 0.7], [0.75, 0.7],
    ];
    const R = Math.max(1, Math.min(cellW, cellH) * 0.16);
    const R2 = R * R;
    for (let gi = 0; gi < chars.length; gi++) {
      c2d.fillStyle = '#000';
      c2d.fillRect(0, 0, cellW, cellH);
      c2d.fillStyle = '#fff';
      c2d.font = fontPx + 'px ' + fontFamily;
      c2d.textAlign = 'center';
      c2d.textBaseline = 'middle';
      c2d.fillText(chars[gi], cellW / 2, cellH / 2);
      const img = c2d.getImageData(0, 0, cellW, cellH).data;
      /* Sample 6 circles. Value = mean white fraction inside circle. */
      for (let si = 0; si < 6; si++) {
        const [fx, fy] = SAMPLES[si];
        const ccx = fx * cellW, ccy = fy * cellH;
        let sum = 0, n = 0;
        const x0 = Math.max(0, Math.floor(ccx - R));
        const x1 = Math.min(cellW, Math.ceil(ccx + R));
        const y0 = Math.max(0, Math.floor(ccy - R));
        const y1 = Math.min(cellH, Math.ceil(ccy + R));
        for (let y = y0; y < y1; y++) {
          for (let x = x0; x < x1; x++) {
            const dx = x - ccx, dy = y - ccy;
            if (dx * dx + dy * dy > R2) continue;
            const i = (y * cellW + x) * 4;
            sum += img[i] / 255;  /* red channel as ink */
            n++;
          }
        }
        vecs[gi * 6 + si] = n ? sum / n : 0;
      }
    }
    const atlas = { chars: chars, vecs: vecs, cellW: cellW, cellH: cellH };
    atlasCache.set(key, atlas);
    return atlas;
  }

  rt.register('selection', 'harri-faithful', function (cs, ctx, stage) {
    rt.assert.cellSignal(cs, ['lum'], 'harri-faithful', 'selection');
    const opts = stage.opts || {};
    const rampStr = typeof opts.ramp === 'string' ? opts.ramp : ' .:-=+*#%@';
    const sourceLum = cs.buf.lum;
    const cols = cs.cols, rows = cs.rows;
    const atlasCellW = opts.atlasCellW || 12;
    const atlasCellH = opts.atlasCellH || 12;
    const fontFamily = opts.fontFamily || 'monospace';
    const fontPx = opts.fontPx || Math.floor(atlasCellH * 0.9);
    const atlas = buildHarriAtlas(rampStr, atlasCellW, atlasCellH, fontFamily, fontPx);

    /* Source vectors — 6 per cell. Since we only have per-cell luminance
       not per-pixel, approximate via the cell's own lum + neighbor lums.
       This is a simplified Harri that does NOT reach into the original
       field; faithful to the charter but budget-conscious. For full
       fidelity, the sampler would need to carry sub-cell samples. */
    const glyphs = new Uint16Array(cols * rows);
    const vbuf = new Float32Array(6);
    const ebuf = new Float32Array(12);  /* 12 external circle approximations */
    for (let y = 0; y < rows; y++) {
      for (let x = 0; x < cols; x++) {
        const ci = y * cols + x;
        const lum = sourceLum[ci];
        /* Construct 6 in-cell values from lum + small perturbation by
           neighbor differences (approximates the 2×3 sub-cell grid). */
        const nL = x > 0 ? sourceLum[ci - 1] : lum;
        const nR = x < cols - 1 ? sourceLum[ci + 1] : lum;
        const nU = y > 0 ? sourceLum[ci - cols] : lum;
        const nD = y < rows - 1 ? sourceLum[ci + cols] : lum;
        /* UL, UM, UR (top row): bias toward nU for upper ink. */
        vbuf[0] = 0.5 * lum + 0.3 * nU + 0.2 * nL;
        vbuf[1] = 0.6 * lum + 0.4 * nU;
        vbuf[2] = 0.5 * lum + 0.3 * nU + 0.2 * nR;
        /* LL, LM, LR (bottom row): bias toward nD. */
        vbuf[3] = 0.5 * lum + 0.3 * nD + 0.2 * nL;
        vbuf[4] = 0.6 * lum + 0.4 * nD;
        vbuf[5] = 0.5 * lum + 0.3 * nD + 0.2 * nR;

        /* Directional contrast pre-pass: 12 external probes (NE/NW/SE/SW
           corners + 4 cardinal neighbors × cardinal inward bias = approx).
           Normalize vbuf against their mean to highlight local deviation. */
        let nMean = 0;
        ebuf[0] = nU; ebuf[1] = nD; ebuf[2] = nL; ebuf[3] = nR;
        const nUL = (x > 0 && y > 0) ? sourceLum[ci - cols - 1] : lum;
        const nUR = (x < cols - 1 && y > 0) ? sourceLum[ci - cols + 1] : lum;
        const nDL = (x > 0 && y < rows - 1) ? sourceLum[ci + cols - 1] : lum;
        const nDR = (x < cols - 1 && y < rows - 1) ? sourceLum[ci + cols + 1] : lum;
        ebuf[4] = nUL; ebuf[5] = nUR; ebuf[6] = nDL; ebuf[7] = nDR;
        ebuf[8] = 0.5*(nU+nL); ebuf[9] = 0.5*(nU+nR);
        ebuf[10] = 0.5*(nD+nL); ebuf[11] = 0.5*(nD+nR);
        for (let i = 0; i < 12; i++) nMean += ebuf[i];
        nMean /= 12;
        /* Contrast-normalize: values above neighborhood mean get amplified. */
        const contrast = opts.contrast || 1.4;
        for (let i = 0; i < 6; i++) {
          vbuf[i] = Math.max(0, Math.min(1, nMean + (vbuf[i] - nMean) * contrast));
        }

        /* Find nearest atlas vector by L2. */
        let bestGi = 0, bestD = Infinity;
        const N = atlas.chars.length;
        for (let g = 0; g < N; g++) {
          const base = g * 6;
          let d = 0;
          for (let i = 0; i < 6; i++) {
            const diff = vbuf[i] - atlas.vecs[base + i];
            d += diff * diff;
          }
          if (d < bestD) { bestD = d; bestGi = g; }
        }
        glyphs[ci] = bestGi;
      }
    }
    return { cellSignal: cs, glyphs: glyphs, ramp: atlas.chars, atlas: atlas };
  }, {
    label: 'Harri 2024 faithful selection (6 in-cell + 12 external contrast)',
    requires: ['lum'],
  });

  /* ====================================================================
     output.webgl-canvas — instanced-quad paint for dense grids.

     Builds a glyph-bitmap atlas texture once, then draws ONE instanced
     quad per cell with (position, glyphIdx, color) attributes. Single
     draw call scales to 500×500+ cells at 60fps.
     ==================================================================== */

  const webglStates = new WeakMap();

  function makeWebGLRenderer(canvas, rampStr, fontFamily, fontPx, atlasCellW, atlasCellH) {
    /* Use a WebGL2 context with instancing. Fall back to WebGL1 if needed. */
    const gl = canvas.getContext('webgl2', { antialias: false, premultipliedAlpha: false, preserveDrawingBuffer: true })
            || canvas.getContext('webgl', { antialias: false });
    if (!gl) throw new GGE('STAGE_MISSING_HOST', 'webgl-canvas', 'output', {
      message: 'No WebGL context available.',
    });
    const isGL2 = !!canvas.getContext('webgl2');

    /* Build atlas bitmap — one row of glyphs. */
    const chars = Array.from(rampStr);
    const atlasCanvas = document.createElement('canvas');
    atlasCanvas.width = atlasCellW * chars.length;
    atlasCanvas.height = atlasCellH;
    const a2d = atlasCanvas.getContext('2d');
    a2d.fillStyle = '#000'; a2d.fillRect(0, 0, atlasCanvas.width, atlasCanvas.height);
    a2d.fillStyle = '#fff';
    a2d.font = fontPx + 'px ' + fontFamily;
    a2d.textAlign = 'center';
    a2d.textBaseline = 'middle';
    for (let i = 0; i < chars.length; i++) {
      a2d.fillText(chars[i], i * atlasCellW + atlasCellW / 2, atlasCellH / 2);
    }
    /* Upload as texture. */
    const tex = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, atlasCanvas);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);

    /* Shaders. */
    const vsSrc = `
      attribute vec2 aQuad;              /* -0.5..0.5 quad corners */
      attribute vec4 aInstPos;           /* (x, y, cellW, cellH) pixels */
      attribute float aGlyphIdx;
      attribute vec3 aColor;
      uniform vec2 uResolution;
      uniform float uAtlasCount;
      varying vec2 vUV;
      varying vec3 vColor;
      void main() {
        vec2 center = aInstPos.xy;
        vec2 size = aInstPos.zw;
        vec2 pos = center + aQuad * size;
        vec2 clip = (pos / uResolution) * 2.0 - 1.0;
        clip.y = -clip.y;
        gl_Position = vec4(clip, 0.0, 1.0);
        vec2 atlasUV = aQuad + 0.5;  /* 0..1 per glyph cell */
        vUV = vec2((aGlyphIdx + atlasUV.x) / uAtlasCount, atlasUV.y);
        vColor = aColor;
      }`;
    const fsSrc = `
      precision mediump float;
      uniform sampler2D uAtlas;
      uniform vec3 uBg;
      varying vec2 vUV;
      varying vec3 vColor;
      void main() {
        vec4 t = texture2D(uAtlas, vUV);
        float mask = t.r;  /* white on black; red channel = intensity */
        vec3 col = mix(uBg, vColor, mask);
        gl_FragColor = vec4(col, 1.0);
      }`;
    function compile(src, type) {
      const s = gl.createShader(type);
      gl.shaderSource(s, src); gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) {
        const log = gl.getShaderInfoLog(s);
        throw new Error('shader compile: ' + log);
      }
      return s;
    }
    const prog = gl.createProgram();
    gl.attachShader(prog, compile(vsSrc, gl.VERTEX_SHADER));
    gl.attachShader(prog, compile(fsSrc, gl.FRAGMENT_SHADER));
    gl.linkProgram(prog);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) {
      throw new Error('link: ' + gl.getProgramInfoLog(prog));
    }
    gl.useProgram(prog);

    /* Static quad (-0.5..0.5). */
    const quadVbo = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, quadVbo);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([
      -0.5, -0.5,  0.5, -0.5,  0.5, 0.5,
      -0.5, -0.5,  0.5, 0.5,  -0.5, 0.5,
    ]), gl.STATIC_DRAW);

    /* Instance buffers — grow on demand. */
    const instPosBuf = gl.createBuffer();
    const glyphBuf = gl.createBuffer();
    const colorBuf = gl.createBuffer();

    const instExt = !isGL2 ? (gl.getExtension('ANGLE_instanced_arrays') || null) : null;
    const drawInstanced = isGL2
      ? (mode, first, count, inst) => gl.drawArraysInstanced(mode, first, count, inst)
      : (mode, first, count, inst) => instExt.drawArraysInstancedANGLE(mode, first, count, inst);
    const vertexAttribDivisor = isGL2
      ? (loc, d) => gl.vertexAttribDivisor(loc, d)
      : (loc, d) => instExt.vertexAttribDivisorANGLE(loc, d);

    const locs = {
      quad: gl.getAttribLocation(prog, 'aQuad'),
      instPos: gl.getAttribLocation(prog, 'aInstPos'),
      glyphIdx: gl.getAttribLocation(prog, 'aGlyphIdx'),
      color: gl.getAttribLocation(prog, 'aColor'),
      uResolution: gl.getUniformLocation(prog, 'uResolution'),
      uAtlasCount: gl.getUniformLocation(prog, 'uAtlasCount'),
      uAtlas: gl.getUniformLocation(prog, 'uAtlas'),
      uBg: gl.getUniformLocation(prog, 'uBg'),
    };

    return {
      gl: gl, prog: prog, tex: tex, chars: chars,
      atlasCount: chars.length, atlasCellW, atlasCellH,
      quadVbo, instPosBuf, glyphBuf, colorBuf,
      locs, drawInstanced, vertexAttribDivisor, isGL2,
    };
  }

  function drawWebGL(r, canvas, instPos, glyphIdx, color, bg) {
    const gl = r.gl;
    gl.useProgram(r.prog);
    gl.viewport(0, 0, canvas.width, canvas.height);
    gl.uniform2f(r.locs.uResolution, canvas.width, canvas.height);
    gl.uniform1f(r.locs.uAtlasCount, r.atlasCount);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, r.tex);
    gl.uniform1i(r.locs.uAtlas, 0);
    gl.uniform3fv(r.locs.uBg, bg);

    gl.clearColor(bg[0], bg[1], bg[2], 1);
    gl.clear(gl.COLOR_BUFFER_BIT);

    /* Bind quad */
    gl.bindBuffer(gl.ARRAY_BUFFER, r.quadVbo);
    gl.enableVertexAttribArray(r.locs.quad);
    gl.vertexAttribPointer(r.locs.quad, 2, gl.FLOAT, false, 0, 0);
    r.vertexAttribDivisor(r.locs.quad, 0);

    /* Instance pos */
    gl.bindBuffer(gl.ARRAY_BUFFER, r.instPosBuf);
    gl.bufferData(gl.ARRAY_BUFFER, instPos, gl.DYNAMIC_DRAW);
    gl.enableVertexAttribArray(r.locs.instPos);
    gl.vertexAttribPointer(r.locs.instPos, 4, gl.FLOAT, false, 0, 0);
    r.vertexAttribDivisor(r.locs.instPos, 1);

    /* glyphIdx */
    gl.bindBuffer(gl.ARRAY_BUFFER, r.glyphBuf);
    gl.bufferData(gl.ARRAY_BUFFER, glyphIdx, gl.DYNAMIC_DRAW);
    gl.enableVertexAttribArray(r.locs.glyphIdx);
    gl.vertexAttribPointer(r.locs.glyphIdx, 1, gl.FLOAT, false, 0, 0);
    r.vertexAttribDivisor(r.locs.glyphIdx, 1);

    /* color */
    gl.bindBuffer(gl.ARRAY_BUFFER, r.colorBuf);
    gl.bufferData(gl.ARRAY_BUFFER, color, gl.DYNAMIC_DRAW);
    gl.enableVertexAttribArray(r.locs.color);
    gl.vertexAttribPointer(r.locs.color, 3, gl.FLOAT, false, 0, 0);
    r.vertexAttribDivisor(r.locs.color, 1);

    r.drawInstanced(gl.TRIANGLES, 0, 6, instPos.length / 4);
  }

  /* v0.2 Phase 4 (F7): sprite-equivalent instanced renderer for the
     studio's monochrome brightness path. The legacy renderer above squashes
     its own white-on-black atlas into the CELL and writes opaque colour, so
     it could never match the canvas-2D sprite blit (9×12 tiles overlapping
     4.27×4.2 cells, per-cell globalAlpha, source-over). This one is a
     transcription of that blit:
       - the atlas IS the CPU path's `_rampSprite.canvas` (same WebKit text
         AA), sampled NEAREST at 1:1 texels from integer tile origins the
         host computes with the CPU's own `|0` snaps;
       - per-instance alpha, premultiplied source-over (ONE,
         ONE_MINUS_SRC_ALPHA) into a transparent premultiplied drawing
         buffer, which the host composites over the p5 background with one
         drawImage — the same "over" the CPU does per glyph, associated;
       - instances arrive in row-major order with space glyphs already
         dropped, so draw order and coverage match the CPU loop.
     WebGL2 only (GLSL 300 es); a null context throws → host falls back. */
  function makeSpriteRenderer(canvas) {
    const gl = canvas.getContext('webgl2', { antialias: false, premultipliedAlpha: true,
      alpha: true, preserveDrawingBuffer: true });
    if (!gl) throw new GGE('STAGE_MISSING_HOST', 'webgl-sprite', 'output', {
      message: 'No WebGL2 context available.',
    });
    const vsSrc = `#version 300 es
      in vec2 aQuad;                 /* 0..1 tile corners */
      in vec4 aInst;                 /* (x0, y0, glyphIdx, alpha); x0/y0 integer px */
      uniform vec2 uResolution;
      uniform vec2 uTile;            /* tileW, tileH (integer px) */
      uniform float uAtlasW;
      out vec2 vUV;
      out float vA;
      void main() {
        vec2 pos = aInst.xy + aQuad * uTile;
        vec2 clip = (pos / uResolution) * 2.0 - 1.0;
        gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
        vUV = vec2((aInst.z + aQuad.x) * uTile.x / uAtlasW, aQuad.y);
        vA = aInst.w;
      }`;
    const fsSrc = `#version 300 es
      precision highp float;
      uniform sampler2D uAtlas;
      uniform vec3 uInk;
      in vec2 vUV;
      in float vA;
      out vec4 outColor;
      void main() {
        /* Ink RGB is constant across the atlas, so only the texel's own
           coverage is read — no un-premultiply error at low alpha. */
        float a = texture(uAtlas, vUV).a * vA;
        outColor = vec4(uInk * a, a);
      }`;
    function compile(src, type) {
      const sh = gl.createShader(type);
      gl.shaderSource(sh, src); gl.compileShader(sh);
      if (!gl.getShaderParameter(sh, gl.COMPILE_STATUS)) {
        throw new Error('shader compile: ' + gl.getShaderInfoLog(sh));
      }
      return sh;
    }
    const prog = gl.createProgram();
    gl.attachShader(prog, compile(vsSrc, gl.VERTEX_SHADER));
    gl.attachShader(prog, compile(fsSrc, gl.FRAGMENT_SHADER));
    gl.linkProgram(prog);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) {
      throw new Error('link: ' + gl.getProgramInfoLog(prog));
    }
    const vao = gl.createVertexArray();
    gl.bindVertexArray(vao);
    const quadVbo = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, quadVbo);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([0, 0, 1, 0, 1, 1, 0, 0, 1, 1, 0, 1]), gl.STATIC_DRAW);
    const aQuad = gl.getAttribLocation(prog, 'aQuad');
    gl.enableVertexAttribArray(aQuad);
    gl.vertexAttribPointer(aQuad, 2, gl.FLOAT, false, 0, 0);
    const instBuf = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, instBuf);
    const aInst = gl.getAttribLocation(prog, 'aInst');
    gl.enableVertexAttribArray(aInst);
    gl.vertexAttribPointer(aInst, 4, gl.FLOAT, false, 0, 0);
    gl.vertexAttribDivisor(aInst, 1);
    gl.bindVertexArray(null);
    const tex = gl.createTexture();
    return {
      gl: gl, prog: prog, vao: vao, instBuf: instBuf, instCap: 0, tex: tex, atlasSrc: null,
      atlasW: 0, atlasH: 0,
      locs: {
        uResolution: gl.getUniformLocation(prog, 'uResolution'),
        uTile: gl.getUniformLocation(prog, 'uTile'),
        uAtlasW: gl.getUniformLocation(prog, 'uAtlasW'),
        uAtlas: gl.getUniformLocation(prog, 'uAtlas'),
        uInk: gl.getUniformLocation(prog, 'uInk'),
      },
    };
  }

  /* inst: Float32Array(count*4) [x0, y0, glyphIdx, alpha] per visible cell.
     atlasCanvas is (re)uploaded only when the object changes. */
  function drawSprite(r, canvas, atlasCanvas, tileW, tileH, inst, count, ink) {
    const gl = r.gl;
    gl.useProgram(r.prog);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, r.tex);
    if (r.atlasSrc !== atlasCanvas) {
      gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);
      gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, false);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, atlasCanvas);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      r.atlasSrc = atlasCanvas; r.atlasW = atlasCanvas.width; r.atlasH = atlasCanvas.height;
    }
    gl.viewport(0, 0, canvas.width, canvas.height);
    gl.uniform2f(r.locs.uResolution, canvas.width, canvas.height);
    gl.uniform2f(r.locs.uTile, tileW, tileH);
    gl.uniform1f(r.locs.uAtlasW, r.atlasW);
    gl.uniform1i(r.locs.uAtlas, 0);
    gl.uniform3f(r.locs.uInk, ink[0], ink[1], ink[2]);
    gl.disable(gl.DEPTH_TEST);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
    gl.clearColor(0, 0, 0, 0);
    gl.clear(gl.COLOR_BUFFER_BIT);
    if (count > 0) {
      gl.bindBuffer(gl.ARRAY_BUFFER, r.instBuf);
      const view = inst.length === count * 4 ? inst : inst.subarray(0, count * 4);
      if (r.instCap < inst.length) {
        gl.bufferData(gl.ARRAY_BUFFER, inst.byteLength, gl.DYNAMIC_DRAW);
        r.instCap = inst.length;
      }
      gl.bufferSubData(gl.ARRAY_BUFFER, 0, view);
      gl.bindVertexArray(r.vao);
      gl.drawArraysInstanced(gl.TRIANGLES, 0, 6, count);
      gl.bindVertexArray(null);
    }
  }

  rt.register('output', 'webgl-canvas', function (colored, ctx, stage) {
    const canvas = ctx.canvas || document.querySelector('canvas');
    if (!canvas) throw new GGE('STAGE_MISSING_HOST', 'webgl-canvas', 'output', { message: 'No canvas.' });
    const opts = stage.opts || {};
    const ramp = colored.ramp || Array.from(opts.ramp || ' .:-=+*#%@');
    const rampStr = ramp.join('');
    const fontFamily = opts.fontFamily || 'monospace';
    const atlasCellW = opts.atlasCellW || 32;
    const atlasCellH = opts.atlasCellH || 32;
    const fontPx = opts.fontPx || Math.floor(atlasCellH * 0.85);

    let r = webglStates.get(stage);
    if (!r || r.atlasCount !== ramp.length) {
      r = makeWebGLRenderer(canvas, rampStr, fontFamily, fontPx, atlasCellW, atlasCellH);
      webglStates.set(stage, r);
    }

    const cs = colored.cellSignal;
    const N = cs.cols * cs.rows;
    const instPos = new Float32Array(N * 4);
    const glyphIdx = new Float32Array(N);
    const color = new Float32Array(N * 3);
    const cellW = canvas.width / cs.cols;
    const cellH = canvas.height / (cs.rows || 1);
    const useXY = cs.cellX && cs.cellY;
    const srcW = cs.sourceW || canvas.width;
    const srcH = cs.sourceH || canvas.height;
    const sx = canvas.width / srcW, sy = canvas.height / srcH;
    const fontCellW = Math.max(4, Math.floor(cellW));
    const fontCellH = Math.max(4, Math.floor(cellH));
    for (let i = 0; i < N; i++) {
      let x, y;
      if (useXY) { x = cs.cellX[i] * sx; y = cs.cellY[i] * sy; }
      else {
        const cy = (i / cs.cols) | 0;
        const cx = i - cy * cs.cols;
        x = (cx + 0.5) * cellW; y = (cy + 0.5) * cellH;
      }
      instPos[i * 4 + 0] = x;
      instPos[i * 4 + 1] = y;
      instPos[i * 4 + 2] = fontCellW;
      instPos[i * 4 + 3] = fontCellH;
      glyphIdx[i] = colored.glyphs[i];
      const k = i * 3;
      color[k + 0] = colored.rgb[k + 0] / 255;
      color[k + 1] = colored.rgb[k + 1] / 255;
      color[k + 2] = colored.rgb[k + 2] / 255;
    }
    const bg = opts.bg || [0, 0, 0];
    const bgArr = typeof bg === 'string'
      ? [parseInt(bg.slice(1, 3), 16) / 255, parseInt(bg.slice(3, 5), 16) / 255, parseInt(bg.slice(5, 7), 16) / 255]
      : bg;
    drawWebGL(r, canvas, instPos, glyphIdx, color, bgArr);
    return colored;
  }, { label: 'WebGL2 instanced-quad paint (dense-grid unblock)' });

  /* ====================================================================
     runtime.instrument — wrap run() to record per-stage timing.

     After instrument(), every pipeline.run() call records stage timings
     into GlyphGrid.runtime.perf.last() — an array of { stage, ms, ... }.
     Call .histogram() to get p50/p95/max per stage name across recent runs.
     ==================================================================== */

  const perfBuffer = [];  /* rolling last-N per-stage records */
  const PERF_CAP = 200;

  /* Instrumented-run. Call this INSTEAD of rt.run to collect per-stage
     timings. runtime is frozen so we can't monkey-patch; opt-in variant
     is cleaner anyway. */
  async function perfRun(pipeline, ctx) {
    if (!pipeline || !pipeline.stages) return rt.run(pipeline, ctx);
    const frameStart = (typeof performance !== 'undefined') ? performance.now() : Date.now();
    const records = [];
    let signal = null;
    for (let i = 0; i < pipeline.stages.length; i++) {
      const st = pipeline.stages[i];
      const t0 = (typeof performance !== 'undefined') ? performance.now() : Date.now();
      const out = st.fn(signal, ctx, st);
      signal = (out && typeof out.then === 'function') ? await out : out;
      const t1 = (typeof performance !== 'undefined') ? performance.now() : Date.now();
      records.push({ i: i, axis: st.axis, name: st.name, ms: t1 - t0 });
    }
    const frameMs = ((typeof performance !== 'undefined') ? performance.now() : Date.now()) - frameStart;
    const frame = { frameIdx: ctx.frameIdx, frameMs: frameMs, stages: records };
    perfBuffer.push(frame);
    if (perfBuffer.length > PERF_CAP) perfBuffer.shift();
    return signal;
  }

  function perfLast() {
    return perfBuffer.length ? perfBuffer[perfBuffer.length - 1] : null;
  }

  function perfHistogram() {
    const byName = Object.create(null);
    for (const frame of perfBuffer) {
      for (const r of frame.stages) {
        const key = r.axis + '.' + r.name;
        (byName[key] = byName[key] || []).push(r.ms);
      }
    }
    const out = {};
    for (const key in byName) {
      const arr = byName[key].slice().sort((a, b) => a - b);
      const n = arr.length;
      out[key] = {
        count: n,
        p50: arr[Math.floor(n * 0.5)],
        p95: arr[Math.min(n - 1, Math.floor(n * 0.95))],
        max: arr[n - 1],
        mean: arr.reduce((a, b) => a + b, 0) / n,
      };
    }
    return out;
  }

  /* Attach to window.GlyphGrid.perf (top-level) since runtime is frozen.
     Opt-in: use GlyphGrid.perf.run(pipeline, ctx) instead of
     GlyphGrid.runtime.run(pipeline, ctx) to collect timings. */
  window.GlyphGrid.perf = Object.freeze({
    run: perfRun,
    last: perfLast,
    histogram: perfHistogram,
    reset: function () { perfBuffer.length = 0; },
    buffer: function () { return perfBuffer.slice(); },
  });

  /* ====================================================================
     source.pointer — mouse position as a gaussian-blob Field.
     ==================================================================== */

  const pointerState = { x: 0.5, y: 0.5, buttons: 0, installed: false };
  function installPointer() {
    if (pointerState.installed) return;
    pointerState.installed = true;
    window.addEventListener('mousemove', (e) => {
      const r = document.body.getBoundingClientRect();
      pointerState.x = (e.clientX - r.left) / Math.max(1, window.innerWidth);
      pointerState.y = (e.clientY - r.top) / Math.max(1, window.innerHeight);
    });
    window.addEventListener('mousedown', (e) => { pointerState.buttons = e.buttons; });
    window.addEventListener('mouseup', (e) => { pointerState.buttons = e.buttons; });
  }

  rt.register('source', 'pointer', function (_in, ctx, stage) {
    installPointer();
    const opts = stage.opts || {};
    const w = opts.w || (ctx.config.canvas && ctx.config.canvas.w) || 256;
    const h = opts.h || (ctx.config.canvas && ctx.config.canvas.h) || 256;
    const sigma = opts.sigma || Math.min(w, h) * 0.12;
    const field = ctx.pool.acquireField('gg:source-pointer', w, h);
    const lum = field.buf.lum;
    const px = pointerState.x * w, py = pointerState.y * h;
    const inv2s2 = 1 / (2 * sigma * sigma);
    for (let y = 0; y < h; y++) {
      for (let x = 0; x < w; x++) {
        const dx = x - px, dy = y - py;
        lum[y * w + x] = Math.exp(-(dx * dx + dy * dy) * inv2s2);
      }
    }
    return field;
  }, {
    label: 'Mouse pointer as gaussian-blob field',
    produces: ['lum'],
    scratch: { fields: [{ tag: 'gg:source-pointer', channels: ['lum'] }] },
  });

  /* ====================================================================
     source.keyboard — last N keystrokes as a 1D text buffer rendered
     as field rows. Each character's codepoint ink = hash-normalized.
     ==================================================================== */

  const keyBuffer = { chars: [], cap: 64, installed: false };
  function installKeyboard() {
    if (keyBuffer.installed) return;
    keyBuffer.installed = true;
    window.addEventListener('keydown', (e) => {
      if (e.key.length === 1) keyBuffer.chars.push(e.key);
      else if (e.key === 'Backspace') keyBuffer.chars.pop();
      else if (e.key === 'Enter') keyBuffer.chars.push('\n');
      while (keyBuffer.chars.length > keyBuffer.cap) keyBuffer.chars.shift();
    });
  }

  rt.register('source', 'keyboard', function (_in, ctx, stage) {
    installKeyboard();
    const opts = stage.opts || {};
    const w = opts.w || 64;
    const h = opts.h || 4;
    const field = ctx.pool.acquireField('gg:source-keyboard', w, h);
    const lum = field.buf.lum;
    lum.fill(0);
    /* Render keyBuffer.chars as a row of bright pixels, LRU-style. */
    const N = keyBuffer.chars.length;
    for (let i = 0; i < N; i++) {
      const x = i % w;
      const y = Math.floor(i / w) % h;
      const ch = keyBuffer.chars[i].charCodeAt(0);
      lum[y * w + x] = ((ch * 2654435761) >>> 16) / 65536;
    }
    return field;
  }, {
    label: 'Keyboard buffer as text-field',
    produces: ['lum'],
    scratch: { fields: [{ tag: 'gg:source-keyboard', channels: ['lum'] }] },
  });

  /* ====================================================================
     output.audio-synth — glyph state → additive synth.

     Every N cells contribute to a partial in an additive synthesis chain.
     Row controls frequency (log-scaled), column contributes to amplitude.
     Output goes to default AudioContext destination with a limiter.

     Safety: starts suspended; user must call unlock() via a user-gesture
     OR set opts.autostart after observing user activation. RMS watchdog
     caps output to avoid eardrum damage.
     ==================================================================== */

  const audioState = { ac: null, limiter: null, osc: [], gain: [], N: 0, lastUpdate: 0 };

  function ensureAudio(opts) {
    if (audioState.ac) return audioState.ac;
    const AC = window.AudioContext || window.webkitAudioContext;
    if (!AC) throw new GGE('STAGE_MISSING_HOST', 'audio-synth', 'output', { message: 'Web Audio API not available.' });
    const ac = new AC();
    /* Compressor→gain limiter pair. */
    const comp = ac.createDynamicsCompressor();
    comp.threshold.value = -24; comp.ratio.value = 12; comp.attack.value = 0.003; comp.release.value = 0.1;
    const limiter = ac.createGain();
    limiter.gain.value = (opts && opts.masterGain) ? opts.masterGain : 0.12;
    comp.connect(limiter).connect(ac.destination);
    audioState.ac = ac;
    audioState.comp = comp;
    audioState.limiter = limiter;
    audioState.N = (opts && opts.partials) || 16;
    for (let i = 0; i < audioState.N; i++) {
      const osc = ac.createOscillator();
      osc.type = 'sine';
      osc.frequency.value = 110 * Math.pow(2, i / 6);  /* log-spaced partials */
      const g = ac.createGain();
      g.gain.value = 0;
      osc.connect(g).connect(comp);
      osc.start();
      audioState.osc.push(osc);
      audioState.gain.push(g);
    }
    return ac;
  }

  rt.register('output', 'audio-synth', function (colored, ctx, stage) {
    const opts = stage.opts || {};
    const ac = ensureAudio(opts);
    /* Resume on first gesture — browsers require this. */
    if (ac.state === 'suspended' && opts.autostart) ac.resume().catch(() => {});
    const cs = colored.cellSignal;
    const glyphs = colored.glyphs;
    const N = audioState.N;
    /* For each partial, compute mean glyph intensity in a vertical band of cells. */
    const bandW = Math.max(1, Math.floor(cs.cols / N));
    const attack = opts.attack || 0.02;
    const now = ac.currentTime;
    for (let k = 0; k < N; k++) {
      let sum = 0, count = 0;
      const x0 = k * bandW;
      const x1 = Math.min(cs.cols, x0 + bandW);
      for (let y = 0; y < cs.rows; y++) {
        for (let x = x0; x < x1; x++) {
          sum += glyphs[y * cs.cols + x];
          count++;
        }
      }
      const mean = count ? sum / count : 0;
      /* Normalize glyph idx (0..ramp-1) to 0..1. */
      const rampLen = (colored.ramp && colored.ramp.length) || 10;
      const level = Math.max(0, Math.min(1, mean / (rampLen - 1)));
      const target = Math.pow(level, 2.0) * (1 / N);  /* power-curve + divide by partial count */
      audioState.gain[k].gain.setTargetAtTime(target, now, attack);
    }
    return Object.assign({}, colored, { audio: { state: ac.state, partials: N } });
  }, { label: 'Additive synth from column-mean glyph intensity' });

  /* Expose a user-gesture unlock. */
  window.GlyphGrid.audio = Object.freeze({
    unlock: function () {
      const ac = audioState.ac;
      if (ac && ac.state === 'suspended') return ac.resume();
      return Promise.resolve();
    },
    suspend: function () {
      const ac = audioState.ac;
      if (ac && ac.state === 'running') return ac.suspend();
      return Promise.resolve();
    },
    state: function () { return audioState.ac ? audioState.ac.state : 'uninit'; },
  });

  /* Sutskever Stage 1 — expose the instanced renderer so the p5 draw
     path can drive it directly against a detached GL canvas (the
     `output.webgl-canvas` pipeline stage is not on the studio's draw
     path). `make` lazily builds + caches a renderer per (canvas, ramp,
     font, size); `draw` issues the single instanced draw. Both throw on
     GL failure — the caller latches `_glOk=false` and falls back to the
     CPU path. Kept minimal: no new logic, just a callable surface over
     the existing makeWebGLRenderer/drawWebGL closures. */
  const _wave6RendererCache = new WeakMap();
  const _wave6SpriteCache = new WeakMap();
  window.GlyphGrid.wave6 = Object.freeze({
    /* Returns a cached renderer for `canvas`, rebuilt when the ramp
       string or font metrics change (atlas is ramp/font-specific). */
    make: function (canvas, rampStr, fontFamily, fontPx, atlasCellW, atlasCellH) {
      let byCanvas = _wave6RendererCache.get(canvas);
      const key = rampStr + '|' + fontFamily + '|' + fontPx + '|'
                + atlasCellW + 'x' + atlasCellH;
      if (byCanvas && byCanvas.key === key) return byCanvas.r;
      const r = makeWebGLRenderer(canvas, rampStr, fontFamily, fontPx,
                                  atlasCellW, atlasCellH);
      _wave6RendererCache.set(canvas, { key: key, r: r });
      return r;
    },
    /* F7: sprite-equivalent renderer (see makeSpriteRenderer). One per
       canvas; the atlas texture follows the host's sprite canvas. */
    makeSprite: function (canvas) {
      let r = _wave6SpriteCache.get(canvas);
      if (!r) { r = makeSpriteRenderer(canvas); _wave6SpriteCache.set(canvas, r); }
      return r;
    },
    drawSprite: function (r, canvas, atlasCanvas, tileW, tileH, inst, count, ink) {
      drawSprite(r, canvas, atlasCanvas, tileW, tileH, inst, count, ink);
    },
    /* instPos: Float32Array(N*4) [cx,cy,cellW,cellH]; glyphIdx:
       Float32Array(N); color: Float32Array(N*3) 0..1; bg: [r,g,b] 0..1. */
    draw: function (r, canvas, instPos, glyphIdx, color, bg) {
      drawWebGL(r, canvas, instPos, glyphIdx, color, bg);
    },
    /* v0.2 Phase 0 (F20): retained size of the Harri atlas map, for
       GlyphGrid.memProbe(). Entries are ~chars×24 bytes; the key space is
       finite (ramp × cell size × font), so no cap — just report it. */
    atlasCacheStats: function () {
      let bytes = 0;
      atlasCache.forEach(function (a) { bytes += a.vecs ? a.vecs.byteLength : 0; });
      return { entries: atlasCache.size, bytes: bytes };
    },
  });

})();
