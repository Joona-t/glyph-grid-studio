"""Deterministically generate the synthetic benchmark sources.

Produces:
  tests/sources/cream-paper.png    — tri-modal luminance test image
                                     (deep ink / mid stipple / paper bg)
  tests/sources/synthetic-noise.png — high-frequency stress test

Both are 720×480 (matches ghost-I.gif aspect, hits the studio's
default canvas-fit path), seed-deterministic, regenerable on demand.
"""
from __future__ import annotations

import argparse
import struct
import zlib
from pathlib import Path

# Pure-stdlib PNG writer keeps the orchestrator script free of Pillow.
def write_png(path: Path, w: int, h: int, rgb: bytes) -> None:
    def chunk(tag: bytes, data: bytes) -> bytes:
        crc = zlib.crc32(tag + data) & 0xFFFFFFFF
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", crc)

    raw = bytearray()
    stride = w * 3
    for y in range(h):
        raw.append(0)  # filter byte: None
        raw.extend(rgb[y * stride:(y + 1) * stride])
    sig = b"\x89PNG\r\n\x1a\n"
    ihdr = struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)  # 8-bit RGB
    idat = zlib.compress(bytes(raw), 9)
    path.write_bytes(sig + chunk(b"IHDR", ihdr) + chunk(b"IDAT", idat) + chunk(b"IEND", b""))


def gen_cream_paper(w: int = 720, h: int = 480, seed: int = 1337) -> bytes:
    """Tri-modal luminance: cream paper bg + dark ink stippling +
    mid-tone glyph stipple grid pattern. Mirrors the ITER-022
    test material that surfaced the dispersal threshold tuning."""
    rng = _Lcg(seed)
    out = bytearray(w * h * 3)
    for y in range(h):
        for x in range(w):
            # Cream paper base (cream-paper palette ~ #F5ECDA)
            r, g, b = 245, 236, 218
            # Vertical stripe at every 4 px (column grid hint)
            if x % 4 == 0:
                r, g, b = max(r - 18, 0), max(g - 22, 0), max(b - 26, 0)
            # Random dark ink dots at ~6% density
            if rng.next() < 0.06:
                r, g, b = 36, 28, 22
            # Mid-tone "shading" band in middle-third
            if h // 3 < y < 2 * h // 3 and rng.next() < 0.18:
                r, g, b = 168, 150, 122
            i = (y * w + x) * 3
            out[i] = r
            out[i + 1] = g
            out[i + 2] = b
    return bytes(out)


def gen_synthetic_noise(w: int = 720, h: int = 480, seed: int = 2026) -> bytes:
    """High-frequency colored noise — pathological stress for the
    grid stage (no spatial coherence to exploit; every cell different)."""
    rng = _Lcg(seed)
    out = bytearray(w * h * 3)
    for y in range(h):
        for x in range(w):
            i = (y * w + x) * 3
            out[i] = int(rng.next() * 256)
            out[i + 1] = int(rng.next() * 256)
            out[i + 2] = int(rng.next() * 256)
    return bytes(out)


class _Lcg:
    """Deterministic PRNG (seed → [0, 1) floats). Numerical Recipes LCG."""
    def __init__(self, seed: int) -> None:
        self.s = seed & 0xFFFFFFFF

    def next(self) -> float:
        self.s = (self.s * 1664525 + 1013904223) & 0xFFFFFFFF
        return self.s / 0x100000000


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out-dir", default="/Users/darkfire/glyph-grid-studio/tests/sources",
                    help="Where to write the synthetic sources.")
    args = ap.parse_args()
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    cream_path = out_dir / "cream-paper.png"
    write_png(cream_path, 720, 480, gen_cream_paper())
    print(f"wrote {cream_path} ({cream_path.stat().st_size} bytes)")

    noise_path = out_dir / "synthetic-noise.png"
    write_png(noise_path, 720, 480, gen_synthetic_noise())
    print(f"wrote {noise_path} ({noise_path.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
