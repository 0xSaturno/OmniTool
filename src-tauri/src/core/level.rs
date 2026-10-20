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
use crate::tools::model_converter::sections::built::TAG_BUILT;
use crate::tools::model_converter::sections::geo::{
    IndexesSection, Uv1Section, VertexesSection, DEFAULT_UV_SCALE, TAG_INDEXES, TAG_UV1,
    TAG_VERTEXES,
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
        TAG_ACTORS, TAG_DECALS, TAG_IMPOSTORS, TAG_LIGHTS, TAG_MODEL_INSTS, TAG_SCENE_OBJECTS, TAG_VFX_INSTS,
        TAG_VOLUMES,
    };
    const HDR: usize = crate::core::zone::ASSET_HEADER_LEN;
    /// The tables `build_index` reads scene-object offsets out of.
    ///
    /// `TAG_IMPOSTORS` belongs here: the 645 shipped `*impostors*` zones carry a
    /// heap and that table alone, so leaving it out reported `hasObjects=false`
    /// for every one of them and hid 2.67 M triangles of megalopolis skyline.
    const INSTANCE_TABLES: [u32; 7] = [
        TAG_MODEL_INSTS,
        TAG_LIGHTS,
        TAG_VOLUMES,
        TAG_DECALS,
        TAG_VFX_INSTS,
        TAG_IMPOSTORS,
        TAG_ACTORS,
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
        if tag == TAG_SCENE_OBJECTS {
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

    let mut names = match &hashes {
        Some(p) => load_names(p).unwrap_or_else(|e| {
            warn!("level index: could not read {}: {e}", p.display());
            HashMap::new()
        }),
        None => HashMap::new(),
    };

    // The game ships its own name list: the `dag` beside `toc` carries a name
    // for every asset it knows, and its id block decodes exactly (169,245 of
    // 169,245 ids reproduce the CRC-64 of their own name on the shipped build).
    // So an external hash list is a convenience, not a requirement — without one
    // the tool would otherwise show nothing but hex. Entries already known from
    // the hash file win, since that list also covers a few assets the dag omits.
    let mut named_from_dag = 0usize;
    match crate::core::dag::Dag::load(Path::new(game_dir)) {
        Ok(dag) => {
            for i in 0..dag.len() {
                let (Some(&id), Some(name)) = (dag.ids.get(i), dag.name(i)) else {
                    continue;
                };
                if name.is_empty() {
                    continue;
                }
                names
                    .entry(id)
                    .or_insert_with(|| crate::core::crc64::normalize_path(name));
                named_from_dag += 1;
            }
            info!("level index: dag contributed {named_from_dag} names");
        }
        Err(e) => warn!("level index: could not read the dag: {e}"),
    }

    let names_missing = names.is_empty();

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
///
/// The hash-verified table in [`crate::core::section_names`] answers first: every
/// one of its entries reproduces its own tag under the engine CRC-32, so where it
/// and the hand-written list below disagree, it is the table that is right. It
/// also covers far more ground — over a 400-zone / 400-model sample it names 70 of
/// the 85 distinct tags seen, against 32 for the list below.
///
/// The fallback keeps the handful of descriptive labels we measured ourselves for
/// tags the table has no name for, so nothing that used to read as text regresses
/// to hex.
pub fn zone_section_name(tag: u32) -> Option<&'static str> {
    if let Some(name) = crate::core::section_names::section_name(tag) {
        return Some(name);
    }
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
        0x8E40_1376 => "Zone Impostors BVH Info",
        // `{ u64 instance_id, u32 vertex_count, u32 byte_offset }` rows that
        // partition 9CCAA06F into one 4-byte-per-vertex run per model
        // instance: baked per-vertex data, not geometry. Verified on
        // tile_e21_art (rows 4009/12411/12411 == the two models' own vertex
        // counts, ids == the kind-0 entries' instance ids, offsets chaining
        // exactly to the section length).
        0x5736_A46F => "Zone Instance Vertex Data Index",
        0x9CCA_A06F => "Zone Instance Vertex Data",
        // n x 64 bytes of 4x4 matrices whose bases are orthonormal on 1952/1952
        // rows sampled and whose translations stay under 13 m while the zones
        // span 180..5800 m — local frames, never world placements.
        0xDE61_C274 => "Zone Local Frames",
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
// Model Built header (0x283D0383, 120 bytes)
// ---------------------------------------------------------------------------

/// Names for the 32 bits of the Model Built flags word at `+0x28`.
///
/// Index = bit number. Bit 29 has no name; it is left empty here so
/// [`model_flag_names`] reports it as `bit29`, which matters because the
/// shipped data does set it — on 2 of the 11,387 models `coverage_audit
/// --models` scans. Every other bit in the table is accounted for.
///
/// Bits 17 (`VertHasUv1`) and 18 (`VertHasColor`) were previously derived here
/// by correlating the word against which vertex-stream sections a model
/// carries, and bit 31 by correlating against `Model Anim Dynamics Def`; the
/// rest of the table is adopted wholesale and re-measured for frequency rather
/// than for meaning.
pub const MODEL_FLAG_NAMES: [&str; 32] = [
    /* 0 */ "AmbientAnimation",
    /* 1 */ "SkipShadowCast",
    /* 2 */ "SkipFarLodCache",
    /* 3 */ "ShadowHasFadeDist",
    /* 4 */ "PhysicsLookLocked",
    /* 5 */ "SkipDecals",
    /* 6 */ "SkipImpostor",
    /* 7 */ "SkipLightCapture",
    /* 8 */ "OnlyLightCapture",
    /* 9 */ "NoEmbeddedSamples",
    /* 10 */ "NeverOcclude",
    /* 11 */ "SkipRainSplashes",
    /* 12 */ "DelayStreaming",
    /* 13 */ "StaticCollOnly",
    /* 14 */ "CollisionOnly",
    /* 15 */ "SkipNavGeneration",
    /* 16 */ "WalkableArea",
    /* 17 */ "VertHasUv1",
    /* 18 */ "VertHasColor",
    /* 19 */ "SkipSkinThreshold",
    /* 20 */ "AnimVert",
    /* 21 */ "HasHairCardsLook",
    /* 22 */ "HibernateNever",
    /* 23 */ "CenteredVerts",
    /* 24 */ "SkipStaticDecals",
    /* 25 */ "HasCustomJointBspheres",
    /* 26 */ "SplineModel",
    /* 27 */ "Hibernate",
    /* 28 */ "HibernateLongDist",
    /* 29 */ "",
    /* 30 */ "HasCinematicShadow",
    /* 31 */ "AnimDynamics",
];

/// Bit numbers of the Model Built flags this module reasons about.
pub mod model_flag {
    /// Model is never drawn by the impostor system.
    pub const SKIP_IMPOSTOR: u32 = 6;
    /// Model's geometry is streamed in late.
    pub const DELAY_STREAMING: u32 = 12;
    /// Geometry exists only to be collided against by static queries.
    pub const STATIC_COLL_ONLY: u32 = 13;
    /// Geometry exists only to be collided against — not art.
    pub const COLLISION_ONLY: u32 = 14;
    /// `Model UV1 Vert` stream is present.
    pub const VERT_HAS_UV1: u32 = 17;
    /// `Model Col Vert` stream is present.
    pub const VERT_HAS_COLOR: u32 = 18;
    /// Vertices were pre-offset by [`ModelBuilt::mesh_centre`] at build time.
    pub const CENTERED_VERTS: u32 = 23;
}

/// The set bits of a Model Built flags word, as names.
///
/// Unnamed bits come back as `bit<n>` so a future build that starts using one
/// is visible instead of silently dropped.
pub fn model_flag_names(flags: u32) -> Vec<String> {
    (0..32)
        .filter(|b| flags >> b & 1 == 1)
        .map(|b| {
            let n = MODEL_FLAG_NAMES[b as usize];
            if n.is_empty() {
                format!("bit{b}")
            } else {
                n.to_string()
            }
        })
        .collect()
}

/// The 120-byte `Model Built` header, fully named.
///
/// Every offset below is a fact about the bytes the shipped PC game ships;
/// `coverage_audit --model-flags` re-measures the ones that can be measured
/// (flag-bit frequency, the `0x62` strand count, the `0x60` LOD index, and
/// whether `mesh_centre` is ever non-zero) against the real archives.
///
/// This is a *named* view kept next to the decoder that consumes it.
/// `tools::model_converter::sections::built::Built` is the older, partly
/// guessed view behind the shipped Model Inspector and glTF exporter; it is
/// left alone here so those paths keep their current behaviour.
#[derive(Debug, Clone, Default)]
pub struct ModelBuilt {
    /// `0x00` bounding sphere: centre xyz then radius.
    pub bsphere: [f32; 4],
    /// `0x10` AABB half-extents about the bounding-sphere centre.
    pub aabb_extents: [f32; 3],
    /// `0x1C` mesh centre. When [`model_flag::CENTERED_VERTS`] is set the
    /// builder subtracted this from every vertex before quantising, so a
    /// decoder has to add it back to land in the model's own space.
    pub mesh_centre: [f32; 3],
    /// `0x28` feature flags — see [`MODEL_FLAG_NAMES`].
    pub flags: u32,
    /// `0x2C` metres per quantised unit. The decoder's position scale.
    pub metres_per_unit: f32,
    /// `0x30` UV log scales: low nibble UV0, next nibble UV1.
    pub uv_log_scales: u32,
    /// `0x34` distance at which each of the five geometry LODs takes over.
    pub geom_lod_dist: [f32; 5],
    /// `0x48` wind-sway amplitude.
    pub ambient_animation: f32,
    /// `0x4C` wind-sway clamp.
    pub max_displacement: f32,
    /// `0x50` / `0x54` wind-sway force range.
    pub max_dynamic_force: f32,
    pub min_dynamic_force: f32,
    /// `0x58` depth bias, stored in 1/16384 units.
    pub z_bias: i16,
    /// `0x5A` transparent-sorting bias.
    pub alpha_sort_bias: i16,
    /// `0x5C` / `0x5E` fade distances in metres.
    pub fade_out_dist: u16,
    pub shadow_fade_dist: u16,
    /// `0x60` which geometry LOD casts the shadow; -1 = all of them.
    pub shadow_casting_lod: i16,
    /// `0x62` number of hair-strand subsets.
    ///
    /// This byte used to be read as the high half of a 32-bit unknown and
    /// treated as "morph targets present"; the values 6 and 8 that reading
    /// keyed on are strand counts on hair models.
    pub strand_subset_count: u8,
    /// `0x64` / `0x68` totals the engine validates the geometry sections against.
    pub index_count: u32,
    pub vertex_count: u32,
    /// `0x6C` physics A/V surface hash, `0x70` audio surface hash.
    pub av_material_hash: u32,
    pub audio_material_hash: u32,
    /// `0x74` which animated-geometry streams exist:
    /// 1 AnimMorph, 2 AnimGeom, 4 AnimZiva, 8 AnimVert.
    pub content_flags: u32,
}

impl ModelBuilt {
    pub const SIZE: usize = 0x78;

    /// The shortest prefix that still carries the fields a decoder needs:
    /// bounds, flags and the position scale at `0x2C`.
    pub const MIN_SIZE: usize = 0x30;

    /// Every shipped model has the full 120 bytes, but a short section is read
    /// for what it does carry rather than rejected outright: returning `None`
    /// would send [`decode_model_mesh`] to a guessed position scale, which is
    /// worse than reading the real one out of a truncated header.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < Self::MIN_SIZE {
            return None;
        }
        let has = |o: usize, n: usize| o + n <= data.len();
        let f = |o: usize| {
            if has(o, 4) {
                f32::from_le_bytes(data[o..o + 4].try_into().unwrap())
            } else {
                0.0
            }
        };
        let u = |o: usize| {
            if has(o, 4) {
                u32::from_le_bytes(data[o..o + 4].try_into().unwrap())
            } else {
                0
            }
        };
        let i16at = |o: usize| {
            if has(o, 2) {
                i16::from_le_bytes(data[o..o + 2].try_into().unwrap())
            } else {
                0
            }
        };
        let u16at = |o: usize| {
            if has(o, 2) {
                u16::from_le_bytes(data[o..o + 2].try_into().unwrap())
            } else {
                0
            }
        };
        let byte = |o: usize| if o < data.len() { data[o] } else { 0 };
        Some(Self {
            bsphere: std::array::from_fn(|i| f(i * 4)),
            aabb_extents: std::array::from_fn(|i| f(0x10 + i * 4)),
            mesh_centre: std::array::from_fn(|i| f(0x1C + i * 4)),
            flags: u(0x28),
            metres_per_unit: f(0x2C),
            uv_log_scales: u(0x30),
            geom_lod_dist: std::array::from_fn(|i| f(0x34 + i * 4)),
            ambient_animation: f(0x48),
            max_displacement: f(0x4C),
            max_dynamic_force: f(0x50),
            min_dynamic_force: f(0x54),
            z_bias: i16at(0x58),
            alpha_sort_bias: i16at(0x5A),
            fade_out_dist: u16at(0x5C),
            shadow_fade_dist: u16at(0x5E),
            shadow_casting_lod: i16at(0x60),
            strand_subset_count: byte(0x62),
            index_count: u(0x64),
            vertex_count: u(0x68),
            av_material_hash: u(0x6C),
            audio_material_hash: u(0x70),
            content_flags: u(0x74),
        })
    }

    pub fn of(model: &ModelFile) -> Option<Self> {
        Self::parse(model.dat1.get_section_data(TAG_BUILT)?)
    }

    pub fn has_flag(&self, bit: u32) -> bool {
        self.flags >> bit & 1 == 1
    }

    /// Vertices were stored relative to [`Self::mesh_centre`].
    pub fn centered_verts(&self) -> bool {
        self.has_flag(model_flag::CENTERED_VERTS)
    }

    /// The offset a decoder must add to every decoded vertex position.
    /// Zero unless `CenteredVerts` is set.
    pub fn vertex_offset(&self) -> [f32; 3] {
        if self.centered_verts() {
            self.mesh_centre
        } else {
            [0.0; 3]
        }
    }

    /// This model exists only to be collided with — it is not art.
    ///
    /// Bit 13 (`StaticCollOnly`) is deliberately **not** part of this test.
    /// Measured over the whole shipped model library (11,387 assets) bit 13 is
    /// set on 6,480 of them, and in the art zones sampled by `coverage_audit`
    /// it accounts for 74-94% of every zone's triangles — it is a property of
    /// how the model's collision is queried, not a statement that the model is
    /// invisible. Bit 14 is set on 357 models, of which the nine sampled zones
    /// place 3 distinct ones (15 instances, 12 triangles each): a design cube,
    /// an invisible collision cube and a hero-collision blocker.
    pub fn collision_only(&self) -> bool {
        self.has_flag(model_flag::COLLISION_ONLY)
    }

    /// Bit 13. Present on most shipped environment art, so it is exposed for
    /// the inspector and never used as a visibility test.
    pub fn static_coll_only(&self) -> bool {
        self.has_flag(model_flag::STATIC_COLL_ONLY)
    }

    /// Should the viewport draw this model at all? See [`model_mesh_bytes`].
    pub fn drawable(&self) -> bool {
        !self.collision_only()
    }

    /// `z_bias` in the units it is applied in.
    pub fn z_bias_units(&self) -> f32 {
        self.z_bias as f32 / 16384.0
    }

    /// UV0 / UV1 texel scale, from the two nibbles at `0x30`.
    pub fn uv_scales(&self) -> (f32, f32) {
        let s = |n: u32| (1u32 << n) as f32 / 16384.0;
        (
            s(self.uv_log_scales & 0xF),
            s((self.uv_log_scales >> 4) & 0xF),
        )
    }

    /// `(min, max)` of the AABB, in the same space decoded vertices end up in.
    pub fn aabb(&self) -> ([f32; 3], [f32; 3]) {
        let c = [self.bsphere[0], self.bsphere[1], self.bsphere[2]];
        let e = self.aabb_extents.map(f32::abs);
        (
            std::array::from_fn(|k| c[k] - e[k]),
            std::array::from_fn(|k| c[k] + e[k]),
        )
    }
}

