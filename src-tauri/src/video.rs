//! R4 (ITER-077): video sources, headless tier.
//!
//! The studio's only animated input used to be GIF.  A video file
//! (mp4 / mov / m4v / webm) now becomes a FrameSource on the JS side; this
//! module is the deterministic half of it: an `mp4`-crate demuxer plus the
//! `openh264` decoder (already linked for the MP4 encoder) that hands the
//! webview frame N as raw RGBA over IPC, so `render --in clip.mp4` does not
//! depend on a `<video>` element's seek timing.
//!
//! Scope: H.264 (`avc1`) in an MP4 / QuickTime container with closed GOPs.
//! Everything else — HEVC (iPhone), VP8/9 / AV1 (webm), ProRes, open-GOP
//! streams — reports `supported: false` and the JS side takes the WebView
//! `<video>` seek path instead.
//!
//! Decode model: samples are fed in decode order from a sync sample, with
//! `Flush::NoFlush`, and the decoder's outputs arrive in presentation order;
//! the k-th output of the GOP starting at sync sample s is presentation
//! frame s + k (closed GOP: a GOP's presentation ranks are exactly its own
//! decode indices — checked at open).  Sequential requests just continue the
//! cursor; a backwards or far-forward request restarts at the GOP's sync
//! sample.  The last `RING` decoded frames are kept, so a 300-frame clip
//! never materialises (RGBA is 4·w·h per frame).

use mp4::{MediaType, Mp4Reader, TrackType};
use openh264::decoder::{DecodedYUV, Decoder, DecoderConfig, Flush};
use openh264::formats::YUVSource;
use openh264::OpenH264API;
use serde::Serialize;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Decoded frames kept per open video.  The JS adapter keeps its own LRU
/// (priming needs ~12 frames at once); this ring only spares a re-decode
/// when the webview asks slightly backwards.
const RING: usize = 8;

/// `video_open` / probe result.  `supported: false` carries `reason`; the
/// JS side then falls back to the `<video>` element.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VideoInfo {
    pub handle: u64,
    pub supported: bool,
    pub codec: String,
    pub fps: f64,
    pub frames: u32,
    pub width: u32,
    pub height: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Container families we recognise as video (sniffed, extension fallback).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Container {
    IsoBmff, // mp4 / mov / m4v (ftyp)
    Webm,    // Matroska / WebM (EBML)
}

pub fn sniff_container(path: &Path) -> Option<Container> {
    let mut head = [0u8; 12];
    let n = File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    if n >= 8 && &head[4..8] == b"ftyp" {
        // HEIF/AVIF images are ISO-BMFF too — they are images, not video.
        if n >= 12 && matches!(&head[8..12], b"avif" | b"avis" | b"heic" | b"heix" | b"mif1" | b"msf1") {
            return None;
        }
        return Some(Container::IsoBmff);
    }
    if n >= 4 && head[..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        return Some(Container::Webm);
    }
    match path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("mp4") | Some("m4v") | Some("mov") => Some(Container::IsoBmff),
        Some("webm") | Some("mkv") => Some(Container::Webm),
        _ => None,
    }
}

pub fn video_mime_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("mov") => "video/quicktime",
        Some("webm") | Some("mkv") => "video/webm",
        _ => "video/mp4",
    }
}

fn unsupported(codec: &str, reason: String) -> VideoInfo {
    VideoInfo {
        handle: 0,
        supported: false,
        codec: codec.into(),
        fps: 0.0,
        frames: 0,
        width: 0,
        height: 0,
        reason: Some(reason),
    }
}

pub struct VideoDecoder {
    reader: Mp4Reader<BufReader<File>>,
    track_id: u32,
    length_size: usize,
    /// Annex-B SPS + PPS, prepended to every sync sample.
    params: Vec<u8>,
    samples: u32,
    /// 0-based decode index of every sync sample, ascending (gops[0] == 0).
    gops: Vec<u32>,
    decoder: Option<Decoder>,
    next_sample: u32,
    next_out: u32,
    gop_start: u32,
    gop_end: u32,
    ring: VecDeque<(u32, Arc<Vec<u8>>)>,
    pub info: VideoInfo,
}

