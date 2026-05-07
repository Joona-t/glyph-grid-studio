//! MCP server — exposes Glyph Grid Studio's rendering pipeline as
//! Model Context Protocol tools over stdio JSON-RPC.
//!
//! Architecture: each tool call spawns a subprocess that re-invokes
//! this same binary in `render` (or `catalog`) mode. This avoids the
//! "only one Tauri runtime per process" constraint and keeps each
//! render isolated. Cost: ~5s of process spawn + Tauri startup per
//! call. Worth it for reliability.
//!
//! Wired tools:
//!   - glyph_grid_render   — render an image to an animated GIF
//!   - glyph_grid_catalog  — list every palette / ramp / dither / glyph set / postprocess stage
//!
//! Future tools (not yet wired): glyph_grid_snapshot (single PNG),
//! glyph_grid_presets (list bundled), glyph_grid_save_preset.

use rmcp::{handler::server::wrapper::Parameters, schemars, tool, tool_router};
use std::process::Command;

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
}

/* ---------- Server handler ---------- */

#[derive(Debug, Clone)]
pub struct GlyphGridServer;

#[tool_router(server_handler)]
impl GlyphGridServer {
    #[tool(
        description = "Render an image to an animated character-grid GIF (ASCII / Unicode glyphs). \
                       Returns the absolute path of the written GIF on success."
    )]
    fn glyph_grid_render(
        &self,
        Parameters(p): Parameters<RenderParams>,
    ) -> String {
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

        match cmd.output() {
            Ok(output) if output.status.success() => {
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
            Ok(output) => format!(
                "Render failed: exit {} | stderr: {}",
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(e) => format!("Render subprocess failed to start: {}", e),
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
