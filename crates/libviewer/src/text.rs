//! Text and hex: virtual lists, so only the visible lines are read and shaped and a
//! 2 GB log scrolls freely.

use std::ops::Range;

use libfileparser_core::find::Spot;
use libfileparser_core::{FileSource, TextSource};
use libgui::*;

use crate::find::{Search, mark_colors, matching};

/// Longer lines are cut for display (minified files would otherwise stall layout).
const MAX_LINE_BYTES: usize = 8 * 1024;
const MONO_SIZE: f32 = 13.0;
const LINE_HEIGHT: f32 = 18.0;
/// Lines kept on screen above and below a found match brought into view.
const CONTEXT: usize = 4;

/// Colours for a line's text, from an app's highlighter: byte ranges into the line
/// (as `String::from_utf8_lossy` gives it) and their RGB colour.
pub trait Highlights {
    fn spans(&self, line: usize, dark: bool) -> Vec<(Range<usize>, [u8; 3])>;
}

fn rgb([r, g, b]: [u8; 3]) -> Color {
    Color::rgba(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0)
}

/// One row of monospaced text in a virtual list: a gutter, then `text` in `spans`, the
/// byte ranges in `marks` filled behind (found matches).
fn mono_row(ui: &mut Ui, i: usize, gutter: &str, text: &str, spans: Vec<TextSpan>, marks: Vec<(Range<usize>, Color)>, tail: Option<&str>) {
    let mono = ui.font_for(FontRole::Mono);
    let pal = ui.theme.palette;
    let gutter = ui.frame_text(gutter);
    // Each mark as the text before it and its own, measured when painted.
    let marks: Vec<_> = marks.into_iter().filter_map(|(r, fill)| Some((ui.frame_text(text.get(..r.start)?), ui.frame_text(text.get(r)?), fill))).collect();
    let text = ui.frame_text(text);
    let tail = tail.map(|t| ui.frame_text(t));
    let id = ui.make_id(("row", i));
    ui.add_leaf(id, Layout::leaf(Size::Grow(1.0), Size::Fixed(LINE_HEIGHT)), Vec2::ZERO, false, move |p, r| {
        // The painter is shared by every paint closure, so put the font back after.
        let body = std::mem::replace(&mut p.font, mono);
        let em = p.measure(MONO_SIZE, "0");
        let y = r.y + ((r.h - em.y) / 2.0).round();
        let gw = p.measure(MONO_SIZE, gutter).x + em.x * 2.0;
        p.text(Vec2::new(r.x, y), MONO_SIZE, pal.text_faint, gutter);
        for &(before, inside, fill) in &marks {
            let x = r.x + gw + p.measure(MONO_SIZE, before).x;
            let w = p.measure(MONO_SIZE, inside).x.max(2.0);
            p.rect(Rect::new(x - 1.0, r.y + 1.0, w + 2.0, r.h - 2.0), fill, 3.0);
        }
        p.text_spans(Vec2::new(r.x + gw, y), MONO_SIZE, pal.text, text, &spans);
        if let Some(tail) = tail {
            let x = r.x + gw + p.measure(MONO_SIZE, text).x;
            p.text(Vec2::new(x, y), MONO_SIZE, pal.text_faint, tail);
        }
        p.font = body;
    });
}

/// Text by line, numbered, coloured by `highlights` when given.
pub fn text(ui: &mut Ui, source: &dyn TextSource, highlights: Option<&dyn Highlights>, dark: bool) {
    text_found(ui, source, highlights, None, dark);
}

/// [`text`], with `search`'s matches marked and the current one brought into view.
pub fn text_found(ui: &mut Ui, source: &dyn TextSource, highlights: Option<&dyn Highlights>, search: Option<&Search>, dark: bool) {
    let count = source.info().lines.known() as usize;
    let digits = count.to_string().len();
    let reveal = search.and_then(|s| s.reveal_row(ui, count, CONTEXT, Spot::index));
    let opts = ListOptions { padding: Insets::xy(8.0, 4.0), reveal, ..ListOptions::new(LINE_HEIGHT) };
    let (other, current) = mark_colors(&ui.theme.palette);
    let found = search.map(|s| (s.found(), s.current()));
    ui.virtual_list_with("text", count, opts, |ui, i| {
        let raw = source.lines(i as u64..i as u64 + 1).ok().and_then(|mut l| l.pop());
        let raw = raw.as_ref().map_or(&[][..], |b| b.as_slice());
        let full = String::from_utf8_lossy(&raw[..raw.len().min(MAX_LINE_BYTES)]);
        let line = full.trim_end_matches(['\n', '\r']);
        let mut spans = Vec::new();
        if let Some(h) = highlights.filter(|_| raw.len() <= MAX_LINE_BYTES) {
            for (range, color) in h.spans(i, dark) {
                let end = range.end.min(line.len());
                if range.start < end {
                    spans.push(TextSpan::new(range.start..end, rgb(color)));
                }
            }
        }
        let mut marks = Vec::new();
        if let Some((f, at)) = &found {
            for k in matching(&f.spots, i as u64, Spot::index) {
                if let Spot::Line { range, .. } = &f.spots[k] {
                    marks.push((range.clone(), if *at == Some(k) { current } else { other }));
                }
            }
        }
        let tail = (raw.len() > MAX_LINE_BYTES).then(|| format!("  … ({} more bytes)", raw.len() - MAX_LINE_BYTES));
        mono_row(ui, i, &format!("{:>digits$}", i + 1), line, spans, marks, tail.as_deref());
    });
}