impl VideoDecoder {
    /// Demux the header, pick the first video track and decode frame 0 (so
    /// "supported" means "openh264 actually produced a picture").  `Err` is
    /// a not-supported reason, never a crash.
    pub fn open(path: &Path) -> Result<Self, VideoInfo> {
        if sniff_container(path) == Some(Container::Webm) {
            return Err(unsupported("webm", "WebM/Matroska container (the Rust decoder reads MP4/MOV)".into()));
        }
        let file = File::open(path).map_err(|e| unsupported("", format!("cannot open: {}", e)))?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        let reader = Mp4Reader::read_header(BufReader::new(file), size)
            .map_err(|e| unsupported("", format!("not a readable MP4/MOV: {}", e)))?;
        let mut ids: Vec<u32> = reader
            .tracks()
            .iter()
            .filter(|(_, t)| matches!(t.track_type(), Ok(TrackType::Video)))
            .map(|(id, _)| *id)
            .collect();
        ids.sort();
        let track_id = *ids.first().ok_or_else(|| unsupported("", "no video track".into()))?;
        let track = &reader.tracks()[&track_id];
        let codec = match track.media_type() {
            Ok(MediaType::H264) => "h264",
            Ok(MediaType::H265) => "hevc",
            Ok(MediaType::VP9) => "vp9",
            Ok(_) => "other",
            Err(_) => "unrecognised (e.g. HEVC hvc1)",
        };
        if codec != "h264" {
            return Err(unsupported(codec, format!("codec {} is not H.264 (the Rust decoder is H.264-only)", codec)));
        }
        let avcc = &track
            .trak
            .mdia
            .minf
            .stbl
            .stsd
            .avc1
            .as_ref()
            .ok_or_else(|| unsupported(codec, "avc1 sample entry missing".into()))?
            .avcc;
        let length_size = avcc.length_size_minus_one as usize + 1;
        if !(1..=4).contains(&length_size) || length_size == 3 {
            return Err(unsupported(codec, format!("unsupported NAL length size {}", length_size)));
        }
        let mut params = Vec::new();
        for nal in avcc.sequence_parameter_sets.iter().chain(avcc.picture_parameter_sets.iter()) {
            params.extend_from_slice(&[0, 0, 0, 1]);
            params.extend_from_slice(&nal.bytes);
        }
        let samples = track.sample_count();
        if samples == 0 {
            return Err(unsupported(codec, "video track has no samples".into()));
        }
        let fps = track_fps(track);
        if !(fps.is_finite() && fps > 0.0) {
            return Err(unsupported(codec, "cannot determine the frame rate".into()));
        }
        let (tw, th) = (track.width() as u32, track.height() as u32);

        let mut dec = VideoDecoder {
            reader,
            track_id,
            length_size,
            params,
            samples,
            gops: Vec::new(),
            decoder: None,
            next_sample: 0,
            next_out: 0,
            gop_start: 0,
            gop_end: 0,
            ring: VecDeque::new(),
            info: VideoInfo {
                handle: 0,
                supported: true,
                codec: codec.into(),
                fps: (fps * 1000.0).round() / 1000.0,
                frames: samples,
                width: tw,
                height: th,
                reason: None,
            },
        };
        dec.index_gops().map_err(|r| unsupported(codec, r))?;
        // Decode frame 0: proves the stream decodes and gives the true
        // (cropped) picture size, which can differ from tkhd.
        dec.info.width = 0;
        dec.info.height = 0;
        if let Err(e) = dec.frame(0) {
            return Err(unsupported(codec, format!("openh264 could not decode frame 0: {}", e)));
        }
        Ok(dec)
    }

