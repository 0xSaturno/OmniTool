//! `.conduit` — the event → response routing table.
//!
//! A conduit is the wiring that turns a *game event* ("this animation reached
//! its fire frame", "this projectile hit something", "this weapon levelled up")
//! into a *response* ("spawn that visual effect on that joint", "post that
//! Wwise event", "override that material parameter"). It is where almost all of
//! the game's per-weapon / per-enemy VFX and SFX association actually lives:
//! neither the `.model` nor the `.actor` names the particle system, the conduit
//! does. Swapping a muzzle flash or a hit sound is therefore a conduit edit.
//!
//! # What ships
//!
//! Measured on the PC build with `conduit_probe census`: the name list knows
//! 3,114 `.conduit` paths, of which **1,070 are present in the TOC**. Every one
//! of the 1,070 parses with the code below.
//!
//! # File layout
//!
//! A built conduit is a bare `DAT1` (no 36-byte asset wrapper once extracted by
//! `Toc::extract_asset`, which prepends one — both forms are accepted here)
//! carrying at most two sections, and nothing else. Measured over all 1,070:
//!
//! ```text
//!   tag         crc32 of              in assets   total bytes
//!   0xCEB30E68  "Conduit Built"           1,070    13,917,312
//!   0x2F4056CE  "Conduit Asset Refs"      1,046        97,728
//! ```
//!
//! Both tag names are confirmed, not guessed: `crc32("Conduit Built")` in the
//! Insomniac variant (see [`crate::core::crc32`]) is exactly `0xCEB30E68` and
//! `crc32("Conduit Asset Refs")` is exactly `0x2F4056CE`. The 24 conduits with
//! no refs section are the ones whose every response is a pure value edit
//! (`kConstant` / `kMaterial`) that names no external asset.
//!
//! `Conduit Built` is one DDL object — `EffectConduitMapping` — and is read by
//! the generic [`crate::core::ddl`] parser. Field *names* come from the asset's
//! own DAT1 string pool, so nothing here depends on an external name list.
//! `Conduit Asset Refs` is the ordinary 16-byte-per-entry asset-reference table
//! ([`ddl::parse_asset_refs`]); Wwise event references are interleaved into the
//! same table by the builder, which is why some entries carry an `asset_id` of
//! zero and a bare event name instead of a path.
//!
//! # Object model
//!
//! Five nested DDL structs, all measured over the shipped 1,070:
//!
//! ```text
//!   EffectConduitMapping           1,070   BuiltValueStackSize, AssetId64, Events
//!     Events  <map, key = crc32(Name)>
//!       ECMEvent                   4,006   Name, IsValid, Filters
//!         ECMFilter                6,105   Name, Scope, ContinueType, IsValid,
//!                                          Selector[], Response[]
//!           ECMSelector            4,310   Op, IsValid, Value[2]
//!           ECMResponse           10,187   Name, Type, GroupIndex, IsValid,
//!                                          FieldMap[]
//!             ECMFieldMap         32,443   Source, Dest, Op
//!               ECMField          64,886   Type, Value        (2 per FieldMap)
//! ```
//!
//! `ECMSelector.Value` is a *fixed* 2-element array — left and right operand of
//! the comparison — which is why the selector count doubles into `ECMField`.
//!
//! The `Events` map key is `crc32(ECMEvent.Name)`; `conduit_probe keys` checks
//! that on every event of every shipped conduit.
//!
//! # How a response is actually parameterised
//!
//! `ECMResponse` has no typed payload. Instead the response's own DDL struct —
//! `VisualResponseEvent` for `kVFX`, `SoundResponseEvent` for `kSound`, and so
//! on — is filled in *by name* through `FieldMap`: each entry is
//! `Dest <op> Source`, where `Dest` is almost always a `kFieldResponse` field
//! path on the response struct (`"FXPackage"`, `"Orientation.Position"`,
//! `"StartDelay"`, `"AttachJoint"`) and `Source` is either a `kFieldConst`
//! literal (an asset path, a number, a Wwise event name) or a `kFieldEvent`
//! field path pulled off the incoming game event.
//!
//! So a conduit editor is a *string-keyed property sheet* per response, not a
//! fixed struct editor, and a VFX swap is a one-string edit: replace the
//! `kFieldConst` value whose `Dest` is `FXPackage`.

use std::collections::BTreeMap;

use crate::core::crc32;
use crate::core::dat1::{Dat1, DAT1_MAGIC};
use crate::core::ddl::{self, DdlObject, DdlValue};
use crate::core::error::{Result, ToolkitError};

