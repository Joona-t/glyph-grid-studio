/* glyph-text-export.js — the grid as text: TXT, 24-bit ANSI, asciinema v2
   .cast and a self-contained HTML player (Phase 5.1, research R2, ITER-074).

   Pure: no p5, no DOM, no Tauri.  Input is a GlyphFrame snapshot (a copy of
   the IR every drawer fills, ITER-073):
     { cols, rows, cp: Uint32Array(N), fg: Uint8Array(N*3), alpha: Uint8Array(N), bg: '#RRGGBB' }
   cp 0 = nothing drawn.  Ink colours are alpha-composited over bg once here
   (what the canvas shows), so the writers only ever see opaque rgb.  Output
   is a deterministic function of the snapshots: no timestamps, no Math.random.

   Loaded by index.html (window.GlyphGrid.textExport) and by the node unit
   test (module.exports). */

(function () {
  'use strict';

  const ESC = '\x1b[';

  /* Copy the live IR (grow-only buffers, overwritten next draw) into a
     self-contained snapshot. */
  function snapFromIr(f) {
    const n = f.cols * f.rows;
    return {
      cols: f.cols, rows: f.rows,
      cp: f.cp.slice(0, n), fg: f.fg.slice(0, n * 3), alpha: f.alpha.slice(0, n),
      bg: f.bgHex || '#000000',
    };
  }

  function parseHex(hex) {
    const m = /^#?([0-9a-f]{6})$/i.exec(String(hex || '').trim());
    const v = m ? parseInt(m[1], 16) : 0;
    return [(v >> 16) & 255, (v >> 8) & 255, v & 255];
  }

  /* Blank = nothing visible to print: no glyph, a space, a control char,
     or fully transparent ink. */
  function isBlank(snap, i) {
    const c = snap.cp[i];
    return c <= 0x20 || (c >= 0x7f && c <= 0x9f) || snap.alpha[i] === 0;
  }

  function cellChar(snap, i) {
    return isBlank(snap, i) ? ' ' : String.fromCodePoint(snap.cp[i]);
  }

  /* fg over bg at the cell's alpha, packed 0xRRGGBB. */
  function cellRgb(snap, i, bgRgb) {
    const a = snap.alpha[i], o = i * 3;
    let r = snap.fg[o], g = snap.fg[o + 1], b = snap.fg[o + 2];
    if (a < 255) {
      r = Math.round(bgRgb[0] + (r - bgRgb[0]) * a / 255);
      g = Math.round(bgRgb[1] + (g - bgRgb[1]) * a / 255);
      b = Math.round(bgRgb[2] + (b - bgRgb[2]) * a / 255);
    }
    return (r << 16) | (g << 8) | b;
  }

  /* xterm-256 index for truecolor:false (6x6x6 cube or the 24-step grey ramp,
     whichever is nearer). */
  function xterm256(rgb) {
    const r = (rgb >> 16) & 255, g = (rgb >> 8) & 255, b = rgb & 255;
    const lv = [0, 95, 135, 175, 215, 255];
    const q = (v) => (v < 48 ? 0 : v < 115 ? 1 : Math.min(5, Math.floor((v - 35) / 40)));
    const qr = q(r), qg = q(g), qb = q(b);
    const cube = 16 + 36 * qr + 6 * qg + qb;
    const dc = (lv[qr] - r) ** 2 + (lv[qg] - g) ** 2 + (lv[qb] - b) ** 2;
    const avg = (r + g + b) / 3;
    const gi = Math.max(0, Math.min(23, Math.round((avg - 8) / 10)));
    const gv = 8 + gi * 10;
    const dg = (gv - r) ** 2 + (gv - g) ** 2 + (gv - b) ** 2;
    return dg < dc ? 232 + gi : cube;
  }

  function sgr(layer, rgb, truecolor) {
    /* layer 38 = fg, 48 = bg */
    if (!truecolor) return ESC + layer + ';5;' + xterm256(rgb) + 'm';
    return ESC + layer + ';2;' + ((rgb >> 16) & 255) + ';' + ((rgb >> 8) & 255) + ';' + (rgb & 255) + 'm';
  }

  /* ── TXT ── rows x cols characters, '\n'-terminated rows.  Not trimmed:
     the rectangle is the contract (a reader can index row/col directly). */
  function toTxt(snap) {
    const out = [];
    for (let r = 0; r < snap.rows; r++) {
      let line = '';
      for (let c = 0; c < snap.cols; c++) line += cellChar(snap, r * snap.cols + c);
      out.push(line);
    }
    return out.join('\n') + '\n';
  }

  /* One SGR-coloured string per row.  The palette bg is set once at the row
     start and never changes; with rle a new fg escape is only emitted when
     the next INKED cell's colour differs (blanks print as bg-only spaces and
     never break a run).  Each row ends with a reset, so rows are
     independent — the .cast writer redraws single rows. */
  function ansiRows(snap, opts) {
    const truecolor = !opts || opts.truecolor !== false;
    const rle = !opts || opts.rle !== false;
    const bgRgb = parseHex(snap.bg);
    const bgSgr = sgr(48, (bgRgb[0] << 16) | (bgRgb[1] << 8) | bgRgb[2], truecolor);
    const rows = [];
    for (let r = 0; r < snap.rows; r++) {
      let s = bgSgr;
      let cur = -1;
      for (let c = 0; c < snap.cols; c++) {
        const i = r * snap.cols + c;
        if (isBlank(snap, i)) { s += ' '; continue; }
        const rgb = cellRgb(snap, i, bgRgb);
        if (!rle || rgb !== cur) { s += sgr(38, rgb, truecolor); cur = rgb; }
        s += String.fromCodePoint(snap.cp[i]);
      }
      rows.push(s + ESC + '0m');
    }
    return rows;
  }

  /* ── ANSI ── `cat file.ans` in any truecolor terminal. */
  function toAnsi(snap, opts) {
    return ansiRows(snap, opts).join('\n') + '\n';
  }

  /* ── asciinema v2 .cast ── header line + [time, "o", data] events.
     Frame 0 clears the screen (and hides the cursor); every later frame
     redraws only the rows that changed, each behind an absolute cursor move,
     so a still source costs one frame and an animated one costs its diff.
     A final event at t = n/fps resets attributes, restores the cursor and
     makes the recording last the whole loop. */
  function toCastV2(frames, fps, opts) {
    opts = opts || {};
    const f0 = frames[0];
    const width = opts.width || (f0 && f0.cols) || 0;
    const height = opts.height || (f0 && f0.rows) || 0;
    const rate = fps > 0 ? fps : 24;
    const header = { version: 2, width: width, height: height, env: { TERM: 'xterm-256color' } };
    if (opts.title) header.title = String(opts.title);
    const lines = [JSON.stringify(header)];
    const t = (k) => Math.round(k / rate * 1e6) / 1e6;
    let prev = null;
    for (let k = 0; k < frames.length; k++) {
      const rows = ansiRows(frames[k], opts);
      let data = k === 0 ? ESC + '?25l' + ESC + '2J' : '';
      for (let r = 0; r < rows.length; r++) {
        if (prev && prev[r] === rows[r]) continue;
        data += ESC + (r + 1) + ';1H' + rows[r];
      }
      if (data) lines.push(JSON.stringify([t(k), 'o', data]));
      prev = rows;
    }
    lines.push(JSON.stringify([t(frames.length), 'o', ESC + '0m' + ESC + (height + 1) + ';1H' + ESC + '?25h']));
    return lines.join('\n') + '\n';
  }

  function htmlEsc(s) {
    return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  }

  function hex6(rgb) {
    return '#' + (0x1000000 + rgb).toString(16).slice(1);
  }

  /* One frame as <pre> inner HTML: rows of colour runs, blanks as spaces. */
  function frameHtml(snap) {
    const bgRgb = parseHex(snap.bg);
    const rows = [];
    for (let r = 0; r < snap.rows; r++) {
      let s = '', run = '', cur = -1;
      for (let c = 0; c < snap.cols; c++) {
        const i = r * snap.cols + c;
        if (isBlank(snap, i)) { run += ' '; continue; }
        const rgb = cellRgb(snap, i, bgRgb);
        if (rgb !== cur) {
          if (run) s += cur < 0 ? run : '<span style="color:' + hex6(cur) + '">' + htmlEsc(run) + '</span>';
          run = ''; cur = rgb;
        }
        run += String.fromCodePoint(snap.cp[i]);
      }
      if (run) s += cur < 0 ? run : '<span style="color:' + hex6(cur) + '">' + htmlEsc(run) + '</span>';
      rows.push(s);
    }
    return rows.join('\n');
  }

  /* Compact frame for the HTML player: the text (rows joined by '\n',
     blanks as spaces) plus flat runs [codepointCount, colourIndex, ...] over
     it; colourIndex -1 = uncoloured (leading blanks), else an index into
     the shared table.  A run only breaks at an inked cell whose colour
     differs — blanks and newlines never break one (same rule as the ANSI
     rle).  ~4x smaller than per-frame span markup on colourful frames. */
  function frameRuns(snap, table, index) {
    const bgRgb = parseHex(snap.bg);
    let text = '';
    const runs = [];
    let cur = -1, len = 0;
    for (let r = 0; r < snap.rows; r++) {
      if (r) { text += '\n'; len++; }
      for (let c = 0; c < snap.cols; c++) {
        const i = r * snap.cols + c;
        if (isBlank(snap, i)) { text += ' '; len++; continue; }
        const rgb = cellRgb(snap, i, bgRgb);
        let ci = index.get(rgb);
        if (ci === undefined) { ci = table.length; table.push(hex6(rgb).slice(1)); index.set(rgb, ci); }
        if (ci !== cur) {
          if (len) runs.push(len, cur);
          cur = ci; len = 0;
        }
        text += String.fromCodePoint(snap.cp[i]);
        len++;
      }
    }
    if (len) runs.push(len, cur);
    return [text, runs];
  }

  /* ── HTML ── one file: cells are real characters, the bundled WOFF2 is
     inlined as a data URI (fontBase64) so it renders off-machine, frame 0 is
     in the markup (works without JS), and a small stepper plays the rest.
     prefers-reduced-motion: no autoplay — the viewer steps or presses Play. */
  function toHtml(frames, opts) {
    opts = opts || {};
    const f0 = frames[0];
    const cols = f0.cols, rows = f0.rows;
    const fps = opts.fps > 0 ? opts.fps : 24;
    const title = htmlEsc(String(opts.title || 'Glyph Grid Studio'));
    const bgRgb = parseHex(f0.bg);
    const bg = '#' + bgRgb.map((v) => (v + 256).toString(16).slice(1)).join('');
    /* Control text: dark on a light palette bg, light on a dark one. */
    const lum = (0.2126 * bgRgb[0] + 0.7152 * bgRgb[1] + 0.0722 * bgRgb[2]) / 255;
    const ctlFg = lum > 0.5 ? '#1a1410' : '#f5f0e6';
    const fam = "'GlyphGridInline', ui-monospace, SFMono-Regular, Menlo, Consolas, monospace";
    const fontFace = (opts.inlineFont !== false && opts.fontBase64)
      ? "@font-face{font-family:'GlyphGridInline';src:url(data:font/woff2;base64," + opts.fontBase64 +
        ") format('woff2');font-display:block}\n"
      : '';
    const multi = frames.length > 1;
    let data = '';
    if (multi) {
      const table = [], index = new Map();
      const enc = frames.map((f) => frameRuns(f, table, index));
      /* JSON inside <script>: break any "</" so frame text cannot close it. */
      data = JSON.stringify({ c: table, f: enc }).replace(/<\//g, '<\\/');
    }
    const parts = [
      '<!doctype html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n',
      '<meta name="viewport" content="width=device-width, initial-scale=1">\n',
      '<meta name="generator" content="Glyph Grid Studio">\n',
      '<title>' + title + '</title>\n<style>\n', fontFace,
      ':root{--bg:' + bg + ';--fg:' + ctlFg + '}\n',
      'html,body{margin:0;background:var(--bg)}\n',
      'body{display:flex;flex-direction:column;align-items:center;padding:16px;box-sizing:border-box;min-height:100vh}\n',
      'pre{margin:0;font-family:' + fam + ';line-height:1;white-space:pre;',
      'font-size:min(14px,calc((100vw - 32px) / ' + cols + ' / 0.6));color:var(--fg)}\n',
      '.ctl{display:flex;gap:8px;align-items:center;margin-top:12px;font:13px ui-monospace,Menlo,monospace;color:var(--fg)}\n',
      '.ctl button{min-width:64px;min-height:32px;font:inherit;background:transparent;color:inherit;',
      'border:1px solid currentColor;border-radius:6px;cursor:pointer}\n',
      '.ctl button:focus-visible{outline:2px solid currentColor;outline-offset:2px}\n',
      '</style>\n</head>\n<body>\n',
      '<pre id="g" role="img" aria-label="Glyph grid, ' + cols + ' by ' + rows + ' characters' +
        (multi ? ', frame 1 of ' + frames.length : '') + '">', frameHtml(f0), '</pre>\n',
    ];
    if (multi) {
      parts.push(
        '<div class="ctl"><button id="play" type="button">Pause</button>',
        '<button id="step" type="button">Step</button>',
        '<span id="n" aria-live="polite">1 / ' + frames.length + '</span></div>\n',
        '<script type="application/json" id="frames">', data, '</script>\n',
        '<script>\n(function(){\n',
        "var D=JSON.parse(document.getElementById('frames').textContent),F=D.f,C=D.c,g=document.getElementById('g'),",
        "n=document.getElementById('n'),pb=document.getElementById('play'),i=0,timer=null,ms=" + (1000 / fps).toFixed(3) + ";\n",
        "var label=g.getAttribute('aria-label').replace(/, frame .*$/,'');\n",
        "function esc(t){return t.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');}\n",
        /* frame k -> markup: walk the runs over the text's codepoints */
        "function html(k){var t=Array.from(F[k][0]),r=F[k][1],o='',p=0;for(var j=0;j<r.length;j+=2){",
        "var s=esc(t.slice(p,p+r[j]).join(''));p+=r[j];o+=r[j+1]<0?s:'<span style=\"color:#'+C[r[j+1]]+'\">'+s+'</span>';}return o;}\n",
        "function show(k){i=(k+F.length)%F.length;g.innerHTML=html(i);n.textContent=(i+1)+' / '+F.length;",
        "g.setAttribute('aria-label',label+', frame '+(i+1)+' of '+F.length);}\n",
        "function stop(){if(timer){clearInterval(timer);timer=null;}pb.textContent='Play';}\n",
        "function play(){stop();timer=setInterval(function(){show(i+1);},ms);pb.textContent='Pause';}\n",
        "pb.onclick=function(){timer?stop():play();};\n",
        "document.getElementById('step').onclick=function(){stop();show(i+1);};\n",
        "var rm=window.matchMedia&&matchMedia('(prefers-reduced-motion: reduce)');\n",
        'if(rm&&rm.matches)stop();else play();\n',
        "if(rm&&rm.addEventListener)rm.addEventListener('change',function(e){if(e.matches)stop();});\n",
        '})();\n</script>\n'
      );
    }
    parts.push('</body>\n</html>\n');
    return parts.join('');
  }

  const FORMATS = Object.freeze(['txt', 'ansi', 'cast', 'html']);
  /* Formats that are one still frame (frame 0) regardless of --frames. */
  const STILL_FORMATS = Object.freeze(['txt', 'ansi']);

  /* Dispatch used by the batch runner and the GUI.  frames: snapshots. */
  function build(format, frames, opts) {
    opts = opts || {};
    switch (format) {
      case 'txt': return toTxt(frames[0]);
      case 'ansi': return toAnsi(frames[0], { truecolor: true, rle: true });
      case 'cast': return toCastV2(frames, opts.fps, {
        width: frames[0].cols, height: frames[0].rows, title: opts.title, truecolor: true, rle: true,
      });
      case 'html': return toHtml(frames, {
        inlineFont: true, fontBase64: opts.fontBase64, fps: opts.fps, title: opts.title,
      });
      default: throw new Error('unknown text export format: ' + format);
    }
  }

  const api = Object.freeze({
    FORMATS: FORMATS,
    STILL_FORMATS: STILL_FORMATS,
    snapFromIr: snapFromIr,
    toTxt: toTxt,
    toAnsi: toAnsi,
    toCastV2: toCastV2,
    toHtml: toHtml,
    build: build,
  });

  const root = (typeof window !== 'undefined') ? window
             : (typeof globalThis !== 'undefined') ? globalThis
             : this;
  root.GlyphGrid = root.GlyphGrid || {};
  root.GlyphGrid.textExport = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})();
