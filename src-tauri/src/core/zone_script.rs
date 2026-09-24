//! Zone script graphs: nodes, plugs, edges and variables.
//!
//! Gameplay logic in Rift Apart is authored as `.scriptgraph` assets and
//! **flattened into zones** at build time: every node becomes a row of
//! `Zone Script Actions`, every connection a row of `Zone Script Plugs`, every
//! variable a row of `Zone Script Vars`. Nothing in the shipped level references
//! a `.scriptgraph` asset — the zone is the whole program.
//!
//! [`crate::core::zone`] already parses the action rows. This module adds the
//! connectivity, the variables and the names, which is what makes the rows a
//! graph.
//!
//! # Sections
//!
//! ```text
//!   0x5E54ACCF  Zone Script Actions        40-byte rows, one per node
//!   0xBEAB52E7  Zone Script Plugs          12-byte rows, sliced by the action rows
//!   0xD86A7934  Zone Script Vars           32-byte rows
//!   0xEF8637D5  Zone Script Strings        u32[] string-pool offsets
//!   0x80D29828  Zone Script String Hashes  u32[] crc32 of each string, parallel
//!   0x2300D240  Zone Script Priuses        DDL blob store, one blob per node
//! ```
//!
//! `Zone Script Hashes` (`0xD266157E`) exists in the engine's section-tag list
//! but ships in **0 of 9 046** zones, so `Zone Script String Hashes` is the
//! `Zone Script Strings`' length in 973 of 973 scripted zones, and
//! `crc32(string i) == hashes[i]` on **52 286 of 52 286** entries.
//!
//! # The plug row — 12 bytes, measured
//!
//! ```text
//!   +0x00 u32 plug_name_hash         crc32 of the OWNING node's own plug name
//!   +0x04 u32 target_index           out plug: Zone Script Actions ROW INDEX
//!                                    var plug: Zone Script Vars ROW INDEX
//!   +0x08 u32 target_plug_name_hash  out plug: crc32 of the TARGET node's input
//!                                    plug; var plug: crc32("In") or crc32("Out")
//! ```
//!
//! The three words were assigned by measurement over every scripted zone in the
//! shipped PC build (973 zones, 247 073 action rows, 529 943 plug rows):
//!
//! 1. **Which word is the target node index.** `+0x04` is `< action row count`
//!    on **254 172 of 254 172** out-plug rows; `+0x00` and `+0x08` on 0.
//!    Stronger still, counting inbound edges per node with `+0x04` as the target
//!    reproduces each node's own `in_plug_count` on **247 073 of 247 073**
//!    actions; using `+0x00` or `+0x08` instead matches only 53 294 (the nodes
//!    whose in-count is 0). This is the connectivity, and it is derived, not
//!    assumed: nothing in the file stores an inbound list.
//! 2. **Which of `+0x00` / `+0x08` is whose.** Scored against the per-node-type
//!    `select XxxInputs` / `XxxOutputs` vocabularies ([`NODE_PLUGS`]), counting
//!    only rows whose node type has a table:
//!
//!    | assignment | hit rate |
//!    |---|---|
//!    | `+0x00` in the **source** type's Outputs | 200 212 / 200 217 (100.00 %) |
//!    | `+0x00` in the **target** type's Inputs  | 6 / 194 289 (0.00 %) |
//!    | `+0x08` in the **source** type's Outputs | 112 / 200 217 (0.06 %) |
//!    | `+0x08` in the **target** type's Inputs  | 194 285 / 194 289 (100.00 %) |
//!
//!    So `+0x00` names the plug on the node that owns the row and `+0x08` names
//!    the plug on the node it fires into — an edge is
//!    `(owner, +0x00) -> (+0x04, +0x08)`.
//! 3. **Var plugs use the same row.** `+0x04` indexes `Zone Script Vars`
//!    instead, and `+0x00` is in the owning type's `select XxxVars` vocabulary
//!    on **176 706 of 176 706** scored rows. `+0x08` takes exactly **two**
//!    values over all 275 771 var-plug rows: `0x6A84A393 = crc32("In")`
//!    (39 023 rows) and `0x93BF4450 = crc32("Out")` (236 748 rows) — the
//!    direction of the binding. `+0x08` keeps its meaning here: it names the
//!    plug on the **target**, and for a var plug the target is the variable, so
//!    the variable's `Out` plug feeding the node means the **node reads** it and
//!    the variable's `In` plug means the **node writes** it. That is not a
//!    `label("kFloat out")` annotations ([`NODE_VAR_DIRS`], 448 of them), and
//!    reading the word from the node's point of view contradicts **every one of
//!    the 105 930 rows those labels cover**, while reading it from the
//!    variable's point of view agrees with **105 930 of 105 930**. The row
//!    counts say the same thing: nodes mostly consume parameters, so reads
//!    (236 748) should far outnumber writes (39 023).
//!
//! `FinalizeScriptAction` then rewrites each out plug's handle as
//! `handles[handle.GetData()]` where `handles` is the zone's script-action array
//! in row order — so the stored index is a row index, not a uid.
//!
//! # The var row — 32 bytes, measured
//!
//! ```text
//!   +0x00 i16 type               0 None, 1 Bool, 2 Float, 3 Vec, 4 String, 5 Actors
//!   +0x02 i16 global_name_index  -1 = unnamed, else -> Zone Script Strings
//!   +0x04 i16 a                  String: string index; Actors: actor row index
//!   +0x06 i16 b                  Actors: actor-group row index
//!   +0x08 u64 var_tuid
//!   +0x10     value union, 16 bytes
//!             Bool   u32  at +0x10
//!             Float  f32  at +0x10
//!             Vec    f32[3] at +0x10
//!             Actors u64  actor/group uid at +0x10
//! ```
//!
//! Row size: the section length is a multiple of 32 in **all 973** scripted
//! zones (946 of them have a non-empty var section), and for **944 of those
//! 946** 32 is the largest size that still leaves room for the largest index any
//! var plug references — the two exceptions have var sections small enough that
//! 36 and 48 also fit. Every var-plug index is in range at 32 bytes a row
//! (0 out of range over 275 771 bindings). `+0x00` is in `0..=5` on
//! **219 339 of 219 339** rows (5 distinct values) and `+0x02` is `-1` or a
//! valid string index on **219 339 of 219 339**; no other halfword slot in the
//!
//! # Names
//!
//! A node's **type name** is stored in the zone: `action_name_index` indexes
//! `Zone Script Strings` and `crc32` of that string equals `action_name_hash` on
//! 215 041 / 215 041 rows (measured in [`crate::core::zone`]) — so type names
//! resolve at 100 % with no external list. 475 distinct type names ship.
//!
//! **Plug names** are *not* in the zone; only their `crc32`. [`PLUG_NAMES`]
//! holds 1 320 names harvested from the `select XxxInputs` / `XxxOutputs` /
//! and C++ forms, then extended with identifiers from elsewhere in the source
//! drop whose `crc32` measurably hits a shipped plug hash. Measured coverage
//! over the whole build:
//!
//! | hash role | resolved |
//! |---|---|
//! | node type name (from the zone's own strings) | 247 073 / 247 073 (100 %) |
//! | out-plug name, source side  | 247 765 / 254 172 (97.48 %) |
//! | in-plug name, target side   | 253 358 / 254 172 (99.68 %) |
//! | var-plug name               | 264 841 / 275 771 (96.04 %) |
//!
//! 85 distinct out-plug and 89 distinct var-plug hashes are still unnamed; they
//! (`SunsetSpawnerFactoryScriptAction`, `ActorAttachAction`, `SimpleMoverAction`
//! …). [`NODE_PLUGS`] has per-type plug tables for 396 of the 475 shipped type
//! names, covering 182 438 / 247 073 nodes (73.84 %).
//!
//! # Node identity: `uid` vs `graph_uid`
//!
//! Measured over all 247 073 nodes:
//!
//! * every node's `uid` has bit 63 set (247 073 / 247 073) — the engine's
//! * `graph_uid` has bit 63 on only 127 058 (51.4 %);
//! * `uid == graph_uid` on 30 523 (12.35 %), `uid == graph_uid ^ (1 << 63)` on
//!   28 669 (11.60 %), and they are unrelated on the remaining 187 881
//!   (76.04 %). So `graph_uid` is a *separate* id, not a masked copy of the uid;
//! * inside one zone, 92 759 `graph_uid`s are used by exactly one node and
//!   20 584 by more than one — those are the repeated prefab placements. The
//!   heaviest is one source node placed 305 times in
//!   `i29_molonoth_gp.zone`.
//!
//! The same plug name may appear more than once on one node: 30 989 nodes repeat
//! an out-plug name (one output fanning out to several targets) and 7 124 repeat
//! a UI must be ready for both.
//!
//! # What is not decoded here
//!
//! `Zone Script Priuses` blobs are the nodes' typed parameters: the DDL struct
//! for a node of type `T` is `TPrius`. This module exposes each node's
//! `prius_offset` / `prius_size` and leaves the DDL decode to the prius work.

use crate::core::crc32;
use crate::core::zone::{Zone, NO_PLUG, T_SCRIPT_ACTIONS, T_SCRIPT_PLUGS, T_SCRIPT_PRIUSES};
use byteorder::{ByteOrder, LE};
use serde::Serialize;
use std::collections::HashMap;
use std::fmt::Write as _;

/// `u32[]` of string-pool offsets; `action_name_index` and the var rows index it.
pub const T_SCRIPT_STRINGS: u32 = 0xEF86_37D5;
/// `u32[]` of `crc32(string)`, parallel to [`T_SCRIPT_STRINGS`].
pub const T_SCRIPT_STRING_HASHES: u32 = 0x80D2_9828;
/// 32-byte `ScriptVarBuilt` rows.
pub const T_SCRIPT_VARS: u32 = 0xD86A_7934;
/// In the engine's section-tag list but absent from every shipped zone.
pub const T_SCRIPT_HASHES: u32 = 0xD266_157E;

pub const ACTION_ROW: usize = 40;
pub const PLUG_ROW: usize = 12;
pub const VAR_ROW: usize = 32;

/// `crc32("In")` — the variable's own `In` plug, so the **node writes** it.
pub const VAR_PLUG_IN: u32 = 0x6A84_A393;
/// `crc32("Out")` — the variable's own `Out` plug, so the **node reads** it.
pub const VAR_PLUG_OUT: u32 = 0x93BF_4450;

/// One 12-byte `Zone Script Plugs` row, as stored.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlugRow {
    pub plug_name_hash: u32,
    /// Out plug: `Zone Script Actions` row index. Var plug: `Zone Script Vars`
    /// row index.
    pub target_index: u32,
    /// Out plug: the target node's input plug. Var plug:
    /// [`VAR_PLUG_IN`]/[`VAR_PLUG_OUT`].
    pub target_plug_name_hash: u32,
}

impl PlugRow {
    pub fn read(b: &[u8]) -> Self {
        PlugRow {
            plug_name_hash: LE::read_u32(&b[0..4]),
            target_index: LE::read_u32(&b[4..8]),
            target_plug_name_hash: LE::read_u32(&b[8..12]),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum VarType {
    None,
    Bool,
    Float,
    Vec,
    String,
    Actors,
    /// A value outside `0..=5`; none ship.
    Unknown(i16),
}

impl VarType {
    pub fn from_raw(v: i16) -> Self {
        match v {
            0 => VarType::None,
            1 => VarType::Bool,
            2 => VarType::Float,
            3 => VarType::Vec,
            4 => VarType::String,
            5 => VarType::Actors,
            other => VarType::Unknown(other),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            VarType::None => "none",
            VarType::Bool => "bool",
            VarType::Float => "number",
            VarType::Vec => "vector",
            VarType::String => "string",
            VarType::Actors => "actors",
            VarType::Unknown(_) => "?",
        }
    }
}

/// The value a variable is authored with.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum VarValue {
    None,
    Bool {
        value: bool,
    },
    Float {
        value: f32,
    },
    Vec {
        value: [f32; 3],
    },
    String {
        value: Option<String>,
    },
    Actors {
        /// `Zone Actors` row index, or `None` when the row stores -1.
        actor_index: Option<u16>,
        /// `Zone Actor Groups` row index, or `None`.
        group_index: Option<u16>,
        /// Fallback uid the engine resolves by search when both indices are -1.
        #[serde(serialize_with = "ser_u64_str")]
        uid: u64,
        /// The actor or group name those indices resolve to in this zone.
        target_name: Option<String>,
    },
    Unknown {
        raw: String,
    },
}

fn ser_u64_str<S: serde::Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&v.to_string())
}

/// One 32-byte `Zone Script Vars` row, decoded.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptVar {
    pub index: u32,
    pub ty: VarType,
    /// Only global (named) vars carry a name; unnamed ones are graph-local.
    pub global_name: Option<String>,
    #[serde(serialize_with = "ser_u64_str")]
    pub tuid: u64,
    pub value: VarValue,
    /// Var plugs that read this var, as `(node index, plug name hash)`.
    pub readers: Vec<(u32, u32)>,
    /// Var plugs that write it.
    pub writers: Vec<(u32, u32)>,
}

/// A var plug: a node binding one of its named parameters to a variable.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VarBinding {
    pub plug_name_hash: u32,
    pub plug_name: Option<String>,
    pub var_index: u32,
    /// `"in"`, `"out"`, or `"?"` when the word is neither.
    pub direction: &'static str,
    pub authored_direction: Option<&'static str>,
}

/// One edge: `(from_node, from_plug) -> (to_node, to_plug)`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptEdge {
    pub from_node: u32,
    pub from_plug_hash: u32,
    pub from_plug: Option<String>,
    pub to_node: u32,
    pub to_plug_hash: u32,
    pub to_plug: Option<String>,
    /// True when `to_node` is out of range for this zone's action table.
    pub dangling: bool,
}

/// One node: a row of `Zone Script Actions` with its names and plugs resolved.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptNode {
    pub index: u32,
    #[serde(serialize_with = "ser_u64_str")]
    pub uid: u64,
    /// The id of the node inside the source `.scriptgraph`, shared by every
    /// placement of the same prefab.
    #[serde(serialize_with = "ser_u64_str")]
    pub graph_uid: u64,
    /// The node's type, e.g. `GateAction`. Its parameter struct is
    /// `<type_name>Prius`.
    pub type_name: Option<String>,
    pub type_name_hash: u32,
    /// Stored inbound-edge count; [`ScriptGraph::parse`] checks it against the
    /// edges it reconstructs.
    pub in_plug_count: u16,
    /// Indices into [`ScriptGraph::edges`] of this node's outgoing edges.
    pub out_edges: Vec<u32>,
    /// Indices into [`ScriptGraph::edges`] of the edges that fire into it.
    pub in_edges: Vec<u32>,
    pub vars: Vec<VarBinding>,
    /// `Zone Script Priuses` span holding the node's typed parameters.
    pub prius_offset: u32,
    pub prius_size: u32,
    /// Which connected component of the zone's script the node belongs to.
    pub subgraph: u32,
}

impl ScriptNode {
    /// The DDL struct name for this node's parameters.
    pub fn prius_type(&self) -> Option<String> {
        self.type_name.as_ref().map(|t| format!("{t}Prius"))
    }
    /// The plugs the node's type declares, when the type is in [`NODE_PLUGS`]:
    /// `(inputs, outputs, vars)`. Lets an editor show unconnected plugs.
    pub fn declared_plugs(
        &self,
    ) -> Option<(
        &'static [&'static str],
        &'static [&'static str],
        &'static [&'static str],
    )> {
        let t = self.type_name.as_deref()?;
        NODE_PLUGS
            .binary_search_by(|p| p.0.cmp(t))
            .ok()
            .map(|i| (NODE_PLUGS[i].1, NODE_PLUGS[i].2, NODE_PLUGS[i].3))
    }
}

/// Counts taken while parsing, so a caller can report coverage without
/// re-walking the graph.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptStats {
    pub nodes: usize,
    pub edges: usize,
    pub dangling_edges: usize,
    pub var_rows: usize,
    pub var_bindings: usize,
    pub subgraphs: usize,
    /// Nodes whose stored `in_plug_count` equals the inbound edges rebuilt.
    pub in_count_match: usize,
    pub in_count_mismatch: usize,
    pub type_names_resolved: usize,
    pub out_plug_names_resolved: usize,
    pub in_plug_names_resolved: usize,
    pub var_plug_names_resolved: usize,
    /// Var bindings whose file direction matches the authored label.
    pub direction_agree: usize,
    pub direction_disagree: usize,
}

/// A zone's whole script program.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptGraph {
    pub nodes: Vec<ScriptNode>,
    pub edges: Vec<ScriptEdge>,
    pub vars: Vec<ScriptVar>,
    pub stats: ScriptStats,
}

/// Resolve a plug-name hash through [`PLUG_NAMES`].
pub fn plug_name(hash: u32) -> Option<&'static str> {
    PLUG_NAMES
        .binary_search_by_key(&hash, |&(h, _)| h)
        .ok()
        .map(|i| PLUG_NAMES[i].1)
}

pub fn authored_var_direction(ty: &str, var: &str) -> Option<&'static str> {
    NODE_VAR_DIRS
        .binary_search_by(|r| (r.0, r.1).cmp(&(ty, var)))
        .ok()
        .map(|i| NODE_VAR_DIRS[i].2)
}

impl ScriptGraph {
    /// Parse a zone's script sections. `None` when the zone has no nodes.
    pub fn parse(zone: &Zone) -> Option<ScriptGraph> {
        let actions = zone.dat1.get_section_data(T_SCRIPT_ACTIONS)?;
        if actions.len() < ACTION_ROW {
            return None;
        }
        let n_act = actions.len() / ACTION_ROW;
        let plugs = zone.dat1.get_section_data(T_SCRIPT_PLUGS).unwrap_or(&[]);
        let n_plug = plugs.len() / PLUG_ROW;
        let var_rows = zone.dat1.get_section_data(T_SCRIPT_VARS).unwrap_or(&[]);
        let n_var = var_rows.len() / VAR_ROW;
        let strings = string_table(zone);

        let mut stats = ScriptStats {
            nodes: n_act,
            var_rows: n_var,
            ..Default::default()
        };

        // ---- variables -------------------------------------------------------
        let mut vars: Vec<ScriptVar> = Vec::with_capacity(n_var);
        for i in 0..n_var {
            let r = &var_rows[i * VAR_ROW..(i + 1) * VAR_ROW];
            let ty = VarType::from_raw(LE::read_i16(&r[0..2]));
            let gname = LE::read_i16(&r[2..4]);
            let a = LE::read_i16(&r[4..6]);
            let b = LE::read_i16(&r[6..8]);
            let value = match ty {
                VarType::None => VarValue::None,
                VarType::Bool => VarValue::Bool {
                    value: LE::read_u32(&r[16..20]) != 0,
                },
                VarType::Float => VarValue::Float {
                    value: LE::read_f32(&r[16..20]),
                },
                VarType::Vec => VarValue::Vec {
                    value: [
                        LE::read_f32(&r[16..20]),
                        LE::read_f32(&r[20..24]),
                        LE::read_f32(&r[24..28]),
                    ],
                },
                VarType::String => VarValue::String {
                    value: if a >= 0 {
                        strings.get(a as usize).cloned()
                    } else {
                        None
                    },
                },
                VarType::Actors => {
                    let ai = if a >= 0 { Some(a as u16) } else { None };
                    let gi = if b >= 0 { Some(b as u16) } else { None };
                    let nonempty = |s: &String| (!s.is_empty()).then(|| s.clone());
                    let target_name = ai
                        .and_then(|x| zone.actors.get(x as usize).and_then(|v| nonempty(&v.name)))
                        .or_else(|| {
                            gi.and_then(|x| {
                                zone.actor_groups
                                    .get(x as usize)
                                    .and_then(|v| nonempty(&v.name))
                            })
                        });
                    VarValue::Actors {
                        actor_index: ai,
                        group_index: gi,
                        uid: LE::read_u64(&r[16..24]),
                        target_name,
                    }
                }
                VarType::Unknown(_) => VarValue::Unknown {
                    raw: hex(&r[16..32]),
                },
            };
            vars.push(ScriptVar {
                index: i as u32,
                ty,
                global_name: if gname >= 0 {
                    strings.get(gname as usize).cloned()
                } else {
                    None
                },
                tuid: LE::read_u64(&r[8..16]),
                value,
                readers: Vec::new(),
                writers: Vec::new(),
            });
        }

        // ---- nodes -----------------------------------------------------------
        let mut nodes: Vec<ScriptNode> = Vec::with_capacity(n_act);
        for i in 0..n_act {
            let r = &actions[i * ACTION_ROW..(i + 1) * ACTION_ROW];
            let name_i = LE::read_u16(&r[26..28]);
            let name_h = LE::read_u32(&r[28..32]);
            let type_name = strings
                .get(name_i as usize)
                .filter(|s| crc32::hash(s) == name_h)
                .cloned();
            if type_name.is_some() {
                stats.type_names_resolved += 1;
            }
            nodes.push(ScriptNode {
                index: i as u32,
                uid: LE::read_u64(&r[0..8]),
                graph_uid: LE::read_u64(&r[8..16]),
                type_name,
                type_name_hash: name_h,
                in_plug_count: LE::read_u16(&r[16..18]),
                out_edges: Vec::new(),
                in_edges: Vec::new(),
                vars: Vec::new(),
                prius_offset: LE::read_u32(&r[32..36]),
                prius_size: LE::read_u32(&r[36..40]),
                subgraph: u32::MAX,
            });
        }

        // ---- edges and var bindings ------------------------------------------
        let mut edges: Vec<ScriptEdge> = Vec::new();
        for i in 0..n_act {
            let r = &actions[i * ACTION_ROW..(i + 1) * ACTION_ROW];
            let out_i = LE::read_u16(&r[18..20]);
            let out_c = LE::read_u16(&r[20..22]);
            let var_i = LE::read_u16(&r[22..24]);
            let var_c = LE::read_u16(&r[24..26]);

            if out_i != NO_PLUG {
                for p in 0..out_c as usize {
                    let row = out_i as usize + p;
                    if row >= n_plug {
                        continue;
                    }
                    let pr = PlugRow::read(&plugs[row * PLUG_ROW..(row + 1) * PLUG_ROW]);
                    let dangling = pr.target_index as usize >= n_act;
                    let from_plug = plug_name(pr.plug_name_hash);
                    let to_plug = plug_name(pr.target_plug_name_hash);
                    if from_plug.is_some() {
                        stats.out_plug_names_resolved += 1;
                    }
                    if to_plug.is_some() {
                        stats.in_plug_names_resolved += 1;
                    }
                    let e = edges.len() as u32;
                    edges.push(ScriptEdge {
                        from_node: i as u32,
                        from_plug_hash: pr.plug_name_hash,
                        from_plug: from_plug.map(str::to_string),
                        to_node: pr.target_index,
                        to_plug_hash: pr.target_plug_name_hash,
                        to_plug: to_plug.map(str::to_string),
                        dangling,
                    });
                    nodes[i].out_edges.push(e);
                    if dangling {
                        stats.dangling_edges += 1;
                    } else {
                        nodes[pr.target_index as usize].in_edges.push(e);
                    }
                }
            }

            if var_i != NO_PLUG {
                for p in 0..var_c as usize {
                    let row = var_i as usize + p;
                    if row >= n_plug {
                        continue;
                    }
                    let pr = PlugRow::read(&plugs[row * PLUG_ROW..(row + 1) * PLUG_ROW]);
                    let name = plug_name(pr.plug_name_hash);
                    if name.is_some() {
                        stats.var_plug_names_resolved += 1;
                    }
                    // `+0x08` names the plug on the *target*, and the target
                    // here is the variable: its `Out` plug feeds the node, its
                    // `In` plug is fed by the node. So the node's direction is
                    // the opposite of the word. Measured: reading it the other
                    // way round contradicts every one of the 105 930 rows the
                    let direction = match pr.target_plug_name_hash {
                        VAR_PLUG_OUT => "in",
                        VAR_PLUG_IN => "out",
                        _ => "?",
                    };
                    let authored = match (nodes[i].type_name.as_deref(), name) {
                        (Some(t), Some(n)) => authored_var_direction(t, n),
                        _ => None,
                    };
                    match authored {
                        Some("In") if direction == "in" => stats.direction_agree += 1,
                        Some("Out") if direction == "out" => stats.direction_agree += 1,
                        Some(_) => stats.direction_disagree += 1,
                        None => {}
                    }
                    if (pr.target_index as usize) < n_var {
                        let v = &mut vars[pr.target_index as usize];
                        let slot = if direction == "in" {
                            &mut v.readers
                        } else {
                            &mut v.writers
                        };
                        slot.push((i as u32, pr.plug_name_hash));
                    }
                    nodes[i].vars.push(VarBinding {
                        plug_name_hash: pr.plug_name_hash,
                        plug_name: name.map(str::to_string),
                        var_index: pr.target_index,
                        direction,
                        authored_direction: authored,
                    });
                    stats.var_bindings += 1;
                }
            }
        }

        for n in &nodes {
            if n.in_plug_count as usize == n.in_edges.len() {
                stats.in_count_match += 1;
            } else {
                stats.in_count_mismatch += 1;
            }
        }
        stats.edges = edges.len();
        stats.subgraphs = label_subgraphs(&mut nodes, &edges);
        Some(ScriptGraph {
            nodes,
            edges,
            vars,
            stats,
        })
    }

