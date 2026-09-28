//! Tauri commands that hand the viewport a zone's navigation data.
//!
//! | command             | answers                                                     |
//! |---------------------|-------------------------------------------------------------|
//! | `level_nav_zones`   | which zones of a region carry navigation, and what sections |
//! | `level_nav_summary` | one nav zone measured: header, layers, per-layer counts, bounds, what is framed but not unpacked |
//! | `level_nav_surface`  | the walkable surface as a packed `"RCMS"` buffer the viewport already knows how to draw |
//! | `level_nav_edges`   | the same unit's edges as a packed `"RCNL"` line buffer, classified |
//! | `level_nav_clues`   | the authored links (vault, jump, grind) as points, typed     |
//!
//! All read-only: nothing here writes to the game directory or to a zone.
//!
//! The decode, and what is measured about it, lives in [`crate::core::nav`].
//! Two things a caller has to know before drawing anything:
//!
//! * **Vertices are already in the region's world space.** Navigation is not
//!   placed by a transform; the numbers come out of the archive in the same
//!   space `level_load_zone` reports scene-object matrices in, so the viewport
//!   uploads them with an identity model matrix. Measured against the shipped
//!   art of the same tile, a reachable polygon's centroid is a median 0.10 m from
//!   the drawn floor and inside half a metre on 82.9% of polygons, against 1.78 m
//!   and 24.3% for the same polygons slid 48 m sideways; the whole table is in
//!   [`crate::core::nav`]. The surface sits about 0.18 m **above** the art, so
//!   draw it with a small +Y bias or with depth writes off.
//! * **Only the reachable polygons are walkable.** A group's polygon array is
//!   the reachable island(s) followed by the polygons the builder rasterised and
//!   then found no island for. Those carry no island, no bot-class flags, no
//!   adjacency and no bounding sphere, and they are four times as much geometry
//!   as the walkable surface. They come back in runs of their own, without
//!   flag bit `0x8000`, and `includeDisconnected` defaults to **true** because
//!   318 of the 673 shipped units have no reachable polygon at all and would
//!   otherwise draw empty. The viewport must colour the two classes differently
//!   and must not call the second one walkable.

use std::time::Instant;

use log::info;
use serde::Serialize;

use crate::core::error::ToolkitError;
use crate::core::level::{self, LevelIndex};
use crate::core::nav::{self, EdgeType, NavMeshFilter, NavUnit};
use crate::core::zone::Zone;

type Result<T> = std::result::Result<T, ToolkitError>;

fn index(game_dir: &str, hashes_path: Option<String>) -> Result<std::sync::Arc<LevelIndex>> {
    level::get_index(game_dir, hashes_path.as_deref())
}

fn load_zone(index: &LevelIndex, zone_path: &str) -> Result<(String, Zone)> {
    let path = crate::core::crc64::normalize_path(zone_path);
    let id = crate::core::crc64::hash_raw(path.as_bytes());
    let raw = index.extract(id)?;
    let zone = Zone::parse(&raw)?;
    Ok((path, zone))
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// One zone of a region that carries navigation, and which of the five nav
/// sections it holds.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavZoneEntry {
    pub path: String,
    pub id: String,
    pub tile: Option<String>,
    pub size: u32,
    /// True when this zone holds [`nav::NAV_UNIT_TAG`], i.e. a mesh to draw.
    pub has_unit: bool,
    /// True when this zone holds the region's [`nav::NAV_CONTAINER_TAG`].
    pub has_container: bool,
    pub has_tile_cache: bool,
    pub has_3d: bool,
}

/// The container's own fields, resolved into metres.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavContainerInfo {
    pub asset_id: String,
    pub layer_count: i32,
    /// False when the whole region is one unit whose mesh ships in this zone.
    pub tiled: bool,
    /// The unit size in metres, absent for a single-unit region.
    pub unit_size_m: Option<i32>,
    /// The lookup grid in world metres, inclusive then exclusive.
    pub min_m: [i32; 2],
    pub max_m: [i32; 2],
    pub units_x: usize,
    pub units_z: usize,
    /// The stored slot count and the one the header implies. They agree on
    /// every shipped container; a mismatch means the decode is wrong.
    pub slots: usize,
    pub implied_slots: usize,
}

/// One layer's totals.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavLayerInfo {
    pub layer: usize,
    pub groups: usize,
    pub empty_groups: usize,
    pub vertices: usize,
    /// Reachable polygons: the walkable surface.
    pub polygons: usize,
    /// Polygons Recast found unreachable. Not walkable, not adjacency-linked.
    pub disconnected: usize,
    /// Internal, external, custom (clue), border.
    pub edges: [usize; 4],
}

