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
//! Re-measured on the PC build with `conduit_probe census`: the name list knows
//! 3,114 `.conduit` paths, of which **1,070 are present in the TOC**. All 1,070
//! parse with the code below and the decoder consumes the `Conduit Built`
//! section to its last byte on 1,070 of 1,070.
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
//! `crc32("Conduit Asset Refs")` is exactly `0x2F4056CE`. 24 conduits carry no
//! refs section; on 19 of those every response really is a pure value edit
//! (`kConstant` / `kMaterial`) that names no external asset, so "value-only
//! conduits need no ref table" is the *tendency*, not a rule — the other 5 have
//! `kVFX` or `kSound` responses whose constants name assets that the builder
//! did not list.
//!
//! `Conduit Built` is one DDL object — `EffectConduitMapping` — and is read by
//! the generic [`crate::core::ddl`] parser. Field *names* come from the asset's
//! own DAT1 string pool, so nothing here depends on an external name list.
//! `Conduit Asset Refs` is the ordinary 16-byte-per-entry asset-reference table
//! ([`ddl::parse_asset_refs`]). Wwise sound-event references are interleaved
//! into the same table by the builder, under their own type tag — see
//! [`ConduitRef`].
//!
//! The two other root fields are small and both constant across the build:
//! `BuiltValueStackSize` is 8 on all 1,070, and `AssetId64` is **0 on all
//! 1,070** (the builder narrows it to a one-byte zero). It is *not* the asset's
//! own id — that matched 0 of 1,070 — so nothing should be keyed off it.
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
//!               ECMField          73,506   Type, Value
//! ```
//!
//! `ECMField` is two per `ECMFieldMap` (`Source` and `Dest`) *plus* two per
//! `ECMSelector` (`Value` is a fixed 2-element array — left and right operand
//! of the comparison): 32,443 * 2 + 4,310 * 2 = 73,506.
//!
//! # The `Events` map key
//!
//! The key is **not** `crc32(ECMEvent.Name)` — that matched 0 of 4,006 shipped
//! events. A DDL map field id is the crc32 of the key *rendered as text*, and
//! the authored key of this map is the event's name hash as a decimal number,
//! so the field id is
//!
//! ```text
//!   field_id = crc32( decimal_text( crc32(ECMEvent.Name) ) )
//! ```
//!
//! which matches **4,006 of 4,006** (`conduit_probe keycheck`). The DAT1 string
//! at the map entry's `name_offset` is that decimal text, e.g. `"329625189"`
//! for `crc32("BotTelekinesisStartedEvent") == 0x13A5AE65`. See
//! [`Event::key_matches_name`] and [`key_for_event_name`].
//!
//! Event names and keys are 1:1 over the whole build: 793 distinct names, 793
//! distinct keys, no name under two keys and no key under two names.
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
use crate::core::ddl;
use crate::core::ddl_schema;
use crate::core::error::{Result, ToolkitError};
use crate::core::prius::{self, NameCtx, PObject, PValue};

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
///
/// All nine members occur in the shipped build (`conduit_probe responses`, over
/// 10,187 responses): `kVFX` 5,256, `kSound` 3,782, `kWWise` 437,
/// `kConstant` 264, `kMaterial` 193, `kDecal` 129, `kKeyFrameMod` 50,
/// `kCompositeShaderConstant` 39, `kCompositeShader` 37. No tenth member
/// appears, so this table is complete for reading shipped data.
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
///
/// Measured over 32,443 mappings: a `Dest` is only ever `kFieldResponse`
/// (32,203) or `kFieldStack` (240); a `Source` is `kFieldConst` 25,543,
/// `kFieldActor` 5,616, `kFieldEvent` 1,222, `kFieldSender` 56,
/// `kFieldGlobal` 6. All seven members occur.
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
///
/// Only four of the ten occur in shipped data: `kMapOpAssign` 32,203,
/// `kMapOpRandRange` 205, `kMapOpMul` 24, `kMapOpRandSelect` 11. The other six
/// are carried here for completeness and have **not** been seen on the wire, so
/// a writer must not assume its encoding of them is exercised.
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
///
/// All seven occur, over 4,310 selectors: `kSelOpEQ` 3,665, `kSelOpGT` 244,
/// `kSelOpNEQ` 156, `kSelOpMABF` 133, `kSelOpGTE` 55, `kSelOpLT` 38,
/// `kSelOpLTE` 19.
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
///
/// Over 6,105 filters: `kSelfOnly` 6,005, `kAll` 90, `kRemoteOnly` 10.
pub const FILTER_SCOPES: &[(&str, &str)] = &[
    ("kAll", "All Events"),
    ("kSelfOnly", "Self Event Only"),
    ("kRemoteOnly", "Remote Event Only"),
];

