//! `.nodegraph` — the mission / objective / nodal-shot graphs.
//!
//! A nodegraph is the *gameplay wiring* of progression. It is not a script and
//! not a state machine in the animation sense: it is a directed graph of typed
//! nodes joined plug-to-plug, and the three flavours that ship are
//!
//!   * **mission graphs** — one per campaign, the list of missions and the
//!     boolean gates between them (`nodegraph/missions/*.missions.nodegraph`);
//!   * **objective graphs** — one per mission, the ordered objectives, their
//!     checkpoints, their save behaviour and their UI hints
//!     (`nodegraph/objectives/*.objectives.nodegraph`);
//!   * **nodal-shot graphs** — per-projectile hit/lifetime reaction graphs
//!     (`nodegraph/nodalshot/*.nodalshot.nodegraph`).
//!
//! # What ships
//!
//! Re-measured on the PC build (`conduit_probe census`, `ngcensus`): 221
//! `.nodegraph` paths are in the name list, **60 are present in the TOC**, and
//! all 60 decode. Largest is `nodegraph/missions/i29_default.missions.nodegraph`
//! at 228,992 bytes. The split is 58 objective graphs, 1 mission graph
//! (`i29_default`, the whole campaign), 1 nodal-shot graph.
//!
//! Totals over the 60: **853 nodes, 802 connections, 4,151 debug id mappings,
//! 188 reference rows.** Every one of the 853 node-index rows has its payload
//! section present, every node id is non-zero and unique, and no section tag in
//! any of the 60 is left unclaimed.
//!
//! # File layout
//!
//! A built nodegraph is a bare `DAT1` with five fixed sections plus **one extra
//! section per node**. The five fixed tags are all CRC32 preimage-verified
//! against readable names by [`tests::section_tags_are_crc32_of_their_names`]:
//!
//! ```text
//!   tag         name                             in   bytes    contents
//!   0xE5065650  "Node Graph Built Header"        60    2,880    GraphTypeHash, ApproximatePlugCount
//!   0xD8110E80  "Node Graph Built Nodes"         60   42,624    the node index
//!   0x7884E530  "Node Graph Built Connections"   60  142,832    the edge list
//!   0xA9894CDB  "Node Graph Built Debug"         60  401,376    flattened-id -> authored-id map
//!   0xFBD496D6  "Node Graph Refs"                55    3,008    16-byte asset refs
//!               <853 per-node payloads>         853  805,712
//! ```
//!
//! `GraphTypeHash` is a DDL **type id**, i.e. `crc32` of the graph's struct
//! name: `0xD2C5E2D6` = `ObjectiveGraphGameDef`, `0x1E213EA9` =
//! `MissionGraphGameDef`, `0x33A51938` = `NodalShotGraphDef`. See
//! [`GRAPH_TYPES`].
//!
//! The node index is an array of `{ u32 BuiltNodeDataBufferNameHash, u32
//! FieldNameHash }`:
//!
//!   * `BuiltNodeDataBufferNameHash` is the **DAT1 section tag** of that one
//!     node's payload. The payload is an ordinary DDL blob, so the node's own
//!     field names come out of the asset's string pool and nothing external is
//!     needed to read them.
//!   * `FieldNameHash` is the **field id of the array slot on the graph struct**
//!     the node belongs to — `crc32("ObjectiveNodes")`, `crc32("AndNodes")`, … —
//!     which is what names the node's class. [`NODE_SLOTS`] carries the 77 slots
//!     this build resolves, every row verified by `crc32(name) == hash`. 19 of
//!     the 77 are actually used by shipped data, covering 836 of the 853 nodes.
//!     `ObjectiveNodeGameDef` alone is 606 of them.
//!   * …**except** when `FieldNameHash == BuiltNodeDataBufferNameHash`, which
//!     happens on exactly 17 of the 853 nodes and on no node that the slot
//!     table can name. Those 17 are flattened sub-graph instances, not elements
//!     of a named array slot: see [`Node::is_flattened_instance`]. Every one of
//!     them carries a `Start` output plug plus `FallbackMissionName` /
//!     `DebugName`, i.e. the shape of `MissionStartObjectiveNodeGameDef`, and
//!     three of them additionally carry a `GraphData` sub-object typed
//!     `ObjectiveGraphDataGame`. All 17 sit in objective graphs.
//!
//! # Node Graph Refs
//!
//! **Two** type tags ship, not one: `0xA9F149C4` = `.config` on 133 rows and
//! `0x53B8EA03` = `.nodegraph` on 55 rows. The 55 `.nodegraph` rows are all in
//! `i29_default.missions.nodegraph` and are the objective graphs its 57
//! `MissionNodes` hand off to — each such node also names its objective graph
//! in the payload at `ObjectiveGraph.AssetPath`, which is how the mission graph
//! and the 58 objective graphs join up. The `.config` rows are equipment and
//! hero loadouts named by the debug-load-info blocks. 91 distinct assets in all.
//!
//! # Plugs and connections
//!
//! Every node payload carries `Id` (a 64-bit tuid), `X`/`Y` (its authoring
//! position, which a viewer can lay a graph out with directly) and a set of
//! *plug* sub-structs. A plug sub-struct has `Type` = `kInputPlug` or
//! `kOutputPlug` and its own `Id`; the plug's field name is the port label
//! (`Output`, `Inputs`, `Start`, `Complete`, `SyncOutput`, `Demo`, `NotDemo`,
//! `DebugOnlyOutput`, …). A connection names a source node + source plug and a
//! target node + target plug, all by tuid.
//!
//! Beside each plug array the authoring tool *also* emits one field per element named
//! `"<plug> <i>"` (`"Inputs 0"`, `"SyncOutput 2"`, …) carrying the **same**
//! `Id`, and often the same `Type`, as the array element. Measured: 637 such
//! alias fields across the 60 graphs, 591 of them an exact id duplicate of a
//! sibling. They are aliases, not ports, and [`read_plugs`] drops them — an
//! earlier version of this module claimed to ignore them and did not, which
//! inflated the port list of 591 nodes and made a viewer draw every input twice.
//! After the fix the 60 graphs have **1,613 plugs** — 797 inputs, 816 outputs,
//! over 12 distinct port labels instead of the 25 the aliases produced — and no
//! plug id occurs twice on one node. `ApproximatePlugCount` in the header counts
//! the aliases too, so it totals 2,250 across the 60 and is an upper bound on
//! the real port count, which is presumably why the authoring tool calls it
//! approximate: 191 real ports against a stated 298 on the mission graph.
//!
//! The array element is the one worth keeping for another reason: on a
//! `MissionNodeGameDef` each `SyncOutput` element carries a `SyncLabel`
//! (`"Zurkon_Met"`, `"First_Pirates_Dead"`, `"Francois_Defeated"`) that the
//! alias field does not, and that label is the only human name a mid-mission
//! branch point has. [`Plug::detail`] carries it.
//!
//! # Stale edges ship
//!
//! 10 of the 802 connections name a node id that exists in **no** shipped graph
//! — 8 in `mis_intro.objectives.nodegraph` and 2 in
//! `mis_zurk_01_visit_01.objectives.nodegraph`. Neither graph references another
//! nodegraph, the ids are absent from the debug map, and every node index row in
//! both graphs has its payload, so these are edges to nodes that were deleted
//! from the authored graph and left in the built one. [`NodeGraph::edges`] drops
//! them; [`NodeGraph::stale_connections`] hands them back so a UI can say so
//! instead of quietly losing them.
//!
//! # What an objective node expresses
//!
//! `ObjectiveNodeGameDef` is 606 of the 853 nodes and is the unit a designer
//! works in. Censused over all 606 (`conduit_probe ngobj`), every single one
//! carries exactly one `Inputs` input plug and one `Output` output plug, so an
//! objective is a **link in a chain**: it becomes live when the previous node's
//! `Output` fires into its `Inputs`, and it reports done by firing its own
//! `Output`. Only 3 of the 606 have a second, third or fourth input, i.e. a
//! join. The node holds no completion *test* — the test lives in the zone's
//! scripts, which raise the objective by its `ObjectiveName`; the graph holds
//! the ordering, the save behaviour and the UI.
//!
//! What it does hold, with counts:
//!
//! ```text
//!   ObjectiveName    606   the key gameplay raises; the objective's identity
//!   DebugLoadInfo    606   "start the game here": SpawnCheckpoint, SpawnAsHero,
//!                          SpawnLoadout / BackpackLoadout .config
//!   TaskMode         585   kNormal 324, kHidden 158, kInherited 103
//!   UIMode           455   kInherited 254, kHidden 102, kNormal 99
//!   InfoUI           433   NameLocTag / DescriptionLocTag for the HUD and map
//!   TaskInfo         326   PlaytimeEstimate and friends
//!   Checkpoint       181   the checkpoint to save at when this objective opens
//!   SegmentComplete  122   true on 61 — the objective closes a save segment
//!   SaveOptions       84
//! ```
//!
//! Worked example, `mis_ard_bar.objectives.nodegraph`, 5 nodes and 4 edges:
//!
//! ```text
//!   MissionStartObjectiveNodeGameDef "MIS_ARD_BAR"
//!     Start   -> OBJ_ARD_BAR            checkpoint CHK_A1_1, task kNormal
//!     Output  -> OBJ_ARD_BAR_FRONT      checkpoint CHK_A1_1, task kNormal
//!     Output  -> OBJ_ARD_BAR_COMPLETE   checkpoint CHK_A1_1, task kNormal
//!     Output  -> MissionCompleteObjectiveNodeGameDef
//! ```
//!
//! So: entering the mission triggers `OBJ_ARD_BAR`; when gameplay raises that
//! objective the graph advances to `OBJ_ARD_BAR_FRONT`, then to
//! `OBJ_ARD_BAR_COMPLETE`, whose `Output` reaches the mission-complete node and
//! ends the mission. `OBJ_ARD_BAR_COMPLETE` also carries
//! `DebugLoadInfo.SpawnCheckpoint = "CHK_ARD_BAR_EXIT"` and
//! `SpawnLoadout = configs/equipment/equip_loadout_ardolis.config`, which is the
//! recipe for booting the game straight into that step.
//!
//! A branching example, `mis_zurk_03_visit_03.objectives.nodegraph`: the chain
//! `OBJ_ZURK_V3_GO_TO_ZURKONS` (task `kHidden`) fans out to both
//! `OBJ_ZURK_V3_START` and a `SyncOutputNodeSharedDef` named `Arrived_Zurk_3`.
//! That sync node is how an objective graph tells the *mission* graph something
//! happened mid-mission: in `i29_default.missions.nodegraph` the
//! `MIS_ZURK_03_VISIT_03` mission node has a `SyncOutput` plug labelled
//! `Arrived_Zurk_3`, and it is what unlocks five arena challenge missions.
//!
//! # Node Graph Built Debug
//!
//! 4,151 rows. `OriginalAssetId` is the graph's **own** asset id on 60 of 60
//! graphs, so the table never reaches outside the asset, and
//! `OriginalEmbeddedGraphId` is `u64::MAX` on all 4,151 rows — nothing shipped
//! came from an embedded sub-graph. Of the 4,151 `FlattenedId` values, 853 are
//! node ids and 2,250 are plug ids (including the alias ids); the remaining
//! 1,048 match neither, and what they identify is **not established** — most
//! likely the ids of nested sub-objects such as `SaveOptions` or `InfoUI`.
//!
//! # Why this uses the prius decoder, not the generic one
//!
//! Node payloads, the node index, the connection list and the debug map are all
//! plain DDL, so [`crate::core::prius::decode`] reads them directly — it is the
//! decoder that already turns a DDL blob into a typed, pool-named tree, and it
//! consumes each of these sections to the byte: measured over all 60 shipped
//! graphs, `PObject::span` equals the section length every time.
//!
//! The generic [`crate::core::ddl`] reader **must not** be used on them. A
//! type-19 (`Default`) field with a declared count of 0 always spends one byte
//! on the wire; `ddl::parse` gives it zero reads, so it loses the boundary
//! between struct-array elements. Every `ConnectionTuidDef` ends in such a
//! field, so `ddl::parse` recovers **1 of the 95** connections of
//! `i29_default.missions.nodegraph` and returns 94 empty objects.
//! `prius::read_object` spends the byte and gets all 95. Nothing else in this
//! module works around it.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::Value;

