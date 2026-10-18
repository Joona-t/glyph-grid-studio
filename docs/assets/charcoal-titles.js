/* Charcoal titles — the large headings drawn by the charcoal-sketch skill.
 *
 * Each [data-ct] heading keeps its real text (screen readers, search, no-JS
 * and failed loads all get plain type). Three layers, each only on success:
 *
 *   text   always in the DOM; visually hidden once a charcoal image shows
 *   still  assets/titles/<name>-<theme>-<layout>.webp  → .ct-on
 *   motion assets/titles/<name>-<theme>-<layout>.mp4   → .ct-anim
 *          the skill's seamless 4 s breathing loop. H.264 has no alpha, so the
 *          marks are baked onto white (cream, candy: mix-blend-mode multiply)
 *          or black (basalt: screen) and the blend drops the paper out.
 *          Created only for titles near the viewport, paused off screen, never
 *          under prefers-reduced-motion (the still stays).
 *
 * layout = mob (≤520 px) or desk. Sizes come from titles.js in em of the
 * heading's own font, so the art scales with the CSS clamp() like the text.
 */
(function () {
  'use strict';
  var M = window.CHARCOAL_TITLES;
  if (!M || !document.querySelectorAll) return;
  var root = document.documentElement;
  var mq = window.matchMedia('(max-width: 520px)');
  var reduce = window.matchMedia('(prefers-reduced-motion: reduce)');
  var THEMES = ['cream', 'candy', 'basalt'];
  var heads = [].slice.call(document.querySelectorAll('[data-ct]'));
  var near = new WeakMap();   // heading → within ~1 screen of the viewport

  function theme() { var t = root.getAttribute('data-theme'); return THEMES.indexOf(t) >= 0 ? t : 'basalt'; }
  function layout() { return mq.matches ? 'mob' : 'desk'; }
  function base(name, t, l) { return 'assets/titles/' + name + '-' + t + '-' + l; }
  function size(el, d) { el.style.width = d.w + 'em'; el.style.margin = (-d.pt) + 'em auto ' + (-d.pb) + 'em'; }

  function dropVideo(h) {
    var v = h.querySelector('video.ct-vid');
    h.classList.remove('ct-anim');
    if (v) { v.pause(); v.removeAttribute('src'); v.load(); v.remove(); }
  }

  // motion layer: only when the still is up, motion is allowed and the title is near
  function motion(h) {
    var key = h.getAttribute('data-ct-key'), d = h._ctData;
    var v = h.querySelector('video.ct-vid');
    if (!d || reduce.matches || !h.classList.contains('ct-on')) { dropVideo(h); return; }
    if (v && v.getAttribute('data-key') !== key) { dropVideo(h); v = null; }
    if (!near.get(h)) { if (v) v.pause(); return; }
    if (!v) {
      v = document.createElement('video');
      v.className = 'ct-vid';
      v.setAttribute('data-key', key);
      v.setAttribute('aria-hidden', 'true');
      v.muted = true; v.loop = true; v.playsInline = true;
      v.setAttribute('muted', ''); v.setAttribute('playsinline', '');
      v.preload = 'auto';
      size(v, d);
      v.addEventListener('playing', function () {
        if (v.getAttribute('data-key') === h.getAttribute('data-ct-key')) h.classList.add('ct-anim');
      });
      v.addEventListener('error', function () { dropVideo(h); });   // still stays up
      v.src = base(key.split('-')[0], key.split('-')[1], key.split('-')[2]) + '.mp4';
      h.appendChild(v);
    }
    var p = v.play();
    if (p && p.catch) p.catch(function () { /* autoplay refused: keep the still */ });
  }

  function sync() {
    var t = theme(), l = layout();
    heads.forEach(function (h) {
      var name = h.getAttribute('data-ct');
      var d = M[name] && M[name][t] && M[name][t][l];
      var key = name + '-' + t + '-' + l;
      if (h.getAttribute('data-ct-key') === key) { motion(h); return; }
      h.setAttribute('data-ct-key', key);
      h._ctData = d;
      dropVideo(h);
      if (!d) { h.classList.remove('ct-on'); return; }
      var img = h.querySelector('img.ct-img');
      if (!img) {
        img = document.createElement('img');
        img.className = 'ct-img';
        img.alt = '';
        img.setAttribute('aria-hidden', 'true');
        img.decoding = 'async';
        h.insertBefore(img, h.firstChild.nextSibling);
      }
      img.onload = function () {
        if (h.getAttribute('data-ct-key') !== key) return;     // a newer theme/layout won
        size(img, d);
        h.classList.add('ct-on');
        motion(h);
      };
      img.onerror = function () { if (h.getAttribute('data-ct-key') === key) h.classList.remove('ct-on'); };
      img.src = base(name, t, l) + '.webp';
    });
  }

  // desktop: warm the other themes' stills once idle, so a theme switch swaps instantly
  function prefetch() {
    var c = navigator.connection;
    if (mq.matches || (c && c.saveData)) return;           // phones / Save-Data: load only what is shown
    var l = layout();
    heads.forEach(function (h) {
      var name = h.getAttribute('data-ct');
      THEMES.forEach(function (t) { if (M[name] && M[name][t] && M[name][t][l]) { var i = new Image(); i.src = base(name, t, l) + '.webp'; } });
    });
  }

  if ('IntersectionObserver' in window) {
    var io = new IntersectionObserver(function (es) {
      es.forEach(function (e) { near.set(e.target, e.isIntersecting); motion(e.target); });
    }, { rootMargin: '300px 0px' });
    heads.forEach(function (h) { io.observe(h); });
  } else {
    heads.forEach(function (h) { near.set(h, true); });
  }

  sync();
  new MutationObserver(sync).observe(root, { attributes: true, attributeFilter: ['data-theme'] });
  if (mq.addEventListener) mq.addEventListener('change', function () { sync(); prefetch(); });
  if (reduce.addEventListener) reduce.addEventListener('change', function () { heads.forEach(motion); });
  window.addEventListener('load', function () {
    (window.requestIdleCallback || function (f) { setTimeout(f, 800); })(prefetch);
  });
})();
