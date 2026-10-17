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
    /// `--set <dotted.path>=<json>` overrides, applied last (ITER-057).
    #[serde(default)]
    pub set: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct HeadlessRenderJob {
    pub in_path: PathBuf,
    pub out_path: PathBuf,
    /// `None` = `--frames auto`: the JS side derives the count from the
    /// resolved `animation.duration` x effective fps, like the GUI export.
    pub frames: Option<u32>,
    pub show_window: bool,
    pub preset_path: Option<PathBuf>,
    pub config_overrides: ConfigOverrides,
    /// Output format. `"gif"` (default) → gifski; `"mp4"` → openh264 + mp4.
    /// When None, derived from `out_path`'s extension.
    pub format: Option<String>,
    /// F25 (ITER-059): overall wall-clock budget in seconds (`--timeout`);
    /// past it the process exits 3.  None = no budget (the stall guard
    /// still applies).
    pub timeout_secs: Option<u64>,
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
    ///
    /// R7 (ITER-056): an unreadable / unparsable / non-object `--preset` is
    /// an error (it used to fall through to an empty config → a render with
    /// defaults and exit 0), and the resolved config is validated against
    /// the embedded preset schema.  Returns the config plus warnings
    /// (unknown top-level keys — passed through, per the schema's
    /// `x-unknown-key-policy: warn`).
    pub fn build_js_config(&self) -> Result<(serde_json::Value, Vec<String>), String> {
        let mut config = serde_json::Value::Object(serde_json::Map::new());

        // 1. Lay the preset (lowest priority)
        if let Some(preset_path) = &self.preset_path {
            let json_str = fs::read_to_string(preset_path)
                .map_err(|e| format!("cannot read --preset {}: {}", preset_path.display(), e))?;
            match serde_json::from_str::<serde_json::Value>(&json_str) {
                Ok(obj @ serde_json::Value::Object(_)) => config = obj,
                Ok(other) => {
                    return Err(format!(
                        "--preset {} must be a JSON object (a partial CONFIG), got {}",
                        preset_path.display(),
                        json_kind(&other)
                    ))
                }
                Err(e) => {
                    return Err(format!(
                        "--preset {} is not valid JSON: {}",
                        preset_path.display(),
                        e
                    ))
                }
            }
        }

        // 2. Layer flag overrides on top.  F24 (ITER-057): the flags form a
        // patch that is DEEP-merged onto the preset — they used to replace
        // whole nested blocks (`--cols` dropped the preset's rows,
        // `--postprocess vignette` dropped `vignette.strength`).
        let o = &self.config_overrides;
        let mut config_patch = serde_json::Map::new();
        if let Some(v) = &o.palette {
            config_patch.insert("palette".into(), serde_json::Value::String(v.clone()));
        }
        if let Some(v) = &o.color_mode {
            config_patch.insert("colorMode".into(), serde_json::Value::String(v.clone()));
        }
        if let Some(v) = &o.ramp {
            config_patch.insert("ramp".into(), serde_json::Value::String(v.clone()));
        }
        if let Some(v) = &o.dither_mode {
            config_patch.insert(
                "dither".into(),
                // Mode only: the preset's asSourcePrefilter survives
                // (CONFIG.dither.levels is never read; levels follow the ramp).
                serde_json::json!({ "mode": v }),
            );
        }
        if let Some(v) = &o.selection_mode {
            config_patch.insert("selectionMode".into(), serde_json::Value::String(v.clone()));
        }
        if let Some(v) = &o.glyph_set {
            let val = if v == "null" || v == "none" {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(v.clone())
            };
            config_patch.insert("glyphSet".into(), val);
        }
        if let Some(v) = &o.sampling_strategy {
            config_patch.insert(
                "samplingStrategy".into(),
                serde_json::Value::String(v.clone()),
            );
        }
        if !o.postprocess.is_empty() {
            // Enable exactly the listed stages, disable every other one
            // explicitly so we don't inherit any scene defaults (the "listed
            // stages only" contract).  Only `enabled` is written, so nested
            // params (vignette.strength, bloom.radius) survive the merge.
            // The stage list is the schema's (ITER-056).
            let enabled: std::collections::HashSet<&str> =
                o.postprocess.iter().map(|s| s.as_str()).collect();
            let mut pp = serde_json::Map::new();
            for stage in schema_postprocess_stages() {
                let on = enabled.contains(stage.as_str());
                pp.insert(stage, serde_json::json!({ "enabled": on }));
            }
            for stage in &o.postprocess {
                if !pp.contains_key(stage) {
                    return Err(format!(
                        "--postprocess {:?} is not a postprocess stage (allowed: {})",
                        stage,
                        schema_postprocess_stages().join(", ")
                    ));
                }
            }
            config_patch.insert("postprocess".into(), serde_json::Value::Object(pp));
        }
        if o.cols.is_some() || o.rows.is_some() {
            let mut grid = serde_json::Map::new();
            if let Some(c) = o.cols {
                grid.insert("cols".into(), serde_json::Value::from(c));
            }
            if let Some(r) = o.rows {
                grid.insert("rows".into(), serde_json::Value::from(r));
            }
            config_patch.insert("grid".into(), serde_json::Value::Object(grid));
        }

        json_deep_merge(&mut config, serde_json::Value::Object(config_patch));

        // 3. `--set a.b.c=<json>` last, through the same merge.
        for spec in &o.set {
            let patch = parse_set_override(spec)?;
            json_deep_merge(&mut config, patch);
        }

        let warnings = validate_config(&config)
            .map_err(|errs| format!("invalid config:\n  config{}", errs.join("\n  config")))?;
        strip_preset_meta(&mut config);
        Ok((config, warnings))
    }
}

fn json_kind(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

/// F24 (ITER-057): recursive JSON merge.  Objects merge key by key; every
/// other value (scalar, array, null) replaces.  Same rule as
/// `GlyphStudio.deepMerge` on the JS side, so preset + flags resolve the way
/// the GUI applies a preset.
pub fn json_deep_merge(target: &mut serde_json::Value, patch: serde_json::Value) {
    match (target, patch) {
        (serde_json::Value::Object(t), serde_json::Value::Object(p)) => {
            for (k, v) in p {
                match t.get_mut(&k) {
                    Some(slot) if slot.is_object() && v.is_object() => json_deep_merge(slot, v),
                    _ => {
                        t.insert(k, v);
                    }
                }
            }
        }
        (t, p) => *t = p,
    }
}

/// `--set grid.cols=200` → `{"grid":{"cols":200}}`.  The value is JSON when
/// it parses (`200`, `true`, `null`, `{"x":1}`, `"quoted"`), otherwise the
/// raw text as a string (`--set palette=spice`).
pub fn parse_set_override(spec: &str) -> Result<serde_json::Value, String> {
    let (path, raw) = spec
        .split_once('=')
        .ok_or_else(|| format!("--set {:?}: expected <dotted.path>=<value>", spec))?;
    let keys: Vec<&str> = path.trim().split('.').collect();
    if keys.iter().any(|k| k.is_empty()) {
        return Err(format!("--set {:?}: empty key in path {:?}", spec, path));
    }
    let mut value = serde_json::from_str::<serde_json::Value>(raw)
        .unwrap_or_else(|_| serde_json::Value::String(raw.to_string()));
    for k in keys.iter().rev() {
        let mut m = serde_json::Map::new();
        m.insert((*k).to_string(), value);
        value = serde_json::Value::Object(m);
    }
    Ok(value)
}

/// Manifest `frames`: a positive integer or `"auto"`.  `None` = absent.
fn manifest_frames(v: Option<&serde_json::Value>, what: &str) -> Result<Option<serde_json::Value>, String> {
    match v {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) if s == "auto" => Ok(Some(serde_json::Value::from("auto"))),
        Some(n) if n.as_u64().map(|f| f >= 1).unwrap_or(false) => Ok(Some(n.clone())),
        Some(other) => Err(format!(
            "{} `frames` must be a positive integer or \"auto\", got {}",
            what, other
        )),
    }
}

/// Manifest `border`: `{width, bgColor?, style?, inkColor?, enabled?}` →
/// the `BorderConfig` shape the encoders take (`enabled` defaults to true
/// when a border object is given; `bgColor` "auto"/absent is resolved to
/// the job's palette bg by the JS driver).
fn manifest_border(v: Option<&serde_json::Value>, what: &str) -> Result<serde_json::Value, String> {
    match v {
        None | Some(serde_json::Value::Null) => Ok(serde_json::Value::Null),
        Some(serde_json::Value::Object(b)) => {
            let mut b = b.clone();
            match b.get("width").and_then(|w| w.as_u64()) {
                Some(w) if w <= 1024 => {}
                _ => {
                    return Err(format!(
                        "{} `border.width` must be an integer 0..=1024 (px), got {}",
                        what,
                        b.get("width").map(|w| w.to_string()).unwrap_or_else(|| "nothing".into())
                    ))
                }
            }
            b.entry("enabled").or_insert(serde_json::Value::Bool(true));
            b.entry("style").or_insert(serde_json::Value::from("solid"));
            Ok(serde_json::Value::Object(b))
        }
        Some(other) => Err(format!("{} `border` must be an object, got {}", what, other)),
    }
}

/* ============================================================================
 *  R7 (ITER-056) — preset schema as the single source of truth.
 *
 *  src/glyph-grid.schema.json is GENERATED by scripts/gen-schema.js from the
 *  live CONFIG defaults and option tables (run-all A9 fails when it is
 *  stale).  It is embedded here so `--preset`, batch job configs, MCP args,
 *  `validate` and `catalog` all read the same enums the renderer accepts.
 * ============================================================================ */

pub const PRESET_SCHEMA: &str = include_str!("../../src/glyph-grid.schema.json");

fn preset_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::from_str(PRESET_SCHEMA).expect("embedded glyph-grid.schema.json is valid JSON")
    })
}

fn preset_validator() -> &'static jsonschema::Validator {
    static VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();
    VALIDATOR.get_or_init(|| {
        jsonschema::validator_for(preset_schema()).expect("embedded glyph-grid.schema.json compiles")
    })
}

fn schema_version() -> u64 {
    preset_schema()["x-preset-version"].as_u64().unwrap_or(1)
}

