"""Benchmark harness for the autonomous optimization loop.

Owns:
  * The fixed 4-source × 5-config bench suite (deterministic seed 1337).
  * Manifest construction (`build_manifest`).
  * Studio invocation (`run_benchmark`).
  * PERF_JOB NDJSON parsing from stderr (`parse_perf_jobs`).

The studio binary is the production ankh app at
`~/Applications/Glyph Grid Studio.app/Contents/MacOS/glyph-grid-studio`.
That path is canonical (Joona's "USE THE APP WITH THE ANKH LOGO").
"""
from __future__ import annotations

import copy
import json
import os
import re
import shlex
import subprocess
from pathlib import Path
from typing import Any

# ---- Constants ----

PROD_BIN = (
    "/Users/darkfire/Applications/Glyph Grid Studio.app/Contents/MacOS/"
    "glyph-grid-studio"
)
REPO_ROOT = Path("/Users/darkfire/glyph-grid-studio")
SOURCES_DIR = REPO_ROOT / "tests" / "sources"

# Bench frame count. Lowered 24→12 for the scaling sweep: the suite now
# spans regimes 10–1000× apart in cost (5 ms → >6 s/frame), so 12 frames
# resolves them with huge margin while keeping the catastrophic corner
# (400×300 @ 2560×1600, CPU ~6 s/frame) affordable: 12 × ~6 s = ~72 s for
# the single worst variant, well under the 900 s manifest timeout across
# ~23 variants. Still ≤ __pfMaxRing (30) so the ring captures every frame.
BENCH_FRAMES = 12

# ---- BASE config (mirrors src/index.html:111 CONFIG) ----

BASE: dict[str, Any] = {
    "canvas":            {"w": 1024, "h": 504},
    "grid":              {"cols": 240, "rows": 120},
    "font":              {"family": "monospace", "size": 8},
    "ramp":              "gradient",
    "brightnessGamma":   0.55,
    "bgThreshold":       0,
    "invertSignal":      False,
    "samplingStrategy":  "average",
    "colorMode":         "monochrome",
    "palette":           "cream-paper",
    "glyphSet":          None,
    "selectionMode":     "brightness",
    "dither":            {"mode": "temporal", "asSourcePrefilter": True},
    "prefilter":         {"mode": "none"},
    "postprocess":       {"vignette": {"enabled": True, "strength": 0.55}},
    "depth":             {"enabled": False},
    "paletteMorph":      None,
    "animation":         {"fps": 24, "duration": 1.0, "loop": True},
    "seed":              1337,
    "studio": {
        "enabled":          True,
        "fitCanvasToImage": True,
        "breathing":        {"emaAlpha": 0.35, "gainSwing": 0.30,
                             "jitter": 0.05, "pulseHz": 0.7},
    },
}


# ---- Suite (scaling sweep — Sutskever v3) ----
#
# The old suite was 4 sources × 5 configs at ONE operating point
# (240×120 grid, 1024×504 canvas). It was blind to the regimes users
# actually hit: grid cost is O(cells) → 545 ms at 400×300; postproc cost
# is O(canvas_px × stages) → >6 s at dense+heavy. Optimizing that
# fixed-point geomean optimized the easy middle while the corners were
# catastrophic.
#
# The reframed suite SWEEPS the two dominant cost axes — grid density and
# canvas pixels — crossed with the two structurally-distinct postproc
# regimes (vignette-only "light" vs the 5-stage "heavy" stack). The
# headline metric becomes per-regime geomeans + `max_regime_ms` (the
# catastrophic corner). "Lower the worst regime" is the frontier the GPU
# pipeline crushes and CPU micro-opts cannot move.

SOURCES = [
    "thor.png",          # deterministic sweep source (fit disabled)
    "ghost-I.gif",       # animated sanity point
    "cream-paper.png",   # source-sensitivity point
    "synthetic-noise.png",  # source-sensitivity point
]