/// `ECMFilter.ContinueType` — what happens to later filters once this one fires.
///
/// Over 6,105 filters: `Unresponded` 4,989, `Continue` 1,107, `Stop` 9. Note
/// these three members carry no `k` prefix in the file, unlike every other
/// conduit select.
pub const CONTINUE_TYPES: &[(&str, &str)] = &[
    ("Unresponded", "Unresponded Systems"),
    ("Continue", "Continue All"),
    ("Stop", "Stop All"),
];

/// The `Events` map field id for an event name.
///
/// A DDL map keys its entries by `crc32` of the key rendered as text, and the
/// authored key here is `crc32(name)` written as a decimal number. Verified on
/// 4,006 of 4,006 shipped events by `conduit_probe keycheck`.
pub fn key_for_event_name(name: &str) -> u32 {
    crc32::hash(&crc32::hash(name).to_string())
}

/// The Wwise id that belongs beside a Wwise event name — FNV-**1** (multiply,
/// then xor) of the lowercased name, the same law `crate::core::zone_refs`
/// established for the low dword of a zone's Wwise reference ids.
///
/// Measured on the shipped build (`conduit_probe sounds`): a `kSound`
/// response's `SoundEventId` equals this for 3,624 of 3,632 pairs — the 8
/// exceptions are placeholder rows with an empty name and an id of 0 — and a
/// `kWWise` response's `WwiseActionId` matches on 437 of 437 and its `GroupId`
/// on 408 of 408. So a sound swap does **not** need a Wwise project: rename the
/// event and recompute the id from the name.
///
/// `crate::core::fnv` implements FNV-1**a**, which matches none of them.
pub fn wwise_event_id(name: &str) -> u32 {
    let mut h: u32 = 2_166_136_261;
    for b in name.to_ascii_lowercase().as_bytes() {
        h = h.wrapping_mul(16_777_619);
        h ^= *b as u32;
    }
    h
}

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

    /// The `.visualeffect` this `kVFX` response spawns, or `None`.
    ///
    /// `FXPackage` is present on 5,224 of the 5,256 shipped `kVFX` responses;
    /// the rest inherit it or only modify an effect already running.
    pub fn vfx_package(&self) -> Option<&str> {
        (self.kind == "kVFX").then(|| self.payload("FXPackage")).flatten()
    }

    /// `(SoundEventName, SoundEventId)` of a `kSound` response. Both are stored
    /// as constants, 3,632 times each, and the id is
    /// [`wwise_event_id`]`(name)`.
    pub fn sound_event(&self) -> Option<(&str, u32)> {
        if self.kind != "kSound" {
            return None;
        }
        let name = self.payload("SoundEventName")?;
        let id = self
            .payload("SoundEventId")
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0);
        Some((name, id))
    }

    /// Does the numeric Wwise id beside the name agree with the name?
    /// `None` when this response carries no such pair.
    pub fn sound_id_derives_from_name(&self) -> Option<bool> {
        let (name, id) = self.sound_event()?;
        Some(id == wwise_event_id(name))
    }

    /// Where this response's effect hangs: the joint or locator it attaches to,
    /// preferring the explicit joint over the bare handle.
    ///
    /// Over the shipped `kVFX` responses: `AttachLocJoint` 2,091,
    /// `AttachHashLocJoint` 116, `AttachHandle` 5,000 (almost always sourced
    /// from the receiving actor rather than a constant, which is why this can
    /// return `None` on a response that does attach).
    pub fn attach_point(&self) -> Option<&str> {
        self.payload("AttachLocJoint")
            .or_else(|| self.payload("AttachHashLocJoint"))
            .or_else(|| self.payload("AttachLocator"))
            .or_else(|| self.payload("RefLocator"))
    }

    /// Does the effect follow the thing it is attached to? `FollowLocJoint` on
    /// 570 responses, `FollowActor` on 443.
    pub fn follows(&self) -> bool {
        matches!(self.payload("FollowLocJoint"), Some(v) if v != "0")
            || matches!(self.payload("FollowActor"), Some(v) if v != "0")
    }

    /// in the file: `(StartDelay, FadeIn, FadeOut, MaxInstances)`.
    ///
    /// Each is a constant when present and `None` when the response leaves the
    /// effect's own default alone. Counts over shipped `kVFX`: `StartDelay` 93,
    /// `FadeIn` 46, `FadeOut` 476, `MaxInstances` 269. There is **no** viewport
    /// mask destination field anywhere in the shipped build — 0 of 32,443
    /// mappings name one — so if `VisualResponseEvent` has one, no conduit sets
    /// it and a UI should not offer it.
    pub fn timing(&self) -> (Option<&str>, Option<&str>, Option<&str>, Option<&str>) {
        (
            self.payload("StartDelay"),
            self.payload("FadeIn"),
            self.payload("FadeOut"),
            self.payload("MaxInstances"),
        )
    }

    /// One line a modder can read: what this response does and with what.
    pub fn summary(&self) -> String {
        let what = match self.kind.as_str() {
            "kVFX" => self.vfx_package().map(str::to_string),
            "kSound" => self.sound_event().map(|(n, i)| format!("{n} (id {i})")),
            "kDecal" => self.payload("Material").map(str::to_string),
            "kMaterial" => self.payload("Material").map(str::to_string),
            "kWWise" => self.payload("WwiseActionName").map(str::to_string),
            "kConstant" | "kCompositeShaderConstant" => {
                self.payload("ConstantName").map(str::to_string)
            }
            _ => None,
        }
        .unwrap_or_else(|| "<from event / actor>".into());
        let mut line = format!("[{}] {what}", self.kind_label());
        if let Some(a) = self.attach_point() {
            line.push_str(&format!("  on {a}"));
        }
        line
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
    /// Does [`Self::key`] agree with [`Self::name`] under the map-key law?
    ///
    /// True on 4,006 of 4,006 shipped events (`conduit_probe keycheck`). An
    /// earlier version of this compared the key against `crc32(name)`, which is
    /// wrong and returned false for every event in the game; see
    /// [`key_for_event_name`].
    pub fn key_matches_name(&self) -> bool {
        key_for_event_name(&self.name) == self.key
    }

    /// `crc32(name)` — the event's own name hash, which is what the map key is
    /// built from and what the runtime dispatches on.
    pub fn name_hash(&self) -> u32 {
        crc32::hash(&self.name)
    }

    /// Every response under this event, with the filter that gates it.
    pub fn responses(&self) -> Vec<(&Filter, &Response)> {
        self.filters
            .iter()
            .flat_map(|f| f.responses.iter().map(move |r| (f, r)))
            .collect()
    }
}