use crate::core::dat1::{Dat1, DAT1_MAGIC};
use crate::core::ddl;
use crate::core::ddl_schema;
use crate::core::error::{Result, ToolkitError};
use crate::core::prius::{self, NameCtx, PObject, PValue};

/// `crc32("Node Graph Built Header")`.
pub const NG_HEADER_TAG: u32 = 0xE506_5650;
/// `crc32("Node Graph Built Nodes")` — the node index.
pub const NG_NODES_TAG: u32 = 0xD811_0E80;
/// `crc32("Node Graph Built Connections")` — the edge list.
pub const NG_CONNECTIONS_TAG: u32 = 0x7884_E530;
/// `crc32("Node Graph Built Debug")` — flattened-id -> authored-id mappings.
pub const NG_DEBUG_TAG: u32 = 0xA989_4CDB;
/// `crc32("Node Graph Refs")` — 16-byte asset references.
pub const NG_REFS_TAG: u32 = 0xFBD4_96D6;

/// The 36-byte header `Toc::extract_asset` prepends to every asset.
const ASSET_HEADER_LEN: usize = 36;

/// The five fixed sections, in the order a UI should show them.
pub const FIXED_SECTIONS: &[(u32, &str)] = &[
    (NG_HEADER_TAG, "Node Graph Built Header"),
    (NG_NODES_TAG, "Node Graph Built Nodes"),
    (NG_CONNECTIONS_TAG, "Node Graph Built Connections"),
    (NG_DEBUG_TAG, "Node Graph Built Debug"),
    (NG_REFS_TAG, "Node Graph Refs"),
];

