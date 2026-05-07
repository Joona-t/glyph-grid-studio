/* glyph-shape-index.js — shape-vector glyph selection (Harri 2024, unified
   with directional contrast per CR-10).

   Given a glyph set JSON (glyph-sets/<name>.json), build a Float32Array
   atlas of 6D vectors (N × 6). At runtime, for each source cell, compute a
   6D shape vector using external samples (reach outside the cell by up to
   1 cell radius to capture edge directionality) and find the nearest glyph
   by squared Euclidean distance in 6D.

   Brute-force search is fast in V8 at n=~256, k=6 (CR-3). We implement that
   first; k-d tree can be added later if profile proves it's needed.

   Input source buffer: Uint8ClampedArray or Uint8Array of RGBA pixels from
   the p5 source p5.Graphics, read via `get()` into an ImageData. We work on
   a single-channel "linear luminance" image of the same dims, computed by
   the caller (see references/pipeline.md). This lib does NOT do the
   linearization — keep it pure and reusable.

   API:
     const atlas = GlyphGrid.shapeIndex.buildAtlas(setJson);
     const idx = GlyphGrid.shapeIndex.select(atlas, vec6);
     const vec = GlyphGrid.shapeIndex.cellVector(
                    luminance, srcW, srcH, cellX, cellY, cellW, cellH);

   All distances are in 6D. Quadrant ink weights dominate; horizontal/
   vertical symmetry are bounded by construction ([0,1]) so the scale
   matches and no renormalization is required.
*/

