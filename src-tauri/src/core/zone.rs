//! `.zone` reader / editor model for Ratchet & Clank: Rift Apart.
//!
//! A `.zone` asset extracted from the TOC is
//!
//! ```text
//!   [36-byte "Archive TOC Asset Header Data"]   (prepended by Toc::extract_asset)
//!   [block 0]  DAT1 "Zone PID Contents"   <- parsed and edited here
//!   [block 1]  DAT1 "Zone PID Physics"    (48-byte empty DAT1 for gameplay zones)
//!   [block 2]  raw geometry appendix      (decal meshes; impostor meshes)
//!   [block 3]  raw texture atlas          (impostor zones only, always 0x40000)
//! ```
//!
//! The header is `{ u32 type_id, u32 sizes[8] }`, but only a **prefix** of
//! `sizes[]` are block sizes — the rest is type-specific metadata (for impostor
//! zones `sizes[4..8]` is a texture descriptor). The block count is therefore
//! the smallest `k` with `sum(sizes[0..k]) == asset size`, which resolves for
//! 712/712 sampled zones. Reading a fixed four sizes mis-splits the tail and
//! loses block 2, which is where every decal and impostor mesh lives.
//!
//! Block 0 sections used here (names verified by `crc32(name) == tag`):
//!
//! ```text
//!   0x06ABCAB2  Zone Scene Objects     byte-packed HEAP of variable-size entries
//!   0x6987F172  Zone Model Insts       u32[] scene-object byte offsets
//!   0xC6A5905E  Zone Model Names       { u64 id[n] }{ u32 string_offset[n] }
//!   0x70682CB8  Zone Actors            32-byte rows
//!   0xDC625B3D  Zone Actor Names       u32[] string offsets
//!   0x78684035  Zone Actor Asset Paths { u64 id[n] }{ u32 string_offset[n] }
//!   0x97FF6EB5  Zone Lights            u32[] scene-object byte offsets
//!   0xBDAB2B0D  Zone Volumes           u32[] scene-object byte offsets
//!   0xBCF43558  Zone Decals            u32[] scene-object byte offsets
//!   0x927C4EC3  Zone Visual Effect Insts u32[] scene-object byte offsets
//!   0xDC311FC3  Zone Impostors         u32[] scene-object byte offsets (kind 6)
//!   0x657512BB  Zone Decal Geometry    88-byte rows, one per Zone Decals row
//!   0x0CF58A6E  Zone Actor Groups      24-byte rows
//!   0x041071EF  Zone Actor Group Data  u16[] actor-row indices
//!   0xC4968A44  Zone Actor Group Names u32[] string offsets
//!   0x5E54ACCF  Zone Script Actions    40-byte rows
//!   0xBEAB52E7  Zone Script Plugs      12-byte rows sliced by the action rows
//!   0xD86A7934  Zone Script Vars       32-byte rows
//!   0xEF8637D5  Zone Script Strings    u32[] string offsets
//!   0x80D29828  Zone Script String Hashes  crc32 of each of those strings
//!   0x2300D240  Zone Script Priuses    DDL blobs, sliced by the action rows
//!   0x50EDC53D  Zone Actor Priuses     32-byte index rows into Actor Prius Data
//!   0x81999057  Zone Actor Prius Data  DDL blobs
//!   0x30DADA09  Zone Asset References  16-byte rows, see `core::zone_refs`
//!   0x5736A46F  Zone Vertex Paint Info 16-byte rows
//!   0x9CCAA06F  Zone Vertex Paint Color one u32 per painted vertex
//!   0x758BAFBD  Zone Actor Vertex Paint Info   the same pair for actors
//!   0xF435AE9C  Zone Actor Vertex Paint Color
//! ```
//!
//! `Zone Scene Objects` is *not* an array of fixed-size records: entries have
//! varying lengths and are addressed by byte offset from the tables above.
//! Every entry begins with a 0x80-byte header:
//!
//! ```text
//!   +0x00  f32 m[4][4]   row-major; rows 0..2 = basis, row 3 = translation
//!   +0x5C  u32 flags     HIGH BYTE = entry kind
//!   +0x70  f32 scale[3]
//!   +0x7C  u32 size      low 24 bits = exact byte length of the entry
//!   +0x80  u64 instance id     (kinds 1..6)
//!   +0x100 u64 instance id     (kind 0)
//!   +0x110 u16 model index     (kind 0) -> index into Zone Model Names
//!   +0x112 u16 extra record count (entry size == 0x140 + 0x20 * count)
//! ```
//!
//! # Two editing paths, and what each one guarantees
//!
//! * **Equal-length in-place patches** ([`Zone::set_matrix`],
//!   [`Zone::set_position`], [`Zone::set_scale`], [`Zone::undo`]). No section
//!   changes size, so no string offset and no inter-section byte offset can be
//!   invalidated, and re-serialising an untouched zone reproduces the original
//!   bytes exactly. `bin/level_check` asserts that round trip.
//! * **Rebuilding sections** ([`Zone::save`], and the model and instance adders
//!   it is built on). Sections are re-emitted from the decoded tables, which is
//!   what importing geometry and editing the script graph need. Untouched
//!   tables are written back from the bytes they were read from — including
//!   each prius blob's `raw` — so this path, too, reproduces a zone it was
//!   handed no edits for, byte for byte.
//!
//! Anything that moves bytes inside the scene-object heap must use that heap's
//! own declared entry lengths: a kind-0 entry is `0x140` bytes *plus* `0x20`
//! per extra record, and over the shipped build entries with extras are rare
//! but real, so a fixed 320-byte stride walks off the end of the first one it
//! meets. [`Zone::entry_len`] is the only reader of that field.

use std::collections::{BTreeMap, HashMap};

use byteorder::{ByteOrder, LE};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::config::{ddl_object_to_typed_json, extension_hash, typed_json_to_ddl_object};
use crate::core::crc32;
use crate::core::crc64;
use crate::core::dat1::Dat1;
use crate::core::ddl;
use crate::core::error::{Result, ToolkitError};

pub const ZONE_TYPE_ID: u32 = 0x1F390AA0;
pub const ASSET_HEADER_LEN: usize = 36;

pub const TAG_SCENE_OBJECTS: u32 = 0x06AB_CAB2;
pub const TAG_MODEL_INSTS: u32 = 0x6987_F172;
pub const TAG_MODEL_NAMES: u32 = 0xC6A5_905E;
pub const TAG_ACTORS: u32 = 0x7068_2CB8;
pub const TAG_ACTOR_NAMES: u32 = 0xDC62_5B3D;
pub const TAG_ACTOR_ASSETS: u32 = 0x7868_4035;
pub const TAG_ACTOR_PRIUSES: u32 = 0x50ED_C53D;
/// 24-byte `ActorGroupBuilt` rows.
pub const TAG_ACTOR_GROUPS: u32 = 0x0CF5_8A6E;
/// `u16[]` of actor-row indices; groups take a `(first, count)` slice of it.
pub const TAG_ACTOR_GROUP_MEMBERS: u32 = 0x0410_71EF;
/// `u32[]` string offsets, one per named group.
pub const TAG_ACTOR_GROUP_NAMES: u32 = 0xC496_8A44;
/// 40-byte `ScriptActionBuilt` rows.
pub const TAG_SCRIPT_ACTIONS: u32 = 0x5E54_ACCF;
/// 12-byte plug rows that the action rows slice by index/count.
pub const TAG_SCRIPT_PLUGS: u32 = 0xBEAB_52E7;
/// DDL blob store addressed by `ScriptAction::prius_offset/prius_size`.
pub const TAG_SCRIPT_PRIUSES: u32 = 0x2300_D240;
/// 16-byte vertex-paint index rows for model instances, and their colour store.
pub const TAG_VERTEX_PAINT_INFO: u32 = 0x5736_A46F;
pub const TAG_VERTEX_PAINT_COLOR: u32 = 0x9CCA_A06F;
/// The same pair for actor-owned instances.
pub const TAG_ACTOR_VERTEX_PAINT_INFO: u32 = 0x758B_AFBD;
pub const TAG_ACTOR_VERTEX_PAINT_COLOR: u32 = 0xF435_AE9C;
pub const TAG_LIGHTS: u32 = 0x97FF_6EB5;
pub const TAG_VOLUMES: u32 = 0xBDAB_2B0D;
pub const TAG_DECALS: u32 = 0xBCF4_3558;
pub const TAG_VFX_INSTS: u32 = 0x927C_4EC3;
pub const TAG_IMPOSTORS: u32 = 0xDC31_1FC3;
pub const TAG_DECAL_GEOMETRY: u32 = 0x6575_12BB;

/// `Zone Script Vars` — 32-byte rows.
pub const TAG_SCRIPT_VARS: u32 = 0xD86A_7934;
/// `Zone Script Strings` — `u32[]` string offsets, the script graph's own pool.
pub const TAG_SCRIPT_STRINGS: u32 = 0xEF86_37D5;
/// `Zone Script String Hashes` — `crc32` of each Zone Script Strings entry.
pub const TAG_SCRIPT_STRING_HASHES: u32 = 0x80D2_9828;
/// `Zone Actor Prius Data` — the DDL blobs the actor prius index addresses.
pub const TAG_ACTOR_PRIUS_DATA: u32 = 0x8199_9057;
/// `Zone Asset References`. The measured reader, the id families and the
/// grow/repoint planner live in [`crate::core::zone_refs`].
pub const TAG_ZONE_ASSET_REFS: u32 = crate::core::zone_refs::TAG_ZONE_ASSET_REFS;

/// Bytes per row of the fixed-size tables.
const GROUP_LEN: usize = 24;
const ACTION_LEN: usize = 40;
const PLUG_LEN: usize = 12;
const VAR_LEN: usize = 32;
const ACTOR_LEN: usize = 32;
const ACTOR_PRIUS_LEN: usize = 32;
const ASSET_REF_LEN: usize = 16;
/// A kind-0 scene-object entry with no extra records. Entries with extras are
/// longer — always read [`Zone::entry_len`] rather than assuming this.
const MODEL_INST_LEN: usize = 0x140;

const OFF_MATRIX: usize = 0x00;
const OFF_FLAGS: usize = 0x5C;
const OFF_SCALE: usize = 0x70;
const OFF_SIZE: usize = 0x7C;
const OFF_INSTANCE_ID: usize = 0x80;
const OFF_INSTANCE_K0: usize = 0x100;
const OFF_MODEL_INDEX: usize = 0x110;
const ENTRY_HEADER_LEN: usize = 0x80;
/// Kind 3 / kind 6 keep an inline NUL-terminated asset path right after the
/// shared header: a `.material` for a decal, a `.texture` atlas for an impostor.
const OFF_INLINE_PATH_DECAL: usize = 0x80;
const OFF_INLINE_PATH_IMPOSTOR: usize = 0x88;
/// Kind 6: `{ u32 vertex_offset, u32 vertex_count, u32 index_offset, u32 index_count }`
/// addressing the zone's raw block 2. Verified on 770/770 impostors of
/// megalopolis + sargasso + savali: every span fits block 2, every index is
/// below the vertex count and every packed vertex carries the 2-bit tag 1.
const OFF_IMPOSTOR_GEO: usize = 0x280;
/// Kind 3: index of this decal's `Zone Decal Geometry` row, or `u32::MAX` when
/// it projects no mesh. See `build_index` for the measurements.
const OFF_DECAL_ROW: usize = 0x2BC;

pub const KIND_MODEL: u8 = 0;
pub const KIND_LIGHT: u8 = 1;
pub const KIND_ACTOR: u8 = 2;
pub const KIND_DECAL: u8 = 3;
pub const KIND_SPLINE: u8 = 4;
pub const KIND_VFX: u8 = 5;
pub const KIND_IMPOSTOR: u8 = 6;

pub fn kind_name(kind: u8) -> &'static str {
    match kind {
        KIND_MODEL => "model",
        KIND_LIGHT => "light",
        KIND_ACTOR => "actor",
        KIND_DECAL => "decal",
        KIND_SPLINE => "spline",
        KIND_VFX => "vfx",
        KIND_IMPOSTOR => "impostor",
        _ => "other",
    }
}

/// Bit 63 of an instance id / actor uid marks the object as **placed**, i.e.
/// authored into a zone rather than spawned at runtime.
///
/// Measured over 4 063 shipped PC zones: **113 046 of 113 046** actor uids carry
/// it, and so does every scene-object instance id of kinds 0 (model), 1 (light)
/// and 2 (actor volume) — 466 527 of the 472 339 ids read. The 5 812 that do not
/// are all kinds whose `+0x80` quadword is *not* a uid: every one of the 1 759
/// vfx entries, 3 944 of 7 853 splines and 109 of 221 impostors. Treat
/// `is_placed` as meaningful for actors and kinds 0..2 only.
///
/// Clearing the bit on a cloned id makes the engine treat the object as
/// runtime-spawned, which is why editing ids is a trap.
pub const PLACED_BIT: u64 = 1 << 63;

pub fn is_placed(uid: u64) -> bool {
    uid & PLACED_BIT != 0
}

/// Bit names of the actor flag word, bit 0 first.
///
/// Most of these are runtime state the engine sets after load. Over 113 046
/// actor rows of 4 063 shipped zones **only bits 2 and 3 ever appear on disk**:
///
/// ```text
///   0x0008  105 106  StartedActive
///   0x0000    3 833  (dormant until a script activates it)
///   0x000C    3 108  Template | StartedActive
///   0x0004      999  Template
/// ```
///
/// i.e. 108 214 rows start active and 4 107 are templates for spawners to clone.
/// No other value occurs, and the half-word behind the flags is 0 on all 113 046
/// rows.
pub const ACTOR_FLAG_NAMES: [&str; 16] = [
    "Activated",
    "Hatched",
    "Template",
    "StartedActive",
    "PriusPermanent",
    "EnableActivationEvents",
    "ActivateEventsBroadcast",
    "DestroyOnUnloadZone",
    "DestroyOnUnloadLevel",
    "Allocated",
    "Destroying",
    "IgnoreZoneVisibilityChanges",
    "IsActivatingOrDeactivating",
    "UpdateEnabled",
    "TimeScaleUpdated",
    "ZoneUnloading",
];

/// The set bits of an actor flag word, as names.
pub fn actor_flag_names(flags: u16) -> Vec<&'static str> {
    (0..16)
        .filter(|b| flags & (1 << b) != 0)
        .map(|b| ACTOR_FLAG_NAMES[b])
        .collect()
}

/// One entry of the Zone Scene Objects heap.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneObject {
    /// Byte offset inside the Zone Scene Objects section. This is the entry's identity.
    pub offset: u32,
    pub kind: u8,
    pub kind_name: String,
    pub size: u32,
    pub flags: u32,
    /// Row-major 4x4; row 3 is the translation.
    pub matrix: [f32; 16],
    pub scale: [f32; 3],
    #[serde(serialize_with = "ser_u64_str", deserialize_with = "de_u64_str")]
    pub instance_id: u64,
    /// Bit 63 of the instance id: authored into this zone, not runtime-spawned.
    #[serde(default)]
    pub is_placed: bool,
    /// Index into Zone Model Names, or -1.
    pub model_index: i32,
    /// Resolved model asset id, or 0.
    #[serde(serialize_with = "ser_u64_str", deserialize_with = "de_u64_str")]
    pub model_id: u64,
    /// Model path taken from the zone's own string pool (no external hash list needed).
    #[serde(default)]
    pub model_path: String,
    /// Number of trailing 0x20-byte records on a kind-0 entry.
    ///
    /// They are **not** extra transforms: each is
    /// `{ f32 value; f32 0,0,0; u32 crc32_tag(parameter name); u32 0,0,0 }`,
    /// i.e. one scalar shader-parameter override. `0x06F19114 == "Emissive"`
    /// and `0x77AF2D2B == "emissive_amount"` recovered by CRC32 inversion; the
    /// other six hashes seen are single-valued too. Over 38 670 kind-0 entries
    /// of eight real zones only 44 carry any, so they cost no geometry.
    #[serde(default)]
    pub extra_count: u16,
    pub name: String,
    pub actor_index: i32,
    /// Kind 3: the inline `.material` path. Kind 6: the inline `.texture`
    /// atlas path. Empty for every other kind.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub asset_path: String,
    /// Where this entry's geometry lives inside the zone's raw block 2 —
    /// kind 6 (impostor shell) and kind 3 (projected decal mesh) only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<GeoRef>,
}

/// A span of the zone's raw block 2 holding one mesh.
///
/// Impostors (kind 6) store `vertex_count` packed `u32` positions
/// (R10G10B10A2, each channel `/1023` inside the entry's own unit box) and
/// `index_count` `u16` indices. Decals (kind 3) store `vertex_count` 16-byte
/// `Model Std Vert` vertices at position scale `1/32768` in the entry's local
/// space, then `index_count` `u16` indices.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoRef {
    pub vertex_offset: u32,
    pub vertex_count: u32,
    pub index_offset: u32,
    pub index_count: u32,
}

impl SceneObject {
    pub fn position(&self) -> [f32; 3] {
        [self.matrix[12], self.matrix[13], self.matrix[14]]
    }

    pub fn label(&self) -> String {
        if self.name.is_empty() {
            format!("{} @{}", self.kind_name, self.offset)
        } else {
            self.name.clone()
        }
    }
}

/// A 32-byte row of Zone Actors.
///
/// ```text
///   +0x00 u32 name_index      -> Zone Actor Names
///   +0x04 u32 asset_index     -> Zone Actor Asset Paths
///   +0x08 u32 scene_offset    -> Zone Scene Objects heap
///   +0x0C i32 prius_index     first row in Zone Actor Priuses, -1 when none
///   +0x10 i32 prius_count
///   +0x14 u16 flags           see [`ACTOR_FLAG_NAMES`]
///   +0x16 u16 pad             0 on every shipped row measured
///   +0x18 u64 uid             bit 63 = placed
/// ```
///
/// The word at +0x14 used to be read here as a `u32` that was "observed always
/// 8"; it is the flag word, and 8 is the single bit `StartedActive`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorRow {
    /// Row ordinal inside Zone Actors.
    pub index: u32,
    /// Index into Zone Actor Names. Never negative on any shipped row.
    pub name_index: u32,
    /// Index into Zone Actor Asset Paths. Never negative on any shipped row.
    pub asset_index: u32,
    pub scene_offset: u32,
    /// First row in Zone Actor Priuses, or -1.
    pub prius_index: i32,
    pub prius_count: i32,
    /// Raw actor flag word.
    pub flags: u16,
    /// The padding half-word that follows the flags. Kept so the harness can
    /// assert it stays zero; never shown in the UI.
    #[serde(skip)]
    pub pad: u16,
    /// The set flag bits, by name, for the inspector badge.
    pub flag_names: Vec<&'static str>,
    #[serde(serialize_with = "ser_u64_str")]
    pub instance_id: u64,
    /// Bit 63 of the uid: authored into this zone rather than spawned.
    pub is_placed: bool,
    pub name: String,
    #[serde(serialize_with = "ser_u64_str")]
    pub asset_id: u64,
    pub asset_path: String,
}

