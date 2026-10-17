/* glyph-preprocess-gl.js — WebGL2 port of the studio scene's source
 * preprocess (v0.2 Phase 4, research F8 Phase A).
 *
 * The JS path in index.html (`SCENES.studio`, "base image preprocess") runs
 * ~10 full-canvas loops per animated source frame: a luminance + 3×3 box
 * blur + unsharp/gain pass, a luminance + 5-tap blur + face-zone fine
 * sharpen, and a luminance + 5-tap blur + chip/tweezer/hand zone pass, with
 * one getImageData before and one putImageData after. The three stages are
 * data-dependent (each re-derives luminance from the previous stage's
 * uint8-clamped pixels), so the port is THREE dependent fragment passes:
 *
 *   source (2D canvas upload) → pass 1 → RGBA8 A → pass 2 → RGBA8 B
 *                             → pass 3 → the GL canvas's drawing buffer
 *
 * RGBA8 intermediates reproduce the Uint8ClampedArray store between stages;
 * `q()` reproduces its rounding (round half to even, clamp 0..255). Blur
 * planes are zero on the 1-px border exactly like the JS scratch planes
 * (ensureSourceScratch zero-fills them), so the border pixels get the same
 * lum×sharpen lift. Every constant is copied verbatim from index.html; the
 * geometry (faceY1, chip/axis/hand anchors) is computed once by the host and
 * passed in, so both paths share one source of truth for it.
 *
 * Expected residual vs the JS path: float32 vs float64 arithmetic flips an
 * occasional exact-.5 rounding (±1 LSB), which then propagates through the
 * later passes' luminance — measured, see BUGS ITER-066.
 *
 * API: GlyphGrid.preprocessGL.run(srcCanvas, W, H, geo) → the GL canvas
 * (caller drawImage()s it) or null when WebGL2 is unavailable / failed
 * (caller runs the JS path). A failure latches the module off for the
 * session. No network, no paid API — a local GL context only.
 */