(function () {
  'use strict';

  /* Build a Float32Array atlas of [N * 6] values from a glyph-set JSON.
     Returns { size: N, vecs: Float32Array, glyphs: Array<{cp,s,ink}> }.
     Glyphs without a shape vector are rejected. */
  function buildAtlas(setJson) {
    if (!setJson || !Array.isArray(setJson.glyphs)) {
      throw new Error('glyph-shape-index: invalid set JSON');
    }
    const gs = setJson.glyphs;
    const N = gs.length;
    const vecs = new Float32Array(N * 6);
    const meta = new Array(N);
    for (let i = 0; i < N; i++) {
      const g = gs[i];
      if (!g.vec || g.vec.length !== 6) {
        throw new Error('glyph-shape-index: glyph cp=' + g.cp + ' missing 6D vec');
      }
      const base = i * 6;
      for (let j = 0; j < 6; j++) vecs[base + j] = g.vec[j];
      meta[i] = { cp: g.cp, s: g.s, ink: g.ink };
    }
    const atlas = {
      name: setJson.name,
      size: N,
      cellW: setJson.cellW,
      cellH: setJson.cellH,
      vecs: vecs,
      glyphs: meta,
      tree: null,
    };
    /* Build the k-d tree once at atlas-load time so selectGrid can do
       log-N descent instead of brute force on every cell every frame.
       For octant (~230 glyphs) at 240×120 cells this drops the per-frame
       NN cost from 6.6M comparisons to ~230K (~28× fewer). */
    atlas.tree = buildKDTree(atlas);
    return atlas;
  }

  /* Compute the 6D shape vector for one cell of a source luminance image.
     luminance is a Float32Array (or Uint8Array) of length srcW*srcH in [0,1]
     or [0,255]. cellX,cellY are cell indices; cellW,cellH are pixels per cell.

     External samples: we reach 1/4 cell beyond the cell edges on each side to
     integrate edge-directional signal (Harri's approach). The outer band
     contributes to the quadrant averages with a 0.5 weight so in-cell shape
     still dominates. */
  function cellVector(luminance, srcW, srcH, cellX, cellY, cellW, cellH, opts) {
    opts = opts || {};
    const reach = opts.reach == null ? Math.floor(Math.min(cellW, cellH) / 4) : opts.reach;
    const reachWeight = opts.reachWeight == null ? 0.5 : opts.reachWeight;
    const scale = opts.luminanceScale || (luminance instanceof Uint8Array || luminance instanceof Uint8ClampedArray ? 1 / 255 : 1);

    const x0 = cellX * cellW;
    const y0 = cellY * cellH;
    const hw = cellW / 2;
    const hh = cellH / 2;

    let tl = 0, tr = 0, bl = 0, br = 0;
    let tlN = 0, trN = 0, blN = 0, brN = 0;

    /* Main cell: weight 1. */
    for (let dy = 0; dy < cellH; dy++) {
      const py = y0 + dy;
      if (py < 0 || py >= srcH) continue;
      const row = py * srcW;
      for (let dx = 0; dx < cellW; dx++) {
        const px = x0 + dx;
        if (px < 0 || px >= srcW) continue;
        const v = luminance[row + px] * scale;
        if (dy < hh) {
          if (dx < hw) { tl += v; tlN++; }
          else         { tr += v; trN++; }
        } else {
          if (dx < hw) { bl += v; blN++; }
          else         { br += v; brN++; }
        }
      }
    }

    /* Outer band (reach beyond cell on all four sides), weighted. */
    if (reach > 0 && reachWeight > 0) {
      /* Top and bottom horizontal bands. */
      for (let dy = -reach; dy < cellH + reach; dy++) {
        const py = y0 + dy;
        if (py < 0 || py >= srcH) continue;
        const row = py * srcW;
        const inMainY = (dy >= 0 && dy < cellH);
        /* Left band. */
        for (let dx = -reach; dx < 0; dx++) {
          const px = x0 + dx;
          if (px < 0 || px >= srcW) continue;
          const v = luminance[row + px] * scale * reachWeight;
          const inTop = dy < hh;
          if (inTop) { tl += v; tlN += reachWeight; }
          else       { bl += v; blN += reachWeight; }
        }
        /* Right band. */
        for (let dx = cellW; dx < cellW + reach; dx++) {
          const px = x0 + dx;
          if (px < 0 || px >= srcW) continue;
          const v = luminance[row + px] * scale * reachWeight;
          const inTop = dy < hh;
          if (inTop) { tr += v; trN += reachWeight; }
          else       { br += v; brN += reachWeight; }
        }
        if (inMainY) continue;
        /* Top or bottom main-x band. */
        for (let dx = 0; dx < cellW; dx++) {
          const px = x0 + dx;
          if (px < 0 || px >= srcW) continue;
          const v = luminance[row + px] * scale * reachWeight;
          if (dy < 0) {
            /* above cell — top quadrants */
            if (dx < hw) { tl += v; tlN += reachWeight; }
            else         { tr += v; trN += reachWeight; }
          } else {
            /* below cell */
            if (dx < hw) { bl += v; blN += reachWeight; }
            else         { br += v; brN += reachWeight; }
          }
        }
      }
    }

    const tlf = tlN > 0 ? tl / tlN : 0;
    const trf = trN > 0 ? tr / trN : 0;
    const blf = blN > 0 ? bl / blN : 0;
    const brf = brN > 0 ? br / brN : 0;

    const top = tlf + trf;
    const bot = blf + brf;
    const left = tlf + blf;
    const right = trf + brf;
    const eps = 1e-6;
    const h_sym = 1 - Math.abs(top - bot) / Math.max(top, bot, eps);
    const v_sym = 1 - Math.abs(left - right) / Math.max(left, right, eps);

    return [tlf, trf, blf, brf, Math.max(0, Math.min(1, h_sym)), Math.max(0, Math.min(1, v_sym))];
  }

  /* Zero-alloc variant of cellVector: writes the 6 components into
     `out[outOff..outOff+5]` instead of returning a fresh array.
     Used by selectGrid's hot loop to eliminate ~28,800 array
     allocations per frame at 240×120 (one per cell). */
  function cellVectorInto(out, outOff, luminance, srcW, srcH, cellX, cellY, cellW, cellH, opts) {
    opts = opts || {};
    const reach = opts.reach == null ? Math.floor(Math.min(cellW, cellH) / 4) : opts.reach;
    const reachWeight = opts.reachWeight == null ? 0.5 : opts.reachWeight;
    const scale = opts.luminanceScale || (luminance instanceof Uint8Array || luminance instanceof Uint8ClampedArray ? 1 / 255 : 1);

    const x0 = cellX * cellW;
    const y0 = cellY * cellH;
    const hw = cellW / 2;
    const hh = cellH / 2;

    let tl = 0, tr = 0, bl = 0, br = 0;
    let tlN = 0, trN = 0, blN = 0, brN = 0;

    for (let dy = 0; dy < cellH; dy++) {
      const py = y0 + dy;
      if (py < 0 || py >= srcH) continue;
      const row = py * srcW;
      for (let dx = 0; dx < cellW; dx++) {
        const px = x0 + dx;
        if (px < 0 || px >= srcW) continue;
        const v = luminance[row + px] * scale;
        if (dy < hh) {
          if (dx < hw) { tl += v; tlN++; }
          else         { tr += v; trN++; }
        } else {
          if (dx < hw) { bl += v; blN++; }
          else         { br += v; brN++; }
        }
      }
    }

    if (reach > 0 && reachWeight > 0) {
      for (let dy = -reach; dy < cellH + reach; dy++) {
        const py = y0 + dy;
        if (py < 0 || py >= srcH) continue;
        const row = py * srcW;
        const inMainY = (dy >= 0 && dy < cellH);
        for (let dx = -reach; dx < 0; dx++) {
          const px = x0 + dx;
          if (px < 0 || px >= srcW) continue;
          const v = luminance[row + px] * scale * reachWeight;
          const inTop = dy < hh;
          if (inTop) { tl += v; tlN += reachWeight; }
          else       { bl += v; blN += reachWeight; }
        }
        for (let dx = cellW; dx < cellW + reach; dx++) {
          const px = x0 + dx;
          if (px < 0 || px >= srcW) continue;
          const v = luminance[row + px] * scale * reachWeight;
          const inTop = dy < hh;
          if (inTop) { tr += v; trN += reachWeight; }
          else       { br += v; brN += reachWeight; }
        }
        if (inMainY) continue;
        for (let dx = 0; dx < cellW; dx++) {
          const px = x0 + dx;
          if (px < 0 || px >= srcW) continue;
          const v = luminance[row + px] * scale * reachWeight;
          if (dy < 0) {
            if (dx < hw) { tl += v; tlN += reachWeight; }
            else         { tr += v; trN += reachWeight; }
          } else {
            if (dx < hw) { bl += v; blN += reachWeight; }
            else         { br += v; brN += reachWeight; }
          }
        }
      }
    }

    const tlf = tlN > 0 ? tl / tlN : 0;
    const trf = trN > 0 ? tr / trN : 0;
    const blf = blN > 0 ? bl / blN : 0;
    const brf = brN > 0 ? br / brN : 0;

    const top = tlf + trf;
    const bot = blf + brf;
    const left = tlf + blf;
    const right = trf + brf;
    const eps = 1e-6;
    let h_sym = 1 - Math.abs(top - bot) / Math.max(top, bot, eps);
    let v_sym = 1 - Math.abs(left - right) / Math.max(left, right, eps);
    h_sym = h_sym < 0 ? 0 : (h_sym > 1 ? 1 : h_sym);
    v_sym = v_sym < 0 ? 0 : (v_sym > 1 ? 1 : v_sym);

    out[outOff]     = tlf;
    out[outOff + 1] = trf;
    out[outOff + 2] = blf;
    out[outOff + 3] = brf;
    out[outOff + 4] = h_sym;
    out[outOff + 5] = v_sym;
  }

  /* Select the glyph in the atlas closest to `vec` (6-element array or typed
     array). Returns the glyph index. Brute-force squared-distance. */
  function select(atlas, vec) {
    const vecs = atlas.vecs;
    const N = atlas.size;
    const v0 = vec[0], v1 = vec[1], v2 = vec[2], v3 = vec[3], v4 = vec[4], v5 = vec[5];
    let best = 0;
    let bestD = Infinity;
    for (let i = 0; i < N; i++) {
      const b = i * 6;
      const d0 = vecs[b]     - v0;
      const d1 = vecs[b + 1] - v1;
      const d2 = vecs[b + 2] - v2;
      const d3 = vecs[b + 3] - v3;
      const d4 = vecs[b + 4] - v4;
      const d5 = vecs[b + 5] - v5;
      const d = d0*d0 + d1*d1 + d2*d2 + d3*d3 + d4*d4 + d5*d5;
      if (d < bestD) { bestD = d; best = i; }
    }
    return best;
  }

  /* Batched select — one pass over the source grid, one output index per cell.
     outIndices is a Uint16Array of length cols*rows. Returns outIndices. */
  function selectAll(atlas, vectors, outIndices) {
    const N = atlas.size;
    const vecs = atlas.vecs;
    const count = vectors.length / 6;
    if (!outIndices || outIndices.length < count) outIndices = new Uint16Array(count);
    for (let c = 0; c < count; c++) {
      const vb = c * 6;
      const v0 = vectors[vb], v1 = vectors[vb+1], v2 = vectors[vb+2],
            v3 = vectors[vb+3], v4 = vectors[vb+4], v5 = vectors[vb+5];
      let best = 0;
      let bestD = Infinity;
      for (let i = 0; i < N; i++) {
        const b = i * 6;
        const d0 = vecs[b]     - v0;
        const d1 = vecs[b + 1] - v1;
        const d2 = vecs[b + 2] - v2;
        const d3 = vecs[b + 3] - v3;
        const d4 = vecs[b + 4] - v4;
        const d5 = vecs[b + 5] - v5;
        const d = d0*d0 + d1*d1 + d2*d2 + d3*d3 + d4*d4 + d5*d5;
        if (d < bestD) { bestD = d; best = i; }
      }
      outIndices[c] = best;
    }
    return outIndices;
  }

  /* Persistent buffers reused across frames (sized to match the current
     grid; resized only when cols*rows changes).  Pre-this-fix selectGrid
     allocated `new Float32Array(count*6)` (~691 KB at 240×120) and
     `new Uint16Array(count)` (~57 KB) every single frame, plus a
     6-element array per cell from cellVector (28,800 allocs/frame). */
  let _selectGridVecs = null;
  let _selectGridIdx = null;
  function _ensureBuffers(count) {
    if (!_selectGridVecs || _selectGridVecs.length < count * 6) {
      _selectGridVecs = new Float32Array(count * 6);
    }
    if (!_selectGridIdx || _selectGridIdx.length < count) {
      _selectGridIdx = new Uint16Array(count);
    }
  }

  /* Convenience: compute 6D vectors for every cell in the source and
     run NN search.  Uses the atlas's pre-built k-d tree when available
     (every atlas built via buildAtlas has one), falling back to brute
     force for unusual atlas shapes. */
  function selectGrid(atlas, luminance, srcW, srcH, cols, rows, cellW, cellH, opts) {
    const count = cols * rows;
    _ensureBuffers(count);
    const vectors = _selectGridVecs;
    const indices = _selectGridIdx;
    for (let y = 0; y < rows; y++) {
      const rowOff = y * cols * 6;
      for (let x = 0; x < cols; x++) {
        cellVectorInto(vectors, rowOff + x * 6, luminance, srcW, srcH, x, y, cellW, cellH, opts);
      }
    }
    if (atlas.tree) {
      return selectAllKDTree(atlas.tree, vectors, indices);
    }
    return selectAll(atlas, vectors, indices);
  }

  /* Stage 2D — k-d tree NN search over the 6-D shape-vector atlas
     (Chen et al. arXiv 2503.14375, 2025: classical k-NN matches CNN
     quality at ~1% the cost). For a single atlas (octant ≈ 230 glyphs)
     brute force is plenty fast, but combined sets (octant + sextant +
     braille ≈ 660+ glyphs) benefit from log-N descent. The tree is
     built once on atlas load and reused for every cell every frame.

     Layout: Int32Array `nodes`, four entries per node:
       nodes[off + 0] = atlas point index
       nodes[off + 1] = split axis (0..5)
       nodes[off + 2] = left child offset, or -1 for absent
       nodes[off + 3] = right child offset, or -1 for absent
     Root is at offset 0.  Each node occupies KD_NODE_SIZE = 4 ints (16
     bytes).  For N=256 nodes total = 4 KB — fits comfortably in L1.

     Search is iterative with an explicit stack (Int32Array, reused across
     frames) — no recursion, no closure.  The simple "push far first,
     near second; pop near first" LIFO works for kd-NN: pruning at push
     time is conservative (uses the bestD known when the parent is
     visited; bestD may shrink during near's traversal, making far
     visits sometimes wasteful but never incorrect). */
  const KD_DIM = 6;
  const KD_NODE_SIZE = 4;
  const KD_STACK_MAX = 128;            // max(2 × tree depth) — for N=256 depth is ~9, so 18; 128 is plenty.

  function buildKDTree(atlas) {
    const N = atlas.size;
    if (N === 0) return null;
    const vecs = atlas.vecs;
    const nodes = new Int32Array(N * KD_NODE_SIZE);
    const ids = new Array(N);
    for (let i = 0; i < N; i++) ids[i] = i;
    let nextOff = 0;

    function build(lo, hi, depth) {
      if (lo >= hi) return -1;
      const axis = depth % KD_DIM;
      /* Sort the slice on this axis. N small per split — JS sort is fine. */
      const slice = ids.slice(lo, hi);
      slice.sort(function (a, b) { return vecs[a * KD_DIM + axis] - vecs[b * KD_DIM + axis]; });
      for (let k = 0; k < slice.length; k++) ids[lo + k] = slice[k];
      const m = (lo + hi) >> 1;
      const myOff = nextOff;
      nextOff += KD_NODE_SIZE;
      const leftOff = build(lo, m, depth + 1);
      const rightOff = build(m + 1, hi, depth + 1);
      nodes[myOff]     = ids[m];
      nodes[myOff + 1] = axis;
      nodes[myOff + 2] = leftOff;
      nodes[myOff + 3] = rightOff;
      return myOff;
    }
    build(0, N, 0);
    return { nodes: nodes, atlas: atlas };
  }

  /* Persistent traversal stack — single Int32Array reused for every
     query.  Avoids 28,800 stack allocations per frame at 240×120. */
  const _kdStack = new Int32Array(KD_STACK_MAX);

  /* Single-vec query.  Inlined squared-distance + axis-delta dispatch.
     Returns the atlas index of the nearest glyph. */
  function selectKDTree(tree, vec) {
    const nodes = tree.nodes;
    const vecs = tree.atlas.vecs;
    const v0 = vec[0], v1 = vec[1], v2 = vec[2], v3 = vec[3], v4 = vec[4], v5 = vec[5];
    let best = 0;
    let bestD = Infinity;
    let sp = 0;
    _kdStack[sp++] = 0;
    while (sp > 0) {
      const node = _kdStack[--sp];
      if (node < 0) continue;
      const idx = nodes[node];
      const axis = nodes[node + 1];
      const leftOff = nodes[node + 2];
      const rightOff = nodes[node + 3];
      const ai = idx * KD_DIM;
      const d0 = vecs[ai] - v0;
      const d1 = vecs[ai + 1] - v1;
      const d2 = vecs[ai + 2] - v2;
      const d3 = vecs[ai + 3] - v3;
      const d4 = vecs[ai + 4] - v4;
      const d5 = vecs[ai + 5] - v5;
      const d = d0*d0 + d1*d1 + d2*d2 + d3*d3 + d4*d4 + d5*d5;
      if (d < bestD) { bestD = d; best = idx; }
      let ad;
      switch (axis) {
        case 0: ad = v0 - vecs[ai];     break;
        case 1: ad = v1 - vecs[ai + 1]; break;
        case 2: ad = v2 - vecs[ai + 2]; break;
        case 3: ad = v3 - vecs[ai + 3]; break;
        case 4: ad = v4 - vecs[ai + 4]; break;
        default: ad = v5 - vecs[ai + 5];
      }
      const adSq = ad * ad;
      /* LIFO: push far first (only if it might improve bestD), then near.
         Near pops first, gets visited before far. */
      if (ad < 0) {
        if (rightOff >= 0 && adSq < bestD) _kdStack[sp++] = rightOff;
        if (leftOff >= 0)                  _kdStack[sp++] = leftOff;
      } else {
        if (leftOff >= 0 && adSq < bestD)  _kdStack[sp++] = leftOff;
        if (rightOff >= 0)                 _kdStack[sp++] = rightOff;
      }
    }
    return best;
  }

  /* Batched query — same outer loop as the brute-force selectAll but
     calls selectKDTree per cell.  Reads vector components directly from
     the input typed array (no per-cell tmp array allocation, unlike the
     pre-flatten version which had `const tmp = [0,0,0,0,0,0]` reused
     but still passed an array). */
  function selectAllKDTree(tree, vectors, outIndices) {
    const count = vectors.length / KD_DIM;
    if (!outIndices || outIndices.length < count) outIndices = new Uint16Array(count);
    const nodes = tree.nodes;
    const vecs = tree.atlas.vecs;
    /* Inline the single-vec path so V8 doesn't pay function-call overhead per cell. */
    for (let c = 0; c < count; c++) {
      const b = c * KD_DIM;
      const v0 = vectors[b], v1 = vectors[b+1], v2 = vectors[b+2];
      const v3 = vectors[b+3], v4 = vectors[b+4], v5 = vectors[b+5];
      let best = 0;
      let bestD = Infinity;
      let sp = 0;
      _kdStack[sp++] = 0;
      while (sp > 0) {
        const node = _kdStack[--sp];
        if (node < 0) continue;
        const idx = nodes[node];
        const axis = nodes[node + 1];
        const leftOff = nodes[node + 2];
        const rightOff = nodes[node + 3];
        const ai = idx * KD_DIM;
        const d0 = vecs[ai] - v0;
        const d1 = vecs[ai + 1] - v1;
        const d2 = vecs[ai + 2] - v2;
        const d3 = vecs[ai + 3] - v3;
        const d4 = vecs[ai + 4] - v4;
        const d5 = vecs[ai + 5] - v5;
        const d = d0*d0 + d1*d1 + d2*d2 + d3*d3 + d4*d4 + d5*d5;
        if (d < bestD) { bestD = d; best = idx; }
        let ad;
        switch (axis) {
          case 0: ad = v0 - vecs[ai];     break;
          case 1: ad = v1 - vecs[ai + 1]; break;
          case 2: ad = v2 - vecs[ai + 2]; break;
          case 3: ad = v3 - vecs[ai + 3]; break;
          case 4: ad = v4 - vecs[ai + 4]; break;
          default: ad = v5 - vecs[ai + 5];
        }
        const adSq = ad * ad;
        if (ad < 0) {
          if (rightOff >= 0 && adSq < bestD) _kdStack[sp++] = rightOff;
          if (leftOff >= 0)                  _kdStack[sp++] = leftOff;
        } else {
          if (leftOff >= 0 && adSq < bestD)  _kdStack[sp++] = leftOff;
          if (rightOff >= 0)                 _kdStack[sp++] = rightOff;
        }
      }
      outIndices[c] = best;
    }
    return outIndices;
  }

  const api = Object.freeze({
    buildAtlas: buildAtlas,
    cellVector: cellVector,
    select: select,
    selectAll: selectAll,
    selectGrid: selectGrid,
    buildKDTree: buildKDTree,
    selectKDTree: selectKDTree,
    selectAllKDTree: selectAllKDTree,
  });

  const root = (typeof window !== 'undefined') ? window
             : (typeof globalThis !== 'undefined') ? globalThis
             : this;
  root.GlyphGrid = root.GlyphGrid || {};
  root.GlyphGrid.shapeIndex = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})();
