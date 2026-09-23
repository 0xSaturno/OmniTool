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
//! Editing is deliberately restricted to **in-place patches of equal length**
//! (transforms and scales). No section ever changes size, so no string offset
//! and no inter-section byte offset can be invalidated, and re-serialising an
//! untouched zone reproduces the original bytes exactly.

use std::collections::HashMap;

use crate::core::dat1::Dat1;
use crate::core::error::{Result, ToolkitError};
use byteorder::{ByteOrder, LE};
use serde::{Deserialize, Serialize};

pub const ZONE_TYPE_ID: u32 = 0x1F390AA0;
pub const ASSET_HEADER_LEN: usize = 36;

pub const T_SCENE_OBJECTS: u32 = 0x06AB_CAB2;
pub const T_MODEL_INSTS: u32 = 0x6987_F172;
pub const T_MODEL_NAMES: u32 = 0xC6A5_905E;
pub const T_ACTORS: u32 = 0x7068_2CB8;
pub const T_ACTOR_NAMES: u32 = 0xDC62_5B3D;
pub const T_ACTOR_REFS: u32 = 0x7868_4035;
pub const T_ACTOR_PRIUSES: u32 = 0x50ED_C53D;
/// 24-byte `ActorGroupBuilt` rows.
pub const T_ACTOR_GROUPS: u32 = 0x0CF5_8A6E;
/// `u16[]` of actor-row indices; groups take a `(first, count)` slice of it.
pub const T_ACTOR_GROUP_DATA: u32 = 0x0410_71EF;
/// `u32[]` string offsets, one per named group.
pub const T_ACTOR_GROUP_NAMES: u32 = 0xC496_8A44;
/// 40-byte `ScriptActionBuilt` rows.
pub const T_SCRIPT_ACTIONS: u32 = 0x5E54_ACCF;
/// 12-byte plug rows that the action rows slice by index/count.
pub const T_SCRIPT_PLUGS: u32 = 0xBEAB_52E7;
/// DDL blob store addressed by `ScriptAction::prius_offset/prius_size`.
pub const T_SCRIPT_PRIUSES: u32 = 0x2300_D240;
/// 16-byte vertex-paint index rows for model instances, and their colour store.
pub const T_VERTEX_PAINT_INFO: u32 = 0x5736_A46F;
pub const T_VERTEX_PAINT_COLOR: u32 = 0x9CCA_A06F;
/// The same pair for actor-owned instances.
pub const T_ACTOR_VERTEX_PAINT_INFO: u32 = 0x758B_AFBD;
pub const T_ACTOR_VERTEX_PAINT_COLOR: u32 = 0xF435_AE9C;
pub const T_LIGHTS: u32 = 0x97FF_6EB5;
pub const T_VOLUMES: u32 = 0xBDAB_2B0D;
pub const T_DECALS: u32 = 0xBCF4_3558;
pub const T_VFX_INSTS: u32 = 0x927C_4EC3;
pub const T_IMPOSTORS: u32 = 0xDC31_1FC3;
pub const T_DECAL_GEOMETRY: u32 = 0x6575_12BB;

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
///   +0x00 i32 name_index      -> Zone Actor Names
///   +0x04 i32 asset_index     -> Zone Actor Asset Paths
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
    /// Index into Zone Actor Names.
    pub name_index: i32,
    /// Index into Zone Actor Asset Paths.
    pub asset_index: i32,
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

impl ActorRow {
    /// True when the actor only exists to be cloned by a spawner.
    pub fn is_template(&self) -> bool {
        self.flags & (1 << 2) != 0
    }

    /// True when the actor is active as soon as its zone loads.
    pub fn starts_active(&self) -> bool {
        self.flags & (1 << 3) != 0
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
    pub uid: u64,
    pub is_placed: bool,
    /// The group's members, as Zone Actors row indices.
    pub members: Vec<u16>,
}

/// A 40-byte row of Zone Script Actions (`ScriptActionBuilt`).
///
/// ```text
///   +0x00 u64 uid
///   +0x08 u64 graph_uid          the script graph this action belongs to
///   +0x10 u16 in_plug_count
///   +0x12 u16 out_plug_index     -> Zone Script Plugs (12-byte rows)
///   +0x14 u16 out_plug_count
///   +0x16 u16 var_plug_index
///   +0x18 u16 var_plug_count
///   +0x1A u16 action_name_index
///   +0x1C u32 action_name_hash
///   +0x20 u32 prius_offset       -> Zone Script Priuses
///   +0x24 u32 prius_size
/// ```
///
/// The out/var plug slices were previously called "link plugs" and "param
/// plugs" here.
///
/// Measured over 215 041 action rows in 620 of 4 063 shipped zones:
///
/// * `crc32(name)` of the string `action_name_index` selects out of Zone Script
///   Strings equals `action_name_hash` on **215 041 of 215 041** rows, which
///   pins both fields to these offsets;
/// * out-plug and var-plug slices are slices of the *same* Zone Script Plugs
///   array — together they tile all 465 730 plug rows with **0 overlaps and 0
///   unclaimed rows**;
/// * 146 199 slices are empty and every one of them stores the index
///   [`NO_PLUG`] with a zero count;
/// * every `prius_offset + prius_size` stays inside Zone Script Priuses.
///
/// `in_plug_count` has no index of its own: in-plugs are named by the *other*
/// actions' out-plug rows. It is 0 on 43 521 rows and 1 on 132 124.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptAction {
    pub index: u32,
    #[serde(serialize_with = "ser_u64_str")]
    pub uid: u64,
    #[serde(serialize_with = "ser_u64_str")]
    pub graph_uid: u64,
    pub in_plug_count: u16,
    pub out_plug_index: u16,
    pub out_plug_count: u16,
    pub var_plug_index: u16,
    pub var_plug_count: u16,
    pub action_name_index: u16,
    pub action_name_hash: u32,
    pub prius_offset: u32,
    pub prius_size: u32,
}

