# Optimization Loop — Plan

> **Status (2026-05-10 — APPROVED + IMPLEMENTED).** Joona approved with "implement it all".
> The implementation already existed in `scripts/loop_orchestrator/` (Python harness,
> staged in git the same evening). This document is preserved as the *design
> rationale* the Python harness was reconciled against — every section below
> maps to a concrete file in that directory:
>
> | Plan section                  | File in `scripts/loop_orchestrator/`        |
> |-------------------------------|---------------------------------------------|
> | Per-cycle protocol            | `orchestrator.py` (`run_cycle`)             |
> | Manifest builder + harness    | `benchmark.py`                              |
> | KEEP/REVERT/ESCALATE rules    | `decide.py`                                 |
> | Patch generation via `claude` | `patch_runner.py`                           |
> | SSIM ≥ 0.985 gate             | `score_ssim.py`                             |
> | Build/freeze/OOM recovery     | `recovery.py`                               |
> | Hypothesis queue (P0/P1/P2)   | `optimization-backlog.yaml`                 |
> | Stop conditions, state, logs  | `orchestrator.py` (`State`, `LOG_PATH`)     |
> | Bash `bench/loop.sh` (drafted)| **not built** — superseded by Python harness|
>
> **What runs in production:** `python3 scripts/loop_orchestrator/orchestrator.py`.
> See `scripts/loop_orchestrator/README.md` for invocation, stop conditions, hard
> gates. The orchestrator writes JSONL events to
> `~/.claude/data/automation-log/glyph-grid-loop.jsonl` so the existing
> automation dashboard renders them unchanged.
>
> **One bug fixed during reconciliation:** `patch_runner.py:_extract_diff` —
> originally rejected any `claude -p` output that didn't *start* with `diff --git`,
> losing two parser-clean hypotheses (OPT-003 postprocess gate, OPT-004 scratch
> buffers) where Claude prepended a one-line ack. The runner now strips markdown
> fences and chatty preambles, then feeds the diff body to `git apply --check`.
> OPT-003 and OPT-004 were re-queued in the backlog.
>
> **Companion:** `OPTIMIZATION-LOOP-RESEARCH.md` (the *why*) and
> `OPTIMIZATION-AUDIT-CARMACK.md` (the *what* of individual fixes).
> **Goal:** ship the existing F1–F10 audit findings + auto-discovered candidates
> via a self-running 24-hour optimize-test-commit loop, with FFmpeg-discipline
> regression gates.

---

## 1. The system at a glance

```
┌──────────────────────────────────────────────────────────────┐
│                     bench/loop.sh                             │
│  (24-hour driver: lockfile-gated, plateau-terminating)        │
│                                                                │
│           ┌────────────────────────────────┐                  │
│           ▼                                ▲                  │
│   ┌────────────────────┐         (commit win  )               │
│   │   bench/cycle.sh   │ ─────►  (or revert  ) ─────┐         │
│   │  (one optimization │         (and update queue) │         │
│   │   cycle)           │ ◄───────────────────────┐  │         │
│   └────────────────────┘                         │  │         │
│           │                                      │  │         │
│           ├──► bench/run.sh        (perf JSON)   │  │         │
│           ├──► bench/visdiff.sh    (visual gate) │  │         │
│           ├──► research candidate                │  │         │
│           ├──► implement (Edit)                  │  │         │
│           ├──► run.sh + visdiff post             │  │         │
│           └──► decide → git commit OR git reset ─┘  │         │
│                                                                │
└──────────────────────────────────────────────────────────────┘
```

Three atomic scripts (`run.sh`, `visdiff.sh`, `cycle.sh`) plus a driver (`loop.sh`). All in a new `bench/` directory at the project root.

The Claude `/loop` skill is an alternative driver if Joona prefers Claude orchestration over a shell loop.

---

## 2. Files to create

All paths relative to `/Users/darkfire/glyph-grid-studio/`.

### 2.1 Bench infrastructure

