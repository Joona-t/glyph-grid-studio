# Agent Integration Plan — Glyph Grid Studio

Roadmap for exposing the renderer as a programmatic surface alongside the GUI. After this work lands, Glyph Grid Studio works in three modes — visual studio (current), headless CLI, MCP-tool — from one ~10 MB binary.

> **Why this matters:** the rendering pipeline is genuinely useful as a *function* (image + config → GIF). Locking it inside a GUI limits it to one operator at a time. Exposing it as a CLI / MCP tool means Claude / Codex / Cursor / any LLM with shell access can compose it into larger workflows: batch experimentation, agentic preset exploration, photo-to-art pipelines. Same pipeline, more surfaces.

---

## Three tiers, recommended build order

| Tier | Surface | Build effort | Distribution effort | Unlocks |
|---|---|---|---|---|
| **1** | CLI subcommand | 1–2 days | none (same binary) | Any agent that can call shell commands |
| **2** | Local HTTP server | 2–3 days on top of T1 | none (same binary) | Web apps, parallel agents, microservice composition |
| **3** | MCP server (stdio) | 2–3 days on top of T1 | one config snippet | Native Claude Desktop / Cursor tool integration |

Each tier shares the **same rendering core**. T1 → T2 → T3 is additive — no rewrites between tiers, just thinner wrappers. Build T1 first; T2 and T3 are then ~1 week each on top.

---

## Tier 1 — CLI subcommand (the foundation)

### What it looks like

```bash
# Single render with explicit flags
glyph-grid-studio render \
  --in ~/Downloads/thor.png \
  --out ~/thor-amber.gif \
  --palette amber-phosphor \
  --color-mode monochrome \
  --ramp unicode-block \
  --dither bayer8 \
  --postprocess crtBeam \
  --frames 24

# Render with a JSON preset (more flexible, matches the Studio's preset format)
glyph-grid-studio render \
  --in ~/Downloads/thor.png \
  --out ~/thor-warm-bone.gif \
  --preset ~/.glyph-grid/presets/warm-bone-stipple.json

# Batch from a manifest
glyph-grid-studio batch --manifest ~/jobs.json
# where jobs.json is the same shape as PHASES[*].jobs in index.html
```

### Architecture

The hard part: the rendering pipeline currently runs inside a Tauri WKWebView. Three options for going headless, in order of simplicity:

1. **Hidden Tauri window** *(recommended)*: launch the existing Tauri runtime with `visible: false`. JS-side detects "CLI mode" via a Tauri command call on startup, auto-loads the image, applies the config, records via the existing batch driver, calls `app.exit(0)` when done. **Minimal new code.** Same `save_gif_to_path` Rust command we already have.
2. Embed a headless browser (servo, headless_chrome): adds ~50 MB to the binary, complicates builds. Don't.
3. Port the entire pipeline to pure Rust: months of work, kills creative-coding ergonomics. Don't.

### Code sketch — `src-tauri/src/main.rs`

```rust
use clap::Parser;

#[derive(Parser)]
#[command(version, about)]
enum Cli {
    /// Launch the GUI studio (default if no subcommand)
    Studio,
    /// Render a single image headlessly
    Render(RenderArgs),
    /// Render a batch from a JSON manifest
    Batch(BatchArgs),
    /// Start the local HTTP server (Tier 2)
    Serve(ServeArgs),
    /// Start the MCP server on stdio (Tier 3)
    Mcp,
}

#[derive(Parser)]
struct RenderArgs {
    #[arg(long)] in_path: PathBuf,
    #[arg(long)] out: PathBuf,
    #[arg(long)] palette: Option<String>,
    #[arg(long, value_name = "MODE")] color_mode: Option<String>,
    #[arg(long)] ramp: Option<String>,
    #[arg(long)] dither: Option<String>,
    #[arg(long)] postprocess: Vec<String>,
    #[arg(long, default_value_t = 24)] frames: u32,
    #[arg(long)] preset: Option<PathBuf>,
    #[arg(long)] glyph_set: Option<String>,
    #[arg(long)] selection_mode: Option<String>,
    // ...
}

fn main() {
    let cli = Cli::parse();
    match cli {
        Cli::Studio => app_lib::run_gui(),
        Cli::Render(args) => app_lib::run_headless_render(args),
        Cli::Batch(args) => app_lib::run_headless_batch(args),
        Cli::Serve(args) => app_lib::run_http_server(args),
        Cli::Mcp => app_lib::run_mcp_server(),
    }
}
```

