/* glyph-text-source.js — R3 (ITER-076): kinetic typography as a source.
 *
 *   CONFIG.textSource = {
 *     enabled: true, w: 1024, h: 512, bg: '#000000', fg: '#ffffff',
 *     blend: 'replace',            // replace | multiply | screen | mask
 *     // block-level defaults, inherited by every line that omits them:
 *     font: 'Cascadia Mono', size: 160, weight: 700, align: 'center',
 *     x: 0.5, y: 0.5, anim: 'none', easeIn: 0.3, easeOut: 0.2,
 *     lines: [{ text: 'DROP', start: 0, end: null, anim: 'typewriter' }],
 *   }
 *
 * Times are seconds of animation time; `end` null/absent = until the loop
 * ends.  `size` is px at the text canvas height `h`; x / y are 0..1 of the
 * canvas (safe-area friendly).  `text` may hold '\n' for stacked rows.
 *
 * Anim envelopes (pIn ramps 0→1 over easeIn from start, pOut 1→0 over
 * easeOut before a finite end; both quantised to 1/255 so a static title
 * is the same state every frame and keeps the layer memo):
 *   none        hard cut in/out
 *   typewriter  floor(len × pIn) characters, fade out on pOut
 *   fade        alpha = easeOutCubic(pIn) × pOut
 *   rise        fade + slides up from 0.35 × size below
 *   scale       fade + grows from 0.4× to 1×
 *
 * Pure except drawOps() (needs a CanvasRenderingContext2D-shaped object)
 * and compositeLinear() (typed arrays).  The host (index.html) owns the
 * offscreen canvas and the FrameSource adapter (kind 'text').
 *
 * API:
 *   GlyphGrid.text.parseSRT(text)  → [{text, start, end}]   (throws on garbage)
 *   GlyphGrid.text.parseLRC(text)  → [{text, start, end}]
 *   GlyphGrid.text.parseCues(text, nameHint) → lines (SRT or LRC by hint/content)
 *   GlyphGrid.text.lastEnd(lines)  → max finite end (0 = none)
 *   GlyphGrid.text.layout(ts, t, w, h) → draw ops for time t on a w×h canvas
 *   GlyphGrid.text.isDynamic(ts)   → can the rendered state change over the loop
 *   GlyphGrid.text.drawOps(ctx, ops, fill)
 *   GlyphGrid.text.compositeLinear(base, mask, blend, fg, bg) (in place, RGBA)
 *   GlyphGrid.text.minStrokeCells(ts, textW, textH, canvasW, cols)
 */

