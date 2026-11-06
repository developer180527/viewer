//! Reading accessors into scene streams: borrowed straight from the buffer when the
//! data is already in the renderer's layout, converted otherwise.

use scene_data::glam::{Vec2, Vec3, Vec4};
use scene_data::{Bytes, Indices, Stream};

use crate::Error;
use crate::json::{Accessor, Root};

/// A loaded glTF buffer: a range of some shared bytes (the GLB's BIN chunk inside the
/// file mapping, a decoded data URI, an external .bin).
#[derive(Clone)]
pub struct BufferData {
    pub bytes: Bytes,
    pub start: usize,
    pub len: usize,
}

impl BufferData {
    pub fn whole(bytes: Bytes) -> Self {
        let len = (*bytes).as_ref().len();
        Self { bytes, start: 0, len }
    }

    fn slice(&self) -> &[u8] {
        &(*self.bytes).as_ref()[self.start..self.start + self.len]
    }
}

const BYTE: u32 = 5120;
const UNSIGNED_BYTE: u32 = 5121;
const SHORT: u32 = 5122;
const UNSIGNED_SHORT: u32 = 5123;
const UNSIGNED_INT: u32 = 5125;
const FLOAT: u32 = 5126;

fn component_size(ty: u32) -> Result<usize, Error> {
    Ok(match ty {
        BYTE | UNSIGNED_BYTE => 1,
        SHORT | UNSIGNED_SHORT => 2,
        UNSIGNED_INT | FLOAT => 4,
        _ => return Err(Error::Format(format!("unknown component type {ty}"))),
    })
}

fn components(ty: &str) -> Result<usize, Error> {
    Ok(match ty {
        "SCALAR" => 1,
        "VEC2" => 2,
        "VEC3" => 3,
        "VEC4" => 4,
        "MAT2" => 4,
        "MAT3" => 9,
        "MAT4" => 16,
        _ => return Err(Error::Format(format!("unknown accessor type {ty:?}"))),
    })
}

/// One component as f32, applying the spec's normalisation rules.
fn component(bytes: &[u8], ty: u32, normalized: bool) -> f32 {
    let n = normalized;
    match ty {
        BYTE => {
            let v = bytes[0] as i8 as f32;
            if n { (v / 127.0).max(-1.0) } else { v }
        }
        UNSIGNED_BYTE => {
            let v = bytes[0] as f32;
            if n { v / 255.0 } else { v }
        }
        SHORT => {
            let v = i16::from_le_bytes([bytes[0], bytes[1]]) as f32;
            if n { (v / 32767.0).max(-1.0) } else { v }
        }
        UNSIGNED_SHORT => {
            let v = u16::from_le_bytes([bytes[0], bytes[1]]) as f32;
            if n { v / 65535.0 } else { v }
        }
        UNSIGNED_INT => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32,
        _ => f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
    }
}

/// An unsigned index; callers check `ty` is one of the three unsigned types first.
fn integer(bytes: &[u8], ty: u32) -> u32 {
    match ty {
        UNSIGNED_BYTE => bytes[0] as u32,
        UNSIGNED_SHORT => u16::from_le_bytes([bytes[0], bytes[1]]) as u32,
        UNSIGNED_INT => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        _ => unreachable!("component type {ty} checked by the caller"),
    }
}

pub struct Reader<'a> {
    pub root: &'a Root,
    pub buffers: &'a [Option<BufferData>],
}

/// Where an accessor's elements live, bounds-checked against its buffer view.
struct Located<'a> {
    buffer: &'a BufferData,
    /// Offset of element 0 within `buffer.slice()`.
    offset: usize,
    stride: usize,
    elem: usize,
}

