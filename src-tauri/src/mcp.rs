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
    model::ProgressNotificationParam,
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
    /// Absolute path to the source image (PNG / JPG / WebP / GIF / BMP / TIFF).
    pub in_path: String,
    /// Absolute path to write the output GIF.
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
    /// Output format: `gif` (default) or `mp4`.  When omitted, inferred
    /// from `out_path`'s extension.
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
    /// border?: {width, bgColor?, style?, inkColor?}, config?: {partial CONFIG}}`
    /// — the same shape as a `batch --manifest` file. `out` must be an
    /// absolute path.
    pub jobs: Vec<serde_json::Value>,
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
async fn run_child(mut cmd: Command, ctx: &RequestContext<RoleServer>) -> Result<ChildEnd, String> {
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
                let _ = child.kill().await;
                return Ok(ChildEnd::Cancelled);
            }
            line = lines.next_line() => match line {
                Ok(Some(line)) => {
                    if let (Some(token), Some((n, m))) = (&token, parse_batch_progress(&line)) {
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
            let _ = child.kill().await;
            return Ok(ChildEnd::Cancelled);
        }
        status = child.wait() => status.map_err(|e| format!("subprocess wait failed: {}", e))?,
    };
    Ok(ChildEnd::Exited(status, stderr))
}

/* ---------- Server handler ---------- */

#[derive(Debug, Clone)]
pub struct GlyphGridServer;

#[tool_router(server_handler)]
impl GlyphGridServer {
    #[tool(
        description = "Render an image to an animated character-grid GIF (ASCII / Unicode glyphs). \
                       Returns the absolute path of the written file (GIF or MP4, per `format`) on success."
    )]
    async fn glyph_grid_render(
        &self,
        Parameters(p): Parameters<RenderParams>,
        ctx: RequestContext<RoleServer>,
    ) -> String {
        // F24 (ITER-057): a `config` object travels as a temp preset file —
        // the CLI already layers flags onto `--preset`, so no new CLI
        // surface is needed.  Removed when this call returns.
        // F22 (ITER-058): a `preset` path alone is forwarded as-is; with a
        // `config` too, the config deep-merges onto the file's contents
        // (the CLI takes one --preset).
        if let Some(other) = p.config.as_ref().filter(|c| !c.is_object()) {
            return format!(
                "Render failed: exit 2 | invalid arguments: `config` must be a JSON object, got {}",
                other
            );
        }
        let mut temp = None;
        let preset_path: Option<std::path::PathBuf> = match (&p.preset, &p.config) {
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
                            return format!(
                                "Render failed: exit 2 | invalid arguments: preset {} must be a JSON object, got {}",
                                path, other
                            )
                        }
                        Err(e) => {
                            return format!(
                                "Render failed: exit 2 | invalid arguments: cannot read preset {}: {}",
                                path, e
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
                    Err(e) => return e,
                }
            }
        };

        // R7 (ITER-056): validate the arguments against the preset schema
        // in-process, before paying ~5 s of Tauri start-up for a render the
        // CLI would reject anyway.  Same resolver as `render`.
        let probe = crate::HeadlessRenderJob {
            in_path: std::path::PathBuf::from(&p.in_path),
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
            },
            format: p.format.clone(),
        };
        if let Err(e) = probe.build_js_config() {
            return format!("Render failed: exit 2 | invalid arguments: {}", e);
        }

        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => return format!("error: cannot locate own binary: {}", e),
        };

        let mut cmd = Command::new(&exe);
        cmd.arg("render")
            .arg("--in")
            .arg(&p.in_path)
            .arg("--out")
            .arg(&p.out_path);
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

        let end = run_child(cmd, &ctx).await;
        drop(temp);
        match end {
            Ok(ChildEnd::Exited(status, _)) if status.success() => {
                // Verify the output file exists and report size.
                match std::fs::metadata(&p.out_path) {
                    Ok(meta) => format!(
                        "Rendered {} ({} KB)",
                        p.out_path,
                        meta.len() / 1024
                    ),
                    Err(_) => format!(
                        "Rendered (no output file at {} — check stderr)",
                        p.out_path
                    ),
                }
            }
            Ok(ChildEnd::Exited(status, stderr)) => format!(
                "Render failed: exit {} | stderr: {}",
                status.code().unwrap_or(-1),
                stderr
            ),
            Ok(ChildEnd::Cancelled) => "Render cancelled (subprocess killed)".to_string(),
            Err(e) => format!("Render {}", e),
        }
    }

    #[tool(
        description = "Render several jobs (same or different sources, any CONFIG each) in ONE app \
                       session — the start-up cost is paid once, not per job. Arguments are a \
                       `batch --manifest` object: {in, frames?, jobs:[{name, out, format?, in?, \
                       frames?, capWidth?, border?, config?}]}. Sends notifications/progress per \
                       finished job when the request carries a progressToken. Returns one line per job."
    )]
    async fn glyph_grid_batch(
        &self,
        Parameters(p): Parameters<BatchParams>,
        ctx: RequestContext<RoleServer>,
    ) -> String {
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
            Err(e) => return e,
        };
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => return format!("error: cannot locate own binary: {}", e),
        };
        let mut cmd = Command::new(&exe);
        cmd.arg("batch").arg("--manifest").arg(&temp.0);

        let end = run_child(cmd, &ctx).await;
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
                    format!("Batch complete: {} jobs\n{}", p.jobs.len(), report.join("\n"))
                } else {
                    format!(
                        "Batch failed: exit {}\n{}\n| stderr: {}",
                        status.code().unwrap_or(-1),
                        report.join("\n"),
                        stderr
                    )
                }
            }
            Ok(ChildEnd::Cancelled) => "Batch cancelled (subprocess killed)".to_string(),
            Err(e) => format!("Batch {}", e),
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
    use super::parse_batch_progress;

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
