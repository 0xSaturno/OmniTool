//! Level index + model-mesh decoding for the Level Editor / Viewer tool.
//!
//! Two jobs live here, both shared by every `commands_level_editor` command:
//!
//! * [`LevelIndex`] — a per-game-directory cache of the parsed TOC, the
//!   `asset id -> path` name map and the derived zone / region tree. Building it
//!   costs a second (the TOC is ~200k assets and the name list ~1.3M lines), so
//!   it is memoised in a process-global map keyed by (game dir, hashes file).
//!
//! * [`decode_model_mesh`] — turns a `.model` asset into the flat
//!   position/normal/uv/index buffers the 3D viewport uploads, reusing the
//!   section parsers in `tools::model_converter`. Results are memoised in a
//!   size-bounded LRU so panning back over a tile does not re-decode.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use dashmap::DashMap;
use log::{debug, info, warn};

use crate::core::dat1::DAT1_MAGIC;
use crate::core::error::{Result, ToolkitError};
use crate::core::toc::{ArchiveCache, Toc, TocAsset};
use crate::tools::model_converter::model::ModelFile;
use crate::tools::model_converter::sections::built::{
    get_position_scale, get_uv_scale, TAG_BUILT,
};
use crate::tools::model_converter::sections::geo::{
    IndexesSection, VertexesSection, DEFAULT_UV_SCALE, TAG_INDEXES, TAG_VERTEXES,
};
use crate::tools::model_converter::sections::look::{LookSection, TAG_LOOK};
use crate::tools::model_converter::sections::meshes::{MeshDefinition, TAG_MESHES};

// ---------------------------------------------------------------------------
// Zone / region classification
// ---------------------------------------------------------------------------

/// A zone whose TOC payload is at most this many bytes is an empty DAT1 stub —
/// measured with `level_check`, the shipped stubs are 100 bytes and the
/// smallest zone that carries scene objects is 324. Used only to skip the
/// extraction probe for the ~40% of zones that are stubs.
pub const STUB_MAX_SIZE: u32 = 256;

/// Does this zone actually carry scene objects?
///
/// Neither size nor kind predicts this: the shipped game has 2.5 MB `_art`
/// zones with no Zone Scene Objects section at all (their content is baked
/// impostors and decal geometry) and 324-byte `_lgt` zones that do have one.
/// So it is measured: the asset is extracted once and its DAT1 section table
/// read (see [`scene_objects_present`]), and the answer memoised per asset id
/// for the life of the process.
///
/// [`prewarm_has_objects`] fills the memo for a whole region in parallel, which
/// is what makes measuring this affordable for a 2,000-zone planet.
pub fn zone_has_objects(index: &LevelIndex, id: u64) -> bool {
    if let Some(hit) = probe_cache().get(&id) {
        return *hit;
    }
    if index.asset(id).is_some_and(|a| a.size <= STUB_MAX_SIZE) {
        probe_cache().insert(id, false);
        return false;
    }
    let answer = index
        .extract(id)
        .ok()
        .is_some_and(|raw| scene_objects_present(&raw));
    probe_cache().insert(id, answer);
    answer
}

