//! `.zone` container: DAT1 sections holding actor instances and the script
//! graph that drives gameplay (arena waves, spawners, rewards).
//!
//! File layout: 36-byte wrapper (`u32 magic`, `u32 dat1_size`, `u32 tail_size`,
//! 24 reserved bytes), the main DAT1, then an optional trailing DAT1 blob
//! ("Zone PID Physics") preserved verbatim.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::core::config::{ddl_object_to_typed_json, extension_hash, typed_json_to_ddl_object};
use crate::core::crc32;
use crate::core::crc64;
use crate::core::dat1::{Dat1, DAT1_MAGIC};
use crate::core::ddl;
use crate::core::error::{Result, ToolkitError};

pub const ZONE_WRAPPER_LEN: usize = 36;

pub const TAG_ACTOR_GROUPS: u32 = 0x0CF5_8A6E;
pub const TAG_ACTOR_GROUP_NAMES: u32 = 0xC496_8A44;
pub const TAG_ACTOR_NAMES: u32 = 0xDC62_5B3D;
pub const TAG_ACTOR_PRIUSES: u32 = 0x50ED_C53D;
pub const TAG_ACTOR_PRIUS_DATA: u32 = 0x8199_9057;
pub const TAG_ACTORS: u32 = 0x7068_2CB8;
pub const TAG_ACTOR_ASSETS: u32 = 0x7868_4035;
pub const TAG_ASSET_REFS: u32 = 0x30DA_DA09;
pub const TAG_MODEL_NAMES: u32 = 0xC6A5_905E;
pub const TAG_SCRIPT_ACTIONS: u32 = 0x5E54_ACCF;
pub const TAG_SCRIPT_PLUGS: u32 = 0xBEAB_52E7;
pub const TAG_SCRIPT_PRIUSES: u32 = 0x2300_D240;
pub const TAG_SCRIPT_STRINGS: u32 = 0xEF86_37D5;
pub const TAG_SCRIPT_STRING_HASHES: u32 = 0x80D2_9828;

pub const TAG_SCRIPT_VARS: u32 = 0xD86A_7934;
pub const TAG_ACTOR_GROUP_MEMBERS: u32 = 0x0410_71EF;

const GROUP_LEN: usize = 24;

const ACTION_LEN: usize = 40;
const PLUG_LEN: usize = 12;
const VAR_LEN: usize = 32;
const ACTOR_LEN: usize = 32;
const ACTOR_PRIUS_LEN: usize = 32;
const ASSET_REF_LEN: usize = 16;

// ---------------------------------------------------------------------------
// Record views
// ---------------------------------------------------------------------------

/// One node of the zone script graph (`Zone Script Actions`, 40 bytes).
#[derive(Debug, Clone)]
pub struct ScriptAction {
    pub node_id: u64,
    pub template_id: u64,
    /// How many link plugs anywhere in the zone target this node.
    pub in_degree: u16,
    /// Outgoing control-flow plugs; `0xFFFF` when the node has none.
    pub link_start: u16,
    pub link_count: u16,
    /// Variable bindings; always starts at `link_start + link_count`.
    pub param_start: u16,
    pub param_count: u16,
    pub type_index: u16,
    pub type_hash: u32,
    /// Byte offset of this node's prius blob inside `TAG_SCRIPT_PRIUSES`.
    pub prius_offset: u32,
    pub prius_size: u32,
}

impl ScriptAction {
    fn read(b: &[u8]) -> Self {
        let rd16 = |o: usize| u16::from_le_bytes(b[o..o + 2].try_into().unwrap());
        let rd32 = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        Self {
            node_id: u64::from_le_bytes(b[0..8].try_into().unwrap()),
            template_id: u64::from_le_bytes(b[8..16].try_into().unwrap()),
            in_degree: rd16(16),
            link_start: rd16(18),
            link_count: rd16(20),
            param_start: rd16(22),
            param_count: rd16(24),
            type_index: rd16(26),
            type_hash: rd32(28),
            prius_offset: rd32(32),
            prius_size: rd32(36),
        }
    }

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

    fn range(start: u16, count: u16) -> std::ops::Range<usize> {
        if start == u16::MAX || count == 0 {
            0..0
        } else {
            start as usize..(start as usize + count as usize)
        }
    }

