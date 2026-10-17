//! MCP server — exposes Glyph Grid Studio's rendering pipeline as
//! Model Context Protocol tools over stdio JSON-RPC.
//!
//! Architecture: each tool call spawns a subprocess that re-invokes
//! this same binary in `render` or `batch` mode. This avoids the
//! "only one Tauri runtime per process" constraint and keeps each
//! render isolated. Cost: ~5s of process spawn + Tauri startup per
//! call — which is why `glyph_grid_batch` exists: N jobs share one
//! session (ITER-024: 6.2 s/job vs ~3 s start-up per spawn).
//!
//! F22 (ITER-058): the tools are async — the child runs under
//! `tokio::process`, so the server keeps answering pings/cancels during a
//! render; a cancelled call kills its child; and the child's `batch: N/M`
//! stderr lines become `notifications/progress` when the client sent a
//! `progressToken`.
//!
//! Wired tools:
//!   - glyph_grid_render   — render an image to an animated GIF / MP4
//!   - glyph_grid_batch    — N render jobs (one manifest) in one session
//!   - glyph_grid_catalog  — list every palette / ramp / dither / glyph set / postprocess stage
//!
//! Future tools (not yet wired): glyph_grid_snapshot (single PNG),
//! glyph_grid_presets (list bundled), glyph_grid_save_preset.

use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, Content, ProgressNotificationParam},
    schemars,
    service::{RequestContext, RoleServer},
    tool, tool_router,
};
use std::process::Stdio;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;

/* ---------- Request schemas ---------- */

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RenderParams {
    /// Absolute path to the source image (PNG / JPG / WebP / GIF / BMP / TIFF)
    /// or video (MP4 / MOV / M4V / WebM; R4 — H.264 decodes frame-exactly
    /// in Rust, other codecs through the WebView).  Optional when `text`, `srt_path` or `text_options.enabled` gives a
    /// text source (the text is rendered as the source raster).
    #[serde(default)]
    pub in_path: Option<String>,
    /// Absolute path to write the output file (GIF / MP4 / text, per `format`).
    pub out_path: String,
    /// Frames in the output animation. Default 24.
    #[serde(default)]
    pub frames: Option<u32>,
    /// Palette name (see catalog tool).
    #[serde(default)]
    pub palette: Option<String>,
    /// Color mode: preserve | monochrome | duotone | gradient.
    #[serde(default)]
    pub color_mode: Option<String>,
    /// Glyph ramp.
    #[serde(default)]
    pub ramp: Option<String>,
    /// Dither mode.
    #[serde(default)]
    pub dither: Option<String>,
    /// Selection mode.
    #[serde(default)]
    pub selection_mode: Option<String>,
    /// Glyph set.
    #[serde(default)]
    pub glyph_set: Option<String>,
    /// Sampling strategy.
    #[serde(default)]
    pub sampling_strategy: Option<String>,
    /// Postprocess stages to enable. May be empty.
    #[serde(default)]
    pub postprocess: Vec<String>,
    /// Grid columns.
    #[serde(default)]
    pub cols: Option<u32>,
    /// Grid rows.
    #[serde(default)]
    pub rows: Option<u32>,
    /// Output format: `gif` (default), `mp4`, or the grid as text: `txt`,
    /// `ansi` (24-bit colour escapes), `cast` (asciinema v2), `html`
    /// (single file, font inlined).  txt/ansi are one still frame and their
    /// body is also returned inline (up to 64 KB) so a text-only model can
    /// read the result.  When omitted, inferred from `out_path`'s extension.
    #[serde(default)]
    pub format: Option<String>,
    /// Any partial CONFIG object (see src/glyph-grid.schema.json — the same
    /// shape as a preset file).  Lowest priority: the named params above
    /// deep-merge on top of it.  Reaches every CONFIG key (seed,
    /// animation.fps/duration, dispersal, temporalStability, ...).
    #[serde(default)]
    pub config: Option<serde_json::Value>,
    /// Absolute path to a preset JSON file (a partial CONFIG, e.g. one the
    /// GUI exported).  Below `config` and the named params: `config`
    /// deep-merges onto it, then the named params on top.
    #[serde(default)]
    pub preset: Option<String>,
    /// Give up after this many seconds (the render child exits 3 → error
    /// code `timeout`).  Default: no overall limit; a render with no
    /// progress for 300 s still times out.
    #[serde(default)]
    pub timeout_s: Option<u64>,
    /// MP4 rate control: `upload` (default; ~5 Mbps, a few MB per loop) or
    /// `archive` (fixed QP 26, near-lossless, ~30-60 Mbps). Ignored for GIF.
    #[serde(default)]
    pub mp4_quality: Option<String>,
    /// Kinetic typography (R3): render this text as the source, one line
    /// over the whole loop (`\n` stacks rows).  With `in_path` too and
    /// `text_options.blend` multiply / screen / mask, it is composited onto
    /// the image.
    #[serde(default)]
    pub text: Option<String>,
    /// Absolute path to a .srt (subtitles) or .lrc (lyrics) file: its timed
    /// cues become text lines and the last cue's end sets the loop length.
    #[serde(default)]
    pub srt_path: Option<String>,
    /// Partial `textSource` block (schema: src/glyph-grid.schema.json):
    /// `{anim: typewriter|rise|fade|scale|none, font, size, weight, align,
    /// x, y, easeIn, easeOut, bg, fg, blend: replace|multiply|screen|mask,
    /// w, h, lines:[{text, start, end, ...}]}`.  Deep-merges onto `config`.
    #[serde(default)]
    pub text_options: Option<serde_json::Value>,
    /// Video sources (R4): `auto` (default — H.264 MP4/MOV decoded in Rust,
    /// anything else through the WebView `<video>` path), `native` (Rust
    /// only; a non-H.264 clip is an error) or `webview` (always the
    /// `<video>` path).  Ignored for image sources.
    #[serde(default)]
    pub video_decode: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BatchParams {
    /// Absolute path to the default source image for every job.
    #[serde(rename = "in")]
    pub in_path: String,
    /// Default frame count for every job: a positive integer or "auto"
    /// (= animation.duration x fps). Default 24.
    #[serde(default)]
    pub frames: Option<serde_json::Value>,
    /// Emit PERF_JOB timing lines on stderr (optimization-loop use).
    #[serde(default)]
    pub perf: Option<bool>,
    /// The jobs, rendered in order in ONE app session. Each is
    /// `{name, out, format?, in?, frames?, capWidth?, targetMaxBytes?,
    /// mp4Quality?: "upload"|"archive", border?: {width, bgColor?, style?,
    /// inkColor?}, config?: {partial CONFIG}}`
    /// — the same shape as a `batch --manifest` file. `out` must be an
    /// absolute path.
    pub jobs: Vec<serde_json::Value>,
    /// Give up on the whole batch after this many seconds (error code
    /// `timeout`).  Default: no overall limit (300 s without progress
    /// still times out).
    #[serde(default)]
    pub timeout_s: Option<u64>,
    /// Skip jobs whose `out` already holds a valid GIF/MP4 of the requested
    /// frame count (`batch --resume`).
    #[serde(default)]
    pub resume: Option<bool>,
}

/// Temp JSON file (a preset for `config`, a manifest for `glyph_grid_batch`);
/// deleted on drop.
struct TempPreset(std::path::PathBuf);

impl TempPreset {
    fn write(kind: &str, value: &serde_json::Value) -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!(
            "glyph-grid-mcp-{}-{}-{}.json",
            kind,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&path, value.to_string())
            .map_err(|e| format!("error: cannot write temp {} {}: {}", kind, path.display(), e))?;
        Ok(TempPreset(path))
    }
}

