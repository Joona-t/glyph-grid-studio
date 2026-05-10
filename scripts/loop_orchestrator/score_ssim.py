"""Visual regression scorer for the autonomous optimization loop.

Each PERF_JOB carries 3 base64-PNG anchor frames (frame 0, mid, last).
We compute SSIM(baseline_anchor, after_anchor) per frame, take the min
across all 3, and report per-variant + global min. Threshold 0.985 is
the "drift detected" gate.

scikit-image is the dependency (~30 MB). PyTorch-based LPIPS is rejected:
1 GB install for marginal sensitivity gain on what are mostly mechanical
sprite-atlas / branch-elimination changes.
"""
from __future__ import annotations

import base64
import io
import json
from pathlib import Path

import numpy as np
from skimage.io import imread
from skimage.metrics import structural_similarity as ssim


# Empirically-justified threshold:
# - 1.000 = pixel-identical
# - 0.999 = floating-point-rounding equivalent (gradient changes)
# - 0.99  = ~1 % perceptual drift (typical "looks the same")
# - 0.985 = ~2 % drift; catches sub-pixel alignment shifts and
#           palette-quantization-induced banding
# - < 0.95 = visible regression
SSIM_DRIFT_THRESHOLD = 0.985


def _decode_anchor(b64: str) -> np.ndarray:
    """Decode base64 PNG (data: prefix optional) into RGB uint8 array."""
    if b64.startswith("data:"):
        b64 = b64.split(",", 1)[1]
    raw = base64.b64decode(b64)
    img = imread(io.BytesIO(raw))
    if img.ndim == 3 and img.shape[2] == 4:
        img = img[..., :3]  # drop alpha
    elif img.ndim == 2:
        img = np.stack([img] * 3, axis=-1)  # grayscale → RGB
    return img.astype(np.uint8)


def _ssim_pair(a: np.ndarray, b: np.ndarray) -> float:
    """SSIM between two same-shape RGB images. If shapes differ, resize
    the smaller to the larger via nearest-neighbor (any geometry-changing
    optimization is a regression by definition; this just keeps the
    function safe)."""
    if a.shape != b.shape:
        # Different shapes → mark as max regression
        return 0.0
    if a.dtype != b.dtype:
        a = a.astype(np.uint8)
        b = b.astype(np.uint8)
    # SSIM with channel_axis for color images, data_range for uint8
    return float(ssim(a, b, channel_axis=-1, data_range=255))


def score_variant(baseline_perf: dict, after_perf: dict) -> dict:
    """Score one variant. Returns {ssim_min, ssim_per_frame[3]}."""
    base_anchors = baseline_perf.get("frame_anchors", [])
    after_anchors = after_perf.get("frame_anchors", [])
    n = min(len(base_anchors), len(after_anchors))
    if n == 0:
        return {"ssim_min": 0.0, "ssim_per_frame": [], "missing": True}
    scores = []
    for i in range(n):
        try:
            a = _decode_anchor(base_anchors[i])
            b = _decode_anchor(after_anchors[i])
            scores.append(_ssim_pair(a, b))
        except Exception as e:
            scores.append(0.0)
    return {"ssim_min": min(scores) if scores else 0.0,
            "ssim_per_frame": scores,
            "missing": False}


def score_suites(baseline_suite: dict, after_suite: dict) -> dict:
    """Score every variant present in both suites. Returns
    {by_variant: {name: {ssim_min, ssim_per_frame}},
     ssim_global_min: float, drift_detected: bool}."""
    base_by = baseline_suite["by_variant"]
    after_by = after_suite["by_variant"]
    by_variant: dict[str, dict] = {}
    global_min = 1.0
    for name, base in base_by.items():
        after = after_by.get(name)
        if after is None:
            by_variant[name] = {"ssim_min": 0.0, "missing": True,
                                "ssim_per_frame": []}
            global_min = 0.0
            continue
        result = score_variant(base, after)
        by_variant[name] = result
        if result["ssim_min"] < global_min:
            global_min = result["ssim_min"]
    return {
        "by_variant":      by_variant,
        "ssim_global_min": global_min,
        "drift_detected":  global_min < SSIM_DRIFT_THRESHOLD,
        "threshold":       SSIM_DRIFT_THRESHOLD,
    }


def main() -> None:
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("--baseline", required=True,
                    help="Path to baseline.json (full suite result)")
    ap.add_argument("--after", required=True,
                    help="Path to after.json (full suite result)")
    ap.add_argument("--out", required=True,
                    help="Where to write ssim.json")
    args = ap.parse_args()

    with open(args.baseline) as f:
        baseline = json.load(f)
    with open(args.after) as f:
        after = json.load(f)

    result = score_suites(baseline, after)
    with open(args.out, "w") as f:
        json.dump(result, f, indent=2)

    print(f"global_min_ssim = {result['ssim_global_min']:.4f}")
    print(f"drift_detected  = {result['drift_detected']} "
          f"(threshold {result['threshold']})")
    for name, sv in sorted(result["by_variant"].items(),
                            key=lambda kv: kv[1]["ssim_min"]):
        marker = "❌" if sv["ssim_min"] < SSIM_DRIFT_THRESHOLD else "✓"
        print(f"  {marker}  {sv['ssim_min']:.4f}  {name}")


if __name__ == "__main__":
    main()
