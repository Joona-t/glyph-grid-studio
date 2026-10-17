# PERF-V0.2-GL — WebGL grid path, F6 step 0 measurement (ITER-065)

Measured 2026-10-17 on the release binary, **under parallel load** (two other
phases building/rendering on the same machine). Headless `batch` with
`"perf": true`; every number is the PERF_JOB `avg_ms` over the last 30
recorded frames of a 40-frame job. Source `tests/sources/cream-paper.png`,
canvas 1024×682, `colorMode: monochrome`, `selectionMode: brightness`,
font 8 px (9×12 sprite tiles), default dither/postprocess (vignette only).

New sub-stage timers (`__pfAdd`, nested inside the existing `grid` stage, so
`grid` still includes them):

| key | what it times |
|---|---|
| `cpu_blit` | the CPU sprite loop (28,800 / 120,000 `drawImage` blits) |
| `gl_fill` | the JS per-cell instance fill loop (idx + alpha + origin) |
| `gl_draw` | `wave6.drawSprite` — upload + one `drawArraysInstanced` (CPU side) |
| `gl_composite` | `drawingContext.drawImage(_glCanvas)` onto the p5 canvas |
| `cap_toBlob_call` | the synchronous part of `canvas.toBlob()` in `handleRecordFrame` |
| `cap_toBlob_async` | call → callback (PNG ready) |

WebKit clamps `performance.now()` to ~1 ms here, so sub-ms values are
averages of quantised samples (0.03 = one 1 ms tick in 30 frames).

## Finding 0: the GL path was dead

Before measuring anything: `CONFIG.renderer = 'webgl'` never rendered.
`drawGridWebGL` read `_stabOnInt` and `bgR/bgG/bgB`, which are locals of
its caller `drawBrightnessGrid` (added there in v0.1.6, commit 2e22901). The
first GL frame threw `ReferenceError: Can't find variable: _stabOnInt`, the
catch latched `_glOk = false` and set `CONFIG.renderer = 'cpu'`, and every
frame after was the CPU path. The step-0 run with the original shader below
passes those values as arguments.

## Step 0 — original (cell-squashed, opaque) shader

| job | total | grid | cpu_blit | gl_fill | gl_draw | gl_composite | toBlob call |
|---|---|---|---|---|---|---|---|
| cpu 240×120 | 12.30 | 8.80 | 5.90 | – | – | – | 27.93* |
| webgl 240×120 | 5.33 | 2.47 | – | 0.23 | 0.30 | 1.17 | 12.83 |
| cpu 400×300 | 115.40 | 110.60 | 108.83 | – | – | – | 73.47 |
| webgl 400×300 | 6.30 | 4.13 | – | 0.97 | 0.50 | 1.40 | 20.53 |

\* from the first (pre-fix) run of the same job; that ring sample had no
capture frames in the second run.

Output vs the CPU path (lossless PNG frame anchors from PERF_JOB):
**10.1 dB** at 240×120, **5.6 dB** at 400×300 (max |Δ| 197–217) — not the
same picture (glyphs squashed into the 4.27×4.2 px cell, opaque writes).

## After F7 — sprite-equivalent shader (ITER-065), two runs

| job | run | total | grid | cpu_blit | gl_fill | gl_draw | gl_composite | toBlob call | toBlob async |
|---|---|---|---|---|---|---|---|---|---|
| cpu 240×120 | 1 | 11.37 | 8.13 | 6.13 | – | – | – | 22.30 | 22.47 |
| cpu 240×120 | 2 | 10.67 | 7.80 | 6.03 | – | – | – | 26.43 | 26.47 |
| webgl 240×120 | 1 | 4.33 | 1.77 | – | 0.10 | 0.27 | 0.57 | 17.37 | 17.53 |
| webgl 240×120 | 2 | 4.23 | 2.10 | – | 0.23 | 0.03 | 1.00 | 16.27 | 16.40 |
| cpu 400×300 | 1 | 105.23 | 101.40 | 99.57 | – | – | – | 57.20 | 57.37 |
| cpu 400×300 | 2 | 131.37 | 120.77 | 119.27 | – | – | – | 64.33 | 64.53 |
| webgl 400×300 | 1 | 4.83 | 2.67 | – | 0.67 | 0.27 | 0.40 | 21.50 | 21.70 |
| webgl 400×300 | 2 | 6.80 | 4.10 | – | 0.67 | 0.27 | 1.80 | 11.93 | 12.00 |

Output vs the CPU path, lossless PNG anchors (first / mid / last frame):
240×120 **57.8 / 60.2 / 61.6 dB** (max |Δ| 3), 400×300 **51.6 / 51.5 /
51.4 dB** (max |Δ| 4). Five-config sweep (12 frames each, MP4 + anchors):

| config | MP4 avg (psnr-gate) | PNG anchors |
|---|---|---|
| 240×120 | 37.47 dB | 57.75 / 58.59 / 59.25 |
| 400×300 | 36.63 dB | 51.59 / 51.51 / 51.47 |
| 80×40, font 14 | 40.71 dB | 65.24 / 68.00 / 71.02 |
| temporalStability on | 40.79 dB | 57.75 / 58.59 / 59.15 |
| kawaii-pink palette, gradient ramp | 54.70 dB | 57.70 / 58.37 / 58.86 |

The MP4 numbers are lower than the PNG anchors because H.264 re-quantises
two slightly different inputs differently on this high-frequency content;
the PNG anchors are the real equivalence measure (±2–4 LSB from 8-bit
premultiplied blending vs canvas-2D compositing).

## Reading

- **The fill loop is 0.1–0.7 ms** at 240×120 and ~0.7–1.0 ms at 400×300 —
  far under the 5 ms bar. F6 step 1 (signal/cellIndex bytes + shader-side
  placement) would save < 1 ms, so it is **not done**; steps 2–3 are not
  justified by this data either.
- The "readback trap" ITER-028 inferred (≈34 ms per GL frame) is not what
  this WebKit does: the synchronous `toBlob` call is **cheaper** on the
  GL-composited canvas (12–22 ms) than on the sprite-blitted one (22–64 ms,
  growing with blit count). `toBlob` is effectively synchronous here — the
  callback lands ~0.1 ms after the call returns.
- With the shader fixed, the opt-in GL path draws the default grid in
  ~2 ms vs ~8 ms and the dense grid in ~3–4 ms vs ~100–120 ms. It stays
  opt-in (`renderer: 'cpu'` default); a GL-default proposal is now
  gate-verifiable (≥ 51 dB lossless vs CPU) but is an owner call.
