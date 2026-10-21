//! Authored `.skate` world geometry → renderable scene.
//!
//! The draw-count architecture lives here (RFC 1 §1–§6). `.skate` vertices
//! already carry a per-vertex material index, so triangles of different
//! materials can share a mesh as long as the shader indexes its material table
//! with that attribute. Geometry is therefore grouped by **space and render
//! class**, not by material, and draw count stops scaling with material count.
//!
//! Collision is a separate concern that happens to live in the same file
//! because it reads the same package. It is byte-for-byte the behaviour physics
//! was validated against and must not be "improved" here.
use bevy::{
    asset::RenderAssetUsages,
    camera::primitives::Aabb,
    mesh::{Indices, MeshVertexAttribute, PrimitiveTopology, VertexFormat},
    prelude::*,
};
use skate_core::{
    math::Vector3,
    physics::{
        board_world::{
            BoardWorld, WorldTriangle,
            query_metadata::{Bounds, QueryMesh, QueryMetadata, QueryPool},
        },
        collision::TriangleFeature,
        contact::RetailContactMaterial,
        drive_frames::RetailAffineTransform,
    },
};
use skate_data::skate_map::SkateMap;
use std::collections::HashMap;

use crate::map_render::{AssetSink, SceneCommands};
use crate::retail_render::{MaterialTable, RenderClass, WorldMaterial};

/// Per-vertex index into the world material table.
///
/// `@interpolate(flat)` in the shader. Interpolating it would silently corrupt
/// material lookup across triangle interiors, which is the one failure mode of
/// this design that does not announce itself.
pub(crate) const ATTRIBUTE_MATERIAL_INDEX: MeshVertexAttribute =
    MeshVertexAttribute::new("MaterialIndex", 0x534B_4D49, VertexFormat::Uint32);

/// Triangles per spatial leaf. The single tuning dial trading draw count against
/// culling granularity (RFC 1 §5): ~1.9M Downtown triangles at this size gives
/// ~120 leaves.
const LEAF_TRIANGLE_BUDGET: usize = 16_384;

#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct SceneStats {
    pub leaves: usize,
    pub draws: usize,
    pub triangles: usize,
    pub slabs: usize,
}

// ---------------------------------------------------------------------------
// Render path
// ---------------------------------------------------------------------------

/// One triangle, reduced to what partitioning needs.
struct Triangle {
    indices: [u32; 3],
    centroid: Vec3,
    /// Slab owning this triangle's material; a leaf spanning two slabs must split.
    slab: u16,
    class: RenderClass,
}

