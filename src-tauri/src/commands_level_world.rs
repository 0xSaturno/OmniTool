//! Tauri commands for the level's world structure: the region tree, the region
//! game data, and hibernation.
//!
//! `commands_level_editor` browses zones by path; this module reads the `.level`
//! asset itself, which is where the game keeps the tree those zones hang in.
//!
//! | command                | answers                                             |
//! |------------------------|-----------------------------------------------------|
//! | `world_open_level`     | the whole region tree of one `.level`, with counts, streaming settings, named links and which regions carry game data |
//! | `world_region_data`    | one region's game-data blocks, decoded from DDL      |
//! | `world_zone_hibernate` | the far-LOD instances a zone contributes to the level, with ready-to-draw matrices |
//! | `world_hibernate_ghost`| whether one placed instance has a far-LOD copy that an edit would leave behind |
//!
//! Every path here is read-only against the shipped archives. The parsed level
//! is memoised per `(game dir, level path)` because a region tree is asked for
//! repeatedly while the UI is open and parsing `i29.level` walks five megabytes.
//!
//! On coordinates: there is no per-region placement transform in a Rift Apart
//! level — see the proof in [`crate::core::level_asset`] — so every position
//! this module returns is already in the one world space the viewer draws in.
//! What the UI does have to respect is that two instance regions may cover the
//! same ground because they are alternate versions of one place; `overlaps`
//! reports that per region so the UI can offer them as a choice rather than a
//! sum.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;

use crate::core::crc64;
use crate::core::error::{Result, ToolkitError};
use crate::core::level;
use crate::core::level_asset::{self as la, LevelAsset};
use crate::core::region_data::{self as rd, RegionData, RegionDataKind};

// ---------------------------------------------------------------------------
// the parsed level, memoised
// ---------------------------------------------------------------------------

fn cache() -> &'static Mutex<HashMap<String, Arc<LevelAsset>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<LevelAsset>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Parse a `.level` asset out of the shipped archives, or hand back the copy
/// this process already parsed.
fn open_level(
    game_dir: &str,
    level_path: &str,
    hashes_path: Option<String>,
) -> Result<Arc<LevelAsset>> {
    let key = format!("{game_dir}\u{1}{level_path}");
    if let Some(hit) = cache().lock().ok().and_then(|c| c.get(&key).cloned()) {
        return Ok(hit);
    }
    let idx = level::get_index(game_dir, hashes_path.as_deref())?;
    // The path is the identity: its CRC-64 is the asset id, so a level resolves
    // even when the name list is missing.
    let id = crc64::hash(level_path);
    let raw = idx.extract(id)?;
    let parsed = Arc::new(LevelAsset::parse(&raw)?);
    if let Ok(mut c) = cache().lock() {
        c.insert(key, parsed.clone());
    }
    Ok(parsed)
}

