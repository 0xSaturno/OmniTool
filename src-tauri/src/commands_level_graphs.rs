//! Tauri commands for the two asset families that hold *behaviour* rather than
//! geometry: `.nodegraph` (mission and objective progression) and `.conduit`
//! (the event → VFX/SFX routing table).
//!
//! | command                   | answers                                                              |
//! |---------------------------|----------------------------------------------------------------------|
//! | `level_list_graphs`       | the 60 shipped nodegraphs, with type, node / edge / stale-edge counts |
//! | `level_nodegraph`         | one graph decoded: nodes, ports, edges, refs, stale edges             |
//! | `level_nodegraph_node`    | one node's full payload, as JSON                                      |
//! | `level_nodegraph_outline` | the same graph as a text listing, for copy-paste and diffs            |
//! | `level_list_conduits`     | the 1,070 shipped conduits, with their response-type mix              |
//! | `level_conduit`           | one conduit decoded: events, filters, selectors, responses, refs      |
//! | `level_conduit_search`    | every conduit whose constants mention a string                        |
//!
//! All seven are read-only. Nothing here writes to the game directory, and
//! neither family has a writer yet — a conduit edit would be an equal-length
//! string patch inside `Conduit Built`, which is not wired up.
//!
//! # Cost
//!
//! `level_conduit_search` and `level_list_conduits` sweep every one of the 1,070
//! conduits. Measured on the shipped build in a release build: **0.33 s** for
//! the whole sweep, 13.9 MB of `Conduit Built` in total, so neither needs a
//! cache. `level_list_graphs` sweeps all 60 nodegraphs in 0.22 s.
//!
//! The decode itself, and the evidence behind every claim, lives in
//! [`crate::core::conduit`] and [`crate::core::nodegraph`].

use std::sync::Arc;
use std::time::Instant;

use log::info;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::conduit::{self, Conduit};
use crate::core::dat1::{Dat1, DAT1_MAGIC};
use crate::core::error::ToolkitError;
use crate::core::level::{self, LevelIndex};
use crate::core::nodegraph::NodeGraph;

type Result<T> = std::result::Result<T, ToolkitError>;

/// The 36-byte header the TOC prepends to an extracted asset.
const ASSET_HEADER_LEN: usize = 36;

fn index(game_dir: &str, hashes_path: Option<String>) -> Result<Arc<LevelIndex>> {
    level::get_index(game_dir, hashes_path.as_deref())
}

/// Open an extracted asset as a `DAT1`, with or without the TOC header.
fn open_dat1(bytes: &[u8]) -> Result<Dat1> {
    let magic_at = |o: usize| {
        bytes.len() >= o + 4 && u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap()) == DAT1_MAGIC
    };
    if magic_at(0) {
        Dat1::parse(bytes)
    } else if magic_at(ASSET_HEADER_LEN) {
        Dat1::parse(&bytes[ASSET_HEADER_LEN..])
    } else {
        Err(ToolkitError::Parse("not a DAT1 container".into()))
    }
}