/// `GraphTypeHash` -> DDL struct name of the graph. Each is `crc32(name)`.
pub const GRAPH_TYPES: &[(u32, &str)] = &[
    (0x1E21_3EA9, "MissionGraphGameDef"),
    (0x33A5_1938, "NodalShotGraphDef"),
    (0xD2C5_E2D6, "ObjectiveGraphGameDef"),
];

/// Node-array slots of the three shipped graph structs:
/// `(crc32(slot name), slot name, DDL struct name of one element)`.
///
/// `FieldNameHash` in the node index is the first column, so this is what turns
/// a node into a named class. Every row is re-hashed by
/// [`tests::node_slots_are_crc32_of_their_names`], so a wrong spelling cannot
/// survive here — the *names* are the fact, not the mapping's provenance.
pub const NODE_SLOTS: &[NodeSlot] = &[
    (0x0035A2E2, "OutputNodes", 0xAB9FCE2B, "OutputNodeBooleanDef"),
    (0x040DBAFF, "ToggleLightsEffectNodes", 0x25EF0B77, "ToggleLightsEffectNodeDef"),
    (0x05DA2827, "OnUpdateIntervalNodes", 0x720FB7DF, "OnUpdateIntervalNodeDef"),
    (0x0D6BC139, "OnHitFriendlyNodes", 0x7F98DA6D, "OnHitFriendlyNodeDef"),
    (0x0F563484, "UIntOutNodes", 0xE3240E36, "UIntOutNodeDef"),
    (0x0F6C7207, "ReplayBranchNodes", 0x6C352121, "ReplayBranchNodeDef"),
    (0x1982BFED, "InputNodes", 0x5293077D, "InputNodeBooleanDef"),
    (0x1D48B928, "BounceEffectNodes", 0x729F1050, "BounceEffectNodeDef"),
    (0x1EDFBB0F, "AddStatusEffectNodes", 0x4654C08E, "AddStatusEffectNodeDef"),
    (0x260039CF, "DemoEndNodes", 0x1438488D, "DemoBranchEndNodeDef"),
    (0x2785A28B, "FloatOutNodes", 0x7DD00C5B, "FloatOutNodeDef"),
    (0x28656389, "CameraShakeEffectNodes", 0xA385931F, "CameraShakeEffectNodeDef"),
    (0x2B7208CF, "DestroyShotEffectNodes", 0xDFC6C05B, "DestroyShotEffectNodeDef"),
    (0x31ED2E57, "OnHitEnemyNodes", 0xB4535F2B, "OnHitEnemyNodeDef"),
    (0x322AADCE, "Connections", 0xF16ECADC, "ConnectionTuidDef"),
    (0x32392A81, "StringHashOutNodes", 0x645CFA7F, "StringHashOutNodeDef"),
    (0x3E08B768, "SpawnHazardDamageAreaEffectNodes", 0x655E5112, "SpawnHazardDamageAreaEffectNodeDef"),
    (0x3EC54F32, "XorStartNodes", 0xB2078453, "XOrStartGroupNodeDef"),
    (0x4187529D, "ObjectiveNodes", 0xDBEB67A3, "ObjectiveNodeGameDef"),
    (0x4D298A09, "OnHitEnvironmentNodes", 0x41D034C4, "OnHitEnvironmentNodeDef"),
    (0x51F269A0, "SyncNodes", 0xA0EF3145, "SyncOutputNodeSharedDef"),
    (0x524FF51C, "DebugMarkerNodes", 0xB6DFA11B, "DebugMarkerNodeDef"),
    (0x536ABDD5, "SignalOutputNodes", 0xD4E0059B, "SignalOutputNodeDef"),
    (0x536BCA39, "HealEffectNodes", 0xE84A5DE5, "HealEffectNodeDef"),
    (0x551A6BD7, "EndNodes", 0x9C19D75A, "MissionCompleteObjectiveNodeGameDef"),
    (0x5895C331, "EmbeddedSubGraphs", 0x313BCA3D, "EmbeddedGraphNodeBaseDef"),
    (0x59CC5798, "OnHitAnythingNodes", 0xD1E83513, "OnHitAnythingNodeDef"),
    (0x61B46C20, "TriggerConduitEventEffectNodes", 0x4CFFC0FF, "TriggerConduitEventEffectNodeDef"),
    (0x6228B283, "DemoStartNodes", 0xDAAE9981, "DemoBranchStartNodeDef"),
    (0x673BA2FC, "ChangeShotMotionEffectNodes", 0x1DAEA3D4, "ChangeShotMotionEffectNodeDef"),
    (0x693FBC22, "ContinueShotMotionEffectNodes", 0xF81A7822, "ContinueShotMotionEffectNodeDef"),
    (0x6B15CE6C, "MissionNodes", 0x33408D17, "MissionNodeGameDef"),
    (0x6B23536F, "IntOutNodes", 0x8D78B49F, "IntOutNodeDef"),
    (0x6DF5C674, "OnPostInitNodes", 0xA42B1E18, "OnPostInitNodeDef"),
    (0x6F4C8CB7, "XorEndNodes", 0x3835D3D6, "XOrEndGroupNodeDef"),
    (0x71B2DCC7, "ChallengeModePrereqNodes", 0x96A1065E, "ChallengeModePrereqNodeDef"),
    (0x733AA768, "NodalShotTransitionStateNodes", 0x78E90C44, "NodalShotTransitionStateNodeDef"),
    (0x774F06EE, "SubGraphs", 0x3E7DA641, "ReferencedGraphNodeBaseDef"),
    (0x8128FB48, "SpawnSyncedActorForHitEffectNodes", 0x8FB0C72B, "SpawnSyncedActorForHitEffectNodeDef"),
    (0x81BB67F8, "DontDestroyShotAfterCollisionEffectNodes", 0x92CCB66F, "DontDestroyShotAfterCollisionEffectNodeDef"),
    (0x95D91DC5, "ReplayBranchEndNodes", 0x48F47291, "ReplayBranchEndNodeDef"),
    (0x98067A6C, "NodalShotUIntVariableNodes", 0xE4E78C15, "NodalShotUIntVariableNodeDef"),
    (0xA19FC9DE, "OnHitNonFriendlyNodes", 0x607C9B80, "OnHitNonFriendlyNodeDef"),
    (0xA59F2B5A, "Comments", 0x956ACA59, "CommentBaseDef"),
    (0xACAE6ECB, "InitNode", 0x4F4E175A, "InitMissionGraphNodeGameDef"),
    (0xACD738F6, "InputCountNodes", 0xDBF734BA, "InputCountMissionSystemNodeDef"),
    (0xADD17EAE, "NotNodes", 0x18DC33FA, "NotMissionSystemNodeDef"),
    (0xAE40268B, "AddStatModifierEffectNodes", 0x9705C2A7, "AddStatModifierEffectNodeDef"),
    (0xAE507F4F, "SpawnSyncedActorEffectNodes", 0x2A2439AD, "SpawnSyncedActorEffectNodeDef"),
    (0xAF1CFAD2, "EnterRagdollStateEffectNodes", 0x7319BB19, "EnterRagdollStateEffectNodeDef"),
    (0xAF9580D8, "BooleanOutNodes", 0x3926CB38, "BooleanOutNodeDef"),
    (0xB2048DE1, "AttachToActorEffectNodes", 0x491327AE, "AttachToActorEffectNodeDef"),
    (0xB4E5255D, "NodalShotInitNewStateNodes", 0xD96EEB21, "NodalShotInitNewStateNodeDef"),
    (0xB79E345C, "SpawnShotEffectNodes", 0xAAC5F9E9, "SpawnShotEffectNodeDef"),
    (0xB8940154, "DebugChainStartNodes", 0x34AFB8C8, "DebugChainStartNodeDef"),
    (0xBACD98DE, "TargetedBounceEffectNodes", 0x7C10E1B0, "TargetedBounceEffectNodeDef"),
    (0xBB431E1A, "Vec3OutNodes", 0xD75DD461, "Vec3OutNodeDef"),
    (0xBD4CF165, "OrNodes", 0xC8EA4B99, "OrMissionSystemNodeDef"),
    (0xBFE4DA68, "StringOutNodes", 0x565DCD1B, "StringOutNodeDef"),
    (0xC374101B, "DealDamageEffectNodes", 0x29FEB010, "DealDamageEffectNodeDef"),
    (0xC63AD40A, "ExplodeEffectNodes", 0xE62E90E7, "ExplodeEffectNodeDef"),
    (0xC73B0357, "ItemPrereqNodes", 0xF18CF588, "ItemPrereqNodeDef"),
    (0xC802FC4D, "EventInitializationOutputNodes", 0x6CA3EFE7, "EventInitializationOutputNodeDef"),
    (0xCD942E2F, "NodalShotInitFirstStateNodes", 0x53B550AC, "NodalShotInitFirstStateNodeDef"),
    (0xCFAD9393, "PlayAnimEffectNodes", 0xAE157C10, "PlayAnimEffectNodeDef"),
    (0xD150652D, "AndNodes", 0x32109E5E, "AndMissionSystemNodeDef"),
    (0xD353A72F, "OnLifetimeExpiredNodes", 0xC7D175EF, "OnLifetimeExpiredNodeDef"),
    (0xDAF1AF95, "GSTNodes", 0x56BE5F12, "GlobalSaveTableNodeDef"),
    (0xE55BD9DE, "GiveItemEffectNodes", 0x7DCBCF20, "GiveItemEffectNodeDef"),
    (0xE6B0DE2B, "StartNode", 0xBB8C566A, "MissionStartObjectiveNodeGameDef"),
    (0xEE56FBE1, "AttachToHitSurfaceEffectNodes", 0xF075AFF5, "AttachToHitSurfaceEffectNodeDef"),
    (0xEF771473, "CharacterPrereqNodes", 0x83B7B9A5, "CharacterPrereqNodeDef"),
    (0xF50EB81F, "SignalInputNodes", 0x959F68B4, "SignalInputNodeDef"),
    (0xF54804BF, "OnPassMaxRangeNodes", 0xF94F2BBC, "OnPassMaxRangeNodeDef"),
    (0xF98D90DF, "EventInitializationInputNodes", 0x6BD01B83, "EventInitializationInputNodeDef"),
    (0xFAFE6B04, "OpenWorldActivityNodes", 0x00000000, ""),
    (0xFB0A9DE1, "DebugChainEndNodes", 0x54A47EC4, "DebugChainEndNodeDef"),
];

