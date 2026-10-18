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
    /// Exit codes: 0 ok, 1 render/encode failed, 2 bad input (usage, preset,
    /// config, source path), 3 timed out (`--timeout` or 300 s stalled),
    /// 130 / 143 interrupted by SIGINT / SIGTERM (`.part` files removed).
    Render(RenderArgs),

    /// Render many variants of the SAME source in ONE Tauri session.  Reads a
    /// manifest JSON with `{in, frames, jobs:[{name,out,format?,frames?,border?,config}, ...]}`
    /// and runs all jobs back-to-back without re-launching the app per variant.
    /// ~10× faster than calling `render` repeatedly because Tauri startup +
    /// source-image decode happens once.  See ITER-024 in BUGS_AND_ITERATIONS.md.
    Batch(BatchArgs),

    /// Print the catalog of available palettes / ramps / dithers / glyph sets / postprocess stages.
    Catalog,

    /// Validate a preset (partial CONFIG) or a batch manifest against the
    /// generated preset schema (src/glyph-grid.schema.json).  Errors carry
    /// the JSON path of the bad value.  Exit 0 = valid, 2 = invalid.
    Validate {
        /// Preset or manifest JSON file.
        #[arg(value_name = "PATH")]
        path: PathBuf,
    },

    /// Run the MCP server (Model Context Protocol over stdio JSON-RPC).
    /// Wire this into Claude Desktop / Cursor via their MCP config; tools then
    /// appear as `glyph_grid_render`, `glyph_grid_batch` and `glyph_grid_catalog`.
    Mcp,
}

#[derive(Parser, Debug, Clone)]
struct RenderArgs {
    /// Path to the source image (PNG / JPG / WebP / GIF / BMP / TIFF) or
    /// video (MP4 / MOV / M4V / WebM — R4).
    /// Optional with `--text` / `--srt` (the text is the source) or a preset
    /// that enables `textSource`.
    #[arg(short = 'i', long = "in", value_name = "PATH")]
    in_path: Option<PathBuf>,

    /// Path to write the output GIF.
    #[arg(short = 'o', long = "out", value_name = "PATH")]
    out_path: PathBuf,

    /// Number of frames in the output animation, or `auto` = the resolved
    /// `animation.duration` x effective fps (what the GUI exports). Default 24.
    #[arg(long, default_value = "24", value_name = "N|auto", value_parser = parse_frames)]
    frames: FramesArg,

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

    /// Set any CONFIG value: `--set grid.cols=200 --set palette=spice
    /// --set 'postprocess.bloom={"enabled":true,"radius":4}'`.  Value is
    /// JSON when it parses, else a string.  Repeatable; applied last and
    /// deep-merged.  Keys: see src/glyph-grid.schema.json.
    #[arg(long = "set", value_name = "PATH=VALUE")]
    set: Vec<String>,

    /// Keyframe a CONFIG path over the loop (Deforum-style):
    /// `--timeline 'studio.breathing.gainSwing=0:(0.1),0.5:(0.6)'`,
    /// `--timeline 'palette=0:(cream-paper),1:(spice)'` (OKLab blend).
    /// Keys are `<u>:(<value>)[:linear|inOutSine|step]`, u in [0, 1] of
    /// animation.duration.  Repeatable (one track each); turns
    /// `timeline.enabled` on.  JSON form: the `timeline` preset key.
    #[arg(long = "timeline", value_name = "PATH=KEYS")]
    timeline: Vec<String>,

    /// Show the rendering window during the headless render (debug aid).
    #[arg(long)]
    show_window: bool,

    /// Output format: `gif` (default), `mp4`, or the grid as text — `txt`,
    /// `ansi` (24-bit colour, `cat` it), `cast` (asciinema v2) or `html`
    /// (single file, font inlined).  txt/ansi are one still frame; cast/html
    /// animate `--frames`.  When omitted, inferred from the output path
    /// extension (.mp4 .txt .ans/.ansi .cast .html; anything else → gif).
    #[arg(long, value_name = "FORMAT", value_parser = app_lib::parse_format_arg)]
    format: Option<String>,

