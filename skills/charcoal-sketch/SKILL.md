---
name: charcoal-sketch
description: Converts any photo into the "charcoal method" look — a figure drawn as a shimmering field of tiny warm-black charcoal marks on aged cream paper, with dust lifting off its outline — as a still PNG and a seamlessly looping, "breathing" GIF or MP4. Use this whenever the user says "charcoal method", "charcoal sketch", "cream paper", "dust portrait", "make it like the Paul one", or asks to turn a photo or portrait into charcoal / ink-on-cream / dissolving-dust glyph art, even if they don't name the skill. Not for generic ASCII art or neon/CRT looks (that's glyph-grid).
---

# charcoal-sketch

One command turns an image into the charcoal look. All the craft lives in
the scripts; your job is to pick the right options, run them, and check
the result.

Read `references/methods.md` only if you need to change how the look is
made (new layers, new palettes) or debug the renderer.

## Requirements

Node 22+, ffmpeg, and Chrome or Edge. For `prepare_figure.py`: Python 3 with
`pip install rembg onnxruntime pillow` (first run downloads a ~170 MB model).

## Step 1 — decide whether the photo needs preparing

The renderer treats anything far from paper-white as "drawing". So:

| Photo | What to do |
|---|---|
| Dark or backlit figure on a bright, plain background (like the Paul reference) | Render directly — skip to Step 2 |
| A person on a busy or dark background, or "remove the background" | Run `prepare_figure.py` first |
| Pale skin, hands, or faces disappearing into the paper | Run `prepare_figure.py` (it darkens the figure) |
| Not a person (object, landscape) | Render directly; `prepare_figure.py` only segments people |

```bash
python3 scripts/prepare_figure.py photo.jpg -o prepared.png --face X,Y,RX,RY
```

`--face` keeps facial features legible (without it the body darkening also
flattens the face into hatching). Look at the image and estimate the face
oval as fractions of width/height: centre X, centre Y, radius X, radius Y.
A centred head-and-shoulders portrait is about `0.5,0.33,0.15,0.13`.
Omit `--face` for back views, silhouettes, or tiny distant faces.

## Step 2 — render

```bash
node scripts/render.mjs prepared.png --formats still,gif --dust edge --gif-colors 16
```

Outputs land in `./charcoal-<name>/`: `<name>.png` (full-res still) and
`<name>.gif` (600px wide, 12fps, 6s seamless loop). Add `mp4` to
`--formats` for a 30fps H.264 video. A still takes seconds; the GIF about
a minute.

Defaults to keep unless there's a reason:

- `--dust edge` — dust only floats off the figure's outer outline. Use
  `--dust all` only to reproduce the original Paul piece exactly.
- `--gif-colors 16` — the art is two-tone, so 16 colors look identical to
  256 and roughly halve the file. Need it smaller? `--gif-width 480`.
- `--palette charcoal` (cream `#EFE3CF`, warm-black `#1F1612`). Pink
  variants for LoveSpark work: `y2k-pink`, `y2k-pastel`, `y2k-neon`.

## Step 3 — check before reporting

Look at the still downscaled to ~600px (e.g.
`ffmpeg -i <name>.png -vf scale=600:-1 check.png`) and view it. Good output:
the figure reads as a solid charcoal mass with soft dissolving edges; the
background is bare cream paper with faint grain.

| You see | Fix |
|---|---|
| Background covered in marks | Run `prepare_figure.py` |
| Hands/skin look like scattered debris | Run `prepare_figure.py`; if still faint, lower `--body-range` to 0.4 |
| Face is a smear of hatching | Add or adjust `--face` so the oval covers the face |
| Face/eyes vanish into paper | Oval too large or `--face` missing on a pale face — re-estimate |
| Marks look coarse / blocky | Source is small; `prepare_figure.py` upscales to 1400px — use it, or `--maxdim 1400` |
| GIF too big | `--gif-colors 16`, then `--gif-width 480` |

## Interactive use

Open `scripts/index.html` in a browser and drop an image on it for a live
preview. Recording from that page needs a server with a `/save` endpoint,
which `render.mjs` provides, so use the command line for GIFs.
