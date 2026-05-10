# Optimization Loop — Research

> **Reconciliation note (2026-05-10).** This research informed `OPTIMIZATION-LOOP-PLAN.md`,
> which Joona approved. Implementation lives at `scripts/loop_orchestrator/` —
> a Python harness that already encoded most of the conclusions reached here
> (24-hour wall clock, hypothesis queue, KEEP/REVERT/ESCALATE state machine,
> SSIM 0.985 visual gate, `claude -p` patch generation per CLAUDE.md rule #10,
> JSONL → automation-log integration). The first 24-hour run is now active.
> See `OPTIMIZATION-LOOP-PLAN.md` for the section-by-section mapping to the
> Python files. This research doc is preserved as the design rationale.

> **Carmack lens:** measure mercilessly, eliminate redundancy, pre-compute what you can, question every allocation.
> **Musk lens:** question the requirement, delete the part, build the factory not just the product, ship at FFmpeg discipline.
> **Iron rule:** no claim of "faster" without a measurement of "before" and "after." A regression that ships is worse than a fix that doesn't.

This document complements `OPTIMIZATION-AUDIT-CARMACK.md` (the *what*) and `OPTIMIZATION-PLAN.md` (the *how* of individual fixes). What this adds: **the system thinking — the cycle that grinds through optimizations 24 hours a day with no human in the loop, until the FFmpeg quality bar is hit or we plateau.**

---

## 1. Where we are (data, not vibes)

From the existing audit + current `tests/PERF-BASELINE.md`:

- **Phase 0/0.5 shipped** (commit `9ba6db5`): per-stage instrumentation + bad-defaults fix. **2.5× speedup confirmed** (382ms → 151ms/frame on default config).
- **Top hotspot:** `drawBrightnessGrid` fillText loop. **138 ms/frame at 240×120 grid.** Dominates everything else.
- **User-visible UX freeze:** glyphSet picker, **1–6 seconds.** 5-minute fix documented (F1).
- **Postprocess passes:** ~5–15ms/frame even when stages are no-ops (F4).
- **Per-frame allocations:** ~30–60 KB GC pressure (F3, F5, F10).
- **Slider drag:** up to 60 reallocs/sec without debounce (F7).
- **Atlas async load:** 200–500ms freeze on first shape-edge-aware switch (F6).

10 findings ranked F1–F10 by leverage. **None are speculative — every line has a file:line reference and an estimated ms gain.**

What's missing: **the discipline to ship them in order, with measured deltas, without breaking things, autonomously.**

---

## 2. The FFmpeg quality bar — concrete numbers

FFmpeg ships at this discipline because of five things, each measurable:

| FFmpeg principle | Glyph Grid translation | Current | Target | Floor |
|---|---|---|---|---|
| Tight inner loops | Per-frame total at default config | 151 ms | 33 ms (30 fps) | ~12 ms (theoretical, sprite atlas) |
| No allocations on the hot path | Dither + EMA per-frame allocs | ~30–60 KB | 0 (reuse buffers) | 0 |
| SIMD where applicable | linearizeToLuminance loop | scalar JS | typed-array unrolled | WASM SIMD if needed |
| Threading where parallelizable | postprocess chain | main thread | OffscreenCanvas + Worker | Same |
| Profile-guided optimization | Per-stage timing | ✓ shipped | continuous loop | continuous loop |

**Stretch (Musk-aggressive):** sustained **60 fps live tuning** at default settings (cell grid 240×120, postprocess on). Frame budget: **16.6 ms.**

