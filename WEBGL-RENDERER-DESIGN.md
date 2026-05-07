# WebGL Renderer — Design Notes for v0.5+

> **Status:** Design only. Not implemented. After commits `8682c5e`, `9f0143d`, `89b660d` plus duotone/gradient sprite atlas extensions, the canvas2D path hits ~11 ms in brightness mode and ~42 ms in shape-edge-aware mode at 240×120. WebGL would push this toward ~5 ms total — useful for very high cell counts (480×240 = 115,200 cells) or 60 fps requirements, but not urgent for current production targets.

## Why WebGL

Canvas2D's `drawImage` blit from an offscreen sprite atlas is fast (~0.5 µs per call) but still costs ~7 ms at 28,800 cells because every call is a separate driver round-trip. WebGL2's instanced draw collapses all 28,800 cells into a single `gl.drawArraysInstanced` call. Sub-millisecond grid render is achievable on M-series macs (see beamterm / the WebGL fundamentals atlas tutorial for prior art, both linked below).

## Architecture

### Three GPU resources

1. **Glyph texture atlas** — `WebGL2RenderingContext.TEXTURE_2D_ARRAY`. One layer per glyph; each layer is a `tileW × tileH` raster. Built once on atlas load by uploading the existing `_spriteAtlas.canvas` (from commit `8682c5e`) chopped into layers. `gl.texSubImage3D` per layer.

2. **Static unit-quad vertex buffer** — 4 vertices defining a `(0..1, 0..1)` quad. Drawn as `TRIANGLE_STRIP`. Shared across every cell.

3. **Per-instance buffer** (uploaded per frame) — `Float32Array` packing
   - 2 floats: cell position in normalized canvas space
   - 1 float: glyph index (cast to int in shader)
   - 4 floats: rgba foreground colour (rgb encodes the per-cell colour after monochrome/duotone/gradient resolution; a holds cell brightness modulation)
   = 7 floats × 28,800 cells = 806,400 bytes uploaded per frame. `bufferSubData` with `gl.DYNAMIC_DRAW` keeps the GPU's allocation stable across frames.

### Vertex shader

```glsl
#version 300 es
in vec2 a_quadPos;          // 0..1 unit quad
in vec2 a_cellPos;          // normalized canvas coords, per-instance
in float a_glyphIdx;        // per-instance
in vec4 a_color;            // per-instance

uniform vec2 u_cellSize;    // tileW/canvasW, tileH/canvasH

out vec2 v_uv;
out float v_glyphIdx;
out vec4 v_color;

void main() {
  vec2 ndc = (a_cellPos + a_quadPos * u_cellSize) * 2.0 - 1.0;
  ndc.y = -ndc.y;
  gl_Position = vec4(ndc, 0.0, 1.0);
  v_uv = a_quadPos;
  v_glyphIdx = a_glyphIdx;
  v_color = a_color;
}
```

### Fragment shader

```glsl
#version 300 es
precision highp float;
precision highp sampler2DArray;

in vec2 v_uv;
in float v_glyphIdx;
in vec4 v_color;

uniform sampler2DArray u_atlas;

out vec4 fragColor;

void main() {
  vec4 g = texture(u_atlas, vec3(v_uv, v_glyphIdx));
  // Atlas tiles are white-on-transparent. v_color.rgb is the desired
  // foreground; v_color.a is the per-cell alpha modulation.
  fragColor = vec4(v_color.rgb, g.a * v_color.a);
}
```

### Per-frame CPU work

For each cell: pack position, glyph index, and resolved RGBA into the instance buffer. This is the loop that currently calls `drawImage` 28,800 times — moved to a single `Float32Array` write loop instead. Estimated ~3–5 ms at 240×120.

### Single draw call

```js
gl.bindVertexArray(vao);
gl.activeTexture(gl.TEXTURE0);
gl.bindTexture(gl.TEXTURE_2D_ARRAY, atlasTex);
gl.bufferSubData(gl.ARRAY_BUFFER, 0, instanceData);
gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, totalCells);
```

