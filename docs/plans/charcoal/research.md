# research.md — integrating the charcoal-sketch skill into Glyph Grid Studio

**Date:** 2026-10-18 · **Skill branch:** `skill/charcoal-sketch` (9238d93) · **Studio baseline:** v0.2.0 (`main`, ITER-045..084)

## 1. What the skill is

`skills/charcoal-sketch/` packages the "charcoal method" behind the Paul showcase plate:

| File | Role |
|---|---|
| `scripts/index.html` (1,002 lines) | Standalone p5.js renderer: paper grain, rising dust, jittered grid, directional hatching, shadow buildup. Loads p5 from a CDN. |
| `scripts/render.mjs` (318) | Zero-dependency driver: serves the page, drives headless Chrome over CDP, captures frames via a `/save` endpoint, stitches GIF/MP4 with ffmpeg. |
| `scripts/prepare_figure.py` (96) | Optional pre-pass: rembg person cut-out on white, pale-skin darkening, contour band, legible face oval. Needs `rembg` + a ~170 MB local model. |
| `SKILL.md`, `references/methods.md` | How to run it and how the look works. |

The look (from `methods.md`): signal = distance of each cell from the **paper** colour (`#EFE3CF`), not from black; `lum^1.10` contrast; gamma 0.62; a charcoal ramp ` .,'\`":;-+/\xtfLG$#@`; five layers per frame — paper grain, dust, jittered grid (±20 % position, ±7° rotation per cell), directional hatching (`- | / \` along Sobel edges), shadow buildup (a lighter offset mark under dark cells). Animation is a pure function of time: every mark oscillates on its own sine phase; dust position is `(t / loop + phase) mod 1`. Rendered at ~1,400 px with 6 px cells, then downscaled to 600 px.

## 2. Measured baseline (Pietà, 1274×2000 source)

| Run | Wall | Output |
|---|---|---|
| Skill, still (`--formats still --dust edge`) | **6.9 s** | 2.33 MB PNG |
| Skill, GIF (`--formats gif --dust edge --gif-colors 16`, 6 s @ 12 fps = 72 frames, 600 px) | **63.7 s** (~0.88 s/frame incl. stitch) | **14.66 MB** — at X's 15 MB limit even with 16 colours |
| Studio v0.2.0, worm 120 frames 760×1024 (`tests/perf-sample.sh`) | 15.2–15.7 s (~0.13 s/frame) | — |

The skill is ~7× slower per frame than the studio's headless path today.

## 3. Where the skill's time goes (read from `index.html`)

1. **Static work redone every frame.** `renderFrame(t)` (l.425) re-runs the scene, `src.loadPixels()`, and in `drawGlyphGrid` (l.623) re-samples every cell (`sampleCell`, a new object per cell), recomputes signal, ramp index, Sobel edge angle and the hatch override. For a still image none of that changes with `t` — only the jitter transforms do. `drawDust` (l.749) also re-samples every cell and re-runs the outline flood fill per frame.
2. **Per-mark p5 state churn.** Each mark is `push(); translate(); rotate(); text(); pop();` (l.710-714), plus a second set for shadow buildup (l.729-733), plus `color(palette.inks[0])` parsed per buildup cell (l.723). At 1,400 px / 6 px cells that is ~230 × 360 ≈ 80 k cells → up to ~160 k `fillText` calls per frame. The studio measured fillText at ~3.7 µs/call (PERF-AFTER.md) before ITER-014/016 moved its modes to sprite atlases.
3. **Paper grain** (`drawPaperGrain`, l.567) draws ~18 % of cells as text every frame although it never moves.
4. **Capture**: `toDataURL('image/png')` + HTTP POST per frame (`postCanvas`, l.457) — the PNG round trip ITER-052 removed from the studio.
5. **Dependencies**: p5 from jsDelivr (l.102) — the studio vendors p5 1.11.2 (ITER-063) and starts offline.

## 4. Studio pieces that already solve each cost

| Skill cost | Studio mechanism (v0.2.0) | Where |
|---|---|---|
| Re-sampling a still every frame | still-source layer memo + `__sourceSignalCache` (ITER-042/052) | `src/index.html` ~l.1052, ~l.2843 |
| Sobel per frame | `glyph-edge.js` `sobel` with persistent grow-only buffers (ITER-042) | `src/lib/glyph-edge.js:53` |
| `fillText` per mark | sprite atlases + `drawImage` (ITER-014/016; edge mono ITER-042) | `_ensureRampSprite` `src/index.html:3404`, `drawBrightnessGrid` :3571 |
| PNG capture | raw RGBA capture over IPC, streaming gifski (ITER-052/053) | `src/index.html` capture path, `src-tauri/src/lib.rs` |
| Nondeterministic warm-up | primed temporal state, byte-identity gate (ITER-046/047) | `primeTemporalState`, `tests/byte-identity.sh` |
| CDN p5 | vendored p5 (ITER-063) | `src/vendor/p5-1.11.2/` |
| Idle CPU | static-frame detector (ITER-062) — charcoal is time-dependent, so it must report itself in `frameIsTimeDependent()` (:1562) | |
| Terminal/SVG export | `GlyphFrame` IR (ITER-073) — charcoal marks are codepoints, so `irPut` (:1236) covers them (rotation is lost in text export, acceptable) | |
| Dissolve | `dispersal` (ITER-018) is a one-shot fade; charcoal **dust** is a different, looping plume — reuse its deterministic-hash style, not its code | `CONFIG.dispersal` :316 |

Not present in the studio: per-mark rotation, per-mark sine "breathing" of position/rotation, paper-distance signal, hatch-glyph override, shadow-buildup second mark, paper grain layer, rising-dust plume with outline mask + hole fill, a GIF palette-size option (gifski has `quality` only, `src-tauri/src/lib.rs:3299`).

## 5. Design constraints from "don't lose performance or optimisations"

- Existing modes must stay byte-identical: charcoal is **opt-in**; the archive golden (`golden-p3-iter057.mp4`) and byte-identity G1–G9 must not move.
- Charcoal output must itself be deterministic (pure function of source, config, seed, frame index) so it joins the byte-identity gate.
- Per-frame work for a still source must be **transforms + blits only**; everything image-derived is computed once per (source revision, source frame, config key) and cached.
- No new network dependency, no paid API (rule 10). `rembg` stays optional and local.

## 6. Options considered

1. **Ship the skill unchanged next to the app** (document it, keep render.mjs). Zero risk to the app, but keeps the 7× slower path, the CDN dependency and a second renderer to maintain. Rejected as the end state; kept as the fallback until parity is proven.
2. **Port the look as a studio rendering mode** (`selectionMode: 'charcoal'` + layers wired into the existing pipeline). Gets caching, sprites, raw capture, streaming GIF, CLI/MCP/schema/timeline/terminal export for free. Chosen.
3. **Run the skill's page inside the Tauri WebView as a second scene.** Faster to wire, but bypasses every cache and gate; same per-frame cost. Rejected.

## 7. Risks / unknowns (UNCERTAIN until measured)

- Rotated sprite blits: `setTransform(cos, sin, -sin, cos, x, y)` + `drawImage(tile)` per mark should be several times cheaper than p5 push/translate/rotate/text/pop, but ~160 k transformed blits per frame at 1,400 px may still cost tens of ms. Fallback: pre-rotated tile variants in K angle buckets (e.g. 16) so marks blit untransformed.
- Visual parity: sprite rasterisation differs from `fillText` sub-pixel AA (the ITER-042 edge-sprite gate accepted 32.8 dB). Parity gate must be a side-by-side review + SSIM threshold against the skill's own output, not PSNR=inf.
- GIF size: the skill's 16-colour trick needs either a quantised-palette path in gifski (quality/`Settings` don't expose palette size) or a pre-quantise step before the encoder. Needs a spike.
- `prepare_figure.py` depends on a 170 MB model download on first run; bundling it would bloat the app. Keep it external (documented pre-step, optionally invoked as a local subprocess by the CLI).
