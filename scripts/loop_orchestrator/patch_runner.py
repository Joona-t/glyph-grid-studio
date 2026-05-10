"""Patch generator for the autonomous loop.

Spawns `claude -p '<prompt>'` as a subprocess (uses Joona's Pro/Max
subscription per CLAUDE.md rule #10 — no paid LLM API). The prompt provides:
  * The hypothesis YAML entry
  * **Inline excerpts of every cited file** — ground truth for the diff
  * Hard constraints: ≤3 files / ≤200 LOC net / single concern
  * Output contract: a unified diff applicable with `git apply`

Returns the diff text (or None on failure / timeout).

Hardening trail (2026-05-10):
  * `_extract_diff` tolerates markdown fences and chatty preambles —
    `claude -p` often prepends a one-line ack even when told not to.
  * Prompt **embeds file excerpts inline** rather than asking the sub-agent
    to call Read tool. Two reasons: (a) `claude -p` in headless mode would
    need `--dangerously-skip-permissions` to skip the per-tool prompts,
    which builds an unbounded agent loop and is correctly blocked by
    permission gating; (b) embedding the source in the prompt is cheaper
    (one round-trip) and gives Claude the *exact* lines it must match in
    the hunk header.
  * Hard constraint: hunks must match the embedded source — Claude is told
    explicitly that hunk-header drift = automatic REJECT.
"""
from __future__ import annotations

import os
import re
import subprocess
import textwrap
from pathlib import Path
from typing import Any

REPO_ROOT = Path("/Users/darkfire/glyph-grid-studio")
CLAUDE_BIN = "claude"  # CLI in PATH (Pro/Max subscription)
PATCH_TIMEOUT_S = 600


# `claude -p` in headless mode often wraps unified diffs in markdown code
# fences (```diff … ```) or prepends a one-line acknowledgement, even when
# told not to. The orchestrator's `git apply` only needs a clean diff, so we
# extract it permissively rather than failing the cycle on cosmetic noise.
_DIFF_START_RE = re.compile(r"^diff --git ", re.MULTILINE)


def _extract_diff(stdout: str) -> str | None:
    """Return the unified diff body found anywhere in `stdout`, or None.

    Handles three observed shapes:
      1. Pure diff (already starts with `diff --git`)
      2. ```diff\\n<diff>\\n```  (markdown fenced block)
      3. <prose preamble>\\n\\ndiff --git …\\n…  (chatty preamble)
    """
    if not stdout:
        return None
    text = stdout.strip()
    # Strip outer markdown fence if present
    if text.startswith("```"):
        nl = text.find("\n")
        if nl != -1:
            text = text[nl + 1:]
        if text.rstrip().endswith("```"):
            text = text.rstrip()[: -3]
    m = _DIFF_START_RE.search(text)
    if not m:
        return None
    diff_body = text[m.start():].rstrip()
    # If a markdown fence appears mid-body (e.g. assistant closed early),
    # cut before it.
    fence_in_body = diff_body.find("\n```")
    if fence_in_body != -1:
        diff_body = diff_body[:fence_in_body].rstrip()
    return diff_body + "\n"  # trailing newline for clean `git apply`


# Maximum lines of source to embed per cited file. Glyph Grid's `index.html`
# is ~3000 lines; embedding all of it in every prompt is wasteful. We grab a
# window around any explicit `path:line` references in the hypothesis text;
# if no line numbers are cited, we embed the head of the file as a fallback.
_EXCERPT_BEFORE = 60
_EXCERPT_AFTER = 60
_FALLBACK_HEAD_LINES = 200
_LINE_REF_RE = re.compile(r"([A-Za-z0-9_./\\-]+\.(?:html|js|ts|css|rs)):(\d+)")


def _read_lines(path: Path) -> list[str]:
    try:
        return path.read_text(errors="replace").splitlines()
    except OSError:
        return []