# Grid densities (cols, rows): sparse / current / dense.
_DENSITIES = [(120, 60), (240, 120), (400, 300)]
# Canvas sizes (w, h): small / current / large.
_CANVASES = [(640, 360), (1024, 504), (1920, 1008)]
# The two structurally-distinct postproc regimes.
_PP_LIGHT = {"vignette": {"enabled": True, "strength": 0.55}}
_PP_HEAVY = {
    "vignette":            {"enabled": True, "strength": 0.55},
    "bloom":               {"enabled": True, "strength": 0.6, "radius": 5},
    "halation":            {"enabled": True, "strength": 0.4},
    "scanlines":           {"enabled": True, "period": 2},
    "chromaticAberration": {"enabled": True, "amount": 0.3},
}


def _deep_merge(target: dict, src: dict) -> dict:
    for k, v in src.items():
        if isinstance(v, dict) and isinstance(target.get(k), dict):
            _deep_merge(target[k], v)
        else:
            target[k] = v
    return target


def _config(overrides: dict) -> dict:
    cfg = copy.deepcopy(BASE)
    _deep_merge(cfg, overrides)
    return cfg


def _sweep_cfg(pp: dict, cols: int, rows: int, cw: int, ch: int) -> dict:
    """Build a sweep variant config: pin grid density + canvas size, and
    DISABLE fitCanvasToImage so canvas.w/h is authoritative (otherwise the
    source image aspect would override it and the canvas axis of the sweep
    would be meaningless)."""
    return _config({
        "grid":   {"cols": cols, "rows": rows},
        "canvas": {"w": cw, "h": ch},
        "postprocess": pp,
        "studio": {"fitCanvasToImage": False},
    })


def _suite() -> list[tuple[str, dict, str]]:
    """Return [(variant_name, config, source_filename), ...].

    ~23 variants in ONE studio launch:
      * 2 regimes × 3 densities × 3 canvases = 18 sweep points (thor,
        fit disabled — deterministic)
      * 2 catastrophic torture points (400×300 @ 2560×1600, light+heavy)
      * 1 animated sanity point (ghost-I.gif, fit enabled, default op-pt)
      * 2 source-sensitivity points (cream-paper, synthetic-noise at the
        old default operating point)

    Variant name encodes the regime: `<light|heavy>__d<C>x<R>__c<W>x<H>`
    (+ a source tag when the source isn't thor). headline_ms() groups by
    regime and surfaces max_regime_ms (the catastrophic corner).
    """
    variants: list[tuple[str, dict, str]] = []

    # 18-point sweep on the deterministic source.
    for pp_name, pp in (("light", _PP_LIGHT), ("heavy", _PP_HEAVY)):
        for (cols, rows) in _DENSITIES:
            for (cw, chh) in _CANVASES:
                name = f"{pp_name}__d{cols}x{rows}__c{cw}x{chh}"
                variants.append((name, _sweep_cfg(pp, cols, rows, cw, chh), "thor.png"))

    # Catastrophic corner — CPU is >6 s here; this is the GPU pipeline's
    # entire reason to exist. Kept IN the suite so the reframed objective
    # SEES the regime instead of being blind to it.
    for pp_name, pp in (("light", _PP_LIGHT), ("heavy", _PP_HEAVY)):
        name = f"{pp_name}__d400x300__c2560x1600"
        variants.append((name, _sweep_cfg(pp, 400, 300, 2560, 1600), "thor.png"))

    # Animated-source sanity (fit enabled — exercises the real GIF path).
    variants.append((
        "light__ghost__d240x120__c1024x504",
        _config({"grid": {"cols": 240, "rows": 120},
                 "postprocess": _PP_LIGHT}),
        "ghost-I.gif",
    ))

    # Source-sensitivity at the legacy operating point.
    for src, tag in (("cream-paper.png", "creampaper"),
                     ("synthetic-noise.png", "noise")):
        variants.append((
            f"light__{tag}__d240x120__c1024x504",
            _sweep_cfg(_PP_LIGHT, 240, 120, 1024, 504),
            src,
        ))

    return variants


# ---- Manifest construction ----