/// `crc32` type tag of a Wwise event reference row, as
/// [`crate::core::zone_refs::TAG_WWISE_EVENT`] — no CRC-32 preimage is known
/// for it, so it is named by what it does.
pub const REF_TAG_WWISE_EVENT: u32 = 0x3F08_A054;
/// `crc32(".visualeffect")` — the tag on an `FXPackage` row.
pub const REF_TAG_VISUAL_EFFECT: u32 = 0xDABA_2AEA;
/// `crc32(".material")`.
pub const REF_TAG_MATERIAL: u32 = 0xB5AA_FACC;

/// One row of the `Conduit Asset Refs` table.
///
/// The builder writes file references and Wwise sound-event references into
/// the *same* table, so a row is not necessarily a file: see
/// [`Self::is_wwise_event`].
///
/// Measured over all 1,070 conduits (`conduit_probe refs`): **6,108 rows**, and
/// exactly three type tags — [`REF_TAG_WWISE_EVENT`] 3,623,
/// [`REF_TAG_VISUAL_EFFECT`] 2,196, [`REF_TAG_MATERIAL`] 289. So a conduit only
/// ever declares three kinds of dependency. Every one of the 2,485 file rows has
/// `asset_id == crc64::hash(lowercased path)`; **no row has `asset_id == 0`**,
/// contrary to an earlier note here.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConduitRef {
    pub asset_id: u64,
    /// An asset path, or a bare Wwise event name.
    pub path: String,
    pub type_id: u32,
}