**Realistic floor analysis:**
- Scene render (p5.js user fn): ~2–8 ms (varies by scene complexity, can't deeply optimize without rewriting user code)
- Luminance + downsample: ~3–5 ms (vectorizable to ~1 ms with typed-array tightening)
- EMA + dither: ~2–4 ms (zero-alloc reduction → 1 ms)
- Selection + drawText: **138 ms today, ~12–25 ms with sprite atlas**
- Postprocess (off): ~5–15 ms (gateable to 0 when stages no-op)
- Postprocess (on): ~10–30 ms (worker offload → not blocking main thread)

**Theoretical 60-fps floor at default config:** ~12 + 5 + 3 + 0 = **~20 ms with postprocess off**. Tight but feasible.

**Pragmatic 30-fps target:** **<33 ms total**. Gap from current 151ms = **4.6× speedup needed**. That's exactly the F2C sprite-atlas + F3-F5-F10 zero-alloc + F4 gate combo.

---

## 3. Why a 24-hour loop, not a sprint

Three reasons this work needs the loop, not a single push:

1. **Each fix needs ~20–60 minutes of measure-implement-test-measure-commit.** F2C alone is "1 day" per the audit. F1 is 5 minutes. The cadence varies; a fixed-length sprint either over-commits (bunched against F2C deadline) or under-utilizes (idle after F1).
2. **Regressions are real.** Sprite-atlas could break the dispersal effect. Worker postprocess could deadlock with a particular config. Each change needs **automated visual + perf regression gate** before commit. Without the gate, "ship 8 fixes in a row" turns into "8 fixes but 3 broke things and we don't know which one."
3. **The plateau is the success signal.** When the loop runs and reports "no candidate would improve perf > 5% without breaking visual identity," we're done. We've hit the floor. That's the right termination, not "ran out of time."

The 24-hour duration isn't sacred. It's a *bound*: **at most 24 hours of autonomous operation per loop session, then a human review**. If the loop hits the floor in 8 hours, ship and stop.

---

## 4. What the existing audit doesn't yet have

| Need | Status | What's missing |
|---|---|---|
| Per-stage perf instrumentation | ✓ shipped | nothing |
| Top-10 priority queue | ✓ shipped (F1–F10) | needs continuous re-ranking as fixes land |
| Visual regression baseline | ✗ not built | golden frames + image diff harness |
| Perf regression gate | ✗ not built | runs N frames, compares p50/p99 vs baseline JSON |
| Atomic commit discipline | partial (BUGS_AND_ITERATIONS.md exists) | per-cycle protocol enforced by tooling |
| Loop driver | ✗ not built | calls cycle, parses result, decides continue/stop |
| Termination conditions | informal | encoded as runnable checks |

That's the gap. The optimization machinery exists at the *fix-by-fix* level. It doesn't exist at the *machine-that-fixes* level.

---

## 5. The Carmack-Musk synthesis applied — what each cycle must enforce

Each iteration of the loop is built around five non-negotiable rules:

### 5.1 Measure before, measure after, commit only on win
- Every cycle starts with `bench/run.sh` capturing p50/p99 perf at 3 representative configs (low / default / heavy).
- Every cycle ends with `bench/run.sh` again on the same configs.
- Commit only if **both p50 improves AND p99 doesn't regress > 5%**. (p99 matters more than p50 for "smoothness" — Musk's frame-variance corollary.)

### 5.2 One change per commit
- Atomic. Revertable. The commit message includes the perf delta (`F1: glyphSet freeze fix · p50 -340ms switch latency · p99 -2.1s`).
- No bundling. If two changes land in one commit and the perf gets worse, we can't revert just one.

### 5.3 Visual identity never breaks
- 6 golden frames at known configs (octant cream-paper, ASCII flow field, sextant dispersal, dense braille, sparse fence-post, postprocess-stack). Stored in `tests/golden/`.
- `bench/visdiff.sh` compares current output to golden, fails if pixel diff > 0.5%.
- The gate fires on EVERY commit. A perf win that breaks visual identity is rejected.

### 5.4 Question every requirement
- Each cycle, the loop's research phase asks: "Is the *spec* of this stage right? Could we delete the part?"
- F2 might not need 240×120 cells if 200×100 looks identical and runs at 60fps. The loop is allowed to propose spec changes, but they require human approval (loop pauses with the question).

### 5.5 Plateau detection terminates the loop
- If 3 consecutive cycles produce < 5% p50 improvement, the loop pauses with a "we hit the floor" report.
- Either we're truly at the floor (good — ship) or we need a paradigm shift (WebGL, WASM SIMD) which is a human decision, not a loop decision.

---

## 6. Updated optimization priority queue (post-audit, with cycle estimates)