def build_manifest(out_dir: Path, perf: bool = True) -> Path:
    """Builds a SINGLE bench manifest with all 4 sources × 5 configs = 20
    jobs. Each job carries its own `in` (source path); the studio's batch
    driver swaps `sourceImg` between jobs when sources differ, so the
    whole suite runs in ONE Tauri session (~one process spawn per bench
    instead of four).

    The top-level `in` is the FIRST job's source — the studio's initial
    load uses it. Subsequent jobs trigger an async source swap via
    `_cliLoadSource` inside `runBatchExport.next()`.

    Output GIFs land at <out_dir>/<source>__<cfg_name>.gif. The studio
    emits PERF_JOB NDJSON via cli_log when perf=True.
    """
    out_dir.mkdir(parents=True, exist_ok=True)
    variants = _suite()
    jobs: list[dict] = []
    for name, cfg, src in variants:
        src_path = SOURCES_DIR / src
        out_path = out_dir / f"{name}.gif"
        jobs.append({
            "name":   name,                          # regime-encoded
            "in":     str(src_path),                  # per-job source
            "out":    str(out_path),
            "format": "gif",
            "config": cfg,
        })
    m_path = out_dir / "bench-manifest.json"
    m = {
        "in":     str(SOURCES_DIR / variants[0][2]),  # initial source
        "frames": BENCH_FRAMES,
        "perf":   perf,
        "jobs":   jobs,
    }
    with open(m_path, "w") as f:
        json.dump(m, f, indent=2)
    return m_path


# ---- Studio invocation ----

PERF_LINE = re.compile(r"PERF_JOB (\{.*\})")


def run_benchmark(manifest_path: Path, *, timeout_s: int = 600,
                  bin_path: str = PROD_BIN) -> dict:
    """Run one manifest end-to-end. Returns {jobs: [...], perf_jobs:
    [...], stdout: str, stderr: str, exit_code: int, duration_s: float}.

    PERF_JOB NDJSON lines are parsed from stderr (cli_log writes there).
    """
    import time
    cmd = [bin_path, "batch", "--manifest", str(manifest_path)]
    t0 = time.time()
    # Audit 2026-06-10 (CS-4): subprocess.run(timeout=...) raises
    # TimeoutExpired but does NOT kill the child — the studio kept
    # rendering in the background as a zombie while the orchestrator
    # moved on to the next cycle.  Popen + explicit kill() guarantees
    # the process tree dies with the timeout.
    proc = subprocess.Popen(
        cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )
    try:
        stdout, stderr = proc.communicate(timeout=timeout_s)
        rc = proc.returncode
    except subprocess.TimeoutExpired:
        proc.kill()
        try:
            stdout, stderr = proc.communicate(timeout=10)
        except subprocess.TimeoutExpired:
            stdout, stderr = "", ""
        rc = 124
    duration_s = time.time() - t0

    perf_jobs = []
    for line in stderr.splitlines():
        # Audit 2026-06-10 (CS-4): surface snapshot failures instead of
        # silently dropping them (regex below only matches PERF_JOB {...}).
        if "PERF_JOB_ERR" in line:
            print(f"benchmark: WARNING perf snapshot failed: {line.strip()[:200]}",
                  flush=True)
        m = PERF_LINE.search(line)
        if m:
            try:
                perf_jobs.append(json.loads(m.group(1)))
            except json.JSONDecodeError:
                pass
    return {
        "manifest":   str(manifest_path),
        "exit_code":  rc,
        "duration_s": duration_s,
        "stdout":     stdout,
        "stderr":     stderr,
        "perf_jobs":  perf_jobs,
    }


def run_full_suite(out_dir: Path, *, timeout_s: int = 900,
                   bin_path: str = PROD_BIN) -> dict:
    """Build + run the ONE multi-source manifest. ONE studio launch
    covers all 20 variants (was previously 4 launches, one per source).
    Returns {by_variant, n_variants, n_failures, total_dur_s, runs,
    manifest_idx}."""
    m_path = build_manifest(out_dir, perf=True)
    res = run_benchmark(m_path, timeout_s=timeout_s, bin_path=bin_path)
    by_variant: dict[str, dict] = {}
    for pj in res["perf_jobs"]:
        by_variant[pj["name"]] = pj
    return {
        "by_variant":   by_variant,
        "n_variants":   len(by_variant),
        "n_failures":   0 if res["exit_code"] == 0 else 1,
        "total_dur_s":  res["duration_s"],
        "runs":         [res],
        "manifest_idx": str(m_path),
    }


# ---- Aggregate ms-per-frame for the suite (the loop's headline metric) ----

def _geomean(vals: list[float]) -> float:
    import math
    vals = [v for v in vals if v > 0]
    if not vals:
        return 0.0
    return math.exp(sum(math.log(v) for v in vals) / len(vals))