/// Will parsing this zone yield any scene objects?
///
/// `Zone::build_index` walks the Zone Scene Objects heap at the offsets listed
/// by the six instance tables, so the heap alone is not enough: 27 of the 400
/// sampled zones carry a non-empty heap with no table pointing into it. Both
/// conditions are therefore required, and both are answered from the DAT1
/// section table — 16 bytes plus 12 per section. Parsing the zone properly
/// would copy every section body, which on a 16 MB art zone dwarfs the question
/// being asked.
fn scene_objects_present(asset: &[u8]) -> bool {
    use crate::core::zone::{
        T_ACTORS, T_DECALS, T_LIGHTS, T_MODEL_INSTS, T_SCENE_OBJECTS, T_VFX_INSTS, T_VOLUMES,
    };
    const HDR: usize = crate::core::zone::ASSET_HEADER_LEN;
    /// The tables `build_index` reads scene-object offsets out of.
    const INSTANCE_TABLES: [u32; 6] = [
        T_MODEL_INSTS,
        T_LIGHTS,
        T_VOLUMES,
        T_DECALS,
        T_VFX_INSTS,
        T_ACTORS,
    ];

    let u32_at = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());

    let body = if asset.len() >= 4 && u32_at(asset, 0) == DAT1_MAGIC {
        asset
    } else if asset.len() >= HDR + 4 && u32_at(asset, HDR) == DAT1_MAGIC {
        &asset[HDR..]
    } else {
        return false;
    };
    if body.len() < 16 {
        return false;
    }

    let count = u16::from_le_bytes(body[12..14].try_into().unwrap()) as usize;
    let (mut heap, mut table) = (false, false);
    for i in 0..count {
        let o = 16 + i * 12;
        if o + 12 > body.len() {
            break;
        }
        let (tag, size) = (u32_at(body, o), u32_at(body, o + 8));
        if size == 0 {
            continue;
        }
        if tag == T_SCENE_OBJECTS {
            heap = true;
        } else if INSTANCE_TABLES.contains(&tag) {
            table = true;
        }
    }
    heap && table
}

/// Measure [`zone_has_objects`] for many zones at once, across the rayon pool.
pub fn prewarm_has_objects(index: &LevelIndex, ids: &[u64]) {
    use rayon::prelude::*;
    ids.par_iter().for_each(|&id| {
        zone_has_objects(index, id);
    });
}

fn probe_cache() -> &'static DashMap<u64, bool> {
    static CACHE: OnceLock<DashMap<u64, bool>> = OnceLock::new();
    CACHE.get_or_init(DashMap::new)
}

/// The four top-level buckets the tool groups regions into.
pub fn group_of(dir: &str) -> &'static str {
    match dir {
        "openarea" => "openarea",
        "overlay" => "overlay",
        "instance" => "instance",
        _ => "other",
    }
}

/// Coarse zone kind, derived from the file name. The suffixes overlap
/// (`tile_x_gp_crates`, `tile_x_art_parade_kalebo`), so this is a priority
/// ordered substring test rather than an exact suffix match.
pub fn zone_kind(file_stem: &str) -> &'static str {
    let s = file_stem;
    if s.contains("_art") {
        "art"
    } else if s.contains("_gp") || s.contains("_gameplay") {
        "gp"
    } else if s.contains("_lgt") || s.contains("_light") {
        "lgt"
    } else if s.contains("_vfx") || s.contains("_fx") {
        "vfx"
    } else if s.contains("nav") {
        "nav"
    } else if s.contains("_geo") {
        "geo"
    } else {
        "other"
    }
}

/// One `.zone` asset present in the TOC.
#[derive(Debug, Clone)]
pub struct ZoneRec {
    /// Lowercase forward-slash asset path.
    pub path: String,
    pub id: u64,
    /// Key of the [`RegionRec`] this zone belongs to.
    pub region: String,
    pub kind: &'static str,
    /// `tile_<xy>` directory component, when the zone lives in one.
    pub tile: Option<String>,
    pub size: u32,
}

/// A group of zones the user picks from the tool's sidebar — a planet under
/// `openarea`, a mission under `overlay`, an instance under `instance`.
#[derive(Debug, Clone)]
pub struct RegionRec {
    pub key: String,
    pub label: String,
    pub group: &'static str,
    pub zone_count: usize,
    pub kinds: HashMap<String, usize>,
}