    pub fn link_range(&self) -> std::ops::Range<usize> {
        Self::range(self.link_start, self.link_count)
    }

    pub fn param_range(&self) -> std::ops::Range<usize> {
        Self::range(self.param_start, self.param_count)
    }
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

/// A named set of actor instances (`Zone Actor Groups`, 24 bytes), used as the
/// pool an `ActorPickAction` draws spawn points from.
#[derive(Debug, Clone)]
pub struct ActorGroup {
    pub name: String,
    pub id: u64,
    /// Zone Actors indices.
    pub members: Vec<u32>,
}

/// One placed actor instance (`Zone Actors`, 32 bytes).
#[derive(Debug, Clone)]
pub struct ZoneActor {
    pub name_index: u32,
    pub asset_index: u32,
    pub scene_object_offset: u32,
    pub prius_start: u32,
    pub prius_count: u32,
    pub flags: u32,
    pub instance_id: u64,
}

impl ZoneActor {
    fn read(b: &[u8]) -> Self {
        let rd = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        Self {
            name_index: rd(0),
            asset_index: rd(4),
            scene_object_offset: rd(8),
            prius_start: rd(12),
            prius_count: rd(16),
            flags: rd(20),
            instance_id: u64::from_le_bytes(b[24..32].try_into().unwrap()),
        }
    }

    pub fn has_priuses(&self) -> bool {
        self.prius_start != u32::MAX
    }
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
}

// ---------------------------------------------------------------------------
// Zone
// ---------------------------------------------------------------------------

pub struct Zone {
    pub wrapper_magic: u32,
    /// Bytes 12..36 of the wrapper, preserved verbatim.
    pub wrapper_reserved: [u8; 24],
    /// Trailing DAT1 blob after the main one; empty when absent.
    pub tail: Vec<u8>,
    pub dat1: Dat1,

    pub actions: Vec<ScriptAction>,
    pub plugs: Vec<ScriptPlug>,
    pub vars: Vec<ScriptVar>,
    pub script_types: Vec<String>,
    /// Original string-pool offsets for `script_types`, preserved verbatim.
    script_string_offsets: Vec<u32>,
    pub actors: Vec<ZoneActor>,
    pub actor_names: Vec<String>,
    pub actor_asset_ids: Vec<u64>,
    pub actor_asset_paths: Vec<String>,
    pub model_names: Vec<String>,
    pub actor_prius_index: Vec<ActorPriusEntry>,
    pub actor_groups: Vec<ActorGroup>,
    pub asset_refs: Vec<AssetRef>,

    pub script_priuses: Vec<PriusBlob>,
    pub actor_priuses: Vec<PriusBlob>,
    /// Blob id backing each `actor_prius_index` entry; entries can share one.
    pub actor_prius_blob: Vec<usize>,
}

impl Zone {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < ZONE_WRAPPER_LEN + 16 {
            return Err(ToolkitError::Parse("zone file too small".into()));
        }
        let wrapper_magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
        let dat1_size = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        let tail_size = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;

        let body_magic = u32::from_le_bytes(
            data[ZONE_WRAPPER_LEN..ZONE_WRAPPER_LEN + 4]
                .try_into()
                .unwrap(),
        );
        if body_magic != DAT1_MAGIC {
            return Err(ToolkitError::InvalidMagic {
                expected: DAT1_MAGIC,
                got: body_magic,
            });
        }

        let dat1_end = ZONE_WRAPPER_LEN
            .checked_add(dat1_size)
            .filter(|e| *e <= data.len())
            .ok_or_else(|| ToolkitError::Parse("zone DAT1 size runs past end of file".into()))?;
        let tail_end = (dat1_end + tail_size).min(data.len());

        let mut wrapper_reserved = [0u8; 24];
        wrapper_reserved.copy_from_slice(&data[12..ZONE_WRAPPER_LEN]);

        let dat1 = Dat1::parse(&data[ZONE_WRAPPER_LEN..dat1_end])?;