/// Upstream's name for the same row.
pub type ZoneActor = ActorRow;

impl ActorRow {
    /// True when the actor only exists to be cloned by a spawner.
    pub fn is_template(&self) -> bool {
        self.flags & (1 << 2) != 0
    }

    /// True when the actor is active as soon as its zone loads.
    pub fn starts_active(&self) -> bool {
        self.flags & (1 << 3) != 0
    }

    /// True when the row owns a slice of Zone Actor Priuses.
    pub fn has_priuses(&self) -> bool {
        self.prius_index >= 0
    }
}

/// A 24-byte row of Zone Actor Groups (`ActorGroupBuilt`).
///
/// ```text
///   +0x00 i32 name_index        -> Zone Actor Group Names, -1 when unnamed
///   +0x04 i32 first_actor_index -> first slot in Zone Actor Group Data
///   +0x08 u16 actor_count
///   +0x0A u16 is_global
///   +0x0C u32 debug_color
///   +0x10 u64 uid
/// ```
///
/// Zone Actor Group Data is a `u16[]` of Zone Actors row indices; a group owns
/// the `actor_count` slots starting at `first_actor_index`.
///
/// Measured over 5 792 groups in 378 of 4 063 shipped zones: every group is
/// named, every one of the 30 943 member indices addresses a real actor row, the
/// slices chain head to tail with no gap and leave no Group Data slot unclaimed,
/// 25 groups are global, and `debug_color` is `0xFFFFFFFF` on every row. Only
/// 2 555 of the 5 792 have `name_index == index`, which is what rules out the
/// older reading of `+0x00` as a repeat of the row ordinal.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorGroup {
    /// Row ordinal inside Zone Actor Groups.
    pub index: u32,
    pub name_index: i32,
    pub name: String,
    pub first_actor_index: i32,
    pub actor_count: u16,
    pub is_global: bool,
    /// ARGB debug colour the authoring tool drew the group with.
    pub debug_color: u32,
    #[serde(serialize_with = "ser_u64_str")]
    pub id: u64,
    pub is_placed: bool,
    /// The group's members, as Zone Actors row indices.
    pub members: Vec<u16>,
}

/// A 40-byte row of Zone Script Actions (`ScriptActionBuilt`).
///
/// ```text
///   +0x00 u64 uid
///   +0x08 u64 template_id          the script graph this action belongs to
///   +0x10 u16 in_degree
///   +0x12 u16 link_start     -> Zone Script Plugs (12-byte rows)
///   +0x14 u16 link_count
///   +0x16 u16 param_start
///   +0x18 u16 param_count
///   +0x1A u16 type_index
///   +0x1C u32 type_hash
///   +0x20 u32 prius_offset       -> Zone Script Priuses
///   +0x24 u32 prius_size
/// ```
///
/// The out/var plug slices were previously called "link plugs" and "param
/// plugs" here.
///
/// Measured over 215 041 action rows in 620 of 4 063 shipped zones:
///
/// * `crc32(name)` of the string `type_index` selects out of Zone Script
///   Strings equals `type_hash` on **215 041 of 215 041** rows, which
///   pins both fields to these offsets;
/// * out-plug and var-plug slices are slices of the *same* Zone Script Plugs
///   array — together they tile all 465 730 plug rows with **0 overlaps and 0
///   unclaimed rows**;
/// * 146 199 slices are empty and every one of them stores the index
///   [`NO_PLUG`] with a zero count;
/// * every `prius_offset + prius_size` stays inside Zone Script Priuses.
///
/// `in_degree` has no index of its own: in-plugs are named by the *other*
/// actions' out-plug rows. It is 0 on 43 521 rows and 1 on 132 124.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptAction {
    pub index: u32,
    #[serde(serialize_with = "ser_u64_str")]
    pub node_id: u64,
    #[serde(serialize_with = "ser_u64_str")]
    pub template_id: u64,
    pub in_degree: u16,
    pub link_start: u16,
    pub link_count: u16,
    pub param_start: u16,
    pub param_count: u16,
    pub type_index: u16,
    pub type_hash: u32,
    pub prius_offset: u32,
    pub prius_size: u32,
}

/// A plug index of `0xFFFF` means the action owns no slice of that kind; the
/// matching count is 0 on every shipped row.
pub const NO_PLUG: u16 = u16::MAX;

impl ScriptAction {
    /// Read one 40-byte row.
    fn read(index: u32, b: &[u8]) -> Self {
        Self {
            index,
            node_id: LE::read_u64(&b[0..8]),
            template_id: LE::read_u64(&b[8..16]),
            in_degree: LE::read_u16(&b[16..18]),
            link_start: LE::read_u16(&b[18..20]),
            link_count: LE::read_u16(&b[20..22]),
            param_start: LE::read_u16(&b[22..24]),
            param_count: LE::read_u16(&b[24..26]),
            type_index: LE::read_u16(&b[26..28]),
            type_hash: LE::read_u32(&b[28..32]),
            prius_offset: LE::read_u32(&b[32..36]),
            prius_size: LE::read_u32(&b[36..40]),
        }
    }

    /// Write the row back at the same 40 bytes. `index` is this row's own
    /// ordinal and is not stored in the file.
    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.node_id.to_le_bytes());
        out.extend_from_slice(&self.template_id.to_le_bytes());
        for v in [
            self.in_degree,
            self.link_start,
            self.link_count,
            self.param_start,
            self.param_count,
            self.type_index,
        ] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.type_hash.to_le_bytes());
        out.extend_from_slice(&self.prius_offset.to_le_bytes());
        out.extend_from_slice(&self.prius_size.to_le_bytes());
    }

    fn span(start: u16, count: u16) -> std::ops::Range<usize> {
        if start == NO_PLUG || count == 0 {
            0..0
        } else {
            start as usize..(start as usize + count as usize)
        }
    }

    /// The out-plug rows as a (possibly empty) range, for slicing with `get`.
    pub fn link_range(&self) -> std::ops::Range<usize> {
        Self::span(self.link_start, self.link_count)
    }

    /// The var-plug rows as a (possibly empty) range.
    pub fn param_range(&self) -> std::ops::Range<usize> {
        Self::span(self.param_start, self.param_count)
    }

    /// The action's out-plug rows inside Zone Script Plugs, if it has any.
    ///
    /// The range comes straight from the file, so it is only as trustworthy as
    /// the asset. Slice with [`Self::link_plugs_within`] instead whenever the row
    /// count is to hand — a hand-edited or truncated zone would otherwise panic
    /// the caller rather than fail the parse.
    pub fn link_plugs(&self) -> Option<std::ops::Range<usize>> {
        plug_range(self.link_start, self.link_count)
    }

    /// The action's var-plug rows inside Zone Script Plugs, if it has any.
    ///
    /// Unchecked, like [`Self::link_plugs`]; prefer [`Self::param_plugs_within`].
    pub fn param_plugs(&self) -> Option<std::ops::Range<usize>> {
        plug_range(self.param_start, self.param_count)
    }

    /// [`Self::link_plugs`] bounded by the number of plug rows the zone actually
    /// has. `None` when the action owns no slice or the slice leaves the section.
    pub fn link_plugs_within(&self, rows: usize) -> Option<std::ops::Range<usize>> {
        bounded(self.link_plugs(), rows)
    }

    /// [`Self::param_plugs`] bounded by the number of plug rows the zone has.
    pub fn param_plugs_within(&self, rows: usize) -> Option<std::ops::Range<usize>> {
        bounded(self.param_plugs(), rows)
    }
}

/// Drop a file-supplied range that would read past the end of its section.
///
/// Every shipped zone measured stays in bounds — the out- and var-plug slices
/// tile Zone Script Plugs exactly — so this only ever fires on a damaged or
/// hand-edited asset.
fn bounded(range: Option<std::ops::Range<usize>>, rows: usize) -> Option<std::ops::Range<usize>> {
    match range {
        Some(r) if r.end <= rows => Some(r),
        _ => None,
    }
}

fn plug_range(index: u16, count: u16) -> Option<std::ops::Range<usize>> {
    if index == NO_PLUG || count == 0 {
        return None;
    }
    Some(index as usize..index as usize + count as usize)
}

/// One plug record (`Zone Script Plugs`, 12 bytes).
///
/// In a *link* plug `index` is a `ScriptAction` index and the connection runs
/// from this node's `name_hash` to that node's `other_hash`. In a *param* plug
/// `index` is a `ScriptVar` index and `other_hash` is the direction —
/// `crc32("Out")` reads the variable, `crc32("In")` writes it.
#[derive(Debug, Clone, Copy)]
pub struct ScriptPlug {
    pub name_hash: u32,
    pub index: u32,
    pub other_hash: u32,
}

/// One script variable (`Zone Script Vars`, 32 bytes).
#[derive(Debug, Clone)]
pub struct ScriptVar {
    pub value_type: u16,
    /// Zone Script Strings index of the name; `0xFFFF` for literal constants.
    pub name_index: u16,
    /// Script Strings index of a string var's value (type 4), e.g. a loc key.
    pub name_index2: u16,
    pub name_index3: u16,
    pub id: u64,
    pub value: [u8; 16],
}

impl ScriptVar {
    pub const TYPE_INT: u16 = 1;
    pub const TYPE_FLOAT: u16 = 2;
    pub const TYPE_STRING: u16 = 4;

    fn read(b: &[u8]) -> Self {
        let rd16 = |o: usize| u16::from_le_bytes(b[o..o + 2].try_into().unwrap());
        Self {
            value_type: rd16(0),
            name_index: rd16(2),
            name_index2: rd16(4),
            name_index3: rd16(6),
            id: u64::from_le_bytes(b[8..16].try_into().unwrap()),
            value: b[16..32].try_into().unwrap(),
        }
    }

    fn write(&self, out: &mut Vec<u8>) {
        for v in [
            self.value_type,
            self.name_index,
            self.name_index2,
            self.name_index3,
        ] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.id.to_le_bytes());
        out.extend_from_slice(&self.value);
    }

    /// Numeric value, for the int and float types only.
    pub fn as_number(&self) -> Option<f64> {
        let raw = u32::from_le_bytes(self.value[0..4].try_into().unwrap());
        match self.value_type {
            Self::TYPE_INT => Some(raw as f64),
            Self::TYPE_FLOAT => Some(f32::from_bits(raw) as f64),
            _ => None,
        }
    }

    fn set_number(&mut self, v: f64) -> Result<()> {
        let bytes = match self.value_type {
            Self::TYPE_INT => (v.max(0.0).round() as u32).to_le_bytes(),
            Self::TYPE_FLOAT => (v as f32).to_le_bytes(),
            other => {
                return Err(ToolkitError::Unsupported(format!(
                    "script var type {other} is not numeric"
                )));
            }
        };
        self.value[0..4].copy_from_slice(&bytes);
        Ok(())
    }

    pub fn id_value(&self) -> u64 {
        u64::from_le_bytes(self.value[0..8].try_into().unwrap())
    }

    fn set_id(&mut self, id: u64) {
        self.value[0..8].copy_from_slice(&id.to_le_bytes());
    }
}

/// One node's plugs, split by group.
#[derive(Debug, Clone, Default)]
pub struct NodePlugs {
    pub links: Vec<ScriptPlug>,
    pub params: Vec<ScriptPlug>,
}

/// Index entry pointing at a per-actor prius blob (`Zone Actor Priuses`, 32 bytes).
#[derive(Debug, Clone)]
pub struct ActorPriusEntry {
    pub type_id: u64,
    pub name_offset: u32,
    pub name_hash: u32,
    pub reserved: u32,
    /// Absolute offset inside the DAT1, not relative to the data section.
    pub data_offset: u32,
    pub data_size: u32,
    pub tail: u32,
}

impl ActorPriusEntry {
    fn read(b: &[u8]) -> Self {
        let rd = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        Self {
            type_id: u64::from_le_bytes(b[0..8].try_into().unwrap()),
            name_offset: rd(8),
            name_hash: rd(12),
            reserved: rd(16),
            data_offset: rd(20),
            data_size: rd(24),
            tail: rd(28),
        }
    }

    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.type_id.to_le_bytes());
        out.extend_from_slice(&self.name_offset.to_le_bytes());
        out.extend_from_slice(&self.name_hash.to_le_bytes());
        out.extend_from_slice(&self.reserved.to_le_bytes());
        out.extend_from_slice(&self.data_offset.to_le_bytes());
        out.extend_from_slice(&self.data_size.to_le_bytes());
        out.extend_from_slice(&self.tail.to_le_bytes());
    }
}

/// Outbound dependency entry (`Zone Asset References`, 16 bytes).
#[derive(Debug, Clone)]
pub struct AssetRef {
    pub asset_id: u64,
    pub name_offset: u32,
    pub ext_hash: u32,
}

/// A static model instance: where it sits, and the bounds its model's Built section gives.
#[derive(Debug, Clone)]
pub struct ModelPlacement {
    /// Index into Zone Model Names, from `Zone::model_slot`.
    pub model: usize,
    /// The model's X, Y and Z axes in world space, each times that axis's scale.
    pub axes: [[f32; 3]; 3],
    pub position: [f32; 3],
    /// Object-space bounding sphere: centre, then radius.
    pub bsphere: [f32; 4],
    pub half_extents: [f32; 3],
}

/// A deduplicated DDL blob shared by one or more owners.
///
/// Untouched blobs are written back from `raw` so that saving a zone with no
/// edits reproduces the original file byte for byte — the shipped pool holds a
/// private copy of each field-name string, which re-serialization would
/// collapse onto the first matching entry.
#[derive(Debug, Clone)]
pub struct PriusBlob {
    pub offset: u32,
    pub size: u32,
    pub json: Value,
    pub owners: Vec<usize>,
    pub raw: Vec<u8>,
    pub dirty: bool,
    /// Why the DDL inside this blob could not be read, when it could not.
    ///
    /// One shipped zone out of the 3 597 that carry objects
    /// (`i29_blizar_startingarea_gp.zone`) holds a script prius blob the DDL
    /// reader will not decode, and a zone the editor cannot open at all is a
    /// worse answer than a zone with one unreadable sheet. `json` is `Null` on
    /// such a blob and [`Zone::blob_bytes`] refuses to re-serialize it, so it
    /// is written back from `raw` and the file still round-trips byte for byte.
    pub error: Option<String>,
}

/// A 16-byte row of Zone Vertex Paint Info (`ZoneVertexPaintInfo`).
///
/// ```text
///   +0x00 u64 uid           the instance id of the scene object it paints
///   +0x08 i32 count         vertices, i.e. `count * 4` bytes of colour
///   +0x0C u32 data_offset   byte offset into the matching Color section
/// ```
///
/// Two independent pairs exist: model instances use
/// [`TAG_VERTEX_PAINT_INFO`] / [`TAG_VERTEX_PAINT_COLOR`], actor-owned instances
/// use [`TAG_ACTOR_VERTEX_PAINT_INFO`] / [`TAG_ACTOR_VERTEX_PAINT_COLOR`].
///
/// Measured over 4 300 rows (128 of them in the actor table) in 68 of 4 063
/// shipped zones:
///
/// * every uid is the instance id of a kind-0 scene object in the same zone
///   (4 300 / 4 300) — no row points at an actor, light or decal;
/// * `count` equals that model's own `Model Built` vertex count on all 4 300
///   rows, so the run is one colour per vertex of the whole model;
/// * `data_offset .. data_offset + count * 4` stays inside the Color section on
///   every row, and the runs tile it exactly: 132 217 952 of 132 217 952 bytes
///   claimed, no gap and no overlap.
///
/// 33 054 488 vertices carry paint in that sample. Nothing here is wired into
/// rendering. A renderer would need to: take the painted instance's model,
/// upload the `count` `u32`s as a per-vertex attribute in the model's *own*
/// vertex order, and map them onto whatever vertices it actually draws —
/// `decode_model_mesh` emits LOD 0 subsets only, so it would have to carry each
/// subset's vertex offset through to index the colour run instead of assuming
/// the mesh it built is vertex 0..n of the model. The channel order inside each
/// `u32` is not measured here.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VertexPaint {
    #[serde(serialize_with = "ser_u64_str")]
    pub uid: u64,
    pub count: i32,
    pub data_offset: u32,
    /// True for the actor-owned table.
    pub actor: bool,
}

/// One applied in-place patch, for undo.
#[derive(Debug, Clone)]
pub struct ZoneEdit {
    pub section: u32,
    pub offset: usize,
    pub old: Vec<u8>,
    pub new: Vec<u8>,
    pub what: String,
}

pub struct Zone {
    /// The 36-byte TOC asset header, when the asset carried one.
    pub header: Option<Vec<u8>>,
    pub sizes: [u32; 4],
    /// All eight header words, of which only a prefix are block sizes.
    pub all_sizes: [u32; 8],
    pub dat1: Dat1,
    /// Blocks after block 0, carried through verbatim.
    pub tail: Vec<u8>,
    /// `(start, len)` of blocks 1.. inside [`Zone::tail`]. Index 0 of this
    /// vector is block 1.
    pub block_spans: Vec<(usize, usize)>,

    // -- the scene-object heap and the tables that address it --------------
    pub scene_objects: Vec<SceneObject>,
    pub actors: Vec<ActorRow>,
    pub actor_names: Vec<String>,
    pub actor_groups: Vec<ActorGroup>,
    /// Both vertex-paint index tables, model-instance rows first.
    pub vertex_paint: Vec<VertexPaint>,
    pub model_ids: Vec<u64>,
    /// Model paths parallel to `model_ids`, read from the zone's own string
    /// pool and kept **exactly as stored** (mixed case, `\` separators), because
    /// their ids are the CRC-64 of that spelling. Use
    /// [`Zone::model_display_path`] for anything user-facing.
    pub model_names: Vec<String>,
    /// Asset ids of Zone Actor Asset Paths, parallel to `actor_asset_paths`.
    pub actor_asset_ids: Vec<u64>,
    /// Actor asset paths, as stored; see `model_names`.
    pub actor_asset_paths: Vec<String>,
    /// Decal heap offset -> its mesh span inside block 2.
    pub decal_geometry: HashMap<u32, GeoRef>,

