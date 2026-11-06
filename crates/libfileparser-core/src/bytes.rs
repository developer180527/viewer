//! Shared bytes: what sources hand out instead of borrows, so a result can outlive the
//! call, cross threads, and later come from a helper process or be decoded on demand
//! (docs/design/libfileparser.md §5, rule 5).

use std::fmt;
use std::ops::{Deref, Range};
use std::sync::Arc;

/// A range of bytes that keeps what it points into alive: part of a mapped file, or
/// bytes of its own. Cheap to clone.
#[derive(Clone)]
pub struct Bytes {
    data: Arc<dyn AsRef<[u8]> + Send + Sync>,
    range: Range<usize>,
}

impl Bytes {
    /// `range` of `data`; clamped to its length.
    pub fn slice_of(data: Arc<dyn AsRef<[u8]> + Send + Sync>, range: Range<usize>) -> Self {
        let len = (*data).as_ref().len();
        let end = range.end.min(len);
        Self { data, range: range.start.min(end)..end }
    }

    pub fn as_slice(&self) -> &[u8] {
        &(*self.data).as_ref()[self.range.clone()]
    }
}

impl Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl From<Vec<u8>> for Bytes {
    fn from(v: Vec<u8>) -> Self {
        let len = v.len();
        Self { data: Arc::new(v), range: 0..len }
    }
}

impl From<&[u8]> for Bytes {
    fn from(v: &[u8]) -> Self {
        v.to_vec().into()
    }
}

impl PartialEq for Bytes {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for Bytes {}

impl PartialEq<[u8]> for Bytes {
    fn eq(&self, other: &[u8]) -> bool {
        self.as_slice() == other
    }
}

impl fmt::Debug for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Bytes({:?})", String::from_utf8_lossy(self.as_slice()))
    }
}
