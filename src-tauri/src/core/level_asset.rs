//! `.level` reader for Ratchet & Clank: Rift Apart.
//!
//! A level is the table of contents of a planet / area. Where a `.zone` holds
//! the actual placed content, the level says *which* zones exist, how they are
//! grouped into a tree of regions, at what distance each open-world region
//! streams in, where the game may drop the player in (named links), and it
//! carries the far-LOD "hibernate" instance data that keeps distant scenery on
//! screen while the owning zone is unloaded.
//!
//! Asset layout, as extracted by `Toc::extract_asset`:
//!
//! ```text
//!   [36-byte "Archive TOC Asset Header Data"]   { u32 type_id, u32 sizes[8] }
//!   [block 0]  DAT1                              <- everything below lives here
//! ```
//!
//! The 36-byte header is split exactly like a zone's: only a prefix of the
//! eight size words are block sizes, so the block count is the smallest `k`
//! whose sizes sum to the asset size. Shipped `i29.level` is a single block.
//!
//! DAT1 sections consumed here. Each tag is the Insomniac CRC-32 of a
//! human-readable block name. Nine of the fourteen tags below reproduce from
//! the obvious name and are marked "named"; the other five do not reproduce
//! from any name variant tried so far, so their role was pinned instead by
//! matching each section's byte size against the counts in `Level Built` —
//! that identification is exact and is re-checked on every run of
//! `level_asset_check`.
//!
//! ```text
//!   0x7CA7267D  Level Built                 named    36-byte header, one record
//!   0x396F9418  Level Regions Built         named    36-byte rows
//!   0x4E023760  Level Zones Built           named    12-byte rows
//!   0x3395AEC1  links built                 by size  48-byte rows   = named_link_count
//!   0xC30D92B6  region LOD controls         by size  24-byte rows   = lod_control_count
//!   0x95F91E24  zone references             by size  i16[]          = zone_ref_count
//!   0x4130D903  Level Region Names          named    u32[]          = region_count
//!   0x2BA33702  Level Zone Names            named    u32[]          = zone_count
//!   0x2236C47A  Level Link Names            named    u32[]          = named_link_count
//!   0x41887FB3  region game struct table    by size  16-byte rows   = game_data_count
//!   0x339C970E  region game struct payload  by size  the DDL blobs those rows point at
//!   0x7077E5F5  Level Hibernate Model Names named    u32[] string offsets
//!   0x6251A0BF  Level Hibernate VFX Names   named    u32[] string offsets (absent in i29)
//!   0x07C75341  Level Hibernate Raw Data    named    opaque heap of containers
//!   0x611F490D  Level Hibernate Data Desc   named    8-byte rows into that heap
//! ```
//!
//! # Record layouts
//!
//! All of these are plain little-endian C structs with natural alignment.
//! Every size and offset below was re-measured on the shipped PC
//! `levels/i29/i29.level`; see `src/bin/level_asset_check.rs`.
//!
//! `LevelBuilt` — 36 bytes on disk, but the section is padded, so read the
//! first 36 bytes and ignore the rest:
//!
//! ```text
//!   +0x00  u64 default_named_link_id   entry point when nothing else is asked for
//!   +0x08  i32 global_region_index     region that is always resident
//!   +0x0C  i32 region_count
//!   +0x10  i32 zone_ref_count          length of Level Zone References
//!   +0x14  i32 lod_control_count
//!   +0x18  i32 zone_count
//!   +0x1C  i32 named_link_count
//!   +0x20  i32 game_data_count
//! ```
//!
//! `LevelRegionBuilt` — 36 bytes, `u64` + fourteen `i16`:
//!
//! ```text
//!   +0x00  u64 asset_id                the `.region` authoring asset
//!   +0x08  i16 type                    RegionType ordinal, see `region_type_name`
//!   +0x0A  i16 name_index              -> Level Region Names
//!   +0x0C  i16 lod_controls_index      -> Level Region Lod Built (open-unit regions)
//!   +0x0E  i16 parent_index            -1 for a root region
//!   +0x10  i16 child_start             -> Level Regions Built
//!   +0x12  i16 child_count
//!   +0x14  i16 zone_refs_start         -> Level Zone References
//!   +0x16  i16 zone_refs_count
//!   +0x18  i16 secondary_zone_refs_start
//!   +0x1A  i16 secondary_zone_refs_count
//!   +0x1C  i16 named_link_start        -> Level Links Built
//!   +0x1E  i16 named_link_count
//!   +0x20  i16 game_data_start         -> Level Region Game Struct Built
//!   +0x22  i16 game_data_count
//! ```
//!
//! Secondary zone refs mean different things per region type: for an open-unit
//! region they are the low-LOD stand-ins for the primary zones, for an overlay
//! region they are shadow zones (a zone that only loads when its shadowing zone
//! is loaded). Other region types do not use them.
//!
//! `LevelZoneBuilt` — 12 bytes. The struct is 16 in memory because of the
//! leading `u64`, but the serialiser writes the two trailing pad bytes and
//! nothing more, so the stride on disk is 12: the shipped section is
//! 110,112 bytes for the 9,176 zones the header declares, which is 12 exactly.
//!
//! ```text
//!   +0x00  u64 asset_id                the `.zone` asset, resolvable in the TOC
//!   +0x08  i16 name_index              -> Level Zone Names
//!   +0x0A  i16 pad
//! ```
//!
//! `LevelNamedLinkBuilt` — 48 bytes, 8-byte aligned:
//!
//! ```text
//!   +0x00  u64 id                      link uid
//!   +0x08  u64 actor_id                uid of the actor the link points at
//!   +0x10  u64 actor_asset_id          that actor's asset id
//!   +0x18  u32 name_hash               CRC-32 of the link name
//!   +0x1C  i16 name_index              -> Level Link Names
//!   +0x1E  i16 load_region_index       region to stream in for this link
//!   +0x20  i16 game_data_start
//!   +0x22  i16 game_data_count
//!   +0x24  f32 x, y, z                 world position in metres
//! ```
//!
//! `LevelRegionLodBuilt` — 24 bytes: `f32 x, y, z` (the reference point the
//! open-world container LODs this unit against), then `u32 tile_radius`,
//! `u32 load_distance`, `u32 unload_distance` in metres.
//!
//! `LevelZoneHibernateBuilt` — 8 bytes: `i32 zone_index` (an index into
//! `Level Zones Built`) and `u32 byte_offset` into `Level Hibernate Raw Data`.
//!
//! # Hibernation
//!
//! Each byte offset lands on a container describing one zone's far-LOD
//! instances. The container is two identical 32-byte sub-containers (models
//! first, visual effects second) plus a material-override slice and the owning
//! zone's asset id — 80 bytes:
//!
//! ```text
//!   +0x00  f32 centre[3]      \
//!   +0x0C  f32 radius          |
//!   +0x10  u32 cluster_offset  |  models
//!   +0x14  u16 cluster_count   |
//!   +0x16  u16 optimized       |  set once at runtime, see below
//!   +0x18  u32 object_offset   |
//!   +0x1C  u32 object_count   /
//!   +0x20  … the same 32 bytes again for visual effects
//!   +0x40  u32 mat_override_offset
//!   +0x44  u32 mat_override_count
//!   +0x48  u64 zone_asset_id
//! ```
//!
//! All three offsets are relative to the start of the container, not to the
//! block. Clusters are 32 bytes — `f32 centre[3]`, `f32 radius`,
//! `f32 draw_distance`, `u32 visible_count`, `u32 object_offset`,
//! `u32 object_count` — and the last two index the container's object array,
//! so a cluster is a contiguous run of instances with a shared centre.
//!
//! Instances are 40 bytes:
//!
//! ```text
//!   +0x00  u16 rot[3]          three half-float Euler angles (radians)
//!   +0x06  u16 offset[3]       half-float metres, relative to the cluster centre
//!   +0x0C  u16 scale[3]        half-float per-axis scale
//!   +0x12  u16 custom[3]
//!   +0x18  u16 mat_override_index
//!   +0x1A  u8  mat_override_count
//!   +0x1B  u8  flags           see below
//!   +0x1C  u32 rgba            r,g,b sqrt-encoded from the high byte down, alpha low
//!   +0x20  u32 parent_id       compacted uid of the model inst this came from
//!   +0x24  i16 asset_index     -> Level Hibernate Model / VFX Names
//!   +0x26  u16 misc
//! ```
//!
//! The flags byte is two things at once. Bits 5, 6 and 7 are the instance's
//! own: visible, z-bias, long range. Bits 0 to 4 are the engine's standard
//! matrix-property bits, which every transform carries: bits 0-1 hold the axis
//! index of the median scale, bit 2 says the matrix is mirrored (negative
//! determinant), bit 3 is unused and bit 4 says the scale is uniform. The
//! shipped level bears that out — across all 87,778 instances the low five bits
//! take the values `0b10111`, i.e. bit 3 is never set.
//!
//! The rotation / offset / scale triples have **two** encodings. Shipped data
//! is the half-float form above. The first time the engine loads a container it
//! rewrites the records in place into a faster form — offsets become `i16`
//! metres over 64, scales `i16` over 256, rotations a 12-bit-per-component
//! quaternion — and sets the `optimized` word so it never repeats. That word is
//! 0 in every shipped container measured here, so the reader decodes the
//! half-float form; it still honours the flag in case a future build ships
//! pre-optimised data.
//!
//! An instance's world position is `cluster.centre + offset`. Because the data
//! lives in the *level*, moving or deleting a hibernating model inside its zone
//! does not move the far-LOD copy: the editor would leave a ghost at distance
//! until the level is rebuilt too.
//!
//! # What the shipped file measures
//!
//! `levels/i29/i29.level` on the PC build, 5,040,380 bytes extracted, one
//! block, DAT1 version word `0x587B60A6`:
//!
//! * 1,216 regions, 226 of them roots, tree depth 1 — containers hold units and
//!   nothing nests deeper. By type: 1 global, 21 instance containers, 72
//!   instance units, 9 open containers, 918 open units, 195 overlays. No region
//!   of the debug type survives into the built level.
//! * 9,176 zones, 10,135 zone-ref entries, 918 LOD control sets, 860 named
//!   links, 312 game-data blocks — every count matches its section's byte size
//!   over the stride exactly.
//! * A region owns a median of 8 zones; the largest, the Megalopolis open
//!   container, owns 486. 129 zones are owned by more than one region, which is
//!   why a one-parent-per-zone grouping cannot reproduce this tree.
//! * Every region name is the crc64 of its own stored asset id, and so is every
//!   zone name. None of the 1,216 `.region` ids is in the TOC: `.region` is an
//!   authoring type the builder flattens away. 133 of the 9,176 zone ids are
//!   *also* absent from the PC TOC, so a level zone row is a reference, not a
//!   guarantee the asset ships.
//! * LOD controls come in three settings — load/unload 150/170 (566 regions),
//!   170/200 (111) and 1024/1200 (241) — and every reference point has y = 0,
//!   so open-world streaming is decided on a 2D grid. The points sit on a 128 m
//!   lattice.
//! * Secondary zone refs appear only on overlay regions (193 of them) and each
//!   list is exactly as long as that region's primary list, so entry *i* covers
//!   primary zone *i*; 732 of the 10,135 entries are -1, meaning "this zone has
//!   no shadow zone".
//! * All 860 link names reproduce their stored `name_hash` under the engine
//!   CRC-32, and all 860 link actor uids carry the engine's "placed in a zone"
//!   bit 63.
//! * 613 zones carry hibernate data: 87,778 model instances in 7,805 clusters,
//!   no visual-effect instances at all, over 353 distinct models. Every
//!   container is in the half-float form. 15.4% of scale axes decode to exactly
//!   1.0, which only happens if the half-float reading is right.
//!
//! The game-data table's rows are `{u32 name string offset, u32 name hash,
//! u32 file offset, u32 byte size}`: all 312 rows point inside the payload
//! section, back to back, and the name offsets resolve to struct names such as
//! `PlanetRegionData` and `DimensionRegionData`. The payloads themselves are
//! DDL struct instances and are left undecoded here.

