# GlyphGrid combined release candidate audit

Candidate: 0.1.10. Collection: 2026-09-05 Europe/Helsinki
(source hashes collected 2026-09-04T22:31:52Z).

**Release decision: HOLD. Not published. Do not update the website download.**

## Findings

1. **Performance-session FPS is mislabeled.** `src/index.html`,
   `__pfSessionSummary`, reports `1000 / avg_frame_ms` as `fps`. This is the
   reciprocal of measured render work, not achieved presentation cadence. A
   synthetic session containing two 2 ms renders over 1000 ms reports 500 FPS,
   despite containing only two rendered frames in that interval. Do not use
   this field to claim before/after FPS improvement. Record render-work cost
   separately from actual frame interval/cadence and input-to-present latency.
2. **Stopped performance sessions still accept unbounded configuration events.**
   `__pfSessionEvent` checks session existence but not `ended_at`, and does not
   cap the events array. A synthetic stopped session retained 10,001 events.
   The frame-record cap does not bound the event or long-task arrays. Stop must
   freeze the session; every retained collection needs a bound and an explicit
   overflow counter. These logger issues came from the local logging changes,
   not the public upstream export safeguards.
3. **The complete native release gate has not passed for the combined source.**
   JavaScript tests passed, but universal packaging, native tests, CLI/MCP and
   GUI smoke checks are not established by those results. Previous local-build
   or public-release results cannot be relabeled as evidence for this candidate.
4. **Build storage is critically constrained.** During this audit the laptop
   data volume reported about 2.8 GiB available and 100% capacity. Do not begin
   more large builds or remove rollback/source evidence implicitly. Arrange
   additional space or a build host before repeated native rebuilds.
5. **The built universal app is not signed.** The universal Tauri build finished,
   but `codesign --verify --deep --strict` reports that the code object is not
   signed at all. This fails the documented ad-hoc-signed distribution contract.
   Fix the packaging/signing step and test the resulting exact installer before
   release. Do not assume a successful Tauri build implies a valid signature.

## Integration performed

- Started from public `origin/main` rather than replacing it with older local source.
- Resolved source conflicts in the preview page, studio controls, and test runner.
- Kept bounded imports, generation guards, staged exports, backpressure,
  cancellation, static source-signal caches, and working-resolution preprocessing.
- Combined the public structural debounce with pause-aware rendering through
  one scheduler, keeping capture ownership intact.
- Reduced scratch storage to two shared planes across three sequential passes.
- Retained integer preview time, seek controls, font/atlas handling, sprite-cache
  bounds, and the opt-in session logger, subject to the findings above.
- Set application metadata to 0.1.10. The cask/download remains on the public
  version until a validated installer and checksum exist.

The candidate is intended for a non-production source checkpoint on
`codex/release-0.1.10`, not a public release tag. Check the remote branch to
determine whether the checkpoint push has completed.

## Current evidence

Command: `npm test`, run in the isolated candidate checkout.

| Check | Result | Scope |
| --- | --- | --- |
| Inline syntax and 14 frontend contracts | PASS | Source/contract checks |
| Native-export frontend orchestration | PASS | Mocked Tauri export tests |
| Scheduler, structural coalescing, pause, capture ownership | PASS | Deterministic VM regression |
| Preview seek endpoints and invalid/capture rejection | PASS | Deterministic VM regression |
| 361 preview ticks | PASS | Two exact 180-frame cycles, then frame zero |
| Scratch-plane allocation | PASS | Two arrays; 64,000 bytes at 100 x 80 working pixels |
| Stopped-session isolation | FAIL | Synthetic stopped session accepted 10,001 events |
| Achieved-FPS interpretation | FAIL | Synthetic two-frame/one-second session reports 500 FPS |
| Native release and GUI audit | PENDING | Not established for combined installer |
| Universal native build | PASS | Tauri built application and DMG; `lipo` reports x86_64 and arm64 |
| App signature verification | FAIL | App bundle is not signed |
| Before/after latency, FPS, RSS, energy | UNAVAILABLE | No matched measurement collected |

Code-derived scratch capacity changes from six float32 planes to two:
24 to 8 bytes per working pixel, a 66.7% reduction **in those planes only**.
At 1024 x 1024 that is 24 MiB to 8 MiB. This is not a measured process-RSS
reduction or speedup. Public 0.1.9 already reused its six arrays; allocation
churn elimination measured against the older local baseline must not be
presented as a new public-version improvement.

## Source identity at collection

Built candidate DMG: `Glyph Grid Studio_0.1.10_universal.dmg`.
Size: 16,564,752 bytes. SHA-256:
`510a9fade9bb4718f3638d00a68631664deab5f6581f0313c5d693f3d7aabc98`.
This records a **rejected candidate**, not an approved download. The installer
has not been uploaded, installed, or substituted for the public release.

These are file hashes, not a complete build provenance manifest or Git commit.

```text
98386e330f64009c19d8afd4a8aa078e287779879da2289e6b8c8260786915d3  src/index.html
4c3d1c612b28a5c461f839f238800562598b3e61eed35410c663b5e66447fe9f  src/lib/glyph-fonts.js
867aef6998bdaa89ab7276f3422346e91f1063ecd8373431e7dbd8dd94bf3e24  src/lib/glyph-studio.js
1d273e7fd4e3f987232b06c94d8a149842631d7df252a8d703019606b503557a  src-tauri/src/lib.rs
d29c0b626162ca302c3f89c139b2fb5c0f7880d96caf9800e243d41ba60600d7  src-tauri/Cargo.toml
27eb6453ee6bc6b0b2a1a79aa9b603e39ea7f89af5da1839510b77e073c3f387  src-tauri/Cargo.lock
76c005c1c626d5fa1b608205c68b1db77ef13af93fc246f9510939fdc408a490  src-tauri/tauri.conf.json
8e3e28fcd58a161c8fb376f34aad378346e43192cfcfb9a0c81ea1001e276b37  package.json
```

## Required release gates

1. Correct the audit findings and add tests that reproduce them.
2. Freeze the candidate source and record its exact Git revision and build inputs.
3. Build once; run native/unit, CLI/MCP, and GUI regression checks on that artifact.
4. Collect matched performance workloads with environment and raw evidence.
5. Update the iteration ledger, audit decision, cask version/checksum, and release notes.
6. Publish the exact tested installer with the source tag only after all required gates pass.
7. Confirm the website's stable latest-release link downloads the same checksum.

Release automation has not yet been implemented. Its design must reject source
changes after testing, reused public version tags, incomplete evidence, failed
checks, unresolved index entries, and installer checksum mismatch. It must not
publish this candidate merely because the frontend suite is green.
