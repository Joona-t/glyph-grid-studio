# Glyph Grid Studio roadmap and device handoff

Updated 2026-09-05. Public release: 0.1.9. Working candidate: 0.1.10.

**Current state: combined source checkpoint, NOT release-ready.**

## What is saved here

- Public 0.1.9 source and its bounded import/staged export safeguards.
- Combined preview scheduler, integer loop/seek controls, font/atlas fixes,
  sprite-cache bounds, reduced preprocessing scratch storage, and session logging.
- Frontend export/contract checks and the new combined preview regressions.
- Evidence and unresolved findings in `COMBINED_RELEASE_AUDIT.md`.

## Resume on another Mac

1. Clone `Joona-t/glyph-grid-studio` and check out `codex/release-0.1.10`.
   Record `git rev-parse HEAD` as the source checkpoint being resumed. Do not
   substitute the default branch and assume it contains this candidate.
2. Read this roadmap, `COMBINED_RELEASE_AUDIT.md`, `BUGS_AND_ITERATIONS.md`, and
   `RELEASING.md` before editing. Existing public 0.1.9 must remain available.
3. Install the supported Node/npm and Rust/Tauri toolchains plus Xcode command
   line tools and the native prerequisites documented by the project. Add both
   `aarch64-apple-darwin` and `x86_64-apple-darwin` Rust targets for universal builds.
4. Use a fresh local build cache with ample free disk space. Do not put build
   caches, credentials, private source images, or app-user databases into Git.
5. Run `npm test` from the repository root. Its previous pass is historical
   evidence, not proof of the new machine's state.
6. Correct the blocking audit findings and build/test a new frozen candidate.

This source checkout does not transfer installed applications, toolchains,
untracked personal assets, credentials, or conversation history. Documentation
is the portable context; required private runtime material needs a separate,
explicitly approved transfer.

## Next work, in order

- [ ] Stop must freeze performance-session logs; bound events and long tasks.
- [ ] Separate render-work throughput from achieved FPS and presentation latency.
- [ ] Add regression tests for those reproduced logging defects.
- [ ] Repair ad-hoc signing and verify the final packaged application.
- [ ] Run Rust tests and the CLI/MCP release gate against the exact candidate.
- [ ] Manually exercise image import, rapid setting changes, pause/seek, export,
      cancellation, and reopening the application on the candidate build.
- [ ] Collect matched latency/frame-time and memory workloads with raw evidence.
- [ ] Record exact source revision, environment, installer checksum, results,
      and limitations together. No unsupported speedup claims.
- [ ] Align cask version/checksum with the accepted installer.
- [ ] Implement release automation that rejects drift, failed/missing gates,
      reused version tags, unresolved merges, and mismatched installer hashes.
- [ ] Publish only the tested installer and matching source tag after approval
      conditions are satisfied; verify the website downloads that checksum.

## Later optimization opportunities

- Profile remaining main-thread rendering before choosing GPU/off-thread work.
- Review export memory beyond the already-bounded compressed capture stage.
- Measure long-session cache and graphics-resource behavior on repeated imports.
- Keep a versioned optimization ledger: problem, cause, change, rationale,
  evidence, measured benefit, limitations, and regression coverage per iteration.

The current laptop reported about 2.8 GiB free during preparation. Moving this
checkpoint to a roomier Mac Mini addresses storage only, not the unresolved
correctness, signing, or measurement findings.
