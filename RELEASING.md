# Releasing Glyph Grid Studio

The ship workflow for a public, free, **unsigned** direct-download macOS release.
No App Store, no paid Apple Developer signing — distribution is a GitHub Release
plus the landing page at `https://lovespark.love/glyph-grid-studio/`.

> Distribution model: same as indie open-source Mac apps (e.g. DinoRip). The app
> is **ad-hoc signed** (Tauri does this automatically), so first-launch shows the
> milder "developer cannot be verified" dialog — never "app is damaged" — and the
> right-click→Open / `xattr` bypass works. See `## First-run` in the README.

## 0. Pre-flight (every release)

- [ ] Working tree clean on a release branch (`fix/vX.Y.Z-*`), never commit to `main` directly.
- [ ] Versions agree across `package.json`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, `Casks/glyph-grid-studio.rb`.
- [ ] `bash tests/run-all.sh` is green (needs a fresh build first — see step 2).
- [ ] `BUGS_AND_ITERATIONS.md` has an entry for every fix in this release.

## 1. Bump the version

Set the same `X.Y.Z` in all four files above. The headless gate's version-sync
check (`tests/run-all.sh`, Phase A) fails the build on drift.

## 2. Build the universal binary

```bash
cd src-tauri
rustup target add x86_64-apple-darwin aarch64-apple-darwin   # one-time
cargo tauri build --target universal-apple-darwin
# → target/universal-apple-darwin/release/bundle/
#     macos/Glyph Grid Studio.app   (universal: arm64 + x86_64)
#     dmg/Glyph Grid Studio_X.Y.Z_universal.dmg
```

Verify it's truly universal and runs:

```bash
lipo -archs "target/universal-apple-darwin/release/bundle/macos/Glyph Grid Studio.app/Contents/MacOS/glyph-grid-studio"
# → x86_64 arm64
"…/Glyph Grid Studio.app/Contents/MacOS/glyph-grid-studio" catalog | head   # CLI sanity
```

## 3. Gate

```bash
BIN="$PWD/target/universal-apple-darwin/release/bundle/macos/Glyph Grid Studio.app/Contents/MacOS/glyph-grid-studio" \
  bash ../tests/run-all.sh           # must end "FAIL: 0"
```

## 4. Package the download artifact

Use a **stable filename** so `releases/latest/download/…` always resolves for the
website button. Rename the built DMG:

```bash
cp "target/universal-apple-darwin/release/bundle/dmg/"*.dmg ./Glyph-Grid-Studio-macOS.dmg
shasum -a 256 Glyph-Grid-Studio-macOS.dmg     # paste into Casks/glyph-grid-studio.rb
```

> Never ship a plain `zip` of the `.app` — it can corrupt the ad-hoc signature
> and downgrade users to the unrecoverable "app is damaged" error. The DMG (or
> `ditto -c -k --keepParent`) preserves the signature and symlinks.

## 5. Update the cask checksum

Replace `REPLACE_WITH_DMG_SHA256` in `Casks/glyph-grid-studio.rb` with the real
hash from step 4 and confirm `version` matches the release tag.

## 6. Commit, push, merge

```bash
git add -A && git commit -m "release: vX.Y.Z"
git push -u origin fix/vX.Y.Z-...
git checkout main && git merge --no-ff fix/vX.Y.Z-... && git push
```

## 7. Publish the GitHub Release

```bash
gh release create vX.Y.Z "Glyph-Grid-Studio-macOS.dmg" \
  -R Joona-t/glyph-grid-studio \
  --title "vX.Y.Z" \
  --notes "…release notes…"
```

The website "Download for macOS" button points at:
`https://github.com/Joona-t/glyph-grid-studio/releases/latest/download/Glyph-Grid-Studio-macOS.dmg`

## 8. The website

The landing page lives in `docs/` and is served by GitHub Pages at
`https://lovespark.love/glyph-grid-studio/` (the `Joona-t.github.io` apex domain
serves every project repo under `/<repo>/`). Edits to `docs/` go live on push to
`main`. To enable once:

```bash
gh api -X POST repos/Joona-t/glyph-grid-studio/pages -f 'source[branch]=main' -f 'source[path]=/docs'
```

## First public release only

```bash
gh repo edit Joona-t/glyph-grid-studio --visibility public --accept-visibility-change-consequences
gh repo edit Joona-t/glyph-grid-studio --homepage "https://lovespark.love/glyph-grid-studio/"
```

## Deferred (future)

- **Paid notarization** (Apple Developer ID, $99/yr) → removes the first-launch
  bypass entirely. Not required for the unsigned model; revisit if download
  friction proves to hurt adoption.
- **Homebrew tap** so `brew install --cask glyph-grid-studio` works without a
  manual cask path.