impl Drop for TempPreset {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// `batch: 3/8 name → path` (the JS driver's per-job cli_log line, which
/// reaches stderr as `glyph-grid-studio[js]: batch: 3/8 …`) → (3, 8).
fn parse_batch_progress(line: &str) -> Option<(u64, u64)> {
    let rest = &line[line.find("batch: ")? + "batch: ".len()..];
    let frac = rest.split_whitespace().next()?;
    let (n, m) = frac.split_once('/')?;
    Some((n.parse().ok()?, m.parse().ok()?))
}

/// `--progress` capture line → (frame, total).  Encode-phase lines are
/// skipped: MCP progress must increase, and capture is the long phase.
fn parse_frame_progress(line: &str) -> Option<(u64, u64)> {
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("type")?.as_str()? != "progress" || v.get("phase")?.as_str()? != "capture" {
        return None;
    }
    Some((v.get("frame")?.as_u64()?, v.get("total")?.as_u64()?))
}

/// F26 (ITER-060): stop a child cleanly — SIGTERM lets its watchdog remove
/// its `.part` files and exit 143; SIGKILL after 2 s if it has not.
async fn stop_child(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        crate::terminate_child(pid);
        if tokio::time::timeout(std::time::Duration::from_secs(2), child.wait())
            .await
            .is_ok()
        {
            return;
        }
    }
    let _ = child.kill().await;
}

/// How a spawned child ended.
enum ChildEnd {
    Exited(std::process::ExitStatus, String),
    Cancelled,
}

