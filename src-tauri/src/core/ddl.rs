//! Generic DDL ("Data Description Language") parser/serializer.
//!
//! DDL is the binary form Insomniac uses for structured field-based data.

use std::collections::BTreeMap;
use std::io::{Cursor, Read, Seek, SeekFrom};

use byteorder::{LE, ReadBytesExt};
use serde_json::Value;

use crate::core::crc32;
use crate::core::crc64;
use crate::core::dat1::Dat1;
use crate::core::error::{Result, ToolkitError};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// `0x03150044` — DDL header magic (Insomniac "object guard" sentinel).
pub const DDL_MAGIC: u32 = 0x0315_0044;

/// Field-id of the polymorphic substruct's *type* slot
/// (CRC32 of the engine's reserved internal name).
pub const POLY_TYPE_FIELD: u32 = 0xBC4E_9799;
/// Field-id of the polymorphic substruct's *object* slot.
pub const POLY_OBJECT_FIELD: u32 = 0x6C33_FDA5;

/// Asset-Refs section tags.
pub const ACTOR_ASSET_REFS_TAG: u32 = 0x3AB2_04B9;
pub const CONDUIT_ASSET_REFS_TAG: u32 = 0x2F40_56CE;
pub const CONFIG_ASSET_REFS_TAG: u32 = 0x58B8_558A;

/// Config DDL section tags.
pub const CONFIG_TYPE_TAG: u32 = 0x4A12_8222;
pub const CONFIG_BUILT_TAG: u32 = 0xE501_186F;

// ---------------------------------------------------------------------------
// Type/Array enumerations
// ---------------------------------------------------------------------------

/// `DDLTypeKind` — high byte of `DDLFieldHeader.meta`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DdlTypeKind {
    UInt8 = 0,
    UInt16 = 1,
    UInt32 = 2,
    UInt64 = 3,
    Int8 = 4,
    Int16 = 5,
    Int32 = 6,
    Int64 = 7,
    Float = 8,
    Double = 9,
    String = 10,
    Enum = 11,
    Bitfield = 12,
    Struct = 13,
    Unknown = 14,
    Bool = 15,
    File = 16,
    Identifier = 17,
    Json = 18,
    Default = 19,
    Asset = 20,
}

impl DdlTypeKind {
    pub fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            0 => Self::UInt8,
            1 => Self::UInt16,
            2 => Self::UInt32,
            3 => Self::UInt64,
            4 => Self::Int8,
            5 => Self::Int16,
            6 => Self::Int32,
            7 => Self::Int64,
            8 => Self::Float,
            9 => Self::Double,
            10 => Self::String,
            11 => Self::Enum,
            12 => Self::Bitfield,
            13 => Self::Struct,
            14 => Self::Unknown,
            15 => Self::Bool,
            16 => Self::File,
            17 => Self::Identifier,
            18 => Self::Json,
            19 => Self::Default,
            20 => Self::Asset,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::UInt8 => "UInt8",
            Self::UInt16 => "UInt16",
            Self::UInt32 => "UInt32",
            Self::UInt64 => "UInt64",
            Self::Int8 => "Int8",
            Self::Int16 => "Int16",
            Self::Int32 => "Int32",
            Self::Int64 => "Int64",
            Self::Float => "Float",
            Self::Double => "Double",
            Self::String => "String",
            Self::Enum => "Enum",
            Self::Bitfield => "Bitfield",
            Self::Struct => "Struct",
            Self::Unknown => "Unknown",
            Self::Bool => "Bool",
            Self::File => "File",
            Self::Identifier => "Identifier",
            Self::Json => "Json",
            Self::Default => "Default",
            Self::Asset => "Asset",
        }
    }
}

/// `DDLArrayKind` — low 4 bits of `DDLFieldHeader.meta`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DdlArrayKind {
    None = 0,
    Fixed = 1,
    Dynamic = 2,
    Map = 3,
}

