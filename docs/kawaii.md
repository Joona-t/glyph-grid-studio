# Kawaii editing feature

A postprocess overlay that scatters soft pink **hearts ♥** and **sparkles ✦** on top of the rendered glyph grid. Pairs with the new `kawaii-pink` palette for a cream→blush→hot-pink→magenta gradient.

## Why a postprocess (not a glyph set)

Glyph sets need font support — most monospace fonts don't have heart/star/flower characters. A postprocess overlay paints particles directly into the canvas RGBA buffer, so it works with any source image, any palette, and any glyph set. The hearts/sparkles literally sit *on top* of the typeset text — not replacing it.

## What it adds

- **`postprocess.kawaii`** stage — drawn LAST in the chain (above vignette, after CRT). Toggle in Studio → Postprocess → kawaii.
- **`kawaii-pink` palette** — 5-stop gradient (cream highlight → pale pink → hot pink → magenta → plum) that pairs with the overlay.
- **`runKawaiiVariants(baseDir)`** dev-console driver — renders 10 curated kawaii looks unattended.

## Particle types

Three pre-baked bitmap patterns, blitted directly into the canvas RGBA buffer:

| Pattern | Size | Count default | Where they live |
|---|---|---|---|
| Heart `♥` | 8×8 | 12 | Drift slowly across the canvas (per-particle `dvx`/`dvy`); slow twinkle phase |
| Sparkle `✦` | 5×5 | 28 | Fixed positions, faster twinkle, lighter pink |
| Twinkle `+` | 3×3 | 60 | Many, fastest, white-ish, brief flashes (clamped at 0.4 floor for visibility) |

All three are deterministic per-particle — no flicker between frames. Smooth twinkling via `Math.sin(frameIdx * speed * factor)`.

## Slider surface (Studio panel)

```
Postprocess → kawaii
  enabled        (off by default)
  intensity      0–1.5      master alpha
  heartCount     0–60       big particles
  sparkleCount   0–120      medium particles
  twinkleCount   0–200      small flashing particles
  speed          0.1–4      twinkle frequency multiplier
  hue R          0–255      red component (default 255)
  hue G          0–255      green component (default 105)
  hue B          0–255      blue component (default 180)
```

## Code locations

| Purpose | File |
|---|---|
| `applyKawaii(rgba, w, h, opts, frameIdx)` + bitmap patterns | `src/lib/glyph-crt.js` ~line 425 |
| `applyChain` integration (last stage) | `src/lib/glyph-crt.js` ~line 372 |
| `kawaii-pink` palette definition | `src/index.html` PALETTES |
| Studio panel sliders | `src/lib/glyph-studio.js` postprocess folder |
| 10-variant batch driver | `src/index.html` `window.runKawaiiVariants` |
| `frameIdx` propagation | `src/index.html` `applyPostprocess()` |

## The 10 curated variants

Run via dev console:

```js
runKawaiiVariants('/Users/darkfire/Documents/Glyph Grid Studio Test/kawaii/')
```

| # | Name | Look |
|---|---|---|
| 1 | classic | kawaii-pink × gradient × stbn × kawaii overlay (default cuteness) |
| 2 | dense-hearts | high heartCount + sparkleCount (kawaii max-density) |
| 3 | pastel-duotone | duotone color mode (softer feel) |
| 4 | dreamy-bloom | + bloom enabled (glowing fuzz) |
| 5 | y2k-pop | + scanlines, blockAscend ramp, bayer8 dither (CD-ROM era) |
| 6 | charcoal-cute | cream-paper + octant glyphs + kawaii (pencil sketch + sparkles) |
| 7 | pink-CRT | + crtBeam scanline-grille (TV monitor look) |
| 8 | minimalist | sparse ramp + low-density kawaii |
| 9 | max-cute | every kawaii dial maxed + halation + bloom |
| 10 | holographic | + chromatic aberration (iridescent shift) |

Each renders as a 32-frame loop at canvas resolution (1024×504 default).

## Performance

The overlay's per-frame cost is roughly:

```
heartCount × 8 × 8 + sparkleCount × 5 × 5 + twinkleCount × 3 × 3
= 12 × 64 + 28 × 25 + 60 × 9
= 768 + 700 + 540
= ~2000 pixel writes per frame
```

Negligible compared to the glyph grid render (which writes ~500,000 pixels per frame).
At max density (60/120/200): ~7,200 writes per frame — still inconsequential.

## Future extensions (not yet built)

- **`postprocess.kawaii.particleType`** — pick from heart / star / flower / cat-face
- **Tied to brightness** — concentrate particles where the source image is brightest (eyes, highlights)
- **Spawn-on-pulse** — hearts pop in time with the breathing pulse
- **Custom particle bitmap upload** — let users paint their own 8×8 patterns
- **Mask-aware spawning** — particles avoid certain canvas regions (so they don't cover the subject's face)

## Test verification

The kawaii feature is verified by:
1. **Visual**: 10 curated variants render to disk and look correct (hearts visible, sparkles twinkle, color matches palette)
2. **Static**: `applyKawaii` is unit-testable as a pure function (input ImageData → output ImageData with particle pixels added)
3. **Regression**: changing `intensity = 0` produces output byte-identical to no-kawaii rendering
