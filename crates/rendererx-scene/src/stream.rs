use std::fmt;
use std::sync::Arc;

use bytemuck::Pod;

use crate::Error;

/// Bytes that some other owner keeps alive: a memory-mapped file, a shared-memory
/// region from the parser worker, or a decoded buffer. `as_ref()` must return the same
/// slice every time (true for `Mmap`, `Vec<u8>`, `Box<[u8]>`).
pub type Bytes = Arc<dyn AsRef<[u8]> + Send + Sync>;

/// One vertex or index stream. Either owned, or a view into shared [`Bytes`], so a GLB
/// loader can hand over vertex data straight from the file mapping without copying.
pub struct Stream<T: Pod>(Repr<T>);

enum Repr<T> {
    Owned(Arc<[T]>),
    Shared { bytes: Bytes, start: usize, len: usize },
}

impl<T: Pod> Stream<T> {
    /// `len` elements of `T` starting at byte `start` of `bytes`, borrowed in place when
    /// the bytes are aligned for `T`, copied once when they aren't.
    pub fn from_bytes(bytes: Bytes, start: usize, len: usize) -> crate::Result<Self> {
        let elem = size_of::<T>();
        let all = (*bytes).as_ref();
        let end = len.checked_mul(elem).and_then(|n| n.checked_add(start));
        let Some(slice) = end.and_then(|end| all.get(start..end)) else {
            return Err(Error::ByteRange { start, end: end.unwrap_or(usize::MAX), len: all.len(), elem });
        };
        if bytemuck::try_cast_slice::<u8, T>(slice).is_ok() {
            Ok(Self(Repr::Shared { bytes, start, len }))
        } else {
            let owned: Vec<T> = slice.chunks_exact(elem).map(bytemuck::pod_read_unaligned).collect();
            Ok(Self(Repr::Owned(owned.into())))
        }
    }

    pub fn as_slice(&self) -> &[T] {
        match &self.0 {
            Repr::Owned(v) => v,
            Repr::Shared { bytes, start, len } => {
                // Checked in `from_bytes`; an `AsRef` that later returns a different
                // slice gets an empty stream rather than a panic.
                let end = start + len * size_of::<T>();
                (**bytes).as_ref().get(*start..end).and_then(|s| bytemuck::try_cast_slice(s).ok()).unwrap_or(&[])
            }
        }
    }

    pub fn len(&self) -> usize {
        match &self.0 {
            Repr::Owned(v) => v.len(),
            Repr::Shared { len, .. } => *len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn byte_len(&self) -> u64 {
        (self.len() * size_of::<T>()) as u64
    }

    /// True when the data lives in someone else's buffer (no copy was made).
    pub fn is_borrowed(&self) -> bool {
        matches!(self.0, Repr::Shared { .. })
    }
}

impl<T: Pod> Clone for Stream<T> {
    fn clone(&self) -> Self {
        Self(match &self.0 {
            Repr::Owned(v) => Repr::Owned(v.clone()),
            Repr::Shared { bytes, start, len } => Repr::Shared { bytes: bytes.clone(), start: *start, len: *len },
        })
    }
}

impl<T: Pod> From<Vec<T>> for Stream<T> {
    fn from(v: Vec<T>) -> Self {
        Self(Repr::Owned(v.into()))
    }
}

impl<T: Pod> From<&[T]> for Stream<T> {
    fn from(v: &[T]) -> Self {
        Self(Repr::Owned(v.into()))
    }
}

impl<T: Pod> fmt::Debug for Stream<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Stream<{}>[{}{}]", std::any::type_name::<T>(), self.len(), if self.is_borrowed() { ", borrowed" } else { "" })
    }
}
