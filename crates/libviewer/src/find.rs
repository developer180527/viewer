//! Find in a view: a [`Search`] runs libfileparser's search of a content
//! ([`libfileparser_core::find`]) on a thread of its own, and the views draw its matches
//! and bring the current one into view ([`crate::show_found`]).
//!
//! ```ignore
//! let mut search = Search::content(&content, "needle", host.waker());  // on typing
//! search.as_mut().map(Search::poll);                                    // each frame
//! libviewer::show_found(ui, &content, &mut view, host, None, search.as_ref(), dark);
//! if next_pressed { search.as_mut().map(Search::next); }
//! ```
//!
//! Matches arrive in document order as the thread finds them; the first becomes current
//! as soon as it's found. A search is cancelled when dropped.

use std::cell::Cell;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use libfileparser_core::find::{self, Query, Spot};
use libfileparser_core::{Cancel, Content, Result};
use libgui::{Color, Palette};

use crate::Waker;

/// Matches a search keeps; past these it stops, and says there are more.
pub const MAX_MATCHES: usize = 10_000;
/// How often the thread wakes the window while it's finding matches.
const WAKE_EVERY: Duration = Duration::from_millis(100);

/// What a search has found so far.
#[derive(Default)]
pub(crate) struct Found {
    pub spots: Vec<Spot>,
    done: bool,
    /// Stopped at [`MAX_MATCHES`].
    capped: bool,
    error: Option<String>,
}

/// Where a search is, for an app's find bar.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Progress {
    /// Matches found so far.
    pub count: usize,
    /// The current one, from 0.
    pub current: Option<usize>,
    /// Every match is found (or the search stopped at [`MAX_MATCHES`], or failed).
    pub done: bool,
    /// Stopped at [`MAX_MATCHES`]: there are more.
    pub capped: bool,
    pub error: Option<String>,
}

/// One search of one content, running or done.
pub struct Search {
    text: String,
    found: Arc<Mutex<Found>>,
    cancel: Cancel,
    current: Option<usize>,
    /// The current match moved, and its view has yet to bring it into view.
    reveal: Cell<bool>,
    /// A row to bring into view next frame: the far side of a match's context.
    then: Cell<Option<usize>>,
}

impl Search {
    /// Searches `content` for `text`; `None` when there's nothing to look for, or
    /// nothing to look in (an image, a 3D scene).
    pub fn content(content: &Content, text: &str, wake: Waker) -> Option<Self> {
        if !find::searchable(content) {
            return None;
        }
        let content = content.clone();
        Self::run(text, wake, move |q, cancel, found| find::content(&content, q, cancel, found))
    }