```
bench/
├── run.sh                    # perf snapshot at 3 configs → tests/perf/<ts>.json
├── visdiff.sh                # render 6 golden configs → diff vs tests/golden/*.png
├── cycle.sh                  # one full optimization cycle (pre → research → impl → post → decide)
├── loop.sh                   # 24-hour driver
├── candidate-queue.jsonl     # priority-ordered queue of optimization candidates
├── configs/
│   ├── low.json              # 60×30, postprocess off, ASCII
│   ├── default.json          # 240×120, vignette only, octant
│   └── heavy.json            # 400×300, full postprocess, dense braille
├── golden/
│   ├── octant-cream-paper.png
│   ├── ascii-flow-field.png
│   ├── sextant-dispersal.png
│   ├── braille-dense.png
│   ├── fence-sparse.png
│   └── postprocess-stack.png
└── lib/
    ├── perf-aggregate.mjs    # parses perf marks JSON, computes p50/p99
    ├── visdiff.mjs           # PNG comparison via pixel diff
    └── decide.mjs            # decision logic for cycle.sh: win/no-win/plateau
```

### 2.2 Output artifacts (generated, not authored)

```
tests/
├── perf/
│   ├── pre.json              # snapshot before this cycle
│   ├── post.json             # snapshot after this cycle
│   ├── cycle-log.jsonl       # append-only log of every cycle decision
│   └── loop-final-<ts>.md    # summary report when loop terminates
├── visdiff-out/              # diff visualizations for failed cycles
└── golden/                   # checked-in golden PNGs (regenerated rarely; visdiff baseline)
```

### 2.3 Documentation (this turn produced first two)

- `OPTIMIZATION-LOOP-RESEARCH.md` ✓ written
- `OPTIMIZATION-LOOP-PLAN.md` ✓ this file
- `bench/README.md` — quick-start, how to invoke, how to interpret cycle-log

---

## 3. Build order — what to write, in what sequence, with acceptance criteria

Each item is a discrete deliverable. The loop only runs once items 1–7 are landed and verified.

### 3.1 Build phase 1 — instrumentation harness (no loop yet)

| # | Deliverable | Acceptance criterion |
|---|---|---|
| 1 | `bench/configs/{low,default,heavy}.json` | Each config loads cleanly via `tauri-cli render --config bench/configs/X.json --frames 1` |
| 2 | `bench/lib/perf-aggregate.mjs` | Given a JSONL stream of `performance.mark` data from 300 frames, emits `{stages: {scene: {p50, p99, max}, ...}, total: {...}}` |
| 3 | `bench/run.sh` | Renders 300 frames per config, calls perf-aggregate, writes `tests/perf/<ts>.json`. Exit 0 on success. |
| 4 | First baseline snapshot committed to `tests/perf/baseline.json` | Run `bench/run.sh` once. The JSON file's p50 totals match the existing audit's claims (default ~151ms ± 15%). |

After phase 1: we can numerically prove "before" for any optimization.

### 3.2 Build phase 2 — visual regression gate

| # | Deliverable | Acceptance criterion |
|---|---|---|
| 5 | `bench/golden/*.png` (6 files) | Generate by hand running each of 6 representative configs; commit. |
| 6 | `bench/lib/visdiff.mjs` | Given two PNGs, computes mean absolute error per channel. Returns numeric value. |
| 7 | `bench/visdiff.sh` | Renders 6 configs, compares each to golden, exits 0 if all < 0.5% MAE, else writes diff vis to `tests/visdiff-out/` and exits 1. |

After phase 2: any optimization can be visually verified against a known-good baseline.

### 3.3 Build phase 3 — cycle script

| # | Deliverable | Acceptance criterion |
|---|---|---|
| 8 | `bench/candidate-queue.jsonl` | Pre-populated with F1–F10 from audit, each line `{ id, source: "audit", finding: "F1", description, expectedGain, risk, attempts: 0, blocklisted: false }` |
| 9 | `bench/lib/decide.mjs` | Given pre-perf, post-perf, visdiff result → returns `"win" | "no-win" | "plateau"` per the rules in §3 below |
| 10 | `bench/cycle.sh` | Full cycle protocol (§4). Returns 0 (win), 1 (no-win), 2 (plateau). |

