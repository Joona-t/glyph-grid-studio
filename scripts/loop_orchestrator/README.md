# Glyph Grid — Autonomous Optimization Loop

24-hour pursuit harness: research → patch → build → measure → verify → decide → commit.

## Bootstrap (one-time)

```bash
cd /Users/darkfire/glyph-grid-studio/scripts/loop_orchestrator
python3 -m pip install -r requirements.txt
python3 gen_synthetic_sources.py        # write tests/sources/*
python3 orchestrator.py --dry-run       # cycle 0 harness validation
```

If the dry-run reports `n_variants=20, n_failures=0` and the headline
geomean lines print, the harness is ready.

## Run modes

```bash
python3 orchestrator.py                  # full 24-h pursuit
python3 orchestrator.py --max-cycles 4   # bounded run (testing)
python3 orchestrator.py --resume         # resume from state.json
```

To stop a running pursuit: `touch runs/STOP` or send SIGINT.

## Files

| File | Role |
|---|---|
| `orchestrator.py` | Main loop, state machine, stop conditions |
| `benchmark.py` | Manifest builder, studio invocation, PERF_JOB parser |
| `score_ssim.py` | scikit-image SSIM at 0.985 threshold |
| `decide.py` | Keep/revert/escalate state machine |
| `patch_runner.py` | `claude code` subprocess for the PATCH phase |
| `recovery.py` | Build / freeze / OOM / disk recovery |
| `optimization-backlog.yaml` | Hypothesis queue (P0/P1/P2) |
| `gen_synthetic_sources.py` | Deterministic bench-source PNGs |
| `state.json` | Run state (auto-saved, gitignored) |
| `runs/` | Per-cycle artifacts (gitignored, auto-pruned to 24) |

## Logs

JSONL appended to
`~/.claude/data/automation-log/glyph-grid-loop.jsonl` — strict superset
of automation-log schema; existing dashboard renders unchanged.

## Stop conditions

- `cfg-default` geomean ≤ 80 ms (declare win)
- 5 consecutive cycles without KEEP (plateau)
- 3 consecutive build timeouts (toolchain broken)
- Wall-clock ≥ 24 h
- `runs/STOP` sentinel file or SIGINT

## Safety rails (denied paths → ESCALATE)

`src-tauri/Cargo.toml`, `tauri.conf.json`, `entitlements.plist`,
`package.json` deps, `.github/workflows/`, `BUGS_AND_ITERATIONS.md`
(existing entries), `tests/run-all.sh`, `scripts/loop_orchestrator/**`.

## Hard gates (cycle fails if any one breaks)

1. Bit-exact reproducibility (same seed = same bytes)
2. No new per-frame allocations on `// HOT-PATH:` functions
3. Default config ≤ 1.05 × current best (no helps-X-regresses-default trades)
4. Hot-path branch count not increased (unless YAML waives)
5. `tests/run-all.sh` passes
6. SSIM ≥ 0.985