    /// Sync-sample table + the closed-GOP check (presentation rank of every
    /// sample must stay inside its own GOP).  Reads sample headers only via
    /// the stbl tables the mp4 crate already parsed.
    fn index_gops(&mut self) -> Result<(), String> {
        let stbl = &self.reader.tracks()[&self.track_id].trak.mdia.minf.stbl;
        let n = self.samples as usize;
        let mut is_sync = vec![stbl.stss.is_none(); n];
        if let Some(stss) = &stbl.stss {
            for &s in &stss.entries {
                if s >= 1 && (s as usize) <= n {
                    is_sync[s as usize - 1] = true;
                }
            }
        }
        if !is_sync[0] {
            return Err("first sample is not a sync sample".into());
        }
        self.gops = (0..n as u32).filter(|&i| is_sync[i as usize]).collect();
        // Presentation time = decode time + composition offset.
        let mut dts = Vec::with_capacity(n);
        let mut t: i64 = 0;
        for e in &stbl.stts.entries {
            for _ in 0..e.sample_count {
                if dts.len() < n {
                    dts.push(t);
                }
                t += e.sample_delta as i64;
            }
        }
        if dts.len() != n {
            return Err("stts does not cover every sample".into());
        }
        let mut pts = dts.clone();
        if let Some(ctts) = &stbl.ctts {
            let mut i = 0usize;
            for e in &ctts.entries {
                for _ in 0..e.sample_count {
                    if i < n {
                        pts[i] += e.sample_offset as i64;
                    }
                    i += 1;
                }
            }
        }
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| (pts[i], i));
        let mut rank = vec![0u32; n];
        for (r, &i) in order.iter().enumerate() {
            rank[i] = r as u32;
        }
        for (g, &s) in self.gops.iter().enumerate() {
            let e = self.gops.get(g + 1).copied().unwrap_or(self.samples);
            if (s..e).any(|i| rank[i as usize] < s || rank[i as usize] >= e) {
                return Err("open-GOP stream (frames reference across keyframes)".into());
            }
        }
        Ok(())
    }

    fn gop_of(&self, p: u32) -> usize {
        match self.gops.binary_search(&p) {
            Ok(g) => g,
            Err(g) => g - 1, // gops[0] == 0, so g >= 1 here
        }
    }

    fn reset(&mut self, g: usize) -> Result<(), String> {
        let api = OpenH264API::from_source();
        let cfg = DecoderConfig::new().flush_after_decode(Flush::NoFlush);
        self.decoder = Some(Decoder::with_api_config(api, cfg).map_err(|e| e.to_string())?);
        self.next_sample = self.gops[g];
        self.next_out = self.gops[g];
        self.gop_start = self.gops[g];
        self.gop_end = self.gops.get(g + 1).copied().unwrap_or(self.samples);
        Ok(())
    }

    fn to_annexb(&self, avcc: &[u8], sync: bool) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(avcc.len() + 64);
        if sync {
            out.extend_from_slice(&self.params);
        }
        let ls = self.length_size;
        let mut i = 0usize;
        while i + ls <= avcc.len() {
            let mut len = 0usize;
            for b in &avcc[i..i + ls] {
                len = (len << 8) | *b as usize;
            }
            i += ls;
            if i + len > avcc.len() {
                return Err("truncated NAL unit".into());
            }
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(&avcc[i..i + len]);
            i += len;
        }
        Ok(out)
    }

    fn emit(&mut self, rgba: Vec<u8>, w: u32, h: u32) -> Result<(), String> {
        if self.info.width == 0 {
            self.info.width = w;
            self.info.height = h;
        } else if (w, h) != (self.info.width, self.info.height) {
            return Err(format!(
                "frame {} is {}x{} (stream started at {}x{}); resolution changes are not supported",
                self.next_out, w, h, self.info.width, self.info.height
            ));
        }
        if self.ring.len() >= RING {
            self.ring.pop_front();
        }
        self.ring.push_back((self.next_out, Arc::new(rgba)));
        self.next_out += 1;
        Ok(())
    }

    /// Presentation frame `p` (0-based) as tightly packed RGBA8.
    pub fn frame(&mut self, p: u32) -> Result<Arc<Vec<u8>>, String> {
        if p >= self.samples {
            return Err(format!("frame {} out of range (0..{})", p, self.samples));
        }
        if let Some((_, f)) = self.ring.iter().find(|(i, _)| *i == p) {
            return Ok(f.clone());
        }
        let g = self.gop_of(p);
        if self.decoder.is_none() || p < self.next_out || self.gops[g] > self.next_out {
            self.reset(g)?;
        }
        while self.next_out <= p {
            if self.next_sample < self.gop_end {
                let sid = self.next_sample + 1; // mp4 sample ids are 1-based
                let sample = self
                    .reader
                    .read_sample(self.track_id, sid)
                    .map_err(|e| format!("read sample {}: {}", sid, e))?
                    .ok_or_else(|| format!("sample {} missing", sid))?;
                let packet = self.to_annexb(&sample.bytes, self.gops.binary_search(&self.next_sample).is_ok())?;
                self.next_sample += 1;
                let dec = self.decoder.as_mut().ok_or("decoder not initialised")?;
                let out = dec
                    .decode(&packet)
                    .map_err(|e| format!("decode sample {}: {}", sid, e))?
                    .map(|y| yuv_rgba(&y));
                if let Some((rgba, w, h)) = out {
                    self.emit(rgba, w, h)?;
                }
            } else {
                // End of this GOP: drain the reorder buffer, then start the
                // next GOP — or stop at the end of the stream.
                let dec = self.decoder.as_mut().ok_or("decoder not initialised")?;
                let rest: Vec<(Vec<u8>, u32, u32)> = dec
                    .flush_remaining()
                    .map_err(|e| format!("flush: {}", e))?
                    .iter()
                    .map(yuv_rgba)
                    .collect();
                for (rgba, w, h) in rest {
                    self.emit(rgba, w, h)?;
                }
                if self.next_out != self.gop_end {
                    return Err(format!(
                        "decoder returned {} of {} frames for the GOP at sample {}",
                        self.next_out.saturating_sub(self.gop_start),
                        self.gop_end - self.gop_start,
                        self.gop_start
                    ));
                }
                if self.gop_end >= self.samples {
                    break;
                }
                // A fresh decoder per GOP: openh264 does not reliably carry on
                // after flush_remaining (dsOutOfMemory on the next IDR in a
                // 15-frame-GOP High-profile clip).  Closed GOPs make it free
                // of state, and every sync sample carries SPS/PPS.
                let next_g = self.gop_of(self.gop_end);
                self.reset(next_g)?;
            }
        }
        self.ring
            .iter()
            .find(|(i, _)| *i == p)
            .map(|(_, f)| f.clone())
            .ok_or_else(|| format!("frame {} was not produced by the decoder", p))
    }
}