(function () {
  'use strict';

  const root = (typeof window !== 'undefined') ? window
             : (typeof globalThis !== 'undefined') ? globalThis
             : this;

  /* Orders are the schema enums (scripts/gen-schema.js reads them). */
  const ANIMS = ['none', 'typewriter', 'rise', 'fade', 'scale'];
  const BLENDS = ['replace', 'multiply', 'screen', 'mask'];
  const ALIGNS = ['left', 'center', 'right'];
  /* Display name → bundled @font-face family (glyph-fonts.js). */
  const FONT_ALIASES = { 'Cascadia Mono': 'LoveSparkGlyphGrid', 'IBM VGA8': 'PxPlusIBMVGA8' };
  const BUNDLED_FONTS = Object.keys(FONT_ALIASES);
  /* Offered by the GUI font dropdown when document.fonts.check() says the
     system has them.  Headless renders should prefer the bundled faces —
     a system font is whatever that machine has. */
  const SYSTEM_FONTS = ['Helvetica Neue', 'Helvetica', 'Arial', 'Georgia', 'Times New Roman',
    'Futura', 'Avenir Next', 'Gill Sans', 'Impact', 'Menlo', 'Courier New', 'Hiragino Sans', 'PingFang SC'];
  const LINE_KEYS = ['font', 'size', 'weight', 'align', 'x', 'y', 'anim', 'easeIn', 'easeOut'];
  const DEFAULTS = Object.freeze({
    font: 'Cascadia Mono', size: 160, weight: 700, align: 'center',
    x: 0.5, y: 0.5, anim: 'none', easeIn: 0.3, easeOut: 0.2,
  });
  const LINE_HEIGHT = 1.15;
  /* Stroke width / em, rough: ~0.14 for bold sans/mono stems, ~0.09 regular. */
  const STROKE_EM_BOLD = 0.14, STROKE_EM_REGULAR = 0.09;
  /* Below this a stem is narrower than one glyph cell and tends to dissolve
     into the background.  UNCERTAIN: chosen, not measured with the PSNR
     harness on a text fixture yet. */
  const MIN_STROKE_CELLS = 1;
  /* LRC: the last cue has no successor; hold it this long unless a
     [length:] tag says otherwise. */
  const LRC_LAST_HOLD_S = 4;

  /* ---------------- parsers ---------------- */

  function stripBom(s) { return String(s == null ? '' : s).replace(/^﻿/, ''); }

  /* "01:02:03,456" | "01:02:03.456" | "02:03,4" → seconds. */
  function srtTime(s) {
    const m = /^\s*(?:(\d+):)?(\d{1,2}):(\d{1,2})(?:[,.](\d{1,3}))?\s*$/.exec(s);
    if (!m) return NaN;
    const ms = m[4] ? Number((m[4] + '00').slice(0, 3)) : 0;
    return (Number(m[1] || 0) * 3600) + Number(m[2]) * 60 + Number(m[3]) + ms / 1000;
  }
  function round3(v) { return Math.round(v * 1000) / 1000; }

  function parseSRT(text) {
    const src = stripBom(text).replace(/\r\n?/g, '\n');
    const out = [];
    for (const block of src.split(/\n\s*\n/)) {
      const rows = block.split('\n').filter(function (r) { return r.trim() !== ''; });
      if (!rows.length) continue;
      let i = 0;
      if (/^\s*\d+\s*$/.test(rows[0]) && rows.length > 1 && rows[1].indexOf('-->') >= 0) i = 1;
      const tm = rows[i] && /^(.*?)-->(.*?)(?:\s+[A-Za-z].*)?$/.exec(rows[i]);
      if (!tm) continue;
      const start = srtTime(tm[1]), end = srtTime(tm[2]);
      if (!isFinite(start) || !isFinite(end)) continue;
      const body = rows.slice(i + 1).join('\n')
        .replace(/<[^>]*>/g, '')          // <i>, <b>, <font ...>
        .replace(/\{\\[^}]*\}/g, '');     // {\an8} ASS overrides
      if (!body.trim()) continue;
      out.push({ text: body, start: round3(start), end: round3(Math.max(start, end)) });
    }
    if (!out.length) throw new Error('SRT: no cues found');
    out.sort(function (a, b) { return a.start - b.start; });
    return out;
  }

  /* [mm:ss.xx] / [mm:ss:xx] / [mm:ss] tags; several tags per row repeat the
     row; [offset:±ms] shifts every tag (+ = earlier, per the LRC spec);
     [length:m:ss] ends the last cue; <mm:ss.xx> word tags are stripped;
     an empty timed row ends the previous cue without a new one. */
  function parseLRC(text) {
    const src = stripBom(text).replace(/\r\n?/g, '\n');
    let offset = 0, length = NaN;
    const stamps = [];
    const tagRe = /\[(\d{1,3}):(\d{1,2})(?:[.:](\d{1,3}))?\]/g;
    for (const row of src.split('\n')) {
      const meta = /^\s*\[(offset|length)\s*:\s*([^\]]*)\]\s*$/i.exec(row);
      if (meta) {
        if (meta[1].toLowerCase() === 'offset') offset = Number(meta[2]) || 0;
        else length = srtTime(meta[2].indexOf(':') >= 0 ? meta[2] : '0:' + meta[2]);
        continue;
      }
      tagRe.lastIndex = 0;
      const times = [];
      let m, lastEnd = 0;
      while ((m = tagRe.exec(row)) && m.index === lastEnd) {
        const frac = m[3] ? Number(m[3]) / Math.pow(10, m[3].length) : 0;
        times.push(Number(m[1]) * 60 + Number(m[2]) + frac);
        lastEnd = tagRe.lastIndex;
      }
      if (!times.length) continue;
      const body = row.slice(lastEnd).replace(/<\d{1,3}:\d{1,2}(?:[.:]\d{1,3})?>/g, '').trim();
      for (const t of times) stamps.push({ t: Math.max(0, t - offset / 1000), text: body });
    }
    stamps.sort(function (a, b) { return a.t - b.t; });
    const out = [];
    for (let i = 0; i < stamps.length; i++) {
      if (!stamps[i].text) continue;
      const next = i + 1 < stamps.length ? stamps[i + 1].t
        : (isFinite(length) && length > stamps[i].t ? length : stamps[i].t + LRC_LAST_HOLD_S);
      out.push({ text: stamps[i].text, start: round3(stamps[i].t), end: round3(next) });
    }
    if (!out.length) throw new Error('LRC: no timed lyric rows found');
    return out;
  }

  function parseCues(text, nameHint) {
    const hint = String(nameHint || '').toLowerCase();
    if (/\.lrc$/.test(hint)) return parseLRC(text);
    if (/\.srt$/.test(hint)) return parseSRT(text);
    return /-->/.test(String(text)) ? parseSRT(text) : parseLRC(text);
  }

  function lastEnd(lines) {
    let e = 0;
    for (const l of (lines || [])) {
      const v = l && l.end;
      if (typeof v === 'number' && isFinite(v) && v > e) e = v;
    }
    return e;
  }

  /* ---------------- layout ---------------- */

  function num(v, d) { return (typeof v === 'number' && isFinite(v)) ? v : d; }
  function clamp01(v) { return v < 0 ? 0 : v > 1 ? 1 : v; }
  function q255(v) { return Math.round(clamp01(v) * 255) / 255; }
  function easeOutCubic(k) { const u = 1 - k; return 1 - u * u * u; }
  function r2(v) { return Math.round(v * 100) / 100; }

  function resolveLine(line, ts) {
    const r = {};
    for (const k of LINE_KEYS) {
      r[k] = (line && line[k] !== undefined && line[k] !== null) ? line[k]
        : (ts && ts[k] !== undefined && ts[k] !== null) ? ts[k] : DEFAULTS[k];
    }
    if (ANIMS.indexOf(r.anim) < 0) r.anim = 'none';
    if (ALIGNS.indexOf(r.align) < 0) r.align = 'center';
    r.text = (line && line.text != null) ? String(line.text) : '';
    r.start = num(line && line.start, 0);
    r.end = num(line && line.end, Infinity);
    return r;
  }

  function fontFamilyCss(font) {
    const f = String(font || DEFAULTS.font);
    if (FONT_ALIASES[f]) return '"' + FONT_ALIASES[f] + '", ui-monospace, Menlo, monospace';
    return '"' + f.replace(/"/g, '') + '", system-ui, -apple-system, sans-serif';
  }
  function fontSpec(weight, sizePx, font) {
    return (num(weight, 700) | 0) + ' ' + r2(sizePx) + 'px ' + fontFamilyCss(font);
  }

  /* Draw ops for time t (s) on a w×h canvas.  Every value that animates is
     derived from the 1/255-quantised envelopes, so JSON.stringify(ops) is
     the rendered-state key. */
  function layout(ts, t, w, h) {
    const ops = [];
    if (!ts || !Array.isArray(ts.lines)) return ops;
    const k = h / Math.max(1, num(ts.h, h));
    for (const line of ts.lines) {
      const r = resolveLine(line, ts);
      if (!r.text || t < r.start || t >= r.end) continue;
      const easeIn = Math.max(0, num(r.easeIn, 0)), easeOut = Math.max(0, num(r.easeOut, 0));
      const pIn = q255(easeIn > 0 ? (t - r.start) / easeIn : 1);
      const pOut = q255(isFinite(r.end) && easeOut > 0 ? (r.end - t) / easeOut : 1);
      const size = Math.max(1, num(r.size, DEFAULTS.size)) * k;
      let text = r.text, alpha = 1, dy = 0, scale = 1;
      if (r.anim === 'typewriter') {
        const chars = Array.from(r.text);
        text = chars.slice(0, Math.floor(chars.length * pIn)).join('');
        alpha = pOut;
      } else if (r.anim !== 'none') {
        const env = easeOutCubic(pIn);
        alpha = env * pOut;
        if (r.anim === 'rise') dy = (1 - env) * 0.35 * size;
        if (r.anim === 'scale') scale = 0.4 + 0.6 * env;
      }
      alpha = Math.round(alpha * 255) / 255;
      if (!text.replace(/\s/g, '') || alpha <= 0) continue;
      ops.push({
        text: text, full: r.text, font: fontSpec(r.weight, size, r.font), size: r2(size),
        align: r.align, x: r2(num(r.x, 0.5) * w), y: r2(num(r.y, 0.5) * h + dy),
        alpha: alpha, scale: Math.round(scale * 1000) / 1000,
      });
    }
    return ops;
  }

  function isDynamic(ts) {
    if (!ts || !Array.isArray(ts.lines)) return false;
    return ts.lines.some(function (line) {
      const r = resolveLine(line, ts);
      return r.anim !== 'none' || r.start > 0 || isFinite(r.end);
    });
  }

  /* fillText per row; a partially typed row stays anchored where the full
     row sits, so a centred typewriter does not drift left as it types. */
  function drawOps(ctx, ops, fill) {
    for (const op of ops) {
      ctx.save();
      ctx.globalAlpha = op.alpha;
      ctx.fillStyle = fill;
      ctx.font = op.font;
      ctx.textBaseline = 'middle';
      ctx.translate(op.x, op.y);
      if (op.scale !== 1) ctx.scale(op.scale, op.scale);
      const rows = op.text.split('\n');
      const full = op.full.split('\n');
      const lh = op.size * LINE_HEIGHT;
      const y0 = -(full.length - 1) * lh / 2;
      for (let i = 0; i < rows.length; i++) {
        if (rows[i] === full[i]) {
          ctx.textAlign = op.align;
          ctx.fillText(rows[i], 0, y0 + i * lh);
        } else {
          const fw = ctx.measureText(full[i]).width;
          ctx.textAlign = 'left';
          ctx.fillText(rows[i], op.align === 'left' ? 0 : op.align === 'right' ? -fw : -fw / 2, y0 + i * lh);
        }
      }
      ctx.restore();
    }
  }

  /* ---------------- linear-light composite ---------------- */

  let _toLin = null, _toSrgb = null;
  function luts() {
    if (_toLin) return;
    _toLin = new Float32Array(256);
    for (let i = 0; i < 256; i++) {
      const c = i / 255;
      _toLin[i] = c <= 0.04045 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
    }
    _toSrgb = new Uint8Array(4096);
    for (let i = 0; i < 4096; i++) {
      const l = i / 4095;
      const c = l <= 0.0031308 ? l * 12.92 : 1.055 * Math.pow(l, 1 / 2.4) - 0.055;
      _toSrgb[i] = Math.max(0, Math.min(255, Math.round(c * 255)));
    }
  }
  function hexRgb(hex) {
    const m = /^#?([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(String(hex || '').trim());
    if (!m) return [255, 255, 255];
    let s = m[1];
    if (s.length === 3) s = s[0] + s[0] + s[1] + s[1] + s[2] + s[2];
    return [parseInt(s.slice(0, 2), 16), parseInt(s.slice(2, 4), 16), parseInt(s.slice(4, 6), 16)];
  }

  /* base: RGBA bytes of the source (rewritten in place); mask: RGBA bytes
     whose alpha is the text coverage.  Linear light so blend edges do not
     halo against linearizeToLuminance.  fg / bg: [r,g,b] 0..255.
       multiply  base × (1 − a + a·fg)       text darkens (fg tints)
       screen    1 − (1 − base)(1 − a·fg)    text lightens
       mask      base·a + bg·(1 − a)         the image shows through the letters */
  function compositeLinear(base, mask, blend, fg, bg) {
    luts();
    const fl = [_toLin[fg[0]], _toLin[fg[1]], _toLin[fg[2]]];
    const bl = [_toLin[bg[0]], _toLin[bg[1]], _toLin[bg[2]]];
    const n = base.length;
    for (let i = 0; i < n; i += 4) {
      const a = mask[i + 3] / 255;
      if (a === 0 && blend !== 'mask') continue;
      for (let c = 0; c < 3; c++) {
        const b = _toLin[base[i + c]];
        let o;
        if (blend === 'multiply') o = b * (1 - a + a * fl[c]);
        else if (blend === 'screen') o = 1 - (1 - b) * (1 - a * fl[c]);
        else o = b * a + bl[c] * (1 - a);
        base[i + c] = _toSrgb[Math.round(clamp01(o) * 4095)];
      }
      base[i + 3] = 255;
    }
    return base;
  }

  /* Smallest estimated stem width over all lines, in glyph cells of the
     main canvas (cellW = canvasW / cols; the text canvas is fitted to the
     main canvas, so text px scale by canvasW / textW).  Infinity = no text. */
  function minStrokeCells(ts, textW, textH, canvasW, cols) {
    if (!ts || !Array.isArray(ts.lines) || !ts.lines.length) return Infinity;
    const k = textH / Math.max(1, num(ts.h, textH));
    const toCanvas = canvasW / Math.max(1, textW);
    const cellW = canvasW / Math.max(1, cols);
    let min = Infinity;
    for (const line of ts.lines) {
      const r = resolveLine(line, ts);
      if (!r.text) continue;
      const em = num(r.weight, 700) >= 600 ? STROKE_EM_BOLD : STROKE_EM_REGULAR;
      const cells = Math.max(1, num(r.size, DEFAULTS.size)) * k * em * toCanvas / cellW;
      if (cells < min) min = cells;
    }
    return min;
  }

  const api = Object.freeze({
    ANIMS: ANIMS,
    BLENDS: BLENDS,
    ALIGNS: ALIGNS,
    DEFAULTS: DEFAULTS,
    FONT_ALIASES: FONT_ALIASES,
    BUNDLED_FONTS: BUNDLED_FONTS,
    SYSTEM_FONTS: SYSTEM_FONTS,
    MIN_STROKE_CELLS: MIN_STROKE_CELLS,
    parseSRT: parseSRT,
    parseLRC: parseLRC,
    parseCues: parseCues,
    lastEnd: lastEnd,
    resolveLine: resolveLine,
    fontSpec: fontSpec,
    layout: layout,
    isDynamic: isDynamic,
    drawOps: drawOps,
    hexRgb: hexRgb,
    compositeLinear: compositeLinear,
    minStrokeCells: minStrokeCells,
  });

  root.GlyphGrid = root.GlyphGrid || {};
  root.GlyphGrid.text = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})();
