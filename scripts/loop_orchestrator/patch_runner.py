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

    After extraction, hunk headers are recomputed from the actual `+`/`-`/` `
    line counts via :func:`_normalize_hunk_headers` — claude often miscounts
    the per-side line totals (e.g. `@@ -2892,17 +2892,29 @@` when the actual
    hunk has 16 / 25), and `git apply` reads to EOF expecting the missing
    lines and fails with `corrupt patch at line N`.
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
    diff_body = _normalize_hunk_headers(diff_body)
    return diff_body + "\n"  # trailing newline for clean `git apply`


_HUNK_HEADER_RE = re.compile(
    r"^@@ -(?P<old_start>\d+)(?:,(?P<old_count>\d+))?"
    r" \+(?P<new_start>\d+)(?:,(?P<new_count>\d+))? @@(?P<tail>.*)$"
)


def _normalize_hunk_headers(diff_body: str) -> str:
    """Recompute `@@ -X,Y +A,B @@` counts from the actual hunk content.

    LLM-generated diffs frequently miscount Y and B (the per-side line
    totals).  Standard tools like `git diff` always emit correct counts,
    but `git apply` is strict and fails with `corrupt patch at line N` if
    the header overstates the count (it tries to read more content lines
    than exist).

    This pass is purely textual — it does NOT touch context, deletes, or
    adds, so the resulting diff still has whatever ground-truth-vs-real
    drift the LLM produced.  If those CONTEXT lines don't actually match
    the source, `git apply --check` will reject for a different reason
    ("patch does not apply") that points at the real mismatch instead of
    the fictional EOF problem.

    Algorithm: split the diff into header lines (everything up to the first
    `@@`) and hunks (each starting with `@@`).  For each hunk, count
    ` `-prefixed (context) → both sides; `-`-prefixed → old side only;
    `+`-prefixed → new side only.  Skip the special "\\ No newline at end
    of file" marker.  Rewrite the header with correct totals.
    """
    if "@@ " not in diff_body:
        return diff_body

    lines = diff_body.split("\n")
    out: list[str] = []
    i = 0
    while i < len(lines):
        ln = lines[i]
        m = _HUNK_HEADER_RE.match(ln)
        if not m:
            out.append(ln)
            i += 1
            continue

        # Walk hunk body until the next `@@` or end of file/diff.
        body_start = i + 1
        j = body_start
        old_count = 0
        new_count = 0
        while j < len(lines):
            cur = lines[j]
            if _HUNK_HEADER_RE.match(cur):
                break
            # New `diff --git` starts a different file — end of this hunk.
            if cur.startswith("diff --git "):
                break
            if not cur:
                # Trailing blank inside diff body: treated as context by
                # `git apply` only if it's preceded by content; we keep it
                # but don't count it.
                j += 1
                continue
            prefix = cur[0]
            if prefix == " ":
                old_count += 1
                new_count += 1
            elif prefix == "-":
                old_count += 1
            elif prefix == "+":
                new_count += 1
            elif prefix == "\\":
                # `\\ No newline at end of file` — neutral
                pass
            else:
                # Anything else (e.g. another git header) — stop.
                break
            j += 1

        # Rewrite header. Preserve old/new starts and any tail text after
        # the last `@@`.
        old_start = m.group("old_start")
        new_start = m.group("new_start")
        tail = m.group("tail") or ""
        # If a side has count 0, git uses `-X,0` form (no special-case).
        new_header = f"@@ -{old_start},{old_count} +{new_start},{new_count} @@{tail}"
        out.append(new_header)
        out.extend(lines[body_start:j])
        i = j
    return "\n".join(out)


