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
use std::io::{BufReader, Read, Seek, SeekFrom};
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
/* ITER-081: the mp4 crate allocates a sample's (stsz) size and a metadata
   box's (header) size before it reads a byte, so both are capped here. */
pub const MAX_SAMPLE_BYTES: u32 = 64 << 20;
pub const MAX_MOOV_BYTES: u64 = 64 << 20;
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

/// ITER-080: true if a NAL holds 00 00 00 or 00 00 01.  Emulation
/// prevention keeps both out of valid H.264, but openh264's Annex-B
/// splitter would cut a second NAL at an embedded 00 00 01 — one
/// to_annexb's type check never saw.  ITER-081: the whole NAL is scanned;
/// skipping the header byte let `00 00 01 67 ...` through as a "type 0" NAL.
fn has_start_code(nal: &[u8]) -> bool {
    nal.windows(3).any(|w| w[0] == 0 && w[1] == 0 && w[2] <= 1)
}

/// ITER-081: why a NAL must not reach openh264, if it must not.  Besides a
/// start code: a header with forbidden_zero_bit set or nal_unit_type 0
/// (header 0x00 is both kinds of wrong), and a trailing zero byte (the
/// rbsp stop bit keeps it out of valid H.264; openh264 would fold it into
/// the next start code).
fn nal_defect(nal: &[u8]) -> Option<&'static str> {
    if has_start_code(nal) {
        return Some("NAL payload contains a start code");
    }
    match nal.first() {
        None => Some("empty NAL unit"),
        Some(&h) if h & 0x80 != 0 || h & 0x1F == 0 => Some("NAL header is invalid (forbidden bit or type 0)"),
        _ if nal.last() == Some(&0) => Some("NAL unit ends in a zero byte"),
        _ => None,
    }
}

/// Both picture sizes an SPS declares.  openh264 allocates `coded`
/// (whole macroblocks) and outputs `cropped`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpsSize {
    pub coded: (u32, u32),
    pub cropped: (u32, u32),
}

/// Cropped picture size from an SPS NAL unit (header byte included), per
/// H.264 7.3.2.1.1 — the size openh264 will output.  `None` = unparseable.
pub fn sps_dimensions(nal: &[u8]) -> Option<(u32, u32)> {
    sps_size(nal).ok().map(|s| s.cropped)
}

/// Coded + cropped size from an SPS NAL unit.  ITER-080: the crop must be
/// under one macroblock on every edge (what encoders emit to reach a
/// non-multiple-of-16 size); a larger one would let a huge coded picture
/// pass check_limits on a small cropped size.
pub fn sps_size(nal: &[u8]) -> Result<SpsSize, String> {
    if nal.first().map(|h| h & 0x1F) != Some(7) {
        return Err("not an SPS".into());
    }
    sps_parse(nal)
        .ok_or_else(|| "unparseable SPS".to_string())?
        .ok_or_else(|| "SPS crops a macroblock or more off an edge".to_string())
}

/// `None` = unparseable, `Some(None)` = crop of a macroblock or more.
fn sps_parse(nal: &[u8]) -> Option<Option<SpsSize>> {
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
    let (coded_w, coded_h) = (w_mbs * 16, (2 - frame_mbs_only) * h_map * 16);
    if coded_w > u32::MAX as u64 || coded_h > u32::MAX as u64 {
        return None;
    }
    // ue() saturates at u32::MAX, so these products cannot overflow u64.
    if [cx * cl, cx * cr, cy * ct, cy * cb].iter().any(|&c| c >= 16) {
        return Some(None);
    }
    let (w, h) = (coded_w - cx * (cl + cr), coded_h - cy * (ct + cb));
    if w == 0 || h == 0 {
        return None;
    }
    Some(Some(SpsSize { coded: (coded_w as u32, coded_h as u32), cropped: (w as u32, h as u32) }))
}

