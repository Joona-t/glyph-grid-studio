use base64::engine::general_purpose;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
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
pub fn run_headless_batch(manifest_path: PathBuf, show_window: bool) -> i32 {
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

#[derive(Debug)]
struct PngFrame {
    bytes: Vec<u8>,
}

/// Hard safety limits for native animation export. These limits are checked
/// from PNG IHDR metadata before any frame is decompressed into RGB/RGBA.
/// 100M pixels is about 381 MiB as raw RGBA before encoder overhead.
const MAX_EXPORT_FRAMES: usize = 600;
const MAX_TOTAL_DECODED_PIXELS: u64 = 100_000_000;
const MAX_TOTAL_COMPRESSED_FRAME_BYTES: u64 = 256 * 1024 * 1024;
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
    frames: Vec<PngFrame>,
    dimensions: Option<(u32, u32)>,
    total_pixels: u64,
    total_bytes: u64,
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

fn atomic_write_output(
    path: &PathBuf,
    bytes: &[u8],
    control: &ExportControl,
) -> Result<(), String> {
    control.check_cancelled()?;
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
    let part_path = parent.join(format!(
        ".{}.{}.{}.part",
        file_name,
        std::process::id(),
        sequence
    ));

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
        control.begin_commit()?;
        if let Err(error) = fs::rename(&part_path, path) {
            let _ = control.state.compare_exchange(
                JOB_COMMITTING,
                JOB_RUNNING,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return Err(format!("rename {:?} to {:?}: {}", part_path, path, error));
        }
        #[cfg(unix)]
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|e| format!("sync export directory {:?}: {}", parent, e))?;
        control.emit("write", 1, 1);
        Ok(())
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
    let state = Arc::new(AtomicU8::new(JOB_RUNNING));
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

#[tauri::command]
fn begin_export_capture(job_id: String, total: usize) -> Result<(), String> {
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
    let capture = StagedExport {
        expected_frames: total,
        frames: Vec::with_capacity(total),
        dimensions: None,
        total_pixels: 0,
        total_bytes: 0,
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
        let dimensions = png_dimensions(&bytes, index)?;
        let mut captures = EXPORT_CAPTURES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| "export capture state is unavailable".to_string())?;
        let capture = captures
            .get_mut(&job_id)
            .ok_or_else(|| format!("unknown export capture: {}", job_id))?;
        if index != capture.frames.len() {
            return Err(format!(
                "out-of-order frame: got index {}, expected {}",
                index,
                capture.frames.len()
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
        let pixels = u64::from(dimensions.0)
            .checked_mul(u64::from(dimensions.1))
            .ok_or_else(|| format!("frame {} pixel count overflow", index))?;
        let next_pixels = capture
            .total_pixels
            .checked_add(pixels)
            .ok_or_else(|| "total decoded pixel count overflow".to_string())?;
        if next_pixels > MAX_TOTAL_DECODED_PIXELS {
            return Err(format!(
                "export is too large: {} decoded pixels exceeds limit {}",
                next_pixels, MAX_TOTAL_DECODED_PIXELS
            ));
        }
        let next_bytes = capture
            .total_bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| "total compressed frame byte count overflow".to_string())?;
        if next_bytes > MAX_TOTAL_COMPRESSED_FRAME_BYTES {
            return Err(format!(
                "compressed frames are too large: {} bytes exceeds limit {} bytes",
                next_bytes, MAX_TOTAL_COMPRESSED_FRAME_BYTES
            ));
        }
        capture.total_pixels = next_pixels;
        capture.total_bytes = next_bytes;
        capture.frames.push(PngFrame { bytes });
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("frame staging worker failed: {}", e))?
}

fn take_export_capture(job_id: &str) -> Result<Vec<PngFrame>, String> {
    let mut captures = EXPORT_CAPTURES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| "export capture state is unavailable".to_string())?;
    let capture = captures
        .get(job_id)
        .ok_or_else(|| format!("unknown export capture: {}", job_id))?;
    if capture.frames.len() != capture.expected_frames {
        return Err(format!(
            "incomplete export capture: received {} of {} frames",
            capture.frames.len(),
            capture.expected_frames
        ));
    }
    Ok(captures
        .remove(job_id)
        .expect("capture existed while lock was held")
        .frames)
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
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
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
        let dimensions = png_dimensions(&frame.bytes, index)?;
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
        let frame_pixels = u64::from(output_width)
            .checked_mul(u64::from(output_height))
            .ok_or_else(|| format!("frame {} pixel count overflow", index))?;
        total_output_pixels = total_output_pixels
            .checked_add(frame_pixels)
            .ok_or_else(|| "total decoded pixel count overflow".to_string())?;
        if total_output_pixels > MAX_TOTAL_DECODED_PIXELS {
            return Err(format!(
                "export is too large: {} decoded pixels exceeds limit {}",
                total_output_pixels, MAX_TOTAL_DECODED_PIXELS
            ));
        }
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
        decoded.push(PngFrame { bytes });
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

/// Encode a recorded sequence of PNG frames as a single animated GIF using
/// the `gifski` crate.  gifski produces ~30-50% smaller files than the
/// previous `gif` crate path because it computes a SHARED palette across
/// all frames (not one palette per frame) and uses error-diffusion
/// dithering across the whole sequence.  Quality 100 is genuinely lossless
/// for our use case (glyph art has ≤ ~20 unique colours).
///
/// `cap_width` resizes frames to that width with aspect ratio preserved
/// (gifski handles the resize internally).  `None` keeps the source size.
/// At Twitter's recommended 720 px wide for posts, kaneki drops from
/// ~30 MB → ~6-8 MB without visible quality change on phone screens.
/// Decode one base64-PNG frame to RGBA, applying the optional border.
/// Pure per-frame function — safe to run on any thread (EXPORT-OPT-1).
fn decode_one_frame(
    frame: &PngFrame,
    idx: usize,
    border: Option<&BorderConfig>,
) -> Result<imgref::ImgVec<rgb::RGBA8>, String> {
    let mut img = image::load_from_memory_with_format(&frame.bytes, image::ImageFormat::Png)
        .map_err(|e| format!("frame {} decode: {}", idx, e))?
        .to_rgba8();
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

/// EXPORT-OPT-1 (2026-06-10): parallel frame decode with std scoped
/// threads (no new deps).  The old sequential base64→PNG→RGBA loop was
/// the single-threaded head of every export: ~40-60 % of GIF encode
/// wall-time on a 150-frame loop, while 7+ cores idled.  Each frame is
/// an independent pure decode, so this is bit-exact by construction —
/// frame N's bytes are identical regardless of which thread decoded it.
fn decode_frames_parallel(
    frames: &[PngFrame],
    delay_ms: u32,
    border: Option<&BorderConfig>,
    control: &ExportControl,
) -> Result<Vec<(imgref::ImgVec<rgb::RGBA8>, f64)>, String> {
    let n = frames.len();
    if n == 0 {
        return Ok(Vec::new());
    }
    // BUG-009: leave 2 cores free — a GUI export runs this while the
    // webview's draw loop is live (the loop resumes during encode since
    // v0.1.7); saturating every core starves the UI thread and the app
    // reads as frozen even though the encode is progressing.
    let threads = std::thread::available_parallelism()
        .map(|p| p.get().saturating_sub(2).max(2))
        .unwrap_or(4)
        .min(n);
    let chunk = n.div_ceil(threads);

    let mut slots: Vec<Option<Result<imgref::ImgVec<rgb::RGBA8>, String>>> = Vec::with_capacity(n);
    slots.resize_with(n, || None);

    std::thread::scope(|s| {
        for (t, slot_chunk) in slots.chunks_mut(chunk).enumerate() {
            let base = t * chunk;
            s.spawn(move || {
                for (i, slot) in slot_chunk.iter_mut().enumerate() {
                    let idx = base + i;
                    *slot = Some(
                        control
                            .check_cancelled()
                            .and_then(|_| decode_one_frame(&frames[idx], idx, border)),
                    );
                }
            });
        }
    });

    let mut out = Vec::with_capacity(n);
    for (idx, slot) in slots.into_iter().enumerate() {
        let img = slot.ok_or_else(|| format!("frame {}: decode slot empty", idx))??;
        let pts = (idx as f64) * (delay_ms as f64) / 1000.0;
        out.push((img, pts));
    }
    control.check_cancelled()?;
    control.emit("decode", n, n);
    Ok(out)
}

/// Encode pre-decoded frames with gifski.  Split out of
/// `encode_gif_gifski` so the twitter-fit ladder can decode ONCE and
/// re-encode at each cap width (EXPORT-OPT-2) — previously every ladder
/// rung re-decoded all frames from base64.  Frames are cloned one at a
/// time into the collector (gifski wants owned ImgVecs); transient peak
/// is the decoded set + one frame.
fn encode_gif_gifski_decoded(
    decoded: &[(imgref::ImgVec<rgb::RGBA8>, f64)],
    cap_width: Option<u32>,
    control: &ExportControl,
    progress_phase: &str,
) -> Result<Vec<u8>, String> {
    if decoded.is_empty() {
        return Err("no frames provided".into());
    }
    let settings = gifski::Settings {
        width: cap_width,
        height: None,
        quality: 100,
        fast: false,
        repeat: gif::Repeat::Infinite,
    };
    let (collector, writer) = gifski::new(settings).map_err(|e| format!("gifski init: {}", e))?;

    struct Progress {
        control: ExportControl,
        phase: String,
        completed: usize,
        total: usize,
    }
    impl gifski::progress::ProgressReporter for Progress {
        fn increase(&mut self) -> bool {
            self.completed = (self.completed + 1).min(self.total);
            self.control.emit(&self.phase, self.completed, self.total);
            self.control.state.load(Ordering::Acquire) != JOB_CANCELLED
        }
        fn done(&mut self, _msg: &str) {}
    }

    // Collector and writer must run concurrently — the collector
    // quantises each frame as it arrives, the writer assembles the final
    // palette + LZW stream after the collector closes.  Scoped thread so
    // the collector can borrow `decoded`.
    let mut output: Vec<u8> = Vec::new();
    std::thread::scope(|s| -> Result<(), String> {
        let handle = s.spawn(move || -> Result<(), String> {
            for (idx, (imgvec, pts)) in decoded.iter().enumerate() {
                control.check_cancelled()?;
                collector
                    .add_frame_rgba(idx, imgvec.clone(), *pts)
                    .map_err(|e| format!("frame {} add: {}", idx, e))?;
            }
            drop(collector); // signals the writer to finalise
            Ok(())
        });
        writer
            .write(
                &mut output,
                &mut Progress {
                    control: control.clone(),
                    phase: progress_phase.to_string(),
                    completed: 0,
                    total: decoded.len(),
                },
            )
            .map_err(|e| format!("gifski write: {}", e))?;
        handle
            .join()
            .map_err(|_| "gifski collector thread panicked".to_string())??;
        Ok(())
    })?;

    control.check_cancelled()?;

    Ok(output)
}

fn encode_gif_gifski(
    frames: &[PngFrame],
    delay_ms: u32,
    cap_width: Option<u32>,
    border: Option<&BorderConfig>,
    control: &ExportControl,
) -> Result<Vec<u8>, String> {
    let dimensions = validate_export_frames(frames, border)?;
    let encode_width = validated_gif_width(dimensions, cap_width)?;
    let t0 = std::time::Instant::now();
    let decoded = decode_frames_parallel(frames, delay_ms, border, control)?;
    let t_decode = t0.elapsed();
    let out = encode_gif_gifski_decoded(&decoded, Some(encode_width), control, "encode-gif")?;
    eprintln!(
        "encode: {} frames — decode {:.0}ms (parallel) + gifski {:.0}ms",
        frames.len(),
        t_decode.as_secs_f64() * 1000.0,
        t0.elapsed().as_secs_f64() * 1000.0 - t_decode.as_secs_f64() * 1000.0,
    );
    Ok(out)
}

/// Adaptive Twitter-fit wrapper.  ITER-025 surfaced that the GUI's
/// `Export GIF (Twitter-fit)` button hardcoded 720 px, which routinely
/// overshot Twitter's 15 MB cap on high-density content (cream-paper
/// monochrome / dense glyph stipple).  This wrapper retries the encoder
/// at progressively smaller widths until the output fits, matching the
/// driver-level safety-net pipeline that proved 53 / 53 rescue on the
/// ghost-I.gif 64-variant batch (see ITER-024 / ITER-025).
///
/// `target_max_bytes`: when `Some(N)`, the function tries `cap_width`
/// first, then `[600, 540, 480, 420, 360]` (skipping any caps ≥ the
/// initial cap), and returns the first buffer ≤ N.  If all six caps
/// overshoot the target, returns an error rather than silently producing an
/// upload that violates the requested hard limit.
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

    // EXPORT-OPT-2 (2026-06-10): decode ONCE for the entire ladder.
    // Previously every rung re-ran the full base64→PNG→RGBA decode of
    // all frames; a 5-rung walk decoded the set 6 times. gifski resizes
    // internally from cap_width, so one decoded set serves every rung.
    let t0 = std::time::Instant::now();
    let decoded = decode_frames_parallel(frames, delay_ms, border, control)?;
    eprintln!(
        "encode: {} frames decoded once in {:.0}ms (parallel, shared across ladder)",
        frames.len(),
        t0.elapsed().as_secs_f64() * 1000.0,
    );
    let single =
        encode_gif_gifski_decoded(&decoded, Some(first_width), control, "encode-gif-attempt-1")?;
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
    // Try shrink ladder.
    let initial_cap = first_width;
    let ladder: [u32; 5] = [600, 540, 480, 420, 360];
    let mut best = single;
    eprintln!(
        "twitter-fit: cap={} size={:.2}MB > target {:.2}MB — entering shrink ladder",
        initial_cap,
        best.len() as f64 / 1024.0 / 1024.0,
        target as f64 / 1024.0 / 1024.0,
    );
    for (attempt_index, cap) in ladder.iter().enumerate() {
        control.check_cancelled()?;
        if *cap >= initial_cap {
            continue;
        }
        let phase = format!("encode-gif-attempt-{}", attempt_index + 2);
        let encode_width = validated_gif_width(dimensions, Some(*cap))?;
        let buf = encode_gif_gifski_decoded(&decoded, Some(encode_width), control, &phase)?;
        eprintln!(
            "twitter-fit: cap={} size={:.2}MB",
            cap,
            buf.len() as f64 / 1024.0 / 1024.0,
        );
        let fits = (buf.len() as u64) <= target;
        if buf.len() < best.len() {
            best = buf;
        }
        if fits {
            eprintln!("twitter-fit: cap={} ✓", cap);
            return Ok(best);
        }
    }
    Err(format!(
        "GIF cannot meet target size: smallest attempt is {} bytes, target is {} bytes",
        best.len(),
        target
    ))
}

/// Save a recorded sequence of PNG frames as a single animated GIF.
/// Pops the native save dialog and writes the encoded bytes there.
/// `cap_width` (optional) resizes frames before encoding for smaller
/// output files (Twitter's 15 MB GIF limit, etc.).
/// `target_max_bytes` (optional, ITER-026): when set, walks a shrink
/// ladder (720 → 600 → 540 → 480 → 420 → 360) until output ≤ N.  The
/// GUI's `Export GIF (Twitter-fit)` button passes 15 MB.
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

fn encode_mp4_h264(
    frames: &[PngFrame],
    fps: u32,
    cap_width: Option<u32>,
    border: Option<&BorderConfig>,
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

    // Configure the encoder explicitly:
    //   - Rate control = Off — never skip frames.  Glyph art has very low
    //     inter-frame motion in static backgrounds; under default bitrate-
    //     based RC openh264 was emitting zero NAL units for some frames,
    //     producing a broken stream.  RC Off makes every encode() yield a
    //     real slice NAL.
    //   - Bitrate = 5 Mbps cap (still small for our content; usually
    //     produces 1-2 MB output for a 90-frame loop).
    //   - Max frame rate 30 — tells the encoder the playback target.
    let cfg = EncoderConfig::new()
        .max_frame_rate(FrameRate::from_hz(fps as f32))
        .bitrate(BitRate::from_bps(5_000_000))
        .rate_control_mode(RateControlMode::Off);
    let mut encoder = Encoder::with_api_config(OpenH264API::from_source(), cfg)
        .map_err(|e| format!("openh264 init: {}", e))?;

    // Decode/preprocess exactly one frame at a time before handing it to the
    // stateful H.264 encoder. The prior parallel implementation retained an
    // RGB Vec for every frame (3 * W * H * frame_count bytes), which caused
    // avoidable export-time RAM spikes. The command itself runs on Tauri's
    // blocking pool, so this sequential loop does not block the async runtime.
    let t_encode = std::time::Instant::now();

    // Walk frames, encode, capture SPS/PPS once and retain only the much
    // smaller compressed samples for the in-memory MP4 muxer.
    let mut sps: Option<Vec<u8>> = None;
    let mut pps: Option<Vec<u8>> = None;
    let mut samples: Vec<(Vec<u8>, bool)> = Vec::with_capacity(frames.len());

    for (idx, frame) in frames.iter().enumerate() {
        control.check_cancelled()?;
        let dyn_img = image::load_from_memory_with_format(&frame.bytes, image::ImageFormat::Png)
            .map_err(|e| format!("frame {} decode: {}", idx, e))?;
        let mut rgba_img = dyn_img.to_rgba8();
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
        let yuv = YUVBuffer::from_rgb_source(rgb_source);

        let bitstream = encoder
            .encode(&yuv)
            .map_err(|e| format!("frame {} encode: {}", idx, e))?;

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
        control.emit("encode-mp4", idx + 1, frames.len());
    }

    eprintln!(
        "encode-mp4: {} frames decoded, preprocessed, and encoded in {:.0}ms (streamed RGB)",
        frames.len(),
        t_encode.elapsed().as_secs_f64() * 1000.0,
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

    let track_config = TrackConfig::from(AvcConfig {
        width: target_w as u16,
        height: target_h as u16,
        seq_param_set: sps,
        pic_param_set: pps,
    });
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
    job_id: Option<String>,
) -> Result<String, String> {
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
        let mp4_buf = encode_mp4_h264(&frames, fps, cap_width, border.as_ref(), &control)?;
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
    job_id: Option<String>,
) -> Result<String, String> {
    let p = PathBuf::from(&path);
    let (control, registration) = register_export(app, job_id);
    tauri::async_runtime::spawn_blocking(move || {
        let _registration = registration;
        let frames = decode_frame_payloads(frames)?;
        let mp4_buf = encode_mp4_h264(&frames, fps, cap_width, border.as_ref(), &control)?;
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
    let (control, registration) = register_export(app, Some(job_id.clone()));
    let frames = take_export_capture(&job_id)?;
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
) -> Result<String, String> {
    let (control, registration) = register_export(app, Some(job_id.clone()));
    let frames = take_export_capture(&job_id)?;
    let path = PathBuf::from(path);
    tauri::async_runtime::spawn_blocking(move || {
        let _registration = registration;
        let mp4_buf = encode_mp4_h264(&frames, fps, cap_width, border.as_ref(), &control)?;
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
        PngFrame { bytes }
    }

    fn valid_png_frame(width: u32, height: u32) -> PngFrame {
        let image =
            image::RgbaImage::from_pixel(width, height, image::Rgba([0x22, 0x44, 0x66, 0xff]));
        let mut cursor = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut cursor, image::ImageFormat::Png)
            .unwrap();
        PngFrame {
            bytes: cursor.into_inner(),
        }
    }

    #[test]
    fn export_guard_accepts_consistent_frames_at_pixel_limit() {
        let frames: Vec<_> = (0..100).map(|_| png_header_frame(1_000, 1_000)).collect();
        let dimensions = validate_export_frames(&frames, None).unwrap();
        assert_eq!(dimensions.width, 1_000);
        assert_eq!(dimensions.height, 1_000);
        assert_eq!(dimensions.total_output_pixels, MAX_TOTAL_DECODED_PIXELS);
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

        let not_png = vec![PngFrame {
            bytes: vec![0_u8; 33],
        }];
        assert!(validate_export_frames(&not_png, None)
            .unwrap_err()
            .contains("not a valid PNG header"));
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
            .contains("decoded pixels exceeds limit"));

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
        begin_export_capture(job_id.clone(), 2).unwrap();
        let frame = png_header_frame(10, 10);
        let b64 = general_purpose::STANDARD.encode(&frame.bytes);

        let out_of_order =
            tauri::async_runtime::block_on(push_export_frame(job_id.clone(), 1, b64.clone()))
                .unwrap_err();
        assert!(out_of_order.contains("out-of-order frame"));

        tauri::async_runtime::block_on(push_export_frame(job_id.clone(), 0, b64.clone())).unwrap();
        assert!(take_export_capture(&job_id)
            .unwrap_err()
            .contains("incomplete export capture"));
        tauri::async_runtime::block_on(push_export_frame(job_id.clone(), 1, b64)).unwrap();
        assert_eq!(take_export_capture(&job_id).unwrap().len(), 2);
        let _ = take_remembered(
            CANCELLED_CAPTURE_JOBS.get_or_init(|| Mutex::new(VecDeque::new())),
            &job_id,
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
            begin_export_capture(early.clone(), 1).unwrap_err(),
            "export cancelled"
        );

        let ids: Vec<_> = (0..MAX_CONCURRENT_CAPTURES)
            .map(|index| format!("rust-capture-limit-{index}"))
            .collect();
        for id in &ids {
            begin_export_capture(id.clone(), 1).unwrap();
        }
        let overflow = begin_export_capture("rust-capture-overflow".into(), 1).unwrap_err();
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
    fn mp4_timing_is_exact_for_supported_frame_rates() {
        for fps in [24, 25, 30] {
            let (timescale, duration) = mp4_timing(fps).unwrap();
            assert_eq!(timescale, fps);
            assert_eq!(duration, 1);
            assert_eq!(mp4_sample_timing(119, duration), (119, 1));
        }
        assert_eq!(mp4_timing(0).unwrap_err(), "fps must be > 0");
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