Acceptance test: `bench/cycle.sh` against a candidate that's been pre-implemented (F1's `clearCache()` removal). Should detect the win, commit it, return 0.

### 3.4 Build phase 4 — loop driver

| # | Deliverable | Acceptance criterion |
|---|---|---|
| 11 | `bench/loop.sh` | 24-hour driver, lockfile-gated, plateau-terminating, max-cycles-capped. |
| 12 | `bench/README.md` | Documents how to start/stop/resume the loop, what each script does, how to interpret outputs. |

Acceptance test: dry-run `bench/loop.sh` with `MAX_CYCLES=1` — runs one cycle, reports outcome, exits cleanly. Lockfile created and removed.

### 3.5 Build phase 5 — first real loop session

| # | Deliverable | Acceptance criterion |
|---|---|---|
| 13 | First multi-cycle loop run | Pause after 3 cycles, review `cycle-log.jsonl`, verify all decisions were correct (no false-positive wins, no false-negative reverts). |
| 14 | Tuning pass | Adjust visdiff threshold (0.5% may be wrong), perf-delta gate (5% may be wrong), candidate priority based on actual results. |

After phase 5: the loop is calibrated and trusted. We can let it run overnight without supervision.

---

## 4. Per-cycle protocol (`bench/cycle.sh`)

The single most important script. This is the contract.

```
1. Pre-flight check
   - Confirm `git status` is clean (no uncommitted changes from previous cycle)
   - Confirm bench/lockfile present (otherwise loop has been paused)

2. Pre-snapshot
   - Run bench/visdiff.sh
     - If FAIL: log "starting state already broken — investigate" and exit 99
   - Run bench/run.sh > tests/perf/pre.json

3. Research phase
   - Read tests/perf/pre.json + bench/candidate-queue.jsonl
   - Find highest-priority unblocked candidate (lowest attempts count, not blocklisted)
   - If queue empty: log "queue exhausted, plateau" and exit 2
   - Output: candidate JSON + brief implementation strategy

4. Implementation phase
   - Invoke Claude in the worktree (claude -p with research context)
   - Claude makes the change as a single atomic edit
   - Claude runs `cargo check` (Tauri side) and confirms the JS frontend loads
   - If implementation fails (compile error, JS load error): increment candidate.attempts, exit 1

5. Post-snapshot
   - Run bench/visdiff.sh
     - If FAIL: git reset --hard, increment candidate.attempts, log diff, exit 1
   - Run bench/run.sh > tests/perf/post.json

6. Decision
   - Invoke bench/lib/decide.mjs with pre/post
   - Decision rules:
     - WIN: p50 improved ≥ 5% on ≥ 2 of 3 configs AND p99 didn't regress > 5% on any config
     - NO-WIN: improvement < 5% or p99 regressed
     - PLATEAU: third no-win in a row (state preserved across cycles in cycle-log)
   - If WIN:
     - Append commit: `OPT-<finding>: <description> · p50 -<X>ms · p99 -<Y>ms`
     - Mark candidate as resolved in queue
     - Return 0
   - If NO-WIN:
     - git reset --hard (revert the implementation)
     - Increment candidate.attempts
     - If candidate.attempts ≥ 2, mark blocklisted (loop won't retry this session)
     - Return 1
   - If PLATEAU: return 2

7. Cycle log
   - Append a JSON line to tests/perf/cycle-log.jsonl with:
     { ts, candidate_id, decision, pre, post, visdiff_pass, commit_sha (if win) }
```

This protocol is the FFmpeg discipline made concrete. Every decision is deterministic, every revert is automatic, every win is measurable.

---

## 5. The decision rules — `bench/lib/decide.mjs`

The brains of the operation. Pseudo-code:

