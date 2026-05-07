use base64::engine::general_purpose;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;

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

/// JS calls this when the headless render is finished. Sets the exit code
/// then triggers the Tauri runtime to shut down.
#[tauri::command]
fn exit_with_status(
    app: tauri::AppHandle,
    state: tauri::State<CliJobState>,
    ok: bool,
) {
    let code = if ok { 0 } else { 1 };
    if let Ok(mut guard) = state.exit_code.lock() {
        *guard = code;
    }
    app.exit(code);
}

/// Public entry point invoked by main.rs for `render` subcommand.  Returns
/// the exit code; in practice the process exits via `app.exit()` from JS
/// before this function returns, so the return value here is a fallback for
/// the "tauri runtime closed without a render" case.
pub fn run_headless_render(job: HeadlessRenderJob) -> i32 {
    let job_json = serde_json::json!({
        "inPath": job.in_path.to_string_lossy().to_string(),
        "outPath": job.out_path.to_string_lossy().to_string(),
        "frames": job.frames,
        "config": job.build_js_config(),
    });
    let exit_code = Arc::new(Mutex::new(2)); // 2 = "didn't complete"
    let state = CliJobState {
        job: Mutex::new(Some(job_json)),
        exit_code: exit_code.clone(),
    };
    run_tauri(state, !job.show_window);
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
            "cream-paper", "silver-charcoal", "spice"
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

/// Save a recorded sequence of PNG frames as a single animated GIF using
/// the `gif` crate.  Each frame is PNG-decoded → RGBA → palette-quantised
/// (NeuQuant via `gif::Frame::from_rgba_speed`) → muxed into the GIF.
/// `delay_ms` is the per-frame delay in milliseconds.  GIF stores delays
/// as hundredths of a second, so we round delay_ms / 10.
#[tauri::command]
async fn save_gif_real(
    app: tauri::AppHandle,
    frames: Vec<GifFrame>,
    delay_ms: u32,
) -> Result<String, String> {
    if frames.is_empty() {
        return Err("no frames provided".into());
    }

    // Decode the first frame to get dimensions.
    let first_bytes = general_purpose::STANDARD
        .decode(&frames[0].b64)
        .map_err(|e| format!("frame 0: {}", e))?;
    let first_img = image::load_from_memory_with_format(&first_bytes, image::ImageFormat::Png)
        .map_err(|e| format!("frame 0 decode: {}", e))?;
    let (w, h) = (first_img.width() as u16, first_img.height() as u16);

    // Build the GIF in memory.
    let mut gif_buf: Vec<u8> = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut gif_buf, w, h, &[])
            .map_err(|e| format!("gif encoder: {}", e))?;
        encoder
            .set_repeat(gif::Repeat::Infinite)
            .map_err(|e| format!("gif repeat: {}", e))?;

        // GIF delay is in hundredths of a second.  Clamp to >= 2 (~50fps).
        let delay_cs = ((delay_ms as f32) / 10.0).round().max(2.0) as u16;

        for (idx, f) in frames.iter().enumerate() {
            let bytes = general_purpose::STANDARD
                .decode(&f.b64)
                .map_err(|e| format!("frame {} b64: {}", idx, e))?;
            let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
                .map_err(|e| format!("frame {} decode: {}", idx, e))?;
            // Reject mismatched sizes — would corrupt the GIF.
            if img.width() as u16 != w || img.height() as u16 != h {
                return Err(format!(
                    "frame {} size {}x{} differs from frame 0 {}x{}",
                    idx,
                    img.width(),
                    img.height(),
                    w,
                    h
                ));
            }
            // RGBA → palette via NeuQuant.  speed 10 = balanced.
            let mut rgba = img.to_rgba8().into_raw();
            let mut frame = gif::Frame::from_rgba_speed(w, h, &mut rgba, 10);
            frame.delay = delay_cs;
            encoder
                .write_frame(&frame)
                .map_err(|e| format!("frame {} write: {}", idx, e))?;
        }
    }

    // Native save dialog.
    let path: Option<PathBuf> = app
        .dialog()
        .file()
        .add_filter("Animated GIF", &["gif"])
        .set_file_name("glyph-loop.gif")
        .blocking_save_file()
        .map(|fp| fp.into_path().ok())
        .flatten();

    if let Some(p) = path {
        fs::write(&p, &gif_buf).map_err(|e| e.to_string())?;
        Ok(p.display().to_string())
    } else {
        Err("cancelled".into())
    }
}