/// Average frame rate from the decode-time table: samples / (sum of stts
/// deltas).  mdhd.duration is not used — with B-frames muxers pad it by the
/// composition shift (a 30 fps clip read as 29.27).  VFR clips are treated
/// as CFR at this average (frame index = floor(t * fps)).
fn track_fps(track: &mp4::Mp4Track) -> f64 {
    let stts = &track.trak.mdia.minf.stbl.stts;
    let (mut n, mut ticks) = (0u64, 0u64);
    for e in &stts.entries {
        n += e.sample_count as u64;
        ticks += e.sample_count as u64 * e.sample_delta as u64;
    }
    if n == 0 || ticks == 0 {
        return track.frame_rate();
    }
    n as f64 * track.timescale().max(1) as f64 / ticks as f64
}

fn yuv_rgba(y: &DecodedYUV<'_>) -> (Vec<u8>, u32, u32) {
    let (w, h) = y.dimensions();
    let mut buf = vec![0u8; w * h * 4];
    y.write_rgba8(&mut buf);
    (buf, w as u32, h as u32)
}

/// Header-only probe for `probe_image_file`: frames / fps / dims / codec,
/// no decode.  `None` when the file is not a parseable MP4/MOV (webm, or a
/// container the mp4 crate rejects) — the JS side then validates after the
/// `<video>` element reports its metadata.
pub fn probe_mp4(path: &Path) -> Option<VideoInfo> {
    let file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let reader = Mp4Reader::read_header(BufReader::new(file), size).ok()?;
    let mut ids: Vec<u32> = reader
        .tracks()
        .iter()
        .filter(|(_, t)| matches!(t.track_type(), Ok(TrackType::Video)))
        .map(|(id, _)| *id)
        .collect();
    ids.sort();
    let track = &reader.tracks()[ids.first()?];
    let samples = track.sample_count();
    let fps = track_fps(track);
    let codec = match track.media_type() {
        Ok(MediaType::H264) => "h264",
        Ok(MediaType::H265) => "hevc",
        Ok(MediaType::VP9) => "vp9",
        Ok(_) => "other",
        Err(_) => "unknown",
    };
    Some(VideoInfo {
        handle: 0,
        supported: codec == "h264",
        codec: codec.into(),
        fps: if fps.is_finite() { (fps * 1000.0).round() / 1000.0 } else { 0.0 },
        frames: samples,
        width: track.width() as u32,
        height: track.height() as u32,
        reason: None,
    })
}