    /// Node row indices of one connected component, in row order.
    pub fn subgraph_nodes(&self, id: u32) -> Vec<u32> {
        self.nodes
            .iter()
            .filter(|n| n.subgraph == id)
            .map(|n| n.index)
            .collect()
    }

    /// Nodes with no inbound signal edge — where a subgraph starts.
    pub fn roots(&self) -> Vec<u32> {
        self.nodes
            .iter()
            .filter(|n| n.in_edges.is_empty())
            .map(|n| n.index)
            .collect()
    }

    fn label(&self, i: u32) -> String {
        match self.nodes.get(i as usize) {
            Some(n) => match &n.type_name {
                Some(t) => format!("#{i} {t}"),
                None => format!("#{i} <type 0x{:08X}>", n.type_name_hash),
            },
            None => format!("#{i} <out of range>"),
        }
    }

    /// A human-readable dump. `only` restricts it to one subgraph.
    pub fn dump_text(&self, title: &str, only: Option<u32>) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "{title}");
        let _ = writeln!(
            s,
            "{} nodes, {} edges ({} dangling), {} vars, {} var bindings, {} subgraphs",
            self.stats.nodes,
            self.stats.edges,
            self.stats.dangling_edges,
            self.stats.var_rows,
            self.stats.var_bindings,
            self.stats.subgraphs
        );
        let _ = writeln!(
            s,
            "in_plug_count reconstructed on {}/{} nodes",
            self.stats.in_count_match, self.stats.nodes
        );
        for sg in 0..self.stats.subgraphs as u32 {
            if only.is_some() && only != Some(sg) {
                continue;
            }
            let members = self.subgraph_nodes(sg);
            let _ = writeln!(s, "\n=== subgraph {sg} : {} nodes ===", members.len());
            for i in members {
                let n = &self.nodes[i as usize];
                let prius = if n.prius_size > 0 {
                    format!(
                        "  prius {}[{}]",
                        n.prius_type().unwrap_or_default(),
                        n.prius_size
                    )
                } else {
                    String::new()
                };
                let _ = writeln!(
                    s,
                    "  {}  uid {:016X}  graph {:016X}{prius}",
                    self.label(i),
                    n.uid,
                    n.graph_uid
                );
                for &e in &n.out_edges {
                    let ed = &self.edges[e as usize];
                    let to = if ed.dangling {
                        format!("<bad node {}>", ed.to_node)
                    } else {
                        self.label(ed.to_node)
                    };
                    let _ = writeln!(
                        s,
                        "      {} -> {to} . {}",
                        ed.from_plug
                            .clone()
                            .unwrap_or_else(|| format!("0x{:08X}", ed.from_plug_hash)),
                        ed.to_plug
                            .clone()
                            .unwrap_or_else(|| format!("0x{:08X}", ed.to_plug_hash)),
                    );
                }
                for v in &n.vars {
                    let desc = self
                        .vars
                        .get(v.var_index as usize)
                        .map(describe_var)
                        .unwrap_or_else(|| "<out of range>".into());
                    let _ = writeln!(
                        s,
                        "      {} {} var[{}] {desc}",
                        if v.direction == "in" { "reads " } else { "writes" },
                        v.plug_name
                            .clone()
                            .unwrap_or_else(|| format!("0x{:08X}", v.plug_name_hash)),
                        v.var_index,
                    );
                }
                if n.in_edges.is_empty() && n.out_edges.is_empty() && n.vars.is_empty() {
                    let _ = writeln!(s, "      (isolated)");
                }
            }
        }
        if only.is_none() && !self.vars.is_empty() {
            let _ = writeln!(s, "\n=== variables ===");
            for v in &self.vars {
                let _ = writeln!(
                    s,
                    "  var[{}] {}  tuid {:016X}  {} reader(s) {} writer(s)",
                    v.index,
                    describe_var(v),
                    v.tuid,
                    v.readers.len(),
                    v.writers.len()
                );
            }
        }
        s
    }
}

/// One line describing a variable's type, name and authored value.
pub fn describe_var(v: &ScriptVar) -> String {
    let name = v.global_name.as_deref().unwrap_or("<local>");
    let val = match &v.value {
        VarValue::None => "-".to_string(),
        VarValue::Bool { value } => value.to_string(),
        VarValue::Float { value } => format!("{value}"),
        VarValue::Vec { value } => format!("({}, {}, {})", value[0], value[1], value[2]),
        VarValue::String { value } => format!("{:?}", value.as_deref().unwrap_or("")),
        VarValue::Actors {
            actor_index,
            group_index,
            uid,
            target_name,
        } => {
            let who = target_name
                .clone()
                .unwrap_or_else(|| format!("uid {uid:016X}"));
            match (actor_index, group_index) {
                (Some(a), _) => format!("actor[{a}] {who}"),
                (_, Some(g)) => format!("group[{g}] {who}"),
                _ => format!("by uid {uid:016X}"),
            }
        }
        VarValue::Unknown { raw } => raw.clone(),
    };
    format!("{}:{name} = {val}", v.ty.name())
}

/// The zone's `Zone Script Strings` table, resolved through the DAT1 pool.
pub fn string_table(zone: &Zone) -> Vec<String> {
    zone.dat1
        .get_section_data(T_SCRIPT_STRINGS)
        .map(|s| {
            s.chunks_exact(4)
                .map(|b| zone.dat1.get_string(LE::read_u32(b)).unwrap_or_default())
                .collect()
        })
        .unwrap_or_default()
}

/// The `Zone Script Priuses` blob of one node.
pub fn prius_blob<'a>(zone: &'a Zone, node: &ScriptNode) -> Option<&'a [u8]> {
    let sec = zone.dat1.get_section_data(T_SCRIPT_PRIUSES)?;
    let a = node.prius_offset as usize;
    let b = a.checked_add(node.prius_size as usize)?;
    sec.get(a..b)
}

/// Flood-fill connected components over signal edges *and* shared variables:
/// two nodes that only meet at a variable are still one piece of logic.
fn label_subgraphs(nodes: &mut [ScriptNode], edges: &[ScriptEdge]) -> usize {
    let n = nodes.len();
    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n];
    for e in edges {
        if e.dangling {
            continue;
        }
        adj[e.from_node as usize].push(e.to_node);
        adj[e.to_node as usize].push(e.from_node);
    }
    let mut by_var: HashMap<u32, Vec<u32>> = HashMap::new();
    for nd in nodes.iter() {
        for v in &nd.vars {
            by_var.entry(v.var_index).or_default().push(nd.index);
        }
    }
    for members in by_var.values() {
        for w in members.windows(2) {
            adj[w[0] as usize].push(w[1]);
            adj[w[1] as usize].push(w[0]);
        }
    }
    let mut next = 0u32;
    let mut stack: Vec<u32> = Vec::new();
    for start in 0..n {
        if nodes[start].subgraph != u32::MAX {
            continue;
        }
        let id = next;
        next += 1;
        stack.push(start as u32);
        nodes[start].subgraph = id;
        while let Some(cur) = stack.pop() {
            let nbs = std::mem::take(&mut adj[cur as usize]);
            for &nb in &nbs {
                if nodes[nb as usize].subgraph == u32::MAX {
                    nodes[nb as usize].subgraph = id;
                    stack.push(nb);
                }
            }
            adj[cur as usize] = nbs;
        }
    }
    next as usize
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        let _ = write!(s, "{x:02X}");
    }
    s
}