/// Batch-mode GIF write: encode frames to GIF and write directly to an
/// absolute path with no save dialog.  Used by the test batch driver to
/// generate hundreds of variation GIFs unattended.  Same encoder logic as
/// `save_gif_real` — only the dialog step is removed.
#[tauri::command]
async fn save_gif_to_path(
    frames: Vec<GifFrame>,
    delay_ms: u32,
    path: String,
) -> Result<String, String> {
    if frames.is_empty() {
        return Err("no frames provided".into());
    }
    let first_bytes = general_purpose::STANDARD
        .decode(&frames[0].b64)
        .map_err(|e| format!("frame 0: {}", e))?;
    let first_img = image::load_from_memory_with_format(&first_bytes, image::ImageFormat::Png)
        .map_err(|e| format!("frame 0 decode: {}", e))?;
    let (w, h) = (first_img.width() as u16, first_img.height() as u16);

    let mut gif_buf: Vec<u8> = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut gif_buf, w, h, &[])
            .map_err(|e| format!("gif encoder: {}", e))?;
        encoder
            .set_repeat(gif::Repeat::Infinite)
            .map_err(|e| format!("gif repeat: {}", e))?;
        let delay_cs = ((delay_ms as f32) / 10.0).round().max(2.0) as u16;
        for (idx, f) in frames.iter().enumerate() {
            let bytes = general_purpose::STANDARD
                .decode(&f.b64)
                .map_err(|e| format!("frame {} b64: {}", idx, e))?;
            let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
                .map_err(|e| format!("frame {} decode: {}", idx, e))?;
            if img.width() as u16 != w || img.height() as u16 != h {
                return Err(format!(
                    "frame {} size {}x{} differs from frame 0 {}x{}",
                    idx, img.width(), img.height(), w, h
                ));
            }
            let mut rgba = img.to_rgba8().into_raw();
            let mut frame = gif::Frame::from_rgba_speed(w, h, &mut rgba, 10);
            frame.delay = delay_cs;
            encoder
                .write_frame(&frame)
                .map_err(|e| format!("frame {} write: {}", idx, e))?;
        }
    }

    let p = PathBuf::from(&path);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir: {}", e))?;
    }
    fs::write(&p, &gif_buf).map_err(|e| e.to_string())?;
    Ok(p.display().to_string())
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

/// Read an image file from an absolute path and return a data URL.
/// Used by the Tauri drag-drop event fallback (when `dragDropEnabled: true`).
#[tauri::command]
async fn read_image_file(path: String) -> Result<String, String> {
    let p = PathBuf::from(&path);
    read_path_as_data_url(&p)
}

fn read_path_as_data_url(p: &PathBuf) -> Result<String, String> {
    let bytes = fs::read(p).map_err(|e| e.to_string())?;
    let _ = Cursor::new(&bytes); // sanity import use
    let b64 = general_purpose::STANDARD.encode(&bytes);
    let mime = match p
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
    };
    Ok(format!("data:{};base64,{}", mime, b64))
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
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            save_png,
            save_gif_real,
            save_gif_to_path,
            save_blob,
            pick_image,
            read_image_file,
            save_preset_json,
            load_preset_json,
            get_cli_render_job,
            exit_with_status,
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
                    let _ = window.set_position(tauri::Position::Physical(
                        tauri::PhysicalPosition { x: -32000, y: -32000 },
                    ));
                    let _ = window.set_size(tauri::Size::Physical(
                        tauri::PhysicalSize { width: 1500, height: 820 },
                    ));
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
