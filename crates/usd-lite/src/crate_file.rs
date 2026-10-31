//! USDC, the binary "crate" format (`PXR-USDC`), versions 0.4–0.10.
//!
//! Layout: an 88-byte bootstrap (magic, version, table-of-contents offset), sections
//! named in the TOC (TOKENS, STRINGS, FIELDS, FIELDSETS, PATHS, SPECS) and the value
//! data they point into. Structural sections are LZ4-compressed (in Pixar's chunked
//! wrapper) and integer lists use Pixar's delta + 2-bit-code integer compression.
//! Each spec is a path, a spec type and a set of fields; each field is a token name
//! and a `ValueRep` (type, flags, and an inline value or a file offset).

use crate::layer::{Layer, Spec, SpecType};
use crate::value::half_to_f32;
use crate::{Error, Limits, Result, Specifier, Value};

const ARRAY: u64 = 1 << 63;
const INLINED: u64 = 1 << 62;
const COMPRESSED: u64 = 1 << 61;
const PAYLOAD: u64 = (1 << 48) - 1;
/// Arrays shorter than this are stored raw even when flagged compressed.
const MIN_COMPRESSED_ARRAY: u64 = 16;

pub fn read(bytes: &[u8], limits: &Limits) -> Result<Layer> {
    let mut c = Cursor::new(bytes, 0);
    if c.take(8)? != b"PXR-USDC" {
        return Err(Error::Format("not a USDC file".into()));
    }
    let v = c.take(8)?;
    let version = (v[0], v[1], v[2]);
    if !((0, 4, 0)..=(0, 10, 0)).contains(&version) {
        return Err(Error::Unsupported(format!("USDC version {}.{}.{}", v[0], v[1], v[2])));
    }
    let toc = c.u64()? as usize;
    let mut f = File { bytes, version, limits: *limits, tokens: Vec::new(), strings: Vec::new(), fields: Vec::new(), paths: Vec::new() };

    let mut t = Cursor::new(bytes, toc);
    let sections = t.count(32, limits)?;
    let mut table = Vec::with_capacity(sections);
    for _ in 0..sections {
        let name = t.take(16)?;
        let name = String::from_utf8_lossy(&name[..name.iter().position(|&b| b == 0).unwrap_or(16)]).into_owned();
        let (start, _size) = (t.u64()? as usize, t.u64()?);
        table.push((name, start));
    }
    let section = |name: &str| {
        table.iter().find(|(n, _)| n == name).map(|&(_, s)| Cursor::new(bytes, s)).ok_or_else(|| Error::Format(format!("missing {name} section")))
    };

    // TOKENS: count, uncompressed size, compressed size, then NUL-separated strings.
    let mut s = section("TOKENS")?;
    let count = s.u64()? as usize;
    let (raw, packed) = (s.u64()? as usize, s.u64()? as usize);
    let text = decompress(s.take(packed)?, raw, limits)?;
    f.tokens = text.split(|&b| b == 0).take(count).map(|t| String::from_utf8_lossy(t).into_owned()).collect();
    if f.tokens.len() < count {
        return Err(Error::Format("fewer tokens than declared".into()));
    }

    let mut s = section("STRINGS")?;
    let n = s.count(4, limits)?;
    f.strings = (0..n).map(|_| s.u32()).collect::<Result<_>>()?;

    // FIELDS: token indices (compressed ints), then ValueReps (LZ4).
    let mut s = section("FIELDS")?;
    let n = s.u64()? as usize;
    let names = compressed_ints32(&mut s, n, limits)?;
    let reps_size = s.u64()? as usize;
    let reps = decompress(s.take(reps_size)?, n.checked_mul(8).ok_or_else(too_big)?, limits)?;
    f.fields = names.iter().zip(reps.as_chunks::<8>().0).map(|(&t, r)| (t as u32, u64::from_le_bytes(*r))).collect();

    // FIELDSETS: field indices, each set ended by !0.
    let mut s = section("FIELDSETS")?;
    let n = s.u64()? as usize;
    let fieldsets = compressed_ints32(&mut s, n, limits)?;

    let mut s = section("PATHS")?;
    let total = s.u64()? as usize;
    f.paths = read_paths(&mut s, total, &f.tokens, limits)?;

    let mut s = section("SPECS")?;
    let n = s.u64()? as usize;
    let path_idx = compressed_ints32(&mut s, n, limits)?;
    let fieldset_idx = compressed_ints32(&mut s, n, limits)?;
    let types = compressed_ints32(&mut s, n, limits)?;

    let mut layer = Layer::default();
    for i in 0..n {
        let path = f.paths.get(path_idx[i] as usize).cloned().flatten().ok_or_else(|| Error::Format("spec path out of range".into()))?;
        let mut fields = Vec::new();
        let mut at = fieldset_idx[i] as usize;
        while let Some(&field) = fieldsets.get(at) {
            if field == -1 {
                break;
            }
            let &(name, rep) = f.fields.get(field as usize).ok_or_else(|| Error::Format("field out of range".into()))?;
            let name = f.token(name)?.to_string();
            // A value this reader can't decode doesn't sink the file.
            let value = f.value(rep, 0).unwrap_or(Value::Other("unreadable"));
            fields.push((name, value));
            at += 1;
        }
        layer.specs.insert(path, Spec { spec_type: SpecType::from_u32(types[i] as u32), fields });
    }
    Ok(layer)
}