use std::collections::HashMap;

use crate::core::dat1::Dat1;
use crate::core::error::{Result, ToolkitError};
use byteorder::{ByteOrder, LE};
use serde::Serialize;

pub const ASSET_HEADER_LEN: usize = 36;

pub const T_LEVEL_BUILT: u32 = 0x7CA7_267D;
pub const T_REGIONS_BUILT: u32 = 0x396F_9418;
pub const T_ZONES_BUILT: u32 = 0x4E02_3760;
pub const T_LINKS_BUILT: u32 = 0x3395_AEC1;
pub const T_REGION_LOD_BUILT: u32 = 0xC30D_92B6;
pub const T_ZONE_REFERENCES: u32 = 0x95F9_1E24;
pub const T_REGION_NAMES: u32 = 0x4130_D903;
pub const T_ZONE_NAMES: u32 = 0x2BA3_3702;
pub const T_LINK_NAMES: u32 = 0x2236_C47A;
pub const T_REGION_GAME_STRUCT: u32 = 0x4188_7FB3;
pub const T_REGION_GAME_DATA: u32 = 0x339C_970E;
pub const T_HIBERNATE_MODEL_NAMES: u32 = 0x7077_E5F5;
pub const T_HIBERNATE_VFX_NAMES: u32 = 0x6251_A0BF;
pub const T_HIBERNATE_RAW_DATA: u32 = 0x07C7_5341;
pub const T_HIBERNATE_DATA_DESC: u32 = 0x611F_490D;