/// `crc32(name)` -> plug name, sorted by hash. 1320 names, no collisions.
///
/// Harvested from the `select XxxInputs` / `XxxOutputs` / `XxxVars` blocks of
/// extended with the identifiers elsewhere in the source drop whose `crc32`
/// measurably hits a plug hash in the shipped zones. Nothing was guessed: an
/// entry is here only because some shipped plug row carries its hash.
pub static PLUG_NAMES: &[(u32, &str)] = &[
    (0x001E9C3B, "Stretch"),
    (0x00BB8867, "OnTwoResults"),
    (0x00C6BDA5, "LookTarget"),
    (0x0113AFF0, "OnMultiply"),
    (0x0142BA1C, "Fidelity"),
    (0x0149FD24, "Submit"),
    (0x01C1A75C, "CheckState"),
    (0x01E9748A, "Off"),
    (0x02171161, "StopAny"),
    (0x0231D314, "EnableGun"),
    (0x0269B79E, "Value6"),
    (0x02872CB8, "OnTriggered"),
    (0x029E19B1, "Show"),
    (0x029FC1B3, "IsImmediateReplay"),
    (0x02A08866, "InvertUp"),
    (0x02D96723, "OnWarp"),
    (0x02FD8897, "LockHeroStand"),
    (0x030ED131, "OnClearInput"),
    (0x0346A464, "OnContinue"),
    (0x034F212C, "TargetShip"),
    (0x03DFA4BA, "GunDisabled"),
    (0x03FFA494, "OnPause"),
    (0x042494F5, "Value"),
    (0x046A32E0, "Delete"),
    (0x0471A526, "Result_Vec"),
    (0x047F4D94, "OnUnFocus"),
    (0x04AF681D, "Melee_Throw"),
    (0x04E03C26, "BitIndex"),
    (0x04E99FA3, "MaxValue"),
    (0x0504203D, "Radius"),
    (0x05047387, "Value2"),
    (0x054D32F2, "SetOverride"),
    (0x0564A062, "Camera"),
    (0x059D203E, "TValue"),
    (0x05D437EB, "Silent"),
    (0x064A9CEC, "OnSetOverrideAutoCollect"),
    (0x0658F354, "PressurePlate"),
    (0x068F6303, "UnblockTokenUsers"),
    (0x069BA790, "Result1"),
    (0x06A221EC, "SomeOverlap"),
    (0x06C4FC8F, "Lights"),
    (0x06D8C59B, "AddToEnd"),
    (0x06ED806C, "CurrentTime"),
    (0x06EEFCC2, "LocalRotation"),
    (0x07363F75, "Solve"),
    (0x07DCD6B2, "KilledNonEnemy"),
    (0x07ED0AC3, "NotNewLevel"),
    (0x081E7FCB, "FlyAwayAndDestroy"),
    (0x083D6AFF, "Kickables"),
    (0x08AF8B11, "OnEnable"),
    (0x08B2B96D, "OnButtonReleased"),
    (0x08E29AA1, "NewLevel"),
    (0x091D2DE4, "OnInvert"),
    (0x0924F421, "In_4"),
    (0x096E94A2, "OnGetNone"),
    (0x0982CF4A, "Is"),
    (0x09A59DC4, "MaxAlive"),
    (0x09B5D5A8, "ForceInstantCollect"),
    (0x09B6FFD3, "IsClank"),
    (0x09F11C6B, "Rotate"),
    (0x0A524D05, "Close"),
    (0x0AA987C6, "Jump_HeliLong"),
    (0x0AF92850, "ResultForward"),
    (0x0B560218, "CaptureInHDR"),
    (0x0BF8DE6E, "OnShotSpawned"),
    (0x0C0AABAE, "BossActor"),
    (0x0C5BAE2B, "StartTime"),
    (0x0CD51F2B, "OnMaxRuns"),
    (0x0D3DA173, "ActorOut"),
    (0x0D485E50, "OnIdle"),
    (0x0D4CD90B, "Holster"),
    (0x0D586410, "Eulers"),
    (0x0D5EC66B, "RewardValue"),
    (0x0DB1C8BB, "InvertCamX"),
    (0x0DE1F598, "DestinationPos"),
    (0x0E493038, "In_0"),
    (0x0E6EA68A, "Boss"),
    (0x0E88C5EE, "OnPortalRespawnEnabled"),
    (0x0EB42269, "kNone"),
    (0x0EBF9FF6, "DrawTurnedOn"),
    (0x0EDB93B3, "Triggered"),
    (0x0EEFD989, "Mat1Left"),
    (0x0F0BC75A, "Performance"),
    (0x0F7BF349, "Trampoline"),
    (0x0FBAFBAB, "InputFlags"),
    (0x0FBE2133, "OverlapEnd"),
    (0x0FDBC36C, "Obstructed"),
    (0x10AC898C, "kAnimDamageAttackAmount"),
    (0x10C6AD4B, "DisabledOnDeathDone"),
    (0x10D9CF90, "BootsOff"),
    (0x11400253, "Facing_Actor"),
    (0x11AE90AC, "OnUnlocked"),
    (0x11C21C78, "LookOn"),
    (0x11DD8BB2, "OnLocked"),
    (0x11E00FC3, "SpawnedTargetAreas"),
    (0x12565C18, "Mat1Forward"),
    (0x1289BB8C, "OnMoved"),
    (0x12BBB2FE, "StartTimer"),
    (0x12D421DF, "CountdownValue"),
    (0x134E9EF7, "ForceBoltCollect"),
    (0x136B3EAB, "InterplanetaryCheckpointName"),
    (0x13C19BBC, "IsSet"),
    (0x13E38AFB, "Slerp"),
    (0x14B6EA69, "UnlockCamera"),
    (0x14CD224A, "LoopDelay"),
    (0x1518FE92, "ValueIn"),
    (0x152F9C1B, "Break"),
    (0x159624AD, "Load"),
    (0x160DE6A4, "OutputFlags"),
    (0x166E515C, "Shown"),
    (0x166FF4F5, "Initialized"),
    (0x16753237, "Speed"),
    (0x169A9B0C, "OnShowInput"),
    (0x16A0CDDB, "OnSetSpeed"),
    (0x16AF89BE, "Primary"),
    (0x16B83961, "MaxSpeed"),
    (0x16B8B291, "DisableMissiles"),
    (0x16EF6F9B, "Arccosine"),
    (0x1716CEC4, "kCustomLightSpecular"),
    (0x173FA849, "OnClearNode"),
    (0x183CCBA8, "LastSpawn"),
    (0x18747671, "Minimum"),
    (0x1887AF02, "StartHeart"),
    (0x18FC4FE3, "WeaponType"),
    (0x19643B59, "StopListen"),
    (0x199A20F7, "Num1"),
    (0x1A5A7710, "ActorGroup"),
    (0x1A5E216E, "InvertForward"),
    (0x1B37A215, "Value_7"),
    (0x1B3D9CC7, "Aim"),
    (0x1B631742, "ClearNode"),
    (0x1B816C65, "Trigger"),
    (0x1B87963E, "Maximum"),
    (0x1B916BF3, "RightTriggerValue"),
    (0x1B92E3EC, "Toggle"),
    (0x1B9F7C13, "DrawOff"),
    (0x1C5A660C, "Value_3"),
    (0x1C7B7B42, "MagbootsOn"),
    (0x1C7F81A0, "PurchasedWeapon"),
    (0x1C826FF4, "ProjectileActor"),
    (0x1CE22F1F, "SwapToKit"),
    (0x1D081EF1, "OnSearching"),
    (0x1D085632, "OnOverrideApplied"),
    (0x1D1B1330, "RemoveOverride"),
    (0x1D878F71, "AngleDegreesMax"),
    (0x1E001001, "PauseDropoff"),
    (0x1E042656, "OnWorldPassive"),
    (0x1E2B81F8, "Sync13"),
    (0x1E4198A0, "OnCheckFalse"),
    (0x1E50DE60, "GetCheckpoint"),
    (0x1E81C554, "HeroIndex"),
    (0x1E97BEED, "FollowDist"),
    (0x1F9E4E0E, "OnSuccess"),
    (0x1FA39425, "UpdateChkpt"),
    (0x1FB0B54D, "SegmentStart"),
    (0x1FDAA692, "OnAdded"),
    (0x1FFD2683, "Normal"),
    (0x2048921D, "EventName"),
    (0x20672A38, "OnEnabled"),
    (0x207160F9, "Failure"),
    (0x20BD201B, "Done"),
    (0x214DD94A, "ShowWaveComplete"),
    (0x2151A5D1, "NoSpawnsRemaining"),
    (0x217C09FD, "UpVec"),
    (0x218AB028, "AngleDegreesMin"),
    (0x21DDE5CF, "OnStartListening"),
    (0x2213726F, "kPixelPosition"),
    (0x2238B7D1, "OnReplay"),
    (0x22B28F3B, "ForceGlovePartialOff"),
    (0x22B92099, "Credit"),
    (0x22FDC666, "Event"),
    (0x23307A36, "IsFalse"),
    (0x234B792F, "NotVisitedBefore"),
    (0x235F0741, "ActorsToQuery"),
    (0x238EE6B3, "LoopCount"),
    (0x23B063E1, "RemainingTime"),
    (0x23B659D5, "OnSkipMovie"),
    (0x23BA36C2, "Mount"),
    (0x23F8E258, "OnTokenBlocked"),
    (0x2432590A, "InRange"),
    (0x24432213, "OnScreen"),
    (0x24A81893, "Sync9"),
    (0x24B53B4E, "AudioTarget"),
    (0x24C08FFC, "GetNumAlive"),
    (0x24C2EB7E, "AttachActor"),
    (0x25008D6A, "Deactivate"),
    (0x2516063C, "Vector"),
    (0x2564B650, "OnGameplayRunning"),
    (0x257A8958, "OnChoseBronze"),
    (0x25C63FA2, "OnAttach"),
    (0x25DD60D2, "Check"),
    (0x25DDEAF2, "OnGet"),
    (0x25F068D0, "Tangent"),
    (0x25FCB765, "OnAdd"),
    (0x260E600A, "OnStartedUsingClue"),
    (0x26605C2A, "Closest"),
    (0x26929DEA, "GalacMapActor"),
    (0x26F8CA1C, "GetElement"),
    (0x271494AE, "Applied"),
    (0x27723B89, "Available"),
    (0x27B8DFD8, "Dropper"),
    (0x288B1023, "OnDetach"),
    (0x289C0E06, "LaserBeamSpawnerActor"),
    (0x28B362BA, "RemovedVector"),
    (0x2934711B, "Y"),
    (0x294E8930, "OnStartPressed"),
    (0x2958F806, "ResetToDefaults"),
    (0x2984EF02, "ActorA"),
    (0x29DC9E4A, "Tag"),
    (0x29F67982, "Damager"),
    (0x29FCDF9E, "VectorE"),
    (0x2A15E13B, "RuinToken"),
    (0x2A233374, "Y_Rotation"),
    (0x2A23D8AD, "DisableImmune"),
    (0x2A7390A1, "Sync1"),
    (0x2AA91A78, "End"),
    (0x2AAE4494, "PuzzleVolumeEnd"),
    (0x2AECFF1F, "TurnToTarget"),
    (0x2B4A2DD8, "Bot"),
    (0x2B6169A1, "BarrageStarted"),
    (0x2B6BF465, "Mover"),
    (0x2BCEDA4C, "Unchecked"),
    (0x2BEB0E11, "HPActual"),
    (0x2BF46D76, "OnReachedEnd"),
    (0x2C411BC6, "PauseCinematic"),
    (0x2C5ED61F, "Reference"),
    (0x2C90A7C8, "OnTrue"),
    (0x2D128F74, "Result"),
    (0x2D1E54B8, "Sync5"),
    (0x2D9ABAC3, "TargetActors"),
    (0x2DA22971, "String1"),
    (0x2E387194, "ResetIdleTime"),
    (0x2E911B87, "VectorA"),
    (0x2E99B105, "PastEnd"),
    (0x2EE92B1B, "ActorE"),
    (0x2F0B3E3F, "OnExitFinale"),
    (0x2FB01A0B, "Damage"),
    (0x2FD980B5, "StringOut"),
    (0x3006A975, "TestName"),
    (0x31133FE5, "OnFull"),
    (0x3153DA7F, "CollisionLocation"),
    (0x3183C992, "AngleOffset"),
    (0x318B1410, "OnExitLoop"),
    (0x31BB1A8D, "MissilesDisabled"),
    (0x31D4C77F, "OnClear"),
    (0x31F50D46, "Factories"),
    (0x31FBF15F, "PingState"),
    (0x320B57AF, "GT"),
    (0x32143A49, "Shuffle"),
    (0x329EA4F1, "False"),
    (0x32A05EF8, "ActorsWithAllegiance"),
    (0x32C77577, "LookOff"),
    (0x3310F55A, "OnGrindbootsOn"),
    (0x33628CDF, "OnUnload"),
    (0x3379725D, "Speaker4"),
    (0x3380A203, "LevelValue"),
    (0x33BFC81E, "Facing_0"),
    (0x33D8B50B, "KilledEnemy"),
    (0x33F2E3D5, "Instigator"),
    (0x3416A975, "TimeSinceInput"),
    (0x34585377, "OnForceOffline"),
    (0x3460D7E3, "OnDestroyShot"),
    (0x34BFD08C, "Next"),
    (0x35974CCA, "DisableAudioFade"),
    (0x35B37A93, "AmbientShooterGroup"),
    (0x35D6DD97, "Portal"),
    (0x361FAEBF, "SpawnShot"),
    (0x36A67EA0, "ManualReset"),
    (0x36C12525, "Looping"),
    (0x376EC118, "Out_OutOfRange"),
    (0x37B65A78, "FlyerInvertX"),
    (0x37B996D9, "Kill"),
    (0x37F8D936, "Equal"),
    (0x382FE313, "SpawnIntoGroup"),
    (0x3846D9C0, "ShotTagC"),
    (0x384991C0, "GetCurrentTime"),
    (0x38548038, "PlanetName"),
    (0x3862ECA3, "IsTriggered"),
    (0x38BE78D2, "Position_1"),
    (0x38D8A6D3, "DrawTurnedOff"),
    (0x3951B797, "BarrageStopped"),
    (0x39746129, "SelectIndex"),
    (0x3976C976, "OneFalse"),
    (0x398D1332, "ForceOffline"),
    (0x39939F60, "Out_6"),
    (0x39B514B4, "Translation"),
    (0x39D39096, "SetImmune"),
    (0x39EC24E5, "kVertexLoD"),
    (0x39F54137, "AnyInvertCam"),
    (0x3A01AC7A, "Gather"),
    (0x3A02AE3C, "InvertPosition"),
    (0x3A4311B4, "OnSpawnShot"),
    (0x3A58E94D, "A"),
    (0x3B446278, "OnDamageReached"),
    (0x3B6B6E0D, "Objective"),
    (0x3BB4414E, "Victim"),
    (0x3BCF183C, "AnyNormalCam"),
    (0x3BE098E8, "Child"),
    (0x3BF5D6BF, "OnThrown"),
    (0x3C1F225F, "EarlyGame"),
    (0x3C371CAC, "OnNumAliveSet"),
    (0x3C49A261, "OnDead"),
    (0x3CB257FE, "OnLineCompletedIndex"),
    (0x3CDE0415, "On"),
    (0x3D086345, "Float1"),
    (0x3D24E232, "Viewer"),
    (0x3D2F5460, "DifferentType"),
    (0x3D352D54, "E"),
    (0x3D773F44, "FilterActor"),
    (0x3DA17A81, "Mod"),
    (0x3DCA3882, "LastFrame"),
    (0x3DEE792A, "Query"),
    (0x3DF56699, "Succeeded"),
    (0x3DF57959, "OnFlyAwayAndDestroy"),
    (0x3E52DC3D, "OnMove"),
    (0x3EB7CEDA, "OnSpawnFinished"),
    (0x3EC5B51F, "GrindbootsOn"),
    (0x3EF2E15E, "OnSet"),
    (0x3EFE5B79, "Out_2"),
    (0x3F05D0D5, "Length"),
    (0x3F10AE1C, "StartDropoff"),
    (0x3F1D1C4B, "InsertIndex"),
    (0x3F343BC7, "NoConnection"),
    (0x3F52BA5F, "ShowerDuration"),
    (0x3F9B75D7, "HPFractionZeroToOne"),
    (0x3FDBDCED, "OnDisable"),
    (0x3FDEC19C, "Arctangent"),
    (0x40B16AEE, "FlyerInvertY"),
    (0x40ED0031, "GTE"),
    (0x40F3E34C, "OnNormalize"),
    (0x4134F313, "Breakables"),
    (0x413F45CD, "IsUnlocked"),
    (0x41707227, "SendEvent"),
    (0x41BC117A, "Filter"),
    (0x41F297FD, "OnDestroyed_Chamber1_NodeA"),
    (0x41F2D10D, "DestinationCheckpointName"),
    (0x421D379B, "Collected"),
    (0x431386D2, "Speaker1"),
    (0x4360C8CD, "OnShuffle"),
    (0x43D5EAD1, "RayCast"),
    (0x446BF99E, "OnSpawn"),
    (0x44AFFA24, "Accel"),
    (0x44B8F888, "Facing_1"),
    (0x44D96BD9, "Velocity"),
    (0x450923DE, "DecrementSpawnCount"),
    (0x45656F4B, "OnExit"),
    (0x456E6AF1, "ShouldForceGlovePartialOff"),
    (0x45A33419, "ResetTime"),
    (0x4614D82E, "OnFirst"),
    (0x462A0976, "OnApplyScale"),
    (0x46A4EEA5, "LocalOffset"),
    (0x46E92197, "StartPos"),
    (0x4741242B, "DimensionPortal"),
    (0x47A0B1D2, "NumAlive"),
    (0x47C53BB4, "KilledSomething"),
    (0x4822588A, "UnloadDistance"),
    (0x482FAE17, "OnAccept"),
    (0x483F6B21, "SetIdle"),
    (0x48798B70, "ActorsFound"),
    (0x490784CB, "IsDemo"),
    (0x4917968E, "UIActor"),
    (0x4932B534, "RemovalIndex"),
    (0x493D92A2, "MaxHeight"),
    (0x494FA359, "Mat2Left"),
    (0x49594B39, "Vec"),
    (0x49A6F116, "TargetPos"),
    (0x49CE738C, "Reset"),
    (0x49DD17B7, "Found"),
    (0x49F96BEF, "Out_3"),
    (0x4A9A0926, "Ray_Vector"),
    (0x4AE88BA1, "AttachCurveT"),
    (0x4AF727D8, "Left"),
    (0x4B1C63F6, "OnStart"),
    (0x4B1E8B2C, "BoolItem"),
    (0x4B619001, "OnInjured"),
    (0x4B78789B, "OnLooped"),
    (0x4C184F8B, "GameMap_Open"),
    (0x4C67B9DD, "Activate"),
    (0x4CAF5C47, "Normalize"),
    (0x4CB8EFD0, "LTE"),
    (0x4CBC1621, "MechActor"),
    (0x4D25343E, "RemoveFromIndex"),
    (0x4D2CCD2C, "OnSendEvent"),
    (0x4D7E4355, "Wait"),
    (0x4DAA76B3, "ReleaseToken"),
    (0x4DE0F6DF, "OnForceBoltCollect"),
    (0x4E4F6380, "OnUnvisited"),
    (0x4E94AFF6, "Out_7"),
    (0x4EAF2E69, "Disable"),
    (0x4ECA9176, "Curve"),
    (0x4ED8AACC, "IsLombax"),
    (0x4EF8CCD9, "StartTValue"),
    (0x4F00C07F, "None"),
    (0x4F41E956, "ShotTagB"),
    (0x4F64D76E, "OnDestroyed_Chamber3_NodeB"),
    (0x4F709A06, "OnMediumIntensity"),
    (0x4F814BCC, "Resumed"),
    (0x4FB94844, "Position_0"),
    (0x4FD5B6A9, "OnLeave"),
    (0x50240EF1, "RawValue"),
    (0x502B77D0, "SpawnCount"),
    (0x5057D23E, "Kickable"),
    (0x505857A6, "ActorH"),
    (0x50BBB71E, "SwitchActor"),
    (0x50CBD121, "OnMaxIdle"),
    (0x50FF9E91, "EnableMissiles"),
    (0x5113D8BC, "Enabled"),
    (0x5115188B, "Interrupted"),
    (0x51261116, "HitActor"),
    (0x51B1F634, "Jetpack"),
    (0x51B28A1C, "CurveFollower"),
    (0x522116CE, "PercentToShow"),
    (0x522F06B0, "RemoveFromStart"),
    (0x525791FC, "SelectedString"),
    (0x5299CE91, "OnOneResult"),
    (0x52B7AFE6, "XpValue"),
    (0x52C205C2, "No"),
    (0x52C25E98, "OnComplete"),
    (0x531B560E, "UnTrigger"),
    (0x53AF2805, "Sync8"),
    (0x53C1D680, "IdealMinDistance"),
    (0x53F0C08A, "ItemCount"),
    (0x5428DDC2, "PauseAny"),
    (0x5430F2F5, "OnReceiveEvent"),
    (0x5472474D, "SetSunset"),
    (0x548BD48A, "TriggeringActor"),
    (0x549CFE88, "Unlock"),
    (0x550102BB, "ValueB"),
    (0x551BAAD0, "XPValue"),
    (0x553DF239, "OnDestroyed"),
    (0x55BAAF55, "NumberOut"),
    (0x55F4165D, "Challenge"),
    (0x563BE470, "DamageAmount"),
    (0x5642D69B, "OverrideColor"),
    (0x564C0A17, "Follow"),
    (0x56596478, "OnOpened"),
    (0x565AE3E9, "ItemIndex"),
    (0x5687706B, "ForceMount"),
    (0x568973E3, "HitPosition"),
    (0x56BE76A9, "RemoveAnimSet"),
    (0x56DB15F9, "OnSelfStopped"),
    (0x56E3704B, "OnLock"),
    (0x56F85357, "Remove"),
    (0x57250506, "OverrideCamera"),
    (0x573BA4EB, "Anim"),
    (0x574DA323, "VectorH"),
    (0x577EA15B, "OnBlock"),
    (0x57A8117F, "OnAllDeactivated"),
    (0x57DBAA33, "OnLanded"),
    (0x5814524D, "LocatorName"),
    (0x5853F850, "Victims"),
    (0x587440D9, "LastTriggeredActor"),
    (0x58746B9B, "ObjectiveName"),
    (0x58EB1630, "HeartActor"),
    (0x58FC2FAB, "IntersectCount"),
    (0x59179FDF, "ShowTime"),
    (0x59A29885, "Exited"),
    (0x59BD58DD, "KillerActor"),
    (0x59C9C958, "StopCounting"),
    (0x59C9DE7E, "OnResetToDefaults"),
    (0x59EE1B8D, "ActorD"),
    (0x5A19642E, "Sync4"),
    (0x5A3607A6, "OnAnimComplete"),
    (0x5A73E831, "OnAnyActivated"),
    (0x5A793C01, "SelectedNumber"),
    (0x5A97FB4E, "OnButtonPressed"),
    (0x5AD60BB4, "SwapToClank"),
    (0x5AE33E11, "SpawnedGroup"),
    (0x5B080CF8, "ActorFilter"),
    (0x5B14A3C0, "LockOn"),
    (0x5B32E17D, "OutFromUnTrigger"),
    (0x5B8D9442, "AssetName"),
    (0x5BF4A6A4, "OnPulseFinished"),
    (0x5C4E164C, "Finished"),
    (0x5C939BD6, "Mass"),
    (0x5CB7395A, "HitPos"),
    (0x5D269A38, "Actor"),
    (0x5D74A037, "Sync0"),
    (0x5D994DC1, "YMax"),
    (0x5DBFD4E1, "DistanceMax"),
    (0x5DDA8475, "StopBarrage"),
    (0x5DE2CF94, "Ranger"),
    (0x5DF1D5C6, "NumSpawned"),
    (0x5E33418D, "X"),
    (0x5EB99EC7, "Resume"),
    (0x5EDA607D, "TowCount"),
    (0x5EEF84F5, "SegmentEnd"),
    (0x5EFBEF08, "VectorD"),
    (0x5F0458F5, "Time"),
    (0x5F2714CA, "NumberItem"),
    (0x5F63676C, "Centipede"),
    (0x5FB21C60, "Yes"),
    (0x5FB75600, "OnDeactivated"),
    (0x600F7D01, "OnUndoMustEngage"),
    (0x603A2FC2, "IsEmpty"),
    (0x60656FF2, "OnNumDead"),
    (0x6072E288, "QuickSelect_Open"),
    (0x60D81676, "StickDirection"),
    (0x615EAE36, "Hero"),
    (0x6171B82A, "Melee_Slam"),
    (0x61773D5A, "StartBarrage"),
    (0x617804A8, "Sender"),
    (0x61947298, "YMin"),
    (0x61B2EBB8, "DistanceMin"),
    (0x61F23114, "Source"),
    (0x624D34AE, "CombatMarkupCollection"),
    (0x625EE280, "SelectedPosition"),
    (0x62A5A7EC, "OffsetRotationZ"),
    (0x62B917E6, "IsTrue"),
    (0x62C56BC8, "Extents"),
    (0x62C9B588, "OnSetDirection"),
    (0x62DA917A, "StopVolume"),
    (0x62F09C30, "Goto"),
    (0x632A765B, "OnUpdate"),
    (0x63343494, "Lerp"),
    (0x63344B7E, "ZoneName"),
    (0x6362B69E, "Heavy"),
    (0x63ED7271, "T_Value"),
    (0x64215572, "TargetNumber"),
    (0x6466DF87, "Replaced"),
    (0x6511F622, "Bool"),
    (0x651C34D0, "StartValue"),
    (0x6584E054, "OnFailed"),
    (0x666E555F, "kUV0_xyz"),
    (0x6678A7EA, "OnAffectingPulseStarted"),
    (0x66AEB3A2, "Queue"),
    (0x675EF276, "TargetMustBeInVolume"),
    (0x6768B329, "SpawnDelay"),
    (0x67A57CE2, "LookedAtActor"),
    (0x67F7EE22, "Aim_And_Fire"),
    (0x682CDA99, "MountedActor"),
    (0x683AFDF3, "OnStop"),
    (0x684EC97F, "Fire"),
    (0x686E95A0, "kUV0_xy"),
    (0x690DE4EB, "OnChoseGold"),
    (0x692CB16E, "Sync12"),
    (0x69D50C2C, "OnNotWarping"),
    (0x69E792B9, "SetTrue"),
    (0x6A4DD793, "SelectedIndex"),
    (0x6A679059, "kObjectLoD"),
    (0x6A84A393, "In"),
    (0x6AB6731E, "OnCheckNoToken"),
    (0x6B068F02, "Flee"),
    (0x6B152522, "TurnOff"),
    (0x6B5D569A, "Value_2"),
    (0x6B652A09, "DeltaTime"),
    (0x6B88128D, "DamagerActor"),
    (0x6B8D93B1, "ChangeAmplitude"),
    (0x6B96E206, "Friend"),
    (0x6BA8C1EC, "OnSent"),
    (0x6BB21F46, "WeaponsDisabled"),
    (0x6C0FF88F, "TargetString"),
    (0x6C1E3AB2, "OnEnergy"),
    (0x6C309283, "Value_6"),
    (0x6C37D594, "SetToEndAndHide"),
    (0x6C6F6A1A, "OnCheckHasToken"),
    (0x6CA7E042, "FOV"),
    (0x6CEA9754, "CompleteCleaning"),
    (0x6D15F617, "OnEnter"),
    (0x6D197E96, "InitList"),
    (0x6D1A71CF, "NumSpawns"),
    (0x6D61A2F3, "Template"),
    (0x6D790649, "BoolOut"),
    (0x6DBCDE9F, "GrindPathFollowerActor"),
    (0x6DF30668, "Ease"),
    (0x6E191E0A, "Play"),
    (0x6EB3BEB6, "Name"),
    (0x6ECD282D, "PushAnimSet"),
    (0x6EEABC8A, "Success"),
    (0x6F086A37, "CaptureWithHUD"),
    (0x6F1CF5CD, "SetDirection"),
    (0x6F75D42E, "OnOtherStopped"),
    (0x6FC8FD23, "Flock"),
    (0x6FDE054C, "OnZeroResults"),
    (0x7017FC56, "OnSetMustEngageBotGroup"),
    (0x701A8D60, "OnDestroyed_Chamber2_NodeA"),
    (0x70AF5E04, "TurnOn"),
    (0x70D6E536, "OptLevelID"),
    (0x714A4251, "SetLocked"),
    (0x71B25F80, "OnPurchase"),
    (0x71C836B3, "ClearOverride"),
    (0x71D061F1, "CheckpointName"),
    (0x71D2B390, "Inactive"),
    (0x72034311, "Value3"),
    (0x7216D54B, "NoClankPackFound"),
    (0x7245FD51, "RTTActor"),
    (0x72499036, "Light"),
    (0x729CCC77, "OnAllDone"),
    (0x72BF98FD, "kStart"),
    (0x73639E81, "MountActor"),
    (0x73AD9EDB, "StartMonitoring"),
    (0x73B142F7, "MaxSpawned"),
    (0x74496F5B, "EmptyList"),
    (0x745838A8, "List"),
    (0x747B3C99, "ShowWarning"),
    (0x748C650D, "StopIndex"),
    (0x74938804, "Group"),
    (0x74A4488B, "IsCollected"),
    (0x74D5E64E, "Attackers"),
    (0x74D9A3D8, "OnCreate"),
    (0x74DCCC67, "OnStopDidMove"),
    (0x750BBF10, "UnFocus"),
    (0x752E4A09, "L2"),
    (0x75484B11, "Recall"),
    (0x7566B265, "Active"),
    (0x756E8708, "Value7"),
    (0x758AA4A7, "Select"),
    (0x7593FE96, "ComboAttackName"),
    (0x75F5206E, "OnHide"),
    (0x7616F022, "kChargeAttackMaxDamage"),
    (0x761FD188, "OnSend"),
    (0x7625930E, "LeftStick"),
    (0x76FA4033, "GunEnabled"),
    (0x76FCC3AD, "Up"),
    (0x77059EF7, "Passive"),
    (0x770C9D48, "OnDecrement"),
    (0x776E6094, "Thrower"),
    (0x777356EC, "OnClearOverride"),
    (0x77810F07, "VectorItem"),
    (0x778AAEAD, "Unhide"),
    (0x77951811, "CameraTarget"),
    (0x7802BB95, "Override"),
    (0x7817619B, "Target"),
    (0x782D7089, "SameType"),
    (0x78A052E6, "QuickSelect_WeaponSwitch"),
    (0x78C900FC, "Paused"),
    (0x78F104FF, "MinResult"),
    (0x7902D9D5, "Interrupt"),
    (0x7918EE64, "VictimActor"),
    (0x792706C3, "PercentThreshold"),
    (0x794E00AE, "In_1"),
    (0x797C88A3, "OnActorPassive"),
    (0x79F25A27, "Easy"),
    (0x79F693E4, "OnSpawned"),
    (0x7A4B6910, "Last"),
    (0x7A8704F5, "DriverName"),
    (0x7AAE1D22, "StopListening"),
    (0x7AB6F82D, "InvertCamY"),
    (0x7B36C92D, "Volumes"),
    (0x7B502243, "Decrement"),
    (0x7B938628, "Focus"),
    (0x7B94A01E, "SetAmmo"),
    (0x7BAE0A99, "InsertAt"),
    (0x7C0ADC9D, "Owners"),
    (0x7C157D7D, "OnStopListening"),
    (0x7C5E3DB0, "Duration"),
    (0x7CC19A04, "NextLookAt"),
    (0x7CD0496F, "Hookshot"),
    (0x7E23C4B7, "In_5"),
    (0x7E2FB047, "Change"),
    (0x7E88A543, "Center"),
    (0x7E946ABB, "Vector1"),
    (0x7EBF0174, "GetSpawnCountRemaing"),
    (0x7F01324C, "OnUnblockedTokenUsers"),
    (0x7F8D0F77, "MinIndex"),
    (0x7FACF762, "Multiplier"),
    (0x7FB2E47E, "OnAllTriggered"),
    (0x7FE3B162, "OnShowWaveComplete"),
    (0x7FE6779A, "Pickup"),
    (0x7FFAE15E, "RequestToken"),
    (0x804F145B, "Sync14"),
    (0x805C4229, "RightStick"),
    (0x805CB93C, "VecVar"),
    (0x8093714D, "Num2"),
    (0x8130AB1E, "LeaveMagnetMode"),
    (0x81313CE4, "CheckHaveToken"),
    (0x813D98DF, "LookGroupApply"),
    (0x814D9DE7, "BlendedValue"),
    (0x8205688C, "OnCheckpointLoaded"),
    (0x821E4847, "DimensionCheckpointName"),
    (0x8234E2C3, "OnHit"),
    (0x823EF3AF, "Value_4"),
    (0x8267F26E, "LedgePathActor"),
    (0x82C39CFF, "OnStartRequested"),
    (0x82C81F61, "OnLerp"),
    (0x82F0D613, "OnHPThreshold"),
    (0x8300BE8B, "Go"),
    (0x836C7248, "Mat1Position"),
    (0x8384061B, "OnTargetDead"),
    (0x83B900D2, "Level"),
    (0x83F7C519, "InvertLeft"),
    (0x840C154D, "DamageType"),
    (0x84363E9F, "SetDone"),
    (0x84796E95, "EnableWeapons"),
    (0x8486BCD3, "Inside"),
    (0x84AC8CF2, "FadeIn"),
    (0x84CDBB14, "StopAndSetNewTime"),
    (0x84E6E062, "SpawnerBot"),
    (0x84F88934, "OnReset"),
    (0x855337B6, "Value_0"),
    (0x858230DE, "StopLoop"),
    (0x859A5BD9, "OnOverrideRemoved"),
    (0x85F558AF, "OffsetVec"),
    (0x860D82C3, "TimedOut"),
    (0x862A994E, "Start"),
    (0x863D4CC7, "Restore"),
    (0x86452A00, "SetUnlocked"),
    (0x86A75F9E, "ItemFound"),
    (0x86C3BECD, "ReasonTag"),
    (0x86CE77DD, "Swingshot"),
    (0x86D7E41C, "IsRatchette"),
    (0x8722D042, "Sync10"),
    (0x874F26CA, "TimeToHit"),
    (0x87E283B9, "Volume"),
    (0x87ED9AE2, "OnNewGame"),
    (0x87FF1D1D, "DisableGun"),
    (0x88024652, "Strafe"),
    (0x88831235, "MaxDistance"),
    (0x88EA47CF, "SetMustEngageBotGroup"),
    (0x8904DA4D, "Ledges"),
    (0x89123996, "OnChaseStarted"),
    (0x895D0326, "Spawn"),
    (0x8979CC14, "NE"),
    (0x898A61C0, "From"),
    (0x8999009C, "OnAccel"),
    (0x89A8337C, "OptionalSecondary"),
    (0x89B88A08, "OnError"),
    (0x89C6D606, "Stop"),
    (0x89F71D9E, "EnergyPercent"),
    (0x8A426F38, "ChallengeMissionName"),
    (0x8A54EA19, "Sent"),
    (0x8A6D500B, "OnGetLight"),
    (0x8A832AD8, "BotGroup"),
    (0x8AACE5B8, "ClankPackActor"),
    (0x8AFAA4F7, "OnFlee"),
    (0x8B222296, "First"),
    (0x8B2E3419, "Min"),
    (0x8BA5FE75, "OnMagbootsOn"),
    (0x8C00E256, "RaycastHit"),
    (0x8C0F2D9D, "OnEase"),
    (0x8C18502A, "GetIdleTime"),
    (0x8C2AD2FA, "OnStopDidNotMove"),
    (0x8C4AAC69, "DestroyShot"),
    (0x8C61D340, "Abandon"),
    (0x8CABC6C0, "OffsetRotationX"),
    (0x8CD13823, "VarName"),
    (0x8D1DADDC, "BoltShowerActor"),
    (0x8D2309F3, "SetVisited"),
    (0x8D721639, "ActorItem"),
    (0x8D8FA590, "String_1"),
    (0x8DAE56CD, "CurrentValue"),
    (0x8DBD7514, "OnConversationStarted"),
    (0x8DCFC06C, "MaxChaseSpeed"),
    (0x8E0A7B55, "GetCount"),
    (0x8E3B4FE9, "Force"),
    (0x8E86CBD9, "Cleared"),
    (0x8E8C052A, "OnManualReset"),
    (0x8E95F555, "Point_0"),
    (0x8EA5690A, "RemoveFromEnd"),
    (0x8F614AB7, "SetOverrideAutoCollect"),
    (0x8F9465F8, "OnAllDead"),
    (0x8FD542C1, "NumDeaths"),
    (0x8FF297A8, "HeroTarget"),
    (0x90028718, "ExtraSpawnGroup"),
    (0x9002E001, "DrawOn"),
    (0x902DA59B, "In_7"),
    (0x9084AF5B, "ForceComplete"),
    (0x908D417B, "OnTargetSwitch"),
    (0x90D607B0, "InitialCount"),
    (0x9139F324, "GetLevel"),
    (0x917E20A8, "ToSet"),
    (0x918D9104, "Mission"),
    (0x91B867F9, "SetUnvisited"),
    (0x91EBF3CC, "OnUnlock"),
    (0x92A6885E, "OnOverride"),
    (0x92CB9281, "kLinearDepth"),
    (0x931EB661, "FirstCameraMoveFromReload"),
    (0x932C46B8, "CreatedCamera"),
    (0x933B0753, "OnFollow"),
    (0x938F5E13, "OnRemove"),
    (0x939C2232, "SetNewTimeAndStart"),
    (0x939F21A5, "OnTokenReceived"),
    (0x93BF4450, "Out"),
    (0x94090B9B, "Hide"),
    (0x945BE058, "OnDisabled"),
    (0x947EF569, "OnHeartStarted"),
    (0x94E04314, "Open"),
    (0x9502BC0C, "FlyerNormalX"),
    (0x9516BB44, "SpawnedTemplate"),
    (0x951BD5B6, "Locations"),
    (0x952822B2, "RemovedActor"),
    (0x952FE93F, "SetDay"),
    (0x954989EE, "Width"),
    (0x9594F688, "ClearOverrideAutoCollect"),
    (0x95E1588E, "OnBlockedTokenUsers"),
    (0x95ED5A86, "StickStrength"),
    (0x95F25209, "OutFromTrigger"),
    (0x961F226B, "OnCheckState"),
    (0x96D5E58B, "Object"),
    (0x970FBE3A, "OnForward"),
    (0x971F8A4F, "OnAdvance"),
    (0x97406182, "In_3"),
    (0x97549C33, "OnStartedLaserPhase"),
    (0x977795DA, "NormalFacingVec"),
    (0x97C438A3, "Controller"),
    (0x97E3FA7D, "Send"),
    (0x97F35FD2, "MidGame"),
    (0x981029D3, "OnRespawn"),
    (0x981E6927, "MaterialMappingName"),
    (0x984A87B9, "Connected"),
    (0x98554E8C, "TargetActor"),
    (0x98A76157, "Priority"),
    (0x98F162F8, "SwapToRachette"),
    (0x98F6DAB9, "EquipWeapon"),
    (0x997B3998, "Micron"),
    (0x9987CF53, "FinalValue"),
    (0x99B8BE1E, "Heal"),
    (0x99EDA096, "MaxIndex"),
    (0x9A287017, "OnLandFin"),
    (0x9A610B3E, "AlmostDone"),
    (0x9A612B6A, "NormalCamX"),
    (0x9B239883, "SwapToRachet"),
    (0x9B2B4EBB, "ReflectionFactor"),
    (0x9B4676A3, "OnGetHeavy"),
    (0x9B60E624, "Value5"),
    (0x9B8B2052, "Interactable"),
    (0x9BB742E5, "OnLast"),
    (0x9BDD8A04, "UndoMustEngage"),
    (0x9BFD6967, "LookGroupOn"),
    (0x9C0D223D, "Value1"),
    (0x9C15072C, "kGeometryCurvature"),
    (0x9C1E8027, "OnDespawn"),
    (0x9C41C559, "NoQueuedUsers"),
    (0x9C6C9BA5, "AllFalse"),
    (0x9C8A88A3, "Count"),
    (0x9CD95BFB, "Mat2Forward"),
    (0x9D99369A, "kCustomLightDiffuse"),
    (0x9DC212F9, "DeltaTimeReal"),
    (0x9DE7FAE2, "SetToEnd"),
    (0x9DF9ADEA, "DetachActor"),
    (0x9E261113, "TimeRemaining"),
    (0x9E2FDC16, "StopTimer"),
    (0x9E335145, "LastShot"),
    (0x9E36FA25, "IsNew"),
    (0x9E49872C, "Results"),
    (0x9E576837, "ClankPackFound"),
    (0x9EA1DD6D, "SectionActor"),
    (0x9EC45E70, "BeamStartActor"),
    (0x9ED620E3, "MinHeight"),
    (0x9F2668AE, "LeftTriggerValue"),
    (0x9F82A66C, "AContainsB"),
    (0x9F92F62A, "Result2"),
    (0x9FCCAD8F, "IdealMaxDistance"),
    (0xA0334B05, "Injure"),
    (0xA07162B5, "Calculate"),
    (0xA098FB1E, "TriggerAndEndOnBlack"),
    (0xA09ACEDA, "Out_5"),
    (0xA0C58260, "Key"),
    (0xA0C6FCCE, "String"),
    (0xA0F3ED10, "Failed"),
    (0xA14E47C0, "StartLaserPhase"),
    (0xA19DD77C, "ResultUp"),
    (0xA1B67974, "OnTokenReleased"),
    (0xA2195EC2, "Changed"),
    (0xA2979036, "WindowSize"),
    (0xA34F3ED8, "OnAnyDeactivated"),
    (0xA351B8F7, "B"),
    (0xA36B8CDD, "OnClearOverrideAutoCollect"),
    (0xA37BF657, "LookAtActor"),
    (0xA3A1ADEE, "OnForceInstantCollect"),
    (0xA40132FF, "Float2"),
    (0xA40E7A50, "OnConversationRestarted"),
    (0xA47C40DE, "Apply"),
    (0xA49F34BB, "Location"),
    (0xA4A08ABA, "ResetToOne"),
    (0xA4BF0722, "BeamEndActor"),
    (0xA4D1B71F, "Dismount"),
    (0xA4F73AF8, "StopLocationActor"),
    (0xA54D35B5, "RemovedNumber"),
    (0xA5585014, "OnRequestQueued"),
    (0xA5972BAE, "End_Position"),
    (0xA5C30E60, "MissionName"),
    (0xA5CE86C8, "Neutral"),
    (0xA5E4403E, "LookAtTarget"),
    (0xA5F8FD64, "Register"),
    (0xA6224C63, "ShotTagD"),
    (0xA6AA4C89, "Incomplete"),
    (0xA6C38A16, "OnActivate"),
    (0xA6C72F2C, "CaptureNoHUD"),
    (0xA74C2BD4, "Stopped"),
    (0xA75A871B, "Mode"),
    (0xA75CB40E, "TargetName"),
    (0xA773EE6C, "AlreadyThere"),
    (0xA789EFAF, "OnUpdateChkpt"),
    (0xA7F70AC3, "Out_1"),
    (0xA81F5602, "NotObstructed"),
    (0xA84BD2DA, "EndCinematic"),
    (0xA86A0E1C, "Parameter"),
    (0xA88022F2, "OldLength"),
    (0xA8853E21, "Backward"),
    (0xA8E85133, "Number"),
    (0xA8EDDDEF, "CenterOffset"),
    (0xA9242A88, "Decel"),
    (0xA95DCF2B, "Pick"),
    (0xA96937F6, "Energy"),
    (0xA98F56A1, "OnGetPuzzleInfo"),
    (0xA9A37991, "Numerator"),
    (0xA9B2D5B2, "OnWorldEngaged"),
    (0xA9E43DB1, "FadeOut"),
    (0xAA3B4631, "GetDifficulty"),
    (0xAA6C80DC, "kMaxPredictiveSpeed"),
    (0xAA8CCF65, "Pos"),
    (0xAAA3B568, "SetDefault"),
    (0xAAEB173A, "DisableWeapons"),
    (0xAB545100, "IsNotTriggered"),
    (0xAB9DF22F, "Reverse"),
    (0xABD047C2, "LastUnTriggeredActor"),
    (0xAC2DDDBE, "OnCalculate"),
    (0xAC563E57, "OnDamaged"),
    (0xAC8268A0, "OnWait"),
    (0xACA37F46, "FaceAtActor"),
    (0xACE9D9BB, "Checked"),
    (0xACF6805A, "kGeometryNormal"),
    (0xACFEFC9A, "TargetBool"),
    (0xAD15BF35, "ChaseTarget"),
    (0xAD198651, "OnCancel"),
    (0xAD1DE7FE, "Speaker3"),
    (0xAD22011A, "kScreenUVs"),
    (0xAD412678, "IsKit"),
    (0xAD639848, "RemovedString"),
    (0xAD7047DA, "LookedAwayActor"),
    (0xADF7B933, "FirepointSet"),
    (0xAE4BE66B, "SelectedActor"),
    (0xAE544C26, "OnHeavy"),
    (0xAE568D37, "HideTime"),
    (0xAE6FD789, "VisitedAlready"),
    (0xAEA97D2B, "Targets"),
    (0xAEF63FE5, "Point"),
    (0xAEFCEB8A, "OnNone"),
    (0xAF1AB6BE, "MaxResult"),
    (0xAF3436FE, "GetPuzzleInfo"),
    (0xAFAC8374, "BirdTemplate"),
    (0xAFC1092B, "HPIndex"),
    (0xB0328856, "BothNormalCam"),
    (0xB03CB11B, "OutLock"),
    (0xB03D20A1, "Z"),
    (0xB03F4655, "OnRecall"),
    (0xB0880404, "OnLoaded"),
    (0xB08C5E61, "ApplyOverride"),
    (0xB08DBEB8, "ActorB"),
    (0xB0A45922, "CompleteVar"),
    (0xB0AC7409, "DemoType"),
    (0xB0D93E25, "Amount"),
    (0xB0DE416C, "OnAffectingPulseHit"),
    (0xB0F58E24, "VectorF"),
    (0xB0FDA9E3, "OnSelect"),
    (0xB11CED30, "SelectedBool"),
    (0xB145C7D8, "Resurrect"),
    (0xB14EC519, "AnimClue"),
    (0xB1AEAE9C, "Create"),
    (0xB1C79F97, "OnEngaged"),
    (0xB1F9B3F1, "FlyingMountActor"),
    (0xB208D15D, "BothInvertCam"),
    (0xB23BA3F6, "BreakableActor"),
    (0xB27E60EF, "ShowRemapTutorial"),
    (0xB296324E, "InteractObject"),
    (0xB2DE085D, "OnDeactivate"),
    (0xB31A6F71, "Destination"),
    (0xB32658AB, "kUV0_x"),
    (0xB37847EE, "Facing"),
    (0xB37AC11B, "Sync2"),
    (0xB3CBE75C, "IsCasual"),
    (0xB4170502, "Sync6"),
    (0xB43FFE79, "ComparisonVariable"),
    (0xB4402DF8, "OnVisited"),
    (0xB4597C69, "OnCompleted"),
    (0xB4A6AAAA, "OnLineComplete"),
    (0xB4AB78CB, "String2"),
    (0xB4C9288A, "OnChoseSilver"),
    (0xB57CF5E2, "Started"),
    (0xB59F464A, "Axis"),
    (0xB59F6958, "GetActors"),
    (0xB5F132D1, "Strafe_And_Fire"),
    (0xB63403BB, "TokenId"),
    (0xB635761B, "OverrideCheckpointName"),
    (0xB6A57C90, "OnFocus"),
    (0xB6B0F22C, "CheckType"),
    (0xB6B7EBF9, "Fail"),
    (0xB6C360DA, "TowGroup"),
    (0xB6EA2587, "OnFinished"),
    (0xB716C472, "OnQueued"),
    (0xB71F5BBE, "Lock"),
    (0xB7230B40, "Max"),
    (0xB739C57D, "StartListen"),
    (0xB754C5C8, "OnLookAt"),
    (0xB7575347, "Message"),
    (0xB757561F, "SetActor"),
    (0xB7984A3D, "VectorB"),
    (0xB7E07AA1, "ActorF"),
    (0xB805C362, "Completed"),
    (0xB80E5C98, "OnCameraCreated"),
    (0xB8666D53, "Complete"),
    (0xB8DD5168, "Low"),
    (0xB919210D, "EndGame"),
    (0xB942A405, "ResetOverride"),
    (0xB943ECD6, "BoolVar"),
    (0xB973DA64, "GiveAmmo"),
    (0xB98C1C37, "DimensionCheckpoint"),
    (0xB9C3D653, "OnGetActors"),
    (0xB9F88DFB, "Acceleration"),
    (0xBA26C1AF, "NewTime"),
    (0xBA75949D, "Door"),
    (0xBA917ADE, "OnPickup"),
    (0xBABD7B99, "OptSource"),
    (0xBAE14E8D, "Mat2Position"),
    (0xBB58BD03, "OnChange"),
    (0xBB99D2AE, "OnAnyUnTrigger"),
    (0xBC236B65, "Compare"),
    (0xBC2D5985, "Position"),
    (0xBC8B3A19, "OnGetAccel"),
    (0xBCA20BBD, "OnEmpty"),
    (0xBCB74EBF, "Direction"),
    (0xBCF8FC10, "RemovedBool"),
    (0xBD2AF6F5, "CleanupComplete"),
    (0xBD606CDF, "OnTarget"),
    (0xBD860130, "ForceStopAll"),
    (0xBD8A93AC, "Val"),
    (0xBDA77FE6, "OnForceGlovePartialOff"),
    (0xBDE6832F, "Divide"),
    (0xBE490D7A, "HasQueuedUsers"),
    (0xBE6CA2F6, "LockHeroInput"),
    (0xBEB55DA5, "SpawnCountRemaining"),
    (0xBEE35768, "UnionCount"),
    (0xBF4547F7, "LookGroupOff"),
    (0xBF54F10A, "OnLookAway"),
    (0xBF7F6A8E, "OnLight"),
    (0xBF869945, "Removed"),
    (0xBFA08C78, "HitWeakspot"),
    (0xBFB415EA, "RequestStart"),
    (0xBFB4ABDC, "AttachToActor"),
    (0xC03BCAFB, "Jump_HeliHigh"),
    (0xC07F559B, "LockCameraInput"),
    (0xC09F7AAB, "VectorC"),
    (0xC0B36D13, "Engaged"),
    (0xC0C8F740, "Counter"),
    (0xC0E74A37, "ActorG"),
    (0xC115D639, "TeamType"),
    (0xC1410BEE, "OnDone"),
    (0xC1744CC9, "SetMultiplier"),
    (0xC19524E5, "High"),
    (0xC1955E70, "Facing_Vec"),
    (0xC20B7618, "YesNo"),
    (0xC2254DA3, "OnLowIntensity"),
    (0xC23A6C53, "CurveActor"),
    (0xC2902B80, "StartLoop"),
    (0xC3103594, "Sync7"),
    (0xC3536DBB, "Platform"),
    (0xC366AF91, "Denominator"),
    (0xC3692EDB, "OnWarnIdle"),
    (0xC3981B5C, "OnAllActivated"),
    (0xC4080766, "OnStarted"),
    (0xC47DF18D, "Sync3"),
    (0xC529E434, "SetComplete"),
    (0xC56D33F6, "AdditonalAttackers"),
    (0xC5C96220, "Starting_Position"),
    (0xC640431B, "ChangeConfig"),
    (0xC6991700, "ConstantName"),
    (0xC6BC624D, "PuzzleVolume"),
    (0xC6F244E1, "WeaponsEnabled"),
    (0xC733BA61, "OutUnlock"),
    (0xC78A8E2E, "ActorC"),
    (0xC7B931E6, "Receiver"),
    (0xC7D40B96, "EndDestActor"),
    (0xC7E39684, "Arachnodroid"),
    (0xC7F2BEB2, "VectorG"),
    (0xC80ED09A, "ResultLeft"),
    (0xC843C120, "RelativeActor"),
    (0xC8CB81D1, "DummyOut"),
    (0xC8E695CD, "Skip"),
    (0xC8FA5ECF, "OnArrive"),
    (0xC992A2CE, "OnTimeOut"),
    (0xC9B3F5AB, "Alpha"),
    (0xC9BFCDFC, "Mat1Up"),
    (0xC9EE30A8, "Deceleration"),
    (0xCA0166CC, "WeaponName"),
    (0xCA1FB3D7, "WindowPosition"),
    (0xCA28941F, "MissilesEnabled"),
    (0xCAC6E239, "ListenTo"),
    (0xCB0940E9, "OnDestroyed_AllNodes"),
    (0xCBCE5683, "OnCheckTrue"),
    (0xCBD3D2DA, "RotatedPosition"),
    (0xCBF973A5, "Mat2Up"),
    (0xCC085301, "ValueA"),
    (0xCC23159E, "ReplaceAt"),
    (0xCC410C15, "Set"),
    (0xCC4A4D21, "ActorName"),
    (0xCC6A20A0, "Invert"),
    (0xCC86112F, "OnRotate"),
    (0xCCC031F4, "Same"),
    (0xCD47271B, "Hardest"),
    (0xCD6C8C3D, "True"),
    (0xCD80B81A, "MinChaseSpeed"),
    (0xCDBBD08D, "OnCleanupComplete"),
    (0xCDD88655, "Enable"),
    (0xCE1AC0CB, "FlockVolume"),
    (0xCE44F4A2, "NumActorsWithAllegiance"),
    (0xCE64FF62, "Lookup"),
    (0xCEC95E2C, "Pause"),
    (0xCECA7B47, "OnActorEngaged"),
    (0xCEF26BC1, "OnRemoved"),
    (0xCEF83FBC, "Degrees"),
    (0xCF30D41B, "HealedSomeone"),
    (0xCFD9F118, "NotSame"),
    (0xD091623C, "OnItemSelected"),
    (0xD0D2F2C5, "kUV1_xy"),
    (0xD0D66CE2, "IsRobot"),
    (0xD0F03A55, "Out_0"),
    (0xD1157663, "Arcsine"),
    (0xD1323F96, "CurrentLoopCount"),
    (0xD136A38A, "TowPercent"),
    (0xD1649F2E, "ShowWaveCountdown"),
    (0xD1A52726, "IsOther"),
    (0xD1BB7F3B, "IsLocked"),
    (0xD1F48C41, "ApplyScale"),
    (0xD1FF8E64, "LT"),
    (0xD2EC5C2A, "Added"),
    (0xD2F0F6BC, "Mangler"),
    (0xD3064C9F, "UpdateShow"),
    (0xD36DAC46, "OnChanged"),
    (0xD37D2581, "Rotation"),
    (0xD3AAD01F, "FlyInStartVolume"),
    (0xD41A2975, "SweptSphere"),
    (0xD436A987, "Checkpoint"),
    (0xD4568861, "C"),
    (0xD4BA744C, "OnNeutral"),
    (0xD543FB79, "OnNext"),
    (0xD638D950, "OnStopped"),
    (0xD648B8EC, "ShotTagA"),
    (0xD66D86D4, "OnDestroyed_Chamber3_NodeA"),
    (0xD6B4B760, "Melee"),
    (0xD6BE6A94, "OnIntroEnd"),
    (0xD6C021F9, "FloatVar"),
    (0xD6F8C91D, "IdealSpeed"),
    (0xD7124EA3, "AttachPoint"),
    (0xD74F5A2E, "Add"),
    (0xD76E07B9, "Get"),
    (0xD775D942, "Increment"),
    (0xD7876B0E, "SelectedVector"),
    (0xD79DFE4C, "Out_4"),
    (0xD8B122F3, "Pushed"),
    (0xD8EF68D6, "SetFalse"),
    (0xD8FBC647, "OnDestroyed_Chamber1_NodeB"),
    (0xD9074BEC, "ActorVar"),
    (0xDA0F12CA, "StartListening"),
    (0xDA1AD768, "Speaker2"),
    (0xDA87F4AC, "HeroActor"),
    (0xDA8C6E3A, "Skipped"),
    (0xDAD4071F, "ClearList"),
    (0xDAE900AB, "OnReverse"),
    (0xDB0803EF, "Deployer"),
    (0xDB2783F1, "Cinematic2"),
    (0xDB296649, "OnIncrement"),
    (0xDB2E826B, "OnSkipPuzzle"),
    (0xDB59A7F9, "Subtract"),
    (0xDBA478FB, "ListIndex"),
    (0xDBA63DC1, "BlockTokenUsers"),
    (0xDBB78549, "SetReady"),
    (0xDBC1A66E, "Speakers"),
    (0xDC2475E2, "Scalar"),
    (0xDC7DA44F, "CheckTarget"),
    (0xDCEBA260, "AnimVarName"),
    (0xDD23E23E, "LandedVolumeOrCurve"),
    (0xDD58CF6E, "AnyQueuedUsers"),
    (0xDD9D2B3F, "OnChecked"),
    (0xDDAA3A52, "StartLocationActor"),
    (0xDDDC559E, "StopMonitoring"),
    (0xDE373841, "Migrated"),
    (0xDED57043, "OnSlerp"),
    (0xDEE5EEA8, "OnToggle"),
    (0xDF21B6CB, "EnablePortalRespawn"),
    (0xDFAEF7C8, "Move"),
    (0xDFB8379B, "OnDecrementdSpawnCount"),
    (0xE004F13F, "OptTargetKillers"),
    (0xE03811FD, "Description"),
    (0xE0475114, "In_2"),
    (0xE062A024, "NotFound"),
    (0xE1287242, "OnHighIntensity"),
    (0xE13B8053, "IsFirstPlaythrough"),
    (0xE153BE82, "Actors"),
    (0xE159C8A9, "DelayAttackTime"),
    (0xE1B46284, "LocationResults"),
    (0xE2058C9A, "FlyerNormalY"),
    (0xE254C40B, "AllowDismount"),
    (0xE270D0BE, "VanityActor"),
    (0xE2898744, "SetCanPickup"),
    (0xE2925E8F, "Cosine"),
    (0xE2CC9668, "Enemy"),
    (0xE2EFFF19, "To"),
    (0xE2F80F71, "SetNight"),
    (0xE30455E1, "LevelID"),
    (0xE3254CD6, "Warp"),
    (0xE33D1FFC, "OnFirstDone"),
    (0xE3623244, "OnShow"),
    (0xE3A780A8, "OnAffectingPulseEnded"),
    (0xE3C5530A, "PickIntoGroup"),
    (0xE4701AC7, "DistractionTime"),
    (0xE48F99BC, "OnShowWaveCountdown"),
    (0xE499E07A, "SelectedLabel"),
    (0xE4C7E89B, "ValueOut"),
    (0xE515DA55, "AddToStart"),
    (0xE57AA4BB, "MaxResults"),
    (0xE5CBDFFC, "NotDemo"),
    (0xE633D58B, "DisablePortalRespawn"),
    (0xE66B78CB, "Advance"),
    (0xE67B4CBE, "Forward"),
    (0xE69327F1, "Distance"),
    (0xE69A20D7, "FacingVec"),
    (0xE6D76949, "GotoFacing"),
    (0xE72A950D, "In_6"),
    (0xE744B589, "HPMax"),
    (0xE74FBA95, "Replay"),
    (0xE79D3B01, "Vector2"),
    (0xE7FBAEFB, "IsFullReplay"),
    (0xE80518C3, "GetAmmo"),
    (0xE8EB9A6A, "OnCheck"),
    (0xE913DCDA, "OnDestroyed_Chamber2_NodeB"),
    (0xE93F6301, "OnInterrupted"),
    (0xE94325C0, "LoadDistance"),
    (0xE950071D, "Amplitude"),
    (0xE964DB57, "Respawn"),
    (0xE992E16A, "ModifierName"),
    (0xE9CC1CC8, "IsNotCasual"),
    (0xE9DF027D, "TargetVector"),
    (0xE9E297AF, "Continue"),
    (0xE9FE338F, "Sine"),
    (0xEA0A7021, "OnGroupEngaged"),
    (0xEA946D39, "Wave"),
    (0xEAD514E4, "HPFraction"),
    (0xEAF96445, "Recipient"),
    (0xEB0A12AB, "Value0"),
    (0xEB200F71, "Equip"),
    (0xEB2E1E87, "QueryActivation"),
    (0xEB320378, "CanPickupCount"),
    (0xEBB531BD, "OrbActor"),
    (0xEBB79C3B, "Multiply"),
    (0xEC67D6B2, "Value4"),
    (0xEC688DE6, "Jump_Single"),
    (0xED197C11, "EnterMagnetMode"),
    (0xED52B0C2, "WeaponActor"),
    (0xED661BFC, "NormalCamY"),
    (0xED6A72A3, "Despawn"),
    (0xED71B337, "DisabledOnDeathStart"),
    (0xEDBC4CD5, "BContainsA"),
    (0xEDD5E282, "Templates"),
    (0xEDEEA0F2, "IsRatchet"),
    (0xEDF971E7, "SongIndex"),
    (0xEE3471C4, "DestinationVec"),
    (0xEE3992D5, "AllTrue"),
    (0xEE5558D2, "SetValue"),
    (0xEE96577D, "StartChase"),
    (0xEEFA1FEF, "Different"),
    (0xEEFBC674, "OnPreClose"),
    (0xEF5D166F, "Facing_Direction"),
    (0xEF786E04, "IdleTime"),
    (0xEF938013, "PrintString"),
    (0xEF952F1C, "AllSame"),
    (0xEFAE4EF3, "LoopOutput"),
    (0xEFCB3CDE, "OnEvent"),
    (0xF0243ED1, "Base"),
    (0xF025E0D4, "Sync11"),
    (0xF04539C2, "AttackTargetActor"),
    (0xF0755BC9, "RemoveFromCinematic"),
    (0xF0B75112, "AllShots"),
    (0xF0D88392, "OnQuery"),
    (0xF0D89680, "MinSpeed"),
    (0xF119FC6D, "VectorOut"),
    (0xF11CC520, "Exponent"),
    (0xF185DF62, "Hard"),
    (0xF1E1BA75, "VolumeTracker"),
    (0xF22459B8, "StartCounting"),
    (0xF24377B7, "ActorCount"),
    (0xF2540720, "Value_1"),
    (0xF2B5BB32, "EventType"),
    (0xF3703347, "OnFadeComplete"),
    (0xF3AF6B25, "Clues"),
    (0xF4CED227, "DamageWeakspot"),
    (0xF515E945, "Scale"),
    (0xF539C339, "Value_5"),
    (0xF5ABFC95, "TotalTransitionTime"),
    (0xF5C96091, "CompleteFinished"),
    (0xF615819B, "Unload"),
    (0xF61C4918, "SaveID"),
    (0xF63D95D8, "FaceActor"),
    (0xF64719B9, "NotChallenge"),
    (0xF6949DF3, "Ray_Start_Point"),
    (0xF6FD8E51, "UnlockHero"),
    (0xF74824CD, "Sync15"),
    (0xF7499071, "PostStop"),
    (0xF765C4E5, "OnValueChanged"),
    (0xF76A3D65, "kVertexColor"),
    (0xF76E8BAF, "MaxActive"),
    (0xF79D0D6D, "UpdateSpawner"),
    (0xF7EDFFC8, "OnAllSpawned"),
    (0xF805A3F6, "BlendTime"),
    (0xF8A8C3C5, "GetHP"),
    (0xF992C5C3, "Point_1"),
    (0xF9EA7060, "ListenEventName"),
    (0xFA11CD7C, "ShowerSpawnCount"),
    (0xFA760ED4, "OnBounced"),
    (0xFA7DFC5B, "OnBootsOff"),
    (0xFA889506, "String_0"),
    (0xFADC7DC1, "Factory"),
    (0xFB73149E, "ResultPosition"),
    (0xFBACF656, "OffsetRotationY"),
    (0xFBC57856, "OnPortalRespawnDisabled"),
    (0xFBE36A2A, "StringVar"),
    (0xFC04FE10, "SetSpeed"),
    (0xFC285810, "Jump_Double"),
    (0xFC60458F, "ForceSpawn"),
    (0xFCE23DC7, "Clear"),
    (0xFCF0B3D6, "OnReady"),
    (0xFD0A00E2, "ReferenceActor"),
    (0xFDD04315, "IsNot"),
    (0xFE43E504, "OnAnyTrigger"),
    (0xFEDC13E1, "Glide"),
    (0xFEDE2AF2, "OneTrue"),
    (0xFEEB8C02, "OnAllUnTriggered"),
    (0xFF57959D, "StringItem"),
    (0xFFA85E49, "OnFalse"),
    (0xFFC80EC7, "OnClosed"),
];

