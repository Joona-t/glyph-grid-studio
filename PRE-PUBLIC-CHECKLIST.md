# Pre-Public Checklist

Mechanical to-do list before flipping `Joona-t/glyph-grid-studio` from private to public. Group order = execution order. Check items as they land.

---

## 1. Asset cleanup (privacy + originality)

- [ ] **Replace `src/assets/eron.png`** with public-domain image OR original artwork OR strip the bundled asset entirely (the empty-state placeholder already handles "no source loaded")
- [ ] If `src/assets/sparky.png` exists in the repo, decide: keep as easter egg, replace, or remove (LoveSpark mascot — currently unused by the studio scenes)
- [ ] Grep for the path `/Users/darkfire/` in `src/index.html` and remove from the baked-in `TEST_BASE` constant. The `runStudioPhase` driver is a dev tool — either gate behind a `?dev=1` URL parameter or strip from public build entirely
- [ ] Grep for "eron", "Claire", "Claymore", "claire2.jpg" across the codebase and either remove or change to neutral names

## 2. Code cleanup (look-pro)

- [ ] **Rename internal scene** `eronChip` → `studio` (or `imagePortrait`) in `src/index.html`. Touch `CONFIG.scene`, the `SCENES.eronChip(...)` definition, and the status-bar template string
- [ ] Remove or gate noisy `console.log` statements from `src/lib/glyph-studio.js`. Keep error-path warnings; drop `glyph-studio: drop event tauri://drag-drop paths: ['...']` and similar
- [ ] Remove `runStudioPhase` / `runAllStudioPhases` from public build, OR move them to `src/lib/glyph-batch-driver.js` and gate the `<script>` tag with a build-time flag
- [ ] Verify there are zero `TODO` / `FIXME` / `XXX` comments — either resolve or move to GitHub Issues

## 3. Tauri release config

- [ ] **Disable `devtools` Cargo feature** in release profile. Edit `src-tauri/Cargo.toml`:
  ```toml
  tauri = { version = "2.10.0", features = ["protocol-asset"] }     # production
  # tauri = { ..., features = ["protocol-asset", "devtools"] }      # dev only
  ```
  Keep devtools for local dev via `cargo tauri dev` (which builds with the `dev` profile).
- [ ] Update `src-tauri/Cargo.toml` metadata: `description`, `repository`, `license`, `homepage`, `documentation`
- [ ] Update `src-tauri/tauri.conf.json` `version` from `0.1.0` (matches Cargo) and bump on each release
- [ ] Optional: add `bundle.shortDescription` and `bundle.longDescription` for the macOS About dialog

## 4. Apple Developer signing + notarization

- [ ] **Apple Developer account** ($99/year) — enroll, get team ID
- [ ] **Developer ID Application** certificate in Keychain
- [ ] Update `src-tauri/tauri.conf.json` with signing identity:
  ```json
  "bundle": {
    "macOS": {
      "signingIdentity": "Developer ID Application: Your Name (TEAMID)",
      "providerShortName": "TEAMID",
      "entitlements": "macos-entitlements.plist",
      "hardenedRuntime": true
    }
  }
  ```
- [ ] Create `src-tauri/macos-entitlements.plist` with hardened-runtime entitlements (file-read/write, network if needed — likely no)
- [ ] **Notarization** via `xcrun notarytool` — needs an app-specific password (App Store Connect) stored in keychain via `xcrun notarytool store-credentials`
- [ ] Build script that: signs → creates DMG → notarizes → staples ticket. Tauri 2 has `tauri.bundle.macOS.signing*` config; verify it produces a notarized DMG

## 5. Distribution artifacts

- [ ] DMG with disk-image background showing "drag to Applications" affordance
- [ ] OR signed `.app.zip` in GitHub Releases as a fallback
- [ ] (Optional, post-launch) Homebrew cask formula: `brew install --cask glyph-grid-studio`
- [ ] SHA-256 checksum file for the DMG (paranoid users + Homebrew want this)

## 6. README polish

- [ ] **Hero screenshot or GIF at the top** — the cream-paper × stbn × octant showcase, or a 1:1 crop of one of the showcase combos
- [ ] **Install instructions update** — replace the `cargo tauri build` instructions with "download the DMG from Releases"; keep the build-from-source path below
- [ ] Add a 30-second screen recording (kapture/quicktime) of: drop image → tweak palette → tweak dither → click Export GIF → done. Convert to GIF at low frame rate; embed in README
- [ ] Add badges row: license MIT · build status (GH Actions) · download size · OS support (macOS 11+ — verify minimum)
- [ ] Add a "Made with: p5.js · Tauri 2 · Rust" tech-stack row
- [ ] Verify the algorithmic notes (STBN, OKLCH, k-d tree) are accurate and link to the source papers / repos

## 7. Bug-and-test resolution

- [ ] **Drag-drop from Finder smoke test** (deferred from Phase 0). Confirm `tauri://drag-drop` event fires and image swaps. If broken, see `BUGS_AND_ITERATIONS.md` "Drag-drop diagnostic protocol"
- [ ] **Preset round-trip smoke test** (deferred from Phase 0). Verify Save current → Load roundtrip and Export JSON → Import JSON
- [ ] **Share URL test** — confirm clipboard copy + parse-on-load round-trip
- [ ] **Canvas-scrub freeze** — debounce `cols`/`rows`/`glyphSet` slider handlers in `src/lib/glyph-studio.js` so heavy work fires only after 120 ms of slider stillness
- [ ] **Empty-state regression test** — launch fresh, confirm "Drop an image to start" still renders before any image loads

## 8. Repo metadata (GitHub side)

- [ ] **About** description: copy from `Cargo.toml` `description` field
- [ ] **Topics**: `ascii-art`, `generative-art`, `creative-coding`, `rust`, `tauri`, `p5js`, `macos`, `unicode`, `glyph-art`, `dithering`
- [ ] **Social preview image** (1280×640 PNG) — render one of the showcase GIFs as a still and add the project name. Upload at Settings → Social preview
- [ ] **Release v0.1.0** with notarized DMG attached + release notes (port from this checklist + BUGS_AND_ITERATIONS.md)
- [ ] **Default branch protection** for `main` — at minimum require PRs (you can self-merge, but the trail is cleaner)
- [ ] (Optional) Enable Discussions for community Q&A

## 9. CI (nice to have, not blocker)

- [ ] GitHub Actions workflow: on push to `main`, `cargo check` + `cargo clippy --no-deps`
- [ ] On tag `v*`, build a notarized DMG and upload as a release asset (needs Apple keychain in CI runner — non-trivial; skip until v0.2)

## 10. Ship checklist

- [ ] All boxes above checked
- [ ] One trusted tech-savvy person installs the DMG cold and reports back ("works", "broke at X")
- [ ] `gh repo edit Joona-t/glyph-grid-studio --visibility public --accept-visibility-change-consequences`
- [ ] `gh release create v0.1.0 ...` with the DMG
- [ ] Execute `PUBLIC-LAUNCH-PLAN.md`