/// A plug index of `0xFFFF` means the action owns no slice of that kind; the
/// matching count is 0 on every shipped row.
pub const NO_PLUG: u16 = u16::MAX;

impl ScriptAction {
    /// The action's out-plug rows inside Zone Script Plugs, if it has any.
    ///
    /// The range comes straight from the file, so it is only as trustworthy as
    /// the asset. Slice with [`Self::out_plugs_within`] instead whenever the row
    /// count is to hand — a hand-edited or truncated zone would otherwise panic
    /// the caller rather than fail the parse.
    pub fn out_plugs(&self) -> Option<std::ops::Range<usize>> {
        plug_range(self.out_plug_index, self.out_plug_count)
    }

    /// The action's var-plug rows inside Zone Script Plugs, if it has any.
    ///
    /// Unchecked, like [`Self::out_plugs`]; prefer [`Self::var_plugs_within`].
    pub fn var_plugs(&self) -> Option<std::ops::Range<usize>> {
        plug_range(self.var_plug_index, self.var_plug_count)
    }

    /// [`Self::out_plugs`] bounded by the number of plug rows the zone actually
    /// has. `None` when the action owns no slice or the slice leaves the section.
    pub fn out_plugs_within(&self, rows: usize) -> Option<std::ops::Range<usize>> {
        bounded(self.out_plugs(), rows)
    }