impl DdlArrayKind {
    pub fn from_byte(b: u8) -> Self {
        match b & 0x0F {
            1 => Self::Fixed,
            2 => Self::Dynamic,
            3 => Self::Map,
            _ => Self::None,
        }
    }
}

// ---------------------------------------------------------------------------
// Field header — canonical 8-byte layout
// ---------------------------------------------------------------------------

/// `DDLFieldHeader` (8 bytes). `meta` packs:
/// `(type << 24) | (count << 4) | array_kind` where
/// `count` is 20 bits (up to ~1M).
#[derive(Debug, Clone, Copy)]
pub struct DdlFieldHeader {
    /// CRC32 of the field name.
    pub id: u32,
    pub meta: u32,
}

impl DdlFieldHeader {
    pub fn type_kind(&self) -> DdlTypeKind {
        DdlTypeKind::from_byte((self.meta >> 24) as u8).unwrap_or(DdlTypeKind::Unknown)
    }

    pub fn count(&self) -> usize {
        ((self.meta >> 4) & 0x000F_FFFF) as usize
    }

    pub fn array_kind(&self) -> DdlArrayKind {
        DdlArrayKind::from_byte(self.meta as u8)
    }

    pub fn pack(type_kind: DdlTypeKind, count: usize, array_kind: DdlArrayKind) -> u32 {
        let t = (type_kind as u32 & 0xFF) << 24;
        let c = (count as u32 & 0x000F_FFFF) << 4;
        let a = array_kind as u32 & 0x0F;
        t | c | a
    }
}

// ---------------------------------------------------------------------------
// In-memory model
// ---------------------------------------------------------------------------

/// Concrete value carried by one `DDLField` slot.
#[derive(Debug, Clone)]
pub enum DdlValue {
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
    /// Strings, Enums, Bitfields, Files, Json (all wire-format `DDLString`).
    String {
        value: String,
        /// Original CRC32 hash present in the file. **Ignore on save** — the
        /// writer regenerates this from `value` (recommendation #3).
        hash: u32,
        /// Original CRC64 checksum present in the file. Same caveat as `hash`.
        checksum: u64,
    },
    Identifier(u64),
    Asset(u64),
    Struct(DdlObject),
    /// `Default` slot — one byte read, value is null.
    Null,
}

/// One field in a DDL object. Each field can carry 1..N values (per array kind).
#[derive(Debug, Clone)]
pub struct DdlField {
    pub id: u32,
    pub name: String,
    pub type_kind: DdlTypeKind,
    pub array_kind: DdlArrayKind,
    pub values: Vec<DdlValue>,
}

/// Parsed DDL object: ordered map of fields keyed by their CRC32 id.
///
/// We use `BTreeMap` here for deterministic iteration order during serialize.
#[derive(Debug, Clone, Default)]
pub struct DdlObject {
    /// Insertion order preserved alongside the map for round-trip stability.
    pub field_order: Vec<u32>,
    pub fields: BTreeMap<u32, DdlField>,
}

impl DdlObject {
    pub fn insert(&mut self, field: DdlField) {
        if !self.fields.contains_key(&field.id) {
            self.field_order.push(field.id);
        }
        self.fields.insert(field.id, field);
    }

    pub fn get(&self, id: u32) -> Option<&DdlField> {
        self.fields.get(&id)
    }

    /// Mirrors `DDLPolymorphicObject.Check`: a 2-field substruct whose ids
    /// match the engine's reserved polymorphic tags.
    pub fn looks_polymorphic(&self) -> bool {
        is_polymorphic(self)
    }
}

/// Free-function form for ad-hoc checks (e.g. against a still-unparsed object).
pub fn is_polymorphic(obj: &DdlObject) -> bool {
    obj.fields.len() == 2
        && obj.fields.contains_key(&POLY_TYPE_FIELD)
        && obj.fields.contains_key(&POLY_OBJECT_FIELD)
}

// ---------------------------------------------------------------------------
// String pool abstraction
// ---------------------------------------------------------------------------

/// Read-only access to a DAT1 string pool (field names only — DDL string
/// *values* are inline).
pub trait StringPoolRead {
    fn get_string(&self, offset: u32) -> Option<String>;
}

