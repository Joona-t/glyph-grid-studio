# The charcoal method — how the look is made

This is a write-up of how the cream-paper charcoal look works, and what we
learned getting it right. The reference piece is a backlit portrait of
Paul Atreides that reads like charcoal dust on paper, its silhouette
breathing while the grain shimmers. The technique descends from the
glyph-grid renderer, which itself follows a long ASCII-art tradition
(Kyle McDonald, fogleman, demoscene text-mode art); the cream-paper
direction was prompted by a post from @The_Null_Path.

## 1. The core idea: a photo becomes a field of marks

The image is never shown directly. It's drawn into a hidden buffer, and a
second pass walks a grid over it — one cell per ~6px. For each cell the
renderer decides *whether* to put a mark there, *which* mark, and *how
dark*. What you see is only those marks, typeset in warm-black ink on a
cream background. Everything below is about making those marks read as a
hand-drawn charcoal drawing instead of a computer printout.

## 2. Measuring each cell against the paper

Each cell's colour is compared to the paper colour (`#EFE3CF`), not to
black. The distance is the cell's **signal**: 0 means "this is blank
paper", 1 means "as far from paper as possible". This is why the method
works on cream: the background drops out by itself, and only things that
differ from paper get drawn.

Before measuring, the image is converted to grayscale with a gentle
contrast curve (`lum^1.10`). A heavier curve crushes highlights into the
paper, because the later layers already amplify contrast.

The signal is then bent with a gamma of 0.62 before choosing a mark, so
mid-tones climb into the expressive middle of the mark set rather than
staying as faint dots.

## 3. Choosing marks: a charcoal ramp

Marks come from a ramp ordered light to dark:

```
 .,'`":;-+/\xtfLG$#@
```

It's skewed toward mid-weight marks on purpose. Real charcoal never leaves
wide clean holes — even light areas carry texture — so the ramp has many
small scratchy marks and only a few heavy ones.

## 4. The five layers

Each frame is drawn in this order:

1. **Paper grain.** About 18% of the paper gets faint marks (`.,'``) at
   very low opacity, scattered on a hash so they never move. Without this,
   empty paper looks digitally flat.
2. **Dust** (drawn *under* the figure). Specks rise off the figure and fade
   as they climb, swaying sideways more the higher they get. Because the
   figure is drawn on top, only specks that escape past the silhouette are
   visible — so the figure looks like it's fragmenting into the air.
3. **The jittered grid.** Every mark is nudged off its cell centre (±20%)
   and rotated (±7°), with a fixed per-cell random value. This one change
   is what stops the grid reading as a grid; it is the keystone of the
   hand-drawn feel.
4. **Directional hatching.** Where the image has a strong edge, the mark is
   swapped for a stroke laid *along* the edge (`- | / \`), perpendicular
   to the brightness gradient. That's how charcoal artists shade form:
   strokes follow the contour, not the light. A ±10° wobble prevents a
   staircase look on curves. Smooth areas like cheeks are left alone.
5. **Shadow buildup.** In the darkest areas a second, lighter mark is laid
   slightly offset at reduced opacity. It reads as the smudge around heavy
   strokes, not as a duplicate.

## 5. Breathing: how the animation loops

Nothing is simulated frame to frame. Every mark's offset and rotation is a
sine wave of time with its own phase, so neighbouring marks never move in
sync — like an artist's hand hovering without letting the marks settle.
The dust is also a pure function of time: each speck's position in its
rise is `(time / loop + its own phase) mod 1`.

Because everything depends only on time, frame N is always identical, and
the last frame flows into the first. Frames are recorded at exactly
`i / total × duration`, which makes the 6-second loop seamless.

## 6. Why render big and shrink

Renders are made at ~1400px with 6px cells, then the GIF is downscaled to
600px. At full size the marks are visibly letters; shrunk, they blur into
charcoal grain while keeping a shimmering, granular texture. Rendering
directly at 600px gives coarse, blocky marks — so small source images are
upscaled to 1400px first.

## 7. Dust only from the outline

Originally dust rose from every part of the figure, and it showed through
the gaps between marks inside the body, which reads as debris. The
`edge` dust mode fixes this in three steps:

1. **Mask the figure.** Each cell is "figure" if its signal clears the
   dust threshold.
2. **Fill holes.** Pale regions inside the figure (a lit face) fall under
   the threshold and look like holes with their own outline. We flood-fill
   paper inward from the canvas border; anything the fill can't reach is
   enclosed by the figure, so it counts as figure. Only the true outer
   silhouette remains.
3. **Emit from the outline, hide over the body.** Only figure cells within
   2 cells of paper emit dust, and any speck currently over the figure is
   skipped — so dust rising from a shoulder never drifts across the neck.

## 8. Preparing ordinary photos

Paul worked untouched because he was a dark silhouette against bright sky.
Most photos aren't. `prepare_figure.py` handles the two common failures:

- **Busy background.** The person is cut out with rembg's human
  segmentation model and placed on pure white, which the renderer treats
  as blank paper.
- **Pale skin dissolves.** Light skin sits close to paper, so hands and
  faces turn into sparse scattered marks. The figure's tones are
  compressed so the brightest skin lands at ~60% grey
  (`0.10 + 0.50 × lum^1.2`), and a soft band just inside the silhouette is
  darkened to 40% — a charcoal contour line.

That compression flattens facial features into hatching, so the face gets
its own soft oval on a lighter curve (`lum^1.5`) where the eyes and lips
stay legible. The oval is estimated from the image and passed as `--face`.

## 9. Keeping GIFs small

The art is effectively two tones. A 256-colour GIF with dithering wastes
almost all its bits on dither noise that changes every frame. Encoding with
a 16-colour palette and no dither looks identical and cut the reference
GIFs roughly in half (8.5 MB → 4.0 MB for Paul; 34 MB → 12 MB for a tall
portrait).

## 10. Pitfalls we hit

- **A name collision with p5.js.** The renderer runs p5 in global mode,
  which installs its own functions as globals and silently replaced our
  `saveCanvas` helper with p5's (a browser download — a no-op when
  headless). Renders "finished" with no file. Avoid naming helpers after
  p5 functions; ours is `postCanvas`.
- **Background throttling.** Headless recording runs with timer throttling
  disabled, otherwise the hidden page slows to a crawl.
- **Busy, low-contrast photos** (a face against a cluttered room) render as
  uniform texture with no readable subject. Prepare them, or pick a photo
  with a clear figure.

## Tuning reference

| Knob | Where | Effect |
|---|---|---|
| Cell size | `--cell` (default 6) | Smaller = finer grain, slower |
| Contrast curve | `preprocessImage`, `lum^1.10` | Heavier crushes highlights |
| Signal gamma | `brightnessGamma` 0.62 | Lower = denser mid-tones |
| Jitter | `ROT_JITTER_RAD`, `POS_JITTER_FRAC` | The hand-drawn feel |
| Breathing amount | `ANIM_ROT_RAD`, `ANIM_POS_FRAC` | How much marks sway |
| Dust | `dust.*` in CONFIG, `--dust` | Plume height, density, mode |
| Figure darkness | `--body-floor`, `--body-range` | How dark pale skin renders |
