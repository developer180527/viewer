use std::sync::Arc;

/// A field or attribute value. Scalars and small vectors keep full precision (f64);
/// arrays, which can be huge (points, normals), are stored as f32/i32 like the files,
/// and shared: cloning one (composition does, per instance) copies a pointer, and two
/// prims with the same `Arc` hold the same data.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Token(String),
    /// An asset path (`@textures/wood.png@`), unresolved.
    Asset(String),
    Vec2([f64; 2]),
    Vec3([f64; 3]),
    Vec4([f64; 4]),
    /// Imaginary part first: (x, y, z, w).
    Quat([f64; 4]),
    /// Row-major, as USD stores it (row vectors: translation in the last row).
    Matrix4([f64; 16]),
    BoolArray(Arc<[bool]>),
    IntArray(Arc<[i32]>),
    Int64Array(Arc<[i64]>),
    FloatArray(Arc<[f32]>),
    DoubleArray(Arc<[f64]>),
    Vec2Array(Arc<[[f32; 2]]>),
    Vec3Array(Arc<[[f32; 3]]>),
    Vec4Array(Arc<[[f32; 4]]>),
    QuatArray(Arc<[[f32; 4]]>),
    Matrix4Array(Arc<[[f64; 16]]>),
    TokenArray(Vec<String>),
    StringArray(Vec<String>),
    AssetArray(Vec<String>),
    /// Relationship targets, connections, path vectors.
    Paths(Vec<String>),
    /// `references` and `payload`: what they bring in.
    References(Vec<Reference>),
    Dictionary(Vec<(String, Value)>),
    /// (time, value), in time order.
    TimeSamples(Vec<(f64, Value)>),
    Specifier(Specifier),
    /// `None` in a file: an explicitly blocked value.
    Blocked,
    /// A type this crate doesn't decode, by name.
    Other(&'static str),
}

/// A reference or payload: a layer (`asset`, empty for one in the same layer stack)
/// and the prim in it (`None`: the layer's `defaultPrim`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Reference {
    pub asset: String,
    pub prim: Option<String>,
}

/// Closed on purpose: USD's three specifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Specifier {
    Def,
    Over,
    Class,
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) | Value::Token(s) | Value::Asset(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Value::Float(f) => Some(f),
            Value::Int(i) => Some(i as f64),
            Value::Bool(b) => Some(b as u8 as f64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match *self {
            Value::Bool(b) => Some(b),
            Value::Int(i) => Some(i != 0),
            _ => None,
        }
    }

    /// A 3-vector (or a scalar repeated), e.g. a colour that may be written either way.
    pub fn as_vec3(&self) -> Option<[f64; 3]> {
        match *self {
            Value::Vec3(v) => Some(v),
            Value::Vec4(v) => Some([v[0], v[1], v[2]]),
            Value::Float(f) => Some([f; 3]),
            _ => None,
        }
    }

    pub fn as_vec4(&self) -> Option<[f64; 4]> {
        match *self {
            Value::Vec4(v) => Some(v),
            Value::Vec3(v) => Some([v[0], v[1], v[2], 1.0]),
            _ => None,
        }
    }

    pub fn as_tokens(&self) -> Option<&[String]> {
        match self {
            Value::TokenArray(t) | Value::StringArray(t) | Value::Paths(t) => Some(t),
            _ => None,
        }
    }

    pub fn as_paths(&self) -> Option<&[String]> {
        match self {
            Value::Paths(p) => Some(p),
            _ => None,
        }
    }

    /// Integer arrays of either width, as i64.
    pub fn as_ints(&self) -> Option<Vec<i64>> {
        match self {
            Value::IntArray(v) => Some(v.iter().map(|&i| i as i64).collect()),
            Value::Int64Array(v) => Some(v.to_vec()),
            _ => None,
        }
    }

    pub fn as_vec3_array(&self) -> Option<&[[f32; 3]]> {
        match self {
            Value::Vec3Array(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_vec2_array(&self) -> Option<&[[f32; 2]]> {
        match self {
            Value::Vec2Array(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_matrix4(&self) -> Option<[f64; 16]> {
        match *self {
            Value::Matrix4(m) => Some(m),
            _ => None,
        }
    }
}

/// IEEE half to f32.
pub(crate) fn half_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) as u32) << 31;
    let exp = ((h >> 10) & 0x1f) as u32;
    let frac = (h & 0x3ff) as u32;
    let bits = match exp {
        0 if frac == 0 => sign,
        0 => {
            // Subnormal: normalise.
            let mut e = 127 - 15 + 1;
            let mut f = frac;
            while f & 0x400 == 0 {
                f <<= 1;
                e -= 1;
            }
            sign | ((e as u32) << 23) | ((f & 0x3ff) << 13)
        }
        0x1f => sign | 0x7f80_0000 | (frac << 13),
        _ => sign | ((exp + 127 - 15) << 23) | (frac << 13),
    };
    f32::from_bits(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves() {
        assert_eq!(half_to_f32(0x3c00), 1.0);
        assert_eq!(half_to_f32(0xc000), -2.0);
        assert_eq!(half_to_f32(0x7bff), 65504.0);
        assert_eq!(half_to_f32(0x0001), 2f32.powi(-24));
        assert!(half_to_f32(0x7e00).is_nan());
    }
}
