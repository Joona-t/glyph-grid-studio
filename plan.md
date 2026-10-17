# plan.md — Glyph Grid Studio v0.2 "SOTA" plan

**Status: DRAFT — do not implement yet.** Built from `research.md` (27 verified findings F1–F27, 13 ranked features R1–R13). Joona annotates inline; implementation starts only on "implement it all".

**Target:** v0.2.0. Six phases, each shippable on its own and gated green (cargo test · npm test · `tests/run-all.sh` 24/24 + MCP 10/10 · PSNR gate · XPC-aware perf sampler). Branch-first, every commit carries the `Co-Authored-By` trailer, patch bump per landed phase. No paid LLM API anywhere (rule #10) — the only LLM touchpoint stays the existing MCP server driven by the user's local CLI.

---

## Guiding decisions

1. **Measure first, then move.** Phase 0 ships the instruments. Every later phase's before/after numbers come from the XPC-aware sampler, not `time`. Today's "throttled/idle" story was a measurement artefact (research §0).
2. **Determinism before GPU.** No WebGL/GPU-preprocess work is gated until headless output is bit-reproducible (F23 + F3). Otherwise every PSNR gate is comparing against self-noise.
3. **Headless path is the product for agents.** The CLI/MCP/batch surface gets the same CONFIG reach as the GUI, a schema, machine-readable errors, progress and cancel (F22–F27 + R7). That's what makes it the renderer pipelines reach for.
4. **Keep one renderer.** F5 confirms a Rust-native rasteriser is an XL port with no bit-exact path. We stay in the WebView and make it fast and deterministic instead.
5. **Features ride on the IR.** R1 (terminal export), R2 (timeline), R3 (typography), R4 (video) all want a per-cell `GlyphFrame` buffer and a `FrameSource` adapter. Build those two seams once in Phase 5 and the features are thin.

---

## Phase 0 — Instruments (effort S, ~1 day)

Goal: no perf number is ever reported without XPC CPU again; a reusable PSNR gate; a memory probe.

- [ ] `tests/perf-sample.sh <out.ndjson> -- <args>`: pgrep-diff the `WebKit.WebContent`/`WebKit.GPU` XPC pids, sample `ps -o pid,%cpu,time,state,pri,ni` at 1 Hz, emit `{wall_s, app_cpu_s, webcontent_cpu_s, gpu_cpu_s, frames}`. Wire into `run-all.sh` B10 and the `PERF-*.md` tables. (F1)
- [ ] `tests/psnr-gate.sh <new> <baseline> [min_db]` wrapping the ffmpeg PSNR command used today by hand; exits non-zero below threshold; prints per-frame min. (F7, F5 both ask for it)
- [ ] `GlyphGrid.memProbe()` dev call returning retained bytes by cache category (`_atlasCache`, `__sourceSignalCache`, sprite caches, recState). Caches are bounded (F20) — this closes the question with a number. (F20)
- [ ] Re-baseline the ITER-041 760×1024 / 120-frame run with the sampler; record WebContent vs GPU split in `tests/PERF-V0.2-BASELINE.md`. Decide from the split whether Phase 2's first lever is readback (GPU) or scene JS (WebContent).

## Phase 1 — Determinism (effort M, ~2 days)

Goal: headless output = pure function of (source, config, seed). Bit-identical across runs, `--show-window`, and consecutive batch jobs.

- [x] `primeTemporalState(total)` in `beginRecord`: reset `cellSignalEma`, phosphor state (expose `GlyphGrid.crt.resetState()`), then K=11 `signalOnly` scene passes over the loop tail (or K× frame 0 for stills). `signalOnly` early-return after the EMA stage (index.html ~:2125). (F23) — ITER-046 (phosphor reset is `crtState = null`, re-made lazily; no lib change needed)
- [x] Replace the 1500/300/200/120 ms sleeps with readiness promises (setup done · source decoded · config applied) — the #20 re-land, now safe. (F3, F23) — ITER-047 (`__setupReady` / `__assetsReady()`; also found and fixed the stray uncaptured p5 draw during backpressure, and pulled F21's GIF-autoplay pause forward)
- [x] Per-job `resetToBaseline()` + priming in batch so job N doesn't inherit job N-1's EMA. Document as an intentional baseline change in BUGS (ITER entry). — ITER-046 (every batch job goes through `beginRecord`, so it is primed; baseline change documented there)
- [x] Pin `_frameIdx`/`_seed`/dither phase to 0 under `prefers-reduced-motion` for still sources (WCAG 2.3.3). (F17 part 1) — ITER-048 (`_rmPinned`; live view only, recordings untouched)
- [x] Regression test: same job twice headless + once `--show-window` + a 2-job batch → decode PNG frames, assert RGBA byte-identity. Regenerate goldens once (`base2.mp4` etc.). — ITER-046 (`tests/byte-identity.sh`, run-all Phase G, G1–G7; golden regenerated, old one kept as `golden-v0.1.10-pre-phase1.mp4`)
- [ ] Bump → v0.1.11.

## Phase 2 — Headless throughput (effort M–L, ~4 days)

Goal: 120-frame CLI render from 18–56 s to a measured, reproducible number; exports stream; memory bounded by design not by a pixel cap.

Quick wins first:
- [ ] **MP4 rate control** (F11, S): `RateControlMode::Bitrate` + `skip_frames(false)` + `track_config.timescale` fix; `--mp4-quality upload|archive` (archive = today's QP-26 path). Cargo test on 120 synthetic frames asserting non-empty samples and size < 1.5× target. Expected 17 MB → ~3–4 MB.
- [ ] **Twitter-fit ladder predictor** (F14, S): first full pass → size/pixel ratio → one-shot width estimate, then at most one correction rung.

Then the capture path:
- [ ] **Raw RGBA capture** (F2 step 2, S→M): replace `canvas.toBlob('image/png')` with `getImageData` and ship RGBA over the existing raw-body IPC; Rust preprocess accepts RGBA (skips PNG decode). Measure with the sampler; PSNR gate vs Phase-1 goldens.
- [ ] **GPU-process canvas off for headless** (F2 step 1, M, behind `--gpu-canvas` flag, private WebKit prefs via objc2, degrade silently). Only if Phase 0's split shows the GPU process dominant.
- [ ] **Streaming GIF encode** (F12, L): gifski collector fed during capture, `.part` file, ladder re-decodes from retained PNG only on overshoot, cancel = `Error::Aborted`. Expected −35 % wall on GIF.
- [ ] **Bounded decode-ahead for GIF** (F13, M) with the ITER-040 reorder-buffer pattern; drop `MAX_CAPTURE_PIXELS`/`MAX_TOTAL_DECODED_PIXELS`, keep `MAX_EXPORT_FRAMES=600` + the 256 MB compressed cap. Peak RSS 930 MB → ~500 MB. **This closes the open owner question about the 100M-pixel cap: the cap goes away because the reason for it goes away.**
- [ ] **`batch --jobs N`** (F4, M): fork N CLI children over the manifest; expected ~3× on 20-job benches. Bench first on the Ghost manifest.
- [ ] Bump → v0.1.12.

Deferred from this phase: APNG chunk-copy export (F15, M) — nice but no user asked; VideoToolbox (F16) — speed gain is 1–7 % of wall, not worth it; revisit only for HEVC.

## Phase 3 — Headless surface as a product (effort M, ~3 days) — includes feature R7 (preset schema)

Goal: CLI/MCP/batch expose everything the GUI can do, validate inputs, report errors and progress like a real tool.

- [ ] **Schema as single source of truth** (R7): `scripts/gen-schema.js` reads live CONFIG defaults/enums → `src/glyph-grid.schema.json` (JSON Schema 2020-12). Rust validates `--preset`, batch job `config` and MCP args with path-qualified errors; `catalog_json()` becomes a projection of the schema; `validate <preset>` and `render --dry-run` subcommands; run-all Phase A gains a "schema up to date" check.
- [ ] **One config merger** (F24): `json_deep_merge` in Rust, `GlyphStudio.deepMerge` shared by `setConfig`/`applyConfig`; `--set <dotted.path>=<json>` repeatable; `--frames auto` (= duration×fps like the GUI); per-job `frames`/`border` in manifests; unreadable preset → exit 2, never silent defaults.
- [ ] **MCP tools** (F22 Tier A+B): `glyph_grid_batch` (inline manifest), `preset`/`config` object on `glyph_grid_render`, async tools with `tokio::process`, stderr `batch: N/M` → `notify_progress`. Tier C (resident warm worker) deferred until Phase 2 numbers say startup still dominates.
- [ ] **Machine-readable errors + bounded time** (F25): MCP sets `isError`, structured `{code, path, message}`; CLI exit codes 0/1/2/3 + `--timeout`.
- [ ] **Progress + cancel** (F26): NDJSON progress on stderr for CLI, SIGINT kills the WebKit child cleanly, MCP cancel kills the render child.
- [ ] **Manifest versioning + resume** (F27): `version`, schema validation before launch, `--resume` skips jobs whose output exists and matches.
- [ ] Update `docs/mcp.md`, README CLI section, `tests/mcp.py` (tool count, batch test, error test).
- [ ] Bump → v0.1.13.

## Phase 4 — Interactive + GPU (effort M–L, ~4 days)

Goal: the GUI is cheap when idle, starts offline, imports big GIFs without copying them four times; the GL path becomes gate-verifiable.

- [x] **Static-frame detector** (F17 part 2, S): `frameIsTimeDependent()` after paint; `noLoop()` when false and EMA converged; unknown ⇒ dynamic; config changes already re-arm. — ITER-062
- [x] **Vendor the 4 CDN scripts** (F19, M): p5, Tweakpane, JSZip, fflate into `src/vendor/` pinned, with licences; collapse the 3-stage font wait. App and CLI boot offline. — ITER-063 (CSP still open: tauri.conf.json is hook-protected)
- [x] **Import via asset protocol** (F18 steps 1+2, S+M): `read_image_file` returns metadata only, `loadImage(convertFileSrc(path))`; Rust `probe_image_file` rejects over-limit GIFs before p5 allocates (closes BUGS backlog #1). Spike: confirm `asset://` doesn't taint the canvas. — ITER-064 (spike: no taint, inf dB; step 3 still deferred)
- [ ] **p5 GIF autoplay off** (F21, S) so `setFrame()` can't race wall-clock advance.
- [x] **GL path, step 0 → 1** (F6): instrument `wave6.draw`/`drawImage`/`toBlob` split at 240×120 and 400×300; then move per-cell placement + gamma into the vertex shader (per-instance `signal`/`cellIndex` bytes). Make the shader output-equivalent to the sprite path (F7) and gate it with the PSNR script. Steps 2–3 (GL as presented surface, FBO postprocess) only if step 0 shows ≥5 ms on the fill loop at default. — ITER-065 (step 0 measured in tests/PERF-V0.2-GL.md: fill loop 0.1–1.0 ms ⇒ step 1 not done; path was dead since v0.1.6, now ≥51 dB vs CPU)
- [ ] **GPU source preprocess, Phase A** (F8, M): three fragment passes replacing the ~10 JS passes for animated sources; JS fallback when no webgl2; keyed by `_processedKey`.
- [ ] **imgData postproc Tier A** (F10, S): skip the full-canvas readback for scanlines/kawaii-only stacks.
- [ ] Skip WASM/SIMD (F9): ceiling is ~2.4 ms/frame at default. Noted, not scheduled.
- [ ] Bump → v0.1.14.

## Phase 5 — Features (the SOTA step)

Two seams first, then four features on top. Each feature ships behind its own flag with CLI/MCP/preset surface from day one (rule: headless parity).

### 5.0 Seams (effort M)
- [ ] **`GlyphFrame` IR** (from R6): `{cols, rows, cp: Uint32Array, fg: Uint8Array×3, alpha: Uint8Array, bgHex, font}` filled by every drawer (brightness/shape/edge/flow) at the point it already knows the pick. `captureGridSnapshot()` reads the IR → TXT/SVG work in every mode. Uint32 because octants are U+1CD00.
- [ ] **`FrameSource` adapter** (from R4): `{kind, durationMs, frameAt(tMs), revision}`; still + GIF become adapters; `__sourceFrameIdx` memo busting unchanged.

### 5.1 Feature R1 — Terminal-native export (effort M)
`--format txt|ansi|cast|html` from GUI, CLI, batch and MCP, every selection mode. Pure module `src/lib/glyph-text-export.js` (`toTxt`, `toAnsi` with RLE 24-bit colour, `toCastV2` asciinema events, `toHtml` single-file with inlined WOFF2 + reduced-motion stepper). Rust `save_text_export`. Default octant/sextant sets to `ascii|blockElements` for ANSI with a compat warning. Why first: it's the "agents can read the output as tokens" feature and needs only the IR.

### 5.2 Feature R2 — Parameter timeline / keyframes (effort M)
`CONFIG.timeline.tracks` keyed by dotted CONFIG path, eased lerp for numerics, OKLab blend for palette tracks (finally consumes `glyph-palette-morph.js`), `ease:'step'` forced for structural keys. Evaluated at top of `draw()` into a shadow `CONFIG_FRAME` so Tweakpane keeps base values. Deforum-style `0:(0.2), 0.5:(0.8)` strings on the CLI; Tweakpane "Timeline" folder with a `u` scrub in v1 (no custom canvas timeline yet). Must evaluate during Phase-1 priming draws.

### 5.3 Feature R3 — Kinetic typography & lyric sources (effort M)
`CONFIG.textSource` with lines `{text, start, end, font, size, anim: typewriter|rise|fade|scale}`; `.srt`/`.lrc` parsers (pure, tested); offscreen canvas rendered via the `FrameSource` adapter, optional composite over an image/video source before linearisation so letters become glyph mass. CLI `render --text 'DROP' --srt lyrics.srt`. Minimum computed stroke size with a status-bar warning.

### 5.4 Feature R4 — Video & webcam sources (effort L)
GUI tier: `<video>` via `convertFileSrc`, `requestVideoFrameCallback` for preview, seek-per-frame (`currentTime = t; await seeked`) while recording so exports are frame-exact. Headless tier: Rust `video_open`/`video_frame` with the `mp4` demuxer + `openh264` decoder, raw RGBA over IPC (H.264 first; HEVC falls back to the WebView path). Camera via the same adapter; needs `NSCameraUsageDescription` — prototype the permission path first.

### 5.5 Next tier (not in v0.2; listed so they aren't lost)
R5 audio-reactive modulation (needs R2's evaluator; AAC passthrough question), R6 full glyph-stream `.ggs` + Rust `rerender` to 4K/PDF/SVG (ab_glyph + printpdf), R8 motion-compensated temporal coherence, R9 TTF→atlas import, R10 agent job protocol Tier C (warm worker), R11 looks gallery/share links, R12 structure-aware matching, R13 zero-touch distribution (GitHub Actions release + Homebrew tap — cheap, do it whenever a release day is quiet).

- [ ] Bump → **v0.2.0** after 5.1–5.4; update README feature list, USER-GUIDE, `docs/mcp.md`, landing page copy, Cask, GitHub Release.

---

## Order of work and the why

```
Phase 0 instruments ─▶ Phase 1 determinism ─▶ Phase 2 throughput ─▶ Phase 3 headless product
                                     └────────────────────────────▶ Phase 4 GUI/GPU (parallel with 2–3 once goldens exist)
Phase 5.0 seams ─▶ 5.1 terminal export ─▶ 5.2 timeline ─▶ 5.3 typography ─▶ 5.4 video
```

Phases 2, 3 and 4 touch disjoint files after Phase 1 (Rust export vs CLI/MCP vs index.html render loop) and can run as separate sessions/worktrees. Phase 5 waits for Phase 1 (goldens) and 3 (schema, so every feature's CONFIG keys are validated from day one).

## Effort summary

| Phase | Effort | Lands as |
|---|---|---|
| 0 Instruments | S (1 d) | v0.1.10 → tests only |
| 1 Determinism | M (2 d) | v0.1.11 |
| 2 Throughput | M–L (4 d) | v0.1.12 |
| 3 Headless product + schema | M (3 d) | v0.1.13 |
| 4 GUI + GPU | M–L (4 d) | v0.1.14 |
| 5 Seams + 4 features | L (6–8 d) | v0.2.0 |

Roughly three to four focused weeks of agent time, in six green-gated releases. Every phase ends with a BUGS_AND_ITERATIONS entry per landed item, `tests/PERF-V0.2-*.md` numbers from the sampler, and a push.

## Open questions for Joona (owner-taste calls)

1. **Phase 1 changes the output baseline once** (first ~15 frames of every render become deterministic rather than warm-up-dependent). OK to regenerate goldens and note it in BUGS as intentional?
2. **`--frames` default:** keep 24 for bare CLI calls (today) or switch to `auto` = duration × fps like the GUI? Plan says `auto` with a BUGS/CHANGELOG note.
3. **Feature order:** plan puts terminal export first (cheapest, most agent-value). If you'd rather see the timeline or video first, say so — they're independent after 5.0.
4. **Video decode scope:** H.264 only in Rust for v0.2, HEVC via the WebView path. Fine for v0.2?

**Do not implement yet.**