pub const LEVEL_BUILT_LEN: usize = 36;
pub const REGION_STRIDE: usize = 36;
pub const ZONE_STRIDE: usize = 12;
pub const LINK_STRIDE: usize = 48;
pub const REGION_LOD_STRIDE: usize = 24;
pub const HIBERNATE_DESC_STRIDE: usize = 8;
pub const HIBERNATE_CONTAINER_LEN: usize = 0x50;
pub const HIBERNATE_CLUSTER_LEN: usize = 32;
pub const HOBJECT_LEN: usize = 40;

/// The `.level` builder's version word, which sits in the DAT1 header where a
/// zone keeps its type id. It is the hash of the builder's version string, not
/// a type id, so a future game patch can change it — hence a warning rather
/// than a hard failure on mismatch.
pub const LEVEL_BUILT_VERSION: u32 = 0x587B_60A6;

/// `RegionType` ordinal -> a readable name.
///
/// order is the declaration order of the matching per-type auxiliary structs,
/// and is corroborated on the shipped level: the region that `LevelBuilt`
/// names as the always-resident global region has type 0, the only regions
/// carrying LOD controls have type 5, and the only regions carrying secondary
/// (shadow) zone refs have type 6. See `level_asset_check` for the counts.
pub fn region_type_name(ty: i16) -> &'static str {
    match ty {
        0 => "kGlobal",
        1 => "kDebug",
        2 => "kInstanceContainer",
        3 => "kInstanceUnit",
        4 => "kOpenContainer",
        5 => "kOpenUnit",
        6 => "kOverlay",
        _ => "unknown",
    }
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct LevelHeader {
    pub default_named_link_id: u64,
    pub global_region_index: i32,
    pub region_count: i32,
    pub zone_ref_count: i32,
    pub lod_control_count: i32,
    pub zone_count: i32,
    pub named_link_count: i32,
    pub game_data_count: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct LevelRegion {
    pub index: usize,
    pub asset_id: u64,
    pub type_id: i16,
    pub type_name: &'static str,
    /// Resolved from `Level Region Names`; this is the `.region` asset path.
    pub name: String,
    pub name_index: i16,
    pub lod_controls_index: i16,
    pub parent_index: i16,
    pub child_start: i16,
    pub child_count: i16,
    pub zone_refs_start: i16,
    pub zone_refs_count: i16,
    pub secondary_zone_refs_start: i16,
    pub secondary_zone_refs_count: i16,
    pub named_link_start: i16,
    pub named_link_count: i16,
    pub game_data_start: i16,
    pub game_data_count: i16,
}

#[derive(Debug, Clone, Serialize)]
pub struct LevelZone {
    pub index: usize,
    pub asset_id: u64,
    pub name_index: i16,
    /// Resolved from `Level Zone Names`; the `.zone` asset path.
    pub name: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct LevelRegionLod {
    pub position: [f32; 3],
    pub tile_radius: u32,
    pub load_distance: u32,
    pub unload_distance: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct LevelNamedLink {
    pub index: usize,
    pub id: u64,
    pub actor_id: u64,
    pub actor_asset_id: u64,
    pub name_hash: u32,
    pub name_index: i16,
    pub load_region_index: i16,
    pub game_data_start: i16,
    pub game_data_count: i16,
    pub position: [f32; 3],
    pub name: String,
}

impl LevelNamedLink {
    /// Whether `name_hash` is the engine CRC-32 of the resolved name string.
    /// The name table stores an asset-style path for regions and zones, so it
    /// is worth checking what the hash is actually taken over before relying
    /// on it to look a link up by name.
    pub fn name_hash_matches(&self) -> bool {
        crate::core::crc32::hash(&self.name) == self.name_hash
    }

    /// The last path component of the name, which is what a link is normally
    /// called in the editor.
    pub fn short_name(&self) -> &str {
        self.name.rsplit('/').next().unwrap_or(&self.name)
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ZoneHibernateRef {
    pub zone_index: i32,
    pub byte_offset: u32,
}

/// One of the two sub-containers (models, visual effects) inside a zone's
/// hibernate container.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct HibernateSubContainer {
    pub centre: [f32; 3],
    pub radius: f32,
    pub cluster_offset: u32,
    pub cluster_count: u16,
    /// Non-zero once the engine has rewritten the records into the packed
    /// runtime form. Zero in shipped data.
    pub optimized: u16,
    pub object_offset: u32,
    pub object_count: u32,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct HibernateCluster {
    pub centre: [f32; 3],
    pub radius: f32,
    pub draw_distance: f32,
    pub visible_count: u32,
    pub object_offset: u32,
    pub object_count: u32,
}

/// One far-LOD instance, decoded into world space.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct HibernateObject {
    /// Index of the owning cluster inside its sub-container.
    pub cluster: usize,
    /// World position in metres: the cluster centre plus the record's offset.
    pub position: [f32; 3],
    /// Offset from the cluster centre, in metres.
    pub offset: [f32; 3],
    pub scale: [f32; 3],
    /// Three Euler angles in radians, in the order the engine composes them.
    pub rotation: [f32; 3],
    pub mat_override_index: u16,
    pub mat_override_count: u8,
    pub flags: u8,
    pub rgba: u32,
    pub parent_id: u32,
    /// Index into `Level Hibernate Model Names` (models) or
    /// `Level Hibernate VFX Names` (visual effects).
    pub asset_index: i16,
    pub misc: u16,
}

impl HibernateObject {
    pub const FLAG_VISIBLE: u8 = 1 << 5;
    pub const FLAG_Z_BIAS: u8 = 1 << 6;
    pub const FLAG_LONG_RANGE: u8 = 1 << 7;

    pub fn visible(&self) -> bool {
        self.flags & Self::FLAG_VISIBLE != 0
    }
    pub fn z_bias(&self) -> bool {
        self.flags & Self::FLAG_Z_BIAS != 0
    }
    pub fn long_range(&self) -> bool {
        self.flags & Self::FLAG_LONG_RANGE != 0
    }

    /// Bit 2 of the transform bits: the instance matrix has a negative
    /// determinant, so the model is mirrored.
    pub fn mirrored(&self) -> bool {
        self.flags & (1 << 2) != 0
    }
    /// Bit 4 of the transform bits: all three scale axes are equal.
    pub fn uniform_scale(&self) -> bool {
        self.flags & (1 << 4) != 0
    }
    /// Bits 0-1 of the transform bits: which axis holds the median scale.
    pub fn median_scale_axis(&self) -> usize {
        (self.flags & 0x3) as usize
    }

    /// Linear RGB and alpha. The three colour bytes are stored sqrt-encoded
    /// from the high byte down, so squaring puts them back in linear space.
    pub fn colour(&self) -> ([f32; 3], f32) {
        let c = |shift: u32| {
            let v = ((self.rgba >> shift) & 0xFF) as f32 / 255.0;
            v * v
        };
        ([c(24), c(16), c(8)], (self.rgba & 0xFF) as f32 / 255.0)
    }
}

/// Everything one zone contributes to the level's hibernate data.
#[derive(Debug, Clone, Serialize)]
pub struct ZoneHibernate {
    /// Index into [`LevelAsset::zones`].
    pub zone_index: i32,
    pub byte_offset: u32,
    /// The zone asset id the container records for itself. Should match
    /// `zones[zone_index].asset_id`.
    pub zone_asset_id: u64,
    pub models: HibernateSubContainer,
    pub effects: HibernateSubContainer,
    pub mat_override_offset: u32,
    pub mat_override_count: u32,
    pub model_clusters: Vec<HibernateCluster>,
    pub effect_clusters: Vec<HibernateCluster>,
    pub model_objects: Vec<HibernateObject>,
    pub effect_objects: Vec<HibernateObject>,
}

// ---------------------------------------------------------------------------
// LevelAsset
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct LevelAsset {
    /// The 36-byte TOC asset header, when the caller passed it in.
    pub header: Option<Vec<u8>>,
    pub all_sizes: [u32; 8],
    pub dat1: Dat1,
    pub built: LevelHeader,
    pub regions: Vec<LevelRegion>,
    pub zones: Vec<LevelZone>,
    pub named_links: Vec<LevelNamedLink>,
    pub region_lods: Vec<LevelRegionLod>,
    /// Flat `i16[]` that region rows slice with `zone_refs_start/count` and
    /// `secondary_zone_refs_start/count`; each entry indexes [`Self::zones`].
    pub zone_refs: Vec<i16>,
    pub hibernate_refs: Vec<ZoneHibernateRef>,
    pub hibernate_model_names: Vec<String>,
    pub hibernate_vfx_names: Vec<String>,
    pub hibernate_zones: Vec<ZoneHibernate>,
    /// Raw `Level Region Game Struct Built` bytes, left undecoded — the DDL
    /// struct instances behind it are a separate job.
    pub game_data_raw: Vec<u8>,
    zone_by_id: HashMap<u64, usize>,
}

impl LevelAsset {
    /// Parse an asset as `Toc::extract_asset` hands it over, with or without
    /// the 36-byte asset header in front.
    pub fn parse(asset: &[u8]) -> Result<Self> {
        let (header, body) = split_header(asset)?;
        let mut all_sizes = [0u32; 8];
        if let Some(h) = &header {
            for (i, v) in all_sizes.iter_mut().enumerate() {
                *v = LE::read_u32(&h[4 + i * 4..8 + i * 4]);
            }
        } else {
            all_sizes[0] = body.len() as u32;
        }
        let block0_len = if all_sizes[0] > 0 && all_sizes[0] as usize <= body.len() {
            all_sizes[0] as usize
        } else {
            body.len()
        };
        let dat1 = Dat1::parse(&body[..block0_len])?;

        let built = parse_level_built(&dat1)?;
        let region_names = string_table(&dat1, T_REGION_NAMES);
        let zone_names = string_table(&dat1, T_ZONE_NAMES);
        let link_names = string_table(&dat1, T_LINK_NAMES);

        let regions = parse_regions(&dat1, &region_names);
        let zones = parse_zones(&dat1, &zone_names);
        let named_links = parse_links(&dat1, &link_names);
        let region_lods = parse_region_lods(&dat1);
        let zone_refs = parse_zone_refs(&dat1);
        let hibernate_refs = parse_hibernate_refs(&dat1);
        let hibernate_model_names = string_table(&dat1, T_HIBERNATE_MODEL_NAMES);
        let hibernate_vfx_names = string_table(&dat1, T_HIBERNATE_VFX_NAMES);
        let hibernate_zones = parse_hibernate(&dat1, &hibernate_refs);
        let game_data_raw = dat1
            .get_section_data(T_REGION_GAME_STRUCT)
            .map(|s| s.to_vec())
            .unwrap_or_default();

        let mut zone_by_id = HashMap::with_capacity(zones.len());
        for (i, z) in zones.iter().enumerate() {
            zone_by_id.entry(z.asset_id).or_insert(i);
        }

        Ok(Self {
            header,
            all_sizes,
            dat1,
            built,
            regions,
            zones,
            named_links,
            region_lods,
            zone_refs,
            hibernate_refs,
            hibernate_model_names,
            hibernate_vfx_names,
            hibernate_zones,
            game_data_raw,
            zone_by_id,
        })
    }

    /// True when the DAT1 header carries the level builder's version word we
    /// know about. A mismatch means the game was patched, not that the file is
    /// broken, so callers should warn rather than refuse to parse.
    pub fn version_known(&self) -> bool {
        self.dat1.unk1 == LEVEL_BUILT_VERSION
    }

    pub fn zone_index_by_asset(&self, id: u64) -> Option<usize> {
        self.zone_by_id.get(&id).copied()
    }

    /// The zone indices a region owns directly.
    pub fn region_zone_indices(&self, region: &LevelRegion) -> &[i16] {
        slice_i16(
            &self.zone_refs,
            region.zone_refs_start,
            region.zone_refs_count,
        )
    }

    /// The region's secondary zone refs — low-LOD zones for an open-unit
    /// region, shadow zones for an overlay region, nothing for the rest.
    pub fn region_secondary_zone_indices(&self, region: &LevelRegion) -> &[i16] {
        slice_i16(
            &self.zone_refs,
            region.secondary_zone_refs_start,
            region.secondary_zone_refs_count,
        )
    }

    pub fn region_named_links(&self, region: &LevelRegion) -> &[LevelNamedLink] {
        let start = region.named_link_start.max(0) as usize;
        let count = region.named_link_count.max(0) as usize;
        let end = (start + count).min(self.named_links.len());
        if start >= end {
            &[]
        } else {
            &self.named_links[start..end]
        }
    }

    pub fn region_lod(&self, region: &LevelRegion) -> Option<&LevelRegionLod> {
        let i = region.lod_controls_index;
        if i < 0 {
            return None;
        }
        self.region_lods.get(i as usize)
    }

    /// Region indices with no parent.
    pub fn root_regions(&self) -> Vec<usize> {
        self.regions
            .iter()
            .enumerate()
            .filter(|(_, r)| r.parent_index < 0)
            .map(|(i, _)| i)
            .collect()
    }

    /// Depth of each region in the parent chain (roots are 0). A region caught
    /// in a cycle, or whose parent index is out of range, gets `None`.
    pub fn region_depths(&self) -> Vec<Option<usize>> {
        let n = self.regions.len();
        let mut out = vec![None; n];
        for i in 0..n {
            let mut depth = 0usize;
            let mut cur = i;
            let mut ok = true;
            // A level has a handful of regions, so walking the chain per
            // region costs nothing and needs no memoisation.
            while self.regions[cur].parent_index >= 0 {
                let p = self.regions[cur].parent_index as usize;
                if p >= n || depth > n {
                    ok = false;
                    break;
                }
                cur = p;
                depth += 1;
            }
            out[i] = ok.then_some(depth);
        }
        out
    }

    /// Total hibernated instances across every zone, models and effects.
    pub fn hibernate_object_count(&self) -> usize {
        self.hibernate_zones
            .iter()
            .map(|h| h.model_objects.len() + h.effect_objects.len())
            .sum()
    }

    /// Structural checks that do not need the TOC. Every message names the
    /// record it came from so a harness can print them verbatim.
    pub fn validate(&self) -> Vec<String> {
        let mut out = Vec::new();
        let regions = self.regions.len();
        let zones = self.zones.len();
        let links = self.named_links.len();

        if self.built.region_count as usize != regions {
            out.push(format!(
                "header region_count {} != {regions} rows in Level Regions Built",
                self.built.region_count
            ));
        }
        if self.built.zone_count as usize != zones {
            out.push(format!(
                "header zone_count {} != {zones} rows in Level Zones Built",
                self.built.zone_count
            ));
        }
        if self.built.named_link_count as usize != links {
            out.push(format!(
                "header named_link_count {} != {links} rows in Level Links Built",
                self.built.named_link_count
            ));
        }
        if self.built.zone_ref_count as usize != self.zone_refs.len() {
            out.push(format!(
                "header zone_ref_count {} != {} entries in Level Zone References",
                self.built.zone_ref_count,
                self.zone_refs.len()
            ));
        }
        if self.built.lod_control_count as usize != self.region_lods.len() {
            out.push(format!(
                "header lod_control_count {} != {} rows in Level Region Lod Built",
                self.built.lod_control_count,
                self.region_lods.len()
            ));
        }
        if self.built.global_region_index < 0 || self.built.global_region_index as usize >= regions {
            out.push(format!(
                "global_region_index {} out of range",
                self.built.global_region_index
            ));
        }
        if links > 0
            && !self
                .named_links
                .iter()
                .any(|l| l.id == self.built.default_named_link_id)
        {
            out.push(format!(
                "default_named_link_id {:#018X} matches no named link",
                self.built.default_named_link_id
            ));
        }

        for (i, r) in self.regions.iter().enumerate() {
            let rng = |name: &str, start: i16, count: i16, len: usize, out: &mut Vec<String>| {
                if count < 0 || start < 0 && count > 0 {
                    out.push(format!("region {i}: {name} start {start} count {count}"));
                } else if count > 0 && start as usize + count as usize > len {
                    out.push(format!(
                        "region {i}: {name} [{start}..{}) exceeds {len}",
                        start as usize + count as usize
                    ));
                }
            };
            rng("children", r.child_start, r.child_count, regions, &mut out);
            rng(
                "zone refs",
                r.zone_refs_start,
                r.zone_refs_count,
                self.zone_refs.len(),
                &mut out,
            );
            rng(
                "secondary zone refs",
                r.secondary_zone_refs_start,
                r.secondary_zone_refs_count,
                self.zone_refs.len(),
                &mut out,
            );
            rng(
                "named links",
                r.named_link_start,
                r.named_link_count,
                links,
                &mut out,
            );
            if r.parent_index >= 0 && r.parent_index as usize >= regions {
                out.push(format!("region {i}: parent_index {} out of range", r.parent_index));
            }
            if r.lod_controls_index >= 0 && r.lod_controls_index as usize >= self.region_lods.len() {
                out.push(format!(
                    "region {i}: lod_controls_index {} out of range",
                    r.lod_controls_index
                ));
            }
            // Every child must point back at this region.
            for c in 0..r.child_count.max(0) as usize {
                let ci = r.child_start.max(0) as usize + c;
                if let Some(child) = self.regions.get(ci) {
                    if child.parent_index != i as i16 {
                        out.push(format!(
                            "region {i}: child {ci} has parent_index {}",
                            child.parent_index
                        ));
                    }
                }
            }
        }

        // -1 is a real value, not a defect: the engine fills a region's
        // secondary zone list with 0xFF bytes and treats -1 as "this primary
        // zone has no low-LOD / shadow stand-in", so the secondary list stays
        // parallel to the primary one.
        for (i, z) in self.zone_refs.iter().enumerate() {
            if *z < -1 || (*z >= 0 && *z as usize >= zones) {
                out.push(format!("zone ref {i} = {z} outside -1..{zones}"));
            }
        }

        for (i, l) in self.named_links.iter().enumerate() {
            if l.load_region_index >= 0 && l.load_region_index as usize >= regions {
                out.push(format!(
                    "named link {i}: load_region_index {} out of range",
                    l.load_region_index
                ));
            }
        }

        for h in &self.hibernate_zones {
            if h.zone_index < 0 || h.zone_index as usize >= zones {
                out.push(format!("hibernate ref: zone_index {} out of range", h.zone_index));
                continue;
            }
            let z = &self.zones[h.zone_index as usize];
            if h.zone_asset_id != z.asset_id {
                out.push(format!(
                    "hibernate container for zone {} records asset id {:#018X}, zone row has {:#018X}",
                    h.zone_index, h.zone_asset_id, z.asset_id
                ));
            }
            let check = |what: &str, objs: &[HibernateObject], names: usize, out: &mut Vec<String>| {
                for o in objs {
                    if o.asset_index < 0 || o.asset_index as usize >= names {
                        out.push(format!(
                            "hibernate {what} in zone {}: asset_index {} outside 0..{names}",
                            h.zone_index, o.asset_index
                        ));
                        break;
                    }
                }
            };
            check(
                "model",
                &h.model_objects,
                self.hibernate_model_names.len(),
                &mut out,
            );
            check(
                "effect",
                &h.effect_objects,
                self.hibernate_vfx_names.len(),
                &mut out,
            );
        }

        out
    }
}

// ---------------------------------------------------------------------------
// Section readers
// ---------------------------------------------------------------------------

fn split_header(asset: &[u8]) -> Result<(Option<Vec<u8>>, &[u8])> {
    if asset.len() < 8 {
        return Err(ToolkitError::Parse("level asset too small".into()));
    }
    if LE::read_u32(&asset[0..4]) == crate::core::dat1::DAT1_MAGIC {
        Ok((None, asset))
    } else if asset.len() > ASSET_HEADER_LEN
        && LE::read_u32(&asset[ASSET_HEADER_LEN..ASSET_HEADER_LEN + 4])
            == crate::core::dat1::DAT1_MAGIC
    {
        Ok((Some(asset[..ASSET_HEADER_LEN].to_vec()), &asset[ASSET_HEADER_LEN..]))
    } else {
        Err(ToolkitError::Parse(
            "level asset has neither a DAT1 magic nor a 36-byte asset header".into(),
        ))
    }
}

fn parse_level_built(dat1: &Dat1) -> Result<LevelHeader> {
    let s = dat1
        .get_section_data(T_LEVEL_BUILT)
        .ok_or(ToolkitError::SectionNotFound(T_LEVEL_BUILT))?;
    if s.len() < LEVEL_BUILT_LEN {
        return Err(ToolkitError::Parse(format!(
            "Level Built section is {} bytes, needs {LEVEL_BUILT_LEN}",
            s.len()
        )));
    }
    Ok(LevelHeader {
        default_named_link_id: LE::read_u64(&s[0x00..0x08]),
        global_region_index: LE::read_i32(&s[0x08..0x0C]),
        region_count: LE::read_i32(&s[0x0C..0x10]),
        zone_ref_count: LE::read_i32(&s[0x10..0x14]),
        lod_control_count: LE::read_i32(&s[0x14..0x18]),
        zone_count: LE::read_i32(&s[0x18..0x1C]),
        named_link_count: LE::read_i32(&s[0x1C..0x20]),
        game_data_count: LE::read_i32(&s[0x20..0x24]),
    })
}

/// A `u32[]` of DAT1 string-pool offsets, resolved to forward-slash paths.
fn string_table(dat1: &Dat1, tag: u32) -> Vec<String> {
    let Some(s) = dat1.get_section_data(tag) else {
        return Vec::new();
    };
    (0..s.len() / 4)
        .map(|i| {
            let off = LE::read_u32(&s[i * 4..i * 4 + 4]);
            dat1.get_string(off)
                .unwrap_or_default()
                .replace('\\', "/")
        })
        .collect()
}

fn name_at(names: &[String], index: i16) -> String {
    if index < 0 {
        return String::new();
    }
    names.get(index as usize).cloned().unwrap_or_default()
}

fn parse_regions(dat1: &Dat1, names: &[String]) -> Vec<LevelRegion> {
    let Some(s) = dat1.get_section_data(T_REGIONS_BUILT) else {
        return Vec::new();
    };
    (0..s.len() / REGION_STRIDE)
        .map(|i| {
            let r = &s[i * REGION_STRIDE..(i + 1) * REGION_STRIDE];
            let f = |o: usize| LE::read_i16(&r[o..o + 2]);
            let name_index = f(0x0A);
            let type_id = f(0x08);
            LevelRegion {
                index: i,
                asset_id: LE::read_u64(&r[0x00..0x08]),
                type_id,
                type_name: region_type_name(type_id),
                name: name_at(names, name_index),
                name_index,
                lod_controls_index: f(0x0C),
                parent_index: f(0x0E),
                child_start: f(0x10),
                child_count: f(0x12),
                zone_refs_start: f(0x14),
                zone_refs_count: f(0x16),
                secondary_zone_refs_start: f(0x18),
                secondary_zone_refs_count: f(0x1A),
                named_link_start: f(0x1C),
                named_link_count: f(0x1E),
                game_data_start: f(0x20),
                game_data_count: f(0x22),
            }
        })
        .collect()
}

fn parse_zones(dat1: &Dat1, names: &[String]) -> Vec<LevelZone> {
    let Some(s) = dat1.get_section_data(T_ZONES_BUILT) else {
        return Vec::new();
    };
    (0..s.len() / ZONE_STRIDE)
        .map(|i| {
            let r = &s[i * ZONE_STRIDE..(i + 1) * ZONE_STRIDE];
            let name_index = LE::read_i16(&r[0x08..0x0A]);
            LevelZone {
                index: i,
                asset_id: LE::read_u64(&r[0x00..0x08]),
                name_index,
                name: name_at(names, name_index),
            }
        })
        .collect()
}

fn parse_links(dat1: &Dat1, names: &[String]) -> Vec<LevelNamedLink> {
    let Some(s) = dat1.get_section_data(T_LINKS_BUILT) else {
        return Vec::new();
    };
    (0..s.len() / LINK_STRIDE)
        .map(|i| {
            let r = &s[i * LINK_STRIDE..(i + 1) * LINK_STRIDE];
            let name_index = LE::read_i16(&r[0x1C..0x1E]);
            LevelNamedLink {
                index: i,
                id: LE::read_u64(&r[0x00..0x08]),
                actor_id: LE::read_u64(&r[0x08..0x10]),
                actor_asset_id: LE::read_u64(&r[0x10..0x18]),
                name_hash: LE::read_u32(&r[0x18..0x1C]),
                name_index,
                load_region_index: LE::read_i16(&r[0x1E..0x20]),
                game_data_start: LE::read_i16(&r[0x20..0x22]),
                game_data_count: LE::read_i16(&r[0x22..0x24]),
                position: [
                    LE::read_f32(&r[0x24..0x28]),
                    LE::read_f32(&r[0x28..0x2C]),
                    LE::read_f32(&r[0x2C..0x30]),
                ],
                name: name_at(names, name_index),
            }
        })
        .collect()
}

fn parse_region_lods(dat1: &Dat1) -> Vec<LevelRegionLod> {
    let Some(s) = dat1.get_section_data(T_REGION_LOD_BUILT) else {
        return Vec::new();
    };
    (0..s.len() / REGION_LOD_STRIDE)
        .map(|i| {
            let r = &s[i * REGION_LOD_STRIDE..(i + 1) * REGION_LOD_STRIDE];
            LevelRegionLod {
                position: [
                    LE::read_f32(&r[0x00..0x04]),
                    LE::read_f32(&r[0x04..0x08]),
                    LE::read_f32(&r[0x08..0x0C]),
                ],
                tile_radius: LE::read_u32(&r[0x0C..0x10]),
                load_distance: LE::read_u32(&r[0x10..0x14]),
                unload_distance: LE::read_u32(&r[0x14..0x18]),
            }
        })
        .collect()
}

fn parse_zone_refs(dat1: &Dat1) -> Vec<i16> {
    let Some(s) = dat1.get_section_data(T_ZONE_REFERENCES) else {
        return Vec::new();
    };
    (0..s.len() / 2)
        .map(|i| LE::read_i16(&s[i * 2..i * 2 + 2]))
        .collect()
}

fn parse_hibernate_refs(dat1: &Dat1) -> Vec<ZoneHibernateRef> {
    let Some(s) = dat1.get_section_data(T_HIBERNATE_DATA_DESC) else {
        return Vec::new();
    };
    (0..s.len() / HIBERNATE_DESC_STRIDE)
        .map(|i| {
            let r = &s[i * HIBERNATE_DESC_STRIDE..(i + 1) * HIBERNATE_DESC_STRIDE];
            ZoneHibernateRef {
                zone_index: LE::read_i32(&r[0..4]),
                byte_offset: LE::read_u32(&r[4..8]),
            }
        })
        .collect()
}

fn read_sub_container(b: &[u8]) -> HibernateSubContainer {
    HibernateSubContainer {
        centre: [
            LE::read_f32(&b[0x00..0x04]),
            LE::read_f32(&b[0x04..0x08]),
            LE::read_f32(&b[0x08..0x0C]),
        ],
        radius: LE::read_f32(&b[0x0C..0x10]),
        cluster_offset: LE::read_u32(&b[0x10..0x14]),
        cluster_count: LE::read_u16(&b[0x14..0x16]),
        optimized: LE::read_u16(&b[0x16..0x18]),
        object_offset: LE::read_u32(&b[0x18..0x1C]),
        object_count: LE::read_u32(&b[0x1C..0x20]),
    }
}

/// IEEE 754 binary16 -> f32. The engine's own converter flushes NaN/Inf to a
/// finite value; the only place that matters here is corrupt data, so this
/// returns 0.0 for the non-finite encodings rather than propagating a NaN into
/// a position.
fn half_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) & 1) as u32;
    let exp = ((h >> 10) & 0x1F) as u32;
    let frac = (h & 0x3FF) as u32;
    let bits = match exp {
        0 if frac == 0 => sign << 31,
        0 => {
            // Subnormal: renormalise into a binary32 normal.
            let mut e = 0i32;
            let mut f = frac;
            while f & 0x400 == 0 {
                f <<= 1;
                e -= 1;
            }
            let e32 = (127 - 15 + e + 1) as u32;
            (sign << 31) | (e32 << 23) | ((f & 0x3FF) << 13)
        }
        0x1F => return 0.0,
        _ => (sign << 31) | ((exp + 127 - 15) << 23) | (frac << 13),
    };
    f32::from_bits(bits)
}

fn read_clusters(raw: &[u8], base: usize, offset: u32, count: u16) -> Vec<HibernateCluster> {
    let start = base + offset as usize;
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        let o = start + i * HIBERNATE_CLUSTER_LEN;
        if o + HIBERNATE_CLUSTER_LEN > raw.len() {
            break;
        }
        let b = &raw[o..o + HIBERNATE_CLUSTER_LEN];
        out.push(HibernateCluster {
            centre: [
                LE::read_f32(&b[0x00..0x04]),
                LE::read_f32(&b[0x04..0x08]),
                LE::read_f32(&b[0x08..0x0C]),
            ],
            radius: LE::read_f32(&b[0x0C..0x10]),
            draw_distance: LE::read_f32(&b[0x10..0x14]),
            visible_count: LE::read_u32(&b[0x14..0x18]),
            object_offset: LE::read_u32(&b[0x18..0x1C]),
            object_count: LE::read_u32(&b[0x1C..0x20]),
        });
    }
    out
}

