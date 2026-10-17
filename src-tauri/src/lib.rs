use base64::engine::general_purpose;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{Emitter, Manager};
use tauri_plugin_dialog::DialogExt;

pub mod mcp;

/* ============================================================================
 *  CLI / headless-render plumbing.
 *
 *  When the binary is launched with `render --in foo --out bar`, main.rs
 *  builds a HeadlessRenderJob and calls run_headless_render(). That function
 *  spins up a Tauri runtime with a hidden window, stores the job in managed
 *  state, and waits for the JS side to call exit_with_status() when done.
 *  The same rendering pipeline is used as the GUI path — only the entry +
 *  exit are different.
 * ============================================================================ */

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConfigOverrides {
    pub palette: Option<String>,
    pub color_mode: Option<String>,
    pub ramp: Option<String>,
    pub dither_mode: Option<String>,
    pub selection_mode: Option<String>,
    pub glyph_set: Option<String>,
    pub sampling_strategy: Option<String>,
    pub postprocess: Vec<String>,
    pub cols: Option<u32>,
    pub rows: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct HeadlessRenderJob {
    pub in_path: PathBuf,
    pub out_path: PathBuf,
    pub frames: u32,
    pub show_window: bool,
    pub preset_path: Option<PathBuf>,
    pub config_overrides: ConfigOverrides,
    /// Output format. `"gif"` (default) → gifski; `"mp4"` → openh264 + mp4.
    /// When None, derived from `out_path`'s extension.
    pub format: Option<String>,
    /// MP4 rate control: `"upload"` (default, bitrate RC) or `"archive"`
    /// (fixed QP 26, the v0.1.x bitstream).  Ignored for GIF output.
    pub mp4_quality: Option<String>,
}

impl HeadlessRenderJob {
    /// Resolve the output format: explicit `format` wins, otherwise infer
    /// from the output extension.  Defaults to `"gif"` for unknown extensions.
    pub fn resolved_format(&self) -> String {
        if let Some(f) = &self.format {
            return f.to_lowercase();
        }
        match self
            .out_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_lowercase())
            .as_deref()
        {
            Some("mp4") => "mp4".into(),
            _ => "gif".into(),
        }
    }
}

impl HeadlessRenderJob {
    /// Translate the CLI-shaped overrides into a JSON value that matches the
    /// JS CONFIG schema (camelCase keys, nested dither/grid/postprocess).
    /// Preset (if given) lays the foundation; explicit flags override.
    pub fn build_js_config(&self) -> serde_json::Value {
        let mut config = serde_json::Map::new();

        // 1. Lay the preset (lowest priority)
        if let Some(preset_path) = &self.preset_path {
            if let Ok(json_str) = fs::read_to_string(preset_path) {
                if let Ok(serde_json::Value::Object(obj)) =
                    serde_json::from_str::<serde_json::Value>(&json_str)
                {
                    for (k, v) in obj {
                        config.insert(k, v);
                    }
                }
            }
        }

        // 2. Layer flag overrides on top
        let o = &self.config_overrides;
        if let Some(v) = &o.palette {
            config.insert("palette".into(), serde_json::Value::String(v.clone()));
        }
        if let Some(v) = &o.color_mode {
            config.insert("colorMode".into(), serde_json::Value::String(v.clone()));
        }
        if let Some(v) = &o.ramp {
            config.insert("ramp".into(), serde_json::Value::String(v.clone()));
        }
        if let Some(v) = &o.dither_mode {
            config.insert(
                "dither".into(),
                serde_json::json!({ "mode": v, "asSourcePrefilter": true, "levels": 16 }),
            );
        }
        if let Some(v) = &o.selection_mode {
            config.insert("selectionMode".into(), serde_json::Value::String(v.clone()));
        }
        if let Some(v) = &o.glyph_set {
            let val = if v == "null" || v == "none" {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(v.clone())
            };
            config.insert("glyphSet".into(), val);
        }
        if let Some(v) = &o.sampling_strategy {
            config.insert(
                "samplingStrategy".into(),
                serde_json::Value::String(v.clone()),
            );
        }
        if !o.postprocess.is_empty() {
            // Build a postprocess block enabling exactly the listed stages,
            // disabling everything else explicitly so we don't inherit any
            // scene defaults.
            let all_stages = [
                "vignette",
                "bloom",
                "halation",
                "scanlines",
                "chromaticAberration",
                "phosphorDecay",
                "depthFog",
                "crtBeam",
            ];
            let enabled: std::collections::HashSet<&str> =
                o.postprocess.iter().map(|s| s.as_str()).collect();
            let mut pp = serde_json::Map::new();
            for stage in all_stages.iter() {
                pp.insert(
                    (*stage).to_string(),
                    serde_json::json!({ "enabled": enabled.contains(*stage) }),
                );
            }
            config.insert("postprocess".into(), serde_json::Value::Object(pp));
        }
        if o.cols.is_some() || o.rows.is_some() {
            let mut grid = serde_json::Map::new();
            if let Some(c) = o.cols {
                grid.insert("cols".into(), serde_json::Value::from(c));
            }
            if let Some(r) = o.rows {
                grid.insert("rows".into(), serde_json::Value::from(r));
            }
            config.insert("grid".into(), serde_json::Value::Object(grid));
        }

        serde_json::Value::Object(config)
    }
}

/// Managed state that the JS side queries on startup to discover the CLI job.
struct CliJobState {
    job: Mutex<Option<serde_json::Value>>,
    exit_code: Arc<Mutex<i32>>,
}

/// Returns the CLI render job to JS, or null in GUI mode.
#[tauri::command]
fn get_cli_render_job(state: tauri::State<CliJobState>) -> Option<serde_json::Value> {
    state.job.lock().ok().and_then(|g| g.clone())
}

/// JS-side log forwarding for headless / production builds where
/// `console.log` doesn't reach stderr.  Useful for surfacing batch errors.
#[tauri::command]
fn cli_log(msg: String) {
    eprintln!("glyph-grid-studio[js]: {}", msg);
}

/// JS calls this when the headless render is finished. Logs to stderr so
/// CLI users can see why a headless render reported failure, then exits
/// the process directly.
///
/// Why `std::process::exit` instead of `app.exit(code)`: in Tauri 2.10,
/// `AppHandle::exit(code)` accepts the code but the actual process always
/// terminates with status 0, which breaks shell-level error handling
/// (CI, MCP subprocess error reporting, scripts that check `$?`).
/// `std::process::exit` bypasses Tauri's cleanup but for a CLI render
/// that's about to terminate anyway, that's acceptable.
#[tauri::command]
fn exit_with_status(_app: tauri::AppHandle, state: tauri::State<CliJobState>, ok: bool) {
    let code = if ok { 0 } else { 1 };
    if let Ok(mut guard) = state.exit_code.lock() {
        *guard = code;
    }
    if !ok {
        eprintln!(
            "glyph-grid-studio: render reported failure (exit code {})",
            code
        );
    }
    std::process::exit(code);
}

/// Public entry point for the `batch` subcommand.  Reads the manifest JSON,
/// validates the basic shape, then hands a normalised batch JSON to the JS
/// side via `CliJobState`.  The JS hook (`tryHeadlessRender`) detects the
/// `batch: true` discriminator and runs `runBatchExport` with all jobs in
/// a single Tauri session — no per-variant Tauri restarts.  See ITER-024.
/// `jobs > 1` fans the manifest out over that many child processes instead
/// (ITER-055); 1 is the single-session behaviour above.
pub fn run_headless_batch(manifest_path: PathBuf, show_window: bool, jobs: usize) -> i32 {
    let raw = match fs::read_to_string(&manifest_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("batch: cannot read manifest {:?}: {}", manifest_path, e);
            return 2;
        }
    };
    let manifest: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("batch: manifest is not valid JSON: {}", e);
            return 2;
        }
    };

    // Required: in (source path), frames, jobs (array)
    let in_path = manifest
        .get("in")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let frames = manifest
        .get("frames")
        .and_then(|v| v.as_u64())
        .unwrap_or(24) as u32;
    // Optional manifest-level "perf": true — when set, the JS side emits
    // PERF_JOB NDJSON via cli_log per finished job (parsed by the
    // optimization-loop orchestrator). Default false (no overhead for
    // regular batch users).
    let perf = manifest
        .get("perf")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let jobs_arr = manifest.get("jobs").and_then(|v| v.as_array());

    let (in_path, jobs_arr) = match (in_path, jobs_arr) {
        (Some(p), Some(arr)) => (p, arr.clone()),
        _ => {
            eprintln!(
                "batch: manifest must have keys `in` (string) and `jobs` (array). Got: {:?}",
                manifest
            );
            return 2;
        }
    };

    // Audit 2026-06-10: canonicalize source paths up front. Relative
    // paths used to flow into the webview's read_image_file with a
    // different effective cwd and hang the session instead of erroring.
    let canon = |p: &str| -> Result<String, String> {
        PathBuf::from(p)
            .canonicalize()
            .map(|c| c.to_string_lossy().to_string())
            .map_err(|e| format!("{}: {}", p, e))
    };
    let in_path = match canon(&in_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("batch: cannot resolve manifest `in` path — {}", e);
            return 2;
        }
    };

    // Audit 2026-06-10: reject jobs with a missing/empty `out` up front —
    // previously the empty string flowed through to fs::write deep inside
    // the encode pipeline and surfaced as a cryptic IO error after the
    // whole render had already run.
    for (i, j) in jobs_arr.iter().enumerate() {
        let out_ok = j
            .get("out")
            .and_then(|v| v.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if !out_ok {
            let name = j.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            eprintln!(
                "batch: job {} ({:?}) has no `out` path — refusing manifest before any render runs",
                i, name
            );
            return 2;
        }
        // ITER-050: a typo'd `mp4Quality` would otherwise only surface after
        // the whole capture ran; refuse it with the manifest instead.
        let quality = j.get("mp4Quality");
        if let Some(e) = match quality {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(q)) => Mp4Quality::parse(Some(q)).err(),
            Some(other) => Some(format!("mp4Quality must be a string, got {}", other)),
        } {
            let name = j.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            eprintln!("batch: job {} ({:?}): {}", i, name, e);
            return 2;
        }
    }

    // ITER-055 (F4): `--jobs N` forks N CLI children, each its own
    // app + WebContent + GPU triple, over a round-robin split of the
    // already-validated manifest.  Wall is bound by each off-screen
    // webview's throttled rAF, not by CPU, so independent processes scale.
    let workers = batch_fan_out_count(
        jobs,
        jobs_arr.len(),
        std::thread::available_parallelism().ok().map(|p| p.get()),
    );
    if workers > 1 {
        if let Some(dup) = duplicate_out_path(&jobs_arr) {
            eprintln!(
                "batch: --jobs {} ignored: two jobs write {:?}; running serially so the last one wins as before",
                jobs, dup
            );
        } else {
            return run_batch_children(&manifest, &in_path, &jobs_arr, workers, show_window);
        }
    }

    // Build the JS-shaped batch JSON.  Each job carries its own out/format/config.
    let jobs_js: Vec<serde_json::Value> = jobs_arr
        .into_iter()
        .map(|j| {
            let name = j
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("job")
                .to_string();
            let out_path = j
                .get("out")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let format = j
                .get("format")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    if out_path.to_lowercase().ends_with(".mp4") {
                        "mp4".into()
                    } else {
                        "gif".into()
                    }
                });
            let config = j.get("config").cloned().unwrap_or(serde_json::json!({}));
            let cap_width = j.get("capWidth").and_then(|v| v.as_u64()).unwrap_or(0);
            // Optional per-job source override — when set, the JS batch
            // driver swaps `sourceImg` before this job's render. Lets
            // ONE studio session cover N sources × M configs without
            // re-spawning the process per source (the autonomous loop
            // needs this: 4 sources × 5 configs in one Tauri launch).
            let in_path_job: Option<String> = j
                .get("in")
                .and_then(|v| v.as_str())
                // Canonicalize per-job sources too (relative path = hang,
                // see audit 2026-06-10). Fall back to the raw string if
                // resolution fails — the JS side surfaces a per-job error
                // for missing files rather than killing the whole batch.
                .map(|s| canon(s).unwrap_or_else(|_| s.to_string()));
            // ITER-026: optional adaptive Twitter-fit per job.  Manifest
            // can set `targetMaxBytes: 15728640` (15 MB) and the encoder
            // walks the shrink ladder until it fits.  When unset AND
            // capWidth==720, we DEFAULT to 15 MB — preserves the implicit
            // "Twitter-fit" promise of 720-cap manifests without forcing
            // every driver script to pass the field explicitly.
            let target_max_bytes_explicit: Option<u64> =
                j.get("targetMaxBytes").and_then(|v| v.as_u64());
            let target_max_bytes = target_max_bytes_explicit.or_else(|| {
                if cap_width == 720 {
                    Some(15 * 1024 * 1024)
                } else {
                    None
                }
            });
            serde_json::json!({
                "name": name,
                "outPath": out_path,
                "inPath": in_path_job,
                "format": format,
                "config": config,
                "capWidth": cap_width,
                "targetMaxBytes": target_max_bytes,
                "mp4Quality": j.get("mp4Quality").and_then(|v| v.as_str()),
            })
        })
        .collect();

    let batch_json = serde_json::json!({
        "batch": true,
        "inPath": in_path,
        "frames": frames,
        "perf": perf,
        "jobs": jobs_js,
    });

    eprintln!(
        "batch: queued {} jobs from {:?}",
        batch_json
            .get("jobs")
            .and_then(|j| j.as_array())
            .map(|a| a.len())
            .unwrap_or(0),
        manifest_path
    );

    let exit_code = Arc::new(Mutex::new(2));
    let state = CliJobState {
        job: Mutex::new(Some(batch_json)),
        exit_code: exit_code.clone(),
    };
    run_tauri(state, !show_window);
    let code = *exit_code.lock().unwrap();
    code
}

/// ITER-055: how many child processes `batch --jobs N` forks.  Capped by the
/// job count and by `cores - 2` (BUG-009's headroom: each child also runs
/// gifski/MP4 workers, and the host stays usable).  1 = run in-process.
fn batch_fan_out_count(requested: usize, n_jobs: usize, cores: Option<usize>) -> usize {
    let cap = cores.map(|c| c.saturating_sub(2)).unwrap_or(1).max(1);
    requested.min(n_jobs).min(cap).max(1)
}

/// First `out` path two jobs share.  Serially the later job overwrites the
/// earlier one; across children the two writes would race, so fan-out is
/// refused for such manifests.
fn duplicate_out_path(jobs: &[serde_json::Value]) -> Option<String> {
    let mut seen = std::collections::HashSet::new();
    jobs.iter()
        .filter_map(|j| j.get("out").and_then(|v| v.as_str()))
        .find(|o| !seen.insert(o.to_string()))
        .map(|o| o.to_string())
}

/// Round-robin split of the manifest into `n` sub-manifests (top-level keys
/// kept, `in` canonical).  Serially a job without its own `in` renders
/// whatever source the previous job left loaded (runBatchExport only swaps
/// when `inPath` is set), so each job is pinned to that effective source
/// here — a child must not fall back to the manifest `in` where the serial
/// batch would not.
fn split_batch_manifest(
    manifest: &serde_json::Value,
    in_path: &str,
    jobs: &[serde_json::Value],
    n: usize,
) -> Vec<serde_json::Value> {
    let mut current_src = in_path.to_string();
    let mut parts: Vec<Vec<serde_json::Value>> = vec![Vec::new(); n];
    for (i, j) in jobs.iter().enumerate() {
        let mut j = j.clone();
        match j.get("in").and_then(|v| v.as_str()) {
            Some(s) => current_src = s.to_string(),
            None => {
                if let Some(obj) = j.as_object_mut() {
                    obj.insert("in".into(), serde_json::Value::String(current_src.clone()));
                }
            }
        }
        parts[i % n].push(j);
    }
    parts
        .into_iter()
        .map(|p| {
            let mut m = manifest.clone();
            if let Some(obj) = m.as_object_mut() {
                obj.insert("in".into(), serde_json::Value::String(in_path.to_string()));
                obj.insert("jobs".into(), serde_json::Value::Array(p));
            }
            m
        })
        .collect()
}