/// Bytes, 16 per row: offset, hex, and the printable ones as text.
/// Bytes in memory, sixteen to a row.
#[deprecated(since = "0.3.1", note = "use `hex_file`, which reads a file a row at a time")]
pub fn hex(ui: &mut Ui, bytes: &[u8]) {
    hex_rows(ui, bytes.len() as u64, None, |offset, buf| {
        let start = (offset as usize).min(bytes.len());
        let n = buf.len().min(bytes.len() - start);
        buf[..n].copy_from_slice(&bytes[start..start + n]);
        n
    });
}

/// A file's bytes, sixteen to a row, each row read as it is drawn: a file opened to
/// be read by position (a live log) is never read whole.
pub fn hex_file(ui: &mut Ui, src: &FileSource) {
    hex_file_found(ui, src, None);
}

/// [`hex_file`], with `search`'s matches marked (in the bytes and the text beside
/// them) and the current one brought into view.
pub fn hex_file_found(ui: &mut Ui, src: &FileSource, search: Option<&Search>) {
    hex_rows(ui, src.len(), search, |offset, buf| src.read_at(offset, buf));
}

fn hex_rows(ui: &mut Ui, len: u64, search: Option<&Search>, read: impl Fn(u64, &mut [u8]) -> usize) {
    const PER_ROW: usize = 16;
    let rows = (len as usize).div_ceil(PER_ROW);
    let row_of = |s: &Spot| s.index() / PER_ROW as u64;
    let reveal = search.and_then(|s| s.reveal_row(ui, rows, CONTEXT, row_of));
    let opts = ListOptions { padding: Insets::xy(8.0, 4.0), reveal, ..ListOptions::new(LINE_HEIGHT) };
    let faint = ui.theme.palette.text_muted;
    let (other, current) = mark_colors(&ui.theme.palette);
    let found = search.map(|s| (s.found(), s.current()));
    ui.virtual_list_with("hex", rows, opts, |ui, i| {
        let mut buf = [0u8; PER_ROW];
        let n = read((i * PER_ROW) as u64, &mut buf);
        let chunk = &buf[..n];
        let mut line = String::with_capacity(80);
        for k in 0..PER_ROW {
            match chunk.get(k) {
                Some(b) => line.push_str(&format!("{b:02x} ")),
                None => line.push_str("   "),
            }
            if k == 7 {
                line.push(' ');
            }
        }
        let ascii_start = line.len() + 1;
        line.push(' ');
        line.extend(chunk.iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '.' }));
        let spans = vec![TextSpan::new(ascii_start..line.len(), faint)];
        // Matches covering this row's bytes: those starting up to a few rows above too.
        let mut marks = Vec::new();
        if let Some((f, at)) = &found {
            let (row_start, row_end) = ((i * PER_ROW) as u64, ((i + 1) * PER_ROW) as u64);
            let first = f.spots.partition_point(|s| s.index() + spot_len(s) <= row_start);
            for (k, s) in f.spots.iter().enumerate().skip(first).take_while(|(_, s)| s.index() < row_end) {
                let (a, b) = (s.index().max(row_start) - row_start, (s.index() + spot_len(s)).min(row_end) - row_start);
                let fill = if *at == Some(k) { current } else { other };
                for c in a as usize..b as usize {
                    let hx = c * 3 + usize::from(c > 7);
                    marks.push((hx..hx + 2, fill));
                    marks.push((ascii_start + c..ascii_start + c + 1, fill));
                }
            }
        }
        mono_row(ui, i, &format!("{:08x}", i * PER_ROW), &line, spans, marks, None);
    });
}

/// How many bytes a match covers.
fn spot_len(s: &Spot) -> u64 {
    match s {
        Spot::Bytes { len, .. } => *len,
        _ => 1,
    }
}