/* ---- the one open video (per process; a batch swaps it per source) ---- */

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

fn open_slot() -> &'static Mutex<Option<VideoDecoder>> {
    static SLOT: OnceLock<Mutex<Option<VideoDecoder>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Open (and replace) the process's video.  Never errors for an
/// unsupported stream — that is `supported: false` + `reason`.
pub fn open_video(path: &Path) -> VideoInfo {
    match VideoDecoder::open(path) {
        Ok(mut dec) => {
            dec.info.handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
            let info = dec.info.clone();
            *open_slot().lock().unwrap_or_else(|e| e.into_inner()) = Some(dec);
            info
        }
        Err(info) => info,
    }
}

pub fn frame_of(handle: u64, index: u32) -> Result<(Arc<Vec<u8>>, u32, u32), String> {
    let mut slot = open_slot().lock().unwrap_or_else(|e| e.into_inner());
    let dec = slot
        .as_mut()
        .filter(|d| d.info.handle == handle)
        .ok_or_else(|| format!("video handle {} is not open (a newer video_open replaced it)", handle))?;
    let f = dec.frame(index)?;
    Ok((f, dec.info.width, dec.info.height))
}

#[tauri::command]
pub async fn video_open(path: String) -> Result<VideoInfo, String> {
    let p = std::path::PathBuf::from(&path);
    crate::source_file_size(&p)?;
    Ok(open_video(&p))
}