/// Run `cmd` to completion without blocking a tokio worker.  stdin/stdout
/// are null — stdout is the JSON-RPC channel, a child writing to it would
/// corrupt the stream (std `output()` captured it implicitly).  stderr is
/// read line by line: kept for the error report, and `batch: N/M` lines
/// are forwarded as progress notifications when the client asked for
/// them.  A cancelled request kills the child.
async fn run_child(
    mut cmd: Command,
    ctx: &RequestContext<RoleServer>,
    frame_progress: bool,
) -> Result<ChildEnd, String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("subprocess failed to start: {}", e))?;
    let mut lines = tokio::io::BufReader::new(child.stderr.take().expect("stderr piped")).lines();
    let token = ctx.meta.get_progress_token();
    let mut stderr = String::new();
    loop {
        tokio::select! {
            _ = ctx.ct.cancelled() => {
                stop_child(&mut child).await;
                return Ok(ChildEnd::Cancelled);
            }
            line = lines.next_line() => match line {
                Ok(Some(line)) => {
                    let frame = if frame_progress { parse_frame_progress(&line) } else { None };
                    if frame.is_some() || line.starts_with("{\"type\":\"progress\"") {
                        // NDJSON progress is not kept for the error report.
                        if let (Some(token), Some((n, m))) = (&token, frame) {
                            let _ = ctx
                                .peer
                                .notify_progress(
                                    ProgressNotificationParam::new(token.clone(), n as f64)
                                        .with_total(m as f64)
                                        .with_message(format!("capture {}/{}", n, m)),
                                )
                                .await;
                        }
                        continue;
                    }
                    // Per-job `batch: N/M` lines are the batch tool's progress;
                    // a render (one job, `batch: 1/1`) reports frames instead.
                    let job = if frame_progress { None } else { parse_batch_progress(&line) };
                    if let (Some(token), Some((n, m))) = (&token, job) {
                        let _ = ctx
                            .peer
                            .notify_progress(
                                ProgressNotificationParam::new(token.clone(), n as f64)
                                    .with_total(m as f64)
                                    .with_message(line.clone()),
                            )
                            .await;
                    }
                    stderr.push_str(&line);
                    stderr.push('\n');
                }
                // EOF (or an unreadable pipe): the child is exiting.
                Ok(None) | Err(_) => break,
            },
        }
    }
    let status = tokio::select! {
        _ = ctx.ct.cancelled() => {
            stop_child(&mut child).await;
            return Ok(ChildEnd::Cancelled);
        }
        status = child.wait() => status.map_err(|e| format!("subprocess wait failed: {}", e))?,
    };
    Ok(ChildEnd::Exited(status, stderr))
}

/* ---------- F25 (ITER-059): machine-readable errors ----------
 *
 * Every failure is an `isError: true` result whose text (and
 * `structuredContent`) is one JSON object:
 *   {code, path, message, exit_code?, stderr_tail?}
 * code: invalid_arguments (child exit 2 / rejected in-process) · timeout
 * (exit 3) · render_failed (exit 1) · no_output (exit 0, no file) ·
 * interrupted (killed by a signal) · cancelled · internal.
 * path: JSON pointer into this tool's arguments when the culprit is known
 * (`/config/dither/mode`, `/jobs/1/config/selectionMode`, `/in_path`),
 * else null. */

fn tool_error(code: &str, path: Option<String>, message: String) -> CallToolResult {
    CallToolResult::structured_error(serde_json::json!({
        "code": code,
        "path": path,
        "message": message,
    }))
}

/// The argument a CLI / validator message blames, as a JSON pointer.
fn error_path(text: &str, batch: bool) -> Option<String> {
    for line in text.lines() {
        let t = line.trim();
        // render: "invalid config:\n  config/dither/mode: ..." (build_js_config)
        if let Some(rest) = t.strip_prefix("config/") {
            let p = rest.split(':').next().unwrap_or("");
            return Some(format!("/config/{}", p));
        }
        // batch (ITER-061 manifest schema): `batch: manifest /jobs/0: Additional properties ...`
        if let Some(i) = t.find("batch: manifest /") {
            let p = t[i + "batch: manifest ".len()..].split(':').next().unwrap_or("");
            return Some(p.to_string());
        }
        // batch: `batch: job 1 ("typo") config /selectionMode: ...`
        if let Some(i) = t.find("batch: job ") {
            let rest = &t[i + "batch: job ".len()..];
            let idx = rest.split_whitespace().next().unwrap_or("");
            if idx.parse::<usize>().is_ok() {
                if let Some(c) = rest.find(" config /") {
                    let p = rest[c + " config ".len()..].split(':').next().unwrap_or("");
                    return Some(format!("/jobs/{}/config{}", idx, p));
                }
                return Some(format!("/jobs/{}", idx));
            }
        }
        if t.contains("cannot resolve --in") || t.contains("cannot resolve manifest `in`") {
            return Some(if batch { "/in" } else { "/in_path" }.to_string());
        }
        if t.contains("--postprocess") {
            return Some("/postprocess".to_string());
        }
        if t.contains("--preset") {
            return Some("/preset".to_string());
        }
    }
    None
}

