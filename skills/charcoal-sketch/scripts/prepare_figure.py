#!/usr/bin/env python3
"""Prepare a photo of a person for the charcoal renderer.

    python3 prepare_figure.py <image> [-o prepared.png] [--face X,Y,RX,RY]

The renderer measures every cell as "distance from cream paper". That works
out of the box for a dark figure against a bright background (the Paul
reference was a backlit silhouette against a pale sky). For ordinary photos
two things go wrong, and this script fixes both:

  1. The background renders as texture. We cut the figure out with rembg's
     human-segmentation model and place it on pure white, which the renderer
     treats as blank paper.
  2. Pale skin barely registers, so hands and faces dissolve into sparse
     "debris". We compress the figure's tones so even the brightest skin sits
     well below paper-white, and darken a thin band just inside the
     silhouette so the outline reads like a charcoal contour.

Compressing tones flattens facial features, so pass --face to keep a soft
oval around the face on a lighter curve. X,Y,RX,RY are fractions of the
image (0-1): centre x, centre y, radius x, radius y. Look at the image and
estimate them; e.g. a centred head-and-shoulders portrait is roughly
--face 0.5,0.33,0.15,0.13.

Output is upscaled so its long edge is 1400px. Small sources render coarse:
the grain only reads as charcoal when the 6px-cell render is downscaled
for the GIF.

Needs: pip install rembg onnxruntime pillow   (first run downloads ~170 MB model)
"""
import argparse
import sys

from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageOps


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("image")
    ap.add_argument("-o", "--out", default="prepared.png")
    ap.add_argument("--face", help="X,Y,RX,RY as 0-1 fractions; keeps facial features light")
    ap.add_argument("--long-edge", type=int, default=1400)
    ap.add_argument("--body-floor", type=float, default=0.10,
                    help="darkest body tone (0-1). Default 0.10")
    ap.add_argument("--body-range", type=float, default=0.50,
                    help="body tone span above the floor; brightest skin = floor+range. "
                         "Lower = denser, clearer pale skin. Default 0.50")
    ap.add_argument("--contour", type=int, default=15,
                    help="contour band width in px at 1400px (0 disables). Default 15")
    ap.add_argument("--mask-out", help="also save the person mask here")
    args = ap.parse_args()

    try:
        from rembg import new_session, remove
    except ImportError:
        sys.exit("rembg not installed: pip install rembg onnxruntime pillow")

    src = Image.open(args.image).convert("RGB")
    mask = remove(src, session=new_session("u2net_human_seg"), only_mask=True).convert("L")

    scale = args.long_edge / max(src.size)
    size = (round(src.width * scale), round(src.height * scale))
    src = src.resize(size, Image.LANCZOS)
    mask = mask.resize(size, Image.LANCZOS)
    W, H = size

    gray = ImageOps.grayscale(src)
    floor, span = args.body_floor, args.body_range
    body = gray.point(lambda v: int(255 * (floor + span * (v / 255) ** 1.2)))

    if args.face:
        fx, fy, rx, ry = (float(p) for p in args.face.split(","))
        face_tone = gray.point(lambda v: int(255 * (v / 255) ** 1.5))
        oval = Image.new("L", size, 0)
        ImageDraw.Draw(oval).ellipse(
            (W * (fx - rx), H * (fy - ry), W * (fx + rx), H * (fy + ry)), fill=255)
        oval = oval.filter(ImageFilter.GaussianBlur(max(W, H) * 0.02))
        body = Image.composite(face_tone, body, oval)

    if args.contour > 0:
        hard = mask.point(lambda v: 255 if v > 128 else 0)
        k = args.contour | 1  # MinFilter needs an odd size
        band = ImageChops.subtract(hard, hard.filter(ImageFilter.MinFilter(k)))
        band = band.filter(ImageFilter.GaussianBlur(2))
        body = Image.composite(body.point(lambda v: int(v * 0.4)), body, band)

    fig = Image.merge("RGB", (body, body, body))
    out = Image.composite(fig, Image.new("RGB", size, (255, 255, 255)), mask)
    out.save(args.out)
    if args.mask_out:
        mask.save(args.mask_out)
    print(f"wrote {args.out} ({W}x{H})")


if __name__ == "__main__":
    main()