/// `crc32("Conduit Built")` — the single DDL section of a built conduit.
pub const CONDUIT_BUILT_TAG: u32 = 0xCEB3_0E68;
/// `crc32("Conduit Asset Refs")` — asset + Wwise references, 16 bytes/entry.
pub const CONDUIT_ASSET_REFS_TAG: u32 = 0x2F40_56CE;

/// The 36-byte header `Toc::extract_asset` prepends to every asset.
const ASSET_HEADER_LEN: usize = 36;

// ---------------------------------------------------------------------------
// Enum member tables
// ---------------------------------------------------------------------------

/// `ECMResponseType` — what kind of thing a response does.
///
/// Members are the literal strings the shipped files carry in the
/// `ECMResponse.Type` slot (DDL writes `select` fields as their member name).
/// The second column is the designer-facing label the same select carries in
/// the DDL type database, kept here so a UI can show it without a database.
pub const RESPONSE_TYPES: &[(&str, &str)] = &[
    ("kKeyFrameMod", "VFX KeyFrame Modifier"),
    ("kVFX", "Visual Effect"),
    ("kWWise", "WWise Game Sync"),
    ("kSound", "Wwise Sound Event"),
    ("kDecal", "Decal Effect"),
    ("kMaterial", "Material Override"),
    ("kConstant", "Constant Override"),
    ("kCompositeShader", "Composite Shader"),
    ("kCompositeShaderConstant", "Composite Shader Constant"),
];

/// `ECMFieldType` — where one side of a field mapping reads from / writes to.
pub const FIELD_TYPES: &[(&str, &str)] = &[
    ("kFieldResponse", "Effect Response"),
    ("kFieldStack", "Field Stack"),
    ("kFieldConst", "Constant"),
    ("kFieldEvent", "Input Event"),
    ("kFieldSender", "Sending Actor"),
    ("kFieldActor", "Receiving Actor"),
    ("kFieldGlobal", "Global Value"),
];

/// `ECMFieldMapOp` — the assignment operator of one `FieldMap` row.
pub const MAP_OPS: &[(&str, &str)] = &[
    ("kMapOpAssign", "="),
    ("kMapOpMul", "*="),
    ("kMapOpDiv", "/="),
    ("kMapOpAdd", "+="),
    ("kMapOpSub", "-="),
    ("kMapOpMod", "mod="),
    ("kMapOpAnd", "&&="),
    ("kMapOpOr", "||="),
    ("kMapOpRandSelect", "RS="),
    ("kMapOpRandRange", "RR="),
];

/// `ECMSelectType` — the comparison a filter's selector performs.
pub const SELECT_OPS: &[(&str, &str)] = &[
    ("kSelOpEQ", "=="),
    ("kSelOpNEQ", "!="),
    ("kSelOpGT", ">"),
    ("kSelOpGTE", ">="),
    ("kSelOpLT", "<"),
    ("kSelOpLTE", "<="),
    ("kSelOpMABF", "matches any bit flag"),
];

/// `ECMFilter.Scope` — which copy of the event the filter answers.
pub const FILTER_SCOPES: &[(&str, &str)] = &[
    ("kAll", "All Events"),
    ("kSelfOnly", "Self Event Only"),
    ("kRemoteOnly", "Remote Event Only"),
];

/// `ECMFilter.ContinueType` — what happens to later filters once this one fires.
pub const CONTINUE_TYPES: &[(&str, &str)] = &[
    ("Unresponded", "Unresponded Systems"),
    ("Continue", "Continue All"),
    ("Stop", "Stop All"),
];

fn label_in(table: &[(&'static str, &'static str)], member: &str) -> Option<&'static str> {
    table.iter().find(|(m, _)| *m == member).map(|(_, l)| *l)
}

/// Designer-facing label for any of the six conduit selects, or `None` when the
/// member is not one this build knows. Never guesses.
pub fn member_label(member: &str) -> Option<&'static str> {
    label_in(RESPONSE_TYPES, member)
        .or_else(|| label_in(FIELD_TYPES, member))
        .or_else(|| label_in(MAP_OPS, member))
        .or_else(|| label_in(SELECT_OPS, member))
        .or_else(|| label_in(FILTER_SCOPES, member))
        .or_else(|| label_in(CONTINUE_TYPES, member))
}

// ---------------------------------------------------------------------------
// Decoded model
// ---------------------------------------------------------------------------

