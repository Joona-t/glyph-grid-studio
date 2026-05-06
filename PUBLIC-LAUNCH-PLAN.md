# Public Launch Plan — Glyph Grid Studio

Strategy doc for the v0.1.0 public release. Covers timing, channels, copy, assets, distribution, and 30-day follow-up. Pair with `PRE-PUBLIC-CHECKLIST.md` (mechanical work) — this file is the *what to say and where*.

---

## North star

A creative-coding desktop app that's good enough to be linked once on Hacker News and survive the visit. Free, MIT, no paywall, no telemetry.

## Audience

| Audience | Where they live | What they care about |
|---|---|---|
| Creative coders | p5.js Discord, openprocessing, Reddit r/generative | "Does it crash? Can I export? What dithering?" |
| Generative-art / aesthetic Twitter | X/Twitter, IG | "Make it look good in 6 seconds" |
| Hacker News / dev Twitter | HN, X | "What's the stack? Is the source readable?" |
| Demoscene / retro folks | Pouet, Twitter | "CRT-beam shader? Octants? Scanlines?" |
| Anime / Claymore community | Reddit r/Claymore, r/anime | "You used Clare? Show me." |

## Pre-launch assets

Build these BEFORE the launch tweet/post, not after:

### Hero assets

- [ ] **Hero GIF** (1024×504 or 1:1 crop) — the cream-paper × stbn × octant showcase. Loops cleanly, under 8 MB
- [ ] **30-second screen recording** of: launch app → drag image → palette flip → dither flip → Export GIF → done. Saved as MP4 (Twitter / IG video) AND as GIF (Reddit / HN inline embed)
- [ ] **5 still images** — different palettes / styles / glyph sets — for the IG carousel and README gallery
- [ ] **Social preview image** (1280×640 PNG) for GitHub repo + OG embed (Twitter card preview)
- [ ] **App icon variants** — already shipped in `src-tauri/icons/`; verify they look good at favicon size

### Pre-written copy

Put each post-copy in this doc as a section so launch day is a copy-paste from here, not a writing session.

#### Hacker News (Show HN)

> **Show HN: Glyph Grid Studio – ASCII art generator with live OKLab palette tuning**
>
> Hi HN. I built a desktop app that converts images into a grid of typeset Unicode glyphs (ASCII, block-elements, octants, sextants, braille). Each cell of an M×N grid maps to a glyph via either a luminance ramp or a 6-D shape-vector nearest-neighbor lookup against a pre-baked atlas.
>
> Stack: p5.js front end, Tauri 2.x shell, Rust backend that handles native file I/O and direct GIF89a muxing via the `gif` crate's NeuQuant quantizer. ~10 MB native macOS binary, zero runtime dependencies.
>
> Notable algorithmic bits:
> - 6-D shape-vector matching (Alex Harri 2024; Chen et al. arXiv 2503.14375 confirm classical k-NN matches CNN quality at ~1% the cost)
> - OKLab/OKLCH palette interpolation for hue-preserving blends
> - Spatiotemporal Blue Noise approximation for smooth per-frame dithering (NVIDIA EGSR 2022)
> - Lottes-style CRT-beam postprocess (rolling raster + slot-mask aperture grille)
>
> Live-tunable via Tweakpane: 70+ bindings. 11 palettes × 4 color modes × 9 dithers exhaustively rendered as a regression test (see `BUGS_AND_ITERATIONS.md`).
>
> macOS only for v0.1; Linux + Windows builds straightforward (Tauri targets all three) but I haven't tested.
>
> GitHub: github.com/Joona-t/glyph-grid-studio
> Direct download: [DMG link to v0.1.0 release]
>
> Happy to answer questions about the rendering pipeline.

#### X / Twitter — launch tweet

> built a desktop ASCII art generator for macOS
>
> 11 palettes, 9 dithering modes (incl. spatiotemporal blue noise), 8 postprocess stages (incl. CRT beam scanlines), live tweakable, native GIF export
>
> p5.js + tauri + rust · MIT · free
>
> [hero GIF attached]
>
> github.com/Joona-t/glyph-grid-studio

Reply tweets:
1. Demo video (30s screen record)
2. "How it works" — character-grid mosaic, 6-D shape-vector glyph picking, link to algorithmic notes section of README
3. "Test image was Clare from Claymore. The cream-paper × stbn × octant preset turned her into a literal ink drawing"

#### Instagram (carousel post)

Slide 1: hero GIF still (1:1)
Slide 2: another palette variant
Slide 3: another palette variant
Slide 4: side-by-side close-up showing the actual glyphs
Slide 5: text-only "Built with: p5.js · Tauri · Rust · MIT · github.com/Joona-t/glyph-grid-studio"

