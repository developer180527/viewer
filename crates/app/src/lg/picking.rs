//! The libgui app's answers to the interface's requests ([`Request`]): libfilepicker's
//! picker, whose answer becomes a grant in the session. The grants' platform access is
//! kept here, and dropped (on this thread) when the session releases them.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use libfilepicker::{PickOptions, Picked};
use viewer_core::{GrantId, Session};
use viewer_surface::chrome::{Request, ViewerApp};

/// An answer from the file picker, and what it was for.
struct Pick {
    picked: Picked,
    /// Reopen this model from inside the picked folder, if it is in there.
    reopen: Option<PathBuf>,
}

#[derive(Default)]
pub struct Picking {
    /// Where the picker leaves its answer, which can come after `pick` returns.
    slot: Rc<RefCell<Option<Pick>>>,
    /// What the user picked, kept while the session's pages read through it.
    grants: HashMap<GrantId, Picked>,
}

impl Picking {
    /// Starts what the interface asked for, and takes in a picker's answer. Returns
    /// true when a file started opening.
    pub fn handle(&mut self, app: &mut ViewerApp) -> bool {
        for request in app.requests.drain(..) {
            let slot = Rc::clone(&self.slot);
            match request {
                Request::Open => libfilepicker::pick(move |picked| deliver(&slot, picked, None)),
                Request::AllowFolder { reopen } => {
                    let opts = PickOptions { folders: true, start_in: Session::open_folder_for(&reopen) };
                    libfilepicker::pick_with(opts, move |picked| deliver(&slot, picked, Some(reopen)));
                }
            }
        }
        let mut started = false;
        let pick = self.slot.borrow_mut().take();
        if let Some(Pick { picked, reopen }) = pick {
            let id = app.session_mut().open_granted(picked.path(), reopen, None);
            self.grants.insert(id, picked);
            started = true;
        }
        for id in app.session_mut().take_released() {
            self.grants.remove(&id);
        }
        started
    }
}

/// Hands a picker's answer over and wakes the window (the answer can come after the
/// frame that asked, on phones). A cancelled pick changes nothing.
fn deliver(slot: &Rc<RefCell<Option<Pick>>>, picked: Option<Picked>, reopen: Option<PathBuf>) {
    let Some(picked) = picked else { return };
    *slot.borrow_mut() = Some(Pick { picked, reopen });
    if let Some(wake) = super::WAKE.get() {
        wake();
    }
}