/// Node type name -> (`select XxxInputs`, `XxxOutputs`, `XxxVars`), 396 types,
/// sorted by type name. Lets an editor draw the pins nothing is wired to.
pub static NODE_PLUGS: &[(&str, &[&str], &[&str], &[&str])] = &[
    ("AccelLerpVecAction", &["Start", "Stop"], &["Done", "OnStart", "OnStop", "OnUpdate"], &["Accel", "Decel", "MaxSpeed", "Result", "StartPos", "TargetPos"]),
    ("ActivateCameraAction", &["In"], &["OnCameraCreated", "Out"], &["AudioTarget", "BlendTime", "Camera", "CameraTarget", "CreatedCamera", "Hero"]),
    ("ActorActivationQueryAction", &["QueryActivation"], &["OnAllActivated", "OnAllDeactivated", "OnAnyActivated", "OnAnyDeactivated"], &["Target"]),
    ("ActorBoundsAction", &["Get"], &["OnGet"], &["Actor", "Center", "Extents", "Radius"]),
    ("ActorControlAction", &["Activate", "Deactivate"], &["OnActivate", "OnDeactivate", "Out"], &["Target"]),
    ("ActorCountAction", &["In"], &["Out"], &["ActorCount", "ActorGroup"]),
    ("ActorDrawAction", &["DrawOff", "DrawOn"], &["DrawTurnedOff", "DrawTurnedOn", "Out"], &["Actor"]),
    ("ActorGroupClearAction", &["Clear"], &["Out"], &["Group"]),
    ("ActorGroupControlAction", &["Add", "Remove"], &["Out"], &["Actors", "Group"]),
    ("ActorPickAction", &["In"], &["Out"], &["Group", "Pick", "PickIntoGroup", "Reference", "Value"]),
    ("ActorPositionAction", &["In"], &["Out"], &["Actor", "Facing", "Left", "LocatorName", "Position", "Up"]),
    ("ActorToggleLookAction", &["LookOff", "LookOn"], &["Out"], &["Actor"]),
    ("ActorToggleLookGroupAction", &["LookGroupApply", "LookGroupOff", "LookGroupOn"], &["Out"], &["Actor"]),
    ("ActorVolumeAction", &["In"], &["IsFalse", "IsTrue", "Out"], &["Actors", "LocationResults", "Locations", "Results"]),
    ("ActorWarpAction", &["In"], &["Out"], &["Actors", "Destination", "DestinationVec", "Facing", "FacingVec", "LookTarget", "OffsetRotationX", "OffsetRotationY", "OffsetRotationZ", "OffsetVec", "Source", "UpVec"]),
    ("AddAction", &["In"], &["Out"], &["Result", "Value_0", "Value_1", "Value_2", "Value_3", "Value_4", "Value_5", "Value_6", "Value_7"]),
    ("AddCurrencyAction", &["In"], &["Out"], &["Result", "Target", "Value_0"]),
    ("AddMapIconAction", &["Add", "Remove"], &["OnAdd", "OnRemove"], &["Actor"]),
    ("AddVectorsAction", &["In"], &["Out"], &["Target", "VectorA", "VectorB", "VectorC", "VectorD", "VectorE", "VectorF", "VectorG", "VectorH"]),
    ("AddXPAction", &["In"], &["Out"], &["Target", "XPValue"]),
    ("AimModLeadTargetPriusMod", &[], &[], &["kMaxPredictiveSpeed", "kNone"]),
    ("AmbientBarrageControlAction", &["ChangeConfig", "StartBarrage", "StopBarrage"], &["BarrageStarted", "BarrageStopped", "Out"], &["AmbientShooterGroup", "Controller", "Targets"]),
    ("AndAction", &["In_0", "In_1", "In_2", "In_3", "In_4", "In_5", "In_6", "In_7", "Reset"], &["OnReset", "Out"], &[]),
    ("AnimDamageAttackMod", &[], &[], &["kAnimDamageAttackAmount", "kChargeAttackMaxDamage", "kNone"]),
    ("AnimateAnimVarAction", &["Forward", "Pause", "Reset", "Reverse"], &["OnComplete", "OnForward", "OnPause", "OnReverse"], &["Actor", "FinalValue", "StartValue", "TotalTransitionTime", "VarName"]),
    ("ApplyEaseAction", &["Ease"], &["OnEase"], &["Float1", "Float2", "Result", "T_Value"]),
    ("AttachActorToViewportAction", &["In"], &["Out"], &["AttachActor", "Hero", "LocalOffset", "LocalRotation"]),
    ("AutoCheckpointAction", &["Disable", "Enable"], &["OnDisable", "OnEnable"], &[]),
    ("BitFlagsAction", &["Check", "Clear", "Set"], &["OnCheckFalse", "OnCheckTrue", "OnClear", "OnSet"], &["BitIndex", "InputFlags", "OutputFlags"]),
    ("BoltCrankSetStateAction", &["SetComplete", "SetIdle", "SetLocked", "SetUnlocked"], &["Out"], &["Actor"]),
    ("BoltScriptAction", &["ClearOverrideAutoCollect", "ForceBoltCollect", "ForceInstantCollect", "SetOverrideAutoCollect"], &["OnClearOverrideAutoCollect", "OnForceBoltCollect", "OnForceInstantCollect", "OnSetOverrideAutoCollect"], &["Recipient"]),
    ("BotCommandGotoScriptAction", &["Queue", "Start"], &["OnCompleted", "OnFailed", "OnInterrupted", "OnQueued", "OnStarted"], &["Bot", "Destination", "DestinationPos", "Facing", "FollowDist", "GotoFacing", "Last"]),
    ("BotCommandPlayAnimScriptAction", &["Queue", "Start"], &["OnCompleted", "OnFailed", "OnInterrupted", "OnQueued", "OnStarted"], &["Bot", "DriverName", "Last", "LookAtTarget", "TurnToTarget"]),
    ("BotCommandUseComboAttackScriptAction", &["Queue", "Start", "Stop"], &["OnCompleted", "OnFailed", "OnInterrupted", "OnQueued", "OnStarted"], &["AttackTargetActor", "Bot", "ComboAttackName", "DelayAttackTime", "Last"]),
    ("BotGroupMustEngageScriptAction", &["SetMustEngageBotGroup", "UndoMustEngage"], &["OnSetMustEngageBotGroup", "OnUndoMustEngage"], &["BotGroup", "Target", "TargetMustBeInVolume"]),
    ("BreakpointAction", &["In"], &["Out"], &[]),
    ("CameraPositionAction", &["In"], &["Out"], &["Facing", "Hero", "Left", "Position", "Up"]),
    ("CameraRaycastPositionAction", &["In"], &["Out", "RaycastHit"], &["NormalFacingVec", "Position"]),
    ("CameraShakeAction", &["ForceStopAll", "Start", "Stop"], &["Out"], &["Hero", "Position"]),
    ("ChangeCurveSpeedAction", &["Set"], &["Done"], &["Accel", "Actor", "Decel", "Speed"]),
    ("ChangeFlockDensityAction", &["Clear", "Set"], &["OnClear", "OnSet"], &[]),
    ("ChangeOpenCurveFollowModeAction", &["Change"], &["Out"], &["CurveFollower"]),
    ("CharacterSwapAction", &["SwapToClank", "SwapToKit", "SwapToRachet", "SwapToRachette"], &["Out"], &["Hero"]),
    ("CheckActorCombatStateAction", &["Check"], &["Engaged", "OnCheck", "Passive"], &["Actor", "Duration"]),
    ("CheckAnimFilterAction", &["Check"], &["OnFalse", "OnTrue"], &["Actor"]),
    ("CheckHeroTypeAction", &["CheckType"], &["IsClank", "IsKit", "IsLombax", "IsOther", "IsRatchet", "IsRatchette", "IsRobot"], &["HeroActor"]),
    ("CheckMissionStateAction", &["CheckState"], &["Active", "Available", "CompleteCleaning", "CompleteFinished", "Failed", "Inactive", "OnCheckState"], &["MissionName"]),
    ("CheckObjectiveStateAction", &["CheckState"], &["Active", "Complete", "Failed", "Inactive", "OnCheckState"], &["MissionName", "ObjectiveName"]),
    ("CheckPuzzleTargetTypeAction", &["CheckTarget"], &["Is", "IsNot"], &["Target"]),
    ("CheckPuzzleVolumeAction", &["StartListening", "StopListening"], &["OnEnter", "OnExit"], &["Target", "Volume"]),
    ("CheckPuzzleVolumeTrackerAction", &["StartListening", "StopListening"], &["OnEnter", "OnExit"], &["Targets", "VolumeTracker"]),
    ("CheckPuzzleVolumeTypeAction", &["Check"], &["OnAccel", "OnEnergy", "OnHeavy", "OnLight", "OnNone"], &["PuzzleVolume"]),
    ("CheckWorldCombatStateAction", &["Check"], &["Engaged", "OnCheck", "Passive"], &[]),
    ("CheckpointWarpAction", &["Warp"], &["Out"], &["CheckpointName", "DimensionCheckpointName", "Hero"]),
    ("Cinematic2Action", &[], &["AlmostDone", "Completed", "LastFrame", "OnExitLoop", "OnReady", "OnTriggered", "Skipped", "Started", "Sync0", "Sync1", "Sync10", "Sync11", "Sync12", "Sync13", "Sync14", "Sync15", "Sync2", "Sync3", "Sync4", "Sync5", "Sync6", "Sync7", "Sync8", "Sync9", "TimedOut"], &["ActorA", "ActorB", "ActorC", "ActorD", "ActorE", "ActorF", "ActorG", "ActorH", "Cinematic2"]),
    ("Cinematic2ActorAction", &["RemoveFromCinematic"], &["OnRemoved"], &["Actors"]),
    ("Cinematic2ListenerAction", &["StartListening", "StopListening"], &["AlmostDone", "Completed", "LastFrame", "OnExitLoop", "OnReady", "OnTriggered", "Skipped", "Started", "Sync0", "Sync1", "Sync10", "Sync11", "Sync12", "Sync13", "Sync14", "Sync15", "Sync2", "Sync3", "Sync4", "Sync5", "Sync6", "Sync7", "Sync8", "Sync9", "TimedOut"], &["Cinematic2"]),
    ("Cinematic2RobotestAction", &[], &[], &["Cinematic2", "TestName"]),
    ("Cinematic2TriggerEventOnActorAction", &["Trigger"], &["OnTriggered"], &["ActorName", "Cinematic2"]),
    ("CinematicPlayerStateAction", &["Activate", "Deactivate"], &["OnActivate", "OnDeactivate"], &["Hero"]),
    ("ClampAction", &["In"], &["Out"], &["Max", "Min", "Result", "Value"]),
    ("ClankPuzzleGetLastNavAction", &["Get"], &["OnGet"], &["Pos", "Target"]),
    ("ClearAnimDamageAction", &["Clear"], &["OnClear"], &["Target"]),
    ("ClearClankPuzzleRecalOrbAction", &["Clear"], &["OnClear"], &[]),
    ("ClearCurveAction", &["Clear"], &["Done"], &["Actor"]),
    ("CompareActorAction", &["In"], &["Different", "DifferentType", "Equal", "SameType"], &["ActorA", "ActorB"]),
    ("CompareActorGroupAction", &["In"], &["AContainsB", "BContainsA", "Different", "NotSame", "Same", "SomeOverlap"], &["ActorA", "ActorB", "IntersectCount", "UnionCount"]),
    ("CompareBooleanAction", &["In"], &["AllFalse", "AllSame", "AllTrue", "NotSame", "OneFalse", "OneTrue"], &["Value_0", "Value_1", "Value_2", "Value_3", "Value_4", "Value_5", "Value_6", "Value_7"]),
    ("CompareNumberAction", &["In"], &["E", "GT", "GTE", "LT", "LTE", "NE"], &["ValueA", "ValueB"]),
    ("CompareStringAction", &["In"], &["E", "NE"], &["String_0", "String_1"]),
    ("CompleteCleanupMissionAction", &["CleanupComplete"], &["OnCleanupComplete"], &["MissionName"]),
    ("CompleteObjectiveAction", &["Complete"], &["OnComplete"], &["MissionName", "ObjectiveName"]),
    ("ConduitAction", &["Add", "Remove"], &[], &["Target"]),
    ("ConduitEventAction", &["In"], &["Out"], &["Reference", "Target", "Value"]),
    ("ConduitVfxToggleAction", &["Hide", "Toggle", "Unhide"], &["Out"], &["Target"]),
    ("ConfigMod", &[], &[], &["kNone"]),
    ("ConfigModAction", &["Add", "Check", "Remove"], &["Added", "Removed"], &[]),
    ("ConversationAction", &["Interrupt", "RequestStart", "Resume", "Stop"], &["OnConversationRestarted", "OnConversationStarted", "OnFinished", "OnInterrupted", "OnLineComplete", "OnStartRequested", "OnStop"], &["OnLineCompletedIndex", "Speaker1", "Speaker2", "Speaker3", "Speaker4"]),
    ("CopyBooleanAction", &["In"], &["Out"], &["Source", "Target"]),
    ("CopyVectorAction", &["In"], &["Out"], &["Source", "Target"]),
    ("CounterAction", &["Decrement", "Increment", "Reset"], &["OnDecrement", "OnIncrement", "OnReset", "Out"], &["Result"]),
    ("CustomDesignEventAction", &["SendEvent"], &["OnSendEvent"], &["Actor", "Bool", "EventName", "Number", "String", "Target", "Vector"]),
    ("CycleSignalAction", &["In", "Reset"], &["Out_0", "Out_1", "Out_2", "Out_3", "Out_4", "Out_5", "Out_6", "Out_7"], &[]),
    ("DamageAction", &["Damage"], &["Out"], &["Amount", "Credit", "HitPos", "OptSource", "Target"]),
    ("DamageSphereAction", &["Damage"], &["Out"], &["Amount", "CenterOffset", "Credit", "Radius", "Source"]),
    ("DeactivateCameraAction", &["In"], &["Out"], &["BlendTime", "Camera", "Hero", "NextLookAt"]),
    ("DebugAddAnimControllerAction", &["In"], &["Out"], &["Actor"]),
    ("DebugMessageAction", &["In"], &[], &["Actor", "Number", "String1", "String2", "Vector"]),
    ("DebugPrintToTTYAction", &["In"], &["Out"], &["PrintString"]),
    ("DebugReportAction", &["In"], &[], &["ActorVar", "BoolVar", "FloatVar", "StringVar", "VecVar"]),
    ("DebugResetTimeLocalAction", &["In"], &["Out"], &["ToSet"]),
    ("DebugScreenCaptureAction", &["CaptureInHDR", "CaptureNoHUD", "CaptureWithHUD"], &["Finished"], &[]),
    ("DebugSetAnimVarScalarAction", &["In"], &["Out"], &["Actor", "AnimVarName", "DriverName", "FloatVar"]),
    ("DebugSetFilterStateAction", &["In"], &["Out"], &["Actor", "Filter", "Group"]),
    ("DebugSetFixedFramerateAction", &["In"], &["Out"], &["Enabled"]),
    ("DebugSetToBindPoseAction", &["In"], &["Out"], &["ToSet"]),
    ("DebugWarningMessageAction", &["Check"], &["OnChecked", "OnTriggered"], &["Actor", "Number", "ShowWarning", "String1", "String2", "Vector"]),
    ("DelayAction", &["In"], &["Out"], &["Duration"]),
    ("DimensionalPulseAction", &["Trigger"], &["OnPulseFinished", "OnTriggered"], &["Source"]),
    ("DimensionalPulseTargetControlAction", &["Disable", "Enable"], &["OnDisabled", "OnEnabled"], &["Targets"]),
    ("DimensionalPulseTargetListenerAction", &["StartListening", "StopListening"], &["OnAffectingPulseEnded", "OnAffectingPulseHit", "OnAffectingPulseStarted", "OnStartListening", "OnStopListening"], &["Source", "Target", "TimeToHit"]),
    ("DimensionalPulseTargetSetTypeAction", &["Set"], &["OnSet"], &["Targets"]),
    ("DisableMapAction", &["Disable", "Enable"], &["OnDisable", "OnEnable", "Out"], &[]),
    ("DisableNextGameplayFadeInAction", &["In"], &["Out"], &[]),
    ("DivideAction", &["In"], &["Out"], &["Mod", "Result", "Value_0", "Value_1"]),
    ("DotProductAction", &["In"], &["Out"], &["Result", "VectorA", "VectorB"]),
    ("EmergentVOAction", &["Disable", "Enable"], &[], &[]),
    ("EnableCurveFollowerAutoStopAction", &["Disable", "Enable"], &["OnDisable", "OnEnable", "Out"], &["Actor"]),
    ("EndLevelDefeatAction", &["In"], &["Out"], &[]),
    ("EndLevelLobbyAction", &["In"], &["Out"], &[]),
    ("EndLevelMainMenuAction", &["In"], &["Out"], &[]),
    ("EndLevelVictoryAction", &["In"], &["Out"], &[]),
    ("ExponentAction", &["Calculate"], &["OnCalculate"], &["Base", "Exponent", "Result"]),
    ("ExpositionTokenAction", &["AnyQueuedUsers", "BlockTokenUsers", "CheckHaveToken", "ReleaseToken", "RequestToken", "UnblockTokenUsers"], &["HasQueuedUsers", "NoQueuedUsers", "OnBlockedTokenUsers", "OnCheckHasToken", "OnCheckNoToken", "OnRequestQueued", "OnTokenBlocked", "OnTokenReceived", "OnTokenReleased", "OnUnblockedTokenUsers"], &["TokenId"]),
    ("FallRespawnAction", &["Disable", "Enable"], &["OnDisabled", "OnEnabled"], &["Actors"]),
    ("FilterSignalAction", &["In"], &["Out"], &[]),
    ("FinaleBossChaseStartSequenceScriptAction", &["StartChase"], &["OnChaseStarted"], &["Boss", "ChaseTarget"]),
    ("FinaleBossMechHeartAllowLaserPhaseScriptAction", &["StartLaserPhase"], &["OnStartedLaserPhase"], &["HeartActor"]),
    ("FinaleBossMechHeartScriptAction", &["StartHeart"], &["OnDestroyed_AllNodes", "OnDestroyed_Chamber1_NodeA", "OnDestroyed_Chamber1_NodeB", "OnDestroyed_Chamber2_NodeA", "OnDestroyed_Chamber2_NodeB", "OnDestroyed_Chamber3_NodeA", "OnDestroyed_Chamber3_NodeB", "OnHeartStarted"], &["HeartActor", "MechActor"]),
    ("FlockControlScriptAction", &["Despawn", "FlyAwayAndDestroy", "Follow", "SetSpeed", "Spawn"], &["OnDespawn", "OnFlyAwayAndDestroy", "OnFollow", "OnSetSpeed", "OnSpawn"], &["Destination", "FlockVolume", "Speed"]),
    ("FollowCurveAction", &["ClearNode", "Start", "StopListening"], &["OnClearNode", "OnIntroEnd", "OnLooped", "OnReachedEnd", "OnStart", "OnStopListening"], &["Actor", "Curve"]),
    ("FollowCurveControlAction", &["Continue", "Stop"], &["OnContinue", "OnStop"], &["Actor"]),
    ("ForEachActorAction", &["First", "Next"], &["Done", "Out"], &["Actor", "ActorGroup"]),
    ("ForceActorCombatStateEngagedAction", &["Clear", "Set"], &["OnClear", "OnSet", "Out"], &["Actor"]),
    ("ForceGrindBoostAction", &["Clear", "Set"], &["OnClear", "OnSet", "Out"], &["Hero"]),
    ("FormatStringAction", &["In"], &["Out"], &["Actor", "Number", "String", "Target", "Vector"]),
    ("FullScreenMovieAction", &["Pause", "PauseAny", "Play", "Stop", "StopAny"], &["Done", "Out"], &["DisableAudioFade"]),
    ("GalacticMapGetTriggeredLandingCineAction", &["Check"], &["No", "OnLandFin", "Out", "Yes"], &[]),
    ("GalacticMapManualOpenMapAction", &["Trigger"], &["Out"], &["GalacMapActor"]),
    ("GalacticMapManualTriggerLandingAction", &["Trigger", "TriggerAndEndOnBlack"], &["Out"], &["GalacMapActor"]),
    ("GalacticMapSetPreMapMovieAction", &["Set"], &["Out"], &["GalacMapActor"]),
    ("GalacticMapSwitchToSetLevelAction", &["In"], &[], &["GalacMapActor"]),
    ("GateAction", &["Close", "In", "Open", "Toggle"], &["Out"], &[]),
    ("GatherAnimCluesAction", &["Gather"], &["Out"], &["Results", "Volume"]),
    ("GetActorGroupByNameAction", &["In"], &["Out"], &["ActorGroup", "String"]),
    ("GetActorInstanceInfoAction", &["In"], &["Out"], &["Actor", "ActorName", "AssetName", "ZoneName"]),
    ("GetActorsInVolumeScriptAction", &["Get"], &["OnGet"], &["ActorFilter", "ActorsFound", "ActorsToQuery"]),
    ("GetActorsWithAllegianceAction", &["Get"], &["OnGet"], &["ActorsWithAllegiance", "Group", "NumActorsWithAllegiance"]),
    ("GetAngleBetweenVectorsAction", &["In"], &["Out"], &["Facing_0", "Facing_1", "Result"]),
    ("GetCameraFOVAction", &["Get"], &["OnGet"], &["Camera", "FOV"]),
    ("GetClankPackActorAction", &["Get"], &["ClankPackFound", "NoClankPackFound"], &["ClankPackActor"]),
    ("GetClosestPointOnLineFromRayVectorAction", &["In"], &["Out"], &["Distance", "Point_0", "Point_1", "Ray_Start_Point", "Ray_Vector", "Result"]),
    ("GetClosestPointOnLineSegmentFromPointAction", &["In"], &["Out"], &["Distance", "Point", "Result", "SegmentEnd", "SegmentStart"]),
    ("GetCurrencyAction", &["In"], &["Out"], &["Amount", "Target"]),
    ("GetCurrentHeroShipAction", &["Check"], &["Micron", "None", "Ranger"], &[]),
    ("GetCurrentLevelIDAction", &["In"], &["Out"], &["LevelID"]),
    ("GetDifficultyAction", &["GetDifficulty"], &["Challenge", "EarlyGame", "Easy", "EndGame", "Hard", "Hardest", "MidGame", "Normal", "NotChallenge"], &["NumDeaths"]),
    ("GetEulerAnglesAction", &["Get"], &["OnGet"], &["Eulers", "Forward", "Left", "Up"]),
    ("GetHPAction", &["GetHP"], &["Out"], &["HPActual", "HPFractionZeroToOne", "HPMax", "Target"]),
    ("GetLevelIDAction", &["In"], &["Out"], &["LevelID"]),
    ("GetOrthogonalNormalVectorAction", &["In"], &["Out"], &["OptionalSecondary", "Primary", "Target"]),
    ("GetPuzzleRecallVolumesAction", &["GetActors"], &["OnGetActors"], &["Actors", "Target"]),
    ("GetPuzzleVolumeInfoAction", &["GetPuzzleInfo"], &["OnGetPuzzleInfo"], &["Accel", "Energy", "Heavy", "Light", "Target", "Volumes"]),
    ("GetSpaceshipMagnetStateAction", &["PingState"], &["Out"], &["CanPickupCount", "EnergyPercent", "TowCount", "TowPercent"]),
    ("GetStickInputAction", &["Get"], &["OnGet"], &["Hero", "StickDirection", "StickStrength"]),
    ("GetTeamScriptAction", &["Get"], &["OnGet"], &["Target", "TeamType"]),
    ("GetTimeSincePlayerEventAction", &["Aim", "Aim_And_Fire", "Fire", "FirstCameraMoveFromReload", "GameMap_Open", "Glide", "Hookshot", "Jetpack", "Jump_Double", "Jump_HeliHigh", "Jump_HeliLong", "Jump_Single", "Melee", "Melee_Slam", "Melee_Throw", "QuickSelect_Open", "QuickSelect_WeaponSwitch", "Strafe", "Strafe_And_Fire", "Swingshot"], &["Out"], &["Hero", "TimeSinceInput"]),
    ("GetTriggerInputAction", &["Get"], &["OnGet"], &["Hero", "LeftTriggerValue", "RightTriggerValue"]),
    ("GetUIActorAction", &["Get"], &["OnGet"], &["UIActor"]),
    ("GetVectorComponentsAction", &["In"], &["Out"], &["Vector", "X", "Y", "Z"]),
    ("GetVectorDirectionAction", &["In"], &["Out"], &["Position_0", "Position_1", "Result"]),
    ("GetVectorDistanceAction", &["In"], &["Out"], &["Position_0", "Position_1", "Result"]),
    ("GetVectorPositionAction", &["In"], &["Out"], &["Distance", "End_Position", "Facing_Actor", "Facing_Direction", "Starting_Position"]),
    ("GetXPAction", &["In"], &["Out"], &["LevelValue", "Target", "XpValue"]),
    ("GoldBoltCountAction", &["In"], &["Out"], &["Count"]),
    ("GrindPathFollowerAction", &["Start", "Stop"], &["OnStart", "OnStop"], &["GrindPathFollowerActor", "LedgePathActor", "Speed", "StartLocationActor", "StopLocationActor"]),
    ("HUDCommunicatorAction", &["Hide", "Show"], &["OnHide", "OnShow"], &[]),
    ("HUDControlAction", &["Add", "Remove"], &["OnAdd", "OnRemove"], &[]),
    ("HUDMessageAction", &["Clear", "Show"], &["Cleared"], &["Number", "String", "Viewer"]),
    ("HUDVisibilityAction", &["Hide", "Show"], &[], &["Viewer"]),
    ("HammerCrankSetStateAction", &["SetComplete", "SetLocked", "SetReady", "SetUnlocked"], &["OnComplete", "OnLocked", "OnReady", "OnUnlocked", "Out"], &["Actor"]),
    ("HasVisitedLevelAction", &["In"], &["NotVisitedBefore", "Out", "VisitedAlready"], &["OptLevelID"]),
    ("HealAction", &["Heal"], &["HealedSomeone", "Out"], &["Target", "Value"]),
    ("HealthBarAction", &["Add", "Remove"], &["OnAdded", "OnRemoved"], &["Target"]),
    ("HeroButtonPressListenerAction", &["StartListening", "StopListening"], &["OnButtonPressed", "OnStartListening", "OnStopListening"], &["Hero"]),
    ("HeroButtonReleaseListenerAction", &["StartListening", "StopListening"], &["OnButtonReleased", "OnStartListening", "OnStopListening"], &["Hero"]),
    ("HeroDeathDisableRespawnAction", &["Disable", "Enable"], &["DisabledOnDeathDone", "DisabledOnDeathStart", "OnDisable", "OnEnable", "Out"], &[]),
    ("HeroDefaultVanityLoadedAction", &[], &["OnLoaded"], &[]),
    ("HeroLockAction", &["LockCameraInput", "LockHeroInput", "LockHeroStand", "UnlockCamera", "UnlockHero"], &["OutLock", "OutUnlock"], &["Hero"]),
    ("HeroPlayScriptedAnimAction", &["Start", "Stop"], &["OnAnimComplete", "OnStart", "OnStop"], &["DriverName", "EndDestActor", "FaceAtActor", "RelativeActor", "Target"]),
    ("HeroTargetImmuneAction", &["DisableImmune", "SetImmune"], &["Out"], &["Hero"]),
    ("HeroWearBootsAction", &["BootsOff", "GrindbootsOn", "MagbootsOn"], &["OnBootsOff", "OnGrindbootsOn", "OnMagbootsOn", "Out"], &[]),
    ("IncomingDamageModifierAction", &["Add", "Remove"], &["Added", "Removed"], &["ModifierName", "Target"]),
    ("InputIdleAction", &["GetIdleTime", "ResetIdleTime", "StartTimer", "StopTimer"], &["OnGet", "OnMaxIdle", "OnReset", "OnStart", "OnStop", "OnWarnIdle"], &["IdleTime"]),
    ("InvertMatrixAction", &["Invert"], &["OnInvert"], &["Forward", "InvertForward", "InvertLeft", "InvertPosition", "InvertUp", "Left", "Position", "Up"]),
    ("IsActorCameraObstructedAction", &["In"], &["NotObstructed", "Obstructed"], &["Distance", "Hero", "Target"]),
    ("IsAliveAction", &["In"], &["False", "True"], &["Target"]),
    ("IsDemoAction", &["In"], &["IsDemo", "NotDemo"], &["DemoType"]),
    ("IsGoldBoltCollectedAction", &["In"], &["Out"], &["IsCollected"]),
    ("IsLevelUnlockedAction", &["Check"], &["IsLocked", "IsUnlocked"], &["LevelID"]),
    ("IsMissionReplayAction", &["Check"], &["IsFirstPlaythrough", "IsFullReplay", "IsImmediateReplay", "OnCheck"], &["MissionName"]),
    ("IsNewLevelAction", &["In"], &["NewLevel", "NotNewLevel", "Out"], &["IsNew"]),
    ("IsNumberInRangeAction", &["In"], &["False", "True"], &["ComparisonVariable", "Maximum", "Minimum"]),
    ("IsPlayerUsingCasualModeAction", &["Check"], &["IsCasual", "IsNotCasual"], &["Hero"]),
    ("IsPlayerUsingInvertedControlsAction", &["Check"], &["AnyInvertCam", "AnyNormalCam", "BothInvertCam", "BothNormalCam", "FlyerInvertX", "FlyerInvertY", "FlyerNormalX", "FlyerNormalY", "InvertCamX", "InvertCamY", "NormalCamX", "NormalCamY"], &["Hero"]),
    ("IsSwitchTriggeredAction", &["Check"], &["IsNotTriggered", "IsTriggered", "Out"], &["SwitchActor"]),
    ("IsVarConnectedAction", &["In"], &["Connected", "NoConnection", "Out"], &["Actor", "Bool", "Number", "String", "Vector"]),
    ("KillAction", &["Injure", "Kill"], &["Out"], &["Target"]),
    ("LandingBirdSystemAction", &["Disable", "Enable"], &["OnDisable", "OnEnable"], &[]),
    ("LedgeBirdLedgeControlAction", &["Disable", "Enable"], &["OnDisable", "OnEnable"], &["Ledges"]),
    ("LedgeBirdSystemAction", &["Disable", "Enable"], &["OnDisable", "OnEnable"], &[]),
    ("LerpFloatAction", &["Lerp"], &["OnLerp"], &["Float1", "Float2", "Result", "T_Value"]),
    ("LerpFloatOverTimeAction", &["Backward", "Forward", "Start", "Stop"], &["Done", "OnStart", "OnStop", "OnUpdate"], &["CurrentValue", "Duration", "From", "To"]),
    ("LerpVectorAction", &["Lerp"], &["OnLerp"], &["Result", "T_Value", "Vector1", "Vector2"]),
    ("LerpVectorOverTimeAction", &["Backward", "Forward", "Start", "Stop"], &["Done", "OnStart", "OnStop", "OnUpdate"], &["CurrentValue", "Duration", "From", "To"]),
    ("LoadLevelAction", &["In"], &[], &[]),
    ("LockMapIconAction", &["Lock", "Unlock"], &[], &[]),
    ("LockUIMissionAction", &["Lock", "Unlock"], &["OnLock", "OnUnlock"], &["Mission"]),
    ("LogarithmAction", &["Calculate"], &["OnCalculate"], &["Base", "Result", "Value"]),
    ("LoopSignalAction", &["StartLoop", "StopLoop"], &["LoopOutput", "OnStart", "OnStop"], &["CurrentLoopCount", "LoopCount", "LoopDelay"]),
    ("MaterialDefault", &["kCustomLightDiffuse", "kCustomLightSpecular", "kGeometryCurvature", "kGeometryNormal", "kLinearDepth", "kObjectLoD", "kPixelPosition", "kScreenUVs", "kUV0_x", "kUV0_xy", "kUV0_xyz", "kUV1_xy", "kVertexColor", "kVertexLoD"], &[], &[]),
    ("MaxMinAction", &["In"], &["Out"], &["MaxIndex", "MaxResult", "MinIndex", "MinResult", "Value0", "Value1", "Value2", "Value3", "Value4", "Value5", "Value6", "Value7"]),
    ("ModAction", &["In"], &["Out"], &["Result", "Value_0", "Value_1"]),
    ("MonitorPlayerIdlingAction", &["StartMonitoring", "StopMonitoring"], &["OnMove", "OnStopDidMove", "OnStopDidNotMove"], &[]),
    ("MonitorPlayerIdlingActionAction", &[], &[], &["Hero"]),
    ("MoveMapPOIAction", &["Move"], &["OnMoved"], &["Target"]),
    ("MultVectorByScalarAction", &["In"], &["Out"], &["Scalar", "Target", "Vector"]),
    ("MultiplyAction", &["In"], &["Out"], &["Result", "Value_0", "Value_1"]),
    ("MultiplyMatricesAction", &["Multiply"], &["OnMultiply"], &["Mat1Forward", "Mat1Left", "Mat1Position", "Mat1Up", "Mat2Forward", "Mat2Left", "Mat2Position", "Mat2Up", "ResultForward", "ResultLeft", "ResultPosition", "ResultUp"]),
    ("MusicIntensityTrackerScriptAction", &["Activate", "Deactivate"], &["OnHighIntensity", "OnLowIntensity", "OnMediumIntensity"], &["BlendedValue", "High", "Low", "Max", "RawValue"]),
    ("NormalizeValueToRangeAction", &["Normalize"], &["OnNormalize"], &["Max", "Min", "Result", "Value"]),
    ("NormalizeVectorAction", &["In"], &["Out"], &["OldLength", "Target", "Vector"]),
    ("NumberGenAction", &["Pause", "Reset", "Start"], &["Out"], &["Result"]),
    ("OnActorCombatStateChangedAction", &["StartListening", "StopListening"], &["OnActorEngaged", "OnActorPassive", "OnStartListening", "OnStopListening"], &["ActorGroup", "TriggeringActor"]),
    ("OnBlockAction", &["Activate", "Deactivate"], &["OnBlock"], &["Counter", "DamagerActor", "Target", "VictimActor"]),
    ("OnBooleanAction", &["StartListening", "StopListening"], &["OnChange", "OnFalse", "OnTrue"], &["Target"]),
    ("OnBouncedAction", &[], &["OnBounced"], &["Instigator", "Target"]),
    ("OnClankPuzzleLockOnStatusChangeAction", &["StartListening", "StopListening"], &["OnEmpty", "OnFull", "OnStartListening", "OnStopListening"], &["LockOn", "OrbActor"]),
    ("OnClankPuzzleSphereDeployedAction", &["StartListening", "StopListening"], &["OnLanded", "OnStartListening", "OnStopListening", "OnThrown"], &["Deployer"]),
    ("OnClankPuzzleVolumeEndAction", &["StartListening", "StopListening"], &["OnComplete"], &["PuzzleVolumeEnd"]),
    ("OnCustomDesignEventAction", &["StartListening", "StopListening"], &["OnReceiveEvent"], &["Actor", "Bool", "EventName", "FilterActor", "ListenEventName", "Number", "Receiver", "Sender", "String", "Target", "Vector"]),
    ("OnDamageCounterAction", &["StartCounting", "StopCounting"], &["OnDamageReached"], &["Target"]),
    ("OnDamagedAction", &["StartListening", "StopListening"], &["OnDamaged"], &["DamageAmount", "DamageType", "Damager", "HitWeakspot", "Instigator", "Target", "WeaponType"]),
    ("OnDeadAction", &["StartListening", "StopListening"], &["OnDead"], &["DamageType", "Damager", "HitWeakspot", "Instigator", "Target", "WeaponName", "WeaponType"]),
    ("OnDropObjectAction", &[], &["Out"], &["Dropper", "Object"]),
    ("OnEnterVolumeAction", &["Activate", "Deactivate"], &["OnEmpty", "OnEnter", "OnFull", "OnLeave"], &["Exited", "Inside", "Instigator", "Locations", "Targets", "Volume"]),
    ("OnFrameEventAction", &["In", "Stop"], &["OnStop", "Out", "PostStop"], &["DeltaTime", "DeltaTimeReal"]),
    ("OnHPThresholdAction", &[], &["OnHPThreshold"], &["HPFraction", "HPIndex", "Target"]),
    ("OnHeroRespawnAction", &["Activate", "Deactivate"], &["OnActivate", "OnDeactivate", "OnRespawn"], &[]),
    ("OnHostMigratedAction", &[], &["Migrated"], &[]),
    ("OnInjuredAction", &[], &["OnInjured"], &["Damager", "Instigator", "Target"]),
    ("OnKilledSomethingAction", &["StartListening", "StopListening"], &["KilledEnemy", "KilledNonEnemy", "KilledSomething"], &["KillerActor", "OptTargetKillers", "VictimActor"]),
    ("OnLobbyRTTEventAction", &["StartListening", "StopListening"], &["OnNewGame"], &["RTTActor"]),
    ("OnLookAtActorAction", &["Activate", "Deactivate"], &["OnLookAt", "OnLookAway"], &["Actor", "Hero"]),
    ("OnLookAtActorByAngleAction", &["Activate", "Deactivate"], &["OnLookAt", "OnLookAway"], &["Actor", "AngleOffset", "Hero", "LookedAtActor", "LookedAwayActor", "MaxDistance"]),
    ("OnLookAtEachActorAction", &["Activate", "Deactivate"], &["OnLookAt", "OnLookAway"], &["Actor", "Hero", "LookedAtActor", "LookedAwayActor"]),
    ("OnMissionStateChangeEventAction", &[], &["Active", "Available", "CompleteCleaning", "CompleteFinished", "Failed", "Inactive"], &["MissionName"]),
    ("OnObjectiveStateChangeEventAction", &[], &["Active", "Complete", "Failed", "Inactive"], &["MissionName", "ObjectiveName"]),
    ("OnPlayerEventAction", &["StartListening", "StopListening"], &["Aim", "Aim_And_Fire", "Fire", "FirstCameraMoveFromReload", "GameMap_Open", "Glide", "Hookshot", "Jetpack", "Jump_Double", "Jump_HeliHigh", "Jump_HeliLong", "Jump_Single", "Melee", "Melee_Slam", "Melee_Throw", "QuickSelect_Open", "QuickSelect_WeaponSwitch", "Strafe", "Strafe_And_Fire", "Swingshot"], &["Hero"]),
    ("OnProjectileHitAction", &["StartListening", "StopListening"], &["OnHit"], &["HitActor", "ProjectileActor", "TargetActors"]),
    ("OnReceiveEventAction", &["StartListening", "StopListening"], &["OnReceiveEvent"], &["FilterActor", "Receiver", "Sender", "Target"]),
    ("OnShotSpawnedEventAction", &["StartListening", "StopListening"], &["OnShotSpawned"], &["AllShots", "LastShot", "Target"]),
    ("OnSpaceshipReticleTargetChangeAction", &["StartListen", "StopListen"], &["OnNeutral", "OnTarget", "OnTargetSwitch"], &["TargetActor"]),
    ("OnStringMessageAction", &["StartListening", "StopListening"], &["OnTriggered"], &["Instigator", "ListenTo", "Message"]),
    ("OnThrowObjectAction", &[], &["Out"], &["Object", "Thrower"]),
    ("OnTriggerAction", &["StartListening", "StopListening"], &["OnAllTriggered", "OnAllUnTriggered", "OnAnyTrigger", "OnAnyUnTrigger"], &["Actor", "LastTriggeredActor", "LastUnTriggeredActor"]),
    ("OnValueChangedEventAction", &[], &["OnValueChanged"], &["Instigator", "Target", "Value"]),
    ("OnVendorRTTEventAction", &["StartListening", "StopListening"], &["OnAccept", "OnCancel", "OnError", "OnPurchase"], &["PurchasedWeapon", "RTTActor", "ShowRemapTutorial"]),
    ("OnWorldCombatStateChangedAction", &["StartListening", "StopListening"], &["OnStartListening", "OnStopListening", "OnWorldEngaged", "OnWorldPassive"], &[]),
    ("OrderedListArrangeAction", &["Shuffle"], &["OnShuffle"], &["List"]),
    ("OrderedListBuildAction", &["AddToEnd", "AddToStart", "ClearList", "InitList", "InsertAt", "ReplaceAt"], &["Added", "Cleared", "Initialized", "Replaced"], &["ActorItem", "BoolItem", "InsertIndex", "ItemCount", "List", "NumberItem", "StringItem", "VectorItem"]),
    ("OrderedListGetCountAction", &["Get"], &["IsEmpty", "OnGet"], &["Count", "List"]),
    ("OrderedListGetIndexAction", &["Lookup"], &["Found", "NotFound"], &["ItemIndex", "List", "TargetActor", "TargetBool", "TargetNumber", "TargetString", "TargetVector"]),
    ("OrderedListGetItemAction", &["Lookup"], &["Found", "NotFound"], &["ActorOut", "BoolOut", "ItemIndex", "List", "NumberOut", "StringOut", "VectorOut"]),
    ("OrderedListIterateAction", &["First", "Next"], &["ItemFound", "OnFirst", "OnLast", "OnNext", "PastEnd"], &["ActorOut", "BoolOut", "List", "ListIndex", "NumberOut", "StringOut", "VectorOut"]),
    ("OrderedListRemoveAction", &["RemoveFromEnd", "RemoveFromIndex", "RemoveFromStart"], &["EmptyList", "Removed"], &["ItemCount", "List", "RemovalIndex", "RemovedActor", "RemovedBool", "RemovedNumber", "RemovedString", "RemovedVector"]),
    ("OrderedListUIAction", &["Focus", "Hide", "Show", "UnFocus"], &["OnFocus", "OnHide", "OnItemSelected", "OnShow", "OnUnFocus"], &["List", "SelectedActor", "SelectedBool", "SelectedIndex", "SelectedLabel", "SelectedNumber", "SelectedPosition", "SelectedString", "SelectedVector", "WindowPosition", "WindowSize"]),
    ("OutgoingDamageModifierAction", &["Add", "Remove"], &["Added", "Removed"], &["ModifierName", "Target"]),
    ("OverlayAlertAction", &["Hide", "Show"], &["OnAccept", "OnCancel", "OnHide", "OnShow"], &["Viewer"]),
    ("OverlayTextAction", &["Clear", "Show"], &["Cleared", "OnShow"], &[]),
    ("OverrideBoltShowerSettingsAction", &["Apply"], &["Out"], &["BoltShowerActor", "RewardValue", "ShowerDuration", "ShowerSpawnCount"]),
    ("OverrideGrindConfigAction", &["Clear", "Set"], &["OnClear", "OnSet", "Out"], &["Hero"]),
    ("OverrideHeroBackpackAction", &["ClearOverride", "Override"], &["OnClearOverride", "OnOverride"], &[]),
    ("OverrideHeroSpaceshipSpeedAction", &["ResetToDefaults", "Set"], &["OnResetToDefaults", "OnSet", "Out"], &[]),
    ("OverrideHeroSpaceshipTurnRateAction", &["ResetToDefaults", "Set"], &["OnResetToDefaults", "OnSet", "Out"], &[]),
    ("OverrideHoverbootConfigAction", &["Clear", "Set"], &["OnClear", "OnSet", "Out"], &["Hero"]),
    ("OverridePlanetLandingCheckpointAction", &["Clear", "Set"], &["OnClear", "OnSet"], &["Checkpoint", "PlanetName"]),
    ("OverrideSlideConfigAction", &["Clear", "Set"], &["OnClear", "OnSet", "Out"], &["Hero"]),
    ("PassThruAction", &["In"], &["Out"], &[]),
    ("PauseMapForceOfflineAction", &["Clear", "ForceOffline"], &["OnClear", "OnForceOffline"], &[]),
    ("PerfSpecGetAction", &["Get"], &["Fidelity", "OnGet", "Performance"], &[]),
    ("PeriodicAction", &["Reset", "Start", "Stop"], &["OnReset", "OnStart", "OnStop", "OnTimeOut"], &["RemainingTime", "ResetTime"]),
    ("PhotoModeControlAction", &["Disable", "Enable"], &["OnDisabled", "OnEnabled"], &[]),
    ("PlanetMenuCheckpointWarpAction", &["Warp"], &["OnWarp"], &["CheckpointName", "Hero"]),
    ("PlatformSetCurveAction", &["In"], &["Out"], &["Platform"]),
    ("PlayAnimAction", &["Start"], &["OnAllDone", "OnFirstDone", "Started"], &["Anim", "Target"]),
    ("PlayCreditsAction", &["Play"], &["Out"], &[]),
    ("PostFXFor3DUIAction", &["Disable", "Enable"], &["OnDisabled", "OnEnabled"], &[]),
    ("PressStartLogoAction", &["Show"], &["OnStartPressed"], &[]),
    ("PushAnimSetAction", &["PushAnimSet", "RemoveAnimSet"], &["Pushed", "Removed"], &["Target"]),
    ("PuzzleBotRunDirectionAction", &["SetDirection"], &["OnSetDirection"], &["Direction", "Target"]),
    ("PuzzleTargetLatestVolumeAction", &["GetElement"], &["OnGetAccel", "OnGetHeavy", "OnGetLight", "OnGetNone"], &["Target"]),
    ("QueryAnimVarAction", &["Query"], &["OnQuery"], &["Actor", "Value", "VarName"]),
    ("RandomNumberAction", &["In"], &["Out"], &["Max", "Min", "Result"]),
    ("RandomSignalAction", &["In"], &["Out_0", "Out_1", "Out_2", "Out_3", "Out_4", "Out_5", "Out_6", "Out_7"], &[]),
    ("RecallPuzzleVolumesAction", &["Recall"], &["OnRecall"], &["Target", "Volumes"]),
    ("RemovePuzzleVolumeDataFromTargetAction", &["Remove"], &["OnRemove"], &["Target"]),
    ("ReplayMissionAction", &["Replay"], &["OnReplay"], &["MissionName"]),
    ("RequestHeroRespawnAction", &["Respawn"], &[], &["Hero"]),
    ("ResurrectAction", &["Resurrect"], &["Out"], &["Target"]),
    ("ReverseCurveFollowerAction", &["Reverse"], &["Out"], &["CurveFollower"]),
    ("RotateByEulerAnglesAction", &["Rotate"], &["OnRotate"], &["Eulers", "Position", "RotatedPosition"]),
    ("RotateByMatrixAction", &["Rotate"], &["OnRotate"], &["Forward", "Left", "Position", "RotatedPosition", "Up"]),
    ("RotateVectorAroundAxisAction", &["Rotate"], &["OnRotate"], &["Axis", "Result", "Rotation", "Vec"]),
    ("RotateVectorAroundYAction", &["In"], &["Out"], &["Facing_Vec", "Result_Vec", "Y_Rotation"]),
    ("SargassoCentipedeMineFiringAction", &["Activate", "Deactivate"], &["Failure", "Success"], &["Centipede"]),
    ("ScaleActorAction", &["ApplyScale"], &["OnApplyScale"], &["Actor", "Scale"]),
    ("ScriptedBirdControlAction", &["Despawn", "Flee"], &["OnDespawn", "OnFlee"], &["Flock"]),
    ("ScriptedDevstatsEventAction", &["Send"], &["OnSent"], &["EventType", "Num1", "Num2", "String1", "String2"]),
    ("SendAnimDamageEventAction", &["Send"], &["OnSend"], &["Target"]),
    ("SendEventAction", &["SendEvent"], &["Sent"], &["From", "To"]),
    ("SetActorAction", &["In"], &["Out"], &["Target", "Value"]),
    ("SetActorGroupAction", &["In"], &["Out"], &["Target", "Value"]),
    ("SetAdditionalAttackersAction", &["Clear", "Set"], &["Out"], &["AdditonalAttackers", "Hero"]),
    ("SetBooleanAction", &["False", "Toggle", "True"], &["Out"], &["Target"]),
    ("SetCameraFOVAction", &["Set"], &["OnSet"], &["Camera", "FOV"]),
    ("SetConfigAction", &["In"], &["Out"], &[]),
    ("SetCurrencyAction", &["In"], &["Out"], &["Result", "Target", "Value_0"]),
    ("SetCurveAction", &["Set"], &["Done"], &["Actor", "Curve", "OverlapEnd"]),
    ("SetDamageInvulnerableAction", &["Off", "On"], &["Done"], &["Target"]),
    ("SetDamageMultiplierAction", &["ResetToOne", "SetMultiplier"], &["Done"], &["Multiplier", "Target"]),
    ("SetDamagedOnlyByHeroAction", &["Off", "On"], &["Done"], &["Target"]),
    ("SetDifficultyAction", &["EarlyGame", "Easy", "EndGame", "Hard", "Hardest", "MidGame", "Normal"], &[], &[]),
    ("SetFlounderingScriptAction", &["SetFalse", "SetTrue"], &["Out"], &["Bot"]),
    ("SetHeroBackpackAction", &["Set"], &["Out"], &[]),
    ("SetHeroCurrentShipAction", &["Set"], &["Out"], &[]),
    ("SetHeroHackedShipAction", &["Set"], &["Out"], &["Curve", "Hero"]),
    ("SetHeroMaskAction", &["Set"], &["Out"], &["Hero"]),
    ("SetHeroRailShooterAction", &["Set"], &["Out"], &["Curve", "Hero"]),
    ("SetHeroSpaceshipFlighConfigAction", &["Set"], &["Out"], &["Hero"]),
    ("SetHeroSpaceshipInputAction", &["DisableGun", "DisableMissiles", "DisableWeapons", "EnableGun", "EnableMissiles", "EnableWeapons"], &["GunDisabled", "GunEnabled", "MissilesDisabled", "MissilesEnabled", "WeaponsDisabled", "WeaponsEnabled"], &["Hero"]),
    ("SetHeroSpaceshipMagnetModeAction", &["EnterMagnetMode", "LeaveMagnetMode"], &["OnEnter", "OnLeave", "OnMaxRuns", "OnTargetDead", "Out"], &["TargetShip", "TowGroup"]),
    ("SetHeroSpaceshipMissileCountAction", &["Set"], &["Out"], &["Hero"]),
    ("SetLevelOverrideAction", &["In"], &["Out"], &[]),
    ("SetLootModeAction", &["In"], &["Out"], &["Mode"]),
    ("SetMissionActiveAction", &["Start"], &["OnStart"], &["MissionName"]),
    ("SetMissionFailedAction", &["Fail"], &["OnFailed"], &["Mission", "ReasonTag"]),
    ("SetNumberAction", &["In"], &["Out"], &["Target", "Value"]),
    ("SetObjectiveFailedAction", &["Fail"], &["OnFailed"], &["Mission", "Objective", "ReasonTag"]),
    ("SetStringAction", &["In"], &["Out"], &["Target", "Value"]),
    ("SetTargetTypeScriptAction", &["Set"], &["OnSet"], &["Targets"]),
    ("SetTeamScriptAction", &["Set", "Toggle"], &["OnSet", "OnToggle"], &["Source", "Target"]),
    ("SetTrampolineParamsAction", &["Set"], &["OnSet"], &["Enabled", "MaxHeight", "MinHeight", "ReflectionFactor", "Trampoline"]),
    ("SetVectorAction", &["In"], &["Out"], &["Target", "X", "Y", "Z"]),
    ("SetXPAction", &["In"], &["Out"], &["Target", "XPValue"]),
    ("SetupControllerAction", &["DecrementSpawnCount", "GetSpawnCountRemaing", "ManualReset"], &["NoSpawnsRemaining", "OnDecrementdSpawnCount", "OnManualReset", "SpawnCountRemaining"], &["Factory", "SpawnCount"]),
    ("ShowMovieLocationTextAction", &["Clear", "Show"], &["OnClearInput", "OnShowInput", "Out"], &[]),
    ("SignalRelayAction", &["In"], &["Out"], &[]),
    ("SkuFilterAction", &["In"], &["Checked", "Unchecked"], &[]),
    ("SlerpVectorAction", &["Slerp"], &["OnSlerp"], &["Result", "T_Value", "Vector1", "Vector2"]),
    ("SolveQuadraticEquationAction", &["Solve"], &["OnOneResult", "OnTwoResults", "OnZeroResults"], &["A", "B", "C", "Result1", "Result2"]),
    ("SpawnRewardAction", &["In"], &["Out"], &["Target"]),
    ("SpawnScriptedBirdAction", &["Spawn"], &["OnSpawn", "OnSpawnFinished"], &["BirdTemplate", "Flock", "FlyInStartVolume", "LandedVolumeOrCurve"]),
    ("SpawnerScriptAction", &["ForceSpawn", "Pause", "Reset", "Resume", "Start", "Stop"], &["OnAllDead", "OnAllSpawned", "OnNumDead", "OnReset", "OnSpawned", "Paused", "Resumed", "Started", "Stopped"], &["Factories", "LastSpawn", "Location", "MaxAlive", "NumSpawned", "NumSpawns", "SpawnIntoGroup", "Template"]),
    ("StructuredLoggingFinishTestAction", &["In"], &["Out"], &["TestName"]),
    ("SubCurrencyAction", &["In"], &["Out"], &["Result", "Target", "Value_0"]),
    ("SubtractAction", &["In"], &["Out"], &["Result", "Value_0", "Value_1"]),
    ("SubtractVectorsAction", &["In"], &["Out"], &["Target", "VectorA", "VectorB"]),
    ("SwitchLevelAction", &["In"], &[], &[]),
    ("SwitchSignalAction", &["In"], &["Out_0", "Out_1", "Out_2", "Out_3", "Out_4", "Out_5", "Out_6", "Out_7", "Out_OutOfRange"], &["Select"]),
    ("SyncValueAction", &["Add", "Divide", "Get", "Multiply", "Reset", "Set", "Subtract"], &["Out"], &["Target", "ValueIn", "ValueOut"]),
    ("TimerAction", &["Reset", "Start", "Stop"], &["OnReset", "OnStart", "OnStop", "OnTimeOut"], &["StartTime", "TimeRemaining"]),
    ("ToggleHeroMovesAction", &["In"], &["Out"], &["Hero"]),
    ("TogglePortalRespawnSystemScriptAction", &["DisablePortalRespawn", "EnablePortalRespawn"], &["OnPortalRespawnDisabled", "OnPortalRespawnEnabled"], &[]),
    ("ToggleUsabilityControlsAction", &["In"], &["Out"], &["Val"]),
    ("ToggleUsabilityMovementAction", &["In"], &["Out"], &["Val"]),
    ("TriggerActorAction", &["Trigger", "UnTrigger"], &["Out", "OutFromTrigger", "OutFromUnTrigger"], &["Actor", "Silent"]),
    ("TriggerEmergentDialogAction", &["Trigger"], &[], &["Target"]),
    ("TrigonometryAction", &["Cosine", "Sine", "Tangent"], &["Out"], &["Degrees", "Value"]),
    ("TrigonometryInverseAction", &["Arccosine", "Arcsine", "Arctangent"], &["Out"], &["Degrees", "Value"]),
    ("UIArenaRewardAction", &["Show"], &["OnShow"], &[]),
    ("UIArenaWaveAction", &["ShowWaveComplete", "ShowWaveCountdown"], &["OnShowWaveComplete", "OnShowWaveCountdown"], &["CountdownValue", "Wave"]),
    ("UICountDownAction", &["GetCurrentTime", "Hide", "SetNewTimeAndStart", "Show", "Start", "Stop", "StopAndSetNewTime"], &["OnFinished", "OnOtherStopped", "OnSelfStopped"], &["CurrentTime", "NewTime", "Viewer"]),
    ("UICounterAction", &["Hide", "UpdateShow"], &[], &["CurrentValue", "MaxValue"]),
    ("UIExitFinaleAction", &["Disable", "Enable"], &["OnExitFinale"], &[]),
    ("UIMapSectionAction", &["SetUnvisited", "SetVisited"], &["OnUnvisited", "OnVisited"], &["SectionActor"]),
    ("UIObjectiveAction", &["Show"], &["OnShow"], &["Mission", "Objective"]),
    ("UIProgressBarAction", &["Hide", "Show"], &["OnHide", "OnShow"], &["BreakableActor", "Denominator", "Numerator", "PercentToShow"]),
    ("UIRaceEndScreenAction", &["Open"], &["OnClosed", "OnOpened"], &[]),
    ("UIRaceStartScreenAction", &["Open"], &["OnChoseBronze", "OnChoseGold", "OnChoseSilver", "OnClosed", "OnOpened"], &[]),
    ("UIShowReticleAction", &["Hide", "Show"], &["OnHide", "OnShow"], &[]),
    ("UISkipMovieAction", &["Disable", "Enable"], &["OnSkipMovie"], &[]),
    ("UISkipPuzzleAction", &["Disable", "Enable"], &["OnSkipPuzzle"], &[]),
    ("UIUpSellScreenAction", &["Open"], &["OnClosed", "OnOpened", "OnPreClose"], &[]),
    ("UnlockLevelAction", &["Lock", "Unlock"], &["OnLock", "OnUnlock", "Out"], &["LevelID"]),
    ("UseLetterboxAction", &["Disable", "Enable"], &["OnDisable", "OnEnable", "Out"], &["HideTime", "ShowTime"]),
    ("VanityEquipBundleAction", &["Start"], &["False", "True"], &["VanityActor"]),
    ("VanityEquipSetAction", &["Start"], &["False", "True"], &["VanityActor"]),
    ("VanityGiveBundleAction", &["Start"], &["False", "True"], &[]),
    ("VanityGiveSetAction", &["Start"], &["False", "True"], &[]),
    ("VanityHasBundleAction", &["Start"], &["False", "True"], &[]),
    ("VanityHasSetAction", &["Start"], &["False", "True"], &[]),
    ("VanityIsBundleEquippedAction", &["Start"], &["False", "True"], &["VanityActor"]),
    ("VanityIsSetEquippedAction", &["Start"], &["False", "True"], &["VanityActor"]),
    ("VendorRTTAction", &["ForceGlovePartialOff"], &["OnForceGlovePartialOff"], &["ShouldForceGlovePartialOff"]),
    ("WaitForCheckpointWarpEndAction", &["Wait"], &["OnNotWarping", "OnWait"], &[]),
];