| Order | Finding | Fix | Cycle est | Expected p50 delta | Risk |
|---|---|---|---|---|---|
| 1 | F1 — glyphSet freeze | remove `clearCache()` | 30 min | switch latency 1–6s → <50ms | Low |
| 2 | F4 — postprocess gate | per-stage no-op check | 45 min | 5–15 ms/frame | Low |
| 3 | F7 — debounce heavy sliders | 120ms wrapper in addInput | 90 min | UX-only, no perf number | Medium (changes interaction) |
| 4 | F2A — drawText skip empty | filter cells where char === ' ' | 45 min | ~30% on dark configs | Low |
| 5 | F6 — pre-warm shape atlases | parallel fetch in setup() | 60 min | first-switch 200–500ms → 0 | Low |
| 6 | F3 + F5 + F10 — zero-alloc | persistent buffers via state object | 3 hours | 1–3 ms/frame | Medium (touches dither API) |
| 7 | F8 — branchless clamp | inline ternary | 20 min | ~1ms/frame | Low |
| 8 | **F2C — sprite atlas** | **prerender glyphs to offscreen canvas, drawImage per cell** | **6–8 hours** | **138 → ~25 ms (5×)** | **High (correctness-critical)** |
| 9 | F9 — aspect-aware grid | auto-adjust cols/rows on canvas resize | 60 min | quality, not perf | Low |

After all 9 land: **expected default-config frame time 151 → ~28 ms**. **5.4× total speedup**. Hits the 30-fps target with margin.

After F2C specifically: the project crosses from "feels alpha/beta" to "ships at FFmpeg quality."

---

## 7. New optimization candidates the loop should auto-discover

The audit captured what was visible at the time. The loop's research phase, at the start of each cycle, runs `bench/run.sh` and inspects the new perf JSON for opportunities the audit didn't catch. Examples the loop might find:

- **Recording-mode toBlob latency** — currently `canvas.toDataURL` synchronous, ~100–300ms per frame during export. Phase 1 of the existing plan, not in F1–F10. Should be on the queue.
- **`Math.pow` in linearizeToLuminance** — 256-entry LUT could replace it. ~1–2 ms gain. Auto-discoverable from sub-stage breakdown.
- **`document.fonts.load` cold-cache flake** — the F1 fix removes the proximate cause but the root cause (load awaits 3 promises) lingers and can resurface. The loop should add a regression test.
- **OffscreenCanvas migration** — once F2C lands, postprocess can move to a worker for true parallelism. New cycle target.
- **WebGL fallback** — when the loop plateaus, this becomes the next paradigm shift candidate.

The loop's research phase reads this list AND inspects the new data each cycle. No hardcoded priority — it's a queue with re-ranking.

---

## 8. Bench harness design (what we need to build)

Three scripts, all under `bench/`:

### 8.1 `bench/run.sh` — perf baseline

Captures p50 / p99 / max latency per stage across N=300 frames at three representative configs:

- **Low** (60×30 cells, postprocess off, ASCII glyph set) — sanity check the floor
- **Default** (240×120 cells, vignette only, octant glyph set) — the typical user
- **Heavy** (400×300 cells, full postprocess stack, dense braille) — the worst-case

Writes `tests/perf/<timestamp>.json`:

```json
{
  "config": "default",
  "frames": 300,
  "stages": {
    "scene":      { "p50": 4.2, "p99": 6.1, "max": 7.8 },
    "lum":        { "p50": 3.1, "p99": 4.4, "max": 5.2 },
    "downsample": { "p50": 1.8, "p99": 2.5, "max": 3.0 },
    "ema":        { "p50": 0.9, "p99": 1.2, "max": 1.5 },
    "dither":     { "p50": 1.1, "p99": 1.6, "max": 2.0 },
    "select":     { "p50": 0.8, "p99": 1.1, "max": 1.4 },
    "draw":       { "p50": 138, "p99": 152, "max": 168 },
    "postproc":   { "p50": 8.4, "p99": 12.1, "max": 14.5 }
  },
  "total":        { "p50": 158, "p99": 181, "max": 203 }
}
```

Mechanism: invokes the existing Tauri `render` CLI subcommand with a perf-emit flag, parses stdout JSON, aggregates.

### 8.2 `bench/visdiff.sh` — visual regression gate

Renders the 6 golden configs, compares pixel-by-pixel against `tests/golden/<name>.png`, fails if mean absolute diff > 0.5% per channel.