// ---------------------------------------------------------------------------
// Model mesh decoding
// ---------------------------------------------------------------------------

/// `"RCMS"` — magic of the packed mesh buffer handed to the viewport.
pub const MESH_MAGIC: u32 = 0x534D_4352;

/// Buffer version. **Deliberately still 1** after UV1 was added — see
/// [`MeshOut::pack`] for why, and for what a reader has to do instead.
pub const MESH_VERSION: u32 = 1;

/// Bits of the RCMS header word at `+0x14`, which was `reserved = 0` before
/// UV1 existed.
pub mod mesh_flag {
    /// A `f32 uv1[2 * vertexCount]` block follows the submesh table.
    pub const HAS_UV1: u32 = 1 << 0;
}

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
    /// The second UV set, `2 * vertex_count` long, or **empty** when the model
    /// carries none. See [`decode_model_mesh`].
    pub uv1: Vec<f32>,
    pub indices: Vec<u32>,
    pub submeshes: Vec<SubmeshOut>,
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl MeshOut {
    pub fn vertex_count(&self) -> usize {
        self.positions.len() / 3
    }

    /// True when [`Self::pack`] will append a UV1 block.
    pub fn has_uv1(&self) -> bool {
        !self.uv1.is_empty() && self.uv1.len() == self.vertex_count() * 2
    }

    /// The little-endian buffer documented in the IPC contract:
    /// header, bounds, positions, normals, uvs, indices, submeshes, **uv1**.
    ///
    /// # Why the version is still 1
    ///
    /// UV1 is appended **after** the submesh table, and announced by bit 0 of
    /// the header word at `+0x14` that used to be a hard-coded `reserved = 0`.
    /// Nothing before the new block moves, and the block is only present when
    /// the bit is set, so every offset a version-1 reader computes is still
    /// correct and it simply ignores trailing bytes — which the shipped
    /// `decodeRcms` already does (it tests `byteLength < needed`, not `!=`).
    ///
    /// Bumping the version instead would have been the honest-looking choice
    /// and the wrong one: `viewport/rcms.ts` rejects any buffer whose version
    /// is not exactly 1, so the moment this backend started emitting version 2
    /// every model in the viewport would decode to `null` until the frontend
    /// landed a matching change. A flag bit in a field that was always zero
    /// costs nothing and lets the two sides land in either order.
    pub fn pack(&self) -> Vec<u8> {
        let vc = self.vertex_count();
        let uv1 = self.has_uv1();
        let mut out = Vec::with_capacity(
            24 + 24
                + vc * 32
                + if uv1 { vc * 8 } else { 0 }
                + self.indices.len() * 4
                + self.submeshes.len() * 16,
        );
        out.extend_from_slice(&MESH_MAGIC.to_le_bytes());
        out.extend_from_slice(&MESH_VERSION.to_le_bytes());
        out.extend_from_slice(&(vc as u32).to_le_bytes());
        out.extend_from_slice(&(self.indices.len() as u32).to_le_bytes());
        out.extend_from_slice(&(self.submeshes.len() as u32).to_le_bytes());
        let flags = if uv1 { mesh_flag::HAS_UV1 } else { 0 };
        out.extend_from_slice(&flags.to_le_bytes());
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
        if uv1 {
            for v in &self.uv1 {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out
    }
}

/// An empty buffer — the "could not decode" answer, so the viewport skips the
/// model instead of surfacing an error dialog per instance.
pub fn empty_mesh_bytes() -> Vec<u8> {
    MeshOut::default().pack()
}

/// The `Built` section's AABB — the bounding-sphere centre at `+0x00` plus and
/// minus the half-extents at `+0x10` — for sanity checks.
pub fn built_box(model: &ModelFile) -> Option<([f32; 3], [f32; 3])> {
    Some(ModelBuilt::of(model)?.aabb())
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
/// the `Built` section's metres-per-unit — and subsets flagged `0x10` index the
/// vertex stream relative to their own `vertex_start`, so both are undone here
/// exactly the way `gltf_writer` does it.
///
/// On top of that, a model whose flags carry `CenteredVerts` (bit 23) had its
/// mesh centre (`Built +0x1C`) subtracted from every vertex at build time, so
/// the centre is added back here. On the shipped PC build no model actually
/// does this: `coverage_audit --models` parsed the `Model Built` header of all
/// 11,387 `.model` assets in the archives and found bit 23 set on 0 of them
/// and `+0x1C` exactly `0,0,0` on all of them. The term is therefore
/// identically zero today; the code is written to be correct anyway, so a
/// patch or a mod that does set the flag is decoded rather than mis-placed.
/// (Independently: 1,420 decoded models all landed inside their own `Built`
/// bounding sphere, worst radius ratio 1.0014, with and without the term.)
///
/// Triangle winding is emitted in the asset's own index order. It used to be
/// reversed here "to match the CCW glTF convention", but the assets are already
/// CCW-front: measured over 596 distinct models drawn from four planets, the
/// reversed order agreed with the models' own stored vertex normals on an
/// area-weighted 0.23% of triangles (591/596 models below 10%, none above 40%)
/// and every one of the 75 watertight models enclosed negative signed volume.
/// In source order those figures become 99.8% and 75/75 positive. Since 58-100%
/// of an art zone's triangles sit on open "sheet" meshes, the reversal made the
/// front faces point away from the camera and hid most of the level. See the
/// `winding vs. stored normals` section of `bin/level_check.rs`.
///
/// `tools::model_converter::gltf_writer` still reverses, deliberately: it is a
/// shipped export path with its own users and is out of scope here.
///
/// # The second UV set
///
/// UV0 is interleaved in `Model Std Vert`; UV1 is a section of its own,
/// `Model UV1 Vert` (`0x6B855EED`), holding `i16` pairs indexed by **absolute**
/// vertex index and quantised at `(1 << n) / 16384` where `n` is the **high**
/// nibble of `Built +0x30`.
///
/// That the high nibble is UV1's and not a spare is measured, not assumed:
/// over the 887 distinct models Nefarious City places, the
/// two nibbles differ on 291 of the 738 that carry UV1, so the sets genuinely
/// do not share a scale, and the decoded values stay finite with 21,117 of
/// 10,261,052 texels outside +/-64 (0.21%, the largest `2270.8` on
/// `nnc_arch_bldg_d_01`, whose UV1 nibble is 11).
///
/// It is emitted into [`MeshOut::uv1`] whenever the section exists, because the
/// shipped `gbl_rgb_blend` pixel shader samples its four colour layers on
/// `TEXCOORD1` while the splat mask and the shared normal sample `TEXCOORD0`;
/// with one UV set the layers can only be right relative to each other, never
/// relative to the mask.
pub fn decode_model_mesh(model: &ModelFile, lod: usize) -> Result<MeshOut> {
    let dat1 = &model.dat1;

    let built = ModelBuilt::of(model);
    let pos_scale = built
        .as_ref()
        .map(|b| b.metres_per_unit)
        .unwrap_or(1.0 / 4096.0);
    let uv_scale = built
        .as_ref()
        .map(|b| b.uv_scales().0)
        .unwrap_or(DEFAULT_UV_SCALE);
    let uv1_scale = built
        .as_ref()
        .map(|b| b.uv_scales().1)
        .unwrap_or(DEFAULT_UV_SCALE);
    // Zero unless the model sets `CenteredVerts`; see [`ModelBuilt::mesh_centre`].
    let offset = built.as_ref().map(ModelBuilt::vertex_offset).unwrap_or_default();

    // The second UV set. It lives in its own section, as `i16` pairs indexed by
    // ABSOLUTE vertex index — not per subset like the positions — and at its
    // own quantum, the high nibble of `Built +0x30`. The section is read rather
    // than the flag tested: measured over the 887 distinct models of Nefarious
    // City, flag bit 17 (`VertHasUv1`) and the presence of `Model UV1 Vert`
    // agree on all 886 that parse (738 with both, 148 with neither, 0 with one
    // and not the other), so the flag is a faithful summary — but the section
    // is what actually has to be indexed, and reading it directly cannot
    // disagree with itself. The rows cover every vertex: 10,449,136 rows for
    // the 10,448,188 vertices those models declare, 0 models short.
    let uv1 = dat1
        .get_section_data(TAG_UV1)
        .and_then(|d| Uv1Section::parse(d).ok());

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
        for (i, v) in vertexes[vstart..vstart + vcount].iter().enumerate() {
            let p = [v.x + offset[0], v.y + offset[1], v.z + offset[2]];
            out.positions.extend_from_slice(&p);
            out.normals.extend_from_slice(&[v.nx, v.ny, v.nz]);
            out.uvs.extend_from_slice(&[v.u, v.v]);
            if let Some(u1) = uv1.as_ref() {
                // Absolute index, and 0,0 past the end rather than a panic: a
                // short section must cost the model its second UV set, not its
                // geometry.
                let (u, v) = u1
                    .uvs
                    .get(vstart + i)
                    .map(|&(u, v)| (u as f32 * uv1_scale, v as f32 * uv1_scale))
                    .unwrap_or((0.0, 0.0));
                out.uv1.extend_from_slice(&[u, v]);
            }
            for (k, c) in p.iter().enumerate() {
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
                indexes[ibase + f * 3].wrapping_sub(sub) as u32,
                indexes[ibase + f * 3 + 1].wrapping_sub(sub) as u32,
                indexes[ibase + f * 3 + 2].wrapping_sub(sub) as u32,
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
// Geometry that lives in the zone itself (block 2)
// ---------------------------------------------------------------------------

/// Decode the mesh a scene-object entry owns inside the zone's raw block 2.
///
/// Two kinds carry one:
///
/// * **kind 6, impostor** — `vertex_count` packed `u32` positions
///   (`R10G10B10A2`, tag bits constant 1 on 770/770 shipped impostors) followed
///   by `index_count` `u16` indices. Each channel divided by 1023 gives a point
///   in the entry's own unit box, so the entry's 4x4 places it in the world
///   exactly the way a model instance is placed. Verified against the matching
///   art: the megalopolis `n19` impostor decodes to a 31.6 x 311 x 31.6 m box
///   whose base sits at the entry's own y (-220.6 vs -220.87).
/// * **kind 3, decal** — `vertex_count` 16-byte `Model Std Vert` vertices at
///   position scale `1/32768`, then `index_count` `u16` indices. Verified on
///   all eight decals of `tile_n19_art`: every decoded mesh, placed by the
///   entry matrix, lands inside the `Zone Decal Geometry` row's own bounding
///   sphere (max radius 0.91 r) and within 0.1 m of the entry position.
///
/// Normals are computed from the triangles for impostors (the format stores
/// none) and taken from the vertex stream for decals. Winding is emitted in the
/// block's own index order, matching [`decode_model_mesh`]. Reversing it here
/// was self-consistent for impostors only because their normals are then
/// accumulated from the reversed triangles; the decals, which do carry stored
/// normals, gave it away — reversed, 509 of 522 decoded decals scored below 0.9
/// winding/normal agreement (mean 0.025); in source order only 13 do (mean
/// 0.975), and those 13 are exactly the ones the reversed order agreed with,
/// i.e. entries that really are wound against their own normals.
pub fn decode_zone_geometry(zone: &crate::core::zone::Zone, offset: u32) -> Result<MeshOut> {
    use crate::core::zone::{KIND_DECAL, KIND_IMPOSTOR};

    let obj = zone
        .find(offset)
        .ok_or_else(|| ToolkitError::Parse(format!("no scene object at {offset}")))?;
    let geo = obj
        .geometry
        .ok_or_else(|| ToolkitError::Parse(format!("object {offset} has no block-2 geometry")))?;
    let block = zone
        .block(2)
        .ok_or_else(|| ToolkitError::Parse("zone has no block 2".into()))?;

    let vstart = geo.vertex_offset as usize;
    let icount = geo.index_count as usize;
    let vcount = geo.vertex_count as usize;
    let istart = geo.index_offset as usize;
    if istart + icount * 2 > block.len() {
        return Err(ToolkitError::Parse(format!(
            "index span {istart}+{} exceeds block 2 ({})",
            icount * 2,
            block.len()
        )));
    }

    let mut out = MeshOut {
        min: [f32::INFINITY; 3],
        max: [f32::NEG_INFINITY; 3],
        ..Default::default()
    };

    match obj.kind {
        KIND_IMPOSTOR => {
            if vstart + vcount * 4 > block.len() {
                return Err(ToolkitError::Parse("vertex span exceeds block 2".into()));
            }
            for i in 0..vcount {
                let w = u32::from_le_bytes(
                    block[vstart + i * 4..vstart + i * 4 + 4].try_into().unwrap(),
                );
                let p = [
                    (w & 0x3FF) as f32 / 1023.0,
                    ((w >> 10) & 0x3FF) as f32 / 1023.0,
                    ((w >> 20) & 0x3FF) as f32 / 1023.0,
                ];
                out.positions.extend_from_slice(&p);
                out.normals.extend_from_slice(&[0.0, 0.0, 0.0]);
                out.uvs.extend_from_slice(&[0.0, 0.0]);
                for k in 0..3 {
                    out.min[k] = out.min[k].min(p[k]);
                    out.max[k] = out.max[k].max(p[k]);
                }
            }
        }
        KIND_DECAL => {
            if vstart + vcount * 16 > block.len() {
                return Err(ToolkitError::Parse("vertex span exceeds block 2".into()));
            }
            let verts = VertexesSection::parse_scaled(
                &block[vstart..vstart + vcount * 16],
                1.0 / 32768.0,
                DEFAULT_UV_SCALE,
            )?
            .vertexes;
            for v in &verts {
                out.positions.extend_from_slice(&[v.x, v.y, v.z]);
                out.normals.extend_from_slice(&[v.nx, v.ny, v.nz]);
                out.uvs.extend_from_slice(&[v.u, v.v]);
                for (k, c) in [v.x, v.y, v.z].iter().enumerate() {
                    out.min[k] = out.min[k].min(*c);
                    out.max[k] = out.max[k].max(*c);
                }
            }
        }
        k => {
            return Err(ToolkitError::Parse(format!(
                "scene-object kind {k} carries no block-2 geometry"
            )))
        }
    }

    for t in 0..icount / 3 {
        let mut tri = [0u32; 3];
        for (j, slot) in tri.iter_mut().enumerate() {
            let at = istart + (t * 3 + j) * 2;
            *slot = u16::from_le_bytes(block[at..at + 2].try_into().unwrap()) as u32;
        }
        if tri.iter().any(|&i| i as usize >= vcount) {
            continue;
        }
        out.indices.extend_from_slice(&tri);
    }

    if obj.kind == KIND_IMPOSTOR {
        accumulate_normals(&mut out);
    }
    out.submeshes.push(SubmeshOut {
        first_index: 0,
        index_count: out.indices.len() as u32,
        material_index: 0,
        flags: 0,
    });
    if out.positions.is_empty() {
        out.min = [0.0; 3];
        out.max = [0.0; 3];
    }
    Ok(out)
}

/// Area-weighted vertex normals, for geometry that ships without any.
fn accumulate_normals(out: &mut MeshOut) {
    for n in out.normals.iter_mut() {
        *n = 0.0;
    }
    for t in out.indices.chunks_exact(3) {
        let p: Vec<[f32; 3]> = t
            .iter()
            .map(|&i| {
                let b = i as usize * 3;
                [out.positions[b], out.positions[b + 1], out.positions[b + 2]]
            })
            .collect();
        let u = [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]];
        let v = [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]];
        let n = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        for &i in t {
            let b = i as usize * 3;
            for k in 0..3 {
                out.normals[b + k] += n[k];
            }
        }
    }
    for n in out.normals.chunks_exact_mut(3) {
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len > 1e-12 {
            for c in n.iter_mut() {
                *c /= len;
            }
        } else {
            n[1] = 1.0;
        }
    }
}

// ---------------------------------------------------------------------------
// Mesh LRU cache
// ---------------------------------------------------------------------------

/// Roughly the geometry of a whole planet's distinct models; well under what a
/// desktop can hold, and evicting from the front keeps a pan across tiles warm.
///
/// It is left at 192 MB now that [`MeshOut::pack`] emits UV1. Measured over the
/// 887 distinct models Nefarious City places, LOD 0 of every one of them packs
/// to 307.93 MB with UV1 against 266.88 MB without — +15.4%, or +41 MB for a
/// whole planet — and 192 MB never held all of that to begin with. The budget
/// is a working set, not a capacity target, and 15% fewer models resident is a
/// cheaper trade than a fifth of the tiles rendering with the wrong UV set.
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
///
/// **Models flagged `CollisionOnly` (bit 14) are deliberately skipped here.**
/// They were being drawn as if they were art: `coverage_audit --models` finds
/// 357 of them in the shipped library, and the nine audited zones place 3
/// distinct ones — `design_cube_invisible_coll`, `design_so_cube_1x1_hero_coll`
/// and `gbl_gp_hero_coll_no_ground_01` — as 15 instances of a 12-triangle box
/// each. Tiny in triangles (180 of 144.0 M across those zones, 0.0001%) but
/// unmissable on screen, because an invisible blocker volume rendered as a
/// solid cube hides whatever is behind it. The skip is at this level, not in
/// [`decode_model_mesh`], so the raw decoder stays faithful to the asset and
/// the audit harnesses can still measure what was skipped.
///
/// Bit 13 `StaticCollOnly` is *not* skipped — see [`ModelBuilt::collision_only`]
/// for why: it covers 74-94% of the triangles of every art zone measured.
pub fn model_mesh_bytes(index: &LevelIndex, model_id: u64, lod: u32) -> Arc<Vec<u8>> {
    let key = (model_id, lod);
    if let Some(hit) = cache_get(key) {
        return hit;
    }

    let packed = (|| -> Result<Vec<u8>> {
        let raw = index.extract(model_id)?;
        let model = parse_model_asset(&raw)?;
        if ModelBuilt::of(&model).is_some_and(|b| !b.drawable()) {
            debug!("model {model_id:016X}: CollisionOnly, not drawn");
            return Ok(empty_mesh_bytes());
        }
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
        // The word that used to be `reserved` is the flags word now, and an
        // empty mesh has no UV1.
        assert_eq!(u32::from_le_bytes(b[20..24].try_into().unwrap()), 0);
    }

    /// A two-vertex mesh, one triangle, one submesh — enough to check offsets.
    fn tiny(uv1: bool) -> MeshOut {
        MeshOut {
            positions: vec![0.0; 3 * 3],
            normals: vec![0.0; 3 * 3],
            uvs: vec![0.25; 3 * 2],
            uv1: if uv1 { vec![0.75; 3 * 2] } else { Vec::new() },
            indices: vec![0, 1, 2],
            submeshes: vec![SubmeshOut {
                first_index: 0,
                index_count: 3,
                material_index: 7,
                flags: 0,
            }],
            min: [0.0; 3],
            max: [1.0; 3],
        }
    }

    #[test]
    fn uv1_is_announced_by_the_header_flag() {
        let without = tiny(false).pack();
        let with = tiny(true).pack();
        assert_eq!(
            u32::from_le_bytes(without[20..24].try_into().unwrap()),
            0,
            "a mesh with no UV1 must leave the flags word zero"
        );
        assert_eq!(
            u32::from_le_bytes(with[20..24].try_into().unwrap()),
            mesh_flag::HAS_UV1
        );
    }

    #[test]
    fn uv1_is_appended_so_every_older_offset_survives() {
        let without = tiny(false).pack();
        let with = tiny(true).pack();
        // Byte-identical up to the end of the version-1 buffer except for the
        // flags word at +0x14, which a version-1 reader never reads because it
        // was documented as `reserved`. So an old reader sees exactly what it
        // saw before, and every offset it computes is unchanged.
        assert_eq!(&with[..20], &without[..20]);
        assert_eq!(&with[24..without.len()], &without[24..]);
        assert_eq!(with.len(), without.len() + 3 * 2 * 4);
        // And the block really is the UVs we put in.
        let tail: Vec<f32> = with[without.len()..]
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        assert_eq!(tail, vec![0.75; 6]);
    }

    #[test]
    fn a_mis_sized_uv1_block_is_dropped_rather_than_emitted() {
        let mut m = tiny(true);
        m.uv1.pop(); // one float short of 2 per vertex
        assert!(!m.has_uv1());
        let b = m.pack();
        assert_eq!(u32::from_le_bytes(b[20..24].try_into().unwrap()), 0);
        assert_eq!(b.len(), tiny(false).pack().len());
    }
}