/// Everything one nav zone is measured to hold.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavSummary {
    pub path: String,
    /// The `0x3D8DBDB8` word, when present.
    pub version_word: Option<u32>,
    pub container: Option<NavContainerInfo>,
    pub unit: Option<NavUnitInfo>,
    /// Detour compressed tiles. Framed and located, not decompressed: a tile's
    /// payload is a height/area/connection layer that only a rebuild needs.
    pub tile_cache_tiles: Option<usize>,
    pub tile_cache_layers: Option<usize>,
    pub tile_cache_bytes: Option<usize>,
    /// The flight octree. Header located, nodes not decoded.
    pub nav3d_node_bytes: Option<usize>,
    /// Anything [`NavUnit::validate`] objected to. Empty on every shipped unit.
    pub problems: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavUnitInfo {
    /// `+0x04`. The nav builder calls this the unit's asset id, but it names no
    /// asset in the PC TOC -- see [`crate::core::nav`]. Diagnostic only.
    pub asset_id: String,
    /// `+0x0C`. The builder calls this the container's asset id; it too names no
    /// TOC asset. It equals [`Self::asset_id`] on the single-unit regions.
    pub container_id: String,
    /// Where the layer count came from: the container, or the section's length.
    pub layers: usize,
    pub layers_from_container: bool,
    /// The group grid, in 16 m tiles: inclusive then exclusive.
    pub min_tile: [i16; 2],
    pub max_tile: [i16; 2],
    pub log_tile_size: i16,
    pub tiles_x: usize,
    pub tiles_z: usize,
    pub per_layer: Vec<NavLayerInfo>,
    /// The bounding box of every vertex the unit holds, in world metres.
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub clues: usize,
}

/// One authored link out of the mesh.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavClueOut {
    pub layer: usize,
    pub group_x: i32,
    pub group_z: i32,
    pub position: [f32; 3],
    pub actor_uid: String,
    pub target_actor_uid: String,
    pub distance: f32,
    pub required_ability: u32,
    pub required_entity: u32,
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

/// Every zone of one region that carries navigation.
///
/// Cheap: the section list of a zone is its DAT1 header, so this parses each
/// candidate zone but decodes no mesh. A region has one container zone and, when
/// it is tiled, one unit zone per tile that has walkable ground.
#[tauri::command]
pub async fn level_nav_zones(
    game_dir: String,
    region: String,
    hashes_path: Option<String>,
) -> Result<Vec<NavZoneEntry>> {
    let idx = index(&game_dir, hashes_path)?;
    let mut out = Vec::new();
    for z in idx.zones.iter().filter(|z| z.region == region) {
        if !z.path.contains("navdata") {
            continue;
        }
        let Ok(raw) = idx.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&raw) else { continue };
        let has = |t: u32| zone.dat1.get_section_data(t).is_some();
        out.push(NavZoneEntry {
            path: z.path.clone(),
            id: format!("{:016X}", z.id),
            tile: z.tile.clone(),
            size: z.size,
            has_unit: has(nav::NAV_UNIT_TAG),
            has_container: has(nav::NAV_CONTAINER_TAG),
            has_tile_cache: has(nav::NAV_TILE_CACHE_TAG),
            has_3d: has(nav::NAV_3D_TAG),
        });
    }
    Ok(out)
}

