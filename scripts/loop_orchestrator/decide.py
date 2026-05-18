"""Decision state machine for the autonomous optimization loop.

Per cycle: take baseline + after suite results, SSIM scores, build status,
test status. Apply the rule table from the plan. Return outcome dict.
"""
from __future__ import annotations

from dataclasses import dataclass
from enum import Enum


class Decision(str, Enum):
    KEEP     = "keep"
    REVERT   = "revert"
    ESCALATE = "escalate"


# Default-config geomean must remain ≤ 1.05 × current best.
DEFAULT_CONFIG_BUDGET_FACTOR = 1.05

# Minimum total speedup to count as a KEEP (anti-noise gate).
# Tuned to 4.0 ms after the first pursuit's 5/5 reverts showed
# measurement variance of ±3 ms on the bench. A 2.0 ms threshold sat
# inside the noise band → ambiguous wins kept getting rejected as
# "no_signal".  4 ms is comfortably above noise + matches the floor a
# Carmack-grade per-frame optimization would clear on at least one
# heavy config (cfg-postproc-heavy or cfg-preserve-stress).
MIN_KEEP_DELTA_MS = 4.0

# Anti-regression: any worsening above +3 ms blocks the cycle. Symmetric
# with the keep floor — patches must either clearly improve or clearly
# regress; "no signal" rolls back.
MAX_ALLOWED_DELTA_MS = 3.0

# SSIM threshold mirrored from score_ssim.py.
SSIM_KEEP_THRESHOLD = 0.985

# B4 — per-component regression guard. A patch may not regress ANY felt
# cost beyond its own noise floor, even if the weighted composite
# improves (prevents trading interactive latency for throughput, or
# tanking export to shave render). Floors scale with each term's
# magnitude: render ~4 ms (matches MIN_KEEP), switch ~10 ms (slider
# felt-latency noise), export ~200 ms (gifski encode is 3–25 s; 200 ms
# is well inside its run-to-run variance). Keyed to headline_ms() keys.
COMPONENT_REGRESS_FLOORS = {
    "__render_ms__": 4.0,
    "__switch_ms__": 10.0,
    "__export_ms__": 200.0,
}


@dataclass
class CycleInputs:
    baseline_headline: dict   # benchmark.headline_ms() output for baseline
    after_headline: dict      # ditto for after
    ssim_global_min: float    # score_ssim.score_suites()["ssim_global_min"]
    build_ok: bool
    tests_ok: bool
    after_geomean_ms: float   # __geomean__ key from after_headline
    baseline_geomean_ms: float
    default_baseline_ms: float    # cfg-default baseline ms
    default_after_ms: float       # cfg-default after ms
    current_best_default_ms: float  # rolling-min default seen so far


@dataclass
class Outcome:
    decision: Decision
    reason: str
    delta_ms: float
    ssim_min: float
    build_ok: bool
    tests_ok: bool