```js
function decide(pre, post, visdiffPass, history) {
  if (!visdiffPass) return "visdiff-fail";  // hard revert

  const configs = ["low", "default", "heavy"];
  const winsByConfig = configs.map(c => {
    const p50_delta = (pre[c].total.p50 - post[c].total.p50) / pre[c].total.p50;
    const p99_regress = (post[c].total.p99 - pre[c].total.p99) / pre[c].total.p99;
    return {
      config: c,
      p50_improved: p50_delta >= 0.05,
      p99_safe: p99_regress <= 0.05,
      net_win: (p50_delta >= 0.05) && (p99_regress <= 0.05)
    };
  });

  const wins = winsByConfig.filter(w => w.net_win).length;
  const anyP99Regression = winsByConfig.some(w => !w.p99_safe);

  if (wins >= 2 && !anyP99Regression) return "win";

  // Check plateau (3 consecutive non-wins in history)
  const recentNoWins = history.slice(-3).filter(h => h.decision !== "win").length;
  if (recentNoWins >= 3) return "plateau";

  return "no-win";
}
```

Tunable parameters:
- `MIN_P50_IMPROVEMENT` (default 0.05 = 5%) — too low = false positives, too high = real wins missed
- `MAX_P99_REGRESSION` (default 0.05) — strict on smoothness
- `MIN_WIN_CONFIGS` (default 2) — quorum: must improve on at least 2 of 3 configs
- `PLATEAU_WINDOW` (default 3) — consecutive no-wins before plateau

These are tunable from the command line: `MIN_P50_IMPROVEMENT=0.03 bench/loop.sh` for tighter optimization.

---

## 6. The candidate queue — `bench/candidate-queue.jsonl`

Pre-populated with F1–F10 from the audit, plus extension candidates from the research doc:

```jsonl
{"id": "F1", "source": "audit", "title": "glyphSet picker freeze", "expectedGain": "switch latency 1-6s → <50ms", "risk": "low", "attempts": 0}
{"id": "F4", "source": "audit", "title": "Postprocess no-op gate", "expectedGain": "5-15ms/frame", "risk": "low", "attempts": 0}
{"id": "F7", "source": "audit", "title": "Debounce heavy sliders", "expectedGain": "UX, not perf", "risk": "medium", "attempts": 0}
{"id": "F2A", "source": "audit", "title": "drawText skip empty cells", "expectedGain": "138ms → ~95ms", "risk": "low", "attempts": 0}
{"id": "F6", "source": "audit", "title": "Pre-warm shape atlases", "expectedGain": "first-switch 200-500ms → 0", "risk": "low", "attempts": 0}
{"id": "F3+F5+F10", "source": "audit", "title": "Zero-alloc dither pipeline", "expectedGain": "1-3ms/frame", "risk": "medium", "attempts": 0}
{"id": "F8", "source": "audit", "title": "Branchless clamp in EMA loop", "expectedGain": "~1ms/frame", "risk": "low", "attempts": 0}
{"id": "F2C", "source": "audit", "title": "Sprite atlas drawText replacement", "expectedGain": "138ms → ~25ms", "risk": "high", "multi_cycle": true, "attempts": 0}
{"id": "F9", "source": "audit", "title": "Aspect-aware grid auto-fit", "expectedGain": "quality not perf", "risk": "low", "attempts": 0}
{"id": "P1-recording", "source": "plan", "title": "canvas.toBlob async export", "expectedGain": "eliminates GIF export freeze", "risk": "medium", "attempts": 0}
{"id": "EXT-LUT", "source": "research", "title": "256-entry sRGB LUT for linearizeToLuminance", "expectedGain": "1-2ms/frame", "risk": "low", "attempts": 0}
```

The loop reads these in priority order (low risk + high gain first, but multi_cycle items only after their predecessors land — F2A before F2C). Auto-discovered candidates from the perf JSON inspection get appended at runtime.

---

## 7. Integration with Claude /loop skill

Two driver modes:

### 7.1 Mode A — Bash-driven loop (`bench/loop.sh`)

```bash
nohup ./bench/loop.sh > tests/perf/loop.log 2>&1 &
echo $! > /tmp/glyph-grid-loop.pid
```

Pure shell. No Claude in the loop driver itself; each cycle calls Claude (`claude -p`) for the research and implementation phases. Self-contained, runs even if Claude session times out.

### 7.2 Mode B — Claude /loop skill