impl ConduitRef {
    /// A Wwise event row rather than a file row, decided by the row's own type
    /// tag. The "no dot in the path" heuristic this used to use agrees on
    /// 3,623 of 3,623 rows, but the tag is what the loader reads, so the tag is
    /// what is trusted here.
    pub fn is_wwise_event(&self) -> bool {
        self.type_id == REF_TAG_WWISE_EVENT
    }

    /// `".visualeffect"`, `".material"`, or `"wwise event"` — what this row
    /// points at, from the tag rather than from the spelling.
    pub fn kind(&self) -> &'static str {
        match self.type_id {
            REF_TAG_WWISE_EVENT => "wwise event",
            REF_TAG_VISUAL_EFFECT => ".visualeffect",
            REF_TAG_MATERIAL => ".material",
            _ => "unknown",
        }
    }

    /// For a file row, does the id match the path? (`crc64` of the normalised
    /// path.) True on 2,485 of 2,485 shipped file rows, so a repointing writer
    /// has to recompute it.
    pub fn id_matches_path(&self) -> bool {
        !self.is_wwise_event() && crate::core::crc64::hash(&self.path) == self.asset_id
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
    /// Size of the `Conduit Built` section.
    pub built_bytes: usize,
    /// Bytes the decoder actually consumed. Equal to `built_bytes` on every one
    /// of the 1,070 shipped conduits -- `conduit_probe census` asserts it.
    pub decoded_bytes: usize,
}

impl Conduit {
    /// Parse a conduit asset, with or without the 36-byte asset header.
    pub fn parse(asset: &[u8]) -> Result<Self> {
        Self::from_dat1(&open_dat1(asset)?)
    }