fn too_big() -> Error {
    Error::Limit("size overflow".into())
}

struct File<'a> {
    bytes: &'a [u8],
    version: (u8, u8, u8),
    limits: Limits,
    tokens: Vec<String>,
    strings: Vec<u32>,
    fields: Vec<(u32, u64)>,
    paths: Vec<Option<String>>,
}

impl File<'_> {
    fn token(&self, i: u32) -> Result<&str> {
        self.tokens.get(i as usize).map(String::as_str).ok_or_else(|| Error::Format("token out of range".into()))
    }

    fn string(&self, i: u32) -> Result<String> {
        let t = *self.strings.get(i as usize).ok_or_else(|| Error::Format("string out of range".into()))?;
        Ok(self.token(t)?.to_string())
    }

    fn path(&self, i: u32) -> Result<String> {
        self.paths.get(i as usize).cloned().flatten().ok_or_else(|| Error::Format("path out of range".into()))
    }

    fn at(&self, offset: u64) -> Cursor<'_> {
        Cursor::new(self.bytes, offset as usize)
    }

    /// Array element count: u32 before 0.7, u64 after.
    fn array_len(&self, c: &mut Cursor<'_>, elem: usize) -> Result<usize> {
        let n = if self.version < (0, 7, 0) { c.u32()? as u64 } else { c.u64()? };
        if n > self.limits.max_elements as u64 {
            return Err(Error::Limit(format!("array of {n} elements")));
        }
        // Uncompressed data must actually be there.
        if elem > 0 && (n as usize).saturating_mul(elem) > c.remaining() {
            return Err(Error::Format("array runs past the file".into()));
        }
        Ok(n as usize)
    }

    fn value(&self, rep: u64, depth: u32) -> Result<Value> {
        if depth > 4 {
            return Err(Error::Format("values nested too deeply".into()));
        }
        let ty = ((rep >> 48) & 0xff) as u8;
        let payload = rep & PAYLOAD;
        if rep & ARRAY != 0 {
            return self.array(ty, payload, rep & COMPRESSED != 0);
        }
        if rep & INLINED != 0 {
            return self.inline(ty, payload);
        }
        let mut c = self.at(payload);
        let f64s = |c: &mut Cursor<'_>, n: usize| (0..n).map(|_| c.f64()).collect::<Result<Vec<_>>>();
        Ok(match ty {
            5 => Value::Int(c.u64()? as i64),
            6 => Value::Int(c.u64()? as i64),
            9 => Value::Float(c.f64()?),
            15 => Value::Matrix4(f64s(&mut c, 16)?.try_into().unwrap()),
            16 => quat(f64s(&mut c, 4)?),
            17 => quat((0..4).map(|_| c.f32().map(f64::from)).collect::<Result<_>>()?),
            18 => quat((0..4).map(|_| c.u16().map(|h| half_to_f32(h) as f64)).collect::<Result<_>>()?),
            19..=30 => {
                let (n, comp) = vec_shape(ty);
                let v = (0..n).map(|_| comp.read(&mut c)).collect::<Result<Vec<_>>>()?;
                vector(&v)
            }
            // List ops: the items they add, in order.
            32 => Value::TokenArray(self.list_op(&mut c, |f, c| f.token(c.u32()?).map(str::to_string))?),
            33 => Value::StringArray(self.list_op(&mut c, |f, c| f.string(c.u32()?))?),
            34 => Value::Paths(self.list_op(&mut c, |f, c| f.path(c.u32()?))?),
            40 => {
                let n = c.count(4, &self.limits)?;
                Value::Paths((0..n).map(|_| self.path(c.u32()?)).collect::<Result<_>>()?)
            }
            41 => {
                let n = c.count(4, &self.limits)?;
                Value::TokenArray((0..n).map(|_| self.token(c.u32()?).map(str::to_string)).collect::<Result<_>>()?)
            }
            46 => self.time_samples(payload, depth)?,
            48 => {
                let n = c.count(8, &self.limits)?;
                Value::DoubleArray(f64s(&mut c, n)?)
            }
            50 => {
                let n = c.count(4, &self.limits)?;
                Value::StringArray((0..n).map(|_| self.string(c.u32()?)).collect::<Result<_>>()?)
            }
            31 => Value::Other("dictionary"),
            35 | 47 | 55 => Value::Other("reference or payload"),
            45 => Value::Other("variant selection"),
            _ => Value::Other("unsupported type"),
        })
    }

    fn inline(&self, ty: u8, p: u64) -> Result<Value> {
        let bytes = p.to_le_bytes();
        Ok(match ty {
            1 => Value::Bool(p != 0),
            2 => Value::Int(p as u8 as i64),
            3 => Value::Int(p as u32 as i32 as i64),
            4 => Value::Int(p as u32 as i64),
            5 | 6 => Value::Int(p as i64),
            7 => Value::Float(half_to_f32(p as u16) as f64),
            // Floats and doubles that fit are inlined as f32 bits.
            8 | 9 => Value::Float(f32::from_bits(p as u32) as f64),
            10 => Value::String(self.string(p as u32)?),
            11 => Value::Token(self.token(p as u32)?.to_string()),
            12 => Value::Asset(self.token(p as u32)?.to_string()),
            // Matrices with small integer diagonals: the diagonal as i8.
            13..=15 => {
                let n = (ty - 11) as usize; // 2, 3, 4
                let mut m = [0.0; 16];
                for i in 0..4 {
                    m[i * 4 + i] = if i < n { bytes[i] as i8 as f64 } else { 1.0 };
                }
                Value::Matrix4(m)
            }
            // Vectors with small integer components: each as i8.
            19..=30 => {
                let (n, _) = vec_shape(ty);
                vector(&bytes[..n].iter().map(|&b| b as i8 as f64).collect::<Vec<_>>())
            }
            42 => Value::Specifier(match p {
                1 => Specifier::Over,
                2 => Specifier::Class,
                _ => Specifier::Def,
            }),
            43 => Value::Token(if p == 0 { "public" } else { "private" }.into()),
            44 => Value::Token(if p == 0 { "varying" } else { "uniform" }.into()),
            51 => Value::Blocked,
            56 => Value::Float(f32::from_bits(p as u32) as f64),
            _ => Value::Other("unsupported inline type"),
        })
    }

    fn array(&self, ty: u8, payload: u64, compressed: bool) -> Result<Value> {
        if payload == 0 {
            return Ok(empty_array(ty));
        }
        let mut c = self.at(payload);
        Ok(match ty {
            1 => {
                let n = self.array_len(&mut c, 1)?;
                Value::BoolArray(c.take(n)?.iter().map(|&b| b != 0).collect())
            }
            2..=4 => {
                let n = self.array_len(&mut c, if compressed { 0 } else { 4 })?;
                let ints = if compressed && n as u64 >= MIN_COMPRESSED_ARRAY {
                    compressed_ints32(&mut c, n, &self.limits)?
                } else {
                    (0..n).map(|_| c.u32().map(|v| v as i32)).collect::<Result<_>>()?
                };
                Value::IntArray(ints)
            }
            5 | 6 => {
                let n = self.array_len(&mut c, if compressed { 0 } else { 8 })?;
                let ints = if compressed && n as u64 >= MIN_COMPRESSED_ARRAY {
                    compressed_ints64(&mut c, n, &self.limits)?
                } else {
                    (0..n).map(|_| c.u64().map(|v| v as i64)).collect::<Result<_>>()?
                };
                Value::Int64Array(ints)
            }
            7..=9 => {
                let comp = match ty {
                    7 => Comp::Half,
                    8 => Comp::F32,
                    _ => Comp::F64,
                };
                let n = self.array_len(&mut c, if compressed { 0 } else { comp.size() })?;
                let values: Vec<f64> = if compressed && n as u64 >= MIN_COMPRESSED_ARRAY {
                    match c.u8()? {
                        // Stored as integers.
                        b'i' => compressed_ints32(&mut c, n, &self.limits)?.into_iter().map(f64::from).collect(),
                        // A table of distinct values plus indices into it.
                        b't' => {
                            let lut_len = c.u32()? as usize;
                            if lut_len.saturating_mul(comp.size()) > c.remaining() {
                                return Err(Error::Format("value table runs past the file".into()));
                            }
                            let lut = (0..lut_len).map(|_| comp.read(&mut c)).collect::<Result<Vec<_>>>()?;
                            let idx = compressed_ints32(&mut c, n, &self.limits)?;
                            idx.iter().map(|&i| lut.get(i as usize).copied().ok_or_else(|| Error::Format("table index out of range".into()))).collect::<Result<_>>()?
                        }
                        _ => return Err(Error::Format("unknown float array encoding".into())),
                    }
                } else {
                    (0..n).map(|_| comp.read(&mut c)).collect::<Result<_>>()?
                };
                if ty == 9 { Value::DoubleArray(values) } else { Value::FloatArray(values.into_iter().map(|v| v as f32).collect()) }
            }
            10 => {
                let n = self.array_len(&mut c, 4)?;
                Value::StringArray((0..n).map(|_| self.string(c.u32()?)).collect::<Result<_>>()?)
            }
            11 | 12 => {
                let n = self.array_len(&mut c, 4)?;
                let t: Vec<String> = (0..n).map(|_| self.token(c.u32()?).map(str::to_string)).collect::<Result<_>>()?;
                if ty == 11 { Value::TokenArray(t) } else { Value::AssetArray(t) }
            }
            15 => {
                let n = self.array_len(&mut c, 128)?;
                Value::Matrix4Array((0..n).map(|_| (0..16).map(|_| c.f64()).collect::<Result<Vec<_>>>().map(|v| v.try_into().unwrap())).collect::<Result<_>>()?)
            }
            16..=18 => {
                let comp = [Comp::F64, Comp::F32, Comp::Half][(ty - 16) as usize];
                let n = self.array_len(&mut c, comp.size() * 4)?;
                let q = (0..n).map(|_| Ok([comp.read(&mut c)? as f32, comp.read(&mut c)? as f32, comp.read(&mut c)? as f32, comp.read(&mut c)? as f32]));
                Value::QuatArray(q.collect::<Result<_>>()?)
            }
            19..=30 => {
                let (dims, comp) = vec_shape(ty);
                let n = self.array_len(&mut c, dims * comp.size())?;
                let mut flat = Vec::with_capacity(n * dims);
                for _ in 0..n * dims {
                    flat.push(comp.read(&mut c)? as f32);
                }
                match dims {
                    2 => Value::Vec2Array(flat.as_chunks::<2>().0.iter().map(|v| [v[0], v[1]]).collect()),
                    3 => Value::Vec3Array(flat.as_chunks::<3>().0.iter().map(|v| [v[0], v[1], v[2]]).collect()),
                    _ => Value::Vec4Array(flat.as_chunks::<4>().0.iter().map(|v| [v[0], v[1], v[2], v[3]]).collect()),
                }
            }
            _ => Value::Other("unsupported array type"),
        })
    }

    /// A list op: a header byte with a bit per list present (1 explicit, 2 added,
    /// 3 deleted, 4 ordered, 5 prepended, 6 appended), then the lists, each a count and
    /// items, written explicit, added, prepended, appended, deleted, ordered. Returns
    /// what it adds.
    fn list_op<T>(&self, c: &mut Cursor<'_>, item: impl Fn(&Self, &mut Cursor<'_>) -> Result<T>) -> Result<Vec<T>> {
        let header = c.u8()?;
        let mut out = Vec::new();
        for (bit, keep) in [(1 << 1, true), (1 << 2, true), (1 << 5, true), (1 << 6, true), (1 << 3, false), (1 << 4, false)] {
            if header & bit == 0 {
                continue;
            }
            let n = c.count(4, &self.limits)?;
            for _ in 0..n {
                let v = item(self, c)?;
                if keep {
                    out.push(v);
                }
            }
        }
        Ok(out)
    }

    /// Time samples: a relative jump to the times (a ValueRep of doubles), then a jump
    /// to the value count and one ValueRep per sample.
    fn time_samples(&self, offset: u64, depth: u32) -> Result<Value> {
        let mut c = self.at(offset);
        let jump = c.u64()? as i64;
        let mut c = Cursor::new(self.bytes, (offset as i64 + jump) as usize);
        let times = match self.value(c.u64()?, depth + 1)? {
            Value::DoubleArray(t) => t,
            _ => return Err(Error::Format("time samples without times".into())),
        };
        let here = c.pos;
        let jump = c.u64()? as i64;
        let mut c = Cursor::new(self.bytes, (here as i64 + jump) as usize);
        let n = c.count(8, &self.limits)?;
        if n != times.len() {
            return Err(Error::Format("time sample count mismatch".into()));
        }
        let reps = (0..n).map(|_| c.u64()).collect::<Result<Vec<_>>>()?;
        let values = reps.into_iter().map(|r| self.value(r, depth + 1)).collect::<Result<Vec<_>>>()?;
        Ok(Value::TimeSamples(times.into_iter().zip(values).collect()))
    }
}