/// ITER-062: `error_path` reads `/config/<ptr>` off the MERGED render
/// config (preset → config → named params).  Blame what the caller sent:
/// the named param that wrote `<ptr>`, else `/config<ptr>` when the
/// `config` argument holds it, else `/preset<ptr>` (the preset file).
fn render_error_path(p: &RenderParams, path: Option<String>) -> Option<String> {
    let path = path?;
    let Some(ptr) = path.strip_prefix("/config") else {
        return Some(path);
    };
    let named: [(&str, bool, &str); 10] = [
        ("/palette", p.palette.is_some(), "/palette"),
        ("/colorMode", p.color_mode.is_some(), "/color_mode"),
        ("/ramp", p.ramp.is_some(), "/ramp"),
        ("/dither/mode", p.dither.is_some(), "/dither"),
        ("/selectionMode", p.selection_mode.is_some(), "/selection_mode"),
        ("/glyphSet", p.glyph_set.is_some(), "/glyph_set"),
        ("/samplingStrategy", p.sampling_strategy.is_some(), "/sampling_strategy"),
        ("/grid/cols", p.cols.is_some(), "/cols"),
        ("/grid/rows", p.rows.is_some(), "/rows"),
        ("/video/decode", p.video_decode.is_some(), "/video_decode"),
    ];
    for (at, sent, arg) in named {
        if sent && (ptr == at || ptr.starts_with(&format!("{}/", at))) {
            return Some(arg.to_string());
        }
    }
    if !p.postprocess.is_empty() && ptr.starts_with("/postprocess/") && ptr.ends_with("/enabled") {
        return Some("/postprocess".to_string());
    }
    // ITER-076: text_options is merged as config.textSource.
    if let (Some(opts), Some(rest)) = (&p.text_options, ptr.strip_prefix("/textSource")) {
        if rest.is_empty() || opts.pointer(rest).is_some() {
            return Some(format!("/text_options{}", rest));
        }
    }
    match (&p.config, &p.preset) {
        (Some(cfg), Some(_)) if cfg.pointer(ptr).is_none() => Some(format!("/preset{}", ptr)),
        (Some(_), _) => Some(format!("/config{}", ptr)),
        (None, Some(_)) => Some(format!("/preset{}", ptr)),
        (None, None) => None,
    }
}

/// A child that exited non-zero → the structured error for its exit code.
fn child_error(
    what: &str,
    status: std::process::ExitStatus,
    stderr: &str,
    batch: bool,
    blame: impl Fn(Option<String>) -> Option<String>,
) -> CallToolResult {
    let code = match status.code() {
        Some(crate::EXIT_USAGE) => "invalid_arguments",
        Some(crate::EXIT_TIMEOUT) => "timeout",
        Some(130) | Some(143) | None => "interrupted",
        _ => "render_failed",
    };
    let lines: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(20)..].join("\n");
    let last = lines.last().copied().unwrap_or("");
    let path = if code == "invalid_arguments" { blame(error_path(stderr, batch)) } else { None };
    CallToolResult::structured_error(serde_json::json!({
        "code": code,
        "path": path,
        "message": format!("{} failed: exit {} | {}", what, status.code().unwrap_or(-1), last),
        "exit_code": status.code(),
        "stderr_tail": tail,
    }))
}

/// The canonical output format of a render call: `format` through the
/// CLI's own parser (any case / padding), else inferred from `out_path`.
fn render_format(format: Option<&str>, out_path: &str) -> String {
    format
        .and_then(|f| crate::parse_format_arg(f).ok())
        .unwrap_or_else(|| crate::format_from_path(out_path).to_string())
}

/// Inline bodies stop here: a 240x120 TXT is ~29 KB, ANSI larger.
const INLINE_TEXT_MAX: u64 = 64 * 1024;

/// The output file's text for `txt` / `ansi` renders up to
/// `INLINE_TEXT_MAX` bytes, else None (binary formats, cast/html, or too
/// big — the path is still returned).
fn inline_text_body(format: &str, out_path: &str) -> Option<String> {
    if format != "txt" && format != "ansi" {
        return None;
    }
    let meta = std::fs::metadata(out_path).ok()?;
    if meta.len() > INLINE_TEXT_MAX {
        return None;
    }
    std::fs::read_to_string(out_path).ok()
}