/// Postprocess stage names, in schema order.
fn schema_postprocess_stages() -> Vec<String> {
    preset_schema()["properties"]["postprocess"]["properties"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// `$schema` / `version` describe the preset file, not CONFIG — drop them
/// before the config reaches the renderer.
fn strip_preset_meta(cfg: &mut serde_json::Value) {
    if let Some(obj) = cfg.as_object_mut() {
        obj.remove("$schema");
        obj.remove("version");
    }
}

/// Validate a (partial) CONFIG object against the preset schema.
/// `Ok(warnings)` — unknown top-level keys, passed through
/// (`x-unknown-key-policy: warn`).  `Err(errors)` — every message starts
/// with the JSON pointer of the offending value (`/dither/mode: …`).
pub fn validate_config(cfg: &serde_json::Value) -> Result<Vec<String>, Vec<String>> {
    let errors: Vec<String> = preset_validator()
        .iter_errors(cfg)
        .map(|e| {
            let at = e.instance_path.to_string();
            let at = if at.is_empty() { "/".to_string() } else { at };
            match &e.kind {
                // The crate abbreviates long enums ("or 7 other candidates");
                // agents need the full allowed list to self-correct.
                jsonschema::error::ValidationErrorKind::Enum { options } => {
                    format!("{}: {} is not one of {}", at, e.instance, options)
                }
                _ => format!("{}: {}", at, e),
            }
        })
        .collect();
    if !errors.is_empty() {
        return Err(errors);
    }
    let known = preset_schema()["properties"].as_object();
    Ok(match (cfg.as_object(), known) {
        (Some(obj), Some(known)) => obj
            .keys()
            .filter(|k| !known.contains_key(*k))
            .map(|k| {
                format!(
                    "/{}: unknown key (not in preset schema v{}; passed through)",
                    k,
                    schema_version()
                )
            })
            .collect(),
        _ => Vec::new(),
    })
}

/* ---------- F27 (ITER-061): manifest schema + `--resume` ---------- */

/// The batch manifest's own shape (versioned, unknown keys are errors);
/// job configs are still checked against the preset schema.
pub const MANIFEST_SCHEMA: &str = include_str!("../../src/glyph-grid.manifest.schema.json");

fn manifest_validator() -> &'static jsonschema::Validator {
    static VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();
    VALIDATOR.get_or_init(|| {
        let schema: serde_json::Value =
            serde_json::from_str(MANIFEST_SCHEMA).expect("embedded manifest schema is JSON");
        jsonschema::validator_for(&schema).expect("embedded manifest schema compiles")
    })
}

/// Manifest-level errors as `"<json path>: <message>"` (empty = valid).
pub fn validate_manifest(doc: &serde_json::Value) -> Vec<String> {
    manifest_validator()
        .iter_errors(doc)
        .map(|e| {
            let at = e.instance_path.to_string();
            format!("{}: {}", if at.is_empty() { "/" } else { at.as_str() }, e)
        })
        .collect()
}

/// Frame count of an existing GIF / MP4, or why it is not a usable output.
/// Cheap: GIF frames are counted without LZW decoding, MP4 samples come
/// from the moov box.
fn output_frame_count(path: &std::path::Path, format: &str) -> Result<u64, String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    if format == "mp4" {
        let size = file.metadata().map_err(|e| e.to_string())?.len();
        let reader = mp4::Mp4Reader::read_header(std::io::BufReader::new(file), size)
            .map_err(|e| format!("not a readable MP4: {}", e))?;
        return reader
            .tracks()
            .values()
            .find(|t| matches!(t.track_type(), Ok(mp4::TrackType::Video)))
            .map(|t| u64::from(t.sample_count()))
            .ok_or_else(|| "MP4 has no video track".to_string());
    }
    let mut opts = gif::DecodeOptions::new();
    opts.skip_frame_decoding(true);
    let mut decoder = opts
        .read_info(std::io::BufReader::new(file))
        .map_err(|e| format!("not a readable GIF: {}", e))?;
    let mut n = 0u64;
    while decoder
        .read_next_frame()
        .map_err(|e| format!("truncated / corrupt GIF after {} frames: {}", n, e))?
        .is_some()
    {
        n += 1;
    }
    Ok(n)
}

/* ---------- ITER-068: `--resume` sidecars ----------
 *
 * A frame count is not proof that a job is done: gifski drops consecutive
 * pixel-identical frames (a still source can finish as a 1-frame GIF for
 * `frames: 24` and would be re-rendered forever), and a matching count says
 * nothing about WHICH config produced the file.  So every batch job that
 * commits its output also writes `<out>.ggs.json`:
 *   {"version":1, "frames":<requested, n | "auto">, "config_sha256":"<hex>",
 *    "bytes":<output size>, "finished_at":<unix seconds>}
 * `--resume` trusts a sidecar when present (skip iff same frames, same
 * config hash and the output still has the recorded size) and falls back to
 * the frame-count check only when there is none. */

const SIDECAR_VERSION: u64 = 1;

/// Batch outputs that get a sidecar when committed: out path → (requested
/// frames, config hash).  Filled by `run_headless_batch`; empty for GUI
/// exports and `render`, so those never write one.
static PENDING_SIDECARS: OnceLock<Mutex<HashMap<PathBuf, (serde_json::Value, String)>>> =
    OnceLock::new();

/// Sidecars take the same temp-file + rename path as outputs so a
/// pre-planted symlink at `<out>.ggs.json` is never followed.
fn write_sidecar_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "sidecar destination has no valid file name".to_string())?;
    let sequence = OUTPUT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let part_path = parent.join(format!(".{}.{}.{}.part", file_name, std::process::id(), sequence));
    let result = (|| -> Result<(), String> {
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
        fs::rename(&part_path, path).map_err(|e| format!("rename {:?}: {}", part_path, e))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part_path);
    }
    result
}

fn sidecar_path(out: &std::path::Path) -> PathBuf {
    let mut s = out.as_os_str().to_owned();
    s.push(".ggs.json");
    PathBuf::from(s)
}

/// Sorted keys at every level, so the hash does not depend on how the
/// manifest happened to order an object.
fn canonical_json(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                out.insert(k.clone(), canonical_json(&m[k]));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(a) => serde_json::Value::Array(a.iter().map(canonical_json).collect()),
        other => other.clone(),
    }
}

/// SHA-256 of everything in a (JS-shaped) batch job that shapes its output
/// bytes — source path, format, frames, border, config, capWidth,
/// targetMaxBytes; not `name` / `outPath`.
fn job_config_sha256(job: &serde_json::Value, batch_in: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut key = serde_json::json!({
        "in": job["inPath"].as_str().unwrap_or(batch_in),
        "format": job["format"],
        "frames": job["frames"],
        "border": job["border"],
        "config": job["config"],
        "capWidth": job["capWidth"],
        "targetMaxBytes": job["targetMaxBytes"],
    });
    // ITER-071: upload vs archive (ITER-050) changes the MP4 bytes, so it is
    // part of the key — for MP4 jobs only, so GIF hashes stay as they were.
    if job["format"].as_str() == Some("mp4") {
        let q = match Mp4Quality::parse(job["mp4Quality"].as_str()) {
            Ok(Mp4Quality::Archive) => "archive",
            _ => "upload",
        };
        key["mp4Quality"] = serde_json::Value::from(q);
    }
    Sha256::digest(canonical_json(&key).to_string().as_bytes())
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect()
}

/// Called by `atomic_write_output` once `out` is committed: record the
/// finished job.  A failed sidecar write only costs a re-render on resume.
fn write_resume_sidecar(out: &std::path::Path, bytes: u64) {
    let entry = PENDING_SIDECARS
        .get()
        .and_then(|m| m.lock().ok().and_then(|m| m.get(out).cloned()));
    let Some((frames, sha)) = entry else {
        return;
    };
    let finished_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let body = serde_json::json!({
        "version": SIDECAR_VERSION,
        "frames": frames,
        "config_sha256": sha,
        "bytes": bytes,
        "finished_at": finished_at,
    });
    if let Err(e) = write_sidecar_atomic(&sidecar_path(out), format!("{}\n", body).as_bytes()) {
        eprintln!(
            "batch: warning: cannot write resume sidecar for {}: {}",
            out.display(),
            e
        );
    }
}

/// `--resume`: Some(reason) when job `out` is already done.
/// With a sidecar: same requested frames + config hash + output size.
/// Without one (outputs from before ITER-062): a valid file whose frame
/// count fits the request — `<=` for GIF (gifski merges identical
/// frames), `==` for MP4, any count for `frames: "auto"`.
fn resume_skip_reason(
    out: &str,
    format: &str,
    frames: &serde_json::Value,
    config_sha: &str,
) -> Option<String> {
    let path = std::path::Path::new(out);
    if !path.is_file() {
        return None;
    }
    let side = sidecar_path(path);
    if side.exists() {
        // Unreadable / foreign sidecar → not trusted → re-render.
        let doc: serde_json::Value = fs::read_to_string(&side)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())?;
        let size = fs::metadata(path).ok()?.len();
        let same = doc["version"].as_u64() == Some(SIDECAR_VERSION)
            && doc["frames"] == *frames
            && doc["config_sha256"].as_str() == Some(config_sha)
            && doc["bytes"].as_u64() == Some(size);
        return same.then(|| "sidecar: same frames + config".to_string());
    }
    let n = output_frame_count(path, format).ok()?;
    match frames.as_u64() {
        Some(want) if format == "mp4" && n == want => Some(format!("{} frames, no sidecar", n)),
        Some(want) if format != "mp4" && n >= 1 && n <= want => {
            Some(format!("{} of {} frames, no sidecar", n, want))
        }
        None if n >= 1 => Some(format!("{} frames, frames:auto, no sidecar", n)),
        _ => None,
    }
}