```
/loop /Users/darkfire/glyph-grid-studio/bench/cycle-prompt.md
```

The /loop skill self-paces; it calls cycle.sh, reads the result, decides whether to continue. Better integration with Claude session management; can pause/resume more naturally. Worse if Claude has session limits.

**Recommendation: Mode A as primary, Mode B as fallback.** Mode A's lockfile-gating + plateau-detection + max-cycles cap is more robust for unattended overnight runs.

---

## 8. Pre-loop sanity checks (must pass before first run)

These are gates that block the first `bench/loop.sh` invocation:

- [ ] `bench/run.sh` produces valid JSON for all 3 configs
- [ ] `bench/visdiff.sh` passes against unmodified main branch (golden frames are correct)
- [ ] `bench/candidate-queue.jsonl` has at least 5 entries
- [ ] `bench/cycle.sh` succeeds against a known-good candidate (F1 — pre-test verifies the cycle protocol works end-to-end)
- [ ] Git working tree clean
- [ ] Disk space > 5 GB (cycle log + perf JSON snapshots accumulate)
- [ ] `cargo check` passes (Tauri backend builds)
- [ ] `node --version` ≥ 18 (for the .mjs scripts)

A pre-flight script (`bench/preflight.sh`) runs all these in one command before the first loop.

---

## 9. What the loop will NOT do (anti-scope)

Per the research doc and the Carmack-Musk discipline:

- **No paradigm-shift rewrites.** No WebGL, no Workers, no WASM, no Rust render path. Those are post-plateau, human-decision items.
- **No feature additions.** Pure optimization. If a candidate "would be nice to have," it's NOT in the queue.
- **No refactor-without-perf-delta.** Cosmetic code cleanup is rejected by the decision rules (no perf win → revert).
- **No spec changes without human approval.** The loop can *propose* "delete the part" but it pauses and asks; doesn't delete autonomously.
- **No cross-project work.** Stays in `/Users/darkfire/glyph-grid-studio/`. Doesn't touch tongue-mac, LoveSparkCards, or anything else.

---

## 10. Risks and mitigations (operational)

| Risk | Mitigation |
|---|---|
| Loop breaks the project at 3am | Atomic commits + visdiff gate + auto-revert. Worst case = single-commit revert. |
| Loop's perf measurements are noisy | 300-frame samples + p50/p99 + 3-config quorum. Single-config flukes can't trigger a win. |
| Loop runs forever | Hard cap at 24 hours, 50 cycles, plateau detection at 3 consecutive no-wins. |
| Visdiff threshold wrong | Tunable env var. First loop session calibrates against actual false-positive rate. |
| Loop wastes battery | `nice -n 10`, 30s idle between cycles, lockfile-gated graceful pause. |
| Implementation phase introduces a security or correctness issue | Loop only optimizes; visdiff catches visual regressions. Functional correctness is partly trusted to the visdiff gate (if output looks the same, behavior is the same to first order). High-risk items (F2C sprite atlas) gated behind multi-cycle protocol with feature-flag intermediate. |
| Joona wakes up to find the project broken | Lockfile + cycle-log makes the state inspectable. Last commit is always a passing-visdiff state. Worst case: `git reset --hard <last-known-good>`. |
| Loop can't make progress (queue exhausted) | Plateau exit. Generates final report. Human decides next paradigm. |

---

## 11. Acceptance criteria (when this plan is "done")

The plan is approved and ready for execution when:

1. ✅ Joona has read `OPTIMIZATION-LOOP-RESEARCH.md` (philosophy + numbers)
2. ✅ Joona has read this plan (system + protocol)
3. ✅ Joona signs off ("implement it all" or equivalent)
4. ✅ Optional Joona-annotations: priority queue order, perf-delta thresholds, visdiff threshold, max cycles, deadline

The loop is "done running" when:

1. **Target hit** — default config p50 < 33ms, OR
2. **Plateau detected** — 3 consecutive no-wins, OR
3. **24 hours elapsed**, OR
4. **50 cycles elapsed**, OR
5. **Joona pauses** via `rm /tmp/glyph-grid-loop.lock`

