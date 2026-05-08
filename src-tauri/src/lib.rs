use base64::engine::general_purpose;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::Manager;
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
fn exit_with_status(
    _app: tauri::AppHandle,
    state: tauri::State<CliJobState>,
    ok: bool,
) {
    let code = if ok { 0 } else { 1 };
    if let Ok(mut guard) = state.exit_code.lock() {
        *guard = code;
    }
    if !ok {
        eprintln!("glyph-grid-studio: render reported failure (exit code {})", code);
    }
    std::process::exit(code);
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
fn encode_gif_gifski(
    frames: &[GifFrame],
    delay_ms: u32,
    cap_width: Option<u32>,
) -> Result<Vec<u8>, String> {
    if frames.is_empty() {
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

    // Decode all frames up-front. PNG decode is fast; doing it before
    // launching the collector thread lets us surface decode errors
    // synchronously.
    let mut decoded: Vec<(imgref::ImgVec<rgb::RGBA8>, f64)> = Vec::with_capacity(frames.len());
    for (idx, f) in frames.iter().enumerate() {
        let bytes = general_purpose::STANDARD
            .decode(&f.b64)
            .map_err(|e| format!("frame {} b64: {}", idx, e))?;
        let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
            .map_err(|e| format!("frame {} decode: {}", idx, e))?
            .to_rgba8();
        let (w, h) = (img.width() as usize, img.height() as usize);
        let pixels: Vec<rgb::RGBA8> = img
            .pixels()
            .map(|p| rgb::RGBA8 { r: p[0], g: p[1], b: p[2], a: p[3] })
            .collect();
        let imgvec = imgref::ImgVec::new(pixels, w, h);
        // Presentation timestamp for this frame, in seconds.
        let pts = (idx as f64) * (delay_ms as f64) / 1000.0;
        decoded.push((imgvec, pts));
    }

    // gifski's collector and writer must run on separate threads — the
    // collector quantises each frame as it arrives, the writer assembles
    // the final palette + LZW stream after the collector closes.
    let collector_handle = std::thread::spawn(move || -> Result<(), String> {
        for (idx, (imgvec, pts)) in decoded.into_iter().enumerate() {
            collector
                .add_frame_rgba(idx, imgvec, pts)
                .map_err(|e| format!("frame {} add: {}", idx, e))?;
        }
        // Dropping the collector signals the writer to finalise.
        drop(collector);
        Ok(())
    });

    struct Silent;
    impl gifski::progress::ProgressReporter for Silent {
        fn increase(&mut self) -> bool { true }
        fn done(&mut self, _msg: &str) {}
    }

    let mut output: Vec<u8> = Vec::new();
    writer
        .write(&mut output, &mut Silent)
        .map_err(|e| format!("gifski write: {}", e))?;
    collector_handle
        .join()
        .map_err(|_| "gifski collector thread panicked".to_string())??;

    Ok(output)
}

/// Save a recorded sequence of PNG frames as a single animated GIF.
/// Pops the native save dialog and writes the encoded bytes there.
/// `cap_width` (optional) resizes frames before encoding for smaller
/// output files (Twitter's 15 MB GIF limit, etc.).
#[tauri::command]
async fn save_gif_real(
    app: tauri::AppHandle,
    frames: Vec<GifFrame>,
    delay_ms: u32,
    cap_width: Option<u32>,
) -> Result<String, String> {
    let gif_buf = encode_gif_gifski(&frames, delay_ms, cap_width)?;

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

/// Batch-mode GIF write: encode frames and write directly to an absolute
/// path with no save dialog.  Used by the test batch driver and the
/// "Export GIF" button when running headlessly.
#[tauri::command]
async fn save_gif_to_path(
    frames: Vec<GifFrame>,
    delay_ms: u32,
    path: String,
    cap_width: Option<u32>,
) -> Result<String, String> {
    let gif_buf = encode_gif_gifski(&frames, delay_ms, cap_width)?;
    let p = PathBuf::from(&path);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir: {}", e))?;
    }
    fs::write(&p, &gif_buf).map_err(|e| e.to_string())?;
    Ok(p.display().to_string())
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
fn encode_mp4_h264(
    frames: &[GifFrame],
    fps: u32,
    cap_width: Option<u32>,
) -> Result<Vec<u8>, String> {
    use mp4::{AvcConfig, Mp4Config, Mp4Sample, Mp4Writer, TrackConfig};
    use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, RateControlMode};
    use openh264::formats::{RgbSliceU8, YUVBuffer};
    use openh264::OpenH264API;

    if frames.is_empty() {
        return Err("no frames provided".into());
    }
    if fps == 0 {
        return Err("fps must be > 0".into());
    }

    // Decode the first frame to learn source dimensions.
    let first_bytes = general_purpose::STANDARD
        .decode(&frames[0].b64)
        .map_err(|e| format!("frame 0 b64: {}", e))?;
    let first_img = image::load_from_memory_with_format(&first_bytes, image::ImageFormat::Png)
        .map_err(|e| format!("frame 0 decode: {}", e))?;
    let src_w = first_img.width();
    let src_h = first_img.height();

    // yuv420p requires even W and H.  When cap_width is set and smaller than
    // the source, scale H proportionally and round both to even.
    fn even(v: u32) -> u32 { v & !1 }
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

    // Walk frames, encode, capture SPS/PPS once and slice NALs per frame.
    let mut sps: Option<Vec<u8>> = None;
    let mut pps: Option<Vec<u8>> = None;
    let mut samples: Vec<(Vec<u8>, bool)> = Vec::with_capacity(frames.len());

    for (idx, f) in frames.iter().enumerate() {
        let bytes = general_purpose::STANDARD
            .decode(&f.b64)
            .map_err(|e| format!("frame {} b64: {}", idx, e))?;
        let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
            .map_err(|e| format!("frame {} decode: {}", idx, e))?;
        let img = if img.width() != target_w || img.height() != target_h {
            img.resize_exact(target_w, target_h, image::imageops::FilterType::Lanczos3)
        } else {
            img
        };
        let rgb = img.to_rgb8();
        let rgb_data: Vec<u8> = rgb.into_raw();

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
                    7 => { if sps.is_none() { sps = Some(payload.to_vec()); } }
                    8 => { if pps.is_none() { pps = Some(payload.to_vec()); } }
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
    }

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
        timescale: 1000,
    };
    let buf = std::io::Cursor::new(Vec::<u8>::new());
    let mut writer = Mp4Writer::write_start(buf, &mp4_config)
        .map_err(|e| format!("mp4 write_start: {}", e))?;

    let track_config = TrackConfig::from(AvcConfig {
        width: target_w as u16,
        height: target_h as u16,
        seq_param_set: sps,
        pic_param_set: pps,
    });
    writer
        .add_track(&track_config)
        .map_err(|e| format!("mp4 add_track: {}", e))?;

    // Frame duration in timescale ticks (timescale = 1000).  Use exact
    // 1000 / fps so total duration ≈ frames / fps.
    let frame_dur_ticks: u32 = (1000.0 / fps as f64).round().max(1.0) as u32;

    for (idx, (bytes, is_idr)) in samples.into_iter().enumerate() {
        let sample = Mp4Sample {
            start_time: (idx as u64) * (frame_dur_ticks as u64),
            duration: frame_dur_ticks,
            rendering_offset: 0,
            is_sync: is_idr,
            bytes: bytes.into(),
        };
        writer
            .write_sample(1, &sample)
            .map_err(|e| format!("write_sample {}: {}", idx, e))?;
    }

    writer.write_end().map_err(|e| format!("mp4 write_end: {}", e))?;
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
) -> Result<String, String> {
    let mp4_buf = encode_mp4_h264(&frames, fps, cap_width)?;

    let path: Option<PathBuf> = app
        .dialog()
        .file()
        .add_filter("MP4 video", &["mp4"])
        .set_file_name("glyph-loop.mp4")
        .blocking_save_file()
        .map(|fp| fp.into_path().ok())
        .flatten();

    if let Some(p) = path {
        fs::write(&p, &mp4_buf).map_err(|e| e.to_string())?;
        Ok(p.display().to_string())
    } else {
        Err("cancelled".into())
    }
}

/// Batch-mode MP4 write: encode frames and write to an absolute path with
/// no save dialog.  Used by `runBatchExport` and the headless `--format mp4`
/// CLI path.
#[tauri::command]
async fn save_mp4_to_path(
    frames: Vec<GifFrame>,
    fps: u32,
    path: String,
    cap_width: Option<u32>,
) -> Result<String, String> {
    let mp4_buf = encode_mp4_h264(&frames, fps, cap_width)?;
    let p = PathBuf::from(&path);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir: {}", e))?;
    }
    fs::write(&p, &mp4_buf).map_err(|e| e.to_string())?;
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
    //    directory on macOS.
    let mut candidate: Option<PathBuf> = None;
    if let Ok(resource_dir) = app.path().resource_dir() {
        let p = resource_dir.join("USER-GUIDE.md");
        if p.exists() {
            candidate = Some(p);
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
                    if pp.exists() { candidate = Some(pp); }
                }
            }
        }
    }

    let path = candidate.ok_or_else(|| "USER-GUIDE.md not found in resources or project root".to_string())?;
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
            save_blob,
            cli_log,
            pick_image,
            read_image_file,
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