    /// Resolve preset + flags, validate, print the job JSON (merged config)
    /// to stdout and exit — no window, no render.
    #[arg(long)]
    dry_run: bool,

    /// Give up after this many seconds: the render is killed, its `.part`
    /// files removed, exit code 3.  Default: no overall limit (a render
    /// with no progress for 300 s still exits 3).
    #[arg(long, value_name = "SECONDS")]
    timeout: Option<u64>,

    /// Print NDJSON progress on stderr, one line per captured frame and
    /// encode step: `{"type":"progress","job":"cli","phase":"capture",
    /// "frame":12,"total":120}`.
    #[arg(long)]
    progress: bool,

    /// MP4 rate control: `upload` (default; ~5 Mbps bitrate target, a few MB
    /// per loop) or `archive` (fixed QP 26, near-lossless, ~30-60 Mbps).
    /// Ignored for GIF output.  Case-insensitive, same parser as the
    /// manifest `mp4Quality` and the MCP tool (ITER-070).
    #[arg(long, value_name = "QUALITY", value_parser = app_lib::parse_mp4_quality_arg)]
    mp4_quality: Option<String>,

    #[command(flatten)]
    text: TextArgs,

    /// Video sources (R4): auto (H.264 MP4/MOV decoded frame-exactly in
    /// Rust, other codecs via the WebView <video>), native (Rust only; a
    /// non-H.264 clip is an error) or webview.  JSON form: `video.decode`.
    #[arg(long, value_name = "MODE")]
    video_decode: Option<String>,
}

/// R3 (ITER-076): kinetic typography.  The text is rendered per frame as the
/// source raster; with `--in` too and `--text-blend` other than `replace` it
/// is composited onto the image.  JSON form: the `textSource` preset key.
#[derive(clap::Args, Debug, Clone)]
struct TextArgs {
    /// Render this text as the source (one line over the whole loop; `\n`
    /// stacks rows).  `--in` becomes optional.  A value may start with
    /// "-" (`--text -5`).
    #[arg(long, value_name = "TEXT", allow_hyphen_values = true)]
    text: Option<String>,

    /// Timed cues from a subtitle (.srt) or lyric (.lrc) file, appended to
    /// the lines; the last cue's end sets animation.duration.
    #[arg(long, value_name = "FILE")]
    srt: Option<PathBuf>,

    /// Text animation for every line: none | typewriter | rise | fade | scale.
    #[arg(long, value_name = "ANIM")]
    text_anim: Option<String>,

    /// Font family: `Cascadia Mono` / `IBM VGA8` (bundled) or a system font.
    #[arg(long, value_name = "FAMILY")]
    text_font: Option<String>,

    /// Font size in px at the text canvas height (textSource.h, 512).
    #[arg(long, value_name = "PX")]
    text_size: Option<f64>,

    /// How text meets an `--in` image: replace | multiply | screen | mask.
    #[arg(long, value_name = "MODE")]
    text_blend: Option<String>,
}

/// `--frames` value: `None` = auto.
#[derive(Debug, Clone)]
struct FramesArg(Option<u32>);

fn parse_frames(s: &str) -> Result<FramesArg, String> {
    if s.eq_ignore_ascii_case("auto") {
        return Ok(FramesArg(None));
    }
    match s.parse::<u32>() {
        Ok(n) if n >= 1 => Ok(FramesArg(Some(n))),
        _ => Err(format!("expected a positive integer or `auto`, got {:?}", s)),
    }
}

#[derive(Parser, Debug, Clone)]
struct BatchArgs {
    /// Path to a JSON manifest (schema: src/glyph-grid.manifest.schema.json;
    /// unknown keys are errors, optional `"version": 1`). Shape:
    /// `{"in":"<source>","frames":<N|"auto">,"jobs":[{"name":"...","out":"...","format":"gif|mp4|txt|ansi|cast|html",
    /// "frames":<N|"auto">,"border":{"width":32,"bgColor":"auto"},"mp4Quality":"upload|archive","config":{...}}, ...]}`
    #[arg(long, value_name = "PATH")]
    manifest: PathBuf,

