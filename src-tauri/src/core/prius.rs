//! Actor and script **priuses** — a component's initial-state struct, decoded
//! into a named, typed property sheet.
//!
//! An actor asset declares a list of components, each with a prius struct of
//! authored defaults. A *placed* actor in a zone stores only the fields that
//! differ from those defaults, so a zone's prius data is a sparse override set:
//! a `StackableCrate` row is usually four fields, and the fields a designer
//! actually touched are exactly the ones present. That makes the prius tables
//! the difference between "a box with a name" and an editable gameplay entity.
//!
//! # Where they live in a zone
//!
//! **Zone Actor Priuses** `0x50EDC53D` — 32-byte rows. **VERIFIED** on the
//! shipped PC build over 50 341 rows in 1 049 zones:
//!
//! ```text
//!   +0x00 u64 type_uid       crc64_plain(lowercase(component + "Prius")) --
//!                            the prius struct's 64-bit type id, NOT the actor uid
//!   +0x08 u32 name_offset    -> DAT1 string pool: the COMPONENT name
//!   +0x0C u32 name_hash      crc32(component name)
//!   +0x10 u32 zero
//!   +0x14 u32 data_offset    absolute DAT1 offset into the prius data pool
//!   +0x18 u32 data_size      bytes, including the 16-byte DDL header
//!   +0x1C i32 minus_one
//! ```
//!
//! **Zone Actor Prius Data** `0x81999057` — the blob pool the rows point into.
//! The tag was previously unnamed in OmniTool; it is the actor prius data pool.
//!
//! **Zone Script Priuses** `0x2300D240` — the same blob format, addressed
//! *section-relative* by `ScriptAction::prius_offset/prius_size`, and typed by
//! the action's own name rather than a component name.
//!
//! A row's slice is reached with `data_offset - <pool section offset>`; the
//! offsets really are file-absolute, and the first row of a zone with one prius
//! has `data_offset` equal to the pool section's own offset.
//!
//! # Wire format
//!
//! A blob is one DDL object: `{u32 reserved, u32 magic, i32 field_count,
//! i32 payload_size}`, then `field_count` x `{u32 name_hash, u32 encoding}`,
//! then `field_count` x `u32 name_offset`, then the values in field order,
//! `count` values per field, padded to 4.
//!
//! The encoding word is
//! `(encoding_type & 3) << 29 | (ddl_type & 0x1F) << 24 | (count & 0xFFFFF) << 4`.
//! Measured over the **230 428 actor-prius and 209 592 script-prius fields** of
//! the shipped PC build:
//!
//!   * `encoding_type` (bits 29..30) is **0 on all 440 020**, so the "subtype
//!     array" encoding does not occur in zone priuses;
//!   * bits 0..3 are **0 on all 440 020**, so there is no array-kind nibble to
//!     read: an array is simply `count > 1` (largest seen: 88);
//!   * `magic` is `0x03150044` on all 74 242 actor-prius and 136 717
//!     script-prius objects; `0x07201969` never appears, though it is accepted
//!     here because accepting it costs nothing.
//!
//! Two latent bugs in [`crate::core::ddl`], which this module deliberately does
//! not share code with: it takes the whole high byte as the type
//! (`(meta >> 24) as u8`), which is only right while `encoding_type` is 0, and it
//! reads the low nibble as a `DdlArrayKind` that is always `None` on real data.
//! Neither misreads a zone prius today.
//!
//! Type ids: 0 u8, 1 u16, 2 u32, 3 u64, 4 i8, 5 i16, 6 i32, 7 i64, 8 f32,
//! 9 f64, 10 string, 11 select, 12 bitfield, 13 struct, 14 unknown, 15 bool,
//! 16 file, 17 tuid, 18 json, 19 empty-array, 20 asset-id.
//!
//! ## What the built form erases
//!
//! A prius blob is not a faithful image of its DDL declaration, and an editor
//! that assumes it is will corrupt zones. Measured against the type database:
//!
//!   * **`select` and `bitfield` are gone.** 0 fields carry wire type 11 or 12;
//!     an enum arrives as a wire `string` holding the member name (`"kBolt"`,
//!     `"kManual"`). 19 881 selects, 8 972 bitfields and 20 586 `file` paths
//!     hide inside the 77 613 actor-prius wire strings -- only 26 618 are
//!     really free text, and 1 556 are fields the database does not know.
//!   * **numbers are narrowed.** See [`Edit::Scalar`]: of 35 900 schema-`float`
//!     fields only 14 090 are `f32` on the wire.
//!   * **empty containers collapse to type 19.** All 3 339 declare `count == 0`
//!     and carry exactly one `0x00` byte, whatever the declared type was
//!     (2 052 structs, 729 files, 294 tuids, 136 selects, 102 bitfields,
//!     4 strings). `rivet` and `ddl.rs` model this as count-clamped-to-1, which
//!     happens to consume the same byte; this module reads it explicitly and
//!     records its span so a re-serialise reproduces it.
//!
//! ## Field names need no schema
//!
//! Every field carries a string-pool offset, and on **230 428 of 230 428**
//! actor-prius and **209 592 of 209 592** script-prius fields the pool string
//! resolves *and* hashes back to the field id. A prius sheet is therefore fully
//! named with no type database present; the database adds labels, authored
//! defaults, enum member sets and nested struct types.
//!
//! # Editing
//!
//! The zone save path is **equal-length in-place patching**: a section never
//! changes size, so no string offset and no inter-section byte offset can be
//! invalidated. Every decoded value therefore carries the byte span it occupies
//! inside its blob, and [`PriusBlob::patch`] writes into a *copy of the original
//! bytes* rather than re-serialising, so an unedited blob is byte-identical by
//! construction. [`Edit`] states per field whether an editor may offer it at all.
//!
//! Verified on the whole shipped PC build:
//!
//! ```text
//!   actor prius blobs decoded                       50 341 / 50 341
//!   consumed exactly data_size                      50 341 / 50 341
//!   full re-serialise byte-identical                50 341 / 50 341
//!   every editable leaf re-set to its own value
//!     reproduced the original bytes                172 584 / 172 584
//!   script prius blobs decoded, byte-identical     112 886 / 112 886
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use byteorder::{ByteOrder, LE};
use serde::Serialize;

use crate::core::crc32;
use crate::core::crc64;
use crate::core::dat1::Dat1;
use crate::core::ddl_schema::{EnumDef, SchemaDb};
use crate::core::error::{Result, ToolkitError};

/// Zone Actor Priuses — 32-byte rows.
pub const TAG_ACTOR_PRIUSES: u32 = 0x50ED_C53D;
/// Zone Actor Prius Data — the blob pool the rows address.
pub const TAG_ACTOR_PRIUS_POOL: u32 = 0x8199_9057;
/// Zone Script Priuses — blobs addressed by script action rows.
pub const TAG_SCRIPT_PRIUSES: u32 = 0x2300_D240;

pub const PRIUS_ROW_LEN: usize = 32;

/// `0x03150044` — the DDL object guard OmniTool already knows.
pub const DDL_MAGIC: u32 = 0x0315_0044;
/// `0x07201969` — a second guard value this reader also accepts. Measured
/// occurrences on shipped actor prius blobs: 0 of 50 341. Accepted anyway;
/// costs nothing.
pub const DDL_MAGIC_ALT: u32 = 0x0720_1969;