/// Frame `index` of the open video as raw RGBA (w·h·4 bytes) in the IPC
/// response body — the mirror of `push_export_frame_raw`: no base64, no PNG.
#[tauri::command]
pub async fn video_frame(handle: u64, index: u32) -> Result<tauri::ipc::Response, String> {
    let (f, _, _) = frame_of(handle, index)?;
    Ok(tauri::ipc::Response::new(f.as_ref().clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/sources").join(name)
    }

    /// Centroid x of bright pixels (the moving box) in an RGBA frame.
    fn box_x(rgba: &[u8], w: usize) -> f64 {
        let (mut sx, mut n) = (0.0, 0.0);
        for (i, px) in rgba.chunks_exact(4).enumerate() {
            if px[0] > 160 && px[1] > 160 {
                sx += (i % w) as f64;
                n += 1.0;
            }
        }
        if n > 0.0 { sx / n } else { -1.0 }
    }

    #[test]
    fn tiny_mp4_opens_with_exact_metadata() {
        let p = fixture("tiny.mp4");
        let info = probe_mp4(&p).expect("probe");
        assert_eq!((info.codec.as_str(), info.frames, info.width, info.height), ("h264", 12, 160, 90));
        assert!((info.fps - 25.0).abs() < 1e-6, "{}", info.fps);
        let dec = VideoDecoder::open(&p).unwrap_or_else(|i| panic!("{:?}", i.reason));
        assert_eq!((dec.info.width, dec.info.height, dec.info.frames), (160, 90, 12));
        assert_eq!(sniff_container(&p), Some(Container::IsoBmff));
    }

    #[test]
    fn frames_are_deterministic_and_random_access_matches_sequential() {
        let p = fixture("tiny.mp4");
        let mut a = VideoDecoder::open(&p).unwrap_or_else(|i| panic!("{:?}", i.reason));
        let seq: Vec<Arc<Vec<u8>>> = (0..12).map(|i| a.frame(i).unwrap()).collect();
        assert_eq!(seq[0].len(), 160 * 90 * 4);
        // The box moves right 10 px a frame.
        let (x0, x11) = (box_x(&seq[0], 160), box_x(&seq[11], 160));
        assert!(x0 > 0.0 && x11 - x0 > 90.0, "box x {} -> {}", x0, x11);
        assert_ne!(seq[0], seq[11]);
        // Backwards / GOP-crossing / repeated requests on a fresh decoder.
        let mut b = VideoDecoder::open(&p).unwrap();
        for &i in &[11u32, 3, 7, 0, 6, 5, 11, 1] {
            assert!(b.frame(i).unwrap() == seq[i as usize], "frame {} differs on random access", i);
        }
        assert!(b.frame(12).unwrap_err().contains("out of range"));
    }

    /// High profile with B-frames (decode order != presentation order): the
    /// reorder model must hand back the box in presentation order.
    #[test]
    fn b_frame_stream_comes_back_in_presentation_order() {
        let mut base = VideoDecoder::open(&fixture("tiny.mp4")).unwrap();
        let mut bf = VideoDecoder::open(&fixture("tiny-bframes.mp4")).unwrap_or_else(|i| panic!("{:?}", i.reason));
        assert_eq!(bf.info.frames, 12);
        for i in 0..12 {
            let (xa, xb) = (box_x(&base.frame(i).unwrap(), 160), box_x(&bf.frame(i).unwrap(), 160));
            assert!((xa - xb).abs() < 2.0, "frame {}: baseline box x {} vs b-frame {}", i, xa, xb);
        }
        // Random access on the reordered stream agrees with sequential.
        let seq: Vec<_> = (0..12).map(|i| bf.frame(i).unwrap()).collect();
        let mut again = VideoDecoder::open(&fixture("tiny-bframes.mp4")).unwrap();
        for &i in &[10u32, 2, 8, 0] {
            assert!(again.frame(i).unwrap() == seq[i as usize], "b-frame random access {}", i);
        }
    }

    #[test]
    fn non_h264_and_non_mp4_report_unsupported() {
        let dir = std::env::temp_dir().join(format!("ggs-video-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let webm = dir.join("x.webm");
        std::fs::write(&webm, [0x1A, 0x45, 0xDF, 0xA3, 0, 0, 0, 0]).unwrap();
        assert_eq!(sniff_container(&webm), Some(Container::Webm));
        let info = open_video(&webm);
        assert!(!info.supported && info.reason.as_deref().unwrap().contains("WebM"), "{:?}", info);
        let junk = dir.join("junk.mp4");
        std::fs::write(&junk, b"\0\0\0\x10ftypisomjunk").unwrap();
        let j = open_video(&junk);
        assert!(!j.supported && j.reason.as_deref().unwrap().contains("MP4"), "{:?}", j);
        let png = fixture("cream-paper.png");
        assert_eq!(sniff_container(&png), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn handles_go_stale_when_a_new_video_opens() {
        let p = fixture("tiny.mp4");
        let a = open_video(&p);
        let b = open_video(&p);
        assert!(a.supported && b.supported && a.handle != b.handle);
        assert!(frame_of(a.handle, 0).unwrap_err().contains("not open"));
        let (f, w, h) = frame_of(b.handle, 4).unwrap();
        assert_eq!((f.len(), w, h), (160 * 90 * 4, 160, 90));
    }
}