fn quat(v: Vec<f64>) -> Value {
    // Stored imaginary first, then real: already (x, y, z, w).
    Value::Quat([v[0], v[1], v[2], v[3]])
}

fn vector(v: &[f64]) -> Value {
    match v.len() {
        2 => Value::Vec2([v[0], v[1]]),
        3 => Value::Vec3([v[0], v[1], v[2]]),
        _ => Value::Vec4([v[0], v[1], v[2], v[3]]),
    }
}

fn empty_array(ty: u8) -> Value {
    match ty {
        1 => Value::BoolArray(Vec::new()),
        2..=4 => Value::IntArray(Vec::new()),
        5 | 6 => Value::Int64Array(Vec::new()),
        9 => Value::DoubleArray(Vec::new()),
        7 | 8 => Value::FloatArray(Vec::new()),
        10 => Value::StringArray(Vec::new()),
        11 => Value::TokenArray(Vec::new()),
        12 => Value::AssetArray(Vec::new()),
        _ => match vec_shape(ty).0 {
            2 => Value::Vec2Array(Vec::new()),
            3 => Value::Vec3Array(Vec::new()),
            _ => Value::Vec4Array(Vec::new()),
        },
    }
}

#[derive(Clone, Copy)]
enum Comp {
    F64,
    F32,
    Half,
    I32,
}

