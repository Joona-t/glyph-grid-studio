/* Glyph field — the page background, drawn the way the studio draws.
 *
 * A fixed canvas behind the page holds a character grid of vertical-hatching
 * glyphs (strokes only, no dots). Each cell samples a slowly drifting value-noise field (stretched
 * vertically so it reads as strands of ink), adds a 4x4 Bayer threshold, and
 * picks a glyph from a density ramp: the same signal → dither → ramp pipeline
 * as the app, at ~12 fps.
 *
 *   - Glyphs are pre-rendered once into an atlas (one sprite per ramp step,
 *     in the theme's ink), so a frame is drawImage calls, never fillText.
 *   - Only cells whose glyph changed are repainted.
 *   - The field thins out behind the centre text column so copy stays clean.
 *   - Paused when the tab is hidden; one still frame under
 *     prefers-reduced-motion (no drift, no parallax).
 *   - Ink and strength come from CSS (--field-ink, --field-alpha), so the
 *     three themes tint it; a data-theme change re-tints the atlas.
 */
(function () {
  'use strict';
  var canvas = document.getElementById('glyphField');
  if (!canvas || !canvas.getContext) return;
  var ctx = canvas.getContext('2d', { alpha: true });
  if (!ctx) return;

  var RAMP = [' ', '╷', '┆', '╎', '¦', '│', '┃', '║'];   // strokes only, faintest first: hatching, never dots
  var CELL_W = 9, CELL_H = 15, FONT_PX = 13;      // CSS px
  var FPS = 12;
  var BAYER = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];

  var reduce = window.matchMedia('(prefers-reduced-motion: reduce)');
  var dpr = 1, W = 0, H = 0, cols = 0, rows = 0;
  var atlas = null, prev = null;
  var t0 = performance.now(), lastFrame = 0, raf = 0, scrollY = window.scrollY;

  // ---- value noise (hash lattice + smoothstep), two octaves ----------------
  function hash(x, y) {
    var h = (x * 374761393 + y * 668265263) | 0;
    h = Math.imul(h ^ (h >>> 13), 1274126177);
    return ((h ^ (h >>> 16)) >>> 0) / 4294967295;
  }
  function vnoise(x, y) {
    var xi = Math.floor(x), yi = Math.floor(y);
    var xf = x - xi, yf = y - yi;
    var u = xf * xf * (3 - 2 * xf), v = yf * yf * (3 - 2 * yf);
    var a = hash(xi, yi), b = hash(xi + 1, yi), c = hash(xi, yi + 1), d = hash(xi + 1, yi + 1);
    return a + (b - a) * u + (c - a) * v + (a - b - c + d) * u * v;
  }
  function field(x, y, t) {
    return 0.65 * vnoise(x * 0.11 + t * 0.06, y * 0.022 - t * 0.09)
         + 0.35 * vnoise(x * 0.31 - t * 0.04, y * 0.07 + t * 0.05);
  }
  function smooth(e0, e1, x) { var k = Math.min(1, Math.max(0, (x - e0) / (e1 - e0))); return k * k * (3 - 2 * k); }

  // ---- atlas: one sprite per ramp glyph, in the theme ink ------------------
  function buildAtlas() {
    var cs = getComputedStyle(document.documentElement);
    var ink = cs.getPropertyValue('--field-ink').trim() || '#5e3a24';
    var alpha = parseFloat(cs.getPropertyValue('--field-alpha')) || 0.1;
    var cw = Math.ceil(CELL_W * dpr), ch = Math.ceil(CELL_H * dpr);
    atlas = document.createElement('canvas');
    atlas.width = cw * RAMP.length; atlas.height = ch;
    var a = atlas.getContext('2d');
    a.font = (FONT_PX * dpr) + 'px ui-monospace, "SF Mono", Menlo, Consolas, monospace';
    a.textAlign = 'center'; a.textBaseline = 'middle';
    a.fillStyle = ink;
    for (var i = 1; i < RAMP.length; i++) {
      a.globalAlpha = alpha * (0.55 + 0.45 * i / (RAMP.length - 1));   // denser glyphs carry a touch more ink
      a.fillText(RAMP[i], i * cw + cw / 2, ch / 2);
    }
    prev = null;   // force a full repaint
  }

  function resize() {
    dpr = Math.min(window.devicePixelRatio || 1, 2);
    W = document.documentElement.clientWidth || window.innerWidth; H = window.innerHeight;   // clientWidth: excludes a classic scrollbar
    canvas.width = Math.ceil(W * dpr); canvas.height = Math.ceil(H * dpr);
    canvas.style.width = W + 'px'; canvas.style.height = H + 'px';
    cols = Math.ceil(W / CELL_W) + 1; rows = Math.ceil(H / CELL_H) + 1;
    buildAtlas();
  }

  function draw(now) {
    var still = reduce.matches;
    var t = still ? 0 : (now - t0) / 1000;
    var drift = still ? 0 : scrollY * 0.12 / CELL_H;     // gentle parallax, in rows
    var cw = Math.ceil(CELL_W * dpr), ch = Math.ceil(CELL_H * dpr);
    var n = cols * rows, full = !prev || prev.length !== n;
    if (full) { prev = new Uint8Array(n).fill(255); ctx.clearRect(0, 0, canvas.width, canvas.height); }
    var top = Math.floor(drift), frac = drift - top;
    // the text column: content is centred, max 1060 px
    var half = Math.min(530, W * 0.5), cx = W / 2;
    var floor = W < 760 ? 0.5 : 0.32;   // narrow screens: copy fills the width, keep a quiet field everywhere
    for (var r = 0; r < rows; r++) {
      var gy = r + top;
      for (var c = 0; c < cols; c++) {
        var px = c * CELL_W;
        var dist = Math.abs(px + CELL_W / 2 - cx);
        var mask = floor + (1 - floor) * smooth(half * 0.55, half * 1.08, dist);
        var s = field(c, gy + frac, t) * mask;
        s = s * 1.45 - 0.02 + (BAYER[(gy & 3) * 4 + (c & 3)] / 16 - 0.5) * 0.22;
        var g = Math.max(0, Math.min(RAMP.length - 1, Math.floor(s * RAMP.length)));
        var i = r * cols + c;
        if (g === prev[i]) continue;
        prev[i] = g;
        var x = Math.round(px * dpr), y = Math.round(r * CELL_H * dpr);
        if (!full) ctx.clearRect(x, y, cw, ch);
        if (g) ctx.drawImage(atlas, g * cw, 0, cw, ch, x, y, cw, ch);
      }
    }
  }

  function loop(now) {
    raf = 0;
    if (document.hidden || reduce.matches) return;
    if (now - lastFrame >= 1000 / FPS) { lastFrame = now; draw(now); }
    raf = requestAnimationFrame(loop);
  }
  function start() {
    draw(performance.now());
    if (!reduce.matches && !document.hidden && !raf) raf = requestAnimationFrame(loop);
  }

  resize(); start();
  canvas.classList.add('on');

  var rt = 0;
  window.addEventListener('resize', function () { clearTimeout(rt); rt = setTimeout(function () { resize(); start(); }, 120); });
  window.addEventListener('scroll', function () { scrollY = window.scrollY; }, { passive: true });
  document.addEventListener('visibilitychange', start);
  if (reduce.addEventListener) reduce.addEventListener('change', start);
  new MutationObserver(function () { buildAtlas(); start(); })
    .observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });
})();
