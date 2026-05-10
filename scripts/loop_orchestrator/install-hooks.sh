#!/usr/bin/env bash
# Install the pre-commit deny-list hook for the autonomous loop.
#
# Hook blocks commits that touch any of the rails-protected paths.
# The loop catches this and ESCALATES (its own commit fails),
# preventing autonomous changes to: build config, codesigning,
# CI workflows, the regulator (run-all.sh), the orchestrator itself,
# or existing BUGS_AND_ITERATIONS entries.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
HOOK_PATH="$REPO_ROOT/.git/hooks/pre-commit"

cat > "$HOOK_PATH" <<'EOF'
#!/usr/bin/env bash
# Auto-installed by scripts/loop_orchestrator/install-hooks.sh.
# Blocks commits that touch any path in the deny-list.
#
# To skip (manual emergency commit only): GLYPHLOOP_BYPASS=1 git commit ...
set -euo pipefail

if [[ "${GLYPHLOOP_BYPASS:-0}" == "1" ]]; then
  echo "pre-commit: GLYPHLOOP_BYPASS=1 → skipping deny-list check"
  exit 0
fi

DENY=(
  "src-tauri/Cargo.toml"
  "src-tauri/tauri.conf.json"
  "src-tauri/entitlements.plist"
  "package.json"
  ".github/workflows/"
  "tests/run-all.sh"
  "scripts/loop_orchestrator/orchestrator.py"
  "scripts/loop_orchestrator/benchmark.py"
  "scripts/loop_orchestrator/score_ssim.py"
  "scripts/loop_orchestrator/decide.py"
  "scripts/loop_orchestrator/patch_runner.py"
  "scripts/loop_orchestrator/recovery.py"
  "scripts/loop_orchestrator/optimization-backlog.yaml"
  "scripts/loop_orchestrator/gen_synthetic_sources.py"
  "scripts/loop_orchestrator/install-hooks.sh"
)

# Check staged paths against deny-list
staged=$(git diff --cached --name-only)
violations=()
while IFS= read -r path; do
  for pattern in "${DENY[@]}"; do
    if [[ "$path" == "$pattern" || "$path" == "$pattern"* ]]; then
      violations+=("$path (matches $pattern)")
    fi
  done
done <<< "$staged"

# BUGS_AND_ITERATIONS.md is APPEND-ONLY for the loop. Reject any commit
# that modifies prior content (via a heuristic: if the line count
# decreased OR existing line ranges are changed). Cheap proxy: forbid
# any line that starts with `-` (a removal) inside that file.
biat_diff=$(git diff --cached -- BUGS_AND_ITERATIONS.md 2>/dev/null | grep -E "^-[^-]" || true)
if [[ -n "$biat_diff" ]]; then
  echo "pre-commit: BUGS_AND_ITERATIONS.md is append-only — removals detected:"
  echo "$biat_diff" | head -20
  exit 1
fi

if (( ${#violations[@]} > 0 )); then
  echo "pre-commit: deny-list paths modified by an autonomous-loop commit:"
  for v in "${violations[@]}"; do echo "  $v"; done
  echo
  echo "If this is intentional human-supervised work:"
  echo "  GLYPHLOOP_BYPASS=1 git commit ..."
  exit 1
fi

exit 0
EOF

chmod +x "$HOOK_PATH"
echo "installed: $HOOK_PATH"