impl Comp {
    fn size(self) -> usize {
        match self {
            Comp::F64 => 8,
            Comp::F32 | Comp::I32 => 4,
            Comp::Half => 2,
        }
    }

    fn read(self, c: &mut Cursor<'_>) -> Result<f64> {
        Ok(match self {
            Comp::F64 => c.f64()?,
            Comp::F32 => c.f32()? as f64,
            Comp::Half => half_to_f32(c.u16()?) as f64,
            Comp::I32 => c.u32()? as i32 as f64,
        })
    }
}

/// (components, component type) of the Vec2d … Vec4i types (19–30).
fn vec_shape(ty: u8) -> (usize, Comp) {
    let i = ty.saturating_sub(19) as usize;
    let dims = [2, 3, 4][(i / 4).min(2)];
    let comp = [Comp::F64, Comp::F32, Comp::Half, Comp::I32][i % 4];
    (dims, comp)
}

/// Paths as a tree walk: for each entry its path index, its name token (negative for
/// a property), and a jump saying whether a child, a sibling or both follow.
fn read_paths(c: &mut Cursor<'_>, total: usize, tokens: &[String], limits: &Limits) -> Result<Vec<Option<String>>> {
    if total > limits.max_elements {
        return Err(Error::Limit(format!("{total} paths")));
    }
    let n = c.u64()? as usize;
    let indices = compressed_ints32(c, n, limits)?;
    let names = compressed_ints32(c, n, limits)?;
    let jumps = compressed_ints32(c, n, limits)?;
    let mut paths: Vec<Option<String>> = vec![None; total];
    // Explicit stack (Pixar's reader recurses on siblings, which a crafted file could
    // make arbitrarily deep).
    let mut stack: Vec<(usize, Option<String>)> = vec![(0, None)];
    let mut steps = 0usize;
    while let Some((mut cur, mut parent)) = stack.pop() {
        loop {
            steps += 1;
            if steps > n.saturating_mul(2) + 2 {
                return Err(Error::Format("path tree loops".into()));
            }
            let this = cur;
            cur += 1;
            let (Some(&index), Some(&jump)) = (indices.get(this), jumps.get(this)) else {
                return Err(Error::Format("path tree runs past its data".into()));
            };
            let path = match &parent {
                None => "/".to_string(),
                Some(p) => {
                    let token = *names.get(this).ok_or_else(|| Error::Format("path name missing".into()))?;
                    let name = tokens.get(token.unsigned_abs() as usize).ok_or_else(|| Error::Format("path token out of range".into()))?;
                    if token < 0 {
                        format!("{p}.{name}")
                    } else if name.starts_with('{') {
                        // A variant selection attaches without a slash: /Prim{set=sel}.
                        format!("{p}{name}")
                    } else if p == "/" {
                        format!("/{name}")
                    } else {
                        format!("{p}/{name}")
                    }
                }
            };
            let slot = paths.get_mut(index as usize).ok_or_else(|| Error::Format("path index out of range".into()))?;
            *slot = Some(path.clone());
            let has_child = jump > 0 || jump == -1;
            let has_sibling = jump >= 0;
            if has_child {
                if has_sibling {
                    stack.push(((this as i64 + jump as i64) as usize, parent.clone()));
                }
                parent = Some(path);
            }
            if !(has_child || has_sibling) {
                break;
            }
        }
    }
    Ok(paths)
}

