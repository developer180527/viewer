//! Normals for triangle meshes that come without them. Runs on the loader's thread
//! (inside [`SceneBuilder::add_mesh`](crate::SceneBuilder::add_mesh)), never on the
//! render thread, so a huge normal-less scan can't stall a frame.

use std::collections::HashMap;

use glam::{Vec2, Vec3, Vec4};

use crate::{Indices, MeshData, Stream, Topology};

/// Faces meeting at more than this angle keep a hard edge (Blender's auto smooth).
const HARD_EDGE_COS: f32 = 0.5; // cos 60°

/// Adds normals to a triangle mesh that has none; other meshes are returned as they are.
///
/// Triangles are smoothed across gentle edges and kept hard across sharp ones, so a scan
/// looks smooth and a CAD part keeps its edges. A vertex on a hard edge is split: the
/// copy is appended, so when nothing splits every stream (borrowed ones included) is
/// kept untouched and only a normal stream is added. Non-indexed meshes come out flat.
///
/// Indices must already be range-checked ([`MeshData::validate`]).
pub(crate) fn generate(mut mesh: MeshData) -> MeshData {
    if mesh.normals.is_some() || mesh.topology != Topology::Triangles {
        return mesh;
    }
    let positions = mesh.positions.as_slice();
    let face = |a: usize, b: usize, c: usize| {
        let n = (positions[b] - positions[a]).cross(positions[c] - positions[a]);
        if n.is_finite() { n } else { Vec3::ZERO }
    };
    let indices: Vec<u32> = match &mesh.indices {
        Indices::None => {
            let mut normals = vec![Vec3::ZERO; positions.len()];
            for t in 0..positions.len() / 3 {
                normals[t * 3..t * 3 + 3].fill(face(t * 3, t * 3 + 1, t * 3 + 2).normalize_or_zero());
            }
            mesh.normals = Some(normals.into());
            return mesh;
        }
        Indices::U16(s) => s.as_slice().iter().map(|&i| i as u32).collect(),
        Indices::U32(s) => s.as_slice().to_vec(),
    };

    let vertices = positions.len();
    let tris = indices.len() / 3;
    let corner = |t: usize, k: usize| indices[t * 3 + k] as usize;
    let faces: Vec<Vec3> = (0..tris).map(|t| face(corner(t, 0), corner(t, 1), corner(t, 2))).collect();
    let units: Vec<Vec3> = faces.iter().map(|n| n.normalize_or_zero()).collect();
    // Triangles around each vertex, as one flat list (CSR).
    let mut start = vec![0u32; vertices + 1];
    indices.iter().for_each(|&v| start[v as usize + 1] += 1);
    (0..vertices).for_each(|v| start[v + 1] += start[v]);
    let mut fill = start.clone();
    let mut around = vec![0u32; indices.len()];
    for (c, &v) in indices.iter().enumerate() {
        around[fill[v as usize] as usize] = (c / 3) as u32;
        fill[v as usize] += 1;
    }

    let mut normals = vec![Vec3::ZERO; vertices];
    let mut assigned = vec![false; vertices];
    // Split copies: (source vertex, normal), appended after the original vertices.
    let mut extra: Vec<(u32, Vec3)> = Vec::new();
    let mut variants: HashMap<(u32, [i32; 3]), u32> = HashMap::new();
    // Only materialised once the first vertex splits.
    let mut new_indices: Option<Vec<u32>> = None;
    for (c, &v) in indices.iter().enumerate() {
        let own = units[c / 3];
        let incident = &around[start[v as usize] as usize..start[v as usize + 1] as usize];
        let n = incident
            .iter()
            .filter(|&&t| own == Vec3::ZERO || units[t as usize].dot(own) >= HARD_EDGE_COS)
            .fold(Vec3::ZERO, |acc, &t| acc + faces[t as usize])
            .normalize_or_zero();
        let vi = v as usize;
        let index = if !assigned[vi] {
            assigned[vi] = true;
            normals[vi] = n;
            v
        } else if normals[vi].dot(n) > 0.9999 || (normals[vi] == Vec3::ZERO && n == Vec3::ZERO) {
            v
        } else {
            *variants.entry((v, quantize(n))).or_insert_with(|| {
                extra.push((v, n));
                (vertices + extra.len() - 1) as u32
            })
        };
        if index != v && new_indices.is_none() {
            new_indices = Some(indices[..c].to_vec());
        }
        if let Some(out) = &mut new_indices {
            out.push(index);
        }
    }

    let Some(new_indices) = new_indices else {
        mesh.normals = Some(normals.into());
        return mesh;
    };
    let total = vertices + extra.len();
    mesh.positions = extend::<Vec3>(positions, &extra);
    normals.extend(extra.iter().map(|&(_, n)| n));
    mesh.normals = Some(normals.into());
    mesh.uvs = mesh.uvs.as_ref().map(|s| extend::<Vec2>(s.as_slice(), &extra));
    mesh.tangents = mesh.tangents.as_ref().map(|s| extend::<Vec4>(s.as_slice(), &extra));
    mesh.colors = mesh.colors.as_ref().map(|s| extend::<Vec4>(s.as_slice(), &extra));
    mesh.indices = if matches!(mesh.indices, Indices::U16(_)) && total <= u16::MAX as usize + 1 {
        Indices::U16(new_indices.iter().map(|&i| i as u16).collect::<Vec<_>>().into())
    } else {
        Indices::U32(new_indices.into())
    };
    mesh
}