    /// [`Self::var_plugs`] bounded by the number of plug rows the zone has.
    pub fn var_plugs_within(&self, rows: usize) -> Option<std::ops::Range<usize>> {
        bounded(self.var_plugs(), rows)
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

/// A 16-byte row of Zone Vertex Paint Info (`ZoneVertexPaintInfo`).
///
/// ```text
///   +0x00 u64 uid           the instance id of the scene object it paints
///   +0x08 i32 count         vertices, i.e. `count * 4` bytes of colour
///   +0x0C u32 data_offset   byte offset into the matching Color section
/// ```
///
/// Two independent pairs exist: model instances use
/// [`T_VERTEX_PAINT_INFO`] / [`T_VERTEX_PAINT_COLOR`], actor-owned instances
/// use [`T_ACTOR_VERTEX_PAINT_INFO`] / [`T_ACTOR_VERTEX_PAINT_COLOR`].
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
    pub scene_objects: Vec<SceneObject>,
    pub actors: Vec<ActorRow>,
    pub actor_groups: Vec<ActorGroup>,
    pub script_actions: Vec<ScriptAction>,
    /// Both vertex-paint index tables, model-instance rows first.
    pub vertex_paint: Vec<VertexPaint>,
    pub model_ids: Vec<u64>,
    /// Model paths parallel to `model_ids`, read from the zone's own string pool.
    pub model_paths: Vec<String>,
    /// Decal heap offset -> its mesh span inside block 2.
    pub decal_geometry: HashMap<u32, GeoRef>,
    pub edits: Vec<ZoneEdit>,
}

/// Read a "struct of arrays" name table: `{ u64 id[n] }{ u32 string_offset[n] }`.
/// Used by Zone Model Names, Zone Actor Asset Paths and Zone Visual Effect Names.
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
        paths.push(
            dat1.get_string(so)
                .unwrap_or_default()
                .replace('\\', "/"),
        );
    }
    (ids, paths)
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
            actor_groups: Vec::new(),
            script_actions: Vec::new(),
            vertex_paint: Vec::new(),
            model_ids: Vec::new(),
            model_paths: Vec::new(),
            decal_geometry: HashMap::new(),
            edits: Vec::new(),
        };
        zone.build_index();
        Ok(zone)
    }

    fn build_index(&mut self) {
        let scene = match self.dat1.get_section_data(T_SCENE_OBJECTS) {
            Some(s) => s.to_vec(),
            None => return,
        };

        // Zone Model Names is { u64 id[n] }{ u32 string_offset[n] }, not a flat u64 array.
        let (ids, paths) = read_soa_table(&self.dat1, T_MODEL_NAMES);
        self.model_ids = ids;
        self.model_paths = paths;

        let mut offsets: Vec<u32> = Vec::new();
        for tag in [
            T_MODEL_INSTS,
            T_LIGHTS,
            T_VOLUMES,
            T_DECALS,
            T_VFX_INSTS,
            // Without this table every kind-6 entry is invisible, and an
            // impostor zone (645 of them ship, 456 on megalopolis alone)
            // parses to zero scene objects.
            T_IMPOSTORS,
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
            self.dat1.get_section_data(T_DECALS).map(<[u8]>::to_vec),
            self.dat1.get_section_data(T_DECAL_GEOMETRY).map(<[u8]>::to_vec),
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
            .get_section_data(T_ACTORS)
            .map(|a| {
                a.chunks_exact(32)
                    .map(|r| {
                        let mut b = [0u8; 32];
                        b.copy_from_slice(r);
                        b
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Zone Actor Names is a plain u32[] of string offsets.
        let name_offsets: Vec<u32> = self
            .dat1
            .get_section_data(T_ACTOR_NAMES)
            .map(|n| n.chunks_exact(4).map(LE::read_u32).collect())
            .unwrap_or_default();
        // Zone Actor Asset Paths is a struct-of-arrays table like Zone Model Names.
        let (actor_ids, actor_paths) = read_soa_table(&self.dat1, T_ACTOR_REFS);

        for (i, r) in raw_rows.iter().enumerate() {
            let name_index = LE::read_i32(&r[0..4]);
            let asset_index = LE::read_i32(&r[4..8]);
            let scene_offset = LE::read_u32(&r[8..12]);
            let prius_index = LE::read_i32(&r[12..16]);
            let prius_count = LE::read_i32(&r[16..20]);
            let flags = LE::read_u16(&r[20..22]);
            let pad = LE::read_u16(&r[22..24]);
            let instance_id = LE::read_u64(&r[24..32]);

            let name = usize::try_from(name_index)
                .ok()
                .and_then(|n| name_offsets.get(n))
                .and_then(|&o| self.dat1.get_string(o))
                .unwrap_or_default();
            let ai = usize::try_from(asset_index).ok();
            let asset_id = ai.and_then(|n| actor_ids.get(n)).copied().unwrap_or(0);
            let asset_path = ai
                .and_then(|n| actor_paths.get(n))
                .cloned()
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
        let Some(rows) = self.dat1.get_section_data(T_ACTOR_GROUPS).map(<[u8]>::to_vec) else {
            return;
        };
        let members: Vec<u16> = self
            .dat1
            .get_section_data(T_ACTOR_GROUP_DATA)
            .map(|d| d.chunks_exact(2).map(LE::read_u16).collect())
            .unwrap_or_default();
        let name_offsets: Vec<u32> = self
            .dat1
            .get_section_data(T_ACTOR_GROUP_NAMES)
            .map(|n| n.chunks_exact(4).map(LE::read_u32).collect())
            .unwrap_or_default();

        for (i, r) in rows.chunks_exact(24).enumerate() {
            let name_index = LE::read_i32(&r[0..4]);
            let first_actor_index = LE::read_i32(&r[4..8]);
            let actor_count = LE::read_u16(&r[8..10]);
            let is_global = LE::read_u16(&r[10..12]) != 0;
            let debug_color = LE::read_u32(&r[12..16]);
            let uid = LE::read_u64(&r[16..24]);

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
                uid,
                is_placed: is_placed(uid),
                members: slice.to_vec(),
            });
        }
    }

    /// Zone Script Actions, 40-byte rows.
    fn read_script_actions(&mut self) {
        let Some(rows) = self.dat1.get_section_data(T_SCRIPT_ACTIONS).map(<[u8]>::to_vec) else {
            return;
        };
        for (i, r) in rows.chunks_exact(40).enumerate() {
            self.script_actions.push(ScriptAction {
                index: i as u32,
                uid: LE::read_u64(&r[0..8]),
                graph_uid: LE::read_u64(&r[8..16]),
                in_plug_count: LE::read_u16(&r[16..18]),
                out_plug_index: LE::read_u16(&r[18..20]),
                out_plug_count: LE::read_u16(&r[20..22]),
                var_plug_index: LE::read_u16(&r[22..24]),
                var_plug_count: LE::read_u16(&r[24..26]),
                action_name_index: LE::read_u16(&r[26..28]),
                action_name_hash: LE::read_u32(&r[28..32]),
                prius_offset: LE::read_u32(&r[32..36]),
                prius_size: LE::read_u32(&r[36..40]),
            });
        }
    }

    /// Both Vertex Paint Info tables, 16-byte rows.
    fn read_vertex_paint(&mut self) {
        for (tag, actor) in [
            (T_VERTEX_PAINT_INFO, false),
            (T_ACTOR_VERTEX_PAINT_INFO, true),
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
                    model_path = self.model_paths.get(mi as usize).cloned().unwrap_or_default();
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
            T_ACTOR_VERTEX_PAINT_COLOR
        } else {
            T_VERTEX_PAINT_COLOR
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
            .get_section_data(T_SCENE_OBJECTS)
            .ok_or(ToolkitError::SectionNotFound(T_SCENE_OBJECTS))?
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
        self.dat1.set_section_data(T_SCENE_OBJECTS, scene)?;
        self.edits.push(ZoneEdit {
            section: T_SCENE_OBJECTS,
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
            .get_section_data(T_SCENE_OBJECTS)
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