/// ITER-055: run the split manifest as `n` concurrent `batch` children of
/// this binary.  Each child's stderr is relayed line by line (PERF_JOB lines
/// verbatim so NDJSON consumers need no change; everything else tagged
/// `batch[i/n]`).  Exit = the worst child's code (a signal counts as 1).
fn run_batch_children(
    manifest: &serde_json::Value,
    in_path: &str,
    jobs: &[serde_json::Value],
    n: usize,
    show_window: bool,
) -> i32 {
    use std::io::BufRead;
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("batch: --jobs: cannot locate own binary: {}", e);
            return 2;
        }
    };
    let parts = split_batch_manifest(manifest, in_path, jobs, n);
    let mut paths = Vec::with_capacity(n);
    for (i, part) in parts.iter().enumerate() {
        let p = std::env::temp_dir().join(format!("ggs-batch-{}-{}.json", std::process::id(), i));
        if let Err(e) = fs::write(&p, part.to_string()) {
            eprintln!("batch: --jobs: cannot write sub-manifest {:?}: {}", p, e);
            for p in &paths {
                let _ = fs::remove_file(p);
            }
            return 2;
        }
        paths.push(p);
    }
    eprintln!("batch: {} jobs across {} processes", jobs.len(), n);

    let mut worst = 0;
    let mut running = Vec::with_capacity(n);
    for (i, p) in paths.iter().enumerate() {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("batch").arg("--manifest").arg(p);
        if show_window {
            cmd.arg("--show-window");
        }
        match cmd.stderr(std::process::Stdio::piped()).spawn() {
            Ok(mut child) => {
                let stderr = child.stderr.take().expect("piped stderr");
                let relay = std::thread::spawn(move || {
                    let mut r = std::io::BufReader::new(stderr);
                    let mut line = Vec::new();
                    while matches!(r.read_until(b'\n', &mut line), Ok(k) if k > 0) {
                        let text = String::from_utf8_lossy(&line);
                        let text = text.trim_end_matches('\n');
                        if text.contains("PERF_JOB") {
                            eprintln!("{}", text);
                        } else {
                            eprintln!("batch[{}/{}]: {}", i + 1, n, text);
                        }
                        line.clear();
                    }
                });
                running.push((child, relay));
            }
            Err(e) => {
                eprintln!("batch: --jobs: cannot spawn child {}: {}", i + 1, e);
                worst = 2;
            }
        }
    }
    for (mut child, relay) in running {
        let code = match child.wait() {
            Ok(s) => s.code().unwrap_or(1),
            Err(_) => 1,
        };
        let _ = relay.join();
        worst = worst.max(code);
    }
    for p in &paths {
        let _ = fs::remove_file(p);
    }
    worst
}

/// Public entry point invoked by main.rs for `render` subcommand.  Returns
/// the exit code; in practice the process exits via `app.exit()` from JS
/// before this function returns, so the return value here is a fallback for
/// the "tauri runtime closed without a render" case.
pub fn run_headless_render(job: HeadlessRenderJob) -> i32 {
    let format = job.resolved_format();
    let job_json = serde_json::json!({
        "inPath": job.in_path.to_string_lossy().to_string(),
        "outPath": job.out_path.to_string_lossy().to_string(),
        "frames": job.frames,
        "format": format,
        "mp4Quality": job.mp4_quality,
        "config": job.build_js_config(),
    });
    let exit_code = Arc::new(Mutex::new(2)); // 2 = "didn't complete"
    let state = CliJobState {
        job: Mutex::new(Some(job_json)),
        exit_code: exit_code.clone(),
    };
    run_tauri(state, !job.show_window);
    // In practice we should never reach here — exit_with_status calls
    // std::process::exit() directly. This is a fallback for the "Tauri
    // shut down without JS firing exit_with_status" case.
    let code = *exit_code.lock().unwrap();
    code
}

/// The catalog of all options the CLI / MCP / HTTP surfaces can pass.  The
/// authoritative source for this is `src/index.html` PALETTES + RAMPS objects
/// and the various module docs.  Kept in sync by hand.
pub fn catalog_json() -> String {
    serde_json::json!({
        "palettes": [
            "monochrome", "phosphor", "bauhaus", "lovespark", "mono-amber",
            "cyber-phosphor", "amber-phosphor", "bone-charcoal",
            "cream-paper", "silver-charcoal", "spice", "kawaii-pink"
        ],
        "color_modes": ["preserve", "monochrome", "duotone", "gradient"],
        "ramps": [
            "classic", "dense", "sparse", "unicode-block", "gradient",
            "gradientNoSpace", "blockShaded", "blockAscend", "radial"
        ],
        "dithers": [
            "none", "bayer4", "bayer8", "blueNoise", "temporal", "stbn",
            "floydSteinberg", "atkinson", "jarvisJudiceNinke"
        ],
        "selection_modes": ["brightness", "shape", "shape-edge-aware", "edge-directional"],
        "glyph_sets": ["null", "ascii", "asciiDense", "blockElements", "braille", "sextant", "octant"],
        "sampling_strategies": ["average", "nearest", "edge-weighted"],
        "postprocess_stages": [
            "vignette", "bloom", "halation", "scanlines",
            "chromaticAberration", "phosphorDecay", "depthFog", "crtBeam"
        ]
    })
    .to_string()
}

#[derive(Debug, Serialize, Deserialize)]
struct GifFrame {
    /// base64-encoded PNG bytes (no `data:image/png;base64,` prefix)
    b64: String,
}

/// One staged frame.  `rgba: None` → `bytes` is a PNG file (GUI staged,
/// base64 and fallback paths).  `rgba: Some((w, h))` → `bytes` is exactly
/// w×h×4 straight (un-premultiplied) RGBA8 from the headless capture's
/// getImageData (ITER-052) and is never PNG-decoded.
#[derive(Debug)]
struct PngFrame {
    bytes: Vec<u8>,
    rgba: Option<(u32, u32)>,
}

impl PngFrame {
    fn png(bytes: Vec<u8>) -> Self {
        PngFrame { bytes, rgba: None }
    }

    /// Width/height from the PNG IHDR or the raw frame's declared size —
    /// never a decode.
    fn dimensions(&self, index: usize) -> Result<(u32, u32), String> {
        match self.rgba {
            Some(dimensions) => Ok(dimensions),
            None => png_dimensions(&self.bytes, index),
        }
    }

    /// RGBA8 image for the encoders: a PNG decode, or one copy of the raw
    /// bytes (the staged set is borrowed so the twitter-fit ladder and the
    /// MP4 workers can share it).
    fn to_rgba8(&self, index: usize) -> Result<image::RgbaImage, String> {
        match self.rgba {
            Some((width, height)) => image::RgbaImage::from_raw(width, height, self.bytes.clone())
                .ok_or_else(|| format!("frame {} raw RGBA length mismatch", index)),
            None => Ok(
                image::load_from_memory_with_format(&self.bytes, image::ImageFormat::Png)
                    .map_err(|e| format!("frame {} decode: {}", index, e))?
                    .into_rgba8(),
            ),
        }
    }
}

/// Hard safety limits for native animation export. These limits are checked
/// from PNG IHDR metadata before any frame is decompressed into RGB/RGBA.
/// ITER-054: there is no total decoded-pixel cap any more.  It existed
/// because the GIF path decoded the whole set up front; both encoders now
/// decode through a bounded window, so decode-side memory is flat in frame
/// count.  What is retained is bounded directly: PNG payloads by the 256 MiB
/// byte cap, raw RGBA frames (ITER-052, exactly w×h×4) by the raw byte cap.
/// `MAX_FRAME_PIXELS` is a per-frame decompression-bomb guard (one
/// 10000×10000 frame), not a budget.
const MAX_EXPORT_FRAMES: usize = 600;
const MAX_FRAME_PIXELS: u64 = 100_000_000;
const MAX_TOTAL_COMPRESSED_FRAME_BYTES: u64 = 256 * 1024 * 1024;
const MAX_TOTAL_RAW_FRAME_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_TOTAL_COMPRESSED_BASE64_CHARS: u64 =
    (((MAX_TOTAL_COMPRESSED_FRAME_BYTES + 2) / 3) * 4) + 4;
const MAX_SOURCE_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CONCURRENT_CAPTURES: usize = 3;
const MAX_JOB_TOMBSTONES: usize = 64;
const JOB_RUNNING: u8 = 0;
const JOB_CANCELLED: u8 = 1;
const JOB_COMMITTING: u8 = 2;

static EXPORT_CANCELLATIONS: OnceLock<Mutex<HashMap<String, Arc<AtomicU8>>>> = OnceLock::new();
static EXPORT_CAPTURES: OnceLock<Mutex<HashMap<String, StagedExport>>> = OnceLock::new();
static CANCELLED_CAPTURE_JOBS: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
static COMMITTED_EXPORT_JOBS: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
static OUTPUT_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct StagedExport {
    expected_frames: usize,
    /// Frames accepted so far (== `frames.len()` unless streaming).
    received: usize,
    /// Retained frames.  Empty for a streaming GIF job without a byte
    /// target: those frames go to the encoder thread and are not kept.
    frames: Vec<PngFrame>,
    dimensions: Option<(u32, u32)>,
    total_bytes: u64,
    total_raw_bytes: u64,
    stream: Option<GifStream>,
}

/// ITER-053 (F12): the GIF encode settings, handed to `begin_export_capture`
/// so gifski can encode while frames are still being captured.
/// `finish_gif_export` must repeat the same values.
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
struct GifStreamConfig {
    path: String,
    delay_ms: u32,
    cap_width: Option<u32>,
    border: Option<BorderConfig>,
    target_max_bytes: Option<u64>,
}

/// What the streaming encoder thread hands back: the `.part` it wrote, the
/// width it encoded at, and the frames it retained for the twitter-fit
/// ladder (only when a byte target is set).
struct StreamedGif {
    part_path: PathBuf,
    frames: usize,
    size: u64,
    width: u32,
    retained: Vec<PngFrame>,
}

/// A per-job streaming GIF encoder (ITER-053).  Frames go over an UNBOUNDED
/// channel so the IPC/capture chain never waits on gifski (index.html
/// pauses the draw loop at 3 in-flight pushes); only the encoder's own
/// decode step feels gifski's bounded queue.
struct GifStream {
    config: GifStreamConfig,
    tx: Option<std::sync::mpsc::Sender<PngFrame>>,
    state: Arc<AtomicU8>,
    handle: Option<std::thread::JoinHandle<Result<StreamedGif, String>>>,
}

impl GifStreamConfig {
    /// Frames are kept for a possible twitter-fit re-encode only when a
    /// byte target is set.
    fn retains_frames(&self) -> bool {
        self.target_max_bytes.is_some_and(|t| t > 0)
    }
}

impl GifStream {
    /// Close the frame channel and wait for the encoder to finish the GIF.
    fn finish(mut self) -> Result<StreamedGif, String> {
        drop(self.tx.take());
        self.handle
            .take()
            .ok_or_else(|| "GIF stream already finished".to_string())?
            .join()
            .map_err(|_| "streaming GIF encoder panicked".to_string())?
    }
}

impl Drop for GifStream {
    /// Abandoned without `finish` (cancel_export dropped the capture, or a
    /// finish was refused): mark the job cancelled BEFORE closing the
    /// channel, so the encoder sees the cancel, gifski stops with
    /// `Error::Aborted` and the thread deletes its `.part`.  Never joins:
    /// this can run under the EXPORT_CAPTURES lock.
    fn drop(&mut self) {
        if self.handle.is_some() {
            let _ = self.state.compare_exchange(
                JOB_RUNNING,
                JOB_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
        drop(self.tx.take());
    }
}

/// Body of the streaming encoder thread.  The first frame fixes the output
/// width (same `validate_export_frames` + `validated_gif_width` as the
/// batch path); then every frame is decoded and fed to gifski in arrival
/// order — `stage_export_frame` already enforces index order — with the
/// same pts, so the GIF is byte-identical to the batch encode.  Any error
/// or cancel removes the `.part`.
fn run_gif_stream(
    rx: std::sync::mpsc::Receiver<PngFrame>,
    config: GifStreamConfig,
    total: usize,
    part_path: PathBuf,
    control: ExportControl,
) -> Result<StreamedGif, String> {
    let retain = config.retains_frames();
    let border = config.border.as_ref();
    let result = (|| -> Result<StreamedGif, String> {
        // Channel closed before any frame: the capture was dropped.
        let first = rx.recv().map_err(|_| "export cancelled".to_string())?;
        control.check_cancelled()?;
        let dimensions = validate_export_frames(std::slice::from_ref(&first), border)?;
        let width = validated_gif_width(dimensions, config.cap_width)?;
        let (collector, writer) =
            gifski::new(gifski_settings(width)).map_err(|e| format!("gifski init: {}", e))?;
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part_path)
            .map_err(|e| format!("create {:?}: {}", part_path, e))?;
        let mut out = std::io::BufWriter::new(file);
        let delay_ms = config.delay_ms;
        let feed_control = control.clone();
        let (written, fed) = std::thread::scope(|s| {
            let feeder = s.spawn(move || -> Result<(Vec<PngFrame>, usize), String> {
                let control = feed_control;
                let mut retained = Vec::new();
                let mut next = Some(first);
                let mut idx = 0usize;
                let result = loop {
                    let frame = match next.take() {
                        Some(frame) => frame,
                        None => match rx.recv() {
                            Ok(frame) => frame,
                            Err(_) => break Ok(()), // capture finished (or dropped)
                        },
                    };
                    if let Err(error) = control.check_cancelled() {
                        break Err(error);
                    }
                    let img = match decode_one_frame(&frame, idx, border) {
                        Ok(img) => img,
                        Err(error) => break Err(error),
                    };
                    if let Err(error) = collector.add_frame_rgba(idx, img, gif_pts(idx, delay_ms)) {
                        break Err(format!("frame {} add: {}", idx, error));
                    }
                    if retain {
                        retained.push(frame);
                    }
                    idx += 1;
                };
                drop(collector); // signals the writer to finalise
                result.map(|_| (retained, idx))
            });
            let written = writer.write(
                &mut out,
                &mut GifProgress {
                    control: control.clone(),
                    phase: "encode-gif-attempt-1".to_string(),
                    completed: 0,
                    total,
                },
            );
            let fed = feeder
                .join()
                .unwrap_or_else(|_| Err("streaming GIF feeder panicked".to_string()));
            (written, fed)
        });
        control.check_cancelled()?;
        let (retained, count) = fed?;
        written.map_err(|e| format!("gifski write: {}", e))?;
        if count != total {
            return Err(format!(
                "incomplete export capture: encoded {} of {} frames",
                count, total
            ));
        }
        let file = out
            .into_inner()
            .map_err(|e| format!("write {:?}: {}", part_path, e.error()))?;
        file.sync_all()
            .map_err(|e| format!("sync {:?}: {}", part_path, e))?;
        let size = file
            .metadata()
            .map_err(|e| format!("stat {:?}: {}", part_path, e))?
            .len();
        Ok(StreamedGif {
            part_path: part_path.clone(),
            frames: count,
            size,
            width,
            retained,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part_path);
    }
    result
}

#[derive(Clone)]
struct ExportControl {
    state: Arc<AtomicU8>,
    app: Option<tauri::AppHandle>,
    job_id: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportProgress<'a> {
    job_id: &'a str,
    phase: &'a str,
    completed: usize,
    total: usize,
}

impl ExportControl {
    fn check_cancelled(&self) -> Result<(), String> {
        if self.state.load(Ordering::Acquire) == JOB_CANCELLED {
            Err("export cancelled".into())
        } else {
            Ok(())
        }
    }

    fn begin_commit(&self) -> Result<(), String> {
        self.state
            .compare_exchange(
                JOB_RUNNING,
                JOB_COMMITTING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|state| {
                if state == JOB_CANCELLED {
                    "export cancelled".into()
                } else {
                    "export commit already started".into()
                }
            })
    }

    fn emit(&self, phase: &str, completed: usize, total: usize) {
        if let (Some(app), Some(job_id)) = (&self.app, &self.job_id) {
            let _ = app.emit(
                "export-progress",
                ExportProgress {
                    job_id,
                    phase,
                    completed: completed.min(total),
                    total,
                },
            );
        }
    }
}

fn remember_bounded(queue: &Mutex<VecDeque<String>>, job_id: String) {
    if let Ok(mut entries) = queue.lock() {
        if let Some(position) = entries.iter().position(|entry| entry == &job_id) {
            entries.remove(position);
        }
        entries.push_back(job_id);
        while entries.len() > MAX_JOB_TOMBSTONES {
            entries.pop_front();
        }
    }
}

fn take_remembered(queue: &Mutex<VecDeque<String>>, job_id: &str) -> bool {
    queue
        .lock()
        .ok()
        .and_then(|mut entries| {
            entries
                .iter()
                .position(|entry| entry == job_id)
                .and_then(|position| entries.remove(position))
        })
        .is_some()
}

fn is_remembered(queue: &Mutex<VecDeque<String>>, job_id: &str) -> bool {
    queue
        .lock()
        .map(|entries| entries.iter().any(|entry| entry == job_id))
        .unwrap_or(false)
}

/// Hidden sibling temp file for `path` (`.<name>.<pid>.<seq>.part`), with the
/// destination directory created.  Shared by `atomic_write_output` and the
/// streaming GIF encoder (ITER-053), which writes the `.part` during capture.
fn output_part_path(path: &PathBuf) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    fs::create_dir_all(parent).map_err(|e| format!("mkdir {:?}: {}", parent, e))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "export destination has no valid file name".to_string())?;
    let sequence = OUTPUT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(
        ".{}.{}.{}.part",
        file_name,
        std::process::id(),
        sequence
    )))
}

