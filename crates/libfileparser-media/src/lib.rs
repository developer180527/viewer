//! Audio and video files through FFmpeg (libavformat to read the container, libavcodec
//! to decode), built from source with the `third_party/ffmpeg` submodule: what a file
//! holds, at once, without playing it.
//!
//! - its streams (codec, size and frame rate, sample rate and channels, language),
//!   chapters and tags, as tables, and the main ones in the info panel;
//! - a picture: a frame from a tenth of the way in for video, the cover art a music
//!   file carries, or its waveform, drawn from the decoded audio.
//!
//! Playing is the app's (a player view); this is the parser. FFmpeg is built LGPL-only
//! with no external libraries (build.rs), so this crate is LGPL (docs/design/media.md).
//!
//! Empty on Windows until FFmpeg's Windows build is wired in (build.rs).

#![cfg(not(windows))]

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::Arc;

use libfileparser_core::{
    Cancel, Category, Confidence, Content, Detection, Document, FacetInfo, FileSource, FormatInfo, Limit, Model, OwnedTable, ParseContext,
    ParseError, Raster, Reader, ReaderInfo, Result, Role,
};

mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    pub type ReadFn = extern "C" fn(*mut c_void, *mut u8, c_int) -> c_int;
    pub type SeekFn = extern "C" fn(*mut c_void, i64, c_int) -> i64;
    pub type StopFn = extern "C" fn(*mut c_void) -> c_int;

    #[repr(C)]
    pub struct Media {
        _private: [u8; 0],
    }

    #[repr(C)]
    pub struct Stream {
        pub kind: c_int,
        pub codec: [c_char; 64],
        pub codec_long: [c_char; 160],
        pub profile: [c_char; 64],
        pub pix_fmt: [c_char; 48],
        pub sample_fmt: [c_char; 48],
        pub layout: [c_char; 96],
        pub width: c_int,
        pub height: c_int,
        pub fps_num: c_int,
        pub fps_den: c_int,
        pub sample_rate: c_int,
        pub channels: c_int,
        pub bit_rate: i64,
        pub duration_us: i64,
        pub frames: i64,
        pub attached_pic: c_int,
        pub is_default: c_int,
        pub rotation: c_int,
    }

    unsafe extern "C" {
        pub fn vm_probe(data: *const u8, len: c_int, name: *const c_char, demuxer: *mut c_char, demuxer_len: c_int) -> c_int;
        pub fn vm_open(opaque: *mut c_void, read: ReadFn, seek: SeekFn, stop: StopFn, err: *mut c_char, errlen: c_int) -> *mut Media;
        pub fn vm_close(m: *mut Media);
        pub fn vm_format_name(m: *mut Media) -> *const c_char;
        pub fn vm_format_long_name(m: *mut Media) -> *const c_char;
        pub fn vm_duration_us(m: *mut Media) -> i64;
        pub fn vm_bit_rate(m: *mut Media) -> i64;
        pub fn vm_stream_count(m: *mut Media) -> c_int;
        pub fn vm_stream_info(m: *mut Media, i: c_int, out: *mut Stream) -> c_int;
        pub fn vm_tag_count(m: *mut Media, stream: c_int) -> c_int;
        pub fn vm_tag(m: *mut Media, stream: c_int, index: c_int, key: *mut *const c_char, value: *mut *const c_char) -> c_int;
        pub fn vm_chapter_count(m: *mut Media) -> c_int;
        pub fn vm_chapter(m: *mut Media, i: c_int, start_us: *mut i64, end_us: *mut i64, title: *mut *const c_char) -> c_int;
        pub fn vm_cover(m: *mut Media, data: *mut *const u8, len: *mut c_int) -> c_int;
        pub fn vm_frame(m: *mut Media, at_us: i64, max_side: c_int, max_pixels: i64, rgba: *mut *mut u8, width: *mut c_int, height: *mut c_int) -> c_int;
        pub fn vm_peaks(m: *mut Media, buckets: c_int, out: *mut f32, max_seconds: f64) -> c_int;
        pub fn vm_free(p: *mut c_void);
    }
}

pub fn readers() -> &'static [&'static dyn Reader] {
    &[&MediaReader]
}