/// Split a zone asset path into the region it belongs to.
///
/// `levels/<level>/<group>/<region…>/[tile_<xy>/]<file>.zone`
///
/// The `tile_<xy>` component is peeled off so every tile of a planet lands in
/// one region; everything else between the group and the file name stays in the
/// key, which keeps `overlay/storymission/<mission>` and
/// `instance/pocketdimensions/<pd>` usefully separate.
fn classify(path: &str) -> (String, String, &'static str, &'static str, Option<String>) {
    let parts: Vec<&str> = path.split('/').collect();
    let file = parts.last().copied().unwrap_or("");
    let stem = file.strip_suffix(".zone").unwrap_or(file);
    let kind = zone_kind(stem);

    if parts.len() < 3 || parts[0] != "levels" {
        let dir = parts[..parts.len().saturating_sub(1)].join("/");
        let key = if dir.is_empty() { "misc".to_string() } else { dir };
        let label = key.clone();
        return (key, label, "other", kind, None);
    }

    let level = parts[1];
    // Directory components between the level and the file name.
    let rest = &parts[2..parts.len() - 1];
    if rest.is_empty() {
        let key = format!("{level}/(root)");
        return (key.clone(), key, "other", kind, None);
    }

    let group_dir = rest[0];
    let group = group_of(group_dir);
    let mut region_dirs = &rest[1..];
    let mut tile = None;
    if let Some(last) = region_dirs.last() {
        if last.starts_with("tile_") {
            tile = Some((*last).to_string());
            region_dirs = &region_dirs[..region_dirs.len() - 1];
        }
    }

    let sub = if region_dirs.is_empty() {
        group_dir.to_string()
    } else {
        region_dirs.join("/")
    };
    let key = format!("{level}/{group_dir}/{sub}");
    let label = format!("{level} / {sub}");
    (key, label, group, kind, tile)
}

// ---------------------------------------------------------------------------
// LevelIndex
// ---------------------------------------------------------------------------

pub struct LevelIndex {
    pub game_dir: PathBuf,
    pub toc: Toc,
    /// Lazily-mmapped archive files. Reading an asset through this costs a
    /// memcpy instead of a whole-archive `read()` — the difference between a
    /// 200 ms and a sub-millisecond zone load.
    archives: ArchiveCache,
    /// Preferred TOC entry per asset id (span 0 when the asset has one).
    pub assets: HashMap<u64, TocAsset>,
    /// `asset id -> lowercase forward-slash path`, from the hashes file.
    pub names: HashMap<u64, String>,
    /// True when no hashes file could be read; `names` is then empty and the
    /// region tree cannot be built.
    pub names_missing: bool,
    pub hashes_source: String,
    pub zones: Vec<ZoneRec>,
    pub regions: Vec<RegionRec>,
    zone_by_path: HashMap<String, usize>,
}

impl LevelIndex {
    pub fn zone(&self, path: &str) -> Option<&ZoneRec> {
        let norm = crate::core::crc64::normalize_path(path);
        self.zone_by_path.get(&norm).map(|&i| &self.zones[i])
    }

    pub fn asset(&self, id: u64) -> Option<&TocAsset> {
        self.assets.get(&id)
    }

    /// Read an asset out of the archives. The TOC's archive names already carry
    /// the `d\` prefix, so the archive root is the game directory itself.
    pub fn extract(&self, id: u64) -> Result<Vec<u8>> {
        let asset = self
            .asset(id)
            .ok_or_else(|| ToolkitError::Parse(format!("asset {id:016X} not found in TOC")))?;
        self.toc.extract_asset_with_cache(asset, &self.archives)
    }

    pub fn name_of(&self, id: u64) -> Option<&str> {
        self.names.get(&id).map(|s| s.as_str())
    }

    /// Error to hand back from commands that cannot work without the name list.
    pub fn require_names(&self) -> Result<()> {
        if self.names_missing {
            return Err(ToolkitError::Parse(format!(
                "asset name list not found ({}). Download it from the Setup tab first.",
                self.hashes_source
            )));
        }
        Ok(())
    }
}

type IndexMap = HashMap<String, Arc<LevelIndex>>;

fn index_cache() -> &'static Mutex<IndexMap> {
    static CACHE: OnceLock<Mutex<IndexMap>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn resolve_hashes_path(explicit: Option<&str>) -> (Option<PathBuf>, String) {
    if let Some(p) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        let path = PathBuf::from(p);
        let display = path.display().to_string();
        return if path.is_file() {
            (Some(path), display)
        } else {
            (None, display)
        };
    }
    match crate::core::filesystem::hashes_path() {
        Ok(path) => {
            let display = path.display().to_string();
            if path.is_file() {
                (Some(path), display)
            } else {
                (None, display)
            }
        }
        Err(e) => (None, format!("<app dir unavailable: {e}>")),
    }
}