/// Pixar's chunked LZ4 wrapper (TfFastCompression): a chunk count byte; 0 means one
/// raw LZ4 block follows, otherwise each chunk is an i32 size and a block.
fn decompress(src: &[u8], size: usize, limits: &Limits) -> Result<Vec<u8>> {
    if size > limits.max_decompressed {
        return Err(Error::Limit(format!("{size} bytes after decompression")));
    }
    let mut out = vec![0u8; size];
    let (&chunks, mut rest) = src.split_first().ok_or_else(|| Error::Format("empty compressed block".into()))?;
    let bad = |e: lz4_flex::block::DecompressError| Error::Format(format!("LZ4: {e}"));
    if chunks == 0 {
        let n = lz4_flex::block::decompress_into(rest, &mut out).map_err(bad)?;
        out.truncate(n);
        return Ok(out);
    }
    let mut written = 0;
    for _ in 0..chunks {
        let (len, tail) = rest.split_at_checked(4).ok_or_else(|| Error::Format("truncated chunk".into()))?;
        let len = i32::from_le_bytes(len.try_into().unwrap()).max(0) as usize;
        let (chunk, tail) = tail.split_at_checked(len).ok_or_else(|| Error::Format("truncated chunk".into()))?;
        written += lz4_flex::block::decompress_into(chunk, &mut out[written..]).map_err(bad)?;
        rest = tail;
    }
    out.truncate(written);
    Ok(out)
}

