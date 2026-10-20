//! Tauri commands backing the Level Editor / Viewer tool.
//!
//! Six entry points, all reading the shipped archives read-only:
//!
//! | command               | answers                                            |
//! |-----------------------|----------------------------------------------------|
//! | `level_list_planets`  | which regions (planets / missions / instances) exist |
//! | `level_list_zones`    | which `.zone` assets a region is made of           |
//! | `level_load_zone`     | one zone's scene objects, actors, models and bounds |
//! | `level_model_mesh`    | packed LOD geometry for one model, as an ArrayBuffer |
//! | `level_zone_geometry` | packed geometry an impostor or decal keeps in the zone's own block 2 |
//! | `level_model_materials` | one model's material slots, the textures they bind, and the blend recipe when they have one |
//! | `level_texture`       | one texture decoded to a PNG, as an ArrayBuffer    |
//! | `level_save_zone`     | apply transform edits into a Stager project        |
//!
//! The TOC, the asset-name list and the derived zone tree are memoised per game
//! directory in [`crate::core::level`], so only the first call pays for them.

use std::collections::HashMap;
use std::time::Instant;

use log::info;
use serde::{Deserialize, Serialize};

use crate::core::error::ToolkitError;
use crate::core::level::{self, LevelIndex};
use crate::core::model_materials::{model_materials, ModelMaterials};
use crate::core::texture_serve::{self, DEFAULT_MAX_SIZE};
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
    /// Real block sizes, block 0 first. Only a prefix of `sizes` are block
    /// sizes, so this is the list the viewer should trust.
    pub blocks: Vec<u32>,
    /// How many scene objects carry a mesh inside block 2 (impostors, decals).
    pub block_geometry_count: usize,
    pub dirty: bool,
    pub bounds: Option<BoundsDto>,
    pub models: Vec<ZoneModelDto>,
    pub objects: Vec<SceneObject>,
    pub actors: Vec<ActorRow>,
    pub sections: Vec<ZoneSectionDto>,
    /// What the zone would not fully decode, so the UI can say so instead of
    /// presenting a partial read as a complete one. Empty on every shipped zone
    /// but one; see [`Zone::undecoded_priuses`](crate::core::zone::Zone::undecoded_priuses).
    pub warnings: Vec<String>,
    /// Read from the selected project's copy rather than the game archives.
    pub from_project: bool,
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

/// Load a zone by path: `project`'s copy when it has one, else the archives, so
/// offsets always match the file a save will patch. The bool is true for the copy.
fn load_zone(index: &LevelIndex, zone_path: &str, project: Option<&str>) -> Result<(String, u64, Zone, bool)> {
    let path = crate::core::crc64::normalize_path(zone_path);
    // The path is the identity: hashing it gives the TOC id directly, so a zone
    // still resolves when the name list is missing or out of date.
    let id = crate::core::crc64::hash_raw(path.as_bytes());
    if let Some(copy) = project_zone_file(project, &path)?.filter(|p| p.is_file()) {
        return Ok((path, id, Zone::parse(&std::fs::read(copy)?)?, true));
    }
    let raw = index.extract(id)?;
    let zone = Zone::parse(&raw)?;
    Ok((path, id, zone, false))
}