/// Commit a fully written + synced `.part` over `path`: the RUNNING →
/// COMMITTING transition (after which cancel reports false), rename, then
/// directory fsync.  The caller removes the `.part` on error.
fn commit_output_part(
    part_path: &PathBuf,
    path: &PathBuf,
    control: &ExportControl,
) -> Result<(), String> {
    control.begin_commit()?;
    if let Err(error) = fs::rename(part_path, path) {
        let _ = control.state.compare_exchange(
            JOB_COMMITTING,
            JOB_RUNNING,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        return Err(format!("rename {:?} to {:?}: {}", part_path, path, error));
    }
    #[cfg(unix)]
    {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|e| format!("sync export directory {:?}: {}", parent, e))?;
    }
    control.emit("write", 1, 1);
    Ok(())
}

fn atomic_write_output(
    path: &PathBuf,
    bytes: &[u8],
    control: &ExportControl,
) -> Result<(), String> {
    control.check_cancelled()?;
    let part_path = output_part_path(path)?;

    let result = (|| -> Result<(), String> {
        control.emit("write", 0, 1);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part_path)
            .map_err(|e| format!("create {:?}: {}", part_path, e))?;
        file.write_all(bytes)
            .map_err(|e| format!("write {:?}: {}", part_path, e))?;
        file.sync_all()
            .map_err(|e| format!("sync {:?}: {}", part_path, e))?;
        drop(file);
        commit_output_part(&part_path, path, control)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&part_path);
    }
    result
}

struct ExportRegistration {
    job_id: Option<String>,
    state: Arc<AtomicU8>,
}

impl Drop for ExportRegistration {
    fn drop(&mut self) {
        let Some(job_id) = &self.job_id else {
            return;
        };
        if let Ok(mut jobs) = EXPORT_CANCELLATIONS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
        {
            if jobs
                .get(job_id)
                .is_some_and(|current| Arc::ptr_eq(current, &self.state))
            {
                jobs.remove(job_id);
            }
        }
        if self.state.load(Ordering::Acquire) == JOB_COMMITTING {
            remember_bounded(
                COMMITTED_EXPORT_JOBS.get_or_init(|| Mutex::new(VecDeque::new())),
                job_id.clone(),
            );
        }
    }
}

fn register_export(
    app: tauri::AppHandle,
    job_id: Option<String>,
) -> (ExportControl, ExportRegistration) {
    register_export_state(app, job_id, Arc::new(AtomicU8::new(JOB_RUNNING)))
}

/// `register_export` with a caller-supplied state: a streaming GIF job
/// (ITER-053) registers the state its encoder thread already watches, so a
/// cancel during finish aborts the running gifski encode.
fn register_export_state(
    app: tauri::AppHandle,
    job_id: Option<String>,
    state: Arc<AtomicU8>,
) -> (ExportControl, ExportRegistration) {
    if let Some(id) = &job_id {
        let _ = take_remembered(
            COMMITTED_EXPORT_JOBS.get_or_init(|| Mutex::new(VecDeque::new())),
            id,
        );
        if let Ok(mut jobs) = EXPORT_CANCELLATIONS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
        {
            if let Some(previous) = jobs.insert(id.clone(), state.clone()) {
                let _ = previous.compare_exchange(
                    JOB_RUNNING,
                    JOB_CANCELLED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            }
        }
    }
    (
        ExportControl {
            state: state.clone(),
            app: Some(app),
            job_id: job_id.clone(),
        },
        ExportRegistration { job_id, state },
    )
}

#[tauri::command]
fn cancel_export(job_id: String) -> bool {
    if is_remembered(
        COMMITTED_EXPORT_JOBS.get_or_init(|| Mutex::new(VecDeque::new())),
        &job_id,
    ) {
        return false;
    }

    let running_state = EXPORT_CANCELLATIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|jobs| jobs.get(&job_id).cloned());
    if let Some(state) = running_state {
        return match state.compare_exchange(
            JOB_RUNNING,
            JOB_CANCELLED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) | Err(JOB_CANCELLED) => true,
            Err(JOB_COMMITTING) => false,
            Err(_) => false,
        };
    }

    let removed_capture = EXPORT_CAPTURES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|mut captures| captures.remove(&job_id))
        .is_some();
    if removed_capture {
        return true;
    }
    remember_bounded(
        CANCELLED_CAPTURE_JOBS.get_or_init(|| Mutex::new(VecDeque::new())),
        job_id,
    );
    true
}

/// `gif_stream` (ITER-053, optional): the GIF job's encode settings.  When
/// present the gifski encode starts now, on its own thread, and consumes
/// frames as they are pushed; `finish_gif_export` then only waits for the
/// tail.  Absent → frames are staged and encoded at finish, as before (the
/// GUI path, MP4, older frontends).
#[tauri::command]
fn begin_export_capture(
    job_id: String,
    total: usize,
    gif_stream: Option<GifStreamConfig>,
) -> Result<(), String> {
    if job_id.trim().is_empty() {
        return Err("jobId must not be empty".into());
    }
    if total == 0 || total > MAX_EXPORT_FRAMES {
        return Err(format!(
            "frame count {} is outside the allowed range 1..={}",
            total, MAX_EXPORT_FRAMES
        ));
    }
    if take_remembered(
        CANCELLED_CAPTURE_JOBS.get_or_init(|| Mutex::new(VecDeque::new())),
        &job_id,
    ) {
        return Err("export cancelled".into());
    }
    let mut capture = StagedExport {
        expected_frames: total,
        received: 0,
        frames: if gif_stream.is_none() { Vec::with_capacity(total) } else { Vec::new() },
        dimensions: None,
        total_bytes: 0,
        total_raw_bytes: 0,
        stream: None,
    };
    let mut captures = EXPORT_CAPTURES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| "export capture state is unavailable".to_string())?;
    if captures.contains_key(&job_id) {
        return Err(format!("export capture already exists: {}", job_id));
    }
    if captures.len() >= MAX_CONCURRENT_CAPTURES {
        return Err(format!(
            "too many concurrent export captures: limit is {}",
            MAX_CONCURRENT_CAPTURES
        ));
    }
    if let Some(config) = gif_stream {
        let part_path = output_part_path(&PathBuf::from(&config.path))?;
        let state = Arc::new(AtomicU8::new(JOB_RUNNING));
        let control = ExportControl {
            state: state.clone(),
            app: None,
            job_id: Some(job_id.clone()),
        };
        let (tx, rx) = std::sync::mpsc::channel::<PngFrame>();
        let thread_config = config.clone();
        let handle = std::thread::Builder::new()
            .name("gif-stream".into())
            .spawn(move || run_gif_stream(rx, thread_config, total, part_path, control))
            .map_err(|e| format!("spawn GIF stream encoder: {}", e))?;
        capture.stream = Some(GifStream {
            config,
            tx: Some(tx),
            state,
            handle: Some(handle),
        });
    }
    captures.insert(job_id, capture);
    Ok(())
}

#[tauri::command]
async fn push_export_frame(job_id: String, index: usize, b64: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        if b64.len() as u64 > MAX_TOTAL_COMPRESSED_BASE64_CHARS {
            return Err(format!(
                "frame {} base64 payload exceeds export byte limit",
                index
            ));
        }
        let bytes = general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| format!("frame {} b64: {}", index, e))?;
        stage_export_frame(&job_id, index, PngFrame::png(bytes))
    })
    .await
    .map_err(|e| format!("frame staging worker failed: {}", e))?
}

/// ITER-040: binary twin of `push_export_frame`.  The PNG arrives as the raw
/// IPC body (`invoke(cmd, Uint8Array, { headers })`), so the webview skips the
/// FileReader data-URL build + split copy and Rust skips the 33 %-larger JSON
/// string parse + base64 decode on every captured frame.  jobId/index travel
/// as headers.  The b64 command stays for older frontends and the fallback.
#[tauri::command]
async fn push_export_frame_raw(request: tauri::ipc::Request<'_>) -> Result<(), String> {
    fn header<'a>(request: &'a tauri::ipc::Request<'_>, name: &str) -> Result<&'a str, String> {
        request
            .headers()
            .get(name)
            .ok_or_else(|| format!("missing {} header", name))?
            .to_str()
            .map_err(|_| format!("invalid {} header", name))
    }
    let job_id = header(&request, "x-job-id")?.to_string();
    let index: usize = header(&request, "x-index")?
        .parse()
        .map_err(|_| "invalid x-index header".to_string())?;
    // ITER-052: `x-format: rgba` + `x-width`/`x-height` marks a straight
    // RGBA8 body (headless getImageData capture); absent → PNG as before.
    let rgba = match request.headers().get("x-format").map(|v| v.to_str()) {
        None => None,
        Some(Ok("png")) => None,
        Some(Ok("rgba")) => {
            let dim = |name: &str| -> Result<u32, String> {
                header(&request, name)?
                    .parse::<u32>()
                    .ok()
                    .filter(|v| *v > 0)
                    .ok_or_else(|| format!("invalid {} header", name))
            };
            Some((dim("x-width")?, dim("x-height")?))
        }
        Some(_) => return Err("invalid x-format header".into()),
    };
    let bytes = match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => {
            match rgba {
                Some((width, height)) => check_raw_rgba_len(bytes.len(), width, height, index)?,
                None => {
                    if bytes.len() as u64 > MAX_TOTAL_COMPRESSED_FRAME_BYTES {
                        return Err(format!("frame {} payload exceeds export byte limit", index));
                    }
                }
            }
            bytes.clone()
        }
        _ => return Err("push_export_frame_raw expects a raw binary body".into()),
    };
    let frame = PngFrame { bytes, rgba };
    tauri::async_runtime::spawn_blocking(move || stage_export_frame(&job_id, index, frame))
        .await
        .map_err(|e| format!("frame staging worker failed: {}", e))?
}

/// ITER-052: a raw frame must be exactly width×height×4 bytes and fit the
/// per-frame pixel guard — checked before the IPC body is copied.
fn check_raw_rgba_len(len: usize, width: u32, height: u32, index: usize) -> Result<(), String> {
    let pixels = check_frame_pixels(width, height, index)?;
    if len as u64 != pixels * 4 {
        return Err(format!(
            "frame {} raw RGBA is {} bytes, expected {}x{}x4 = {}",
            index,
            len,
            width,
            height,
            pixels * 4
        ));
    }
    Ok(())
}

/// Per-frame decompression-bomb guard (ITER-054): one frame's pixel count,
/// never a running total.
fn check_frame_pixels(width: u32, height: u32, index: usize) -> Result<u64, String> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| format!("frame {} pixel count overflow", index))?;
    if pixels > MAX_FRAME_PIXELS {
        return Err(format!(
            "frame {} is {}x{} = {} pixels, exceeds the per-frame pixel limit {}",
            index, width, height, pixels, MAX_FRAME_PIXELS
        ));
    }
    Ok(pixels)
}

/// Validate one frame against the job's staged capture (order, declared
/// total, consistent dimensions, retained-byte budgets) and retain it.  Shared
/// by the base64 and raw-body push commands.
fn stage_export_frame(job_id: &str, index: usize, frame: PngFrame) -> Result<(), String> {
    let dimensions = frame.dimensions(index)?;
    let mut captures = EXPORT_CAPTURES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| "export capture state is unavailable".to_string())?;
    let capture = captures
        .get_mut(job_id)
        .ok_or_else(|| format!("unknown export capture: {}", job_id))?;
    if index != capture.received {
        return Err(format!(
            "out-of-order frame: got index {}, expected {}",
            index, capture.received
        ));
    }
    if index >= capture.expected_frames {
        return Err(format!(
            "frame index {} exceeds declared total {}",
            index, capture.expected_frames
        ));
    }
    if let Some(expected) = capture.dimensions {
        if dimensions != expected {
            return Err(format!(
                "frame {} dimensions {}x{} do not match first frame {}x{}",
                index, dimensions.0, dimensions.1, expected.0, expected.1
            ));
        }
    } else {
        capture.dimensions = Some(dimensions);
    }
    check_frame_pixels(dimensions.0, dimensions.1, index)?;
    // ITER-054: PNG payloads count against the compressed-byte cap, raw
    // RGBA (ITER-052) against the raw-byte cap — what is held, not what a
    // decode would produce.  ITER-053: a streaming job without a byte target
    // retains nothing (each frame is dropped once gifski has it), so its
    // frames count against neither.
    let retained = capture
        .stream
        .as_ref()
        .map_or(true, |stream| stream.config.retains_frames());
    let (mut next_bytes, mut next_raw_bytes) = (capture.total_bytes, capture.total_raw_bytes);
    let len = if retained { frame.bytes.len() as u64 } else { 0 };
    if frame.rgba.is_some() {
        next_raw_bytes = next_raw_bytes
            .checked_add(len)
            .ok_or_else(|| "total raw frame byte count overflow".to_string())?;
        if next_raw_bytes > MAX_TOTAL_RAW_FRAME_BYTES {
            return Err(format!(
                "raw frames are too large: {} bytes exceeds limit {} bytes; lower duration, FPS, or output size",
                next_raw_bytes, MAX_TOTAL_RAW_FRAME_BYTES
            ));
        }
    } else {
        next_bytes = next_bytes
            .checked_add(len)
            .ok_or_else(|| "total compressed frame byte count overflow".to_string())?;
        if next_bytes > MAX_TOTAL_COMPRESSED_FRAME_BYTES {
            return Err(format!(
                "compressed frames are too large: {} bytes exceeds limit {} bytes",
                next_bytes, MAX_TOTAL_COMPRESSED_FRAME_BYTES
            ));
        }
    }
    match &capture.stream {
        // The encoder keeps retained frames itself, for the ladder.
        Some(stream) => stream
            .tx
            .as_ref()
            .ok_or_else(|| "GIF stream is closed".to_string())?
            .send(frame)
            .map_err(|_| {
                "streaming GIF encoder stopped; finish or cancel the export for the cause".to_string()
            })?,
        None => capture.frames.push(frame),
    }
    capture.total_bytes = next_bytes;
    capture.total_raw_bytes = next_raw_bytes;
    capture.received += 1;
    Ok(())
}

fn take_export_capture(job_id: &str) -> Result<StagedExport, String> {
    let mut captures = EXPORT_CAPTURES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| "export capture state is unavailable".to_string())?;
    let capture = captures
        .get(job_id)
        .ok_or_else(|| format!("unknown export capture: {}", job_id))?;
    if capture.received != capture.expected_frames {
        return Err(format!(
            "incomplete export capture: received {} of {} frames",
            capture.received, capture.expected_frames
        ));
    }
    Ok(captures
        .remove(job_id)
        .expect("capture existed while lock was held"))
}