pub fn detect(header: &[u8], name: &str) -> Vec<(&'static dyn Reader, Detection)> {
    libfileparser_core::detect(readers(), header, name)
}

pub fn open(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    libfileparser_core::open(readers(), src, cx)
}

static MEDIA: ReaderInfo = ReaderInfo::new("media", "Audio or video", Category::Media).extensions(&[
    // Video.
    "mp4", "m4v", "mov", "qt", "mkv", "webm", "avi", "wmv", "asf", "flv", "f4v", "mpg", "mpeg", "mpe", "m2v", "ts", "m2ts", "mts", "vob", "3gp", "3g2",
    "ogv", "mxf", "dv", "nut", "ivf", "y4m", "rm", "rmvb", "divx",
    // Audio.
    "mp3", "mp2", "m4a", "m4b", "aac", "ac3", "eac3", "dts", "flac", "wav", "w64", "aif", "aiff", "aifc", "caf", "au", "snd", "ogg", "oga", "opus",
    "spx", "wma", "amr", "awb", "ape", "wv", "mpc", "tta", "tak", "ra", "mka", "voc", "gsm", "dsf", "dff",
]);

/// The audio and video containers FFmpeg reads.
pub struct MediaReader;

/// Demuxers that read what other readers own: still images, and subtitles and lyrics
/// (text). A video stream made of one PNG is an image, not a video.
fn not_media(demuxer: &str) -> bool {
    demuxer.ends_with("_pipe")
        || matches!(
            demuxer,
            "image2" | "gif" | "apng" | "mjpeg" | "jpegxl_anim" | "tty" | "srt" | "ass" | "webvtt" | "lrc" | "subviewer" | "subviewer1" | "microdvd" | "sami"
                | "jacosub" | "mpl2" | "pjs" | "realtext" | "stl" | "vplayer" | "scc" | "sup" | "aqtitle" | "mpsub" | "vobsub" | "data" | "bin" | "txt"
                | "ico" | "svg"
        )
}

/// FFmpeg's verdict on these bytes: its confidence (0–100) and the demuxer it would use.
fn probe(header: &[u8], name: &str) -> (i32, String) {
    let name = std::ffi::CString::new(name.replace('\0', "")).unwrap_or_default();
    let mut demuxer = [0 as c_char; 64];
    let len = header.len().min(1 << 20) as c_int;
    let score = unsafe { ffi::vm_probe(header.as_ptr(), len, name.as_ptr(), demuxer.as_mut_ptr(), demuxer.len() as c_int) };
    (score, text(&demuxer))
}

impl Reader for MediaReader {
    fn info(&self) -> &ReaderInfo {
        &MEDIA
    }

