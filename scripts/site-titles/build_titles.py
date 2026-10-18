"""Render the site's large titles through the charcoal-sketch skill.

usage: python3 build_titles.py <docs dir> <skill render.mjs> <work dir>

1. Set every title in the site's own CSS (index.html <style>) per theme and
   layout (desktop 1280 px window, phone 375 px body), on the palette bg in the
   palette ink, and screenshot at 2x.
2. Crop each title with room for dust, run the real charcoal renderer.
3. Key the paper out: alpha from each pixel's distance to the bg, colour = ink.
4. Write docs/assets/titles/<name>-<theme>-<layout>.webp + titles.js manifest
   (size in em of the title's font so the image scales with the CSS clamp()).
"""
import json, os, re, subprocess, sys
from PIL import Image
import numpy as np

DOCS, RENDER, WORK = sys.argv[1], sys.argv[2], sys.argv[3]
CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
os.makedirs(WORK, exist_ok=True)
OUT = os.path.join(DOCS, 'assets', 'titles')
os.makedirs(OUT, exist_ok=True)

TITLES = [  # name, tag/class, html
    ('hero', 'h1', 'headline', 'Render any image into <em>living</em> glyphs.'),
    ('portraits', 'h2', 'sec-title', 'Portraits, rendered in glyphs'),
    ('new', 'h2', 'sec-title', 'Four new ways to make glyph art'),
    ('specimens', 'h2', 'sec-title', 'Nine specimens from the studio'),
    ('cli', 'h2', 'sec-title', 'Render from the shell, or let an agent drive'),
    ('firstlaunch', 'h2', 'sec-title', 'First launch on macOS'),
    ('gallery', 'h1', 'sec-title', 'Gallery'),
]
THEMES = {  # site theme -> charcoal palette (bg, ink)
    'cream': ('charcoal', '#EFE3CF', '#1F1612'),
    'candy': ('y2k-pink', '#FFE6F2', '#8A4259'),
    'basalt': ('basalt', '#16171A', '#E6D6AD'),
}
LAYOUTS = {'desk': (1280, None), 'mob': (500, 375)}  # window width, forced body width
PAD = dict(t=0.55, b=0.22, l=0.30, r=0.30)  # em of the title's font: room for dust + jitter
DPR = 2

css = re.search(r'<style>(.*?)</style>', open(os.path.join(DOCS, 'index.html')).read(), re.S).group(1)

def page(theme, layout):
    pal, bg, ink = THEMES[theme]
    body_w = LAYOUTS[layout][1]
    rows = []
    for name, tag, cls, html in TITLES:
        inner = f'<{tag} class="{cls}" data-ct-src="{name}">{html}</{tag}>'
        if cls == 'headline':
            rows.append(f'<header class="hero wrap" style="padding:60px 0 40px">{inner}</header>')
        else:
            rows.append(f'<section class="wrap"><div class="sec-head" style="margin:60px 0">{inner}</div></section>')
    over = f"""
      html,body{{background:{bg} !important;background-image:none !important}}
      body::after,.spark{{display:none !important}}
      h1,h2{{color:{ink} !important;-webkit-text-fill-color:{ink} !important}}
      h1.headline em{{color:{ink} !important;-webkit-text-fill-color:{ink} !important;background:none !important}}
      h1.headline em::after{{display:none !important}}
      {f'body{{width:{body_w}px;margin:0}}' if body_w else ''}"""
    probe = """<script>document.fonts.ready.then(function(){var o={};
      document.querySelectorAll('[data-ct-src]').forEach(function(e){var r=e.getBoundingClientRect();
      o[e.dataset.ctSrc]={x:r.left+scrollX,y:r.top+scrollY,w:r.width,h:r.height,fs:parseFloat(getComputedStyle(e).fontSize)};});
      document.body.setAttribute('data-rects',JSON.stringify(o));
      document.body.setAttribute('data-h',document.documentElement.scrollHeight);});</script>"""
    return (f'<!doctype html><html lang="en" data-theme="{theme}"><head><meta charset="utf-8">'
            f'<meta name="viewport" content="width=device-width,initial-scale=1"><style>{css}{over}</style></head>'
            f'<body>{"".join(rows)}{probe}</body></html>')