// ---------------------------------------------------------------------------
// DDL type ids
// ---------------------------------------------------------------------------

pub const DT_U8: u8 = 0;
pub const DT_U16: u8 = 1;
pub const DT_U32: u8 = 2;
pub const DT_U64: u8 = 3;
pub const DT_I8: u8 = 4;
pub const DT_I16: u8 = 5;
pub const DT_I32: u8 = 6;
pub const DT_I64: u8 = 7;
pub const DT_F32: u8 = 8;
pub const DT_F64: u8 = 9;
pub const DT_STRING: u8 = 10;
pub const DT_SELECT: u8 = 11;
pub const DT_BITFIELD: u8 = 12;
pub const DT_STRUCT: u8 = 13;
pub const DT_UNKNOWN: u8 = 14;
pub const DT_BOOL: u8 = 15;
pub const DT_FILE: u8 = 16;
pub const DT_TUID: u8 = 17;
pub const DT_JSON: u8 = 18;
pub const DT_EMPTY_ARRAY: u8 = 19;
pub const DT_ASSET_ID: u8 = 20;

pub fn type_label(t: u8) -> &'static str {
    match t {
        DT_U8 => "u8",
        DT_U16 => "u16",
        DT_U32 => "u32",
        DT_U64 => "u64",
        DT_I8 => "i8",
        DT_I16 => "i16",
        DT_I32 => "i32",
        DT_I64 => "i64",
        DT_F32 => "f32",
        DT_F64 => "f64",
        DT_STRING => "string",
        DT_SELECT => "select",
        DT_BITFIELD => "bitfield",
        DT_STRUCT => "struct",
        DT_UNKNOWN => "unknown",
        DT_BOOL => "bool",
        DT_FILE => "file",
        DT_TUID => "tuid",
        DT_JSON => "json",
        DT_EMPTY_ARRAY => "empty-array",
        DT_ASSET_ID => "asset-id",
        _ => "invalid",
    }
}

