# Workflow self-reflection — Glyph Grid Studio session
## How AI + computer-use + Tauri shipped a desktop app

**Audience:** future me, future AI sessions, future model designers.

This doc captures lessons-learned from a multi-session collaboration that took /glyph-grid from a Claude skill into a public-ready macOS app with CLI + MCP server. Written candidly. Anti-flattery, pro-evidence.

---

## 1. The shape of the work

What we actually shipped (~3 sessions, ~12 hours of focused work):

- **GUI app** (Tauri 2 + p5.js, 10 MB binary)
- **CLI subcommand** (`glyph-grid-studio render --in foo --out bar`)
- **MCP server** (stdio JSON-RPC, two tools: render + catalog)
- **Kawaii postprocess feature** (this session)
- **Test infrastructure** (17 automated tests + RESULTS doc)
- **Strategy stack** (3 docs: pre-public checklist, launch plan, agent-integration roadmap)
- **~1000 GIFs** rendered across Claire, Thor, and Sparky test runs

The work that compounded was not the GIFs — it was the **batch driver pattern**. Once `runStudioPhasesAt(phases, baseDir)` existed, every new test image was 10 minutes of setup + an hour of unattended compute.

## 2. What worked dramatically well

### a) Three-doc strategy stack
`PRE-PUBLIC-CHECKLIST.md` / `PUBLIC-LAUNCH-PLAN.md` / `AGENT-INTEGRATION-PLAN.md`. Splitting by *moment-of-re-read* instead of by topic kept each doc short enough to actually use. Pattern transfers to every project.

### b) The unattended batch driver
The single highest-leverage move was extracting the test loop from "click 469 buttons" into "type one console call." Reduced 6 hours of computer-use clicks → 90 minutes of unattended Tauri rendering. The pattern (Rust command writes-to-path, JS driver iterates configs, dev-console kicks off) generalizes — any GUI app with a config + an export can become unattended-batchable.

### c) Build-test-verify protocol
The user's "build-test-verify everything yourself" rule. Without it, the BUG-002 (Tauri's `app.exit(code)` discarding the code) would have shipped to v0.1 and broken every CI / agent-driven flow downstream. The cost of one extra `cargo tauri build` cycle is genuinely cheap insurance.

### d) Computer-use for pure verification (not for control)
Driving the GUI via screenshots + clicks worked best when used for *checking* (does the empty-state placeholder render? does the slider scrub? does the dialog appear?), not for *driving* (clicking 469 export buttons). Use computer-use for the manual-test phase, not for production automation.

### e) Native dialogs replacing localhost fallback
Replacing `URL.createObjectURL → anchor click` with native Tauri save dialogs felt small, was huge for UX. People file-pick into structured paths in real software; only hobby tools dump into ~/Downloads.

## 3. What failed or wasted time

### a) Inline images aren't accessible
When the user uploaded the bunny-slipper Sparky inline, I couldn't read its bytes. Inline images in Claude Code conversations are visible to me as a vision input but aren't materialized to disk under any path I can `Read`. Three turns wasted negotiating around this. **Future model designers: please give the assistant programmatic access to the inline-attached image bytes.** This is a strict capability gap, not a workflow issue.

### b) Computer-use focus glitches
Cmd+V / Cmd+Opt+I sequences sometimes silently fired into the wrong app because focus jumped between Claude and the controlled app between actions. Workarounds: explicit `open_application` before EVERY keystroke sequence, plus a screenshot-verify before sending Cmd-key combos. Lost ~30 minutes across the whole project to this. **Future improvement:** computer-use should expose a "verify frontmost is X, otherwise raise" preflight that I can put before keyboard shortcut chains.

### c) `app.exit(code)` silently discards the code (Tauri 2.10 bug)
Already logged as BUG-002. Cost ~20 minutes of debugging. Future agents working with Tauri 2: check this before assuming `app.exit` propagates, or skip it and use `std::process::exit` directly.

### d) `tauri::generate_context!()` is single-use per crate
Caused a build error when I tried to define `run_gui()` and `run_headless_render()` as separate functions both calling `tauri::Builder::default().run(...)`. The macro embeds a binary symbol that can't be embedded twice. Refactored to a shared `run_tauri()`. **General principle:** any Rust macro with the word "embed" or "generate" in it is single-use; plan accordingly.

### e) `cargo build --release` doesn't bundle the frontend
Several cycles were wasted building with `cargo build` (which builds the binary) instead of `cargo tauri build` (which also bundles the WebView assets). The binary would launch but couldn't find its own HTML. **Mental model fix:** for any Tauri project, default to `cargo tauri build` always; `cargo build` is essentially useless on its own.