The loop is "successful" when:

1. **At least 4 of F1–F10 landed** (cumulative perf delta vs current baseline > 30%)
2. **Visual identity preserved** (visdiff has never failed in `cycle-log.jsonl`)
3. **No regressions in production** (golden frames still pass after final loop terminates)
4. **Final report generated** at `tests/perf/loop-final-<ts>.md`

---

## 12. The actual command Joona runs after approving

```bash
cd /Users/darkfire/glyph-grid-studio

# (Once, before first loop session)
./bench/preflight.sh   # builds & verifies harness; ~10 minutes

# Start the loop
nohup ./bench/loop.sh > tests/perf/loop-$(date +%Y%m%d-%H%M%S).log 2>&1 &
echo $! > /tmp/glyph-grid-loop.pid

# To pause gracefully at any point
rm /tmp/glyph-grid-loop.lock

# To force-stop
kill $(cat /tmp/glyph-grid-loop.pid)

# To check progress
tail -f tests/perf/loop-*.log
cat tests/perf/cycle-log.jsonl | jq -s 'group_by(.decision) | map({(.[0].decision): length})'
```

---

## 13. Implementation work split

If approved, the implementation breaks into 12 deliverables (per §3) totaling ~6–10 hours of focused work before the first real loop run. Concretely:

| Hour | Work |
|---|---|
| 0–1 | bench/configs/{low,default,heavy}.json + bench/run.sh + perf-aggregate.mjs |
| 1–2 | First baseline snapshot, verify p50 totals against existing audit (~151ms) |
| 2–3 | bench/visdiff.sh + visdiff.mjs + golden/*.png (render & commit) |
| 3–4 | candidate-queue.jsonl populated, decide.mjs implemented |
| 4–6 | cycle.sh — the protocol script. Most complex of the bunch. |
| 6–7 | loop.sh + bench/README.md + preflight.sh |
| 7–8 | First end-to-end test against F1 (the lowest-risk known-win candidate) — verify cycle protocol works |
| 8–10 | Tune thresholds based on first 3 cycles' actual output |

After hour 10: the loop runs unsupervised. Joona sleeps. The loop ships F1–F10 (and likely auto-discovers extras). Morning report ships F2C-or-equivalent landed and the project is at 30+ fps default.

---

## 14. The ask

This plan is the contract. Sign off and the loop is built. Two checkpoints:

- **Checkpoint 1 (after hour 4):** harness works, golden visdiff passes against current main, candidate queue ready. **Joona reviews before unleashing the loop.**
- **Checkpoint 2 (after hour 8):** F1 has been processed end-to-end via the cycle protocol; loop is calibrated. **Joona starts the 24-hour run.**

Without sign-off, no code is written. With sign-off, the loop runs and ships the existing audit's findings + auto-discovered work, while Joona sleeps.

---

## 15. Closing — Carmack-Musk fingerprint

What this plan does that a generic "let's optimize Glyph Grid" doesn't:

1. **Reuses the existing instrumentation** — Phase 0 already shipped a per-stage timer. The loop reads its output. No reinvention.
2. **Starts from a measured baseline** — F1–F10 with concrete ms numbers and effort-to-gain ranking. The loop consumes the queue, doesn't argue with it.
3. **Has a hard regression gate** — 6 golden visdiff frames. No optimization ships without preserving visual identity.
4. **Has a self-termination condition** — plateau detection. The loop knows when to stop.
5. **Atomic, revertable commits** — every win is one commit with measured delta. Every failure is auto-reverted.
6. **24-hour bound** — not "we'll get to it" but "by tomorrow morning the project is fast or the loop has plateaued." Time pressure is a feature.
7. **Builds the factory, not just the product** — the bench/ harness outlives this loop session and serves every future optimization round.

Carmack would recognize the discipline. Musk would recognize the timeline pressure and the factory-thinking. Joona's CLAUDE.md rules (research-first, plan-before-implement, atomic commits, no premature abstraction, no paid LLM API anywhere) are honored throughout.

Sign off and we ship.
