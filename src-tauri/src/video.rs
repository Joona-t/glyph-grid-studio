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

/* ITER-078: the SOURCE_LIMITS of index.html, enforced here too, before
   the first frame is decoded — a header that lies (tkhd / avc1 entry vs
   the SPS the decoder will actually honour) cannot buy a bigger
   allocation than the probe admitted. */
pub const MAX_DIMENSION: u32 = 16384;
pub const MAX_FRAMES: u32 = 480;
pub const MAX_DECODED_PIXELS: u64 = 100_000_000;
/// GlyphGrid.video.CACHE: decoded frames resident at once (JS LRU).
pub const RESIDENT_FRAMES: u64 = 16;

pub fn check_limits(w: u32, h: u32, frames: u32) -> Result<(), String> {
    if w == 0 || h == 0 {
        return Err("video has invalid dimensions".into());
    }
    if w > MAX_DIMENSION || h > MAX_DIMENSION {
        return Err(format!("video is {}x{}; dimensions exceed {} px", w, h, MAX_DIMENSION));
    }
    if frames > MAX_FRAMES {
        return Err(format!("video has {} frames (limit {})", frames, MAX_FRAMES));
    }
    let px = w as u64 * h as u64 * (frames as u64).clamp(1, RESIDENT_FRAMES);
    if px > MAX_DECODED_PIXELS {
        return Err(format!(
            "decoded video would exceed the {}-pixel safety limit ({}x{})",
            MAX_DECODED_PIXELS, w, h
        ));
    }
    Ok(())
}

/// Exp-Golomb reader over an SPS RBSP (emulation-prevention bytes removed).
struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Option<u32> {
        let byte = *self.b.get(self.pos / 8)?;
        let v = (byte >> (7 - (self.pos % 8))) & 1;
        self.pos += 1;
        Some(v as u32)
    }
    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Some(v)
    }
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0u32;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some(((1u64 << zeros) - 1 + self.bits(zeros)? as u64).min(u32::MAX as u64) as u32)
    }
    fn se(&mut self) -> Option<i64> {
        let k = self.ue()? as i64;
        Some(if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) })
    }
}

/// Cropped picture size from an SPS NAL unit (header byte included), per
/// H.264 7.3.2.1.1 — the size openh264 will output.  `None` = unparseable.
pub fn sps_dimensions(nal: &[u8]) -> Option<(u32, u32)> {
    if nal.first().map(|h| h & 0x1F) != Some(7) {
        return None;
    }
    let mut rbsp = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in &nal[1..] {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        rbsp.push(b);
    }
    let mut r = Bits { b: &rbsp, pos: 0 };
    let profile = r.bits(8)?;
    r.bits(16)?; // constraint flags + level_idc
    r.ue()?; // seq_parameter_set_id
    let mut chroma = 1u32;
    let mut separate_planes = false;
    if matches!(profile, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135) {
        chroma = r.ue()?;
        if chroma > 3 {
            return None;
        }
        if chroma == 3 {
            separate_planes = r.bit()? == 1;
        }
        r.ue()?; // bit_depth_luma_minus8
        r.ue()?; // bit_depth_chroma_minus8
        r.bit()?; // qpprime_y_zero_transform_bypass_flag
        if r.bit()? == 1 {
            // seq_scaling_matrix_present_flag: skip the lists (7.3.2.1.1.1).
            for i in 0..(if chroma == 3 { 12 } else { 8 }) {
                if r.bit()? == 1 {
                    let size = if i < 6 { 16 } else { 64 };
                    let (mut last, mut next) = (8i64, 8i64);
                    for _ in 0..size {
                        if next != 0 {
                            next = (last + r.se()? + 256).rem_euclid(256);
                        }
                        last = if next == 0 { last } else { next };
                    }
                }
            }
        }
    }
    r.ue()?; // log2_max_frame_num_minus4
    match r.ue()? {
        0 => {
            r.ue()?; // log2_max_pic_order_cnt_lsb_minus4
        }
        1 => {
            r.bit()?;
            r.se()?;
            r.se()?;
            let cycle = r.ue()?;
            if cycle > 255 {
                return None;
            }
            for _ in 0..cycle {
                r.se()?;
            }
        }
        2 => {}
        _ => return None,
    }
    r.ue()?; // max_num_ref_frames
    r.bit()?; // gaps_in_frame_num_value_allowed_flag
    let w_mbs = r.ue()? as u64 + 1;
    let h_map = r.ue()? as u64 + 1;
    let frame_mbs_only = r.bit()? as u64;
    if frame_mbs_only == 0 {
        r.bit()?; // mb_adaptive_frame_field_flag
    }
    r.bit()?; // direct_8x8_inference_flag
    let (mut cl, mut cr, mut ct, mut cb) = (0u64, 0u64, 0u64, 0u64);
    if r.bit()? == 1 {
        cl = r.ue()? as u64;
        cr = r.ue()? as u64;
        ct = r.ue()? as u64;
        cb = r.ue()? as u64;
    }
    let array_type = if separate_planes { 0 } else { chroma };
    let (cx, cy) = match array_type {
        0 => (1, 2 - frame_mbs_only),
        1 => (2, 2 * (2 - frame_mbs_only)),
        2 => (2, 2 - frame_mbs_only),
        _ => (1, 2 - frame_mbs_only),
    };
    let w = (w_mbs * 16).checked_sub(cx * (cl + cr))?;
    let h = ((2 - frame_mbs_only) * h_map * 16).checked_sub(cy * (ct + cb))?;
    if w == 0 || h == 0 || w > u32::MAX as u64 || h > u32::MAX as u64 {
        return None;
    }
    Some((w as u32, h as u32))
}