    /// By content when FFmpeg is sure (a signature: `ftyp`, EBML, `RIFF…AVI`, an MP3
    /// frame), else by extension.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if header.is_empty() {
            return if MEDIA.matches_extension(name) { Detection::new(Confidence::Extension) } else { Detection::NONE };
        }
        // The name is left out: an extension alone isn't a signature.
        let (score, demuxer) = probe(header, "");
        if score >= 76 && !not_media(&demuxer) {
            Detection::new(Confidence::Magic)
        } else if MEDIA.matches_extension(name) {
            Detection::new(Confidence::Extension)
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let m = Opened::new(src, cx)?;
        if not_media(&m.format_name()) {
            return Err(ParseError::NotThisFormat);
        }
        let streams = m.streams();
        // Something to play: audio, or video that isn't only a cover picture.
        if !streams.iter().any(|s| s.kind == Kind::Audio || (s.kind == Kind::Video && !s.attached_pic)) {
            return Err(ParseError::NotThisFormat);
        }
        cx.limits.check(Limit::Rows, streams.len() as u64)?;
        let duration = m.duration();
        let video = streams.iter().find(|s| s.kind == Kind::Video && !s.attached_pic);
        // The picture this will show: a frame (at most 1920 a side) or the waveform.
        let pixels = match video {
            Some(v) => {
                let k = (1920.0 / v.width.max(v.height).max(1) as f64).min(1.0);
                (v.width as f64 * k).round() as u64 * (v.height as f64 * k).round() as u64
            }
            None => WAVE_W as u64 * WAVE_H as u64,
        };
        cx.limits.check(Limit::ImagePixels, pixels)?;
        let audio = streams.iter().find(|s| s.kind == Kind::Audio);
        let format = container_name(&m.format_name(), &m.format_long_name(), src.name(), video.is_some());
        let mut doc = Document::new(FormatInfo::new("media", format.clone(), Category::Media));

        // The picture: made when shown, as it decodes.
        let (picture, has_cover) = (if video.is_some() { "Frame" } else if m.cover().is_some() { "Cover" } else { "Waveform" }, m.cover().is_some());
        let from = src.clone();
        let is_video = video.is_some();
        doc = doc.with_lazy_facet(FacetInfo::new(Role::Primary, picture, Model::Raster), move |cx| {
            let m = Opened::new(&from, cx)?;
            let raster = if is_video {
                m.poster(cx)?
            } else if has_cover {
                m.cover_raster().map_or_else(|| m.waveform(cx), Ok)?
            } else {
                m.waveform(cx)?
            };
            Ok(Content::Raster(raster))
        });
        if video.is_some() && audio.is_some() {
            let from = src.clone();
            doc = doc.with_lazy_facet(FacetInfo::new(Role::Structure, "Waveform", Model::Raster), move |cx| Ok(Content::Raster(Opened::new(&from, cx)?.waveform(cx)?)));
        }
        doc = doc.with_facet(Role::Structure, "Streams", Content::Table(Arc::new(streams_table(&streams))));
        let chapters = m.chapters();
        if !chapters.is_empty() {
            let rows = chapters.into_iter().map(|(a, b, t)| vec![clock(a), clock(b), t]).collect();
            doc = doc.with_facet(Role::Structure, "Chapters", Content::Table(Arc::new(OwnedTable::text(vec!["Start".into(), "End".into(), "Title".into()], rows))));
        }
        let tags = m.tags();
        if !tags.is_empty() {
            doc = doc.with_facet(Role::Inspector, "Tags", Content::Table(Arc::new(OwnedTable::text(vec!["Of".into(), "Tag".into(), "Value".into()], tags.clone()))));
        }

        doc = doc.with_meta("Container", format);
        if let Some(d) = duration {
            doc = doc.with_meta("Duration", clock(d));
        }
        if let Some(b) = m.bit_rate() {
            doc = doc.with_meta("Bitrate", kbps(b));
        }
        if let Some(v) = video {
            doc = doc.with_meta("Video", v.summary());
        }
        if let Some(a) = audio {
            doc = doc.with_meta("Audio", a.summary());
        }
        for key in ["title", "artist", "album", "date"] {
            if let Some(row) = tags.iter().find(|r| r[0] == "file" && r[1].eq_ignore_ascii_case(key)) {
                let mut name = key.to_string();
                name[..1].make_ascii_uppercase();
                doc = doc.with_meta(name, &row[2]);
            }
        }
        Ok(doc.with_file_facets(src, None))
    }
}

/// What FFmpeg reads from: the file, a position in it, and whether to stop.
struct Reading {
    src: FileSource,
    pos: u64,
    cancel: Cancel,
}

extern "C" fn read(opaque: *mut c_void, buf: *mut u8, size: c_int) -> c_int {
    let r = unsafe { &mut *(opaque as *mut Reading) };
    if size <= 0 {
        return 0;
    }
    let out = unsafe { std::slice::from_raw_parts_mut(buf, size as usize) };
    let n = r.src.read_at(r.pos, out);
    r.pos += n as u64;
    n as c_int
}

extern "C" fn seek(opaque: *mut c_void, offset: i64, whence: c_int) -> i64 {
    const AVSEEK_SIZE: c_int = 0x10000;
    const AVSEEK_FORCE: c_int = 0x20000;
    let r = unsafe { &mut *(opaque as *mut Reading) };
    let len = r.src.len() as i64;
    let to = match whence & !AVSEEK_FORCE {
        AVSEEK_SIZE => return len,
        0 => offset,
        1 => r.pos as i64 + offset,
        2 => len + offset,
        _ => return -1,
    };
    if to < 0 {
        return -1;
    }
    r.pos = to as u64;
    to
}

extern "C" fn stop(opaque: *mut c_void) -> c_int {
    let r = unsafe { &*(opaque as *const Reading) };
    r.cancel.is_cancelled() as c_int
}