    /// Parse from an already-opened `DAT1`.
    ///
    /// Field names come from the asset's own string pool. The external DDL type
    /// database is consulted only for labels and enum member sets when one is
    /// configured; nothing here needs it.
    pub fn from_dat1(dat1: &Dat1) -> Result<Self> {
        let section = dat1
            .get_section_data(CONDUIT_BUILT_TAG)
            .ok_or(ToolkitError::SectionNotFound(CONDUIT_BUILT_TAG))?;
        let schema = ddl_schema::get_default();
        let schema_ref = if schema.is_empty() { None } else { Some(schema.as_ref()) };
        let ctx = NameCtx::new(Some(dat1), schema_ref);
        let root = prius::decode(section, 0, None, &ctx)?;
        let consumed = root.span.1;

        // `Events` is a DDL map: one field per entry, and the field id is the key.
        let mut events = Vec::new();
        if let Some(map) = struct_of(&root, "Events") {
            for f in &map.fields {
                for v in &f.values {
                    if let PValue::Struct(ev) = v {
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
            built_bytes: section.len(),
            decoded_bytes: consumed,
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

    /// Every `.visualeffect` this conduit can spawn, deduplicated, in file
    /// order of first mention — the list a VFX swap picks from.
    pub fn visual_effects(&self) -> Vec<&str> {
        let mut seen = std::collections::BTreeSet::new();
        self.responses()
            .into_iter()
            .filter_map(|(_, _, r)| r.vfx_package())
            .filter(|p| seen.insert(*p))
            .collect()
    }

    /// Every Wwise event this conduit can post, as `(name, id)`, deduplicated.
    pub fn sound_events(&self) -> Vec<(&str, u32)> {
        let mut seen = std::collections::BTreeSet::new();
        self.responses()
            .into_iter()
            .filter_map(|(_, _, r)| r.sound_event())
            .filter(|(n, _)| seen.insert(*n))
            .collect()
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
//
// These run on [`crate::core::prius::decode`], not on [`crate::core::ddl::parse`].
// Both readers agree on the shipped conduits -- `conduit_probe cmpddl` re-measures
// that -- but only the prius one is provably complete: it records the byte span of
// every object it reads, so the census can assert that decoding a `Conduit Built`
// section consumes it to the last byte, and it spends the one byte a type-19
// `Default` field occupies even when its declared count is 0, which is where the
// generic reader loses struct-array element boundaries.

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

/// Every struct value of a named field, in file order.
fn structs_of<'a>(obj: &'a PObject, name: &str) -> Vec<&'a PObject> {
    obj.field_named(name)
        .map(|f| {
            f.values
                .iter()
                .filter_map(|v| match v {
                    PValue::Struct(o) => Some(o.as_ref()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn struct_of<'a>(obj: &'a PObject, name: &str) -> Option<&'a PObject> {
    structs_of(obj, name).into_iter().next()
}

fn string_of(obj: &PObject, name: &str) -> String {
    match obj.field_named(name).and_then(|f| f.values.first()) {
        Some(PValue::Str { value, .. }) => value.clone(),
        _ => String::new(),
    }
}

fn bool_of(obj: &PObject, name: &str) -> bool {
    matches!(
        obj.field_named(name).and_then(|f| f.values.first()),
        Some(PValue::Bool(true))
    )
}

/// The builder narrows every integer to the smallest lossless type, so a field
/// declared `uint32` can arrive as `U8`. Read whatever width it used.
fn uint_of(obj: &PObject, name: &str) -> Option<u64> {
    Some(match obj.field_named(name)?.values.first()? {
        PValue::U8(v) => *v as u64,
        PValue::U16(v) => *v as u64,
        PValue::U32(v) => *v as u64,
        PValue::U64(v) => *v,
        PValue::I8(v) => *v as u64,
        PValue::I16(v) => *v as u64,
        PValue::I32(v) => *v as u64,
        PValue::I64(v) => *v as u64,
        PValue::Tuid(v) | PValue::AssetId(v) => *v,
        _ => return None,
    })
}

fn read_field_ref(obj: &PObject) -> FieldRef {
    FieldRef {
        kind: string_of(obj, "Type"),
        value: string_of(obj, "Value"),
    }
}

fn read_event(key: u32, obj: &PObject) -> Event {
    Event {
        key,
        name: string_of(obj, "Name"),
        is_valid: bool_of(obj, "IsValid"),
        filters: structs_of(obj, "Filters").into_iter().map(read_filter).collect(),
    }
}

fn read_filter(obj: &PObject) -> Filter {
    Filter {
        name: string_of(obj, "Name"),
        scope: string_of(obj, "Scope"),
        continue_type: string_of(obj, "ContinueType"),
        is_valid: bool_of(obj, "IsValid"),
        selectors: structs_of(obj, "Selector").into_iter().map(read_selector).collect(),
        responses: structs_of(obj, "Response").into_iter().map(read_response).collect(),
    }
}

fn read_selector(obj: &PObject) -> Selector {
    let operands = structs_of(obj, "Value");
    Selector {
        op: string_of(obj, "Op"),
        is_valid: bool_of(obj, "IsValid"),
        lhs: operands.first().map(|o| read_field_ref(o)).unwrap_or_default(),
        rhs: operands.get(1).map(|o| read_field_ref(o)).unwrap_or_default(),
    }
}

fn read_response(obj: &PObject) -> Response {
    Response {
        name: string_of(obj, "Name"),
        kind: string_of(obj, "Type"),
        group_index: uint_of(obj, "GroupIndex").unwrap_or(0) as u32,
        is_valid: bool_of(obj, "IsValid"),
        field_map: structs_of(obj, "FieldMap").into_iter().map(read_mapping).collect(),
    }
}

fn read_mapping(obj: &PObject) -> Mapping {
    Mapping {
        dest: struct_of(obj, "Dest").map(read_field_ref).unwrap_or_default(),
        source: struct_of(obj, "Source").map(read_field_ref).unwrap_or_default(),
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

    /// The map-key law, on a row read straight out of
    /// `emp_nef_finale_telekinesis_object.conduit`: the event is
    /// `BotTelekinesisStartedEvent`, its name hash is `0x13A5AE65` = 329625189,
    /// the string the builder put in the pool at the map entry is `"329625189"`,
    /// and the DDL field id is `0xA8AEFD06`.
    #[test]
    fn events_map_key_is_crc32_of_the_decimal_name_hash() {
        assert_eq!(crc32::hash("BotTelekinesisStartedEvent"), 0x13A5_AE65);
        assert_eq!(0x13A5_AE65u32.to_string(), "329625189");
        assert_eq!(crc32::hash("329625189"), 0xA8AE_FD06);
        assert_eq!(key_for_event_name("BotTelekinesisStartedEvent"), 0xA8AE_FD06);
        // and the old, wrong law does not agree with it
        assert_ne!(crc32::hash("BotTelekinesisStartedEvent"), 0xA8AE_FD06);
    }

    /// Four `SoundEventName` / `SoundEventId` pairs copied out of
    /// `conduit/equipment/weapon/...` blaster conduits.
    #[test]
    fn wwise_ids_are_fnv1_of_the_lowercased_name() {
        for (name, id) in [
            ("Play_HAP_WPN_IcicleBlaster_Fire", 261_346_987u32),
            ("Play_WPN_Blaster_Ardolis_Capgun_fire", 3_282_583_458),
            ("Play_WPN_blaster_equip", 2_876_778_432),
            ("Stop_WPN_Global_equip", 1_654_849_968),
        ] {
            assert_eq!(wwise_event_id(name), id, "{name}");
        }
        // FNV-1a, which core::fnv implements, matches none of them.
        assert_ne!(
            crate::core::fnv::hash_string("play_wpn_blaster_equip"),
            2_876_778_432
        );
    }

    #[test]
    fn ref_tags_split_the_three_shipped_kinds() {
        let wwise = ConduitRef {
            asset_id: 1,
            path: "Play_WPN_blaster_equip".into(),
            type_id: REF_TAG_WWISE_EVENT,
        };
        assert!(wwise.is_wwise_event());
        assert_eq!(wwise.kind(), "wwise event");
        assert!(!wwise.id_matches_path());
        let path = "visualeffect/enm/nefarious/finale_emp_nef_tk_object.visualeffect";
        let vfx = ConduitRef {
            asset_id: crate::core::crc64::hash(path),
            path: path.into(),
            type_id: REF_TAG_VISUAL_EFFECT,
        };
        assert!(!vfx.is_wwise_event());
        assert_eq!(vfx.kind(), ".visualeffect");
        assert!(vfx.id_matches_path());
    }

    #[test]
    fn labels_resolve_for_known_members_only() {
        assert_eq!(member_label("kVFX"), Some("Visual Effect"));
        assert_eq!(member_label("kMapOpAssign"), Some("="));
        assert_eq!(member_label("kSelOpMABF"), Some("matches any bit flag"));
        assert_eq!(member_label("kNotAThing"), None);
    }
}