impl<'a> Reader<'a> {
    fn accessor(&self, index: usize) -> Result<&'a Accessor, Error> {
        self.root.accessors.get(index).ok_or_else(|| Error::Format(format!("accessor {index} doesn't exist")))
    }

    /// Checks that `count` elements of `elem` bytes at `offset` into `view` fit inside
    /// both the view and its buffer.
    fn locate(&self, view: usize, offset: usize, count: usize, elem: usize) -> Result<Located<'a>, Error> {
        let bad = |what: &str| Error::Format(format!("buffer view {view}: {what}"));
        let v = self.root.buffer_views.get(view).ok_or_else(|| bad("doesn't exist"))?;
        let buffer = self.buffers.get(v.buffer).and_then(Option::as_ref).ok_or_else(|| bad("its buffer isn't loaded"))?;
        let stride = v.byte_stride.unwrap_or(elem);
        if stride < elem || stride > 252 && v.byte_stride.is_some() {
            return Err(bad("bad stride"));
        }
        let view_end = v.byte_offset.checked_add(v.byte_length).ok_or_else(|| bad("overflow"))?;
        if view_end > buffer.len {
            return Err(bad("runs past its buffer"));
        }
        if count > 0 {
            let last = stride.checked_mul(count - 1).and_then(|n| n.checked_add(offset)).and_then(|n| n.checked_add(elem));
            if last.is_none_or(|end| end > v.byte_length) {
                return Err(bad("accessor runs past the view"));
            }
        }
        Ok(Located { buffer, offset: v.byte_offset + offset, stride, elem })
    }

    /// All components of every element as f32, sparse substitution applied.
    pub fn floats(&self, index: usize) -> Result<(Vec<f32>, usize), Error> {
        let a = self.accessor(index)?;
        let comps = components(&a.ty)?;
        let csize = component_size(a.component_type)?;
        let total = a.count.checked_mul(comps).ok_or_else(|| Error::Format("accessor too large".into()))?;
        let mut out = match a.buffer_view {
            Some(view) => {
                let at = self.locate(view, a.byte_offset, a.count, comps * csize)?;
                let data = at.buffer.slice();
                let mut out = Vec::with_capacity(total);
                for i in 0..a.count {
                    let base = at.offset + i * at.stride;
                    for c in 0..comps {
                        out.push(component(&data[base + c * csize..], a.component_type, a.normalized));
                    }
                }
                out
            }
            // No view: zeros, possibly patched by sparse values. Bounded by the sparse
            // data actually present, so a huge count can't allocate from nothing.
            None => {
                let backing = a.sparse.as_ref().map_or(0, |s| s.count);
                if a.count > backing.max(1) * 1024 {
                    return Err(Error::Format(format!("accessor {index} has no data")));
                }
                vec![0.0; total]
            }
        };
        if let Some(s) = &a.sparse {
            // The spec allows only unsigned indices; `integer` reads exactly these widths.
            if !matches!(s.indices.component_type, UNSIGNED_BYTE | UNSIGNED_SHORT | UNSIGNED_INT) {
                return Err(Error::Format(format!("accessor {index}: sparse indices of component type {}", s.indices.component_type)));
            }
            let isize = component_size(s.indices.component_type)?;
            let idx = self.locate(s.indices.buffer_view, s.indices.byte_offset, s.count, isize)?;
            let val = self.locate(s.values.buffer_view, s.values.byte_offset, s.count, comps * csize)?;
            let (idata, vdata) = (idx.buffer.slice(), val.buffer.slice());
            for i in 0..s.count {
                let target = integer(&idata[idx.offset + i * idx.stride..], s.indices.component_type) as usize;
                if target >= a.count {
                    return Err(Error::Format(format!("accessor {index}: sparse index out of range")));
                }
                for c in 0..comps {
                    out[target * comps + c] = component(&vdata[val.offset + i * val.stride + c * csize..], a.component_type, a.normalized);
                }
            }
        }
        Ok((out, comps))
    }

    /// The accessor's bytes in place when it is plain `FLOAT` data of exactly `comps`
    /// components, tightly packed and not sparse.
    fn packed_floats<T: bytemuck::Pod>(&self, index: usize, comps: usize) -> Result<Option<Stream<T>>, Error> {
        let a = self.accessor(index)?;
        let (Some(view), None, FLOAT) = (a.buffer_view, &a.sparse, a.component_type) else { return Ok(None) };
        if components(&a.ty)? != comps {
            return Ok(None);
        }
        let at = self.locate(view, a.byte_offset, a.count, comps * 4)?;
        if at.stride != at.elem {
            return Ok(None);
        }
        Ok(Some(Stream::from_bytes(at.buffer.bytes.clone(), at.buffer.start + at.offset, a.count).map_err(|e| Error::Format(e.to_string()))?))
    }

    pub fn vec2(&self, index: usize) -> Result<Stream<Vec2>, Error> {
        if let Some(s) = self.packed_floats(index, 2)? {
            return Ok(s);
        }
        let (f, comps) = self.floats(index)?;
        Ok(f.chunks_exact(comps).map(|c| Vec2::new(c[0], *c.get(1).unwrap_or(&0.0))).collect::<Vec<_>>().into())
    }

    pub fn vec3(&self, index: usize) -> Result<Stream<Vec3>, Error> {
        if let Some(s) = self.packed_floats(index, 3)? {
            return Ok(s);
        }
        let (f, comps) = self.floats(index)?;
        if comps < 3 {
            return Err(Error::Format(format!("accessor {index}: expected VEC3")));
        }
        Ok(f.chunks_exact(comps).map(|c| Vec3::new(c[0], c[1], c[2])).collect::<Vec<_>>().into())
    }

    /// VEC4, or VEC3 with w = `w` (colours without alpha).
    pub fn vec4(&self, index: usize, w: f32) -> Result<Stream<Vec4>, Error> {
        if let Some(s) = self.packed_floats(index, 4)? {
            return Ok(s);
        }
        let (f, comps) = self.floats(index)?;
        if comps < 3 {
            return Err(Error::Format(format!("accessor {index}: expected VEC3 or VEC4")));
        }
        Ok(f.chunks_exact(comps).map(|c| Vec4::new(c[0], c[1], c[2], *c.get(3).unwrap_or(&w))).collect::<Vec<_>>().into())
    }

    /// Index data: u16 and u32 borrowed in place, u8 widened to u16.
    pub fn indices(&self, index: usize) -> Result<Indices, Error> {
        let a = self.accessor(index)?;
        if a.ty != "SCALAR" {
            return Err(Error::Format(format!("index accessor {index} isn't SCALAR")));
        }
        let size = component_size(a.component_type)?;
        if let (Some(view), None) = (a.buffer_view, &a.sparse) {
            let at = self.locate(view, a.byte_offset, a.count, size)?;
            let start = at.buffer.start + at.offset;
            let shared = |e: scene_data::Error| Error::Format(e.to_string());
            if at.stride == size {
                match a.component_type {
                    UNSIGNED_SHORT => return Ok(Indices::U16(Stream::from_bytes(at.buffer.bytes.clone(), start, a.count).map_err(shared)?)),
                    UNSIGNED_INT => return Ok(Indices::U32(Stream::from_bytes(at.buffer.bytes.clone(), start, a.count).map_err(shared)?)),
                    _ => {}
                }
            }
        }
        // Strided or u8 indices: read as integers (f32 would lose indices above 2^24).
        let Some(view) = a.buffer_view.filter(|_| a.sparse.is_none()) else {
            return Err(Error::Format(format!("index accessor {index} is sparse or has no data")));
        };
        let at = self.locate(view, a.byte_offset, a.count, size)?;
        let data = at.buffer.slice();
        let read = |i: usize| integer(&data[at.offset + i * at.stride..], a.component_type);
        Ok(match a.component_type {
            UNSIGNED_BYTE | UNSIGNED_SHORT => Indices::U16((0..a.count).map(|i| read(i) as u16).collect::<Vec<_>>().into()),
            UNSIGNED_INT => Indices::U32((0..a.count).map(read).collect::<Vec<_>>().into()),
            t => return Err(Error::Format(format!("index accessor {index}: component type {t}"))),
        })
    }

    /// Bounds from the accessor's min/max (required for POSITION by the spec).
    pub fn min_max(&self, index: usize) -> Option<(Vec3, Vec3)> {
        let a = self.root.accessors.get(index)?;
        let (min, max) = (a.min.as_ref()?, a.max.as_ref()?);
        let v = |s: &[f32]| (s.len() >= 3).then(|| Vec3::new(s[0], s[1], s[2]));
        let (mut lo, mut hi) = (v(min)?, v(max)?);
        if a.normalized {
            // Quantized positions: min/max are in raw integer units.
            let scale = match a.component_type {
                BYTE => 1.0 / 127.0,
                UNSIGNED_BYTE => 1.0 / 255.0,
                SHORT => 1.0 / 32767.0,
                UNSIGNED_SHORT => 1.0 / 65535.0,
                _ => 1.0,
            };
            lo *= scale;
            hi *= scale;
        }
        (lo.is_finite() && hi.is_finite()).then_some((lo, hi))
    }

    /// The bytes of a buffer view (embedded images).
    pub fn view_bytes(&self, view: usize) -> Result<Stream<u8>, Error> {
        let v = self.root.buffer_views.get(view).ok_or_else(|| Error::Format(format!("buffer view {view} doesn't exist")))?;
        let at = self.locate(view, 0, 1, v.byte_length)?;
        Stream::from_bytes(at.buffer.bytes.clone(), at.buffer.start + at.offset, v.byte_length).map_err(|e| Error::Format(e.to_string()))
    }
}