/// One nav zone, measured.
///
/// This is the command a panel shows before anything is drawn: it says how many
/// layers the unit has and where they came from, how much of each class of
/// geometry is in each layer, and which sections are only framed. A zone with a
/// unit but no reachable polygon is a real shape in the shipped data, not a
/// decode failure, and this is where that shows up.
#[tauri::command]
pub async fn level_nav_summary(
    game_dir: String,
    zone_path: String,
    hashes_path: Option<String>,
) -> Result<NavSummary> {
    let start = Instant::now();
    let idx = index(&game_dir, hashes_path)?;
    let (path, zone) = load_zone(&idx, &zone_path)?;

    let container = zone
        .dat1
        .get_section_data(nav::NAV_CONTAINER_TAG)
        .and_then(|d| nav::NavContainer::parse(d).ok())
        .map(|c| {
            let (min_m, max_m) = c.bounds_metres();
            let (ux, uz) = c.unit_grid();
            NavContainerInfo {
                asset_id: format!("{:016X}", c.asset_id),
                layer_count: c.layer_count,
                tiled: c.is_tiled(),
                unit_size_m: c.unit_size_m(),
                min_m,
                max_m,
                units_x: ux,
                units_z: uz,
                slots: c.slots,
                implied_slots: c.implied_slots(),
            }
        });

    let mut problems = Vec::new();
    let unit = match zone.dat1.get_section_data(nav::NAV_UNIT_TAG) {
        None => None,
        Some(d) => {
            let layers_from_container = container
                .as_ref()
                .and_then(|c| usize::try_from(c.layer_count).ok())
                .is_some_and(|l| {
                    let nx = (LE_i16(d, 0x18) - LE_i16(d, 0x14)).max(0) as usize;
                    let nz = (LE_i16(d, 0x1A) - LE_i16(d, 0x16)).max(0) as usize;
                    nav::layer_count_fits(d, nx * nz, l)
                });
            // A unit that will not parse must not sink the whole summary: the
            // container and the tile cache of the same zone are still worth
            // reporting, and the reason goes in `problems`.
            let u = match NavUnit::from_zone(&zone) {
                Ok(u) => u,
                Err(e) => {
                    problems.push(format!("unit section: {e}"));
                    return Ok(NavSummary {
                        path,
                        version_word: nav::nav_version_word(&zone),
                        container,
                        unit: None,
                        tile_cache_tiles: None,
                        tile_cache_layers: None,
                        tile_cache_bytes: zone
                            .dat1
                            .get_section_data(nav::NAV_TILE_CACHE_TAG)
                            .map(|d| d.len()),
                        nav3d_node_bytes: None,
                        problems,
                    });
                }
            };
            problems = u.validate();

            let mut per_layer: Vec<NavLayerInfo> = (0..u.layers.max(1))
                .map(|layer| NavLayerInfo {
                    layer,
                    groups: 0,
                    empty_groups: 0,
                    vertices: 0,
                    polygons: 0,
                    disconnected: 0,
                    edges: [0; 4],
                })
                .collect();
            let mut min = [f32::INFINITY; 3];
            let mut max = [f32::NEG_INFINITY; 3];
            let mut clues = 0usize;
            for g in &u.groups {
                let Some(l) = per_layer.get_mut(g.layer) else { continue };
                if g.is_empty() {
                    l.empty_groups += 1;
                    continue;
                }
                l.groups += 1;
                l.vertices += g.vert_count;
                l.polygons += g.poly_count;
                l.disconnected += g.disconnected_poly_count;
                for i in 0..4 {
                    l.edges[i] += g.edge_counts[i];
                }
                clues += g.edge_counts[EdgeType::Custom as usize];
                for v in u.verts(g) {
                    for i in 0..3 {
                        min[i] = min[i].min(v[i]);
                        max[i] = max[i].max(v[i]);
                    }
                }
            }
            if !min[0].is_finite() {
                min = [0.0; 3];
                max = [0.0; 3];
            }
            let (tx, tz) = u.grid();
            Some(NavUnitInfo {
                asset_id: format!("{:016X}", u.asset_id),
                container_id: format!("{:016X}", u.container_id),
                layers: u.layers,
                layers_from_container,
                min_tile: u.min_tile,
                max_tile: u.max_tile,
                log_tile_size: u.log_scale,
                tiles_x: tx,
                tiles_z: tz,
                per_layer,
                min,
                max,
                clues,
            })
        }
    };

    let tc = zone
        .dat1
        .get_section_data(nav::NAV_TILE_CACHE_TAG)
        .map(|d| (d.len(), nav::tile_cache_entries(d)));
    let (tile_cache_bytes, tile_cache_tiles, tile_cache_layers) = match tc {
        None => (None, None, None),
        Some((len, Ok((tiles, layers)))) => (Some(len), Some(tiles.len()), Some(layers)),
        Some((len, Err(e))) => {
            problems.push(format!("tile cache: {e}"));
            (Some(len), None, None)
        }
    };

    let nav3d_node_bytes = zone
        .dat1
        .get_section_data(nav::NAV_3D_TAG)
        .and_then(|d| nav::Nav3D::parse(d).ok())
        .map(|n| n.node_bytes);

    let out = NavSummary {
        path,
        version_word: nav::nav_version_word(&zone),
        container,
        unit,
        tile_cache_tiles,
        tile_cache_layers,
        tile_cache_bytes,
        nav3d_node_bytes,
        problems,
    };
    info!(
        "level_nav_summary {}: {} layers in {:?}",
        out.path,
        out.unit.as_ref().map(|u| u.layers).unwrap_or(0),
        start.elapsed()
    );
    Ok(out)
}