        let actions = slice_records(&dat1, TAG_SCRIPT_ACTIONS, ACTION_LEN, ScriptAction::read);
        let plugs = slice_records(&dat1, TAG_SCRIPT_PLUGS, PLUG_LEN, |b| ScriptPlug {
            name_hash: u32::from_le_bytes(b[0..4].try_into().unwrap()),
            index: u32::from_le_bytes(b[4..8].try_into().unwrap()),
            other_hash: u32::from_le_bytes(b[8..12].try_into().unwrap()),
        });
        let vars = slice_records(&dat1, TAG_SCRIPT_VARS, VAR_LEN, ScriptVar::read);
        let actors = slice_records(&dat1, TAG_ACTORS, ACTOR_LEN, ZoneActor::read);
        let actor_prius_index = slice_records(
            &dat1,
            TAG_ACTOR_PRIUSES,
            ACTOR_PRIUS_LEN,
            ActorPriusEntry::read,
        );
        let asset_refs = slice_records(&dat1, TAG_ASSET_REFS, ASSET_REF_LEN, |b| AssetRef {
            asset_id: u64::from_le_bytes(b[0..8].try_into().unwrap()),
            name_offset: u32::from_le_bytes(b[8..12].try_into().unwrap()),
            ext_hash: u32::from_le_bytes(b[12..16].try_into().unwrap()),
        });

        let actor_groups = read_actor_groups(&dat1);
        let script_types = string_table(&dat1, TAG_SCRIPT_STRINGS);
        let actor_names = string_table(&dat1, TAG_ACTOR_NAMES);
        let model_names = string_table(&dat1, TAG_MODEL_NAMES);
        let (actor_asset_ids, actor_asset_paths) = read_actor_assets(&dat1);
        let script_string_offsets = u32_table(&dat1, TAG_SCRIPT_STRINGS);

