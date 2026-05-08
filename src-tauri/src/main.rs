// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Real-time character-grid image renderer.
///
/// Default behavior with no subcommand: launches the visual studio (GUI).
/// Subcommands provide headless operation for scripts and AI agents.
#[derive(Parser, Debug)]
#[command(name = "glyph-grid-studio", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Launch the visual studio (default if no subcommand given).
    Studio,

    /// Render a single image to an animated GIF, headlessly.
    Render(RenderArgs),

    /// Print the catalog of available palettes / ramps / dithers / glyph sets / postprocess stages.
    Catalog,

    /// Run the MCP server (Model Context Protocol over stdio JSON-RPC).
    /// Wire this into Claude Desktop / Cursor via their MCP config; tools then
    /// appear as `glyph_grid_render` and `glyph_grid_catalog`.
    Mcp,
}

#[derive(Parser, Debug, Clone)]
struct RenderArgs {
    /// Path to the source image (PNG / JPG / WebP / GIF / BMP / TIFF).
    #[arg(short = 'i', long = "in", value_name = "PATH")]
    in_path: PathBuf,

    /// Path to write the output GIF.
    #[arg(short = 'o', long = "out", value_name = "PATH")]
    out_path: PathBuf,

    /// Number of frames in the output animation. Default 24.
    #[arg(long, default_value_t = 24)]
    frames: u32,

    /// Palette name. See `glyph-grid-studio catalog` for the full list.
    #[arg(long)]
    palette: Option<String>,

    /// Color mode: preserve | monochrome | duotone | gradient.
    #[arg(long, value_name = "MODE")]
    color_mode: Option<String>,

    /// Glyph ramp.
    #[arg(long)]
    ramp: Option<String>,

    /// Dither mode.
    #[arg(long)]
    dither: Option<String>,

    /// Selection mode.
    #[arg(long)]
    selection_mode: Option<String>,

    /// Glyph set.
    #[arg(long)]
    glyph_set: Option<String>,

    /// Sampling strategy.
    #[arg(long)]
    sampling_strategy: Option<String>,

    /// Postprocess stage to enable. May be repeated.
    #[arg(long = "postprocess", value_name = "STAGE")]
    postprocess: Vec<String>,

    /// Grid columns.
    #[arg(long)]
    cols: Option<u32>,

    /// Grid rows.
    #[arg(long)]
    rows: Option<u32>,

    /// Path to a JSON preset file (overridable by explicit flags).
    #[arg(long, value_name = "PATH")]
    preset: Option<PathBuf>,

    /// Show the rendering window during the headless render (debug aid).
    #[arg(long)]
    show_window: bool,

    /// Output format: `gif` (default) or `mp4`.  When omitted, the format
    /// is inferred from the output path extension (`.mp4` → mp4).
    #[arg(long, value_name = "FORMAT")]
    format: Option<String>,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        None | Some(Command::Studio) => app_lib::run(),
        Some(Command::Render(args)) => {
            // Convert clap struct to a plain job struct that lib.rs can consume.
            let job = app_lib::HeadlessRenderJob {
                in_path: args.in_path,
                out_path: args.out_path,
                frames: args.frames,
                show_window: args.show_window,
                preset_path: args.preset,
                config_overrides: app_lib::ConfigOverrides {
                    palette: args.palette,
                    color_mode: args.color_mode,
                    ramp: args.ramp,
                    dither_mode: args.dither,
                    selection_mode: args.selection_mode,
                    glyph_set: args.glyph_set,
                    sampling_strategy: args.sampling_strategy,
                    postprocess: args.postprocess,
                    cols: args.cols,
                    rows: args.rows,
                },
                format: args.format,
            };
            let code = app_lib::run_headless_render(job);
            // Tauri's `app.exit(code)` triggers `tauri::run()` to return without
            // process-exiting (despite docs implying otherwise). Always exit
            // explicitly with the captured code so shell-level CI can detect
            // failures.
            std::process::exit(code);
        }
        Some(Command::Catalog) => {
            println!("{}", app_lib::catalog_json());
        }
        Some(Command::Mcp) => {
            // MCP server is async; spin up a tokio runtime ad-hoc for it.
            // GUI / Render modes don't need tokio so we don't pay for it there.
            let rt = tokio::runtime::Runtime::new().expect("failed to start tokio runtime");
            if let Err(e) = rt.block_on(app_lib::mcp::run()) {
                eprintln!("MCP server error: {}", e);
                std::process::exit(1);
            }
        }
    }
}