/// The cancellation state a streaming capture's encoder already watches
/// (ITER-053), so `finish_gif_export` can register it before taking the
/// capture — the same register-then-take order as the non-streaming path.
fn capture_stream_state(job_id: &str) -> Option<Arc<AtomicU8>> {
    EXPORT_CAPTURES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()?
        .get(job_id)?
        .stream
        .as_ref()
        .map(|stream| stream.state.clone())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExportFrameDimensions {
    width: u32,
    height: u32,
    output_width: u32,
    output_height: u32,
    total_output_pixels: u64,
}

/// Bordered-export config passed from JS.  When `enabled` is true and
/// `width > 0`, frames get a palette-bg matte band of `width` px on all
/// four sides before encoding.  v0.1.1 supports `style: "solid"` only;
/// v0.1.2 will add `inner-line`, `passepartout`, and `deckle`.
#[derive(Debug, Serialize, Deserialize, Default, Clone, PartialEq)]
#[serde(default)]
struct BorderConfig {
    enabled: bool,
    width: u32,
    style: String,
    #[serde(rename = "bgColor")]
    bg_color: String,
    #[serde(rename = "inkColor")]
    ink_color: String,
}

fn png_dimensions(bytes: &[u8], index: usize) -> Result<(u32, u32), String> {
    // A PNG starts with the 8-byte signature followed by the 25-byte IHDR
    // chunk (length, type, 13 bytes of data). Width and height are the first
    // two IHDR fields. Reading only these fixed offsets avoids image decode
    // and allocation based on attacker-controlled dimensions.
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 33 || &bytes[..8] != PNG_SIGNATURE {
        return Err(format!("frame {} is not a valid PNG header", index));
    }
    if &bytes[12..16] != b"IHDR" || u32::from_be_bytes(bytes[8..12].try_into().unwrap()) != 13 {
        return Err(format!("frame {} has an invalid PNG IHDR", index));
    }

    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    if width == 0 || height == 0 {
        return Err(format!(
            "frame {} has invalid dimensions {}x{}",
            index, width, height
        ));
    }
    Ok((width, height))
}

fn validate_export_frames(
    frames: &[PngFrame],
    border: Option<&BorderConfig>,
) -> Result<ExportFrameDimensions, String> {
    if frames.is_empty() {
        return Err("no frames provided".into());
    }
    if frames.len() > MAX_EXPORT_FRAMES {
        return Err(format!(
            "too many frames: {} exceeds export limit {}",
            frames.len(),
            MAX_EXPORT_FRAMES
        ));
    }

    let mut first_dimensions = None;
    let mut output_dimensions = None;
    let mut total_output_pixels = 0_u64;
    for (index, frame) in frames.iter().enumerate() {
        let dimensions = frame.dimensions(index)?;
        if let Some(expected) = first_dimensions {
            if dimensions != expected {
                return Err(format!(
                    "frame {} dimensions {}x{} do not match first frame {}x{}",
                    index, dimensions.0, dimensions.1, expected.0, expected.1
                ));
            }
        } else {
            first_dimensions = Some(dimensions);
        }

        let (output_width, output_height) = match border {
            Some(config) if config.enabled && config.width > 0 => {
                let even_border = (config.width / 2) * 2;
                if even_border == 0 {
                    dimensions
                } else {
                    let border_span = even_border
                        .checked_mul(2)
                        .ok_or_else(|| "border width is too large".to_string())?;
                    let width = dimensions
                        .0
                        .checked_add(border_span)
                        .ok_or_else(|| "bordered frame width is too large".to_string())?;
                    let height = dimensions
                        .1
                        .checked_add(border_span)
                        .ok_or_else(|| "bordered frame height is too large".to_string())?;
                    ((width / 2) * 2, (height / 2) * 2)
                }
            }
            _ => dimensions,
        };
        output_dimensions = Some((output_width, output_height));
        // ITER-054: per-frame guard only; the encoders decode through a
        // bounded window, so the total is not a memory figure any more.
        let frame_pixels = check_frame_pixels(output_width, output_height, index)?;
        total_output_pixels = total_output_pixels
            .checked_add(frame_pixels)
            .ok_or_else(|| "total decoded pixel count overflow".to_string())?;
    }

    let (width, height) = first_dimensions.expect("non-empty frames checked above");
    let (output_width, output_height) = output_dimensions.expect("non-empty frames checked above");
    Ok(ExportFrameDimensions {
        width,
        height,
        output_width,
        output_height,
        total_output_pixels,
    })
}

fn validated_gif_width(
    dimensions: ExportFrameDimensions,
    cap_width: Option<u32>,
) -> Result<u32, String> {
    let width = cap_width
        .filter(|cap| *cap > 0 && *cap < dimensions.output_width)
        .unwrap_or(dimensions.output_width);
    let height = if width < dimensions.output_width {
        u64::from(dimensions.output_height)
            .checked_mul(u64::from(width))
            .ok_or_else(|| "GIF output height overflow".to_string())?
            .div_ceil(u64::from(dimensions.output_width))
    } else {
        u64::from(dimensions.output_height)
    };
    if width > u16::MAX as u32 || height > u64::from(u16::MAX) {
        return Err(format!(
            "GIF output dimensions {}x{} exceed format limit {}",
            width,
            height,
            u16::MAX
        ));
    }
    Ok(width)
}

fn decode_frame_payloads(frames: Vec<GifFrame>) -> Result<Vec<PngFrame>, String> {
    if frames.is_empty() {
        return Err("no frames provided".into());
    }
    if frames.len() > MAX_EXPORT_FRAMES {
        return Err(format!(
            "too many frames: {} exceeds export limit {}",
            frames.len(),
            MAX_EXPORT_FRAMES
        ));
    }

    let mut total_bytes = 0_u64;
    let mut decoded = Vec::with_capacity(frames.len());
    for (index, frame) in frames.into_iter().enumerate() {
        if frame.b64.len() as u64 > MAX_TOTAL_COMPRESSED_BASE64_CHARS {
            return Err(format!(
                "frame {} base64 payload exceeds export byte limit",
                index
            ));
        }
        let bytes = general_purpose::STANDARD
            .decode(frame.b64)
            .map_err(|e| format!("frame {} b64: {}", index, e))?;
        total_bytes = total_bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| "total compressed frame byte count overflow".to_string())?;
        if total_bytes > MAX_TOTAL_COMPRESSED_FRAME_BYTES {
            return Err(format!(
                "compressed frames are too large: {} bytes exceeds limit {} bytes",
                total_bytes, MAX_TOTAL_COMPRESSED_FRAME_BYTES
            ));
        }
        png_dimensions(&bytes, index)?;
        decoded.push(PngFrame::png(bytes));
    }
    Ok(decoded)
}

fn parse_hex_rgb(s: &str) -> (u8, u8, u8) {
    let s = s.trim().trim_start_matches('#');
    if s.len() == 6 && s.is_ascii() {
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&s[0..2], 16),
            u8::from_str_radix(&s[2..4], 16),
            u8::from_str_radix(&s[4..6], 16),
        ) {
            return (r, g, b);
        }
    }
    // Default: cream-paper bg.
    (232, 221, 200)
}

/// Wrap an RGBA frame in a palette-bg matte band of `border.width` px on
/// all four sides.  Output dims are even (yuv420p compatibility).
/// Returns the input image unmodified if the border is disabled or
/// width is 0.  Currently only the `solid` style is implemented;
/// unknown styles fall back to solid.
fn apply_border(img: image::RgbaImage, border: &BorderConfig) -> image::RgbaImage {
    if !border.enabled || border.width == 0 {
        return img;
    }
    let bw = (border.width / 2) * 2; // even
    if bw == 0 {
        return img;
    }
    let (src_w, src_h) = (img.width(), img.height());
    let new_w_raw = src_w + 2 * bw;
    let new_h_raw = src_h + 2 * bw;
    let new_w = (new_w_raw / 2) * 2;
    let new_h = (new_h_raw / 2) * 2;

    let (r, g, b) = parse_hex_rgb(&border.bg_color);
    let mut out = image::RgbaImage::from_pixel(new_w, new_h, image::Rgba([r, g, b, 255]));

    let dst_x = (new_w - src_w) / 2;
    let dst_y = (new_h - src_h) / 2;

    // Copy source into the centred region.
    for y in 0..src_h {
        for x in 0..src_w {
            let p = img.get_pixel(x, y);
            out.put_pixel(dst_x + x, dst_y + y, *p);
        }
    }

    out
}

/// Save a single PNG snapshot to disk, prompting the user via a native
/// save dialog. `data_url` is the canvas.toDataURL('image/png') string.
#[tauri::command]
async fn save_png(app: tauri::AppHandle, data_url: String) -> Result<String, String> {
    // Strip `data:image/png;base64,` prefix.
    let b64 = data_url
        .split(',')
        .nth(1)
        .ok_or_else(|| "invalid data URL".to_string())?;
    let bytes = general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| e.to_string())?;

    let path: Option<PathBuf> = app
        .dialog()
        .file()
        .add_filter("PNG image", &["png"])
        .set_file_name("glyph-frame.png")
        .blocking_save_file()
        .map(|fp| fp.into_path().ok())
        .flatten();

    if let Some(p) = path {
        fs::write(&p, &bytes).map_err(|e| e.to_string())?;
        Ok(p.display().to_string())
    } else {
        Err("cancelled".into())
    }
}

/// Decode one staged frame (PNG, or raw RGBA since ITER-052) to RGBA, applying the optional border.
/// Pure per-frame function — safe to run on any thread (EXPORT-OPT-1).
fn decode_one_frame(
    frame: &PngFrame,
    idx: usize,
    border: Option<&BorderConfig>,
) -> Result<imgref::ImgVec<rgb::RGBA8>, String> {
    let mut img = frame.to_rgba8(idx)?;
    if let Some(b) = border {
        img = apply_border(img, b);
    }
    let (w, h) = (img.width() as usize, img.height() as usize);
    let pixels: Vec<rgb::RGBA8> = img
        .pixels()
        .map(|p| rgb::RGBA8 {
            r: p[0],
            g: p[1],
            b: p[2],
            a: p[3],
        })
        .collect();
    Ok(imgref::ImgVec::new(pixels, w, h))
}

/// gifski progress hook: forwards per-frame progress and turns a cancelled
/// job into `false`, which makes gifski stop with `Error::Aborted`.
struct GifProgress {
    control: ExportControl,
    phase: String,
    completed: usize,
    total: usize,
}

impl gifski::progress::ProgressReporter for GifProgress {
    fn increase(&mut self) -> bool {
        self.completed = (self.completed + 1).min(self.total);
        self.control.emit(&self.phase, self.completed, self.total);
        self.control.state.load(Ordering::Acquire) != JOB_CANCELLED
    }
    fn done(&mut self, _msg: &str) {}
}

/// Presentation timestamp gifski gets for frame `idx` (seconds).
fn gif_pts(idx: usize, delay_ms: u32) -> f64 {
    (idx as f64) * (delay_ms as f64) / 1000.0
}

/// gifski settings shared by every GIF encode.  Quality 100 is genuinely
/// lossless for our use case (glyph art has ≤ ~20 unique colours).
fn gifski_settings(width: u32) -> gifski::Settings {
    gifski::Settings {
        width: Some(width),
        height: None,
        quality: 100,
        fast: false,
        repeat: gif::Repeat::Infinite,
    }
}

/// Encode staged frames as one animated GIF with the `gifski` crate.
/// gifski produces ~30-50% smaller files than the old `gif` crate path
/// because it computes a SHARED palette across all frames and uses
/// error-diffusion dithering across the whole sequence.  `width` is the
/// output width (gifski resizes internally, aspect preserved; at Twitter's
/// 720 px kaneki drops from ~30 MB → ~6-8 MB).
///
/// ITER-054 (F13): bounded decode-ahead, the ITER-040 MP4 pattern.  K
/// workers decode frames (`decode_one_frame`, pure per frame) into a
/// sync_channel; a feeder thread hands them to the collector strictly in
/// index order through a reorder buffer, so gifski sees exactly the
/// sequence the old decode-everything-first loop fed it and the output is
/// bit-identical.  A window of 2K+2 frames caps how far workers run ahead,
/// so at most a handful of decoded frames exist at once instead of the whole
/// set (373 MB for 120 × 760×1024).  The twitter-fit ladder re-decodes per
/// rung — a raw RGBA "decode" is one copy, a PNG set ~0.3 s.
fn encode_gif_gifski_frames(
    frames: &[PngFrame],
    delay_ms: u32,
    border: Option<&BorderConfig>,
    width: u32,
    control: &ExportControl,
    progress_phase: &str,
) -> Result<Vec<u8>, String> {
    let n = frames.len();
    if n == 0 {
        return Err("no frames provided".into());
    }
    let (collector, writer) =
        gifski::new(gifski_settings(width)).map_err(|e| format!("gifski init: {}", e))?;

    // BUG-009: leave 2 cores free — a GUI export runs this while the
    // webview's draw loop is live (the loop resumes during encode since
    // v0.1.7); saturating every core starves the UI thread.
    let workers = std::thread::available_parallelism()
        .map(|p| p.get().saturating_sub(2).max(1))
        .unwrap_or(2)
        .min(n);
    let window = 2 * workers + 2;
    let next_index = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    // Frames handed to gifski; a worker holding index >= fed + window waits
    // here until the feeder catches up.
    let fed = (Mutex::new(0usize), std::sync::Condvar::new());
    let (tx, rx) =
        std::sync::mpsc::sync_channel::<(usize, Result<imgref::ImgVec<rgb::RGBA8>, String>)>(window);

    let mut output: Vec<u8> = Vec::new();
    std::thread::scope(|s| -> Result<(), String> {
        for _ in 0..workers {
            let tx = tx.clone();
            let (next_index, stop, fed) = (&next_index, &stop, &fed);
            s.spawn(move || loop {
                let idx = next_index.fetch_add(1, Ordering::Relaxed);
                if idx >= n {
                    return;
                }
                {
                    let mut done = fed.0.lock().unwrap_or_else(|e| e.into_inner());
                    while idx >= *done + window && !stop.load(Ordering::Relaxed) {
                        done = fed.1.wait(done).unwrap_or_else(|e| e.into_inner());
                    }
                }
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                // A panic here would leave the feeder waiting on this index
                // forever; turn it into an ordinary per-frame error instead.
                let result = control.check_cancelled().and_then(|_| {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        decode_one_frame(&frames[idx], idx, border)
                    }))
                    .unwrap_or_else(|_| Err(format!("frame {} decode panicked", idx)))
                });
                let failed = result.is_err();
                if tx.send((idx, result)).is_err() || failed {
                    return;
                }
            });
        }
        drop(tx);

        // Collector and writer must run concurrently: gifski quantises frames
        // as they arrive and the writer streams them out, so the feeder gets
        // its own thread and the writer runs on this one.
        let (stop_ref, fed_ref) = (&stop, &fed);
        let feeder = s.spawn(move || -> Result<(), String> {
            let mut pending: std::collections::BTreeMap<
                usize,
                Result<imgref::ImgVec<rgb::RGBA8>, String>,
            > = std::collections::BTreeMap::new();
            let result = (|| -> Result<(), String> {
                for idx in 0..n {
                    let img = loop {
                        if let Some(ready) = pending.remove(&idx) {
                            break ready?;
                        }
                        let (got, result) = rx
                            .recv()
                            .map_err(|_| format!("frame {} decode worker exited", idx))?;
                        pending.insert(got, result);
                    };
                    control.check_cancelled()?;
                    collector
                        .add_frame_rgba(idx, img, gif_pts(idx, delay_ms))
                        .map_err(|e| format!("frame {} add: {}", idx, e))?;
                    *fed_ref.0.lock().unwrap_or_else(|e| e.into_inner()) = idx + 1;
                    fed_ref.1.notify_all();
                }
                Ok(())
            })();
            drop(collector); // signals the writer to finalise
            // Release any worker parked on the window or blocked on send.
            // Set the flag under the mutex so a worker can't check it, miss
            // this notify, and park forever (lost wakeup).
            {
                let _g = fed_ref.0.lock().unwrap_or_else(|e| e.into_inner());
                stop_ref.store(true, Ordering::Relaxed);
            }
            fed_ref.1.notify_all();
            drop(rx);
            result
        });
        let written = writer.write(
            &mut output,
            &mut GifProgress {
                control: control.clone(),
                phase: progress_phase.to_string(),
                completed: 0,
                total: n,
            },
        );
        let feed = feeder
            .join()
            .map_err(|_| "gifski feeder thread panicked".to_string())?;
        control.check_cancelled()?;
        feed?;
        written.map_err(|e| format!("gifski write: {}", e))
    })?;

    control.check_cancelled()?;
    Ok(output)
}

/// Adaptive Twitter-fit wrapper.  ITER-025 surfaced that the GUI's
/// `Export GIF (Twitter-fit)` button hardcoded 720 px, which routinely
/// overshot Twitter's 15 MB cap on high-density content (cream-paper
/// monochrome / dense glyph stipple).  This wrapper retries the encoder
/// at progressively smaller widths until the output fits, matching the
/// driver-level safety-net pipeline that proved 53 / 53 rescue on the
/// ghost-I.gif 64-variant batch (see ITER-024 / ITER-025).
///
/// `target_max_bytes`: when `Some(N)`, the function encodes at `cap_width`
/// first; on overshoot it predicts the width that fits from that pass's
/// size (`twitter_fit_width`, ITER-051), encodes once there, and if that
/// still overshoots re-predicts from the second pass for one correction
/// rung — at most three full gifski passes, never below
/// `TWITTER_FIT_MIN_WIDTH`.  Returns the first buffer ≤ N; if none fits,
/// returns an error rather than silently producing an upload that violates
/// the requested hard limit.
/// When `None`, behaves identically to `encode_gif_gifski` (single try).
fn encode_gif_gifski_adaptive(
    frames: &[PngFrame],
    delay_ms: u32,
    cap_width: Option<u32>,
    border: Option<&BorderConfig>,
    target_max_bytes: Option<u64>,
    control: &ExportControl,
) -> Result<Vec<u8>, String> {
    control.emit("validate", 0, frames.len());
    let dimensions = validate_export_frames(frames, border)?;
    let first_width = validated_gif_width(dimensions, cap_width)?;
    control.check_cancelled()?;
    control.emit("validate", frames.len(), frames.len());

    // ITER-054: no shared decoded set any more (EXPORT-OPT-2 decoded once for
    // the whole ladder and held all of it); each pass decodes through the
    // bounded window, which costs a raw-RGBA copy or ~0.3 s of PNG decode
    // per extra rung, and only when target_max_bytes is set.
    let t0 = std::time::Instant::now();
    let single = encode_gif_gifski_frames(
        frames,
        delay_ms,
        border,
        first_width,
        control,
        "encode-gif-attempt-1",
    )?;
    eprintln!(
        "encode: {} frames decoded (bounded window) + gifski in {:.0}ms",
        frames.len(),
        t0.elapsed().as_secs_f64() * 1000.0,
    );
    let target = match target_max_bytes {
        Some(t) if t > 0 => t,
        _ => return Ok(single),
    };
    if (single.len() as u64) <= target {
        eprintln!(
            "twitter-fit: cap={:?} size={:.2}MB ≤ target {:.2}MB ✓",
            cap_width,
            single.len() as f64 / 1024.0 / 1024.0,
            target as f64 / 1024.0 / 1024.0,
        );
        return Ok(single);
    }
    twitter_fit_shrink(
        frames,
        delay_ms,
        border,
        dimensions,
        first_width,
        single.len(),
        target,
        control,
    )
}