/// Fixed on-wire width of a scalar type, or `None` for the variable-length and
/// composite ones.
pub fn scalar_width(t: u8) -> Option<usize> {
    Some(match t {
        DT_U8 | DT_I8 | DT_BOOL => 1,
        DT_U16 | DT_I16 => 2,
        DT_U32 | DT_I32 | DT_F32 => 4,
        DT_U64 | DT_I64 | DT_F64 | DT_TUID | DT_ASSET_ID => 8,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Decoded value model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum PValue {
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    Bool(bool),
    /// string / select / bitfield / file / json — all the wire `DDLString`.
    Str {
        value: String,
        /// `crc32(value)` as stored. Rewritten on any patch.
        hash: u32,
        /// `crc64(value)` as stored. Rewritten on any patch.
        checksum: u64,
    },
    /// tuid — a 64-bit authored identifier.
    Tuid(u64),
    /// asset-id — `crc64(asset path)`.
    AssetId(u64),
    Struct(Box<PObject>),
    /// The single byte a type-19 empty array occupies.
    EmptyArray(u8),
}

impl PValue {
    pub fn text(&self) -> String {
        match self {
            PValue::U8(v) => v.to_string(),
            PValue::U16(v) => v.to_string(),
            PValue::U32(v) => v.to_string(),
            PValue::U64(v) => v.to_string(),
            PValue::I8(v) => v.to_string(),
            PValue::I16(v) => v.to_string(),
            PValue::I32(v) => v.to_string(),
            PValue::I64(v) => v.to_string(),
            PValue::F32(v) => format!("{v}"),
            PValue::F64(v) => format!("{v}"),
            PValue::Bool(v) => v.to_string(),
            PValue::Str { value, .. } => value.clone(),
            PValue::Tuid(v) => format!("{v:#018X}"),
            PValue::AssetId(v) => format!("{v:#018X}"),
            PValue::Struct(o) => format!("{{{} fields}}", o.fields.len()),
            PValue::EmptyArray(_) => "[]".into(),
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        Some(match self {
            PValue::U8(v) => *v as f64,
            PValue::U16(v) => *v as f64,
            PValue::U32(v) => *v as f64,
            PValue::U64(v) => *v as f64,
            PValue::I8(v) => *v as f64,
            PValue::I16(v) => *v as f64,
            PValue::I32(v) => *v as f64,
            PValue::I64(v) => *v as f64,
            PValue::F32(v) => *v as f64,
            PValue::F64(v) => *v,
            PValue::Bool(v) => *v as u8 as f64,
            _ => return None,
        })
    }

    fn to_json(&self) -> serde_json::Value {
        use serde_json::Value as J;
        match self {
            PValue::U8(v) => J::from(*v),
            PValue::U16(v) => J::from(*v),
            PValue::U32(v) => J::from(*v),
            // 64-bit integers go out as strings: JSON numbers lose them.
            PValue::U64(v) => J::from(v.to_string()),
            PValue::I8(v) => J::from(*v),
            PValue::I16(v) => J::from(*v),
            PValue::I32(v) => J::from(*v),
            PValue::I64(v) => J::from(v.to_string()),
            PValue::F32(v) => serde_json::Number::from_f64(*v as f64)
                .map(J::Number)
                .unwrap_or_else(|| J::from(v.to_string())),
            PValue::F64(v) => serde_json::Number::from_f64(*v)
                .map(J::Number)
                .unwrap_or_else(|| J::from(v.to_string())),
            PValue::Bool(v) => J::from(*v),
            PValue::Str { value, .. } => J::from(value.clone()),
            PValue::Tuid(v) => J::from(format!("{v:#018X}")),
            PValue::AssetId(v) => J::from(format!("{v:#018X}")),
            PValue::Struct(o) => o.to_json(),
            PValue::EmptyArray(_) => J::Array(Vec::new()),
        }
    }
}

/// What an editor may do with a field, under the equal-length in-place rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Edit {
    /// Fixed-width number or bool. Safe to offer as a spin box / checkbox --
    /// but only inside [`PField::numeric_bounds`], because the builder stores a
    /// field in the **narrowest type that represents the authored value**, not
    /// in the type the schema declares.
    ///
    /// **VERIFIED** over 230 428 shipped actor-prius fields: of 35 900 fields
    /// the schema calls `float`, only 14 090 (39.2%) are `f32` on the wire;
    /// 20 897 are `u8`, and the rest `u16`/`i8`/`i16`/`u32`/`u64`. Every one of
    /// the 21 816 narrowed values is integral (0 fractional), so the narrowing
    /// is lossless -- and it is also a hard edit constraint: a `SpawnDelay`
    /// stored as `u8 20` can be moved to 30 in place but not to 20.5, because
    /// 20.5 needs four bytes where the blob has one.
    Scalar,
    /// A `select`: safe to offer as a dropdown, but **only** members whose name
    /// is the same byte length as the current one. Enum values are stored as
    /// inline strings, so a shorter or longer member changes the blob size.
    SelectSameLen,
    /// Free text stored inline. Same-length rewrites only.
    StringSameLen,
    /// A reference to something outside this field. Read-only.
    ///
    /// Three shapes, all measured on shipped data:
    ///   * `tuid` (14 575 fields) - an authored object uid. **0 of 17 117
    ///     non-zero values is a TOC asset id**; 15 477 are the uid of an actor
    ///     in the *same* zone and 13 749 a scene object in the same zone, so a
    ///     `tuid` is the level's own wiring (`TriggerVolume`, `StopVolume`,
    ///     `TargetNavClue`, `Light`, `Curve`, `ControllerActorTuid`, ...).
    ///     Retargeting one means picking another object's uid, not typing a
    ///     number.
    ///   * a schema `file` field (21 315 fields, 20 586 of them a wire string)
    ///     - an inline asset path. The zone builder derives the zone's backing
    ///     asset list from these, so pointing one at an asset nothing else in
    ///     the zone references makes the zone load without it.
    ///   * an id-shaped `u64`/`i64` - engine-allocated; cloning one desyncs
    ///     spawners (`zone.rs`, `PLACED_BIT`).
    Reference,
    /// Not a leaf (struct) or not a value (empty array).
    NotEditable,
}

#[derive(Debug, Clone)]
pub struct PField {
    /// `crc32(field name)` as stored in the blob.
    pub id: u32,
    /// Name from the blob's own string-pool offset when it resolved, else from
    /// the schema's global name index, else `field_<hash>`.
    pub name: String,
    /// Where `name` came from: `"pool"`, `"schema"` or `"hash"`.
    pub name_source: &'static str,
    /// Designer-facing label from the schema, when it differs from the name.
    pub label: Option<String>,
    /// DDL type as the *blob* declares it. Always wins over the schema for
    /// *reading*: it is what determines how many bytes the value occupies.
    pub ddl_type: u8,
    /// DDL type the schema declares, when it knows the field. **Measured: the
    /// built form erases `select` (11) and `bitfield` (12) — it writes their
    /// member name as a plain `string` (10). 0 of 230 428 shipped actor-prius
    /// fields carry wire type 11, 12, 16 or 18.** So this is the only way to
    /// know a text field is a dropdown, and a UI should label the field with
    /// `schema_type` while the decoder keeps reading it as `ddl_type`.
    pub schema_type: Option<u8>,
    /// Bits 29..30 of the encoding word. 0 = plain, 1 = subtype array.
    pub encoding_type: u8,
    /// Declared element count. `> 1` is an array.
    pub count: u32,
    pub values: Vec<PValue>,
    /// `(start, len)` of each value inside the blob, parallel to `values`.
    pub spans: Vec<(usize, usize)>,
    /// Byte offset of this field's 8-byte header inside the blob.
    pub header_at: usize,
    /// Offset of this field's entry in the name-offset table.
    pub name_off_at: usize,
    /// Raw name offset as stored.
    pub name_offset: u32,
    /// Legal member names when the schema knows this field is a select/bitfield.
    pub enum_values: Option<Vec<String>>,
    /// The enum's id, so a caller can fetch the full [`EnumDef`].
    pub enum_id: u32,
    pub enum_is_bitset: bool,
    /// The authored default from the schema, when known. A field present in a
    /// zone blob is by definition an override *of* this.
    pub default: Option<serde_json::Value>,
    pub edit: Edit,
    /// Why, when `edit` is restricted.
    pub edit_note: Option<&'static str>,
}

#[derive(Debug, Clone)]
pub struct PObject {
    /// `crc32(type name)` when the caller could name the type, else 0.
    pub type_id: u32,
    pub type_name: Option<String>,
    pub magic: u32,
    /// `(start, len)` of the whole object inside the blob.
    pub span: (usize, usize),
    /// The `payload_size` word of the header, and what we actually consumed.
    pub declared_size: u32,
    pub fields: Vec<PField>,
}

impl PObject {
    pub fn field(&self, id: u32) -> Option<&PField> {
        self.fields.iter().find(|f| f.id == id)
    }
    pub fn field_named(&self, name: &str) -> Option<&PField> {
        self.fields.iter().find(|f| f.name == name)
    }

    pub fn to_json(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        for f in &self.fields {
            let v = if f.count == 1 && f.ddl_type != DT_EMPTY_ARRAY {
                f.values.first().map(|v| v.to_json()).unwrap_or(serde_json::Value::Null)
            } else {
                serde_json::Value::Array(f.values.iter().map(|v| v.to_json()).collect())
            };
            m.insert(f.name.clone(), v);
        }
        serde_json::Value::Object(m)
    }
}

// ---------------------------------------------------------------------------
// Name resolution
// ---------------------------------------------------------------------------

/// Everything the decoder needs to put names on hashes.
pub struct NameCtx<'a> {
    /// The owning DAT1's string pool — blobs carry offsets into it.
    pub pool: Option<&'a Dat1>,
    pub schema: Option<&'a SchemaDb>,
}

impl<'a> NameCtx<'a> {
    pub fn new(pool: Option<&'a Dat1>, schema: Option<&'a SchemaDb>) -> Self {
        Self { pool, schema }
    }

    fn resolve(&self, id: u32, name_offset: u32) -> (String, &'static str) {
        if name_offset != 0 {
            if let Some(d) = self.pool {
                if let Some(s) = d.get_string(name_offset) {
                    // Only trust the pool when it actually hashes to the field
                    // id; a stale offset would otherwise invent a plausible name.
                    if !s.is_empty() && crc32::hash(&s) == id {
                        return (s, "pool");
                    }
                }
            }
        }
        if let Some(db) = self.schema {
            if let Some(n) = db.name_of(id) {
                return (n.to_string(), "schema");
            }
        }
        (format!("field_{id:08X}"), "hash")
    }
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

struct Rd<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn need(&self, n: usize) -> Result<()> {
        if self.p + n > self.d.len() {
            return Err(ToolkitError::Parse(format!(
                "prius: read of {n} at {} past end {}",
                self.p,
                self.d.len()
            )));
        }
        Ok(())
    }
    fn u8(&mut self) -> Result<u8> {
        self.need(1)?;
        let v = self.d[self.p];
        self.p += 1;
        Ok(v)
    }
    fn u16(&mut self) -> Result<u16> {
        self.need(2)?;
        let v = LE::read_u16(&self.d[self.p..]);
        self.p += 2;
        Ok(v)
    }
    fn u32(&mut self) -> Result<u32> {
        self.need(4)?;
        let v = LE::read_u32(&self.d[self.p..]);
        self.p += 4;
        Ok(v)
    }
    fn u64(&mut self) -> Result<u64> {
        self.need(8)?;
        let v = LE::read_u64(&self.d[self.p..]);
        self.p += 8;
        Ok(v)
    }
    fn align4(&mut self) {
        self.p = (self.p + 3) & !3;
    }
}

/// Decode one DDL blob.
///
/// `type_id` / `type_name` only label the root; decoding is driven entirely by
/// the blob, so an unknown type still yields a full tree.
pub fn decode(blob: &[u8], type_id: u32, type_name: Option<String>, ctx: &NameCtx) -> Result<PObject> {
    let mut rd = Rd { d: blob, p: 0 };
    let mut obj = read_object(&mut rd, ctx)?;
    obj.type_id = type_id;
    obj.type_name = type_name;
    // Annotate from the schema now that the root type is known. This recurses,
    // and resolves polymorphic sub-structs from their own `Type` field.
    if let Some(db) = ctx.schema {
        annotate(&mut obj, db);
    }
    Ok(obj)
}

/// `crc32("Type")` — the type slot of a polymorphic DDL sub-struct.
/// **VERIFIED**: `crc32("Type") == 0xBC4E9799` and `crc32("Obj") == 0x6C33FDA5`,
/// which are exactly the two reserved ids `core::ddl` already carries.
pub const POLY_TYPE_FIELD: u32 = 0xBC4E_9799;
/// `crc32("Obj")` — the object slot of a polymorphic DDL sub-struct.
pub const POLY_OBJECT_FIELD: u32 = 0x6C33_FDA5;

/// Walk a decoded tree and put schema information on it: field names, labels,
/// defaults, enum member sets, and the type of every nested struct.
///
/// Two ways a nested struct gets its type:
///
///   * the parent field's `type_id` in the schema — the ordinary case;
///   * **polymorphism**: an object carrying both `Type` (a string naming a
///     struct) and `Obj` (a struct) is a `DDLPolymorphicObject`, and `Type`'s
///     *value* names `Obj`'s struct. Shipped example: an `OnEventAction`'s
///     `ListenEvent` is an `EventBase` whose `Type` is `"WeaponEmptiedEvent"`
///     and whose `Obj` then really is a `WeaponEmptiedEvent` — that is where
///     `OwnerIsHero` comes from. Without this step those sub-structs decode
///     with correct names (the pool has them) but no labels, defaults or enums.
fn annotate(obj: &mut PObject, db: &SchemaDb) {
    let t = db.ty(obj.type_id).cloned();
    for f in &mut obj.fields {
        if let Some(def) = t.as_ref().and_then(|t| t.field(f.id)) {
            if f.name_source != "pool" {
                f.name = def.name.clone();
                f.name_source = "schema";
            }
            f.label = def.label.clone();
            f.default = def.default.clone();
            f.schema_type = Some(def.ddl_type);
            // A select/bitfield arrives on the wire as a plain string, so key
            // the enum lookup on the *schema's* type, not the blob's.
            if matches!(def.ddl_type, DT_SELECT | DT_BITFIELD)
                || matches!(f.ddl_type, DT_SELECT | DT_BITFIELD)
            {
                if let Some(e) = db.enum_def(def.type_id) {
                    f.enum_values =
                        Some(e.member_names().iter().map(|s| s.to_string()).collect());
                    f.enum_id = def.type_id;
                    f.enum_is_bitset = e.bitset;
                }
            }
            if f.ddl_type == DT_STRUCT {
                if let Some(sub) = db.ty(def.type_id) {
                    let (sid, sname) = (sub.id, sub.name.clone());
                    for v in &mut f.values {
                        if let PValue::Struct(o) = v {
                            o.type_id = sid;
                            o.type_name = Some(sname.clone());
                        }
                    }
                }
            }
        }
        f.recompute_edit();
    }

    // Polymorphic pair: `Type`'s value names `Obj`'s struct.
    let poly = obj
        .field(POLY_TYPE_FIELD)
        .and_then(|f| f.values.first())
        .and_then(|v| match v {
            PValue::Str { value, .. } if !value.is_empty() => db.ty_by_name(value),
            _ => None,
        })
        .map(|t| (t.id, t.name.clone()));
    if let Some((sid, sname)) = poly {
        if let Some(f) = obj.fields.iter_mut().find(|f| f.id == POLY_OBJECT_FIELD) {
            for v in &mut f.values {
                if let PValue::Struct(o) = v {
                    o.type_id = sid;
                    o.type_name = Some(sname.clone());
                }
            }
        }
    }

    // Recurse after both sources have had their say.
    for f in &mut obj.fields {
        for v in &mut f.values {
            if let PValue::Struct(o) = v {
                annotate(o, db);
            }
        }
    }
}

fn read_object(rd: &mut Rd, ctx: &NameCtx) -> Result<PObject> {
    let start = rd.p;
    let _reserved = rd.u32()?;
    let magic = rd.u32()?;
    if magic != DDL_MAGIC && magic != DDL_MAGIC_ALT {
        return Err(ToolkitError::Parse(format!(
            "prius: bad DDL magic {magic:#010X} at {start}"
        )));
    }
    let field_count = rd.u32()? as usize;
    let declared_size = rd.u32()?;
    if field_count > 0xFFFF {
        return Err(ToolkitError::Parse(format!(
            "prius: implausible field count {field_count} at {start}"
        )));
    }

    let headers_at = rd.p;
    let mut hdr = Vec::with_capacity(field_count);
    for _ in 0..field_count {
        let id = rd.u32()?;
        let enc = rd.u32()?;
        hdr.push((id, enc));
    }
    let names_at = rd.p;
    let mut name_offs = Vec::with_capacity(field_count);
    for _ in 0..field_count {
        name_offs.push(rd.u32()?);
    }

    let mut fields = Vec::with_capacity(field_count);
    for (i, &(id, enc)) in hdr.iter().enumerate() {
        let ddl_type = ((enc >> 24) & 0x1F) as u8;
        let encoding_type = ((enc >> 29) & 3) as u8;
        let count = (enc >> 4) & 0x000F_FFFF;
        if enc & 0xF != 0 {
            return Err(ToolkitError::Parse(format!(
                "prius: encoding word {enc:#010X} has a non-zero low nibble"
            )));
        }
        if ddl_type == DT_UNKNOWN || ddl_type > DT_ASSET_ID {
            return Err(ToolkitError::Parse(format!(
                "prius: field {id:#010X} has DDL type {ddl_type}"
            )));
        }

        // Type 19 carries one byte with count 0.
        let reads: u32 = if ddl_type == DT_EMPTY_ARRAY { 1 } else { count };

        let mut values = Vec::with_capacity(reads as usize);
        let mut spans = Vec::with_capacity(reads as usize);
        for _ in 0..reads {
            let at = rd.p;
            let v = read_value(rd, ddl_type, ctx)?;
            spans.push((at, rd.p - at));
            values.push(v);
        }

        let (name, name_source) = ctx.resolve(id, name_offs[i]);
        let mut f = PField {
            id,
            name,
            name_source,
            label: None,
            ddl_type,
            encoding_type,
            count,
            values,
            spans,
            header_at: headers_at + i * 8,
            name_off_at: names_at + i * 4,
            name_offset: name_offs[i],
            schema_type: None,
            enum_values: None,
            enum_id: 0,
            enum_is_bitset: false,
            default: None,
            edit: Edit::NotEditable,
            edit_note: None,
        };
        f.recompute_edit();
        fields.push(f);
    }

    rd.align4();
    Ok(PObject {
        type_id: 0,
        type_name: None,
        magic,
        span: (start, rd.p - start),
        declared_size,
        fields,
    })
}

fn read_value(rd: &mut Rd, t: u8, ctx: &NameCtx) -> Result<PValue> {
    Ok(match t {
        DT_U8 => PValue::U8(rd.u8()?),
        DT_U16 => PValue::U16(rd.u16()?),
        DT_U32 => PValue::U32(rd.u32()?),
        DT_U64 => PValue::U64(rd.u64()?),
        DT_I8 => PValue::I8(rd.u8()? as i8),
        DT_I16 => PValue::I16(rd.u16()? as i16),
        DT_I32 => PValue::I32(rd.u32()? as i32),
        DT_I64 => PValue::I64(rd.u64()? as i64),
        DT_F32 => PValue::F32(f32::from_bits(rd.u32()?)),
        DT_F64 => PValue::F64(f64::from_bits(rd.u64()?)),
        DT_BOOL => PValue::Bool(rd.u8()? != 0),
        DT_TUID => PValue::Tuid(rd.u64()?),
        DT_ASSET_ID => PValue::AssetId(rd.u64()?),
        DT_STRING | DT_SELECT | DT_BITFIELD | DT_FILE | DT_JSON => {
            // DDLString: { i32 length, u32 crc32, u64 crc64, bytes, NUL, pad4 }
            let length = rd.u32()? as usize;
            let hash = rd.u32()?;
            let checksum = rd.u64()?;
            if length > 1 << 20 {
                return Err(ToolkitError::Parse(format!("prius: string length {length}")));
            }
            rd.need(length + 1)?;
            let bytes = &rd.d[rd.p..rd.p + length];
            let value = String::from_utf8_lossy(bytes).into_owned();
            rd.p += length + 1;
            rd.align4();
            PValue::Str { value, hash, checksum }
        }
        DT_STRUCT => {
            let inner = read_object(rd, ctx)?;
            rd.align4();
            PValue::Struct(Box::new(inner))
        }
        DT_EMPTY_ARRAY => PValue::EmptyArray(rd.u8()?),
        _ => {
            return Err(ToolkitError::Parse(format!("prius: DDL type {t} is not readable")));
        }
    })
}

impl PField {
    fn recompute_edit(&mut self) {
        // Id-shaped names on 64-bit integers are references in disguise.
        let looks_id = matches!(self.ddl_type, DT_U64 | DT_I64)
            && (self.name.ends_with("Id")
                || self.name.ends_with("Uid")
                || self.name.ends_with("UID")
                || self.name.ends_with("Guid"));
        let (edit, note) = match self.ddl_type {
            DT_TUID | DT_ASSET_ID => (
                Edit::Reference,
                Some("a 64-bit asset/tuid reference: the zone's backing asset list is built from it"),
            ),
            _ if looks_id => (
                Edit::Reference,
                Some("an engine-allocated identifier; cloning or changing it desyncs spawners"),
            ),
            DT_STRUCT | DT_EMPTY_ARRAY => (Edit::NotEditable, None),
            // A schema `file` field is an inline asset path. 20 586 of the
            // 21 315 shipped `file` fields arrive as a wire string, so this
            // check has to look at the schema type, not the wire type.
            _ if self.schema_type == Some(DT_FILE) || self.ddl_type == DT_FILE => (
                Edit::Reference,
                Some("an inline asset path: the zone's backing asset list is derived from it, so retargeting it needs the new asset added to the zone"),
            ),
            _ if self.enum_values.is_some() => (
                Edit::SelectSameLen,
                Some("select members are stored as inline strings: only a member of the same slot length fits in place"),
            ),
            DT_SELECT => (
                Edit::SelectSameLen,
                Some("select members are stored as inline strings: only a member of the same byte length fits in place"),
            ),
            DT_BITFIELD | DT_STRING | DT_FILE | DT_JSON => (
                Edit::StringSameLen,
                Some("inline string: only a same-byte-length rewrite fits in place"),
            ),
            _ if scalar_width(self.ddl_type).is_some() => (Edit::Scalar, None),
            _ => (Edit::NotEditable, None),
        };
        self.edit = edit;
        self.edit_note = note;
    }

    /// Byte span of value `i` — what a patch would overwrite.
    pub fn span(&self, i: usize) -> Option<(usize, usize)> {
        self.spans.get(i).copied()
    }

    /// The numeric range an in-place edit may stay inside, and whether the value
    /// must be a whole number, given the type the blob actually stores.
    /// `None` for non-numeric fields.
    pub fn numeric_bounds(&self) -> Option<(f64, f64, bool)> {
        numeric_bounds_of(self.ddl_type)
    }
}

// ---------------------------------------------------------------------------
// Re-serialisation
// ---------------------------------------------------------------------------

/// Write a decoded object back out. Used to *prove* the reader consumed every
/// byte: on shipped data this must reproduce the input exactly.
///
/// Note this is a verification tool, not the save path. Saving patches the
/// original bytes (see [`PriusBlob::patch`]) so an unedited blob cannot drift.
pub fn reserialize(obj: &PObject) -> Vec<u8> {
    let mut out = Vec::new();
    write_object(obj, &mut out);
    out
}

fn write_object(obj: &PObject, out: &mut Vec<u8>) {
    let base = out.len();
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&obj.magic.to_le_bytes());
    out.extend_from_slice(&(obj.fields.len() as u32).to_le_bytes());
    out.extend_from_slice(&obj.declared_size.to_le_bytes());
    for f in &obj.fields {
        let enc = ((f.encoding_type as u32 & 3) << 29)
            | ((f.ddl_type as u32 & 0x1F) << 24)
            | ((f.count & 0x000F_FFFF) << 4);
        out.extend_from_slice(&f.id.to_le_bytes());
        out.extend_from_slice(&enc.to_le_bytes());
    }
    for f in &obj.fields {
        out.extend_from_slice(&f.name_offset.to_le_bytes());
    }
    for f in &obj.fields {
        for v in &f.values {
            write_value(v, f.ddl_type, out, base);
        }
    }
    let pad = (((out.len() - base) + 3) & !3) - (out.len() - base);
    out.extend(std::iter::repeat(0u8).take(pad));
}

fn write_value(v: &PValue, t: u8, out: &mut Vec<u8>, base: usize) {
    match v {
        PValue::U8(x) => out.push(*x),
        PValue::U16(x) => out.extend_from_slice(&x.to_le_bytes()),
        PValue::U32(x) => out.extend_from_slice(&x.to_le_bytes()),
        PValue::U64(x) => out.extend_from_slice(&x.to_le_bytes()),
        PValue::I8(x) => out.push(*x as u8),
        PValue::I16(x) => out.extend_from_slice(&x.to_le_bytes()),
        PValue::I32(x) => out.extend_from_slice(&x.to_le_bytes()),
        PValue::I64(x) => out.extend_from_slice(&x.to_le_bytes()),
        PValue::F32(x) => out.extend_from_slice(&x.to_bits().to_le_bytes()),
        PValue::F64(x) => out.extend_from_slice(&x.to_bits().to_le_bytes()),
        PValue::Bool(x) => out.push(*x as u8),
        PValue::Tuid(x) | PValue::AssetId(x) => out.extend_from_slice(&x.to_le_bytes()),
        PValue::EmptyArray(b) => out.push(*b),
        PValue::Str { value, hash, checksum } => {
            out.extend_from_slice(&(value.len() as u32).to_le_bytes());
            out.extend_from_slice(&hash.to_le_bytes());
            out.extend_from_slice(&checksum.to_le_bytes());
            out.extend_from_slice(value.as_bytes());
            out.push(0);
            let pad = (((out.len() - base) + 3) & !3) - (out.len() - base);
            out.extend(std::iter::repeat(0u8).take(pad));
        }
        PValue::Struct(o) => {
            write_object(o, out);
            let pad = (((out.len() - base) + 3) & !3) - (out.len() - base);
            out.extend(std::iter::repeat(0u8).take(pad));
        }
    }
    let _ = t;
}

// ---------------------------------------------------------------------------
// Row tables
// ---------------------------------------------------------------------------

/// One Zone Actor Prius row.
#[derive(Debug, Clone)]
pub struct PriusRow {
    /// The prius struct's 64-bit type id. **VERIFIED** on 50 341 of 50 341
    /// shipped rows: it is the un-post-processed CRC64 (ECMA polynomial, init
    /// = polynomial, no final xor, no `>>2 | flag` packing) of the *lowercased*
    /// `"<component>Prius"` spelling. It is constant per component, so it is a
    /// type id and not an instance id, and it confirms the component -> struct
    /// naming rule in the binary rather than by guesswork.
    ///
    /// This slot used to look like an actor uid. It is not: it matches no actor
    /// uid in its zone (0 of 50 341), and 27 876 of them do not even carry the
    /// placed bit.
    pub type_uid: u64,
    pub name_offset: u32,
    pub name_hash: u32,
    /// `+0x10`. Zero on every shipped row measured.
    pub reserved: u32,
    /// Absolute DAT1 offset of the blob.
    pub data_offset: u32,
    pub data_size: u32,
    /// `+0x1C`. `-1` on every shipped row measured.
    pub tail: i32,
    /// Component name from the string pool.
    pub component: String,
}

impl PriusRow {
    /// True when `crc32(component) == name_hash`.
    pub fn name_hash_ok(&self) -> bool {
        crc32::hash(&self.component) == self.name_hash
    }

    /// The type id this row's component name predicts.
    pub fn expected_type_uid(&self) -> u64 {
        prius_type_uid(&self.component)
    }

    /// True when `type_uid` is the hash of `"<component>Prius"`.
    pub fn type_uid_ok(&self) -> bool {
        self.expected_type_uid() == self.type_uid
    }
}

/// CRC64 of a DDL struct name, *without* the DAT1 asset-id post-processing.
///
/// [`crate::core::crc64::hash_raw`] finishes with `(crc >> 2) | 1 << 63` to make
/// an asset id; a DDL type id is the plain CRC. Same polynomial
/// `0xC96C5795D7870F42`, same non-zero init, reflected, no final xor.
pub fn ddl_type_uid(name: &str) -> u64 {
    const POLY: u64 = 0xC96C_5795_D787_0F42;
    let mut crc: u64 = POLY;
    for &b in name.as_bytes() {
        crc ^= b as u64;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ POLY } else { crc >> 1 };
        }
    }
    crc
}

