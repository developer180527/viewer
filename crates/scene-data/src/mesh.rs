use glam::{Mat4, Vec2, Vec3, Vec4};

use crate::error::check;
use crate::{Error, SceneLimits, Stream};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Topology {
    Triangles,
    Lines,
    Points,
}

impl Topology {
    fn vertices_per_primitive(self) -> usize {
        match self {
            Topology::Triangles => 3,
            Topology::Lines => 2,
            Topology::Points => 1,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Indices {
    /// Non-indexed: vertices are taken in order.
    None,
    U16(Stream<u16>),
    U32(Stream<u32>),
}

/// Axis-aligned bounding box. [`Aabb::EMPTY`] contains nothing and is the identity for
/// [`union`](Aabb::union).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub const EMPTY: Aabb = Aabb { min: Vec3::INFINITY, max: Vec3::NEG_INFINITY };

    pub fn new(min: Vec3, max: Vec3) -> Self {
        Self { min, max }
    }

    /// Bounds of the finite points; NaN and infinite positions are ignored.
    pub fn from_points(points: &[Vec3]) -> Self {
        points.iter().filter(|p| p.is_finite()).fold(Self::EMPTY, |b, &p| Self { min: b.min.min(p), max: b.max.max(p) })
    }

    pub fn is_empty(&self) -> bool {
        !(self.min.cmple(self.max).all())
    }

    /// Finite and not inverted: safe to frame a camera on.
    pub fn is_valid(&self) -> bool {
        !self.is_empty() && self.min.is_finite() && self.max.is_finite()
    }

    pub fn union(self, other: Aabb) -> Aabb {
        Aabb { min: self.min.min(other.min), max: self.max.max(other.max) }
    }

    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn size(&self) -> Vec3 {
        self.max - self.min
    }

    /// Bounds of this box after `m`, from its eight corners.
    pub fn transformed(&self, m: &Mat4) -> Aabb {
        if self.is_empty() {
            return *self;
        }
        let (a, b) = (self.min, self.max);
        let corners = [
            Vec3::new(a.x, a.y, a.z),
            Vec3::new(b.x, a.y, a.z),
            Vec3::new(a.x, b.y, a.z),
            Vec3::new(b.x, b.y, a.z),
            Vec3::new(a.x, a.y, b.z),
            Vec3::new(b.x, a.y, b.z),
            Vec3::new(a.x, b.y, b.z),
            Vec3::new(b.x, b.y, b.z),
        ];
        Aabb::from_points(&corners.map(|c| m.transform_point3(c)))
    }
}

/// One mesh's vertex streams. Missing triangle normals are generated when the mesh is
/// added to a scene (on the loader's thread); tangents will be generated on upload.
#[derive(Debug, Clone)]
pub struct MeshData {
    pub positions: Stream<Vec3>,
    pub normals: Option<Stream<Vec3>>,
    pub uvs: Option<Stream<Vec2>>,
    pub tangents: Option<Stream<Vec4>>,
    pub colors: Option<Stream<Vec4>>,
    pub indices: Indices,
    pub topology: Topology,
}

impl MeshData {
    /// Non-indexed triangles with only positions; set the other fields directly.
    pub fn new(positions: impl Into<Stream<Vec3>>) -> Self {
        Self {
            positions: positions.into(),
            normals: None,
            uvs: None,
            tangents: None,
            colors: None,
            indices: Indices::None,
            topology: Topology::Triangles,
        }
    }

    pub fn with_indices(mut self, indices: Indices) -> Self {
        self.indices = indices;
        self
    }

    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    /// Indices, or vertices for a non-indexed mesh: what a draw call walks.
    pub fn element_count(&self) -> usize {
        match &self.indices {
            Indices::None => self.positions.len(),
            Indices::U16(s) => s.len(),
            Indices::U32(s) => s.len(),
        }
    }

    pub fn byte_len(&self) -> u64 {
        let opt = |n: Option<u64>| n.unwrap_or(0);
        self.positions.byte_len()
            + opt(self.normals.as_ref().map(Stream::byte_len))
            + opt(self.uvs.as_ref().map(Stream::byte_len))
            + opt(self.tangents.as_ref().map(Stream::byte_len))
            + opt(self.colors.as_ref().map(Stream::byte_len))
            + match &self.indices {
                Indices::None => 0,
                Indices::U16(s) => s.byte_len(),
                Indices::U32(s) => s.byte_len(),
            }
    }

    /// Checks everything the GPU would trust: stream lengths, whole primitives, every
    /// index in range, per-mesh limits. Returns the summary the scene keeps even after
    /// the data itself is released.
    pub fn validate(&self, limits: &SceneLimits) -> crate::Result<MeshInfo> {
        let vertices = self.positions.len();
        check("vertices", vertices as u64, limits.max_vertices)?;
        let same_len = |stream: &'static str, len: Option<usize>| match len {
            Some(len) if len != vertices => Err(Error::StreamLength { stream, len, expected: vertices }),
            _ => Ok(()),
        };
        same_len("normals", self.normals.as_ref().map(Stream::len))?;
        same_len("uvs", self.uvs.as_ref().map(Stream::len))?;
        same_len("tangents", self.tangents.as_ref().map(Stream::len))?;
        same_len("colors", self.colors.as_ref().map(Stream::len))?;

        let elements = self.element_count();
        if !elements.is_multiple_of(self.topology.vertices_per_primitive()) {
            return Err(Error::PartialPrimitive { count: elements, topology: self.topology });
        }
        let out_of_range = |index: u32| Error::IndexOutOfRange { index, vertices };
        match &self.indices {
            Indices::None => {}
            Indices::U16(s) => {
                if let Some(&i) = s.as_slice().iter().find(|&&i| i as usize >= vertices) {
                    return Err(out_of_range(i as u32));
                }
            }
            Indices::U32(s) => {
                if let Some(&i) = s.as_slice().iter().find(|&&i| i as usize >= vertices) {
                    return Err(out_of_range(i));
                }
            }
        }
        let primitives = (elements / self.topology.vertices_per_primitive()) as u64;
        check("triangles", primitives, limits.max_triangles)?;

        // Always measured, never taken from the file: stored min/max can be wrong, and
        // these bounds decide framing (loaders pass the file's for early framing through
        // `SceneBuilder::set_bounds` instead).
        let bounds = Aabb::from_points(self.positions.as_slice());
        Ok(MeshInfo { vertices, elements, primitives, topology: self.topology, bounds, bytes: self.byte_len() })
    }
}

/// What the scene remembers about a mesh after its data has been uploaded and freed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeshInfo {
    pub vertices: usize,
    pub elements: usize,
    pub primitives: u64,
    pub topology: Topology,
    pub bounds: Aabb,
    pub bytes: u64,
}