Uses Tauri's existing `render` CLI to emit PNG, ImageMagick `compare -metric MAE` for diff.

Returns 0 (pass) or non-zero (fail with diff visualization in `tests/visdiff-out/`).

### 8.3 `bench/cycle.sh` — one full optimization cycle

Sequence:

1. **Pre-snapshot** — `bench/run.sh > tests/perf/pre.json` and `bench/visdiff.sh` (must pass before starting; bail if visual identity already broken)
2. **Research** — invoke a Claude session with the perf JSON + audit + this research doc, ask: "what's the next optimization candidate, ordered by expected p50 gain / risk ratio?"
3. **Implement** — invoke a Claude session in the same git worktree with the candidate, write code
4. **Test** — `swift build` equivalent (it's a Tauri Rust project; `cargo build` for the backend, no build step for the JS frontend) + manual smoke
5. **Post-snapshot** — `bench/run.sh > tests/perf/post.json` and `bench/visdiff.sh`
6. **Decide** — if p50 improved AND p99 didn't regress > 5% AND visdiff passed → commit with delta in message; else → `git reset --hard` and log the failed attempt
7. **Update queue** — append this cycle's outcome to `tests/perf/cycle-log.jsonl`

Returns 0 (committed a win), 1 (no win, queue unchanged), 2 (plateau detected — 3 consecutive non-wins).

### 8.4 `bench/loop.sh` — 24-hour driver

```bash
#!/bin/bash
# Runs cycle.sh until either:
#   - 24 hours elapsed
#   - Plateau detected (cycle.sh returns 2)
#   - Max-cycles cap (default 50) hit
#   - Lockfile removed by user (graceful pause)
#
# All output to tests/perf/loop-<timestamp>.log

START=$(date +%s)
DEADLINE=$((START + 24*60*60))
CYCLES=0
MAX_CYCLES=${MAX_CYCLES:-50}
touch /tmp/glyph-grid-loop.lock

while [ -f /tmp/glyph-grid-loop.lock ] \
   && [ $(date +%s) -lt $DEADLINE ] \
   && [ $CYCLES -lt $MAX_CYCLES ]; do
  CYCLES=$((CYCLES + 1))
  echo "=== Cycle $CYCLES at $(date) ==="
  bench/cycle.sh
  case $? in
    0) echo "  ✓ committed a win" ;;
    1) echo "  · no win this cycle" ;;
    2) echo "  ⊥ plateau detected — stopping" ; break ;;
    *) echo "  ⚠️  unexpected exit code; stopping for safety" ; break ;;
  esac
done

rm -f /tmp/glyph-grid-loop.lock
echo "Loop ended after $CYCLES cycles ($(($(date +%s) - START))s)"
```

Driven by `nohup ./bench/loop.sh &` or the Claude `/loop` skill calling `bench/cycle.sh` on its own self-paced cadence.

---

## 9. Risks and mitigations

### 9.1 Loop breaks the project at 3am
- Atomic commits + visdiff gate + p99 gate. Worst case: a single-commit revert.
- Lockfile mechanism: `rm /tmp/glyph-grid-loop.lock` from any terminal pauses the loop gracefully after the current cycle.

### 9.2 Loop's "research" picks bad candidates
- Pre-vetted priority queue (F1–F10) consumed first. Candidates discovered by perf JSON inspection are added but tagged "auto-discovered" and reviewed by the visdiff gate same as queue items.
- Candidates that fail twice are blocklisted for the rest of the loop session.

### 9.3 Visual diff threshold too tight or too loose
- Initial threshold 0.5% MAE per channel. If too tight (false positives on legitimate changes), bump to 1.0%. If too loose (real regressions slip through), drop to 0.2%.
- The threshold itself is a tunable; the loop adjusts based on false-positive rate.

### 9.4 Perf measurement noise
- 300-frame samples with p50/p99 (not mean) suppresses noise.
- Three representative configs prevent over-tuning to a single workload.
- A single 5% improvement on one config requires 5% on at least two of three configs to count as a win.

### 9.5 Sprite atlas (F2C) is correctness-critical and risky
- Treated as a multi-cycle change. First cycle: implement behind a feature flag (`CONFIG.studio.useSpriteAtlas = false` default). Second cycle: enable in default config after visdiff confirms identical output. Third cycle: remove the flag.
- The loop is allowed to take multiple cycles per finding; not every cycle is one finding.

### 9.6 Battery drain / heat from 24-hour loop
- Cycles include a 30-second idle between iterations to let the laptop cool.
- Loop runs in low-priority mode (`nice -n 10`) so it doesn't fight foreground apps.
- Overnight is fine; daytime can be paused via lockfile.

---

## 10. Termination conditions (the success signal)

The loop terminates when ANY of:

1. **Target hit** — default config frame time p50 < 33 ms (30 fps). Optionally stretch: < 17 ms (60 fps).
2. **Plateau** — 3 consecutive cycles with no win > 5%. The floor has been hit.
3. **Time** — 24 hours elapsed.
4. **Cycles cap** — 50 cycles default.
5. **User pause** — `rm /tmp/glyph-grid-loop.lock`.
6. **Hard fail** — visdiff gate consistently fails (3 in a row); something is structurally wrong, human review needed.

Each termination produces a final report (`tests/perf/loop-final-<timestamp>.md`) with:

- Cycles executed, wins committed, total p50/p99 improvement
- Findings landed (mapped to F1–F10)
- New candidates discovered
- Outstanding work (queue items not yet attempted)
- Recommended next loop session (parameters tuned based on this run)

---

## 11. The Carmack-Musk fingerprint on this design

What's Carmack:
- Per-stage instrumentation reused, not reinvented
- Atomic commits with measured delta
- Visual regression gate as hard non-negotiable
- p99 weighted equally with p50 (frame variance kills "smoothness" more than mean does)
- "Profile, fix, profile" discipline embedded in the cycle script

What's Musk:
- 24-hour loop as the timeline pressure (not "we'll get to it this week")
- "Question the requirement" allowed as a research-phase output (the loop can ask "do we need this stage at all?")
- Vertical integration — sprite atlas isn't a half-step, it's a complete rebuild of the drawText path
- Aggressive plateau detection — when the floor is hit, stop, don't waste cycles on diminishing returns
- The factory matters more than the product — this doc designs the optimization *factory*, individual fixes are output

What's neither and shouldn't be:
- Marketing copy / "feels production-ready" subjective criteria. Replaced with measurable perf JSON.
- Unbounded scope. The plateau condition makes the loop self-terminating.
- "Move fast and break things." We move fast AND don't break things — the visdiff gate enforces this.

---

## 12. Companion: what NOT to optimize before the loop runs

Per Carmack: profile *before* optimizing. The loop is the profiler-driven optimizer. Before it runs, do not:

- Migrate to WebGL / WebGPU (premature; F2C sprite atlas first)
- Move to OffscreenCanvas + Workers (premature until F2C lands)
- Rewrite scene rendering in Rust (massive scope; defer until JS-side hits the floor)
- Add Service Worker caching (network is not the bottleneck)
- Touch p5.js → bare canvas2D (huge breaking change for low payoff)
- Refactor for readability without a perf delta (no win, only risk)

This list is the *anti-roadmap*. Each item is in `OPTIMIZATION-PLAN.md` "Out of scope" and stays out of scope until the loop says we've plateaued at the JS-renderer floor.

---

## 13. Output

The loop produces three artifact streams:

1. **Code commits** — atomic, one-fix-per-commit, with measured perf delta in commit message.
2. **`tests/perf/cycle-log.jsonl`** — append-only log of every cycle's pre/post perf, decision, and outcome.
3. **`tests/perf/loop-final-<timestamp>.md`** — summary report when the loop terminates.

The first artifact is the actual product (faster Glyph Grid). The other two are the audit trail — proof that the speedup is real, that no regressions slipped through, and that the floor (or wall) was reached deliberately.

---

This research file is the source-of-truth for designing the optimization loop. The companion `OPTIMIZATION-LOOP-PLAN.md` translates this into concrete tasks: scripts to write, files to add, commands to run, with acceptance criteria per artifact.

Once the plan is approved, the loop starts. From that point forward the code commits, not this doc, are the source of truth for "what's optimized so far."
