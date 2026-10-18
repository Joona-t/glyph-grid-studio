/* Charcoal titles — the large headings drawn by the charcoal-sketch skill.
 *
 * Each [data-ct] heading keeps its real text (screen readers, search,
 * no-JS and failed loads all get plain type). Once the image for the
 * current theme and layout has loaded, the heading gets .ct-on: the text
 * becomes visually hidden and the charcoal image shows in its place.
 *
 * Images: assets/titles/<name>-<theme>-<layout>.webp, layout = mob (≤520 px)
 * or desk. Sizes come from titles.js in em of the heading's own font, so an
 * image scales with the CSS clamp() exactly like the text it replaces.
 */
(function () {
  'use strict';
  var M = window.CHARCOAL_TITLES;
  if (!M || !document.querySelectorAll) return;
  var root = document.documentElement;
  var mq = window.matchMedia('(max-width: 520px)');
  var THEMES = ['cream', 'candy', 'basalt'];

  function theme() { var t = root.getAttribute('data-theme'); return THEMES.indexOf(t) >= 0 ? t : 'basalt'; }
  function layout() { return mq.matches ? 'mob' : 'desk'; }
  function src(name, t, l) { return 'assets/titles/' + name + '-' + t + '-' + l + '.webp'; }

  function sync() {
    var t = theme(), l = layout();
    document.querySelectorAll('[data-ct]').forEach(function (h) {
      var name = h.getAttribute('data-ct');
      var d = M[name] && M[name][t] && M[name][t][l];
      var key = name + '-' + t + '-' + l;
      if (h.getAttribute('data-ct-key') === key) return;
      h.setAttribute('data-ct-key', key);
      if (!d) { h.classList.remove('ct-on'); return; }
      var img = h.querySelector('img.ct-img');
      if (!img) {
        img = document.createElement('img');
        img.className = 'ct-img';
        img.alt = '';
        img.setAttribute('aria-hidden', 'true');
        img.decoding = 'async';
        h.appendChild(img);
      }
      img.onload = function () {
        if (h.getAttribute('data-ct-key') !== key) return;     // a newer theme/layout won
        img.style.width = d.w + 'em';
        img.style.margin = (-d.pt) + 'em auto ' + (-d.pb) + 'em';
        h.classList.add('ct-on');
      };
      img.onerror = function () { if (h.getAttribute('data-ct-key') === key) h.classList.remove('ct-on'); };
      img.src = src(name, t, l);
    });
  }

  // desktop: warm the other themes once the page is idle, so a theme switch
  // swaps instantly instead of showing the old colours for a beat
  function prefetch() {
    var c = navigator.connection;
    if (mq.matches || (c && c.saveData)) return;           // phones / Save-Data: load only what is shown
    var l = layout();
    Object.keys(M).forEach(function (name) {
      if (!document.querySelector('[data-ct="' + name + '"]')) return;
      THEMES.forEach(function (t) { if (M[name][t] && M[name][t][l]) { var i = new Image(); i.src = src(name, t, l); } });
    });
  }

  sync();
  new MutationObserver(sync).observe(root, { attributes: true, attributeFilter: ['data-theme'] });
  if (mq.addEventListener) mq.addEventListener('change', function () { sync(); prefetch(); });
  window.addEventListener('load', function () {
    (window.requestIdleCallback || function (f) { setTimeout(f, 800); })(prefetch);
  });
})();