# Source-embedding strategy. The previous version under-shipped excerpts
# (200-line head fallback when sites were at lines 1800-2700) and ignored
# the "lines N, M, P" / "path:N-M" forms used in 6+ of the YAML hypotheses.
# Five consecutive cycles failed with "source excerpts only show lines 1-200"
# before this rewrite — see runs/cycle-000 through cycle-004 (2026-05-10).
#
# New strategy:
#   * Files at or below FULL_FILE_LINES_MAX ship as full content (most cited
#     glyph-grid modules are ≤ 600 lines).
#   * Larger files (index.html is ~3800) ship windows around every detected
#     line reference, with the windows widened from ±60 to ±200.
#   * If still no line refs after expanded extraction, embed a wider fallback
#     of FALLBACK_HEAD_LINES so even uncited files include their public API.
_FULL_FILE_LINES_MAX = 1500
_EXCERPT_BEFORE = 200
_EXCERPT_AFTER = 200
_FALLBACK_HEAD_LINES = 600

# Path-qualified ref: "src/index.html:1774" or "glyph-crt.js:85". Captures a
# range endpoint too so `:1774-1783` produces both 1774 and 1783 windows.
_LINE_REF_RE = re.compile(
    r"(?P<path>[A-Za-z0-9_./\\-]+\.(?:html|js|ts|css|rs)):"
    r"(?P<lo>\d+)(?:[-–](?P<hi>\d+))?"
)
# Standalone "lines 2142, 2485, 2668" or "line 1774" — no path prefix. The
# numbers attribute to whatever file(s) the hypothesis lists. Used when the
# hypothesis says e.g. "depthFog at lines 2142, 2485, 2668" without quoting
# the file path each time.
_STANDALONE_LINES_RE = re.compile(
    r"\blines?\s+(\d+(?:\s*[-,–]\s*\d+)*)",
    re.IGNORECASE,
)


def _read_lines(path: Path) -> list[str]:
    try:
        return path.read_text(errors="replace").splitlines()
    except OSError:
        return []


def _extract_line_refs(text: str, listed_files: list[str]) -> dict[str, list[int]]:
    """Return {file: [line_numbers]} from both qualified and standalone refs.

    Qualified refs (`path:N` or `path:N-M`) attribute to that exact path. A
    range adds both endpoints so the merger covers the full span via two
    windows. Standalone refs (`lines 2142, 2485, 2668`) attribute to every
    file in `listed_files` — the hypothesis is talking about those files.
    """
    refs: dict[str, list[int]] = {}

    # Qualified path:line[-line]
    for m in _LINE_REF_RE.finditer(text):
        path = m.group("path")
        lo = int(m.group("lo"))
        # Heuristic: match by suffix so "glyph-crt.js" matches "src/lib/glyph-crt.js"
        target = next(
            (f for f in listed_files if f.endswith(path) or path.endswith(f)),
            path,
        )
        refs.setdefault(target, []).append(lo)
        if m.group("hi"):
            refs[target].append(int(m.group("hi")))

    # Standalone "lines N, M, P" / "line N"
    standalone_lines: list[int] = []
    for m in _STANDALONE_LINES_RE.finditer(text):
        for token in re.split(r"[,\s]+", m.group(1)):
            for span in token.split("-"):
                span = span.replace("–", "").strip()
                if span.isdigit():
                    standalone_lines.append(int(span))
    if standalone_lines:
        for f in listed_files:
            refs.setdefault(f, []).extend(standalone_lines)

    return refs