pub(crate) fn spawn(
    map: &SkateMap,
    tuning: &crate::retail_render::MaterialTuning,
    environment: &crate::retail_sky::SkyEnvironment,
    commands: &mut SceneCommands,
    meshes: &mut impl AssetSink<Mesh>,
    materials: &mut impl AssetSink<WorldMaterial>,
    images: &mut impl AssetSink<Image>,
    buffers: &mut impl AssetSink<bevy::render::storage::ShaderStorageBuffer>,
) -> SceneStats {
    let _span = info_span!("spawn_world").entered();
    let table = MaterialTable::build(map, tuning, environment, materials, images, buffers);

    let mut triangles: Vec<Triangle> = Vec::with_capacity(map.geometry.indices.len() / 3);
    for tri in map.geometry.indices.chunks_exact(3) {
        let [a, b, c] = [tri[0], tri[1], tri[2]];
        // The format guarantees all three corners share a material, and the
        // loader enforces it, so corner 0 is authoritative. `Vertex::material`
        // is one-based with zero meaning "no material", matching the collision
        // path below and `map.materials` indexing everywhere else.
        let Some(source) = (map.geometry.vertices[a as usize].material as usize).checked_sub(1)
        else {
            continue;
        };
        let Some(entry) = table.entry(source) else { continue };
        let position = |i: u32| Vec3::from_array(map.geometry.vertices[i as usize].position);
        triangles.push(Triangle {
            indices: [a, b, c],
            centroid: (position(a) + position(b) + position(c)) / 3.0,
            slab: entry.slab,
            class: entry.class,
        });
    }

    // Partition by render class first: classes cannot share a draw (RFC 1 §4),
    // so splitting here keeps every leaf single-class for free.
    let mut stats = SceneStats { slabs: table.slab_count(), triangles: triangles.len(), ..default() };
    let mut buckets: HashMap<(RenderClass, u16), Vec<usize>> = HashMap::new();
    for (index, triangle) in triangles.iter().enumerate() {
        buckets.entry((triangle.class, triangle.slab)).or_default().push(index);
    }

    // Draws per render class. The class mix is what decides whether the draw
    // budget holds on a big map: spatial subdivision is a number we choose, but
    // every two-sided or blended material forces an extra bucket we do not.
    let mut per_class = [0usize; RenderClass::ALL.len()];
    for ((class, slab), mut members) in buckets {
        let leaves = partition(&mut members, &triangles);
        stats.leaves += leaves.len();
        per_class[class as usize] += leaves.len();
        for leaf in leaves {
            let mesh = merge(map, &table, &triangles, &leaf);
            let aabb = mesh.1;
            commands.spawn((
                Name::new(format!("world {class:?} slab {slab}")),
                Mesh3d(meshes.add(mesh.0)),
                MeshMaterial3d(table.material(slab, class)),
                Transform::default(),
                // Precomputed so Bevy's CalculateBounds never walks this mesh
                // (RFC 1 D4). The extents were already computed while merging.
                aabb,
            ));
            stats.draws += 1;
        }
    }

    spawn_lights(map, commands);
    eprintln!(
        "SKATE_RENDER_READY draws={} triangles={} slabs={} materials={} \
         opaque={} opaque_two_sided={} cutout={} cutout_two_sided={} \
         blended={} blended_two_sided={}",
        stats.draws,
        stats.triangles,
        stats.slabs,
        map.materials.len(),
        per_class[RenderClass::Opaque as usize],
        per_class[RenderClass::OpaqueTwoSided as usize],
        per_class[RenderClass::Cutout as usize],
        per_class[RenderClass::CutoutTwoSided as usize],
        per_class[RenderClass::Blended as usize],
        per_class[RenderClass::BlendedTwoSided as usize],
    );
    stats
}

/// Median-split over triangle centroids until every leaf fits the budget.
///
/// Sorting by the longest axis of the current extent keeps leaves roughly cubic,
/// which matters because leaf AABBs are what frustum culling tests.
fn partition(members: &mut Vec<usize>, triangles: &[Triangle]) -> Vec<Vec<usize>> {
    let mut pending = vec![std::mem::take(members)];
    let mut leaves = Vec::new();
    while let Some(mut group) = pending.pop() {
        if group.len() <= LEAF_TRIANGLE_BUDGET {
            leaves.push(group);
            continue;
        }
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for &i in &group {
            min = min.min(triangles[i].centroid);
            max = max.max(triangles[i].centroid);
        }
        let extent = max - min;
        let axis = if extent.x >= extent.y && extent.x >= extent.z {
            0
        } else if extent.y >= extent.z {
            1
        } else {
            2
        };
        group.sort_unstable_by(|&a, &b| {
            triangles[a].centroid[axis].total_cmp(&triangles[b].centroid[axis])
        });
        let half = group.split_off(group.len() / 2);
        pending.push(group);
        pending.push(half);
    }
    leaves
}