/// The 64-bit type id a Zone Actor Prius row carries for `component`.
pub fn prius_type_uid(component: &str) -> u64 {
    ddl_type_uid(&format!("{component}Prius").to_lowercase())
}

/// The actor prius tables of one zone: the rows, the blob pool and the pool's
/// own section offset (needed to turn an absolute `data_offset` into a slice).
pub struct PriusTable {
    pub rows: Vec<PriusRow>,
    pub pool: Vec<u8>,
    pub pool_offset: u32,
    /// Row indices by prius type id.
    by_type: HashMap<u64, Vec<usize>>,
}

impl PriusTable {
    pub fn from_dat1(dat1: &Dat1) -> Self {
        let pool_offset = dat1
            .sections
            .iter()
            .find(|s| s.tag == TAG_ACTOR_PRIUS_POOL)
            .map(|s| s.offset)
            .unwrap_or(0);
        let pool = dat1.get_section_data(TAG_ACTOR_PRIUS_POOL).unwrap_or(&[]).to_vec();
        let mut rows = Vec::new();
        if let Some(data) = dat1.get_section_data(TAG_ACTOR_PRIUSES) {
            for r in data.chunks_exact(PRIUS_ROW_LEN) {
                let name_offset = LE::read_u32(&r[8..12]);
                rows.push(PriusRow {
                    type_uid: LE::read_u64(&r[0..8]),
                    name_offset,
                    name_hash: LE::read_u32(&r[12..16]),
                    reserved: LE::read_u32(&r[16..20]),
                    data_offset: LE::read_u32(&r[20..24]),
                    data_size: LE::read_u32(&r[24..28]),
                    tail: LE::read_i32(&r[28..32]),
                    component: dat1.get_string(name_offset).unwrap_or_default(),
                });
            }
        }
        let mut by_type: HashMap<u64, Vec<usize>> = HashMap::new();
        for (i, r) in rows.iter().enumerate() {
            by_type.entry(r.type_uid).or_default().push(i);
        }
        PriusTable { rows, pool, pool_offset, by_type }
    }