/// The walkable surface of one nav zone, as the `"RCMS"` buffer the viewport's
/// model path already decodes.
///
/// Triangles come out de-indexed and grouped into submeshes by
/// `(layer, reachable, bot-class flags)`; see [`nav::pack_nav_mesh`] for what
/// each submesh field carries. Positions are world-space, so draw with an
/// identity model matrix.
///
/// A zone with no unit section, or a unit with nothing in the requested layer,
/// returns a well-formed empty buffer rather than an error, so a caller can ask
/// about any zone.
#[tauri::command]
pub async fn level_nav_surface(
    game_dir: String,
    zone_path: String,
    layer: Option<usize>,
    include_disconnected: Option<bool>,
    hashes_path: Option<String>,
) -> Result<tauri::ipc::Response> {
    let start = Instant::now();
    let idx = index(&game_dir, hashes_path)?;
    let (path, zone) = load_zone(&idx, &zone_path)?;
    let filter = NavMeshFilter {
        layer,
        include_disconnected: include_disconnected.unwrap_or(true),
    };
    let bytes = match NavUnit::from_zone(&zone) {
        Ok(u) => {
            let m = nav::nav_mesh_of(&u, filter);
            info!(
                "level_nav_surface {path}: {} triangles in {} runs in {:?}",
                m.indices.len() / 3,
                m.runs.len(),
                start.elapsed()
            );
            nav::pack_nav_mesh(&m)
        }
        Err(e) => {
            info!("level_nav_surface {path}: {e}");
            nav::pack_nav_mesh(&nav::NavMeshOut::empty())
        }
    };
    Ok(tauri::ipc::Response::new(bytes))
}

/// The same unit's edges as world-space line segments, classified.
///
/// The buffer is `"RCNL"`: magic, version 1, segment count, a pad word, the
/// min/max box, then two `f32[3]` per segment and one `u32` class per segment,
/// where the class is `layer << 8 | edgeType`. Border edges (type 3) are the rim
/// of the walkable surface, internal (0) are shared between two triangles of one
/// group, external (1) cross into a neighbouring group. Custom edges (2) are
/// **not** in this buffer -- they carry no vertex pair; ask `level_nav_clues` for
/// those, which returns them as points.
#[tauri::command]
pub async fn level_nav_edges(
    game_dir: String,
    zone_path: String,
    layer: Option<usize>,
    hashes_path: Option<String>,
) -> Result<tauri::ipc::Response> {
    let idx = index(&game_dir, hashes_path)?;
    let (path, zone) = load_zone(&idx, &zone_path)?;
    let bytes = match NavUnit::from_zone(&zone) {
        Ok(u) => nav::pack_nav_edges(
            &u,
            NavMeshFilter {
                layer,
                include_disconnected: false,
            },
        ),
        Err(e) => {
            info!("level_nav_edges {path}: {e}");
            nav::pack_nav_edges_empty()
        }
    };
    Ok(tauri::ipc::Response::new(bytes))
}

/// Every authored link out of one unit's mesh, as typed points.
///
/// One per custom edge. `requiredAbility` and `requiredEntity` are the gate the
/// bot has to satisfy to use the link; the two uids are raw and are *not* scene
/// object instance ids on the shipped data, so treat them as opaque until
/// something matches them.
#[tauri::command]
pub async fn level_nav_clues(
    game_dir: String,
    zone_path: String,
    hashes_path: Option<String>,
) -> Result<Vec<NavClueOut>> {
    let idx = index(&game_dir, hashes_path)?;
    let (_, zone) = load_zone(&idx, &zone_path)?;
    let mut out = Vec::new();
    let Ok(u) = NavUnit::from_zone(&zone) else {
        return Ok(out);
    };
    for g in &u.groups {
        if g.is_empty() {
            continue;
        }
        for c in u.clues(g) {
            out.push(NavClueOut {
                layer: g.layer,
                group_x: g.x,
                group_z: g.z,
                position: c.position,
                actor_uid: format!("{:016X}", c.actor_uid),
                target_actor_uid: format!("{:016X}", c.target_actor_uid),
                distance: c.distance,
                required_ability: c.required_ability,
                required_entity: c.required_entity,
            });
        }
    }
    Ok(out)
}

/// A little `i16` reader, so the layer-count provenance check can look at the
/// unit header without parsing it twice.
#[allow(non_snake_case)]
fn LE_i16(d: &[u8], at: usize) -> i16 {
    if at + 2 <= d.len() {
        i16::from_le_bytes([d[at], d[at + 1]])
    } else {
        0
    }
}
