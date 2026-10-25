#!/usr/bin/env node
// charcoal-sketch renderer driver — zero npm dependencies.
//
//   node render.mjs <image> [options]
//
// Converts an image into the cream-paper charcoal style: a still PNG plus
// (by default) a seamlessly looping GIF. Pipeline:
//   1. copy index.html + the image into a work dir
//   2. serve it locally (static files + POST /save for canvas captures)
//   3. drive headless Chrome/Edge over the DevTools protocol (Node's
//      built-in WebSocket, Node ≥ 22) with ?src=…&mode=still|record
//   4. stitch recorded frames with ffmpeg (GIF palette pipeline / H.264)
//
// Options:
//   --out DIR           output dir (default ./charcoal-<image name>)
//   --formats LIST      comma list of still,gif,mp4   (default still,gif)
//   --palette NAME      charcoal | basalt | y2k-pink | y2k-pastel | y2k-neon | …  (default charcoal)
//   --cell PX           grid cell size, smaller = finer grain        (default 6)
//   --maxdim PX         render canvas long edge                      (default 1400)
//   --duration S        loop length in seconds                       (default 6)
//   --gif-fps N         GIF frame rate                               (default 12)
//   --gif-width PX      GIF width; downscaling is what turns glyphs
//                       into charcoal grain                          (default 600)
//   --mp4-fps N         MP4 frame rate                               (default 30)
//   --dust MODE         all | edge | off — edge = dust only floats off the
//                       figure's outline, never across its interior  (default all)
//   --no-dust           same as --dust off
//   --gif-colors N      GIF palette size; 16 keeps two-tone art sharp and
//                       roughly halves file size vs 256                (default 256)
//   --keep-frames       keep the PNG frames + work dir
//   --chrome PATH       browser binary (else CHROME_PATH, else auto-detect)
//   --timeout MIN       give up after N minutes                      (default 45)

import { spawn, spawnSync } from 'node:child_process';
import http from 'node:http';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));

// ---------------------------------------------------------------- args
function parseArgs(argv) {
  const o = {
    formats: 'still,gif', palette: 'charcoal', cell: 6, maxdim: 1400, duration: 6,
    gifFps: 12, gifWidth: 600, gifColors: 256, mp4Fps: 30, dust: 'all', keepFrames: false,
    chrome: process.env.CHROME_PATH || '', timeout: 45, out: '', image: '',
  };
  const map = {
    '--out': 'out', '--formats': 'formats', '--palette': 'palette', '--cell': 'cell',
    '--maxdim': 'maxdim', '--duration': 'duration', '--gif-fps': 'gifFps',
    '--gif-width': 'gifWidth', '--gif-colors': 'gifColors', '--dust': 'dust', '--mp4-fps': 'mp4Fps', '--chrome': 'chrome', '--timeout': 'timeout',
  };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--no-dust') o.dust = 'off';
    else if (a === '--keep-frames') o.keepFrames = true;
    else if (a === '-h' || a === '--help') { o.help = true; }
    else if (map[a]) {
      const v = argv[++i];
      if (v === undefined) die(`missing value for ${a}`);
      o[map[a]] = typeof o[map[a]] === 'number' ? Number(v) : v;
    } else if (a.startsWith('--')) die(`unknown option ${a}`);
    else o.image = a;
  }
  return o;
}

function die(msg) { console.error(`error: ${msg}`); process.exit(1); }
const log = (...a) => console.log('[charcoal]', ...a);

// --------------------------------------------------------- browser find
function findBrowser(explicit) {
  if (explicit) {
    if (!fs.existsSync(explicit)) die(`browser not found at ${explicit}`);
    return explicit;
  }
  const pf = process.env['PROGRAMFILES'] || 'C:\\Program Files';
  const pf86 = process.env['PROGRAMFILES(X86)'] || 'C:\\Program Files (x86)';
  const local = process.env['LOCALAPPDATA'] || '';
  const candidates = [
    path.join(pf, 'Google/Chrome/Application/chrome.exe'),
    path.join(pf86, 'Google/Chrome/Application/chrome.exe'),
    path.join(local, 'Google/Chrome/Application/chrome.exe'),
    path.join(pf86, 'Microsoft/Edge/Application/msedge.exe'),
    path.join(pf, 'Microsoft/Edge/Application/msedge.exe'),
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
    '/usr/bin/google-chrome', '/usr/bin/chromium', '/usr/bin/chromium-browser', '/usr/bin/microsoft-edge',
  ];
  const hit = candidates.find((c) => c && fs.existsSync(c));
  if (!hit) die('no Chrome/Edge found — pass --chrome PATH or set CHROME_PATH');
  return hit;
}