### Code sketch — JS-side CLI hook (in `index.html`)

```js
// After p5 setup() finishes:
if (window.__TAURI__ && window.__TAURI__.core) {
  window.__TAURI__.core.invoke('get_cli_render_job').then(job => {
    if (!job) return;  // GUI mode, no auto-render
    // Auto-load image, apply config, record, save, exit
    window.loadImage(job.imageDataUrl, img => {
      eronImg = img;
      __glyphGridTest.setConfig(job.config);
      setTimeout(() => {
        __glyphGridTest.runBatchExport([{
          name: 'cli',
          path: job.outPath,
          frames: job.frames,
          config: {},  // already applied above
        }], {
          onComplete: r => window.__TAURI__.core.invoke('exit_with_status', { ok: r.errors.length === 0 }),
        });
      }, 200);
    });
  });
}
```

### Rust-side helpers

```rust
#[tauri::command]
fn get_cli_render_job(state: State<CliState>) -> Option<CliRenderJob> {
    state.render_job.lock().unwrap().clone()
}

#[tauri::command]
async fn exit_with_status(app: AppHandle, ok: bool) {
    app.exit(if ok { 0 } else { 1 });
}
```

### Effort estimate

- 4–6 hours: clap parsing + main dispatch + render-args struct
- 4–6 hours: hidden window mode + CLI state injection
- 2–4 hours: JS-side auto-render hook
- 2 hours: testing on 5 fixture images
- **Total: 1–2 days**

### What you can build on top of T1, immediately

- Cron-triggered "ASCII art of the day" — render a wallpaper at midnight
- Make a `glyph-grid` shell function that pipes any clipboard image through the renderer
- A drag-drop droplet (macOS Automator → shell action → `glyph-grid-studio render`)
- `find ~/Pictures -name '*.jpg' | xargs -I {} glyph-grid-studio render --in {} --out {}.gif`

---

## Tier 2 — Local HTTP server

### What it looks like

```bash
# Start the server
glyph-grid-studio serve --port 8765 &

# Render via curl
curl -X POST localhost:8765/render \
  -H 'Content-Type: application/json' \
  -d '{"image_path":"/Users/darkfire/Downloads/thor.png","config":{"palette":"spice","dither":{"mode":"stbn"}},"out_path":"/tmp/out.gif"}'

# Catalog the available options
curl localhost:8765/catalog | jq '.palettes'
```

### Endpoints

| Method | Path | Body | Returns |
|---|---|---|---|
| `POST` | `/render` | `{ image_path \| image_b64, config, out_path?, frames? }` | `{ path, size, ms }` or `{ gif_b64, size, ms }` |
| `POST` | `/snapshot` | same as `/render` but single frame | `{ path, size }` or `{ png_b64 }` |
| `GET`  | `/catalog` | — | `{ palettes:[], ramps:[], dithers:[], glyph_sets:[], postprocess:[], color_modes:[] }` |
| `GET`  | `/presets` | — | `[{ name, config }]` (bundled + user presets) |
| `POST` | `/presets` | `{ name, config }` | `{ saved: true }` (saves to user preset dir) |
| `GET`  | `/healthz` | — | `{ status: "ok", version, render_queue_depth }` |

### Architecture

A thin `axum` server (or `actix-web`) wraps the same headless render function from T1. Each request boots a hidden Tauri window, runs the render, returns the result. Concurrency: limit to N parallel renders (Tauri webview spawn is the bottleneck) — use a `tokio::sync::Semaphore`.

### Security notes

- Bind to `127.0.0.1` only by default. CORS off by default (no browser access). Add `--cors-origin` flag for explicit opt-in.
- Path-based renders: validate that `image_path` and `out_path` resolve under user-provided allow-listed roots (default: `$HOME/Pictures`, `$HOME/Downloads`, `/tmp`). Reject `..` traversals.
- No auth in v1. Local-only by default, the user controls the port.

### Effort estimate

- 4 hours: axum scaffolding + endpoint stubs
- 6 hours: render endpoint wired to T1 headless function + concurrency limiter
- 4 hours: catalog + presets endpoints
- 4 hours: path validation + tests
- **Total: 2–3 days on top of T1**

---

## Tier 3 — MCP server (the AI-native path)

### What it looks like