        let mut zone = Self {
            wrapper_magic,
            wrapper_reserved,
            tail: data[dat1_end..tail_end].to_vec(),
            dat1,
            actions,
            plugs,
            vars,
            script_string_offsets,
            script_types,
            actors,
            actor_names,
            actor_asset_ids,
            actor_asset_paths,
            model_names,
            actor_prius_index,
            actor_groups,
            asset_refs,
            script_priuses: Vec::new(),
            actor_priuses: Vec::new(),
            actor_prius_blob: Vec::new(),
        };
        zone.script_priuses = zone.collect_script_priuses()?;
        let (blobs, mapping) = zone.collect_actor_priuses()?;
        zone.actor_priuses = blobs;
        zone.actor_prius_blob = mapping;
        Ok(zone)
    }

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
            let json = parse_blob(&section, offset, size, &self.dat1)?;
            out.push(PriusBlob {
                offset,
                size,
                json,
                owners,
                raw: raw_slice(&section, offset, size),
                dirty: false,
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
            let json = parse_blob(&section, offset, size, &self.dat1)?;
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
        let start = actor.prius_start as usize;
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

        self.apply_actor_asset_edits(edits)?;
        self.apply_model_name_edits(edits);
        self.apply_script_var_edits(edits)?;
        self.apply_prius_edits(edits)?;
        // Successive copies of the same wave chain onto the previous copy.
        let mut chain_tail: BTreeMap<u32, usize> = BTreeMap::new();
        for req in &edits.clone_waves {
            let after = chain_tail.get(&req.source).copied();
            let report = self.clone_wave(req.source, req.new_number, after)?;
            chain_tail.insert(req.source, report.tail_node);
            if let Some(w) = &report.warning {
                eprintln!("[zone] clone_wave: {w}");
            }
        }
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
        self.dat1.set_section_data(TAG_SCRIPT_ACTIONS, actions)?;

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
        self.dat1.set_section_data(TAG_ACTOR_PRIUSES, index)?;

        let dat1_bytes = self.dat1.save();

        let mut out = Vec::with_capacity(ZONE_WRAPPER_LEN + dat1_bytes.len() + self.tail.len());
        out.extend_from_slice(&self.wrapper_magic.to_le_bytes());
        out.extend_from_slice(&(dat1_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&(self.tail.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.wrapper_reserved);
        out.extend_from_slice(&dat1_bytes);
        out.extend_from_slice(&self.tail);
        Ok(out)
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
        self.dat1.set_section_data(TAG_SCRIPT_PLUGS, plugs)?;

        let mut vars = Vec::with_capacity(self.vars.len() * VAR_LEN);
        for v in &self.vars {
            v.write(&mut vars);
        }
        self.dat1.set_section_data(TAG_SCRIPT_VARS, vars)?;

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
        self.dat1.set_section_data(TAG_SCRIPT_STRINGS, offsets)?;
        self.dat1.set_section_data(TAG_SCRIPT_STRING_HASHES, hashes)
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

        let mut section = Vec::with_capacity(self.actor_asset_ids.len() * 12);
        for id in &self.actor_asset_ids {
            section.extend_from_slice(&id.to_le_bytes());
        }
        let offsets: Vec<u32> = self
            .actor_asset_paths
            .iter()
            .map(|p| intern(&mut self.dat1, p))
            .collect();
        for off in offsets {
            section.extend_from_slice(&off.to_le_bytes());
        }
        self.dat1.set_section_data(TAG_ACTOR_ASSETS, section)
    }

    fn apply_model_name_edits(&mut self, edits: &ZoneEdits) {
        if edits.model_names.is_empty() {
            return;
        }
        for (idx, path) in &edits.model_names {
            if let Some(slot) = self.model_names.get_mut(*idx) {
                *slot = path.trim().to_string();
            }
        }
        let offsets: Vec<u32> = self
            .model_names
            .iter()
            .map(|p| intern(&mut self.dat1, p))
            .collect();
        let mut section = Vec::with_capacity(offsets.len() * 4);
        for off in offsets {
            section.extend_from_slice(&off.to_le_bytes());
        }
        let _ = self.dat1.set_section_data(TAG_MODEL_NAMES, section);
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
        self.dat1.set_section_data(TAG_SCRIPT_PRIUSES, section)
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
        self.dat1.set_section_data(TAG_ACTOR_PRIUS_DATA, section)
    }

    fn blob_bytes(&mut self, blob: &PriusBlob) -> Result<Vec<u8>> {
        if blob.dirty {
            serialize_blob(&blob.json, &mut self.dat1)
        } else {
            Ok(blob.raw.clone())
        }
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
            let asset_id = crc64::hash(path);
            let name_offset = intern(&mut self.dat1, path);
            let ext_hash = extension_hash(path);
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
        let _ = self.dat1.set_section_data(TAG_ASSET_REFS, section);
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
                        self.script_priuses.push(PriusBlob {
                            offset: 0,
                            size: 0,
                            json,
                            owners: vec![new_index],
                            raw: Vec::new(),
                            dirty: true,
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

fn read_actor_groups(dat1: &Dat1) -> Vec<ActorGroup> {
    let Some(d) = dat1.get_section_data(TAG_ACTOR_GROUPS) else {
        return Vec::new();
    };
    let names = string_table(dat1, TAG_ACTOR_GROUP_NAMES);
    let members: Vec<u32> = dat1
        .get_section_data(TAG_ACTOR_GROUP_MEMBERS)
        .map(|m| {
            m.chunks_exact(2)
                .map(|c| u16::from_le_bytes(c.try_into().unwrap()) as u32)
                .collect()
        })
        .unwrap_or_default();

    d.chunks_exact(GROUP_LEN)
        .enumerate()
        .map(|(i, c)| {
            let start = u32::from_le_bytes(c[4..8].try_into().unwrap()) as usize;
            let count = u32::from_le_bytes(c[8..12].try_into().unwrap()) as usize;
            let end = (start + count).min(members.len());
            ActorGroup {
                name: names.get(i).cloned().unwrap_or_default(),
                id: u64::from_le_bytes(c[16..24].try_into().unwrap()),
                members: members.get(start..end).unwrap_or(&[]).to_vec(),
            }
        })
        .collect()
}

/// `TAG_ACTOR_ASSETS` is `u64 id[n]` followed by `u32 name_offset[n]`.
fn read_actor_assets(dat1: &Dat1) -> (Vec<u64>, Vec<String>) {
    let Some(d) = dat1.get_section_data(TAG_ACTOR_ASSETS) else {
        return (Vec::new(), Vec::new());
    };
    let n = d.len() / 12;
    let mut ids = Vec::with_capacity(n);
    let mut paths = Vec::with_capacity(n);
    for i in 0..n {
        ids.push(u64::from_le_bytes(d[i * 8..i * 8 + 8].try_into().unwrap()));
        let o = n * 8 + i * 4;
        let off = u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
        paths.push(dat1.get_string(off).unwrap_or_default());
    }
    (ids, paths)
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
