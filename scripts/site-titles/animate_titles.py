"""Animate the charcoal titles: the skill's own seamless breathing loop, as MP4.

usage: python3 animate_titles.py <work dir from build_titles.py> <render.mjs> <titles dir> [jobs]

For each title source the build wrote (<name>-<theme>-<layout>.png), render
the skill's loop (4 s, 12 fps; every mark is a pure sine of time, so frame N
is exact and the loop is seamless), key each frame to alpha exactly like the
stills (rekey.py), crop it to the trimmed still's width, and composite it for
a CSS blend so H.264 can carry the transparency:

  basalt (light marks)  -> premultiplied onto black,  shown mix-blend-mode: screen
  cream / bubblegum (dark)  -> composited onto white,     shown mix-blend-mode: multiply

Writes <titles dir>/<name>-<theme>-<layout>.mp4 (CRF 32, faststart, yuv420p).
An alpha WebP of the same loop was 10.7 MB for the hero; this is ~0.7 MB.
"""
import glob, os, shutil, subprocess, sys
from concurrent.futures import ThreadPoolExecutor
import numpy as np
from PIL import Image

WORK, RENDER, TITLES = sys.argv[1], sys.argv[2], sys.argv[3]
JOBS = int(sys.argv[4]) if len(sys.argv) > 4 else 3
PAL = {'cream': ('charcoal', '#EFE3CF', '#1F1612'), 'bubblegum': ('y2k-pink', '#FFE6F2', '#8A4259'),
       'basalt': ('basalt', '#16171A', '#E6D6AD')}
DURATION, FPS = 4, 12
hexv = lambda h: np.array([int(h[i:i + 2], 16) for i in (1, 3, 5)], float)

def smooth(e0, e1, x):
    k = np.clip((x - e0) / (e1 - e0), 0, 1)
    return k * k * (3 - 2 * k)

def one(src):
    stem = os.path.basename(src)[:-4]
    theme = stem.split('-')[-2]
    pal, bgh, inkh = PAL[theme]
    bg, ink = hexv(bgh), hexv(inkh)
    out = os.path.join(TITLES, stem + '.mp4')
    still_w = Image.open(os.path.join(TITLES, stem + '.webp')).width
    rdir = os.path.join(WORK, 'anim-' + stem)
    w0 = Image.open(src).width
    subprocess.run(['node', RENDER, src, '--palette', pal, '--formats', 'gif', '--dust', 'edge', '--cell', '3',
                    '--maxdim', str(max(Image.open(src).size)), '--duration', str(DURATION), '--gif-fps', str(FPS),
                    '--gif-width', '160', '--gif-colors', '4', '--keep-frames', '--out', rdir],
                   check=True, capture_output=True)
    frames = sorted(glob.glob(os.path.join(rdir, '.work', 'frames', 'frame_*.png')))[:DURATION * FPS]
    x0 = (w0 - still_w) // 2
    comp = os.path.join(rdir, 'comp'); os.makedirs(comp, exist_ok=True)
    for i, f in enumerate(frames):
        a = np.asarray(Image.open(f).convert('RGB')).astype(float)
        al = np.clip(np.linalg.norm(a - bg, axis=2) / np.linalg.norm(ink - bg) * 1.12, 0, 1)
        al = al * smooth(0.05, 0.22, al)
        al = al[:, x0:x0 + still_w]
        h, w = al.shape
        fy, fx = max(8, int(h * 0.08)), max(8, int(w * 0.04))
        ey = smooth(0, 1, np.minimum(np.arange(h), np.arange(h)[::-1]) / fy)
        ex = smooth(0, 1, np.minimum(np.arange(w), np.arange(w)[::-1]) / fx)
        al = (al * ey[:, None] * ex[None, :])[..., None]
        rgb = ink * al if theme == 'basalt' else 255 * (1 - al) + ink * al
        h2, w2 = h - h % 2, w - w % 2                               # H.264 needs even dimensions
        Image.fromarray(rgb[:h2, :w2].round().astype(np.uint8), 'RGB').save(os.path.join(comp, f'c_{i:03d}.png'))
    subprocess.run(['ffmpeg', '-loglevel', 'error', '-y', '-framerate', str(FPS), '-i', os.path.join(comp, 'c_%03d.png'),
                    '-c:v', 'libx264', '-preset', 'slow', '-crf', '32', '-pix_fmt', 'yuv420p', '-movflags', '+faststart',
                    '-an', out], check=True)
    shutil.rmtree(rdir)
    return stem, len(frames), os.path.getsize(out) // 1024

srcs = sorted(p for p in glob.glob(os.path.join(WORK, '*-*-*.png'))
              if os.path.basename(p).split('-')[-2] in PAL and not os.path.basename(p).startswith('shot-'))
with ThreadPoolExecutor(JOBS) as ex:
    for stem, n, kb in ex.map(one, srcs):
        print(stem, n, 'frames', kb, 'KB', flush=True)
print('done')