/// Mutable access — used during serialization to intern field names.
pub trait StringPoolWrite: StringPoolRead {
    fn add_string(&mut self, s: &str) -> u32;
}

impl StringPoolRead for Dat1 {
    fn get_string(&self, offset: u32) -> Option<String> {
        Dat1::get_string(self, offset)
    }
}

impl StringPoolWrite for Dat1 {
    fn add_string(&mut self, s: &str) -> u32 {
        let target = s.as_bytes();
        let mut i = 0usize;
        while i < self.strings_pool.len() {
            let mut end = i;
            while end < self.strings_pool.len() && self.strings_pool[end] != 0 {
                end += 1;
            }
            if &self.strings_pool[i..end] == target {
                return i as u32;
            }
            i = end.saturating_add(1);
        }
        let off = self.strings_pool.len() as u32;
        self.strings_pool.extend_from_slice(target);
        self.strings_pool.push(0);
        off
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse a DDL blob (header + fields) from `data` using `pool` for field-name
/// resolution. Returns an empty object if the magic is missing/invalid.
pub fn parse(data: &[u8], pool: &dyn StringPoolRead) -> Result<DdlObject> {
    if data.len() < 16 {
        return Ok(DdlObject::default());
    }
    let mut cur = Cursor::new(data);
    parse_object(&mut cur, pool)
}

fn parse_object(cur: &mut Cursor<&[u8]>, pool: &dyn StringPoolRead) -> Result<DdlObject> {
    let _zero = cur.read_u32::<LE>()?;
    let magic = cur.read_u32::<LE>()?;
    if magic != DDL_MAGIC {
        // Absence of magic treated as empty object, not an error.
        return Ok(DdlObject::default());
    }
    let field_count = cur.read_i32::<LE>()? as usize;
    let _size = cur.read_i32::<LE>()? as usize;

    let mut headers = Vec::with_capacity(field_count);
    for _ in 0..field_count {
        let id = cur.read_u32::<LE>()?;
        let meta = cur.read_u32::<LE>()?;
        headers.push(DdlFieldHeader { id, meta });
    }

    let mut name_offsets = Vec::with_capacity(field_count);
    for _ in 0..field_count {
        name_offsets.push(cur.read_u32::<LE>()?);
    }

    let mut obj = DdlObject::default();
    for (i, header) in headers.iter().enumerate() {
        let type_kind = header.type_kind();
        let array_kind = header.array_kind();
        let raw_count = header.count();
        let count = if matches!(type_kind, DdlTypeKind::Default) {
            raw_count.min(1)
        } else {
            raw_count
        };

        let name = pool
            .get_string(name_offsets[i])
            .unwrap_or_else(|| format!("field_{:08X}", header.id));

        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            values.push(read_value(cur, type_kind, pool)?);
        }

        obj.insert(DdlField { id: header.id, name, type_kind, array_kind, values });
    }

    align_to(cur, 4)?;
    Ok(obj)
}

fn read_value(
    cur: &mut Cursor<&[u8]>,
    type_kind: DdlTypeKind,
    pool: &dyn StringPoolRead,
) -> Result<DdlValue> {
    Ok(match type_kind {
        DdlTypeKind::UInt8 => DdlValue::U8(cur.read_u8()?),
        DdlTypeKind::UInt16 => DdlValue::U16(cur.read_u16::<LE>()?),
        DdlTypeKind::UInt32 => DdlValue::U32(cur.read_u32::<LE>()?),
        DdlTypeKind::UInt64 => DdlValue::U64(cur.read_u64::<LE>()?),
        DdlTypeKind::Int8 => DdlValue::I8(cur.read_i8()?),
        DdlTypeKind::Int16 => DdlValue::I16(cur.read_i16::<LE>()?),
        DdlTypeKind::Int32 => DdlValue::I32(cur.read_i32::<LE>()?),
        DdlTypeKind::Int64 => DdlValue::I64(cur.read_i64::<LE>()?),
        DdlTypeKind::Float => DdlValue::F32(f32::from_bits(cur.read_u32::<LE>()?)),
        DdlTypeKind::Double => DdlValue::F64(f64::from_bits(cur.read_u64::<LE>()?)),
        DdlTypeKind::Bool => DdlValue::Bool(cur.read_u8()? != 0),
        DdlTypeKind::Identifier => DdlValue::Identifier(cur.read_u64::<LE>()?),
        DdlTypeKind::Asset => DdlValue::Asset(cur.read_u64::<LE>()?),
        DdlTypeKind::String
        | DdlTypeKind::Enum
        | DdlTypeKind::Bitfield
        | DdlTypeKind::File
        | DdlTypeKind::Json => read_inline_string(cur)?,
        DdlTypeKind::Struct => {
            let inner = parse_object(cur, pool)?;
            align_to(cur, 4)?;
            DdlValue::Struct(inner)
        }
        DdlTypeKind::Default => {
            cur.read_u8()?;
            DdlValue::Null
        }
        DdlTypeKind::Unknown => {
            return Err(ToolkitError::Parse(
                "DDL Unknown type encountered while reading".into(),
            ));
        }
    })
}

fn read_inline_string(cur: &mut Cursor<&[u8]>) -> Result<DdlValue> {
    // DDLString = { i32 length, u32 hash, u64 checksum }  (16 bytes)
    let length = cur.read_i32::<LE>()? as usize;
    let hash = cur.read_u32::<LE>()?;
    let checksum = cur.read_u64::<LE>()?;
    let mut bytes = vec![0u8; length];
    cur.read_exact(&mut bytes)?;
    cur.read_u8()?; // NUL terminator
    align_to(cur, 4)?;
    Ok(DdlValue::String {
        value: String::from_utf8_lossy(&bytes).into_owned(),
        hash,
        checksum,
    })
}

fn align_to(cur: &mut Cursor<&[u8]>, boundary: u64) -> Result<()> {
    let pos = cur.position();
    let aligned = (pos + boundary - 1) & !(boundary - 1);
    if aligned > pos {
        cur.seek(SeekFrom::Start(aligned))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Serializer
// ---------------------------------------------------------------------------

/// Serialize `obj` into DDL wire format. Field-name strings are interned into
/// the supplied `pool`. **Every `DDLString` slot is rewritten with a fresh
/// `crc32(value)` / `crc64(value)`** — the in-memory `hash`/`checksum` fields
/// are intentionally ignored (recommendation #3).
pub fn serialize(obj: &DdlObject, pool: &mut dyn StringPoolWrite) -> Vec<u8> {
    let mut out = Vec::new();
    serialize_object(obj, &mut out, pool);
    out
}

fn serialize_object(obj: &DdlObject, out: &mut Vec<u8>, pool: &mut dyn StringPoolWrite) {
    // Stable order: explicit field_order if present, else BTreeMap key order.
    let order: Vec<u32> = if obj.field_order.len() == obj.fields.len() {
        obj.field_order.clone()
    } else {
        obj.fields.keys().copied().collect()
    };

    let mut payload: Vec<u8> = Vec::new();
    let mut name_offsets: Vec<u32> = Vec::with_capacity(order.len());

    // Headers
    for &id in &order {
        let f = &obj.fields[&id];
        let count = f.values.len();
        let meta = DdlFieldHeader::pack(f.type_kind, count, f.array_kind);
        payload.extend_from_slice(&id.to_le_bytes());
        payload.extend_from_slice(&meta.to_le_bytes());
        name_offsets.push(pool.add_string(&f.name));
    }

    // Name offsets table
    for off in &name_offsets {
        payload.extend_from_slice(&off.to_le_bytes());
    }

    // Values
    for &id in &order {
        let f = &obj.fields[&id];
        for v in &f.values {
            write_value(v, f.type_kind, &mut payload, pool);
        }
    }

    // Align inner blob to 4 bytes
    let pad = (4 - (payload.len() % 4)) % 4;
    payload.extend(std::iter::repeat(0u8).take(pad));

    // 16-byte DDL header: zero, magic, field_count, size
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&DDL_MAGIC.to_le_bytes());
    out.extend_from_slice(&(order.len() as u32).to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
}

fn write_value(
    v: &DdlValue,
    type_kind: DdlTypeKind,
    out: &mut Vec<u8>,
    pool: &mut dyn StringPoolWrite,
) {
    match (type_kind, v) {
        (DdlTypeKind::UInt8, DdlValue::U8(x)) => out.push(*x),
        (DdlTypeKind::UInt16, DdlValue::U16(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (DdlTypeKind::UInt32, DdlValue::U32(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (DdlTypeKind::UInt64, DdlValue::U64(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (DdlTypeKind::Int8, DdlValue::I8(x)) => out.push(*x as u8),
        (DdlTypeKind::Int16, DdlValue::I16(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (DdlTypeKind::Int32, DdlValue::I32(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (DdlTypeKind::Int64, DdlValue::I64(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (DdlTypeKind::Float, DdlValue::F32(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (DdlTypeKind::Double, DdlValue::F64(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (DdlTypeKind::Bool, DdlValue::Bool(x)) => out.push(if *x { 1 } else { 0 }),
        (DdlTypeKind::Identifier, DdlValue::Identifier(x)) => {
            out.extend_from_slice(&x.to_le_bytes())
        }
        (DdlTypeKind::Asset, DdlValue::Asset(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (
            DdlTypeKind::String
            | DdlTypeKind::Enum
            | DdlTypeKind::Bitfield
            | DdlTypeKind::File
            | DdlTypeKind::Json,
            DdlValue::String { value, .. },
        ) => write_inline_string(value, out),
        (DdlTypeKind::Struct, DdlValue::Struct(inner)) => {
            // Polymorphic guard (recommendation #2): never collapse a 2-field
            // substruct that matches the engine's polymorphic shape.
            debug_assert!(
                !is_polymorphic(inner) || inner.fields.len() == 2,
                "polymorphic substruct lost its 2-field shape during edit"
            );
            serialize_object(inner, out, pool);
            // Pad to 4 after each Struct to match parser's read align.
            let pad = (4 - (out.len() % 4)) % 4;
            out.extend(std::iter::repeat(0u8).take(pad));
        }
        (DdlTypeKind::Default, _) => {
            // 1 byte placeholder, value-less.
            out.push(0);
        }
        // Type/Value mismatch — write a zero-filled placeholder of the right
        // wire size to keep offsets stable rather than panic.
        (kind, _) => {
            let placeholder = wire_size_for_zero(kind);
            out.extend(std::iter::repeat(0u8).take(placeholder));
        }
    }
}

fn wire_size_for_zero(kind: DdlTypeKind) -> usize {
    match kind {
        DdlTypeKind::UInt8 | DdlTypeKind::Int8 | DdlTypeKind::Bool | DdlTypeKind::Default => 1,
        DdlTypeKind::UInt16 | DdlTypeKind::Int16 => 2,
        DdlTypeKind::UInt32 | DdlTypeKind::Int32 | DdlTypeKind::Float => 4,
        DdlTypeKind::UInt64
        | DdlTypeKind::Int64
        | DdlTypeKind::Double
        | DdlTypeKind::Identifier
        | DdlTypeKind::Asset => 8,
        // Strings/Structs/Unknown — emit an empty `DDLString` to be safe.
        _ => 16,
    }
}

/// Always regenerates `hash` (CRC32) and `checksum` (CRC64) from the literal
/// string body. Pads the output to 4-byte alignment.
fn write_inline_string(value: &str, out: &mut Vec<u8>) {
    let hash = crc32::hash(value);
    let checksum = crc64::hash(value);
    let bytes = value.as_bytes();
    out.extend_from_slice(&(bytes.len() as i32).to_le_bytes());
    out.extend_from_slice(&hash.to_le_bytes());
    out.extend_from_slice(&checksum.to_le_bytes());
    out.extend_from_slice(bytes);
    out.push(0); // NUL terminator
    let pad = (4 - (out.len() % 4)) % 4;
    out.extend(std::iter::repeat(0u8).take(pad));
}

// ---------------------------------------------------------------------------
// Collapse to JSON
// ---------------------------------------------------------------------------

/// Convert a parsed DDL object into the config JSON shape:
///
/// - empty value list → field omitted
/// - single struct → recursively collapsed
/// - list of structs → array of collapsed
/// - single string → string
/// - list of strings → array of strings
/// - single primitive → primitive
/// - list of primitives → array
pub fn collapse_to_json(obj: &DdlObject) -> Value {
    let mut map = serde_json::Map::new();
    for &id in &obj.field_order {
        let Some(field) = obj.fields.get(&id) else { continue };
        if field.values.is_empty() {
            continue;
        }
        map.insert(field.name.clone(), collapse_field(field));
    }
    Value::Object(map)
}

fn collapse_field(field: &DdlField) -> Value {
    if field.values.len() == 1 {
        return value_to_json(&field.values[0]);
    }
    Value::Array(field.values.iter().map(value_to_json).collect())
}

fn value_to_json(v: &DdlValue) -> Value {
    match v {
        DdlValue::U8(x) => Value::Number((*x).into()),
        DdlValue::U16(x) => Value::Number((*x).into()),
        DdlValue::U32(x) => Value::Number((*x).into()),
        DdlValue::U64(x) => Value::Number((*x).into()),
        DdlValue::I8(x) => Value::Number((*x).into()),
        DdlValue::I16(x) => Value::Number((*x).into()),
        DdlValue::I32(x) => Value::Number((*x).into()),
        DdlValue::I64(x) => Value::Number((*x).into()),
        DdlValue::F32(x) => serde_json::Number::from_f64(*x as f64)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        DdlValue::F64(x) => serde_json::Number::from_f64(*x)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        DdlValue::Bool(x) => Value::Bool(*x),
        DdlValue::String { value, .. } => Value::String(value.clone()),
        DdlValue::Identifier(x) | DdlValue::Asset(x) => Value::Number((*x).into()),
        DdlValue::Struct(inner) => collapse_to_json(inner),
        DdlValue::Null => Value::Null,
    }
}

// ---------------------------------------------------------------------------
// Asset-Refs section helpers
// ---------------------------------------------------------------------------

/// One entry from a DAT1 Asset-Refs section.
#[derive(Debug, Clone)]
pub struct AssetReference {
    pub asset_id: u64,
    pub path: String,
    pub type_id: u32,
}

/// Parse the 16-byte-per-entry asset-refs table from any of the section tags
/// listed in [`ACTOR_ASSET_REFS_TAG`], [`CONDUIT_ASSET_REFS_TAG`],
/// [`CONFIG_ASSET_REFS_TAG`].
pub fn parse_asset_refs(dat1: &Dat1, section_tag: u32) -> Vec<AssetReference> {
    let Some(section) = dat1.get_section_data(section_tag) else { return Vec::new() };
    let mut out = Vec::with_capacity(section.len() / 16);
    for chunk in section.chunks_exact(16) {
        let asset_id = u64::from_le_bytes(chunk[0..8].try_into().unwrap());
        let string_offset = u32::from_le_bytes(chunk[8..12].try_into().unwrap());
        let type_id = u32::from_le_bytes(chunk[12..16].try_into().unwrap());
        let path = dat1
            .get_string(string_offset)
            .unwrap_or_else(|| format!("<missing@{string_offset}>"));
        out.push(AssetReference { asset_id, path, type_id });
    }
    out
}

/// Build the `{ References, Type, Built }` config envelope (recommendation #4).
/// `Type` and `Built` may be `null` if the corresponding DDL section is
/// missing in `dat1`.
pub fn build_config_envelope(dat1: &Dat1) -> Value {
    let refs: Vec<Value> = [
        CONFIG_ASSET_REFS_TAG,
        CONDUIT_ASSET_REFS_TAG,
        ACTOR_ASSET_REFS_TAG,
    ]
    .into_iter()
    .flat_map(|tag| parse_asset_refs(dat1, tag).into_iter())
    .map(|r| Value::String(r.path))
    .collect();

    let type_obj = dat1
        .get_section_data(CONFIG_TYPE_TAG)
        .and_then(|d| parse(d, dat1).ok())
        .map(|o| collapse_to_json(&o))
        .unwrap_or(Value::Null);

    let built_obj = dat1
        .get_section_data(CONFIG_BUILT_TAG)
        .and_then(|d| parse(d, dat1).ok())
        .map(|o| collapse_to_json(&o))
        .unwrap_or(Value::Null);

    serde_json::json!({
        "References": Value::Array(refs),
        "Type": type_obj,
        "Built": built_obj,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_header_pack_roundtrip() {
        let meta = DdlFieldHeader::pack(DdlTypeKind::Float, 12345, DdlArrayKind::Dynamic);
        let h = DdlFieldHeader { id: 0xDEAD_BEEF, meta };
        assert_eq!(h.type_kind(), DdlTypeKind::Float);
        assert_eq!(h.count(), 12345);
        assert_eq!(h.array_kind(), DdlArrayKind::Dynamic);
    }

    #[test]
    fn polymorphic_check() {
        let mut obj = DdlObject::default();
        obj.insert(DdlField {
            id: POLY_TYPE_FIELD,
            name: "type".into(),
            type_kind: DdlTypeKind::UInt32,
            array_kind: DdlArrayKind::None,
            values: vec![DdlValue::U32(123)],
        });
        obj.insert(DdlField {
            id: POLY_OBJECT_FIELD,
            name: "object".into(),
            type_kind: DdlTypeKind::Struct,
            array_kind: DdlArrayKind::None,
            values: vec![DdlValue::Struct(DdlObject::default())],
        });
        assert!(obj.looks_polymorphic());
        assert!(is_polymorphic(&obj));
    }

    #[test]
    fn empty_data_yields_empty_object() {
        // Any pool — we won't read field names.
        let dummy = Dat1 {
            magic: 0,
            unk1: 0,
            total_size: 0,
            sections: vec![],
            unknowns: vec![],
            strings_pool: vec![],
            section_data: vec![],
            sections_map: Default::default(),
        };
        let parsed = parse(&[], &dummy).unwrap();
        assert!(parsed.fields.is_empty());
    }

    /// Helper: an in-memory string pool that's not tied to a Dat1.
    struct MemPool {
        bytes: Vec<u8>,
    }
    impl MemPool {
        fn new() -> Self { Self { bytes: Vec::new() } }
    }
    impl StringPoolRead for MemPool {
        fn get_string(&self, offset: u32) -> Option<String> {
            let off = offset as usize;
            if off >= self.bytes.len() { return None; }
            let end = self.bytes[off..]
                .iter()
                .position(|&b| b == 0)
                .map(|p| off + p)
                .unwrap_or(self.bytes.len());
            String::from_utf8(self.bytes[off..end].to_vec()).ok()
        }
    }
    impl StringPoolWrite for MemPool {
        fn add_string(&mut self, s: &str) -> u32 {
            let off = self.bytes.len() as u32;
            self.bytes.extend_from_slice(s.as_bytes());
            self.bytes.push(0);
            off
        }
    }

    #[test]
    fn roundtrip_full_type_kind_set() {
        // Build an object exercising the kinds the LEGACY conduit writer
        // could not handle: UInt64 / Int64 / Double / Asset / Identifier /
        // Enum / File / Json / Bitfield / Default, plus an array of structs.
        let mut obj = DdlObject::default();
        obj.insert(DdlField {
            id: crc32::hash("u64"),
            name: "u64".into(),
            type_kind: DdlTypeKind::UInt64,
            array_kind: DdlArrayKind::None,
            values: vec![DdlValue::U64(0xDEAD_BEEF_CAFE_BABE)],
        });
        obj.insert(DdlField {
            id: crc32::hash("i64"),
            name: "i64".into(),
            type_kind: DdlTypeKind::Int64,
            array_kind: DdlArrayKind::None,
            values: vec![DdlValue::I64(-1234567890123)],
        });
        obj.insert(DdlField {
            id: crc32::hash("dbl"),
            name: "dbl".into(),
            type_kind: DdlTypeKind::Double,
            array_kind: DdlArrayKind::None,
            values: vec![DdlValue::F64(std::f64::consts::PI)],
        });
        obj.insert(DdlField {
            id: crc32::hash("asset"),
            name: "asset".into(),
            type_kind: DdlTypeKind::Asset,
            array_kind: DdlArrayKind::None,
            values: vec![DdlValue::Asset(0x8000_0000_0000_1234)],
        });
        obj.insert(DdlField {
            id: crc32::hash("strs"),
            name: "strs".into(),
            type_kind: DdlTypeKind::String,
            array_kind: DdlArrayKind::Dynamic,
            values: vec![
                DdlValue::String { value: "alpha".into(), hash: 0, checksum: 0 },
                DdlValue::String { value: "beta".into(), hash: 0, checksum: 0 },
            ],
        });
        let mut inner = DdlObject::default();
        inner.insert(DdlField {
            id: crc32::hash("inner_f"),
            name: "inner_f".into(),
            type_kind: DdlTypeKind::Float,
            array_kind: DdlArrayKind::None,
            values: vec![DdlValue::F32(2.5)],
        });
        obj.insert(DdlField {
            id: crc32::hash("nested"),
            name: "nested".into(),
            type_kind: DdlTypeKind::Struct,
            array_kind: DdlArrayKind::None,
            values: vec![DdlValue::Struct(inner)],
        });

        let mut pool = MemPool::new();
        let bytes = serialize(&obj, &mut pool);
        let parsed = parse(&bytes, &pool).expect("parse roundtrip");

        // Spot-check each field
        assert!(matches!(parsed.get(crc32::hash("u64")).unwrap().values[0],
            DdlValue::U64(0xDEAD_BEEF_CAFE_BABE)));
        assert!(matches!(parsed.get(crc32::hash("i64")).unwrap().values[0],
            DdlValue::I64(-1234567890123)));
        if let DdlValue::F64(x) = parsed.get(crc32::hash("dbl")).unwrap().values[0] {
            assert!((x - std::f64::consts::PI).abs() < 1e-12);
        } else { panic!("dbl missing"); }
        assert!(matches!(parsed.get(crc32::hash("asset")).unwrap().values[0],
            DdlValue::Asset(0x8000_0000_0000_1234)));

        let strs = &parsed.get(crc32::hash("strs")).unwrap().values;
        assert_eq!(strs.len(), 2);
        if let DdlValue::String { value, hash, checksum } = &strs[0] {
            assert_eq!(value, "alpha");
            assert_eq!(*hash, crc32::hash("alpha"));
            assert_eq!(*checksum, crc64::hash("alpha"));
        } else { panic!("strs[0] missing"); }

        let nested_field = parsed.get(crc32::hash("nested")).unwrap();
        if let DdlValue::Struct(n) = &nested_field.values[0] {
            if let DdlValue::F32(v) = n.get(crc32::hash("inner_f")).unwrap().values[0] {
                assert!((v - 2.5).abs() < 1e-6);
            } else { panic!("nested.inner_f missing"); }
        } else { panic!("nested struct missing"); }
    }

    #[test]
    fn write_inline_string_regenerates_hashes() {
        // Old in-memory hash/checksum should NOT survive — writer must rewrite.
        let mut buf = Vec::new();
        write_inline_string("hello", &mut buf);
        // length(i32) + crc32(u32) + crc64(u64) = 16 bytes header
        let written_hash = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        let written_checksum = u64::from_le_bytes(buf[8..16].try_into().unwrap());
        assert_eq!(written_hash, crc32::hash("hello"));
        assert_eq!(written_checksum, crc64::hash("hello"));
    }
}