/// One row of [`NODE_SLOTS`]: `(crc32(slot name), slot name, crc32(class name),
/// class name)`. `class_id == 0` with an empty `class` means the slot name is
/// established (its hash is a preimage of `FieldNameHash`) but the element
/// struct's name is not.
pub type NodeSlot = (u32, &'static str, u32, &'static str);

/// Graph struct name for a `GraphTypeHash`.
pub fn graph_type_name(hash: u32) -> Option<&'static str> {
    GRAPH_TYPES.iter().find(|(h, _)| *h == hash).map(|(_, n)| *n)
}

/// The [`NODE_SLOTS`] row for a `FieldNameHash`.
pub fn node_slot(hash: u32) -> Option<&'static NodeSlot> {
    NODE_SLOTS.iter().find(|(h, _, _, _)| *h == hash)
}

/// Slot field name for a `FieldNameHash`.
pub fn slot_name(hash: u32) -> Option<&'static str> {
    node_slot(hash).map(|r| r.1)
}

/// Element struct name for a `FieldNameHash`, when this build has established
/// one.
pub fn slot_class(hash: u32) -> Option<&'static str> {
    node_slot(hash).map(|r| r.3).filter(|c| !c.is_empty())
}

// ---------------------------------------------------------------------------
// Decoded model
// ---------------------------------------------------------------------------