/// Twitter-fit passes 2-3 after a first pass at `first_width` measured
/// `first_size` bytes > `target`.  Split out of `encode_gif_gifski_adaptive`
/// so the streaming encoder (ITER-053), whose first pass is already on disk,
/// can run the same ladder from its retained frames.  Returns the first
/// buffer ≤ target, or the "cannot meet target size" error.
fn twitter_fit_shrink(
    frames: &[PngFrame],
    delay_ms: u32,
    border: Option<&BorderConfig>,
    dimensions: ExportFrameDimensions,
    first_width: u32,
    first_size: usize,
    target: u64,
    control: &ExportControl,
) -> Result<Vec<u8>, String> {
    // ITER-051 (F14): predict instead of walking the fixed 600/540/480/420/360
    // ladder.  GIF bytes scale ~linearly with pixel count at fixed content
    // (ITER-026 log: 18.54 MB × (600/720)² = 12.87 MB predicted vs 12.82 MB
    // measured), and pixels scale with width², so one pass gives the width.
    // Below ~400 px the linear model over-predicts (LZW + palette do better),
    // which errs toward fitting.
    let initial_cap = first_width;
    let mut best = first_size;
    let pixels = |width: u32| -> f64 {
        let height = (u64::from(dimensions.output_height) * u64::from(width))
            .div_ceil(u64::from(dimensions.output_width));
        frames.len() as f64 * f64::from(width) * height as f64
    };
    eprintln!(
        "twitter-fit: cap={} size={:.2}MB ({:.3} B/px) > target {:.2}MB — entering shrink ladder",
        initial_cap,
        best as f64 / 1024.0 / 1024.0,
        best as f64 / pixels(initial_cap),
        target as f64 / 1024.0 / 1024.0,
    );
    // Pass 2: predicted width × 0.96 (≈8 % byte headroom).  Measured B/px
    // drift between 720 px and the fitted width: +6.4 % on worm-move.gif,
    // +1.9 % on cream-paper, -0.4 % on the ITER-026 sample.  Pass 3
    // (correction): re-predict from pass 2's own size × 0.97 — B/px measured
    // that close to the answer moves < 1 %, and there is no fourth pass.
    let mut width = initial_cap;
    let mut size = best;
    for (attempt, margin) in [(2usize, 0.96f64), (3, 0.97)] {
        control.check_cancelled()?;
        let Some(next) = twitter_fit_width(width, size, target, margin) else {
            break;
        };
        let phase = format!("encode-gif-attempt-{}", attempt);
        let encode_width = validated_gif_width(dimensions, Some(next))?;
        let buf =
            encode_gif_gifski_frames(frames, delay_ms, border, encode_width, control, &phase)?;
        eprintln!(
            "twitter-fit: predicted cap={} size={:.2}MB ({:.3} B/px)",
            next,
            buf.len() as f64 / 1024.0 / 1024.0,
            buf.len() as f64 / pixels(next),
        );
        width = next;
        size = buf.len();
        best = best.min(buf.len());
        // A fitting pass is also the smallest so far: every earlier one
        // was over the target.
        if (buf.len() as u64) <= target {
            eprintln!("twitter-fit: cap={} ✓", next);
            return Ok(buf);
        }
    }
    Err(format!(
        "GIF cannot meet target size: smallest attempt is {} bytes, target is {} bytes",
        best,
        target
    ))
}

/// Narrowest width the Twitter-fit predictor will try (the old ladder's
/// last rung); below it glyph cells stop being legible.
const TWITTER_FIT_MIN_WIDTH: u32 = 360;

/// Width predicted to bring a GIF that measured `bytes` at `width` under
/// `target`, assuming bytes ∝ width² (pixels at a fixed aspect), scaled by
/// `margin` and rounded down to even.  Clamped to
/// [`TWITTER_FIT_MIN_WIDTH`, largest even width < `width`]; `None` when that
/// range is empty, i.e. there is nothing narrower left to try.
fn twitter_fit_width(width: u32, bytes: usize, target: u64, margin: f64) -> Option<u32> {
    let upper = width.saturating_sub(1) & !1;
    if upper < TWITTER_FIT_MIN_WIDTH || bytes == 0 {
        return None;
    }
    let predicted = f64::from(width) * (target as f64 / bytes as f64).sqrt() * margin;
    let predicted = (predicted.max(0.0) as u32) & !1;
    Some(predicted.clamp(TWITTER_FIT_MIN_WIDTH, upper))
}

/// Save a recorded sequence of PNG frames as a single animated GIF.
/// Pops the native save dialog and writes the encoded bytes there.
/// `cap_width` (optional) resizes frames before encoding for smaller
/// output files (Twitter's 15 MB GIF limit, etc.).
/// `target_max_bytes` (optional, ITER-026): when set, shrinks the width
/// until output ≤ N — predicted from the first pass, plus at most one
/// correction (ITER-051).  The GUI's `Export GIF (Twitter-fit)` button
/// passes 15 MB.
#[tauri::command]
async fn save_gif_real(
    app: tauri::AppHandle,
    frames: Vec<GifFrame>,
    delay_ms: u32,
    cap_width: Option<u32>,
    border: Option<BorderConfig>,
    target_max_bytes: Option<u64>,
    job_id: Option<String>,
) -> Result<String, String> {
    // Ask first: cancelling the dialog must not decode or encode any frames.
    let path: Option<PathBuf> = app
        .dialog()
        .file()
        .add_filter("Animated GIF", &["gif"])
        .set_file_name("glyph-loop.gif")
        .blocking_save_file()
        .map(|fp| fp.into_path().ok())
        .flatten();

    let p = path.ok_or_else(|| "cancelled".to_string())?;
    let (control, registration) = register_export(app, job_id);
    tauri::async_runtime::spawn_blocking(move || {
        let _registration = registration;
        let frames = decode_frame_payloads(frames)?;
        let gif_buf = encode_gif_gifski_adaptive(
            &frames,
            delay_ms,
            cap_width,
            border.as_ref(),
            target_max_bytes,
            &control,
        )?;
        atomic_write_output(&p, &gif_buf, &control)?;
        Ok::<String, String>(p.display().to_string())
    })
    .await
    .map_err(|e| format!("GIF export worker failed: {}", e))?
}

/// Batch-mode GIF write: encode frames and write directly to an absolute
/// path with no save dialog.  Used by the test batch driver and the
/// "Export GIF" button when running headlessly.
#[tauri::command]
async fn save_gif_to_path(
    app: tauri::AppHandle,
    frames: Vec<GifFrame>,
    delay_ms: u32,
    path: String,
    cap_width: Option<u32>,
    border: Option<BorderConfig>,
    target_max_bytes: Option<u64>,
    job_id: Option<String>,
) -> Result<String, String> {
    let p = PathBuf::from(&path);
    let (control, registration) = register_export(app, job_id);
    tauri::async_runtime::spawn_blocking(move || {
        let _registration = registration;
        let frames = decode_frame_payloads(frames)?;
        let gif_buf = encode_gif_gifski_adaptive(
            &frames,
            delay_ms,
            cap_width,
            border.as_ref(),
            target_max_bytes,
            &control,
        )?;
        atomic_write_output(&p, &gif_buf, &control)?;
        Ok::<String, String>(p.display().to_string())
    })
    .await
    .map_err(|e| format!("GIF export worker failed: {}", e))?
}

/// Encode a recorded sequence of PNG frames as an MP4/H.264 video using the
/// `openh264` (Cisco's BSD-licensed encoder) + `mp4` (ISO-MP4 muxer) crates.
/// This path exists because Instagram (Reels / Stories / feed posts) strips
/// uploaded GIFs — only MP4 plays back inline.  For a 90-frame loop the
/// output is typically ~1-3 MB, well under any platform limit.
///
/// `cap_width` resizes frames to that width (preserving aspect, rounded to
/// even because yuv420p requires even dimensions).  `None` keeps the source
/// size (still rounded to even).
fn mp4_timing(fps: u32) -> Result<(u32, u32), String> {
    if fps == 0 {
        Err("fps must be > 0".into())
    } else {
        Ok((fps, 1))
    }
}

fn mp4_sample_timing(index: usize, frame_duration: u32) -> (u64, u32) {
    (index as u64 * u64::from(frame_duration), frame_duration)
}

/// MP4 rate-control profile (F11 / ITER-050).
///   - `Upload` (default): openh264 bitrate RC at `mp4_upload_bitrate`, frame
///     skipping off — a few MB for a typical loop, sized for social uploads.
///   - `Archive`: the pre-v0.2 path, RC Off = fixed QP 26 (openh264's
///     SVC_QUALITY_BASE_QP).  Near-lossless for glyph art and bit-identical
///     to v0.1.x output, at ~30-60 Mbps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mp4Quality {
    Upload,
    Archive,
}

impl Mp4Quality {
    /// `None` / empty → `Upload`.  Unknown values are an error rather than a
    /// silent fallback, so a typo in a batch manifest can't change the output.
    fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") | Some("upload") => Ok(Self::Upload),
            Some("archive") => Ok(Self::Archive),
            Some(other) => Err(format!(
                "unknown mp4 quality {:?} (expected \"upload\" or \"archive\")",
                other
            )),
        }
    }
}

/// Upload-mode target bitrate: 5 Mbps up to 1080p, scaled by pixel count
/// above that so 1440p/4K exports don't get starved.
fn mp4_upload_bitrate(width: u32, height: u32) -> u32 {
    const BASE_BPS: u64 = 5_000_000;
    const BASE_PIXELS: u64 = 1920 * 1080;
    let pixels = (u64::from(width) * u64::from(height)).max(BASE_PIXELS);
    (BASE_BPS * pixels / BASE_PIXELS).min(u64::from(u32::MAX)) as u32
}

fn encode_mp4_h264(
    frames: &[PngFrame],
    fps: u32,
    cap_width: Option<u32>,
    border: Option<&BorderConfig>,
    quality: Mp4Quality,
    control: &ExportControl,
) -> Result<Vec<u8>, String> {
    use mp4::{AvcConfig, Mp4Config, Mp4Sample, Mp4Writer, TrackConfig};
    use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, RateControlMode};
    use openh264::formats::{RgbSliceU8, YUVBuffer};
    use openh264::OpenH264API;

    control.emit("validate", 0, frames.len());
    let dimensions = validate_export_frames(frames, border)?;
    control.check_cancelled()?;
    control.emit("validate", frames.len(), frames.len());
    let (timescale, frame_duration) = mp4_timing(fps)?;

    // The shared header guard has already established consistent dimensions,
    // including the optional border, without decompressing a frame.
    let src_w = dimensions.output_width;
    let src_h = dimensions.output_height;

    // yuv420p requires even W and H.  When cap_width is set and smaller than
    // the source, scale H proportionally and round both to even.
    fn even(v: u32) -> u32 {
        v & !1
    }
    let (target_w, target_h) = match cap_width {
        Some(cap) if cap > 0 && cap < src_w => {
            let scale = cap as f64 / src_w as f64;
            (even(cap), even((src_h as f64 * scale).round() as u32))
        }
        _ => (even(src_w), even(src_h)),
    };
    if target_w == 0 || target_h == 0 {
        return Err(format!("invalid output dims: {}x{}", target_w, target_h));
    }
    if target_w > u16::MAX as u32 || target_h > u16::MAX as u32 {
        return Err(format!(
            "MP4 output dimensions {}x{} exceed encoder limit {}",
            target_w,
            target_h,
            u16::MAX
        ));
    }

    // Configure the encoder explicitly (F11 / ITER-050):
    //   - Frame skipping OFF.  The "zero NAL units for some frames" breakage
    //     that once forced RC Off was openh264 *skipping* frames to hold the
    //     bitrate (the crate defaults `enable_skip_frame: true`).  With skip
    //     off every encode() yields a slice; the "no slice NALs" check below
    //     stays as the tripwire.
    //   - Upload: bitrate RC at `mp4_upload_bitrate`.  Without skipping, RC
    //     steers QP and can overshoot the target on high-entropy sources.
    //   - Archive: RC Off = fixed QP 26 — the bitrate field is never read on
    //     that path (ratectl.cpp WelsRcPictureInitDisable).  Kept verbatim so
    //     archive output is bit-identical to v0.1.x.
    //   - Max frame rate = the export fps — the playback target RC budgets
    //     per-frame bits against.
    let cfg = match quality {
        Mp4Quality::Upload => EncoderConfig::new()
            .max_frame_rate(FrameRate::from_hz(fps as f32))
            .bitrate(BitRate::from_bps(mp4_upload_bitrate(target_w, target_h)))
            .rate_control_mode(RateControlMode::Bitrate)
            .skip_frames(false),
        Mp4Quality::Archive => EncoderConfig::new()
            .max_frame_rate(FrameRate::from_hz(fps as f32))
            .bitrate(BitRate::from_bps(5_000_000))
            .rate_control_mode(RateControlMode::Off),
    };
    let mut encoder = Encoder::with_api_config(OpenH264API::from_source(), cfg)
        .map_err(|e| format!("openh264 init: {}", e))?;

    // ITER-040: bounded preprocess pipeline.  PNG decode, border, Lanczos3
    // resize and RGB->YUV are pure per-frame work, so K worker threads run
    // them while this thread feeds the stateful H.264 encoder strictly in
    // index order through a reorder buffer — the bitstream is identical to
    // the old sequential loop.  A window of 2K+2 frames caps how far workers
    // may run ahead of the encoder, so peak RAM stays a handful of YUV frames
    // rather than the v0.1.5 design that retained an RGB Vec for every frame.
    // The command runs on Tauri's blocking pool, so this never blocks async.
    let t_encode = std::time::Instant::now();
    let n = frames.len();
    // BUG-009: leave 2 cores free for the live webview draw loop.
    let workers = std::thread::available_parallelism()
        .map(|p| p.get().saturating_sub(2).max(1))
        .unwrap_or(2)
        .min(n.max(1));
    let window = 2 * workers + 2;
    let next_index = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    // Frames consumed by the encoder; a worker holding index >= encoded +
    // window waits here until the encoder catches up.
    let encoded = (Mutex::new(0usize), std::sync::Condvar::new());
    let (tx, rx) = std::sync::mpsc::sync_channel::<(usize, Result<YUVBuffer, String>)>(window);

    let preprocess = |idx: usize| -> Result<YUVBuffer, String> {
        let mut rgba_img = frames[idx].to_rgba8(idx)?;
        if let Some(b) = border {
            rgba_img = apply_border(rgba_img, b);
        }
        let img_dyn = image::DynamicImage::ImageRgba8(rgba_img);
        let img_dyn = if img_dyn.width() != target_w || img_dyn.height() != target_h {
            img_dyn.resize_exact(target_w, target_h, image::imageops::FilterType::Lanczos3)
        } else {
            img_dyn
        };
        let rgb_data = img_dyn.to_rgb8().into_raw();
        let rgb_source = RgbSliceU8::new(&rgb_data, (target_w as usize, target_h as usize));
        Ok(YUVBuffer::from_rgb_source(rgb_source))
    };

    // Walk frames, encode, capture SPS/PPS once and retain only the much
    // smaller compressed samples for the in-memory MP4 muxer.
    let mut sps: Option<Vec<u8>> = None;
    let mut pps: Option<Vec<u8>> = None;
    let mut samples: Vec<(Vec<u8>, bool)> = Vec::with_capacity(n);
    let mut encode_ms = 0.0f64;

    std::thread::scope(|s| -> Result<(), String> {
        for _ in 0..workers {
            let tx = tx.clone();
            let (next_index, stop, encoded, preprocess) =
                (&next_index, &stop, &encoded, &preprocess);
            s.spawn(move || loop {
                let idx = next_index.fetch_add(1, Ordering::Relaxed);
                if idx >= n {
                    return;
                }
                {
                    let mut done = encoded.0.lock().unwrap_or_else(|e| e.into_inner());
                    while idx >= *done + window && !stop.load(Ordering::Relaxed) {
                        done = encoded.1.wait(done).unwrap_or_else(|e| e.into_inner());
                    }
                }
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                // A panic here would leave the encoder waiting on this index
                // forever; turn it into an ordinary per-frame error instead.
                let result = control.check_cancelled().and_then(|_| {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| preprocess(idx)))
                        .unwrap_or_else(|_| Err(format!("frame {} preprocess panicked", idx)))
                });
                let failed = result.is_err();
                if tx.send((idx, result)).is_err() || failed {
                    return;
                }
            });
        }
        drop(tx);

        let mut pending: std::collections::BTreeMap<usize, Result<YUVBuffer, String>> =
            std::collections::BTreeMap::new();
        let mut run = || -> Result<(), String> {
            for idx in 0..n {
                let yuv = loop {
                    if let Some(ready) = pending.remove(&idx) {
                        break ready?;
                    }
                    let (got, result) = rx
                        .recv()
                        .map_err(|_| format!("frame {} preprocess worker exited", idx))?;
                    pending.insert(got, result);
                };
                control.check_cancelled()?;
                let t_frame = std::time::Instant::now();
                let bitstream = encoder
                    .encode(&yuv)
                    .map_err(|e| format!("frame {} encode: {}", idx, e))?;
                drop(yuv);

                // openh264 emits each NAL with a 4-byte Annex-B start code 00 00 00 01.
                // For MP4/AVCC we need: SPS (NAL type 7) and PPS (NAL type 8) extracted
                // and stored in AvcConfig once; slice NALs (type 5 = IDR, type 1 =
                // non-IDR) length-prefixed (4-byte big-endian) and concatenated as the
                // sample payload.
                let mut sample_bytes: Vec<u8> = Vec::new();
                let mut is_idr = false;
                let mut nal_types_seen: Vec<u8> = Vec::new();

                for li in 0..bitstream.num_layers() {
                    let layer = bitstream
                        .layer(li)
                        .ok_or_else(|| format!("frame {} layer {} missing", idx, li))?;
                    for ni in 0..layer.nal_count() {
                        let nal = layer
                            .nal_unit(ni)
                            .ok_or_else(|| format!("frame {} layer {} nal {} missing", idx, li, ni))?;
                        let payload: &[u8] = if nal.starts_with(&[0, 0, 0, 1]) {
                            &nal[4..]
                        } else if nal.starts_with(&[0, 0, 1]) {
                            &nal[3..]
                        } else {
                            return Err(format!(
                                "frame {} unexpected NAL prefix: {:?}",
                                idx,
                                &nal[..nal.len().min(5)]
                            ));
                        };
                        if payload.is_empty() {
                            continue;
                        }
                        let nal_type = payload[0] & 0x1F;
                        nal_types_seen.push(nal_type);
                        match nal_type {
                            7 => {
                                if sps.is_none() {
                                    sps = Some(payload.to_vec());
                                }
                            }
                            8 => {
                                if pps.is_none() {
                                    pps = Some(payload.to_vec());
                                }
                            }
                            5 => {
                                // IDR slice — keyframe.
                                is_idr = true;
                                sample_bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
                                sample_bytes.extend_from_slice(payload);
                            }
                            1 | 2 | 3 | 4 => {
                                // Non-IDR slice variants: 1=non-IDR, 2-4=DPA/B/C
                                // (data-partitioned slice types).  All count as slice
                                // payload for MP4 sample bytes.
                                sample_bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
                                sample_bytes.extend_from_slice(payload);
                            }
                            // Skip SEI (6) / AUD (9) / filler (12) / other NALs —
                            // the AVC1 file format doesn't require them, and including
                            // them in samples can confuse strict players.
                            _ => {}
                        }
                    }
                }

                if sample_bytes.is_empty() {
                    return Err(format!(
                        "frame {} produced no slice NALs (saw NAL types {:?})",
                        idx, nal_types_seen
                    ));
                }
                samples.push((sample_bytes, is_idr));
                control.emit("encode-mp4", idx + 1, n);
                encode_ms += t_frame.elapsed().as_secs_f64() * 1000.0;
                *encoded.0.lock().unwrap_or_else(|e| e.into_inner()) = idx + 1;
                encoded.1.notify_all();
            }
            Ok(())
        };
        let result = run();
        // Release any worker parked on the window or blocked on send. Set the
        // flag under the mutex so a worker can't check it, miss this notify,
        // and park forever (lost wakeup).
        {
            let _g = encoded.0.lock().unwrap_or_else(|e| e.into_inner());
            stop.store(true, Ordering::Relaxed);
        }
        encoded.1.notify_all();
        drop(rx);
        result
    })?;

    eprintln!(
        "encode-mp4: {} frames decoded, preprocessed, and encoded in {:.0}ms ({} preprocess workers, openh264 {:.0}ms, quality {:?})",
        n,
        t_encode.elapsed().as_secs_f64() * 1000.0,
        workers,
        encode_ms,
        quality,
    );

    let sps = sps.ok_or("no SPS NAL emitted by encoder")?;
    let pps = pps.ok_or("no PPS NAL emitted by encoder")?;

    // Mux to MP4 in memory.
    let mp4_config = Mp4Config {
        major_brand: "isom".parse().map_err(|_| "isom brand parse")?,
        minor_version: 512,
        compatible_brands: vec![
            "isom".parse().map_err(|_| "isom brand parse")?,
            "iso2".parse().map_err(|_| "iso2 brand parse")?,
            "avc1".parse().map_err(|_| "avc1 brand parse")?,
            "mp41".parse().map_err(|_| "mp41 brand parse")?,
        ],
        timescale,
    };
    let buf = std::io::Cursor::new(Vec::<u8>::new());
    let mut writer =
        Mp4Writer::write_start(buf, &mp4_config).map_err(|e| format!("mp4 write_start: {}", e))?;

    let mut track_config = TrackConfig::from(AvcConfig {
        width: target_w as u16,
        height: target_h as u16,
        seq_param_set: sps,
        pic_param_set: pps,
    });
    // mp4 0.14 leaves the track (mdhd) timescale at 1000 regardless of the
    // movie timescale, so `frame_duration = 1` meant 1 ms per frame: a
    // 120-frame clip claimed 0.12 s at 1000 fps (ITER-050).
    track_config.timescale = timescale;
    writer
        .add_track(&track_config)
        .map_err(|e| format!("mp4 add_track: {}", e))?;

    for (idx, (bytes, is_idr)) in samples.into_iter().enumerate() {
        control.check_cancelled()?;
        let (start_time, duration) = mp4_sample_timing(idx, frame_duration);
        let sample = Mp4Sample {
            start_time,
            duration,
            rendering_offset: 0,
            is_sync: is_idr,
            bytes: bytes.into(),
        };
        writer
            .write_sample(1, &sample)
            .map_err(|e| format!("write_sample {}: {}", idx, e))?;
    }

    writer
        .write_end()
        .map_err(|e| format!("mp4 write_end: {}", e))?;
    control.check_cancelled()?;
    Ok(writer.into_writer().into_inner())
}