/// `(node type, var plug name, authored direction)` from the `label("kFloat in")`
/// / `label("kFloat out")` annotations on the var selects, 448 rows, sorted.
/// Used only to cross-check the direction word the file stores.
pub static NODE_VAR_DIRS: &[(&str, &str, &str)] = &[
    ("AccelLerpVecAction", "Accel", "In"),
    ("AccelLerpVecAction", "Decel", "In"),
    ("AccelLerpVecAction", "MaxSpeed", "In"),
    ("AccelLerpVecAction", "Result", "Out"),
    ("AccelLerpVecAction", "StartPos", "In"),
    ("AccelLerpVecAction", "TargetPos", "In"),
    ("ActorActivationQueryAction", "Target", "In"),
    ("ActorBoundsAction", "Actor", "In"),
    ("ActorBoundsAction", "Center", "Out"),
    ("ActorBoundsAction", "Extents", "Out"),
    ("ActorBoundsAction", "Radius", "Out"),
    ("ActorControlAction", "Target", "In"),
    ("ActorCountAction", "ActorCount", "Out"),
    ("ActorCountAction", "ActorGroup", "In"),
    ("ActorDrawAction", "Actor", "In"),
    ("ActorGroupClearAction", "Group", "In"),
    ("ActorGroupControlAction", "Actors", "In"),
    ("ActorGroupControlAction", "Group", "In"),
    ("ActorPickAction", "Group", "In"),
    ("ActorPickAction", "Pick", "Out"),
    ("ActorPickAction", "PickIntoGroup", "Out"),
    ("ActorPickAction", "Reference", "In"),
    ("ActorPickAction", "Value", "In"),
    ("ActorPositionAction", "Actor", "In"),
    ("ActorPositionAction", "Facing", "Out"),
    ("ActorPositionAction", "Left", "Out"),
    ("ActorPositionAction", "LocatorName", "In"),
    ("ActorPositionAction", "Position", "Out"),
    ("ActorPositionAction", "Up", "Out"),
    ("ActorToggleLookAction", "Actor", "In"),
    ("ActorToggleLookGroupAction", "Actor", "In"),
    ("ActorVolumeAction", "Actors", "In"),
    ("ActorVolumeAction", "LocationResults", "Out"),
    ("ActorVolumeAction", "Locations", "In"),
    ("ActorVolumeAction", "Results", "Out"),
    ("ActorWarpAction", "Actors", "In"),
    ("ActorWarpAction", "Destination", "In"),
    ("ActorWarpAction", "DestinationVec", "In"),
    ("ActorWarpAction", "Facing", "In"),
    ("ActorWarpAction", "FacingVec", "In"),
    ("ActorWarpAction", "LookTarget", "In"),
    ("ActorWarpAction", "OffsetRotationX", "In"),
    ("ActorWarpAction", "OffsetRotationY", "In"),
    ("ActorWarpAction", "OffsetRotationZ", "In"),
    ("ActorWarpAction", "OffsetVec", "In"),
    ("ActorWarpAction", "Source", "In"),
    ("ActorWarpAction", "UpVec", "In"),
    ("AddAction", "Result", "Out"),
    ("AddAction", "Value_0", "In"),
    ("AddAction", "Value_1", "In"),
    ("AddAction", "Value_2", "In"),
    ("AddAction", "Value_3", "In"),
    ("AddAction", "Value_4", "In"),
    ("AddAction", "Value_5", "In"),
    ("AddAction", "Value_6", "In"),
    ("AddAction", "Value_7", "In"),
    ("AddCurrencyAction", "Result", "Out"),
    ("AddCurrencyAction", "Target", "In"),
    ("AddCurrencyAction", "Value_0", "In"),
    ("AddVectorsAction", "Target", "Out"),
    ("AddVectorsAction", "VectorA", "In"),
    ("AddVectorsAction", "VectorB", "In"),
    ("AddVectorsAction", "VectorC", "In"),
    ("AddVectorsAction", "VectorD", "In"),
    ("AddVectorsAction", "VectorE", "In"),
    ("AddVectorsAction", "VectorF", "In"),
    ("AddVectorsAction", "VectorG", "In"),
    ("AddVectorsAction", "VectorH", "In"),
    ("AddXPAction", "Target", "In"),
    ("AddXPAction", "XPValue", "In"),
    ("AmbientBarrageControlAction", "AmbientShooterGroup", "In"),
    ("AmbientBarrageControlAction", "Controller", "In"),
    ("AmbientBarrageControlAction", "Targets", "In"),
    ("ApplyEaseAction", "Float1", "In"),
    ("ApplyEaseAction", "Float2", "In"),
    ("ApplyEaseAction", "Result", "Out"),
    ("ApplyEaseAction", "T_Value", "In"),
    ("BitFlagsAction", "BitIndex", "In"),
    ("BitFlagsAction", "InputFlags", "In"),
    ("BitFlagsAction", "OutputFlags", "Out"),
    ("CharacterSwapAction", "Hero", "In"),
    ("CheckHeroTypeAction", "HeroActor", "In"),
    ("CheckpointWarpAction", "CheckpointName", "In"),
    ("CheckpointWarpAction", "DimensionCheckpointName", "In"),
    ("CheckpointWarpAction", "Hero", "In"),
    ("Cinematic2Action", "ActorA", "In"),
    ("Cinematic2Action", "ActorB", "In"),
    ("Cinematic2Action", "ActorC", "In"),
    ("Cinematic2Action", "ActorD", "In"),
    ("Cinematic2Action", "ActorE", "In"),
    ("Cinematic2Action", "ActorF", "In"),
    ("Cinematic2Action", "ActorG", "In"),
    ("Cinematic2Action", "ActorH", "In"),
    ("Cinematic2Action", "Cinematic2", "In"),
    ("Cinematic2ActorAction", "Actors", "In"),
    ("Cinematic2ListenerAction", "Cinematic2", "In"),
    ("Cinematic2RobotestAction", "Cinematic2", "In"),
    ("Cinematic2RobotestAction", "TestName", "In"),
    ("Cinematic2TriggerEventOnActorAction", "ActorName", "In"),
    ("Cinematic2TriggerEventOnActorAction", "Cinematic2", "In"),
    ("CinematicPlayerStateAction", "Hero", "In"),
    ("ClampAction", "Max", "In"),
    ("ClampAction", "Min", "In"),
    ("ClampAction", "Result", "Out"),
    ("ClampAction", "Value", "In"),
    ("CompareActorAction", "ActorA", "In"),
    ("CompareActorAction", "ActorB", "In"),
    ("CompareActorGroupAction", "ActorA", "In"),
    ("CompareActorGroupAction", "ActorB", "In"),
    ("CompareActorGroupAction", "IntersectCount", "Out"),
    ("CompareActorGroupAction", "UnionCount", "Out"),
    ("CompareBooleanAction", "Value_0", "In"),
    ("CompareBooleanAction", "Value_1", "In"),
    ("CompareBooleanAction", "Value_2", "In"),
    ("CompareBooleanAction", "Value_3", "In"),
    ("CompareBooleanAction", "Value_4", "In"),
    ("CompareBooleanAction", "Value_5", "In"),
    ("CompareBooleanAction", "Value_6", "In"),
    ("CompareBooleanAction", "Value_7", "In"),
    ("CompareNumberAction", "ValueA", "In"),
    ("CompareNumberAction", "ValueB", "In"),
    ("CompareStringAction", "String_0", "In"),
    ("CompareStringAction", "String_1", "In"),
    ("CopyBooleanAction", "Source", "In"),
    ("CopyBooleanAction", "Target", "Out"),
    ("CopyVectorAction", "Source", "In"),
    ("CopyVectorAction", "Target", "Out"),
    ("CounterAction", "Result", "Out"),
    ("DebugAddAnimControllerAction", "Actor", "In"),
    ("DebugMessageAction", "Actor", "In"),
    ("DebugMessageAction", "Number", "In"),
    ("DebugMessageAction", "String1", "In"),
    ("DebugMessageAction", "String2", "In"),
    ("DebugMessageAction", "Vector", "In"),
    ("DebugPrintToTTYAction", "PrintString", "In"),
    ("DebugReportAction", "ActorVar", "In"),
    ("DebugReportAction", "BoolVar", "In"),
    ("DebugReportAction", "FloatVar", "In"),
    ("DebugReportAction", "StringVar", "In"),
    ("DebugReportAction", "VecVar", "In"),
    ("DebugResetTimeLocalAction", "ToSet", "In"),
    ("DebugSetAnimVarScalarAction", "Actor", "In"),
    ("DebugSetAnimVarScalarAction", "AnimVarName", "In"),
    ("DebugSetAnimVarScalarAction", "DriverName", "In"),
    ("DebugSetAnimVarScalarAction", "FloatVar", "In"),
    ("DebugSetFilterStateAction", "Actor", "In"),
    ("DebugSetFilterStateAction", "Filter", "In"),
    ("DebugSetFilterStateAction", "Group", "In"),
    ("DebugSetFixedFramerateAction", "Enabled", "In"),
    ("DebugSetToBindPoseAction", "ToSet", "In"),
    ("DebugWarningMessageAction", "Actor", "In"),
    ("DebugWarningMessageAction", "Number", "In"),
    ("DebugWarningMessageAction", "ShowWarning", "In"),
    ("DebugWarningMessageAction", "String1", "In"),
    ("DebugWarningMessageAction", "String2", "In"),
    ("DebugWarningMessageAction", "Vector", "In"),
    ("DelayAction", "Duration", "In"),
    ("DivideAction", "Mod", "Out"),
    ("DivideAction", "Result", "Out"),
    ("DivideAction", "Value_0", "In"),
    ("DivideAction", "Value_1", "In"),
    ("DotProductAction", "Result", "Out"),
    ("DotProductAction", "VectorA", "In"),
    ("DotProductAction", "VectorB", "In"),
    ("ExponentAction", "Base", "In"),
    ("ExponentAction", "Exponent", "In"),
    ("ExponentAction", "Result", "Out"),
    ("FallRespawnAction", "Actors", "In"),
    ("ForEachActorAction", "Actor", "Out"),
    ("ForEachActorAction", "ActorGroup", "In"),
    ("ForceGrindBoostAction", "Hero", "In"),
    ("FormatStringAction", "Actor", "In"),
    ("FormatStringAction", "Number", "In"),
    ("FormatStringAction", "String", "In"),
    ("FormatStringAction", "Target", "Out"),
    ("FormatStringAction", "Vector", "In"),
    ("GalacticMapManualOpenMapAction", "GalacMapActor", "In"),
    ("GalacticMapManualTriggerLandingAction", "GalacMapActor", "In"),
    ("GalacticMapSetPreMapMovieAction", "GalacMapActor", "In"),
    ("GalacticMapSwitchToSetLevelAction", "GalacMapActor", "In"),
    ("GetActorGroupByNameAction", "ActorGroup", "Out"),
    ("GetActorGroupByNameAction", "String", "In"),
    ("GetActorInstanceInfoAction", "Actor", "In"),
    ("GetActorInstanceInfoAction", "ActorName", "Out"),
    ("GetActorInstanceInfoAction", "AssetName", "Out"),
    ("GetActorInstanceInfoAction", "ZoneName", "Out"),
    ("GetActorsWithAllegianceAction", "ActorsWithAllegiance", "Out"),
    ("GetActorsWithAllegianceAction", "Group", "In"),
    ("GetActorsWithAllegianceAction", "NumActorsWithAllegiance", "Out"),
    ("GetAngleBetweenVectorsAction", "Facing_0", "In"),
    ("GetAngleBetweenVectorsAction", "Facing_1", "In"),
    ("GetAngleBetweenVectorsAction", "Result", "Out"),
    ("GetClankPackActorAction", "ClankPackActor", "Out"),
    ("GetClosestPointOnLineFromRayVectorAction", "Distance", "Out"),
    ("GetClosestPointOnLineFromRayVectorAction", "Point_0", "In"),
    ("GetClosestPointOnLineFromRayVectorAction", "Point_1", "In"),
    ("GetClosestPointOnLineFromRayVectorAction", "Ray_Start_Point", "In"),
    ("GetClosestPointOnLineFromRayVectorAction", "Ray_Vector", "In"),
    ("GetClosestPointOnLineFromRayVectorAction", "Result", "Out"),
    ("GetClosestPointOnLineSegmentFromPointAction", "Distance", "Out"),
    ("GetClosestPointOnLineSegmentFromPointAction", "Point", "In"),
    ("GetClosestPointOnLineSegmentFromPointAction", "Result", "Out"),
    ("GetClosestPointOnLineSegmentFromPointAction", "SegmentEnd", "In"),
    ("GetClosestPointOnLineSegmentFromPointAction", "SegmentStart", "In"),
    ("GetCurrencyAction", "Amount", "Out"),
    ("GetCurrencyAction", "Target", "In"),
    ("GetCurrentLevelIDAction", "LevelID", "Out"),
    ("GetEulerAnglesAction", "Eulers", "Out"),
    ("GetEulerAnglesAction", "Forward", "In"),
    ("GetEulerAnglesAction", "Left", "In"),
    ("GetEulerAnglesAction", "Up", "In"),
    ("GetLevelIDAction", "LevelID", "Out"),
    ("GetOrthogonalNormalVectorAction", "OptionalSecondary", "In"),
    ("GetOrthogonalNormalVectorAction", "Primary", "In"),
    ("GetOrthogonalNormalVectorAction", "Target", "Out"),
    ("GetSpaceshipMagnetStateAction", "CanPickupCount", "Out"),
    ("GetSpaceshipMagnetStateAction", "EnergyPercent", "Out"),
    ("GetSpaceshipMagnetStateAction", "TowCount", "Out"),
    ("GetSpaceshipMagnetStateAction", "TowPercent", "Out"),
    ("GetStickInputAction", "Hero", "In"),
    ("GetStickInputAction", "StickDirection", "Out"),
    ("GetStickInputAction", "StickStrength", "Out"),
    ("GetTriggerInputAction", "Hero", "In"),
    ("GetTriggerInputAction", "LeftTriggerValue", "Out"),
    ("GetTriggerInputAction", "RightTriggerValue", "Out"),
    ("GetVectorComponentsAction", "Vector", "In"),
    ("GetVectorComponentsAction", "X", "Out"),
    ("GetVectorComponentsAction", "Y", "Out"),
    ("GetVectorComponentsAction", "Z", "Out"),
    ("GetVectorDirectionAction", "Position_0", "In"),
    ("GetVectorDirectionAction", "Position_1", "In"),
    ("GetVectorDirectionAction", "Result", "Out"),
    ("GetVectorDistanceAction", "Position_0", "In"),
    ("GetVectorDistanceAction", "Position_1", "In"),
    ("GetVectorDistanceAction", "Result", "Out"),
    ("GetVectorPositionAction", "Distance", "In"),
    ("GetVectorPositionAction", "End_Position", "Out"),
    ("GetVectorPositionAction", "Facing_Actor", "In"),
    ("GetVectorPositionAction", "Facing_Direction", "In"),
    ("GetVectorPositionAction", "Starting_Position", "In"),
    ("GetXPAction", "LevelValue", "In"),
    ("GetXPAction", "Target", "In"),
    ("GetXPAction", "XpValue", "Out"),
    ("GoldBoltCountAction", "Count", "Out"),
    ("HasVisitedLevelAction", "OptLevelID", "In"),
    ("HeroButtonPressListenerAction", "Hero", "In"),
    ("HeroButtonReleaseListenerAction", "Hero", "In"),
    ("HeroPlayScriptedAnimAction", "DriverName", "In"),
    ("HeroPlayScriptedAnimAction", "EndDestActor", "In"),
    ("HeroPlayScriptedAnimAction", "FaceAtActor", "In"),
    ("HeroPlayScriptedAnimAction", "RelativeActor", "In"),
    ("HeroPlayScriptedAnimAction", "Target", "In"),
    ("HeroTargetImmuneAction", "Hero", "In"),
    ("InputIdleAction", "IdleTime", "Out"),
    ("InvertMatrixAction", "Forward", "In"),
    ("InvertMatrixAction", "InvertForward", "Out"),
    ("InvertMatrixAction", "InvertLeft", "Out"),
    ("InvertMatrixAction", "InvertPosition", "Out"),
    ("InvertMatrixAction", "InvertUp", "Out"),
    ("InvertMatrixAction", "Left", "In"),
    ("InvertMatrixAction", "Position", "In"),
    ("InvertMatrixAction", "Up", "In"),
    ("IsGoldBoltCollectedAction", "IsCollected", "Out"),
    ("IsLevelUnlockedAction", "LevelID", "In"),
    ("IsNewLevelAction", "IsNew", "Out"),
    ("IsNumberInRangeAction", "ComparisonVariable", "In"),
    ("IsNumberInRangeAction", "Maximum", "In"),
    ("IsNumberInRangeAction", "Minimum", "In"),
    ("IsVarConnectedAction", "Actor", "In"),
    ("IsVarConnectedAction", "Bool", "In"),
    ("IsVarConnectedAction", "Number", "In"),
    ("IsVarConnectedAction", "String", "In"),
    ("IsVarConnectedAction", "Vector", "In"),
    ("LerpFloatAction", "Float1", "In"),
    ("LerpFloatAction", "Float2", "In"),
    ("LerpFloatAction", "Result", "Out"),
    ("LerpFloatAction", "T_Value", "In"),
    ("LerpFloatOverTimeAction", "CurrentValue", "Out"),
    ("LerpFloatOverTimeAction", "Duration", "In"),
    ("LerpFloatOverTimeAction", "From", "In"),
    ("LerpFloatOverTimeAction", "To", "In"),
    ("LerpVectorAction", "Result", "Out"),
    ("LerpVectorAction", "T_Value", "In"),
    ("LerpVectorAction", "Vector1", "In"),
    ("LerpVectorAction", "Vector2", "In"),
    ("LerpVectorOverTimeAction", "CurrentValue", "Out"),
    ("LerpVectorOverTimeAction", "Duration", "In"),
    ("LerpVectorOverTimeAction", "From", "In"),
    ("LerpVectorOverTimeAction", "To", "In"),
    ("LogarithmAction", "Base", "In"),
    ("LogarithmAction", "Result", "Out"),
    ("LogarithmAction", "Value", "In"),
    ("LoopSignalAction", "CurrentLoopCount", "Out"),
    ("LoopSignalAction", "LoopCount", "In"),
    ("LoopSignalAction", "LoopDelay", "In"),
    ("MaxMinAction", "MaxIndex", "Out"),
    ("MaxMinAction", "MaxResult", "Out"),
    ("MaxMinAction", "MinIndex", "Out"),
    ("MaxMinAction", "MinResult", "Out"),
    ("MaxMinAction", "Value0", "In"),
    ("MaxMinAction", "Value1", "In"),
    ("MaxMinAction", "Value2", "In"),
    ("MaxMinAction", "Value3", "In"),
    ("MaxMinAction", "Value4", "In"),
    ("MaxMinAction", "Value5", "In"),
    ("MaxMinAction", "Value6", "In"),
    ("MaxMinAction", "Value7", "In"),
    ("ModAction", "Result", "Out"),
    ("ModAction", "Value_0", "In"),
    ("ModAction", "Value_1", "In"),
    ("MultVectorByScalarAction", "Scalar", "In"),
    ("MultVectorByScalarAction", "Target", "Out"),
    ("MultVectorByScalarAction", "Vector", "In"),
    ("MultiplyAction", "Result", "Out"),
    ("MultiplyAction", "Value_0", "In"),
    ("MultiplyAction", "Value_1", "In"),
    ("MultiplyMatricesAction", "Mat1Forward", "In"),
    ("MultiplyMatricesAction", "Mat1Left", "In"),
    ("MultiplyMatricesAction", "Mat1Position", "In"),
    ("MultiplyMatricesAction", "Mat1Up", "In"),
    ("MultiplyMatricesAction", "Mat2Forward", "In"),
    ("MultiplyMatricesAction", "Mat2Left", "In"),
    ("MultiplyMatricesAction", "Mat2Position", "In"),
    ("MultiplyMatricesAction", "Mat2Up", "In"),
    ("MultiplyMatricesAction", "ResultForward", "Out"),
    ("MultiplyMatricesAction", "ResultLeft", "Out"),
    ("MultiplyMatricesAction", "ResultPosition", "Out"),
    ("MultiplyMatricesAction", "ResultUp", "Out"),
    ("NormalizeValueToRangeAction", "Max", "In"),
    ("NormalizeValueToRangeAction", "Min", "In"),
    ("NormalizeValueToRangeAction", "Result", "Out"),
    ("NormalizeValueToRangeAction", "Value", "In"),
    ("NormalizeVectorAction", "OldLength", "Out"),
    ("NormalizeVectorAction", "Target", "Out"),
    ("NormalizeVectorAction", "Vector", "In"),
    ("NumberGenAction", "Result", "Out"),
    ("OnBooleanAction", "Target", "In"),
    ("OnBouncedAction", "Instigator", "Out"),
    ("OnBouncedAction", "Target", "In"),
    ("OnDropObjectAction", "Dropper", "Out"),
    ("OnDropObjectAction", "Object", "Out"),
    ("OnFrameEventAction", "DeltaTime", "Out"),
    ("OnFrameEventAction", "DeltaTimeReal", "Out"),
    ("OnSpaceshipReticleTargetChangeAction", "TargetActor", "Out"),
    ("OnStringMessageAction", "Instigator", "Out"),
    ("OnStringMessageAction", "ListenTo", "In"),
    ("OnStringMessageAction", "Message", "Out"),
    ("OnThrowObjectAction", "Object", "Out"),
    ("OnThrowObjectAction", "Thrower", "Out"),
    ("OverrideGrindConfigAction", "Hero", "In"),
    ("OverrideHoverbootConfigAction", "Hero", "In"),
    ("OverrideSlideConfigAction", "Hero", "In"),
    ("PeriodicAction", "RemainingTime", "Out"),
    ("PeriodicAction", "ResetTime", "In"),
    ("RandomNumberAction", "Max", "In"),
    ("RandomNumberAction", "Min", "In"),
    ("RandomNumberAction", "Result", "Out"),
    ("RequestHeroRespawnAction", "Hero", "In"),
    ("RotateByEulerAnglesAction", "Eulers", "In"),
    ("RotateByEulerAnglesAction", "Position", "In"),
    ("RotateByEulerAnglesAction", "RotatedPosition", "Out"),
    ("RotateByMatrixAction", "Forward", "In"),
    ("RotateByMatrixAction", "Left", "In"),
    ("RotateByMatrixAction", "Position", "In"),
    ("RotateByMatrixAction", "RotatedPosition", "Out"),
    ("RotateByMatrixAction", "Up", "In"),
    ("RotateVectorAroundAxisAction", "Axis", "In"),
    ("RotateVectorAroundAxisAction", "Result", "Out"),
    ("RotateVectorAroundAxisAction", "Rotation", "In"),
    ("RotateVectorAroundAxisAction", "Vec", "In"),
    ("RotateVectorAroundYAction", "Facing_Vec", "In"),
    ("RotateVectorAroundYAction", "Result_Vec", "Out"),
    ("RotateVectorAroundYAction", "Y_Rotation", "In"),
    ("ScaleActorAction", "Actor", "In"),
    ("ScaleActorAction", "Scale", "In"),
    ("SetActorAction", "Target", "Out"),
    ("SetActorAction", "Value", "In"),
    ("SetActorGroupAction", "Target", "Out"),
    ("SetActorGroupAction", "Value", "In"),
    ("SetAdditionalAttackersAction", "AdditonalAttackers", "In"),
    ("SetAdditionalAttackersAction", "Hero", "In"),
    ("SetBooleanAction", "Target", "Out"),
    ("SetCurrencyAction", "Result", "Out"),
    ("SetCurrencyAction", "Target", "In"),
    ("SetCurrencyAction", "Value_0", "In"),
    ("SetDamagedOnlyByHeroAction", "Target", "In"),
    ("SetHeroHackedShipAction", "Curve", "In"),
    ("SetHeroHackedShipAction", "Hero", "In"),
    ("SetHeroMaskAction", "Hero", "In"),
    ("SetHeroRailShooterAction", "Curve", "In"),
    ("SetHeroRailShooterAction", "Hero", "In"),
    ("SetHeroSpaceshipFlighConfigAction", "Hero", "In"),
    ("SetHeroSpaceshipInputAction", "Hero", "In"),
    ("SetHeroSpaceshipMagnetModeAction", "TargetShip", "In"),
    ("SetHeroSpaceshipMagnetModeAction", "TowGroup", "In"),
    ("SetHeroSpaceshipMissileCountAction", "Hero", "In"),
    ("SetNumberAction", "Target", "Out"),
    ("SetNumberAction", "Value", "In"),
    ("SetStringAction", "Target", "Out"),
    ("SetStringAction", "Value", "In"),
    ("SetTrampolineParamsAction", "Enabled", "In"),
    ("SetTrampolineParamsAction", "MaxHeight", "In"),
    ("SetTrampolineParamsAction", "MinHeight", "In"),
    ("SetTrampolineParamsAction", "ReflectionFactor", "In"),
    ("SetTrampolineParamsAction", "Trampoline", "In"),
    ("SetVectorAction", "Target", "Out"),
    ("SetVectorAction", "X", "In"),
    ("SetVectorAction", "Y", "In"),
    ("SetVectorAction", "Z", "In"),
    ("SetXPAction", "Target", "In"),
    ("SetXPAction", "XPValue", "In"),
    ("SetupControllerAction", "Factory", "In"),
    ("SetupControllerAction", "SpawnCount", "Out"),
    ("SlerpVectorAction", "Result", "Out"),
    ("SlerpVectorAction", "T_Value", "In"),
    ("SlerpVectorAction", "Vector1", "In"),
    ("SlerpVectorAction", "Vector2", "In"),
    ("SolveQuadraticEquationAction", "A", "In"),
    ("SolveQuadraticEquationAction", "B", "In"),
    ("SolveQuadraticEquationAction", "C", "In"),
    ("SolveQuadraticEquationAction", "Result1", "Out"),
    ("SolveQuadraticEquationAction", "Result2", "Out"),
    ("SpawnRewardAction", "Target", "In"),
    ("SubCurrencyAction", "Result", "Out"),
    ("SubCurrencyAction", "Target", "In"),
    ("SubCurrencyAction", "Value_0", "In"),
    ("SubtractAction", "Result", "Out"),
    ("SubtractAction", "Value_0", "In"),
    ("SubtractAction", "Value_1", "In"),
    ("SubtractVectorsAction", "Target", "Out"),
    ("SubtractVectorsAction", "VectorA", "In"),
    ("SubtractVectorsAction", "VectorB", "In"),
    ("SwitchSignalAction", "Select", "In"),
    ("TimerAction", "StartTime", "In"),
    ("TimerAction", "TimeRemaining", "Out"),
    ("ToggleUsabilityControlsAction", "Target", "In"),
    ("ToggleUsabilityControlsAction", "Val", "In"),
    ("ToggleUsabilityMovementAction", "Target", "In"),
    ("ToggleUsabilityMovementAction", "Val", "In"),
    ("TrigonometryAction", "Degrees", "In"),
    ("TrigonometryAction", "Value", "Out"),
    ("TrigonometryInverseAction", "Degrees", "Out"),
    ("TrigonometryInverseAction", "Value", "In"),
    ("UnlockLevelAction", "LevelID", "In"),
    ("VanityEquipBundleAction", "VanityActor", "In"),
    ("VanityEquipSetAction", "VanityActor", "In"),
    ("VanityIsBundleEquippedAction", "VanityActor", "In"),
    ("VanityIsSetEquippedAction", "VanityActor", "In"),
];