def chrome(args):
    return subprocess.run([CHROME, '--headless=new', '--hide-scrollbars', '--virtual-time-budget=3000'] + args,
                          capture_output=True, text=True).stdout

def key(src_png, bg, ink, dst_webp):
    a = np.asarray(Image.open(src_png).convert('RGB')).astype(float)
    bgv = np.array([int(bg[i:i + 2], 16) for i in (1, 3, 5)], float)
    inkv = np.array([int(ink[i:i + 2], 16) for i in (1, 3, 5)], float)
    alpha = np.clip(np.linalg.norm(a - bgv, axis=2) / np.linalg.norm(inkv - bgv) * 1.12, 0, 1)
    rgba = np.zeros(a.shape[:2] + (4,), np.uint8)
    rgba[..., :3] = inkv.astype(np.uint8)
    rgba[..., 3] = (alpha * 255).round().astype(np.uint8)
    Image.fromarray(rgba, 'RGBA').save(dst_webp, 'WEBP', quality=86, method=6)

manifest = {}
for theme in THEMES:
    pal, bg, ink = THEMES[theme]
    for layout, (win_w, _) in LAYOUTS.items():
        tag = f'{theme}-{layout}'
        html_path = os.path.join(DOCS, f'_ct-{tag}.html')   # inside docs so assets/fonts resolve
        open(html_path, 'w').write(page(theme, layout))
        dom = chrome([f'--window-size={win_w},900', '--dump-dom', 'file://' + html_path])
        rects = json.loads(re.search(r'data-rects="([^"]*)"', dom).group(1).replace('&quot;', '"'))
        doc_h = int(float(re.search(r'data-h="([^"]*)"', dom).group(1)))
        shot = os.path.join(WORK, f'shot-{tag}.png')
        chrome([f'--window-size={win_w},{doc_h}', f'--force-device-scale-factor={DPR}', '--screenshot=' + shot,
                'file://' + html_path])
        os.remove(html_path)
        im = Image.open(shot).convert('RGB')
        for name, *_ in TITLES:
            r = rects[name]; fs = r['fs']
            box = (r['x'] - PAD['l'] * fs, r['y'] - PAD['t'] * fs, r['x'] + r['w'] + PAD['r'] * fs, r['y'] + r['h'] + PAD['b'] * fs)
            crop = im.crop(tuple(round(v * DPR) for v in box))
            src = os.path.join(WORK, f'{name}-{tag}.png'); crop.save(src)
            outdir = os.path.join(WORK, f'cc-{name}-{tag}')
            subprocess.run(['node', RENDER, src, '--palette', pal, '--formats', 'still', '--dust', 'edge',
                            '--cell', '3', '--maxdim', str(max(crop.size)), '--out', outdir],
                           check=True, capture_output=True)
            rendered = os.path.join(outdir, os.path.basename(src))
            assert Image.open(rendered).size == crop.size, (rendered, Image.open(rendered).size, crop.size)
            key(rendered, bg, ink, os.path.join(OUT, f'{name}-{tag}.webp'))
            manifest.setdefault(name, {}).setdefault(theme, {})[layout] = {
                'w': round((box[2] - box[0]) / fs, 4), 'pt': PAD['t'], 'pb': PAD['b']}
            print(tag, name, crop.size, flush=True)

with open(os.path.join(OUT, 'titles.js'), 'w') as f:
    f.write('/* generated by the charcoal title build: width and dust padding in em of each title\'s font */\n')
    f.write('window.CHARCOAL_TITLES = ' + json.dumps(manifest, separators=(',', ':')) + ';\n')
print('done')