(function () {
  'use strict';

  const VS = `#version 300 es
    void main() {
      vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
      gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
    }`;

  const COMMON = `#version 300 es
    precision highp float;
    precision highp int;
    precision highp sampler2D;
    uniform sampler2D uSrc;
    uniform ivec2 uSize;
    uniform int uFlipOut;   /* 1 = writing the default framebuffer (bottom-up rows) */
    out vec4 outColor;
    vec4 px(ivec2 p) { return floor(texelFetch(uSrc, p, 0) * 255.0 + 0.5); }
    float lumAt(ivec2 p) { vec4 c = px(p); return 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b; }
    /* Uint8ClampedArray store: clamp, then round half to even. */
    float q(float v) {
      v = clamp(v, 0.0, 255.0);
      float f = floor(v);
      float d = v - f;
      if (d > 0.5) return f + 1.0;
      if (d < 0.5) return f;
      return mod(f, 2.0) == 0.0 ? f : f + 1.0;
    }
    ivec2 coord() {
      ivec2 p = ivec2(gl_FragCoord.xy);
      if (uFlipOut == 1) p.y = uSize.y - 1 - p.y;
      return p;
    }
    bool interior(ivec2 p) {
      return p.x >= 1 && p.y >= 1 && p.x < uSize.x - 1 && p.y < uSize.y - 1;
    }
    float blur5(ivec2 p) {
      if (!interior(p)) return 0.0;
      return (lumAt(p) + lumAt(p + ivec2(-1, 0)) + lumAt(p + ivec2(1, 0))
            + lumAt(p + ivec2(0, -1)) + lumAt(p + ivec2(0, 1))) / 5.0;
    }
  `;

  /* Pass 1 — Step A+B: 3×3 box blur of luminance, unsharp + global contrast,
     face zone (y < faceY1) gets sharpenFace/gainFace. */
  const FS1 = COMMON + `
    uniform int uFaceY1;
    void main() {
      ivec2 p = coord();
      vec4 c = px(p);
      float l = lumAt(p);
      float b = 0.0;
      if (interior(p)) {
        float s = 0.0;
        for (int dy = -1; dy <= 1; dy++)
          for (int dx = -1; dx <= 1; dx++) s += lumAt(p + ivec2(dx, dy));
        b = s / 9.0;
      }
      const float gainGlobal = 1.55, midpoint = 128.0, sharpenGlobal = 1.10;
      const float sharpenFace = 1.85, gainFace = 1.12;
      bool inFace = p.y < uFaceY1;
      float gain = inFace ? gainGlobal * gainFace : gainGlobal;
      float sharpen = inFace ? sharpenFace : sharpenGlobal;
      float hi = (l - b) * sharpen;
      vec3 v = (c.rgb + hi - midpoint) * gain + midpoint;
      outColor = vec4(q(v.r), q(v.g), q(v.b), c.a) / 255.0;
    }`;

  /* Pass 2 — fine sharpen over the face zone with the 5-tap cross blur. */
  const FS2 = COMMON + `
    uniform int uFaceY1;
    void main() {
      ivec2 p = coord();
      vec4 c = px(p);
      if (p.y >= uFaceY1) { outColor = c / 255.0; return; }
      const float fineSharpen = 0.75;
      float hi = (lumAt(p) - blur5(p)) * fineSharpen;
      vec3 v = c.rgb + hi;
      outColor = vec4(q(v.r), q(v.g), q(v.b), c.a) / 255.0;
    }`;

  /* Pass 3 — chip / tweezer-axis / hand soft-mask zones. */
  const FS3 = COMMON + `
    uniform vec2 uTip;       /* tipX, tipY */
    uniform vec2 uAxisU;     /* ux, uy */
    uniform vec2 uTRange;    /* -sourceDrawW*0.06, axisLen + sourceDrawW*0.04 */
    uniform vec3 uBand;      /* bandHalf, bandFlat, chipRadius */
    uniform vec2 uHand;      /* handX, handY */
    uniform vec3 uHandR;     /* handRx, handRy, handFlatR */
    void main() {
      ivec2 p = coord();
      vec4 c = px(p);
      float x = float(p.x), y = float(p.y);
      float axisW = 0.0, chipW = 0.0;
      float dx_tip = x - uTip.x, dy_tip = y - uTip.y;
      float t = dx_tip * uAxisU.x + dy_tip * uAxisU.y;
      if (t >= uTRange.x && t <= uTRange.y) {
        float projX = uTip.x + t * uAxisU.x, projY = uTip.y + t * uAxisU.y;
        float perp = length(vec2(x - projX, y - projY));
        float bandHalf = uBand.x, bandFlat = uBand.y, chipRadius = uBand.z;
        if (perp < bandHalf) {
          axisW = perp <= bandFlat ? 1.0 : 1.0 - (perp - bandFlat) / (bandHalf - bandFlat);
        }
        float dTip = length(vec2(dx_tip, dy_tip));
        chipW = dTip <= chipRadius ? 1.0 : max(0.0, 1.0 - (dTip - chipRadius) / chipRadius);
      }
      float ndx = (x - uHand.x) / uHandR.x;
      float ndy = (y - uHand.y) / uHandR.y;
      float ellipseR = sqrt(ndx * ndx + ndy * ndy);
      float handW = 0.0;
      if (ellipseR < 1.0) {
        handW = ellipseR <= uHandR.z ? 1.0 : 1.0 - (ellipseR - uHandR.z) / (1.0 - uHandR.z);
      }
      if (axisW == 0.0 && chipW == 0.0 && handW == 0.0) { outColor = c / 255.0; return; }
      const float sharpAxis = 1.30, sharpChip = 2.10, sharpHand = 1.45;
      const float gainAxis = 1.18, gainChip = 1.45, gainHand = 1.30;
      const float midpoint2 = 110.0;
      float sharpenContrib = (sharpAxis * axisW) + (sharpChip - sharpAxis) * chipW + (sharpHand * handW);
      float gainContrib = 1.0 + (gainAxis - 1.0) * axisW + (gainChip - gainAxis) * chipW + (gainHand - 1.0) * handW;
      float hi = (lumAt(p) - blur5(p)) * sharpenContrib;
      vec3 v = (c.rgb + hi - midpoint2) * gainContrib + midpoint2;
      outColor = vec4(q(v.r), q(v.g), q(v.b), c.a) / 255.0;
    }`;

  let state = null;      // { canvas, gl, progs, vao, texSrc, texA, texB, fboA, fboB, w, h }
  let failed = false;
  let lastErr = null;    // reason for the latch (ITER-069)

  function compileProgram(gl, fsSrc) {
    function sh(type, src) {
      const s = gl.createShader(type);
      gl.shaderSource(s, src); gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) {
        throw new Error('preprocessGL shader compile: ' + gl.getShaderInfoLog(s));
      }
      return s;
    }
    const prog = gl.createProgram();
    gl.attachShader(prog, sh(gl.VERTEX_SHADER, VS));
    gl.attachShader(prog, sh(gl.FRAGMENT_SHADER, fsSrc));
    gl.linkProgram(prog);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) {
      throw new Error('preprocessGL link: ' + gl.getProgramInfoLog(prog));
    }
    const u = {};
    const n = gl.getProgramParameter(prog, gl.ACTIVE_UNIFORMS);
    for (let i = 0; i < n; i++) {
      const name = gl.getActiveUniform(prog, i).name;
      u[name] = gl.getUniformLocation(prog, name);
    }
    return { prog: prog, u: u };
  }

  function makeTex(gl, w, h) {
    const tex = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    if (w > 0) gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, w, h, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
    return tex;
  }

  function init() {
    const canvas = document.createElement('canvas');
    const gl = canvas.getContext('webgl2', { antialias: false, alpha: true, premultipliedAlpha: false,
      preserveDrawingBuffer: true, depth: false, stencil: false });
    if (!gl) return null;
    const progs = [compileProgram(gl, FS1), compileProgram(gl, FS2), compileProgram(gl, FS3)];
    return {
      canvas: canvas, gl: gl, progs: progs, vao: gl.createVertexArray(),
      texSrc: makeTex(gl, 0, 0), texA: null, texB: null, fboA: null, fboB: null, w: 0, h: 0,
    };
  }

  function ensureSize(s, w, h) {
    if (s.w === w && s.h === h) return;
    const gl = s.gl;
    if (s.texA) { gl.deleteTexture(s.texA); gl.deleteTexture(s.texB); gl.deleteFramebuffer(s.fboA); gl.deleteFramebuffer(s.fboB); }
    s.canvas.width = w; s.canvas.height = h;
    s.texA = makeTex(gl, w, h); s.texB = makeTex(gl, w, h);
    s.fboA = gl.createFramebuffer();
    gl.bindFramebuffer(gl.FRAMEBUFFER, s.fboA);
    gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, s.texA, 0);
    s.fboB = gl.createFramebuffer();
    gl.bindFramebuffer(gl.FRAMEBUFFER, s.fboB);
    gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, s.texB, 0);
    if (gl.checkFramebufferStatus(gl.FRAMEBUFFER) !== gl.FRAMEBUFFER_COMPLETE) {
      throw new Error('preprocessGL: RGBA8 framebuffer incomplete');
    }
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    s.w = w; s.h = h;
  }

  function pass(s, p, inTex, fbo, flipOut, setUniforms) {
    const gl = s.gl;
    gl.bindFramebuffer(gl.FRAMEBUFFER, fbo);
    gl.viewport(0, 0, s.w, s.h);
    gl.useProgram(p.prog);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, inTex);
    gl.uniform1i(p.u.uSrc, 0);
    gl.uniform2i(p.u.uSize, s.w, s.h);
    gl.uniform1i(p.u.uFlipOut, flipOut ? 1 : 0);
    setUniforms(gl, p.u);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }

  /* geo: { faceY1, tipX, tipY, ux, uy, tLo, tHi, bandHalf, bandFlat,
            chipRadius, handX, handY, handRx, handRy, handFlatR } */
  function run(srcCanvas, w, h, geo) {
    if (failed) return null;
    try {
      if (!state) {
        state = init();
        if (!state) { failed = true; lastErr = 'WebGL2 unavailable'; return null; }
      }
      const s = state, gl = s.gl;
      if (gl.isContextLost()) { failed = true; lastErr = 'WebGL2 context lost'; return null; }
      ensureSize(s, w, h);
      gl.disable(gl.BLEND);
      gl.disable(gl.DITHER);
      gl.bindVertexArray(s.vao);
      gl.activeTexture(gl.TEXTURE0);
      gl.bindTexture(gl.TEXTURE_2D, s.texSrc);
      gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, false);
      gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);
      gl.pixelStorei(gl.UNPACK_COLORSPACE_CONVERSION_WEBGL, gl.NONE);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, gl.RGBA, gl.UNSIGNED_BYTE, srcCanvas);
      const face = function (gl2, u) { gl2.uniform1i(u.uFaceY1, geo.faceY1 | 0); };
      pass(s, s.progs[0], s.texSrc, s.fboA, false, face);
      pass(s, s.progs[1], s.texA, s.fboB, false, face);
      pass(s, s.progs[2], s.texB, null, true, function (gl2, u) {
        gl2.uniform2f(u.uTip, geo.tipX, geo.tipY);
        gl2.uniform2f(u.uAxisU, geo.ux, geo.uy);
        gl2.uniform2f(u.uTRange, geo.tLo, geo.tHi);
        gl2.uniform3f(u.uBand, geo.bandHalf, geo.bandFlat, geo.chipRadius);
        gl2.uniform2f(u.uHand, geo.handX, geo.handY);
        gl2.uniform3f(u.uHandR, geo.handRx, geo.handRy, geo.handFlatR);
      });
      gl.bindVertexArray(null);
      return s.canvas;
    } catch (e) {
      console.warn('glyph-preprocess-gl: WebGL2 preprocess failed, latching the JS path:', e);
      failed = true;
      lastErr = String((e && e.message) || e);
      return null;
    }
  }

  const root = (typeof window !== 'undefined') ? window : globalThis;
  root.GlyphGrid = root.GlyphGrid || {};
  root.GlyphGrid.preprocessGL = Object.freeze({
    run: run,
    /* true until a GL failure latches the JS path for the session */
    available: function () { return !failed; },
    /* ITER-069: why it latched (null while healthy) — forwarded to cli_log */
    lastError: function () { return lastErr; },
  });
})();