/// An opened file. FFmpeg's context isn't shared between threads: each use opens its
/// own (probing again costs milliseconds).
struct Opened {
    m: *mut ffi::Media,
    // Boxed so its address, which FFmpeg holds, doesn't move.
    _reading: Box<Reading>,
}

impl Drop for Opened {
    fn drop(&mut self) {
        unsafe { ffi::vm_close(self.m) }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Video,
    Audio,
    Subtitle,
    Data,
    Attachment,
    Other,
}

#[derive(Clone, Debug)]
struct StreamInfo {
    kind: Kind,
    codec: String,
    codec_long: String,
    profile: String,
    pix_fmt: String,
    layout: String,
    width: i32,
    height: i32,
    fps: Option<f64>,
    sample_rate: i32,
    channels: i32,
    bit_rate: Option<i64>,
    duration: Option<f64>,
    attached_pic: bool,
    is_default: bool,
    rotation: i32,
    language: String,
    title: String,
}

impl StreamInfo {
    /// "h264 (High), 1920 × 1080, 29.97 fps" or "aac, 48 kHz, stereo".
    fn summary(&self) -> String {
        let mut parts = vec![if self.profile.is_empty() { self.codec.clone() } else { format!("{} ({})", self.codec, self.profile) }];
        match self.kind {
            Kind::Video => {
                if self.width > 0 {
                    parts.push(format!("{} × {}", self.width, self.height));
                }
                if let Some(f) = self.fps {
                    parts.push(format!("{} fps", trim(f, 3)));
                }
                if self.rotation != 0 {
                    parts.push(format!("rotated {}°", self.rotation));
                }
            }
            Kind::Audio => {
                if self.sample_rate > 0 {
                    parts.push(format!("{} kHz", trim(self.sample_rate as f64 / 1000.0, 3)));
                }
                if !self.layout.is_empty() {
                    parts.push(self.layout.clone());
                } else if self.channels > 0 {
                    parts.push(format!("{} channels", self.channels));
                }
            }
            _ => {}
        }
        parts.join(", ")
    }
}

impl Opened {
    fn new(src: &FileSource, cx: &ParseContext) -> Result<Self> {
        let mut reading = Box::new(Reading { src: src.clone(), pos: 0, cancel: cx.cancel.clone() });
        let mut err = [0 as c_char; 256];
        let opaque = &mut *reading as *mut Reading as *mut c_void;
        let m = unsafe { ffi::vm_open(opaque, read, seek, stop, err.as_mut_ptr(), err.len() as c_int) };
        if m.is_null() {
            cx.cancel.check()?;
            let why = text(&err);
            return Err(if why.contains("Invalid data") { ParseError::NotThisFormat } else { ParseError::corrupt(format!("FFmpeg: {why}")) });
        }
        Ok(Self { m, _reading: reading })
    }

    fn format_name(&self) -> String {
        cstr(unsafe { ffi::vm_format_name(self.m) })
    }

    fn format_long_name(&self) -> String {
        cstr(unsafe { ffi::vm_format_long_name(self.m) })
    }

    fn duration(&self) -> Option<f64> {
        let us = unsafe { ffi::vm_duration_us(self.m) };
        (us > 0).then(|| us as f64 / 1e6)
    }

    fn bit_rate(&self) -> Option<i64> {
        let b = unsafe { ffi::vm_bit_rate(self.m) };
        (b > 0).then_some(b)
    }

    fn tags_of(&self, stream: c_int) -> Vec<(String, String)> {
        let n = unsafe { ffi::vm_tag_count(self.m, stream) };
        (0..n)
            .filter_map(|i| {
                let (mut k, mut v) = (std::ptr::null(), std::ptr::null());
                (unsafe { ffi::vm_tag(self.m, stream, i, &mut k, &mut v) } == 0).then(|| (cstr(k), cstr(v)))
            })
            .collect()
    }