/// One port on a node. `label` is the payload field name that held it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Plug {
    pub label: String,
    /// `"kInputPlug"` or `"kOutputPlug"` as the file spells it.
    pub kind: String,
    /// The flattened id connections reference.
    pub id: u64,
    /// Position within the plug array when the field held more than one, else 0.
    pub index: usize,
    /// The element's own `SyncLabel`, when it has one — the designer's name for
    /// a mid-mission sync point, e.g. `"First_Pirates_Dead"`. 6 of the 1,613
    /// shipped plugs carry one, all on `MissionNodeGameDef` nodes of the mission
    /// graph.
    pub detail: Option<String>,
}

impl Plug {
    pub fn is_input(&self) -> bool {
        self.kind == "kInputPlug"
    }
    pub fn is_output(&self) -> bool {
        self.kind == "kOutputPlug"
    }
    /// What to print on the port: the `SyncLabel` when there is one, else the
    /// field name, with the array index appended when the array has several.
    pub fn display(&self) -> String {
        match (&self.detail, self.index) {
            (Some(d), _) => format!("{} \"{d}\"", self.label),
            (None, 0) => self.label.clone(),
            (None, i) => format!("{}[{i}]", self.label),
        }
    }
}

/// One node of the graph.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Node {
    pub id: u64,
    /// `FieldNameHash` — the graph-struct slot this node came out of.
    pub slot_hash: u32,
    /// Slot name, e.g. `"ObjectiveNodes"`, when this build knows it.
    pub slot: Option<&'static str>,
    /// Element struct name, e.g. `"ObjectiveNodeGameDef"`, when known.
    pub class: Option<&'static str>,
    /// `BuiltNodeDataBufferNameHash` — the DAT1 section holding the payload.
    pub buffer_tag: u32,
    /// Authoring canvas position; a viewer can lay the graph out with it.
    pub x: f32,
    pub y: f32,
    /// Best human label: `ObjectiveName`, else `MissionName`, else `DebugName`.
    pub name: String,
    pub plugs: Vec<Plug>,
    /// Everything else in the payload, as JSON. Plugs and `X`/`Y`/`Id`/`Type`
    /// are left in so nothing is hidden from an inspector.
    pub params: Value,
}

impl Node {
    /// A flattened sub-graph instance: its `FieldNameHash` is its own payload
    /// section tag instead of a slot field id, so it belongs to no named array.
    ///
    /// True on exactly 17 of the 853 shipped nodes, and on none that
    /// [`NODE_SLOTS`] can name, so this is the whole of the "unresolved slot"
    /// population — there is no missing slot name to go and find.
    pub fn is_flattened_instance(&self) -> bool {
        self.slot_hash == self.buffer_tag
    }

    /// `class` when known, else the slot, else what the node actually is.
    pub fn kind_label(&self) -> String {
        if let Some(c) = self.class {
            return c.to_string();
        }
        if let Some(sl) = self.slot {
            return sl.to_string();
        }
        if self.is_flattened_instance() {
            return "<flattened sub-graph instance>".to_string();
        }
        format!("slot_{:08X}", self.slot_hash)
    }

    /// Assets this node's payload names by path, e.g. a `MissionNodeGameDef`'s
    /// `ObjectiveGraph.AssetPath` or a debug-load-info `SpawnLoadout` config.
    /// Every string in the payload that looks like an asset path, deduplicated.
    pub fn linked_assets(&self) -> Vec<String> {
        fn walk(v: &Value, out: &mut BTreeSet<String>) {
            match v {
                Value::String(s) => {
                    let l = s.to_ascii_lowercase();
                    if l.contains('/') && l.rsplit('.').next().is_some_and(|e| !e.contains('/')) && l.contains('.') {
                        out.insert(s.clone());
                    }
                }
                Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
                Value::Object(m) => m.values().for_each(|x| walk(x, out)),
                _ => {}
            }
        }
        let mut out = BTreeSet::new();
        walk(&self.params, &mut out);
        out.into_iter().collect()
    }

    /// The payload value at a top-level field, as text. `None` when absent.
    pub fn param(&self, field: &str) -> Option<String> {
        match self.params.get(field) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Null) | None => None,
            Some(v) => Some(v.to_string()),
        }
    }
}

/// One edge. All four ids are tuids, not indices.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Connection {
    pub id: u64,
    pub source_node: u64,
    pub source_plug: u64,
    pub target_node: u64,
    pub target_plug: u64,
}

/// One row of `Node Graph Built Debug`: what an id was before the authoring tool
/// flattened the graph.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IdMapping {
    pub flattened_id: u64,
    pub original_asset_id: u64,
    pub original_id: u64,
    /// `u64::MAX` when the id did not come from an embedded sub-graph.
    pub original_embedded_graph_id: u64,
}

/// An edge with both ends resolved to node indices and port labels.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Edge {
    pub from: usize,
    pub from_plug: String,
    pub to: usize,
    pub to_plug: String,
}

/// A decoded `.nodegraph`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NodeGraph {
    pub graph_type: u32,
    /// DDL struct name of the graph, when known.
    pub graph_type_name: Option<&'static str>,
    pub approximate_plug_count: u64,
    pub nodes: Vec<Node>,
    pub connections: Vec<Connection>,
    pub id_mappings: Vec<IdMapping>,
    /// `Node Graph Refs`. Two tags ship: `.config` (`0xA9F149C4`) on 133 rows
    /// and `.nodegraph` (`0x53B8EA03`) on 55, the latter only on the mission
    /// graph. An earlier note here claimed every row was a `.config`.
    pub refs: Vec<GraphRef>,
    /// Section tags in the asset that are neither a fixed section nor a node
    /// payload. Empty on every shipped graph; non-empty means something new.
    pub unclaimed_sections: Vec<u32>,
}