/// Decode a sub-container's instances, resolving each one's cluster so the
/// cluster centre can be folded into the stored offset.
fn read_objects(
    raw: &[u8],
    base: usize,
    sub: &HibernateSubContainer,
    clusters: &[HibernateCluster],
) -> Vec<HibernateObject> {
    let start = base + sub.object_offset as usize;
    let n = sub.object_count as usize;
    // Which cluster owns each instance. `object_offset` on a cluster is an
    // index into this same array, so one pass fills the map.
    let mut owner = vec![usize::MAX; n];
    for (ci, c) in clusters.iter().enumerate() {
        let s = c.object_offset as usize;
        let e = s.saturating_add(c.object_count as usize).min(n);
        for slot in owner.iter_mut().take(e).skip(s.min(n)) {
            *slot = ci;
        }
    }

    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let o = start + i * HOBJECT_LEN;
        if o + HOBJECT_LEN > raw.len() {
            break;
        }
        let b = &raw[o..o + HOBJECT_LEN];
        let optimized = sub.optimized != 0;
        let triple = |off: usize, div: f32| -> [f32; 3] {
            let mut v = [0f32; 3];
            for (k, slot) in v.iter_mut().enumerate() {
                let w = LE::read_u16(&b[off + k * 2..off + k * 2 + 2]);
                *slot = if optimized {
                    w as i16 as f32 / div
                } else {
                    half_to_f32(w)
                };
            }
            v
        };
        let rotation = if optimized {
            // Packed-quaternion form; report it as-is rather than pretending
            // it is Euler angles.
            [0.0, 0.0, 0.0]
        } else {
            [
                half_to_f32(LE::read_u16(&b[0x00..0x02])),
                half_to_f32(LE::read_u16(&b[0x02..0x04])),
                half_to_f32(LE::read_u16(&b[0x04..0x06])),
            ]
        };
        let offset = triple(0x06, 64.0);
        let scale = triple(0x0C, 256.0);
        let cluster = owner.get(i).copied().unwrap_or(usize::MAX);
        let centre = clusters
            .get(cluster)
            .map(|c| c.centre)
            .unwrap_or([0.0, 0.0, 0.0]);
        out.push(HibernateObject {
            cluster,
            position: [
                centre[0] + offset[0],
                centre[1] + offset[1],
                centre[2] + offset[2],
            ],
            offset,
            scale,
            rotation,
            mat_override_index: LE::read_u16(&b[0x18..0x1A]),
            mat_override_count: b[0x1A],
            flags: b[0x1B],
            rgba: LE::read_u32(&b[0x1C..0x20]),
            parent_id: LE::read_u32(&b[0x20..0x24]),
            asset_index: LE::read_i16(&b[0x24..0x26]),
            misc: LE::read_u16(&b[0x26..0x28]),
        });
    }
    out
}