/// Concatenate a leaf's triangles into one mesh, preserving each vertex's
/// material index so the shader can still tell them apart.
fn merge(
    map: &SkateMap,
    table: &MaterialTable,
    triangles: &[Triangle],
    leaf: &[usize],
) -> (Mesh, Aabb) {
    let mut remap: HashMap<u32, u32> = HashMap::with_capacity(leaf.len() * 2);
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(leaf.len() * 2);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(leaf.len() * 2);
    let mut uv: Vec<[f32; 2]> = Vec::with_capacity(leaf.len() * 2);
    let mut lightmap_uv: Vec<[f32; 2]> = Vec::with_capacity(leaf.len() * 2);
    let mut decal_uv: Vec<[f32; 4]> = Vec::with_capacity(leaf.len() * 2);
    let mut material_index: Vec<u32> = Vec::with_capacity(leaf.len() * 2);
    let mut tangents: Vec<[f32; 4]> = Vec::with_capacity(leaf.len() * 2);
    let mut indices: Vec<u32> = Vec::with_capacity(leaf.len() * 3);
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);

    for &triangle in leaf {
        for source in triangles[triangle].indices {
            let next = positions.len() as u32;
            let index = *remap.entry(source).or_insert(next);
            if index == next {
                let v = &map.geometry.vertices[source as usize];
                let position = Vec3::from_array(v.position);
                min = min.min(position);
                max = max.max(position);
                positions.push(v.position);
                normals.push(v.normal);
                uv.push(v.uv);
                lightmap_uv.push(v.lightmap_uv);
                // Decal UVs ride in COLOR to match the retail shader, which
                // reads them from `color.xy`.
                let [du, dv] = v.decal_uv.unwrap_or_default();
                decal_uv.push([du, dv, 0.0, 1.0]);
                // One-based, as in the bucketing pass above. Triangles whose
                // material is absent were dropped there, so every vertex
                // reachable here has one.
                material_index
                    .push(table.slab_index(v.material.saturating_sub(1) as usize));
                tangents.push(tangent(v));
            }
            indices.push(index);
        }
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_1, lightmap_uv)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, decal_uv)
        .with_inserted_attribute(ATTRIBUTE_MATERIAL_INDEX, material_index)
        .with_inserted_attribute(Mesh::ATTRIBUTE_TANGENT, tangents);
    mesh.insert_indices(Indices::U32(indices));
    let aabb = Aabb::from_min_max(min, max);
    (mesh, aabb)
}

/// Decodes the authored tangent frame, which the format stores as a signed-byte
/// binormal plus a handedness byte, both scaled by 127.
///
/// A merged mesh spans many materials, so unlike a per-material mesh it cannot
/// choose between authored and generated tangents for the whole draw. Vertices
/// with no authored frame get a zero tangent instead, which both Bevy's
/// local-to-world helper and the shader read as "fall back to derivatives" —
/// a per-vertex decision rather than a per-mesh one.
fn tangent(v: &skate_data::skate_map::Vertex) -> [f32; 4] {
    let Some(frame) = v.tangent_frame else {
        return [0.0; 4];
    };
    let frame = frame.map(|b| (b as i8 as f32 / 127.).max(-1.));
    let binormal = Vec3::new(frame[0], frame[1], frame[2]);
    let tangent = binormal.cross(Vec3::from_array(v.normal)) * frame[3];
    [tangent.x, tangent.y, tangent.z, frame[3]]
}