/* ---------- Server handler ---------- */

#[derive(Debug, Clone)]
pub struct GlyphGridServer;

#[tool_router(server_handler)]
impl GlyphGridServer {
    #[tool(
        description = "Render an image, or text (`text` / `srt_path` — kinetic typography, no image needed), \
                       to an animated character-grid GIF (ASCII / Unicode glyphs). \
                       Returns the absolute path of the written file (GIF, MP4, or TXT/ANSI/CAST/HTML text, per \
                       `format`) on success; for txt/ansi the text itself (up to 64 KB) follows the path."
    )]
    async fn glyph_grid_render(
        &self,
        Parameters(p): Parameters<RenderParams>,
        ctx: RequestContext<RoleServer>,
    ) -> CallToolResult {
        // F24 (ITER-057): a `config` object travels as a temp preset file —
        // the CLI already layers flags onto `--preset`, so no new CLI
        // surface is needed.  Removed when this call returns.
        // F22 (ITER-058): a `preset` path alone is forwarded as-is; with a
        // `config` too, the config deep-merges onto the file's contents
        // (the CLI takes one --preset).
        if let Some(other) = p.config.as_ref().filter(|c| !c.is_object()) {
            return tool_error(
                "invalid_arguments",
                Some("/config".into()),
                format!("invalid arguments: `config` must be a JSON object, got {}", other),
            );
        }
        if let Some(other) = p.text_options.as_ref().filter(|c| !c.is_object()) {
            return tool_error(
                "invalid_arguments",
                Some("/text_options".into()),
                format!("invalid arguments: `text_options` must be a JSON object, got {}", other),
            );
        }
        // F25 (ITER-059): the CLI would exit 2 on this after ~0.1 s; answer
        // in-process with the argument path instead.
        if let Some(in_path) = p.in_path.as_ref().filter(|ip| !std::path::Path::new(ip).is_file()) {
            return tool_error(
                "invalid_arguments",
                Some("/in_path".into()),
                format!("invalid arguments: in_path {} is not a readable file", in_path),
            );
        }
        if let Some(Err(e)) = p.srt_path.as_deref().map(|sp| crate::read_text_import(std::path::Path::new(sp))) {
            return tool_error("invalid_arguments", Some("/srt_path".into()), format!("invalid arguments: {}", e));
        }
        // ITER-076: text_options travels as config.textSource.
        let config_arg: Option<serde_json::Value> = match (&p.config, &p.text_options) {
            (c, None) => c.clone(),
            (c, Some(opts)) => {
                let mut v = c.clone().unwrap_or_else(|| serde_json::json!({}));
                crate::json_deep_merge(&mut v, serde_json::json!({ "textSource": opts }));
                Some(v)
            }
        };
        let mut temp = None;
        let preset_path: Option<std::path::PathBuf> = match (&p.preset, &config_arg) {
            (None, None) => None,
            (Some(path), None) => Some(std::path::PathBuf::from(path)),
            (base, Some(cfg)) => {
                let mut merged = serde_json::json!({});
                if let Some(path) = base {
                    match std::fs::read_to_string(path)
                        .map_err(|e| e.to_string())
                        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).map_err(|e| e.to_string()))
                    {
                        Ok(v @ serde_json::Value::Object(_)) => merged = v,
                        Ok(other) => {
                            return tool_error(
                                "invalid_arguments",
                                Some("/preset".into()),
                                format!("invalid arguments: preset {} must be a JSON object, got {}", path, other),
                            )
                        }
                        Err(e) => {
                            return tool_error(
                                "invalid_arguments",
                                Some("/preset".into()),
                                format!("invalid arguments: cannot read preset {}: {}", path, e),
                            )
                        }
                    }
                }
                crate::json_deep_merge(&mut merged, cfg.clone());
                match TempPreset::write("preset", &merged) {
                    Ok(t) => {
                        let path = t.0.clone();
                        temp = Some(t);
                        Some(path)
                    }
                    Err(e) => return tool_error("internal", None, e),
                }
            }
        };

        // R7 (ITER-056): validate the arguments against the preset schema
        // in-process, before paying ~5 s of Tauri start-up for a render the
        // CLI would reject anyway.  Same resolver as `render`.
        let probe = crate::HeadlessRenderJob {
            in_path: p.in_path.as_ref().map(std::path::PathBuf::from),
            out_path: std::path::PathBuf::from(&p.out_path),
            frames: Some(p.frames.unwrap_or(24)),
            show_window: false,
            preset_path: preset_path.clone(),
            config_overrides: crate::ConfigOverrides {
                palette: p.palette.clone(),
                color_mode: p.color_mode.clone(),
                ramp: p.ramp.clone(),
                dither_mode: p.dither.clone(),
                selection_mode: p.selection_mode.clone(),
                glyph_set: p.glyph_set.clone(),
                sampling_strategy: p.sampling_strategy.clone(),
                postprocess: p.postprocess.clone(),
                cols: p.cols,
                rows: p.rows,
                set: Vec::new(),
                timeline: Vec::new(),
                text: p.text.clone(),
                video_decode: p.video_decode.clone(),
                ..Default::default()
            },
            format: p.format.clone(),
            timeout_secs: p.timeout_s,
            mp4_quality: p.mp4_quality.clone(),
            text_import: p.srt_path.as_ref().map(std::path::PathBuf::from),
        };
        // ITER-071: a typo'd mp4_quality is refused here too, not after a
        // spawn (the CLI's clap parser would exit 2 anyway).
        if let Some(Err(e)) = p.mp4_quality.as_deref().map(crate::parse_mp4_quality_arg) {
            return tool_error("invalid_arguments", Some("/mp4_quality".into()), e);
        }
        // ITER-074: same parser as `render --format`.
        if let Some(Err(e)) = p.format.as_deref().map(crate::parse_format_arg) {
            return tool_error("invalid_arguments", Some("/format".into()), e);
        }
        // ITER-078: text formats are only written to text extensions.
        if let Some(e) = crate::text_out_mismatch(&render_format(p.format.as_deref(), &p.out_path), &p.out_path) {
            return tool_error("invalid_arguments", Some("/out_path".into()), e);
        }
        match probe.build_js_config() {
            Err(e) => {
                return tool_error(
                    "invalid_arguments",
                    render_error_path(&p, error_path(&e, false)),
                    format!("invalid arguments: {}", e),
                );
            }
            // ITER-076: in_path is optional only for a text source.
            Ok((cfg, _)) => {
                if let Err(e) = crate::require_source(&probe, &cfg) {
                    return tool_error(
                        "invalid_arguments",
                        Some("/in_path".into()),
                        format!("invalid arguments: {} (MCP: in_path, text, srt_path or text_options.enabled)", e),
                    );
                }
            }
        }

        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => return tool_error("internal", None, format!("cannot locate own binary: {}", e)),
        };

        let mut cmd = Command::new(&exe);
        cmd.arg("render");
        if let Some(in_path) = &p.in_path {
            cmd.arg("--in").arg(in_path);
        }
        cmd.arg("--out").arg(&p.out_path);
        if let Some(v) = &p.text {
            cmd.arg("--text").arg(v);
        }
        if let Some(v) = &p.srt_path {
            cmd.arg("--srt").arg(v);
        }
        if let Some(f) = p.frames {
            cmd.arg("--frames").arg(f.to_string());
        }
        if let Some(path) = &preset_path {
            cmd.arg("--preset").arg(path);
        }
        if let Some(v) = &p.palette {
            cmd.arg("--palette").arg(v);
        }
        if let Some(v) = &p.color_mode {
            cmd.arg("--color-mode").arg(v);
        }
        if let Some(v) = &p.ramp {
            cmd.arg("--ramp").arg(v);
        }
        if let Some(v) = &p.dither {
            cmd.arg("--dither").arg(v);
        }
        if let Some(v) = &p.selection_mode {
            cmd.arg("--selection-mode").arg(v);
        }
        if let Some(v) = &p.glyph_set {
            cmd.arg("--glyph-set").arg(v);
        }
        if let Some(v) = &p.sampling_strategy {
            cmd.arg("--sampling-strategy").arg(v);
        }
        for stage in &p.postprocess {
            cmd.arg("--postprocess").arg(stage);
        }
        if let Some(v) = p.cols {
            cmd.arg("--cols").arg(v.to_string());
        }
        if let Some(v) = p.rows {
            cmd.arg("--rows").arg(v.to_string());
        }
        if let Some(v) = &p.format {
            cmd.arg("--format").arg(v);
        }
        if let Some(v) = &p.mp4_quality {
            cmd.arg("--mp4-quality").arg(v);
        }
        if let Some(v) = &p.video_decode {
            cmd.arg("--video-decode").arg(v);
        }
        if let Some(v) = p.timeout_s {
            cmd.arg("--timeout").arg(v.to_string());
        }
        // F26 (ITER-060): per-frame progress → notifications/progress.
        cmd.arg("--progress");

        let end = run_child(cmd, &ctx, true).await;
        drop(temp);
        match end {
            Ok(ChildEnd::Exited(status, _)) if status.success() => {
                // Verify the output file exists and report size.
                match std::fs::metadata(&p.out_path) {
                    Ok(meta) => {
                        let mut content = vec![Content::text(format!(
                            "Rendered {} ({} KB)",
                            p.out_path,
                            meta.len() / 1024
                        ))];
                        // ITER-074 (R2): txt/ansi come back as text too, so
                        // a model without vision can judge the render.
                        if let Some(body) = inline_text_body(&probe.resolved_format(), &p.out_path) {
                            content.push(Content::text(body));
                        }
                        CallToolResult::success(content)
                    }
                    // F25: exit 0 without the file is a failure, not a success.
                    Err(_) => tool_error(
                        "no_output",
                        Some("/out_path".into()),
                        format!("render exited 0 but wrote no file at {}", p.out_path),
                    ),
                }
            }
            Ok(ChildEnd::Exited(status, stderr)) => {
                child_error("Render", status, &stderr, false, |x| render_error_path(&p, x))
            }
            Ok(ChildEnd::Cancelled) => tool_error("cancelled", None, "Render cancelled (subprocess killed)".into()),
            Err(e) => tool_error("internal", None, format!("Render {}", e)),
        }
    }

    #[tool(
        description = "Render several jobs (same or different sources, any CONFIG each) in ONE app \
                       session — the start-up cost is paid once, not per job. Arguments are a \
                       `batch --manifest` object: {in, frames?, jobs:[{name, out, format?, in?, \
                       frames?, capWidth?, mp4Quality?, border?, config?}]}. Sends notifications/progress per \
                       finished job when the request carries a progressToken. Returns one line per job."
    )]
    async fn glyph_grid_batch(
        &self,
        Parameters(p): Parameters<BatchParams>,
        ctx: RequestContext<RoleServer>,
    ) -> CallToolResult {
        // F22 Tier A (ITER-058): the manifest travels as a temp file to
        // `batch --manifest`, which validates every job (exit 2, before
        // Tauri starts) exactly like the CLI.
        let mut manifest = serde_json::json!({ "in": p.in_path, "jobs": p.jobs });
        if let Some(f) = &p.frames {
            manifest["frames"] = f.clone();
        }
        if let Some(perf) = p.perf {
            manifest["perf"] = serde_json::Value::Bool(perf);
        }
        let temp = match TempPreset::write("manifest", &manifest) {
            Ok(t) => t,
            Err(e) => return tool_error("internal", None, e),
        };
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => return tool_error("internal", None, format!("cannot locate own binary: {}", e)),
        };
        let mut cmd = Command::new(&exe);
        cmd.arg("batch").arg("--manifest").arg(&temp.0);
        if p.resume == Some(true) {
            cmd.arg("--resume");
        }
        if let Some(v) = p.timeout_s {
            cmd.arg("--timeout").arg(v.to_string());
        }

        let end = run_child(cmd, &ctx, false).await;
        drop(temp);
        match end {
            Ok(ChildEnd::Exited(status, stderr)) => {
                let mut report = Vec::new();
                for (i, j) in p.jobs.iter().enumerate() {
                    let name = j.get("name").and_then(|v| v.as_str()).unwrap_or("job");
                    let out = j.get("out").and_then(|v| v.as_str()).unwrap_or("");
                    report.push(match std::fs::metadata(out) {
                        Ok(meta) => {
                            format!("job {} ({}): {} ({} KB)", i, name, out, meta.len() / 1024)
                        }
                        _ => format!("job {} ({}): no output at {}", i, name, out),
                    });
                }
                if status.success() {
                    CallToolResult::success(vec![Content::text(format!(
                        "Batch complete: {} jobs\n{}",
                        p.jobs.len(),
                        report.join("\n")
                    ))])
                } else {
                    // Structured error first; the per-job report (which
                    // outputs do exist after a partial failure) second.
                    let mut r = child_error("Batch", status, &stderr, true, |x| x);
                    r.content.push(Content::text(report.join("\n")));
                    r
                }
            }
            Ok(ChildEnd::Cancelled) => tool_error("cancelled", None, "Batch cancelled (subprocess killed)".into()),
            Err(e) => tool_error("internal", None, format!("Batch {}", e)),
        }
    }

    #[tool(
        description = "List all available palettes, ramps, dither modes, selection modes, \
                       glyph sets, sampling strategies, and postprocess stages. Returns JSON."
    )]
    fn glyph_grid_catalog(&self) -> String {
        crate::catalog_json()
    }
}