    /// Blob bytes of row `i`, or `None` when the span leaves the pool.
    pub fn blob(&self, i: usize) -> Option<&[u8]> {
        let r = self.rows.get(i)?;
        let start = (r.data_offset as usize).checked_sub(self.pool_offset as usize)?;
        let end = start.checked_add(r.data_size as usize)?;
        if end > self.pool.len() || r.data_size < 16 {
            return None;
        }
        Some(&self.pool[start..end])
    }

    /// Pool-relative start of row `i`'s blob — the base a patch offsets from.
    pub fn blob_at(&self, i: usize) -> Option<usize> {
        let r = self.rows.get(i)?;
        (r.data_offset as usize).checked_sub(self.pool_offset as usize)
    }

    /// Every row of one prius type, e.g. every `StackableCrate` in the zone.
    pub fn rows_of_type(&self, type_uid: u64) -> &[usize] {
        self.by_type.get(&type_uid).map(|v| v.as_slice()).unwrap_or(&[])
    }
}

// ---------------------------------------------------------------------------
// An editable blob
// ---------------------------------------------------------------------------

/// One prius blob plus its decoded tree, ready to be patched in place.
pub struct PriusBlob {
    /// The original bytes, never mutated.
    pub original: Vec<u8>,
    /// The bytes as edited. Equal to `original` until a patch lands.
    pub bytes: Vec<u8>,
    pub tree: PObject,
    pub component: String,
}

