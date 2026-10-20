//! Tauri commands for the non-model zone entity kinds.
//!
//! | command                  | answers                                                     |
//! |--------------------------|-------------------------------------------------------------|
//! | `level_zone_entities`    | one zone's volumes, curves, VFX instances and decals        |
//! | `level_zone_paths`       | just the curves, as world polylines a viewport can draw     |
//! | `level_nav_info`         | whether a zone holds nav data, and its grid / tile summary  |
//! | `level_nav_mesh`         | one nav unit's walkable triangles, packed as an ArrayBuffer |
//! | `level_nav_tiles`        | the nav tile footprints, for a coverage overlay             |
//!
//! Everything reads the shipped archives read-only through
//! [`crate::core::level`], which memoises the TOC per game directory.

use serde::Serialize;

use crate::core::error::ToolkitError;
use crate::core::level::{self, LevelIndex};
use crate::core::zone::Zone;
use crate::core::zone_entities as ent;

type Result<T> = std::result::Result<T, ToolkitError>;

fn index(game_dir: &str, hashes_path: Option<String>) -> Result<std::sync::Arc<LevelIndex>> {
    level::get_index(game_dir, hashes_path.as_deref())
}

/// Load a zone asset by path. The path *is* the identity, so this works with no
/// name list at all.
fn load_zone(index: &LevelIndex, zone_path: &str) -> Result<Zone> {
    let path = crate::core::crc64::normalize_path(zone_path);
    let id = crate::core::crc64::hash_raw(path.as_bytes());
    Ok(Zone::parse(&index.extract(id)?)?)
}

// ---------------------------------------------------------------------------
// entities
// ---------------------------------------------------------------------------

/// Every non-model entity in one zone, ready for the inspector.
#[tauri::command]
pub async fn level_zone_entities(
    game_dir: String,
    zone_path: String,
    hashes_path: Option<String>,
) -> Result<ent::ZoneEntities> {
    let idx = index(&game_dir, hashes_path)?;
    let zone = load_zone(&idx, &zone_path)?;
    Ok(ent::zone_entities(&zone))
}

/// One authored path, flattened for drawing.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathLine {
    pub offset: u32,
    pub name: String,
    pub actor_asset: String,
    pub spline_type: &'static str,
    pub looping: bool,
    pub length: f32,
    /// World-space control points, with the loop closed when it loops.
    pub points: Vec<[f32; 3]>,
    /// World-space per-point "up", parallel to `points` before closing.
    pub normals: Option<Vec<[f32; 3]>>,
}

/// Just the curves of a zone, as polylines in world space.
#[tauri::command]
pub async fn level_zone_paths(
    game_dir: String,
    zone_path: String,
    hashes_path: Option<String>,
) -> Result<Vec<PathLine>> {
    let idx = index(&game_dir, hashes_path)?;
    let zone = load_zone(&idx, &zone_path)?;
    Ok(ent::curves(&zone)
        .into_iter()
        .map(|c| PathLine {
            offset: c.offset,
            name: if c.actor_name.is_empty() {
                format!("curve @{}", c.offset)
            } else {
                c.actor_name.clone()
            },
            actor_asset: c.actor_asset.clone(),
            spline_type: c.spline_type_name,
            looping: c.looping,
            length: c.length,
            points: c.world_polyline(),
            normals: c.normals.as_ref().map(|ns| {
                ns.iter()
                    .map(|&n| ent::xform_dir(&c.matrix, n))
                    .collect()
            }),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// navigation
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavInfo {
    pub has_nav: bool,
    pub container: Option<ent::NavContainer>,
    pub unit: Option<ent::NavUnit>,
    /// Number of `dtTileCache` layer records in the zone.
    pub tile_count: usize,
    /// Layer terminators in the tile cache; equals the unit's nav layer count.
    pub tile_cache_layers: usize,
    /// Total walkable triangles the unit holds, across all nav layers.
    pub triangle_count: usize,
    pub vertex_count: usize,
    /// Groups whose data blob is nothing but vertices and triangles.
    pub plain_groups: usize,
    /// Groups that also carry inter-group link arrays.
    pub link_groups: usize,
}

#[tauri::command]
pub async fn level_nav_info(
    game_dir: String,
    zone_path: String,
    hashes_path: Option<String>,
) -> Result<NavInfo> {
    let idx = index(&game_dir, hashes_path)?;
    let zone = load_zone(&idx, &zone_path)?;
    let (tiles, layers) = ent::nav_tiles(&zone);
    let unit = ent::nav_unit(&zone);
    let (tri, vert, plain, link) = match &unit {
        Some(u) => (
            u.groups.iter().map(|g| g.poly_count as usize).sum(),
            u.groups.iter().map(|g| g.vert_count as usize).sum(),
            u.groups.iter().filter(|g| g.plain).count(),
            u.groups.iter().filter(|g| !g.plain).count(),
        ),
        None => (0, 0, 0, 0),
    };
    Ok(NavInfo {
        has_nav: ent::has_nav(&zone),
        container: ent::nav_container(&zone),
        unit,
        tile_count: tiles.len(),
        tile_cache_layers: layers,
        triangle_count: tri,
        vertex_count: vert,
        plain_groups: plain,
        link_groups: link,
    })
}

/// One nav unit's walkable surface, packed for the viewport as
/// `{u32 vertex_count, u32 triangle_count, f32[3] * vertex_count,
/// u32[3] * triangle_count}` — the same little-endian, tightly packed shape the
/// model mesh commands use, so the front end can upload it without reshaping.
#[tauri::command]
pub async fn level_nav_mesh(
    game_dir: String,
    zone_path: String,
    layer: Option<usize>,
) -> Result<tauri::ipc::Response> {
    let idx = index(&game_dir, None)?;
    let zone = load_zone(&idx, &zone_path)?;
    let mesh = match ent::nav_unit(&zone) {
        Some(u) => ent::nav_mesh(&zone, &u, layer),
        None => ent::NavMesh::default(),
    };
    let mut out = Vec::with_capacity(8 + mesh.verts.len() * 12 + mesh.tris.len() * 12);
    out.extend_from_slice(&(mesh.verts.len() as u32).to_le_bytes());
    out.extend_from_slice(&(mesh.tris.len() as u32).to_le_bytes());
    for v in &mesh.verts {
        for c in v {
            out.extend_from_slice(&c.to_le_bytes());
        }
    }
    for t in &mesh.tris {
        for i in t {
            out.extend_from_slice(&i.to_le_bytes());
        }
    }
    Ok(tauri::ipc::Response::new(out))
}

/// The nav tile footprints, for a "where is there navigation at all" overlay.
#[tauri::command]
pub async fn level_nav_tiles(
    game_dir: String,
    zone_path: String,
) -> Result<Vec<ent::NavTile>> {
    let idx = index(&game_dir, None)?;
    let zone = load_zone(&idx, &zone_path)?;
    Ok(ent::nav_tiles(&zone).0)
}