## Integration with existing pipeline

| Existing stage | Stays / Changes |
|---|---|
| `drawGlyphGridV2` driver | unchanged — chooses `drawBrightnessGrid` / `drawShapeGrid` / `drawEdgeDirectionalGrid` |
| `selectGrid` (k-d tree NN) | unchanged — computes `indices[]` per frame |
| `_ensureSpriteAtlas` / `_ensureRampSprite` etc | reused — feeds the WebGL texture array uploader |
| `applyPostprocess` Phase A (imgData) | unchanged — `getImageData` still works on a WebGL canvas (but more expensive) |
| `applyPostprocess` Phase B (vignette/letterbox overlay) | unchanged — `globalCompositeOperation = 'multiply'` still works on top of a WebGL canvas |
| Recording (`canvas.toBlob`) | unchanged — `toBlob` works on a WebGL canvas but requires `preserveDrawingBuffer: true` in context creation |

## Risk: postprocess interaction

`getImageData` on a WebGL canvas is much slower than on a CPU-backed canvas2D (full GPU readback every call). With the postproc overlay refactor (commit `9f0143d`), the default vignette-only config no longer calls `getImageData` — Phase A is skipped. But enabling bloom/halation/scanlines forces Phase A and the readback would be 50+ ms.

**Mitigation:** the existing canvas2D path stays as a fallback. Add `CONFIG.renderer = 'webgl' | 'cpu'` (already mentioned in the comment header at `src/index.html:101`). Default to `'cpu'`. Users who want max throughput in monochrome / duotone / gradient + vignette only opt into `'webgl'`.

## Implementation phases

1. **Spike** — feature-flagged WebGL path that handles the monochrome shape mode only. Validate single-draw rendering at 240×120, measure frame time, compare visual output against canvas2D path.
2. **Color mode coverage** — extend per-instance attributes to handle duotone (lerp in shader using two ink uniforms) and gradient (sample a 1-D texture LUT in fragment shader instead of per-instance RGB).
3. **Selection mode coverage** — both `drawBrightnessGrid` and `drawShapeGrid` need WebGL paths. They share the same per-cell instance pack, just differ in which atlas they bind.
4. **Postproc fallback** — when any imgData stage is enabled, do the WebGL pass into a hidden FBO, then `gl.readPixels` once into the shared imgData, then run `applyChain`, then upload back. Avoid the per-cell readback.
5. **Recording compatibility** — set `preserveDrawingBuffer: true` so `canvas.toBlob` returns the rendered frame without losing it on the next swap.

Estimated effort: ~3–5 days for a working v0.5 spike covering all four colorModes + brightness/shape modes + recording.

## When NOT to do this

If the user is mostly running 240×120 grids at 30 fps target — current canvas2D performance is already there and WebGL adds complexity (shader bugs, GPU driver interactions, debug story is harder). Defer until either:
- Target grid density rises to 480×240+ (current cpu path will hit 40+ ms grid stage at that density).
- 60 fps becomes a hard requirement.
- A user reports the cpu path doesn't keep up on their hardware.

## Sources

- WebGL fundamentals — *Glyph Texture Atlas* — single-buffer-update, per-string draw approach. https://webglfundamentals.org/webgl/lessons/webgl-text-glyphs.html
- beamterm — sub-millisecond terminal grid rendering with WebGL2 + texture array + 4-int instance attributes. The model we should mirror most closely. https://github.com/junkdog/beamterm
- *Techniques for Rendering Text with WebGL* — CSS Tricks overview of bitmap, SDF, and MSDF approaches. https://css-tricks.com/techniques-for-rendering-text-with-webgl/
- Red Blob Games — *SDF+MSDF Fonts* — when to use signed distance fields (zoom-friendly) vs simple bitmap atlas (fixed size, simpler). For our fixed-cell grid, plain bitmap atlas is the right call. https://www.redblobgames.com/articles/sdf-fonts/