/// One side of a `FieldMap` row, or one operand of a `Selector` — an
/// `ECMField`.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct FieldRef {
    /// `ECMFieldType` member, e.g. `"kFieldConst"`, `"kFieldResponse"`.
    pub kind: String,
    /// Free-form: a dotted field path, a literal number, an asset path, or a
    /// Wwise event name — which it is depends on `kind`.
    pub value: String,
}

impl FieldRef {
    /// Label for [`Self::kind`], or the raw member when unknown.
    pub fn kind_label(&self) -> &str {
        label_in(FIELD_TYPES, &self.kind).unwrap_or(&self.kind)
    }
    /// Is this a literal rather than a reference to some other struct's field?
    pub fn is_const(&self) -> bool {
        self.kind == "kFieldConst"
    }
}

/// One `ECMFieldMap` row: `dest <op> source`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Mapping {
    pub dest: FieldRef,
    pub source: FieldRef,
    /// `ECMFieldMapOp` member, e.g. `"kMapOpAssign"`.
    pub op: String,
}

impl Mapping {
    /// `"="`, `"+="`, … for [`Self::op`], or the raw member when unknown.
    pub fn op_label(&self) -> &str {
        label_in(MAP_OPS, &self.op).unwrap_or(&self.op)
    }
    /// One-line rendering a modder can read: `FXPackage = "foo.visualeffect"`.
    pub fn to_line(&self) -> String {
        format!("{} {} {}", self.dest.value, self.op_label(), self.source.value)
    }
}

/// One `ECMResponse` — what to do, and how to fill its parameters in.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Response {
    pub name: String,
    /// `ECMResponseType` member, e.g. `"kVFX"`.
    pub kind: String,
    pub group_index: u32,
    pub is_valid: bool,
    pub field_map: Vec<Mapping>,
}

impl Response {
    /// `"Visual Effect"`, `"Wwise Sound Event"`, … or the raw member.
    pub fn kind_label(&self) -> &str {
        label_in(RESPONSE_TYPES, &self.kind).unwrap_or(&self.kind)
    }

    /// The constant assigned to response field `dest`, if any.
    ///
    /// This is the accessor a VFX/SFX swap uses: `payload("FXPackage")` on a
    /// `kVFX` response is the `.visualeffect` asset that will be spawned.
    pub fn payload(&self, dest: &str) -> Option<&str> {
        self.field_map
            .iter()
            .find(|m| m.dest.value == dest && m.source.is_const())
            .map(|m| m.source.value.as_str())
    }

    /// Every constant this response feeds into its effect struct, in file
    /// order, as `(dest field path, literal)`.
    pub fn constants(&self) -> Vec<(&str, &str)> {
        self.field_map
            .iter()
            .filter(|m| m.source.is_const())
            .map(|m| (m.dest.value.as_str(), m.source.value.as_str()))
            .collect()
    }
}

/// One `ECMSelector` — a guard on a filter. `Value` is a fixed 2-element array.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Selector {
    /// `ECMSelectType` member, e.g. `"kSelOpEQ"`.
    pub op: String,
    pub is_valid: bool,
    pub lhs: FieldRef,
    pub rhs: FieldRef,
}

impl Selector {
    pub fn op_label(&self) -> &str {
        label_in(SELECT_OPS, &self.op).unwrap_or(&self.op)
    }
    pub fn to_line(&self) -> String {
        format!("{} {} {}", self.lhs.value, self.op_label(), self.rhs.value)
    }
}

/// One `ECMFilter` — guards plus the responses they gate.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Filter {
    pub name: String,
    /// `ECMFilter.Scope` member.
    pub scope: String,
    /// `ECMFilter.ContinueType` member.
    pub continue_type: String,
    pub is_valid: bool,
    pub selectors: Vec<Selector>,
    pub responses: Vec<Response>,
}

impl Filter {
    pub fn scope_label(&self) -> &str {
        label_in(FILTER_SCOPES, &self.scope).unwrap_or(&self.scope)
    }
    pub fn continue_label(&self) -> &str {
        label_in(CONTINUE_TYPES, &self.continue_type).unwrap_or(&self.continue_type)
    }
}

/// One `ECMEvent` — a named game event and everything it triggers.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Event {
    /// The map key the event sits under in `EffectConduitMapping.Events`.
    pub key: u32,
    /// The game-event name, e.g. `"CentipedeChaseMineFireEvent"`.
    pub name: String,
    pub is_valid: bool,
    pub filters: Vec<Filter>,
}

impl Event {
    /// Does the map key equal `crc32(name)`? True for every shipped event —
    /// `conduit_probe keys` re-measures it.
    pub fn key_matches_name(&self) -> bool {
        crc32::hash(&self.name) == self.key
    }
}