def _file_excerpts(hyp: dict[str, Any]) -> str:
    """Build inline source excerpts for every file the hypothesis cites.

    Small files ship in full. Large files ship merged ±200-line windows
    around every detected line reference (qualified and standalone). Each
    excerpt is rendered with absolute line numbers so Claude can match them
    to hunk headers without guessing.
    """
    text = " ".join(str(hyp.get(k, "")) for k in
                    ("hypothesis", "code_shape", "area", "risks"))
    listed_files: list[str] = list(hyp.get("files", []))
    refs = _extract_line_refs(text, listed_files)

    out_blocks: list[str] = []
    for f in listed_files:
        path = REPO_ROOT / f
        if not path.exists():
            out_blocks.append(f"  ### {f}\n  (file does not exist)\n")
            continue
        lines = _read_lines(path)
        n = len(lines)

        # Small file: ship the whole thing.
        if n <= _FULL_FILE_LINES_MAX:
            body_lines: list[str] = [f"  ### {f}  ({n} lines total — full file)"]
            for i in range(1, n + 1):
                body_lines.append(f"  {i:5d}  {lines[i - 1]}")
            out_blocks.append("\n".join(body_lines))
            continue

        # Large file: window around references.
        windows: list[tuple[int, int]] = []
        for ln in refs.get(f, []):
            lo = max(1, ln - _EXCERPT_BEFORE)
            hi = min(n, ln + _EXCERPT_AFTER)
            windows.append((lo, hi))
        if not windows:
            windows.append((1, min(n, _FALLBACK_HEAD_LINES)))
        windows.sort()
        merged: list[tuple[int, int]] = []
        for lo, hi in windows:
            if merged and lo <= merged[-1][1] + 1:
                merged[-1] = (merged[-1][0], max(merged[-1][1], hi))
            else:
                merged.append((lo, hi))
        body_lines = [f"  ### {f}  ({n} lines total — {sum(hi-lo+1 for lo,hi in merged)} lines embedded)"]
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
    Returns {ok, error, mode}.

    Two-stage tolerance ladder:
      1. Strict apply (no flags) — preferred.  Catches every drift.
      2. If strict fails, retry with `--ignore-whitespace`.  This catches
         the common LLM mistake of emitting context lines with subtly
         wrong leading whitespace (e.g. cycle-3 OPT-010 had 11-space
         indent where the source has 9-space).  JS / HTML are
         whitespace-insensitive at runtime, so semantically the patch
         still produces a bit-identical build of the affected functions.
    The `mode` field on success says which path applied — `"strict"` or
    `"ws-relaxed"` — so the orchestrator can flag ws-relaxed diffs as
    "manual review recommended" before they hit a KEEP commit.
    """
    diff_path = REPO_ROOT / ".git" / "auto-patches" / f"{branch}.diff"
    diff_path.parent.mkdir(parents=True, exist_ok=True)
    diff_path.write_text(diff_text)

    def _run(extra: list[str]) -> tuple[int, str]:
        proc = subprocess.run(
            ["git", "apply", *extra, str(diff_path)],
            capture_output=True, text=True, cwd=str(REPO_ROOT),
        )
        return proc.returncode, proc.stderr.strip()

    def _check(extra: list[str]) -> tuple[int, str]:
        proc = subprocess.run(
            ["git", "apply", "--check", *extra, str(diff_path)],
            capture_output=True, text=True, cwd=str(REPO_ROOT),
        )
        return proc.returncode, proc.stderr.strip()

    # Stage 1: strict
    rc, err = _check([])
    mode = "strict"
    if rc != 0:
        # Stage 2: try whitespace-tolerant.  We pass --recount as
        # belt-and-braces in case anything slipped past _normalize_hunk_headers.
        rc2, err2 = _check(["--ignore-whitespace", "--recount"])
        if rc2 != 0:
            # Original error is more useful than the relaxed error —
            # they often match anyway.
            return {"ok": False, "error": f"check failed: {err}",
                    "mode": None}
        mode = "ws-relaxed"

    # Branch + apply with the same flags that passed --check
    subprocess.run(["git", "checkout", "-B", branch], capture_output=True,
                   text=True, cwd=str(REPO_ROOT))
    apply_flags = ["--ignore-whitespace", "--recount"] if mode == "ws-relaxed" else []
    rc, err = _run(apply_flags)
    if rc != 0:
        return {"ok": False, "error": f"apply failed: {err}", "mode": None}
    return {"ok": True, "error": None, "mode": mode}


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