def headline_ms(suite_result: dict) -> dict:
    """Reframed (Sutskever v3): the headline is a SCALING PROFILE, not a
    single fixed-point geomean.

    Returns:
      * one key per variant (regime-encoded name → avg total ms)
      * `geomean_light` / `geomean_heavy` — per-postproc-regime geomeans
      * `max_regime_ms` — the single worst variant (the catastrophic
        corner). THIS is the new optimization frontier: "lower the worst
        regime." The GPU pipeline crushes it; CPU micro-opts cannot.
      * `__geomean__` — overall geomean (kept for orchestrator back-compat)
      * `cfg-default` — alias for the light__d240x120__c1024x504 variant
        (the closest analog of the legacy default operating point), so
        the orchestrator's default-ceiling guard keeps working until B4
        reworks decide.py onto the composite objective.
    """
    by_variant = suite_result["by_variant"]
    headline: dict[str, float] = {}
    all_ms: list[float] = []
    light_ms: list[float] = []
    heavy_ms: list[float] = []
    worst_name, worst_ms = None, 0.0

    for name, pj in by_variant.items():
        ms = pj.get("avg_ms", {}).get("total", 0.0)
        if ms <= 0:
            continue
        headline[name] = ms
        all_ms.append(ms)
        if name.startswith("heavy"):
            heavy_ms.append(ms)
        else:
            light_ms.append(ms)
        if ms > worst_ms:
            worst_ms, worst_name = ms, name

    if light_ms:
        headline["geomean_light"] = _geomean(light_ms)
    if heavy_ms:
        headline["geomean_heavy"] = _geomean(heavy_ms)
    if all_ms:
        headline["__geomean__"] = _geomean(all_ms)
    if worst_name is not None:
        headline["max_regime_ms"] = worst_ms
        headline["__worst_variant__"] = worst_name  # str — diagnostic only

    # ---- B4: composite objective ----
    # The render geomean is only one of three felt costs. Surface the two
    # the loop has never measured — interactive switch latency and export
    # wall-time (the dominant real cost, ITER-026) — and a weighted
    # composite. Weights are IMPORTANCE weights (export ≈ half of felt
    # time and was never optimized). decide.py self-serves these from the
    # headline dict it already carries; missing terms (old binary) drop
    # out and the remaining weights renormalize, so it degrades to
    # render-only gracefully.
    import statistics
    switch_vals = [pj["switch_ms"] for pj in by_variant.values()
                   if isinstance(pj.get("switch_ms"), (int, float))]
    export_vals = [pj["encode_ms"] for pj in by_variant.values()
                   if isinstance(pj.get("encode_ms"), (int, float))]
    render_term = headline.get("__geomean__", 0.0)
    headline["__render_ms__"] = render_term
    if switch_vals:
        headline["__switch_ms__"] = statistics.median(switch_vals)
    if export_vals:
        headline["__export_ms__"] = statistics.median(export_vals)

    terms = [(0.3, render_term if render_term > 0 else None),
             (0.2, headline.get("__switch_ms__")),
             (0.5, headline.get("__export_ms__"))]
    present = [(w, v) for w, v in terms if v is not None]
    if present:
        wsum = sum(w for w, _ in present)
        headline["__score__"] = sum((w / wsum) * v for w, v in present)

    # Back-compat alias for the orchestrator's default-ceiling guard.
    if "light__d240x120__c1024x504" in headline:
        headline["cfg-default"] = headline["light__d240x120__c1024x504"]
    elif all_ms:
        headline["cfg-default"] = _geomean(all_ms)
    return headline


if __name__ == "__main__":
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="/tmp/glyph-bench-test", help="Output dir")
    ap.add_argument("--bin", default=PROD_BIN, help="Studio binary")
    args = ap.parse_args()
    out_dir = Path(args.out)
    print(f"=== full suite → {out_dir} ===")
    result = run_full_suite(out_dir, bin_path=args.bin)
    headline = headline_ms(result)
    print(f"variants={result['n_variants']}, failures={result['n_failures']}, "
          f"dur={result['total_dur_s']:.1f}s")
    for cfg, ms in sorted(headline.items()):
        print(f"  {cfg:>30}  {ms:6.1f} ms/frame")