/// The picture size every SPS in the avcC declares, checked against the
/// container's own claims (tkhd, avc1 sample entry; 0 = not stated).
fn stream_dimensions(track: &mp4::Mp4Track) -> Result<(u32, u32), String> {
    let avc1 = track
        .trak
        .mdia
        .minf
        .stbl
        .stsd
        .avc1
        .as_ref()
        .ok_or("avc1 sample entry missing")?;
    let mut dims: Option<(u32, u32)> = None;
    for sps in &avc1.avcc.sequence_parameter_sets {
        let d = sps_dimensions(&sps.bytes).ok_or("unparseable SPS in avcC")?;
        if dims.map_or(false, |prev| prev != d) {
            return Err("avcC SPS entries disagree on the picture size".into());
        }
        dims = Some(d);
    }
    let (w, h) = dims.ok_or("avcC carries no SPS")?;
    let tkhd = &track.trak.tkhd;
    for (what, cw, ch) in [
        ("tkhd", tkhd.width.value() as u32, tkhd.height.value() as u32),
        ("avc1 sample entry", avc1.width as u32, avc1.height as u32),
    ] {
        if (cw, ch) != (0, 0) && (cw, ch) != (w, h) {
            return Err(format!(
                "{} says {}x{} but the H.264 SPS says {}x{}; refusing a header that disagrees with its stream",
                what, cw, ch, w, h
            ));
        }
    }
    Ok((w, h))
}

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
    /// ITER-078: refused for safety (limits, a lying header) — an error
    /// for the caller, not a fall-back to the WebView decoder.
    #[serde(skip)]
    pub rejected: bool,
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
        rejected: false,
    }
}