    fn streams(&self) -> Vec<StreamInfo> {
        let n = unsafe { ffi::vm_stream_count(self.m) };
        (0..n)
            .filter_map(|i| {
                let mut s: ffi::Stream = unsafe { std::mem::zeroed() };
                if unsafe { ffi::vm_stream_info(self.m, i, &mut s) } != 0 {
                    return None;
                }
                let tags = self.tags_of(i);
                let tag = |k: &str| tags.iter().find(|(key, _)| key.eq_ignore_ascii_case(k)).map(|(_, v)| v.clone()).unwrap_or_default();
                Some(StreamInfo {
                    kind: match s.kind {
                        0 => Kind::Video,
                        1 => Kind::Audio,
                        2 => Kind::Subtitle,
                        3 => Kind::Data,
                        4 => Kind::Attachment,
                        _ => Kind::Other,
                    },
                    codec: text(&s.codec),
                    codec_long: text(&s.codec_long),
                    profile: text(&s.profile),
                    pix_fmt: text(&s.pix_fmt),
                    layout: text(&s.layout),
                    width: s.width,
                    height: s.height,
                    fps: (s.fps_num > 0 && s.fps_den > 0).then(|| s.fps_num as f64 / s.fps_den as f64).filter(|f| *f < 1000.0),
                    sample_rate: s.sample_rate,
                    channels: s.channels,
                    bit_rate: (s.bit_rate > 0).then_some(s.bit_rate),
                    duration: (s.duration_us > 0).then(|| s.duration_us as f64 / 1e6),
                    attached_pic: s.attached_pic != 0,
                    is_default: s.is_default != 0,
                    rotation: s.rotation,
                    language: tag("language"),
                    title: tag("title"),
                })
            })
            .collect()
    }

    /// Each tag: (of what: "file" or "stream N", key, value).
    fn tags(&self) -> Vec<Vec<String>> {
        let mut rows: Vec<Vec<String>> = self.tags_of(-1).into_iter().map(|(k, v)| vec!["file".into(), k, v]).collect();
        let n = unsafe { ffi::vm_stream_count(self.m) };
        for i in 0..n {
            rows.extend(self.tags_of(i).into_iter().map(|(k, v)| vec![format!("stream {i}"), k, v]));
        }
        rows
    }

    fn chapters(&self) -> Vec<(f64, f64, String)> {
        let n = unsafe { ffi::vm_chapter_count(self.m) };
        (0..n)
            .filter_map(|i| {
                let (mut a, mut b, mut t) = (0i64, 0i64, std::ptr::null());
                (unsafe { ffi::vm_chapter(self.m, i, &mut a, &mut b, &mut t) } == 0).then(|| (a as f64 / 1e6, b as f64 / 1e6, cstr(t)))
            })
            .collect()
    }

    fn cover(&self) -> Option<&[u8]> {
        let (mut data, mut len) = (std::ptr::null(), 0);
        (unsafe { ffi::vm_cover(self.m, &mut data, &mut len) } == 0 && len > 0).then(|| unsafe { std::slice::from_raw_parts(data, len as usize) })
    }

    /// The cover art, decoded (it's a JPEG or PNG inside the file).
    fn cover_raster(&self) -> Option<Raster> {
        let img = image::load_from_memory(self.cover()?).ok()?.into_rgba8();
        let (w, h) = img.dimensions();
        Raster::new(w, h, img.into_raw()).ok()
    }

    /// A frame a tenth of the way in (the start of a short clip), at most 1920 px a side,
    /// turned upright when the file says it was recorded rotated.
    fn poster(&self, cx: &ParseContext) -> Result<Raster> {
        let at = self.duration().filter(|d| *d > 4.0).map_or(0, |d| (d * 0.1 * 1e6) as i64);
        let (mut px, mut w, mut h) = (std::ptr::null_mut(), 0, 0);
        // The decoder's own pictures are held to the image limit too, whatever the header said.
        let max_pixels = i64::try_from(cx.limits.image_pixels).unwrap_or(i64::MAX).min(i32::MAX as i64);
        let r = unsafe { ffi::vm_frame(self.m, at, 1920, max_pixels, &mut px, &mut w, &mut h) };
        cx.cancel.check()?;
        if r != 0 || px.is_null() {
            return Err(ParseError::corrupt(format!("FFmpeg couldn't decode a frame ({r})")));
        }
        let rgba = unsafe { std::slice::from_raw_parts(px, w as usize * h as usize * 4) }.to_vec();
        unsafe { ffi::vm_free(px as *mut c_void) };
        let rotation = self.streams().into_iter().find(|s| s.kind == Kind::Video && !s.attached_pic).map_or(0, |s| s.rotation);
        let (w, h, rgba) = rotate(w as u32, h as u32, rgba, rotation);
        Raster::new(w, h, rgba)
    }