### f) WebKit pauses requestAnimationFrame in hidden windows
The headless render via `window.hide()` worked except… it didn't, because hidden windows have rAF paused as a battery-save optimization. Off-screen positioning (-32000, -32000) is the workaround. Same issue applies to iframe / background tab / minimized window — assume the platform optimizes for "not visible."

## 4. Computer-use lessons-learned

Concrete takeaways from ~50 computer-use interactions:

| Pattern | Lesson |
|---|---|
| `open_application` then immediate `key` | Often fails — focus hop. Take a screenshot to verify frontmost first. |
| `cmd+v` paste | Reliable when console is focused; flaky when triggered before app focus settles. Build a 1-second wait between focus actions and keyboard. |
| Long `type` calls (>500 chars) | Frequently truncated or eaten. Use clipboard + `cmd+v` for anything > ~200 chars. |
| `computer_batch` for keyboard chains | Better than individual `key` calls — atomicity + faster. But verify focus *before* the batch. |
| Screenshots between every action | Slow but accurate. Doing it once per ~3 actions is the right cost/value. |
| Native save dialogs | Cmd+Shift+G + type path + Return is fast and reliable. Better than navigating sidebars. |
| Dropdown selection in Tweakpane | Click → wait → click target. Don't try keyboard navigation; the panel doesn't always cooperate. |

**The single most useful workflow improvement for computer-use:**

A "wait for X" tool — `wait_for_state(condition: 'window-frontmost', target: 'Glyph Grid Studio', timeout: 5000)` — that synchronously polls until the OS state matches, then returns. Right now this is emulated with `wait` + screenshot + retry, which is brittle.

## 5. Suggestions for future AI models

What would have made this project faster:

1. **Inline-attached file access.** As above — bytes from the conversation should be reachable via a known path or a `read_attachment(index)` API.
2. **Streaming subprocess output.** Right now `Bash` returns when the command exits. For a 1.5-hour build process I want a "tail this background log" stream that emits each line as a notification. (Monitor sort of does this; it's coarse for our needs.)
3. **Explicit "focus + key" tool.** Computer-use's `key` does not preflight focus. A `focus_and_key(app, chord)` would eliminate a class of bug.
4. **Direct webview JS execution.** Driving JS via Cmd+Opt+I + clipboard paste is fragile. A first-class `webview_invoke(app, js_function_name, args)` for granted apps would reduce the kawaii-batch trigger from 8 actions to 1, with no shell-escaping pitfalls.
5. **Multimodal output of GIFs.** I can emit images in responses, but animated GIFs in chat are flat. If the model could "show" the user a rendered GIF inline, validation cycles would close in seconds instead of minutes of back-and-forth.
6. **Build-output filtering.** `cargo build` emits 200+ lines of progress; I always `tail -3` to see if it succeeded. A structured "did it build?" tool that returns `{ status: 'ok'|'failed', errors: [...] }` from a build is a 50-line wrapper I keep reinventing.
7. **Asynchronous task "checkpoints."** When kicking off a 1.5-hour batch, I want to emit a "checkpoint" instruction the platform respects — `please poll this monitor and wake me when it hits a milestone, otherwise let me work in parallel.` Today's ScheduleWakeup + Monitor combo is workable but high-friction.

## 6. Lessons specific to /glyph-grid

For the next time someone (me or another AI) extends this project:

- **Build the unattended batch driver first.** Don't run a single test by hand. The cost of writing the driver pays for itself within ~10 GIFs.
- **Parallelize design + render.** Render runs on the GPU/CPU; design runs in my head. Always have a 1-hour-batch + 30-minute-design pair queued together so I don't waste compute time idle.
- **Snake_case Rust struct → camelCase JS config.** The translation layer (`HeadlessRenderJob::build_js_config`) is small but critical. Keep it in Rust so JS gets a clean blob to apply.
- **Shape-vector NN, not CNN.** Per Chen et al. arXiv 2503.14375, classical k-NN matches CNN quality for ASCII rendering at ~1% the cost. Stay with the current architecture; don't add ML inference.
- **Postprocess > glyph-set for visual effects.** Adding the kawaii overlay as a postprocess (no font dependency) was 100x faster than designing a kawaii glyph atlas. Pick this pattern for any future visual-effect features.

## 7. The single most important meta-lesson

**Compounding tooling > individual outputs.** Spending 2 hours on the batch driver beat spending 6 hours clicking buttons. Spending 1 hour on the test suite beat spending 10 hours debugging shipped regressions. Spending 30 minutes on three strategy docs beat one giant doc nobody re-reads.

The user's CLAUDE.md captures this as the Karpathy principle ("look at the data, build the instrument"). The pattern recurs: **whenever something feels manual and repetitive, write a tool for it. The next time you do it manually is the second time you've already missed the chance to automate.**