fn rejected(codec: &str, reason: String) -> VideoInfo {
    VideoInfo { rejected: true, ..unsupported(codec, reason) }
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
        // ITER-078: the SPS (what openh264 will allocate) must agree with
        // the container's dims and fit the source limits — checked before
        // any table is indexed or a frame decoded.
        let (sw, sh) = stream_dimensions(track).map_err(|r| rejected(codec, r))?;
        check_limits(sw, sh, track.sample_count()).map_err(|r| rejected(codec, r))?;
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
                width: sw,
                height: sh,
                reason: None,
                rejected: false,
            },
        };
        dec.index_gops().map_err(|r| unsupported(codec, r))?;
        // Decode frame 0: proves the stream decodes.  emit() holds every
        // output to the SPS size (resolution changes are refused).
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
        let stts: Vec<(u32, u32)> = stbl.stts.entries.iter().map(|e| (e.sample_count, e.sample_delta)).collect();
        let ctts: Option<Vec<(u32, i32)>> = stbl
            .ctts
            .as_ref()
            .map(|c| c.entries.iter().map(|e| (e.sample_count, e.sample_offset)).collect());
        // ITER-078: the mp4 crate sums these counts (from 1) with
        // checked_add and unwraps the overflow inside read_sample — a panic
        // on the IPC worker.  Refuse such a table up front.
        let ctts_counts: Vec<u32> = ctts.iter().flatten().map(|e| e.0).collect();
        if !counts_fit(stts.iter().map(|e| e.0)) || !counts_fit(ctts_counts.into_iter()) {
            return Err("stts/ctts sample counts overflow (corrupt or crafted sample tables)".into());
        }
        let pts = presentation_times(n, &stts, ctts.as_deref())?;
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

}

/// 1 + the sum of `counts` fits a u32 (the mp4 crate's own bookkeeping).
fn counts_fit(mut counts: impl Iterator<Item = u32>) -> bool {
    counts.try_fold(1u32, |t, c| t.checked_add(c)).is_some()
}

/// Presentation time of the first `n` samples = decode time (stts) +
/// composition offset (ctts).  ITER-078: the per-sample loops are bounded
/// by the samples still missing, never by a file-supplied `sample_count`
/// (one stts entry claiming u32::MAX samples used to spin 4 G iterations),
/// and the running time saturates instead of overflowing.
fn presentation_times(n: usize, stts: &[(u32, u32)], ctts: Option<&[(u32, i32)]>) -> Result<Vec<i64>, String> {
    let mut pts = Vec::with_capacity(n);
    let mut t: i64 = 0;
    for &(count, delta) in stts {
        let take = (count as usize).min(n - pts.len());
        for _ in 0..take {
            pts.push(t);
            t = t.saturating_add(delta as i64);
        }
        if pts.len() == n {
            break;
        }
    }
    if pts.len() != n {
        return Err("stts does not cover every sample".into());
    }
    if let Some(ctts) = ctts {
        let mut i = 0usize;
        for &(count, offset) in ctts {
            let take = (count as usize).min(n - i);
            for p in &mut pts[i..i + take] {
                *p = p.saturating_add(offset as i64);
            }
            i += take;
            if i == n {
                break;
            }
        }
    }
    Ok(pts)
}

impl VideoDecoder {
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

