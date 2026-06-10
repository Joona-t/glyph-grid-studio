"""Autonomous Optimization Loop Orchestrator.

24-hour pursuit: research → patch → build → measure → verify → decide → commit.

Run:
  python3 orchestrator.py                      # full pursuit (24 h cap)
  python3 orchestrator.py --dry-run            # cycle 0 validation, no patches
  python3 orchestrator.py --max-cycles 4       # bounded run
  python3 orchestrator.py --resume             # pick up from state.json

Stop conditions (any one ends the loop cleanly):
  - cfg-default geomean ≤ 80 ms (declare win)
  - 5 consecutive cycles without KEEP (plateau)
  - Backlog empty + synthesis failed
  - 3 consecutive build timeouts (toolchain broken)
  - Wall-clock ≥ 24 h
  - SIGINT or `runs/STOP` sentinel file
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
import traceback
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import yaml  # type: ignore

# Local modules
ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))
import benchmark
import score_ssim
import decide as dec
import patch_runner
import recovery

REPO_ROOT = Path("/Users/darkfire/glyph-grid-studio")
RUNS_DIR = ROOT / "runs"
STATE_PATH = ROOT / "state.json"
BACKLOG_PATH = ROOT / "optimization-backlog.yaml"
LOG_PATH = Path("/Users/darkfire/.claude/data/automation-log") / "glyph-grid-loop.jsonl"
STOP_SENTINEL = RUNS_DIR / "STOP"

WALL_CLOCK_MAX_S = 24 * 3600          # hard 24 h cap
# Early-exit condition: GEOMEAN across all 5 configs drops below this.
# Current baseline (post ITER-013→016): geomean ≈ 74 ms across the 5
# bench configs (cfg-default 11 / mono-fast 24 / preserve-stress 115 /
# postproc-heavy 272 / duotone-dispersal 261). Target: 50 ms (33%
# headroom) — that requires real wins on the heavy configs.
EARLY_EXIT_GEOMEAN_MS = 50.0
PLATEAU_CYCLES = 5                    # consecutive no-keep → escalate
MAX_CONSEC_BUILD_TIMEOUTS = 3
BENCH_TIMEOUT_S = 900
BUILD_TIMEOUT_S = 600
TESTS_TIMEOUT_S = 300
PATCH_TIMEOUT_S = 600


# ---------- State ----------

@dataclass
class State:
    started_at: str
    cycles_completed: int = 0
    cycles_kept: int = 0
    cycles_reverted: int = 0
    cycles_escalated: int = 0
    consecutive_no_keep: int = 0
    consecutive_build_timeouts: int = 0
    # `current_best_default_ms` tracks rolling-min GEOMEAN across all 5
    # bench configs (used by stop-conditions, named historically).
    # `cfg_default_floor_ms` separately tracks cfg-default rolling-min,
    # used by decide.py's default-ceiling guard so a heavy-config win
    # doesn't silently regress the default path.
    current_best_default_ms: float = 999.0
    cfg_default_floor_ms: float = 999.0
    last_baseline_path: str | None = None
    last_baseline_ts: float = 0.0
    interrupted: bool = False

    def to_dict(self) -> dict:
        return asdict(self)


def save_state(s: State) -> None:
    # Audit 2026-06-10 (CS-4): fsync before the atomic rename — a SIGKILL
    # mid-write used to leave a truncated .tmp and, on restart, load_state
    # silently fell back to a fresh State (all cycle progress lost).
    tmp = STATE_PATH.with_suffix(".tmp")
    try:
        with open(tmp, "w") as f:
            f.write(json.dumps(s.to_dict(), indent=2))
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, STATE_PATH)
    except Exception:
        try:
            tmp.unlink()
        except OSError:
            pass
        raise


def load_state() -> State | None:
    if not STATE_PATH.exists():
        return None
    try:
        d = json.loads(STATE_PATH.read_text())
        return State(**d)
    except Exception:
        return None


# ---------- Logging ----------

def now_iso() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def log_event(event: dict) -> None:
    """Append one event to the JSONL log (atomic per line)."""
    LOG_PATH.parent.mkdir(parents=True, exist_ok=True)
    with open(LOG_PATH, "a") as f:
        f.write(json.dumps(event, default=str) + "\n")


# ---------- Backlog ----------

def _load_historical_attempts() -> dict[str, dict]:
    """Scan runs/cycle-*/outcome.json for prior hypothesis attempts.

    The backlog YAML's in-memory `status` updates are clobbered by the
    per-cycle `git checkout main` (the YAML on disk holds whatever Joona
    last committed, which is usually "all queued" plus whatever newly-
    added items). To prevent the loop from re-attempting candidates it
    has already burned cycles on, replay each cycle's outcome.json into
    an `attempts` dict keyed by hypothesis id.

    A "keep" outcome wins over earlier reverts (the candidate landed at
    some point); otherwise the first revert reason is preserved.
    """
    attempts: dict[str, dict] = {}
    if not RUNS_DIR.exists():
        return attempts
    for outcome_path in RUNS_DIR.glob("cycle-*/outcome.json"):
        try:
            o = json.loads(outcome_path.read_text())
        except (json.JSONDecodeError, OSError):
            continue
        hid = o.get("hypothesis")
        decision = o.get("decision")
        if not hid or not decision:
            continue  # dry-run / no-hypothesis cycles don't poison the history
        if decision == "keep":
            attempts[hid] = {"status": "kept", "reason": o.get("reason", "")}
        elif hid not in attempts:
            attempts[hid] = {
                "status": "rejected",
                "reason": (o.get("reason", "") or "")[:200],
            }
    return attempts


def load_backlog() -> list[dict]:
    if not BACKLOG_PATH.exists():
        return []
    with open(BACKLOG_PATH) as f:
        data = yaml.safe_load(f) or []
    # Merge historical attempts so previously-rejected candidates aren't
    # re-tried just because git checkout reset their status. Only escalates
    # `queued` items — never overrides a YAML-author-set `kept` / `frozen`.
    history = _load_historical_attempts()
    if history:
        for h in data:
            cur = h.get("status", "queued")
            if cur != "queued":
                continue
            past = history.get(h["id"])
            if past:
                h["status"] = past["status"]
                h["reason"] = past["reason"]
    return data


def save_backlog(backlog: list[dict]) -> None:
    with open(BACKLOG_PATH, "w") as f:
        yaml.safe_dump(backlog, f, sort_keys=False, default_flow_style=False)


def pop_next_hypothesis(backlog: list[dict],
                         attempted_ids: set[str]) -> dict | None:
    """Return next queued, not-yet-attempted, priority-ordered hypothesis."""
    p_order = {"P0": 0, "P1": 1, "P2": 2}
    candidates = [
        h for h in backlog
        if h.get("status", "queued") == "queued"
        and h["id"] not in attempted_ids
    ]
    candidates.sort(key=lambda h: p_order.get(h.get("priority", "P2"), 9))
    return candidates[0] if candidates else None


def update_hypothesis(backlog: list[dict], hid: str, **fields) -> None:
    for h in backlog:
        if h["id"] == hid:
            h.update(fields)
            return


# ---------- Cycle ----------

def cycle_dir(n: int) -> Path:
    d = RUNS_DIR / f"cycle-{n:03d}"
    d.mkdir(parents=True, exist_ok=True)
    return d


def write_json(path: Path, obj: Any) -> None:
    path.write_text(json.dumps(obj, indent=2, default=str))


# ---- Phase: BASELINE ----

def run_baseline(cycle_n: int, state: State, *,
                 force: bool = False) -> tuple[dict, dict]:
    """Run benchmark suite against current HEAD binary. Returns
    (suite_result, headline). Reuses last baseline if < 30 min old."""
    cdir = cycle_dir(cycle_n)
    bench_out = cdir / "baseline-bench"
    if (not force and state.last_baseline_path
            and (time.time() - state.last_baseline_ts) < 1800):
        prev = Path(state.last_baseline_path)
        if prev.exists():
            with open(prev) as f:
                suite_result = json.load(f)
            headline = benchmark.headline_ms(suite_result)
            # Copy into this cycle's dir for self-contained artifacts
            write_json(cdir / "baseline.json", suite_result)
            write_json(cdir / "baseline-headline.json", headline)
            return suite_result, headline
    suite_result = benchmark.run_full_suite(bench_out, timeout_s=BENCH_TIMEOUT_S)
    headline = benchmark.headline_ms(suite_result)
    baseline_json = cdir / "baseline.json"
    write_json(baseline_json, suite_result)
    write_json(cdir / "baseline-headline.json", headline)
    state.last_baseline_path = str(baseline_json)
    state.last_baseline_ts = time.time()
    save_state(state)
    return suite_result, headline


# ---- Phase: PATCH ----

def request_patch(cycle_n: int, hyp: dict, baseline_headline: dict) -> dict:
    cdir = cycle_dir(cycle_n)
    res = patch_runner.request_patch(hyp, baseline_headline,
                                     timeout_s=PATCH_TIMEOUT_S)
    if res["ok"]:
        (cdir / "patch.diff").write_text(res["diff"])
    write_json(cdir / "patch-meta.json", {
        "ok": res["ok"],
        "reject_reason": res["reject_reason"],
        "exit_code": res["exit_code"],
    })
    return res


# ---- Phase: APPLY + BUILD ----

def apply_and_build(cycle_n: int, diff_text: str, slug: str) -> dict:
    cdir = cycle_dir(cycle_n)
    branch = f"auto/cycle-{cycle_n:03d}-{slug}"

    apply_res = patch_runner.apply_patch(diff_text, branch=branch)
    write_json(cdir / "apply.json", apply_res)
    if not apply_res["ok"]:
        return {"build_ok": False, "branch": branch,
                "error": apply_res["error"], "phase": "apply"}

    # Build
    log = cdir / "build.log"
    t0 = time.time()
    try:
        with open(log, "wb") as f:
            r = subprocess.run(
                # `cargo tauri build` defaults to release profile (the
                # `--release` flag is not accepted by tauri-cli; pass via
                # -- to cargo if explicit profile is ever needed).
                ["cargo", "tauri", "build"],
                cwd=str(REPO_ROOT),
                stdout=f, stderr=subprocess.STDOUT,
                timeout=BUILD_TIMEOUT_S,
            )
        build_ok = (r.returncode == 0)
        build_err = None
    except subprocess.TimeoutExpired:
        build_ok = False
        build_err = f"timeout after {BUILD_TIMEOUT_S}s"
    dur = time.time() - t0
    write_json(cdir / "build-meta.json", {
        "ok": build_ok, "duration_s": dur,
        "branch": branch, "error": build_err,
    })

    # Install + codesign on success
    if build_ok:
        inst = recovery.install_built_app()
        write_json(cdir / "install.json", inst)
        if not inst["recovered"]:
            return {"build_ok": False, "branch": branch,
                    "error": f"install failed: {inst['note']}",
                    "phase": "install"}
    return {"build_ok": build_ok, "branch": branch,
            "error": build_err, "phase": "build", "duration_s": dur}


# ---- Phase: MEASURE ----

def run_measure(cycle_n: int) -> tuple[dict, dict]:
    cdir = cycle_dir(cycle_n)
    bench_out = cdir / "after-bench"
    suite_result = benchmark.run_full_suite(bench_out, timeout_s=BENCH_TIMEOUT_S)
    headline = benchmark.headline_ms(suite_result)
    write_json(cdir / "after.json", suite_result)
    write_json(cdir / "after-headline.json", headline)
    return suite_result, headline


# ---- Phase: VERIFY ----

def run_verify(cycle_n: int, baseline_suite: dict,
                after_suite: dict) -> dict:
    cdir = cycle_dir(cycle_n)
    ssim_res = score_ssim.score_suites(baseline_suite, after_suite)
    write_json(cdir / "ssim.json", ssim_res)
    # Run static tests
    log = cdir / "tests.log"
    tests_path = REPO_ROOT / "tests" / "run-all.sh"
    if not tests_path.exists():
        # No test runner — treat as pass (the harness IS the test)
        log.write_text("(no tests/run-all.sh in repo — skipped)\n")
        tests_ok = True
    else:
        try:
            with open(log, "wb") as f:
                r = subprocess.run(["bash", str(tests_path)],
                                   cwd=str(REPO_ROOT),
                                   stdout=f, stderr=subprocess.STDOUT,
                                   timeout=TESTS_TIMEOUT_S)
            tests_ok = (r.returncode == 0)
        except subprocess.TimeoutExpired:
            tests_ok = False
    return {
        "ssim_global_min":  ssim_res["ssim_global_min"],
        "drift_detected":   ssim_res["drift_detected"],
        "tests_ok":         tests_ok,
    }


# ---- Phase: DECIDE + COMMIT ----

_PATCH_FILE_RE = re.compile(r"^diff --git a/\S+ b/(?P<path>\S+)$", re.MULTILINE)


def _files_in_patch(patch_path: Path) -> list[str]:
    """Extract `b/<path>` from every `diff --git` line in a unified diff.

    The autonomous commit must stage ONLY the source files actually
    touched by the patch. Using `git add -A` (the prior implementation)
    also staged the orchestrator's own metadata changes —
    backlog.yaml status updates, run artifacts under runs/, etc — all
    of which are on the pre-commit deny-list and caused every KEEP
    commit to fail. Path-specific staging is the contract that lets
    the loop commit cleanly without bypass.
    """
    if not patch_path.exists():
        return []
    text = patch_path.read_text(errors="replace")
    return sorted({m.group("path") for m in _PATCH_FILE_RE.finditer(text)})


def commit_and_push(cycle_n: int, hyp: dict, outcome: dec.Outcome) -> dict:
    """On KEEP: ff-merge auto branch into main, commit ITER entry,
    push origin/main.

    Path-specific staging (2026-05-13 fix): stages only the source files
    actually modified by the patch, plus BUGS_AND_ITERATIONS.md for the
    audit-trail append. backlog.yaml status updates live in memory; if
    they need persistence across restarts that happens via the separate
    save_backlog() call (which writes to disk but is NOT included in the
    autonomous commit).
    """
    cdir = cycle_dir(cycle_n)
    apply_meta_path = cdir / "apply.json"
    try:
        apply_meta = json.loads(apply_meta_path.read_text()) \
            if apply_meta_path.exists() else {}
    except json.JSONDecodeError:
        apply_meta = {}

    msg = (
        f"perf({hyp.get('area', 'studio')}): "
        f"{outcome.delta_ms:+.2f}ms/frame geomean | "
        f"SSIM {outcome.ssim_min:.4f} | cycle {cycle_n} | {hyp['id']}"
    )

    # Append ITER entry to BUGS_AND_ITERATIONS (allowed: append-only).
    iter_id = f"ITER-{cycle_n + 100:03d}"  # cycle 1 → ITER-101 etc.
    iter_text = (
        f"\n## {now_iso().split('T')[0]} — auto-loop cycle {cycle_n} — {hyp['id']}\n\n"
        f"### {iter_id} — {hyp.get('area', 'studio perf')}\n\n"
        f"- **Hypothesis:** {hyp.get('hypothesis', '(see backlog)')}\n"
        f"- **Geomean delta:** {outcome.delta_ms:+.2f} ms/frame\n"
        f"- **SSIM min:** {outcome.ssim_min:.4f} (threshold {dec.SSIM_KEEP_THRESHOLD})\n"
        f"- **Decision:** {outcome.decision.value} — {outcome.reason}\n"
    )
    iter_path = REPO_ROOT / "BUGS_AND_ITERATIONS.md"
    with open(iter_path, "a") as f:
        f.write(iter_text)

    # Stage ONLY the patch-touched source files + the BUGS_AND_ITERATIONS
    # append. Everything else (backlog.yaml status, runs/ artifacts,
    # state.json) stays local to the working tree.
    patch_files = _files_in_patch(cdir / "patch.diff")
    stage_paths = patch_files + ["BUGS_AND_ITERATIONS.md"]

    cmds = [
        ["git", "add", "--", *stage_paths],
        ["git", "commit", "-m", msg],
        ["git", "checkout", "main"],
        ["git", "merge", "--ff-only", apply_meta.get("branch") or "HEAD"],
        ["git", "push", "origin", "main"],
    ]
    notes = []
    committed_ok = False
    for cmd in cmds:
        r = subprocess.run(cmd, capture_output=True, text=True,
                           cwd=str(REPO_ROOT), timeout=120)
        notes.append({"cmd": " ".join(cmd), "rc": r.returncode,
                       "stderr": r.stderr.strip()[:300]})
        if cmd[1] == "commit" and r.returncode == 0:
            committed_ok = True
        if r.returncode != 0:
            # Audit 2026-06-10 (CS-4): previously the loop kept executing
            # checkout/merge/push after a failed commit (e.g. pre-commit
            # hook rejection), leaving the repo on main with the cycle
            # branch unmerged and the failure half-hidden in the notes.
            print(f"orchestrator: git step failed, aborting chain: "
                  f"{' '.join(cmd)} rc={r.returncode}", flush=True)
            break
    return {"committed": committed_ok, "msg": msg, "iter_id": iter_id,
            "log": notes, "staged": stage_paths}


# ---- One full cycle ----

def run_one_cycle(cycle_n: int, state: State, backlog: list[dict],
                   *, dry_run: bool = False) -> dict:
    """Returns the cycle outcome dict (also written to outcome.json)."""
    cdir = cycle_dir(cycle_n)
    cycle_t0 = time.time()
    out: dict = {"cycle": cycle_n, "started": now_iso()}

    # PICK
    attempted = {h["id"] for h in backlog
                 if h.get("status") in ("kept", "rejected", "in_progress")}
    hyp = pop_next_hypothesis(backlog, attempted) if not dry_run else None

    if dry_run:
        out["mode"] = "dry-run"
        # Just run baseline to validate harness end-to-end
        baseline_suite, baseline_headline = run_baseline(cycle_n, state, force=True)
        out["baseline_headline"] = baseline_headline
        out["baseline_n_variants"] = baseline_suite["n_variants"]
        out["baseline_n_failures"] = baseline_suite["n_failures"]
        write_json(cdir / "outcome.json", out)
        log_event({
            "ts": now_iso(), "cycle": cycle_n, "phase": "dry-run",
            "category": "optimization", "action": "dry-run",
            "summary": f"baseline harness validated; "
                       f"variants={baseline_suite['n_variants']}, "
                       f"failures={baseline_suite['n_failures']}",
            "details": baseline_headline,
            "impact": "verification",
            "tags": ["glyph-grid-loop", "dry-run"],
        })
        return out

    if hyp is None:
        # LOOP-BUG-001 fix (2026-05-13): when the queue is exhausted, bump
        # consecutive_no_keep so should_stop's PLATEAU_CYCLES check fires
        # within a few iterations instead of spinning forever. Also sleep
        # briefly so the spin (until plateau triggers) doesn't burn CPU.
        out["mode"] = "no-hypothesis"
        out["reason"] = "backlog exhausted (synthesis not yet implemented)"
        write_json(cdir / "outcome.json", out)
        state.consecutive_no_keep += 1
        save_state(state)
        time.sleep(2)  # avoid tight loop until plateau threshold trips
        return out

    out["hypothesis"] = hyp["id"]
    update_hypothesis(backlog, hyp["id"], status="in_progress")
    save_backlog(backlog)
    write_json(cdir / "hypothesis.json", hyp)

    # BASELINE
    baseline_suite, baseline_headline = run_baseline(cycle_n, state)
    out["baseline_headline"] = baseline_headline

    if state.current_best_default_ms == 999.0:
        # First-ever cycle: seed the rolling-min trackers from baseline.
        state.current_best_default_ms = baseline_headline.get("__geomean__", 999.0)
        state.cfg_default_floor_ms = baseline_headline.get("cfg-default", 999.0)
        save_state(state)

    # PATCH
    patch_res = request_patch(cycle_n, hyp, baseline_headline)
    if not patch_res["ok"]:
        out["decision"] = dec.Decision.REVERT.value
        out["reason"] = f"patch_failed: {patch_res['reject_reason']}"
        update_hypothesis(backlog, hyp["id"], status="rejected",
                          reason=out["reason"])
        save_backlog(backlog)
        write_json(cdir / "outcome.json", out)
        log_event({
            "ts": now_iso(), "cycle": cycle_n, "phase": "patch",
            "category": "optimization", "action": "patch_failed",
            "summary": f"{hyp['id']}: {patch_res['reject_reason']}",
            "impact": "low",
            "tags": ["glyph-grid-loop", "revert", "patch-failed"],
        })
        state.cycles_reverted += 1
        state.consecutive_no_keep += 1
        save_state(state)
        return out

    # APPLY + BUILD
    slug = hyp["id"].lower().replace("_", "-")
    build_res = apply_and_build(cycle_n, patch_res["diff"], slug)
    out["build"] = build_res
    if not build_res["build_ok"]:
        if build_res.get("error", "").startswith("timeout"):
            state.consecutive_build_timeouts += 1
        out["decision"] = dec.Decision.REVERT.value
        out["reason"] = f"build_failed: {build_res.get('error')}"
        update_hypothesis(backlog, hyp["id"], status="rejected",
                          reason=out["reason"])
        save_backlog(backlog)
        recovery.revert_workspace()
        write_json(cdir / "outcome.json", out)
        log_event({
            "ts": now_iso(), "cycle": cycle_n, "phase": "build",
            "category": "optimization", "action": "build_failed",
            "summary": f"{hyp['id']}: {build_res.get('error')}",
            "impact": "low",
            "tags": ["glyph-grid-loop", "revert", "build-failed"],
        })
        state.cycles_reverted += 1
        state.consecutive_no_keep += 1
        save_state(state)
        return out

    state.consecutive_build_timeouts = 0  # build succeeded, reset

    # MEASURE
    after_suite, after_headline = run_measure(cycle_n)
    out["after_headline"] = after_headline

    # VERIFY
    verify = run_verify(cycle_n, baseline_suite, after_suite)
    out["ssim_global_min"] = verify["ssim_global_min"]
    out["tests_ok"] = verify["tests_ok"]

    # DECIDE
    inputs = dec.CycleInputs(
        baseline_headline=baseline_headline,
        after_headline=after_headline,
        ssim_global_min=verify["ssim_global_min"],
        build_ok=True, tests_ok=verify["tests_ok"],
        after_geomean_ms=after_headline.get("__geomean__", 999.0),
        baseline_geomean_ms=baseline_headline.get("__geomean__", 999.0),
        default_baseline_ms=baseline_headline.get("cfg-default", 999.0),
        default_after_ms=after_headline.get("cfg-default", 999.0),
        current_best_default_ms=state.cfg_default_floor_ms,
    )
    outcome = dec.decide(inputs)
    out["decision"] = outcome.decision.value
    out["reason"] = outcome.reason
    out["delta_ms"] = outcome.delta_ms

    # COMMIT-OR-REVERT
    if outcome.decision == dec.Decision.KEEP:
        commit_res = commit_and_push(cycle_n, hyp, outcome)
        out["commit"] = commit_res
        # Invalidate stale baseline (we just changed HEAD)
        state.last_baseline_path = None
        state.last_baseline_ts = 0.0
        # Update rolling-min trackers
        new_geomean = after_headline.get("__geomean__", state.current_best_default_ms)
        if new_geomean < state.current_best_default_ms:
            state.current_best_default_ms = new_geomean
        new_default = after_headline.get("cfg-default", state.cfg_default_floor_ms)
        if new_default < state.cfg_default_floor_ms:
            state.cfg_default_floor_ms = new_default
        update_hypothesis(backlog, hyp["id"], status="kept",
                          delta_ms=outcome.delta_ms,
                          ssim_min=outcome.ssim_min,
                          cycle=cycle_n)
        state.cycles_kept += 1
        state.consecutive_no_keep = 0
    else:
        recovery.revert_workspace()
        update_hypothesis(backlog, hyp["id"],
                          status="rejected" if outcome.decision == dec.Decision.REVERT else "escalated",
                          reason=outcome.reason,
                          cycle=cycle_n)
        state.cycles_reverted += 1
        state.consecutive_no_keep += 1
        if outcome.decision == dec.Decision.ESCALATE:
            state.cycles_escalated += 1
    save_backlog(backlog)
    save_state(state)

    out["duration_s"] = time.time() - cycle_t0

    write_json(cdir / "outcome.json", out)
    log_event({
        "ts": now_iso(), "cycle": cycle_n, "phase": "decided",
        "category": "optimization", "action": outcome.decision.value,
        "hypothesis": hyp["id"],
        "summary": f"{hyp['id']}: {outcome.reason}",
        "delta_ms": outcome.delta_ms,
        "ssim_min": outcome.ssim_min,
        "default_after_ms": inputs.default_after_ms,
        "geomean_after_ms": inputs.after_geomean_ms,
        "duration_s": out["duration_s"],
        "impact": "high" if outcome.decision == dec.Decision.KEEP else "low",
        "tags": ["glyph-grid-loop", outcome.decision.value],
    })

    return out


# ---------- Stop conditions ----------

def should_stop(state: State, started_at: float, max_cycles: int | None
                ) -> tuple[bool, str]:
    if STOP_SENTINEL.exists():
        return True, "stop_sentinel"
    if state.interrupted:
        return True, "interrupted"
    if state.consecutive_no_keep >= PLATEAU_CYCLES:
        return True, f"plateau_{PLATEAU_CYCLES}_no_keeps"
    if state.consecutive_build_timeouts >= MAX_CONSEC_BUILD_TIMEOUTS:
        return True, "build_timeout_chain"
    if state.current_best_default_ms <= EARLY_EXIT_GEOMEAN_MS:
        return True, f"target_hit_{EARLY_EXIT_GEOMEAN_MS}ms_geomean"
    if (time.time() - started_at) >= WALL_CLOCK_MAX_S:
        return True, "wall_clock_24h"
    if max_cycles is not None and state.cycles_completed >= max_cycles:
        return True, f"max_cycles_{max_cycles}"
    return False, ""


# ---------- Main ----------

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dry-run", action="store_true",
                    help="Cycle 0 harness validation only")
    ap.add_argument("--max-cycles", type=int, default=None,
                    help="Stop after this many cycles (default: 24h cap)")
    ap.add_argument("--resume", action="store_true",
                    help="Resume from state.json if present")
    args = ap.parse_args()

    RUNS_DIR.mkdir(parents=True, exist_ok=True)
    LOG_PATH.parent.mkdir(parents=True, exist_ok=True)

    backlog = load_backlog()
    if not backlog and not args.dry_run:
        print("ERROR: empty optimization-backlog.yaml", file=sys.stderr)
        return 2

    state = load_state() if args.resume else None
    if state is None:
        state = State(started_at=now_iso())
    save_state(state)

    started_at_ts = time.time()
    log_event({
        "ts": now_iso(), "cycle": -1, "phase": "init",
        "category": "optimization", "action": "loop_start",
        "summary": ("dry-run" if args.dry_run else "full pursuit"),
        "tags": ["glyph-grid-loop", "init"],
        "max_cycles": args.max_cycles, "resume": args.resume,
    })

    def sigint_handler(*_):
        state.interrupted = True
        try:
            save_state(state)
        except Exception as e:   # audit CS-4: never let the handler raise
            print(f"orchestrator: save_state failed in signal handler: {e}",
                  flush=True)
    signal.signal(signal.SIGINT, sigint_handler)
    signal.signal(signal.SIGTERM, sigint_handler)

    cycle_n = state.cycles_completed
    try:
        while True:
            stop, reason = should_stop(state, started_at_ts, args.max_cycles)
            if stop:
                print(f"== loop stop: {reason} ==", flush=True)
                log_event({
                    "ts": now_iso(), "cycle": cycle_n, "phase": "stop",
                    "category": "optimization", "action": "loop_stop",
                    "summary": reason,
                    "tags": ["glyph-grid-loop", "stop", reason],
                })
                break
            try:
                outcome = run_one_cycle(cycle_n, state, backlog,
                                         dry_run=args.dry_run)
                state.cycles_completed = cycle_n + 1
                save_state(state)
                if args.dry_run:
                    print(f"== dry-run cycle {cycle_n} done ==")
                    print(json.dumps(outcome, indent=2, default=str))
                    break
                # Prune old run dirs
                recovery.prune_old_runs(RUNS_DIR, keep=24)
            except Exception:
                tb = traceback.format_exc()
                err_path = cycle_dir(cycle_n) / "error.log"
                err_path.write_text(tb)
                log_event({
                    "ts": now_iso(), "cycle": cycle_n, "phase": "exception",
                    "category": "optimization", "action": "uncaught_exception",
                    "summary": tb.splitlines()[-1][:200],
                    "tags": ["glyph-grid-loop", "exception"],
                })
                recovery.revert_workspace()
                state.cycles_reverted += 1
                state.consecutive_no_keep += 1
                save_state(state)
            cycle_n += 1
    finally:
        log_event({
            "ts": now_iso(), "cycle": state.cycles_completed,
            "phase": "final", "category": "optimization",
            "action": "loop_end",
            "summary": (f"cycles={state.cycles_completed} "
                        f"kept={state.cycles_kept} "
                        f"reverted={state.cycles_reverted} "
                        f"escalated={state.cycles_escalated} "
                        f"best_default={state.current_best_default_ms:.1f}ms"),
            "tags": ["glyph-grid-loop", "end"],
        })
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