    // -- the script graph --------------------------------------------------
    pub actions: Vec<ScriptAction>,
    pub plugs: Vec<ScriptPlug>,
    pub vars: Vec<ScriptVar>,
    pub script_types: Vec<String>,
    /// Original string-pool offsets for `script_types`, preserved verbatim.
    script_string_offsets: Vec<u32>,

    // -- prius (DDL) blobs and the outbound reference table ----------------
    pub actor_prius_index: Vec<ActorPriusEntry>,
    pub asset_refs: Vec<AssetRef>,
    pub script_priuses: Vec<PriusBlob>,
    pub actor_priuses: Vec<PriusBlob>,
    /// Blob id backing each `actor_prius_index` entry; entries can share one.
    pub actor_prius_blob: Vec<usize>,

    /// Applied equal-length patches, newest last.
    pub edits: Vec<ZoneEdit>,
}

impl Zone {
    /// The script-action rows. The field is [`Zone::actions`]; this is the
    /// longer spelling the Level Editor commands were written against.
    pub fn script_actions(&self) -> &[ScriptAction] {
        &self.actions
    }
}

/// Read a "struct of arrays" name table: `{ u64 id[n] }{ u32 string_offset[n] }`.
///
/// Used by Zone Model Names, Zone Actor Asset Paths and Zone Visual Effect
/// Names. Both sides of this merge measured the same layout independently; it
/// is *not* a flat `u64[]` followed by nothing, and reading it as one makes
/// every path come out empty. The strings are returned exactly as stored, since
/// the id beside each one is the CRC-64 of that spelling.
fn read_soa_table(dat1: &Dat1, tag: u32) -> (Vec<u64>, Vec<String>) {
    let Some(sec) = dat1.get_section_data(tag) else {
        return (Vec::new(), Vec::new());
    };
    let n = sec.len() / 12;
    let mut ids = Vec::with_capacity(n);
    let mut paths = Vec::with_capacity(n);
    for i in 0..n {
        ids.push(LE::read_u64(&sec[i * 8..i * 8 + 8]));
        let so = LE::read_u32(&sec[n * 8 + i * 4..n * 8 + i * 4 + 4]);
        paths.push(dat1.get_string(so).unwrap_or_default());
    }
    (ids, paths)
}

/// The first byte past the last entry of a run of heap offsets.
///
/// Every entry's length comes from its own `+0x7C` word, so a run holding an
/// entry with extra records ends later than `offset + MODEL_INST_LEN` would
/// say. `None` when an offset does not address an entry that fits.
fn heap_run_end(scene: &[u8], offsets: &[u32]) -> Option<usize> {
    let mut end = 0usize;
    for &o in offsets {
        end = end.max(o as usize + Zone::entry_len(scene, o as usize)?);
    }
    Some(end)
}

/// A stored asset path, as the UI and the hash lookups want it.
pub fn display_path(stored: &str) -> String {
    stored.replace('\\', "/")
}

impl Zone {
    /// Parse an asset as extracted by `Toc::extract_asset` (with or without the
    /// 36-byte asset header prepended).
    pub fn parse(asset: &[u8]) -> Result<Self> {
        let (header, body) = split_header(asset)?;
        let all_sizes = match &header {
            Some(h) => {
                let mut s = [0u32; 8];
                for (i, v) in s.iter_mut().enumerate() {
                    *v = LE::read_u32(&h[4 + i * 4..8 + i * 4]);
                }
                s
            }
            None => [body.len() as u32, 0, 0, 0, 0, 0, 0, 0],
        };
        let mut sizes = [0u32; 4];
        sizes.copy_from_slice(&all_sizes[..4]);

        let block0_len = if all_sizes[0] as usize <= body.len() && all_sizes[0] > 0 {
            all_sizes[0] as usize
        } else {
            body.len()
        };
        let block_spans = split_blocks(&all_sizes, body.len(), block0_len);
        let dat1 = Dat1::parse(&body[..block0_len])?;
        if dat1.unk1 != ZONE_TYPE_ID {
            return Err(ToolkitError::Parse(format!(
                "not a zone DAT1 (type {:#010x})",
                dat1.unk1
            )));
        }
        let tail = body[block0_len..].to_vec();

        let mut zone = Zone {
            header,
            sizes,
            all_sizes,
            dat1,
            tail,
            block_spans,
            scene_objects: Vec::new(),
            actors: Vec::new(),
            actor_names: Vec::new(),
            actor_groups: Vec::new(),
            vertex_paint: Vec::new(),
            model_ids: Vec::new(),
            model_names: Vec::new(),
            actor_asset_ids: Vec::new(),
            actor_asset_paths: Vec::new(),
            decal_geometry: HashMap::new(),
            actions: Vec::new(),
            plugs: Vec::new(),
            vars: Vec::new(),
            script_types: Vec::new(),
            script_string_offsets: Vec::new(),
            actor_prius_index: Vec::new(),
            asset_refs: Vec::new(),
            script_priuses: Vec::new(),
            actor_priuses: Vec::new(),
            actor_prius_blob: Vec::new(),
            edits: Vec::new(),
        };
        zone.build_index();
        zone.read_script_graph();
        zone.script_priuses = zone.collect_script_priuses()?;
        let (blobs, mapping) = zone.collect_actor_priuses()?;
        zone.actor_priuses = blobs;
        zone.actor_prius_blob = mapping;
        Ok(zone)
    }

    /// The fixed-size script-graph tables and the outbound reference table.
    fn read_script_graph(&mut self) {
        self.plugs = slice_records(&self.dat1, TAG_SCRIPT_PLUGS, PLUG_LEN, |b| ScriptPlug {
            name_hash: LE::read_u32(&b[0..4]),
            index: LE::read_u32(&b[4..8]),
            other_hash: LE::read_u32(&b[8..12]),
        });
        self.vars = slice_records(&self.dat1, TAG_SCRIPT_VARS, VAR_LEN, ScriptVar::read);
        self.script_types = string_table(&self.dat1, TAG_SCRIPT_STRINGS);
        self.script_string_offsets = u32_table(&self.dat1, TAG_SCRIPT_STRINGS);
        self.actor_prius_index = slice_records(
            &self.dat1,
            TAG_ACTOR_PRIUSES,
            ACTOR_PRIUS_LEN,
            ActorPriusEntry::read,
        );
        self.asset_refs = slice_records(&self.dat1, TAG_ZONE_ASSET_REFS, ASSET_REF_LEN, |b| AssetRef {
            asset_id: LE::read_u64(&b[0..8]),
            name_offset: LE::read_u32(&b[8..12]),
            ext_hash: LE::read_u32(&b[12..16]),
        });
    }

    /// The exact byte length of the scene-object entry at `off`.
    ///
    /// The low 24 bits of the word at `+0x7C` are the entry's own length. This
    /// is what makes the section a heap rather than an array: a kind-0 entry is
    /// `0x140` bytes plus `0x20` for each of the `+0x112` extra records.
    /// `None` when the field is absent or does not describe an entry that fits.
    pub fn entry_len(scene: &[u8], off: usize) -> Option<usize> {
        let w = scene.get(off + OFF_SIZE..off + OFF_SIZE + 4)?;
        let len = (LE::read_u32(w) & 0x00FF_FFFF) as usize;
        (len >= ENTRY_HEADER_LEN && off + len <= scene.len()).then_some(len)
    }

    /// The model path at `slot`, with `/` separators, for display and for the
    /// normalised hash lookups.
    pub fn model_display_path(&self, slot: usize) -> String {
        self.model_names.get(slot).map(|p| display_path(p)).unwrap_or_default()
    }