Caption:
> Built a real-time character-grid image renderer for macOS. Every pixel you see is a typeset Unicode glyph — block-elements, octants, sextants, braille — selected per cell via 6-D shape-vector nearest-neighbor lookup. p5.js front end, Tauri shell, Rust backend for native GIF muxing. Free + open source. Test image: Clare from Claymore.
>
> #generativeart #creativecoding #asciiart #unicodeart #rustlang #tauri #p5js #macapp #vibecoded

#### Reddit

- **r/generative**: hero GIF + paragraph version of HN post + link
- **r/creativecoding**: same as r/generative
- **r/p5js**: focus on the p5 sketch architecture, link to `src/index.html`
- **r/rust**: focus on the Tauri command + `gif` crate usage, link to `src-tauri/src/lib.rs`
- **r/Claymore**: post just the cream-paper Clare GIF, "made an ASCII art generator and tested it on Clare"
- **r/macapps**: download link + screenshots

#### p5.js Discord (#showcase)

> Made a Tauri-wrapped p5.js sketch for converting images to character-grid art. Octants + braille + block elements via 6-D shape-vector NN. [hero GIF] [GitHub link]

## Timing

- **Day**: Tuesday or Wednesday, ~9 AM Pacific (12 PM Eastern, 5 PM UTC). Avoids Friday afternoon dead zone, catches US morning + EU evening
- **Show HN first**, then within 30 minutes: Twitter post, then IG, then Reddit (stagger so you can respond to early HN comments without splitting attention)
- **Avoid**: holiday weeks, the day before/after a major Apple/Google event, anytime you can't sit on the couch and reply to comments for 4 hours

## Distribution

- **GitHub Releases v0.1.0** — notarized DMG + .app.zip fallback + SHA-256 + release notes
- **Direct download link** in README (don't make people hunt through Releases)
- **Optional v0.1+**: Homebrew cask formula → `brew install --cask glyph-grid-studio`
- **NOT shipping to**: Mac App Store (sandbox restrictions break the file picker UX), SetApp, or any paid distribution platform — would conflict with the free / MIT positioning

## Pricing

Free. MIT. No payment integration, no premium tier, no telemetry. If demand emerges later for "support development" → GitHub Sponsors button on the repo, never a paywall in the app.

## Metrics to track

24-hour window after launch:
- GitHub stars + forks
- Direct downloads from Releases (gh api)
- HN ranking + comment count
- Twitter impressions + likes + RTs
- IG saves (saves are the IG metric that actually matters — likes are noise)
- Bug reports filed as GitHub Issues

7-day window:
- Sustained star growth (vs spike-and-die)
- Issues closed vs opened ratio
- Any forks worth merging back

30-day window:
- Did anyone build something with it and post about it?
- Any feature requests with consistent voting (3+ thumbs-up reactions)?

## Day-of war room

Have these tabs open BEFORE you press post:
- HN submission page (use `Show HN:` prefix)
- Tweet draft saved
- IG post draft saved (in Notes app — IG won't let you save drafts > 1 day)
- GitHub repo open in one tab, Issues tab in another
- This doc open for copy-paste

Have ready:
- Phone charged for IG
- 4 hours blocked on the calendar after posting
- Nothing else shipped that week (don't dilute attention)

## Day +1 to +7

- Reply to every HN comment (yes, even the rude ones — terse, factual, no defensiveness)
- File any bug repros as GitHub Issues immediately
- If a demo gets shared a lot, retweet it from your account
- DON'T immediately ship features in response to feature requests — let the dust settle. v0.1.1 patch release after a week if there are real bugs

## Day +30

- Write a postmortem blog post / X thread: "what I learned from launching X"
- Decide on v0.2 scope based on what people actually used
- Cross-post the project to your portfolio site

## Out of scope for v0.1 launch

- Linux build (do v0.2)
- Windows build (do v0.3 — no Mac to test on)
- iOS / iPad version (would require Tauri Mobile, currently alpha)
- Web version (would require dropping the Rust backend; defer)
- LLM-driven preset generation (Stage 3B — needs local Claude bridge per CLAUDE.md rule #10)
- Mac App Store submission

## Risks

- **Notarization fails on launch day** → mitigation: notarize a week early and verify the DMG opens cleanly on a friend's Mac
- **HN Show HN dies in /new with 0 votes** → that's fine, the X+IG posts are independent. Resubmit `Show HN` once 7 days later if it didn't get traction (HN allows this)
- **Critical bug discovered hour 1** → take down the Releases binary, post a "patching" comment on HN, ship v0.1.1 within 24h
- **Someone calls it "just a filter"** → respond once with the technical explanation and link to the algorithmic notes; don't argue further

## Success definition

If 50 people install it and 5 of them tell me it's cool, the launch was a success. Anything beyond that is a bonus.