/// `crc32(".config")`.
pub const REF_TAG_CONFIG: u32 = 0xA9F1_49C4;
/// `crc32(".nodegraph")` — a referenced sub-graph.
pub const REF_TAG_NODEGRAPH: u32 = 0x53B8_EA03;

/// One row of `Node Graph Refs`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GraphRef {
    pub asset_id: u64,
    pub path: String,
    pub type_id: u32,
}

impl GraphRef {
    /// `".config"`, `".nodegraph"`, or `"unknown"` — from the tag, not the
    /// spelling.
    pub fn kind(&self) -> &'static str {
        match self.type_id {
            REF_TAG_CONFIG => ".config",
            REF_TAG_NODEGRAPH => ".nodegraph",
            _ => "unknown",
        }
    }
}

impl NodeGraph {
    /// Parse a nodegraph asset, with or without the 36-byte asset header.
    pub fn parse(asset: &[u8]) -> Result<Self> {
        Self::from_dat1(&open_dat1(asset)?)
    }

    /// Parse from an already-opened `DAT1`.
    ///
    /// The schema database, when one is configured, only adds labels and enum
    /// member sets; every name this returns comes out of the asset's own string
    /// pool or out of [`NODE_SLOTS`].
    pub fn from_dat1(dat1: &Dat1) -> Result<Self> {
        let schema = ddl_schema::get_default();
        let schema_ref = if schema.is_empty() { None } else { Some(schema.as_ref()) };
        let ctx = NameCtx::new(Some(dat1), schema_ref);

        let header = dat1
            .get_section_data(NG_HEADER_TAG)
            .ok_or(ToolkitError::SectionNotFound(NG_HEADER_TAG))?;
        let header = prius::decode(header, 0, None, &ctx)?;
        let graph_type = uint_of(&header, "GraphTypeHash").unwrap_or(0) as u32;

        // ---- node index -------------------------------------------------
        let mut nodes = Vec::new();
        let mut node_tags: BTreeSet<u32> = BTreeSet::new();
        if let Some(sec) = dat1.get_section_data(NG_NODES_TAG) {
            let index = prius::decode(sec, 0, None, &ctx)?;
            for row in structs_of(&index, "Nodes") {
                let buffer_tag = uint_of(&row, "BuiltNodeDataBufferNameHash").unwrap_or(0) as u32;
                let slot_hash = uint_of(&row, "FieldNameHash").unwrap_or(0) as u32;
                let Some(payload) = dat1.get_section_data(buffer_tag) else { continue };
                node_tags.insert(buffer_tag);
                let obj = prius::decode(payload, 0, None, &ctx)?;
                nodes.push(read_node(buffer_tag, slot_hash, &obj));
            }
        }

        // ---- connections -------------------------------------------------
        let mut connections = Vec::new();
        if let Some(sec) = dat1.get_section_data(NG_CONNECTIONS_TAG) {
            let outer = prius::decode(sec, 0, None, &ctx)?;
            for c in structs_of(&outer, "Connections") {
                connections.push(Connection {
                    id: uint_of(&c, "Id").unwrap_or(0),
                    source_node: uint_of(&c, "SourceNodeId").unwrap_or(0),
                    source_plug: uint_of(&c, "SourcePlugId").unwrap_or(0),
                    target_node: uint_of(&c, "TargetNodeId").unwrap_or(0),
                    target_plug: uint_of(&c, "TargetPlugId").unwrap_or(0),
                });
            }
        }

        // ---- debug id mappings -------------------------------------------
        let mut id_mappings = Vec::new();
        if let Some(sec) = dat1.get_section_data(NG_DEBUG_TAG) {
            let outer = prius::decode(sec, 0, None, &ctx)?;
            for m in structs_of(&outer, "IdMappings") {
                id_mappings.push(IdMapping {
                    flattened_id: uint_of(&m, "FlattenedId").unwrap_or(0),
                    original_asset_id: uint_of(&m, "OriginalAssetId").unwrap_or(0),
                    original_id: uint_of(&m, "OriginalId").unwrap_or(0),
                    original_embedded_graph_id: uint_of(&m, "OriginalEmbeddedGraphId")
                        .unwrap_or(u64::MAX),
                });
            }
        }

        let refs: Vec<GraphRef> = ddl::parse_asset_refs(dat1, NG_REFS_TAG)
            .into_iter()
            .map(|r| GraphRef { asset_id: r.asset_id, path: r.path, type_id: r.type_id })
            .collect();

        let fixed: BTreeSet<u32> = FIXED_SECTIONS.iter().map(|(t, _)| *t).collect();
        let unclaimed = dat1
            .sections
            .iter()
            .map(|s| s.tag)
            .filter(|t| !fixed.contains(t) && !node_tags.contains(t))
            .collect();

        Ok(Self {
            graph_type,
            graph_type_name: graph_type_name(graph_type),
            approximate_plug_count: uint_of(&header, "ApproximatePlugCount").unwrap_or(0),
            nodes,
            connections,
            id_mappings,
            refs,
            unclaimed_sections: unclaimed,
        })
    }

    /// `plug id -> (node index, printable port label)` over every plug of every
    /// node. 1,613 entries across the 60 shipped graphs, one per real port.
    pub fn plug_owners(&self) -> HashMap<u64, (usize, String)> {
        let mut m = HashMap::new();
        for (i, n) in self.nodes.iter().enumerate() {
            for p in &n.plugs {
                m.insert(p.id, (i, p.display()));
            }
        }
        m
    }

    /// `node id -> node index`.
    pub fn node_index(&self) -> HashMap<u64, usize> {
        self.nodes.iter().enumerate().map(|(i, n)| (n.id, i)).collect()
    }