    fn build_index(&mut self) {
        // Zone Scene Objects is OPTIONAL. The shipped game has 2.5 MB `_art`
        // zones with no heap section at all (see `core::level`), and this used
        // to `return` on them -- which skipped not just the heap walk but every
        // plain section table read below it: Zone Model Names, Zone Actors,
        // Zone Actor Groups, Zone Script Actions and the vertex paint tables.
        //
        // That was a silent data-destroying bug, because `save` re-emits those
        // sections from these vectors and `put` only declines to write an empty
        // body when the section is *absent*. A zone with a script but no heap
        // would parse to zero actions and then be written back with its Zone
        // Script Actions and Zone Script Priuses bodies zeroed, leaving every
        // surviving plug pointing at rows that no longer existed.
        //
        // An absent heap is now an empty one: the two places that index into it
        // (the decal-geometry mapping and the heap walk) are already bounds
        // checked and yield nothing, while every table below is read as usual.
        let scene = self
            .dat1
            .get_section_data(TAG_SCENE_OBJECTS)
            .map(<[u8]>::to_vec)
            .unwrap_or_default();

        // Zone Model Names is { u64 id[n] }{ u32 string_offset[n] }, not a flat u64 array.
        let (ids, paths) = read_soa_table(&self.dat1, TAG_MODEL_NAMES);
        self.model_ids = ids;
        self.model_names = paths;

        let mut offsets: Vec<u32> = Vec::new();
        for tag in [
            TAG_MODEL_INSTS,
            TAG_LIGHTS,
            TAG_VOLUMES,
            TAG_DECALS,
            TAG_VFX_INSTS,
            // Without this table every kind-6 entry is invisible, and an
            // impostor zone (645 of them ship, 456 on megalopolis alone)
            // parses to zero scene objects.
            TAG_IMPOSTORS,
        ] {
            if let Some(sec) = self.dat1.get_section_data(tag) {
                offsets.extend(sec.chunks_exact(4).map(LE::read_u32));
            }
        }

        // Zone Decal Geometry rows are addressed by the kind-3 entry itself:
        // `entry + OFF_DECAL_ROW` holds the row index, or `u32::MAX` when the
        // decal projects no mesh at all.
        //
        // It is NOT parallel to Zone Decals. That looks true on any zone where
        // every decal has geometry (`tile_n19_art`, 8 of 8), and false the
        // moment one does not: over 108 shipped zones from every planet, 416 of
        // 4818 kind-3 entries carry `u32::MAX` here, and each one shifts every
        // later decal off its row. Measured on the positional mapping, only
        // 1340 of 1935 decals sampled had their own mesh inside their own row's
        // bounding sphere (worst case 1745 radii away — a neighbour's mesh
        // hoisted onto the wrong transform); reading the index makes it 4000 of
        // 4008, the 8 stragglers overshooting by 1.09-7.81 r. The field is
        // exact: it agrees with the row every one of 2186 sampled decals was
        // matched to by sphere, never names a row twice inside a zone, and
        // leaves no row unclaimed.
        let mut decal_geo: HashMap<u32, GeoRef> = HashMap::new();
        if let (Some(list), Some(geo)) = (
            self.dat1.get_section_data(TAG_DECALS).map(<[u8]>::to_vec),
            self.dat1.get_section_data(TAG_DECAL_GEOMETRY).map(<[u8]>::to_vec),
        ) {
            const ROW: usize = 88;
            for off in list.chunks_exact(4).map(LE::read_u32) {
                let row = match scene.get(off as usize + OFF_DECAL_ROW..off as usize + OFF_DECAL_ROW + 4) {
                    Some(b) => LE::read_u32(b) as usize,
                    None => continue,
                };
                let base = row * ROW;
                if base + ROW > geo.len() {
                    continue; // u32::MAX: this decal projects no mesh
                }
                let vertex_count = LE::read_u16(&geo[base..base + 2]) as u32;
                let index_count = LE::read_u16(&geo[base + 2..base + 4]) as u32;
                let vertex_offset = LE::read_u32(&geo[base + 0x50..base + 0x54]);
                if vertex_count == 0 || index_count == 0 {
                    continue;
                }
                decal_geo.insert(
                    off,
                    GeoRef {
                        vertex_offset,
                        vertex_count,
                        // The index run follows the vertex run immediately.
                        index_offset: vertex_offset + vertex_count * 16,
                        index_count,
                    },
                );
            }
        }
        self.decal_geometry = decal_geo;

        // Zone Actors: 32-byte `ActorBuilt` rows, see [`ActorRow`].
        let raw_rows: Vec<[u8; 32]> = self
            .dat1
            .get_section_data(TAG_ACTORS)
            .map(|a| {
                a.chunks_exact(ACTOR_LEN)
                    .map(|r| {
                        let mut b = [0u8; ACTOR_LEN];
                        b.copy_from_slice(r);
                        b
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Zone Actor Names is a plain u32[] of string offsets.
        let name_offsets: Vec<u32> = self
            .dat1
            .get_section_data(TAG_ACTOR_NAMES)
            .map(|n| n.chunks_exact(4).map(LE::read_u32).collect())
            .unwrap_or_default();
        // Zone Actor Asset Paths is a struct-of-arrays table like Zone Model Names.
        let (actor_ids, actor_paths) = read_soa_table(&self.dat1, TAG_ACTOR_ASSETS);
        self.actor_names = name_offsets
            .iter()
            .map(|&o| self.dat1.get_string(o).unwrap_or_default())
            .collect();
        self.actor_asset_ids = actor_ids.clone();
        self.actor_asset_paths = actor_paths.clone();

        for (i, r) in raw_rows.iter().enumerate() {
            let name_index = LE::read_u32(&r[0..4]);
            let asset_index = LE::read_u32(&r[4..8]);
            let scene_offset = LE::read_u32(&r[8..12]);
            let prius_index = LE::read_i32(&r[12..16]);
            let prius_count = LE::read_i32(&r[16..20]);
            let flags = LE::read_u16(&r[20..22]);
            let pad = LE::read_u16(&r[22..24]);
            let instance_id = LE::read_u64(&r[24..32]);

            let name = name_offsets
                .get(name_index as usize)
                .and_then(|&o| self.dat1.get_string(o))
                .unwrap_or_default();
            let ai = asset_index as usize;
            let asset_id = actor_ids.get(ai).copied().unwrap_or(0);
            let asset_path = actor_paths
                .get(ai)
                .map(|p| display_path(p))
                .unwrap_or_default();

            self.actors.push(ActorRow {
                index: i as u32,
                name_index,
                asset_index,
                scene_offset,
                prius_index,
                prius_count,
                flags,
                pad,
                flag_names: actor_flag_names(flags),
                instance_id,
                is_placed: is_placed(instance_id),
                name,
                asset_id,
                asset_path,
            });
            offsets.push(scene_offset);
        }

        self.read_actor_groups();
        self.read_script_actions();
        self.read_vertex_paint();

        offsets.sort_unstable();
        offsets.dedup();

        for off in offsets {
            let o = off as usize;
            if o + ENTRY_HEADER_LEN > scene.len() {
                continue;
            }
            let mut obj = self.read_object(&scene, o);
            if let Some(ai) = self.actors.iter().position(|a| a.scene_offset == off) {
                obj.actor_index = ai as i32;
                obj.name = self.actors[ai].name.clone();
            }
            self.scene_objects.push(obj);
        }
    }

    /// Zone Actor Groups + Group Data + Group Names.
    fn read_actor_groups(&mut self) {
        let Some(rows) = self.dat1.get_section_data(TAG_ACTOR_GROUPS).map(<[u8]>::to_vec) else {
            return;
        };
        let members: Vec<u16> = self
            .dat1
            .get_section_data(TAG_ACTOR_GROUP_MEMBERS)
            .map(|d| d.chunks_exact(2).map(LE::read_u16).collect())
            .unwrap_or_default();
        let name_offsets: Vec<u32> = self
            .dat1
            .get_section_data(TAG_ACTOR_GROUP_NAMES)
            .map(|n| n.chunks_exact(4).map(LE::read_u32).collect())
            .unwrap_or_default();

        for (i, r) in rows.chunks_exact(GROUP_LEN).enumerate() {
            let name_index = LE::read_i32(&r[0..4]);
            let first_actor_index = LE::read_i32(&r[4..8]);
            let actor_count = LE::read_u16(&r[8..10]);
            let is_global = LE::read_u16(&r[10..12]) != 0;
            let debug_color = LE::read_u32(&r[12..16]);
            let id = LE::read_u64(&r[16..24]);

            let name = usize::try_from(name_index)
                .ok()
                .and_then(|n| name_offsets.get(n))
                .and_then(|&o| self.dat1.get_string(o))
                .unwrap_or_default();
            let start = usize::try_from(first_actor_index).unwrap_or(usize::MAX);
            let slice = start
                .checked_add(actor_count as usize)
                .and_then(|end| members.get(start..end))
                .unwrap_or(&[]);

            self.actor_groups.push(ActorGroup {
                index: i as u32,
                name_index,
                name,
                first_actor_index,
                actor_count,
                is_global,
                debug_color,
                id,
                is_placed: is_placed(id),
                members: slice.to_vec(),
            });
        }
    }

    /// Zone Script Actions, 40-byte rows.
    fn read_script_actions(&mut self) {
        let Some(rows) = self.dat1.get_section_data(TAG_SCRIPT_ACTIONS).map(<[u8]>::to_vec) else {
            return;
        };
        for (i, r) in rows.chunks_exact(ACTION_LEN).enumerate() {
            self.actions.push(ScriptAction::read(i as u32, r));
        }
    }

    /// Both Vertex Paint Info tables, 16-byte rows.
    fn read_vertex_paint(&mut self) {
        for (tag, actor) in [
            (TAG_VERTEX_PAINT_INFO, false),
            (TAG_ACTOR_VERTEX_PAINT_INFO, true),
        ] {
            let Some(rows) = self.dat1.get_section_data(tag).map(<[u8]>::to_vec) else {
                continue;
            };
            for r in rows.chunks_exact(16) {
                self.vertex_paint.push(VertexPaint {
                    uid: LE::read_u64(&r[0..8]),
                    count: LE::read_i32(&r[8..12]),
                    data_offset: LE::read_u32(&r[12..16]),
                    actor,
                });
            }
        }
    }

    fn read_object(&self, scene: &[u8], off: usize) -> SceneObject {
        let flags = LE::read_u32(&scene[off + OFF_FLAGS..off + OFF_FLAGS + 4]);
        let kind = (flags >> 24) as u8;
        let mut size = LE::read_u32(&scene[off + OFF_SIZE..off + OFF_SIZE + 4]) & 0x00FF_FFFF;
        if (size as usize) < ENTRY_HEADER_LEN || off + size as usize > scene.len() {
            size = ENTRY_HEADER_LEN as u32;
        }

        let mut matrix = [0f32; 16];
        LE::read_f32_into(&scene[off + OFF_MATRIX..off + OFF_MATRIX + 64], &mut matrix);
        let mut scale = [0f32; 3];
        LE::read_f32_into(&scene[off + OFF_SCALE..off + OFF_SCALE + 12], &mut scale);

        let mut instance_id = 0u64;
        let mut model_index = -1i32;
        let mut model_id = 0u64;
        let mut model_path = String::new();
        let mut extra_count = 0u16;
        let end = off + size as usize;

        if kind == KIND_MODEL {
            if off + OFF_INSTANCE_K0 + 8 <= end {
                instance_id = LE::read_u64(&scene[off + OFF_INSTANCE_K0..off + OFF_INSTANCE_K0 + 8]);
            }
            // +0x110 is a u16 index and +0x112 a u16 count of trailing 0x20-byte
            // records (entry size == 0x140 + 0x20 * extra_count). Reading the pair
            // as one u32 makes every entry with extras look out of range.
            if off + OFF_MODEL_INDEX + 4 <= end {
                let mi = LE::read_u16(&scene[off + OFF_MODEL_INDEX..off + OFF_MODEL_INDEX + 2]);
                extra_count = LE::read_u16(&scene[off + OFF_MODEL_INDEX + 2..off + OFF_MODEL_INDEX + 4]);
                if let Some(&id) = self.model_ids.get(mi as usize) {
                    model_index = mi as i32;
                    model_id = id;
                    model_path = self.model_display_path(mi as usize);
                }
            }
        } else if kind != KIND_DECAL && off + OFF_INSTANCE_ID + 8 <= end {
            // A kind-3 entry starts its inline `.material` path at +0x80, so the
            // eight bytes there are the first characters of that string, not an
            // id — every decal used to report instance
            // `0x6C6169726574616D` ("material" little-endian). Leave it zero.
            instance_id = LE::read_u64(&scene[off + OFF_INSTANCE_ID..off + OFF_INSTANCE_ID + 8]);
        }

        let mut asset_path = String::new();
        let mut geometry = None;
        match kind {
            KIND_DECAL => {
                asset_path = read_cstr(scene, off + OFF_INLINE_PATH_DECAL, end);
                geometry = self.decal_geometry.get(&(off as u32)).copied();
            }
            KIND_IMPOSTOR => {
                asset_path = read_cstr(scene, off + OFF_INLINE_PATH_IMPOSTOR, end);
                if off + OFF_IMPOSTOR_GEO + 16 <= end {
                    let g = off + OFF_IMPOSTOR_GEO;
                    let r = GeoRef {
                        vertex_offset: LE::read_u32(&scene[g..g + 4]),
                        vertex_count: LE::read_u32(&scene[g + 4..g + 8]),
                        index_offset: LE::read_u32(&scene[g + 8..g + 12]),
                        index_count: LE::read_u32(&scene[g + 12..g + 16]),
                    };
                    if r.vertex_count > 0 && r.index_count >= 3 {
                        geometry = Some(r);
                    }
                }
            }
            _ => {}
        }

        SceneObject {
            offset: off as u32,
            kind,
            kind_name: kind_name(kind).to_string(),
            size,
            flags,
            matrix,
            scale,
            instance_id,
            is_placed: is_placed(instance_id),
            model_index,
            model_id,
            model_path,
            extra_count,
            name: String::new(),
            actor_index: -1,
            asset_path,
            geometry,
        }
    }

    /// One of the asset's raw blocks. Block 0 is the DAT1 in [`Zone::dat1`], so
    /// only `i >= 1` ever resolves; block 2 is the geometry appendix.
    pub fn block(&self, i: usize) -> Option<&[u8]> {
        let (start, len) = *self.block_spans.get(i.checked_sub(1)?)?;
        self.tail.get(start..start + len)
    }

    // -- queries ---------------------------------------------------------------------------

    pub fn model_instances(&self) -> impl Iterator<Item = &SceneObject> {
        self.scene_objects
            .iter()
            .filter(|o| o.kind == KIND_MODEL && o.model_id != 0)
    }

    pub fn entities(&self) -> impl Iterator<Item = &SceneObject> {
        self.scene_objects.iter().filter(|o| o.kind != KIND_MODEL)
    }

    /// Axis-aligned bounds over every scene object position.
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let mut it = self.scene_objects.iter();
        let first = it.next()?;
        let mut min = first.position();
        let mut max = min;
        for o in it {
            let p = o.position();
            for i in 0..3 {
                min[i] = min[i].min(p[i]);
                max[i] = max[i].max(p[i]);
            }
        }
        Some((min, max))
    }

    pub fn find(&self, offset: u32) -> Option<&SceneObject> {
        self.scene_objects.iter().find(|o| o.offset == offset)
    }

    /// The colour store a [`VertexPaint`] row addresses: `count` little-endian
    /// `u32`s starting at `data_offset`, one per vertex of the painted model.
    pub fn vertex_paint_colors(&self, actor: bool) -> Option<&[u8]> {
        self.dat1.get_section_data(if actor {
            TAG_ACTOR_VERTEX_PAINT_COLOR
        } else {
            TAG_VERTEX_PAINT_COLOR
        })
    }

    // -- editing ---------------------------------------------------------------------------

    /// Patch a scene object's 4x4 transform (row-major, row 3 = translation).
    pub fn set_matrix(&mut self, offset: u32, matrix: [f32; 16]) -> Result<()> {
        let mut buf = [0u8; 64];
        LE::write_f32_into(&matrix, &mut buf);
        self.patch(offset, OFF_MATRIX, &buf, "transform")?;
        if let Some(o) = self.scene_objects.iter_mut().find(|o| o.offset == offset) {
            o.matrix = matrix;
        }
        Ok(())
    }

    /// Patch a scene object's translation only.
    pub fn set_position(&mut self, offset: u32, pos: [f32; 3]) -> Result<()> {
        let obj = self
            .find(offset)
            .ok_or_else(|| ToolkitError::Parse(format!("no scene object at {offset}")))?;
        let mut m = obj.matrix;
        m[12] = pos[0];
        m[13] = pos[1];
        m[14] = pos[2];
        self.set_matrix(offset, m)
    }

    /// Patch a scene object's scale triple.
    pub fn set_scale(&mut self, offset: u32, scale: [f32; 3]) -> Result<()> {
        let mut buf = [0u8; 12];
        LE::write_f32_into(&scale, &mut buf);
        self.patch(offset, OFF_SCALE, &buf, "scale")?;
        if let Some(o) = self.scene_objects.iter_mut().find(|o| o.offset == offset) {
            o.scale = scale;
        }
        Ok(())
    }

    fn patch(&mut self, obj_offset: u32, field: usize, bytes: &[u8], what: &str) -> Result<()> {
        let mut scene = self
            .dat1
            .get_section_data(TAG_SCENE_OBJECTS)
            .ok_or(ToolkitError::SectionNotFound(TAG_SCENE_OBJECTS))?
            .to_vec();
        let at = obj_offset as usize + field;
        if at + bytes.len() > scene.len() {
            return Err(ToolkitError::Parse(format!(
                "patch at {at} exceeds scene section ({})",
                scene.len()
            )));
        }
        let old = scene[at..at + bytes.len()].to_vec();
        if old == bytes {
            return Ok(());
        }
        scene[at..at + bytes.len()].copy_from_slice(bytes);
        self.dat1.set_section_data(TAG_SCENE_OBJECTS, scene)?;
        self.edits.push(ZoneEdit {
            section: TAG_SCENE_OBJECTS,
            offset: at,
            old,
            new: bytes.to_vec(),
            what: format!("{what} @{obj_offset}"),
        });
        Ok(())
    }

    /// Revert the most recent patch. Returns its description.
    pub fn undo(&mut self) -> Option<String> {
        let e = self.edits.pop()?;
        if let Some(sec) = self.dat1.get_section_data(e.section) {
            let mut scene = sec.to_vec();
            scene[e.offset..e.offset + e.old.len()].copy_from_slice(&e.old);
            let _ = self.dat1.set_section_data(e.section, scene);
        }
        let refreshed = self
            .dat1
            .get_section_data(TAG_SCENE_OBJECTS)
            .map(|s| s.to_vec())
            .unwrap_or_default();
        for i in 0..self.scene_objects.len() {
            let off = self.scene_objects[i].offset as usize;
            if off + ENTRY_HEADER_LEN <= refreshed.len() {
                let fresh = self.read_object(&refreshed, off);
                self.scene_objects[i].matrix = fresh.matrix;
                self.scene_objects[i].scale = fresh.scale;
            }
        }
        Some(e.what)
    }

    pub fn is_dirty(&self) -> bool {
        !self.edits.is_empty()
    }

    // -- serialisation ---------------------------------------------------------------------

    /// The full asset: 36-byte header (sizes fixed up), block 0, then every
    /// trailing block unchanged.
    pub fn to_bytes(&mut self) -> Vec<u8> {
        let block0 = self.dat1.save();
        let mut out = Vec::with_capacity(ASSET_HEADER_LEN + block0.len() + self.tail.len());
        if let Some(h) = &self.header {
            let mut hdr = h.clone();
            LE::write_u32(&mut hdr[4..8], block0.len() as u32);
            out.extend_from_slice(&hdr);
        }
        out.extend_from_slice(&block0);
        out.extend_from_slice(&self.tail);
        out
    }

    /// Asset bytes without the 36-byte TOC header (what goes into an archive).
    pub fn to_asset_bytes(&mut self) -> Vec<u8> {
        let all = self.to_bytes();
        if self.header.is_some() {
            all[ASSET_HEADER_LEN..].to_vec()
        } else {
            all
        }
    }
}

// -------------------------------------------------------------------------
// The script graph, the prius blobs and the writing path.
// -------------------------------------------------------------------------

impl Zone {
    /// Node type label for an action, resolved through the script string table.
    pub fn action_type(&self, action: &ScriptAction) -> &str {
        self.script_types
            .get(action.type_index as usize)
            .map(String::as_str)
            .unwrap_or("<unknown>")
    }

    fn collect_script_priuses(&self) -> Result<Vec<PriusBlob>> {
        let section = self
            .dat1
            .get_section_data(TAG_SCRIPT_PRIUSES)
            .unwrap_or(&[])
            .to_vec();

        let mut by_key: BTreeMap<(u32, u32), Vec<usize>> = BTreeMap::new();
        for (i, a) in self.actions.iter().enumerate() {
            if a.prius_size == 0 {
                continue;
            }
            by_key.entry((a.prius_offset, a.prius_size)).or_default().push(i);
        }

        let mut out = Vec::with_capacity(by_key.len());
        for ((offset, size), owners) in by_key {
            let (json, error) = match parse_blob(&section, offset, size, &self.dat1) {
                Ok(v) => (v, None),
                Err(e) => (Value::Null, Some(e.to_string())),
            };
            out.push(PriusBlob {
                offset,
                size,
                json,
                owners,
                raw: raw_slice(&section, offset, size),
                dirty: false,
                error,
            });
        }
        Ok(out)
    }

    /// Index entries with identical data share one blob in the pool, so the
    /// blobs are deduplicated by (offset, size) exactly like script priuses.
    fn collect_actor_priuses(&self) -> Result<(Vec<PriusBlob>, Vec<usize>)> {
        let base = self.section_offset(TAG_ACTOR_PRIUS_DATA).unwrap_or(0);
        let section = self
            .dat1
            .get_section_data(TAG_ACTOR_PRIUS_DATA)
            .unwrap_or(&[])
            .to_vec();

        let mut by_key: BTreeMap<(u32, u32), Vec<usize>> = BTreeMap::new();
        for (i, e) in self.actor_prius_index.iter().enumerate() {
            let local = e.data_offset.saturating_sub(base);
            by_key.entry((local, e.data_size)).or_default().push(i);
        }

        let mut blobs = Vec::with_capacity(by_key.len());
        let mut mapping = vec![0usize; self.actor_prius_index.len()];
        for ((offset, size), owners) in by_key {
            let (json, error) = match parse_blob(&section, offset, size, &self.dat1) {
                Ok(v) => (v, None),
                Err(e) => (Value::Null, Some(e.to_string())),
            };
            for o in &owners {
                mapping[*o] = blobs.len();
            }
            blobs.push(PriusBlob {
                offset,
                size,
                json,
                owners,
                raw: raw_slice(&section, offset, size),
                dirty: false,
                error,
            });
        }
        Ok((blobs, mapping))
    }

    fn section_offset(&self, tag: u32) -> Option<u32> {
        self.dat1.sections.iter().find(|s| s.tag == tag).map(|s| s.offset)
    }

    /// Actor instances that use `asset_index`.
    pub fn instances_of_asset(&self, asset_index: u32) -> Vec<&str> {
        self.actors
            .iter()
            .filter(|a| a.asset_index == asset_index)
            .filter_map(|a| self.actor_names.get(a.name_index as usize).map(String::as_str))
            .collect()
    }

    /// Actor prius index entries attached to `actor`.
    pub fn priuses_of_actor(&self, actor: &ZoneActor) -> Vec<usize> {
        if !actor.has_priuses() {
            return Vec::new();
        }
        let start = actor.prius_index as usize;
        (start..start + actor.prius_count as usize)
            .filter(|i| *i < self.actor_prius_index.len())
            .collect()
    }

    /// Name of a script variable, or `None` for literal constants.
    /// The text a string var (type 4) holds.
    pub fn string_value(&self, index: usize) -> Option<&str> {
        let v = self.vars.get(index)?;
        if v.value_type != ScriptVar::TYPE_STRING || v.name_index2 == u16::MAX {
            return None;
        }
        self.script_types.get(v.name_index2 as usize).map(String::as_str)
    }

    pub fn var_name(&self, index: usize) -> Option<&str> {
        let v = self.vars.get(index)?;
        for idx in [v.name_index, v.name_index2, v.name_index3] {
            if idx != u16::MAX {
                if let Some(s) = self.script_types.get(idx as usize) {
                    if !s.is_empty() {
                        return Some(s);
                    }
                }
            }
        }
        None
    }

    /// The param plug on `action` named `name`, if it has one.
    pub fn param(&self, action: &ScriptAction, name: &str) -> Option<&ScriptPlug> {
        let hash = crc32::hash(name);
        self.plugs
            .get(action.param_range())?
            .iter()
            .find(|p| p.name_hash == hash)
    }

    /// Node index that writes `var_index`, i.e. binds it with direction `In`.
    pub fn var_writer(&self, var_index: usize) -> Option<usize> {
        let write = crc32::hash("In");
        self.actions.iter().position(|a| {
            self.plugs
                .get(a.param_range())
                .is_some_and(|ps| {
                    ps.iter()
                        .any(|p| p.index as usize == var_index && p.other_hash == write)
                })
        })
    }

    /// Split the flat plug array into per-node lists so nodes can be added or
    /// rewired without hand-maintaining every start index.
    pub fn node_plugs(&self) -> Vec<NodePlugs> {
        self.actions
            .iter()
            .map(|a| NodePlugs {
                links: self.plugs.get(a.link_range()).unwrap_or(&[]).to_vec(),
                params: self.plugs.get(a.param_range()).unwrap_or(&[]).to_vec(),
            })
            .collect()
    }

    /// Flatten per-node plug lists back into the packed array, restoring the
    /// start/count fields and the cached in-degree on every node.
    pub fn set_node_plugs(&mut self, per_node: &[NodePlugs]) {
        let mut flat: Vec<ScriptPlug> = Vec::new();
        for (i, np) in per_node.iter().enumerate() {
            let Some(action) = self.actions.get_mut(i) else {
                continue;
            };
            let start = flat.len();
            action.link_start = if np.links.is_empty() {
                u16::MAX
            } else {
                start as u16
            };
            action.link_count = np.links.len() as u16;
            flat.extend_from_slice(&np.links);

            let pstart = flat.len();
            action.param_start = if np.params.is_empty() {
                u16::MAX
            } else {
                pstart as u16
            };
            action.param_count = np.params.len() as u16;
            flat.extend_from_slice(&np.params);
        }

        let mut in_degree = vec![0u16; self.actions.len()];
        for np in per_node {
            for p in &np.links {
                if let Some(slot) = in_degree.get_mut(p.index as usize) {
                    *slot += 1;
                }
            }
        }
        for (a, d) in self.actions.iter_mut().zip(in_degree) {
            a.in_degree = d;
        }
        self.plugs = flat;
    }

    /// Intern a script-graph string, returning its index in the type/name table.
    pub fn intern_script_string(&mut self, s: &str) -> u16 {
        if let Some(i) = self.script_types.iter().position(|x| x == s) {
            return i as u16;
        }
        self.script_types.push(s.to_string());
        (self.script_types.len() - 1) as u16
    }

    /// Prius blob ids attached to `actor`.
    pub fn prius_blobs_of_actor(&self, actor: &ZoneActor) -> Vec<usize> {
        self.priuses_of_actor(actor)
            .into_iter()
            .filter_map(|i| self.actor_prius_blob.get(i).copied())
            .collect()
    }

    // -----------------------------------------------------------------------
    // Writing
    // -----------------------------------------------------------------------

    /// Apply an edit set and re-emit the full `.zone` file.
    pub fn save(mut self, edits: &ZoneEdits) -> Result<Vec<u8>> {
        let old_paths = self.path_like_strings();

        // Copies first: the editor previews them the same way, so value edits
        // can address a copy's own vars and blobs by index.
        for report in self.apply_clones(&edits.clone_waves)? {
            if let Some(w) = &report.warning {
                eprintln!("[zone] clone_wave: {w}");
            }
        }
        self.apply_actor_asset_edits(edits)?;
        self.apply_model_name_edits(edits);
        self.apply_script_var_edits(edits)?;
        self.apply_prius_edits(edits)?;
        self.unhook_nodes(&edits.remove_messages);
        for m in &edits.wave_messages {
            self.add_wave_message(m)?;
        }
        if let Some(text) = edits.victory_text.as_deref().filter(|t| !t.trim().is_empty()) {
            self.set_victory_text(text, edits.victory_style)?;
        }
        // Heals zones written by earlier clone code, which could drop the flag.
        for a in &mut self.actions {
            a.node_id |= ID_FLAG;
        }
        for v in &mut self.vars {
            v.id |= ID_FLAG;
        }

        self.rebuild_script_priuses()?;
        self.rebuild_actor_priuses()?;

        let new_paths = self.path_like_strings();
        self.rebuild_asset_refs(&old_paths, &new_paths);

        self.rebuild_script_tables()?;

        let actions = self.serialize_actions();
        put(&mut self.dat1, TAG_SCRIPT_ACTIONS, actions)?;

        // Actor prius entries store DAT1-absolute offsets, so the section
        // layout has to be settled before they can be written.
        self.dat1.recalculate_section_headers();
        let base = self.section_offset(TAG_ACTOR_PRIUS_DATA).unwrap_or(0);
        let mut index = Vec::with_capacity(self.actor_prius_index.len() * ACTOR_PRIUS_LEN);
        for (i, e) in self.actor_prius_index.iter_mut().enumerate() {
            let blob = self
                .actor_prius_blob
                .get(i)
                .and_then(|b| self.actor_priuses.get(*b));
            if let Some(blob) = blob {
                e.data_offset = base + blob.offset;
                e.data_size = blob.size;
            }
            e.write(&mut index);
        }
        put(&mut self.dat1, TAG_ACTOR_PRIUSES, index)?;

        // [`Zone::to_bytes`] re-emits the 36-byte asset header with word 0 --
        // the length of block 0 -- fixed up and every trailing block byte for
        // byte. That is the same file this used to assemble from a two-size
        // wrapper, except that it also keeps `sizes[2..]`, which the zones with
        // a geometry appendix need.
        Ok(self.to_bytes())
    }

    /// Re-emit the plug, variable and script-string sections from the
    /// in-memory graph. Reproduces the shipped bytes when nothing was added.
    fn rebuild_script_tables(&mut self) -> Result<()> {
        let mut plugs = Vec::with_capacity(self.plugs.len() * PLUG_LEN);
        for p in &self.plugs {
            plugs.extend_from_slice(&p.name_hash.to_le_bytes());
            plugs.extend_from_slice(&p.index.to_le_bytes());
            plugs.extend_from_slice(&p.other_hash.to_le_bytes());
        }
        put(&mut self.dat1, TAG_SCRIPT_PLUGS, plugs)?;

        let mut vars = Vec::with_capacity(self.vars.len() * VAR_LEN);
        for v in &self.vars {
            v.write(&mut vars);
        }
        put(&mut self.dat1, TAG_SCRIPT_VARS, vars)?;

        // Shipped pools hold duplicate copies of some strings, so existing
        // entries keep their original offsets and only new names are interned.
        let names = std::mem::take(&mut self.script_types);
        let mut offsets = Vec::with_capacity(names.len() * 4);
        let mut hashes = Vec::with_capacity(names.len() * 4);
        for (i, n) in names.iter().enumerate() {
            let off = match self.script_string_offsets.get(i) {
                Some(o) => *o,
                None => intern(&mut self.dat1, n),
            };
            offsets.extend_from_slice(&off.to_le_bytes());
            hashes.extend_from_slice(&crc32::hash(n).to_le_bytes());
        }
        self.script_types = names;
        put(&mut self.dat1, TAG_SCRIPT_STRINGS, offsets)?;
        put(&mut self.dat1, TAG_SCRIPT_STRING_HASHES, hashes)
    }

    fn serialize_actions(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.actions.len() * ACTION_LEN);
        for a in &self.actions {
            a.write(&mut out);
        }
        out
    }

    fn apply_actor_asset_edits(&mut self, edits: &ZoneEdits) -> Result<()> {
        for (idx, path) in &edits.actor_assets {
            let i = *idx;
            if i >= self.actor_asset_paths.len() {
                return Err(ToolkitError::Parse(format!(
                    "actor asset index {i} out of range"
                )));
            }
            let path = path.trim();
            if path.is_empty() {
                return Err(ToolkitError::Parse(
                    "actor asset path cannot be empty".into(),
                ));
            }
            self.actor_asset_paths[i] = path.to_string();
            self.actor_asset_ids[i] = crc64::hash(path);
        }
        if edits.actor_assets.is_empty() {
            return Ok(());
        }

        let section = id_name_table(&mut self.dat1, &self.actor_asset_ids, &self.actor_asset_paths);
        self.dat1.set_section_data(TAG_ACTOR_ASSETS, section)
    }

    fn apply_model_name_edits(&mut self, edits: &ZoneEdits) {
        if edits.model_names.is_empty() {
            return;
        }
        for (idx, path) in &edits.model_names {
            if let Some(slot) = self.model_names.get_mut(*idx) {
                *slot = path.trim().to_string();
                self.model_ids[*idx] = crc64::hash(slot);
            }
        }
        let section = id_name_table(&mut self.dat1, &self.model_ids, &self.model_names);
        let _ = self.dat1.set_section_data(TAG_MODEL_NAMES, section);
    }

    /// Slot of `path` in Zone Model Names, adding it when new.
    pub fn model_slot(&mut self, path: &str) -> Result<usize> {
        let id = crc64::hash(path.trim());
        if let Some(i) = self.model_ids.iter().position(|&x| x == id) {
            return Ok(i);
        }
        self.add_models(&[path.to_string()])?;
        Ok(self.model_ids.iter().position(|&x| x == id).unwrap_or(0))
    }

    /// Adds the models Zone Model Names lacks. The table stays in id order, as shipped zones keep
    /// it, so existing instances are renumbered.
    pub fn add_models(&mut self, paths: &[String]) -> Result<()> {
        let old = self.model_ids.clone();
        let mut all: Vec<(u64, String)> = old.iter().copied().zip(std::mem::take(&mut self.model_names)).collect();
        let mut known: std::collections::HashSet<u64> = old.iter().copied().collect();
        for p in paths {
            let p = p.trim().replace('/', "\\");
            let id = crc64::hash(&p);
            if known.insert(id) {
                all.push((id, p));
            }
        }
        all.sort_by_key(|e| e.0);
        let slot_of: std::collections::HashMap<u64, u16> = all.iter().enumerate().map(|(i, e)| (e.0, i as u16)).collect();
        (self.model_ids, self.model_names) = all.into_iter().unzip();
        let names = id_name_table(&mut self.dat1, &self.model_ids, &self.model_names);
        self.dat1.set_section_data(TAG_MODEL_NAMES, names)?;

        let mut scene = self.dat1.get_section_data(TAG_SCENE_OBJECTS).map(|d| d.to_vec()).unwrap_or_default();
        // +0x110 is a u16 model index (0xFFFF when the entry names no model)
        // and +0x112 is the extra-record count, so only two bytes may move.
        for o in u32_table(&self.dat1, TAG_MODEL_INSTS) {
            let o = o as usize + OFF_MODEL_INDEX;
            if let Some(b) = scene.get_mut(o..o + 2) {
                let slot = u16::from_le_bytes([b[0], b[1]]);
                if slot == u16::MAX {
                    continue;
                }
                if let Some(new) = old.get(slot as usize).and_then(|id| slot_of.get(id)) {
                    b.copy_from_slice(&new.to_le_bytes());
                }
            }
        }
        put(&mut self.dat1, TAG_SCENE_OBJECTS, scene)
    }

    /// Adds static model instances, each cloned from the zone's first one. They go right after the
    /// last model instance, so the 320-byte records stay in one run as shipped zones have them.
    pub fn add_model_instances(&mut self, placements: &[ModelPlacement]) -> Result<()> {
        if placements.is_empty() {
            return Ok(());
        }
        let bad = |m: &str| ToolkitError::Parse(m.into());
        let mut insts = u32_table(&self.dat1, TAG_MODEL_INSTS);
        let mut scene = self.dat1.get_section_data(TAG_SCENE_OBJECTS).map(|d| d.to_vec()).unwrap_or_default();

        // Every offset here comes out of the heap's own length field
        // ([`Zone::entry_len`]). A kind-0 entry is MODEL_INST_LEN bytes *plus*
        // 0x20 for each of its `+0x112` extra shader-parameter records, so a
        // fixed stride would both clone a partial record and splice into the
        // middle of the last instance on any zone that has one.
        let record = |scene: &[u8], o: u32| {
            let off = o as usize;
            let len = Self::entry_len(scene, off)?;
            scene.get(off..off + len.min(MODEL_INST_LEN)).map(<[u8]>::to_vec)
        };
        let template = insts
            .first()
            .and_then(|&o| record(&scene, o))
            .filter(|r| r.len() == MODEL_INST_LEN)
            .ok_or_else(|| bad("the zone has no model instance to clone"))?;
        let at = heap_run_end(&scene, &insts)
            .ok_or_else(|| bad("Zone Model Insts points past Zone Scene Objects"))?;

        // Records behind the insertion point move, and only actors are known to point at them.
        let mut o = at;
        while o < scene.len() {
            let size = Self::entry_len(&scene, o)
                .ok_or_else(|| bad("a scene object behind the model instances has no usable length"))?;
            if !self.actors.iter().any(|a| a.scene_offset as usize == o) {
                return Err(bad("the zone has scene objects behind its model instances that cannot be moved"));
            }
            o += size;
        }

        // Same length guard as `template` above: `entry_len` only promises
        // ENTRY_HEADER_LEN (0x80) bytes, so an entry declaring less than a full
        // base record would make `id_of`/`uid_of` index past the end below.
        let existing: Vec<Vec<u8>> = insts
            .iter()
            .filter_map(|&o| record(&scene, o))
            .filter(|r| r.len() == MODEL_INST_LEN)
            .collect();
        let id_of = |r: &[u8]| u16::from_le_bytes([r[156], r[157]]);
        let uid_of = |r: &[u8]| u64::from_le_bytes(r[256..264].try_into().unwrap());
        let mut ids: std::collections::HashSet<u16> = existing.iter().map(|r| id_of(r)).collect();
        let mut uids: std::collections::HashSet<u64> = existing.iter().map(|r| uid_of(r)).collect();
        let (mut id, mut uid) = (id_of(&template), uid_of(&template));

        let mut added = Vec::with_capacity(placements.len() * MODEL_INST_LEN);
        for p in placements {
            if p.model >= self.model_ids.len() {
                return Err(bad("a model instance names a slot Zone Model Names does not have"));
            }
            if ids.len() >= 0xFFFF {
                return Err(bad("a zone holds at most 65535 model instances"));
            }
            loop {
                id = id.wrapping_add(1);
                if id != 0xFFFF && ids.insert(id) {
                    break;
                }
            }
            loop {
                uid = mix64(uid) | ID_FLAG;
                if uids.insert(uid) {
                    break;
                }
            }
            let [x, y, z] = p.axes;
            let scale = p.axes.map(|a| (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt());
            let det = x[0] * (y[1] * z[2] - y[2] * z[1]) - x[1] * (y[0] * z[2] - y[2] * z[0]) + x[2] * (y[0] * z[1] - y[1] * z[0]);
            let uniform = (scale[0] - scale[1]).abs() < 1e-4 && (scale[1] - scale[2]).abs() < 1e-4;
            // Static, plus uniform scale and mirrored; the two low bits follow no rule the matrix shows and stay 0.
            let flags = 0x100u32 | ((uniform as u32) << 4) | (((det < 0.0) as u32) << 2);

            // m_Flags at +128 stay the template's: bit 22 set is what drew in game, clearing it is untested.
            let mut r = template.clone();
            // The copy carries no extra records: MODEL_INST_LEN bytes long,
            // which the length word at +0x7C has to agree with (its high byte
            // is left as the template had it) and the count at +0x112 is
            // cleared below with the rest of that word.
            let len_word = (LE::read_u32(&template[OFF_SIZE..OFF_SIZE + 4]) & 0xFF00_0000)
                | MODEL_INST_LEN as u32;
            r[OFF_SIZE..OFF_SIZE + 4].copy_from_slice(&len_word.to_le_bytes());
            let mut floats = |o: usize, v: &[f32]| {
                for (k, f) in v.iter().enumerate() {
                    r[o + k * 4..o + k * 4 + 4].copy_from_slice(&f.to_le_bytes());
                }
            };
            floats(0, &[x[0], x[1], x[2], 0.0]);
            floats(16, &[y[0], y[1], y[2], 0.0]);
            floats(32, &[z[0], z[1], z[2], 0.0]);
            floats(48, &[p.position[0], p.position[1], p.position[2], 1.0]);
            floats(64, &p.bsphere);
            floats(80, &p.half_extents);
            floats(112, &scale);
            r[92..96].copy_from_slice(&flags.to_le_bytes());
            r[156..158].copy_from_slice(&id.to_le_bytes());
            r[159] = 0;
            r[256..264].copy_from_slice(&uid.to_le_bytes());
            r[264..272].fill(0);
            r[OFF_MODEL_INDEX..OFF_MODEL_INDEX + 2].copy_from_slice(&(p.model as u16).to_le_bytes());
            // +0x112: the clone carries no extra records.
            r[OFF_MODEL_INDEX + 2..OFF_MODEL_INDEX + 4].fill(0);
            r[308..312].copy_from_slice(&(-1i32).to_le_bytes());
            insts.push((at + added.len()) as u32);
            added.extend_from_slice(&r);
        }

        let grew = added.len() as u32;
        scene.splice(at..at, added);
        if let Some(mut actors) = self.dat1.get_section_data(TAG_ACTORS).map(|d| d.to_vec()) {
            for (a, rec) in self.actors.iter_mut().zip(actors.chunks_exact_mut(ACTOR_LEN)) {
                if a.scene_offset != u32::MAX && a.scene_offset as usize >= at {
                    a.scene_offset += grew;
                    rec[8..12].copy_from_slice(&a.scene_offset.to_le_bytes());
                }
            }
            self.dat1.set_section_data(TAG_ACTORS, actors)?;
        }
        self.dat1.set_section_data(TAG_MODEL_INSTS, insts.iter().flat_map(|o| o.to_le_bytes()).collect())?;
        self.dat1.set_section_data(TAG_SCENE_OBJECTS, scene)
    }

    fn apply_script_var_edits(&mut self, edits: &ZoneEdits) -> Result<()> {
        if edits.script_vars.is_empty()
            && edits.script_var_ids.is_empty()
            && edits.script_var_strings.is_empty()
        {
            return Ok(());
        }
        for (idx, text) in &edits.script_var_strings {
            if self.vars.get(*idx).map(|v| v.value_type) != Some(ScriptVar::TYPE_STRING) {
                return Err(ToolkitError::Parse(format!("script var {idx} is not a string")));
            }
            let s = self.intern_script_string(text);
            self.vars[*idx].name_index2 = s;
        }
        for (idx, value) in &edits.script_vars {
            let var = self.vars.get_mut(*idx).ok_or_else(|| {
                ToolkitError::Parse(format!("script var index {idx} out of range"))
            })?;
            var.set_number(*value)?;
        }
        for (idx, hex) in &edits.script_var_ids {
            let id = u64::from_str_radix(hex.trim_start_matches("0x"), 16).map_err(|e| {
                ToolkitError::Parse(format!("script var {idx}: invalid id {hex:?} — {e}"))
            })?;
            let var = self.vars.get_mut(*idx).ok_or_else(|| {
                ToolkitError::Parse(format!("script var index {idx} out of range"))
            })?;
            var.set_id(id);
        }
        let mut section = Vec::with_capacity(self.vars.len() * VAR_LEN);
        for v in &self.vars {
            v.write(&mut section);
        }
        self.dat1.set_section_data(TAG_SCRIPT_VARS, section)
    }

    fn apply_prius_edits(&mut self, edits: &ZoneEdits) -> Result<()> {
        replace_blobs(&mut self.script_priuses, &edits.script_priuses, "script")?;
        replace_blobs(&mut self.actor_priuses, &edits.actor_priuses, "actor")?;
        replace_blobs_from_text(
            &mut self.script_priuses,
            &edits.script_prius_json,
            "script",
        )?;
        replace_blobs_from_text(&mut self.actor_priuses, &edits.actor_prius_json, "actor")?;
        patch_blobs(&mut self.script_priuses, &edits.script_prius_patches, "script")?;
        patch_blobs(&mut self.actor_priuses, &edits.actor_prius_patches, "actor")?;
        Ok(())
    }

    fn rebuild_script_priuses(&mut self) -> Result<()> {
        let mut section: Vec<u8> = Vec::new();
        let blobs = std::mem::take(&mut self.script_priuses);
        let mut rebuilt = Vec::with_capacity(blobs.len());
        for blob in blobs {
            let bytes = self.blob_bytes(&blob)?;
            let offset = section.len() as u32;
            let size = bytes.len() as u32;
            section.extend_from_slice(&bytes);
            pad4(&mut section);
            for owner in &blob.owners {
                if let Some(a) = self.actions.get_mut(*owner) {
                    a.prius_offset = offset;
                    a.prius_size = size;
                }
            }
            rebuilt.push(PriusBlob { offset, size, raw: bytes, ..blob });
        }
        self.script_priuses = rebuilt;
        put(&mut self.dat1, TAG_SCRIPT_PRIUSES, section)
    }

    fn rebuild_actor_priuses(&mut self) -> Result<()> {
        let mut section: Vec<u8> = Vec::new();
        let blobs = std::mem::take(&mut self.actor_priuses);
        let mut rebuilt = Vec::with_capacity(blobs.len());
        for blob in blobs {
            let bytes = self.blob_bytes(&blob)?;
            let offset = section.len() as u32;
            section.extend_from_slice(&bytes);
            pad4(&mut section);
            rebuilt.push(PriusBlob {
                offset,
                size: bytes.len() as u32,
                raw: bytes,
                ..blob
            });
        }
        self.actor_priuses = rebuilt;
        put(&mut self.dat1, TAG_ACTOR_PRIUS_DATA, section)
    }

    fn blob_bytes(&mut self, blob: &PriusBlob) -> Result<Vec<u8>> {
        match (&blob.error, blob.dirty) {
            // Its `json` is Null, so serializing it would write an empty blob
            // over whatever the game shipped. Nothing may mark it dirty, but if
            // anything ever does, say so rather than silently truncating it.
            (Some(why), true) => Err(ToolkitError::Parse(format!(
                "prius blob at {} was never decoded ({why}), so it cannot be rewritten",
                blob.offset
            ))),
            (_, true) => serialize_blob(&blob.json, &mut self.dat1),
            (_, false) => Ok(blob.raw.clone()),
        }
    }

    /// The prius blobs whose DDL would not decode, as `(offset, reason)`.
    pub fn undecoded_priuses(&self) -> Vec<(u32, &str)> {
        self.script_priuses
            .iter()
            .chain(self.actor_priuses.iter())
            .filter_map(|b| b.error.as_deref().map(|e| (b.offset, e)))
            .collect()
    }

    /// Every path-like string currently reachable from a prius blob.
    fn path_like_strings(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for blob in self.script_priuses.iter().chain(self.actor_priuses.iter()) {
            collect_paths(&blob.json, &mut out, &mut seen);
        }
        out
    }

    /// Keep the reference table in step with prius path edits: reuse the slots
    /// of paths that disappeared, then append whatever is left over.
    fn rebuild_asset_refs(&mut self, old_paths: &[String], new_paths: &[String]) {
        let old_set: std::collections::HashSet<&str> =
            old_paths.iter().map(String::as_str).collect();
        let new_set: std::collections::HashSet<&str> =
            new_paths.iter().map(String::as_str).collect();
        let added: Vec<&str> = new_paths
            .iter()
            .map(String::as_str)
            .filter(|p| !old_set.contains(p))
            .collect();
        if added.is_empty() {
            return;
        }

        let mut free_slots: Vec<usize> = Vec::new();
        for (i, r) in self.asset_refs.iter().enumerate() {
            let Some(name) = self.dat1.get_string(r.name_offset) else {
                continue;
            };
            if old_set.contains(name.as_str()) && !new_set.contains(name.as_str()) {
                free_slots.push(i);
            }
        }

        let mut slots = free_slots.into_iter();
        for path in added {
            let name_offset = intern(&mut self.dat1, path);
            let ext_hash = extension_hash(path);
            // `zone_refs` measured both id families this table uses; every path
            // that reaches here has an extension, so this is the CRC-64 one.
            let asset_id = crate::core::zone_refs::asset_id_for(path, ext_hash);
            let entry = AssetRef { asset_id, name_offset, ext_hash };
            match slots.next() {
                Some(i) => self.asset_refs[i] = entry,
                None => self.asset_refs.push(entry),
            }
        }

        let mut section = Vec::with_capacity(self.asset_refs.len() * ASSET_REF_LEN);
        for r in &self.asset_refs {
            section.extend_from_slice(&r.asset_id.to_le_bytes());
            section.extend_from_slice(&r.name_offset.to_le_bytes());
            section.extend_from_slice(&r.ext_hash.to_le_bytes());
        }
        let _ = self.dat1.set_section_data(TAG_ZONE_ASSET_REFS, section);
    }
}

/// One wave's script nodes, from the HUD countdown that announces it to the
/// `OnNumAliveAction` that decides it is cleared.
#[derive(Debug, Clone)]
pub struct WaveSegment {
    pub number: u32,
    /// Every node belonging to the wave, ascending.
    pub nodes: Vec<usize>,
    /// First node of the countdown chain — the segment's only entry.
    pub head: usize,
    /// The node whose links leave the segment when the wave is cleared.
    pub tail: usize,
    /// External nodes that link into `head`.
    pub feeders: Vec<usize>,
}

#[derive(Debug, serde::Serialize)]
pub struct CloneReport {
    pub source_wave: u32,
    pub new_wave: u32,
    pub nodes_added: usize,
    pub vars_added: usize,
    pub priuses_added: usize,
    /// Edges from outside the segment re-pointed at the copy.
    pub inbound_replicated: usize,
    /// The copy's clear-check node — where the next copy in a chain attaches.
    pub tail_node: usize,
    /// First node, var and script prius the copy added; everything it owns is at or past these.
    pub first_node: usize,
    pub first_var: usize,
    pub first_prius: usize,
    pub warning: Option<String>,
}

impl Zone {
    fn prius_name_of(&self, node: usize) -> Option<String> {
        self.actions.get(node)?;
        self.script_priuses
            .iter()
            .find(|b| b.owners.contains(&node))?
            .json
            .pointer("/Name/Value")?
            .as_str()
            .map(str::to_ascii_lowercase)
    }

    /// Locate the script nodes that make up one wave.
    pub fn wave_segment(&self, number: u32) -> Result<WaveSegment> {
        let tag = format!("wave_{number:02}");
        let plugs = self.node_plugs();

        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); self.actions.len()];
        let mut radj: Vec<Vec<usize>> = vec![Vec::new(); self.actions.len()];
        for (i, np) in plugs.iter().enumerate() {
            for p in &np.links {
                let t = p.index as usize;
                if t < self.actions.len() {
                    adj[i].push(t);
                    radj[t].push(i);
                }
            }
        }

        let mut listeners = Vec::new();
        let mut emitters = Vec::new();
        for i in 0..self.actions.len() {
            if self.prius_name_of(i).is_some_and(|n| n.contains(&tag)) {
                if radj[i].is_empty() {
                    listeners.push(i);
                } else {
                    emitters.push(i);
                }
            }
        }
        let spawn_into = crc32::hash("SpawnIntoGroup");
        let spawners: Vec<usize> = (0..self.actions.len())
            .filter(|i| {
                plugs[*i].params.iter().any(|p| {
                    p.name_hash == spawn_into
                        && self
                            .var_name(p.index as usize)
                            .is_some_and(|n| n.to_ascii_lowercase().contains(&tag))
                })
            })
            .collect();
        if listeners.is_empty() && spawners.is_empty() {
            return Err(ToolkitError::Parse(format!("no wave {number} in this zone")));
        }

        let seeds_fwd: Vec<usize> = listeners.iter().chain(&spawners).copied().collect();
        let seeds_bwd: Vec<usize> = emitters.iter().chain(&spawners).copied().collect();
        let fwd = closure(&seeds_fwd, &adj);
        let bwd = closure(&seeds_bwd, &radj);

        let mut nodes: std::collections::BTreeSet<usize> =
            fwd.intersection(&bwd).copied().collect();
        nodes.extend(seeds_fwd.iter().chain(&seeds_bwd).copied());

        // The factory behind each spawner is reached through a variable, not a link.
        let factories_plug = crc32::hash("Factories");
        for s in &spawners {
            if let Some(p) = plugs[*s].params.iter().find(|p| p.name_hash == factories_plug) {
                if let Some(w) = self.var_writer(p.index as usize) {
                    nodes.insert(w);
                }
            }
        }

        // Walk back from the wave's start relay through the HUD countdown.
        let start_relay = emitters
            .iter()
            .copied()
            .find(|i| {
                self.prius_name_of(*i)
                    .is_some_and(|n| n.ends_with(&format!("{tag}_start")))
            })
            .or_else(|| emitters.first().copied())
            .ok_or_else(|| {
                ToolkitError::Parse(format!("wave {number} has no start relay"))
            })?;
        let mut head = start_relay;
        loop {
            let preds = &radj[head];
            if preds.len() != 1 {
                break;
            }
            let p = preds[0];
            let ty = self.action_type(&self.actions[p]);
            if ty != "DelayAction" && ty != "UIArenaWaveAction" {
                break;
            }
            nodes.insert(p);
            head = p;
        }

        // Extend forward to the wave-cleared check.
        let mut tail = start_relay;
        for i in nodes.clone() {
            if self.action_type(&self.actions[i]) == "CompareNumberAction" {
                for t in &adj[i] {
                    if self.action_type(&self.actions[*t]) == "OnNumAliveAction" {
                        nodes.insert(*t);
                        tail = *t;
                    }
                }
            }
        }

        // HUD banners and messages (with their delays) fired only from inside
        // the wave are dead ends, so the path walk above misses them; without
        // a copy the clone shows the source wave's number and messages.
        loop {
            let mut added = false;
            for i in nodes.clone() {
                for t in &adj[i] {
                    if nodes.contains(t) || !radj[*t].iter().all(|s| nodes.contains(s)) {
                        continue;
                    }
                    let banner = adj[*t].is_empty()
                        && self.action_type(&self.actions[*t]) == "UIArenaWaveAction";
                    if banner || self.is_message_chain(&plugs, *t) {
                        nodes.insert(*t);
                        added = true;
                    }
                }
            }
            if !added {
                break;
            }
        }

        let feeders = radj[head].clone();
        Ok(WaveSegment {
            number,
            nodes: nodes.into_iter().collect(),
            head,
            tail,
            feeders,
        })
    }

    /// Duplicate a wave and splice the copy in after `after`, or after the
    /// source wave itself when `after` is `None`.
    ///
    /// Repeated copies always clone the *original* wave — cloning a copy loses
    /// nodes, because a copy's countdown chain no longer looks like the
    /// shipped one — and chain onto each other through `after`.
    pub fn clone_wave(
        &mut self,
        source: u32,
        new_number: u32,
        after: Option<usize>,
    ) -> Result<CloneReport> {
        self.clone_wave_inner(source, new_number, after, true)
    }

    /// Apply a list of copies in order; successive copies of one wave chain
    /// onto the previous copy.
    pub fn apply_clones(&mut self, requests: &[CloneWaveRequest]) -> Result<Vec<CloneReport>> {
        let mut chain_tail: BTreeMap<u32, usize> = BTreeMap::new();
        let mut reports = Vec::with_capacity(requests.len());
        for req in requests {
            let after = chain_tail.get(&req.source).copied();
            let report = self.clone_wave(req.source, req.new_number, after)?;
            chain_tail.insert(req.source, report.tail_node);
            reports.push(report);
        }
        Ok(reports)
    }

    /// `rename` off keeps the copy on the source wave's signal and group names,
    /// which is only useful for narrowing down routing problems.
    pub fn clone_wave_inner(
        &mut self,
        source: u32,
        new_number: u32,
        after: Option<usize>,
        rename: bool,
    ) -> Result<CloneReport> {
        let seg = self.wave_segment(source)?;
        let old_tag = format!("wave_{source:02}");
        let new_tag = if rename {
            format!("wave_{new_number:02}")
        } else {
            old_tag.clone()
        };
        let mut plugs = self.node_plugs();
        let mut ids = IdAllocator::new(self, (new_number as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let first_prius = self.script_priuses.len();

        let clone_base = self.actions.len();
        let index_of: std::collections::HashMap<usize, usize> = seg
            .nodes
            .iter()
            .enumerate()
            .map(|(k, n)| (*n, clone_base + k))
            .collect();

        // Variables referenced only from inside the segment are duplicated so
        // the clone gets its own enemy group, counter and spawn-point picks.
        let in_seg: std::collections::HashSet<usize> = seg.nodes.iter().copied().collect();
        let mut var_map: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
        let mut vars_added = 0usize;
        let var_base = self.vars.len();
        for n in &seg.nodes {
            for p in plugs[*n].params.clone() {
                let v = p.index as usize;
                if var_map.contains_key(&v) {
                    continue;
                }
                let shared = plugs.iter().enumerate().any(|(i, np)| {
                    !in_seg.contains(&i) && np.params.iter().any(|q| q.index as usize == v)
                });
                if shared {
                    continue;
                }
                let Some(mut var) = self.vars.get(v).cloned() else { continue };
                if let Some(name) = self.var_name(v) {
                    if name.to_ascii_lowercase().contains(&old_tag) {
                        let renamed = replace_wave_tag(name, &old_tag, &new_tag);
                        var.name_index = self.intern_script_string(&renamed);
                        var.name_index2 = u16::MAX;
                        var.name_index3 = u16::MAX;
                    }
                }
                var.id = ids.fresh(var.id);
                self.vars.push(var);
                var_map.insert(v, self.vars.len() - 1);
                vars_added += 1;
            }
        }

        let mut priuses_added = 0usize;
        let mut blob_map: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
        let mut instance_keys: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
        // Fixed base: `self.actions.len()` grows as copies are pushed, so it
        // cannot be re-read per iteration without skewing every index.
        let base = self.actions.len();
        for (k, n) in seg.nodes.iter().enumerate() {
            let new_index = base + k;
            let mut action = self.actions[*n].clone();
            // Authored nodes get their own template id; prefab nodes stay on
            // theirs and move to a new instance, as a second placement would.
            let key = (action.node_id ^ action.template_id) & !ID_FLAG;
            if key == 0 {
                let id = ids.fresh(action.node_id);
                action.template_id = (id & !ID_FLAG) | (action.template_id & ID_FLAG);
                action.node_id = id;
            } else {
                let new_key = *instance_keys
                    .entry(key)
                    .or_insert_with(|| ids.fresh(key) & !ID_FLAG);
                let id = (action.template_id ^ new_key) | ID_FLAG;
                action.node_id = if ids.claim(id) { id } else { ids.fresh(id) };
            }
            action.prius_offset = 0;
            action.prius_size = 0;

            // Source nodes that shared a prius blob keep sharing its copy.
            if let Some(src) = self.script_priuses.iter().position(|b| b.owners.contains(n)) {
                match blob_map.get(&src) {
                    Some(existing) => self.script_priuses[*existing].owners.push(new_index),
                    None => {
                        let mut json = self.script_priuses[src].json.clone();
                        rename_wave_strings(&mut json, &old_tag, &new_tag);
                        // Carried over so a copy of a blob that never decoded
                        // cannot be written back as an empty one.
                        let error = self.script_priuses[src].error.clone();
                        self.script_priuses.push(PriusBlob {
                            offset: 0,
                            size: 0,
                            json,
                            owners: vec![new_index],
                            raw: Vec::new(),
                            dirty: true,
                            error,
                        });
                        blob_map.insert(src, self.script_priuses.len() - 1);
                        priuses_added += 1;
                    }
                }
            }

            let src_plugs = plugs[*n].clone();
            let links = src_plugs
                .links
                .iter()
                .map(|p| ScriptPlug {
                    index: *index_of
                        .get(&(p.index as usize))
                        .map(|v| *v as u32)
                        .as_ref()
                        .unwrap_or(&p.index),
                    ..*p
                })
                .collect();
            let params = src_plugs
                .params
                .iter()
                .map(|p| ScriptPlug {
                    index: var_map
                        .get(&(p.index as usize))
                        .map(|v| *v as u32)
                        .unwrap_or(p.index),
                    ..*p
                })
                .collect();

            self.actions.push(action);
            plugs.push(NodePlugs { links, params });
        }

        // The HUD reads its wave number from a variable, and a segment carries
        // two of them: "wave N-1 complete" and "wave N start". Both shift by
        // the same amount, and both are shared with other waves, so the copies
        // need private variables even though the sharing rule skipped them.
        let wave_plug = crc32::hash("Wave");
        let delta = new_number as f64 - source as f64;
        for (k, n) in seg.nodes.iter().enumerate() {
            if self.action_type(&self.actions[*n]) != "UIArenaWaveAction" {
                continue;
            }
            for p in plugs[base + k].params.iter_mut() {
                if p.name_hash != wave_plug {
                    continue;
                }
                let idx = p.index as usize;
                let Some(value) = self.vars.get(idx).and_then(ScriptVar::as_number) else {
                    continue;
                };
                if idx >= var_base {
                    // Already a private copy from the sharing pass.
                    let _ = self.vars[idx].set_number(value + delta);
                } else {
                    let mut copy = self.vars[idx].clone();
                    let _ = copy.set_number(value + delta);
                    copy.id = ids.fresh(copy.id);
                    self.vars.push(copy);
                    p.index = (self.vars.len() - 1) as u32;
                    vars_added += 1;
                }
            }
        }

        // Links live on the source node, so copying a destination does not
        // reproduce the edges that reach it from outside the segment — the
        // copy would sit there untriggered. Replicate each of those onto the
        // copy. The head is skipped: the splice below is its inbound edge.
        let mut replicated = 0usize;
        for (k, n) in seg.nodes.iter().enumerate() {
            if *n == seg.head {
                continue;
            }
            let clone_index = (base + k) as u32;
            for ext in 0..clone_base {
                if in_seg.contains(&ext) {
                    continue;
                }
                let extra: Vec<ScriptPlug> = plugs[ext]
                    .links
                    .iter()
                    .filter(|p| p.index as usize == *n)
                    .map(|p| ScriptPlug { index: clone_index, ..*p })
                    .collect();
                replicated += extra.len();
                plugs[ext].links.extend(extra);
            }
        }

        // Splice: the source wave's clear check now starts the clone, and the
        // clone hands on to whatever the source used to trigger.
        // Messages on the clear check belong to their own wave and stay put.
        let clone_head = index_of[&seg.head];
        let clone_tail = index_of[&seg.tail];
        let attach = after.unwrap_or(seg.tail);
        let chain: std::collections::HashSet<usize> = (0..self.actions.len())
            .filter(|i| self.is_message_chain(&plugs, *i))
            .collect();
        let is_message = |p: &ScriptPlug| chain.contains(&(p.index as usize));
        let (kept, handoff): (Vec<ScriptPlug>, Vec<ScriptPlug>) =
            plugs[attach].links.iter().copied().partition(|p| is_message(p));
        let mut tail_links: Vec<ScriptPlug> =
            plugs[clone_tail].links.iter().copied().filter(|p| is_message(p)).collect();
        tail_links.extend(handoff.iter().copied());
        plugs[clone_tail].links = tail_links;
        plugs[attach].links = kept;
        plugs[attach].links.extend(handoff.iter().map(|p| ScriptPlug {
            index: clone_head as u32,
            ..*p
        }));

        self.set_node_plugs(&plugs);
        // When the segment's own clear check also feeds its countdown the two
        // ends have collapsed, and the copy lands beside the source rather
        // than after it.
        let warning = seg.feeders.contains(&seg.tail).then(|| {
            format!(
                "wave {source} segment start and end resolved to the same node; \
                 the copy runs adjacent to it, not strictly after — clone the \
                 last wave for a predictable order"
            )
        });
        Ok(CloneReport {
            source_wave: source,
            new_wave: new_number,
            nodes_added: seg.nodes.len(),
            vars_added,
            priuses_added,
            inbound_replicated: replicated,
            tail_node: clone_tail,
            first_node: clone_base,
            first_var: var_base,
            first_prius,
            warning,
        })
    }

    /// Show `msg.text` on the HUD when a wave starts or is cleared, through a
    /// `HUDMessageAction`. The text goes in `LocTag`, which the game shows
    /// verbatim when it is not a localization key.
    pub fn add_wave_message(&mut self, msg: &WaveMessageRequest) -> Result<usize> {
        let node = match msg.node {
            Some(n) => {
                if self.actions.get(n).map(|a| self.action_type(a)) != Some(HUD_MESSAGE_TYPE) {
                    return Err(ToolkitError::Parse(format!("node {n} is not a HUD message")));
                }
                self.set_message_prius(n, msg);
                self.unhook_nodes(&[n]);
                n
            }
            None => self.push_message_node(msg),
        };
        self.hook_message(node, msg.wave, msg.when, msg.delay)?;
        Ok(node)
    }

    fn set_message_prius(&mut self, node: usize, msg: &WaveMessageRequest) {
        let blob = self.script_priuses.iter().position(|b| b.owners.contains(&node));
        match blob {
            Some(b) if self.script_priuses[b].owners.len() == 1 => {
                self.script_priuses[b].json = message_prius(msg);
                self.script_priuses[b].dirty = true;
            }
            // A shared blob keeps its other owners; this node gets its own.
            Some(b) => {
                self.script_priuses[b].owners.retain(|o| *o != node);
                self.push_message_prius(node, msg);
            }
            None => self.push_message_prius(node, msg),
        }
    }

    fn reward_banner(&self) -> Option<usize> {
        self.actions.iter().position(|a| self.action_type(a) == REWARD_BANNER_TYPE)
    }

    /// The message standing in for the victory banner, once it has been swapped.
    fn victory_message(&self, plugs: &[NodePlugs]) -> Option<usize> {
        let reward = self.reward_banner()?;
        if plugs.iter().any(|np| np.links.iter().any(|p| p.index as usize == reward)) {
            return None;
        }
        let next: Vec<usize> = plugs[reward].links.iter().map(|p| p.index as usize).collect();
        let show = crc32::hash("Show");
        plugs
            .iter()
            .enumerate()
            .filter(|(i, np)| {
                *i != reward && np.links.iter().any(|p| next.contains(&(p.index as usize)))
            })
            .flat_map(|(_, np)| np.links.iter())
            .find(|p| {
                p.other_hash == show
                    && self.action_type(&self.actions[p.index as usize]) == HUD_MESSAGE_TYPE
            })
            .map(|p| p.index as usize)
    }

    /// The victory banner's text if it has been swapped for a message; `None`
    /// while the stock banner (words from the HUD) is still in place.
    pub fn victory_text(&self) -> Option<(String, MessageStyle)> {
        let m = self.victory_message(&self.node_plugs())?;
        let json = &self.script_priuses.iter().find(|b| b.owners.contains(&m))?.json;
        let field = |k: &str| json.get(k).and_then(|f| f.get("Value")).and_then(Value::as_str);
        Some((
            field("LocTag").unwrap_or("").to_string(),
            message_style(field("HudMessageType"), field("MessageType")),
        ))
    }

    pub fn has_victory_banner(&self) -> bool {
        self.reward_banner().is_some()
    }

    /// Swap the stock victory banner (`UIArenaRewardAction`, which takes no
    /// text) for a HUD message. Its feeders are wired straight to its
    /// successors, so the win sequence keeps its timing.
    pub fn set_victory_text(&mut self, text: &str, style: MessageStyle) -> Result<usize> {
        let msg = WaveMessageRequest {
            node: None,
            wave: 0,
            text: text.to_string(),
            duration: default_message_seconds(),
            style,
            when: MessageWhen::Cleared,
            delay: 0.0,
        };
        if let Some(m) = self.victory_message(&self.node_plugs()) {
            self.set_message_prius(m, &msg);
            return Ok(m);
        }
        let reward = self
            .reward_banner()
            .ok_or_else(|| ToolkitError::Parse("zone has no victory banner".into()))?;
        let before = self.node_plugs();
        let feeders: Vec<(usize, ScriptPlug)> = before
            .iter()
            .enumerate()
            .flat_map(|(i, np)| {
                np.links
                    .iter()
                    .filter(|p| p.index as usize == reward)
                    .map(move |p| (i, *p))
            })
            .collect();
        if feeders.is_empty() {
            return Err(ToolkitError::Parse("the victory banner is not connected".into()));
        }
        let outs = before[reward].links.clone();

        let node = self.push_message_node(&msg);
        let mut plugs = self.node_plugs();
        let show = crc32::hash("Show");
        for (src, p) in &feeders {
            plugs[*src].links.retain(|q| q.index as usize != reward);
            for o in &outs {
                plugs[*src].links.push(ScriptPlug {
                    name_hash: p.name_hash,
                    index: o.index,
                    other_hash: o.other_hash,
                });
            }
            plugs[*src].links.push(ScriptPlug {
                name_hash: p.name_hash,
                index: node as u32,
                other_hash: show,
            });
        }
        self.set_node_plugs(&plugs);
        Ok(node)
    }

    fn push_message_node(&mut self, msg: &WaveMessageRequest) -> usize {
        let seed = crc32::hash(&msg.text) as u64 ^ ((msg.wave as u64) << 32);
        let mut ids = IdAllocator::new(self, 0x4855_444D_5347);
        let node = self.actions.len();
        let mut plugs = self.node_plugs();

        // Stock messages address the local players through a `_Players` var.
        let mut params = Vec::new();
        if let Some(v) = (0..self.vars.len()).find(|v| self.var_name(*v) == Some("_Players")) {
            let mut var = self.vars[v].clone();
            var.id = ids.fresh(var.id ^ seed);
            self.vars.push(var);
            params.push(ScriptPlug {
                name_hash: MESSAGE_PLAYERS_PLUG,
                index: (self.vars.len() - 1) as u32,
                other_hash: crc32::hash("Out"),
            });
        }

        let id = ids.fresh(seed);
        let type_index = self.intern_script_string(HUD_MESSAGE_TYPE);
        self.actions.push(ScriptAction {
            index: self.actions.len() as u32,
            node_id: id,
            template_id: id,
            in_degree: 0,
            link_start: u16::MAX,
            link_count: 0,
            param_start: u16::MAX,
            param_count: 0,
            type_index,
            type_hash: crc32::hash(HUD_MESSAGE_TYPE),
            prius_offset: 0,
            prius_size: 0,
        });
        plugs.push(NodePlugs { links: Vec::new(), params });
        self.set_node_plugs(&plugs);
        self.push_message_prius(node, msg);
        node
    }

    fn push_message_prius(&mut self, node: usize, msg: &WaveMessageRequest) {
        self.script_priuses.push(PriusBlob {
            offset: 0,
            size: 0,
            json: message_prius(msg),
            owners: vec![node],
            raw: Vec::new(),
            dirty: true,
            error: None,
        });
    }

    fn hook_message(
        &mut self,
        node: usize,
        wave: u32,
        when: MessageWhen,
        delay: f64,
    ) -> Result<()> {
        let mut plugs = self.node_plugs();
        let hook = self.wave_trigger(&plugs, wave, when).ok_or_else(|| {
            ToolkitError::Parse(format!("wave {wave} has no {when:?} trigger for a message"))
        })?;
        let pin = plugs[hook]
            .links
            .first()
            .map(|p| p.name_hash)
            .unwrap_or_else(|| crc32::hash("Out"));
        let (target, input) = if delay > 0.0 {
            (self.push_delay_node(&mut plugs, node, delay), crc32::hash("In"))
        } else {
            (node, crc32::hash("Show"))
        };
        plugs[hook].links.push(ScriptPlug {
            name_hash: pin,
            index: target as u32,
            other_hash: input,
        });
        self.set_node_plugs(&plugs);
        Ok(())
    }

    /// A `DelayAction` that fires `message` after `seconds`; returns its index.
    fn push_delay_node(&mut self, plugs: &mut Vec<NodePlugs>, message: usize, seconds: f64) -> usize {
        let mut ids = IdAllocator::new(self, 0x4445_4C41_59);
        let seed = (message as u64) << 20 ^ seconds.to_bits();
        let mut value = [0u8; 16];
        value[0..4].copy_from_slice(&(seconds as f32).to_le_bytes());
        self.vars.push(ScriptVar {
            value_type: ScriptVar::TYPE_FLOAT,
            name_index: u16::MAX,
            name_index2: u16::MAX,
            name_index3: u16::MAX,
            id: ids.fresh(seed ^ 0x5641_52),
            value,
        });
        let id = ids.fresh(seed);
        let type_index = self.intern_script_string(DELAY_TYPE);
        let node = self.actions.len();
        self.actions.push(ScriptAction {
            index: self.actions.len() as u32,
            node_id: id,
            template_id: id,
            in_degree: 0,
            link_start: u16::MAX,
            link_count: 0,
            param_start: u16::MAX,
            param_count: 0,
            type_index,
            type_hash: crc32::hash(DELAY_TYPE),
            prius_offset: 0,
            prius_size: 0,
        });
        plugs.push(NodePlugs {
            links: vec![ScriptPlug {
                name_hash: crc32::hash("Out"),
                index: message as u32,
                other_hash: crc32::hash("Show"),
            }],
            params: vec![ScriptPlug {
                name_hash: crc32::hash("Duration"),
                index: (self.vars.len() - 1) as u32,
                other_hash: crc32::hash("Out"),
            }],
        });
        node
    }

    /// A HUD message, or a delay whose only job is to fire HUD messages.
    fn is_message_chain(&self, plugs: &[NodePlugs], node: usize) -> bool {
        let is_message =
            |i: usize| self.actions.get(i).is_some_and(|a| self.action_type(a) == HUD_MESSAGE_TYPE);
        if is_message(node) {
            return true;
        }
        self.actions.get(node).is_some_and(|a| self.action_type(a) == DELAY_TYPE)
            && !plugs[node].links.is_empty()
            && plugs[node].links.iter().all(|p| is_message(p.index as usize))
    }

    /// Disconnect HUD message nodes, and any delay feeding only them; other
    /// nodes are left alone.
    fn unhook_nodes(&mut self, nodes: &[usize]) {
        let mut plugs = self.node_plugs();
        let mut targets: Vec<usize> = nodes
            .iter()
            .copied()
            .filter(|n| self.actions.get(*n).map(|a| self.action_type(a)) == Some(HUD_MESSAGE_TYPE))
            .collect();
        if targets.is_empty() {
            return;
        }
        let delays: Vec<usize> = (0..self.actions.len())
            .filter(|d| {
                self.is_message_chain(&plugs, *d)
                    && !targets.contains(d)
                    && plugs[*d].links.iter().all(|p| targets.contains(&(p.index as usize)))
            })
            .collect();
        targets.extend(delays);
        for np in &mut plugs {
            np.links.retain(|p| !targets.contains(&(p.index as usize)));
        }
        self.set_node_plugs(&plugs);
    }

    pub fn has_message_trigger(&self, wave: u32, when: MessageWhen) -> bool {
        self.wave_trigger(&self.node_plugs(), wave, when).is_some()
    }

    /// The listener a wave's start signal lands on, or the check that fires
    /// once all of its enemies are dead.
    fn wave_trigger(&self, plugs: &[NodePlugs], wave: u32, when: MessageWhen) -> Option<usize> {
        (0..self.actions.len()).find(|i| {
            self.trigger_wave(plugs, *i) == Some((wave, when))
                && (when == MessageWhen::Cleared
                    || !plugs.iter().any(|np| np.links.iter().any(|p| p.index as usize == *i)))
        })
    }

    fn trigger_wave(&self, plugs: &[NodePlugs], node: usize) -> Option<(u32, MessageWhen)> {
        let short = |s: &str| s.rsplit("::").next().unwrap_or(s).to_ascii_lowercase();
        match self.action_type(&self.actions[node]) {
            "SignalRelayAction" => {
                let name = short(&self.prius_name_of(node)?);
                // Every challenge opens wave 1 off the shared intro, not a wave relay.
                if name == "arena_title_and_countdown_complete" {
                    return Some((1, MessageWhen::Start));
                }
                let n = name.strip_prefix("wave_")?.strip_suffix("_start")?;
                Some((n.parse().ok()?, MessageWhen::Start))
            }
            "OnNumAliveAction" => {
                let group = crc32::hash("Group");
                let p = plugs[node].params.iter().find(|p| p.name_hash == group)?;
                let name = short(self.var_name(p.index as usize)?);
                let n = name.strip_prefix("wave_")?.strip_suffix("_enemies")?;
                Some((n.parse().ok()?, MessageWhen::Cleared))
            }
            _ => None,
        }
    }

    /// HUD messages fired by a wave's start listener or clear check, directly
    /// or through a delay of their own.
    pub fn wave_messages(&self) -> Vec<WaveMessage> {
        let plugs = self.node_plugs();
        let show = crc32::hash("Show");
        let duration = crc32::hash("Duration");
        let mut feeders: Vec<Vec<usize>> = vec![Vec::new(); self.actions.len()];
        for (src, np) in plugs.iter().enumerate() {
            for p in &np.links {
                if let Some(f) = feeders.get_mut(p.index as usize) {
                    f.push(src);
                }
            }
        }
        let mut out = Vec::new();
        for (src, np) in plugs.iter().enumerate() {
            for p in &np.links {
                let node = p.index as usize;
                if p.other_hash != show
                    || self.actions.get(node).map(|a| self.action_type(a)) != Some(HUD_MESSAGE_TYPE)
                {
                    continue;
                }
                let own_delay = self.action_type(&self.actions[src]) == DELAY_TYPE
                    && self.is_message_chain(&plugs, src);
                let (trigger, delay) = if own_delay {
                    let seconds = plugs[src]
                        .params
                        .iter()
                        .find(|q| q.name_hash == duration)
                        .and_then(|q| self.vars.get(q.index as usize))
                        .and_then(ScriptVar::as_number)
                        .unwrap_or(0.0);
                    match feeders[src].first() {
                        Some(t) => (*t, seconds),
                        None => continue,
                    }
                } else {
                    (src, 0.0)
                };
                let Some((wave, when)) = self.trigger_wave(&plugs, trigger) else {
                    continue;
                };
                let blob = self.script_priuses.iter().position(|b| b.owners.contains(&node));
                let field = |k: &str| {
                    blob.and_then(|b| self.script_priuses[b].json.get(k))
                        .and_then(|f| f.get("Value"))
                };
                out.push(WaveMessage {
                    node,
                    wave,
                    when,
                    style: message_style(
                        field("HudMessageType").and_then(Value::as_str),
                        field("MessageType").and_then(Value::as_str),
                    ),
                    text: field("LocTag").and_then(Value::as_str).unwrap_or("").to_string(),
                    duration: field("Duration")
                        .and_then(Value::as_f64)
                        .unwrap_or_else(default_message_seconds),
                    delay: (delay * 100.0).round() / 100.0,
                    prius_id: blob,
                });
            }
        }
        out
    }
}

const HUD_MESSAGE_TYPE: &str = "HUDMessageAction";
const DELAY_TYPE: &str = "DelayAction";
/// The stock "Victory!" banner; its words come from the HUD, not the zone.
const REWARD_BANNER_TYPE: &str = "UIArenaRewardAction";
/// Param that carries `_Players` on every shipped HUD message; name unresolved.
const MESSAGE_PLAYERS_PLUG: u32 = 0x3D24_E232;

fn message_style(kind: Option<&str>, placement: Option<&str>) -> MessageStyle {
    if kind == Some("kHelp") {
        return MessageStyle::Help;
    }
    // An unset `MessageType` is the DDL default, `kGeneric`.
    let placement = placement.unwrap_or("kGeneric");
    MESSAGE_STYLES
        .iter()
        .find(|(_, _, p)| *p == Some(placement))
        .map(|(s, _, _)| *s)
        .unwrap_or(MessageStyle::Banner)
}

fn message_prius(msg: &WaveMessageRequest) -> Value {
    let (kind, placement) = MESSAGE_STYLES
        .iter()
        .find(|(s, _, _)| *s == msg.style)
        .map(|(_, k, p)| (*k, *p))
        .unwrap_or(("kObjective", Some("kCenter")));
    let mut json = serde_json::json!({
        "LocTag": { "Type": "String", "Value": msg.text },
        "HudMessageType": { "Type": "String", "Value": kind },
        "Duration": { "Type": "Float", "Value": msg.duration },
    });
    if let Some(p) = placement {
        json["MessageType"] = serde_json::json!({ "Type": "String", "Value": p });
    }
    json
}

fn closure(seeds: &[usize], adj: &[Vec<usize>]) -> std::collections::HashSet<usize> {
    let mut seen: std::collections::HashSet<usize> = seeds.iter().copied().collect();
    let mut frontier: Vec<usize> = seeds.to_vec();
    while let Some(n) = frontier.pop() {
        for t in &adj[n] {
            if seen.insert(*t) {
                frontier.push(*t);
            }
        }
    }
    seen
}

/// Set on every script node and variable id the game ships. A spawner whose
/// node id lacks it never spawns, which stalls the wave it belongs to.
const ID_FLAG: u64 = 1 << 63;

/// Hands out node and variable ids that follow the shipped scheme: flagged,
/// and unique in their low 63 bits (a node id is its template id XOR the
/// prefab instance it came from, zero for nodes authored in the zone itself).
struct IdAllocator {
    taken: std::collections::HashSet<u64>,
    salt: u64,
}

impl IdAllocator {
    fn new(zone: &Zone, salt: u64) -> Self {
        let mut taken = std::collections::HashSet::new();
        for a in &zone.actions {
            taken.insert(a.node_id & !ID_FLAG);
            taken.insert(a.template_id & !ID_FLAG);
        }
        for v in &zone.vars {
            taken.insert(v.id & !ID_FLAG);
        }
        Self { taken, salt }
    }

    fn fresh(&mut self, seed: u64) -> u64 {
        let mut x = seed ^ self.salt;
        loop {
            x = mix64(x);
            let low = x & !ID_FLAG;
            if low != 0 && self.taken.insert(low) {
                return low | ID_FLAG;
            }
        }
    }

    fn claim(&mut self, id: u64) -> bool {
        self.taken.insert(id & !ID_FLAG)
    }
}

fn mix64(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Replace `wave_05` with `wave_06` in any case the authoring data uses.
fn replace_wave_tag(s: &str, old: &str, new: &str) -> String {
    let upper_old = old.to_ascii_uppercase();
    let upper_new = new.to_ascii_uppercase();
    s.replace(old, new).replace(&upper_old, &upper_new)
}

fn rename_wave_strings(v: &mut Value, old: &str, new: &str) {
    match v {
        Value::String(s) => *s = replace_wave_tag(s, old, new),
        Value::Array(a) => a.iter_mut().for_each(|x| rename_wave_strings(x, old, new)),
        Value::Object(m) => m.values_mut().for_each(|x| rename_wave_strings(x, old, new)),
        _ => {}
    }
}

/// A single field assignment inside a prius blob.
///
/// `path` names fields by their DDL name; nesting through a `Struct` is
/// implicit, so `["BotBaseData", "Health"]` targets `BotBaseData.Health`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct FieldPatch {
    pub path: Vec<String>,
    pub value: Value,
}

/// Edits addressed by the indices handed to the frontend by `Zone::parse`.
///
/// `*_json` replaces a whole blob from raw text; it is deserialized on this
/// side so that 64-bit identifiers never pass through a JavaScript `Number`.
#[derive(Debug, Default, serde::Deserialize)]
pub struct ZoneEdits {
    #[serde(default)]
    pub script_priuses: BTreeMap<usize, Value>,
    #[serde(default)]
    pub actor_priuses: BTreeMap<usize, Value>,
    #[serde(default)]
    pub script_prius_json: BTreeMap<usize, String>,
    #[serde(default)]
    pub actor_prius_json: BTreeMap<usize, String>,
    #[serde(default)]
    pub script_prius_patches: BTreeMap<usize, Vec<FieldPatch>>,
    #[serde(default)]
    pub actor_prius_patches: BTreeMap<usize, Vec<FieldPatch>>,
    #[serde(default)]
    pub actor_assets: BTreeMap<usize, String>,
    #[serde(default)]
    pub model_names: BTreeMap<usize, String>,
    /// Numeric script variables by index — this is where `NumSpawns` lives.
    #[serde(default)]
    pub script_vars: BTreeMap<usize, f64>,
    /// 64-bit id variables by index, as hex — actor instance or actor group
    /// ids, which is how a spawner's `Locations` binding is repointed.
    #[serde(default)]
    pub script_var_ids: BTreeMap<usize, String>,
    /// String variables by index — challenge title, victory text.
    #[serde(default)]
    pub script_var_strings: BTreeMap<usize, String>,
    /// Waves to duplicate, applied after every value edit.
    #[serde(default)]
    pub clone_waves: Vec<CloneWaveRequest>,
    /// HUD messages to add, applied after cloning so copies can carry them.
    #[serde(default)]
    pub wave_messages: Vec<WaveMessageRequest>,
    /// Message nodes (by action index) to disconnect from their trigger.
    #[serde(default)]
    pub remove_messages: Vec<usize>,
    /// Text shown instead of the stock victory banner.
    #[serde(default)]
    pub victory_text: Option<String>,
    #[serde(default)]
    pub victory_style: MessageStyle,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct CloneWaveRequest {
    pub source: u32,
    pub new_number: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageStyle {
    /// Big centred banner, as the stock "FIGHT!" message.
    #[default]
    Banner,
    /// Smaller help-box message, as the stock weapon-swap tip.
    Help,
    /// The HUD's arena-wave banner, showing our text.
    Wave,
    /// The HUD's arena-reward slot: always the stock "Victory!" graphic, text ignored.
    Victory,
    // The remaining `MessageType` slots, not yet tried in game.
    Generic,
    Pickup,
    Collectible,
    Location,
    Planet,
    Corner,
    Tutorial,
}

/// Style → (`HudMessageType`, `MessageType`) as written to the prius.
const MESSAGE_STYLES: [(MessageStyle, &str, Option<&str>); 11] = [
    (MessageStyle::Banner, "kObjective", Some("kCenter")),
    (MessageStyle::Help, "kHelp", None),
    (MessageStyle::Wave, "kObjective", Some("kArenaWave")),
    (MessageStyle::Victory, "kObjective", Some("kArenaReward")),
    (MessageStyle::Generic, "kObjective", Some("kGeneric")),
    (MessageStyle::Pickup, "kObjective", Some("kPickup")),
    (MessageStyle::Collectible, "kObjective", Some("kCollectible")),
    (MessageStyle::Location, "kObjective", Some("kLocation")),
    (MessageStyle::Planet, "kObjective", Some("kPlanet")),
    (MessageStyle::Corner, "kObjective", Some("kCorner")),
    (MessageStyle::Tutorial, "kObjective", Some("kTutorial")),
];

#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageWhen {
    #[default]
    Start,
    Cleared,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct WaveMessageRequest {
    /// An existing message node to rewrite and re-hook; `None` adds one.
    #[serde(default)]
    pub node: Option<usize>,
    pub wave: u32,
    pub text: String,
    #[serde(default = "default_message_seconds")]
    pub duration: f64,
    #[serde(default)]
    pub style: MessageStyle,
    #[serde(default)]
    pub when: MessageWhen,
    /// Seconds between the trigger and the message, to clear the stock banners.
    #[serde(default)]
    pub delay: f64,
}

fn default_message_seconds() -> f64 {
    4.0
}

/// A `HUDMessageAction` hooked to a wave's start relay or clear check.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WaveMessage {
    pub node: usize,
    pub wave: u32,
    pub when: MessageWhen,
    pub style: MessageStyle,
    pub text: String,
    pub duration: f64,
    pub delay: f64,
    pub prius_id: Option<usize>,
}

impl ZoneEdits {
    pub fn is_empty(&self) -> bool {
        self.script_priuses.is_empty()
            && self.actor_priuses.is_empty()
            && self.script_prius_json.is_empty()
            && self.actor_prius_json.is_empty()
            && self.script_prius_patches.is_empty()
            && self.actor_prius_patches.is_empty()
            && self.actor_assets.is_empty()
            && self.model_names.is_empty()
            && self.script_vars.is_empty()
            && self.script_var_ids.is_empty()
            && self.script_var_strings.is_empty()
            && self.clone_waves.is_empty()
            && self.wave_messages.is_empty()
            && self.remove_messages.is_empty()
            && self.victory_text.is_none()
    }
}

/// Assign `value` to the field named by `path`, descending `Struct` wrappers.
fn apply_patch(json: &mut Value, patch: &FieldPatch) -> Result<()> {
    let Some((last, parents)) = patch.path.split_last() else {
        return Err(ToolkitError::Parse("field patch has an empty path".into()));
    };
    let mut cur = json;
    for seg in parents {
        cur = cur
            .get_mut(seg.as_str())
            .and_then(|f| f.get_mut("Value"))
            .ok_or_else(|| ToolkitError::Parse(format!("no such field: {seg}")))?;
    }
    let field = cur
        .get_mut(last.as_str())
        .ok_or_else(|| ToolkitError::Parse(format!("no such field: {last}")))?;
    let Some(slot) = field.get_mut("Value") else {
        return Err(ToolkitError::Parse(format!("field {last} has no Value")));
    };
    *slot = patch.value.clone();
    Ok(())
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn slice_records<T>(dat1: &Dat1, tag: u32, len: usize, f: impl Fn(&[u8]) -> T) -> Vec<T> {
    dat1.get_section_data(tag)
        .map(|d| d.chunks_exact(len).map(&f).collect())
        .unwrap_or_default()
}

fn u32_table(dat1: &Dat1, tag: u32) -> Vec<u32> {
    dat1.get_section_data(tag)
        .map(|d| {
            d.chunks_exact(4)
                .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
                .collect()
        })
        .unwrap_or_default()
}

fn string_table(dat1: &Dat1, tag: u32) -> Vec<String> {
    dat1.get_section_data(tag)
        .map(|d| {
            d.chunks_exact(4)
                .map(|c| {
                    let off = u32::from_le_bytes(c.try_into().unwrap());
                    dat1.get_string(off).unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default()
}


fn id_name_table(dat1: &mut Dat1, ids: &[u64], paths: &[String]) -> Vec<u8> {
    let mut out: Vec<u8> = ids.iter().flat_map(|id| id.to_le_bytes()).collect();
    for p in paths {
        out.extend_from_slice(&intern(dat1, p).to_le_bytes());
    }
    out
}

/// Writes a section; a zone that lacks it can only be given nothing.
fn put(dat1: &mut Dat1, tag: u32, data: Vec<u8>) -> Result<()> {
    if data.is_empty() && dat1.get_section_data(tag).is_none() {
        return Ok(());
    }
    dat1.set_section_data(tag, data)
}

fn parse_blob(section: &[u8], offset: u32, size: u32, dat1: &Dat1) -> Result<Value> {
    let start = offset as usize;
    let end = start + size as usize;
    if size == 0 || end > section.len() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    let obj = ddl::parse(&section[start..end], dat1)?;
    Ok(ddl_object_to_typed_json(&obj))
}

/// Zone DDL blobs store field-name offsets relative to the DAT1 header end,
/// unlike `Dat1`'s own pool-relative `add_string`.
struct ZoneStringPool<'a>(&'a mut Dat1);

impl ddl::StringPoolRead for ZoneStringPool<'_> {
    fn get_string(&self, offset: u32) -> Option<String> {
        self.0.get_string(offset)
    }
}

impl ddl::StringPoolWrite for ZoneStringPool<'_> {
    fn add_string(&mut self, s: &str) -> u32 {
        intern(self.0, s)
    }
}

fn serialize_blob(json: &Value, dat1: &mut Dat1) -> Result<Vec<u8>> {
    let obj = typed_json_to_ddl_object(json)?;
    let mut pool = ZoneStringPool(dat1);
    Ok(ddl::serialize(&obj, &mut pool))
}

fn blob_mut<'a>(
    blobs: &'a mut [PriusBlob],
    idx: usize,
    kind: &str,
) -> Result<&'a mut PriusBlob> {
    blobs
        .get_mut(idx)
        .ok_or_else(|| ToolkitError::Parse(format!("{kind} prius index {idx} out of range")))
}

fn replace_blobs(
    blobs: &mut [PriusBlob],
    edits: &BTreeMap<usize, Value>,
    kind: &str,
) -> Result<()> {
    for (idx, json) in edits {
        let blob = blob_mut(blobs, *idx, kind)?;
        blob.json = json.clone();
        blob.dirty = true;
    }
    Ok(())
}

fn replace_blobs_from_text(
    blobs: &mut [PriusBlob],
    edits: &BTreeMap<usize, String>,
    kind: &str,
) -> Result<()> {
    for (idx, text) in edits {
        let json: Value = serde_json::from_str(text).map_err(|e| {
            ToolkitError::Parse(format!("{kind} prius {idx}: invalid JSON — {e}"))
        })?;
        let blob = blob_mut(blobs, *idx, kind)?;
        blob.json = json;
        blob.dirty = true;
    }
    Ok(())
}

fn patch_blobs(
    blobs: &mut [PriusBlob],
    edits: &BTreeMap<usize, Vec<FieldPatch>>,
    kind: &str,
) -> Result<()> {
    for (idx, patches) in edits {
        let blob = blob_mut(blobs, *idx, kind)?;
        for patch in patches {
            apply_patch(&mut blob.json, patch)?;
        }
        blob.dirty = true;
    }
    Ok(())
}

fn raw_slice(section: &[u8], offset: u32, size: u32) -> Vec<u8> {
    let start = offset as usize;
    let end = start + size as usize;
    if size == 0 || end > section.len() {
        return Vec::new();
    }
    section[start..end].to_vec()
}

fn pad4(buf: &mut Vec<u8>) {
    let r = buf.len() % 4;
    if r != 0 {
        buf.resize(buf.len() + (4 - r), 0);
    }
}

fn intern(dat1: &mut Dat1, s: &str) -> u32 {
    let header_end = dat1.header_end() as u32;
    let mut i = 0usize;
    while i < dat1.strings_pool.len() {
        let mut end = i;
        while end < dat1.strings_pool.len() && dat1.strings_pool[end] != 0 {
            end += 1;
        }
        if &dat1.strings_pool[i..end] == s.as_bytes() {
            return i as u32 + header_end;
        }
        i = end.saturating_add(1);
    }
    let off = dat1.strings_pool.len() as u32;
    dat1.strings_pool.extend_from_slice(s.as_bytes());
    dat1.strings_pool.push(0);
    off + header_end
}

pub fn looks_like_asset_path(s: &str) -> bool {
    s.len() >= 5
        && s.len() < 512
        && (s.contains('/') || s.contains('\\'))
        && s.rsplit('.').next().is_some_and(|e| {
            !e.is_empty() && e.len() <= 16 && e.chars().all(|c| c.is_ascii_alphanumeric())
        })
        && s.contains('.')
}

fn collect_paths(v: &Value, out: &mut Vec<String>, seen: &mut std::collections::HashSet<String>) {
    match v {
        Value::String(s) if looks_like_asset_path(s) => {
            if seen.insert(s.clone()) {
                out.push(s.clone());
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_paths(x, out, seen)),
        Value::Object(m) => m.values().for_each(|x| collect_paths(x, out, seen)),
        _ => {}
    }
}

/// CRC32 of a node/plug name, exposed for callers that display hashes.
pub fn name_hash(s: &str) -> u32 {
    crc32::hash(s)
}

/// Where blocks 1.. start and end inside the tail (the body past block 0).
///
/// Only a prefix of the eight header words are block sizes, so the block count
/// is the smallest `k` with `sum(sizes[0..k]) == body_len`. When no `k` fits
/// (an asset whose header we cannot trust) the whole tail is reported as one
/// block, which is what the previous fixed-four-sizes reader always did.
fn split_blocks(sizes: &[u32; 8], body_len: usize, block0_len: usize) -> Vec<(usize, usize)> {
    let mut acc = 0usize;
    let mut ends: Vec<usize> = Vec::new();
    for &s in sizes {
        acc = acc.saturating_add(s as usize);
        ends.push(acc);
        if acc == body_len {
            break;
        }
        if acc > body_len || s == 0 {
            ends.clear();
            break;
        }
    }
    if ends.last() != Some(&body_len) || ends.len() < 2 {
        let len = body_len.saturating_sub(block0_len);
        return if len > 0 { vec![(0, len)] } else { Vec::new() };
    }
    let mut spans = Vec::with_capacity(ends.len() - 1);
    for i in 1..ends.len() {
        let start = ends[i - 1] - block0_len;
        spans.push((start, ends[i] - ends[i - 1]));
    }
    spans
}

/// Read a NUL-terminated UTF-8 string out of the heap, bounded by the entry.
fn read_cstr(scene: &[u8], off: usize, end: usize) -> String {
    if off >= end || end > scene.len() {
        return String::new();
    }
    let slice = &scene[off..end];
    let n = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    String::from_utf8_lossy(&slice[..n])
        .replace('\\', "/")
        .to_string()
}

/// Split an extracted asset into its optional 36-byte header and the body.
fn split_header(asset: &[u8]) -> Result<(Option<Vec<u8>>, &[u8])> {
    if asset.len() < 8 {
        return Err(ToolkitError::Parse("zone asset too small".into()));
    }
    // The body always starts with the DAT1 magic; if the first four bytes are
    // not it, the 36-byte asset header is present.
    let magic = LE::read_u32(&asset[0..4]);
    if magic == crate::core::dat1::DAT1_MAGIC {
        Ok((None, asset))
    } else if asset.len() > ASSET_HEADER_LEN
        && LE::read_u32(&asset[ASSET_HEADER_LEN..ASSET_HEADER_LEN + 4])
            == crate::core::dat1::DAT1_MAGIC
    {
        Ok((
            Some(asset[..ASSET_HEADER_LEN].to_vec()),
            &asset[ASSET_HEADER_LEN..],
        ))
    } else {
        Err(ToolkitError::Parse(
            "zone asset has neither a DAT1 magic nor a 36-byte asset header".into(),
        ))
    }
}

fn ser_u64_str<S: serde::Serializer>(v: &u64, s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_str(&format!("{v:#018X}"))
}

fn de_u64_str<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<u64, D::Error> {
    use serde::Deserialize;
    let s = String::deserialize(d)?;
    let t = s.trim_start_matches("0x").trim_start_matches("0X");
    u64::from_str_radix(t, 16).map_err(serde::de::Error::custom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_names_are_stable() {
        assert_eq!(kind_name(KIND_MODEL), "model");
        assert_eq!(kind_name(KIND_ACTOR), "actor");
        assert_eq!(kind_name(KIND_IMPOSTOR), "impostor");
    }

    /// A heap of kind-0 entries with the given lengths, laid out the way the
    /// shipped section does it.
    fn heap(lens: &[usize]) -> (Vec<u8>, Vec<u32>) {
        let mut scene = Vec::new();
        let mut offsets = Vec::new();
        for (i, len) in lens.iter().enumerate() {
            offsets.push(scene.len() as u32);
            let mut e = vec![0u8; *len];
            // The high byte of the length word is 0x80 on every shipped entry.
            LE::write_u32(&mut e[OFF_SIZE..OFF_SIZE + 4], 0x8000_0000 | *len as u32);
            let extras = ((*len - MODEL_INST_LEN) / 0x20) as u16;
            LE::write_u16(&mut e[OFF_MODEL_INDEX..OFF_MODEL_INDEX + 2], i as u16);
            LE::write_u16(&mut e[OFF_MODEL_INDEX + 2..OFF_MODEL_INDEX + 4], extras);
            scene.extend_from_slice(&e);
        }
        (scene, offsets)
    }

    #[test]
    fn entry_len_reads_the_heap_entry_s_own_length() {
        // The middle entry carries two extra records.
        let (scene, offsets) = heap(&[MODEL_INST_LEN, MODEL_INST_LEN + 0x40, MODEL_INST_LEN]);
        assert_eq!(Zone::entry_len(&scene, offsets[0] as usize), Some(MODEL_INST_LEN));
        assert_eq!(
            Zone::entry_len(&scene, offsets[1] as usize),
            Some(MODEL_INST_LEN + 0x40)
        );
        // The entry with extras declares the count the length implies.
        let at = offsets[1] as usize + OFF_MODEL_INDEX + 2;
        assert_eq!(LE::read_u16(&scene[at..at + 2]), 2);
        // An entry that would run past the section is rejected, not clamped.
        assert_eq!(Zone::entry_len(&scene[..scene.len() - 4], offsets[2] as usize), None);
        assert_eq!(Zone::entry_len(&scene, scene.len()), None);
    }

    /// The bug a fixed 320-byte stride produces: the insertion point lands
    /// inside the last entry whenever anything in the run carries extras.
    #[test]
    fn heap_run_end_follows_the_declared_lengths() {
        // The last entry of the run is the one with extras, which is where a
        // fixed stride does its damage: it puts the end of the run 0x40 bytes
        // inside that entry, so an insertion there would cut it in half.
        let (scene, offsets) = heap(&[MODEL_INST_LEN, MODEL_INST_LEN, MODEL_INST_LEN + 0x40]);
        assert_eq!(heap_run_end(&scene, &offsets), Some(scene.len()));
        let fixed_stride = offsets.iter().map(|&o| o as usize + MODEL_INST_LEN).max();
        assert_eq!(fixed_stride, Some(scene.len() - 0x40));

        // Extras anywhere else still move every later offset along.
        let (scene, offsets) = heap(&[MODEL_INST_LEN + 0x20, MODEL_INST_LEN]);
        assert_eq!(offsets, vec![0, MODEL_INST_LEN as u32 + 0x20]);
        assert_eq!(heap_run_end(&scene, &offsets), Some(scene.len()));

        assert!(heap_run_end(&scene, &[scene.len() as u32]).is_none());
    }

    #[test]
    fn blocks_split_on_the_first_prefix_that_sums_to_the_body() {
        // A shipped art zone: block0 + physics + the raw geometry appendix.
        let sizes = [1_382_288, 490_864, 73_952, 0, 0, 0, 0, 0];
        let body = 1_382_288 + 490_864 + 73_952;
        assert_eq!(
            split_blocks(&sizes, body, 1_382_288),
            vec![(0, 490_864), (490_864, 73_952)]
        );

        // An impostor zone: sizes[4..] are a texture descriptor, not sizes, so
        // the walk must stop at k = 4 rather than adding them.
        let sizes = [1268, 48, 34_440, 262_144, 0x1000_0150, 0x10800, 0x301_0248, 0x480_0000];
        let body = 1268 + 48 + 34_440 + 262_144;
        assert_eq!(
            split_blocks(&sizes, body, 1268),
            vec![(0, 48), (48, 34_440), (34_488, 262_144)]
        );

        // A plain two-block zone.
        let sizes = [18_536, 19_924, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            split_blocks(&sizes, 18_536 + 19_924, 18_536),
            vec![(0, 19_924)]
        );

        // A header we cannot trust falls back to one opaque tail block.
        let sizes = [10, 999, 0, 0, 0, 0, 0, 0];
        assert_eq!(split_blocks(&sizes, 100, 10), vec![(0, 90)]);
    }
}
