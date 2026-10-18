# plan.md — charcoal mode in Glyph Grid Studio

**Status: DRAFT — do not implement yet.** Based on `research.md` in this folder. Annotate inline; implementation starts on "implement it all".

**Goal:** the charcoal look becomes a first-class studio mode (`selectionMode: 'charcoal'`), reachable from the GUI, CLI, batch, MCP, presets and the timeline, at least **5× faster than the skill** (Pietà 72-frame GIF: 63.7 s → ≤ 12 s) with every existing mode byte-identical and the v0.2 gates green.

## Principles

1. **Opt-in, zero cost when off.** No new work on any existing code path unless `selectionMode === 'charcoal'`. Archive golden stays PSNR inf; byte-identity G1–G9 unchanged.
2. **Static once, animate cheap.** Everything derived from the image is computed once per (source revision, source frame index, charcoal config key) into typed arrays. A frame is: one cached paper-grain blit + dust blits + one transformed sprite blit per mark.
3. **Deterministic.** Marks, jitter phases and dust are pure functions of (cell, seed, frameIdx). Charcoal renders join the byte-identity gate.
4. **Reuse, don't fork.** Paper-distance signal plugs into the existing sampling stage; Sobel comes from `glyph-edge.js`; sprites from the `_ensureRampSprite` family; capture/encode/CLI/MCP/schema are untouched machinery.

## Phase A — measure and pin parity (S)

- [ ] Commit the Pietà and Paul references: skill output still + 72-frame GIF + timings (`research.md` §2) under `tests/charcoal/` (small posters only; large GIFs stay out of git).
- [ ] Add a `tests/charcoal-parity.sh` that renders the studio's charcoal still and reports SSIM vs the skill's still plus a side-by-side PNG for review. Gate: SSIM ≥ 0.85 (UNCERTAIN; set after the first comparison) and owner sign-off on the side-by-side.

## Phase B — the static layer cache (M)

- [ ] `src/lib/glyph-charcoal.js` (pure, node-testable like `glyph-timeline.js`): `buildCharcoalField(rgba, w, h, cols, rows, cfg) → { idx: Uint8Array, hatch: Uint8Array, buildup: Uint8Array, alpha: Uint8Array, edgeMask: Uint8Array, emitters: Uint32Array }`. Signal = distance to paper colour with `lum^1.10` and gamma 0.62; hatch override from `GlyphGrid.edge.sobel` (persistent buffers); outline mask + flood-fill hole closing + edge-radius emitters for dust.
- [ ] Cache it under the existing `__sourceSignalCache` scheme with a charcoal key (palette, cell size, gamma, dust mode, edge radius). Still sources: computed once. Animated/video/text sources: once per source frame, reusing the ITER-052 layer memo.
- [ ] Per-cell constants precomputed once per grid size: static jitter (dx, dy, rot) and the three oscillation phases from `cellHash` → `Float32Array`s. Sine via a 1,024-entry LUT.

## Phase C — the fast draw path (M)

