# PERF-V0.2-BASELINE — headless render CPU, XPC-aware (Phase 0)

**Date:** 2026-10-17
**Build:** v0.1.10 + Phase 0 instruments (branch `audit/v0.2-sota`, on top of `bebe7cc`)
**Hardware:** Apple M1 Pro, 8 cores, 16 GB, macOS 27.0.1
**Job:** the ITER-041 reference render — `worm-move.gif` (760×1024 canvas via `fitCanvasToImage`), 150×200 grid, preset `pB.json` (spice / preserve / gradient ramp / temporal dither / vignette + bloom), `--frames 120`, MP4 out.
**Method:** `tests/perf-sample.sh` (below). Three consecutive runs, machine otherwise idle, screen unlocked.

---

## Why this file exists

Every headless number in `BUGS_AND_ITERATIONS.md` before ITER-045 came from `/usr/bin/time`, which only sees the Tauri host. WebKit renders in two launchd-spawned XPC services (`com.apple.WebKit.WebContent`, `com.apple.WebKit.GPU`, ppid 1) that `time`/rusage structurally cannot see. "user ≈2.7 s on a 28 s wall → the window is throttled and mostly idle" was a measurement artefact (research.md §0, F1). This file is the first baseline that counts all three processes.

## The instrument

```
tests/perf-sample.sh <out.ndjson> -- <binary> <args...>
```

1. Snapshots `pgrep -f 'WebKit\.(WebContent|GPU)'` before launch.
2. Launches the binary in the background; diffs pgrep every 0.2 s to pick up *our* WebContent/GPU pids (role from the XPC bundle path, logged as `{"type":"xpc",...}` lines with the full command so a stray pid from another WebKit app can be excluded by hand).
3. Samples `ps -o %cpu,time,state,pri,ni` for host + XPC pids at 1 Hz → `{"type":"sample",...}` lines.
4. On exit takes one final sweep (the XPC services outlive the host by a few hundred ms), then appends and prints one summary line:

```
{"type":"summary","wall_s":27.797,"app_cpu_s":2.54,"webcontent_cpu_s":24.49,"gpu_cpu_s":6.77,"frames":120,"exit":0,"samples":73,"pids":{...}}
```

`PERF_TIMEOUT=<s>` kills the binary and reports `exit:124`. `tests/run-all.sh` B8 now runs the batch job through it and B10 fails unless the summary carries numeric `webcontent_cpu_s`/`gpu_cpu_s`. Under-count per process is at most one sample interval (≈0.2 s after the final sweep); `app_cpu_s` is noisier than the XPC columns because the host's last burst (the 6-worker MP4 preprocess) lands in the final ~1 s.

Companion gate: `tests/psnr-gate.sh <new> <baseline> [min_db=35]` — ffmpeg PSNR, prints average + worst frame, exit 1 below threshold.

## The numbers — 120 frames, 760×1024, three runs

| run | wall s | app CPU s | WebContent CPU s | GPU CPU s | XPC total s | XPC / wall |
|---|---|---|---|---|---|---|
| 1 | 27.80 | 2.54 | 24.49 | 6.77 | 31.26 | 1.12 |
| 2 | 28.20 | 1.46 | 24.52 | 6.41 | 30.93 | 1.10 |
| 3 | 28.27 | 1.65 | 24.56 | 6.44 | 31.00 | 1.10 |
| **mean** | **28.09** | **1.88** | **24.52** | **6.54** | **31.06** | **1.11** |

Per frame (mean): wall **234 ms**, WebContent **204 ms**, GPU **55 ms**, host **16 ms**.

Per-sample detail (identical across runs):

| process | busy %cpu min / median / max | state | pri | ni |
|---|---|---|---|---|
| WebContent | 10 / 96 / 113 | `Rs`/`Ss` | 47 | 0 |
| GPU | 5 / 17 / 72 | `Rs`/`Ss`/`Us` | 47 | 0 |
| host | idle until the encode burst (146–201 % for ~1 s) | `R`/`S` | 31–54 | 0 |

Output check: run 1 vs the current golden `38.18 dB` average (gate ≥ 35, PASS); run 2 vs run 1 `37.95 dB`, run 3 vs run 1 `38.18 dB`. The worst frame in every pairing is **frame #1 at 28.5–28.9 dB** — the warm-up nondeterminism Phase 1 (F23) removes; frames 2–120 sit at 38–39.4 dB.

MP4 encode (host side, from stderr): 1037 / 1022 / 1011 ms for 120 frames — ≈3.6 % of wall. The export is not the problem.

## What the split says

1. **WebContent, not the GPU process, is the bottleneck on the real preset.** 24.5 s of 28.1 s wall is WebContent CPU, at a median 96 % — one saturated main thread for the whole capture. The GPU process is 6.5 s, median 17 %, and its total (6.5 s) is well under the wall, so it is nowhere near bounding the run. F1's own sample (synthetic-noise 720×480, GPU 11.6 s > WebContent 6.4 s) does not transfer to 760×1024 / 150×200 cells: more cells and the bloom/vignette stack move the cost into scene JS + `toBlob` PNG encoding, which both execute in WebContent.
2. **"Throttled" is dead.** pri 47, ni 0, `Rs` states, 96 % busy: the XPC processes run at normal priority and are CPU-bound. 204 ms/frame is work, not waiting.
3. **Host CPU is irrelevant** (1.9 s, mostly the 1 s encode burst). Rust-side optimisation of the export path has no wall-time left to win until the capture path moves.

## Conclusion — which lever Phase 2 pulls first

Phase 2's first lever is the **WebContent path, starting with the capture readback: F2 step 2 (raw RGBA `getImageData` over the existing raw-body IPC, Rust preprocess accepting RGBA) and the ITER-042 layer-memo extension (F2 step 3), then scene JS.** The research sampled 40 % of WebContent main-thread time inside the synchronous `canvas.toBlob` wait plus ~3 % PNG encode; on this baseline that is ≈10 s of the 24.5 s, all on the thread that bounds the run. The GPU-process-off experiment (F2 step 1, private WebKit prefs) is **deferred**: with GPU CPU at 6.5 s and never on the critical path, it cannot recover more than a fraction of that and carries the private-API risk. Re-measure with this script after each Phase 2 item; the success criterion is `webcontent_cpu_s` falling, with `wall_s` following it 1:1 while `gpu_cpu_s` stays flat.

## How to reproduce

```bash
cd ~/glyph-grid-studio-perf
pkill -f "glyph-grid-studio render"; pkill -f "glyph-grid-studio batch"
PERF_TIMEOUT=300 bash tests/perf-sample.sh /tmp/p0.ndjson -- \
  src-tauri/target/release/glyph-grid-studio render \
  --in <worm-move.gif> --out /tmp/p0.mp4 --frames 120 --preset <pB.json>
bash tests/psnr-gate.sh /tmp/p0.mp4 <golden.mp4>     # ≥ 35 dB
```

Three runs, report all three; do not report a wall time without the two XPC columns.