fn parse_hibernate(dat1: &Dat1, refs: &[ZoneHibernateRef]) -> Vec<ZoneHibernate> {
    let Some(raw) = dat1.get_section_data(T_HIBERNATE_RAW_DATA) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(refs.len());
    for r in refs {
        let base = r.byte_offset as usize;
        if base + HIBERNATE_CONTAINER_LEN > raw.len() {
            continue;
        }
        let c = &raw[base..base + HIBERNATE_CONTAINER_LEN];
        let models = read_sub_container(&c[0x00..0x20]);
        let effects = read_sub_container(&c[0x20..0x40]);
        let model_clusters = read_clusters(raw, base, models.cluster_offset, models.cluster_count);
        let effect_clusters =
            read_clusters(raw, base, effects.cluster_offset, effects.cluster_count);
        let model_objects = read_objects(raw, base, &models, &model_clusters);
        let effect_objects = read_objects(raw, base, &effects, &effect_clusters);
        out.push(ZoneHibernate {
            zone_index: r.zone_index,
            byte_offset: r.byte_offset,
            zone_asset_id: LE::read_u64(&c[0x48..0x50]),
            models,
            effects,
            mat_override_offset: LE::read_u32(&c[0x40..0x44]),
            mat_override_count: LE::read_u32(&c[0x44..0x48]),
            model_clusters,
            effect_clusters,
            model_objects,
            effect_objects,
        });
    }
    out
}

