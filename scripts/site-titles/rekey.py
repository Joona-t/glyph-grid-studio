"""Re-key the rendered charcoal titles: soft floor on faint grain + edge feather.

usage: python3 rekey.py <work dir> <titles out dir>
"""
import glob, os, sys
import numpy as np
from PIL import Image

WORK, OUT = sys.argv[1], sys.argv[2]
THEMES = {'cream': ('#EFE3CF', '#1F1612'), 'candy': ('#FFE6F2', '#8A4259'), 'basalt': ('#16171A', '#E6D6AD')}
hexv = lambda h: np.array([int(h[i:i + 2], 16) for i in (1, 3, 5)], float)

def smooth(e0, e1, x):
    k = np.clip((x - e0) / (e1 - e0), 0, 1)
    return k * k * (3 - 2 * k)

total = 0
for d in sorted(glob.glob(os.path.join(WORK, 'cc-*'))):
    stem = os.path.basename(d)[3:]                     # name-theme-layout
    theme = stem.split('-')[-2]
    bg, ink = map(hexv, THEMES[theme])
    a = np.asarray(Image.open(os.path.join(d, stem + '.png')).convert('RGB')).astype(float)
    al = np.clip(np.linalg.norm(a - bg, axis=2) / np.linalg.norm(ink - bg) * 1.12, 0, 1)
    al = al * smooth(0.05, 0.22, al)                   # fade the faint paper grain, keep marks and dust
    h, w = al.shape
    fy, fx = max(8, int(h * 0.08)), max(8, int(w * 0.08))
    ey = smooth(0, 1, np.minimum(np.arange(h), np.arange(h)[::-1]) / fy)
    ex = smooth(0, 1, np.minimum(np.arange(w), np.arange(w)[::-1]) / fx)
    al = al * ey[:, None] * ex[None, :]                # feather: no crop rectangle can show
    rgba = np.zeros((h, w, 4), np.uint8)
    rgba[..., :3] = ink.astype(np.uint8)
    rgba[..., 3] = (al * 255).round().astype(np.uint8)
    dst = os.path.join(OUT, stem + '.webp')
    Image.fromarray(rgba, 'RGBA').save(dst, 'WEBP', quality=80, method=6)
    total += os.path.getsize(dst)
    print(stem, (w, h), os.path.getsize(dst) // 1024, 'KB')
print('total', total // 1024, 'KB')