    /// The audio's peaks drawn as a waveform, 1600 × 320, on a clear background.
    fn waveform(&self, cx: &ParseContext) -> Result<Raster> {
        const W: usize = WAVE_W;
        const H: usize = WAVE_H;
        let mut peaks = vec![0f32; W];
        // An hour at most: a longer file's waveform shows its first hour.
        let reached = unsafe { ffi::vm_peaks(self.m, W as c_int, peaks.as_mut_ptr(), 3600.0) };
        cx.cancel.check()?;
        if reached <= 0 {
            return Err(ParseError::corrupt(format!("FFmpeg couldn't decode the audio ({reached})")));
        }
        // Scaled to the loudest moment, so quiet recordings show their shape too.
        let loudest = peaks.iter().copied().fold(0f32, f32::max);
        if loudest > 0.0 {
            peaks.iter_mut().for_each(|p| *p /= loudest);
        }
        Raster::new(W as u32, H as u32, draw_waveform(&peaks, W, H))
    }
}

/// What people call a container: FFmpeg reads MP4, M4A and MOV with one demuxer
/// ("QuickTime / MOV"), and Matroska and WebM with another, so the extension decides.
fn container_name(demuxer: &str, long: &str, name: &str, video: bool) -> String {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let kind = if video { "video" } else { "audio" };
    let named = match demuxer {
        "mov,mp4,m4a,3gp,3g2,mj2" => match ext.as_str() {
            "mov" | "qt" => "QuickTime movie".to_string(),
            "m4a" | "m4b" => "MPEG-4 audio".to_string(),
            "3gp" | "3g2" => format!("3GPP {kind}"),
            _ => format!("MP4 {kind}"),
        },
        "matroska,webm" => if ext == "webm" { format!("WebM {kind}") } else { format!("Matroska {kind}") },
        "flac" => "FLAC audio".into(),
        "mp3" => "MP3 audio".into(),
        "wav" => "WAV audio".into(),
        "aiff" => "AIFF audio".into(),
        "ogg" => match ext.as_str() {
            "opus" => "Opus audio".into(),
            "ogv" => "Ogg video".into(),
            _ => format!("Ogg {kind}"),
        },
        "avi" => "AVI video".into(),
        "asf" => if video { "Windows Media video".into() } else { "Windows Media audio".into() },
        "flv" => "Flash video".into(),
        "mpegts" => "MPEG transport stream".into(),
        "mpeg" => format!("MPEG {kind}"),
        "caf" => "Core Audio file".into(),
        "aac" => "AAC audio".into(),
        _ => String::new(),
    };
    if !named.is_empty() {
        named
    } else if !long.is_empty() {
        long.to_string()
    } else {
        demuxer.to_string()
    }
}

/// The waveform's size.
const WAVE_W: usize = 1600;
const WAVE_H: usize = 320;

fn streams_table(streams: &[StreamInfo]) -> OwnedTable {
    let columns = ["#", "Kind", "Codec", "Details", "Bitrate", "Duration", "Language", "Title", "Default"];
    let rows = streams
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let kind = match s.kind {
                Kind::Video if s.attached_pic => "cover",
                Kind::Video => "video",
                Kind::Audio => "audio",
                Kind::Subtitle => "subtitles",
                Kind::Data => "data",
                Kind::Attachment => "attachment",
                Kind::Other => "other",
            };
            let details = match s.kind {
                Kind::Video => {
                    let mut d = s.summary();
                    if !s.pix_fmt.is_empty() {
                        d += &format!(", {}", s.pix_fmt);
                    }
                    d
                }
                Kind::Audio => s.summary(),
                _ => s.codec_long.clone(),
            };
            vec![
                i.to_string(),
                kind.into(),
                s.codec.clone(),
                details,
                s.bit_rate.map(kbps).unwrap_or_default(),
                s.duration.map(clock).unwrap_or_default(),
                s.language.clone(),
                s.title.clone(),
                if s.is_default { "yes".into() } else { String::new() },
            ]
        })
        .collect();
    OwnedTable::text(columns.iter().map(|c| c.to_string()).collect(), rows)
}