    /// Show the rendering window during the headless batch (debug aid).
    #[arg(long)]
    show_window: bool,

    /// Give up on the whole batch after this many seconds (exit 3, `.part`
    /// files removed).  Default: no overall limit (300 s without progress
    /// still exits 3).
    #[arg(long, value_name = "SECONDS")]
    timeout: Option<u64>,

    /// Print NDJSON progress on stderr (see `render --progress`); `job` is
    /// the manifest job's `name`.
    #[arg(long)]
    progress: bool,

    /// Skip jobs whose `out` already exists and is a valid GIF/MP4 with the
    /// requested frame count (any count for `frames: "auto"`).  Re-run a
    /// timed-out or interrupted batch with the same manifest + `--resume`.
    #[arg(long)]
    resume: bool,

    /// Render the manifest across N child processes (each its own webview),
    /// jobs split round-robin. Capped at the job count and at CPU cores - 2.
    /// Default 1 renders every job in one session. One render already keeps
    /// ~3 cores busy, so past ~cores/4 the children contend (ITER-055).
    #[arg(long, value_name = "N", default_value_t = 1)]
    jobs: usize,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        None | Some(Command::Studio) => app_lib::run(),
        Some(Command::Render(args)) => {
            // Audit 2026-06-10: canonicalize --in at the CLI boundary.
            // A relative path used to flow into the webview's
            // read_image_file with a different effective cwd and HANG the
            // process instead of erroring (found during the CS-2 gate
            // bisection). Fail fast, with a message, before Tauri spins up.
            let in_path = match args.in_path.as_ref().map(|p| p.canonicalize()) {
                None => None,
                Some(Ok(p)) => Some(p),
                Some(Err(e)) => {
                    eprintln!(
                        "render: cannot resolve --in {:?}: {} (file must exist; relative paths resolve against the current directory)",
                        args.in_path.as_ref().unwrap(), e
                    );
                    std::process::exit(2);
                }
            };
            // Convert clap struct to a plain job struct that lib.rs can consume.
            let job = app_lib::HeadlessRenderJob {
                in_path,
                out_path: args.out_path,
                frames: args.frames.0,
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
                    set: args.set,
                    timeline: args.timeline,
                    text: args.text.text,
                    text_anim: args.text.text_anim,
                    text_font: args.text.text_font,
                    text_size: args.text.text_size,
                    text_blend: args.text.text_blend,
                    video_decode: args.video_decode,
                },
                format: args.format,
                timeout_secs: args.timeout,
                mp4_quality: args.mp4_quality,
                text_import: args.text.srt,
            };
            if args.dry_run {
                match app_lib::headless_render_json(&job) {
                    Ok(v) => {
                        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
                        std::process::exit(0);
                    }
                    Err(e) => {
                        eprintln!("render: {}", e);
                        std::process::exit(2);
                    }
                }
            }
            if args.progress {
                app_lib::enable_progress_ndjson();
            }
            let code = app_lib::run_headless_render(job);
            // Tauri's `app.exit(code)` triggers `tauri::run()` to return without
            // process-exiting (despite docs implying otherwise). Always exit
            // explicitly with the captured code so shell-level CI can detect
            // failures.
            std::process::exit(code);
        }
        Some(Command::Batch(args)) => {
            if args.progress {
                app_lib::enable_progress_ndjson();
            }
            let code = app_lib::run_headless_batch(args.manifest, args.show_window, args.timeout, args.resume, args.jobs);
            std::process::exit(code);
        }
        Some(Command::Catalog) => {
            println!("{}", app_lib::catalog_json());
        }
        Some(Command::Validate { path }) => {
            std::process::exit(app_lib::run_validate(path));
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
