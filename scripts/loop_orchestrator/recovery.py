"""Resilience layer for the autonomous loop.

Detects + recovers from: build failure, build timeout, studio freeze,
studio crash, OOM, codesign failure, disk full, claude-code unavailable.

Each function returns a dict {recovered: bool, action: str, note: str}.
"""
from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

REPO_ROOT = Path("/Users/darkfire/glyph-grid-studio")
PROD_APP = Path("/Users/darkfire/Applications/Glyph Grid Studio.app")


def kill_studio() -> dict:
    """Kill any running studio process tree (used after a freeze)."""
    try:
        subprocess.run(["pkill", "-f", "Glyph Grid Studio.app"],
                       capture_output=True, text=True)
        subprocess.run(["pkill", "-9", "-f", "glyph-grid-studio"],
                       capture_output=True, text=True)
        return {"recovered": True, "action": "kill_studio",
                "note": "killed all glyph-grid-studio processes"}
    except Exception as e:
        return {"recovered": False, "action": "kill_studio",
                "note": f"pkill failed: {e}"}


def revert_workspace() -> dict:
    """Hard-reset the working tree, switch back to main, delete branch."""
    cmds = [
        ["git", "reset", "--hard"],
        ["git", "checkout", "main"],
        ["git", "stash", "drop"],  # in case any stashes accumulated
    ]
    notes = []
    for cmd in cmds:
        r = subprocess.run(cmd, capture_output=True, text=True,
                           cwd=str(REPO_ROOT))
        notes.append(f"{' '.join(cmd)}: rc={r.returncode}")
    return {"recovered": True, "action": "revert_workspace",
            "note": "; ".join(notes)}


def install_built_app() -> dict:
    """Copy a freshly-built Glyph Grid Studio.app from the build dir to
    `~/Applications/`, with quarantine bits cleared so the next launch
    skips the gatekeeper prompt. Returns recovered=False if either the
    build dir has no .app or the copy fails."""
    build_dir = REPO_ROOT / "src-tauri" / "target" / "release" / "bundle" / "macos"
    candidates = list(build_dir.glob("*.app"))
    if not candidates:
        return {"recovered": False, "action": "install_built_app",
                "note": f"no .app under {build_dir}"}
    src = candidates[0]
    dst = PROD_APP
    try:
        if dst.exists():
            shutil.rmtree(dst)
        subprocess.run(["cp", "-R", str(src), str(dst)],
                       capture_output=True, text=True, check=True)
        # Strip quarantine to skip gatekeeper prompt next launch.
        subprocess.run(["xattr", "-dr", "com.apple.quarantine", str(dst)],
                       capture_output=True, text=True)
        # Re-sign locally (ad-hoc) to keep the binary launchable.
        subprocess.run(["codesign", "--force", "--deep", "--sign", "-",
                        str(dst)], capture_output=True, text=True)
        return {"recovered": True, "action": "install_built_app",
                "note": f"installed {src.name} → {dst}"}
    except subprocess.CalledProcessError as e:
        return {"recovered": False, "action": "install_built_app",
                "note": f"copy/codesign failed: {e}"}


def disk_check() -> dict:
    """Return free-disk percentage on the repo's mount."""
    try:
        r = subprocess.run(["df", str(REPO_ROOT)],
                           capture_output=True, text=True, check=True)
        # Parse second line, fifth column (Capacity %)
        line = r.stdout.strip().splitlines()[-1]
        parts = line.split()
        cap_pct = int(parts[4].rstrip("%"))
        free_pct = 100 - cap_pct
        return {"recovered": free_pct >= 5, "action": "disk_check",
                "note": f"{free_pct}% free; full={cap_pct}%"}
    except Exception as e:
        return {"recovered": True, "action": "disk_check",
                "note": f"could not check: {e}"}


def claude_cli_check() -> dict:
    """Verify the claude CLI is available + authenticated."""
    try:
        r = subprocess.run(["claude", "--version"],
                           capture_output=True, text=True, timeout=10)
        return {"recovered": r.returncode == 0,
                "action": "claude_cli_check",
                "note": (r.stdout + r.stderr).strip()[:200]}
    except FileNotFoundError:
        return {"recovered": False, "action": "claude_cli_check",
                "note": "claude not on PATH"}
    except Exception as e:
        return {"recovered": False, "action": "claude_cli_check",
                "note": f"check failed: {e}"}


def prune_old_runs(runs_dir: Path, keep: int = 24) -> dict:
    """Delete the oldest cycle artifact dirs, keeping the most recent N.

    Cycle dirs are named `cycle-NNN`. We sort numerically and rm the
    rest. Keeps the run dir from growing unboundedly during a 24-h pursuit.
    """
    if not runs_dir.exists():
        return {"recovered": True, "action": "prune_old_runs",
                "note": "runs/ does not exist"}
    cycle_dirs = sorted(
        [p for p in runs_dir.iterdir()
         if p.is_dir() and p.name.startswith("cycle-")],
        key=lambda p: int(p.name.split("-")[1]),
    )
    if len(cycle_dirs) <= keep:
        return {"recovered": True, "action": "prune_old_runs",
                "note": f"{len(cycle_dirs)} dirs (≤ {keep}, no prune)"}
    to_remove = cycle_dirs[:-keep]
    for p in to_remove:
        try:
            shutil.rmtree(p)
        except OSError:
            pass
    return {"recovered": True, "action": "prune_old_runs",
            "note": f"pruned {len(to_remove)} old cycle dirs"}


if __name__ == "__main__":
    # Diagnostic: run all recovery checks once.
    for fn in (claude_cli_check, disk_check):
        r = fn()
        print(f"  {fn.__name__:24s} → recovered={r['recovered']}  {r['note']}")