/// One row of the `Conduit Asset Refs` table.
///
/// The builder writes file references and Wwise sound-event references into
/// the *same* table, in that order, so a row is not necessarily a file: see
/// [`Self::is_wwise_event`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConduitRef {
    pub asset_id: u64,
    /// An asset path, or a bare Wwise event name.
    pub path: String,
    pub type_id: u32,
}

impl ConduitRef {
    /// A Wwise event row rather than a file row. Measured discriminator on the
    /// shipped build — `conduit_probe refs` prints the split.
    pub fn is_wwise_event(&self) -> bool {
        !self.path.contains('.')
    }
}

/// A decoded `.conduit` asset.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Conduit {
    /// Events in file order (which is DDL field order, i.e. sorted by key).
    pub events: Vec<Event>,
    /// `Conduit Asset Refs`, if the section is present.
    pub asset_refs: Vec<ConduitRef>,
    pub built_value_stack_size: u64,
    pub asset_id64: u64,
}

impl Conduit {
    /// Parse a conduit asset, with or without the 36-byte asset header.
    pub fn parse(asset: &[u8]) -> Result<Self> {
        Self::from_dat1(&open_dat1(asset)?)
    }

    /// Parse from an already-opened `DAT1`.
    pub fn from_dat1(dat1: &Dat1) -> Result<Self> {
        let section = dat1
            .get_section_data(CONDUIT_BUILT_TAG)
            .ok_or(ToolkitError::SectionNotFound(CONDUIT_BUILT_TAG))?;
        let root = ddl::parse(section, dat1)?;

        let mut events = Vec::new();
        if let Some(map) = struct_of(&root, "Events") {
            for id in &map.field_order {
                let f = &map.fields[id];
                for v in &f.values {
                    if let DdlValue::Struct(ev) = v {
                        events.push(read_event(f.id, ev));
                    }
                }
            }
        }

        Ok(Self {
            events,
            asset_refs: ddl::parse_asset_refs(dat1, CONDUIT_ASSET_REFS_TAG)
                .into_iter()
                .map(|r| ConduitRef {
                    asset_id: r.asset_id,
                    path: r.path,
                    type_id: r.type_id,
                })
                .collect(),
            built_value_stack_size: uint_of(&root, "BuiltValueStackSize").unwrap_or(0),
            asset_id64: uint_of(&root, "AssetId64").unwrap_or(0),
        })
    }

    /// `(events, filters, selectors, responses, field maps)` — the five counts
    /// the census totals.
    pub fn counts(&self) -> (usize, usize, usize, usize, usize) {
        let mut c = (self.events.len(), 0, 0, 0, 0);
        for e in &self.events {
            c.1 += e.filters.len();
            for f in &e.filters {
                c.2 += f.selectors.len();
                c.3 += f.responses.len();
                for r in &f.responses {
                    c.4 += r.field_map.len();
                }
            }
        }
        c
    }

    /// Every response in the asset, tagged with the event and filter it hangs
    /// off — the flat view a census or a list UI wants.
    pub fn responses(&self) -> Vec<(&Event, &Filter, &Response)> {
        let mut out = Vec::new();
        for e in &self.events {
            for f in &e.filters {
                for r in &f.responses {
                    out.push((e, f, r));
                }
            }
        }
        out
    }

    /// `ECMResponseType` member -> count, for this asset.
    pub fn response_histogram(&self) -> BTreeMap<String, usize> {
        let mut h = BTreeMap::new();
        for (_, _, r) in self.responses() {
            *h.entry(r.kind.clone()).or_insert(0) += 1;
        }
        h
    }
}

// ---------------------------------------------------------------------------
// DDL readers
// ---------------------------------------------------------------------------

fn open_dat1(asset: &[u8]) -> Result<Dat1> {
    let magic_at = |o: usize| {
        asset.len() >= o + 4
            && u32::from_le_bytes(asset[o..o + 4].try_into().unwrap()) == DAT1_MAGIC
    };
    if magic_at(0) {
        Dat1::parse(asset)
    } else if magic_at(ASSET_HEADER_LEN) {
        Dat1::parse(&asset[ASSET_HEADER_LEN..])
    } else {
        Err(ToolkitError::InvalidMagic {
            expected: DAT1_MAGIC,
            got: if asset.len() >= 4 {
                u32::from_le_bytes(asset[0..4].try_into().unwrap())
            } else {
                0
            },
        })
    }
}

fn field<'a>(obj: &'a DdlObject, name: &str) -> Option<&'a ddl::DdlField> {
    obj.fields.values().find(|f| f.name == name)
}

