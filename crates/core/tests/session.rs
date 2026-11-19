//! The session on real files, through the app's registry, with no UI: what a shell
//! would see after each command.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use libfileparser_core::Content as Model;
use viewer_core::{Hooks, Info, Notice, Session, Status, ViewChoice};
use viewer_model::Content;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A session, and how many times it woke its shell.
fn session() -> (Session, Arc<AtomicUsize>) {
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    (Session::new(Hooks::new(Arc::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    }))), wakes)
}

/// Polls until the open in flight finishes, as a shell does when woken.
fn settle(s: &mut Session) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !s.poll() {
        assert!(Instant::now() < deadline, "an open never finished");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn opening_a_file() {
    let (mut s, wakes) = session();
    assert_eq!(s.status(), Status::Empty);
    assert!(!s.shown());
    s.open_path(root().join("samples/data.json"));
    assert!(matches!(s.status(), Status::Loading(ref n) if n == "data.json"));
    settle(&mut s);
    assert!(wakes.load(Ordering::SeqCst) >= 1, "the loader woke the shell");
    assert!(s.shown());
    assert_eq!(s.pages().len(), 1);
    assert_eq!(s.take_title().as_deref(), Some("data.json"));
    assert_eq!(s.take_title(), None, "the title is given once, when it changes");
    let Status::Shown { size, model, .. } = s.status() else { panic!() };
    assert_eq!(size, Some(std::fs::metadata(root().join("samples/data.json")).unwrap().len()));
    assert!(!model.is_empty());
    let info = s.info();
    assert!(info.contains(&Info::Row("Name".into(), "data.json".into())));
    assert!(info.iter().any(|i| matches!(i, Info::Row(k, _) if k == "Format")));
    assert!(s.notices().is_empty());
}

#[test]
fn a_failed_open_keeps_the_pages() {
    let (mut s, _) = session();
    s.open_path(root().join("samples/data.json"));
    settle(&mut s);
    let id = s.current().unwrap().id;
    s.open_path(root().join("samples/no such file.txt"));
    settle(&mut s);
    assert!(s.error().unwrap().contains("no such file.txt"), "{:?}", s.error());
    assert_eq!(s.current().unwrap().id, id, "the page on screen stays");
    assert!(s.take_closed().is_empty());
}

#[test]
fn into_a_zip_and_back() {
    let (mut s, _) = session();
    s.open_path(root().join("samples/bundle.zip"));
    settle(&mut s);
    let zip = s.current().unwrap().id;
    let Content::Model(Model::Container(c)) = &s.current().unwrap().doc.content else { panic!("a zip lists its entries") };
    let entries = c.entries(0..10).unwrap();
    let json = entries.iter().position(|e| e.path.ends_with("data.json")).unwrap() as u64;
    s.open_entry(c.clone(), json);
    settle(&mut s);
    assert_eq!(s.pages().len(), 2);
    assert!(s.can_go_back());
    assert_eq!(s.take_title().as_deref(), Some("bundle.zip › data/data.json"), "an entry by its path in the zip");
    let entry = s.current().unwrap().id;
    assert!(s.back());
    assert_eq!(s.current().unwrap().id, zip);
    assert_eq!(s.take_closed(), vec![entry]);
    assert!(!s.back(), "nothing below the root");
}

#[test]
fn viewing_another_way_replaces_the_page() {
    let (mut s, _) = session();
    s.open_path(root().join("samples/table.csv"));
    settle(&mut s);
    let first = s.current().unwrap().id;
    let (choices, current) = s.view_choices();
    assert!(choices.len() > 1, "{choices:?}");
    // The page is shown as what it was opened as.
    assert_eq!(current, choices.iter().position(|(c, _)| *c == ViewChoice::Facet(0)).unwrap_or(current));
    let source = choices.iter().find(|(_, n)| n == "Source").map(|(c, _)| *c).expect("a CSV's source as text");
    s.view(source);
    settle(&mut s);
    assert_eq!(s.pages().len(), 1, "a view replaces the page, not pushes one");
    assert_eq!(s.take_closed(), vec![first]);
    assert_eq!(s.view_choices().1, choices.iter().position(|(c, _)| *c == source).unwrap());
    // A loader by id: hex.
    s.view(ViewChoice::Loader("hex"));
    settle(&mut s);
    assert_eq!(s.current().unwrap().loader, "hex");
}

#[test]
fn a_fallback_says_why() {
    let (mut s, _) = session();
    s.open_path(root().join("corpus/files/hostile/bomb.txt.gz"));
    settle(&mut s);
    assert_eq!(s.current().unwrap().loader, "hex");
    let notices = s.notices();
    assert!(matches!(&notices[..], [Notice::Fallback(m)] if m.starts_with("Couldn't read this as ") && m.contains("Showing it as")), "{notices:?}");
    assert!(s.info().iter().any(|i| matches!(i, Info::Section(t, text) if t == "Loaders that failed first" && text.starts_with("gzip:"))));
}

#[test]
fn grants_are_released_when_no_page_reads_through_them() {
    let (mut s, _) = session();
    let folder = root().join("samples");
    let g = s.open_granted(&folder, None);
    settle(&mut s);
    assert!(s.current().unwrap().src.is_folder());
    assert!(s.take_released().is_empty(), "the folder's page reads through it");
    // A new file replaces the folder's page: its grant goes.
    s.open_path(root().join("samples/data.json"));
    settle(&mut s);
    assert_eq!(s.take_released(), vec![g]);
    // A grant whose open fails is released at once, and the pages stay.
    let bad = s.open_granted(&root().join("samples/missing.bin"), None);
    settle(&mut s);
    assert!(s.error().is_some());
    assert_eq!(s.take_released(), vec![bad]);
    assert_eq!(s.current().unwrap().src.name(), "data.json");
}

#[test]
fn reopening_from_a_granted_folder() {
    let (mut s, _) = session();
    let model = root().join("samples/data.json");
    // The folder picked for a blocked model reaches it: the model opens, not the folder.
    s.open_granted(&root().join("samples"), Some(model.clone()));
    settle(&mut s);
    assert_eq!(s.current().unwrap().src.name(), "data.json");
    // A folder that doesn't: what was picked opens.
    s.open_granted(&root().join("corpus"), Some(model));
    settle(&mut s);
    assert!(s.current().unwrap().src.is_folder());
    assert_eq!(Session::open_folder_for(&root().join("samples/data.json")), Some(root().join("samples")));
}

/// A shell can hand a session to another thread (Swift's async runtime does).
#[test]
fn a_session_is_send() {
    fn send<T: Send>() {}
    send::<Session>();
}