    /// Every connection whose two endpoints both resolve to a node in this
    /// asset, as indices and port labels. Connections that do not resolve are
    /// reported separately by [`Self::dangling`].
    pub fn edges(&self) -> Vec<Edge> {
        let by_id = self.node_index();
        let plugs = self.plug_owners();
        let mut out = Vec::new();
        for c in &self.connections {
            let Some(&from) = by_id.get(&c.source_node) else { continue };
            let Some(&to) = by_id.get(&c.target_node) else { continue };
            out.push(Edge {
                from,
                from_plug: plugs
                    .get(&c.source_plug)
                    .map(|(_, l)| l.clone())
                    .unwrap_or_else(|| format!("{:#018X}", c.source_plug)),
                to,
                to_plug: plugs
                    .get(&c.target_plug)
                    .map(|(_, l)| l.clone())
                    .unwrap_or_else(|| format!("{:#018X}", c.target_plug)),
            });
        }
        out
    }

    /// Connections [`Self::edges`] throws away, because at least one endpoint
    /// is not in this asset.
    ///
    /// 10 of the 802 shipped connections, in 2 of the 60 graphs, and none of
    /// them resolves in any other shipped graph either — they are edges to
    /// deleted nodes that the authoring tool kept. A UI should list these rather than
    /// let them vanish.
    pub fn stale_connections(&self) -> Vec<&Connection> {
        let by_id = self.node_index();
        let plugs = self.plug_owners();
        self.connections
            .iter()
            .filter(|c| {
                !by_id.contains_key(&c.source_node)
                    || !by_id.contains_key(&c.target_node)
                    || !plugs.contains_key(&c.source_plug)
                    || !plugs.contains_key(&c.target_plug)
            })
            .collect()
    }

    /// `(connections whose node ids do not resolve, connections whose plug ids
    /// do not resolve)`. Both are 0 on 58 of the 60 shipped graphs;
    /// `mis_intro.objectives` reports 8/8 and `mis_zurk_01_visit_01.objectives`
    /// 2/2. See [`Self::stale_connections`].
    pub fn dangling(&self) -> (usize, usize) {
        let by_id = self.node_index();
        let plugs = self.plug_owners();
        let mut bad_nodes = 0;
        let mut bad_plugs = 0;
        for c in &self.connections {
            if !by_id.contains_key(&c.source_node) || !by_id.contains_key(&c.target_node) {
                bad_nodes += 1;
            }
            if !plugs.contains_key(&c.source_plug) || !plugs.contains_key(&c.target_plug) {
                bad_plugs += 1;
            }
        }
        (bad_nodes, bad_plugs)
    }

    /// Node indices with no incoming edge — the graph's entry points.
    pub fn roots(&self) -> Vec<usize> {
        let mut has_in = vec![false; self.nodes.len()];
        for e in self.edges() {
            has_in[e.to] = true;
        }
        (0..self.nodes.len()).filter(|i| !has_in[*i]).collect()
    }

    /// Node class (or slot) -> count.
    pub fn class_histogram(&self) -> BTreeMap<String, usize> {
        let mut h = BTreeMap::new();
        for n in &self.nodes {
            *h.entry(n.kind_label()).or_insert(0) += 1;
        }
        h
    }