/// `glyph-grid-studio validate <file>` — a preset (partial CONFIG) or a
/// batch manifest (`{"jobs":[{config},...]}`: every job config is checked).
/// Exit 0 when valid (warnings on stderr), 2 on any error.
pub fn run_validate(path: PathBuf) -> i32 {
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("validate: cannot read {}: {}", path.display(), e);
            return 2;
        }
    };
    let doc: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("validate: {} is not valid JSON: {}", path.display(), e);
            return 2;
        }
    };
    let mut targets: Vec<(String, serde_json::Value)> = Vec::new();
    let mut manifest_errors = 0usize;
    // ITER-062: a `jobs` key of ANY type makes it a manifest — `{"jobs":{}}`
    // used to be taken for a preset and pass.
    match doc.get("jobs") {
        Some(jobs) => {
            // F27 (ITER-061): the manifest's own keys first.
            for e in validate_manifest(&doc) {
                eprintln!("validate: error: manifest {}", e);
                manifest_errors += 1;
            }
            // ITER-062: the same per-field checks `batch` runs before launch,
            // so validate rejects exactly what batch rejects.
            if let Err(e) = manifest_frames(doc.get("frames"), "manifest") {
                eprintln!("validate: error: {}", e);
                manifest_errors += 1;
            }
            for (i, j) in jobs.as_array().into_iter().flatten().enumerate() {
                let name = j.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let what = format!("job {} ({:?})", i, name);
                if j.get("out").and_then(|v| v.as_str()).map(|s| !s.is_empty() && s.trim().is_empty()) == Some(true) {
                    eprintln!("validate: error: {} has no `out` path", what);
                    manifest_errors += 1;
                }
                if let Err(e) = manifest_frames(j.get("frames"), &what)
                    .and_then(|_| manifest_border(j.get("border"), &what))
                {
                    eprintln!("validate: error: {}", e);
                    manifest_errors += 1;
                }
                // ITER-071: the same mp4Quality check `batch` runs (ITER-050).
                if let Some(q) = j.get("mp4Quality").and_then(|v| v.as_str()) {
                    if let Err(e) = Mp4Quality::parse(Some(q)) {
                        eprintln!("validate: error: {}: {}", what, e);
                        manifest_errors += 1;
                    }
                }
                let cfg = j.get("config").cloned().unwrap_or(serde_json::json!({}));
                targets.push((format!("{} config", what), cfg));
            }
        }
        None => {
            if !doc.is_object() {
                eprintln!(
                    "validate: {} must be a JSON object (a preset or a batch manifest), got {}",
                    path.display(),
                    json_kind(&doc)
                );
                return 2;
            }
            targets.push(("preset".into(), doc));
        }
    }
    let (mut n_err, mut n_warn) = (manifest_errors, 0usize);
    for (label, cfg) in &targets {
        match validate_config(cfg) {
            Ok(warnings) => {
                for w in warnings {
                    eprintln!("validate: warning: {} {}", label, w);
                    n_warn += 1;
                }
            }
            Err(errors) => {
                for e in errors {
                    eprintln!("validate: error: {} {}", label, e);
                    n_err += 1;
                }
            }
        }
    }
    if n_err > 0 {
        eprintln!(
            "validate: {} is INVALID ({} error(s), {} warning(s); preset schema v{})",
            path.display(),
            n_err,
            n_warn,
            schema_version()
        );
        return 2;
    }
    println!(
        "validate: {} ok ({} config(s), {} warning(s); preset schema v{})",
        path.display(),
        targets.len(),
        n_warn,
        schema_version()
    );
    0
}

/// Managed state that the JS side queries on startup to discover the CLI job.
struct CliJobState {
    job: Mutex<Option<serde_json::Value>>,
    exit_code: Arc<Mutex<i32>>,
}

/// Returns the CLI render job to JS, or null in GUI mode.
#[tauri::command]
fn get_cli_render_job(state: tauri::State<CliJobState>) -> Option<serde_json::Value> {
    touch_liveness();
    state.job.lock().ok().and_then(|g| g.clone())
}

/// JS-side log forwarding for headless / production builds where
/// `console.log` doesn't reach stderr.  Useful for surfacing batch errors.
#[tauri::command]
fn cli_log(msg: String) {
    touch_liveness();
    eprintln!("glyph-grid-studio[js]: {}", msg);
}

/* ---------- F25 (ITER-059): exit codes + the headless watchdog ----------
 *
 * Headless `render` / `batch` exit codes (documented in README):
 *   0 ok · 1 render/encode failed · 2 bad input (usage, preset, manifest,
 *   config, source path) · 3 timed out (`--timeout`, or no progress for
 *   STALL_SECS).
 *
 * The render runs inside this process's WebView, so the watchdog is a plain
 * thread: on expiry it removes the `.part` files this process owns and
 * `std::process::exit`s — the WebKit WebContent/GPU XPC children die with
 * their host.  Liveness is touched by every staged frame, every encode
 * progress tick and every `cli_log`, so a legitimately slow job (throttled
 * capture ~400 ms/frame, a 25 s gifski encode — both tick) never trips it. */

pub const EXIT_OK: i32 = 0;
pub const EXIT_FAILED: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_TIMEOUT: i32 = 3;

/// No liveness touch for this long = a stalled WebView (the 35-min hang
/// class tests/run-all.sh used to need `timeout` for).
const STALL_SECS: u64 = 300;
/// Test-only override of STALL_SECS (tests/batch-jobs-stall.sh, ITER-072):
/// lets a test reach the stall guard in seconds instead of minutes.  A
/// `--jobs` parent strips it from its children so only the parent's guard
/// is under test.
const STALL_SECS_TEST_ENV: &str = "GGS_TEST_STALL_SECS";

fn stall_secs() -> u64 {
    std::env::var(STALL_SECS_TEST_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(STALL_SECS)
}

static WATCHDOG_EPOCH: OnceLock<std::time::Instant> = OnceLock::new();
/// Milliseconds since WATCHDOG_EPOCH of the last liveness touch.
static LAST_LIVENESS_MS: AtomicU64 = AtomicU64::new(0);
/// `.part` files this process is writing (atomic_write_output) — removed
/// by `abort_headless` so a timeout / Ctrl-C leaves no strays.
static LIVE_PARTS: OnceLock<Mutex<std::collections::HashSet<PathBuf>>> = OnceLock::new();

fn watchdog_now_ms() -> u64 {
    WATCHDOG_EPOCH
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as u64
}

fn touch_liveness() {
    LAST_LIVENESS_MS.store(watchdog_now_ms(), Ordering::Relaxed);
}

fn live_parts() -> &'static Mutex<std::collections::HashSet<PathBuf>> {
    LIVE_PARTS.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Remove this process's in-flight `.part` files and exit with `code`.
fn abort_headless(code: i32, why: &str) -> ! {
    eprintln!("glyph-grid-studio: {} (exit {})", why, code);
    if let Ok(parts) = live_parts().lock() {
        for p in parts.iter() {
            let _ = fs::remove_file(p);
        }
    }
    // ITER-071: a `batch --jobs` parent asks its children to stop the same
    // clean way (SIGTERM → their own sweep, exit 143), waits up to 2 s for
    // the main thread to reap them — the relays must keep draining their
    // stderr meanwhile: a child whose stderr pipe is gone panics in
    // eprintln! and never runs its sweep — then SIGKILLs any straggler and
    // drops the private sub-manifest directory.
    BATCH_ABORTING.store(true, Ordering::SeqCst);
    let pids = BATCH_CHILDREN_LIVE.lock().map(|l| l.0.clone()).unwrap_or_default();
    if !pids.is_empty() {
        for pid in &pids {
            terminate_child(*pid);
        }
        let grace = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < grace
            && BATCH_CHILDREN_LIVE.lock().map(|l| !l.0.is_empty()).unwrap_or(false)
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if let Ok(live) = BATCH_CHILDREN_LIVE.lock() {
            for pid in &live.0 {
                kill_child_hard(*pid);
            }
        }
    }
    if let Ok(live) = BATCH_CHILDREN_LIVE.lock() {
        if let Some(dir) = &live.1 {
            let _ = fs::remove_dir_all(dir);
        }
    }
    std::process::exit(code);
}

/* ---------- F26 (ITER-060): NDJSON progress + SIGINT/SIGTERM ---------- */

/// `--progress`: one `{"type":"progress",...}` line per staged frame and
/// per encode tick on stderr.  Off by default — stderr stays the human /
/// `PERF_JOB` / `batch:` stream existing parsers read.
static PROGRESS_NDJSON: AtomicBool = AtomicBool::new(false);
/// Capture job id → the manifest job name (`begin_export_capture` label).
static CAPTURE_LABELS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
/// Signal number caught by `on_signal` (0 = none); the watchdog acts on it.
static PENDING_SIGNAL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

pub fn enable_progress_ndjson() {
    PROGRESS_NDJSON.store(true, Ordering::Relaxed);
}

fn progress_line(job_id: &str, phase: &str, frame: usize, total: usize) {
    if !PROGRESS_NDJSON.load(Ordering::Relaxed) {
        return;
    }
    let job = CAPTURE_LABELS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|m| m.get(job_id).cloned())
        .unwrap_or_else(|| job_id.to_string());
    // Hand-ordered (json! sorts keys) so every line starts with
    // `{"type":"progress"` — a cheap prefix test for consumers.
    eprintln!(
        "{{\"type\":\"progress\",\"job\":{},\"phase\":{},\"frame\":{},\"total\":{}}}",
        serde_json::Value::from(job),
        serde_json::Value::from(phase),
        frame.min(total),
        total
    );
}

#[cfg(unix)]
mod sig {
    extern "C" {
        fn signal(signum: i32, handler: extern "C" fn(i32)) -> usize;
        fn kill(pid: i32, sig: i32) -> i32;
    }
    pub const SIGINT: i32 = 2;
    pub const SIGTERM: i32 = 15;