fn slice_i16(all: &[i16], start: i16, count: i16) -> &[i16] {
    if count <= 0 || start < 0 {
        return &[];
    }
    let s = start as usize;
    let e = (s + count as usize).min(all.len());
    if s >= e {
        &[]
    } else {
        &all[s..e]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats_round_trip_the_values_that_matter() {
        assert_eq!(half_to_f32(0x0000), 0.0);
        assert_eq!(half_to_f32(0x3C00), 1.0);
        assert_eq!(half_to_f32(0xBC00), -1.0);
        assert_eq!(half_to_f32(0x4000), 2.0);
        assert_eq!(half_to_f32(0x3800), 0.5);
        // 100 m, a typical cluster-relative offset magnitude.
        assert_eq!(half_to_f32(0x5640), 100.0);
        // Subnormal: the smallest positive half.
        assert!((half_to_f32(0x0001) - 5.960_464_5e-8).abs() < 1e-14);
        // Inf / NaN are flushed rather than poisoning a position.
        assert_eq!(half_to_f32(0x7C00), 0.0);
        assert_eq!(half_to_f32(0xFE00), 0.0);
    }

    #[test]
    fn index_slices_clamp_instead_of_panicking() {
        let all = [0i16, 1, 2, 3];
        assert_eq!(slice_i16(&all, 1, 2), &[1, 2]);
        assert_eq!(slice_i16(&all, -1, 2), &[] as &[i16]);
        assert_eq!(slice_i16(&all, 0, 0), &[] as &[i16]);
        assert_eq!(slice_i16(&all, 3, 9), &[3]);
        assert_eq!(slice_i16(&all, 9, 1), &[] as &[i16]);
    }

    #[test]
    fn instance_flags_split_into_instance_bits_and_transform_bits() {
        let o = HibernateObject {
            cluster: 0,
            position: [0.0; 3],
            offset: [0.0; 3],
            scale: [1.0; 3],
            rotation: [0.0; 3],
            mat_override_index: 0,
            mat_override_count: 0,
            // 0xB1, the value the first shipped instance carries: visible,
            // long range, uniform scale, median-scale axis 0, not mirrored.
            flags: 0xB1,
            rgba: 0xFF00_0080,
            parent_id: 0,
            asset_index: 0,
            misc: 0,
        };
        assert!(o.visible());
        assert!(o.long_range());
        assert!(!o.z_bias());
        assert!(o.uniform_scale());
        assert!(!o.mirrored());
        assert_eq!(o.median_scale_axis(), 1);
        // Colour is sqrt-encoded from the high byte down, alpha in the low byte.
        let (rgb, a) = o.colour();
        assert!((rgb[0] - 1.0).abs() < 1e-6);
        assert_eq!(rgb[1], 0.0);
        assert_eq!(rgb[2], 0.0);
        assert!((a - 128.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn region_types_cover_the_engines_vocabulary() {
        assert_eq!(region_type_name(0), "kGlobal");
        assert_eq!(region_type_name(6), "kOverlay");
        assert_eq!(region_type_name(7), "unknown");
        assert_eq!(region_type_name(-1), "unknown");
    }
}