fn spawn_lights(map: &SkateMap, commands: &mut SceneCommands) {
    for light in &map.lights {
        let position = Vec3::from_array(light.position);
        let color = Color::srgb(light.color[0], light.color[1], light.color[2]);
        match light.kind {
            0 => commands.spawn((
                PointLight {
                    color,
                    intensity: light.intensity,
                    range: light.range,
                    radius: light.radius,
                    // RFC 1 D5: no shadow casting anywhere in v1.
                    shadows_enabled: false,
                    ..default()
                },
                Transform::from_translation(position),
            )),
            1 => commands.spawn((
                SpotLight {
                    color,
                    intensity: light.intensity,
                    range: light.range,
                    radius: light.radius,
                    outer_angle: light.outer_cos.clamp(-1., 1.).acos(),
                    inner_angle: light.inner_cos.clamp(-1., 1.).acos(),
                    shadows_enabled: false,
                    ..default()
                },
                Transform::from_translation(position)
                    .looking_to(Vec3::from_array(light.direction), Vec3::Y),
            )),
            // Area lights are parsed but have no Bevy equivalent; validation
            // already warned about them.
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Validation and collision
//
// Everything below is carried over unchanged. Physics was validated against this
// exact behaviour, including the bit patterns and epsilons, and RFC 1 §10.1 puts
// it out of scope for the renderer rewrite.
// ---------------------------------------------------------------------------

pub(crate) fn validate_runtime(map: &SkateMap) -> Result<(), String> {
    let _span = info_span!("validate_map").entered();
    let archive = retail_archive(map)?;
    if map.geometry.collision.is_empty() && archive.is_none() {
        return Err("SKATE map has no collision geometry".into());
    }
    for triangle in map.geometry.collision.iter().filter(|_| archive.is_none()) {
        if let Some(edges) = triangle.native_edges {
            decode_native_edges(edges)?;
        }
    }
    if !map.doors.is_empty() {
        return Err(format!(
            "Map '{}' contains {} hinged doors. This imported game has no door body/controller adapter yet; refusing to drop their geometry or turn them into static walls.",
            map.name,
            map.doors.len()
        ));
    }
    for extension in &map.extensions {
        let tag = String::from_utf8_lossy(&extension.tag);
        if extension.tag == *b"RWCM" {
            continue;
        }
        if extension.tag == skate_data::trigger_volumes::EXTENSION_TAG {
            // Named trigger volumes (data only, never collision); see trigger_volumes.rs.
            if extension.schema != skate_data::trigger_volumes::EXTENSION_SCHEMA {
                return Err(format!("SKATE extension TVOL schema {} is not supported", extension.schema));
            }
            skate_data::trigger_volumes::parse(&extension.payload)?;
            continue;
        }
        if extension.tag == *b"MOBJ" {
            skate_data::skate_map::validate_static_objects(map, extension)?;
            continue;
        }
        if extension.tag == *b"SKYB" && extension.schema == 1 {
            eprintln!(
                "SKATE LIMITATION: SKYB retail sky retained; using the map horizon until its shader adapter is available."
            );
            continue;
        }
        if extension.tag != *b"WMET" && extension.tag != *b"WCFG" && extension.tag != *b"BMAT" {
            return Err(format!(
                "SKATE extension {tag} schema {} is decoded but its runtime adapter is not implemented. Refusing to silently omit potentially required world geometry.",
                extension.schema
            ));
        }
        eprintln!(
            "SKATE LIMITATION: {tag} extension retained; its runtime behavior is not connected."
        );
    }
    if map.textures.iter().any(|t| t.width == 0) {
        return Err(
            "SKATE contains external texture placeholders; supply a package with embedded textures"
                .into(),
        );
    }
    if !map.routes.is_empty() {
        eprintln!(
            "SKATE LIMITATION: {} NPC routes parsed; supplied game has no NPC controller.",
            map.routes.len()
        );
    }
    if map.lights.iter().any(|l| l.kind == 2) {
        eprintln!(
            "SKATE LIMITATION: area-light records retained; Bevy adapter currently renders point and spot lights only."
        );
    }
    eprintln!(
        "SKATE_MAP_LOADED name={:?} version={} render_triangles={} collision_triangles={} textures={} spawn={:?}",
        map.name,
        map.version,
        map.geometry.indices.len() / 3,
        map.geometry.collision.len(),
        map.textures.len(),
        map.spawn
    );
    Ok(())
}

/// TU3 ClusteredMesh::GetUnitVolumes (82AC8A68): fdivs then fsubs,
/// using the pi-squared word at 822F88D0. This is not acos/angle decoding.
/// Bit 7 denotes an unmatched compiler edge and is not a triangle flag.
fn decode_native_edges(edges: [u8; 3]) -> Result<(u32, [f32; 3]), String> {
    let mut flags = 1 | TriangleFeature::ONE_SIDED | TriangleFeature::USE_EDGE_COSINES;
    let mut cosines = [0.; 3];
    for (i, code) in edges.into_iter().enumerate() {
        let exponent = code & 0x1f;
        // The native signed 32-bit shift becomes negative at 28 and zero
        // above it. Reject those malformed codes instead of producing NaNs.
        if exponent >= 28 {
            return Err(format!(
                "Invalid SKATE native edge angle code {exponent} at corner {i}"
            ));
        }
        cosines[i] = 1.0 - f32::from_bits(0x411d_e9e7) / ((8_u32 << exponent) as f32);
        flags |= u32::from(code & 0x20) << i;
        flags |= u32::from(code & 0x40) << (i + 3);
    }
    Ok((flags, cosines))
}

fn retail_archive(map: &SkateMap) -> Result<Option<&[u8]>, String> {
    let mut archives = map.extensions.iter().filter(|e| e.tag == *b"RWCM");
    let Some(archive) = archives.next() else {
        return Ok(None);
    };
    if archive.schema != 1 || archives.next().is_some() {
        return Err("SKATE requires one RWCM extension with schema 1".into());
    }
    Ok(Some(&archive.payload))
}

fn retail_collision_world(
    archive: &[u8],
    material: RetailContactMaterial,
) -> Result<BoardWorld, String> {
    // Reserved once from the mesh headers: pushing Downtown's 1.5 million 128-byte
    // triangles one at a time doubles the list to 256 MiB while the 128 MiB copy is
    // still alive.
    let capacity = skate_data::retail_collision::triangle_capacity(archive)?;
    let mut triangles = Vec::new();
    let mut packed_surfaces = Vec::new();
    triangles.try_reserve_exact(capacity)
        .and_then(|()| packed_surfaces.try_reserve_exact(capacity))
        .map_err(|_| format!("RWCM collision of {capacity} triangles cannot be reserved"))?;
    let mut meshes = Vec::new();
    let count = skate_data::retail_collision::visit_clusters(archive, |_, cluster| {
        // Preserve cluster order and partition further only for group filters.
        let mut cursor = 0;
        while cursor < cluster.len() {
            let group = cluster[cursor].group;
            let start = triangles.len();
            while cursor < cluster.len() && cluster[cursor].group == group {
                let source = cluster[cursor];
                let (flags, cosines) = match source.edges {
                    Some(edges) => {
                        let (mut flags, cosines) = decode_native_edges(edges)?;
                        flags &= !TriangleFeature::ONE_SIDED;
                        if source.one_sided {
                            flags |= TriangleFeature::ONE_SIDED;
                        }
                        (flags, cosines)
                    }
                    // TriangleVolume ctor82AC7770 retains these defaults when
                    // the unit has no edge data; the mesh sidedness is not read.
                    None => (0x1e1, [-1.; 3]),
                };
                triangles.push(
                    WorldTriangle::from_vertices(
                        source.points.map(|p| Vector3::new(p[0], p[1], p[2])),
                        material,
                        u32::from(source.surface),
                        flags,
                        cosines,
                        0.,
                    )
                    .ok_or("Invalid RWCM collision triangle")?,
                );
                packed_surfaces.push(source.surface);
                cursor += 1;
            }
            let range = start..triangles.len();
            let bounds = Bounds::from_points(
                triangles[range.clone()]
                    .iter()
                    .flat_map(|t| t.triangle.vertices),
            )
            .ok_or("Invalid RWCM cluster bounds")?;
            meshes.push(QueryMesh {
                geometry: 0, rejection_flags: 0,
                triangle_range: range,
                local_to_world: RetailAffineTransform::IDENTITY,
                world_to_local: RetailAffineTransform::IDENTITY,
                local_bounds: bounds,
                // Native static registration uses matchingID -1; the unit group
                // only splits clusters and must not filter actor queries.
                matching_group: -1,
                pool: QueryPool::Ground,
            });
        }
        Ok(())
    })?;
    eprintln!(
        "SKATE_RWCM_READY triangles={count} query_clusters={} source=embedded",
        meshes.len()
    );
    BoardWorld::with_query_metadata(
        triangles,
        QueryMetadata {
            packed_surfaces,
            meshes,
            static_edges: vec![],
            island_flags: 0,
        },
    )
    .map_err(str::to_owned)
}

pub(crate) fn collision_world(
    map: &SkateMap,
    material: RetailContactMaterial,
) -> Result<BoardWorld, String> {
    if let Some(archive) = retail_archive(map)? {
        return retail_collision_world(archive, material);
    }
    // Match the reference RW mesh compiler's 1 mm vertex welding and reversed
    // edge pairing. Triangle diagonals are adjacency, never authored ledges.
    let mut welded = HashMap::<[i64; 3], usize>::new();
    let mut positions = Vec::<Vec3>::new();
    let mut vertices = Vec::with_capacity(map.geometry.collision.len());
    let mut normals = Vec::with_capacity(map.geometry.collision.len());
    for tri in &map.geometry.collision {
        let ids = tri.points.map(|p| {
            let inverse = 1.0 / f64::from(0.001_f32);
            let key = p.map(|v| (f64::from(v) * inverse).round() as i64);
            *welded.entry(key).or_insert_with(|| {
                let id = positions.len();
                positions.push(Vec3::from_array(p));
                id
            })
        });
        let [a, b, c] = tri.points.map(Vec3::from_array);
        let normal = (b - a)
            .cross(c - a)
            .try_normalize()
            .ok_or("Invalid SKATE collision triangle normal")?;
        vertices.push(ids);
        normals.push(normal);
    }
    let mut cosines = vec![[1.; 3]; vertices.len()];
    let mut flags =
        vec![TriangleFeature::ONE_SIDED | TriangleFeature::USE_EDGE_COSINES | 0xe0; vertices.len()];
    // Fully native maps need no reconstructed adjacency. Mixed maps still
    // include every face when finding neighbors for their authored geometry.
    if map
        .geometry
        .collision
        .iter()
        .any(|t| t.native_edges.is_none())
    {
        let mut open = HashMap::<(usize, usize), (usize, usize)>::new();
        for (i, ids) in vertices.iter().enumerate() {
            for edge in 0..3 {
                let (a, b) = (ids[edge], ids[(edge + 1) % 3]);
                if let Some((other, oe)) = open.remove(&(b, a)) {
                    let cosine = normals[i].dot(normals[other]).clamp(-1., 1.);
                    let orientation =
                        (positions[b] - positions[a]).dot(normals[i].cross(normals[other]));
                    // ExtendedEdgeCosine / MakeEdgeCode in rw_collision_mesh.cpp:
                    // orientation >= -1e-6 is convex; flat edges have no convex bit.
                    for (ti, e) in [(i, edge), (other, oe)] {
                        cosines[ti][e] = cosine;
                        if orientation <= -1.0e-6 || cosine >= 1. {
                            flags[ti] &= !(0x20 << e);
                        }
                    }
                } else {
                    open.entry((a, b)).or_insert((i, edge));
                }
            }
        }
        let mut adjacent = vec![Vec::new(); positions.len()];
        for (i, ids) in vertices.iter().enumerate() {
            for &v in ids {
                adjacent[v].push(i);
            }
        }
        for (v, faces) in adjacent.iter().enumerate() {
            let reference = normals[faces[0]];
            if faces
                .iter()
                .all(|&i| (reference.dot(normals[i]) - 1.).abs() <= 0.01)
            {
                for &i in faces {
                    for corner in 0..3 {
                        if vertices[i][corner] == v {
                            flags[i] |= 0x200 << corner;
                        }
                    }
                }
            }
        }
    }
    let mut triangles = Vec::with_capacity(vertices.len());
    let mut packed_surfaces = Vec::with_capacity(vertices.len());
    for (i, source) in map.geometry.collision.iter().enumerate() {
        if let Some(edges) = source.native_edges {
            (flags[i], cosines[i]) = decode_native_edges(edges)?;
        }
        let m = &map.materials[source.material as usize - 1];
        // Exact EncodeRwSurfaceId mapping from the reference native adapter.
        packed_surfaces.push((m.audio | (m.physics << 7) | (m.pattern << 12)) as u16);
        let points = vertices[i].map(|id| {
            let p = positions[id];
            Vector3::new(p.x, p.y, p.z)
        });
        // Keep the supplied game's original static-world contact combine values.
        // The native map bridge supplies packed surfaces, not a guessed split of
        // the package's single friction scalar into static/dynamic coefficients.
        triangles.push(
            WorldTriangle::from_vertices(
                points,
                material,
                source.surface,
                flags[i],
                cosines[i],
                0.,
            )
            .ok_or("Invalid SKATE collision volume")?,
        );
    }
    // Portable maps have no native cluster hierarchy. Bound contiguous ranges
    // once at load time so the existing BVH can reject distant geometry. Keep
    // triangle order and mesh identity/filter values: contact tie-breaking,
    // packed surfaces and adjacency must not change with this acceleration.
    let mut meshes = Vec::new();
    for start in (0..triangles.len()).step_by(64) {
        let end = (start + 64).min(triangles.len());
        let bounds = Bounds::from_points(triangles[start..end].iter().flat_map(|t| t.triangle.vertices))
            .ok_or("SKATE collision bounds empty")?;
        meshes.push(QueryMesh {
            geometry: 0, rejection_flags: 0,
            triangle_range: start..end,
            local_to_world: RetailAffineTransform::IDENTITY,
            world_to_local: RetailAffineTransform::IDENTITY,
            local_bounds: bounds,
            matching_group: -1,
            pool: QueryPool::Ground,
        });
    }
    let metadata = QueryMetadata {
        packed_surfaces,
        meshes,
        static_edges: vec![],
        island_flags: 0,
    };
    BoardWorld::with_query_metadata(triangles, metadata).map_err(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Measures the static draw budget on a real installed map. This is the
    /// check that the whole architecture exists to pass, so it reports the class
    /// mix rather than only the total: if the budget is ever missed, the mix says
    /// whether to subdivide less or to stop splitting on a material property.
    ///
    /// `SKATE_BUDGET_MAP` names the `.skate` file; `SKATE_TRANSITION_TEST_ASSETS`
    /// gives the asset root.
    #[test]
    #[ignore = "Requires private installed assets; CPU only, no GPU or window"]
    fn static_draw_budget_holds_on_an_installed_map() {
        let root = std::path::PathBuf::from(
            std::env::var("SKATE_TRANSITION_TEST_ASSETS")
                .expect("SKATE_TRANSITION_TEST_ASSETS"),
        );
        let path = std::path::PathBuf::from(
            std::env::var("SKATE_BUDGET_MAP").expect("SKATE_BUDGET_MAP"),
        );
        let map = skate_data::skate_map::SkateMap::load(&path).unwrap();
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<Image>>();
        world.init_resource::<Assets<StandardMaterial>>();
        world.init_resource::<Assets<crate::retail_render::WorldMaterial>>();
        world.init_resource::<Assets<bevy::render::storage::ShaderStorageBuffer>>();
        let mut scene = crate::map_render::PreparedScene::new(&world);
        let started = std::time::Instant::now();
        scene.prepare(Some(&map), &root);
        let elapsed = started.elapsed();
        let stats = scene.stats;
        eprintln!(
            "SKATE_BUDGET map={} draws={} triangles={} slabs={} materials={} prepare_ms={}",
            path.file_stem().unwrap_or_default().to_string_lossy(),
            stats.draws,
            stats.triangles,
            stats.slabs,
            map.materials.len(),
            elapsed.as_millis()
        );
        assert!(
            stats.draws < 300,
            "static draw budget is under 300; got {} draws",
            stats.draws
        );
        assert!(stats.triangles > 0, "map produced no geometry");
    }

    fn triangle(centroid: Vec3) -> Triangle {
        Triangle { indices: [0, 1, 2], centroid, slab: 0, class: RenderClass::Opaque }
    }

    #[test]
    fn partition_respects_the_leaf_budget() {
        let triangles: Vec<Triangle> = (0..LEAF_TRIANGLE_BUDGET * 3 + 7)
            .map(|i| triangle(Vec3::new(i as f32, 0., 0.)))
            .collect();
        let mut members: Vec<usize> = (0..triangles.len()).collect();
        let leaves = partition(&mut members, &triangles);
        assert!(leaves.len() >= 4, "expected several leaves, got {}", leaves.len());
        for leaf in &leaves {
            assert!(leaf.len() <= LEAF_TRIANGLE_BUDGET, "leaf of {} triangles", leaf.len());
        }
    }

    #[test]
    fn partition_preserves_every_triangle_exactly_once() {
        let triangles: Vec<Triangle> = (0..5_000)
            .map(|i| triangle(Vec3::new((i % 71) as f32, (i % 13) as f32, i as f32)))
            .collect();
        let mut members: Vec<usize> = (0..triangles.len()).collect();
        let mut seen: Vec<usize> = partition(&mut members, &triangles)
            .into_iter()
            .flatten()
            .collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..triangles.len()).collect::<Vec<_>>());
    }

    #[test]
    fn a_single_leaf_is_not_split() {
        let triangles: Vec<Triangle> = (0..64).map(|i| triangle(Vec3::splat(i as f32))).collect();
        let mut members: Vec<usize> = (0..triangles.len()).collect();
        assert_eq!(partition(&mut members, &triangles).len(), 1);
    }
}