    /// Runs `search` for `text` on a thread: a content of an app's own (its own text
    /// type) searched with [`libfileparser_core::find`]'s functions. `None` for empty
    /// text.
    pub fn run(text: &str, wake: Waker, search: impl FnOnce(&Query, &Cancel, find::Found<'_>) -> Result<()> + Send + 'static) -> Option<Self> {
        let query = Query::new(text)?;
        let found = Arc::new(Mutex::new(Found::default()));
        let cancel = Cancel::new();
        let (shared, stop) = (found.clone(), cancel.clone());
        let spawned = std::thread::Builder::new().name("viewer-find".into()).spawn(move || {
            let mut woke = Instant::now();
            let mut batch = Vec::new();
            let mut total = 0usize;
            // Matches are handed over in batches, so the UI thread rarely waits on the lock.
            let result = search(&query, &stop, &mut |spot| {
                batch.push(spot);
                total += 1;
                if total == 1 || woke.elapsed() >= WAKE_EVERY {
                    lock(&shared).spots.append(&mut batch);
                    woke = Instant::now();
                    wake();
                }
                total < MAX_MATCHES
            });
            let mut f = lock(&shared);
            f.spots.append(&mut batch);
            f.capped = total >= MAX_MATCHES;
            f.done = true;
            if let Err(e) = result
                && !stop.is_cancelled()
            {
                f.error = Some(e.to_string());
            }
            drop(f);
            wake();
        });
        if let Err(e) = spawned {
            let mut f = found.lock().unwrap_or_else(|e| e.into_inner());
            f.done = true;
            f.error = Some(format!("couldn't start searching: {e}"));
        }
        Some(Self { text: text.to_owned(), found, cancel, current: None, reveal: Cell::new(false), then: Cell::new(None) })
    }

    /// What it looks for.
    pub fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn found(&self) -> MutexGuard<'_, Found> {
        self.found.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Takes in what the thread found: the first match becomes current. Call once a
    /// frame, before drawing.
    pub fn poll(&mut self) {
        if self.current.is_none() && !self.found().spots.is_empty() {
            self.go(0);
        }
    }

    pub fn progress(&self) -> Progress {
        let f = self.found();
        Progress { count: f.spots.len(), current: self.current, done: f.done, capped: f.capped, error: f.error.clone() }
    }

    /// The current match, from 0.
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// The match after the current one, round to the first.
    pub fn next(&mut self) {
        let n = self.found().spots.len();
        if n > 0 {
            self.go(self.current.map_or(0, |c| (c + 1) % n));
        }
    }

    /// The match before the current one, round to the last.
    pub fn previous(&mut self) {
        let n = self.found().spots.len();
        if n > 0 {
            self.go(self.current.map_or(n - 1, |c| (c + n - 1) % n));
        }
    }

    /// Makes match `i` current and asks its view to show it.
    pub fn go(&mut self, i: usize) {
        self.current = Some(i);
        self.reveal.set(true);
    }

    /// The current match, once, when it has just moved: for its view to scroll to.
    pub(crate) fn take_reveal(&self) -> Option<Spot> {
        if !self.reveal.replace(false) {
            return None;
        }
        let i = self.current?;
        self.found().spots.get(i).cloned()
    }

    /// For a list of `rows`: the row to bring into view this frame, so the current
    /// match's row (`row_of` it) ends up with `context` rows either side of it on
    /// screen. Lists scroll a row just into view, so the row past the match is
    /// brought in first, then the one before it: whichever way the list went, the
    /// match isn't left at its edge.
    pub(crate) fn reveal_row(&self, ui: &mut libgui::Ui, rows: usize, context: usize, row_of: impl Fn(&Spot) -> u64) -> Option<usize> {
        let last = rows.checked_sub(1)?;
        if let Some(spot) = self.take_reveal() {
            let row = (row_of(&spot) as usize).min(last);
            self.then.set(Some(row.saturating_sub(context)));
            ui.request_repaint();
            return Some((row + context).min(last));
        }
        self.then.take().map(|r| r.min(last))
    }

    /// The current match.
    pub fn current_spot(&self) -> Option<Spot> {
        self.found().spots.get(self.current?).cloned()
    }
}

fn lock(m: &Mutex<Found>) -> MutexGuard<'_, Found> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Drop for Search {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// The fill behind a match, and behind the current one.
pub(crate) fn mark_colors(pal: &Palette) -> (Color, Color) {
    (pal.warning.with_alpha(0.35), pal.accent.with_alpha(0.55))
}

/// The matches in `spots` whose key (by `key`, in document order) is `k`: a range of
/// indices.
pub(crate) fn matching(spots: &[Spot], k: u64, key: impl Fn(&Spot) -> u64) -> std::ops::Range<usize> {
    let start = spots.partition_point(|s| key(s) < k);
    let end = start + spots[start..].partition_point(|s| key(s) == k);
    start..end
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use libfileparser_core::{FileSource, LineText};

    use super::*;

    fn wait(s: &mut Search) -> Progress {
        let start = Instant::now();
        while !s.progress().done {
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(5));
        }
        s.poll();
        s.progress()
    }

    #[test]
    fn matches_arrive_and_go_round() {
        let src = FileSource::from_bytes("a.txt", b"one\ntwo one\nthree\n".to_vec());
        let content = Content::Text(Arc::new(LineText::new(src, None)));
        let mut s = Search::content(&content, "ONE", Arc::new(|| {})).unwrap();
        let p = wait(&mut s);
        assert_eq!((p.count, p.current, p.capped, p.error), (2, Some(0), false, None));
        assert_eq!(s.take_reveal(), Some(Spot::Line { line: 0, range: 0..3 }));
        assert_eq!(s.take_reveal(), None);
        s.next();
        assert_eq!(s.take_reveal(), Some(Spot::Line { line: 1, range: 4..7 }));
        s.next();
        assert_eq!(s.current(), Some(0));
        s.previous();
        assert_eq!(s.current(), Some(1));
        assert!(Search::content(&content, "", Arc::new(|| {})).is_none());
    }

    #[test]
    fn it_stops_at_the_cap() {
        let src = FileSource::from_bytes("a.txt", "x\n".repeat(MAX_MATCHES + 50).into_bytes());
        let content = Content::Text(Arc::new(LineText::new(src, None)));
        let p = wait(&mut Search::content(&content, "x", Arc::new(|| {})).unwrap());
        assert_eq!((p.count, p.capped), (MAX_MATCHES, true));
    }

    #[test]
    fn matching_finds_a_lines_run() {
        let s = |line| Spot::Line { line, range: 0..1 };
        let spots = [s(1), s(3), s(3), s(7)];
        assert_eq!(matching(&spots, 3, Spot::index), 1..3);
        assert_eq!(matching(&spots, 4, Spot::index), 3..3);
    }
}
