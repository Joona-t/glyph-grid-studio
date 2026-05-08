# TODO — Glyph Grid Studio

Forward-looking, post-v0.1 items.  Bugs go in `BUGS_AND_ITERATIONS.md`;
launch-blocking work goes in `PRE-PUBLIC-CHECKLIST.md`; this file is for
small features and follow-ups that came up while shipping something else
and want a place to live until they're done.

Newest at top.

---

## Export polish (queued after the MP4 v1 ship — commit `45f5898`)

### 1. Bundle `libopenh264.dylib` so MP4 export works offline on first run

**Why:** the openh264 crate (libloading default; we use the `source` feature
which builds from C source via nasm at build time, so this *should* be a
no-op).  Verify on a fresh clone of the repo + machine without nasm:
either nasm is required at user's build time (in which case bundling the
prebuilt encoder removes that), or the `source` feature genuinely vendors
everything at our build time and there's nothing to do.  If the latter,
close this item.

**How to apply:** clone fresh on a machine without nasm, try `cargo tauri
build`.  If it fails, switch to `openh264 = { version = "0.9.3", default-features = false, features = ["libloading"] }` and ship Cisco's
prebuilt dylib in `src-tauri/resources/`.

### 2. Instagram-aspect-ratio export presets (9:16 Stories, 1:1 feed, 4:5 portrait)

**Why:** v1 outputs at native canvas aspect (1024×572 for kaneki).  Posting
to IG Stories or Reels then requires the user to crop / pad in IG's
editor.  A "for Instagram" dropdown in the Export folder that letterboxes
to a target aspect (with a configurable letterbox colour, defaulting to
the palette's bg colour) would be a nice-to-have.

**How to apply:** add a sibling control to `sizeOpts.capWidth` —
`aspectMode: 'native' | '9:16' | '1:1' | '4:5'`.  In `encode_mp4_h264`
and `encode_gif_gifski`, when `aspect_mode != native`, compute target
dimensions for the requested aspect at the cap width, then composite each
decoded frame onto the letterbox canvas before encoding.  Letterbox fill
colour reads from `CONFIG.palette` bg.

### 3. Loop-count config in the Studio panel (export N seamless loops in one click)

**Why:** currently to export 3 loops you have to do the math yourself —
multiply `animation.duration` by 3, change the slider, then export.
Animated GIF sources already loop seamlessly thanks to `loopT = (t × 1000)
% totalMs`, so the studio mathematically supports it; the UI just doesn't.

**How to apply:** add `animation.loops: number` (default 1, range 1–10) to
the CONFIG schema and the Animation folder.  In `exportPlan()`, multiply
the computed frame count by `loops`.  Verify the loop seam stays clean
(ratio ≈ 1.0) for `loops = 3` (already verified manually for the kaneki
3-loops MP4 — ratio 1.148).

---

## (older items, if any, would go below)