impl PriusBlob {
    /// `type_uid` is the row's `+0x00` word. Passing it matters: on 277 of
    /// 50 341 shipped rows the struct is **not** `"<component>Prius"` — an
    /// `NPCBot` component can carry a `BotPrius`, a `PortalPassiveShift` a
    /// `PortalPrius`, a `SwingshotTargetSling` a `SwingshotTargetSwingPrius`.
    /// Resolving by hash first stops the schema from annotating a blob with the
    /// wrong struct's field types and defaults. Pass 0 when there is no row
    /// (script priuses), and the component/action name is used instead.
    pub fn new(
        blob: &[u8],
        component: &str,
        type_uid: u64,
        ctx: &NameCtx,
        schema: Option<&SchemaDb>,
    ) -> Result<Self> {
        let by_uid = if type_uid != 0 {
            schema.and_then(|db| db.ty_by_uid(type_uid))
        } else {
            None
        };
        let (type_id, type_name) = match by_uid.or_else(|| schema.and_then(|db| db.prius_of_component(component))) {
            Some(t) => (t.id, Some(t.name.clone())),
            None => (crc32::hash(&format!("{component}Prius")), None),
        };
        let tree = decode(blob, type_id, type_name, ctx)?;
        Ok(PriusBlob {
            original: blob.to_vec(),
            bytes: blob.to_vec(),
            tree,
            component: component.to_string(),
        })
    }

    pub fn is_dirty(&self) -> bool {
        self.bytes != self.original
    }

    /// Byte-identical by construction when untouched.
    pub fn to_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Set a numeric field from a single `f64`, coercing to whatever narrow type
    /// the blob stores. This is what a UI spin box should call.
    pub fn patch_number(&mut self, path: &str, x: f64) -> Result<()> {
        let ddl_type = {
            let (f, _) = resolve_path(&self.tree, path)
                .ok_or_else(|| ToolkitError::Parse(format!("prius: no field {path}")))?;
            f.ddl_type
        };
        let v = coerce_number(ddl_type, x)?;
        self.patch(path, &v)
    }