User adds to their `~/.claude/claude_desktop_config.json` (or Cursor's MCP config):

```json
{
  "mcpServers": {
    "glyph-grid-studio": {
      "command": "/Applications/Glyph Grid Studio.app/Contents/MacOS/glyph-grid-studio",
      "args": ["mcp"]
    }
  }
}
```

Restart Claude Desktop. Now in any chat:

> *"Render the image at ~/Pictures/sunset.jpg as a cream-paper monochrome glyph drawing, save it next to the original."*

Claude calls the MCP tool, the studio renders, the file lands. Or:

> *"Try this image with 5 different palettes and tell me which one looks most like a charcoal sketch."*

Claude renders 5 variants in parallel via 5 MCP calls, reads each back as multimodal input, picks the winner. **The studio becomes an instrument the AI plays.**

### MCP tool surface

| Tool | Inputs | Outputs |
|---|---|---|
| `glyph_grid_render` | `image_path`, `config: {...}`, `out_path?`, `frames?` | `{ path, size_kb, render_ms }` |
| `glyph_grid_snapshot` | `image_path`, `config: {...}`, `out_path?` | `{ path, size_kb }` |
| `glyph_grid_catalog` | — | `{ palettes, ramps, dithers, glyph_sets, postprocess, color_modes }` |
| `glyph_grid_presets` | — | `[{ name, config }]` |
| `glyph_grid_save_preset` | `name`, `config` | `{ saved: true }` |

Each tool's `inputSchema` is JSON Schema with enums for the categorical params (palettes, ramps, etc.) — so the LLM gets autocomplete-quality suggestions and can't pass invalid values.

### Architecture

The MCP transport for local desktop integration is **stdio JSON-RPC**. The binary speaks JSON-RPC 2.0 on stdin/stdout. The Rust ecosystem has a [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk) SDK that handles the protocol — you implement the tool functions, it handles framing.

The tool functions are thin wrappers over the same headless render core from T1. Each tool call → render → response.

### Code sketch

```rust
use rmcp::{ServerHandler, model::*};

struct GlyphGridServer;

impl ServerHandler for GlyphGridServer {
    async fn list_tools(&self) -> ListToolsResult {
        ListToolsResult {
            tools: vec![
                Tool::new("glyph_grid_render", "Render image to animated GIF", render_schema()),
                Tool::new("glyph_grid_catalog", "List all palettes/ramps/dithers/...", empty_schema()),
                // ...
            ],
        }
    }

    async fn call_tool(&self, req: CallToolRequest) -> CallToolResult {
        match req.name.as_str() {
            "glyph_grid_render" => self.render(req.arguments).await,
            "glyph_grid_catalog" => self.catalog().await,
            _ => CallToolResult::error("unknown tool"),
        }
    }
}

async fn run_mcp_server() {
    rmcp::serve_stdio(GlyphGridServer).await.unwrap();
}
```

### Effort estimate

- 4 hours: rmcp scaffolding, stdio loop
- 6 hours: tool schemas + dispatchers
- 4 hours: tool functions wired to T1 headless render
- 4 hours: testing with real Claude Desktop session
- **Total: 2–3 days on top of T1**

---

## Distribution implications

### Single-binary strategy *(recommended)*

One signed `.app` bundle. Default behavior = launch GUI. Subcommands trigger headless modes:

```
$ /Applications/Glyph\ Grid\ Studio.app/Contents/MacOS/glyph-grid-studio
  → launches GUI (current behavior)

$ /Applications/Glyph\ Grid\ Studio.app/Contents/MacOS/glyph-grid-studio render --help
  → prints CLI render help

$ /Applications/Glyph\ Grid\ Studio.app/Contents/MacOS/glyph-grid-studio mcp
  → starts MCP server on stdio
```

### Add a Homebrew tap shim

```ruby
# Formula/glyph-grid-studio.rb
class GlyphGridStudio < Formula
  desc "Real-time character-grid image renderer (CLI + MCP + GUI)"
  homepage "https://github.com/Joona-t/glyph-grid-studio"
  url "https://github.com/Joona-t/glyph-grid-studio/releases/download/v0.2.0/glyph-grid-studio.dmg"
  sha256 "..."
  version "0.2.0"

  def install
    # Symlink the headless binary into /usr/local/bin so `glyph-grid-studio` works in shells
    bin.install_symlink "/Applications/Glyph Grid Studio.app/Contents/MacOS/glyph-grid-studio"
  end
end
```

After `brew install --cask glyph-grid-studio`, users can run `glyph-grid-studio render ...` from any shell. **This is the single most important UX detail for agent adoption** — if Claude has to look up the full bundle path, the friction is too high. A `glyph-grid-studio` on `$PATH` is what unlocks one-line agent recipes.

---

## Build order (concrete)

### v0.1 (current state, awaiting public release)

GUI only. Tauri 2 + p5 + Rust GIF muxer. **Don't ship CLI yet** — adding it now ahead of the public release would risk delaying the launch on infrastructure work users don't see.

### v0.2 — "CLI mode" release

- Tier 1 CLI shipped
- Updated README "Two ways to use it" section
- Homebrew cask formula
- Blog post / X thread: "Glyph Grid Studio now has a CLI — here's why agents will love it"

### v0.3 — "MCP server" release

- Tier 3 MCP server shipped
- Documentation: how to add to Claude Desktop / Cursor
- 1-minute video showing Claude rendering 5 variants and picking the best
- Blog post / X thread: "I taught Claude to make ASCII art"

### v0.4 — "HTTP server" release (optional)

- Tier 2 only if user demand emerges. The HTTP server is mostly redundant if MCP works — it serves the same use cases for non-MCP-aware clients.

---

## Documentation needs (per tier)

### CLI

- `glyph-grid-studio render --help` output that's actually useful
- A `docs/cli.md` page in the repo with every flag, plus 5 worked examples
- README "Quick start" section: install → `glyph-grid-studio render --in foo.jpg --out bar.gif`

### MCP

- `docs/mcp.md` with the full tool reference (each tool's input/output schema, behavioral notes, examples)
- Setup snippet for Claude Desktop config
- Setup snippet for Cursor config
- A "recipes" section: 5 prompts that demonstrate the studio used as a tool

---

## Testing strategy

### Per-tier integration tests

- T1: `cargo test --test cli_render` — fixture image + config → assert output GIF is valid 89a
- T2: `cargo test --test http_server` — boot server, curl request, assert response
- T3: `cargo test --test mcp_protocol` — JSON-RPC fixture conversation, assert tool dispatch + response

### End-to-end smoke

A `scripts/smoke.sh` that exercises every surface against a fixture image and diffs against committed reference outputs. Run on every CI build. Frame fingerprints rather than byte-identical (NeuQuant has nondeterminism in palette ordering).

### Agent dogfooding

Ask Claude (via MCP) to:

1. *"Render this image as 5 different palettes. Pick the one that looks most like a charcoal drawing."*
2. *"This image is too dark. Tweak the gamma to make it more legible."*
3. *"Make me a series of 12 GIFs, varying just the dither mode, kept identical otherwise."*

If it can do these without prompting tricks, the integration is good.

---

## Open questions

1. **License clarification on bundled fonts:** The Cascadia subset and IBM VGA fallback are bundled in `src/fonts/`. CLI/MCP modes will distribute the same fonts. Verify the Cascadia license permits redistribution in a CLI tool. If not, fallback to user-system fonts in headless modes.
2. **Concurrency in the HTTP/MCP servers:** Each render spawns a hidden Tauri window. How many concurrent windows is safe? Probably 4–8 on an M-series Mac. Need a tokio Semaphore + queue.
3. **CLI args vs. preset JSON precedence:** If the user passes both `--preset` and `--palette`, which wins? My recommendation: CLI flags override preset values (Unix convention).
4. **MCP tool naming:** `glyph_grid_render` vs. `render_glyph_grid` vs. just `render`. Convention in the MCP ecosystem leans toward namespaced (`glyph_grid_render`) to avoid collisions when multiple servers are mounted.
5. **Headless mode and assertion-based logging:** the GUI logs to the dev console. Headless modes need a structured log to stderr. `tracing` crate.

---

## Out of scope for this plan

- Linux + Windows headless support (T1 architecture is portable; just needs build/CI work)
- iOS / iPad version (Tauri Mobile is alpha; not yet practical)
- Web-based version (would require dropping the Rust backend; whole different design)
- Auth / multi-tenant HTTP mode (it's a personal local tool, not a SaaS)
- Mac App Store submission (sandbox restrictions break the file-pick UX, and CLI binaries are a poor fit for App Store distribution)

---

## Success metrics (90-day post-CLI release)

- Has anyone written a recipe / blog post using `glyph-grid-studio render` in a pipeline?
- Has anyone added the MCP server to their Claude Desktop config and posted about it?
- Are there GitHub Issues asking for new CLI flags? (good signal — means people are using it)
- Is the binary download count via Releases > 100? (rough threshold for "people are trying it")

If 2 of 4 are true at 90 days, the agent integration was worth doing.
