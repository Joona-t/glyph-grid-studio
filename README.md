# Glyph Grid Studio

A real-time character-grid image renderer for macOS. Drop in any image and convert it to a tunable mosaic of typeset Unicode glyphs — ASCII, block elements, octants, sextants, braille — with live OKLab-palette interpolation, spatiotemporal blue-noise dithering, and a Lottes-style CRT-beam postprocess.

[![Download](https://img.shields.io/badge/Download-macOS-E07A4D?style=for-the-badge)](https://github.com/Joona-t/glyph-grid-studio/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-A3B18A.svg)](LICENSE)
&nbsp;**Free · open source · macOS 11+ (Intel + Apple Silicon)**

🌐 **Website & download:** https://lovespark.love/glyph-grid-studio/

## Download

1. **[Download the latest release](https://github.com/Joona-t/glyph-grid-studio/releases/latest)** (`.dmg`), open it, drag **Glyph Grid Studio** into **Applications**.
2. **First launch (one time only).** The app is free and open source but isn't signed with a paid Apple Developer certificate, so macOS blocks the first open. To allow it:
   - **Right-click** (or Control-click) the app → **Open** → **Open** again in the dialog, **or**
   - **System Settings → Privacy & Security** → scroll down → **"Open Anyway"**, **or** run:
     ```bash
     xattr -dr com.apple.quarantine "/Applications/Glyph Grid Studio.app"
     ```

**Requirements:** macOS 11 (Big Sur) or later. Universal binary — runs natively on both Apple Silicon and Intel Macs.

Prefer to build it yourself? See [Build from source](#build) below.

## What it does

The pipeline samples a source raster image into an `M × N` cell grid. For each cell, the renderer either:
- Picks a glyph from a luminance ramp (classic ASCII / block-shaded / gradient ramps), or
- Performs a 6-D shape-vector nearest-neighbor lookup against a pre-baked Unicode atlas (octants, sextants, blocks, braille) — the "shape-edge-aware" mode

The selected glyph is typeset into the cell, in a chosen palette and color mode. The output is a fully-composed character-grid — every pixel on the canvas is part of a typeset Unicode character.

## Tech stack

- **Front end:** [p5.js](https://p5js.org) 1.11 (single-file sketch)
- **Native shell:** [Tauri 2.x](https://tauri.app) (Rust backend, WKWebView front)
- **Rust crates:** `gifski` (high-quality palette GIF encoding, adaptive Twitter-fit shrink ladder), `openh264` + `mp4` (native MP4/H.264 export), `image`, `base64`, `tauri-plugin-dialog`, `tauri-plugin-fs`
- **UI:** [Tweakpane v3](https://cocopon.github.io/tweakpane/) — 70+ live-tunable bindings
- **Binary size:** ~10 MB arm64 macOS .app

## Features

| Axis | Options |
|---|---|
| Palettes (11) | monochrome, phosphor, bauhaus, lovespark, mono-amber, cyber-phosphor, amber-phosphor, bone-charcoal, cream-paper, silver-charcoal, spice |
| Color modes (4) | preserve, monochrome, duotone, gradient |
| Ramps (9) | classic, dense, sparse, unicode-block, gradient, gradientNoSpace, blockShaded, blockAscend, radial |
| Dithering (9) | none, bayer4, bayer8, blueNoise, **STBN** (NVIDIA EGSR 2022), temporal, floydSteinberg, atkinson, jarvisJudiceNinke |
| Selection (4) | brightness, shape, shape-edge-aware, edge-directional |
| Glyph sets (7) | ASCII, asciiDense, blockElements, braille, sextant, octant, ramp-only |
| Postprocess (8) | vignette, bloom, halation, scanlines, chromaticAberration, phosphorDecay, depthFog, **CRT-beam** (Lottes / Blur Busters 2024) |
| Animation | breathing-cell EMA, per-frame gain pulse, per-cell jitter — runtime-tunable |
| Export | PNG snapshot · animated GIF89a · MP4 (H.264) · preset JSON · share URL |

## Algorithmic notes

- **6-D shape-vector matching** (Alex Harri 2024): each glyph is encoded as 6 floats describing local mass distribution; per-cell lookup picks the closest match via k-d tree. Matches CNN-quality at ~1% the cost (Chen et al. arXiv 2503.14375, 2025).
- **STBN dithering**: a Halton(2,3)-driven approximation of NVIDIA's Spatiotemporal Blue Noise. Smoother per-frame variation than hash-jittered Bayer without shipping a 50 KB texture asset.
- **OKLCH palette interpolation**: cylindrical OKLab interpolation along the shorter hue arc, so palette transitions across distant hues stay saturated instead of dipping through gray.
- **Native GIF/MP4 encoding**: frames go straight from the JS canvas → Rust via base64 IPC. GIF through `gifski` (quality-100 palette encoding; the Twitter-fit path retries at 600→540→480→420→360 px until the output fits the 15 MB cap). MP4 through `openh264` + the `mp4` muxer for Instagram-compatible loops.

## Two ways to use it

### As a human — visual studio (GUI mode)

Drop image into the window, slide sliders, watch the canvas update live, click Export GIF when you have a look you like. Default behavior when launched with no arguments.

### As an agent / script — headless CLI

Same rendering pipeline, no GUI. Runs in an off-screen Tauri window, exits when the GIF is written.

```bash
# Single render with explicit flags
glyph-grid-studio render \
  --in ~/Downloads/portrait.jpg \
  --out ~/portrait-warm-bone.gif \
  --palette cream-paper \
  --color-mode monochrome \
  --ramp gradient \
  --dither stbn \
  --selection-mode shape-edge-aware \
  --glyph-set octant \
  --frames 24

# List every available palette / ramp / dither / glyph set
glyph-grid-studio catalog | jq

# Batch: render N variants of one or more sources in a SINGLE app session
# (~10x faster than per-variant render calls — startup + decode paid once).
# Manifest shape: {"in": "<default source>", "frames": N, "perf": false,
#   "jobs": [{"name", "out", "format": "gif|mp4", "in": "<per-job source>",
#             "capWidth": 720, "targetMaxBytes": 15728640, "config": {...}}]}
# capWidth 720 auto-targets Twitter's 15 MB cap (adaptive shrink ladder).
glyph-grid-studio batch --manifest variants.json

# Help
glyph-grid-studio render --help
glyph-grid-studio batch --help
```

### As a Claude / Cursor tool — MCP server

Wire it into any MCP-aware AI client and the renderer becomes a native tool. Edit `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "glyph-grid-studio": {
      "command": "/Applications/Glyph Grid Studio.app/Contents/MacOS/glyph-grid-studio",
      "args": ["mcp"]
    }
  }
}
```

Restart Claude Desktop. Then in any chat:

> *"Render the image at ~/Pictures/sunset.jpg as a cream-paper monochrome glyph drawing using shape-edge-aware octant selection. Save it next to the original."*

Tools exposed: `glyph_grid_render`, `glyph_grid_catalog`. Full setup + agentic-exploration examples in [docs/mcp.md](docs/mcp.md).

## Build

```bash
# Prereqs: Rust 1.77+, Cargo, Xcode CLI tools
cd src-tauri
cargo tauri build       # ~40 s incremental, ~3 min cold
# output: src-tauri/target/release/bundle/macos/Glyph Grid Studio.app
```

To install:
```bash
ditto "src-tauri/target/release/bundle/macos/Glyph Grid Studio.app" \
      "$HOME/Applications/Glyph Grid Studio.app"
xattr -cr "$HOME/Applications/Glyph Grid Studio.app"
open "$HOME/Applications/Glyph Grid Studio.app"
```

The headless binary is at `Glyph Grid Studio.app/Contents/MacOS/glyph-grid-studio`. After v0.2's Homebrew cask, it will be on `$PATH` as `glyph-grid-studio`.

## Project layout

```
src/
  index.html            single-file p5 sketch (CONFIG, scenes, draw loop, testHook)
  lib/
    glyph-fonts.js      WOFF2 cascade + sentinel availability detection
    glyph-dither.js     ordered + error-diffusion + STBN dithering
    glyph-palette-morph.js  OKLab/OKLCH interpolation
    glyph-shape-index.js    6-D vectors + k-d tree NN selection
    glyph-crt.js        postprocess chain (vignette → CRT-beam)
    glyph-studio.js     Tweakpane bindings + drag-drop + recording + exports
    ...
  glyph-sets/           pre-baked atlas JSONs (ASCII, octant, sextant, braille…)
  fonts/                bundled WOFF2 (Cascadia + IBM VGA fallback)

src-tauri/
  src/lib.rs            Tauri commands: save_png, save_gif_real, save_gif_to_path,
                        pick_image, read_image_file, save_preset_json, load_preset_json
  Cargo.toml            tauri 2.10, gif 0.13, image 0.25
  tauri.conf.json       window 1500×820, dragDropEnabled, withGlobalTauri, devtools
  capabilities/         core:default, dialog, fs
```

## Test session

A 469-GIF Cartesian feature test was run during development to verify every code path renders cleanly. See `BUGS_AND_ITERATIONS.md` for the bug log + summary. The test driver functions (`runStudioPhase(N)` / `runAllStudioPhases()`) are still available in the build for regression testing.

## License

[MIT](LICENSE) — Joona Tyrninoksa, 2026