def decide(inp: CycleInputs) -> Outcome:
    """Apply the keep/revert/escalate rule table.

    Order matters — earliest match wins.
    """
    delta = inp.after_geomean_ms - inp.baseline_geomean_ms

    # Rule 0 — sentinel: visible drift to nothingness.
    if inp.ssim_global_min == 0.0 and delta < -10.0:
        return Outcome(Decision.ESCALATE,
                       reason="rendering_disappeared_sentinel",
                       delta_ms=delta, ssim_min=inp.ssim_global_min,
                       build_ok=inp.build_ok, tests_ok=inp.tests_ok)

    # Rule 1 — build broken.
    if not inp.build_ok:
        return Outcome(Decision.REVERT,
                       reason="build_failed",
                       delta_ms=delta, ssim_min=inp.ssim_global_min,
                       build_ok=False, tests_ok=inp.tests_ok)

    # Rule 2 — tests broken (escalate; main may be poisoned).
    if not inp.tests_ok:
        return Outcome(Decision.ESCALATE,
                       reason="tests_broken",
                       delta_ms=delta, ssim_min=inp.ssim_global_min,
                       build_ok=inp.build_ok, tests_ok=False)

    # Rule 3 — visual drift.
    if inp.ssim_global_min < SSIM_KEEP_THRESHOLD:
        return Outcome(Decision.REVERT,
                       reason=f"visual_drift_below_{SSIM_KEEP_THRESHOLD}",
                       delta_ms=delta, ssim_min=inp.ssim_global_min,
                       build_ok=inp.build_ok, tests_ok=inp.tests_ok)

    # Rule 4 — explicit regression on geomean.
    if delta > MAX_ALLOWED_DELTA_MS:
        return Outcome(Decision.REVERT,
                       reason="geomean_regressed",
                       delta_ms=delta, ssim_min=inp.ssim_global_min,
                       build_ok=inp.build_ok, tests_ok=inp.tests_ok)

    # Rule 5 — default-config budget violated (helps preserve, regresses default).
    default_ceiling = inp.current_best_default_ms * DEFAULT_CONFIG_BUDGET_FACTOR
    if inp.default_after_ms > default_ceiling:
        return Outcome(Decision.REVERT,
                       reason=f"default_ceiling_violated_"
                              f"({inp.default_after_ms:.1f}>{default_ceiling:.1f})",
                       delta_ms=delta, ssim_min=inp.ssim_global_min,
                       build_ok=inp.build_ok, tests_ok=inp.tests_ok)

    # Rule 5.5 — B4 per-component regression guard. Self-served from the
    # headline dicts CycleInputs already carries. Any felt cost (render /
    # switch / export) regressing beyond its own floor reverts the cycle
    # even if the composite or geomean improved. Missing terms (old
    # binary, MP4 path) are skipped — degrades to render-only cleanly.
    bH, aH = inp.baseline_headline or {}, inp.after_headline or {}
    for key, floor in COMPONENT_REGRESS_FLOORS.items():
        bv, av = bH.get(key), aH.get(key)
        if isinstance(bv, (int, float)) and isinstance(av, (int, float)):
            if (av - bv) > floor:
                return Outcome(Decision.REVERT,
                               reason=f"component_regressed_{key}_"
                                      f"(+{av - bv:.1f}>{floor:.0f})",
                               delta_ms=delta, ssim_min=inp.ssim_global_min,
                               build_ok=inp.build_ok, tests_ok=inp.tests_ok)

    # Rule 6 — net win must clear the noise floor.
    if delta > -MIN_KEEP_DELTA_MS:
        return Outcome(Decision.REVERT,
                       reason=f"no_signal_(delta_{delta:+.2f}ms_<_keep_floor)",
                       delta_ms=delta, ssim_min=inp.ssim_global_min,
                       build_ok=inp.build_ok, tests_ok=inp.tests_ok)

    # Rule 7 — KEEP.
    return Outcome(Decision.KEEP,
                   reason=f"net_-{abs(delta):.2f}ms_ssim_{inp.ssim_global_min:.4f}",
                   delta_ms=delta, ssim_min=inp.ssim_global_min,
                   build_ok=inp.build_ok, tests_ok=inp.tests_ok)


if __name__ == "__main__":
    # Smoke test: inject various scenarios.
    tests = [
        ("clean keep", CycleInputs(
            baseline_headline={"__geomean__": 122.0},
            after_headline={"__geomean__": 110.0},
            ssim_global_min=0.998, build_ok=True, tests_ok=True,
            after_geomean_ms=110.0, baseline_geomean_ms=122.0,
            default_baseline_ms=122.0, default_after_ms=108.0,
            current_best_default_ms=122.0)),
        ("noise → revert", CycleInputs(
            baseline_headline={"__geomean__": 122.0},
            after_headline={"__geomean__": 121.0},
            ssim_global_min=0.999, build_ok=True, tests_ok=True,
            after_geomean_ms=121.0, baseline_geomean_ms=122.0,
            default_baseline_ms=122.0, default_after_ms=121.0,
            current_best_default_ms=122.0)),
        ("drift", CycleInputs(
            baseline_headline={"__geomean__": 122.0},
            after_headline={"__geomean__": 100.0},
            ssim_global_min=0.92, build_ok=True, tests_ok=True,
            after_geomean_ms=100.0, baseline_geomean_ms=122.0,
            default_baseline_ms=122.0, default_after_ms=100.0,
            current_best_default_ms=122.0)),
        ("sentinel", CycleInputs(
            baseline_headline={"__geomean__": 122.0},
            after_headline={"__geomean__": 5.0},
            ssim_global_min=0.0, build_ok=True, tests_ok=True,
            after_geomean_ms=5.0, baseline_geomean_ms=122.0,
            default_baseline_ms=122.0, default_after_ms=5.0,
            current_best_default_ms=122.0)),
    ]
    for label, inp in tests:
        out = decide(inp)
        print(f"  {label:20s}: {out.decision.value:9s} | {out.reason}")