/// Bars from the middle line, one a column, in the colour of a selection.
fn draw_waveform(peaks: &[f32], w: usize, h: usize) -> Vec<u8> {
    let mut px = vec![0u8; w * h * 4];
    let mid = h as f32 / 2.0;
    for (x, &p) in peaks.iter().enumerate().take(w) {
        let half = (p.clamp(0.0, 1.0) * (mid - 2.0)).max(0.5);
        let (top, bottom) = ((mid - half).floor() as usize, ((mid + half).ceil() as usize).min(h));
        for y in top..bottom {
            let i = (y * w + x) * 4;
            px[i..i + 4].copy_from_slice(&[0x4a, 0x90, 0xd9, 0xff]);
        }
    }
    px
}

/// An RGBA image turned by a quarter, half or three quarters (clockwise degrees).
fn rotate(w: u32, h: u32, rgba: Vec<u8>, degrees: i32) -> (u32, u32, Vec<u8>) {
    // FFmpeg's display matrix is anticlockwise.
    let turn = (((-degrees % 360) + 360) % 360) / 90;
    if turn == 0 {
        return (w, h, rgba);
    }
    let (w, h) = (w as usize, h as usize);
    let (nw, nh) = if turn == 2 { (w, h) } else { (h, w) };
    let mut out = vec![0u8; rgba.len()];
    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = match turn {
                1 => (h - 1 - y, x),
                2 => (w - 1 - x, h - 1 - y),
                _ => (y, w - 1 - x),
            };
            let (s, d) = ((y * w + x) * 4, (ny * nw + nx) * 4);
            out[d..d + 4].copy_from_slice(&rgba[s..s + 4]);
        }
    }
    (nw as u32, nh as u32, out)
}

/// "1:02:03.5", "3:07", "0:04.25".
fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0);
    let (h, m, s) = ((total / 3600.0) as u64, ((total % 3600.0) / 60.0) as u64, total % 60.0);
    let s = if (s - s.round()).abs() < 0.005 { format!("{:02}", s.round() as u64) } else { format!("{:05.2}", s) };
    if h > 0 { format!("{h}:{m:02}:{s}") } else { format!("{m}:{s}") }
}

fn kbps(bits: i64) -> String {
    format!("{} kb/s", (bits as f64 / 1000.0).round() as i64)
}

/// `f` with at most `places` decimals, trailing zeros cut: 29.97, 25, 44.1.
fn trim(f: f64, places: usize) -> String {
    let s = format!("{f:.places$}");
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

fn text(buf: &[c_char]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end].iter().map(|&c| c as u8).collect::<Vec<_>>()).into_owned()
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks_and_rates_read_as_people_write_them() {
        assert_eq!(clock(187.0), "3:07");
        assert_eq!(clock(3723.5), "1:02:03.50");
        assert_eq!(clock(4.25), "0:04.25");
        assert_eq!(trim(29.97002997, 3), "29.97");
        assert_eq!(trim(25.0, 3), "25");
        assert_eq!(kbps(128_000), "128 kb/s");
    }

    #[test]
    fn containers_are_named_as_people_name_them() {
        assert_eq!(container_name("mov,mp4,m4a,3gp,3g2,mj2", "QuickTime / MOV", "a.mp4", true), "MP4 video");
        assert_eq!(container_name("mov,mp4,m4a,3gp,3g2,mj2", "QuickTime / MOV", "a.m4a", false), "MPEG-4 audio");
        assert_eq!(container_name("matroska,webm", "Matroska / WebM", "a.webm", true), "WebM video");
        assert_eq!(container_name("nut", "NUT", "a.nut", true), "NUT");
    }

    #[test]
    fn a_quarter_turn_moves_the_corners_where_they_go() {
        // 2 × 1: red then blue; turned clockwise it's 1 × 2, red on top.
        let (w, h, px) = rotate(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255], -90);
        assert_eq!((w, h), (1, 2));
        assert_eq!(&px[..4], &[255, 0, 0, 255]);
    }
}