/// A u64 compressed size, then `n` 32-bit integers in Pixar's integer compression.
fn compressed_ints32(c: &mut Cursor<'_>, n: usize, limits: &Limits) -> Result<Vec<i32>> {
    Ok(compressed_ints(c, n, 4, limits)?.into_iter().map(|v| v as i32).collect())
}

fn compressed_ints64(c: &mut Cursor<'_>, n: usize, limits: &Limits) -> Result<Vec<i64>> {
    compressed_ints(c, n, 8, limits)
}

/// After LZ4: the most common delta, then 2 bits per value (0: the common delta,
/// 1/2/3: a delta of 1/2/4 bytes, or 2/4/8 for 64-bit), then those deltas. Values
/// are running sums of the deltas.
fn compressed_ints(c: &mut Cursor<'_>, n: usize, width: usize, limits: &Limits) -> Result<Vec<i64>> {
    if n > limits.max_elements {
        return Err(Error::Limit(format!("{n} integers")));
    }
    let packed = c.u64()? as usize;
    let src = c.take(packed)?;
    if n == 0 {
        return Ok(Vec::new());
    }
    let codes_len = (n * 2).div_ceil(8);
    let buf = decompress(src, width + codes_len + n * width, limits)?;
    let mut d = Cursor::new(&buf, 0);
    let read = |d: &mut Cursor<'_>, bytes: usize| -> Result<i64> {
        Ok(match bytes {
            1 => d.u8()? as i8 as i64,
            2 => d.u16()? as i16 as i64,
            4 => d.u32()? as i32 as i64,
            _ => d.u64()? as i64,
        })
    };
    let common = read(&mut d, width)?;
    let codes = d.take(codes_len)?.to_vec();
    let sizes = if width == 4 { [0, 1, 2, 4] } else { [0, 2, 4, 8] };
    let mut out = Vec::with_capacity(n);
    let mut prev: i64 = 0;
    for i in 0..n {
        let code = (codes[i / 4] >> ((i % 4) * 2)) & 3;
        let delta = if code == 0 { common } else { read(&mut d, sizes[code as usize])? };
        prev = prev.wrapping_add(delta);
        out.push(if width == 4 { prev as i32 as i64 } else { prev });
    }
    Ok(out)
}

/// Bounds-checked little-endian reads; every read past the end is an error.
pub(crate) struct Cursor<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(b: &'a [u8], pos: usize) -> Self {
        Self { b, pos }
    }

    fn remaining(&self) -> usize {
        self.b.len().saturating_sub(self.pos)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.b.len()).ok_or_else(|| Error::Format("unexpected end of file".into()))?;
        let s = &self.b[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }

    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.u64()?))
    }

    /// A u64 element count whose elements (of `elem` bytes) must fit in the file.
    fn count(&mut self, elem: usize, limits: &Limits) -> Result<usize> {
        let n = self.u64()?;
        if n > limits.max_elements as u64 || (n as usize).saturating_mul(elem) > self.remaining() {
            return Err(Error::Format(format!("count {n} runs past the file")));
        }
        Ok(n as usize)
    }
}