/* ---------- Server entry point ---------- */

/// Boot the MCP server: speak JSON-RPC 2.0 on stdin/stdout. Blocks until EOF.
pub async fn run() -> anyhow::Result<()> {
    use rmcp::{transport::stdio, ServiceExt};

    // Trace to stderr so it doesn't pollute the JSON-RPC channel on stdout.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();

    tracing::info!("Glyph Grid MCP server starting on stdio");
    let service = GlyphGridServer.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{error_path, parse_batch_progress, parse_frame_progress, render_error_path, RenderParams};

    #[test]
    fn render_errors_blame_what_the_caller_sent() {
        let params = |v: serde_json::Value| -> RenderParams { serde_json::from_value(v).unwrap() };
        let at = |p: &RenderParams, ptr: &str| render_error_path(p, Some(format!("/config{}", ptr)));
        let base = serde_json::json!({"in_path": "/a.png", "out_path": "/a.gif"});
        // A preset file alone → /preset/<ptr>, not /config/...
        let mut v = base.clone();
        v["preset"] = "/p.json".into();
        assert_eq!(at(&params(v.clone()), "/dither/mode").as_deref(), Some("/preset/dither/mode"));
        // Named params are blamed by name.
        v["dither"] = "STBN".into();
        v["cols"] = 7.into();
        v["palette"] = "nope".into();
        let p = params(v);
        assert_eq!(at(&p, "/dither/mode").as_deref(), Some("/dither"));
        assert_eq!(at(&p, "/grid/cols").as_deref(), Some("/cols"));
        assert_eq!(at(&p, "/palette").as_deref(), Some("/palette"));
        assert_eq!(at(&p, "/dither").as_deref(), Some("/preset/dither"));  // e.g. a typo'd key in the file
        // `config` holds it → /config; config + preset, not in config → /preset.
        let mut v = base.clone();
        v["config"] = serde_json::json!({"seed": 3, "dither": {"mode": "STBN"}});
        assert_eq!(at(&params(v.clone()), "/dither/mode").as_deref(), Some("/config/dither/mode"));
        v["preset"] = "/p.json".into();
        assert_eq!(at(&params(v.clone()), "/ramp").as_deref(), Some("/preset/ramp"));
        assert_eq!(at(&params(v), "/seed").as_deref(), Some("/config/seed"));
        // Non-config paths pass through; nothing sent → no guess.
        let p = params(base);
        assert_eq!(render_error_path(&p, Some("/in_path".into())).as_deref(), Some("/in_path"));
        assert_eq!(at(&p, "/palette"), None);
    }

    #[test]
    fn frame_progress_lines_are_parsed() {
        assert_eq!(
            parse_frame_progress(r#"{"type":"progress","job":"cli","phase":"capture","frame":12,"total":120}"#),
            Some((12, 120))
        );
        assert_eq!(
            parse_frame_progress(r#"{"type":"progress","job":"cli","phase":"encode","frame":3,"total":120}"#),
            None
        );
        assert_eq!(parse_frame_progress("glyph-grid-studio[js]: batch: 1/1 cli → x"), None);
    }

    #[test]
    fn error_paths_point_at_the_argument() {
        assert_eq!(
            error_path("invalid config:\n  config/dither/mode: \"STBN\" is not one of [..]", false).as_deref(),
            Some("/config/dither/mode")
        );
        assert_eq!(
            error_path("batch: job 1 (\"typo\") config /selectionMode: \"shapes\" is not one of", true).as_deref(),
            Some("/jobs/1/config/selectionMode")
        );
        assert_eq!(
            error_path("batch: job 0 (\"?\") has no `out` path — refusing", true).as_deref(),
            Some("/jobs/0")
        );
        assert_eq!(
            error_path("render: cannot resolve --in \"x\": No such file", false).as_deref(),
            Some("/in_path")
        );
        assert_eq!(
            error_path("batch: manifest /jobs/0: Additional properties are not allowed ('capwidth' was unexpected)", true).as_deref(),
            Some("/jobs/0")
        );
        assert_eq!(error_path("glyph-grid-studio: render reported failure", false), None);
    }

    #[test]
    fn batch_progress_lines_are_parsed() {
        assert_eq!(
            parse_batch_progress("glyph-grid-studio[js]: batch: 3/8 ok → /tmp/a.gif"),
            Some((3, 8))
        );
        assert_eq!(parse_batch_progress("batch: 1/1 x → y"), Some((1, 1)));
        // Not progress: summary, queue and error lines.
        assert_eq!(parse_batch_progress("glyph-grid-studio[js]: batch complete: 3/3 ok"), None);
        assert_eq!(parse_batch_progress("batch: queued 3 jobs from \"m.json\""), None);
        assert_eq!(parse_batch_progress("batch: save failed a fmt=gif err=x"), None);
    }
}
