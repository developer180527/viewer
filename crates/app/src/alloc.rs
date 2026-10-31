//! The app's global allocator: the system allocator, except that blocks of 1 MB and
//! up (decoded images, decode buffers, vertex arrays) are mapped from the OS directly
//! and unmapped when freed.
//!
//! Why: macOS's allocator keeps freed large blocks for reuse and only gives them back
//! under memory pressure, so the app's footprint stayed at its peak after every big
//! load (a 4×50 MB free left 200 MB counted; `malloc_zone_pressure_relief` returned
//! nothing). glibc similarly raises its mmap threshold after big frees. Unmapping
//! returns the pages at once on every Unix; a syscall per megabyte-sized block costs
//! nothing next to filling it.

use std::alloc::{GlobalAlloc, Layout, System};

const BIG: usize = 1 << 20;

pub struct Allocator;

fn page() -> usize {
    // SAFETY: sysconf has no preconditions.
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize }
}

fn mapped(layout: &Layout) -> bool {
    layout.size() >= BIG && layout.align() <= page()
}

unsafe fn map(size: usize) -> *mut u8 {
    // SAFETY: an anonymous private mapping; the kernel picks the address.
    let p = unsafe { libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANON, -1, 0) };
    if p == libc::MAP_FAILED { std::ptr::null_mut() } else { p.cast() }
}

// SAFETY: big blocks come from mmap (page aligned, which covers their alignment) and
// go back to munmap with the same size; everything else is the system allocator's.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if mapped(&layout) { unsafe { map(layout.size()) } } else { unsafe { System.alloc(layout) } }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // Fresh mappings are already zero: no memset.
        if mapped(&layout) { unsafe { map(layout.size()) } } else { unsafe { System.alloc_zeroed(layout) } }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if mapped(&layout) {
            unsafe { libc::munmap(ptr.cast(), layout.size()) };
        } else {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
        // Both small: the system allocator grows in place when it can.
        if !mapped(&layout) && !mapped(&new) {
            return unsafe { System.realloc(ptr, layout, new_size) };
        }
        let out = unsafe { self.alloc(new) };
        if !out.is_null() {
            unsafe {
                std::ptr::copy_nonoverlapping(ptr, out, layout.size().min(new_size));
                self.dealloc(ptr, layout);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_and_small_blocks_round_trip() {
        let a = Allocator;
        for size in [16, BIG - 1, BIG, 5 * BIG + 3] {
            let layout = Layout::from_size_align(size, 8).unwrap();
            unsafe {
                let p = a.alloc_zeroed(layout);
                assert!(!p.is_null());
                assert!(std::slice::from_raw_parts(p, size).iter().all(|&b| b == 0));
                p.write_bytes(7, size);
                // Grow across the threshold and back.
                let q = a.realloc(p, layout, size * 2 + BIG);
                assert_eq!(*q.add(size - 1), 7);
                let layout2 = Layout::from_size_align(size * 2 + BIG, 8).unwrap();
                let r = a.realloc(q, layout2, 8);
                assert_eq!(*r, 7);
                a.dealloc(r, Layout::from_size_align(8, 8).unwrap());
            }
        }
    }
}
