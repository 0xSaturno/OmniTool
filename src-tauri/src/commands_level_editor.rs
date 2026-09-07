//! Tauri commands backing the Level Editor / Viewer tool.
//!
//! Five entry points, all reading the shipped archives read-only:
//!
//! | command             | answers                                             |
//! |---------------------|-----------------------------------------------------|
//! | `level_list_planets`| which regions (planets / missions / instances) exist |
//! | `level_list_zones`  | which `.zone` assets a region is made of            |
//! | `level_load_zone`   | one zone's scene objects, actors, models and bounds |
//! | `level_model_mesh`  | packed LOD geometry for one model, as an ArrayBuffer|
//! | `level_save_zone`   | apply transform edits into a Stager project         |
//!
//! The TOC, the asset-name list and the derived zone tree are memoised per game
//! directory in [`crate::core::level`], so only the first call pays for them.

use std::collections::HashMap;
use std::time::Instant;

use log::info;
use serde::{Deserialize, Serialize};

use crate::core::error::ToolkitError;
use crate::core::level::{self, LevelIndex};
use crate::core::zone::{ActorRow, SceneObject, Zone};

type Result<T> = std::result::Result<T, ToolkitError>;

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LevelRegion {
    pub key: String,
    pub label: String,
    /// `openarea` | `overlay` | `instance` | `other`.
    pub group: String,
    pub zone_count: usize,
    /// Zone count per kind, e.g. `{"art": 42, "gp": 12}`.
    pub kinds: HashMap<String, usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneEntry {
    pub path: String,
    pub id: String,
    /// `art` | `gp` | `lgt` | `vfx` | `nav` | `geo` | `other`.
    pub kind: String,
    pub tile: Option<String>,
    pub size: u32,
    /// False for the many stub zones that hold an empty DAT1 and no geometry.
    pub has_objects: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundsDto {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneModelDto {
    /// `0x`-prefixed 16-hex asset id — the same string `SceneObject.modelId`
    /// carries, so the viewport can key its geometry cache off either.
    pub id: String,
    /// Asset path when the name list knows it, otherwise the hex id.
    pub path: String,
    pub instance_count: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneSectionDto {
    pub tag: String,
    pub name: String,
    pub size: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneScene {
    pub path: String,
    pub id: String,
    pub sizes: Vec<u32>,
    pub dirty: bool,
    pub bounds: Option<BoundsDto>,
    pub models: Vec<ZoneModelDto>,
    pub objects: Vec<SceneObject>,
    pub actors: Vec<ActorRow>,
    pub sections: Vec<ZoneSectionDto>,
}

/// One in-place transform patch. `matrix` is row-major with row 3 as the
/// translation; either field may be omitted.
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ZoneEditDto {
    pub offset: u32,
    #[serde(default)]
    pub matrix: Option<Vec<f32>>,
    #[serde(default)]
    pub scale: Option<[f32; 3]>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveResult {
    pub asset_path: String,
    pub bytes_written: usize,
    pub project_file: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneStats {
    pub total_zones: usize,
    pub regions: usize,
    pub with_objects: usize,
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn hex_id(id: u64) -> String {
    format!("{id:#018X}")
}

/// Parse an id as produced by [`hex_id`] (the `0x` prefix is optional).
fn parse_hex_id(s: &str) -> Result<u64> {
    let t = s.trim();
    let t = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    u64::from_str_radix(t, 16)
        .map_err(|e| ToolkitError::Parse(format!("invalid asset id {s:?}: {e}")))
}

fn index(game_dir: &str, hashes_path: Option<String>) -> Result<std::sync::Arc<LevelIndex>> {
    level::get_index(game_dir, hashes_path.as_deref())
}

/// Load a zone asset by path and parse it.
fn load_zone(index: &LevelIndex, zone_path: &str) -> Result<(String, u64, Zone)> {
    let path = crate::core::crc64::normalize_path(zone_path);
    // The path is the identity: hashing it gives the TOC id directly, so a zone
    // still resolves when the name list is missing or out of date.
    let id = crate::core::crc64::hash_raw(path.as_bytes());
    let raw = index.extract(id)?;
    let zone = Zone::parse(&raw)?;
    Ok((path, id, zone))
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

/// Every region the shipped archives contain, sorted by key.
#[tauri::command]
pub async fn level_list_planets(
    game_dir: String,
    hashes_path: Option<String>,
) -> Result<Vec<LevelRegion>> {
    let start = Instant::now();
    let idx = index(&game_dir, hashes_path)?;
    idx.require_names()?;

    let regions: Vec<LevelRegion> = idx
        .regions
        .iter()
        .map(|r| LevelRegion {
            key: r.key.clone(),
            label: r.label.clone(),
            group: r.group.to_string(),
            zone_count: r.zone_count,
            kinds: r.kinds.clone(),
        })
        .collect();

    info!(
        "level_list_planets: {} regions in {:?}",
        regions.len(),
        start.elapsed()
    );
    Ok(regions)
}

/// Every `.zone` asset belonging to one region, sorted by path.
#[tauri::command]
pub async fn level_list_zones(
    game_dir: String,
    region: String,
    hashes_path: Option<String>,
) -> Result<Vec<ZoneEntry>> {
    let idx = index(&game_dir, hashes_path)?;
    idx.require_names()?;

    let start = Instant::now();
    let recs: Vec<&level::ZoneRec> = idx.zones.iter().filter(|z| z.region == region).collect();

    // `hasObjects` is measured, not guessed (see `level::zone_has_objects`), so
    // warm the whole region in parallel before reading the answers back.
    let ids: Vec<u64> = recs.iter().map(|z| z.id).collect();
    level::prewarm_has_objects(&idx, &ids);

    let zones: Vec<ZoneEntry> = recs
        .iter()
        .map(|z| ZoneEntry {
            path: z.path.clone(),
            id: hex_id(z.id),
            kind: z.kind.to_string(),
            tile: z.tile.clone(),
            size: z.size,
            has_objects: level::zone_has_objects(&idx, z.id),
        })
        .collect();

    info!(
        "level_list_zones: {} zones in {region} in {:?}",
        zones.len(),
        start.elapsed()
    );
    Ok(zones)
}

/// Parse one zone: every scene object, the actor table, the distinct models
/// with their instance counts and the DAT1 section census.
#[tauri::command]
pub async fn level_load_zone(
    game_dir: String,
    zone_path: String,
    hashes_path: Option<String>,
) -> Result<ZoneScene> {
    let start = Instant::now();
    let idx = index(&game_dir, hashes_path)?;
    let (path, id, zone) = load_zone(&idx, &zone_path)?;

    // Distinct models, most-instanced first — the viewport uploads them in that
    // order so the heaviest instanced draws appear first.
    let mut counts: HashMap<u64, usize> = HashMap::new();
    for o in zone.model_instances() {
        *counts.entry(o.model_id).or_insert(0) += 1;
    }
    let mut models: Vec<ZoneModelDto> = counts
        .into_iter()
        .map(|(mid, n)| ZoneModelDto {
            id: hex_id(mid),
            path: idx
                .name_of(mid)
                .map(str::to_string)
                .unwrap_or_else(|| hex_id(mid)),
            instance_count: n,
        })
        .collect();
    models.sort_by(|a, b| {
        b.instance_count
            .cmp(&a.instance_count)
            .then_with(|| a.path.cmp(&b.path))
    });

    let sections: Vec<ZoneSectionDto> = zone
        .dat1
        .sections
        .iter()
        .enumerate()
        .map(|(i, s)| ZoneSectionDto {
            tag: format!("{:#010X}", s.tag),
            name: level::zone_section_name(s.tag)
                .unwrap_or("unknown")
                .to_string(),
            size: zone.dat1.section_data[i].len(),
        })
        .collect();

    let bounds = zone.bounds().map(|(min, max)| BoundsDto { min, max });

    let scene = ZoneScene {
        path,
        id: hex_id(id),
        sizes: zone.sizes.to_vec(),
        dirty: zone.is_dirty(),
        bounds,
        models,
        objects: zone.scene_objects.clone(),
        actors: zone.actors.clone(),
        sections,
    };

    info!(
        "level_load_zone {}: {} objects, {} models, {} actors in {:?}",
        scene.path,
        scene.objects.len(),
        scene.models.len(),
        scene.actors.len(),
        start.elapsed()
    );
    Ok(scene)
}

/// Packed LOD geometry for one model, delivered to JS as an `ArrayBuffer`.
///
/// A model that cannot be extracted or decoded returns the empty buffer (header
/// with zero counts) rather than an error, so one bad asset does not abort the
/// zone's upload loop.
#[tauri::command]
pub async fn level_model_mesh(
    game_dir: String,
    model_id: String,
    lod: Option<u32>,
) -> Result<tauri::ipc::Response> {
    let idx = index(&game_dir, None)?;
    let id = parse_hex_id(&model_id)?;
    let bytes = level::model_mesh_bytes(&idx, id, lod.unwrap_or(0));
    Ok(tauri::ipc::Response::new(bytes.to_vec()))
}

/// Apply transform edits to a fresh copy of the zone and write the rebuilt
/// asset into a Stager project, ready for the existing `export_stage` packer.
///
/// The file lands at `projects/<project>/0/<asset path>` holding the 36-byte
/// TOC asset header followed by the asset body — byte-for-byte the layout
/// `extract_asset_to_project` writes, which is what `export_stage` zips and
/// what Overstrike installs.
#[tauri::command]
pub async fn level_save_zone(
    game_dir: String,
    zone_path: String,
    project_name: String,
    edits: Vec<ZoneEditDto>,
) -> Result<SaveResult> {
    let start = Instant::now();
    let idx = index(&game_dir, None)?;
    let (path, _, mut zone) = load_zone(&idx, &zone_path)?;

    for e in &edits {
        if let Some(m) = &e.matrix {
            if m.len() != 16 {
                return Err(ToolkitError::Parse(format!(
                    "edit at offset {}: matrix must have 16 elements, got {}",
                    e.offset,
                    m.len()
                )));
            }
            let mut arr = [0f32; 16];
            arr.copy_from_slice(m);
            zone.set_matrix(e.offset, arr)?;
        }
        if let Some(s) = e.scale {
            zone.set_scale(e.offset, s)?;
        }
    }

    let project_dir = crate::core::filesystem::projects_dir()?.join(&project_name);
    if !project_dir.join("info.json").exists() {
        crate::commands::create_project(
            project_name.clone(),
            "RCRA".to_string(),
            "OmniTool".to_string(),
        )
        .await?;
    }

    let out_path = project_dir.join("0").join(&path);
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = zone.to_bytes();
    std::fs::write(&out_path, &bytes)?;

    info!(
        "level_save_zone: {} edit(s) -> {} ({} bytes) in {:?}",
        edits.len(),
        out_path.display(),
        bytes.len(),
        start.elapsed()
    );

    Ok(SaveResult {
        asset_path: path,
        bytes_written: bytes.len(),
        project_file: out_path.to_string_lossy().into_owned(),
    })
}

/// Headline counts for the tool's landing screen.
///
/// `withObjects` is measured, not estimated, which means probing all ~9,000
/// zones once: a few seconds on the first call (then instant, and every later
/// `level_list_zones` in the session is instant too). Call it lazily, behind a
/// spinner — not on mount alongside `level_list_planets`.
#[tauri::command]
pub async fn level_zone_stats(
    game_dir: String,
    hashes_path: Option<String>,
) -> Result<ZoneStats> {
    let idx = index(&game_dir, hashes_path)?;
    // `withObjects` is measured, not estimated: every zone is probed once,
    // across the rayon pool, and memoised. That costs a couple of seconds on
    // the first call and warms the cache every later `level_list_zones` reads.
    let start = Instant::now();
    let ids: Vec<u64> = idx.zones.iter().map(|z| z.id).collect();
    level::prewarm_has_objects(&idx, &ids);
    let with_objects = ids
        .iter()
        .filter(|&&id| level::zone_has_objects(&idx, id))
        .count();
    info!(
        "level_zone_stats: {} of {} zones carry objects, probed in {:?}",
        with_objects,
        ids.len(),
        start.elapsed()
    );

    Ok(ZoneStats {
        total_zones: idx.zones.len(),
        regions: idx.regions.len(),
        with_objects,
    })
}