- [ ] Charcoal sprite atlas: ramp + hatch glyphs (`- | / \`) + dust glyphs, rasterised once per (font, size, ink) through the existing sprite helpers.
- [ ] Per frame, per mark: `ctx.setTransform(c, s, -s, c, x, y); ctx.drawImage(atlas, tileX, 0, tw, th, -tw/2, -th/2, tw, th)` with `globalAlpha` from the cached alpha. Shadow buildup is a second blit from the same arrays. No p5 push/pop, no `color()` per cell, no `fillText`.
- [ ] Measure at 1,400 px / 6 px cells (`__pfAdd` timers + `tests/perf-sample.sh`). If the mark pass exceeds 25 ms/frame, switch to **pre-rotated tiles**: K = 16 angle buckets per glyph baked into the atlas, untransformed blits (keeps the ±16° range visually smooth; verify on the side-by-side).
- [ ] Paper grain: baked once into an offscreen canvas per (size, palette, seed), one `drawImage` per frame.
- [ ] Dust: loop over the cached `emitters` only; particle position/alpha from `(t/loop + phase) mod 1`; skip specks over the figure via `edgeMask`. Same atlas.
- [ ] `irPut` for every mark so TXT/ANSI/cast/HTML export works in charcoal mode (rotation is dropped in text formats — documented).
- [ ] `frameIsTimeDependent()` returns true for charcoal (marks breathe), so the idle detector never parks it; reduced motion pins `t = 0` for still sources (marks freeze, dust hidden), matching ITER-048.

## Phase D — surface (S–M)

- [ ] CONFIG: `selectionMode: 'charcoal'` + `charcoal: { cell, gamma, contrast, jitter: {pos, rot}, breathe: {pos, rot}, buildup: {threshold, alpha}, grain: {density}, dust: {mode: 'edge'|'all'|'off', perCell, rise, drift, alphaMax, edgeRadius} }`, defaults = the skill's tuned values. Palettes `charcoal`, `y2k-pink`, `y2k-pastel`, `y2k-neon` added to the palette table.
- [ ] Schema regenerated (`scripts/gen-schema.js`), so presets/manifests/MCP validate the new keys; timeline tracks work on any `charcoal.*` number (e.g. animate dust rise).
- [ ] Preset `presets/charcoal.json` (the Paul look). CLI one-liner documented: `glyph-grid-studio render --in photo.jpg --preset charcoal --frames auto --out x.gif`.
- [ ] GUI: a "Charcoal" folder in the Tweakpane panel, shown when the mode is selected.
- [ ] GIF size: spike a palette-size option. Either feed gifski pre-quantised 16-colour frames, or add `gifColors` that quantises in the encoder worker before `add_frame_rgba`. Target: Pietà charcoal GIF ≤ 8 MB at 600 px.

## Phase E — skill + prepare step (S)

- [ ] `prepare_figure.py` stays external (rembg's 170 MB model is not bundled). Add `--prepare` to the CLI only as an optional local subprocess call when `python3 -c "import rembg"` succeeds; otherwise a clear message pointing at the script.
- [ ] Rewrite `SKILL.md` to drive the studio CLI (`render --preset charcoal`) once Phase C passes parity; keep `render.mjs` + `index.html` as a documented fallback for one release, then delete.

## Phase F — gates and docs (S)

- [ ] Full gate: cargo test, npm test (new `glyph-charcoal` unit tests: signal, hole fill, emitters, determinism), `tests/run-all.sh` (new charcoal cases: still, GIF, terminal export, timeline track), byte-identity with a charcoal job added, archive golden PSNR inf.
- [ ] Perf record in `tests/PERF-V0.2-CHARCOAL.md`: skill vs studio on Pietà + Paul, still and GIF, wall + WebContent/GPU CPU.
- [ ] BUGS_AND_ITERATIONS entries per landed item; README/USER-GUIDE/site copy for the new mode; gallery plate for the Paul piece rendered by the studio.

## Success metrics

| Metric | Skill today | Target |
|---|---|---|
| Pietà 72-frame 600 px GIF wall | 63.7 s | ≤ 12 s |
| Pietà still | 6.9 s | ≤ 2 s |
| Pietà GIF size | 14.66 MB | ≤ 8 MB |
| Existing-mode output | — | byte-identical (archive golden inf, G1–G9) |
| Charcoal determinism | not tested | byte-identical across runs |

## Open questions for Joona

1. **Mode or preset?** Plan makes charcoal a selection mode (full controls). Alternative: only a preset over existing modes — simpler but loses rotation, hatching and dust.
2. **Parity bar:** accept small rasterisation differences vs the skill (side-by-side review), or must it match pixel-close?
3. **Keep the y2k pink palettes** from the skill as studio palettes?
4. **`--prepare`:** wire the optional rembg pre-step into the CLI, or leave it as a separate script?

**Do not implement yet.**
