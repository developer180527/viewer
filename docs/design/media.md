# Audio and video

Every audio and video file opens at once, whether or not it can play: what it holds,
and a picture of it. Playing comes after, in its own stage.

## 1. Stages

1. **The parser** (done, 2026-11-23): `libfileparser-media`, through FFmpeg. Streams,
   chapters, tags, a poster frame, cover art, a waveform. Shown with the content models
   every app already draws (raster, table), so nothing in the apps changed.
2. **The player**: a view that plays. On Apple, AVFoundation for what it reads (H.264,
   HEVC, ProRes, AAC, ALAC, MP3, FLAC, Opus in MP4, MOV, M4A, CAF, WAV), with
   VideoToolbox decoding and frames handed GPU to GPU (CVPixelBuffer → IOSurface →
   Metal texture) into the canvas; FFmpeg decoding for the rest (MKV, AVI, WebM,
   odd codecs). Chapters are markers on the position rail (long-strip navigation).
3. **More decoders** as the real-file runs find them. (AV1 in software, through dav1d,
   is in since 2026-11-23.)

## 2. The parser

`libfileparser-media` (LGPL-2.1-or-later) has one reader, `media`, in
`Category::Media`. It's behind libfileparser's `media-ffmpeg` feature, off by default
like `docs-mupdf`. `viewer-formats` turns it on for every platform but Windows (where
FFmpeg's configure doesn't run yet), the iPhone and iPad included, unlike MuPDF: FFmpeg
here is LGPL-2.1 and dav1d BSD, linked statically, which LGPL allows because the app's
source is open and users can relink it against a changed FFmpeg (as VLC for iOS does).
A closed-source build would need FFmpeg as dynamic frameworks instead.

**Detection.** FFmpeg's own probe on the header, with the name left out: a score of
76 or more (a signature: `ftyp`, EBML, `RIFF…AVI`, `fLaC`, an MP3 frame) is
`Confidence::Magic`. Demuxers that read what other readers own are excluded: still
images (`image2`, `*_pipe`, `gif`, `apng`, `mjpeg`) and subtitles and lyrics (`srt`,
`ass`, `webvtt`, `lrc`…), and HEIF and AVIF photos (`heif_brand` in `-core`: an
`ftyp` naming `heic`, `mif1`, `avif`…), which use MP4's container and which FFmpeg
claims with full confidence but reads as one tile of the photo's grid: the reader
declines them whatever their name, even when asked by id (2026-11-28; they open with
`libfileparser-imageio` on Apple platforms). Otherwise the extension (MP4, MKV, WebM, MOV, AVI, MP3,
FLAC, WAV, Ogg, Opus, M4A and 50 more). A file opens as media only when it has audio,
or video that isn't just a cover picture.

**What a document holds.**

| Facet | What |
|---|---|
| Frame / Cover / Waveform (primary, raster) | Video: a frame a tenth of the way in (the start of a clip under 4 s), at most 1920 px a side, turned upright when the file says it was recorded rotated. Music: its cover art, else its waveform. |
| Waveform | For video with audio, too. 1600 × 320, the best audio stream decoded to mono at 8 kHz, scaled to its loudest moment. An hour at most. |
| Streams (table) | Kind, codec, details (profile, size, frame rate, pixel format; rate, layout), bitrate, duration, language, title, default. |
| Chapters (table) | Start, end, title. |
| Tags (table) | The file's and each stream's. |

The info panel has the container ("MP4 video", "WebM video", "FLAC audio": named by
demuxer and extension, as FFmpeg reads MP4 and MOV with one demuxer), duration,
bitrate, the main video and audio streams, and title, artist, album and date.

**Reading.** FFmpeg reads through callbacks into Rust (read, seek), so a memory-mapped
file and one read by position work alike and nothing is copied whole. Cancelling goes
through FFmpeg's interrupt callback. Each facet opens the file afresh: FFmpeg's context
isn't shared between threads, and opening costs milliseconds.

**Limits.** The picture's pixels (`Limit::ImagePixels`) and the streams table's rows
are checked at open. FFmpeg's own allocations can only be capped for the whole process
(`av_max_alloc`), so per-document memory limits don't reach inside it.

**The shim** (`shim/media.c`): Rust calls a dozen plain C functions and never sees
FFmpeg's structs, whose layout changes between releases.

## 3. FFmpeg and dav1d

Two git submodules at release tags, shallow: `third_party/ffmpeg` (n9.1) and
`third_party/dav1d` (1.5.5, VideoLAN's AV1 decoder, BSD-2-Clause). `build.rs` builds
dav1d first, with its own meson and ninja build (meson is needed to build the app:
`brew install meson`), as a static library from a generated cross file that carries
the target's compiler and flags from `cc`. Then FFmpeg's configure, out of tree in
OUT_DIR, and `make`. Each configures again only when its arguments change (a stamp
file), and builds incrementally after that. FFmpeg's configuration:

- every demuxer, decoder and parser, plus swscale and swresample;
- no programs, encoders, muxers, network, protocols, devices, filters or hardware
  decoding (the player's business);
- `--disable-autodetect`: no external library but the system's zlib and iconv, and
  dav1d (`--enable-libdav1d`): FFmpeg's own AV1 decoder only drives hardware;
- no `--enable-gpl`: the build is LGPL-2.1-or-later.

Cross builds (iPhone, simulator) take the target's compiler and flags from `cc`, as
MuPDF's do, without cc-rs's optimisation and debug flags (both choose their own: a
debug build's `-O0` made every decode crawl). FFmpeg's host programs (swscale's assembly generator) get an explicit macOS
target: without one, Apple's clang reads `.cargo/config.toml`'s
`IPHONEOS_DEPLOYMENT_TARGET` as a request for iOS, and macOS kills what it builds.

The first build takes a few minutes at `-j2`. The libraries add about 15 MB to a
release app.

## 4. Measured

| | |
|---|---|
| Corpus (`corpus/make-media.sh`, 13 files, 200 KB) | H.264/AAC MP4 with chapters, tags and rotation; HEVC; VP9/Opus WebM; AV1 MKV; ProRes MOV; MJPEG/PCM AVI; MKV with subtitles; MP3 with cover art; FLAC, WAV, Opus, ALAC, AIFF. All open, and every picture decodes. |
| Real files (web-platform-tests, 146) | 145 open as media (the other is a Media Source fragment). 142 give a picture, AV1 Main and High included; of the 3 that don't, 2 are fragments with no samples and 1 is DRM-encrypted. No crash or hang. |
| Conformance and fuzz suites | Pass. |

## 5. Gaps

- **Playback** (stage 2).
- **MIDI and tracker modules** (`.mid`, `.mod`, `.xm`) need libraries FFmpeg doesn't
  include (FluidSynth, libopenmpt).