    /// Breadth-first order from every root, then anything unreached. This is
    /// the reading order a "what happens in this mission" list wants.
    pub fn walk_order(&self) -> Vec<usize> {
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); self.nodes.len()];
        for e in self.edges() {
            adj[e.from].push(e.to);
        }
        let mut seen = vec![false; self.nodes.len()];
        let mut out = Vec::new();
        let mut queue: std::collections::VecDeque<usize> = self.roots().into_iter().collect();
        for &r in queue.iter() {
            seen[r] = true;
        }
        while let Some(i) = queue.pop_front() {
            out.push(i);
            for &j in &adj[i] {
                if !seen[j] {
                    seen[j] = true;
                    queue.push_back(j);
                }
            }
        }
        for i in 0..self.nodes.len() {
            if !seen[i] {
                out.push(i);
            }
        }
        out
    }

    /// One line per node in [`Self::walk_order`]: what a designer would read.
    /// Any stale connection is listed after the nodes rather than dropped.
    pub fn outline(&self) -> Vec<String> {
        let edges = self.edges();
        let mut out = Vec::new();
        for i in self.walk_order() {
            let n = &self.nodes[i];
            let outs: Vec<String> = edges
                .iter()
                .filter(|e| e.from == i)
                .map(|e| format!("{} -> #{} {}", e.from_plug, e.to, self.nodes[e.to].name))
                .collect();
            let label = if n.name.is_empty() {
                n.param("DebugName").unwrap_or_default()
            } else {
                n.name.clone()
            };
            let mut line = format!("#{i} [{}] {label}", n.kind_label());
            if let Some(cp) = n.param("Checkpoint") {
                line.push_str(&format!("  checkpoint={cp}"));
            }
            if let Some(tm) = n.param("TaskMode") {
                line.push_str(&format!("  task={tm}"));
            }
            if let Some(og) = n
                .params
                .get("ObjectiveGraph")
                .and_then(|v| v.get("AssetPath"))
                .and_then(|v| v.as_str())
            {
                line.push_str(&format!("  objectives={og}"));
            }
            if !outs.is_empty() {
                line.push_str(&format!("  |  {}", outs.join("; ")));
            }
            out.push(line);
        }
        for c in self.stale_connections() {
            out.push(format!(
                "! stale connection {:016X}: {:016X}.{:016X} -> {:016X}.{:016X} (endpoint not in this asset)",
                c.id, c.source_node, c.source_plug, c.target_node, c.target_plug
            ));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// DDL readers
// ---------------------------------------------------------------------------

fn open_dat1(asset: &[u8]) -> Result<Dat1> {
    let magic_at = |o: usize| {
        asset.len() >= o + 4 && u32::from_le_bytes(asset[o..o + 4].try_into().unwrap()) == DAT1_MAGIC
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

/// Every struct value of a named field, in file order. The decoder already
/// read the whole array, so this is just a projection.
fn structs_of<'a>(obj: &'a PObject, field: &str) -> Vec<&'a PObject> {
    obj.field_named(field)
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

fn uint_of(obj: &PObject, name: &str) -> Option<u64> {
    let f = obj.field_named(name)?;
    Some(match f.values.first()? {
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

fn f32_of(obj: &PObject, name: &str) -> f32 {
    obj.field_named(name)
        .and_then(|f| f.values.first())
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as f32
}

fn str_of(obj: &PObject, name: &str) -> Option<String> {
    match obj.field_named(name)?.values.first()? {
        PValue::Str { value, .. } if !value.is_empty() => Some(value.clone()),
        _ => None,
    }
}

/// Is this field one of the authoring tool's `"<base> <i>"` per-element aliases, given
/// the set of field names the node actually has?
///
/// The alias repeats an array element's `Id` verbatim and adds nothing, so it is
/// a duplicate port. Only treated as an alias when the base field is really
/// present, so a designer-authored field that happens to end in a number is
/// safe.
fn is_plug_alias(name: &str, siblings: &BTreeSet<&str>) -> bool {
    let Some((base, idx)) = name.rsplit_once(' ') else { return false };
    !idx.is_empty() && idx.bytes().all(|b| b.is_ascii_digit()) && siblings.contains(base)
}

/// A plug sub-struct is any struct value whose `Type` is one of the two plug
/// members. The field name is the port label.
///
/// Per-element alias fields are skipped ([`is_plug_alias`]) and any id that
/// still turns up twice on one node is kept only once, so the port list is the
/// node's real ports: 1,613 across the 60 shipped graphs.
fn read_plugs(obj: &PObject) -> Vec<Plug> {
    let siblings: BTreeSet<&str> = obj.fields.iter().map(|f| f.name.as_str()).collect();
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    let mut out = Vec::new();
    for f in &obj.fields {
        if is_plug_alias(&f.name, &siblings) {
            continue;
        }
        for (i, v) in f.values.iter().enumerate() {
            let PValue::Struct(inner) = v else { continue };
            let Some(kind) = str_of(inner, "Type") else { continue };
            if kind != "kInputPlug" && kind != "kOutputPlug" {
                continue;
            }
            let Some(id) = uint_of(inner, "Id") else { continue };
            if !seen.insert(id) {
                continue;
            }
            out.push(Plug {
                label: f.name.clone(),
                kind,
                id,
                index: i,
                detail: str_of(inner, "SyncLabel"),
            });
        }
    }
    out
}

fn read_node(buffer_tag: u32, slot_hash: u32, obj: &PObject) -> Node {
    let slot = slot_name(slot_hash);
    let class = slot_class(slot_hash);
    let name = str_of(obj, "ObjectiveName")
        .or_else(|| str_of(obj, "MissionName"))
        .or_else(|| str_of(obj, "DebugName"))
        .or_else(|| str_of(obj, "EffectName"))
        .or_else(|| str_of(obj, "SyncLabel"))
        .unwrap_or_default();
    Node {
        id: uint_of(obj, "Id").unwrap_or(0),
        slot_hash,
        slot,
        class,
        buffer_tag,
        x: f32_of(obj, "X"),
        y: f32_of(obj, "Y"),
        name,
        plugs: read_plugs(obj),
        params: obj.to_json(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::crc32;

    /// Every fixed section tag is the Insomniac crc32 of a readable name, not a
    /// magic number.
    #[test]
    fn section_tags_are_crc32_of_their_names() {
        for (tag, name) in FIXED_SECTIONS {
            assert_eq!(crc32::hash(name), *tag, "{name}");
        }
    }

    #[test]
    fn graph_types_are_crc32_of_their_names() {
        for (hash, name) in GRAPH_TYPES {
            assert_eq!(crc32::hash(name), *hash, "{name}");
        }
    }

    /// Both name columns of every slot row re-hash to the id they claim: the
    /// slot field id, and the element struct's DDL type id. A row with no
    /// established class name carries `(0, "")` and is skipped.
    #[test]
    fn node_slots_are_crc32_of_their_names() {
        for (hash, slot, class_id, class) in NODE_SLOTS {
            assert_eq!(crc32::hash(slot), *hash, "slot {slot}");
            if *class_id == 0 {
                assert!(class.is_empty(), "class {class} has no id");
            } else {
                assert_eq!(crc32::hash(class), *class_id, "class {class}");
            }
        }
    }

    #[test]
    fn slot_table_is_sorted_and_unique() {
        let mut prev = 0u32;
        for (hash, slot, _, _) in NODE_SLOTS {
            assert!(*hash > prev, "slot table out of order at {slot}");
            prev = *hash;
        }
    }

    /// `"Inputs 0"` beside `"Inputs"` is an alias; `"Inputs"` on its own, and a
    /// field whose base name the node does not have, are not.
    #[test]
    fn plug_aliases_are_only_dropped_when_the_base_field_exists() {
        let sibs: BTreeSet<&str> = ["Inputs", "SyncOutput", "Stage 2"].into_iter().collect();
        assert!(is_plug_alias("Inputs 0", &sibs));
        assert!(is_plug_alias("SyncOutput 12", &sibs));
        assert!(!is_plug_alias("Inputs", &sibs));
        assert!(!is_plug_alias("Stage 2", &sibs));
        assert!(!is_plug_alias("Complete 0", &sibs));
        assert!(!is_plug_alias("Inputs x", &sibs));
    }

    #[test]
    fn plug_display_prefers_the_sync_label() {
        let p = Plug {
            label: "SyncOutput".into(),
            kind: "kOutputPlug".into(),
            id: 1,
            index: 1,
            detail: Some("First_Pirates_Dead".into()),
        };
        assert_eq!(p.display(), "SyncOutput \"First_Pirates_Dead\"");
        let q = Plug { detail: None, ..p.clone() };
        assert_eq!(q.display(), "SyncOutput[1]");
        let r = Plug { index: 0, ..q };
        assert_eq!(r.display(), "SyncOutput");
    }

    #[test]
    fn ref_tags_name_the_two_shipped_kinds() {
        assert_eq!(crc32::hash(".config"), REF_TAG_CONFIG);
        assert_eq!(crc32::hash(".nodegraph"), REF_TAG_NODEGRAPH);
    }

    #[test]
    fn lookups_resolve_known_entries_only() {
        assert_eq!(graph_type_name(0xD2C5_E2D6), Some("ObjectiveGraphGameDef"));
        assert_eq!(graph_type_name(1), None);
        assert_eq!(slot_name(0x4187_529D), Some("ObjectiveNodes"));
        assert_eq!(slot_class(0x4187_529D), Some("ObjectiveNodeGameDef"));
        assert_eq!(slot_name(0xFAFE_6B04), Some("OpenWorldActivityNodes"));
        assert_eq!(slot_class(0xFAFE_6B04), None);
        assert_eq!(slot_name(1), None);
    }
}