/// The picture size every SPS in the avcC declares, checked against the
/// container's own claims (tkhd, avc1 sample entry; 0 = not stated).
/// ITER-080: no SPS/PPS entry may hide a start code (has_start_code).
/// ITER-081: nor carry any other nal_defect, and an SPS entry must be
/// type 7, a PPS entry type 8.
fn stream_dimensions(track: &mp4::Mp4Track) -> Result<SpsSize, String> {
    let avc1 = track
        .trak
        .mdia
        .minf
        .stbl
        .stsd
        .avc1
        .as_ref()
        .ok_or("avc1 sample entry missing")?;
    let avcc = &avc1.avcc;
    for nal in avcc.sequence_parameter_sets.iter().chain(avcc.picture_parameter_sets.iter()) {
        if let Some(e) = nal_defect(&nal.bytes) {
            return Err(e.into());
        }
    }
    if avcc.sequence_parameter_sets.iter().any(|n| n.bytes[0] & 0x1F != 7) {
        return Err("avcC SPS entry is not an SPS (type 7)".into());
    }
    if avcc.picture_parameter_sets.iter().any(|n| n.bytes[0] & 0x1F != 8) {
        return Err("avcC PPS entry is not a PPS (type 8)".into());
    }
    let mut dims: Option<SpsSize> = None;
    for sps in &avcc.sequence_parameter_sets {
        let d = sps_size(&sps.bytes).map_err(|e| format!("{} in avcC", e))?;
        if dims.map_or(false, |prev| prev != d) {
            return Err("avcC SPS entries disagree on the picture size".into());
        }
        dims = Some(d);
    }
    let size = dims.ok_or("avcC carries no SPS")?;
    let (w, h) = size.cropped;
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
    Ok(size)
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

/// One box header in `b` with `room` bytes left in its parent (or the
/// file): (type, header length, total size).  size 1 = 64-bit largesize,
/// size 0 = to the end of the parent.
fn box_header(b: &[u8], room: u64) -> Result<([u8; 4], u64, u64), String> {
    if b.len() < 8 {
        return Err("truncated box header".into());
    }
    let typ: [u8; 4] = b[4..8].try_into().unwrap();
    let name = String::from_utf8_lossy(&typ).into_owned();
    let (hdr, size) = match u32::from_be_bytes(b[0..4].try_into().unwrap()) {
        0 => (8, room),
        1 => {
            let l = b.get(8..16).ok_or("truncated box header")?;
            match u64::from_be_bytes(l.try_into().unwrap()) {
                0 => (16, room),
                l => (16, l),
            }
        }
        s => (8, s as u64),
    };
    if size < hdr {
        return Err(format!("box '{}' is smaller than its header", name));
    }
    if size > room {
        return Err(format!("box '{}' claims {} bytes, only {} remain", name, size, room));
    }
    Ok((typ, hdr, size))
}

/// Children of a container box: each must fit what is left of its parent.
/// Recurses into the containers the mp4 crate descends into (meta: after
/// its version/flags when present; ilst: every item, which holds `data`).
/// `in_ilst`: the children are ilst items; `in_item`: they are an item's.
fn walk_boxes(b: &[u8], depth: u32, in_ilst: bool, in_item: bool) -> Result<(), String> {
    if depth > 16 {
        return Err("boxes nested too deep".into());
    }
    let mut o = 0usize;
    while o < b.len() {
        let rest = &b[o..];
        // QuickTime lets a udta list end in a 32-bit zero.
        if rest.len() < 8 && rest.iter().all(|&x| x == 0) {
            break;
        }
        let (typ, hdr, size) = box_header(rest, rest.len() as u64)?;
        let body = &rest[hdr as usize..size as usize];
        // ITER-082: DataBox::read_box reads 8 fixed bytes past the header,
        // then allocates `start + size - current` unchecked (size 12 wraps).
        if in_item && &typ == b"data" && size < 16 {
            return Err("box data is smaller than its fixed fields".into());
        }
        let skip = match &typ {
            _ if in_ilst => Some(0),
            b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"edts" | b"dinf" | b"udta" | b"mvex" | b"ilst" => Some(0),
            b"meta" => Some(if body.get(0..4) == Some(&[0, 0, 0, 0]) { 4 } else { 0 }),
            b"dref" => Some(8),
            _ => None,
        };
        if let Some(k) = skip {
            walk_boxes(body.get(k..).unwrap_or(&[]), depth + 1, &typ == b"ilst", in_ilst)?;
        }
        o += size as usize;
    }
    Ok(())
}

/// ITER-081: the mp4 crate sizes allocations from box headers before it
/// checks them against anything (a meta child: `vec![0; size]`, size from
/// a 64-bit largesize).  Walk the top-level boxes first: each must fit the
/// file, the moov must fit MAX_MOOV_BYTES, and every box inside the moov
/// must fit its parent — so no header can ask for more than the file holds.
fn check_box_tree<R: Read + Seek>(r: &mut R, len: u64) -> Result<(), String> {
    let io = |e: std::io::Error| format!("cannot read box headers: {}", e);
    let mut off = 0u64;
    while off < len {
        let mut head = [0u8; 16];
        let n = (len - off).min(16) as usize;
        r.seek(SeekFrom::Start(off)).map_err(io)?;
        r.read_exact(&mut head[..n]).map_err(io)?;
        let (typ, hdr, size) = box_header(&head[..n], len - off)?;
        // ITER-082: the crate parses these into structures it then trusts
        // (trun counts summed with expect(), trun sizes fed to vec![0; n],
        // emsg's unchecked size subtraction and unbounded strings).  The
        // decoder reads stbl only, so a fragmented file goes to the WebView.
        if &typ == b"moof" {
            return Err("fragmented MP4 (moof) is not supported".into());
        }
        if &typ == b"emsg" {
            return Err("box emsg is not supported".into());
        }
        if &typ == b"moov" {
            if size > MAX_MOOV_BYTES {
                return Err(format!("moov is {} bytes (limit {})", size, MAX_MOOV_BYTES));
            }
            let mut body = vec![0u8; (size - hdr) as usize];
            r.seek(SeekFrom::Start(off + hdr)).map_err(io)?;
            r.read_exact(&mut body).map_err(io)?;
            walk_boxes(&body, 1, false, false)?;
        }
        off += size;
    }
    Ok(())
}

/// Open `path` and parse its header (check_box_tree first).  Also returns
/// the file length.
fn read_mp4(path: &Path) -> Result<(Mp4Reader<BufReader<File>>, u64), String> {
    let file = File::open(path).map_err(|e| format!("cannot open: {}", e))?;
    let len = file.metadata().map_err(|e| format!("cannot open: {}", e))?.len();
    let mut r = BufReader::new(file);
    let bad = |e: String| format!("not a readable MP4/MOV: {}", e);
    check_box_tree(&mut r, len).map_err(bad)?;
    r.seek(SeekFrom::Start(0)).map_err(|e| bad(e.to_string()))?;
    let reader = Mp4Reader::read_header(r, len).map_err(|e| bad(e.to_string()))?;
    Ok((reader, len))
}

/// ITER-081: the stsz / stsc entries read_sample trusts.  The crate
/// allocates `vec![0; sample_size]` before read_exact (one 0xFFFFFFFF entry
/// = a 4 GiB zeroed allocation, an abort if it fails), divides by
/// samples_per_chunk (0 = a panic, even in release) and subtracts 1 from
/// first_chunk (0 underflows).
fn sample_tables_fit(track: &mp4::Mp4Track, file_len: u64) -> Result<(), String> {
    let stbl = &track.trak.mdia.minf.stbl;
    let stsz = &stbl.stsz;
    let big = |s: u32| s > MAX_SAMPLE_BYTES || s as u64 > file_len;
    if big(stsz.sample_size) || stsz.sample_sizes.iter().any(|&s| big(s)) {
        return Err(format!("stsz sample size exceeds the file or {} bytes", MAX_SAMPLE_BYTES));
    }
    if !stsc_fits(stbl.stsc.entries.iter().map(|e| (e.first_chunk, e.samples_per_chunk))) {
        return Err("stsc has a zero or decreasing chunk entry (corrupt or crafted sample tables)".into());
    }
    Ok(())
}

/// (first_chunk, samples_per_chunk): both >= 1, first_chunk non-decreasing.
fn stsc_fits(entries: impl Iterator<Item = (u32, u32)>) -> bool {
    let mut prev = 1u32;
    for (first, per) in entries {
        if first == 0 || per == 0 || first < prev {
            return false;
        }
        prev = first;
    }
    true
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
    /// Coded (macroblock) size of the avcC SPS; an in-band SPS must match.
    coded: (u32, u32),
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
        let (reader, file_len) = read_mp4(path).map_err(|r| unsupported("", r))?;
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
        // ITER-080: the limits apply to the coded size (what openh264
        // allocates), not the cropped one it outputs.
        let size = stream_dimensions(track).map_err(|r| rejected(codec, r))?;
        let (sw, sh) = size.cropped;
        // ITER-082: sample_count() sums trafs with expect() (check_box_tree
        // already refuses moof; this keeps the crate's fragment path shut).
        if !track.trafs.is_empty() {
            return Err(unsupported(codec, "fragmented MP4 (moof) is not supported".into()));
        }
        check_limits(size.coded.0, size.coded.1, track.sample_count()).map_err(|r| rejected(codec, r))?;
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
        sample_tables_fit(track, file_len).map_err(|r| unsupported(codec, r))?;
        let fps = track_fps(track);
        if !(fps.is_finite() && fps > 0.0) {
            return Err(unsupported(codec, "cannot determine the frame rate".into()));
        }

        let mut dec = VideoDecoder {
            reader,
            track_id,
            length_size,
            params,
            coded: size.coded,
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

    /// Length-prefixed sample -> Annex-B.  ITER-079: an in-band SPS
    /// (type 7) must declare the admitted size.  openh264 honours the
    /// newest SPS it sees and reallocates for it, so one that grew the
    /// picture would bypass check_limits, which only saw the avcC.  Such
    /// NALs are checked, not dropped: encoders that repeat headers
    /// (x264 --repeat-headers, broadcast muxes) may carry SPS/PPS that
    /// differ from the avcC in ids or VUI, and those streams keep decoding.
    /// Subset SPS (type 15, SVC/MVC) cannot be parsed here and is refused.
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
            let nal = &avcc[i..i + len];
            if let Some(e) = nal_defect(nal) {
                return Err(e.into());
            }
            match nal.first().map(|h| h & 0x1F) {
                Some(7) => {
                    let admitted = (self.info.width, self.info.height);
                    let got = sps_size(nal);
                    if got.as_ref().map(|s| s.cropped) != Ok(admitted) {
                        return Err(format!(
                            "in-band SPS changes the stream size ({} vs the admitted {}x{})",
                            got.map_or("unparseable".into(), |s| format!("{}x{}", s.cropped.0, s.cropped.1)),
                            admitted.0,
                            admitted.1
                        ));
                    }
                    let coded = got.map(|s| s.coded).unwrap_or_default();
                    if coded != self.coded {
                        return Err(format!(
                            "in-band SPS changes the coded size ({}x{} vs the admitted {}x{})",
                            coded.0, coded.1, self.coded.0, self.coded.1
                        ));
                    }
                }
                Some(15) => return Err("in-band subset SPS (SVC/MVC) is not supported".into()),
                _ => {}
            }
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
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
                let want = (self.info.width, self.info.height);
                let dec = self.decoder.as_mut().ok_or("decoder not initialised")?;
                let out = dec
                    .decode(&packet)
                    .map_err(|e| format!("decode sample {}: {}", sid, e))?
                    .map(|y| yuv_rgba(&y, want))
                    .transpose()?;
                if let Some(rgba) = out {
                    self.emit(rgba, want.0, want.1, Some(p))?;
                }
            } else {
                // End of this GOP: drain the reorder buffer, then start the
                // next GOP — or stop at the end of the stream.
                let want = (self.info.width, self.info.height);
                let dec = self.decoder.as_mut().ok_or("decoder not initialised")?;
                let rest: Vec<Vec<u8>> = dec
                    .flush_remaining()
                    .map_err(|e| format!("flush: {}", e))?
                    .iter()
                    .map(|y| yuv_rgba(y, want))
                    .collect::<Result<_, _>>()?;
                for rgba in rest {
                    self.emit(rgba, want.0, want.1, Some(p))?;
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

/// RGBA8 of a decoded picture.  ITER-079: the picture must be the
/// admitted `want` size, checked BEFORE the w·h·4 buffer is allocated —
/// emit() used to be the first size check, after the allocation.
fn yuv_rgba(y: &DecodedYUV<'_>, want: (u32, u32)) -> Result<Vec<u8>, String> {
    let (w, h) = y.dimensions();
    if (w as u64, h as u64) != (want.0 as u64, want.1 as u64) {
        return Err(format!(
            "decoded picture is {}x{} (stream admitted at {}x{}); resolution changes are not supported",
            w, h, want.0, want.1
        ));
    }
    let mut buf = vec![0u8; w * h * 4];
    y.write_rgba8(&mut buf);
    Ok(buf)
}

/// Header-only probe for `probe_image_file`: frames / fps / dims / codec,
/// no decode.  `Ok(None)` when the file is not a parseable MP4/MOV (webm,
/// or a container the mp4 crate rejects) — the JS side then validates
/// after the `<video>` element reports its metadata.  ITER-078: for H.264
/// the dims are the SPS's, and a container that disagrees with them is an
/// `Err` (the probe is what admits the clip past SOURCE_LIMITS).
pub fn probe_mp4(path: &Path) -> Result<Option<VideoInfo>, String> {
    let Ok((reader, _)) = read_mp4(path) else { return Ok(None) };
    let mut ids: Vec<u32> = reader
        .tracks()
        .iter()
        .filter(|(_, t)| matches!(t.track_type(), Ok(TrackType::Video)))
        .map(|(id, _)| *id)
        .collect();
    ids.sort();
    let Some(id) = ids.first() else { return Ok(None) };
    let track = &reader.tracks()[id];
    if !track.trafs.is_empty() {
        return Ok(None); // ITER-082: fragmented; the WebView path handles it
    }
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
        stream_dimensions(track)?.cropped
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

    /// A baseline SPS NAL for a `w_mbs` x `h_mbs` macroblock picture, no
    /// cropping, emulation prevention applied — hand-built for the crafted
    /// in-band fixtures below.
    fn sps_nal(w_mbs: u32, h_mbs: u32) -> Vec<u8> {
        sps_nal_crop(w_mbs, h_mbs, None)
    }

    /// `crop` = frame_crop_{left,right,top,bottom}_offset (4:2:0: x2 px).
    fn sps_nal_cropped(w_mbs: u32, h_mbs: u32, crop: [u32; 4]) -> Vec<u8> {
        sps_nal_crop(w_mbs, h_mbs, Some(crop))
    }

    fn sps_nal_crop(w_mbs: u32, h_mbs: u32, crop: Option<[u32; 4]>) -> Vec<u8> {
        let mut bits: Vec<u8> = Vec::new();
        let mut put = |v: u32, n: u32| (0..n).rev().for_each(|k| bits.push(((v >> k) & 1) as u8));
        let ue = |put: &mut dyn FnMut(u32, u32), v: u32| {
            let n = 32 - (v + 1).leading_zeros();
            put(0, n - 1);
            put(v + 1, n);
        };
        put(66, 8); // profile_idc: Baseline
        put(0, 8); // constraint flags
        put(51, 8); // level_idc
        ue(&mut put, 0); // seq_parameter_set_id
        ue(&mut put, 0); // log2_max_frame_num_minus4
        ue(&mut put, 2); // pic_order_cnt_type
        ue(&mut put, 1); // max_num_ref_frames
        put(0, 1); // gaps_in_frame_num_value_allowed_flag
        ue(&mut put, w_mbs - 1);
        ue(&mut put, h_mbs - 1);
        put(1, 1); // frame_mbs_only_flag
        put(1, 1); // direct_8x8_inference_flag
        put(crop.is_some() as u32, 1); // frame_cropping_flag
        for v in crop.unwrap_or_default() {
            ue(&mut put, v);
        }
        put(0, 1); // vui_parameters_present_flag
        put(1, 1); // rbsp_stop_one_bit
        while bits.len() % 8 != 0 {
            bits.push(0);
        }
        let mut nal = vec![0x67];
        let mut zeros = 0;
        for byte in bits.chunks(8).map(|c| c.iter().fold(0u8, |a, b| (a << 1) | b)) {
            if zeros >= 2 && byte <= 3 {
                nal.push(3);
                zeros = 0;
            }
            zeros = if byte == 0 { zeros + 1 } else { 0 };
            nal.push(byte);
        }
        nal
    }

    /// A copy of `name` with `nal` (length-prefixed) inserted at the start
    /// of sample `k` (0-based): the stsz entry and the mdat size grow.  The
    /// fixtures hold every sample in one chunk, so the chunk offset does not
    /// move (a moov after the mdat moves, but nothing points into it).
    fn with_inband(name: &str, k: usize, nal: &[u8], tag: &str) -> PathBuf {
        let mut b = std::fs::read(fixture(name)).unwrap();
        let be = |b: &[u8], o: usize| u32::from_be_bytes(b[o..o + 4].try_into().unwrap());
        let stco = b.windows(4).rposition(|w| w == b"stco").unwrap() - 4;
        assert_eq!(be(&b, stco + 12), 1, "{}: one chunk", name);
        let chunk = be(&b, stco + 16) as usize;
        let stsz = b.windows(4).rposition(|w| w == b"stsz").unwrap() - 4;
        let at = chunk + (0..k).map(|i| be(&b, stsz + 20 + 4 * i) as usize).sum::<usize>();
        let mdat = b.windows(4).rposition(|w| w == b"mdat").unwrap() - 4;
        assert!(mdat < at, "{}: sample inside mdat", name);
        let grow = 4 + nal.len() as u32;
        let e = stsz + 20 + 4 * k;
        let v = be(&b, e) + grow;
        b[e..e + 4].copy_from_slice(&v.to_be_bytes());
        let v = be(&b, mdat) + grow;
        b[mdat..mdat + 4].copy_from_slice(&v.to_be_bytes());
        let mut ins = (nal.len() as u32).to_be_bytes().to_vec(); // length_size 4
        ins.extend_from_slice(nal);
        b.splice(at..at, ins);
        let dir = std::env::temp_dir().join(format!("ggs-video-inband-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join(format!("{}-{}", tag, name));
        std::fs::write(&out, b).unwrap();
        out
    }

    /// ITER-079: an in-band SPS declaring 8192x8192 used to reach openh264,
    /// which honours the newest SPS; the first size check was emit(), after
    /// yuv_rgba had allocated 8192·8192·4 bytes.  It is now refused in
    /// to_annexb, before the packet is decoded.
    #[test]
    fn an_in_band_sps_cannot_grow_the_picture() {
        let big = sps_nal(512, 512);
        assert_eq!(sps_dimensions(&big), Some((8192, 8192)));
        assert_eq!(sps_dimensions(&sps_nal(10, 6)), Some((160, 96)), "the writer agrees with the reader");
        let base = VideoDecoder::open(&fixture("tiny.mp4")).unwrap();
        assert_eq!(base.length_size, 4);

        // First sample: frame 0 is decoded inside open(), so open refuses it.
        let first = with_inband("tiny.mp4", 0, &big, "sps0");
        let reason = VideoDecoder::open(&first).err().and_then(|i| i.reason).unwrap_or_default();
        assert!(reason.contains("in-band SPS changes the stream size (8192x8192 vs the admitted 160x90)"), "{}", reason);

        // A later sample (the last of GOP 0, a non-sync sample): open
        // succeeds, frame() refuses the packet.
        let k = base.gops.get(1).copied().unwrap_or(base.samples) - 1;
        assert!(k >= 2, "gops {:?}", base.gops);
        let later = with_inband("tiny.mp4", k as usize, &big, "sps-late");
        let mut d = VideoDecoder::open(&later).unwrap_or_else(|i| panic!("{:?}", i.reason));
        let e = d.frame(k).unwrap_err();
        assert!(e.contains("in-band SPS changes the stream size"), "{}", e);
        assert!(d.ring.iter().all(|(_, f)| f.len() == 160 * 90 * 4));

        // An unparseable in-band SPS is refused too (openh264 might read it).
        let junk = with_inband("tiny.mp4", 0, &[0x67, 0x42], "spsjunk");
        let reason = VideoDecoder::open(&junk).err().and_then(|i| i.reason).unwrap_or_default();
        assert!(reason.contains("unparseable vs the admitted 160x90"), "{}", reason);
        let svc = with_inband("tiny.mp4", 0, &[0x6F, 0x53, 0x80], "subset");
        let reason = VideoDecoder::open(&svc).err().and_then(|i| i.reason).unwrap_or_default();
        assert!(reason.contains("subset SPS"), "{}", reason);

        // Repeated headers of the admitted size (the avcC's own SPS in-band,
        // as x264 --repeat-headers writes them) still decode identically.
        for name in ["tiny.mp4", "tiny-bframes.mp4"] {
            let mut orig = VideoDecoder::open(&fixture(name)).unwrap();
            let own = {
                let t = orig.reader.tracks()[&orig.track_id].trak.mdia.minf.stbl.stsd.avc1.as_ref().unwrap();
                t.avcc.sequence_parameter_sets[0].bytes.clone()
            };
            let same = with_inband(name, 0, &own, "same");
            let mut d = VideoDecoder::open(&same).unwrap_or_else(|i| panic!("{}: {:?}", name, i.reason));
            for i in 0..12 {
                assert!(d.frame(i).unwrap() == orig.frame(i).unwrap(), "{} frame {}", name, i);
            }
        }
    }

    /// FNV-1a 64 over every frame of `name` in presentation order.
    /// GGS_VIDEO_DUMP=<dir> also writes each frame's RGBA there (md5 it).
    fn frames_fnv(name: &str) -> u64 {
        let mut d = VideoDecoder::open(&fixture(name)).unwrap_or_else(|i| panic!("{}: {:?}", name, i.reason));
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for i in 0..d.samples {
            let f = d.frame(i).unwrap();
            if let Ok(dir) = std::env::var("GGS_VIDEO_DUMP") {
                std::fs::write(std::path::Path::new(&dir).join(format!("{}-{:02}.rgba", name, i)), &f[..]).unwrap();
            }
            for &b in f.iter() {
                h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
            }
        }
        h
    }

    /// ITER-080 (d): the start-code scan and coded-size admission leave the
    /// fixtures' pixels alone (hashes taken before the change; the md5 of
    /// the 24 GGS_VIDEO_DUMP frames matched too).
    #[test]
    fn fixture_frames_are_unchanged_by_the_iter080_checks() {
        assert_eq!(frames_fnv("tiny.mp4"), 0xf056_e448_4f99_41b2);
        assert_eq!(frames_fnv("tiny-bframes.mp4"), 0x62e3_9b83_75ba_698b);
    }

    /// A copy of `name` whose (single) avcC SPS is replaced by `sps`, with
    /// every enclosing box, the stco offset (moov before mdat) and the
    /// tkhd / avc1 dims (zeroed = "not stated") patched to match.
    fn with_avcc_sps(name: &str, sps: &[u8], tag: &str) -> PathBuf {
        with_avcc_nal(name, false, sps, tag)
    }

    /// with_avcc_sps for the SPS (`pps` false) or the (single) PPS entry.
    fn with_avcc_nal(name: &str, pps: bool, sps: &[u8], tag: &str) -> PathBuf {
        let mut b = std::fs::read(fixture(name)).unwrap();
        let be = |b: &[u8], o: usize| u32::from_be_bytes(b[o..o + 4].try_into().unwrap());
        let pos = |b: &[u8], t: &[u8]| b.windows(4).rposition(|w| w == t).unwrap() - 4;
        let avcc = pos(&b, b"avcC");
        assert_eq!(b[avcc + 13], 0xE1, "{}: one SPS", name);
        let mut at = avcc + 14;
        if pps {
            at += 2 + u16::from_be_bytes([b[at], b[at + 1]]) as usize;
            assert_eq!(b[at], 1, "{}: one PPS", name);
            at += 1;
        }
        let old = u16::from_be_bytes([b[at], b[at + 1]]) as usize;
        let delta = sps.len() as i64 - old as i64;
        let grow = |b: &mut Vec<u8>, o: usize| {
            let v = (be(b, o) as i64 + delta) as u32;
            b[o..o + 4].copy_from_slice(&v.to_be_bytes());
        };
        for t in [&b"moov"[..], b"trak", b"mdia", b"minf", b"stbl", b"stsd", b"avc1", b"avcC"] {
            let o = pos(&b, t);
            grow(&mut b, o);
        }
        if pos(&b, b"moov") < pos(&b, b"mdat") {
            let stco = pos(&b, b"stco");
            assert_eq!(be(&b, stco + 12), 1, "{}: one chunk", name);
            grow(&mut b, stco + 16);
        }
        let tkhd = pos(&b, b"tkhd");
        assert_eq!(b[tkhd + 8], 0, "tkhd v0");
        b[tkhd + 84..tkhd + 92].fill(0);
        let avc1 = pos(&b, b"avc1");
        b[avc1 + 32..avc1 + 36].fill(0);
        b[at..at + 2].copy_from_slice(&(sps.len() as u16).to_be_bytes());
        b.splice(at + 2..at + 2 + old, sps.iter().copied());
        let dir = std::env::temp_dir().join(format!("ggs-video-avcc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join(format!("{}-{}", tag, name));
        std::fs::write(&out, b).unwrap();
        out
    }

    fn open_reason(p: &Path) -> String {
        VideoDecoder::open(p).err().and_then(|i| i.reason).unwrap_or_default()
    }

    /// ITER-080 (a, b): a NAL hiding 00 00 01 is refused, in a sample (here
    /// an IDR smuggling an 8192x8192 SPS) and in the avcC.
    #[test]
    fn a_nal_hiding_a_start_code_is_refused() {
        let mut idr = vec![0x65, 0x88, 0x84, 0x00, 0x00, 0x01];
        idr.extend_from_slice(&sps_nal(512, 512));
        let smuggled = with_inband("tiny.mp4", 0, &idr, "idr-sc");
        assert!(open_reason(&smuggled).contains("NAL payload contains a start code"), "{}", open_reason(&smuggled));
        let d = VideoDecoder::open(&fixture("tiny.mp4")).unwrap();
        let mut pkt = (idr.len() as u32).to_be_bytes().to_vec();
        pkt.extend_from_slice(&idr);
        assert_eq!(d.to_annexb(&pkt, false).unwrap_err(), "NAL payload contains a start code");
        // 00 00 00 too; 00 00 03 (emulation prevention) and the header byte are fine.
        assert!(has_start_code(&[0x65, 1, 0, 0, 0, 2]));
        assert!(!has_start_code(&[0x65, 0, 0, 3, 0, 0, 3]));
        assert!(has_start_code(&[0x00, 0, 1]), "ITER-081: the header byte is scanned too");

        // The rewriter is sound: the fixture's own SPS put back decodes the same.
        let mut orig = VideoDecoder::open(&fixture("tiny.mp4")).unwrap();
        let own = orig.reader.tracks()[&orig.track_id].trak.mdia.minf.stbl.stsd.avc1.as_ref().unwrap().avcc.sequence_parameter_sets[0].bytes.clone();
        let mut same = VideoDecoder::open(&with_avcc_sps("tiny.mp4", &own, "avcc-same")).unwrap_or_else(|i| panic!("{:?}", i.reason));
        for i in [0u32, 5, 11] {
            assert!(same.frame(i).unwrap() == orig.frame(i).unwrap(), "frame {}", i);
        }
        let mut sps = sps_nal(10, 6);
        sps.extend_from_slice(&[0x00, 0x00, 0x01, 0x68]);
        let bad = with_avcc_sps("tiny.mp4", &sps, "avcc-sc");
        let r = VideoDecoder::open(&bad).err().unwrap();
        assert!(r.rejected && r.reason.as_deref() == Some("NAL payload contains a start code"), "{:?}", r);
        assert_eq!(probe_mp4(&bad).unwrap_err(), "NAL payload contains a start code");
    }

    /// ITER-080 (c): limits apply to the coded size openh264 allocates; a
    /// crop cannot buy a bigger picture, and crops are under a macroblock.
    #[test]
    fn the_coded_size_is_what_is_admitted() {
        // 16400x16 coded, 7+7 (x2) px cropped -> 16372x16, under the limit.
        let wide = sps_nal_cropped(1025, 1, [7, 7, 0, 0]);
        assert_eq!(sps_size(&wide), Ok(SpsSize { coded: (16400, 16), cropped: (16372, 16) }));
        assert!(check_limits(16372, 16, 12).is_ok(), "the cropped size alone would pass");
        let r = VideoDecoder::open(&with_avcc_sps("tiny.mp4", &wide, "coded-wide")).err().unwrap();
        assert!(r.rejected && r.reason.as_deref().unwrap_or("").contains("16400x16; dimensions exceed"), "{:?}", r);

        // A crop of a macroblock or more: 8192x8192 coded "cropped" to 160x90.
        let huge = sps_nal_cropped(512, 512, [0, 4016, 0, 4051]);
        assert!(sps_dimensions(&huge).is_none());
        assert_eq!(sps_size(&huge).unwrap_err(), "SPS crops a macroblock or more off an edge");
        let r = open_reason(&with_avcc_sps("tiny.mp4", &huge, "crop-huge"));
        assert!(r.contains("crops a macroblock or more"), "{}", r);
        assert!(sps_size(&sps_nal_cropped(10, 6, [0, 0, 0, 8])).is_err(), "16 px off one edge");

        // In-band: same cropped size (160x90) but a different coded size.
        let other = sps_nal_cropped(11, 6, [4, 4, 0, 3]);
        assert_eq!(sps_size(&other), Ok(SpsSize { coded: (176, 96), cropped: (160, 90) }));
        let r = open_reason(&with_inband("tiny.mp4", 0, &other, "coded-inband"));
        assert!(r.contains("in-band SPS changes the coded size (176x96 vs the admitted 160x96)"), "{}", r);
        // The fixtures: 160x96 coded, cropped to 160x90.
        for name in ["tiny.mp4", "tiny-bframes.mp4"] {
            assert_eq!(VideoDecoder::open(&fixture(name)).unwrap().coded, (160, 96), "{}", name);
        }
    }

    /// ITER-079: yuv_rgba checks the picture against the admitted size
    /// before allocating, so emit() is no longer the first guard.
    #[test]
    fn yuv_rgba_refuses_a_picture_of_another_size() {
        let mut d = VideoDecoder::open(&fixture("tiny.mp4")).unwrap();
        let mut dec = Decoder::with_api_config(OpenH264API::from_source(), DecoderConfig::new()).unwrap();
        let mut checked = false;
        for sid in 1..=d.samples {
            let s = d.reader.read_sample(d.track_id, sid).unwrap().unwrap();
            let pkt = d.to_annexb(&s.bytes, sid == 1).unwrap();
            if let Some(y) = dec.decode(&pkt).unwrap() {
                assert_eq!(yuv_rgba(&y, (160, 90)).unwrap().len(), 160 * 90 * 4);
                let e = yuv_rgba(&y, (8192, 8192)).unwrap_err();
                assert!(e.contains("decoded picture is 160x90 (stream admitted at 8192x8192)"), "{}", e);
                assert!(yuv_rgba(&y, (160, 96)).is_err(), "coded size is not the cropped size");
                checked = true;
                break;
            }
        }
        assert!(checked, "openh264 produced no picture");
        d.ring.clear();
    }

    /// ITER-081 (18): the start-code scan covers the header byte, and a NAL
    /// with a bad header or a trailing zero never reaches openh264.
    #[test]
    fn a_nal_with_a_bad_header_is_refused() {
        // 00 00 01 67 <8192x8192 SPS>: before, the scan skipped byte 0.
        let mut sc = vec![0x00, 0x00, 0x01];
        sc.extend_from_slice(&sps_nal(512, 512));
        let r = open_reason(&with_inband("tiny.mp4", 0, &sc, "hdr-sc"));
        assert!(r.contains("NAL payload contains a start code"), "{}", r);
        assert_eq!(nal_defect(&[0x00, 0x42]), Some("NAL header is invalid (forbidden bit or type 0)"));
        assert_eq!(nal_defect(&[0xE5, 0x42]), Some("NAL header is invalid (forbidden bit or type 0)"));
        assert_eq!(nal_defect(&[0x60, 0x42]), Some("NAL header is invalid (forbidden bit or type 0)"));
        assert_eq!(nal_defect(&[0x65, 0x42, 0x00]), Some("NAL unit ends in a zero byte"));
        assert_eq!(nal_defect(&[]), Some("empty NAL unit"));
        assert_eq!(nal_defect(&[0x65, 0x88, 0x00, 0x00, 0x03, 0x01]), None);
        let d = VideoDecoder::open(&fixture("tiny.mp4")).unwrap();
        assert_eq!(d.to_annexb(&[0, 0, 0, 2, 0x65, 0x00], false).unwrap_err(), "NAL unit ends in a zero byte");
        // avcC: the PPS slot holding a type-7 SPS is refused at open.
        let r = VideoDecoder::open(&with_avcc_nal("tiny.mp4", true, &sps_nal(10, 6), "pps-is-sps")).err().unwrap();
        assert_eq!(r.reason.as_deref(), Some("avcC PPS entry is not a PPS (type 8)"), "{:?}", r);
        let mut pps_as_sps = sps_nal(10, 6);
        pps_as_sps[0] = 0x68;
        let r = open_reason(&with_avcc_sps("tiny.mp4", &pps_as_sps, "sps-is-pps"));
        assert_eq!(r, "avcC SPS entry is not an SPS (type 7)");
    }

    /// ITER-081 (19, 20): stsz sizes and stsc chunk entries are checked
    /// before read_sample sees them.
    #[test]
    fn sample_size_and_chunk_tables_are_bounded() {
        // stsz entry 0 (at +20) = 0xFFFFFFFF: was a 4 GiB zeroed allocation.
        for (off, tag) in [(20usize, "stsz-entry"), (12, "stsz-const")] {
            let p = patched("tiny.mp4", b"stsz", move |_| off, u32::MAX, tag);
            let i = VideoDecoder::open(&p).err().unwrap();
            assert!(!i.rejected && i.reason.as_deref().unwrap_or("").contains("stsz sample size exceeds"), "{}: {:?}", tag, i);
        }
        // A sample larger than the file but under the cap.
        let p = patched("tiny.mp4", b"stsz", |_| 20, 1 << 20, "stsz-file");
        assert!(open_reason(&p).contains("stsz sample size exceeds"), "{}", open_reason(&p));
        // stsc samples_per_chunk (+20) = 0 was a division by zero; first_chunk (+16) = 0 an underflow.
        for (off, tag) in [(20usize, "stsc-per"), (16, "stsc-first")] {
            let p = patched("tiny.mp4", b"stsc", move |_| off, 0, tag);
            let r = open_reason(&p);
            assert!(r.contains("stsc has a zero or decreasing chunk entry"), "{}: {}", tag, r);
        }
        assert!(stsc_fits([(1, 12)].into_iter()));
        assert!(stsc_fits([(1, 4), (3, 2), (3, 1)].into_iter()));
        assert!(!stsc_fits([(1, 4), (3, 2), (2, 1)].into_iter()), "decreasing");
        assert!(!stsc_fits([(1, 4), (2, 0)].into_iter()));
    }

    /// A copy of `name` with `bytes` appended to the file, or to the end of
    /// the moov (its size and, for a moov before the mdat, the stco grow).
    fn with_extra(name: &str, in_moov: bool, bytes: &[u8], tag: &str) -> PathBuf {
        let mut b = std::fs::read(fixture(name)).unwrap();
        let be = |b: &[u8], o: usize| u32::from_be_bytes(b[o..o + 4].try_into().unwrap());
        let pos = |b: &[u8], t: &[u8]| b.windows(4).rposition(|w| w == t).unwrap() - 4;
        if in_moov {
            let moov = pos(&b, b"moov");
            let end = moov + be(&b, moov) as usize;
            let grow = |b: &mut Vec<u8>, o: usize| {
                let v = be(b, o) + bytes.len() as u32;
                b[o..o + 4].copy_from_slice(&v.to_be_bytes());
            };
            grow(&mut b, moov);
            if moov < pos(&b, b"mdat") {
                let stco = pos(&b, b"stco");
                assert_eq!(be(&b, stco + 12), 1);
                grow(&mut b, stco + 16);
            }
            b.splice(end..end, bytes.iter().copied());
        } else {
            b.extend_from_slice(bytes);
        }
        let dir = std::env::temp_dir().join(format!("ggs-video-extra-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join(format!("{}-{}", tag, name));
        std::fs::write(&out, b).unwrap();
        out
    }

    /// ITER-081 (21): no box header may claim more than its parent (or the
    /// file) holds, and the moov is capped, before read_header allocates.
    #[test]
    fn box_sizes_are_checked_before_the_header_is_parsed() {
        let mut free40 = vec![0, 0, 0, 1];
        free40.extend_from_slice(b"free");
        free40.extend_from_slice(&(1u64 << 40).to_be_bytes());
        for name in ["tiny.mp4", "tiny-bframes.mp4"] {
            let p = with_extra(name, false, &free40, "free40");
            let r = open_reason(&p);
            assert!(r.contains("box 'free' claims 1099511627776 bytes"), "{}: {}", name, r);
            assert_eq!(probe_mp4(&p).unwrap(), None, "the probe hands it to the WebView");
            // The moov claiming the whole file (it does not start at 0).
            let len = std::fs::metadata(fixture(name)).unwrap().len() as u32;
            let p = patched(name, b"moov", |_| 0, len, "moov-eof");
            assert!(open_reason(&p).contains("box 'moov' claims"), "{}: {}", name, open_reason(&p));
            // A meta child (non-mdir hdlr: the crate's vec![0; size] path) claiming 2^40.
            let mut meta = vec![0, 0, 0, 0];
            meta.extend_from_slice(b"meta");
            meta.extend_from_slice(&[0, 0, 0, 0]);
            meta.extend_from_slice(&[0, 0, 0, 33]);
            meta.extend_from_slice(b"hdlr");
            meta.extend_from_slice(&[0; 8]);
            meta.extend_from_slice(b"xxxx");
            meta.extend_from_slice(&[0; 13]);
            meta.extend_from_slice(&[0, 0, 0, 1]);
            meta.extend_from_slice(b"junk");
            meta.extend_from_slice(&(1u64 << 40).to_be_bytes());
            let n = meta.len() as u32;
            meta[0..4].copy_from_slice(&n.to_be_bytes());
            let r = open_reason(&with_extra(name, true, &meta, "meta40"));
            assert!(r.contains("box 'junk' claims 1099511627776 bytes, only 16 remain"), "{}: {}", name, r);
            // A well-formed extra box in the moov is fine.
            let mut ok = vec![0, 0, 0, 12];
            ok.extend_from_slice(b"free");
            ok.extend_from_slice(&[1, 2, 3, 4]);
            assert!(VideoDecoder::open(&with_extra(name, true, &ok, "moov-free")).is_ok(), "{}", name);
        }
        assert!(box_header(&[0, 0, 0, 4, b'f', b'r', b'e', b'e'], 100).unwrap_err().contains("smaller than its header"));
        assert_eq!(box_header(&[0, 0, 0, 0, b'm', b'd', b'a', b't'], 100), Ok((*b"mdat", 8, 100)));
        let mut cap = vec![0, 0, 0, 1];
        cap.extend_from_slice(b"moov");
        cap.extend_from_slice(&(MAX_MOOV_BYTES + 16).to_be_bytes());
        let e = check_box_tree(&mut std::io::Cursor::new(cap), MAX_MOOV_BYTES + 16).unwrap_err();
        assert!(e.contains("moov is 67108880 bytes (limit 67108864)"), "{}", e);
    }

    /// A box: 32-bit size, type, body.
    fn mp4_box(typ: &[u8], body: &[u8]) -> Vec<u8> {
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(typ);
        b.extend_from_slice(body);
        b
    }

    /// ITER-082 (22-24): top-level moof / emsg and an ilst `data` box too
    /// small for its fixed fields are refused before read_header parses them.
    #[test]
    fn fragment_emsg_and_short_data_boxes_are_refused() {
        // moof/traf/trun with one sample of size 0xFFFFFFFF (the crate's
        // vec![0; sample_size] path) — refused at the box walk, no allocation.
        let mut trun = vec![0, 0, 0x02, 0x00, 0, 0, 0, 1];
        trun.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        let traf = [mp4_box(b"tfhd", &[0, 0, 0, 0, 0, 0, 0, 1]), mp4_box(b"trun", &trun)].concat();
        let moof = mp4_box(b"moof", &[mp4_box(b"mfhd", &[0, 0, 0, 0, 0, 0, 0, 1]), mp4_box(b"traf", &traf)].concat());
        // emsg v0 whose NUL-terminated strings run past its declared 16 bytes.
        let mut emsg = mp4_box(b"emsg", &[0, 0, 0, 0, b'a', b'b', b'c', b'd']);
        emsg.extend_from_slice(b"efghijkl");
        // meta(mdir)/ilst/(c)nam/data with data size 12 (< 16 fixed bytes).
        let mut hdlr = vec![0; 8];
        hdlr.extend_from_slice(b"mdir");
        hdlr.extend_from_slice(&[0; 13]);
        let meta_with = |data: &[u8]| {
            let ilst = mp4_box(b"ilst", &mp4_box(&[0xA9, b'n', b'a', b'm'], data));
            mp4_box(b"meta", &[&[0, 0, 0, 0][..], &mp4_box(b"hdlr", &hdlr), &ilst].concat())
        };
        let short = meta_with(&[0, 0, 0, 12, b'd', b'a', b't', b'a', 0, 0, 0, 1]);
        let good = meta_with(&mp4_box(b"data", &[0, 0, 0, 1, 0, 0, 0, 0, b'a', b'b', b'c']));
        for name in ["tiny.mp4", "tiny-bframes.mp4"] {
            let p = with_extra(name, false, &moof, "moof");
            let r = open_reason(&p);
            assert!(r.contains("fragmented MP4 (moof) is not supported"), "{}: {}", name, r);
            assert_eq!(probe_mp4(&p).unwrap(), None, "the probe hands it to the WebView");
            let p = with_extra(name, false, &emsg, "emsg");
            let r = open_reason(&p);
            assert!(r.contains("box emsg is not supported"), "{}: {}", name, r);
            assert_eq!(probe_mp4(&p).unwrap(), None);
            let r = open_reason(&with_extra(name, true, &short, "data12"));
            assert!(r.contains("box data is smaller than its fixed fields"), "{}: {}", name, r);
            assert!(VideoDecoder::open(&with_extra(name, true, &good, "data-ok")).is_ok(), "{}", name);
        }
    }
}