    /// Async-signal-safe: an atomic store only; the watchdog thread does the
    /// cleanup + exit within 100 ms.
    extern "C" fn on_signal(signum: i32) {
        super::PENDING_SIGNAL.store(signum, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn install() {
        unsafe {
            signal(SIGINT, on_signal);
            signal(SIGTERM, on_signal);
        }
    }

    pub fn terminate(pid: u32) {
        unsafe {
            kill(pid as i32, SIGTERM);
        }
    }

    pub fn kill_hard(pid: u32) {
        unsafe {
            kill(pid as i32, 9);
        }
    }
}

/// SIGKILL a `--jobs` child that ignored SIGTERM for the 2 s grace (ITER-071).
fn kill_child_hard(pid: u32) {
    #[cfg(unix)]
    sig::kill_hard(pid);
    #[cfg(not(unix))]
    let _ = pid;
}

/// Ask a headless child (`render`/`batch`) to stop cleanly: SIGTERM → its
/// watchdog removes its `.part` files and exits 143.  Used by MCP cancel.
pub fn terminate_child(pid: u32) {
    #[cfg(unix)]
    sig::terminate(pid);
    #[cfg(not(unix))]
    let _ = pid;
}

/// Spawn the watchdog for a headless run.  `timeout_secs` = the overall
/// `--timeout` budget (None/0 = none); the stall guard always applies.
/// F26 (ITER-060): it also turns a caught SIGINT/SIGTERM into a clean exit
/// (128 + signal: 130 / 143).
fn start_watchdog(timeout_secs: Option<u64>) {
    touch_liveness();
    #[cfg(unix)]
    sig::install();
    let deadline_ms = timeout_secs.filter(|s| *s > 0).map(|s| s * 1000);
    let stall_ms = stall_secs() * 1000;
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let signum = PENDING_SIGNAL.load(Ordering::SeqCst);
        if signum != 0 {
            let name = if signum == 2 { "SIGINT" } else { "SIGTERM" };
            abort_headless(128 + signum, &format!("interrupted by {}", name));
        }
        let now = watchdog_now_ms();
        if let Some(d) = deadline_ms {
            if now >= d {
                abort_headless(
                    EXIT_TIMEOUT,
                    &format!("timed out: --timeout {} s elapsed", d / 1000),
                );
            }
        }
        let idle = now.saturating_sub(LAST_LIVENESS_MS.load(Ordering::Relaxed));
        if idle >= stall_ms {
            abort_headless(
                EXIT_TIMEOUT,
                &format!("stalled: no render progress for {} s", idle / 1000),
            );
        }
    });
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
    let code = if ok { EXIT_OK } else { EXIT_FAILED };
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
pub fn run_headless_batch(
    manifest_path: PathBuf,
    show_window: bool,
    timeout_secs: Option<u64>,
    resume: bool,
    jobs: usize,
) -> i32 {
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
    // F27 (ITER-061): the manifest schema (version, known keys, types)
    // before anything else — a typo'd key (`capwidth`) used to be silently
    // ignored and cost a full batch of the wrong thing.
    let manifest_errors = validate_manifest(&manifest);
    if !manifest_errors.is_empty() {
        for e in &manifest_errors {
            eprintln!("batch: manifest {}", e);
        }
        eprintln!(
            "batch: manifest {:?} is invalid ({} error(s); see src/glyph-grid.manifest.schema.json) — refusing before any render runs",
            manifest_path,
            manifest_errors.len()
        );
        return 2;
    }

    // Required: in (source path), frames, jobs (array)
    let in_path = manifest
        .get("in")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    // ITER-057: `frames` is a positive integer or "auto" (default 24); a
    // malformed value used to be silently replaced by 24.
    let frames = match manifest_frames(manifest.get("frames"), "manifest") {
        Ok(f) => f.unwrap_or_else(|| serde_json::Value::from(24)),
        Err(e) => {
            eprintln!("batch: {}", e);
            return 2;
        }
    };
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

    // R7 (ITER-056): validate every job config against the preset schema
    // before Tauri launches — a typo'd enum used to cost a full render of
    // the wrong thing (or a silent fallback).
    let mut config_errors = 0usize;
    for (i, j) in jobs_arr.iter().enumerate() {
        let name = j.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let what = format!("job {} ({:?})", i, name);
        if let Err(e) = manifest_frames(j.get("frames"), &what)
            .and_then(|_| manifest_border(j.get("border"), &what))
        {
            eprintln!("batch: {}", e);
            config_errors += 1;
        }
        let cfg = j.get("config").cloned().unwrap_or(serde_json::json!({}));
        match validate_config(&cfg) {
            Ok(warnings) => {
                for w in warnings {
                    eprintln!("batch: warning: job {} ({:?}) config {}", i, name, w);
                }
            }
            Err(errors) => {
                for e in errors {
                    eprintln!("batch: job {} ({:?}) config {}", i, name, e);
                }
                config_errors += 1;
            }
        }
    }
    if config_errors > 0 {
        eprintln!(
            "batch: {} job(s) invalid — refusing manifest before any render runs",
            config_errors
        );
        return 2;
    }

    // Build the JS-shaped batch JSON.  Each job carries its own out/format/config.
    let jobs_js: Vec<serde_json::Value> = jobs_arr
        .iter()
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
            let mut config = j.get("config").cloned().unwrap_or(serde_json::json!({}));
            strip_preset_meta(&mut config);
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
            // ITER-057: per-job `frames` (else the manifest's) and `border`
            // — both were manifest-top-level-only / GUI-only before.
            // Validated above, so the fallbacks never fire.
            let job_frames = manifest_frames(j.get("frames"), "")
                .ok()
                .flatten()
                .unwrap_or_else(|| frames.clone());
            let border = manifest_border(j.get("border"), "").unwrap_or(serde_json::Value::Null);
            serde_json::json!({
                "name": name,
                "outPath": out_path,
                "inPath": in_path_job,
                "format": format,
                "frames": job_frames,
                "border": border,
                "config": config,
                "capWidth": cap_width,
                "targetMaxBytes": target_max_bytes,
                "mp4Quality": j.get("mp4Quality").and_then(|v| v.as_str()),
            })
        })
        .collect();

    // F27 (ITER-061): `--resume` drops jobs whose output is already done
    // (ITER-024's out-of-scope item: a timed-out 64-job run resumes with
    // one flag).  ITER-068: "done" = a matching `<out>.ggs.json` sidecar,
    // else the frame-count fallback — see resume_skip_reason.
    // ITER-071: the raw manifest job rides along so `--jobs` can split
    // exactly the jobs resume kept.
    let mut paired: Vec<(serde_json::Value, serde_json::Value)> =
        jobs_arr.into_iter().zip(jobs_js).collect();
    let job_sha = |j: &serde_json::Value| job_config_sha256(j, &in_path);
    if resume {
        let before = paired.len();
        paired.retain(|(_, j)| {
            let out = j["outPath"].as_str().unwrap_or("");
            match resume_skip_reason(
                out,
                j["format"].as_str().unwrap_or("gif"),
                &j["frames"],
                &job_sha(j),
            ) {
                Some(why) => {
                    let name = j["name"].as_str().unwrap_or("job");
                    eprintln!("batch: resume: skip {:?} — {} already done ({})", name, out, why);
                    false
                }
                None => true,
            }
        });
        eprintln!("batch: resume: skipped {} of {} jobs", before - paired.len(), before);
        if paired.is_empty() {
            eprintln!("batch complete: nothing to do (every output is already done)");
            return 0;
        }
    }
    let (jobs_arr, jobs_js): (Vec<serde_json::Value>, Vec<serde_json::Value>) =
        paired.into_iter().unzip();

    // ITER-055 (F4): `--jobs N` forks N CLI children, each its own
    // app + WebContent + GPU triple, over a round-robin split of the
    // already-validated manifest.  ITER-071: after every pre-launch check
    // and the resume filter, so the children see only jobs that must run;
    // each child writes its own sidecars.  Wall is bound by each off-screen
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
            return run_batch_children(&manifest, &in_path, &jobs_arr, workers, show_window, timeout_secs);
        }
    }

    // ITER-068: every job that runs records a sidecar when its output lands.
    if let Ok(mut pending) = PENDING_SIDECARS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        for j in &jobs_js {
            let out = PathBuf::from(j["outPath"].as_str().unwrap_or(""));
            pending.insert(out, (j["frames"].clone(), job_sha(j)));
        }
    }

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

    // ITER-062: Tauri returning without exit_with_status is a failed run
    // (1); 2 strictly means rejected before launch (README table).
    let exit_code = Arc::new(Mutex::new(EXIT_FAILED));
    let state = CliJobState {
        job: Mutex::new(Some(batch_json)),
        exit_code: exit_code.clone(),
    };
    start_watchdog(timeout_secs);
    run_tauri(state, !show_window);
    let code = *exit_code.lock().unwrap();
    code
}

/// Resolve a render job into the JSON the JS side consumes (and that
/// `render --dry-run` prints).  Warnings go to stderr; errors (unreadable
/// preset, schema violations) are returned for the caller to exit 2 on.
pub fn headless_render_json(job: &HeadlessRenderJob) -> Result<serde_json::Value, String> {
    let (config, warnings) = job.build_js_config()?;
    for w in warnings {
        eprintln!("render: warning: config{}", w);
    }
    Ok(serde_json::json!({
        "inPath": job.in_path.to_string_lossy().to_string(),
        "outPath": job.out_path.to_string_lossy().to_string(),
        "frames": match job.frames {
            Some(n) => serde_json::Value::from(n),
            None => serde_json::Value::from("auto"),
        },
        "format": job.resolved_format(),
        "mp4Quality": job.mp4_quality,
        "config": config,
    }))
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
/// refused for such manifests.  ITER-070: compared after `normalize_out_path`
/// so `a.gif`, `./a.gif` and `sub/../a.gif` count as the same file.
fn duplicate_out_path(jobs: &[serde_json::Value]) -> Option<String> {
    let mut seen = std::collections::HashSet::new();
    jobs.iter()
        .filter_map(|j| j.get("out").and_then(|v| v.as_str()))
        .find(|o| !seen.insert(normalize_out_path(o)))
        .map(|o| o.to_string())
}

/// Absolute, lexically normalised form of an output path: `.` dropped, `..`
/// pops the previous component.  Lexical on purpose — the file usually does
/// not exist yet, so `canonicalize` can't be used.  The parent directory is
/// canonicalised when it exists, so a symlinked directory and its target
/// also collide.
fn normalize_out_path(out: &str) -> PathBuf {
    use std::path::Component;
    let abs = std::path::absolute(out).unwrap_or_else(|_| PathBuf::from(out));
    let mut norm = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                norm.pop();
            }
            other => norm.push(other.as_os_str()),
        }
    }
    match (norm.parent().and_then(|p| p.canonicalize().ok()), norm.file_name()) {
        (Some(parent), Some(name)) => parent.join(name),
        _ => norm,
    }
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
/// this binary.  Each child's stderr is relayed line by line (PERF_JOB and
/// `--progress` NDJSON lines verbatim so NDJSON consumers need no change;
/// everything else tagged `batch[i/n]`).  ITER-072: children always run
/// with `--progress` so every frame touches the parent's liveness; the
/// NDJSON is dropped unless the parent itself has `--progress`.  Exit = the worst child's code (a
/// signal counts as 1).  ITER-071: the parent runs the ITER-059 watchdog
/// (`--timeout`, stall guard, SIGINT/SIGTERM); `abort_headless` SIGTERMs
/// the registered children (each sweeps its own `.part` files) and removes
/// the private sub-manifest directory before exiting.
fn run_batch_children(
    manifest: &serde_json::Value,
    in_path: &str,
    jobs: &[serde_json::Value],
    n: usize,
    show_window: bool,
    timeout_secs: Option<u64>,
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
    // ITER-070: sub-manifests live in a private 0700 directory created for
    // this run (fresh name, mkdir fails rather than following anything that
    // already exists) and are opened create_new 0600 — never a predictable
    // path in the shared temp dir.  The guard removes the directory and
    // kills any child still running on every exit path, panics included.
    let mut guard = match BatchChildren::new() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("batch: --jobs: cannot create private sub-manifest dir: {}", e);
            return 2;
        }
    };
    let mut paths = Vec::with_capacity(n);
    for (i, part) in parts.iter().enumerate() {
        let p = guard.dir.join(format!("part-{}.json", i));
        if let Err(e) = write_private_file(&p, part.to_string().as_bytes()) {
            eprintln!("batch: --jobs: cannot write sub-manifest {:?}: {}", p, e);
            return 2;
        }
        paths.push(p);
    }
    eprintln!("batch: {} jobs across {} processes", jobs.len(), n);
    if let Ok(mut live) = BATCH_CHILDREN_LIVE.lock() {
        live.1 = Some(guard.dir.clone());
    }
    start_watchdog(timeout_secs);

    let relay_progress = PROGRESS_NDJSON.load(Ordering::Relaxed);
    let mut worst = 0;
    let mut relays = Vec::with_capacity(n);
    for (i, p) in paths.iter().enumerate() {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("batch").arg("--manifest").arg(p).env_remove(STALL_SECS_TEST_ENV);
        if show_window {
            cmd.arg("--show-window");
        }
        // ITER-072: always ask for per-frame NDJSON — the parent's stall
        // guard only sees liveness through these stderr lines, and a quiet
        // child prints nothing for a whole job.  Relayed only when the
        // parent itself has `--progress`.
        cmd.arg("--progress");
        match cmd.stderr(std::process::Stdio::piped()).spawn() {
            Ok(mut child) => {
                let stderr = child.stderr.take().expect("piped stderr");
                if let Ok(mut live) = BATCH_CHILDREN_LIVE.lock() {
                    live.0.push(child.id());
                }
                guard.children.push(Some(child));
                let relay = std::thread::spawn(move || {
                    let mut r = std::io::BufReader::new(stderr);
                    let mut line = Vec::new();
                    while matches!(r.read_until(b'\n', &mut line), Ok(k) if k > 0) {
                        let text = String::from_utf8_lossy(&line);
                        let text = text.trim_end_matches('\n');
                        touch_liveness();
                        if text.starts_with("{\"type\":\"progress\"") {
                            if relay_progress {
                                eprintln!("{}", text);
                            }
                        } else if text.contains("PERF_JOB") {
                            eprintln!("{}", text);
                        } else {
                            eprintln!("batch[{}/{}]: {}", i + 1, n, text);
                        }
                        line.clear();
                    }
                });
                relays.push(relay);
            }
            Err(e) => {
                eprintln!("batch: --jobs: cannot spawn child {}: {}", i + 1, e);
                worst = 2;
            }
        }
    }
    for k in 0..guard.children.len() {
        let code = match guard.children[k].as_mut().map(|c| c.wait()) {
            Some(Ok(s)) => s.code().unwrap_or(1),
            _ => 1,
        };
        // Reaped: neither the guard nor abort_headless may signal a pid
        // that may be reused.
        if let (Some(pid), Ok(mut live)) =
            (guard.children[k].as_ref().map(|c| c.id()), BATCH_CHILDREN_LIVE.lock())
        {
            live.0.retain(|p| *p != pid);
        }
        guard.children[k] = None;
        worst = worst.max(code);
    }
    for relay in relays {
        let _ = relay.join();
    }
    // ITER-071: abort_headless owns the exit code (3 / 130 / 143).
    while BATCH_ABORTING.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    worst
}