/// Parse a `hashes` file into `asset id -> path`.
///
/// The shipped list mixes separators and casing, so paths are normalised the
/// same way [`crate::core::crc64::hash`] does before they are stored — the
/// viewer compares them against paths it hashes itself.
fn load_names(path: &Path) -> Result<HashMap<u64, String>> {
    let text = std::fs::read_to_string(path)?;
    let mut map = HashMap::with_capacity(text.len() / 64);
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, ',');
        let (Some(hex), Some(p)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Ok(id) = u64::from_str_radix(hex, 16) else {
            continue;
        };
        map.entry(id)
            .or_insert_with(|| crate::core::crc64::normalize_path(p));
    }
    Ok(map)
}

/// Get (building on first use) the index for a game directory.
pub fn get_index(game_dir: &str, hashes_path: Option<&str>) -> Result<Arc<LevelIndex>> {
    let game = PathBuf::from(game_dir.trim());
    if !game.is_dir() {
        return Err(ToolkitError::Parse(format!(
            "game directory not found: {}",
            game.display()
        )));
    }
    let (hashes, hashes_source) = resolve_hashes_path(hashes_path);
    // Keyed by game directory alone. The name list is a property of the index,
    // not of the caller: `level_model_mesh` and `level_save_zone` do not take a
    // hashes path, and re-parsing a 280,000-entry TOC for them would cost a
    // second every time. A cached index is reused unless it has no names and
    // this call can supply some.
    let key = game.to_string_lossy().to_lowercase();

    if let Some(hit) = index_cache().lock().unwrap().get(&key) {
        if !hit.names_missing || hashes.is_none() {
            return Ok(hit.clone());
        }
    }

    let start = std::time::Instant::now();
    let toc_path = game.join("toc");
    let toc_bytes = std::fs::read(&toc_path).map_err(|e| {
        ToolkitError::Parse(format!("failed to read {}: {e}", toc_path.display()))
    })?;
    let toc = Toc::parse(&toc_bytes)?;

    // One entry per id. Assets can appear in several spans (SD/HD textures);
    // span 0 is the one the zone/model readers want.
    let mut assets: HashMap<u64, TocAsset> = HashMap::with_capacity(toc.asset_ids().len());
    for a in toc.assets() {
        match assets.get(&a.asset_id) {
            Some(existing) if existing.span_index <= a.span_index => {}
            _ => {
                assets.insert(a.asset_id, a);
            }
        }
    }

    let names_missing = hashes.is_none();
    let names = match &hashes {
        Some(p) => load_names(p).unwrap_or_else(|e| {
            warn!("level index: could not read {}: {e}", p.display());
            HashMap::new()
        }),
        None => HashMap::new(),
    };

    // Zones = every TOC asset whose known name is a `.zone` under `levels/`.
    let mut zones: Vec<ZoneRec> = Vec::new();
    for (id, asset) in &assets {
        let Some(path) = names.get(id) else { continue };
        if !path.ends_with(".zone") {
            continue;
        }
        let (region, _, _, kind, tile) = classify(path);
        zones.push(ZoneRec {
            path: path.clone(),
            id: *id,
            region,
            kind,
            tile,
            size: asset.size,
        });
    }
    zones.sort_by(|a, b| a.path.cmp(&b.path));

    let mut regions: HashMap<String, RegionRec> = HashMap::new();
    for z in &zones {
        let (key, label, group, _, _) = classify(&z.path);
        let e = regions.entry(key.clone()).or_insert_with(|| RegionRec {
            key,
            label,
            group,
            zone_count: 0,
            kinds: HashMap::new(),
        });
        e.zone_count += 1;
        *e.kinds.entry(z.kind.to_string()).or_insert(0) += 1;
    }
    let mut regions: Vec<RegionRec> = regions.into_values().collect();
    regions.sort_by(|a, b| a.key.cmp(&b.key));

    let zone_by_path = zones
        .iter()
        .enumerate()
        .map(|(i, z)| (z.path.clone(), i))
        .collect();

    let archives = toc.archive_cache(&game);
    let index = Arc::new(LevelIndex {
        game_dir: game,
        toc,
        archives,
        assets,
        names,
        names_missing,
        hashes_source,
        zones,
        regions,
        zone_by_path,
    });

    info!(
        "level index built: {} assets, {} zones, {} regions in {:?}",
        index.assets.len(),
        index.zones.len(),
        index.regions.len(),
        start.elapsed()
    );

    index_cache()
        .lock()
        .unwrap()
        .insert(key, Arc::clone(&index));
    Ok(index)
}