    /// Push the next presentation frame into the ring.  ITER-078: `pin`
    /// (the frame a `frame()` call is producing) is never the one evicted,
    /// so an end-of-GOP drain larger than RING cannot drop it before it
    /// is returned.
    fn emit(&mut self, rgba: Vec<u8>, w: u32, h: u32, pin: Option<u32>) -> Result<(), String> {
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
            if let Some(k) = self.ring.iter().position(|(i, _)| Some(*i) != pin) {
                self.ring.remove(k);
            }
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
                    self.emit(rgba, w, h, Some(p))?;
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
                    self.emit(rgba, w, h, Some(p))?;
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
        n = n.saturating_add(e.sample_count as u64);
        ticks = ticks.saturating_add(e.sample_count as u64 * e.sample_delta as u64);
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
/// no decode.  `Ok(None)` when the file is not a parseable MP4/MOV (webm,
/// or a container the mp4 crate rejects) — the JS side then validates
/// after the `<video>` element reports its metadata.  ITER-078: for H.264
/// the dims are the SPS's, and a container that disagrees with them is an
/// `Err` (the probe is what admits the clip past SOURCE_LIMITS).
pub fn probe_mp4(path: &Path) -> Result<Option<VideoInfo>, String> {
    let Some(file) = File::open(path).ok() else { return Ok(None) };
    let Some(size) = file.metadata().ok().map(|m| m.len()) else { return Ok(None) };
    let Ok(reader) = Mp4Reader::read_header(BufReader::new(file), size) else { return Ok(None) };
    let mut ids: Vec<u32> = reader
        .tracks()
        .iter()
        .filter(|(_, t)| matches!(t.track_type(), Ok(TrackType::Video)))
        .map(|(id, _)| *id)
        .collect();
    ids.sort();
    let Some(id) = ids.first() else { return Ok(None) };
    let track = &reader.tracks()[id];
    let samples = track.sample_count();
    let fps = track_fps(track);
    let codec = match track.media_type() {
        Ok(MediaType::H264) => "h264",
        Ok(MediaType::H265) => "hevc",
        Ok(MediaType::VP9) => "vp9",
        Ok(_) => "other",
        Err(_) => "unknown",
    };
    let (width, height) = if codec == "h264" {
        stream_dimensions(track)?
    } else {
        (track.width() as u32, track.height() as u32)
    };
    Ok(Some(VideoInfo {
        handle: 0,
        supported: codec == "h264",
        codec: codec.into(),
        fps: if fps.is_finite() { (fps * 1000.0).round() / 1000.0 } else { 0.0 },
        frames: samples,
        width,
        height,
        reason: None,
        rejected: false,
    }))
}

/* ---- the one open video (per process; a batch swaps it per source) ---- */

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

fn open_slot() -> &'static Mutex<Option<VideoDecoder>> {
    static SLOT: OnceLock<Mutex<Option<VideoDecoder>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Open (and replace) the process's video.  An unsupported stream is
/// `Ok` with `supported: false` + `reason` (the WebView decodes it); a
/// rejected one (limits, a lying header — ITER-078) is `Err`.
///
/// ITER-078: the slot lock is held for the whole open, so opens are
/// serialised — the handle order is the install order, a later open
/// always wins, and `frame_of` never sees a decoder newer than its
/// handle or a handle older than the slot.
pub fn open_video(path: &Path) -> Result<VideoInfo, String> {
    let mut slot = open_slot().lock().unwrap_or_else(|e| e.into_inner());
    match VideoDecoder::open(path) {
        Ok(mut dec) => {
            dec.info.handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
            let info = dec.info.clone();
            *slot = Some(dec);
            Ok(info)
        }
        Err(info) if info.rejected => Err(format!("video rejected: {}", info.reason.unwrap_or_default())),
        Err(info) => Ok(info),
    }
}

/// ITER-078: drop the open decoder if `handle` still owns the slot (the
/// JS adapter's dispose).  `false` = a newer open already replaced it.
pub fn close_video(handle: u64) -> bool {
    let mut slot = open_slot().lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().map_or(false, |d| d.info.handle == handle) {
        *slot = None;
        true
    } else {
        false
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
    open_video(&p)
}

/// Free the decoder behind `handle` (no-op for a stale handle).
#[tauri::command]
pub async fn video_close(handle: u64) -> bool {
    close_video(handle)
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
        let info = probe_mp4(&p).unwrap().expect("probe");
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
        let info = open_video(&webm).unwrap();
        assert!(!info.supported && info.reason.as_deref().unwrap().contains("WebM"), "{:?}", info);
        let junk = dir.join("junk.mp4");
        std::fs::write(&junk, b"\0\0\0\x10ftypisomjunk").unwrap();
        let j = open_video(&junk).unwrap();
        assert!(!j.supported && j.reason.as_deref().unwrap().contains("MP4"), "{:?}", j);
        let png = fixture("cream-paper.png");
        assert_eq!(sniff_container(&png), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A copy of `name` with the 4 bytes at `box_type` + `off` (offset
    /// from the box start) replaced — crafted headers, no binary fixture.
    fn patched(name: &str, box_type: &[u8; 4], off: impl Fn(&[u8]) -> usize, val: u32, tag: &str) -> PathBuf {
        let mut b = std::fs::read(fixture(name)).unwrap();
        // Last match: `avc1` is also an ftyp compatible brand.
        let at = b.windows(4).rposition(|w| w == box_type).expect("box") - 4;
        let o = at + off(&b[at..]);
        b[o..o + 4].copy_from_slice(&val.to_be_bytes());
        let dir = std::env::temp_dir().join(format!("ggs-video-patch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join(format!("{}-{}", tag, name));
        std::fs::write(&out, b).unwrap();
        out
    }

    /// ITER-078: stts/ctts loops are bounded by the samples still missing.
    #[test]
    fn stts_and_ctts_sample_counts_cannot_drive_the_loops() {
        let t0 = std::time::Instant::now();
        let pts = presentation_times(12, &[(u32::MAX, 512)], Some(&[(u32::MAX, 1024)])).unwrap();
        assert_eq!(pts.len(), 12);
        assert_eq!((pts[0], pts[11]), (1024, 11 * 512 + 1024));
        let big = presentation_times(3, &[(2, u32::MAX), (u32::MAX, u32::MAX)], Some(&[(1, i32::MAX), (u32::MAX, i32::MAX)])).unwrap();
        assert!(big.windows(2).all(|w| w[0] <= w[1]), "{:?}", big);
        assert!(presentation_times(12, &[(5, 1)], None).unwrap_err().contains("stts"));
        assert!(presentation_times(0, &[(u32::MAX, 1)], None).unwrap().is_empty());
        // The same through a real file: tiny.mp4 with its stts entry 0
        // claiming u32::MAX samples is refused at open — no 4 G loop, and
        // no panic in the mp4 crate's read_sample (it unwraps the overflow).
        let crafted = patched("tiny.mp4", b"stts", |_| 16, u32::MAX, "stts");
        let reason = VideoDecoder::open(&crafted).err().and_then(|i| i.reason).unwrap_or_default();
        assert!(reason.contains("overflow"), "{}", reason);
        let info = open_video(&crafted).unwrap();
        assert!(!info.supported && !info.rejected, "corrupt tables fall back to the WebView: {:?}", info);
        // A large but valid count (the 12 real samples + slack) still opens.
        let slack = patched("tiny.mp4", b"stts", |_| 16, u32::MAX - 16, "stts-slack");
        let mut d = VideoDecoder::open(&slack).unwrap_or_else(|i| panic!("{:?}", i.reason));
        let mut base = VideoDecoder::open(&fixture("tiny.mp4")).unwrap();
        assert_eq!((d.info.frames, d.info.fps), (12, 25.0));
        assert!(d.frame(11).unwrap() == base.frame(11).unwrap());
        assert!(t0.elapsed().as_secs() < 10, "bounded loops took {:?}", t0.elapsed());
    }

    /// ITER-078: dims come from the SPS (cropping applied) and a container
    /// that disagrees with them is refused before any frame is decoded.
    #[test]
    fn sps_dims_rule_and_a_lying_header_is_rejected() {
        for name in ["tiny.mp4", "tiny-bframes.mp4"] {
            let f = File::open(fixture(name)).unwrap();
            let size = f.metadata().unwrap().len();
            let r = Mp4Reader::read_header(BufReader::new(f), size).unwrap();
            let t = r.tracks().values().next().unwrap();
            let sps = &t.trak.mdia.minf.stbl.stsd.avc1.as_ref().unwrap().avcc.sequence_parameter_sets[0];
            assert_eq!(sps_dimensions(&sps.bytes), Some((160, 90)), "{} (coded 160x96, cropped)", name);
        }
        assert_eq!(sps_dimensions(&[0x68, 0xff]), None, "a PPS is not an SPS");
        assert_eq!(sps_dimensions(&[0x67, 0x42]), None, "truncated");
        // tkhd width (16.16 at +84 in a v0 box, +96 in v1) → 3840.
        let tkhd = patched("tiny.mp4", b"tkhd", |b| if b[8] == 1 { 96 } else { 84 }, 3840 << 16, "tkhd");
        let e = probe_mp4(&tkhd).unwrap_err();
        assert!(e.contains("tkhd says 3840x90") && e.contains("160x90"), "{}", e);
        assert!(open_video(&tkhd).unwrap_err().contains("rejected"));
        assert!(crate::probe_image_path(&tkhd).is_err());
        // avc1 sample entry width (u16 at +32) → 4000.
        let avc1 = patched("tiny.mp4", b"avc1", |_| 32, (4000u32 << 16) | 90, "avc1");
        assert!(probe_mp4(&avc1).unwrap_err().contains("avc1 sample entry says 4000x90"));
        assert!(VideoDecoder::open(&avc1).err().map_or(false, |i| i.rejected));
        // The limits themselves (SOURCE_LIMITS, 16 resident frames).
        assert!(check_limits(1920, 1080, 480).is_ok());
        assert!(check_limits(3840, 2160, 300).unwrap_err().contains("pixel"));
        assert!(check_limits(3840, 2160, 2).is_ok(), "a 2-frame 4K clip holds 2 frames");
        assert!(check_limits(16385, 8, 1).unwrap_err().contains("16384"));
        assert!(check_limits(64, 64, 481).unwrap_err().contains("481"));
        assert!(check_limits(0, 64, 1).is_err());
    }

    /// Tests that open / close the process-wide slot run one at a time.
    fn slot_guard() -> std::sync::MutexGuard<'static, ()> {
        static G: Mutex<()> = Mutex::new(());
        G.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// ITER-078: an end-of-GOP drain larger than RING keeps the frame being
    /// produced.  openh264 releases ~one picture per decode call once its
    /// POC is below the current picture's, so no x264 / VideoToolbox clip
    /// measured drains more than 2 (bframes 16, b-pyramid, 120-frame GOPs);
    /// the drain is driven through emit() directly, as flush_remaining
    /// would hand it over.
    #[test]
    fn a_drain_larger_than_ring_keeps_the_requested_frame() {
        let mut d = VideoDecoder::open(&fixture("tiny.mp4")).unwrap();
        let (w, h) = (d.info.width, d.info.height);
        let px = |v: u8| vec![v; (w * h * 4) as usize];
        for pin in [Some(3u32), None] {
            d.ring.clear();
            d.next_out = 3;
            for k in 0..(RING as u8 + 5) {
                d.emit(px(k), w, h, pin).unwrap();
            }
            assert_eq!(d.ring.len(), RING);
            let kept = d.ring.iter().any(|(i, f)| *i == 3 && f[0] == 0);
            assert_eq!(kept, pin.is_some(), "pin {:?}: ring {:?}", pin, d.ring.iter().map(|e| e.0).collect::<Vec<_>>());
            assert_eq!(d.ring.back().unwrap().0, 3 + RING as u32 + 4, "the newest frame is always kept");
        }
    }

    /// ITER-078: serialised opens — the slot always holds the newest
    /// handle; video_close frees it only for its own handle.
    #[test]
    fn concurrent_opens_leave_the_newest_handle_and_close_is_handle_scoped() {
        let _g = slot_guard();
        let p = fixture("tiny.mp4");
        let handles: Vec<u64> = (0..4)
            .map(|_| {
                let p = p.clone();
                std::thread::spawn(move || open_video(&p).unwrap().handle)
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect();
        let newest = *handles.iter().max().unwrap();
        for &h in &handles {
            assert_eq!(frame_of(h, 1).is_ok(), h == newest, "handle {} of {:?}", h, handles);
        }
        let stale = *handles.iter().min().unwrap();
        assert!(!close_video(stale), "a stale handle never closes the slot");
        assert!(frame_of(newest, 0).is_ok());
        assert!(close_video(newest));
        assert!(frame_of(newest, 0).unwrap_err().contains("not open"));
        assert!(!close_video(newest), "closing twice is a no-op");
    }

    #[test]
    fn handles_go_stale_when_a_new_video_opens() {
        let _g = slot_guard();
        let p = fixture("tiny.mp4");
        let a = open_video(&p).unwrap();
        let b = open_video(&p).unwrap();
        assert!(a.supported && b.supported && a.handle != b.handle);
        assert!(frame_of(a.handle, 0).unwrap_err().contains("not open"));
        let (f, w, h) = frame_of(b.handle, 4).unwrap();
        assert_eq!((f.len(), w, h), (160 * 90 * 4, 160, 90));
    }
}
