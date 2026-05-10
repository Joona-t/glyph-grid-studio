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

# Bench frame count is 24 (matches __pfMaxRing capacity → ring captures
# every frame of the job, no shifting). Cycle time at ~6 s/job × 20 jobs =
# ~2 min per BASELINE/MEASURE phase.
BENCH_FRAMES = 24

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


# ---- Suite (4 sources × 5 configs) ----

SOURCES = [
    "thor.png",          # high-contrast portrait (current default)
    "ghost-I.gif",       # animated 97-frame source (worst-case density)
    "cream-paper.png",   # tri-modal luminance (ITER-022 test material)
    "synthetic-noise.png",  # high-frequency stress
]


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


def _suite() -> list[tuple[str, dict]]:
    """Return [(cfg_name, config_dict), ...] for the 5 bench configs."""
    pp_off = {"vignette": {"enabled": False}}
    return [
        ("cfg-default", _config({})),
        ("cfg-monochrome-fast", _config({
            "colorMode": "monochrome",
            "selectionMode": "shape-edge-aware",
            "glyphSet": "ascii",
            "postprocess": pp_off,
        })),
        ("cfg-preserve-stress", _config({
            "colorMode": "preserve",
            "depth": {"enabled": True, "fog": {"enabled": True,
                                                "strength": 0.6,
                                                "r": 200, "g": 180, "b": 160}},
            "postprocess": pp_off,
        })),
        ("cfg-postproc-heavy", _config({
            "postprocess": {
                "vignette":            {"enabled": True, "strength": 0.55},
                "bloom":               {"enabled": True, "strength": 0.6, "radius": 5},
                "halation":            {"enabled": True, "strength": 0.4},
                "scanlines":           {"enabled": True, "period": 2},
                "chromaticAberration": {"enabled": True, "amount": 0.3},
            },
        })),
        ("cfg-duotone-dispersal", _config({
            "colorMode": "duotone",
            "dispersal": {"enabled": True, "startT": 0.5, "endT": 0.95,
                          "intensity": 0.5, "upwardBias": 0.5,
                          "swayAmount": 0.4, "rippleAmt": 0.2},
            "postprocess": pp_off,
        })),
    ]


# ---- Manifest construction ----

def build_manifest(out_dir: Path, perf: bool = True) -> Path:
    """Builds the bench manifest at <out_dir>/bench-manifest.json.

    Output GIFs land at <out_dir>/<source>__<cfg_name>.gif. The studio
    emits PERF_JOB NDJSON via cli_log when perf=True.

    The manifest's `in` (source path) is set to the FIRST source. The
    studio loads exactly one source per session, so we group jobs by
    source — meaning callers actually call this once per source.

    To run all 4×5 = 20 variants, callers iterate sources and call
    run_benchmark() per source manifest. Total = 4 manifests × 5 jobs
    each.
    """
    out_dir.mkdir(parents=True, exist_ok=True)
    manifests: list[Path] = []
    for src in SOURCES:
        src_path = SOURCES_DIR / src
        jobs = []
        for cfg_name, cfg in _suite():
            slug = src.replace(".", "-").replace("/", "-")
            out_path = out_dir / f"{slug}__{cfg_name}.gif"
            jobs.append({
                "name":     f"{slug}__{cfg_name}",
                "out":      str(out_path),
                "format":   "gif",
                "config":   cfg,
            })
        m = {"in": str(src_path), "frames": BENCH_FRAMES, "perf": perf, "jobs": jobs}
        m_path = out_dir / f"manifest-{src.replace('.', '-')}.json"
        with open(m_path, "w") as f:
            json.dump(m, f, indent=2)
        manifests.append(m_path)
    # Convenience: a top-level pointer
    pointer = out_dir / "bench-manifest.json"
    with open(pointer, "w") as f:
        json.dump({"manifests": [str(p) for p in manifests],
                   "sources": SOURCES,
                   "configs": [c[0] for c in _suite()]}, f, indent=2)
    return pointer


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
    try:
        proc = subprocess.run(
            cmd,
            capture_output=True, text=True,
            timeout=timeout_s,
        )
        rc = proc.returncode
        stdout = proc.stdout
        stderr = proc.stderr
    except subprocess.TimeoutExpired as e:
        rc = 124
        stdout = (e.stdout or b"").decode("utf-8", "replace") if e.stdout else ""
        stderr = (e.stderr or b"").decode("utf-8", "replace") if e.stderr else ""
    duration_s = time.time() - t0

    perf_jobs = []
    for line in stderr.splitlines():
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
    """Build + run all 4 source manifests sequentially, aggregate results.
    Returns {by_variant: {variant_name: perf_dict}, totals: {...}}."""
    pointer = build_manifest(out_dir, perf=True)
    with open(pointer) as f:
        idx = json.load(f)
    by_variant: dict[str, dict] = {}
    runs: list[dict] = []
    total_t = 0.0
    total_jobs = 0
    failures = 0
    for m_path in idx["manifests"]:
        res = run_benchmark(Path(m_path), timeout_s=timeout_s, bin_path=bin_path)
        runs.append(res)
        total_t += res["duration_s"]
        if res["exit_code"] != 0:
            failures += 1
        for pj in res["perf_jobs"]:
            by_variant[pj["name"]] = pj
            total_jobs += 1
    return {
        "by_variant":   by_variant,
        "n_variants":   total_jobs,
        "n_failures":   failures,
        "total_dur_s":  total_t,
        "runs":         runs,
        "manifest_idx": str(pointer),
    }


# ---- Aggregate ms-per-frame for the suite (the loop's headline metric) ----

def headline_ms(suite_result: dict) -> dict:
    """Compress the suite's 20 variant readings into headline numbers
    the orchestrator cares about: per-config geometric mean + global
    geometric mean. Lower is better."""
    import math
    by_cfg: dict[str, list[float]] = {}
    for name, pj in suite_result["by_variant"].items():
        cfg = name.split("__", 1)[1] if "__" in name else "default"
        ms = pj.get("avg_ms", {}).get("total", 0.0)
        if ms > 0:
            by_cfg.setdefault(cfg, []).append(ms)
    headline = {}
    all_ms = []
    for cfg, vals in by_cfg.items():
        if vals:
            headline[cfg] = math.exp(sum(math.log(v) for v in vals) / len(vals))
            all_ms.extend(vals)
    if all_ms:
        headline["__geomean__"] = math.exp(sum(math.log(v) for v in all_ms) / len(all_ms))
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