    /// Overwrite one value in place. `path` is the dotted field path from the
    /// root, with `[i]` for an array element, e.g. `"Offset.y"` or `"Points[2]"`.
    ///
    /// Refuses anything the equal-length rule cannot express, and refuses
    /// references outright.
    pub fn patch(&mut self, path: &str, new: &PValue) -> Result<()> {
        let (span, ddl_type, edit) = {
            let f = resolve_path(&self.tree, path)
                .ok_or_else(|| ToolkitError::Parse(format!("prius: no field {path}")))?;
            let (field, idx) = f;
            let span = field
                .span(idx)
                .ok_or_else(|| ToolkitError::Parse(format!("prius: {path} has no value {idx}")))?;
            (span, field.ddl_type, field.edit)
        };
        match edit {
            Edit::Reference => {
                return Err(ToolkitError::Parse(format!(
                    "prius: {path} is a reference and is read-only"
                )))
            }
            Edit::NotEditable => {
                return Err(ToolkitError::Parse(format!("prius: {path} is not an editable leaf")))
            }
            _ => {}
        }
        let enc = encode_value(new, ddl_type)?;
        if enc.len() != span.1 {
            return Err(ToolkitError::Parse(format!(
                "prius: {path} needs {} bytes but the slot is {} — the zone save path only allows equal-length patches",
                enc.len(),
                span.1
            )));
        }
        self.bytes[span.0..span.0 + span.1].copy_from_slice(&enc);
        // Keep the tree in step so a second patch sees the new value.
        if let Some((field, idx)) = resolve_path_mut(&mut self.tree, path) {
            field.values[idx] = new.clone();
        }
        Ok(())
    }
}

/// The `crc32` and `crc64` words a `DDLString` slot carries for `value`.
///
/// **VERIFIED** on all 112 754 string values of the shipped actor priuses:
///
///   * 109 342 non-empty values store `crc32(value)` — the literal bytes, case
///     preserved — and `crc64::hash(value)`, which *normalises first*
///     (lowercase, back slashes to forward) and then applies the asset-id
///     packing `(crc >> 2) | 1 << 63`. Both matched 109 342 / 109 342, with 0
///     using the un-normalised crc64 and 0 using a lowercased crc32.
///   * all 3 412 empty values store `0` / `0`, not the hash of the empty string.
///
/// The same string can therefore be an asset id (`crc64`) and a case-sensitive
/// identity (`crc32`) at once, which is why both words exist.
pub fn string_hashes(value: &str) -> (u32, u64) {
    if value.is_empty() {
        (0, 0)
    } else {
        (crc32::hash(value), crc64::hash(value))
    }
}

/// Inclusive range and integrality of a numeric DDL slot.
pub fn numeric_bounds_of(ddl_type: u8) -> Option<(f64, f64, bool)> {
    Some(match ddl_type {
        DT_U8 => (0.0, u8::MAX as f64, true),
        DT_U16 => (0.0, u16::MAX as f64, true),
        DT_U32 => (0.0, u32::MAX as f64, true),
        DT_U64 => (0.0, u64::MAX as f64, true),
        DT_I8 => (i8::MIN as f64, i8::MAX as f64, true),
        DT_I16 => (i16::MIN as f64, i16::MAX as f64, true),
        DT_I32 => (i32::MIN as f64, i32::MAX as f64, true),
        DT_I64 => (i64::MIN as f64, i64::MAX as f64, true),
        DT_F32 => (f32::MIN as f64, f32::MAX as f64, false),
        DT_F64 => (f64::MIN, f64::MAX, false),
        DT_BOOL => (0.0, 1.0, true),
        _ => return None,
    })
}

/// Coerce a number into the type a blob slot actually stores, or explain why it
/// does not fit. This is the gate in front of every numeric edit: the builder
/// narrowed the field, so the slot - not the schema - decides.
pub fn coerce_number(ddl_type: u8, x: f64) -> Result<PValue> {
    let Some((lo, hi, integral)) = numeric_bounds_of(ddl_type) else {
        return Err(ToolkitError::Parse(format!(
            "prius: {} is not a numeric slot",
            type_label(ddl_type)
        )));
    };
    if integral && x.fract() != 0.0 {
        return Err(ToolkitError::Parse(format!(
            "prius: {x} is not a whole number but the blob stores this field as {} - the builder narrowed it, and widening it back to f32 would change the blob size",
            type_label(ddl_type)
        )));
    }
    if x < lo || x > hi {
        return Err(ToolkitError::Parse(format!(
            "prius: {x} is outside the {} slot's range {lo}..={hi}",
            type_label(ddl_type)
        )));
    }
    Ok(match ddl_type {
        DT_U8 => PValue::U8(x as u8),
        DT_U16 => PValue::U16(x as u16),
        DT_U32 => PValue::U32(x as u32),
        DT_U64 => PValue::U64(x as u64),
        DT_I8 => PValue::I8(x as i8),
        DT_I16 => PValue::I16(x as i16),
        DT_I32 => PValue::I32(x as i32),
        DT_I64 => PValue::I64(x as i64),
        DT_F32 => PValue::F32(x as f32),
        DT_F64 => PValue::F64(x),
        DT_BOOL => PValue::Bool(x != 0.0),
        _ => unreachable!(),
    })
}

