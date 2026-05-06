use base64::engine::general_purpose;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use tauri_plugin_dialog::DialogExt;

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
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .invoke_handler(tauri::generate_handler![
            save_png,
            save_gif_real,
            save_gif_to_path,
            save_blob,
            pick_image,
            read_image_file,
            save_preset_json,
            load_preset_json,
        ])
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