/// Drop every cached index. Used by the verification harness to time a cold load.
pub fn clear_index_cache() {
    index_cache().lock().unwrap().clear();
}

/// Drop the memoised `hasObjects` answers. Harness-only, for cold timings.
pub fn clear_probe_cache() {
    probe_cache().clear();
}

// ---------------------------------------------------------------------------
// Zone DAT1 section names
// ---------------------------------------------------------------------------

/// Names for the DAT1 section tags that show up in `.zone` assets, so the
/// inspector panel reads as something other than a column of hex.
pub fn zone_section_name(tag: u32) -> Option<&'static str> {
    Some(match tag {
        0x06AB_CAB2 => "Zone Scene Objects",
        0x6987_F172 => "Zone Model Insts",
        0xC6A5_905E => "Zone Model Names",
        0x7068_2CB8 => "Zone Actors",
        0xDC62_5B3D => "Zone Actor Names",
        0x7868_4035 => "Actor Asset Refs",
        0x50ED_C53D => "Zone Actor Priuses",
        0x0CF5_8A6E => "Zone Actor Groups",
        0x97FF_6EB5 => "Zone Lights",
        0xBDAB_2B0D => "Zone Volumes",
        0xBCF4_3558 => "Zone Decals",
        0x91DE_11D9 => "Zone Decal Assets",
        0x6575_12BB => "Zone Decal Geometry",
        0x927C_4EC3 => "Zone Visual Effect Insts",
        0x3D5E_2FEF => "Zone Visual Effect Names",
        0x30DA_DA09 => "Zone Asset References",
        0xB6A0_B72A => "Zone Atmosphere Name",
        0x0277_95C5 => "Zone EnvProbe Assets",
        0x5570_13E9 => "Zone EnvProbe List",
        0xDC31_1FC3 => "Zone Impostors",
        0xCB8D_34F9 => "Zone Impostors Atlas",
        0xE415_8AC3 => "Zone Material Overrides",
        0x17AF_FFCE => "Zone Override Tris",
        0x5E54_ACCF => "Zone Script Actions",
        0xBEAB_52E7 => "Zone Script Plugs",
        0x2300_D240 => "Zone Script Priuses",
        0xEF86_37D5 => "Zone Script Strings",
        0xD86A_7934 => "Zone Script Vars",
        0x7CA7_267D => "Level Built",
        0x2BA3_3702 => "Level Zone Names",
        0x4E02_3760 => "Level Zones Built",
        0x4130_D903 => "Level Region Names",
        0x396F_9418 => "Level Regions Built",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Model mesh decoding
// ---------------------------------------------------------------------------

/// `"RCMS"` — magic of the packed mesh buffer handed to the viewport.
pub const MESH_MAGIC: u32 = 0x534D_4352;
pub const MESH_VERSION: u32 = 1;

#[derive(Debug, Default)]
pub struct SubmeshOut {
    pub first_index: u32,
    pub index_count: u32,
    pub material_index: u32,
    pub flags: u32,
}

#[derive(Debug, Default)]
pub struct MeshOut {
    pub positions: Vec<f32>,
    pub normals: Vec<f32>,
    pub uvs: Vec<f32>,
    pub indices: Vec<u32>,
    pub submeshes: Vec<SubmeshOut>,
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl MeshOut {
    pub fn vertex_count(&self) -> usize {
        self.positions.len() / 3
    }

    /// The little-endian buffer documented in the IPC contract:
    /// header, bounds, positions, normals, uvs, indices, submeshes.
    pub fn pack(&self) -> Vec<u8> {
        let vc = self.vertex_count();
        let mut out = Vec::with_capacity(
            24 + 24 + vc * 32 + self.indices.len() * 4 + self.submeshes.len() * 16,
        );
        out.extend_from_slice(&MESH_MAGIC.to_le_bytes());
        out.extend_from_slice(&MESH_VERSION.to_le_bytes());
        out.extend_from_slice(&(vc as u32).to_le_bytes());
        out.extend_from_slice(&(self.indices.len() as u32).to_le_bytes());
        out.extend_from_slice(&(self.submeshes.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // reserved
        for v in self.min.iter().chain(self.max.iter()) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in &self.positions {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in &self.normals {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in &self.uvs {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for i in &self.indices {
            out.extend_from_slice(&i.to_le_bytes());
        }
        for s in &self.submeshes {
            out.extend_from_slice(&s.first_index.to_le_bytes());
            out.extend_from_slice(&s.index_count.to_le_bytes());
            out.extend_from_slice(&s.material_index.to_le_bytes());
            out.extend_from_slice(&s.flags.to_le_bytes());
        }
        out
    }
}

/// An empty buffer — the "could not decode" answer, so the viewport skips the
/// model instead of surfacing an error dialog per instance.
pub fn empty_mesh_bytes() -> Vec<u8> {
    MeshOut::default().pack()
}

/// The `Built` section's bounding sphere / half-extents, for sanity checks.
/// `bounds[0..3]` is the centre and `bounds[4..7]` the half-extents.
pub fn built_box(model: &ModelFile) -> Option<([f32; 3], [f32; 3])> {
    let b = model.dat1.get_section_data(TAG_BUILT)?;
    let built = crate::tools::model_converter::sections::built::Built::parse(b)?;
    let c = [built.bounds[0], built.bounds[1], built.bounds[2]];
    let e = [
        built.bounds[4].abs(),
        built.bounds[5].abs(),
        built.bounds[6].abs(),
    ];
    Some((
        [c[0] - e[0], c[1] - e[1], c[2] - e[2]],
        [c[0] + e[0], c[1] + e[1], c[2] + e[2]],
    ))
}

/// Parse a `.model` asset as extracted from the TOC.
///
/// Model assets normally have no TOC asset header, but `Toc::extract_asset`
/// prepends one whenever the entry declares it, so retry past the 36 bytes
/// rather than failing.
pub fn parse_model_asset(bytes: &[u8]) -> Result<ModelFile> {
    match ModelFile::parse(bytes) {
        Ok(m) => Ok(m),
        Err(e) => {
            const HDR: usize = crate::core::zone::ASSET_HEADER_LEN;
            if bytes.len() > HDR + 4 {
                let magic = u32::from_le_bytes(bytes[HDR..HDR + 4].try_into().unwrap());
                if magic == DAT1_MAGIC
                    || magic == crate::tools::model_converter::model::MAGIC_RCRA
                {
                    return ModelFile::parse(&bytes[HDR..]);
                }
            }
            Err(e)
        }
    }
}

/// Decode one LOD of look 0 into flat viewport buffers.
///
/// Positions come out of `Model Std Vert` quantised — `VertexesSection` applies
/// the `Built` section's position scale — and subsets flagged `0x10` index the
/// vertex stream relative to their own `vertex_start`, so both are undone here
/// exactly the way `gltf_writer` does it. Triangle winding is reversed to match
/// the glTF/three.js counter-clockwise front-face convention.
pub fn decode_model_mesh(model: &ModelFile, lod: usize) -> Result<MeshOut> {
    let dat1 = &model.dat1;

    let pos_scale = dat1
        .get_section_data(TAG_BUILT)
        .map(get_position_scale)
        .unwrap_or(1.0 / 4096.0);
    let uv_scale = dat1
        .get_section_data(TAG_BUILT)
        .map(get_uv_scale)
        .unwrap_or(DEFAULT_UV_SCALE);

    let vert_data = dat1
        .get_section_data(TAG_VERTEXES)
        .ok_or(ToolkitError::SectionNotFound(TAG_VERTEXES))?;
    let vertexes = VertexesSection::parse_scaled(vert_data, pos_scale, uv_scale)?.vertexes;

    let mesh_data = dat1
        .get_section_data(TAG_MESHES)
        .ok_or(ToolkitError::SectionNotFound(TAG_MESHES))?;
    let meshes = MeshDefinition::parse_all(mesh_data)?;

    let idx_data = dat1
        .get_section_data(TAG_INDEXES)
        .ok_or(ToolkitError::SectionNotFound(TAG_INDEXES))?;
    let indexes = IndexesSection::parse(idx_data)?.values;

    // Look 0, requested LOD. A model with no Model Look section still has
    // subsets, so fall back to drawing all of them rather than nothing.
    let subset_ids: Vec<usize> = match dat1.get_section_data(TAG_LOOK) {
        Some(look_data) => {
            let looks = LookSection::parse(look_data)?.looks;
            let entry = looks
                .first()
                .and_then(|l| l.lods.get(lod).copied())
                .or_else(|| looks.first().and_then(|l| l.lods.first().copied()));
            match entry {
                Some(r) => (r.start as usize..r.start as usize + r.count as usize).collect(),
                None => Vec::new(),
            }
        }
        None => (0..meshes.len()).collect(),
    };

    let mut out = MeshOut {
        min: [f32::INFINITY; 3],
        max: [f32::NEG_INFINITY; 3],
        ..Default::default()
    };

    for mi in subset_ids {
        let Some(mesh) = meshes.get(mi) else { continue };
        let vstart = mesh.vertex_start as usize;
        let vcount = mesh.vertex_count as usize;
        if vcount == 0 || vstart + vcount > vertexes.len() {
            continue;
        }
        let ibase = mesh.index_start as usize;
        let icount = mesh.index_count as usize;
        if ibase + icount > indexes.len() {
            continue;
        }

        let vertex_base = out.vertex_count() as u32;
        for v in &vertexes[vstart..vstart + vcount] {
            out.positions.extend_from_slice(&[v.x, v.y, v.z]);
            out.normals.extend_from_slice(&[v.nx, v.ny, v.nz]);
            out.uvs.extend_from_slice(&[v.u, v.v]);
            for (k, c) in [v.x, v.y, v.z].iter().enumerate() {
                if *c < out.min[k] {
                    out.min[k] = *c;
                }
                if *c > out.max[k] {
                    out.max[k] = *c;
                }
            }
        }

        // Absolute-index subsets are offset by their own vertex_start; relative
        // ones already start at 0.
        let sub = if mesh.has_relative_indices() {
            0u16
        } else {
            mesh.vertex_start as u16
        };
        let first_index = out.indices.len() as u32;
        let mut emitted = 0u32;
        for f in 0..icount / 3 {
            let t = [
                indexes[ibase + f * 3 + 2].wrapping_sub(sub) as u32,
                indexes[ibase + f * 3 + 1].wrapping_sub(sub) as u32,
                indexes[ibase + f * 3].wrapping_sub(sub) as u32,
            ];
            if t.iter().any(|&i| i as usize >= vcount) {
                continue; // malformed subset — drop the triangle, keep the mesh
            }
            for i in t {
                out.indices.push(vertex_base + i);
            }
            emitted += 3;
        }

        out.submeshes.push(SubmeshOut {
            first_index,
            index_count: emitted,
            material_index: mesh.material_index as u32,
            flags: mesh.flags as u32,
        });
    }

    if out.positions.is_empty() {
        out.min = [0.0; 3];
        out.max = [0.0; 3];
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Mesh LRU cache
// ---------------------------------------------------------------------------

/// Roughly the geometry of a whole planet's distinct models; well under what a
/// desktop can hold, and evicting from the front keeps a pan across tiles warm.
const MESH_CACHE_BUDGET: usize = 192 * 1024 * 1024;

struct MeshCache {
    map: DashMap<(u64, u32), Arc<Vec<u8>>>,
    order: Mutex<Vec<(u64, u32)>>,
    bytes: AtomicUsize,
}

fn mesh_cache() -> &'static MeshCache {
    static CACHE: OnceLock<MeshCache> = OnceLock::new();
    CACHE.get_or_init(|| MeshCache {
        map: DashMap::new(),
        order: Mutex::new(Vec::new()),
        bytes: AtomicUsize::new(0),
    })
}

fn cache_get(key: (u64, u32)) -> Option<Arc<Vec<u8>>> {
    let hit = mesh_cache().map.get(&key).map(|v| Arc::clone(&v))?;
    let mut order = mesh_cache().order.lock().unwrap();
    if let Some(pos) = order.iter().position(|k| *k == key) {
        let k = order.remove(pos);
        order.push(k);
    }
    Some(hit)
}

fn cache_put(key: (u64, u32), data: Arc<Vec<u8>>) {
    let cache = mesh_cache();
    let len = data.len();
    if cache.map.insert(key, data).is_none() {
        cache.bytes.fetch_add(len, Ordering::Relaxed);
    }
    let mut order = cache.order.lock().unwrap();
    if let Some(pos) = order.iter().position(|k| *k == key) {
        order.remove(pos);
    }
    order.push(key);
    while cache.bytes.load(Ordering::Relaxed) > MESH_CACHE_BUDGET && order.len() > 1 {
        let victim = order.remove(0);
        if let Some((_, v)) = cache.map.remove(&victim) {
            cache.bytes.fetch_sub(v.len(), Ordering::Relaxed);
        }
    }
}

pub fn mesh_cache_stats() -> (usize, usize) {
    let c = mesh_cache();
    (c.map.len(), c.bytes.load(Ordering::Relaxed))
}

/// Packed LOD geometry for a model asset, decoded at most once per (id, lod).
///
/// Never fails: a model that cannot be extracted or decoded yields the empty
/// buffer, which the viewport treats as "nothing to draw here".
pub fn model_mesh_bytes(index: &LevelIndex, model_id: u64, lod: u32) -> Arc<Vec<u8>> {
    let key = (model_id, lod);
    if let Some(hit) = cache_get(key) {
        return hit;
    }

    let packed = (|| -> Result<Vec<u8>> {
        let raw = index.extract(model_id)?;
        let model = parse_model_asset(&raw)?;
        Ok(decode_model_mesh(&model, lod as usize)?.pack())
    })()
    .unwrap_or_else(|e| {
        debug!("model {model_id:016X} lod {lod}: {e}");
        empty_mesh_bytes()
    });

    let arc = Arc::new(packed);
    cache_put(key, Arc::clone(&arc));
    arc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_openarea_tile() {
        let (key, label, group, kind, tile) =
            classify("levels/i29/openarea/sargasso/tile_n19/tile_n19_art.zone");
        assert_eq!(key, "i29/openarea/sargasso");
        assert_eq!(label, "i29 / sargasso");
        assert_eq!(group, "openarea");
        assert_eq!(kind, "art");
        assert_eq!(tile.as_deref(), Some("tile_n19"));
    }

    #[test]
    fn classify_overlay_mission() {
        let (key, _, group, kind, tile) = classify(
            "levels/i29/overlay/storymission/mis_intro_parade_kalebo/i29_malinon_intro_art_parade_kalebo.zone",
        );
        assert_eq!(key, "i29/overlay/storymission/mis_intro_parade_kalebo");
        assert_eq!(group, "overlay");
        assert_eq!(kind, "art");
        assert_eq!(tile, None);
    }

    #[test]
    fn classify_instance_geo() {
        let (key, _, group, kind, _) = classify(
            "levels/i29/instance/pocketdimensions/pocketdimension_10/pd_10_geo.zone",
        );
        assert_eq!(key, "i29/instance/pocketdimensions/pocketdimension_10");
        assert_eq!(group, "instance");
        assert_eq!(kind, "geo");
    }

    #[test]
    fn kinds() {
        assert_eq!(zone_kind("tile_n19_gp_crates"), "gp");
        assert_eq!(zone_kind("tile_n19_lgt"), "lgt");
        assert_eq!(zone_kind("tile_n19_vfx"), "vfx");
        assert_eq!(zone_kind("unitnavdata"), "nav");
        assert_eq!(zone_kind("tile_n19_ground"), "other");
    }

    #[test]
    fn empty_mesh_header_is_well_formed() {
        let b = empty_mesh_bytes();
        assert_eq!(b.len(), 24 + 24);
        assert_eq!(u32::from_le_bytes(b[0..4].try_into().unwrap()), MESH_MAGIC);
        assert_eq!(u32::from_le_bytes(b[8..12].try_into().unwrap()), 0);
    }
}