fn short(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldRegion {
    pub index: usize,
    /// The `.region` authoring path the level stores.
    pub path: String,
    /// Its last component, which is what the UI shows.
    pub label: String,
    /// `kGlobal` | `kInstanceContainer` | `kInstanceUnit` | `kOpenContainer` |
    /// `kOpenUnit` | `kOverlay` | `kDebug`.
    pub region_type: String,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    /// Zone rows this region loads.
    pub zone_count: usize,
    /// Low-LOD stand-ins on an open unit, shadow zones on an overlay.
    pub secondary_zone_count: usize,
    pub link_count: usize,
    /// Struct names of the game-data blocks this region carries.
    pub game_data: Vec<String>,
    /// Streaming settings, open units only: (load, unload) distance in metres
    /// and the reference point the container LODs against.
    pub load_distance: Option<u32>,
    pub unload_distance: Option<u32>,
    pub lod_position: Option<[f32; 3]>,
    /// Hibernating instances the region's zones contribute to the level.
    pub hibernate_instances: usize,
    /// The planet this region belongs to, when it carries `PlanetRegionData`.
    pub planet: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldZone {
    pub index: usize,
    pub path: String,
    pub label: String,
    /// Hex, because a `u64` does not survive JSON intact.
    pub asset_id: String,
    /// True when the asset is in this build's TOC; 133 of `i29.level`'s 9,176
    /// zone rows are not.
    pub present: bool,
    pub hibernate_instances: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldLink {
    pub index: usize,
    pub name: String,
    pub position: [f32; 3],
    /// Region the game streams in for this entry point.
    pub load_region: Option<usize>,
    pub actor_uid: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldSummary {
    pub level_path: String,
    pub bytes: usize,
    /// False when the DAT1 version word is not the one this reader was measured
    /// against — a patched game, not a broken file.
    pub version_known: bool,
    pub regions: Vec<WorldRegion>,
    pub zones: Vec<WorldZone>,
    pub links: Vec<WorldLink>,
    pub roots: Vec<usize>,
    /// Region that is always resident.
    pub global_region: Option<usize>,
    /// Entry point used when nothing else is asked for.
    pub default_link: Option<usize>,
    pub hibernate_zones: usize,
    pub hibernate_instances: usize,
    /// Structural problems found while parsing; empty on the shipped file.
    pub warnings: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegionDataBlock {
    pub row: usize,
    pub struct_name: String,
    pub name_hash: String,
    pub payload_size: u32,
    /// The level stored this block as an override wrapper naming this struct.
    pub override_of: Option<String>,
    /// Flat `field = value` lines, for the blocks with no typed view.
    pub fields: serde_json::Value,
    pub planet: Option<rd::PlanetInfo>,
    pub dimension: Option<rd::DimensionInfo>,
    pub shadow_zone: Option<rd::ShadowZoneInfo>,
    pub overlay_trigger: Option<rd::OverlayTriggerInfo>,
    pub zone_suffixes: Vec<String>,
    /// `PerfSpecRegionData`: impostor draw distance at 30 Hz and 60 Hz.
    pub impostor_draw_distance: Option<[f32; 2]>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HibernateInstance {
    /// Model asset path; its id is the CRC-64 of this.
    pub model: String,
    /// Row-major 4x4 in world space, the same layout a zone scene object uses.
    pub matrix: [f32; 16],
    pub position: [f32; 3],
    pub cluster: usize,
    pub visible: bool,
    pub long_range: bool,
    pub mirrored: bool,
    /// Linear RGB tint and alpha.
    pub tint: [f32; 4],
    /// The zone instance this stands in for: the folded uid stored in the
    /// record, hex.
    pub parent_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HibernateCluster {
    pub centre: [f32; 3],
    pub radius: f32,
    /// 0 or 16,384 m in the shipped level.
    pub draw_distance: f32,
    pub first_instance: usize,
    pub instance_count: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneHibernateView {
    pub zone_index: i32,
    pub zone_path: String,
    pub centre: [f32; 3],
    pub radius: f32,
    pub clusters: Vec<HibernateCluster>,
    pub instances: Vec<HibernateInstance>,
    /// Instances the level keeps for this zone as a share of the models the zone
    /// itself places — a container holds at most 2,048, so this is a subset.
    pub note: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GhostCheck {
    /// True when the level holds a far-LOD copy of this instance, so editing it
    /// in the zone alone leaves a ghost at distance until the level is rebuilt.
    pub has_far_lod_copy: bool,
    pub model: String,
    pub position: Option<[f32; 3]>,
    pub parent_id: String,
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

/// The region tree of one `.level`, with everything the UI needs to offer a
/// region: what type it is, what it loads, how it streams, where the game can
/// drop the player in, and what game data hangs off it.
#[tauri::command]
pub async fn world_open_level(
    game_dir: String,
    level_path: String,
    hashes_path: Option<String>,
) -> Result<WorldSummary> {
    let level = open_level(&game_dir, &level_path, hashes_path.clone())?;
    let idx = level::get_index(&game_dir, hashes_path.as_deref())?;
    let blocks = rd::decode_all(&level);

    // Hibernating instances per zone row, then per region.
    let mut hib_per_zone: HashMap<i32, usize> = HashMap::new();
    for h in &level.hibernate_zones {
        hib_per_zone.insert(
            h.zone_index,
            h.model_objects.len() + h.effect_objects.len(),
        );
    }

    let mut planet_of: HashMap<usize, String> = HashMap::new();
    for b in &blocks {
        if let (Some(ri), Some(p)) = (b.owner_region(), b.planet()) {
            if !p.planet_name.is_empty() {
                planet_of.insert(ri, p.planet_name);
            }
        }
    }

    let regions: Vec<WorldRegion> = level
        .regions
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let lod = level.region_lod(r);
            let zones = level.region_zone_indices(r);
            WorldRegion {
                index: i,
                path: r.name.clone(),
                label: short(&r.name).trim_end_matches(".region").to_string(),
                region_type: r.type_name.to_string(),
                parent: (r.parent_index >= 0).then(|| r.parent_index as usize),
                children: (r.child_start.max(0)..(r.child_start.max(0) + r.child_count.max(0)))
                    .map(|c| c as usize)
                    .collect(),
                zone_count: zones.len(),
                secondary_zone_count: level.region_secondary_zone_indices(r).len(),
                link_count: level.region_named_links(r).len(),
                game_data: level
                    .region_game_data(r)
                    .iter()
                    .map(|g| g.name.clone())
                    .collect(),
                load_distance: lod.map(|l| l.load_distance),
                unload_distance: lod.map(|l| l.unload_distance),
                lod_position: lod.map(|l| l.position),
                hibernate_instances: zones
                    .iter()
                    .filter_map(|z| hib_per_zone.get(&(*z as i32)))
                    .sum(),
                planet: planet_of.get(&i).cloned(),
            }
        })
        .collect();

    let zones: Vec<WorldZone> = level
        .zones
        .iter()
        .enumerate()
        .map(|(i, z)| WorldZone {
            index: i,
            path: z.name.clone(),
            label: short(&z.name).to_string(),
            asset_id: format!("{:016X}", z.asset_id),
            present: idx.asset(z.asset_id).is_some(),
            hibernate_instances: hib_per_zone.get(&(i as i32)).copied().unwrap_or(0),
        })
        .collect();

    let links: Vec<WorldLink> = level
        .named_links
        .iter()
        .map(|l| WorldLink {
            index: l.index,
            name: l.short_name().to_string(),
            position: l.position,
            load_region: (l.load_region_index >= 0).then(|| l.load_region_index as usize),
            actor_uid: format!("{:016X}", l.actor_id),
        })
        .collect();

    let global = level.built.global_region_index;
    Ok(WorldSummary {
        level_path,
        bytes: level.game_data_payload.len() + level.dat1.strings_pool.len(),
        version_known: level.version_known(),
        regions,
        zones,
        links,
        roots: level.root_regions(),
        global_region: (global >= 0 && (global as usize) < level.regions.len())
            .then_some(global as usize),
        default_link: level
            .named_links
            .iter()
            .position(|l| l.id == level.built.default_named_link_id),
        hibernate_zones: level.hibernate_zones.len(),
        hibernate_instances: level.hibernate_object_count(),
        warnings: level.validate(),
    })
}

/// One region's game-data blocks, decoded. `region_index` indexes
/// [`WorldSummary::regions`].
#[tauri::command]
pub async fn world_region_data(
    game_dir: String,
    level_path: String,
    region_index: usize,
    hashes_path: Option<String>,
) -> Result<Vec<RegionDataBlock>> {
    let level = open_level(&game_dir, &level_path, hashes_path)?;
    let region = level.regions.get(region_index).ok_or_else(|| {
        ToolkitError::Parse(format!(
            "region {region_index} is outside the {} this level has",
            level.regions.len()
        ))
    })?;
    Ok(level
        .region_game_data(region)
        .iter()
        .map(|row| {
            let b = RegionData::decode(&level, row, rd::DataOwner::Region(region_index));
            block_dto(&b)
        })
        .collect())
}

fn block_dto(b: &RegionData) -> RegionDataBlock {
    RegionDataBlock {
        row: b.row,
        struct_name: b.struct_name.clone(),
        name_hash: format!("{:08X}", b.name_hash),
        payload_size: b.payload_size,
        override_of: b.override_of.clone(),
        fields: crate::core::ddl::collapse_to_json(&b.fields),
        planet: b.planet(),
        dimension: b.dimension(),
        shadow_zone: b.shadow_zone(),
        overlay_trigger: b.overlay_trigger(),
        zone_suffixes: b.zone_suffixes(),
        impostor_draw_distance: b.perf_spec().map(|(a, c)| [a, c]),
    }
}

/// The far-LOD instances the level keeps for one zone, with matrices in the same
/// world space and the same row-major layout as that zone's own scene objects,
/// so a viewer can draw both with one pipeline.
///
/// A zone with no hibernate container returns `Ok(None)`; 613 of `i29.level`'s
/// 9,176 zones have one.
#[tauri::command]
pub async fn world_zone_hibernate(
    game_dir: String,
    level_path: String,
    zone_path: String,
    hashes_path: Option<String>,
) -> Result<Option<ZoneHibernateView>> {
    let level = open_level(&game_dir, &level_path, hashes_path)?;
    let asset_id = crc64::hash(&zone_path);
    let Some(h) = level.hibernate_for_zone_asset(asset_id) else {
        return Ok(None);
    };

    let mut instances: Vec<HibernateInstance> = Vec::with_capacity(h.model_objects.len());
    for o in h.model_objects.iter().chain(h.effect_objects.iter()) {
        let (rgb, a) = o.colour();
        instances.push(HibernateInstance {
            model: level.hibernate_model_name(o).to_string(),
            matrix: o.matrix(),
            position: o.position,
            cluster: o.cluster,
            visible: o.visible(),
            long_range: o.long_range(),
            mirrored: o.mirrored(),
            tint: [rgb[0], rgb[1], rgb[2], a],
            parent_id: format!("{:08X}", o.parent_id),
        });
    }
    let clusters: Vec<HibernateCluster> = h
        .model_clusters
        .iter()
        .chain(h.effect_clusters.iter())
        .map(|c| HibernateCluster {
            centre: c.centre,
            radius: c.radius,
            draw_distance: c.draw_distance,
            first_instance: c.object_offset as usize,
            instance_count: c.object_count as usize,
        })
        .collect();

    Ok(Some(ZoneHibernateView {
        zone_index: h.zone_index,
        zone_path: level
            .zones
            .get(h.zone_index.max(0) as usize)
            .map(|z| z.name.clone())
            .unwrap_or(zone_path),
        centre: h.models.centre,
        radius: h.models.radius,
        clusters,
        instances,
        note: format!(
            "{} far-LOD instances in {} clusters; a container holds at most 2048, so this is the level's budgeted subset of the zone's models, drawn while the zone is unloaded",
            h.model_objects.len() + h.effect_objects.len(),
            h.model_clusters.len() + h.effect_clusters.len()
        ),
    }))
}

/// Does the level hold a far-LOD copy of this placed instance?
///
/// `instance_uid` is a zone scene object's 64-bit uid, as hex or decimal. The
/// level stores it folded to 32 bits, so this is the only way to connect the two
/// — and the answer is what an editor shows before it moves or deletes a model:
/// a yes means the change leaves a ghost at distance until the level is rebuilt.
#[tauri::command]
pub async fn world_hibernate_ghost(
    game_dir: String,
    level_path: String,
    zone_path: String,
    instance_uid: String,
    hashes_path: Option<String>,
) -> Result<GhostCheck> {
    let level = open_level(&game_dir, &level_path, hashes_path)?;
    let trimmed = instance_uid.trim().trim_start_matches("0x");
    let uid = u64::from_str_radix(trimmed, 16)
        .or_else(|_| trimmed.parse::<u64>())
        .map_err(|_| ToolkitError::Parse(format!("cannot read an instance uid from {instance_uid:?}")))?;
    let asset_id = crc64::hash(&zone_path);
    let zone_index = level
        .zone_index_by_asset(asset_id)
        .ok_or_else(|| ToolkitError::Parse(format!("{zone_path} is not a zone of this level")))?;

    let found = level.hibernate_object_for_uid(zone_index as i32, uid);
    Ok(GhostCheck {
        has_far_lod_copy: found.is_some(),
        model: found
            .map(|(_, o)| level.hibernate_model_name(o).to_string())
            .unwrap_or_default(),
        position: found.map(|(_, o)| o.position),
        parent_id: format!("{:08X}", la::fold_instance_uid(uid)),
    })
}

/// Which region-data struct types this build knows how to decode, for a UI that
/// wants to say so. The eighth possibility, `Other`, means a struct this build
/// has no description for — the block is still returned with its fields.
pub fn known_region_data_types() -> Vec<&'static str> {
    vec![
        RegionDataKind::Planet.label(),
        RegionDataKind::Dimension.label(),
        RegionDataKind::CustomOverlay.label(),
        RegionDataKind::AutoShadowZone.label(),
        RegionDataKind::AutoGeneratedOverlay.label(),
        RegionDataKind::PerfSpec.label(),
        RegionDataKind::ClankPuzzle.label(),
    ]
}