fn struct_of<'a>(obj: &'a DdlObject, name: &str) -> Option<&'a DdlObject> {
    match field(obj, name)?.values.first()? {
        DdlValue::Struct(s) => Some(s),
        _ => None,
    }
}

fn structs_of<'a>(obj: &'a DdlObject, name: &str) -> Vec<&'a DdlObject> {
    field(obj, name)
        .map(|f| {
            f.values
                .iter()
                .filter_map(|v| match v {
                    DdlValue::Struct(s) => Some(s),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn string_of(obj: &DdlObject, name: &str) -> String {
    match field(obj, name).and_then(|f| f.values.first()) {
        Some(DdlValue::String { value, .. }) => value.clone(),
        _ => String::new(),
    }
}

fn bool_of(obj: &DdlObject, name: &str) -> bool {
    matches!(
        field(obj, name).and_then(|f| f.values.first()),
        Some(DdlValue::Bool(true))
    )
}

/// The builder narrows every integer to the smallest lossless type, so a field
/// declared `uint32` can arrive as `UInt8`. Read whatever width it used.
fn uint_of(obj: &DdlObject, name: &str) -> Option<u64> {
    Some(match field(obj, name)?.values.first()? {
        DdlValue::U8(v) => *v as u64,
        DdlValue::U16(v) => *v as u64,
        DdlValue::U32(v) => *v as u64,
        DdlValue::U64(v) => *v,
        DdlValue::I8(v) => *v as u64,
        DdlValue::I16(v) => *v as u64,
        DdlValue::I32(v) => *v as u64,
        DdlValue::I64(v) => *v as u64,
        _ => return None,
    })
}

fn read_field_ref(obj: &DdlObject) -> FieldRef {
    FieldRef {
        kind: string_of(obj, "Type"),
        value: string_of(obj, "Value"),
    }
}

fn read_event(key: u32, obj: &DdlObject) -> Event {
    Event {
        key,
        name: string_of(obj, "Name"),
        is_valid: bool_of(obj, "IsValid"),
        filters: structs_of(obj, "Filters")
            .into_iter()
            .map(read_filter)
            .collect(),
    }
}

fn read_filter(obj: &DdlObject) -> Filter {
    Filter {
        name: string_of(obj, "Name"),
        scope: string_of(obj, "Scope"),
        continue_type: string_of(obj, "ContinueType"),
        is_valid: bool_of(obj, "IsValid"),
        selectors: structs_of(obj, "Selector")
            .into_iter()
            .map(read_selector)
            .collect(),
        responses: structs_of(obj, "Response")
            .into_iter()
            .map(read_response)
            .collect(),
    }
}

fn read_selector(obj: &DdlObject) -> Selector {
    let operands = structs_of(obj, "Value");
    Selector {
        op: string_of(obj, "Op"),
        is_valid: bool_of(obj, "IsValid"),
        lhs: operands.first().map(|o| read_field_ref(o)).unwrap_or_default(),
        rhs: operands.get(1).map(|o| read_field_ref(o)).unwrap_or_default(),
    }
}

fn read_response(obj: &DdlObject) -> Response {
    Response {
        name: string_of(obj, "Name"),
        kind: string_of(obj, "Type"),
        group_index: uint_of(obj, "GroupIndex").unwrap_or(0) as u32,
        is_valid: bool_of(obj, "IsValid"),
        field_map: structs_of(obj, "FieldMap")
            .into_iter()
            .map(read_mapping)
            .collect(),
    }
}

fn read_mapping(obj: &DdlObject) -> Mapping {
    Mapping {
        dest: struct_of(obj, "Dest").map(read_field_ref).unwrap_or_default(),
        source: struct_of(obj, "Source")
            .map(read_field_ref)
            .unwrap_or_default(),
        op: string_of(obj, "Op"),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The two section tags are the Insomniac crc32 of the builder's buffer
    /// names, not magic numbers.
    #[test]
    fn section_tags_are_crc32_of_their_names() {
        assert_eq!(crc32::hash("Conduit Built"), CONDUIT_BUILT_TAG);
        assert_eq!(crc32::hash("Conduit Asset Refs"), CONDUIT_ASSET_REFS_TAG);
    }

    #[test]
    fn labels_resolve_for_known_members_only() {
        assert_eq!(member_label("kVFX"), Some("Visual Effect"));
        assert_eq!(member_label("kMapOpAssign"), Some("="));
        assert_eq!(member_label("kSelOpMABF"), Some("matches any bit flag"));
        assert_eq!(member_label("kNotAThing"), None);
    }
}