fn extend<T: bytemuck::Pod>(src: &[T], extra: &[(u32, Vec3)]) -> Stream<T> {
    src.iter().copied().chain(extra.iter().map(|&(v, _)| src[v as usize])).collect::<Vec<_>>().into()
}

fn quantize(n: Vec3) -> [i32; 3] {
    (n * 4096.0).round().as_ivec3().to_array()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indexed(positions: Vec<Vec3>, indices: Vec<u32>) -> MeshData {
        MeshData::new(positions).with_indices(Indices::U32(indices.into()))
    }

    fn index_list(m: &MeshData) -> Vec<u32> {
        match &m.indices {
            Indices::None => Vec::new(),
            Indices::U16(s) => s.as_slice().iter().map(|&i| i as u32).collect(),
            Indices::U32(s) => s.as_slice().to_vec(),
        }
    }

    #[test]
    fn hard_edges_split_vertices_and_flat_surfaces_share_them() {
        // Two coplanar triangles: one smooth surface, nothing to split or copy.
        let quad = indexed(vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::ONE.with_z(0.0)], vec![0, 1, 2, 2, 1, 3]);
        let g = generate(quad);
        assert_eq!(g.positions.len(), 4);
        assert_eq!(index_list(&g), [0, 1, 2, 2, 1, 3], "indices kept as they were");
        assert!(g.normals.unwrap().as_slice().iter().all(|n| (*n - Vec3::Z).length() < 1e-5));

        // A 90° fold: the two shared vertices split, each side stays flat.
        let mut fold = indexed(vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::Z], vec![0, 1, 2, 1, 0, 3]);
        fold.uvs = Some(vec![Vec2::ZERO, Vec2::X, Vec2::Y, Vec2::ONE].into());
        let g = generate(fold);
        assert_eq!(g.positions.len(), 6);
        assert_eq!(g.uvs.as_ref().unwrap().len(), 6, "other streams follow the split");
        let idx = index_list(&g);
        let n = g.normals.unwrap();
        assert!(n.as_slice()[idx[0] as usize].dot(n.as_slice()[idx[3] as usize]).abs() < 1e-5);
        assert_eq!(g.uvs.unwrap().as_slice()[idx[3] as usize], Vec2::X, "a split copy keeps its source's uv");

        // A gentle bend (under 60°) is smoothed across: shared vertices stay shared.
        let bend = indexed(vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::new(0.5, -1.0, 0.3)], vec![0, 1, 2, 1, 0, 3]);
        assert_eq!(generate(bend).positions.len(), 4);
    }

    #[test]
    fn degenerate_triangles_and_other_topologies_dont_produce_nan() {
        let mut lines = MeshData::new(vec![Vec3::ZERO, Vec3::X]);
        lines.topology = Topology::Lines;
        assert!(generate(lines).normals.is_none(), "lines and points are left alone");

        let sliver = indexed(vec![Vec3::ZERO, Vec3::X, Vec3::X * 2.0], vec![0, 1, 2]);
        assert!(generate(sliver).normals.unwrap().as_slice().iter().all(|n| n.is_finite()));
    }
}