def _file_excerpts(hyp: dict[str, Any]) -> str:
    """Build inline source excerpts for every file the hypothesis cites.

    Strategy:
      1. Parse `<file>:<line>` references from `hypothesis` and `code_shape`.
      2. For each cited line, take a ±60-line window.
      3. Merge overlapping windows per file.
      4. If a `files:` entry has no explicit line reference, embed the first
         200 lines of the file (better than nothing for short modules).
    Each excerpt is rendered with absolute line numbers so Claude can match
    them to hunk headers without guessing.
    """
    text = " ".join(str(hyp.get(k, "")) for k in
                    ("hypothesis", "code_shape", "area", "risks"))
    refs: dict[str, list[int]] = {}
    for m in _LINE_REF_RE.finditer(text):
        refs.setdefault(m.group(1), []).append(int(m.group(2)))

    listed_files: list[str] = list(hyp.get("files", []))
    out_blocks: list[str] = []
    for f in listed_files:
        path = REPO_ROOT / f
        if not path.exists():
            out_blocks.append(f"  ### {f}\n  (file does not exist)\n")
            continue
        lines = _read_lines(path)
        n = len(lines)
        windows: list[tuple[int, int]] = []
        for ln in refs.get(f, []):
            lo = max(1, ln - _EXCERPT_BEFORE)
            hi = min(n, ln + _EXCERPT_AFTER)
            windows.append((lo, hi))
        if not windows:
            windows.append((1, min(n, _FALLBACK_HEAD_LINES)))
        # Merge overlaps
        windows.sort()
        merged: list[tuple[int, int]] = []
        for lo, hi in windows:
            if merged and lo <= merged[-1][1] + 1:
                merged[-1] = (merged[-1][0], max(merged[-1][1], hi))
            else:
                merged.append((lo, hi))
        body_lines: list[str] = [f"  ### {f}  ({n} lines total)"]
        for lo, hi in merged:
            body_lines.append(f"  --- lines {lo}-{hi} ---")
            for i in range(lo, hi + 1):
                src = lines[i - 1] if 1 <= i <= n else ""
                body_lines.append(f"  {i:5d}  {src}")
        out_blocks.append("\n".join(body_lines))
    return "\n\n".join(out_blocks) if out_blocks else "  (no files cited)"


def build_patch_prompt(hyp: dict[str, Any], baseline_ms: dict[str, float]) -> str:
    """Compose the prompt that goes to `claude -p`. Self-contained
    (the subprocess Claude session has no memory of this conversation)."""
    files_txt = "\n".join(f"  - {f}" for f in hyp.get("files", []))
    code_shape = hyp.get("code_shape", "(see hypothesis)")
    baseline_lines = "\n".join(f"  - {k}: {v:.1f} ms" for k, v in
                                sorted(baseline_ms.items()))
    risks = hyp.get("risks", "Standard hot-path risks (see CLAUDE.md).")
    expected = hyp.get("expected_gain_ms", "unspecified")
    benchmark_first = hyp.get("benchmark_first", False)
    prior = hyp.get("prior_attempt") or hyp.get("reason")
    excerpts = _file_excerpts(hyp)

    prior_block = (f"\n\n        # Prior attempt notes\n        {prior}\n"
                   if prior else "")

    return textwrap.dedent(f"""
        You are the patch generator inside a 24-hour autonomous optimization
        loop for /Users/darkfire/glyph-grid-studio (a Tauri desktop app).
        You have NO tool access — write the diff entirely from the source
        excerpts embedded below.

        # Hypothesis
        ID: {hyp['id']}
        Area: {hyp.get('area', 'unspecified')}
        Statement: {hyp.get('hypothesis', '(see backlog)')}
        Expected gain: {expected}
        BENCHMARK FIRST: {benchmark_first}

        # Critical files
        {files_txt}

        # Code shape sketch (illustrative — match real source, not this)
        ```
        {code_shape}
        ```

        # Risks
        {risks}{prior_block}

        # Current baseline (geomean ms/frame, this cycle)
        {baseline_lines}

        # Source excerpts (ground truth)
{excerpts}

        # Hard constraints
        - Diff must touch ≤3 files and ≤200 LOC net.
        - Single concern only — do not bundle other improvements.
        - NO new per-frame allocations inside any function tagged
          `// HOT-PATH:` (look at the comment markers in the excerpts).
        - NO modifications to: src-tauri/Cargo.toml, src-tauri/tauri.conf.json,
          src-tauri/entitlements.plist, package.json dependencies,
          .github/workflows/, BUGS_AND_ITERATIONS.md (existing entries),
          tests/run-all.sh, scripts/loop_orchestrator/.
        - Bit-exact reproducibility: same (source, config, seed) must produce
          identical bytes pre/post change. If the change introduces non-
          determinism, REJECT yourself.
        - **Hunk headers and context lines must match the embedded source
          EXACTLY.** Do NOT fabricate `// HOT-PATH:` comments or any other
          context that is not present in the excerpts above. The orchestrator
          runs `git apply --check` and any drift = patch rejected.

        # Output contract
        Respond with ONLY a unified diff (`diff --git a/... b/...` format)
        that applies with `git apply`. No prose, no explanation. Optional
        markdown fence around the diff is tolerated; chatty preambles are
        not. The orchestrator feeds your stdout to a permissive extractor.

        If you determine the hypothesis cannot be implemented within the
        constraints, or the embedded source contradicts the hypothesis,
        respond with the literal single line:
        `REJECT: <one-sentence reason>`
        and nothing else.

        Begin diff:
    """).strip()


