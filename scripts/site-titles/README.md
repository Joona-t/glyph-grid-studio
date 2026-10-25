# Charcoal site titles

Rebuilds `docs/assets/titles/*.webp` + `titles.js`: every large heading on the
site, drawn by the charcoal-sketch skill (`skills/charcoal-sketch`, branch
`skill/charcoal-sketch`) in each theme's palette (cream → `charcoal`,
bubblegum → `y2k-pink`, basalt → `basalt`) at a desktop and a phone layout.
Run after changing a heading's text, font or size, in this order:

```bash
python3 scripts/site-titles/build_titles.py docs <path to charcoal-sketch>/scripts/render.mjs <work dir>
python3 scripts/site-titles/rekey.py <work dir> docs/assets/titles
python3 scripts/site-titles/trim.py docs/assets/titles
python3 scripts/site-titles/animate_titles.py <work dir> <path to charcoal-sketch>/scripts/render.mjs docs/assets/titles 3
```

Needs Chrome, Node 22+, Python 3 with Pillow + numpy. About 9 minutes for the 42 stills,
~20 more for the 42 breathing MP4s (3 in parallel). To add a heading: give it `data-ct="<name>"` and wrap its text in
`<span class="ct-text">`, add it to `TITLES` in `build_titles.py`, rebuild.