// -------------------------------------------------------------- server
// Serves ROOT and accepts POST /save?path=rel with a data-URL/base64 body.
function startServer(root) {
  const MIME = {
    '.html': 'text/html; charset=utf-8', '.js': 'application/javascript',
    '.png': 'image/png', '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg',
    '.webp': 'image/webp', '.avif': 'image/avif', '.gif': 'image/gif', '.bmp': 'image/bmp',
  };
  const server = http.createServer((req, res) => {
    const url = new URL(req.url, 'http://x');
    if (req.method === 'POST' && url.pathname === '/save') {
      const target = path.resolve(root, url.searchParams.get('path') || '');
      if (!target.startsWith(root + path.sep)) { res.writeHead(403); return res.end('forbidden'); }
      const chunks = [];
      req.on('data', (c) => chunks.push(c));
      req.on('end', () => {
        try {
          let body = Buffer.concat(chunks).toString('utf8');
          if (body.startsWith('data:')) body = body.slice(body.indexOf(',') + 1);
          fs.mkdirSync(path.dirname(target), { recursive: true });
          fs.writeFileSync(target, Buffer.from(body, 'base64'));
          res.writeHead(200); res.end('ok');
        } catch (e) { res.writeHead(500); res.end(String(e)); }
      });
      return;
    }
    let rel = decodeURIComponent(url.pathname);
    if (rel === '/') rel = '/index.html';
    const file = path.resolve(root, '.' + rel);
    if (!file.startsWith(root + path.sep)) { res.writeHead(403); return res.end(); }
    fs.readFile(file, (err, data) => {
      if (err) { res.writeHead(404); return res.end('not found'); }
      res.writeHead(200, {
        'Content-Type': MIME[path.extname(file).toLowerCase()] || 'application/octet-stream',
        'Cache-Control': 'no-store',
      });
      res.end(data);
    });
  });
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server)));
}

// ------------------------------------------------------------- browser
async function launchBrowser(bin, startUrl) {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'charcoal-profile-'));
  const proc = spawn(bin, [
    '--headless=new', '--remote-debugging-port=0', `--user-data-dir=${profile}`,
    '--no-first-run', '--no-default-browser-check', '--disable-extensions',
    // Recording is long-running in an invisible page — keep timers at full speed.
    '--disable-background-timer-throttling', '--disable-renderer-backgrounding',
    '--disable-backgrounding-occluded-windows',
    startUrl,
  ], { stdio: ['ignore', 'ignore', 'pipe'] });

  const wsUrl = await new Promise((resolve, reject) => {
    let buf = '';
    const t = setTimeout(() => reject(new Error('browser did not report a DevTools port')), 30000);
    proc.stderr.on('data', (d) => {
      buf += d.toString();
      const m = buf.match(/DevTools listening on (ws:\/\/\S+)/);
      if (m) { clearTimeout(t); resolve(m[1]); }
    });
    proc.on('exit', (code) => reject(new Error(`browser exited early (code ${code})`)));
  });
  const port = new URL(wsUrl).port;

  // Find the page target that loaded startUrl.
  let pageWs = null;
  for (let i = 0; i < 50 && !pageWs; i++) {
    const list = await fetch(`http://127.0.0.1:${port}/json/list`).then((r) => r.json()).catch(() => []);
    const page = list.find((t) => t.type === 'page' && t.url.startsWith(startUrl.split('?')[0]));
    if (page) pageWs = page.webSocketDebuggerUrl;
    else await sleep(200);
  }
  if (!pageWs) throw new Error('could not find the render page in the browser');

  const ws = new WebSocket(pageWs);
  await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
  let id = 0;
  const pending = new Map();
  ws.onmessage = (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id && pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id); }
  };
  const evaluate = (expression) => new Promise((resolve) => {
    const myId = ++id;
    pending.set(myId, (msg) => resolve(msg.result?.result?.value));
    ws.send(JSON.stringify({ id: myId, method: 'Runtime.evaluate', params: { expression, returnByValue: true } }));
  });

  const close = () => {
    try { ws.close(); } catch {}
    try { proc.kill(); } catch {}
    setTimeout(() => fs.rmSync(profile, { recursive: true, force: true }), 1500);
  };
  return { evaluate, close };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Run one page job (mode=still or mode=record) to completion.
async function runJob(bin, port, query, timeoutMin) {
  const url = `http://127.0.0.1:${port}/index.html?${query}`;
  const b = await launchBrowser(bin, url);
  const deadline = Date.now() + timeoutMin * 60000;
  let lastProgress = '';
  try {
    while (Date.now() < deadline) {
      const state = await b.evaluate(
        'JSON.stringify({done: !!window.__charcoalDone, err: window.__charcoalError || null, p: window.__charcoalProgress || null})'
      );
      if (state) {
        const s = JSON.parse(state);
        if (s.err) throw new Error(s.err);
        if (s.done) return;
        if (s.p && s.p !== lastProgress) {
          lastProgress = s.p;
          const [a, n] = s.p.split('/').map(Number);
          if (a === n || a % Math.max(1, Math.round(n / 10)) === 0) log(`  frames ${s.p}`);
        }
      }
      await sleep(500);
    }
    throw new Error(`timed out after ${timeoutMin} min`);
  } finally {
    b.close();
  }
}