def request_patch(hyp: dict[str, Any], baseline_ms: dict[str, float],
                  *, timeout_s: int = PATCH_TIMEOUT_S) -> dict:
    """Call `claude -p '<prompt>'` and return the result.

    Returns:
      {ok: bool, diff: str | None, reject_reason: str | None,
       stderr: str, exit_code: int}
    """
    prompt = build_patch_prompt(hyp, baseline_ms)
    # NOTE: we deliberately do NOT pass --dangerously-skip-permissions.
    # The sub-session has no tool access; everything it needs (file
    # excerpts, baseline ms, prior-attempt notes) is embedded in the
    # prompt above. This keeps the loop's blast radius small.
    cmd = [CLAUDE_BIN, "-p", prompt]
    env = os.environ.copy()
    try:
        proc = subprocess.run(
            cmd, capture_output=True, text=True,
            timeout=timeout_s, env=env, cwd=str(REPO_ROOT),
        )
        stdout = proc.stdout.strip()
        stderr = proc.stderr
        rc = proc.returncode
    except subprocess.TimeoutExpired as e:
        return {"ok": False, "diff": None,
                "reject_reason": f"timeout after {timeout_s}s",
                "stderr": (e.stderr or b"").decode("utf-8", "replace") if e.stderr else "",
                "exit_code": 124}
    except FileNotFoundError:
        return {"ok": False, "diff": None,
                "reject_reason": f"`{CLAUDE_BIN}` not on PATH",
                "stderr": "", "exit_code": 127}

    if rc != 0:
        return {"ok": False, "diff": None,
                "reject_reason": f"claude exited {rc}",
                "stderr": stderr, "exit_code": rc}

    # Self-rejected — accept the literal first line OR a leading `REJECT:`
    # token in the first non-empty line (claude sometimes prepends an ack).
    first_line = next((ln for ln in stdout.splitlines() if ln.strip()), "")
    if first_line.lstrip().startswith("REJECT:"):
        reason = first_line.split("REJECT:", 1)[1].strip()
        return {"ok": False, "diff": None,
                "reject_reason": reason or "self-rejected",
                "stderr": stderr, "exit_code": rc}

    # Permissively extract the diff body. Tolerates markdown fences and
    # chatty preambles — only the diff is fed to `git apply`, so cosmetic
    # noise is harmless once stripped.
    diff_text = _extract_diff(stdout)
    if not diff_text:
        return {"ok": False, "diff": None,
                "reject_reason": "no `diff --git` block found in stdout",
                "stderr": stderr, "exit_code": rc}

    return {"ok": True, "diff": diff_text,
            "reject_reason": None,
            "stderr": stderr, "exit_code": rc}


def apply_patch(diff_text: str, *, branch: str) -> dict:
    """Create branch, write diff, `git apply --check` then `git apply`.
    Returns {ok, error}."""
    diff_path = REPO_ROOT / ".git" / "auto-patches" / f"{branch}.diff"
    diff_path.parent.mkdir(parents=True, exist_ok=True)
    diff_path.write_text(diff_text)

    # Validate
    chk = subprocess.run(["git", "apply", "--check", str(diff_path)],
                         capture_output=True, text=True, cwd=str(REPO_ROOT))
    if chk.returncode != 0:
        return {"ok": False, "error": f"check failed: {chk.stderr.strip()}"}

    # Branch + apply
    subprocess.run(["git", "checkout", "-B", branch], capture_output=True,
                   text=True, cwd=str(REPO_ROOT))
    ap = subprocess.run(["git", "apply", str(diff_path)],
                        capture_output=True, text=True, cwd=str(REPO_ROOT))
    if ap.returncode != 0:
        return {"ok": False, "error": f"apply failed: {ap.stderr.strip()}"}
    return {"ok": True, "error": None}


def revert_to_main() -> None:
    """Discard working-tree changes and return to main."""
    subprocess.run(["git", "reset", "--hard"], capture_output=True,
                   text=True, cwd=str(REPO_ROOT))
    subprocess.run(["git", "checkout", "main"], capture_output=True,
                   text=True, cwd=str(REPO_ROOT))


if __name__ == "__main__":
    # Smoke: build prompt for a fake hypothesis.
    fake = {
        "id": "OPT-FAKE",
        "area": "demo",
        "hypothesis": "Add a no-op comment to verify the harness.",
        "files": ["README.md"],
        "code_shape": "// safe to ignore",
        "risks": "None.",
        "expected_gain_ms": "0",
        "benchmark_first": False,
    }
    p = build_patch_prompt(fake, {"__geomean__": 122.0, "cfg-default": 122.0})
    print(p[:500])
    print("...")
