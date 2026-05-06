/* glyph-runtime.js — Wave 1 toolkit skeleton.
 *
 * Provides:
 *   GlyphGrid.runtime.register(axis, name, fn, meta)
 *   GlyphGrid.runtime.pipeline()                        → Pipeline builder
 *   GlyphGrid.runtime.run(pipeline, ctx)                → execute one frame
 *   GlyphGrid.runtime.lint(pipeline)                    → construction-time checks
 *   GlyphGrid.runtime.makeContext({ t, frameIdx, ... }) → FrameContext factory
 *   GlyphGrid.runtime.fromScene(sceneFn, opts)          → legacy scene adapter
 *   GlyphGrid.runtime.assert                            → runtime channel/arg checks
 *   GlyphGrid.GlyphGridError                            → typed error
 *
 * Design decisions per
 *   ~/.claude/plans/glyph-grid-wave1-prior-art.md (synthesis section)
 *   ~/.claude/plans/glyph-grid-types.md
 *
 * Single IIFE, attaches to window.GlyphGrid, works in any <script src=>
 * context. No bundler, no Node runtime dependencies.
 */

(function () {
  'use strict';

  /* ====================================================================
     GlyphGridError — typed exception per charter principle "fail loudly".
     ==================================================================== */

  class GlyphGridError extends Error {
    constructor(code, primitive, axis, detail) {
      const msgLines = [
        `GlyphGridError [${code}]:`,
        primitive ? `  primitive: ${axis ? axis + '.' : ''}${primitive}` : null,
        detail && detail.message ? '  ' + detail.message : null,
      ].filter(Boolean);
      super(msgLines.join('\n'));
      this.name = 'GlyphGridError';
      this.code = code;
      this.primitive = primitive || null;
      this.axis = axis || null;
      this.detail = detail || {};
    }
  }

  /* ====================================================================
     Axes — canonical pipeline ordering (informs pipe() validation).
     ==================================================================== */

  const AXES = Object.freeze([
    'source', 'transform', 'sampling',
    'selection', 'color', 'composition', 'postProcess', 'output',
  ]);
  const AXIS_ORDER = Object.freeze(Object.fromEntries(
    AXES.map((a, i) => [a, i])
  ));

  /* Axis → what output kind it produces. */
  const AXIS_OUTPUT = Object.freeze({
    source:      'field',
    transform:   'field',
    sampling:    'cellSignal',
    selection:   'glyphGrid',    /* Uint16Array of glyph indices per cell */
    color:       'rendered',     /* cells + colors painted to canvas */
    composition: 'rendered',     /* compositional merge of sub-pipelines */
    postProcess: 'rendered',
    output:      'void',
  });

  /* ====================================================================
     SeededRng — xorshift-128 with fork().
     ==================================================================== */

  function hash32(a, b, c, d) {
    let h = 0x811c9dc5 ^ (a | 0);
    h = Math.imul(h ^ (b | 0), 2654435761) >>> 0;
    h = Math.imul(h ^ (c | 0), 1597334677) >>> 0;
    h = Math.imul(h ^ (d | 0), 3266489917) >>> 0;
    h ^= h >>> 16; h = Math.imul(h, 2246822507) >>> 0;
    h ^= h >>> 13; h = Math.imul(h, 3266489909) >>> 0;
    h ^= h >>> 16;
    return h >>> 0;
  }

  function makeSeededRng(seed32) {
    /* xorshift-128 state from one seed via splitmix64-ish spreads. */
    let s0 = hash32(seed32, 0x243f6a88, 0x85a308d3, 0x13198a2e) | 0;
    let s1 = hash32(seed32, 0x03707344, 0xa4093822, 0x299f31d0) | 0;
    let s2 = hash32(seed32, 0x082efa98, 0xec4e6c89, 0x452821e6) | 0;
    let s3 = hash32(seed32, 0x38d01377, 0xbe5466cf, 0x34e90c6c) | 0;
    function nextInt() {
      let t = s0;
      const s = s3;
      t ^= t << 11; t ^= t >>> 8;
      s0 = s1; s1 = s2; s2 = s3;
      s3 ^= s3 >>> 19; s3 ^= t;
      return s3 >>> 0;
    }
    return {
      next: function () { return nextInt() / 4294967296; },
      nextInt: function (n) { return Math.floor((nextInt() / 4294967296) * n); },
      fork: function (salt) { return makeSeededRng(hash32(s0, s1, salt | 0, seed32)); },
    };
  }

  /* ====================================================================
     FieldPool — tagged scratch buffers pre-allocated at setup.

     Primitives declare scratch at registration: meta.scratch = {
       fields: [{ tag, channels }],
       cellSignals: [{ tag, channels }],
     }
     On init(w, h, cols, rows) the pool allocates one buffer per tag.
     acquire(tag) returns the pre-allocated buffer (reused each frame).
     ==================================================================== */

  function makeFieldPool() {
    const fields = Object.create(null);       /* tag → Field */
    const cellSignals = Object.create(null);  /* tag → CellSignal */

    return {
      registerField: function (tag, channels) {
        if (tag in fields) {
          throw new GlyphGridError('REGISTRATION_CONFLICT', null, null, {
            message: 'Field pool tag "' + tag + '" already claimed.',
          });
        }
        fields[tag] = { tag: tag, channels: new Set(channels), buf: null, w: 0, h: 0 };
      },
      registerCellSignal: function (tag, channels) {
        if (tag in cellSignals) {
          throw new GlyphGridError('REGISTRATION_CONFLICT', null, null, {
            message: 'CellSignal pool tag "' + tag + '" already claimed.',
          });
        }
        cellSignals[tag] = { tag: tag, channels: new Set(channels), buf: null, cols: 0, rows: 0 };
      },
      initFields: function (w, h) {
        for (const tag in fields) {
          const slot = fields[tag];
          if (slot.w === w && slot.h === h && slot.buf) continue;
          const bufs = Object.create(null);
          for (const ch of slot.channels) bufs[ch] = new Float32Array(w * h);
          slot.buf = bufs; slot.w = w; slot.h = h;
        }
      },
      initCellSignals: function (cols, rows) {
        for (const tag in cellSignals) {
          const slot = cellSignals[tag];
          if (slot.cols === cols && slot.rows === rows && slot.buf) continue;
          const bufs = Object.create(null);
          for (const ch of slot.channels) bufs[ch] = new Float32Array(cols * rows);
          slot.buf = bufs; slot.cols = cols; slot.rows = rows;
        }
      },
      acquireField: function (tag, w, h) {
        const slot = fields[tag];
        if (!slot) throw new GlyphGridError('POOL_UNKNOWN_TAG', null, null, {
          message: 'No field pool entry for tag "' + tag + '". Declare it at registration.',
        });
        if (slot.w !== w || slot.h !== h || !slot.buf) {
          const bufs = Object.create(null);
          for (const ch of slot.channels) bufs[ch] = new Float32Array(w * h);
          slot.buf = bufs; slot.w = w; slot.h = h;
        }
        return { w: w, h: h, channels: slot.channels, buf: slot.buf, _poolTag: tag };
      },
      acquireCellSignal: function (tag, cols, rows) {
        const slot = cellSignals[tag];
        if (!slot) throw new GlyphGridError('POOL_UNKNOWN_TAG', null, null, {
          message: 'No cellSignal pool entry for tag "' + tag + '". Declare it at registration.',
        });
        if (slot.cols !== cols || slot.rows !== rows || !slot.buf) {
          const bufs = Object.create(null);
          for (const ch of slot.channels) bufs[ch] = new Float32Array(cols * rows);
          slot.buf = bufs; slot.cols = cols; slot.rows = rows;
        }
        return { cols: cols, rows: rows, channels: slot.channels, buf: slot.buf, _poolTag: tag };
      },
      listFieldTags: function () { return Object.keys(fields); },
      listCellTags: function () { return Object.keys(cellSignals); },
    };
  }

  /* ====================================================================
     Registry — plain-object primitive descriptors (hydra-synth pattern).

     Registration surface:
       GlyphGrid.runtime.register(axis, name, fn, meta = {
         label?: string,
         requires?: ChannelName[],
         produces?: ChannelName[],
         mutatesInput?: boolean,
         scratch?: { fields?: [{tag,channels}], cellSignals?: [...] },
         makeState?: (w, h) => unknown,
       })
     ==================================================================== */

  const registry = Object.create(null);
  for (const axis of AXES) registry[axis] = new Map();

  function register(axis, name, fn, meta) {
    if (!(axis in registry)) {
      throw new GlyphGridError('REGISTRATION_ERROR', name, axis, {
        message: 'Unknown axis "' + axis + '". Valid: ' + AXES.join(', '),
      });
    }
    if (typeof fn !== 'function') {
      throw new GlyphGridError('REGISTRATION_ERROR', name, axis, {
        message: 'Registered fn must be a function.',
      });
    }
    if (registry[axis].has(name)) {
      throw new GlyphGridError('REGISTRATION_CONFLICT', name, axis, {
        message: 'Primitive "' + axis + '.' + name + '" is already registered.',
      });
    }
    const reg = Object.freeze({
      axis: axis,
      name: name,
      fn: fn,
      label: (meta && meta.label) || name,
      requires: (meta && meta.requires) ? meta.requires.slice() : [],
      produces: (meta && meta.produces) ? meta.produces.slice() : [],
      mutatesInput: !!(meta && meta.mutatesInput),
      scratch: Object.freeze({
        fields: (meta && meta.scratch && meta.scratch.fields) || [],
        cellSignals: (meta && meta.scratch && meta.scratch.cellSignals) || [],
      }),
      makeState: (meta && meta.makeState) || null,
    });
    registry[axis].set(name, reg);
    return reg;
  }

  function lookup(axis, name) {
    if (!registry[axis]) {
      throw new GlyphGridError('REGISTRATION_ERROR', name, axis, {
        message: 'Unknown axis "' + axis + '".',
      });
    }
    const reg = registry[axis].get(name);
    if (!reg) {
      throw new GlyphGridError('REGISTRATION_ERROR', name, axis, {
        message: 'No primitive registered at "' + axis + '.' + name + '". ' +
                 'Registered: ' + Array.from(registry[axis].keys()).join(', '),
      });
    }
    return reg;
  }

  /* ====================================================================
     Pipeline — immutable builder (returns new Pipeline per chain call).

     Entry: GlyphGrid.runtime.pipeline()
     Chain: .source('name', opts).transform('name', opts).sampling(...).select(...).color(...).post(...).output(...)
     Terminal: last stage whose axis produces non-void is the terminal.
     ==================================================================== */

  function makePipeline(stages) {
    stages = Object.freeze(stages.slice());
    const terminalIdx = stages.length - 1;
    const terminalKind = stages.length
      ? AXIS_OUTPUT[stages[terminalIdx].axis]
      : null;
    const api = {
      stages: stages,
      outputKind: terminalKind,
    };
    for (const axis of AXES) {
      api[axis] = function (name, opts) { return appendStage(stages, axis, name, opts || {}); };
    }
    /* Aliases matching user-facing builder vocabulary. */
    api.select = api.selection;
    api.post = api.postProcess;
    return Object.freeze(api);
  }

  function appendStage(prevStages, axis, name, opts) {
    const reg = lookup(axis, name);
    const prev = prevStages.length ? prevStages[prevStages.length - 1] : null;
    if (prev) {
      const prevOrder = AXIS_ORDER[prev.axis];
      const thisOrder = AXIS_ORDER[axis];
      /* Allow same-axis (chain of transforms or post-processes) or forward. */
      if (thisOrder < prevOrder) {
        throw new GlyphGridError('TYPE_MISMATCH', name, axis, {
          message: 'Stage ' + axis + '.' + name + ' cannot follow ' +
                   prev.axis + '.' + prev.name + '. ' +
                   'Canonical axis order: ' + AXES.join(' → ') + '.',
        });
      }
    }
    const stage = Object.freeze({
      axis: axis,
      name: name,
      fn: reg.fn,
      reg: reg,
      opts: Object.freeze(Object.assign({}, opts)),
    });
    return makePipeline(prevStages.concat([stage]));
  }

  function pipeline() {
    return makePipeline([]);
  }

  /* ====================================================================
     Lint — offline checks (construction-time diagnostics).
     ==================================================================== */

  function lint(pl) {
    const issues = [];
    if (!pl || !pl.stages || !pl.stages.length) {
      issues.push({ severity: 'warn', code: 'EMPTY_PIPELINE',
        message: 'Pipeline has no stages.' });
      return issues;
    }
    /* Track what channels are produced as we walk forward. */
    const produced = new Set();
    for (let i = 0; i < pl.stages.length; i++) {
      const st = pl.stages[i];
      const reg = st.reg;
      for (const req of reg.requires) {
        if (!produced.has(req)) {
          issues.push({
            severity: 'error', code: 'MISSING_CHANNEL',
            stage: i, primitive: st.axis + '.' + st.name,
            message: 'Requires channel "' + req + '" but no upstream stage produces it.',
          });
        }
      }
      for (const p of reg.produces) produced.add(p);
    }
    /* Sources should come first. */
    if (pl.stages[0].axis !== 'source') {
      issues.push({
        severity: 'warn', code: 'NO_SOURCE',
        message: 'Pipeline does not begin with a source stage.',
      });
    }
    return issues;
  }

  /* ====================================================================
     Runtime assert — the fail-loud runtime channel check.
     ==================================================================== */

  const assert = Object.freeze({
    field: function (f, requiredChannels, primitive, axis) {
      if (!f || !f.buf || typeof f.w !== 'number' || typeof f.h !== 'number') {
        throw new GlyphGridError('TYPE_MISMATCH', primitive, axis, {
          message: 'Expected a Field; got ' + (f === null ? 'null' : typeof f) + '.',
        });
      }
      if (!requiredChannels) return;
      for (const ch of requiredChannels) {
        if (!f.channels.has(ch)) {
          throw new GlyphGridError('MISSING_CHANNEL', primitive, axis, {
            message: 'Field missing required channel "' + ch + '". ' +
                     'Available: [' + Array.from(f.channels).join(', ') + '].',
          });
        }
      }
    },
    cellSignal: function (c, requiredChannels, primitive, axis) {
      if (!c || !c.buf || typeof c.cols !== 'number' || typeof c.rows !== 'number') {
        throw new GlyphGridError('TYPE_MISMATCH', primitive, axis, {
          message: 'Expected a CellSignal; got ' + (c === null ? 'null' : typeof c) + '.',
        });
      }
      if (!requiredChannels) return;
      for (const ch of requiredChannels) {
        if (!c.channels.has(ch)) {
          throw new GlyphGridError('MISSING_CHANNEL', primitive, axis, {
            message: 'CellSignal missing required channel "' + ch + '".',
          });
        }
      }
    },
  });

  /* ====================================================================
     FrameContext factory.
     ==================================================================== */

  let sharedPool = null;  /* module-level pool populated at first run(). */

  function makeContext(opts) {
    const seed = (opts && opts.seed != null) ? (opts.seed | 0) : 0;
    const frameIdx = (opts && opts.frameIdx != null) ? (opts.frameIdx | 0) : 0;
    const t = (opts && opts.t != null) ? +opts.t : 0;
    const config = (opts && opts.config) || {};
    const pool = (opts && opts.pool) || getSharedPool();
    const rngSeed = hash32(seed, frameIdx, 0x47f1, 0xab3d);
    const rng = makeSeededRng(rngSeed);
    /* NOT frozen — callers set `ctx.canvas` (the target paint surface) and
       per-test scratch after creation. The immutability we care about is
       seed/config/rng, which are already read-only-by-convention. */
    return {
      t: t, frameIdx: frameIdx, seed: seed,
      config: config, rng: rng, pool: pool,
    };
  }

  function getSharedPool() {
    if (!sharedPool) sharedPool = makeFieldPool();
    return sharedPool;
  }

  /* ====================================================================
     Pool init — caller runs this once at setup() after all primitives
     are registered. Allocates scratch per-primitive declaration.
     ==================================================================== */

  function initPool(w, h, cols, rows) {
    const pool = getSharedPool();
    /* Register every primitive's scratch tags with the pool. */
    for (const axis of AXES) {
      for (const reg of registry[axis].values()) {
        for (const s of reg.scratch.fields) {
          try { pool.registerField(s.tag, s.channels); }
          catch (e) { if (e.code !== 'REGISTRATION_CONFLICT') throw e; }
        }
        for (const s of reg.scratch.cellSignals) {
          try { pool.registerCellSignal(s.tag, s.channels); }
          catch (e) { if (e.code !== 'REGISTRATION_CONFLICT') throw e; }
        }
      }
    }
    pool.initFields(w, h);
    pool.initCellSignals(cols, rows);
    return pool;
  }

  /* ====================================================================
     run — execute a pipeline for one frame.

     Carries a mutable "signal" that each stage transforms:
       source → Field
       transform → Field (consumes Field, produces Field)
       sampling → CellSignal (consumes Field, produces CellSignal)
       selection → { cellSignal, glyphs: Uint16Array, atlas }
       color → { imageData } (cells painted; this is where we hand off to
                              the legacy per-cell color+text draw)
       postProcess → mutates imageData in place
       output → async sink (returns Promise if needed)

     For Wave 1 we keep signatures flexible: each primitive receives
     (signal, ctx, stage) and returns the new signal. The primitive
     wrappers in glyph-primitives.js encode the conversion.
     ==================================================================== */

  async function run(pl, ctx) {
    if (!pl || !pl.stages) {
      throw new GlyphGridError('TYPE_MISMATCH', null, null, {
        message: 'run() expected a Pipeline; got ' + typeof pl + '.',
      });
    }
    let signal = null;
    for (let i = 0; i < pl.stages.length; i++) {
      const st = pl.stages[i];
      try {
        const out = st.fn(signal, ctx, st);
        signal = (out && typeof out.then === 'function') ? await out : out;
      } catch (e) {
        if (e instanceof GlyphGridError) {
          /* Rethrow with stage index annotation for debugging. */
          e.detail = Object.assign({}, e.detail, { stageIdx: i });
          throw e;
        }
        throw new GlyphGridError('STAGE_EXCEPTION', st.name, st.axis, {
          message: 'Stage threw: ' + (e && e.message ? e.message : String(e)),
          stageIdx: i,
          originalError: e,
        });
      }
    }
    return signal;
  }

  /* ====================================================================
     fromScene — legacy adapter.

     Registers 'source.from-scene' once. When used in a pipeline, opts.scene
     is the legacy (g, t, config) => void | {depth?} function. We call it
     into a cached p5.Graphics buffer provided via ctx.srcGraphics (set by
     render.html at setup), loadPixels(), and linearize into a Field.

     This is the bridge that lets every existing scene drop into the new
     runtime without modification.
     ==================================================================== */

  function fromScene(sceneFn, optsArg) {
    const label = (optsArg && optsArg.label) || sceneFn.name || 'anonymous-scene';
    /* We don't register a new primitive per scene; the 'from-scene' primitive
       dispatches on the `scene` field in opts. The caller invokes it as:
         pipeline().source('from-scene', { scene: sceneFn })
       This factory is a convenience that lifts the fn into opts. */
    return { primitive: 'from-scene', opts: { scene: sceneFn, label: label } };
  }

  /* ====================================================================
     Sanity — expose to window.
     ==================================================================== */

  const root = (typeof window !== 'undefined') ? window
             : (typeof globalThis !== 'undefined') ? globalThis
             : this;
  root.GlyphGrid = root.GlyphGrid || {};
  root.GlyphGrid.GlyphGridError = GlyphGridError;
  root.GlyphGrid.runtime = Object.freeze({
    AXES: AXES,
    AXIS_ORDER: AXIS_ORDER,
    AXIS_OUTPUT: AXIS_OUTPUT,
    hash32: hash32,
    makeSeededRng: makeSeededRng,
    makeFieldPool: makeFieldPool,
    getSharedPool: getSharedPool,
    initPool: initPool,
    register: register,
    lookup: lookup,
    pipeline: pipeline,
    lint: lint,
    assert: assert,
    makeContext: makeContext,
    run: run,
    fromScene: fromScene,
    registryView: function () {
      /* Read-only snapshot for debugging. */
      const out = Object.create(null);
      for (const axis of AXES) out[axis] = Array.from(registry[axis].keys());
      return out;
    },
  });

  if (typeof module !== 'undefined' && module.exports) {
    module.exports = root.GlyphGrid.runtime;
  }
})();