/// Save a recorded sequence of PNG frames as a single MP4/H.264 video.
/// Pops the native save dialog and writes the encoded bytes there.
#[tauri::command]
async fn save_mp4_real(
    app: tauri::AppHandle,
    frames: Vec<GifFrame>,
    fps: u32,
    cap_width: Option<u32>,
    border: Option<BorderConfig>,
    mp4_quality: Option<String>,
    job_id: Option<String>,
) -> Result<String, String> {
    let quality = Mp4Quality::parse(mp4_quality.as_deref())?;
    // Ask first: cancelling the dialog must not decode or encode any frames.
    let path: Option<PathBuf> = app
        .dialog()
        .file()
        .add_filter("MP4 video", &["mp4"])
        .set_file_name("glyph-loop.mp4")
        .blocking_save_file()
        .map(|fp| fp.into_path().ok())
        .flatten();

    let p = path.ok_or_else(|| "cancelled".to_string())?;
    let (control, registration) = register_export(app, job_id);
    tauri::async_runtime::spawn_blocking(move || {
        let _registration = registration;
        let frames = decode_frame_payloads(frames)?;
        let mp4_buf = encode_mp4_h264(
            &frames,
            fps,
            cap_width,
            border.as_ref(),
            quality,
            &control,
        )?;
        atomic_write_output(&p, &mp4_buf, &control)?;
        Ok::<String, String>(p.display().to_string())
    })
    .await
    .map_err(|e| format!("MP4 export worker failed: {}", e))?
}

/// Batch-mode MP4 write: encode frames and write to an absolute path with
/// no save dialog.  Used by `runBatchExport` and the headless `--format mp4`
/// CLI path.
#[tauri::command]
async fn save_mp4_to_path(
    app: tauri::AppHandle,
    frames: Vec<GifFrame>,
    fps: u32,
    path: String,
    cap_width: Option<u32>,
    border: Option<BorderConfig>,
    mp4_quality: Option<String>,
    job_id: Option<String>,
) -> Result<String, String> {
    let quality = Mp4Quality::parse(mp4_quality.as_deref())?;
    let p = PathBuf::from(&path);
    let (control, registration) = register_export(app, job_id);
    tauri::async_runtime::spawn_blocking(move || {
        let _registration = registration;
        let frames = decode_frame_payloads(frames)?;
        let mp4_buf = encode_mp4_h264(
            &frames,
            fps,
            cap_width,
            border.as_ref(),
            quality,
            &control,
        )?;
        atomic_write_output(&p, &mp4_buf, &control)?;
        Ok::<String, String>(p.display().to_string())
    })
    .await
    .map_err(|e| format!("MP4 export worker failed: {}", e))?
}

/// Choose the destination before frame capture starts. This lightweight
/// bridge lets the frontend avoid recording an animation the user will
/// immediately discard by cancelling a later save dialog.
#[tauri::command]
async fn choose_export_path(
    app: tauri::AppHandle,
    format: String,
    suggested_name: String,
) -> Result<String, String> {
    let format = format.to_ascii_lowercase();
    let path = match format.as_str() {
        "gif" => app
            .dialog()
            .file()
            .add_filter("Animated GIF", &["gif"])
            .set_file_name(&suggested_name)
            .blocking_save_file(),
        "mp4" => app
            .dialog()
            .file()
            .add_filter("MP4 video", &["mp4"])
            .set_file_name(&suggested_name)
            .blocking_save_file(),
        _ => return Err(format!("unsupported export format: {}", format)),
    }
    .and_then(|file_path| file_path.into_path().ok())
    .ok_or_else(|| "cancelled".to_string())?;
    Ok(path.display().to_string())
}

/// Finish a streaming GIF job (ITER-053): wait for the encoder's tail, then
/// commit its `.part` — or, when a byte target is set and the streamed pass
/// overshot it, run the twitter-fit shrink from the frames the encoder
/// retained and write that result through `atomic_write_output`.
fn finish_gif_stream(
    stream: GifStream,
    finish_config: GifStreamConfig,
    control: &ExportControl,
) -> Result<String, String> {
    if finish_config != stream.config {
        // Dropping `stream` cancels the encoder and removes its .part.
        return Err("finish_gif_export settings differ from begin_export_capture".into());
    }
    let config = stream.config.clone();
    let t0 = std::time::Instant::now();
    let streamed = stream.finish()?;
    let path = PathBuf::from(&config.path);
    let result = (|| -> Result<String, String> {
        control.check_cancelled()?;
        eprintln!(
            "encode: {} frames streamed into gifski; tail after capture {:.0}ms",
            streamed.frames,
            t0.elapsed().as_secs_f64() * 1000.0,
        );
        let target = config.target_max_bytes.filter(|t| *t > 0);
        if target.map_or(true, |t| streamed.size <= t) {
            if let Some(t) = target {
                eprintln!(
                    "twitter-fit: cap={:?} size={:.2}MB ≤ target {:.2}MB ✓",
                    config.cap_width,
                    streamed.size as f64 / 1024.0 / 1024.0,
                    t as f64 / 1024.0 / 1024.0,
                );
            }
            control.emit("write", 0, 1);
            commit_output_part(&streamed.part_path, &path, control)?;
            return Ok(path.display().to_string());
        }
        let _ = fs::remove_file(&streamed.part_path);
        let frames = &streamed.retained;
        let border = config.border.as_ref();
        let dimensions = validate_export_frames(frames, border)?;
        let gif_buf = twitter_fit_shrink(
            frames,
            config.delay_ms,
            border,
            dimensions,
            streamed.width,
            streamed.size as usize,
            target.expect("overshoot implies a target"),
            control,
        )?;
        atomic_write_output(&path, &gif_buf, control)?;
        Ok(path.display().to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&streamed.part_path);
    }
    result
}

#[tauri::command]
async fn finish_gif_export(
    app: tauri::AppHandle,
    job_id: String,
    path: String,
    delay_ms: u32,
    cap_width: Option<u32>,
    border: Option<BorderConfig>,
    target_max_bytes: Option<u64>,
) -> Result<String, String> {
    // ITER-053: a streaming capture registers the state its encoder thread
    // watches, so cancel during the tail aborts gifski.
    let (control, registration) = match capture_stream_state(&job_id) {
        Some(state) => register_export_state(app, Some(job_id.clone()), state),
        None => register_export(app, Some(job_id.clone())),
    };
    let capture = take_export_capture(&job_id)?;
    if let Some(stream) = capture.stream {
        let finish_config = GifStreamConfig {
            path,
            delay_ms,
            cap_width,
            border,
            target_max_bytes,
        };
        return tauri::async_runtime::spawn_blocking(move || {
            let _registration = registration;
            finish_gif_stream(stream, finish_config, &control)
        })
        .await
        .map_err(|e| format!("GIF export worker failed: {}", e))?;
    }
    let frames = capture.frames;
    let path = PathBuf::from(path);
    tauri::async_runtime::spawn_blocking(move || {
        let _registration = registration;
        let gif_buf = encode_gif_gifski_adaptive(
            &frames,
            delay_ms,
            cap_width,
            border.as_ref(),
            target_max_bytes,
            &control,
        )?;
        atomic_write_output(&path, &gif_buf, &control)?;
        Ok::<String, String>(path.display().to_string())
    })
    .await
    .map_err(|e| format!("GIF export worker failed: {}", e))?
}

#[tauri::command]
async fn finish_mp4_export(
    app: tauri::AppHandle,
    job_id: String,
    path: String,
    fps: u32,
    cap_width: Option<u32>,
    border: Option<BorderConfig>,
    mp4_quality: Option<String>,
) -> Result<String, String> {
    let (control, registration) = register_export(app, Some(job_id.clone()));
    let capture = take_export_capture(&job_id)?;
    if capture.stream.is_some() {
        // Dropping the capture cancels the GIF encoder and removes its .part.
        return Err("export capture was started as a streaming GIF".into());
    }
    let frames = capture.frames;
    // Parsed after the capture is taken so a bad value still releases it.
    let quality = Mp4Quality::parse(mp4_quality.as_deref())?;
    let path = PathBuf::from(path);
    tauri::async_runtime::spawn_blocking(move || {
        let _registration = registration;
        let mp4_buf = encode_mp4_h264(
            &frames,
            fps,
            cap_width,
            border.as_ref(),
            quality,
            &control,
        )?;
        atomic_write_output(&path, &mp4_buf, &control)?;
        Ok::<String, String>(path.display().to_string())
    })
    .await
    .map_err(|e| format!("MP4 export worker failed: {}", e))?
}

/// Save an arbitrary blob (e.g. an already-assembled GIF or ZIP) to a path
/// chosen via native save dialog.  Kept for back-compat with the ZIP path.
#[tauri::command]
async fn save_blob(
    app: tauri::AppHandle,
    bytes_b64: String,
    suggested_name: String,
    extension: String,
) -> Result<String, String> {
    let bytes = general_purpose::STANDARD
        .decode(&bytes_b64)
        .map_err(|e| e.to_string())?;
    let path: Option<PathBuf> = app
        .dialog()
        .file()
        .add_filter(&format!("{} file", extension.to_uppercase()), &[&extension])
        .set_file_name(&suggested_name)
        .blocking_save_file()
        .map(|fp| fp.into_path().ok())
        .flatten();
    if let Some(p) = path {
        fs::write(&p, &bytes).map_err(|e| e.to_string())?;
        Ok(p.display().to_string())
    } else {
        Err("cancelled".into())
    }
}

/// Open a native file-picker dialog for an image, return its bytes as a
/// data URL the JS side can hand to `loadImage`.
#[tauri::command]
async fn pick_image(app: tauri::AppHandle) -> Result<String, String> {
    let path = app
        .dialog()
        .file()
        .add_filter(
            "Image",
            &["png", "jpg", "jpeg", "webp", "gif", "bmp", "tiff", "avif"],
        )
        .blocking_pick_file()
        .and_then(|fp| fp.into_path().ok());

    match path {
        Some(p) => read_path_as_data_url(&p),
        None => Err("cancelled".into()),
    }
}

/// Same as `pick_image` but returns both the absolute path and the data URL,
/// so the JS side can persist a "recent sources" list keyed on path.
#[tauri::command]
async fn pick_image_with_path(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    let path = app
        .dialog()
        .file()
        .add_filter(
            "Image",
            &["png", "jpg", "jpeg", "webp", "gif", "bmp", "tiff", "avif"],
        )
        .blocking_pick_file()
        .and_then(|fp| fp.into_path().ok());

    match path {
        Some(p) => {
            let image = read_path_as_data_url_with_metadata(&p)?;
            Ok(serde_json::json!({
                "path": p.to_string_lossy().to_string(),
                "dataUrl": image.data_url,
                "sizeBytes": image.size_bytes,
                "mimeType": image.mime_type
            }))
        }
        None => Err("cancelled".into()),
    }
}

/// Read an image file from an absolute path and return a metadata payload.
/// Used by the Tauri drag-drop event fallback (when `dragDropEnabled: true`).
#[tauri::command]
async fn read_image_file(path: String) -> Result<serde_json::Value, String> {
    let p = PathBuf::from(&path);
    let image = read_path_as_data_url_with_metadata(&p)?;
    Ok(serde_json::json!({
        "dataUrl": image.data_url,
        "sizeBytes": image.size_bytes,
        "mimeType": image.mime_type
    }))
}

/// ---- Rust-backed key-value persistence (AUDIT-2026-06-10 / BUG-006) ----
///
/// WKWebView does NOT persist localStorage for Tauri's custom-scheme origin:
/// ~/Library/WebKit/<bundle-id>/WebsiteData/LocalStorage/ has been empty since
/// first install, so user presets ("Save current") and the recent-sources list
/// silently vanished on every relaunch.  The JS side keeps using localStorage
/// as its in-session store but mirrors every write here; on startup it seeds
/// localStorage from this file.  Storage: one JSON object map in
/// app_config_dir()/persist.json, written atomically (tmp + rename).
fn persist_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("app_config_dir: {}", e))?;
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {:?}: {}", dir, e))?;
    Ok(dir.join("persist.json"))
}

