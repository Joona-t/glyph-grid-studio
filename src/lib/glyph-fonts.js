/* glyph-fonts.js — bundled font loader + runtime availability detection.

   Unicode 16 octants (U+1CD00–U+1CDE5) are the dense end of the new glyph
   sets. No OS ships an octant-capable font by default. We bundle a Cascadia
   Mono WOFF2 subset (primary) and BabelStone Pseudographica (fallback),
   plus int10h's PxPlus IBM VGA 8 for CP437 retro modes. All three are
   permissively licensed (OFL 1.1 / CC0).

   Availability detection is required: even when the CSS claims we loaded
   the font, a browser may fail to parse the WOFF2 or a user may have
   overridden monospace. We measure-text a sentinel from each set and
   compare against tofu width + ASCII 'M' width. A missing set cascades to
   the next fallback in order: octant → sextant → braille → blockElements →
   asciiDense → ascii.

   Nothing in this file rasters glyphs. That happens in build-glyph-sets.py
   (offline, deterministic) and at render time by the p5 text() call or
   the GPU atlas blit. */

(function () {
  'use strict';

  const FONT_FAMILY = 'LoveSparkGlyphGrid';
  const FALLBACK_FAMILY = 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace';

  /* Bundled fonts. `src` is a list of { url, format } candidates.
     The browser picks the first one it can actually decode. */
  const BUNDLED_FONTS = [
    {
      family: FONT_FAMILY,
      weight: 400,
      style: 'normal',
      src: [
        { url: './fonts/cascadia-mono-subset.woff2', format: 'woff2' },
      ],
      unicodeRange:
        'U+0020-007E, U+00A0-00FF, ' +            /* ASCII + Latin-1 */
        'U+2500-257F, U+2580-259F, ' +            /* Box + Block Elements */
        'U+2800-28FF, ' +                         /* Braille */
        'U+1FB00-1FB3B, U+1FB70-1FB8B, ' +        /* Sextants + Legacy Computing */
        'U+1CC00-1CC80, U+1CD00-1CDE5',           /* Supplement + Octants */
    },
    {
      family: FONT_FAMILY + '-Fallback',
      weight: 400,
      style: 'normal',
      src: [
        { url: './fonts/babelstone-pseudographica-subset.woff2', format: 'woff2' },
      ],
      unicodeRange: 'U+1FB00-1FB3B, U+1CD00-1CDE5',
    },
    {
      family: 'PxPlusIBMVGA8',
      weight: 400,
      style: 'normal',
      src: [
        { url: './fonts/pxplus-ibm-vga8.woff', format: 'woff' },
      ],
      unicodeRange: 'U+0020-007E, U+00A0-00FF, U+2500-259F',
    },
  ];

  /* Glyph-set sentinels — one representative codepoint per set.
     The set is "available" iff its sentinel renders at non-tofu width. */
  const SENTINELS = {
    ascii: 'A',
    asciiDense: '@',
    blockElements: '\u2588',       /* FULL BLOCK */
    braille: '\u2800',             /* BRAILLE BLANK PATTERN — zero-width in most fonts, see fallback check */
    brailleVisible: '\u28FF',      /* BRAILLE WITH ALL DOTS */
    boxDrawing: '\u2500',          /* BOX DRAWINGS LIGHT HORIZONTAL */
    sextant: '\uD83E\uDF00',       /* U+1FB00 BLOCK SEXTANT-1 */
    octant: '\uD83E\uDF00',        /* placeholder; real octant below via codepoint */
    cp437: '\u00B0',               /* Latin-1, but proxies well for CP437 presence */
  };

  /* Real octant sentinel — U+1CD00 is a block octant.
     JavaScript string literal: high surrogate 0xD833 + low 0xDD00. */
  SENTINELS.octant = String.fromCodePoint(0x1CD00);

  const CASCADE_ORDER = ['octant', 'sextant', 'braille', 'blockElements', 'asciiDense', 'ascii'];

  const CACHE_KEY = '__glyphGridFontCache';

  /* Inject @font-face rules into the document head. Safe to call multiple
     times — does nothing on the second call. */
  function injectFaces(doc) {
    doc = doc || document;
    if (doc.getElementById('__glyph-grid-font-faces')) return;
    const css = BUNDLED_FONTS.map(function (f) {
      const srcs = f.src.map(function (s) {
        return 'url("' + s.url + '") format("' + s.format + '")';
      }).join(', ');
      return '@font-face {\n' +
             '  font-family: "' + f.family + '";\n' +
             '  font-weight: ' + f.weight + ';\n' +
             '  font-style: ' + f.style + ';\n' +
             '  font-display: block;\n' +
             '  src: ' + srcs + ';\n' +
             (f.unicodeRange ? '  unicode-range: ' + f.unicodeRange + ';\n' : '') +
             '}';
    }).join('\n');
    const style = doc.createElement('style');
    style.id = '__glyph-grid-font-faces';
    style.textContent = css;
    doc.head.appendChild(style);
  }

  /* Wait for a specific family to finish loading (or fail).
     Uses the Font Loading API if available, polls document.fonts otherwise,
     and falls back to a fixed timeout if neither works. */
  function waitForFace(family, sizePx, timeoutMs) {
    sizePx = sizePx || 16;
    timeoutMs = timeoutMs || 3000;
    if (typeof document === 'undefined' || !document.fonts || !document.fonts.load) {
      return new Promise(function (resolve) { setTimeout(resolve, 250); });
    }
    const spec = sizePx + 'px "' + family + '"';
    return Promise.race([
      document.fonts.load(spec).then(function () { /* ok */ }),
      new Promise(function (resolve) { setTimeout(resolve, timeoutMs); }),
    ]);
  }

  /* Measure a single codepoint's advance width in the active font,
     by drawing it on an offscreen 2D canvas. Returns 0 if canvas not
     available (jsdom / node). */
  function measureGlyphWidth(codepoint, family, sizePx) {
    if (typeof document === 'undefined' || !document.createElement) return 0;
    sizePx = sizePx || 32;
    const cvs = document.createElement('canvas');
    cvs.width = sizePx * 4;
    cvs.height = sizePx * 2;
    const ctx = cvs.getContext('2d');
    if (!ctx) return 0;
    ctx.font = sizePx + 'px "' + family + '", ' + FALLBACK_FAMILY;
    return ctx.measureText(codepoint).width;
  }

  /* Detect availability by comparing the sentinel's rendered width to
     tofu (U+FFFD) and to ASCII 'M'. If sentinel == tofu, the set is
     unsupported. If sentinel == 'M' width AND the codepoint isn't ASCII,
     the family fell back to monospace — treat as unsupported. */
  function detectAvailability(family, sizePx) {
    sizePx = sizePx || 32;
    const tofu = measureGlyphWidth('\uFFFD', family, sizePx);
    const mWidth = measureGlyphWidth('M', family, sizePx);
    const out = {};
    for (const setName of Object.keys(SENTINELS)) {
      const ch = SENTINELS[setName];
      const w = measureGlyphWidth(ch, family, sizePx);
      /* A codepoint in the BMP above 0x2000 that measures exactly as
         monospace 'M' is almost certainly a font-substitution miss. */
      const isAscii = setName === 'ascii' || setName === 'asciiDense';
      const missingByTofu = Math.abs(w - tofu) < 0.5 && tofu > 0;
      const missingByFallback = !isAscii && mWidth > 0 && Math.abs(w - mWidth) < 0.5;
      out[setName] = {
        width: w,
        available: w > 0 && !missingByTofu && !missingByFallback,
      };
    }
    return out;
  }

  /* Pick the densest available set given the requested ordering.
     Returns { id, fallback: boolean, chain: [requested, ...until-found] }. */
  function resolveSet(requested, availability) {
    const startIdx = CASCADE_ORDER.indexOf(requested);
    if (startIdx === -1) {
      return { id: 'ascii', fallback: true, chain: [requested, 'ascii'] };
    }
    const chain = [];
    for (let i = startIdx; i < CASCADE_ORDER.length; i++) {
      const name = CASCADE_ORDER[i];
      chain.push(name);
      if (availability[name] && availability[name].available) {
        return { id: name, fallback: i !== startIdx, chain: chain };
      }
    }
    return { id: 'ascii', fallback: true, chain: chain };
  }

  /* High-level: inject faces, wait for primary, probe availability,
     cache result for session, return descriptor.

     Stage 2A — octant cascade fix:
       The browser sometimes reports `font.load()` resolved while the WOFF2
       glyph table for U+1CD00–U+1CDE5 isn't actually queryable yet via
       measureText.  This caused valid octants to be detected as "missing"
       and cascaded down to ASCII.  Two changes make this robust:

       1. Retry the availability probe up to 3× with short backoff after
          the initial font.load() resolves.  Cold-cache cases now succeed.
       2. If `options.trustRequested === true`, skip availability-based
          cascading entirely and pin the resolvedSet to the requested set.
          The renderer will paint the actual glyph; the browser falls back
          per-codepoint via the cssStack if the glyph genuinely is missing,
          which is fine and looks the same as the old cascade in practice. */
  function load(options) {
    options = options || {};
    const sizePx = options.sizePx || 32;
    const requestedSet = options.glyphSet || 'ascii';
    const trust = options.trustRequested === true;

    const cached = (typeof window !== 'undefined') && window[CACHE_KEY];
    if (cached && cached.requestedSet === requestedSet && cached.trustRequested === trust) {
      return Promise.resolve(cached);
    }

    function probeWithRetry(attemptsLeft) {
      const avail = detectAvailability(FONT_FAMILY, sizePx);
      const resolved = resolveSet(requestedSet, avail);
      // If the requested set landed at fallback AND we have retries left,
      // wait a tick for the WOFF2 to finish decoding and probe again.
      if (resolved.fallback && attemptsLeft > 0 && requestedSet !== 'ascii') {
        return new Promise(function (r) { setTimeout(r, 120); })
          .then(function () { return probeWithRetry(attemptsLeft - 1); });
      }
      return { avail: avail, resolved: resolved };
    }

    injectFaces();
    return waitForFace(FONT_FAMILY, sizePx, 3000)
      .then(function () { return waitForFace(FONT_FAMILY + '-Fallback', sizePx, 1500); })
      .then(function () { return waitForFace('PxPlusIBMVGA8', sizePx, 1500); })
      .then(function () {
        if (trust) {
          // Skip the cascade entirely — the user has asked for a specific set.
          return {
            avail: detectAvailability(FONT_FAMILY, sizePx),
            resolved: { id: requestedSet, fallback: false, chain: [requestedSet] },
          };
        }
        return probeWithRetry(3);
      })
      .then(function (probe) {
        const descriptor = {
          requestedSet: requestedSet,
          resolvedSet: probe.resolved.id,
          usedFallback: probe.resolved.fallback,
          cascade: probe.resolved.chain,
          availability: probe.avail,
          trustRequested: trust,
          primaryFamily: FONT_FAMILY,
          fallbackFamily: FONT_FAMILY + '-Fallback',
          cp437Family: 'PxPlusIBMVGA8',
          cssStack: '"' + FONT_FAMILY + '", "' + FONT_FAMILY + '-Fallback", ' + FALLBACK_FAMILY,
        };
        if (typeof window !== 'undefined') {
          window[CACHE_KEY] = descriptor;
          if (probe.resolved.fallback) {
            const msg = 'glyph-grid: requested set "' + requestedSet + '" unavailable; ' +
                        'cascaded to "' + probe.resolved.id + '" via ' + probe.resolved.chain.join(' → ');
            if (console && console.warn) console.warn(msg);
          }
        }
        return descriptor;
      });
  }

  function clearCache() {
    if (typeof window !== 'undefined' && window[CACHE_KEY]) delete window[CACHE_KEY];
  }

  const api = Object.freeze({
    FONT_FAMILY: FONT_FAMILY,
    FALLBACK_FAMILY: FALLBACK_FAMILY,
    BUNDLED_FONTS: BUNDLED_FONTS,
    SENTINELS: SENTINELS,
    CASCADE_ORDER: CASCADE_ORDER,
    injectFaces: injectFaces,
    waitForFace: waitForFace,
    measureGlyphWidth: measureGlyphWidth,
    detectAvailability: detectAvailability,
    resolveSet: resolveSet,
    load: load,
    clearCache: clearCache,
  });

  const root = (typeof window !== 'undefined') ? window
             : (typeof globalThis !== 'undefined') ? globalThis
             : this;
  root.GlyphGrid = root.GlyphGrid || {};
  root.GlyphGrid.fonts = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})();