function ffmpeg(args) {
  const r = spawnSync('ffmpeg', ['-v', 'error', '-y', ...args], { stdio: ['ignore', 'inherit', 'inherit'] });
  if (r.error) die(`ffmpeg not runnable (${r.error.message}) — install it or drop gif/mp4 from --formats`);
  if (r.status !== 0) die(`ffmpeg failed (exit ${r.status})`);
}

// ---------------------------------------------------------------- main
async function main() {
  const o = parseArgs(process.argv.slice(2));
  if (o.help || !o.image) {
    console.log(fs.readFileSync(fileURLToPath(import.meta.url), 'utf8').split('\n').slice(1, 30).map((l) => l.replace(/^\/\/ ?/, '')).join('\n'));
    process.exit(o.image ? 0 : 1);
  }
  if (typeof WebSocket === 'undefined') die('needs Node ≥ 22 (built-in WebSocket)');
  const image = path.resolve(o.image);
  if (!fs.existsSync(image)) die(`image not found: ${image}`);

  const formats = new Set(o.formats.split(',').map((s) => s.trim()).filter(Boolean));
  for (const f of formats) if (!['still', 'gif', 'mp4'].includes(f)) die(`unknown format ${f}`);

  const base = path.basename(image, path.extname(image)).replace(/[^\w.-]+/g, '_');
  const outDir = path.resolve(o.out || `charcoal-${base}`);
  const work = path.join(outDir, '.work');
  fs.rmSync(work, { recursive: true, force: true });
  fs.mkdirSync(work, { recursive: true });
  const ext = path.extname(image).toLowerCase() || '.png';
  fs.copyFileSync(path.join(HERE, 'index.html'), path.join(work, 'index.html'));
  fs.copyFileSync(image, path.join(work, 'input' + ext));

  const bin = findBrowser(o.chrome);
  const server = await startServer(work);
  const port = server.address().port;
  log(`browser: ${bin}`);
  log(`output:  ${outDir}`);

  const q = new URLSearchParams({
    src: 'input' + ext, palette: o.palette, cell: String(o.cell),
    maxdim: String(o.maxdim), duration: String(o.duration),
  });
  if (!['all', 'edge', 'off'].includes(o.dust)) die(`--dust must be all, edge, or off`);
  if (o.dust !== 'all') q.set('dust', o.dust);

  const t0 = Date.now();
  const produced = [];
  try {
    if (formats.has('still')) {
      log('rendering still…');
      await runJob(bin, port, `${q}&mode=still`, o.timeout);
      const dst = path.join(outDir, `${base}.png`);
      fs.copyFileSync(path.join(work, 'still.png'), dst);
      produced.push(dst);
    }

    if (formats.has('gif') || formats.has('mp4')) {
      // Record once at the highest needed rate; ffmpeg resamples for the GIF.
      const recFps = formats.has('mp4') ? o.mp4Fps : o.gifFps;
      log(`recording ${Math.round(o.duration * recFps)} frames @ ${recFps}fps…`);
      await runJob(bin, port, `${q}&mode=record&fps=${recFps}`, o.timeout);
      const pattern = path.join(work, 'frames', 'frame_%04d.png');

      if (formats.has('gif')) {
        const dst = path.join(outDir, `${base}.gif`);
        log('stitching GIF…');
        ffmpeg(['-framerate', String(recFps), '-i', pattern, '-filter_complex',
          `fps=${o.gifFps},scale=${o.gifWidth}:-1:flags=lanczos,split[s0][s1];` +
          // Small palettes skip dithering: the art is two-tone, and dither noise
          // that changes every frame is what bloats animated GIFs.
          `[s0]palettegen=max_colors=${o.gifColors}[p];[s1][p]paletteuse=` +
          (o.gifColors <= 32 ? 'dither=none:diff_mode=rectangle' : 'dither=bayer:bayer_scale=5'),
          '-loop', '0', dst]);
        produced.push(dst);
      }
      if (formats.has('mp4')) {
        const dst = path.join(outDir, `${base}.mp4`);
        log('encoding MP4…');
        ffmpeg(['-framerate', String(recFps), '-i', pattern,
          '-vf', 'scale=trunc(iw/2)*2:trunc(ih/2)*2', '-c:v', 'libx264',
          '-pix_fmt', 'yuv420p', '-crf', '18', '-movflags', '+faststart', dst]);
        produced.push(dst);
      }
    }
  } finally {
    server.close();
  }

  if (o.keepFrames) log(`work dir kept: ${work}`);
  else fs.rmSync(work, { recursive: true, force: true });

  log(`done in ${((Date.now() - t0) / 1000).toFixed(0)}s`);
  for (const p of produced) log(`  ${p}  (${(fs.statSync(p).size / 1048576).toFixed(2)} MB)`);
}

main().catch((e) => die(e.message || String(e)));
