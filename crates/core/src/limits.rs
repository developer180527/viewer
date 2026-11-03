//! Caps every loader applies to what a file claims, so a small hostile file can't make
//! Viewer allocate gigabytes (PLAN.md §8). They will move into `ParseContext`'s
//! `Limits` (docs/architecture.md §3), where an app can set its own.

/// Most pixels a decoded image may have: 16384 × 16384, 1 GiB as RGBA. Above it the
/// file is refused, not allocated; the largest real panoramas and scans fit under it.
pub const MAX_IMAGE_PIXELS: u64 = 1 << 28;

/// Most bytes a decoder may allocate for one image, with room for its own buffers
/// beside the RGBA result.
pub const MAX_IMAGE_ALLOC: u64 = 2 * MAX_IMAGE_PIXELS * 4;

/// Inflating up to this much is always allowed, whatever the ratio.
pub const UNPACK_ALWAYS: u64 = 64 << 20;

/// Past [`UNPACK_ALWAYS`], an entry may inflate to at most this many times its packed
/// size: real data rarely beats 20:1, a decompression bomb runs past 1000:1.
pub const MAX_UNPACK_RATIO: u64 = 200;

/// The most `packed` bytes may inflate to: [`UNPACK_ALWAYS`], or
/// [`MAX_UNPACK_RATIO`] times `packed` when that is more, never past `ceiling`.
pub fn unpack_cap(packed: u64, ceiling: u64) -> u64 {
    UNPACK_ALWAYS.max(packed.saturating_mul(MAX_UNPACK_RATIO)).min(ceiling)
}