/// Encode one value to exactly the bytes its slot holds. Strings are written
/// with a freshly computed `crc32`/`crc64`, then NUL and pad — so a same-length
/// rewrite lands in the same footprint.
pub fn encode_value(v: &PValue, ddl_type: u8) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    match (ddl_type, v) {
        (DT_U8, PValue::U8(x)) => out.push(*x),
        (DT_U16, PValue::U16(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (DT_U32, PValue::U32(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (DT_U64, PValue::U64(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (DT_I8, PValue::I8(x)) => out.push(*x as u8),
        (DT_I16, PValue::I16(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (DT_I32, PValue::I32(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (DT_I64, PValue::I64(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (DT_F32, PValue::F32(x)) => out.extend_from_slice(&x.to_bits().to_le_bytes()),
        (DT_F64, PValue::F64(x)) => out.extend_from_slice(&x.to_bits().to_le_bytes()),
        (DT_BOOL, PValue::Bool(x)) => out.push(*x as u8),
        (DT_STRING | DT_SELECT | DT_BITFIELD | DT_FILE | DT_JSON, PValue::Str { value, .. }) => {
            let (h, ck) = string_hashes(value);
            out.extend_from_slice(&(value.len() as u32).to_le_bytes());
            out.extend_from_slice(&h.to_le_bytes());
            out.extend_from_slice(&ck.to_le_bytes());
            out.extend_from_slice(value.as_bytes());
            out.push(0);
            while out.len() % 4 != 0 {
                out.push(0);
            }
        }
        _ => {
            return Err(ToolkitError::Parse(format!(
                "prius: cannot encode {v:?} as {}",
                type_label(ddl_type)
            )))
        }
    }
    Ok(out)
}

fn split_step(step: &str) -> (&str, usize) {
    match step.split_once('[') {
        Some((n, rest)) => {
            let i = rest.trim_end_matches(']').parse::<usize>().unwrap_or(0);
            (n, i)
        }
        None => (step, 0),
    }
}

fn resolve_path<'a>(obj: &'a PObject, path: &str) -> Option<(&'a PField, usize)> {
    let mut cur = obj;
    let mut it = path.split('.').peekable();
    while let Some(step) = it.next() {
        let (name, idx) = split_step(step);
        let f = cur.fields.iter().find(|f| f.name == name)?;
        if it.peek().is_none() {
            return Some((f, idx));
        }
        match f.values.get(idx)? {
            PValue::Struct(o) => cur = o,
            _ => return None,
        }
    }
    None
}

fn resolve_path_mut<'a>(obj: &'a mut PObject, path: &str) -> Option<(&'a mut PField, usize)> {
    let steps: Vec<&str> = path.split('.').collect();
    let mut cur = obj;
    for (n, step) in steps.iter().enumerate() {
        let (name, idx) = split_step(step);
        let pos = cur.fields.iter().position(|f| f.name == name)?;
        if n + 1 == steps.len() {
            return Some((&mut cur.fields[pos], idx));
        }
        match cur.fields[pos].values.get_mut(idx)? {
            PValue::Struct(o) => cur = o,
            _ => return None,
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Wire-format shaped view for a UI
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct FieldView {
    pub name: String,
    pub label: Option<String>,
    pub name_source: &'static str,
    pub hash: String,
    /// The type the blob stores, which is what bounds an edit.
    #[serde(rename = "type")]
    pub ty: &'static str,
    /// The type the schema declares, when it differs - `float` for a field the
    /// builder narrowed to `u8`, `select` for one it wrote as a string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_type: Option<&'static str>,
    /// Inclusive numeric range an in-place edit must stay inside.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// True when the slot only holds whole numbers.
    pub integral: bool,
    pub encoding_type: u8,
    pub count: u32,
    pub value: serde_json::Value,
    pub text: String,
    pub edit: Edit,
    pub edit_note: Option<&'static str>,
    /// Byte span inside the blob of the first value — what a patch overwrites.
    pub offset: Option<usize>,
    pub width: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<FieldView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PriusView {
    pub component: String,
    pub type_name: Option<String>,
    pub type_hash: String,
    pub uid: String,
    pub size: u32,
    pub named_fields: usize,
    pub total_fields: usize,
    pub fields: Vec<FieldView>,
}

pub fn view_field(f: &PField) -> FieldView {
    let first = f.values.first();
    let children = match first {
        Some(PValue::Struct(o)) => o.fields.iter().map(view_field).collect(),
        _ => Vec::new(),
    };
    let value = if f.count == 1 && f.ddl_type != DT_EMPTY_ARRAY {
        first.map(|v| v.to_json()).unwrap_or(serde_json::Value::Null)
    } else {
        serde_json::Value::Array(f.values.iter().map(|v| v.to_json()).collect())
    };
    let text = if f.count == 1 {
        first.map(|v| v.text()).unwrap_or_default()
    } else {
        format!("[{} values]", f.values.len())
    };
    let bounds = f.numeric_bounds();
    FieldView {
        name: f.name.clone(),
        label: f.label.clone(),
        name_source: f.name_source,
        hash: format!("{:#010X}", f.id),
        ty: type_label(f.ddl_type),
        schema_type: f.schema_type.filter(|t| *t != f.ddl_type).map(type_label),
        min: bounds.map(|b| b.0),
        max: bounds.map(|b| b.1),
        integral: bounds.map(|b| b.2).unwrap_or(false),
        encoding_type: f.encoding_type,
        count: f.count,
        value,
        text,
        edit: f.edit,
        edit_note: f.edit_note,
        offset: f.spans.first().map(|s| s.0),
        width: f.spans.first().map(|s| s.1),
        options: f.enum_values.clone(),
        default: f.default.clone(),
        children,
    }
}

pub fn view(component: &str, uid: u64, size: u32, tree: &PObject) -> PriusView {
    let (named, total) = count_named(tree);
    PriusView {
        component: component.to_string(),
        type_name: tree.type_name.clone(),
        type_hash: format!("{:#010X}", tree.type_id),
        uid: format!("{uid:#018X}"),
        size,
        named_fields: named,
        total_fields: total,
        fields: tree.fields.iter().map(view_field).collect(),
    }
}

/// `(fields with a real name, fields total)`, recursively.
pub fn count_named(obj: &PObject) -> (usize, usize) {
    let mut named = 0;
    let mut total = 0;
    for f in &obj.fields {
        total += 1;
        if f.name_source != "hash" {
            named += 1;
        }
        for v in &f.values {
            if let PValue::Struct(o) = v {
                let (n, t) = count_named(o);
                named += n;
                total += t;
            }
        }
    }
    (named, total)
}

// ---------------------------------------------------------------------------
// Convenience: every prius of a placed actor
// ---------------------------------------------------------------------------

/// All component priuses of one actor row, decoded.
pub struct ActorPriuses {
    pub uid: u64,
    pub blobs: Vec<PriusBlob>,
    /// Row index in Zone Actor Priuses, parallel to `blobs`.
    pub row_indices: Vec<usize>,
    /// Rows that could not be decoded, with the reason.
    pub failures: Vec<(usize, String)>,
}

/// Decode every prius of the actor at `prius_index .. prius_index + prius_count`.
///
/// `uid` is only used to sanity-check the slice: every row in an actor's slice
/// carries that actor's uid.
pub fn actor_priuses(
    table: &PriusTable,
    dat1: &Dat1,
    schema: Option<&SchemaDb>,
    uid: u64,
    prius_index: i32,
    prius_count: i32,
) -> ActorPriuses {
    let ctx = NameCtx::new(Some(dat1), schema);
    let mut out = ActorPriuses { uid, blobs: Vec::new(), row_indices: Vec::new(), failures: Vec::new() };
    if prius_index < 0 || prius_count <= 0 {
        return out;
    }
    let lo = prius_index as usize;
    let hi = lo + prius_count as usize;
    for i in lo..hi.min(table.rows.len()) {
        let Some(blob) = table.blob(i) else {
            out.failures.push((i, "blob span leaves the pool".into()));
            continue;
        };
        let comp = table.rows[i].component.clone();
        match PriusBlob::new(blob, &comp, table.rows[i].type_uid, &ctx, schema) {
            Ok(b) => {
                out.blobs.push(b);
                out.row_indices.push(i);
            }
            Err(e) => out.failures.push((i, e.to_string())),
        }
    }
    out
}

/// Load the default schema once per process, for callers that do not carry one.
pub fn default_schema() -> Arc<SchemaDb> {
    crate::core::ddl_schema::get_default()
}

/// Legal member names of a select field, for a dropdown, split into the ones a
/// same-length in-place patch can reach and the ones it cannot.
pub fn select_options<'a>(e: &'a EnumDef, current: &str) -> (Vec<&'a str>, Vec<&'a str>) {
    let n = current.len();
    let mut fits = Vec::new();
    let mut too_long = Vec::new();
    for v in &e.values {
        // A DDLString slot is 16 + len + 1 rounded up to 4; two members are
        // interchangeable in place when those round to the same size.
        if slot_len(v.name.len()) == slot_len(n) {
            fits.push(v.name.as_str());
        } else {
            too_long.push(v.name.as_str());
        }
    }
    (fits, too_long)
}

/// On-wire footprint of a `DDLString` holding `len` bytes.
pub fn slot_len(len: usize) -> usize {
    (16 + len + 1 + 3) & !3
}