/// ITER-070: owns the `--jobs` children and their private sub-manifest
/// directory.  Drop kills (and reaps) every child not yet waited for and
/// removes the directory, so neither outlives the parent's batch call.
/// ITER-071: `std::process::exit` (the watchdog's `abort_headless`) skips
/// Drop, so the live pids + directory are also mirrored in
/// `BATCH_CHILDREN_LIVE` for that path.
struct BatchChildren {
    dir: PathBuf,
    children: Vec<Option<std::process::Child>>,
}

impl BatchChildren {
    fn new() -> std::io::Result<Self> {
        use std::hash::{BuildHasher, Hasher};
        #[cfg(unix)]
        use std::os::unix::fs::DirBuilderExt;
        let mut last = None;
        for attempt in 0..8u32 {
            // RandomState is seeded from the OS RNG per process; hashing the
            // pid, attempt and clock through it gives an unguessable suffix.
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u32(std::process::id());
            h.write_u32(attempt);
            h.write_u128(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
            );
            let dir = std::env::temp_dir()
                .join(format!("ggs-batch-{}-{:016x}", std::process::id(), h.finish()));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            builder.mode(0o700);
            match builder.create(&dir) {
                Ok(()) => {
                    return Ok(BatchChildren {
                        dir,
                        children: Vec::new(),
                    })
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(last.unwrap_or_else(|| std::io::Error::other("no free temp dir name")))
    }
}

/// `--jobs` children still running + their sub-manifest dir (ITER-071).
static BATCH_CHILDREN_LIVE: Mutex<(Vec<u32>, Option<PathBuf>)> = Mutex::new((Vec::new(), None));
/// Set by `abort_headless`: the batch parent's main thread must not race
/// it to `exit` with the children's 143 once it has reaped them.
static BATCH_ABORTING: AtomicBool = AtomicBool::new(false);

impl Drop for BatchChildren {
    fn drop(&mut self) {
        if let Ok(mut live) = BATCH_CHILDREN_LIVE.lock() {
            *live = (Vec::new(), None);
        }
        for child in self.children.iter_mut().flatten() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Write `bytes` to a file that must not exist yet, mode 0600 (ITER-070).
fn write_private_file(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)?.write_all(bytes)
}

/// Public entry point invoked by main.rs for `render` subcommand.  Returns
/// the exit code; in practice the process exits via `app.exit()` from JS
/// before this function returns, so the return value here is a fallback for
/// the "tauri runtime closed without a render" case.
pub fn run_headless_render(job: HeadlessRenderJob) -> i32 {
    let job_json = match headless_render_json(&job) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("render: {}", e);
            return 2;
        }
    };
    // ITER-062: "didn't complete" is a failure (1), not a usage error (2).
    let exit_code = Arc::new(Mutex::new(EXIT_FAILED));
    let state = CliJobState {
        job: Mutex::new(Some(job_json)),
        exit_code: exit_code.clone(),
    };
    start_watchdog(job.timeout_secs);
    run_tauri(state, !job.show_window);
    // In practice we should never reach here — exit_with_status calls
    // std::process::exit() directly. This is a fallback for the "Tauri
    // shut down without JS firing exit_with_status" case.
    let code = *exit_code.lock().unwrap();
    code
}

/// The catalog of all options the CLI / MCP / HTTP surfaces can pass.
/// R7 (ITER-056): a projection of the generated preset schema's enums — it
/// used to be a hand-kept list that had already drifted from the renderer
/// (no `flow` selection mode, no `kawaii` stage).
pub fn catalog_json() -> String {
    let p = &preset_schema()["properties"];
    let glyph_sets: Vec<serde_json::Value> = p["glyphSet"]["enum"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|v| if v.is_null() { "null".into() } else { v.clone() })
                .collect()
        })
        .unwrap_or_default();
    serde_json::json!({
        "palettes": p["palette"]["enum"],
        "color_modes": p["colorMode"]["enum"],
        "ramps": p["ramp"]["enum"],
        "dithers": p["dither"]["properties"]["mode"]["enum"],
        "selection_modes": p["selectionMode"]["enum"],
        "glyph_sets": glyph_sets,
        "sampling_strategies": p["samplingStrategy"]["enum"],
        "postprocess_stages": schema_postprocess_stages(),
        "schema_version": schema_version(),
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

/// A per-job streaming GIF encoder (ITER-053).  Frames go over an unbounded
/// channel so the IPC/capture chain rarely waits on gifski (index.html
/// pauses the draw loop at 3 in-flight pushes).  ITER-070: for a job that
/// retains nothing, the bytes sitting in that channel are counted in
/// `queued` (the feeder subtracts each frame once gifski has it) and
/// `stage_export_frame` waits while a push would take the queue past the
/// same byte caps a retaining job is held to — a slow encoder throttles the
/// capture instead of growing memory without bound.
struct GifStream {
    config: GifStreamConfig,
    tx: Option<std::sync::mpsc::Sender<PngFrame>>,
    state: Arc<AtomicU8>,
    handle: Option<std::thread::JoinHandle<Result<StreamedGif, String>>>,
    queued: Arc<AtomicU64>,
    /// (compressed, raw) queue caps; the export byte caps outside tests.
    queue_caps: (u64, u64),
}

impl GifStreamConfig {
    /// Frames are kept for a possible twitter-fit re-encode only when a
    /// byte target is set.
    fn retains_frames(&self) -> bool {
        self.target_max_bytes.is_some_and(|t| t > 0)
    }
}

/// What `stage_export_frame` does with a frame for a non-retaining stream
/// (ITER-070): send it, wait for the encoder to drain, or fail.
#[derive(Debug, PartialEq, Eq)]
enum StreamAdmit {
    Send,
    Wait,
}

/// `queued` bytes are in the channel; can `len` more go in under `cap`?
/// A frame that alone exceeds the cap can never fit, and a stopped encoder
/// never drains, so both fail instead of waiting forever.
fn stream_queue_admit(queued: u64, len: u64, cap: u64, running: bool) -> Result<StreamAdmit, String> {
    match queued.checked_add(len) {
        Some(total) if total <= cap => Ok(StreamAdmit::Send),
        _ if queued == 0 => Err(format!(
            "frame of {} bytes exceeds the streaming queue limit {} bytes",
            len, cap
        )),
        _ if !running => Err(
            "streaming GIF encoder stopped; finish or cancel the export for the cause".to_string(),
        ),
        _ => Ok(StreamAdmit::Wait),
    }
}

impl GifStream {
    /// Encoder thread alive and the job neither cancelled nor committing.
    fn is_running(&self) -> bool {
        self.state.load(Ordering::Acquire) == JOB_RUNNING
            && self.handle.as_ref().is_some_and(|h| !h.is_finished())
    }

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
    queued: Arc<AtomicU64>,
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
                    } else {
                        // ITER-070: gifski has it; free its queue budget.
                        let len = frame.bytes.len() as u64;
                        drop(frame);
                        queued.fetch_sub(len, Ordering::AcqRel);
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
        touch_liveness();
        if let Some(job_id) = &self.job_id {
            progress_line(job_id, phase, completed, total);
        }
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
    // ITER-068 sidecar, here so the streamed GIF commit (ITER-053) records
    // one too, not only atomic_write_output.
    let bytes = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    write_resume_sidecar(path, bytes);
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

    if let Ok(mut parts) = live_parts().lock() {
        parts.insert(part_path.clone());
    }
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
    if let Ok(mut parts) = live_parts().lock() {
        parts.remove(&part_path);
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
    label: Option<String>,
) -> Result<(), String> {
    if job_id.trim().is_empty() {
        return Err("jobId must not be empty".into());
    }
    // F26 (ITER-060): the batch driver names its capture so `--progress`
    // lines say which manifest job they belong to.
    if let Some(label) = label {
        if let Ok(mut labels) = CAPTURE_LABELS.get_or_init(|| Mutex::new(HashMap::new())).lock() {
            labels.insert(job_id.clone(), label);
        }
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
        // ITER-059's abort sweep must see the streamed .part too: it lives
        // for the whole capture (ITER-053 gotcha a).
        if let Ok(mut parts) = live_parts().lock() {
            parts.insert(part_path.clone());
        }
        let state = Arc::new(AtomicU8::new(JOB_RUNNING));
        let control = ExportControl {
            state: state.clone(),
            app: None,
            job_id: Some(job_id.clone()),
        };
        let (tx, rx) = std::sync::mpsc::channel::<PngFrame>();
        let thread_config = config.clone();
        let queued = Arc::new(AtomicU64::new(0));
        let thread_queued = queued.clone();
        let handle = std::thread::Builder::new()
            .name("gif-stream".into())
            .spawn(move || run_gif_stream(rx, thread_config, total, part_path, control, thread_queued))
            .map_err(|e| format!("spawn GIF stream encoder: {}", e))?;
        capture.stream = Some(GifStream {
            config,
            tx: Some(tx),
            state,
            handle: Some(handle),
            queued,
            queue_caps: (MAX_TOTAL_COMPRESSED_FRAME_BYTES, MAX_TOTAL_RAW_FRAME_BYTES),
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
/// by the base64 and raw-body push commands.  ITER-070: a non-retaining
/// stream whose queue is full hands the frame back; wait (outside the
/// captures lock, so the encoder, other jobs and cancel_export proceed) and
/// retry.  A cancel drops the capture, so the retry fails as unknown.
fn stage_export_frame(job_id: &str, index: usize, frame: PngFrame) -> Result<(), String> {
    touch_liveness();
    let mut frame = frame;
    loop {
        match try_stage_export_frame(job_id, index, frame)? {
            None => return Ok(()),
            Some(back) => {
                frame = back;
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    }
}

/// One attempt of `stage_export_frame`: `Ok(None)` = staged, `Ok(Some)` =
/// the stream queue is full, try again later with the returned frame.
fn try_stage_export_frame(
    job_id: &str,
    index: usize,
    frame: PngFrame,
) -> Result<Option<PngFrame>, String> {
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
    // frames count against neither running total — ITER-070 bounds what is
    // still queued for the encoder instead, below.
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
    let mut reserved = 0;
    if let Some(stream) = capture.stream.as_ref().filter(|_| !retained) {
        let cap = if frame.rgba.is_some() { stream.queue_caps.1 } else { stream.queue_caps.0 };
        let frame_len = frame.bytes.len() as u64;
        // Serialised by the captures lock; the feeder only ever subtracts.
        let queued = stream.queued.load(Ordering::Acquire);
        match stream_queue_admit(queued, frame_len, cap, stream.is_running())? {
            StreamAdmit::Send => {
                stream.queued.fetch_add(frame_len, Ordering::AcqRel);
                reserved = frame_len;
            }
            StreamAdmit::Wait => return Ok(Some(frame)),
        }
    }
    match &capture.stream {
        // The encoder keeps retained frames itself, for the ladder.
        Some(stream) => {
            let sent = stream
                .tx
                .as_ref()
                .ok_or_else(|| "GIF stream is closed".to_string())
                .and_then(|tx| {
                    tx.send(frame).map_err(|_| {
                        "streaming GIF encoder stopped; finish or cancel the export for the cause"
                            .to_string()
                    })
                });
            if let Err(error) = sent {
                stream.queued.fetch_sub(reserved, Ordering::AcqRel);
                return Err(error);
            }
        }
        None => capture.frames.push(frame),
    }
    capture.total_bytes = next_bytes;
    capture.total_raw_bytes = next_raw_bytes;
    capture.received += 1;
    let (staged, expected) = (capture.received, capture.expected_frames);
    drop(captures);
    progress_line(job_id, "capture", staged, expected);
    Ok(None)
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
            // ITER-070: release parked workers even if this thread panics —
            // the guard's Drop runs on unwind, an inline block would not.
            let release = ReleaseGifWorkers { stop: stop_ref, fed: fed_ref };
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
            drop(release);
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

/// Releases every decode worker parked on the window (or about to park) in
/// `encode_gif_gifski_frames`.  The flag is set under the mutex so a worker
/// can't check it, miss this notify, and park forever (lost wakeup).  A
/// Drop guard (ITER-070) so a feeder panic still runs it.
struct ReleaseGifWorkers<'a> {
    stop: &'a AtomicBool,
    fed: &'a (Mutex<usize>, std::sync::Condvar),
}

impl Drop for ReleaseGifWorkers<'_> {
    fn drop(&mut self) {
        {
            let _g = self.fed.0.lock().unwrap_or_else(|e| e.into_inner());
            self.stop.store(true, Ordering::Relaxed);
        }
        self.fed.1.notify_all();
    }
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

/// ITER-070: clap value parser for `render --mp4-quality`, so the CLI
/// accepts exactly the spellings the manifest and MCP accept (any case,
/// surrounding whitespace) and hands the canonical name on.
pub fn parse_mp4_quality_arg(value: &str) -> Result<String, String> {
    Ok(match Mp4Quality::parse(Some(value))? {
        Mp4Quality::Upload => "upload".to_string(),
        Mp4Quality::Archive => "archive".to_string(),
    })
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
    if let Ok(mut parts) = live_parts().lock() {
        parts.remove(&streamed.part_path);
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

/// Path-aware picker: returns `{path, sizeBytes, mimeType}` so the JS side
/// can persist a "recent sources" list keyed on path.  ITER-064 (F18): no
/// data URL — the webview loads the file itself via the asset protocol.
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
            let size_bytes = source_file_size(&p)?;
            Ok(serde_json::json!({
                "path": p.to_string_lossy().to_string(),
                "sizeBytes": size_bytes,
                "mimeType": image_mime_type(&p)
            }))
        }
        None => Err("cancelled".into()),
    }
}

/// Metadata for an image file at an absolute path: `{path, sizeBytes,
/// mimeType}`.  ITER-064 (F18): the bytes no longer cross IPC as a base64
/// data URL (≤6 full-file copies for a 64 MB GIF); the webview loads
/// `convertFileSrc(path)` from disk.  `inline: true` restores the data URL
/// for the fallback when `convertFileSrc` is unavailable.
#[tauri::command]
async fn read_image_file(path: String, inline: Option<bool>) -> Result<serde_json::Value, String> {
    let p = PathBuf::from(&path);
    if inline.unwrap_or(false) {
        let image = read_path_as_data_url_with_metadata(&p)?;
        return Ok(serde_json::json!({
            "path": path,
            "dataUrl": image.data_url,
            "sizeBytes": image.size_bytes,
            "mimeType": image.mime_type
        }));
    }
    let size_bytes = source_file_size(&p)?;
    Ok(serde_json::json!({
        "path": path,
        "sizeBytes": size_bytes,
        "mimeType": image_mime_type(&p)
    }))
}

/// Header-only source probe (ITER-064, F18 step 2; closes BUGS backlog #1).
/// p5 allocates one logical-size ImageData per GIF frame before
/// `numFrames()` exists, so the post-decode guard could only reject an
/// over-limit animation after paying for it.  This reads the logical screen
/// size and walks the frame headers (LZW skipped, no pixel buffers) so
/// `validateSourceMetadata` runs before `loadImage`.  `probed: false` means
/// the format is outside the `image` crate's features (AVIF) — the caller
/// falls back to the post-decode guard.
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ImageProbe {
    width: u32,
    height: u32,
    frames: u32,
    size_bytes: u64,
    mime_type: &'static str,
    probed: bool,
}

fn probe_image_path(p: &PathBuf) -> Result<ImageProbe, String> {
    let size_bytes = source_file_size(p)?;
    let mime_type = image_mime_type(p);
    let reader = image::ImageReader::open(p)
        .map_err(|e| format!("could not open {:?}: {}", p, e))?
        .with_guessed_format()
        .map_err(|e| format!("could not read {:?}: {}", p, e))?;
    match reader.format() {
        Some(image::ImageFormat::Gif) => {
            let file = fs::File::open(p).map_err(|e| format!("could not open {:?}: {}", p, e))?;
            let mut opts = gif::DecodeOptions::new();
            opts.skip_frame_decoding(true);
            let mut decoder = opts
                .read_info(std::io::BufReader::new(file))
                .map_err(|e| format!("not a readable GIF: {}", e))?;
            let (width, height) = (decoder.width() as u32, decoder.height() as u32);
            let mut frames: u32 = 0;
            while decoder
                .next_frame_info()
                .map_err(|e| format!("not a readable GIF: {}", e))?
                .is_some()
            {
                frames += 1;
            }
            Ok(ImageProbe {
                width,
                height,
                frames: frames.max(1),
                size_bytes,
                mime_type: "image/gif",
                probed: true,
            })
        }
        Some(format) if format.reading_enabled() => {
            let (width, height) = reader
                .into_dimensions()
                .map_err(|e| format!("not a readable image: {}", e))?;
            Ok(ImageProbe {
                width,
                height,
                frames: 1,
                size_bytes,
                mime_type,
                probed: true,
            })
        }
        _ => Ok(ImageProbe {
            width: 0,
            height: 0,
            frames: 0,
            size_bytes,
            mime_type,
            probed: false,
        }),
    }
}

#[tauri::command]
async fn probe_image_file(path: String) -> Result<ImageProbe, String> {
    probe_image_path(&PathBuf::from(&path))
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

fn source_file_size(p: &PathBuf) -> Result<u64, String> {
    let size_bytes = fs::metadata(p)
        .map_err(|e| format!("could not inspect {:?}: {}", p, e))?
        .len();
    if size_bytes > MAX_SOURCE_FILE_BYTES {
        return Err(format!(
            "image file is too large: {} bytes exceeds import limit {} bytes",
            size_bytes, MAX_SOURCE_FILE_BYTES
        ));
    }
    Ok(size_bytes)
}

fn read_path_as_data_url_with_metadata(p: &PathBuf) -> Result<ImageDataUrl, String> {
    let size_bytes = source_file_size(p)?;
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
            probe_image_file,
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
        begin_export_capture(job_id.clone(), 2, None, None).unwrap();
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
        begin_export_capture(job_id.clone(), 150, None, None).unwrap();
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
        begin_export_capture(job_id.to_string(), frames.len(), Some(config.clone()), None).unwrap();
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
    fn streamed_gif_queue_is_byte_bounded_and_drains() {
        // ITER-070: a non-retaining stream's queued bytes are capped.  With
        // the cap at one frame, every push past the first waits for the
        // feeder; the GIF must still be byte-identical and the counter must
        // drain to zero.  Admission rules checked directly too.
        assert_eq!(stream_queue_admit(0, 10, 10, true), Ok(StreamAdmit::Send));
        assert_eq!(stream_queue_admit(5, 6, 10, true), Ok(StreamAdmit::Wait));
        assert!(stream_queue_admit(5, 6, 10, false).unwrap_err().contains("stopped"));
        assert!(stream_queue_admit(0, 11, 10, true).unwrap_err().contains("queue limit"));
        assert!(stream_queue_admit(u64::MAX, 1, 10, true).is_ok());

        let _guard = STATE_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let dir = stream_test_dir("gif-stream-queue");
        let frames = gif_test_frames();
        let expected =
            encode_gif_gifski_adaptive(&frames, 40, None, None, None, &test_control(JOB_RUNNING))
                .unwrap();
        let cap = frames.iter().map(|f| f.bytes.len() as u64).max().unwrap();
        let job_id = "rust-gif-stream-queue";
        let path = dir.join("queued.gif");
        let config = GifStreamConfig {
            path: path.display().to_string(),
            delay_ms: 40,
            cap_width: None,
            border: None,
            target_max_bytes: None,
        };
        begin_export_capture(job_id.to_string(), frames.len(), Some(config.clone()), None).unwrap();
        let queued = {
            let mut captures = EXPORT_CAPTURES.get().unwrap().lock().unwrap();
            let stream = captures.get_mut(job_id).unwrap().stream.as_mut().unwrap();
            stream.queue_caps = (cap, cap);
            stream.queued.clone()
        };
        for (index, frame) in frames.into_iter().enumerate() {
            stage_export_frame(job_id, index, frame).unwrap();
            assert!(queued.load(Ordering::Acquire) <= cap, "queue over cap at frame {index}");
        }
        let stream = take_export_capture(job_id).ok().unwrap().stream.unwrap();
        let control = ExportControl {
            state: stream.state.clone(),
            app: None,
            job_id: None,
        };
        finish_gif_stream(stream, config, &control).unwrap();
        assert_eq!(queued.load(Ordering::Acquire), 0);
        assert_eq!(fs::read(&path).unwrap(), expected);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn duplicate_out_path_normalises_relative_spellings() {
        // ITER-070: `a.gif` and `./a.gif` (and `x/../a.gif`) are one file.
        let jobs: Vec<serde_json::Value> = ["a.gif", "./a.gif"]
            .iter()
            .map(|o| serde_json::json!({ "out": o }))
            .collect();
        assert_eq!(duplicate_out_path(&jobs).as_deref(), Some("./a.gif"));
        let jobs: Vec<serde_json::Value> = ["out/b.gif", "out/x/../b.gif", "out/c.gif"]
            .iter()
            .map(|o| serde_json::json!({ "out": o }))
            .collect();
        assert_eq!(duplicate_out_path(&jobs).as_deref(), Some("out/x/../b.gif"));
        let cwd = std::env::current_dir().unwrap();
        let abs = cwd.join("d.gif").display().to_string();
        let jobs = vec![serde_json::json!({ "out": "d.gif" }), serde_json::json!({ "out": abs })];
        assert!(duplicate_out_path(&jobs).is_some());
    }

    #[test]
    fn batch_children_dir_is_private_and_removed_on_drop() {
        // ITER-070: fresh 0700 dir, 0600 create_new files, gone after Drop
        // (with any still-running child killed and reaped).
        use std::os::unix::fs::PermissionsExt;
        let mut guard = BatchChildren::new().unwrap();
        let dir = guard.dir.clone();
        assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        let file = dir.join("part-0.json");
        write_private_file(&file, b"{}").unwrap();
        assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(write_private_file(&file, b"{}").is_err(), "create_new must refuse an existing file");
        let child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        guard.children.push(Some(child));
        drop(guard);
        assert!(!dir.exists());
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .unwrap()
            .success();
        assert!(!alive, "child {pid} must be killed by the guard");
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
        begin_export_capture(job_id.clone(), 40, Some(config), None).unwrap();
        for (index, frame) in gif_test_frames().into_iter().take(10).enumerate() {
            stage_export_frame(&job_id, index, frame).unwrap();
        }
        // ITER-071: the encoder opens its .part only after the first frame
        // arrives; wait for it, or a cancel racing that open sees 0 parts
        // here and 1 a moment later (flaked ~1 in 13 under parallel tests).
        let opened = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while part_files(&dir) == 0 && std::time::Instant::now() < opened {
            std::thread::sleep(std::time::Duration::from_millis(5));
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
            begin_export_capture(early.clone(), 1, None, None).unwrap_err(),
            "export cancelled"
        );

        let ids: Vec<_> = (0..MAX_CONCURRENT_CAPTURES)
            .map(|index| format!("rust-capture-limit-{index}"))
            .collect();
        for id in &ids {
            begin_export_capture(id.clone(), 1, None, None).unwrap();
        }
        let overflow = begin_export_capture("rust-capture-overflow".into(), 1, None, None).unwrap_err();
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
        assert_eq!(run_headless_batch(manifest, false, None, false, 1), 2);
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
        // ITER-070: the CLI flag shares the parser (case-insensitive).
        assert_eq!(parse_mp4_quality_arg("ARCHIVE").unwrap(), "archive");
        assert_eq!(parse_mp4_quality_arg(" Upload ").unwrap(), "upload");
        assert!(parse_mp4_quality_arg("lossless").is_err());
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

#[cfg(test)]
mod preset_schema_tests {
    use super::*;

    fn job_with(preset: Option<PathBuf>, overrides: ConfigOverrides) -> HeadlessRenderJob {
        HeadlessRenderJob {
            in_path: PathBuf::from("/tmp/in.png"),
            out_path: PathBuf::from("/tmp/out.gif"),
            frames: Some(24),
            show_window: false,
            preset_path: preset,
            config_overrides: overrides,
            format: None,
            timeout_secs: None,
            mp4_quality: None,
        }
    }

    fn temp_preset(tag: &str, body: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "ggs-schema-test-{}-{}.json",
            std::process::id(),
            tag
        ));
        fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn embedded_schema_compiles_and_catalog_is_its_projection() {
        let cat: serde_json::Value = serde_json::from_str(&catalog_json()).unwrap();
        for key in [
            "palettes",
            "color_modes",
            "ramps",
            "dithers",
            "selection_modes",
            "glyph_sets",
            "sampling_strategies",
            "postprocess_stages",
        ] {
            assert!(
                cat[key].as_array().map(|a| !a.is_empty()).unwrap_or(false),
                "catalog {} empty",
                key
            );
        }
        let p = &preset_schema()["properties"];
        assert_eq!(cat["palettes"], p["palette"]["enum"]);
        assert_eq!(cat["dithers"], p["dither"]["properties"]["mode"]["enum"]);
        assert!(cat["glyph_sets"].as_array().unwrap().contains(&"null".into()));
        assert!(cat["selection_modes"].as_array().unwrap().contains(&"flow".into()));
        assert!(cat["postprocess_stages"].as_array().unwrap().contains(&"vignette".into()));
    }

    #[test]
    fn schema_rejects_bad_enums_with_json_paths_and_warns_on_unknown_keys() {
        let errs = validate_config(&serde_json::json!({
            "palette": "cream",
            "dither": { "mode": "STBN" },
            "grid": { "cols": "wide" }
        }))
        .unwrap_err();
        assert!(errs.iter().any(|e| e.starts_with("/palette:")), "{:?}", errs);
        assert!(errs.iter().any(|e| e.starts_with("/dither/mode:")), "{:?}", errs);
        assert!(errs.iter().any(|e| e.starts_with("/grid/cols:")), "{:?}", errs);

        let warns = validate_config(&serde_json::json!({
            "$schema": "x", "version": 1, "palete": "spice", "glyphSet": null
        }))
        .unwrap();
        assert_eq!(warns.len(), 1, "{:?}", warns);
        assert!(warns[0].starts_with("/palete:"));
    }

    #[test]
    fn manifest_schema_rejects_typos_and_unknown_versions() {
        let ok = serde_json::json!({"$schema": "x", "version": 1, "in": "/a.png", "frames": "auto",
            "perf": false, "jobs": [{"name": "a", "out": "/a.gif", "format": "gif", "frames": 3,
            "capWidth": 720, "targetMaxBytes": 1, "border": {"width": 8}, "config": {"palette": "spice"}}]});
        assert!(validate_manifest(&ok).is_empty(), "{:?}", validate_manifest(&ok));
        let typo = serde_json::json!({"in": "/a.png", "jobs": [{"out": "/a.gif", "capwidth": 720}]});
        let errs = validate_manifest(&typo);
        assert!(errs.len() == 1 && errs[0].starts_with("/jobs/0:") && errs[0].contains("capwidth"), "{:?}", errs);
        let v2 = serde_json::json!({"version": 2, "in": "/a.png", "jobs": []});
        assert!(validate_manifest(&v2).iter().any(|e| e.starts_with("/version:")));
        let frames = serde_json::json!({"in": "/a.png", "frames": 0, "jobs": [{"out": "/a.gif", "format": "webm"}]});
        assert_eq!(validate_manifest(&frames).len(), 2, "{:?}", validate_manifest(&frames));
        assert!(!validate_manifest(&serde_json::json!({"jobs": []})).is_empty(), "`in` is required");
    }

    #[test]
    fn preset_errors_are_explicit_not_silent_defaults() {
        let missing = job_with(Some(PathBuf::from("/nonexistent/ggs.json")), Default::default());
        assert!(missing.build_js_config().unwrap_err().contains("cannot read --preset"));

        let bad_json = temp_preset("badjson", "{ not json");
        let err = job_with(Some(bad_json.clone()), Default::default()).build_js_config().unwrap_err();
        assert!(err.contains("not valid JSON"), "{}", err);

        let not_obj = temp_preset("array", "[1,2]");
        let err = job_with(Some(not_obj.clone()), Default::default()).build_js_config().unwrap_err();
        assert!(err.contains("must be a JSON object"), "{}", err);

        let bad_enum = temp_preset("enum", r#"{"palette":"cream"}"#);
        let err = job_with(Some(bad_enum.clone()), Default::default()).build_js_config().unwrap_err();
        assert!(err.contains("config/palette:"), "{}", err);

        let bad_flag = job_with(
            None,
            ConfigOverrides { postprocess: vec!["glow".into()], ..Default::default() },
        );
        assert!(bad_flag.build_js_config().unwrap_err().contains("not a postprocess stage"));

        let ok = temp_preset("meta", r#"{"$schema":"x","version":1,"palette":"spice"}"#);
        let (cfg, warns) = job_with(Some(ok.clone()), Default::default()).build_js_config().unwrap();
        assert!(warns.is_empty());
        assert!(cfg.get("$schema").is_none() && cfg.get("version").is_none());
        assert_eq!(cfg["palette"], "spice");
        for p in [bad_json, not_obj, bad_enum, ok] {
            let _ = fs::remove_file(p);
        }
    }

    #[test]
    fn deep_merge_layers_objects_and_replaces_everything_else() {
        let mut t = serde_json::json!({
            "grid": { "cols": 240, "rows": 120 },
            "postprocess": { "vignette": { "enabled": true, "strength": 0.55 } },
            "glyphSet": null,
            "inks": [1, 2]
        });
        json_deep_merge(
            &mut t,
            serde_json::json!({
                "grid": { "cols": 80 },
                "postprocess": { "vignette": { "enabled": false }, "bloom": { "enabled": true } },
                "glyphSet": { "x": 1 },
                "inks": [3]
            }),
        );
        assert_eq!(t["grid"], serde_json::json!({ "cols": 80, "rows": 120 }));
        assert_eq!(t["postprocess"]["vignette"], serde_json::json!({ "enabled": false, "strength": 0.55 }));
        assert_eq!(t["postprocess"]["bloom"]["enabled"], true);
        assert_eq!(t["glyphSet"], serde_json::json!({ "x": 1 }));
        assert_eq!(t["inks"], serde_json::json!([3]));
    }

    #[test]
    fn flags_and_set_layer_onto_the_preset_instead_of_replacing_blocks() {
        let preset = temp_preset(
            "layer",
            r#"{"grid":{"cols":150,"rows":200},"dither":{"mode":"temporal","asSourcePrefilter":false},
               "postprocess":{"vignette":{"enabled":false,"strength":0.4},"bloom":{"enabled":true,"radius":3}}}"#,
        );
        let job = job_with(
            Some(preset.clone()),
            ConfigOverrides {
                cols: Some(80),
                dither_mode: Some("stbn".into()),
                postprocess: vec!["vignette".into()],
                set: vec![
                    "grid.cols=99".into(),
                    "palette=spice".into(),
                    "seed=7".into(),
                    "animation.fps=25".into(),
                    "glyphSet=null".into(),
                ],
                ..Default::default()
            },
        );
        let (cfg, _) = job.build_js_config().unwrap();
        assert_eq!(cfg["grid"], serde_json::json!({ "cols": 99, "rows": 200 }), "--set wins, rows survive");
        assert_eq!(cfg["dither"], serde_json::json!({ "mode": "stbn", "asSourcePrefilter": false }));
        assert_eq!(cfg["postprocess"]["vignette"], serde_json::json!({ "enabled": true, "strength": 0.4 }));
        assert_eq!(cfg["postprocess"]["bloom"], serde_json::json!({ "enabled": false, "radius": 3 }));
        assert_eq!(cfg["postprocess"]["kawaii"]["enabled"], false, "unlisted stages disabled");
        assert_eq!(cfg["palette"], "spice");
        assert_eq!(cfg["seed"], 7);
        assert_eq!(cfg["animation"]["fps"], 25);
        assert!(cfg["glyphSet"].is_null());
        let _ = fs::remove_file(preset);
    }

    #[test]
    fn set_override_parsing() {
        assert_eq!(parse_set_override("a.b=1").unwrap(), serde_json::json!({ "a": { "b": 1 } }));
        assert_eq!(parse_set_override("p=spice").unwrap(), serde_json::json!({ "p": "spice" }));
        assert_eq!(parse_set_override("p=\"1\"").unwrap(), serde_json::json!({ "p": "1" }));
        assert_eq!(parse_set_override("p=a=b").unwrap(), serde_json::json!({ "p": "a=b" }));
        assert_eq!(
            parse_set_override(r#"postprocess.bloom={"enabled":true}"#).unwrap(),
            serde_json::json!({ "postprocess": { "bloom": { "enabled": true } } })
        );
        assert!(parse_set_override("novalue").is_err());
        assert!(parse_set_override("a..b=1").is_err());
        assert!(parse_set_override("=1").is_err());
        // A --set that breaks the schema is rejected like any other config.
        let job = job_with(None, ConfigOverrides { set: vec!["palette=cream".into()], ..Default::default() });
        assert!(job.build_js_config().unwrap_err().contains("config/palette:"));
    }

    #[test]
    fn manifest_frames_and_border_are_validated_and_normalised() {
        assert_eq!(manifest_frames(None, "m").unwrap(), None);
        assert_eq!(manifest_frames(Some(&serde_json::json!(12)), "m").unwrap(), Some(serde_json::json!(12)));
        assert_eq!(manifest_frames(Some(&serde_json::json!("auto")), "m").unwrap(), Some(serde_json::json!("auto")));
        for bad in [serde_json::json!(0), serde_json::json!(-3), serde_json::json!("lots"), serde_json::json!(2.5)] {
            assert!(manifest_frames(Some(&bad), "m").is_err(), "{}", bad);
        }
        assert!(manifest_border(None, "j").unwrap().is_null());
        let b = manifest_border(Some(&serde_json::json!({ "width": 16 })), "j").unwrap();
        assert_eq!(b, serde_json::json!({ "width": 16, "enabled": true, "style": "solid" }));
        let off = manifest_border(Some(&serde_json::json!({ "width": 16, "enabled": false })), "j").unwrap();
        assert_eq!(off["enabled"], false);
        assert!(manifest_border(Some(&serde_json::json!({ "width": "wide" })), "j").is_err());
        assert!(manifest_border(Some(&serde_json::json!(32)), "j").is_err());
        // The normalised object deserialises into the encoders' BorderConfig.
        let bc: BorderConfig = serde_json::from_value(b).unwrap();
        assert!(bc.enabled && bc.width == 16);
    }
    #[test]
    fn resume_trusts_a_matching_sidecar_and_falls_back_to_gif_frames_le() {
        let dir = std::env::temp_dir().join(format!("ggs-resume-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // A real 1-frame GIF: what gifski leaves of 4 identical frames.
        let gif_path = dir.join("still.gif");
        {
            let mut f = fs::File::create(&gif_path).unwrap();
            let mut enc = gif::Encoder::new(&mut f, 2, 2, &[0, 0, 0, 255, 255, 255]).unwrap();
            enc.write_frame(&gif::Frame::from_indexed_pixels(2, 2, vec![0u8; 4], None)).unwrap();
        }
        let out = gif_path.to_str().unwrap();
        let four = serde_json::json!(4);
        // No sidecar: a valid GIF with 1 <= n <= requested is done...
        assert!(resume_skip_reason(out, "gif", &four, "h").is_some());
        // ...but never an MP4 count mismatch or a 0-request.
        assert!(resume_skip_reason(out, "gif", &serde_json::json!(0), "h").is_none());

        let job = serde_json::json!({"inPath": null, "format": "gif", "frames": 4,
            "border": null, "config": {"palette": "spice", "dither": {"mode": "stbn"}},
            "capWidth": 0, "targetMaxBytes": null});
        let sha = job_config_sha256(&job, "/src.png");
        // Key order never changes the hash.
        let reordered = serde_json::json!({"config": {"dither": {"mode": "stbn"}, "palette": "spice"},
            "targetMaxBytes": null, "capWidth": 0, "border": null, "frames": 4, "format": "gif", "inPath": null});
        assert_eq!(sha, job_config_sha256(&reordered, "/src.png"));
        assert_ne!(sha, job_config_sha256(&job, "/other.png"));

        PENDING_SIDECARS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .insert(gif_path.clone(), (four.clone(), sha.clone()));
        let size = fs::metadata(&gif_path).unwrap().len();
        write_resume_sidecar(&gif_path, size);
        let side: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(sidecar_path(&gif_path)).unwrap()).unwrap();
        assert_eq!(side["version"], 1);
        assert_eq!(side["frames"], 4);
        assert_eq!(side["config_sha256"], sha.as_str());
        assert!(resume_skip_reason(out, "gif", &four, &sha).is_some());
        // With a sidecar it is authoritative: another config, another frame
        // request or a replaced output all re-render.
        assert!(resume_skip_reason(out, "gif", &four, "other").is_none());
        assert!(resume_skip_reason(out, "gif", &serde_json::json!(3), &sha).is_none());
        fs::write(&gif_path, b"GIF89a-not-the-recorded-file").unwrap();
        assert!(resume_skip_reason(out, "gif", &four, &sha).is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod source_probe_tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/sources")
            .join(name)
    }

    #[test]
    fn probe_counts_gif_frames_from_headers() {
        let probe = probe_image_path(&fixture("limit-481-frames.gif")).unwrap();
        assert_eq!((probe.width, probe.height, probe.frames), (1, 1, 481));
        assert_eq!(probe.mime_type, "image/gif");
        assert!(probe.probed);
    }

    #[test]
    fn probe_reads_still_dimensions_without_decoding() {
        let probe = probe_image_path(&fixture("limit-16385-wide.png")).unwrap();
        assert_eq!((probe.width, probe.height, probe.frames), (16385, 1, 1));
        assert!(probe.probed);
        let normal = probe_image_path(&fixture("cream-paper.png")).unwrap();
        assert_eq!((normal.width, normal.height, normal.frames), (720, 480, 1));
        assert_eq!(
            normal.size_bytes,
            fs::metadata(fixture("cream-paper.png")).unwrap().len()
        );
    }

    #[test]
    fn probe_rejects_unreadable_and_flags_unknown_formats() {
        let dir = std::env::temp_dir().join(format!("ggs-probe-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let junk = dir.join("junk.gif");
        fs::write(&junk, b"GIF89a\x01").unwrap();
        assert!(probe_image_path(&junk).is_err());
        let unknown = dir.join("unknown.avif");
        fs::write(&unknown, b"not an image header at all").unwrap();
        assert!(!probe_image_path(&unknown).unwrap().probed);
        assert!(probe_image_path(&dir.join("missing.png")).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;

    /// ITER-071: `--resume` must re-render an MP4 job whose mp4Quality
    /// changed (ITER-050 changes the bytes); GIF hashes ignore the field.
    #[test]
    fn resume_hash_covers_mp4_quality_for_mp4_only() {
        let job = |format: &str, q: Option<&str>| {
            serde_json::json!({
                "inPath": null, "format": format, "frames": 4, "border": null,
                "config": {}, "capWidth": 0, "targetMaxBytes": null, "mp4Quality": q,
            })
        };
        let h = |format: &str, q: Option<&str>| job_config_sha256(&job(format, q), "/src.png");
        assert_ne!(h("mp4", None), h("mp4", Some("archive")));
        assert_eq!(h("mp4", None), h("mp4", Some("upload")));
        assert_eq!(h("mp4", Some("archive")), h("mp4", Some("ARCHIVE")));
        assert_eq!(h("gif", None), h("gif", Some("archive")));
    }
}