/// `projects/<name>/0/<path>`, refusing names and paths that would leave it.
fn project_zone_file(project: Option<&str>, path: &str) -> Result<Option<std::path::PathBuf>> {
    let Some(name) = project.filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    let bad_name = name.contains(['/', '\\', ':']) || name == "." || name == "..";
    let bad_path = path.contains(':') || path.starts_with('/') || path.split('/').any(|s| s == "..");
    if bad_name || bad_path {
        return Err(ToolkitError::Parse(format!("refusing project {name:?} / zone {path:?}")));
    }
    Ok(Some(crate::core::filesystem::projects_dir()?.join(name).join("0").join(path)))
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
    project_name: Option<String>,
) -> Result<ZoneScene> {
    let start = Instant::now();
    let idx = index(&game_dir, hashes_path)?;
    let (path, id, zone, from_project) = load_zone(&idx, &zone_path, project_name.as_deref())?;

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

    let mut blocks = vec![zone.sizes[0]];
    blocks.extend(zone.block_spans.iter().map(|&(_, n)| n as u32));
    let block_geometry_count = zone
        .scene_objects
        .iter()
        .filter(|o| o.geometry.is_some())
        .count();

    let warnings: Vec<String> = zone
        .undecoded_priuses()
        .iter()
        .map(|(off, why)| format!("prius blob at {off} did not decode: {why}"))
        .collect();

    let scene = ZoneScene {
        path,
        id: hex_id(id),
        sizes: zone.sizes.to_vec(),
        blocks,
        block_geometry_count,
        dirty: zone.is_dirty(),
        bounds,
        models,
        objects: zone.scene_objects.clone(),
        actors: zone.actors.clone(),
        sections,
        warnings,
        from_project,
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

/// Packed geometry for one scene object that owns a mesh inside the zone's own
/// raw block 2 — an impostor (kind 6) or a decal (kind 3).
///
/// The buffer is the same `"RCMS"` layout `level_model_mesh` returns, and the
/// vertices are in the entry's local space, so the viewport places it with the
/// object's own 4x4 exactly as it places a model instance.
///
/// This is the only route to impostor geometry: 645 `*impostors*` zones ship
/// (456 on megalopolis), holding 2.67 M triangles of distant skyline that no
/// `.model` asset contains.
#[tauri::command]
pub async fn level_zone_geometry(
    game_dir: String,
    zone_path: String,
    offset: u32,
    project_name: Option<String>,
) -> Result<tauri::ipc::Response> {
    let idx = index(&game_dir, None)?;
    let (_, _, zone, _) = load_zone(&idx, &zone_path, project_name.as_deref())?;
    let bytes = match level::decode_zone_geometry(&zone, offset) {
        Ok(m) => m.pack(),
        Err(e) => {
            info!("level_zone_geometry {zone_path} @{offset}: {e}");
            level::empty_mesh_bytes()
        }
    };
    Ok(tauri::ipc::Response::new(bytes))
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

/// One model's material slots, resolved down to the textures they bind.
///
/// `slots[i].index` is exactly the `materialIndex` the RCMS submesh header
/// carries, so a draw call goes straight to its textures. Nothing here fails
/// per slot: a material missing from the archives comes back
/// `resolved: false, placeholder: true` with no textures, which is the
/// viewport's cue to keep its neutral debug colour for that subset — 27% of
/// shipped materials bind nothing but 2x2 pink placeholders, and drawing those
/// would paper a grid over the level.
///
/// Resolution is memoised per model id, and the `.texture` headers it reads are
/// **partial** archive reads (256 bytes each), so cataloguing a busy zone's
/// several hundred models costs a fraction of a second and never decompresses a
/// pixel.
///
/// # `slot.blend`
///
/// A slot whose material instances the `gbl_rgb_blend` family — about two thirds
/// of an art tile's slots — also carries a
/// [`BlendRecipe`](crate::core::model_materials::BlendRecipe): an RGB splat map
/// plus four colour layers with per-layer tiling and HSL, which is what the
/// surface actually shows. For those slots a single albedo out of `textures` is
/// one substance out of four. The field is additive and omitted entirely for
/// every other material, so a caller that ignores it behaves exactly as before.
#[tauri::command]
pub async fn level_model_materials(
    game_dir: String,
    model_id: String,
) -> Result<ModelMaterials> {
    let idx = index(&game_dir, None)?;
    let id = parse_hex_id(&model_id)?;
    Ok((*model_materials(&idx, id)).clone())
}

/// One texture decoded to a PNG, delivered to JS as an `ArrayBuffer`.
///
/// The mip served is the **smallest resident mip at or above `maxSize`** (512
/// by default, clamped to 16..2048): a 4K albedo is pointless for a model
/// covering a few hundred screen pixels, and the resident chain already starts
/// well below native. The streamed HD tiles are never fetched.
///
/// A texture that cannot be found or decoded returns an **empty** buffer rather
/// than an error, so one unsupported format does not abort a zone's upload
/// loop. Decoded PNGs are memoised in a byte-budgeted LRU keyed by
/// `(texture, cap)`.
#[tauri::command]
pub async fn level_texture(
    game_dir: String,
    texture_id: String,
    max_size: Option<u32>,
) -> Result<tauri::ipc::Response> {
    let idx = index(&game_dir, None)?;
    let id = parse_hex_id(&texture_id)?;
    let png = texture_serve::texture_png_bytes(&idx, id, max_size.unwrap_or(DEFAULT_MAX_SIZE));
    Ok(tauri::ipc::Response::new(png.to_vec()))
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
    // Start from the project's copy so earlier saves and other tools' edits survive.
    let (path, _, mut zone, _) = load_zone(&idx, &zone_path, Some(project_name.as_str()))?;

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