#[tauri::command]
fn kv_load_all(app: tauri::AppHandle) -> Result<String, String> {
    let p = persist_path(&app)?;
    if !p.exists() {
        return Ok("{}".into());
    }
    fs::read_to_string(&p).map_err(|e| format!("read {:?}: {}", p, e))
}

#[tauri::command]
fn kv_save(app: tauri::AppHandle, key: String, value: Option<String>) -> Result<(), String> {
    let p = persist_path(&app)?;
    let mut map: serde_json::Map<String, serde_json::Value> = if p.exists() {
        let raw = fs::read_to_string(&p).map_err(|e| format!("read {:?}: {}", p, e))?;
        serde_json::from_str(&raw).unwrap_or_default()
    } else {
        Default::default()
    };
    match value {
        Some(v) => {
            map.insert(key, serde_json::Value::String(v));
        }
        None => {
            map.remove(&key);
        }
    }
    let body = serde_json::to_string(&map).map_err(|e| e.to_string())?;
    let tmp = p.with_extension("json.tmp");
    fs::write(&tmp, body).map_err(|e| format!("write {:?}: {}", tmp, e))?;
    fs::rename(&tmp, &p).map_err(|e| format!("rename {:?}: {}", tmp, e))?;
    Ok(())
}

struct ImageDataUrl {
    data_url: String,
    size_bytes: u64,
    mime_type: &'static str,
}

fn image_mime_type(p: &PathBuf) -> &'static str {
    match p
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("bmp") => "image/bmp",
        Some("tiff") => "image/tiff",
        Some("avif") => "image/avif",
        _ => "image/png",
    }
}

fn read_path_as_data_url_with_metadata(p: &PathBuf) -> Result<ImageDataUrl, String> {
    let size_bytes = fs::metadata(p)
        .map_err(|e| format!("could not inspect {:?}: {}", p, e))?
        .len();
    if size_bytes > MAX_SOURCE_FILE_BYTES {
        return Err(format!(
            "image file is too large: {} bytes exceeds import limit {} bytes",
            size_bytes, MAX_SOURCE_FILE_BYTES
        ));
    }
    let bytes = fs::read(p).map_err(|e| e.to_string())?;
    let mime_type = image_mime_type(p);
    let b64 = general_purpose::STANDARD.encode(&bytes);
    Ok(ImageDataUrl {
        data_url: format!("data:{};base64,{}", mime_type, b64),
        size_bytes,
        mime_type,
    })
}

fn read_path_as_data_url(p: &PathBuf) -> Result<String, String> {
    read_path_as_data_url_with_metadata(p).map(|image| image.data_url)
}

/// Save preset JSON via dialog.
#[tauri::command]
async fn save_preset_json(app: tauri::AppHandle, json: String) -> Result<String, String> {
    let path: Option<PathBuf> = app
        .dialog()
        .file()
        .add_filter("Preset JSON", &["json"])
        .set_file_name("glyph-preset.json")
        .blocking_save_file()
        .map(|fp| fp.into_path().ok())
        .flatten();
    if let Some(p) = path {
        fs::write(&p, json.as_bytes()).map_err(|e| e.to_string())?;
        Ok(p.display().to_string())
    } else {
        Err("cancelled".into())
    }
}