/// Every asset in the TOC whose name ends with `suffix`, as `(id, name)`,
/// sorted by name. Only assets with a non-zero size, i.e. really present.
fn shipped_with_suffix(idx: &LevelIndex, suffix: &str) -> Vec<(u64, String)> {
    let mut out: Vec<(u64, String)> = idx
        .names
        .iter()
        .filter(|(id, name)| {
            name.to_ascii_lowercase().ends_with(suffix)
                && idx.asset(**id).is_some_and(|a| a.size > 0)
        })
        .map(|(id, name)| (*id, name.clone()))
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

/// Resolve an asset by path the way the game does, then extract it.
fn extract_by_path(idx: &LevelIndex, path: &str) -> Result<(u64, Vec<u8>)> {
    let norm = crate::core::crc64::normalize_path(path);
    let id = crate::core::crc64::hash_raw(norm.as_bytes());
    match idx.extract(id) {
        Ok(bytes) => Ok((id, bytes)),
        // Fall back to a case-insensitive suffix match on the name list, so a
        // UI can pass the leaf name a listing showed it.
        Err(e) => {
            let want = path.to_ascii_lowercase().replace('\\', "/");
            let hit = idx
                .names
                .iter()
                .find(|(i, n)| {
                    let l = n.to_ascii_lowercase().replace('\\', "/");
                    (l == want || l.ends_with(&format!("/{want}")))
                        && idx.asset(**i).is_some_and(|a| a.size > 0)
                })
                .map(|(i, _)| *i);
            match hit {
                Some(i) => Ok((i, idx.extract(i)?)),
                None => Err(e),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Nodegraph DTOs
// ---------------------------------------------------------------------------

/// One row of the graph picker.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphListEntry {
    pub path: String,
    pub asset_id: String,
    /// `"MissionGraphGameDef"`, `"ObjectiveGraphGameDef"`, `"NodalShotGraphDef"`.
    pub graph_type: String,
    pub nodes: usize,
    pub connections: usize,
    /// Connections whose endpoints all resolve inside the asset.
    pub edges: usize,
    /// Connections pointing at something not in the asset. Non-zero on 2 of the
    /// 60 shipped graphs; see [`nodegraph::NodeGraph::stale_connections`].
    pub stale_edges: usize,
    pub bytes: u64,
}

/// One port, flattened for the front end.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlugDto {
    /// What to draw on the port — the `SyncLabel` when the plug has one.
    pub label: String,
    /// The raw payload field name.
    pub field: String,
    pub input: bool,
    pub id: String,
}

/// One node, without its full payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNodeDto {
    pub index: usize,
    pub id: String,
    /// Element struct name when known, else the slot name, else what the node
    /// is (a flattened sub-graph instance).
    pub kind: String,
    /// The graph-struct array slot, when the slot table names it.
    pub slot: Option<String>,
    /// True for the 17 shipped nodes that belong to no named slot.
    pub flattened_instance: bool,
    /// Best human label: `ObjectiveName`, `MissionName`, or `DebugName`.
    pub name: String,
    /// Authoring canvas position — lay the graph out with it directly.
    pub x: f32,
    pub y: f32,
    pub plugs: Vec<PlugDto>,
    /// `Checkpoint`, `TaskMode`, `UIMode`, … the fields a designer reads.
    pub highlights: Vec<(String, String)>,
    /// Assets this node's payload names by path, e.g. the objective graph a
    /// mission node hands off to.
    pub links: Vec<String>,
}

/// One edge, resolved to node indices and port labels.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEdgeDto {
    pub from: usize,
    pub from_plug: String,
    pub to: usize,
    pub to_plug: String,
}

/// A connection the asset cannot resolve on its own.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaleEdgeDto {
    pub id: String,
    pub source_node: String,
    pub source_plug: String,
    pub target_node: String,
    pub target_plug: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphRefDto {
    pub asset_id: String,
    pub path: String,
    /// `".config"` or `".nodegraph"`, from the row's type tag.
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeGraphDto {
    pub path: String,
    pub graph_type: String,
    pub graph_type_hash: String,
    /// The header's own plug count. It counts the authoring tool's per-element alias
    /// fields, so it is an upper bound on `plugs`.
    pub approximate_plug_count: u64,
    /// Real ports, after the aliases are dropped.
    pub plugs: usize,
    pub nodes: Vec<GraphNodeDto>,
    pub edges: Vec<GraphEdgeDto>,
    pub stale_edges: Vec<StaleEdgeDto>,
    pub refs: Vec<GraphRefDto>,
    /// Node indices with no inbound edge — where the graph starts.
    pub roots: Vec<usize>,
    /// Node class -> count.
    pub class_counts: Vec<(String, usize)>,
    /// `Node Graph Built Debug` row count.
    pub id_mappings: usize,
    /// Section tags that are neither a fixed section nor a node payload. Empty
    /// on every shipped graph; anything here means the format moved.
    pub unclaimed_sections: Vec<String>,
}

/// One node with everything in it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNodeDetailDto {
    pub node: GraphNodeDto,
    /// The payload section tag — also the node's DDL blob.
    pub buffer_tag: String,
    /// The whole payload, field-named out of the asset's own string pool.
    pub params: Value,
    pub incoming: Vec<GraphEdgeDto>,
    pub outgoing: Vec<GraphEdgeDto>,
}

// ---------------------------------------------------------------------------
// Conduit DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConduitListEntry {
    pub path: String,
    pub asset_id: String,
    pub events: usize,
    pub responses: usize,
    /// `ECMResponseType` member -> count, so a picker can say "3 VFX, 2 sounds".
    pub response_counts: Vec<(String, usize)>,
    /// Distinct `.visualeffect` assets this conduit can spawn.
    pub visual_effects: usize,
    /// Distinct Wwise events it can post.
    pub sound_events: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MappingDto {
    /// `"FXPackage = foo.visualeffect"` — the line a modder edits.
    pub line: String,
    pub dest: String,
    pub dest_kind: String,
    pub source: String,
    pub source_kind: String,
    pub op: String,
    /// The source is a literal, so this row is editable in place.
    pub constant: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseDto {
    pub name: String,
    /// `ECMResponseType` member, e.g. `"kVFX"`.
    pub kind: String,
    /// Designer-facing label, e.g. `"Visual Effect"`.
    pub kind_label: String,
    pub group_index: u32,
    /// One line: what this does and with what.
    pub summary: String,
    /// The `.visualeffect` a `kVFX` response spawns.
    pub vfx_package: Option<String>,
    /// `(name, id)` of a `kSound` response's Wwise event.
    pub sound_event: Option<(String, u32)>,
    /// Does that id derive from that name under
    /// [`conduit::wwise_event_id`]? `None` when there is no pair.
    pub sound_id_ok: Option<bool>,
    /// Joint or locator the effect hangs off.
    pub attach: Option<String>,
    pub follows: bool,
    /// `StartDelay`, `FadeIn`, `FadeOut`, `MaxInstances` when the response sets
    /// them.
    pub timing: Vec<(String, String)>,
    pub field_map: Vec<MappingDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterDto {
    pub name: String,
    pub scope: String,
    pub continue_type: String,
    /// `"Label == laser_beam_main"` per selector — the guard on this filter.
    pub conditions: Vec<String>,
    pub responses: Vec<ResponseDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventDto {
    /// The game-event name the runtime dispatches, e.g. `"WeaponFiredEvent"`.
    pub name: String,
    /// `crc32(name)` — what the runtime matches on.
    pub name_hash: String,
    /// The `Events` map field id.
    pub key: String,
    /// Does the key agree with the name? True on every shipped event.
    pub key_ok: bool,
    pub filters: Vec<FilterDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConduitRefDto {
    pub asset_id: String,
    pub path: String,
    /// `".visualeffect"`, `".material"` or `"wwise event"`, from the type tag.
    pub kind: String,
    /// For a file row, does `asset_id` match `path`?
    pub id_ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConduitDto {
    pub path: String,
    pub asset_id: String,
    pub events: Vec<EventDto>,
    pub refs: Vec<ConduitRefDto>,
    /// Distinct `.visualeffect` assets, in order of first mention.
    pub visual_effects: Vec<String>,
    /// Distinct Wwise events, as `(name, id)`.
    pub sound_events: Vec<(String, u32)>,
    pub response_counts: Vec<(String, usize)>,
    /// `Conduit Built` size, and how much of it the decoder consumed. Equal on
    /// all 1,070 shipped conduits.
    pub built_bytes: usize,
    pub decoded_bytes: usize,
    pub notes: Vec<String>,
}

/// One hit of [`level_conduit_search`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConduitHitDto {
    pub path: String,
    pub event: String,
    pub filter: String,
    pub response: String,
    pub kind: String,
    /// The field the matching literal was assigned to, e.g. `"FXPackage"`.
    pub dest: String,
    pub value: String,
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

/// Payload fields worth putting on the node card without opening it.
const HIGHLIGHT_FIELDS: &[&str] = &[
    "Checkpoint",
    "TaskMode",
    "UIMode",
    "SegmentComplete",
    "MissionType",
    "MissionName",
    "FallbackMissionName",
    "DemoType",
    "UnlockBehavior",
    "GotoCleanupState",
];

fn node_dto(g: &NodeGraph, i: usize) -> GraphNodeDto {
    let n = &g.nodes[i];
    GraphNodeDto {
        index: i,
        id: format!("{:016X}", n.id),
        kind: n.kind_label(),
        slot: n.slot.map(str::to_string),
        flattened_instance: n.is_flattened_instance(),
        name: if n.name.is_empty() {
            n.param("DebugName").unwrap_or_default()
        } else {
            n.name.clone()
        },
        x: n.x,
        y: n.y,
        plugs: n
            .plugs
            .iter()
            .map(|p| PlugDto {
                label: p.display(),
                field: p.label.clone(),
                input: p.is_input(),
                id: format!("{:016X}", p.id),
            })
            .collect(),
        highlights: HIGHLIGHT_FIELDS
            .iter()
            .filter_map(|f| n.param(f).map(|v| (f.to_string(), v)))
            .collect(),
        links: n.linked_assets(),
    }
}

fn edge_dtos(g: &NodeGraph) -> Vec<GraphEdgeDto> {
    g.edges()
        .into_iter()
        .map(|e| GraphEdgeDto {
            from: e.from,
            from_plug: e.from_plug,
            to: e.to,
            to_plug: e.to_plug,
        })
        .collect()
}

fn graph_dto(path: String, g: &NodeGraph) -> NodeGraphDto {
    NodeGraphDto {
        path,
        graph_type: g.graph_type_name.unwrap_or("unknown").to_string(),
        graph_type_hash: format!("{:#010X}", g.graph_type),
        approximate_plug_count: g.approximate_plug_count,
        plugs: g.nodes.iter().map(|n| n.plugs.len()).sum(),
        nodes: (0..g.nodes.len()).map(|i| node_dto(g, i)).collect(),
        edges: edge_dtos(g),
        stale_edges: g
            .stale_connections()
            .into_iter()
            .map(|c| StaleEdgeDto {
                id: format!("{:016X}", c.id),
                source_node: format!("{:016X}", c.source_node),
                source_plug: format!("{:016X}", c.source_plug),
                target_node: format!("{:016X}", c.target_node),
                target_plug: format!("{:016X}", c.target_plug),
            })
            .collect(),
        refs: g
            .refs
            .iter()
            .map(|r| GraphRefDto {
                asset_id: format!("{:016X}", r.asset_id),
                path: r.path.clone(),
                kind: r.kind().to_string(),
            })
            .collect(),
        roots: g.roots(),
        class_counts: g.class_histogram().into_iter().collect(),
        id_mappings: g.id_mappings.len(),
        unclaimed_sections: g
            .unclaimed_sections
            .iter()
            .map(|t| format!("{t:#010X}"))
            .collect(),
    }
}

fn response_dto(r: &conduit::Response) -> ResponseDto {
    let (delay, fade_in, fade_out, max) = r.timing();
    let timing = [
        ("StartDelay", delay),
        ("FadeIn", fade_in),
        ("FadeOut", fade_out),
        ("MaxInstances", max),
    ]
    .into_iter()
    .filter_map(|(k, v)| v.map(|v| (k.to_string(), v.to_string())))
    .collect();
    ResponseDto {
        name: r.name.clone(),
        kind: r.kind.clone(),
        kind_label: r.kind_label().to_string(),
        group_index: r.group_index,
        summary: r.summary(),
        vfx_package: r.vfx_package().map(str::to_string),
        sound_event: r.sound_event().map(|(n, i)| (n.to_string(), i)),
        sound_id_ok: r.sound_id_derives_from_name(),
        attach: r.attach_point().map(str::to_string),
        follows: r.follows(),
        timing,
        field_map: r
            .field_map
            .iter()
            .map(|m| MappingDto {
                line: m.to_line(),
                dest: m.dest.value.clone(),
                dest_kind: m.dest.kind_label().to_string(),
                source: m.source.value.clone(),
                source_kind: m.source.kind_label().to_string(),
                op: m.op_label().to_string(),
                constant: m.source.is_const(),
            })
            .collect(),
    }
}

fn conduit_dto(path: String, id: u64, c: &Conduit) -> ConduitDto {
    let mut notes = Vec::new();
    if c.decoded_bytes != c.built_bytes {
        notes.push(format!(
            "decoder consumed {} of {} bytes of Conduit Built — the tail is unread",
            c.decoded_bytes, c.built_bytes
        ));
    }
    let bad_keys = c.events.iter().filter(|e| !e.key_matches_name()).count();
    if bad_keys > 0 {
        notes.push(format!(
            "{bad_keys} event(s) whose map key is not crc32 of the decimal of crc32(Name)"
        ));
    }
    let bad_ids = c
        .responses()
        .iter()
        .filter(|(_, _, r)| r.sound_id_derives_from_name() == Some(false))
        .count();
    if bad_ids > 0 {
        notes.push(format!(
            "{bad_ids} sound response(s) whose SoundEventId is not fnv1(lowercase(SoundEventName))"
        ));
    }
    if c.asset_refs.is_empty() {
        notes.push("no Conduit Asset Refs section".into());
    }
    ConduitDto {
        path,
        asset_id: format!("{id:016X}"),
        events: c
            .events
            .iter()
            .map(|e| EventDto {
                name: e.name.clone(),
                name_hash: format!("{:#010X}", e.name_hash()),
                key: format!("{:#010X}", e.key),
                key_ok: e.key_matches_name(),
                filters: e
                    .filters
                    .iter()
                    .map(|f| FilterDto {
                        name: f.name.clone(),
                        scope: f.scope_label().to_string(),
                        continue_type: f.continue_label().to_string(),
                        conditions: f.selectors.iter().map(|s| s.to_line()).collect(),
                        responses: f.responses.iter().map(response_dto).collect(),
                    })
                    .collect(),
            })
            .collect(),
        refs: c
            .asset_refs
            .iter()
            .map(|r| ConduitRefDto {
                asset_id: format!("{:016X}", r.asset_id),
                path: r.path.clone(),
                kind: r.kind().to_string(),
                id_ok: r.id_matches_path(),
            })
            .collect(),
        visual_effects: c.visual_effects().into_iter().map(str::to_string).collect(),
        sound_events: c
            .sound_events()
            .into_iter()
            .map(|(n, i)| (n.to_string(), i))
            .collect(),
        response_counts: c.response_histogram().into_iter().collect(),
        built_bytes: c.built_bytes,
        decoded_bytes: c.decoded_bytes,
        notes,
    }
}

// ---------------------------------------------------------------------------
// Nodegraph commands
// ---------------------------------------------------------------------------

/// Every `.nodegraph` present in the TOC, decoded far enough to describe it.
/// 60 on the shipped build; the sweep is 0.22 s in a release build.
#[tauri::command]
pub async fn level_list_graphs(
    game_dir: String,
    hashes_path: Option<String>,
) -> Result<Vec<GraphListEntry>> {
    let t = Instant::now();
    let idx = index(&game_dir, hashes_path)?;
    let mut out = Vec::new();
    for (id, path) in shipped_with_suffix(&idx, ".nodegraph") {
        let Ok(bytes) = idx.extract(id) else { continue };
        let Ok(d1) = open_dat1(&bytes) else { continue };
        let Ok(g) = NodeGraph::from_dat1(&d1) else { continue };
        out.push(GraphListEntry {
            path,
            asset_id: format!("{id:016X}"),
            graph_type: g.graph_type_name.unwrap_or("unknown").to_string(),
            nodes: g.nodes.len(),
            connections: g.connections.len(),
            edges: g.edges().len(),
            stale_edges: g.stale_connections().len(),
            bytes: idx.asset(id).map(|a| a.size as u64).unwrap_or(0),
        });
    }
    info!("level_list_graphs: {} graphs in {:?}", out.len(), t.elapsed());
    Ok(out)
}

/// One `.nodegraph`, fully decoded. `graph_path` may be the full asset path or
/// the leaf file name a listing showed.
#[tauri::command]
pub async fn level_nodegraph(
    game_dir: String,
    graph_path: String,
    hashes_path: Option<String>,
) -> Result<NodeGraphDto> {
    let idx = index(&game_dir, hashes_path)?;
    let (_, bytes) = extract_by_path(&idx, &graph_path)?;
    let g = NodeGraph::from_dat1(&open_dat1(&bytes)?)?;
    Ok(graph_dto(graph_path, &g))
}

/// One node of one graph, with its whole payload and both edge lists.
#[tauri::command]
pub async fn level_nodegraph_node(
    game_dir: String,
    graph_path: String,
    node_index: usize,
    hashes_path: Option<String>,
) -> Result<GraphNodeDetailDto> {
    let idx = index(&game_dir, hashes_path)?;
    let (_, bytes) = extract_by_path(&idx, &graph_path)?;
    let g = NodeGraph::from_dat1(&open_dat1(&bytes)?)?;
    if node_index >= g.nodes.len() {
        return Err(ToolkitError::Parse(format!(
            "node {node_index} out of range ({} nodes)",
            g.nodes.len()
        )));
    }
    let edges = edge_dtos(&g);
    Ok(GraphNodeDetailDto {
        node: node_dto(&g, node_index),
        buffer_tag: format!("{:#010X}", g.nodes[node_index].buffer_tag),
        params: g.nodes[node_index].params.clone(),
        incoming: edges.iter().filter(|e| e.to == node_index).cloned().collect(),
        outgoing: edges.iter().filter(|e| e.from == node_index).cloned().collect(),
    })
}

/// The graph as a breadth-first text listing from its roots, one line per node,
/// with any stale connection named at the end. Copy-pasteable and diffable.
#[tauri::command]
pub async fn level_nodegraph_outline(
    game_dir: String,
    graph_path: String,
    hashes_path: Option<String>,
) -> Result<String> {
    let idx = index(&game_dir, hashes_path)?;
    let (_, bytes) = extract_by_path(&idx, &graph_path)?;
    let g = NodeGraph::from_dat1(&open_dat1(&bytes)?)?;
    let mut s = format!(
        "{graph_path}\n{} ({:#010X})  nodes {}  connections {}  ports {}\n",
        g.graph_type_name.unwrap_or("unknown"),
        g.graph_type,
        g.nodes.len(),
        g.connections.len(),
        g.nodes.iter().map(|n| n.plugs.len()).sum::<usize>(),
    );
    for r in &g.refs {
        s.push_str(&format!("  ref [{}] {}\n", r.kind(), r.path));
    }
    for line in g.outline() {
        s.push_str(&line);
        s.push('\n');
    }
    Ok(s)
}

// ---------------------------------------------------------------------------
// Conduit commands
// ---------------------------------------------------------------------------

/// Every `.conduit` present in the TOC, with its response mix. 1,070 on the
/// shipped build; the sweep is 0.33 s in a release build.
///
/// `filter` keeps only paths containing it, case-insensitively.
#[tauri::command]
pub async fn level_list_conduits(
    game_dir: String,
    hashes_path: Option<String>,
    filter: Option<String>,
) -> Result<Vec<ConduitListEntry>> {
    let t = Instant::now();
    let idx = index(&game_dir, hashes_path)?;
    let needle = filter.unwrap_or_default().to_ascii_lowercase();
    let mut out = Vec::new();
    for (id, path) in shipped_with_suffix(&idx, ".conduit") {
        if !needle.is_empty() && !path.to_ascii_lowercase().contains(&needle) {
            continue;
        }
        let Ok(bytes) = idx.extract(id) else { continue };
        let Ok(d1) = open_dat1(&bytes) else { continue };
        let Ok(c) = Conduit::from_dat1(&d1) else { continue };
        let counts = c.counts();
        out.push(ConduitListEntry {
            path,
            asset_id: format!("{id:016X}"),
            events: counts.0,
            responses: counts.3,
            response_counts: c.response_histogram().into_iter().collect(),
            visual_effects: c.visual_effects().len(),
            sound_events: c.sound_events().len(),
            bytes: idx.asset(id).map(|a| a.size as u64).unwrap_or(0),
        });
    }
    info!(
        "level_list_conduits: {} conduits in {:?}",
        out.len(),
        t.elapsed()
    );
    Ok(out)
}

/// One `.conduit`, fully decoded: every event, filter, selector and response,
/// with the constants a VFX or sound swap would edit.
#[tauri::command]
pub async fn level_conduit(
    game_dir: String,
    conduit_path: String,
    hashes_path: Option<String>,
) -> Result<ConduitDto> {
    let idx = index(&game_dir, hashes_path)?;
    let (id, bytes) = extract_by_path(&idx, &conduit_path)?;
    let c = Conduit::from_dat1(&open_dat1(&bytes)?)?;
    Ok(conduit_dto(conduit_path, id, &c))
}

/// Every conduit response whose *constant* mentions `needle`, case-insensitively
/// — "which conduit plays this effect / posts this sound", which is the question
/// a modder actually has. `limit` caps the hit list (default 200, 0 means all).
///
/// The match is against the literal on the right-hand side of a field mapping,
/// so it finds `.visualeffect` paths, Wwise event names, material paths and
/// constant names alike. Set `events` to also match the game-event name.
///
/// The cap is applied per asset while sweeping and again to the result, so a
/// capped list is exactly `limit` long but the *last* asset scanned may
/// contribute more hits than a strict streaming cap would have taken.
#[tauri::command]
pub async fn level_conduit_search(
    game_dir: String,
    needle: String,
    hashes_path: Option<String>,
    limit: Option<usize>,
    events: Option<bool>,
) -> Result<Vec<ConduitHitDto>> {
    let t = Instant::now();
    let idx = index(&game_dir, hashes_path)?;
    let needle = needle.to_ascii_lowercase();
    if needle.is_empty() {
        return Err(ToolkitError::Parse("search needs a non-empty needle".into()));
    }
    let cap = limit.unwrap_or(200);
    let match_events = events.unwrap_or(false);
    let mut out: Vec<ConduitHitDto> = Vec::new();
    for (id, path) in shipped_with_suffix(&idx, ".conduit") {
        if cap > 0 && out.len() >= cap {
            break;
        }
        let Ok(bytes) = idx.extract(id) else { continue };
        let Ok(d1) = open_dat1(&bytes) else { continue };
        let Ok(c) = Conduit::from_dat1(&d1) else { continue };
        for (e, f, r) in c.responses() {
            if match_events && e.name.to_ascii_lowercase().contains(&needle) {
                out.push(ConduitHitDto {
                    path: path.clone(),
                    event: e.name.clone(),
                    filter: f.name.clone(),
                    response: r.name.clone(),
                    kind: r.kind.clone(),
                    dest: "<event name>".into(),
                    value: e.name.clone(),
                });
            }
            for (dest, lit) in r.constants() {
                if lit.to_ascii_lowercase().contains(&needle) {
                    out.push(ConduitHitDto {
                        path: path.clone(),
                        event: e.name.clone(),
                        filter: f.name.clone(),
                        response: r.name.clone(),
                        kind: r.kind.clone(),
                        dest: dest.to_string(),
                        value: lit.to_string(),
                    });
                }
            }
        }
    }
    if cap > 0 && out.len() > cap {
        out.truncate(cap);
    }
    info!(
        "level_conduit_search({needle:?}): {} hits in {:?}",
        out.len(),
        t.elapsed()
    );
    Ok(out)
}
