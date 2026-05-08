# User Guide — Glyph Grid Studio

A friendly walkthrough for both **non-technical readers** (drop a GIF in, get glyph art out) and **technical readers** (every "Under the hood" box explains the algorithm). Read top-to-bottom for a tour, or jump to a section you need.

---

## Contents

1. [Welcome](#1-welcome)
2. [Quick Start (3 minutes)](#2-quick-start-3-minutes)
3. [The Canvas + Status Bar](#3-the-canvas--status-bar)
4. [Loading Images and GIFs](#4-loading-images-and-gifs)
5. [The Studio Panel — Folder Walkthrough](#5-the-studio-panel--folder-walkthrough)
6. [Color Modes Deep Dive](#6-color-modes-deep-dive)
7. [Selection Modes Deep Dive](#7-selection-modes-deep-dive)
8. [Postprocess Catalog](#8-postprocess-catalog)
9. [Recipes](#9-recipes)
10. [Tips & Troubleshooting](#10-tips--troubleshooting)
11. [Glossary](#11-glossary)
12. [Reference Tables](#12-reference-tables)
13. [Performance Notes](#13-performance-notes)
14. [Keyboard / Mouse Quick Reference](#14-keyboard--mouse-quick-reference)
15. [Where Files Go](#15-where-files-go)

---

## 1. Welcome

**Glyph Grid Studio** turns any image — still or animated — into a mosaic of typeset Unicode characters. Drop in a portrait, an album cover, an animated GIF; choose a palette and a glyph set; export a still, a GIF, or an MP4. Every pixel of the output is part of a real character (`@`, `▓`, `⠿`, `🬕`, etc.).

**What you can make:**
- ASCII / glyph art portraits in the cream-paper-ink-stipple style
- Phosphor-CRT terminal aesthetics (green-on-black, amber-on-black)
- Kawaii pink overlays with hearts and sparkles
- Spice / amber / bone-charcoal duotone gradients
- Looped animated GIFs and MP4s for social media
- Frame-perfect PNG snapshots

**System requirements:**
- macOS 11+ (Apple Silicon or Intel)
- ~50 MB free disk space (~10 MB app, ~30 MB working space for renders)
- An image to render — supported: PNG, JPG, WebP, GIF (animated), BMP, TIFF, AVIF

> **Under the hood**
> The pipeline samples each source pixel into an `M × N` cell grid, picks a glyph per cell from either a luminance ramp or a 6-D shape-vector nearest-neighbour lookup, and typesets the result. Front end: p5.js sketch in a Tauri 2 WKWebView. Back end: Rust (gifski for GIF, openh264 + mp4 for MP4). UI: Tweakpane v3 with 70+ live-tunable bindings. See `README.md` for the algorithmic citations.

---

## 2. Quick Start (3 minutes)

1. **Launch the app.** A blank canvas appears with the message "Drop an image to start".
2. **Drop or pick an image.** Drag any image file onto the window, or click `Pick image…` in the **Image** folder of the right-hand panel. Animated GIFs work too — the studio plays them in real time.
3. **Try a preset.** Open the **Presets** folder, pick one from the `Load` dropdown. The canvas updates instantly.
4. **Tweak until you like it.** Common first moves:
   - **Color** folder → change `palette` (try `cream-paper` for the ink-stipple look, `phosphor` for terminal green, `kawaii-pink` for hot-pink-on-cream).
   - **Color** folder → change `colorMode` to `monochrome` or `gradient` to compare looks.
   - **Selection (advanced)** folder → set `selectionMode` to `shape-edge-aware` and `glyphSet` to `octant` for crisp shape rendering.
5. **Export.** Open the **Export** folder:
   - `Snapshot PNG` for a still.
   - `Export GIF` for an animated GIF (auto-loops cleanly).
   - `Export MP4` for an Instagram-friendly video (smaller files than GIF).

That's it. Everything else in this doc is depth on individual settings.

---

## 3. The Canvas + Status Bar

The main canvas shows the live render. Below it (or in the title bar) you'll see a status string like:

```
1024×683 · 240×120 · brightness · cream-paper · 11 ms · gif=12/30 · shape-edge-aware/atlas=ascii
```

Each segment means:

| Segment | Meaning |
|---|---|
| `1024×683` | Canvas pixels (width × height) |
| `240×120` | Grid cells (cols × rows) |
| `brightness` | Active selection mode (or `shape`, `shape-edge-aware`, `edge-directional`) |
| `cream-paper` | Active palette |
| `11 ms` | Total per-frame render time |
| `gif=12/30` | Playing frame 12 of an animated source (only shows for animated GIFs) |
| `shape-edge-aware/atlas=ascii` | Diagnostic: which atlas was loaded for shape-mode rendering |

If the frame time creeps above ~33 ms you'll see motion stutter — see the [Performance Notes](#13-performance-notes) for what to drop first.

---

## 4. Loading Images and GIFs

Three paths into the studio:

**Drag and drop.** Drop an image file anywhere on the window. Reads from disk via Tauri's drag-drop event.

**`Pick image…` button.** In the **Image** panel folder. Opens a native file picker.

**Headless CLI** (advanced): `glyph-grid-studio render --in foo.gif --out bar.mp4 --frames 90`. Same pipeline, no GUI. See `README.md` for the full flag list.

### Static images (PNG, JPG, WebP, BMP, TIFF, AVIF)
Loaded once, rendered every frame using the same source pixels. Per-frame motion comes from breathing/jitter (Animation folder), not from the source.

### Animated GIFs
Each source frame plays in real time. The studio computes the right source frame for the current playback time using cumulative frame-delay scanning, so a GIF with non-uniform inter-frame delays plays at correct speed.

> **Under the hood**
> Animated GIF source playback uses p5's `sourceImg.gifProperties.frames[i].delay` array. On every studio frame:
> ```
> t      = recState.frameIdx / CONFIG.animation.fps     (recording)
>          (millis() - startMs) / 1000                   (live preview)
> totalMs = sum of frame delays
> loopT   = (t × 1000) mod totalMs
> srcIdx  = first i where Σ delays[0..=i] > loopT
> sourceImg.setFrame(srcIdx)
> ```
> This wraps cleanly on loop (no stutter at the seam) and supports recording the GIF straight back to MP4/GIF at any duration.

### Aspect-ratio auto-fit
When `studio.fitCanvasToImage = true` (default), loading an image resizes the canvas to match the source aspect, capping the long edge at 1024 px. This keeps render cost predictable while preserving framing.

---

## 5. The Studio Panel — Folder Walkthrough

The right-hand panel (Tweakpane v3) is built dynamically from `CONFIG`. Twelve folders cover the whole pipeline. **Each setting below is followed by what changes visually, when to use it, and an "Under the hood" box for the technical why.**

### 5.1 Image

Two controls:

#### `drag & drop an image anywhere`
A reminder, not a button. Drag any image file onto the window.

#### `Pick image…`
Opens the native file picker. Tauri's `pick_image` command reads the chosen file via Rust's filesystem layer, base64-encodes the bytes, and returns a data URL the JS sketch can decode with `p5.loadImage`.

---

### 5.2 Grid

Defines the cell grid the source image samples into.

#### `cols` (default 240, range 60–400, step 5)
How many columns the glyph grid has.

**What changes visually:** more columns → smaller cells → more detail; fewer → chunkier "ascii art" feel.

**When to use:** bump up for portrait detail (320+); bump down for retro chunkier vibe (120 or 80).

#### `rows` (default 120, range 40–300, step 5)
How many rows the glyph grid has. Same shape as `cols`.

> **Under the hood**
> `cols × rows` is the total cell count. Each cell samples a `cellW × cellH` region of the source. At 240×120 = 28,800 cells on a 1024×683 canvas (≈ 4.27 × 5.69 px per cell). Render cost is roughly linear in cell count — see Perf folder. Above ~480×240 (115,200 cells) the canvas2D path stops keeping up at 30 fps; consider the planned WebGL renderer (`WEBGL-RENDERER-DESIGN.md`).

#### `font.size` (default 8, range 3–14, step 1)
Pixel size of the glyph drawn into each cell.

**What changes visually:** size 8 with 4.27-px cells = chars overlap ~94% horizontally and bleed into neighbours, dissolving column stripes. Smaller font = visible grid lines between cells. Larger = chars overflow cell bounds, can cause neighbour collisions.

**When to use:** tune until column/row stripes vanish into the texture.

---

### 5.3 Mapping

How source pixels map to glyph indices.

#### `ramp` (default `gradient`)
The character set used for brightness-based rendering. Eleven options:

| Ramp | Characters | Use case |
|---|---|---|
| `classic` | ` .·:-=+*#%@` | Traditional ASCII art |
| `dense` | ` .,:;i1tfLCG08@` | Smoother tonal range |
| `sparse` | ` .:+#@` | Heavy minimalist look |
| `unicode-block` | ` ░▒▓█` | Block-shaded gradient |
| `gradient` | (22-char monotonic) | Smooth tonal washes — pairs with `colorMode: gradient` |
| `gradientNoSpace` | (21-char no leading space) | Every cell gets a glyph (no transparent cells) |
| `blockShaded` | `░▒▓█` | 4-band shaded blocks, no stroke alignment |
| `blockAscend` | `▁▂▃▄▅▆▇█` | Vertical block ascender — no cell baselines |
| `radial` | ` .*+xX%vY` | Glyphs biased to radial/diagonal — for radially-symmetric subjects |

#### `brightnessGamma` (default 0.55, range 0.2–2.5, step 0.05)
Gamma curve applied to source brightness before ramp lookup.

**What changes visually:** lower gamma (0.4) crushes mid-tones into dark glyphs (high contrast, "stippled" look). Higher gamma (1.5) lifts mid-tones into mid-density glyphs (smooth wash).

**When to use:** start at 0.55 for portraits (matches the cream-paper preset). Go higher for low-key photographs that need lifting.

#### `samplingStrategy` (default `average`)
How each cell reads the source region beneath it.

- `average` — mean luminance of all source pixels in the cell. Best for photos.
- `nearest` — single sample at cell center. Fast, pixelated, good for sprite art.
- `edge-weighted` — biases the sample toward high-gradient pixels. Preserves thin lines (eyebrows, antenna) that `average` would blur away.

> **Under the hood**
> `average` does an integer-binned mean over the cell's source rect; `nearest` is a single texelFetch; `edge-weighted` weights each source pixel by its local Sobel gradient before averaging.

---

### 5.4 Dither (collapsed by default)

Quantization noise pattern applied to the brightness signal before ramp lookup. Quantization steps the smooth signal into discrete glyph indices; dithering distributes that step error so the grid pattern doesn't read as banding.

**Only applies in `selectionMode: brightness`** (and as a source prefilter for shape modes). If the dropdown does nothing visually, check that selection mode.

#### `mode`
Nine options. Trade-off: spatial vs temporal vs error-diffusion.

| Mode | Type | Use case |
|---|---|---|
| `none` | — | Hard quantization. Visible banding on smooth gradients. |
| `bayer4`, `bayer8` | Spatial ordered | Classic 4×4 / 8×8 ordered dither. Slightly hash-jittered for animation. |
| `blueNoise` | Spatial blue noise | Less structured pattern than Bayer; reads as "grain". |
| `temporal` | Temporal Bayer8 | Per-frame pixel offset hashed by `(seed, frameIdx)`. **The default.** Cells on quantization boundaries roll between adjacent ramp chars every frame, animating the texture. |
| `stbn` | Spatiotemporal blue noise | NVIDIA EGSR 2022's STBN, approximated via Halton(2,3) sequence. Smoothest temporal variation. |
| `floydSteinberg` | Error diffusion | Classic FS. Sharp edges, slight character bleed. |
| `atkinson` | Error diffusion | Original Macintosh dither. Lighter overall, preserves detail. |
| `jarvisJudiceNinke` | Error diffusion | Larger kernel than FS. Smoothest gradients. |

#### `levels` (range 2–32, step 1) — only visible for spatial/temporal modes
Number of brightness bands. Lower = chunkier, higher = more glyph variety.

#### `asSourcePrefilter` (boolean) — only visible for `temporal` mode
When on, the dither also runs over the source image before the cell signal is computed (not just on the cell signal itself). Cheap, helps stabilize shape-mode picks under animation.

> **Under the hood**
> Dither runs on the per-cell brightness signal `s ∈ [0, 1]`. For ordered modes, `s' = s + jitter[x % N][y % N]` where `jitter` is a Bayer threshold matrix. For temporal, `jitter` shifts each frame by a `wrap_uv(seed, frameIdx)` hash. For STBN, `jitter` is a 2D Halton lattice point indexed by `(x, y, frameIdx mod 16)`. Error diffusion modes propagate `(s' - quantize(s')) × kernel_weights` to forward neighbours; this is incompatible with the `asSourcePrefilter` flag.

---

### 5.5 Color

Two settings that together determine the look.

#### `palette` (default `cream-paper`)
Eleven palettes. Each has a background colour and 1–5 ink stops:

| Palette | Bg | Inks | Vibe |
|---|---|---|---|
| `monochrome` | `#000000` | white | Pure b/w |
| `phosphor` | `#001100` | green stops | Classic terminal green |
| `bauhaus` | `#F2EBDA` | red, blue, yellow | Bauhaus primaries on cream |
| `lovespark` | `#000000` | pink, magenta, cyan | High-contrast neon |
| `mono-amber` | `#1A0D00` | amber | Single warm ink on near-black |
| `cyber-phosphor` | `#020609` | cyan stops | Mr-Robot hacker terminal |
| `amber-phosphor` | `#0A0500` | warm-amber stops | Vintage warm CRT |
| `bone-charcoal` | `#0A0907` | bone → charcoal | Hand-drawn study feel |
| `cream-paper` | `#E8DDC8` | deep charcoal | Ink stipple on aged paper |
| `silver-charcoal` | `#0A0907` | charcoal → silver | Brightest stop = silver, for chrome highlights |
| `spice` | `#0A0302` | amber, rust, gold, ember | Arrakis melange |
| `kawaii-pink` | `#FFF0F5` | cream → magenta | Pairs with kawaii postproc overlay |

#### `colorMode` (default `monochrome` for cream-paper preset)
How the palette maps to per-cell colour. Four options — see [Color Modes Deep Dive](#6-color-modes-deep-dive) for full detail. Quick summary:

- **`preserve`** — keep source pixel colour. Glyph is the alpha mask.
- **`monochrome`** — single ink with alpha driven by signal.
- **`duotone`** — lerp between two inks (`inks[0]` ↔ `inks[1]`) by signal.
- **`gradient`** — interpolate across all ink stops in OKLab. Smooth multi-stop wash.

> **Under the hood**
> Monochrome alpha: `α = signal × 1.1` clamped to `[0, 1]`. Duotone lerp: `c = mix(inks[0], inks[1], signal)` in OKLab cylindrical space. Gradient interpolation: cylindrical OKLab interpolation along the shorter hue arc, so pink → green stays saturated through orange instead of dipping through gray.

---

### 5.6 Selection (advanced) — collapsed by default

Switches the whole rendering algorithm.

#### `selectionMode` (default `brightness`)
- **`brightness`** — pick glyph by per-cell luminance. Fast (~11 ms at 240×120), classic ASCII look.
- **`shape`** — match per-cell shape vector to a glyph atlas via k-d tree. ~30 ms.
- **`shape-edge-aware`** — like `shape` but biases toward edge-aligned glyphs. ~42 ms. **The strongest aesthetic.**
- **`edge-directional`** — picks glyphs by local gradient direction (`/`, `\`, `|`, `—`). Most "drawn" feel.

See [Selection Modes Deep Dive](#7-selection-modes-deep-dive) for visual comparison + when to pick each.

#### `glyphSet` (default `null`)
The atlas used in shape modes. `null` falls back to brightness mode regardless of `selectionMode`.

| Set | Description | Cell density |
|---|---|---|
| `ascii` | Letters, digits, punctuation | Mid |
| `asciiDense` | Larger ASCII set including box-drawing | Mid-high |
| `blockElements` | `█▓▒░` and friends | High |
| `braille` | All 256 8-dot braille codepoints | Highest |
| `sextant` | 6-cell block patterns | Very high |
| `octant` | 2×4 block fills (Unicode 16) | Highest, modern only |

> **Under the hood**
> Each glyph is encoded as a 6-D shape vector capturing local mass distribution (Alex Harri 2024). Atlas built once at startup (or on first shape-mode entry), stored in a k-d tree. Per-cell lookup: compute the cell's shape vector, query the tree for the nearest glyph. ~7 µs per cell after warm-up. The `_atlasCache` (F6 pre-warm) holds all six atlases ready; switching `glyphSet` is an instant pointer swap when the cache hits.

---

### 5.7 Breathing

Subtle per-frame motion that gives static images a "living" feel. Off when `colorMode: preserve` (would corrupt source colour). Otherwise on.

#### `emaAlpha` (default 0.35, range 0–1, step 0.01)
EMA mixing factor for the per-cell brightness signal. 0 = no smoothing (cells flicker at noise rate). 1 = full hold (no animation). 0.35 = relaxed breathing.

#### `gain swing` (default 0.30, range 0–0.6, step 0.01)
Per-frame multiplicative pulse on the global signal. The whole image breathes brighter/darker by this amount.

#### `jitter` (default 0.05, range 0–0.2, step 0.005)
Per-cell random offset added to the signal each frame. Higher = more "live" texture.

#### `pulseHz` (default 0.7, range 0.1–3, step 0.05)
The rate of the gain swing in Hz. 0.7 = ~once per 1.4 seconds. Slow = meditative, fast = nervous.

> **Under the hood**
> Per-cell signal `s_t = α × s_raw + (1 − α) × s_{t-1}` (EMA). Then `s_t × (1 + gainSwing × sin(2π × pulseHz × t))` for the global pulse. Then `+ jitter × hash(x, y, frameIdx)` for per-cell variation. The combined motion reads as breathing without breaking the figure.

---

### 5.8 Postprocess (collapsed by default)

CRT / film overlay effects applied AFTER the glyph grid renders. Each is a sub-folder with its own `enabled` toggle plus stage-specific parameters.

See [Postprocess Catalog](#8-postprocess-catalog) for the full list with cost bands and recommended strengths.

**Default:** only `vignette` is enabled (cheap, ~5 ms, frames the canvas).

> **Under the hood**
> Two phases. **Phase A** runs imageData-based effects (bloom, halation, chromatic aberration, godRays, depthFog, barrel) — needs a `getImageData` round trip, ~20–80 ms each. **Phase B** runs canvas-composited overlays (vignette, scanlines, crtBeam, kawaii, letterbox) — `globalCompositeOperation = 'multiply'` overlay, ~5–15 ms each. Phase A is skipped entirely when no Phase A stage is enabled, which is why "vignette only" is so cheap (and why the WebGL renderer plan stays optional).

---

### 5.9 Animation (collapsed by default)

Controls how long an export plays for and at what frame rate.

#### `duration` (default 6.0 s, range 0.5–60, step 0.1)
Output animation length in seconds. The Export folder shows the resulting frame count and final-quantized length live.

#### `fps` (default 30, range 12–60, step 6)
Target frames per second. GIFs quantize this to centisecond delays, so the actual playback fps is `1000 / round(1000 / fps / 10) × 10` (e.g. 30 → 33.33 effective).

#### `loop` (boolean, default true)
Whether the exported GIF loops. MP4 always plays once per cycle (IG / X auto-loop video).

> **Under the hood**
> `exportPlan()` in glyph-studio.js computes `delayMs = max(20, round(1000/fps/10) × 10)` (centisecond floor), `effFps = 1000/delayMs`, `frames = max(2, round(duration × effFps))`, `dur = frames × delayMs / 1000`. The Export GIF and Export MP4 buttons override `CONFIG.animation.fps` to `effFps` for the duration of the recording so studio frame timing matches encoded frame timestamps — without this, GIFs stutter at the loop wrap because the encoded fps and the studio's playback fps disagree.

---

### 5.10 Perf (collapsed by default)

Per-stage frame timing monitors. Refresh interval 200 ms. Read-only.

| Field | Meaning |
|---|---|
| `total ms` | Total frame time. Target ≤ 33 ms for 30 fps. |
| `scene ms` | Scene rendering (the source image draw) |
| `lum ms` | Luminance computation |
| `downsample ms` | Source → cell-grid downsample |
| `ema ms` | Breathing EMA hook |
| `dither ms` | Dither pass |
| `grid ms` | Glyph grid render (the bulk for canvas2D) |
| `select ms` | Sub-stage of grid: glyph selection (brightness vs shape lookup) |
| `draw ms` | Sub-stage of grid: canvas drawImage calls |
| `postproc ms` | Postprocess chain |
| `last switch` | Time of last config-changed-and-rebuilt event |

Two buttons: `Report (console)` dumps a window-averaged report; `Clear` resets accumulators.

---

### 5.11 Presets

Save / restore named CONFIG snapshots in browser localStorage.

#### `Save as` (text input)
Type a name, click `Save current`. The current CONFIG is JSON-stringified and stored under that key.

#### `Load` (dropdown)
Lists all saved presets plus the bundled ones. Selecting a preset applies its CONFIG immediately.

#### `Export JSON`
Downloads the current CONFIG as a `.json` file. Can be passed to the headless CLI via `--preset path/to/file.json`.

#### `Import JSON`
Opens a file picker, applies the chosen JSON.

#### `Copy share URL`
Encodes the current CONFIG into the URL query string and copies it to the clipboard. Anyone visiting that URL loads the same CONFIG.

> **Under the hood**
> Presets are full CONFIG objects (no diffing). Storage key prefix is `glyphgrid:preset:`. Share URLs use a base64-encoded JSON in the `?p=` parameter, decoded on page load by `presetFromURL()`.

---

### 5.12 Export

Export the current visual as a still or animation.

#### `Snapshot PNG`
Saves the current canvas frame as PNG. Pops a save dialog; default filename `glyph-frame.png`.

#### `frames` (read-only monitor)
Frame count Export GIF / Export MP4 will produce, derived from `duration × effFps`.

#### `length` (read-only monitor)
Final length the GIF/MP4 will have, accounting for centisecond quantization.

#### `output size` (dropdown)
- `full` — native canvas dimensions
- `720` — cap longest edge at 720 px (Instagram / Twitter mobile-friendly)
- `480` — cap at 480 px (smallest, fits Twitter's strict GIF size limits)

#### `Export GIF`
Records `frames` frames, encodes via gifski (Rust), pops save dialog. Quantization quality 100, shared global palette, error-diffusion dither across the sequence.

#### `Export MP4`
Records `frames` frames, encodes via openh264 + mp4 (Rust), pops save dialog. H.264 Constrained Baseline yuv420p — IG-compatible. Smaller than GIF (~3× smaller for kaneki: 13 MB GIF → 4.3 MB MP4).

> **Under the hood**
> Both buttons override `CONFIG.animation.fps = effFps` for the recording duration, restore on completion. Frame collection is identical (base64-encoded PNGs sent over IPC). The Rust encoders differ:
> - **GIF**: gifski 1.34, quality 100, fast=false, repeat=Infinite. Frames decoded via `image::load_from_memory`, converted to `imgref::ImgVec<rgb::RGBA8>`, encoded with shared global palette.
> - **MP4**: openh264 0.9.3 with `RateControlMode::Off` (default RC was skipping low-motion P-frames and emitting zero NALs), 5 Mbps target. Output muxed into ISOBMFF via the `mp4` crate with an avc1 video track, AVCC-framed slice NALs, SPS/PPS in `AvcConfig`.

---

## 6. Color Modes Deep Dive

Four modes. The biggest aesthetic lever in the studio.

### `preserve` — keep source colour
Each cell renders the glyph at full opacity, using the colour of the source pixel beneath it. The glyph shape acts as an alpha mask over the source.

**Looks like:** the source image, but textured into a glyph mosaic. Colours unchanged.

**When to use:** when you want the original photograph's palette preserved (e.g. a colour photo turned into glyph art without a stylized palette).

**Caveat:** breathing / EMA is disabled in this mode (would corrupt source colour).

### `monochrome` — one ink, alpha by signal
Single ink colour (`palette.inks[0]`). Each cell's alpha is driven by the brightness signal.

**Looks like:** ink stipple on coloured paper. Cream-paper preset is a perfect match.

**When to use:** anything where the subject's silhouette + texture should dominate over palette colour. Portraits, logos, stark imagery.

**Math:** `α = signal × 1.1` clamped to `[0, 1]`. The `× 1.1` ensures bright cells reach full opacity.

### `duotone` — lerp two inks by signal
`mix(inks[0], inks[1], signal)`. Bright cells = `inks[0]`, dim cells = `inks[1]`.

**Looks like:** classic risograph 2-colour print. Phosphor presets read as duotone naturally.

**When to use:** when you want palette character but not the multi-stop wash of gradient mode.

**Math:** linear mix in OKLab cylindrical space along the shorter hue arc.

### `gradient` — interpolate across all ink stops
`interpAtU(palette.inks, signal)`. Ink stops are evenly spaced from `signal=0` (last ink) to `signal=1` (first ink).

**Looks like:** smooth multi-stop wash. Spice / silver-charcoal / kawaii-pink shine in this mode.

**When to use:** when you want palette colour to drive the whole image, not just texture.

**Math:** OKLab cylindrical interpolation. For an N-ink palette, signal `u` lands between `inks[floor(u × (N-1))]` and `inks[ceil(u × (N-1))]`, weighted by the fractional part. Hue arc traversed via the shorter direction (e.g. pink → green via orange, not via blue).

> **Under the hood**
> All four modes call into the same per-cell alpha computation. The colour computation differs:
> - `preserve`: `colour = sourcePixel`
> - `monochrome`: `colour = inks[0]; α = signal × 1.1`
> - `duotone`: `colour = mix(inks[0], inks[1], signal)` in OKLab
> - `gradient`: `colour = interpAtU(inks, signal)` in OKLab
>
> The atlas blit uses `globalCompositeOperation = 'source-over'` with the resolved RGBA. Atlas tiles are pre-rasterised white-on-transparent — the per-cell tint is applied via canvas `globalCompositeOperation` magic before the blit.

---

## 7. Selection Modes Deep Dive

Four ways to pick the glyph for each cell. Different aesthetics, different costs.

### `brightness` — luminance-only ramp lookup
**How:** compute cell's mean luminance, look up the corresponding character in `RAMPS[ramp]`.

**Looks like:** classic ASCII art. Smooth tonal gradients, no shape information.

**Cost:** ~11 ms at 240×120. The fastest mode by far.

**Best with:** any palette + any colour mode. Default for portraits and animated GIF inputs.

### `shape` — 6-D shape-vector nearest-neighbour
**How:** compute a 6-D shape vector for each cell (representing local mass distribution), query the glyph atlas k-d tree for the nearest match.

**Looks like:** shapes start matching the source structure — diagonals match diagonals, corners match corners. More "drawn" feel than brightness.

**Cost:** ~30 ms at 240×120 with the ASCII atlas.

**Best with:** glyph sets with strong shape variety (ascii, octant). Less interesting for blockShaded.

### `shape-edge-aware` — shape vector with edge bias
**How:** like `shape`, but cells sitting on detected edges get biased toward edge-aligned glyphs in the atlas.

**Looks like:** the strongest aesthetic. Edges of the subject pop with directional glyphs (`╱`, `╲`, `│`, `─`); flat regions get block fills. The look in the README hero shot.

**Cost:** ~42 ms at 240×120. About 4× slower than brightness.

**Best with:** octant or ascii glyph set. Cream-paper monochrome. Portraits especially.

### `edge-directional` — gradient direction → glyph
**How:** compute local gradient direction with a Sobel filter; pick a glyph whose dominant orientation matches.

**Looks like:** stylized line drawing. Hand-drawn, sketchy.

**Cost:** moderate, similar to `shape`.

**Best with:** high-contrast subjects with clear edges. Less effective on smooth gradients.

> **Under the hood**
> The 6-D shape vector for a cell:
> ```
> v[0] = mean       (overall intensity)
> v[1] = top mean − bottom mean      (vertical gradient)
> v[2] = left mean − right mean      (horizontal gradient)
> v[3] = NE mean − SW mean           (diagonal 1)
> v[4] = NW mean − SE mean           (diagonal 2)
> v[5] = corner mean − centre mean   (centroid eccentricity)
> ```
> Each glyph's atlas bitmap is reduced to the same 6-D vector at atlas-load time. K-d tree built once, queried per cell. Distance metric is L2.
>
> `shape-edge-aware` adds an extra term: cells with high local gradient magnitude (Sobel) get a bonus for atlas glyphs whose vector has high `|v[1]| + |v[2]| + |v[3]| + |v[4]|` (any of the directional axes is strong).

---

## 8. Postprocess Catalog

Twelve stages. Only `vignette` is enabled by default. Each stage's sub-folder exposes its own parameters.

| Stage | Phase | Cost | Effect |
|---|---|---|---|
| `vignette` | B (composite) | ~5 ms | Radial darkening at edges. Reads as a frame around the canvas. |
| `bloom` | A (imageData) | ~80 ms | Bright cells bleed glow into surroundings. |
| `halation` | A (imageData) | ~60 ms | Warm ring around bright highlights. Film-style. |
| `scanlines` | B | ~10 ms | Horizontal line pattern. CRT line aperture. |
| `chromaticAberration` | A | ~40 ms | RGB channel offset at the edges. Lens distortion feel. |
| `phosphorDecay` | B | ~5 ms | Per-frame fade of bright cells. Phosphor persistence. |
| `depthFog` | A | ~50 ms | Tints distant cells toward a fog colour. |
| `crtBeam` | B | ~15 ms | Lottes / Blur Busters 2024 CRT shader. Aperture-grille slot mask + beam scan. |
| `godRays` | A | ~70 ms | Radial light-shaft beams from bright spots. |
| `barrel` | A | ~30 ms | Light radial pincushion / barrel warp (CRT bulge). |
| `letterbox` | B | ~3 ms | Top/bottom letterbox bars in palette bg colour. |
| `kawaii` | B | ~10–30 ms | Hearts + sparkles + twinkles overlay. Pairs with kawaii-pink palette. |

**Combinations:**
- `vignette + scanlines + crtBeam` = full CRT terminal
- `vignette + bloom + halation` = warm glow / phosphor
- `kawaii` alone or with `bloom` = soft pink overlay
- `vignette + chromaticAberration + barrel` = analog TV distortion

> **Under the hood**
> Phase A stages share a single `getImageData` round trip: imgData read once, mutated by each enabled Phase A stage in order, written back. Phase B stages composite directly onto the canvas with `globalCompositeOperation`. Toggling any Phase A stage on triggers the round trip cost (~20 ms minimum). All-Phase-B configurations stay under 15 ms total postproc, which is why "vignette only" is the cheap default.

---

## 9. Recipes

Six configs that come up often.

### Recipe 1 — Convert a GIF to glyph art (the kaneki workflow)
1. Drop your animated GIF onto the window. The status bar shows `gif=N/M`.
2. Color folder: `palette = cream-paper`, `colorMode = monochrome`.
3. Selection (advanced): `selectionMode = shape-edge-aware`, `glyphSet = ascii`.
4. Animation: `duration = 2.7` (or whatever your source loops at), `fps = 30`.
5. Export: pick `output size = 720`, click `Export MP4`.

Result: ~4 MB MP4, IG-compatible, clean loop.

### Recipe 2 — Clean cream-paper portrait
1. Drop a portrait photo.
2. Color: `palette = cream-paper`, `colorMode = monochrome`.
3. Mapping: `ramp = gradient`, `brightnessGamma = 0.55`.
4. Selection: `selectionMode = brightness` (fast, default).
5. Postprocess: leave `vignette` on (default), everything else off.

Result: ink stipple on aged paper. Click `Snapshot PNG`.

### Recipe 3 — Phosphor terminal aesthetic
1. Drop any image.
2. Color: `palette = phosphor`, `colorMode = duotone`.
3. Selection: `selectionMode = brightness`.
4. Postprocess: enable `scanlines` (period 2), `crtBeam` (intensity 0.4, slotMask on), `vignette`.

Result: Mr-Robot terminal vibe.

### Recipe 4 — Kawaii overlay with hearts
1. Drop any image.
2. Color: `palette = kawaii-pink`, `colorMode = gradient`.
3. Postprocess: enable `kawaii` (intensity 0.85, heartCount 14), `bloom` (radius 4, strength 0.6).

Result: pink wash + heart overlay. Pair with kawaii-pink palette for full effect.

### Recipe 5 — Spice / amber CRT
1. Drop any image (works best with warm-toned subjects).
2. Color: `palette = spice`, `colorMode = gradient`.
3. Selection: `selectionMode = shape-edge-aware`, `glyphSet = octant`.
4. Postprocess: `vignette` strong (0.7), `bloom` (radius 5, strength 0.4), `halation` (strength 0.4).

Result: Arrakis-orange textured wash.

### Recipe 6 — Maximum performance (slow hardware)
Aim: keep `total ms` ≤ 16 ms (60 fps).

1. Grid: `cols = 160`, `rows = 80` (12,800 cells, half default).
2. Selection: `selectionMode = brightness` (avoid shape modes).
3. Color: `colorMode = monochrome` (avoid gradient OKLab math per cell).
4. Postprocess: only `vignette`, nothing else.
5. Dither: `temporal` is fine; avoid `floydSteinberg / atkinson` (error diffusion is per-cell sequential).

---

## 10. Tips & Troubleshooting

#### "Output GIF is shorter than I expected"
The Export folder's `length` monitor shows the post-quantization length. GIFs round delays to centiseconds, so a `duration = 6.0` at `fps = 30` quantizes to `delayMs = 30 ms` and `frames = 90 → length = 2.700 s` — wait, that's not 6 s. Right, `fps = 30` → 33.33 effFps → 200 frames over 6 s, so 200 frames at 30 ms = 6.000 s. The math checks out. If your length is wrong, check the `frames` monitor matches your expectation.

#### "Glyphs look wrong / cell pattern is visible at small font sizes"
Pump `font.size` until characters overlap their cell bounds (~94% horizontally is the sweet spot). At size 8 with 4.27-px cells, characters bleed into neighbours and the column stripe pattern dissolves.

#### "App feels laggy"
Open the Perf folder. `total ms > 33 ms` → drop one of: cell count, postproc Phase A stages, shape mode → brightness mode. The `select ms` and `draw ms` monitors pinpoint which stage of grid rendering is the bottleneck.

#### "Switching glyphSet doesn't change anything visually"
Check `selectionMode` is set to a shape mode (`shape`, `shape-edge-aware`, `edge-directional`). Brightness mode ignores `glyphSet` and uses `ramp` instead.

#### "Dither dropdown does nothing"
Dither only applies in `selectionMode: brightness`. In shape modes, dither is bypassed (or runs as a source prefilter only when `asSourcePrefilter` is on for `temporal`).

#### "Vignette looks weird with bloom"
Bloom is Phase A (modifies imgData), vignette is Phase B (composite). Both run after the grid. The interaction depends on order — bloom before vignette means the vignette darkens already-bloomed cells, which can crush highlights. If it looks wrong, try lowering bloom strength.

#### "Animated GIF source plays at wrong speed"
The studio reads per-frame delay from the GIF. If the GIF was authored with broken delays (e.g. all 0 ms), the studio defaults to ~10 fps for those. Check the source GIF in QuickTime first — if it plays at correct speed there, the studio will too.

#### "MP4 export fails with `frame N produced no slice NALs`"
This was a known bug on the v1 ship (commit `45f5898`) caused by openh264's default rate control skipping low-motion P-frames. The fix is `RateControlMode::Off` in the encoder config. If you see this error, you're running an older build — rebuild from `main`.

---

## 11. Glossary

**Cell** — one square in the `cols × rows` grid. The unit of glyph placement.

**Glyph** — a single typeset character. Drawn into a cell.

**Atlas** — pre-rasterised bitmap of all glyphs in a glyph set, one tile per glyph. Built once at startup.

**k-d tree** — spatial data structure for fast nearest-neighbour lookups in `k` dimensions. Used for the 6-D shape-vector glyph match.

**Sprite atlas** — the rendered atlas used for fast `drawImage` blits. White-on-transparent so per-cell tint applies via canvas composition.

**Dither** — controlled noise added to a signal before quantization, distributing the rounding error across pixels/cells/frames so it reads as texture instead of banding.

**Ramp** — ordered sequence of glyphs by visual density (light → dense). The lookup table for brightness mode.

**Sigma** (in shape vector terms) — the standard deviation of mass distribution across the cell. One axis of the 6-D vector.

**OKLab / OKLCH** — perceptually uniform colour space. Used for palette interpolation so transitions stay saturated through their hue arc.

**EMA (Exponential Moving Average)** — smoothing filter for the per-cell brightness signal. Driven by `emaAlpha`. The basis of the "breathing" effect.

**Breathing** — the studio's per-frame motion: EMA + global gain pulse + per-cell jitter. Makes static images "live".

**Signal** (in this codebase) — the per-cell brightness value after EMA smoothing, gamma, and dither, in `[0, 1]`. The input to all selection modes.

**Sample** — one pixel-level read from the source image. Cells perform many samples (or one, with `nearest`) when computing their signal.

**Postproc** — the postprocess chain that runs after the glyph grid renders. Two phases (A: imgData, B: composite).

---

## 12. Reference Tables

### Palettes

| Name | Bg hex | Inks (count) | Vibe |
|---|---|---|---|
| `monochrome` | `#000000` | 1 | Pure b/w |
| `phosphor` | `#001100` | 2 | Terminal green |
| `bauhaus` | `#F2EBDA` | 3 | Bauhaus primaries on cream |
| `lovespark` | `#000000` | 3 | Neon pink + cyan |
| `mono-amber` | `#1A0D00` | 1 | Single amber on near-black |
| `cyber-phosphor` | `#020609` | 4 | Hacker terminal cyan |
| `amber-phosphor` | `#0A0500` | 4 | Vintage warm CRT |
| `bone-charcoal` | `#0A0907` | 4 | Hand-drawn study |
| `cream-paper` | `#E8DDC8` | 1 | Ink stipple aesthetic |
| `silver-charcoal` | `#0A0907` | 5 | Charcoal → silver gradient |
| `spice` | `#0A0302` | 4 | Arrakis melange |
| `kawaii-pink` | `#FFF0F5` | 5 | Cream → magenta |

### Ramps

| Name | Length | Use case |
|---|---|---|
| `classic` | 11 | Traditional ASCII |
| `dense` | 15 | Smoother tonal range |
| `sparse` | 6 | Heavy minimalist |
| `unicode-block` | 5 | Block-shaded gradient |
| `gradient` | 22 | Default for cream-paper |
| `gradientNoSpace` | 21 | Every cell gets a glyph |
| `blockShaded` | 4 | No stroke alignment |
| `blockAscend` | 8 | No cell baselines |
| `radial` | 9 | Radial-symmetric subjects |

### Glyph sets

| Name | Codepoints | Cell density |
|---|---|---|
| `null` | (falls back to brightness) | — |
| `ascii` | 0x20–0x7E | mid |
| `asciiDense` | ASCII + box-drawing | mid-high |
| `blockElements` | U+2580–U+259F | high |
| `braille` | U+2800–U+28FF | highest |
| `sextant` | U+1FB00–U+1FB3B | very high |
| `octant` | U+1CD00–U+1CDE5 | highest, modern only |

### Dither modes

| Name | Type | Cost | Best for |
|---|---|---|---|
| `none` | — | 0 | Hard quantization |
| `bayer4` | Spatial ordered | low | Static images, retro feel |
| `bayer8` | Spatial ordered | low | Smoother than bayer4 |
| `blueNoise` | Spatial blue noise | low | Less structured grain |
| `temporal` | Temporal Bayer8 | low | **Default** — animates the grid |
| `stbn` | Spatiotemporal blue noise | low | Smoothest temporal variation |
| `floydSteinberg` | Error diffusion | mid | Sharp edges |
| `atkinson` | Error diffusion | mid | Lighter, original Mac dither |
| `jarvisJudiceNinke` | Error diffusion | mid | Smoothest gradients |

### Postprocess stages

| Name | Phase | Cost band |
|---|---|---|
| `vignette` | B | 1 (cheap) |
| `letterbox` | B | 1 |
| `phosphorDecay` | B | 1 |
| `scanlines` | B | 1 |
| `kawaii` | B | 2 |
| `crtBeam` | B | 2 |
| `barrel` | A | 3 |
| `chromaticAberration` | A | 3 |
| `depthFog` | A | 3 |
| `halation` | A | 4 |
| `godRays` | A | 4 |
| `bloom` | A | 5 (most expensive) |

---

## 13. Performance Notes

**Default config (240×120, brightness mode, cream-paper, vignette only):** ~11 ms total. Plenty of headroom for 60 fps live preview.

**Shape-edge-aware over ASCII at 240×120:** ~42 ms. 30 fps target hit if no Phase A postproc is enabled.

**Animated GIF source playback:** adds ~50 ms steady-state per source-frame swap. Mostly the time `setFrame` spends decoding the next GIF frame's pixels. Cached after first pass.

**At 480×240 (115,200 cells):** the canvas2D path hits ~40 ms grid render. WebGL renderer (designed in `WEBGL-RENDERER-DESIGN.md`) is the path forward when this becomes a bottleneck. Currently not implemented — drop cell count instead.

**Frame budget for 30 fps:** 33 ms. Drop in priority order if you exceed:
1. Postproc Phase A stages (each one hides a 20–80 ms imgData round trip).
2. Cell count (cut `cols` and `rows` proportionally; cost is linear).
3. Shape mode → brightness mode (drops ~30 ms).
4. Dither mode `floydSteinberg / atkinson / jarvisJudiceNinke` → `temporal` (error diffusion is sequential per-cell).

---

## 14. Keyboard / Mouse Quick Reference

| Action | How |
|---|---|
| Load image | Drag onto window, or `Pick image…` button |
| Save preset | `Save current` button in Presets folder |
| Load preset | `Load` dropdown in Presets folder |
| Export PNG | `Snapshot PNG` button in Export folder |
| Export GIF | `Export GIF` button in Export folder |
| Export MP4 | `Export MP4` button in Export folder |
| Open devtools (dev builds only) | Cmd+Opt+I |
| Reload (production builds may block) | Cmd+R |
| Quit | Cmd+Q |

Right-click on the canvas → standard context menu (mostly empty in production builds).

---

## 15. Where Files Go

| Output | Path |
|---|---|
| `Snapshot PNG` | User picks via save dialog (default: `~/Downloads/glyph-frame.png`) |
| `Export GIF` | User picks via save dialog (default: `~/Downloads/glyph-loop.gif`) |
| `Export MP4` | User picks via save dialog (default: `~/Downloads/glyph-loop.mp4`) |
| Presets (saved) | Browser localStorage (key prefix `glyphgrid:preset:`). Per-machine only. |
| Preset JSON export | User picks via save dialog |
| The app itself | `~/Applications/Glyph Grid Studio.app` (or `/Applications/` if installed system-wide) |
| Source-image cache | None — everything is held in memory |

---

## Where to go next

- `README.md` — project overview, algorithmic notes, citations
- `BUGS_AND_ITERATIONS.md` — engineering log of every bug fixed
- `WEBGL-RENDERER-DESIGN.md` — design notes for the v0.5+ WebGL renderer
- `TODO.md` — roadmap items queued for after v0.1

If something in this guide doesn't match what you see in the app, the app is the ground truth — the panel folder is built from `CONFIG`, so any new setting will appear in the panel before it appears here. File an issue and the doc gets updated.