/// Load preset JSON via dialog.
#[tauri::command]
async fn load_preset_json(app: tauri::AppHandle) -> Result<String, String> {
    let path = app
        .dialog()
        .file()
        .add_filter("Preset JSON", &["json"])
        .blocking_pick_file()
        .and_then(|fp| fp.into_path().ok());
    match path {
        Some(p) => fs::read_to_string(&p).map_err(|e| e.to_string()),
        None => Err("cancelled".into()),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // GUI mode also registers the CLI commands so the JS hook can poll
    // `get_cli_render_job` cleanly (returns null in GUI mode).
    let empty_cli_state = CliJobState {
        job: Mutex::new(None),
        exit_code: Arc::new(Mutex::new(0)),
    };
    run_tauri(empty_cli_state, false);
}

/// Open USER-GUIDE.md in the user's default markdown / text app.  Tries
/// the bundled resource path first (production .app), then falls back to
/// the project root for `cargo tauri dev` runs.  Uses tauri-plugin-shell
/// `open` which on macOS invokes Launch Services (`open` command), on
/// Linux `xdg-open`, on Windows `ShellExecute`.
#[tauri::command]
async fn open_user_guide(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    use tauri_plugin_shell::ShellExt;

    // 1) Try the bundled resource path (production .app — see tauri.conf.json
    //    bundle.resources entry).  resource_dir is the .app/Contents/Resources
    //    directory on macOS.  Tauri 2 places parent-relative resources
    //    (`../USER-GUIDE.md`) under the `_up_/` subfolder, so check both.
    let mut candidate: Option<PathBuf> = None;
    if let Ok(resource_dir) = app.path().resource_dir() {
        for sub in &["USER-GUIDE.md", "_up_/USER-GUIDE.md"] {
            let p = resource_dir.join(sub);
            if p.exists() {
                candidate = Some(p);
                break;
            }
        }
    }
    // 2) Fall back to the project-root path for dev / cargo tauri dev runs.
    if candidate.is_none() {
        if let Ok(cwd) = std::env::current_dir() {
            let p = cwd.join("USER-GUIDE.md");
            if p.exists() {
                candidate = Some(p);
            }
            // Also try parent (in case current dir is src-tauri/)
            if candidate.is_none() {
                let parent_p = cwd.parent().map(|d| d.join("USER-GUIDE.md"));
                if let Some(pp) = parent_p {
                    if pp.exists() {
                        candidate = Some(pp);
                    }
                }
            }
        }
    }

    let path = candidate
        .ok_or_else(|| "USER-GUIDE.md not found in resources or project root".to_string())?;
    app.shell()
        .open(path.to_string_lossy().into_owned(), None)
        .map_err(|e| format!("shell.open failed: {}", e))
}

/// Shared Tauri builder used by both `run()` (GUI) and `run_headless_render()`
/// (CLI). Tauri's `generate_context!` macro can only be invoked once per crate
/// because it embeds the Info.plist binary blob — so this function is the
/// single call site.
///
/// `state` carries the CLI job (Some for headless, None for GUI).
/// `hide_window` controls whether the main window is shown on launch (false
/// for GUI, true for headless).
fn run_tauri(state: CliJobState, hide_window: bool) {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            save_png,
            save_gif_real,
            save_gif_to_path,
            save_mp4_real,
            save_mp4_to_path,
            choose_export_path,
            begin_export_capture,
            push_export_frame,
            push_export_frame_raw,
            finish_gif_export,
            finish_mp4_export,
            cancel_export,
            save_blob,
            cli_log,
            pick_image,
            pick_image_with_path,
            read_image_file,
            kv_load_all,
            kv_save,
            save_preset_json,
            load_preset_json,
            get_cli_render_job,
            exit_with_status,
            open_user_guide,
        ])
        .setup(move |app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            if hide_window {
                if let Some(window) = app.get_webview_window("main") {
                    // Don't `hide()` — WebKit pauses requestAnimationFrame in
                    // hidden windows, which breaks the recording pipeline.
                    // Instead move the window off-screen so the webview keeps
                    // rendering normally but the user doesn't see it.
                    let _ =
                        window.set_position(tauri::Position::Physical(tauri::PhysicalPosition {
                            x: -32000,
                            y: -32000,
                        }));
                    let _ = window.set_size(tauri::Size::Physical(tauri::PhysicalSize {
                        width: 1500,
                        height: 820,
                    }));
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod export_guard_tests {
    use super::*;

    static STATE_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn test_control(state: u8) -> ExportControl {
        ExportControl {
            state: Arc::new(AtomicU8::new(state)),
            app: None,
            job_id: None,
        }
    }

    fn png_header_frame(width: u32, height: u32) -> PngFrame {
        let mut bytes = Vec::with_capacity(33);
        bytes.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        bytes.extend_from_slice(&13_u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes.extend_from_slice(&[0, 0, 0, 0]); // CRC is decoded by image, not the guard.
        PngFrame::png(bytes)
    }

    fn valid_png_frame(width: u32, height: u32) -> PngFrame {
        let image =
            image::RgbaImage::from_pixel(width, height, image::Rgba([0x22, 0x44, 0x66, 0xff]));
        let mut cursor = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut cursor, image::ImageFormat::Png)
            .unwrap();
        PngFrame::png(cursor.into_inner())
    }

    #[test]
    fn export_guard_has_no_total_pixel_cap() {
        // ITER-054: 150 × 760×1024 (116.7M pixels) was rejected by the old
        // 100M total cap; 600 × 1024×682 is the MAX_EXPORT_FRAMES ceiling.
        let frames: Vec<_> = (0..150).map(|_| png_header_frame(760, 1_024)).collect();
        let dimensions = validate_export_frames(&frames, None).unwrap();
        assert_eq!((dimensions.width, dimensions.height), (760, 1_024));
        assert_eq!(dimensions.total_output_pixels, 150 * 760 * 1_024);
        let frames: Vec<_> = (0..MAX_EXPORT_FRAMES)
            .map(|_| png_header_frame(1_024, 682))
            .collect();
        assert!(validate_export_frames(&frames, None).is_ok());
    }

    #[test]
    fn export_guard_rejects_empty_and_excessive_frame_sets() {
        assert_eq!(
            validate_export_frames(&[], None).unwrap_err(),
            "no frames provided"
        );

        let frames: Vec<_> = (0..=MAX_EXPORT_FRAMES)
            .map(|_| png_header_frame(1, 1))
            .collect();
        let error = validate_export_frames(&frames, None).unwrap_err();
        assert!(error.contains("too many frames"), "{error}");
    }

    #[test]
    fn export_guard_rejects_invalid_base64_and_png_headers() {
        let invalid_b64 = vec![GifFrame { b64: "%%%".into() }];
        assert!(decode_frame_payloads(invalid_b64)
            .unwrap_err()
            .contains("frame 0 b64"));

        let not_png = vec![PngFrame::png(vec![0_u8; 33])];
        assert!(validate_export_frames(&not_png, None)
            .unwrap_err()
            .contains("not a valid PNG header"));
    }

    #[test]
    fn raw_rgba_frames_match_the_png_round_trip() {
        // ITER-052: an opaque raw frame must decode to the same pixels the
        // PNG path produced, through both the GIF decode and the MP4 path.
        let png = valid_png_frame(6, 4);
        let raw_bytes = png.to_rgba8(0).unwrap().into_raw();
        let raw = PngFrame {
            bytes: raw_bytes.clone(),
            rgba: Some((6, 4)),
        };
        assert_eq!(raw.dimensions(0).unwrap(), (6, 4));
        assert_eq!(png.to_rgba8(0).unwrap(), raw.to_rgba8(0).unwrap());
        let a = decode_one_frame(&png, 0, None).unwrap();
        let b = decode_one_frame(&raw, 0, None).unwrap();
        assert_eq!(a.buf(), b.buf());
        // Mixed PNG/raw sets validate as long as the sizes agree.
        let mixed = vec![valid_png_frame(6, 4), raw];
        assert_eq!(validate_export_frames(&mixed, None).unwrap().width, 6);
        let short = PngFrame {
            bytes: raw_bytes[..raw_bytes.len() - 4].to_vec(),
            rgba: Some((6, 4)),
        };
        assert!(short.to_rgba8(0).unwrap_err().contains("length mismatch"));
    }

    #[test]
    fn raw_rgba_length_and_pixel_guard() {
        assert!(check_raw_rgba_len(6 * 4 * 4, 6, 4, 0).is_ok());
        assert!(check_raw_rgba_len(6 * 4 * 4 - 1, 6, 4, 3)
            .unwrap_err()
            .contains("frame 3 raw RGBA is 95 bytes"));
        assert!(check_raw_rgba_len(0, 100_000, 100_000, 0)
            .unwrap_err()
            .contains("per-frame pixel limit"));
    }

    #[test]
    fn export_guard_rejects_zero_or_inconsistent_dimensions() {
        let zero = vec![png_header_frame(0, 10)];
        assert!(validate_export_frames(&zero, None)
            .unwrap_err()
            .contains("invalid dimensions"));

        let mismatched = vec![png_header_frame(10, 10), png_header_frame(11, 10)];
        assert!(validate_export_frames(&mismatched, None)
            .unwrap_err()
            .contains("do not match first frame"));
    }

    #[test]
    fn export_guard_rejects_pixel_limit_and_border_overflow() {
        let oversized = vec![png_header_frame(10_001, 10_000)];
        assert!(validate_export_frames(&oversized, None)
            .unwrap_err()
            .contains("per-frame pixel limit"));

        let border = BorderConfig {
            enabled: true,
            width: u32::MAX,
            ..BorderConfig::default()
        };
        assert!(
            validate_export_frames(&[png_header_frame(1, 1)], Some(&border))
                .unwrap_err()
                .contains("border width is too large")
        );
    }

    #[test]
    fn cancellation_token_and_command_are_observed() {
        let _guard = STATE_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let job_id = "rust-export-guard-test".to_string();
        let state = Arc::new(AtomicU8::new(JOB_RUNNING));
        EXPORT_CANCELLATIONS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .insert(job_id.clone(), state.clone());

        assert!(cancel_export(job_id.clone()));
        assert_eq!(state.load(Ordering::Acquire), JOB_CANCELLED);
        let control = ExportControl {
            state,
            app: None,
            job_id: Some(job_id.clone()),
        };
        assert_eq!(control.check_cancelled().unwrap_err(), "export cancelled");

        EXPORT_CANCELLATIONS
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .remove(&job_id);
        let _ = take_remembered(
            CANCELLED_CAPTURE_JOBS.get_or_init(|| Mutex::new(VecDeque::new())),
            &job_id,
        );
    }

    #[test]
    fn staged_capture_enforces_order_and_completeness() {
        let _guard = STATE_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let job_id = "rust-staged-export-test".to_string();
        begin_export_capture(job_id.clone(), 2, None).unwrap();
        let frame = png_header_frame(10, 10);
        let b64 = general_purpose::STANDARD.encode(&frame.bytes);

        let out_of_order =
            tauri::async_runtime::block_on(push_export_frame(job_id.clone(), 1, b64.clone()))
                .unwrap_err();
        assert!(out_of_order.contains("out-of-order frame"));

        tauri::async_runtime::block_on(push_export_frame(job_id.clone(), 0, b64.clone())).unwrap();
        assert!(take_export_capture(&job_id)
            .err()
            .unwrap()
            .contains("incomplete export capture"));
        tauri::async_runtime::block_on(push_export_frame(job_id.clone(), 1, b64)).unwrap();
        assert_eq!(take_export_capture(&job_id).ok().unwrap().frames.len(), 2);
        let _ = take_remembered(
            CANCELLED_CAPTURE_JOBS.get_or_init(|| Mutex::new(VecDeque::new())),
            &job_id,
        );
    }

    #[test]
    fn staged_capture_accepts_sets_past_the_old_pixel_cap() {
        // ITER-054: 150 × 760×1024 failed staging with "decoded pixels
        // exceeds limit"; PNG headers keep the test allocation tiny.
        let _guard = STATE_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let job_id = "rust-staged-past-pixel-cap".to_string();
        begin_export_capture(job_id.clone(), 150, None).unwrap();
        for index in 0..150 {
            stage_export_frame(&job_id, index, png_header_frame(760, 1_024)).unwrap();
        }
        assert_eq!(take_export_capture(&job_id).ok().unwrap().frames.len(), 150);
    }

    /// 40 small frames, PNG and raw RGBA mixed, more than any decode window.
    fn gif_test_frames() -> Vec<PngFrame> {
        (0..40u8)
            .map(|i| {
                let image = image::RgbaImage::from_fn(32, 24, |x, y| {
                    image::Rgba([i.wrapping_mul(6), (x * 8) as u8, (y * 10) as u8, 0xff])
                });
                if i % 2 == 0 {
                    PngFrame {
                        bytes: image.into_raw(),
                        rgba: Some((32, 24)),
                    }
                } else {
                    let mut cursor = std::io::Cursor::new(Vec::new());
                    image::DynamicImage::ImageRgba8(image)
                        .write_to(&mut cursor, image::ImageFormat::Png)
                        .unwrap();
                    PngFrame::png(cursor.into_inner())
                }
            })
            .collect()
    }

    fn stream_test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "glyph-grid-{}-{}-{}",
            name,
            std::process::id(),
            OUTPUT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn part_files(dir: &PathBuf) -> usize {
        fs::read_dir(dir)
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".part")
            })
            .count()
    }

    /// Stage `frames` into a streaming capture and finish it.
    fn run_stream(job_id: &str, config: GifStreamConfig) -> Result<String, String> {
        let frames = gif_test_frames();
        begin_export_capture(job_id.to_string(), frames.len(), Some(config.clone())).unwrap();
        for (index, frame) in frames.into_iter().enumerate() {
            stage_export_frame(job_id, index, frame).unwrap();
        }
        let stream = take_export_capture(job_id).ok().unwrap().stream.unwrap();
        let control = ExportControl {
            state: stream.state.clone(),
            app: None,
            job_id: None,
        };
        finish_gif_stream(stream, config, &control)
    }

    #[test]
    fn streamed_gif_is_byte_identical_to_the_staged_encode() {
        // ITER-053: same frames, same order, same pts → same bytes, with and
        // without a (met) byte target; the .part is renamed, never left.
        let _guard = STATE_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let dir = stream_test_dir("gif-stream");
        let expected = encode_gif_gifski_adaptive(
            &gif_test_frames(),
            40,
            None,
            None,
            None,
            &test_control(JOB_RUNNING),
        )
        .unwrap();
        for (name, target) in [("plain.gif", None), ("fit.gif", Some(64 * 1024 * 1024))] {
            let path = dir.join(name);
            let config = GifStreamConfig {
                path: path.display().to_string(),
                delay_ms: 40,
                cap_width: None,
                border: None,
                target_max_bytes: target,
            };
            run_stream("rust-gif-stream", config).unwrap();
            assert_eq!(fs::read(&path).unwrap(), expected, "{name}");
        }
        assert_eq!(part_files(&dir), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn streamed_gif_overshoot_and_cancel_leave_no_part_files() {
        let _guard = STATE_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let dir = stream_test_dir("gif-stream-fail");
        // Overshoot: 32 px frames have no ladder rung (floor is 360 px), so
        // the retained-frame shrink fails closed with the B11 error.
        let path = dir.join("tiny.gif");
        let config = GifStreamConfig {
            path: path.display().to_string(),
            delay_ms: 40,
            cap_width: None,
            border: None,
            target_max_bytes: Some(1),
        };
        assert!(run_stream("rust-gif-stream-overshoot", config)
            .unwrap_err()
            .contains("GIF cannot meet target size"));
        assert!(!path.exists());

        // Cancel mid-capture: cancel_export drops the capture, the encoder
        // aborts and deletes its .part on its own thread.
        let job_id = "rust-gif-stream-cancel".to_string();
        let config = GifStreamConfig {
            path: dir.join("cancelled.gif").display().to_string(),
            delay_ms: 40,
            cap_width: None,
            border: None,
            target_max_bytes: None,
        };
        begin_export_capture(job_id.clone(), 40, Some(config)).unwrap();
        for (index, frame) in gif_test_frames().into_iter().take(10).enumerate() {
            stage_export_frame(&job_id, index, frame).unwrap();
        }
        assert!(cancel_export(job_id.clone()));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while part_files(&dir) > 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(part_files(&dir), 0);
        assert!(!dir.join("cancelled.gif").exists());
        let _ = take_remembered(
            CANCELLED_CAPTURE_JOBS.get_or_init(|| Mutex::new(VecDeque::new())),
            &job_id,
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn windowed_gif_encode_is_deterministic_and_cancellable() {
        // ITER-054: more frames than the decode window, PNG and raw mixed;
        // two runs must produce the same bytes (in-order feed), and a
        // cancelled job must stop with the cancel error.
        let frames = gif_test_frames();
        let control = test_control(JOB_RUNNING);
        let a = encode_gif_gifski_frames(&frames, 40, None, 32, &control, "t").unwrap();
        let b = encode_gif_gifski_frames(&frames, 40, None, 32, &control, "t").unwrap();
        assert!(a.starts_with(b"GIF89a"));
        assert_eq!(a, b);
        let cancelled = test_control(JOB_CANCELLED);
        assert_eq!(
            encode_gif_gifski_frames(&frames, 40, None, 32, &cancelled, "t").unwrap_err(),
            "export cancelled"
        );
    }

    #[test]
    fn staged_capture_honours_early_cancel_and_concurrency_limit() {
        let _guard = STATE_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let early = "rust-early-cancel-test".to_string();
        assert!(cancel_export(early.clone()));
        assert_eq!(
            begin_export_capture(early.clone(), 1, None).unwrap_err(),
            "export cancelled"
        );

        let ids: Vec<_> = (0..MAX_CONCURRENT_CAPTURES)
            .map(|index| format!("rust-capture-limit-{index}"))
            .collect();
        for id in &ids {
            begin_export_capture(id.clone(), 1, None).unwrap();
        }
        let overflow = begin_export_capture("rust-capture-overflow".into(), 1, None).unwrap_err();
        assert!(overflow.contains("too many concurrent export captures"));
        for id in ids {
            assert!(cancel_export(id));
        }
        let _ = take_remembered(
            CANCELLED_CAPTURE_JOBS.get_or_init(|| Mutex::new(VecDeque::new())),
            &early,
        );
    }

    #[test]
    fn cancel_returns_false_after_commit_transition() {
        let _guard = STATE_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let job_id = "rust-commit-state-test".to_string();
        let state = Arc::new(AtomicU8::new(JOB_COMMITTING));
        EXPORT_CANCELLATIONS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .insert(job_id.clone(), state);
        assert!(!cancel_export(job_id.clone()));
        EXPORT_CANCELLATIONS
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .remove(&job_id);
    }

    #[test]
    fn gif_dimensions_are_explicit_and_format_bounded() {
        let dimensions = validate_export_frames(&[png_header_frame(640, 480)], None).unwrap();
        assert_eq!(validated_gif_width(dimensions, None).unwrap(), 640);

        let too_wide = validate_export_frames(&[png_header_frame(70_000, 1)], None).unwrap();
        assert!(validated_gif_width(too_wide, None)
            .unwrap_err()
            .contains("exceed format limit"));
        assert_eq!(validated_gif_width(too_wide, Some(700)).unwrap(), 700);
    }

    #[test]
    fn gif_without_cap_preserves_validated_dimensions() {
        let frames = vec![valid_png_frame(64, 48)];
        let encoded =
            encode_gif_gifski_adaptive(&frames, 100, None, None, None, &test_control(JOB_RUNNING))
                .unwrap();
        let decoded =
            image::load_from_memory_with_format(&encoded, image::ImageFormat::Gif).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (64, 48));
    }

    #[test]
    fn adaptive_gif_target_is_a_hard_limit() {
        let frames = vec![valid_png_frame(8, 8)];
        let error = encode_gif_gifski_adaptive(
            &frames,
            100,
            None,
            None,
            Some(1),
            &test_control(JOB_RUNNING),
        )
        .unwrap_err();
        assert!(error.contains("cannot meet target size"));
    }

    #[test]
    fn twitter_fit_width_predicts_from_bytes_per_pixel() {
        // run-all B12: 720 px → 1.60 MB, 700 KB target. sqrt(0.4375)·0.96·720
        // = 457.2 → 456 (the old ladder walked 600/540/480 to land on 420).
        assert_eq!(twitter_fit_width(720, 1_600_000, 700_000, 0.96), Some(456));
        // worm-move.gif 120 f: 31.23 MB at 720, 15 MB cap → 478 px in one
        // pass (the ladder spent 600/540 to land on 480).
        assert_eq!(
            twitter_fit_width(720, 32_747_000, 15 * 1024 * 1024, 0.96),
            Some(478)
        );
        // Tiny targets clamp to the floor; nothing narrower than the floor
        // (or than the current width) is ever proposed.
        assert_eq!(twitter_fit_width(720, 1_600_000, 1, 0.96), Some(360));
        assert_eq!(twitter_fit_width(360, 1_600_000, 1, 0.90), None);
        assert_eq!(twitter_fit_width(8, 100, 1, 0.95), None);
        // A barely-over first pass still shrinks by at least one step.
        assert_eq!(twitter_fit_width(720, 700_001, 700_000, 1.0), Some(718));
    }

    #[test]
    fn mp4_timing_is_exact_for_supported_frame_rates() {
        for fps in [24, 25, 30] {
            let (timescale, duration) = mp4_timing(fps).unwrap();
            assert_eq!(timescale, fps);
            assert_eq!(duration, 1);
            assert_eq!(mp4_sample_timing(119, duration), (119, 1));
        }
        assert_eq!(mp4_timing(0).unwrap_err(), "fps must be > 0");
    }

    /// ITER-050 (F11): upload mode must hold its bitrate target without
    /// dropping frames.  60 flat frames (where the old skip-frame RC emitted
    /// nothing) then 60 frames with a fresh noise band each (where RC without
    /// skipping overshoots), 640x360 @ 30 fps = 4 s.
    #[test]
    fn mp4_upload_rate_control_keeps_every_frame_near_target() {
        const W: u32 = 640;
        const H: u32 = 360;
        const FPS: u32 = 30;
        const N: u32 = 120;
        let mut seed: u32 = 0x1234_5678;
        let frames: Vec<PngFrame> = (0..N)
            .map(|i| {
                let mut img = image::RgbImage::from_pixel(W, H, image::Rgb([0xf2, 0xe8, 0xd8]));
                if i >= N / 2 {
                    for y in H / 4..3 * H / 4 {
                        for x in 0..W {
                            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                            let v = (seed >> 24) as u8;
                            img.put_pixel(x, y, image::Rgb([v, v, v]));
                        }
                    }
                }
                let mut cursor = std::io::Cursor::new(Vec::new());
                image::DynamicImage::ImageRgb8(img)
                    .write_to(&mut cursor, image::ImageFormat::Png)
                    .unwrap();
                PngFrame::png(cursor.into_inner())
            })
            .collect();

        let out = encode_mp4_h264(
            &frames,
            FPS,
            None,
            None,
            Mp4Quality::Upload,
            &test_control(JOB_RUNNING),
        )
        .unwrap();

        let size = out.len() as u64;
        let mut reader = mp4::Mp4Reader::read_header(std::io::Cursor::new(out), size).unwrap();
        let track_id = *reader.tracks().keys().next().unwrap();
        assert_eq!(reader.tracks()[&track_id].timescale(), FPS);
        assert_eq!(reader.sample_count(track_id).unwrap(), N);
        let mut sample_bytes = 0u64;
        for sample_id in 1..=N {
            let sample = reader.read_sample(track_id, sample_id).unwrap().unwrap();
            assert!(!sample.bytes.is_empty(), "sample {} is empty", sample_id);
            assert_eq!(sample.duration, 1);
            sample_bytes += sample.bytes.len() as u64;
        }
        let duration_s = f64::from(N) / f64::from(FPS);
        let budget = f64::from(mp4_upload_bitrate(W, H)) / 8.0 * duration_s;
        assert!(
            (size as f64) < 1.5 * budget,
            "upload MP4 is {} bytes ({} in samples), over 1.5x the {:.0}-byte budget",
            size,
            sample_bytes,
            budget
        );
    }

    /// A typo'd per-job `mp4Quality` must be refused with the manifest
    /// (exit 2, before any webview starts), not after the capture ran.
    #[test]
    fn batch_manifest_rejects_unknown_mp4_quality() {
        let dir = std::env::temp_dir().join(format!(
            "glyph-grid-mp4-quality-{}-{}",
            std::process::id(),
            OUTPUT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("in.png");
        fs::write(&source, &valid_png_frame(4, 4).bytes).unwrap();
        let manifest = dir.join("manifest.json");
        fs::write(
            &manifest,
            serde_json::json!({
                "in": source,
                "jobs": [{"name": "typo", "out": dir.join("out.mp4"), "mp4Quality": "lossless"}],
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(run_headless_batch(manifest, false, 1), 2);
        fs::remove_dir_all(dir).unwrap();
    }

    /// ITER-055: `--jobs` clamps to the job count and cores-2, never below 1,
    /// and the split keeps every job once, round-robin, each pinned to the
    /// source the serial batch would have rendered it with.
    #[test]
    fn batch_fan_out_clamps_and_split_pins_effective_sources() {
        assert_eq!(batch_fan_out_count(1, 20, Some(8)), 1);
        assert_eq!(batch_fan_out_count(4, 20, Some(8)), 4);
        assert_eq!(batch_fan_out_count(16, 20, Some(8)), 6);
        assert_eq!(batch_fan_out_count(4, 3, Some(8)), 3);
        assert_eq!(batch_fan_out_count(4, 20, Some(2)), 1);
        assert_eq!(batch_fan_out_count(0, 20, None), 1);

        let jobs: Vec<serde_json::Value> = vec![
            serde_json::json!({"name": "a", "out": "a.gif"}),
            serde_json::json!({"name": "b", "out": "b.gif", "in": "/x.png"}),
            serde_json::json!({"name": "c", "out": "c.gif"}),
            serde_json::json!({"name": "d", "out": "d.gif", "in": "/y.png"}),
            serde_json::json!({"name": "e", "out": "e.gif"}),
        ];
        let manifest =
            serde_json::json!({"in": "rel.png", "frames": 7, "perf": true, "jobs": jobs});
        let parts = split_batch_manifest(&manifest, "/m.png", &jobs, 2);
        let names = |p: &serde_json::Value| -> Vec<(String, String)> {
            p["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|j| {
                    (
                        j["name"].as_str().unwrap().into(),
                        j["in"].as_str().unwrap().into(),
                    )
                })
                .collect()
        };
        let s = |a: &str, b: &str| (a.to_string(), b.to_string());
        assert_eq!(
            names(&parts[0]),
            vec![s("a", "/m.png"), s("c", "/x.png"), s("e", "/y.png")]
        );
        assert_eq!(names(&parts[1]), vec![s("b", "/x.png"), s("d", "/y.png")]);
        for p in &parts {
            assert_eq!(p["in"], "/m.png");
            assert_eq!(p["frames"], 7);
            assert_eq!(p["perf"], true);
        }

        assert_eq!(duplicate_out_path(&jobs), None);
        let mut dup = jobs.clone();
        dup.push(serde_json::json!({"name": "f", "out": "c.gif"}));
        assert_eq!(duplicate_out_path(&dup).as_deref(), Some("c.gif"));
    }

    #[test]
    fn mp4_quality_parse_defaults_to_upload_and_rejects_typos() {
        assert_eq!(Mp4Quality::parse(None).unwrap(), Mp4Quality::Upload);
        assert_eq!(Mp4Quality::parse(Some("")).unwrap(), Mp4Quality::Upload);
        assert_eq!(Mp4Quality::parse(Some("Archive")).unwrap(), Mp4Quality::Archive);
        assert!(Mp4Quality::parse(Some("lossless")).is_err());
        assert_eq!(mp4_upload_bitrate(1024, 682), 5_000_000);
        assert_eq!(mp4_upload_bitrate(3840, 2160), 20_000_000);
    }

    #[test]
    fn unicode_hex_input_falls_back_without_panicking() {
        assert_eq!(parse_hex_rgb("#1122ff"), (0x11, 0x22, 0xff));
        assert_eq!(parse_hex_rgb("ééé"), (232, 221, 200));
    }

    #[test]
    fn atomic_output_write_replaces_only_after_success_and_cleans_parts() {
        let test_dir = std::env::temp_dir().join(format!(
            "glyph-grid-atomic-write-{}-{}",
            std::process::id(),
            OUTPUT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&test_dir).unwrap();
        let destination = test_dir.join("result.gif");
        atomic_write_output(&destination, b"first", &test_control(JOB_RUNNING)).unwrap();
        atomic_write_output(&destination, b"second", &test_control(JOB_RUNNING)).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"second");

        let cancelled = test_control(JOB_CANCELLED);
        let cancelled_path = test_dir.join("cancelled.gif");
        assert_eq!(
            atomic_write_output(&cancelled_path, b"unused", &cancelled).unwrap_err(),
            "export cancelled"
        );
        assert!(!cancelled_path.exists());
        assert!(fs::read_dir(&test_dir).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".part")));
        fs::remove_dir_all(test_dir).unwrap();
    }
}
