//! The media reader on the corpus's audio and video (corpus/make-media.sh): what each
//! file holds, its picture, and the conformance and fuzz suites every reader passes.

use libfileparser_core::{Content, FileSource, ParseContext, Reader};
use libfileparser_media::MediaReader;
use libfileparser_testkit::{Sample, assert_conforms};

fn corpus_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files")
}

fn file(name: &str) -> FileSource {
    let p = corpus_dir().join("media").join(name);
    FileSource::from_bytes(name, std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())))
}

fn meta(doc: &libfileparser_core::Document, key: &str) -> String {
    doc.metadata().iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap_or_else(|| panic!("no {key}: {:?}", doc.metadata()))
}

fn table(doc: &libfileparser_core::Document, name: &str, cx: &ParseContext) -> Vec<Vec<String>> {
    let i = doc.facets().position(|f| f.name == name).unwrap_or_else(|| panic!("no {name} facet"));
    let Ok(Content::Table(t)) = doc.facet(i, cx) else { panic!("{name} isn't a table") };
    let n = t.info().rows.known();
    t.rows(0..n).unwrap().into_iter().map(|r| r.into_iter().map(|c| c.to_string()).collect()).collect()
}

fn picture(doc: &libfileparser_core::Document, cx: &ParseContext) -> (u32, u32, Vec<u8>) {
    let Ok(Content::Raster(r)) = doc.facet(0, cx) else { panic!("the first facet isn't a picture") };
    let rgba = r.decode().expect("decodes");
    (r.width(), r.height(), rgba)
}

#[test]
fn a_video_says_its_streams_chapters_and_tags_and_shows_a_frame_upright() {
    let cx = ParseContext::new();
    let doc = MediaReader.open(&file("h264-aac.mp4"), &cx).expect("opens");
    assert_eq!(doc.format().category, libfileparser_core::Category::Media);
    assert!(meta(&doc, "Video").starts_with("h264"), "{}", meta(&doc, "Video"));
    assert!(meta(&doc, "Video").contains("96 × 64") && meta(&doc, "Video").contains("24 fps"));
    assert!(meta(&doc, "Audio").starts_with("aac") && meta(&doc, "Audio").contains("44.1 kHz"));
    assert_eq!(meta(&doc, "Duration"), "0:01");
    assert_eq!(meta(&doc, "Title"), "Test pattern");
    let chapters = table(&doc, "Chapters", &cx);
    assert_eq!(chapters.iter().map(|r| r[2].as_str()).collect::<Vec<_>>(), ["First half", "Second half"]);
    let streams = table(&doc, "Streams", &cx);
    // MP4 keeps its chapters in a text track of their own: a data stream.
    assert_eq!(streams.iter().map(|r| r[1].as_str()).collect::<Vec<_>>(), ["video", "audio", "data"]);
    // Recorded "rotated" 90°: shown upright, so taller than wide.
    let (w, h, rgba) = picture(&doc, &cx);
    assert_eq!((w, h), (64, 96));
    assert!(rgba.chunks(4).any(|p| p[0] > 100), "a real frame, not blank");
    // And the audio's own picture too.
    assert!(doc.facets().any(|f| f.name == "Waveform"));
}

#[test]
fn every_codec_the_samples_use_decodes_a_picture() {
    let cx = ParseContext::new();
    for (name, video) in [
        ("hevc.mp4", "hevc"),
        ("vp9-opus.webm", "vp9"),
        // Through dav1d: FFmpeg's own AV1 decoder only drives hardware.
        ("av1.mkv", "av1"),
        ("prores.mov", "prores"),
        ("mjpeg-pcm.avi", "mjpeg"),
        ("subtitled.mkv", "h264"),
    ] {
        let doc = MediaReader.open(&file(name), &cx).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(meta(&doc, "Video").starts_with(video), "{name}: {}", meta(&doc, "Video"));
        let (w, h, _) = picture(&doc, &cx);
        assert_eq!((w, h), (96, 64), "{name}");
    }
    let doc = MediaReader.open(&file("subtitled.mkv"), &cx).unwrap();
    let streams = table(&doc, "Streams", &cx);
    let subs = streams.iter().find(|r| r[1] == "subtitles").expect("a subtitle stream");
    assert_eq!(subs[6], "fra");
}

#[test]
fn music_shows_its_cover_or_its_waveform() {
    let cx = ParseContext::new();
    let doc = MediaReader.open(&file("cover.mp3"), &cx).expect("opens");
    assert_eq!(meta(&doc, "Artist"), "Viewer corpus");
    assert_eq!(doc.facets().next().unwrap().name, "Cover");
    let (w, h, rgba) = picture(&doc, &cx);
    assert_eq!((w, h), (32, 32));
    assert!(rgba[0] > 200 && rgba[2] < 60, "the orange cover: {:?}", &rgba[..4]);

    for name in ["tone.flac", "tone.wav", "tone.opus", "tone.m4a", "tone.aiff"] {
        let doc = MediaReader.open(&file(name), &cx).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(doc.facets().next().unwrap().name, "Waveform", "{name}");
        let (w, h, rgba) = picture(&doc, &cx);
        assert_eq!((w, h), (1600, 320));
        // A steady tone: bars all the way along.
        let mid = (h as usize / 2) * w as usize * 4;
        let lit = (0..w as usize).filter(|x| rgba[mid + x * 4 + 3] > 0).count();
        assert!(lit > 1500, "{name}: {lit} of 1600 columns drawn");
    }
}

#[test]
fn what_isnt_media_is_left_to_its_own_reader() {
    let cx = ParseContext::new();
    let png = std::fs::read(corpus_dir().join("images/checker.png")).unwrap();
    assert!(!MediaReader.detect(&png, "checker.png").is_match(), "an image");
    assert!(MediaReader.open(&FileSource::from_bytes("checker.png", png), &cx).is_err());
    let srt = b"1\n00:00:00,000 --> 00:00:01,000\nHello\n";
    assert!(!MediaReader.detect(srt, "a.srt").is_match(), "subtitles are text");
    let mp4 = std::fs::read(corpus_dir().join("media/h264-aac.mp4")).unwrap();
    assert!(MediaReader.detect(&mp4[..4096.min(mp4.len())], "renamed.bin").is_match(), "by its signature, whatever the name");
}

#[test]
fn every_reader_conforms() {
    let samples: Vec<Sample> = ["h264-aac.mp4", "vp9-opus.webm", "cover.mp3", "tone.flac", "tone.wav"]
        .iter()
        .map(|n| Sample::new(*n, std::fs::read(corpus_dir().join("media").join(n)).unwrap()))
        .collect();
    assert_conforms(&MediaReader, &samples);
}

#[test]
fn every_reader_survives_fuzzing() {
    libfileparser_testkit::assert_fuzz_clean(libfileparser_media::readers(), |_| {
        ["h264-aac.mp4", "vp9-opus.webm", "cover.mp3", "tone.flac", "mjpeg-pcm.avi"]
            .iter()
            .map(|n| Sample::new(*n, std::fs::read(corpus_dir().join("media").join(n)).unwrap()))
            .collect()
    });
}
