//! Tauri commands exposing a zone's script graph to the Level Editor UI.
//!
//! | command                     | answers                                          |
//! |-----------------------------|--------------------------------------------------|
//! | `level_zone_script_graph`   | one zone's nodes, edges, variables and stats      |
//! | `level_zone_script_dump`    | the same graph as a text listing a human can read |
//! | `level_zone_script_node`    | one node in detail, including its declared plugs and its raw prius blob |
//!
//! All three read the shipped archives read-only through the memoised index in
//! [`crate::core::level`], so only the first call per game directory pays for
//! the TOC. The decode itself lives in [`crate::core::zone_script`]; nothing
//! here interprets the format.
//!
//! A big gameplay zone is a big graph (the largest shipped one is 14 707 nodes
//! in `i29_molonoth_gp.zone`), so `level_zone_script_graph` takes an optional
//! `subgraph` and a `node_limit`: the UI can list the connected components
//! cheaply, then fetch one.

use serde::Serialize;

use crate::core::error::ToolkitError;
use crate::core::level::{self, LevelIndex};
use crate::core::zone::Zone;
use crate::core::zone_script::{self, ScriptEdge, ScriptGraph, ScriptStats, ScriptVar};

type Result<T> = std::result::Result<T, ToolkitError>;

fn index(game_dir: &str, hashes_path: Option<String>) -> Result<std::sync::Arc<LevelIndex>> {
    level::get_index(game_dir, hashes_path.as_deref())
}

fn load_zone(index: &LevelIndex, zone_path: &str) -> Result<Zone> {
    let path = crate::core::crc64::normalize_path(zone_path);
    let id = crate::core::crc64::hash_raw(path.as_bytes());
    let raw = index.extract(id)?;
    Ok(Zone::parse(&raw)?)
}

fn graph_of(index: &LevelIndex, zone_path: &str) -> Result<(Zone, ScriptGraph)> {
    let zone = load_zone(index, zone_path)?;
    match ScriptGraph::parse(&zone) {
        Some(g) => Ok((zone, g)),
        None => Err(ToolkitError::Parse(format!(
            "{zone_path} carries no script graph"
        ))),
    }
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// One connected component, for the component picker.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubgraphEntry {
    pub id: u32,
    pub node_count: usize,
    /// Nodes with no inbound signal edge — where the component starts.
    pub root_count: usize,
    /// A short label: the type names of up to three of its root nodes.
    pub label: String,
}

/// A node as the UI wants it: the parsed row plus the plugs its *type* declares,
/// so an editor can draw the pins that are not connected to anything.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeDto {
    pub index: u32,
    pub uid: String,
    pub graph_uid: String,
    pub type_name: Option<String>,
    pub type_name_hash: String,
    pub prius_type: Option<String>,
    pub prius_offset: u32,
    pub prius_size: u32,
    pub subgraph: u32,
    pub in_plug_count: u16,
    pub in_edges: Vec<u32>,
    pub out_edges: Vec<u32>,
    pub vars: Vec<VarBindingDto>,
    /// `select XxxInputs` for this node's type, when the type is known.
    pub declared_inputs: Vec<&'static str>,
    pub declared_outputs: Vec<&'static str>,
    pub declared_vars: Vec<&'static str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VarBindingDto {
    pub plug_name: Option<String>,
    pub plug_name_hash: String,
    pub var_index: u32,
    /// `"in"` (the node reads the variable) or `"out"` (it writes it).
    pub direction: &'static str,
    pub authored_direction: Option<&'static str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptGraphDto {
    pub zone_path: String,
    pub stats: ScriptStats,
    pub subgraphs: Vec<SubgraphEntry>,
    /// Empty when `subgraph` was given and the component was not found.
    pub nodes: Vec<NodeDto>,
    pub edges: Vec<ScriptEdge>,
    pub vars: Vec<ScriptVar>,
    /// True when `node_limit` cut the node list short.
    pub truncated: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeDetailDto {
    pub node: NodeDto,
    /// Edges into and out of this node, already resolved to names.
    pub in_edges: Vec<ScriptEdge>,
    pub out_edges: Vec<ScriptEdge>,
    /// The variables this node is bound to, in binding order.
    pub vars: Vec<Option<ScriptVar>>,
    /// The node's `Zone Script Priuses` blob as hex. Its DDL struct is
    /// `prius_type`; decoding it is the prius work, not this module's.
    pub prius_hex: Option<String>,
}

fn node_dto(g: &ScriptGraph, i: usize) -> NodeDto {
    let n = &g.nodes[i];
    let (di, do_, dv) = n.declared_plugs().unwrap_or((&[], &[], &[]));
    NodeDto {
        index: n.index,
        uid: format!("{:016X}", n.uid),
        graph_uid: format!("{:016X}", n.graph_uid),
        type_name: n.type_name.clone(),
        type_name_hash: format!("{:08X}", n.type_name_hash),
        prius_type: n.prius_type(),
        prius_offset: n.prius_offset,
        prius_size: n.prius_size,
        subgraph: n.subgraph,
        in_plug_count: n.in_plug_count,
        in_edges: n.in_edges.clone(),
        out_edges: n.out_edges.clone(),
        vars: n
            .vars
            .iter()
            .map(|v| VarBindingDto {
                plug_name: v.plug_name.clone(),
                plug_name_hash: format!("{:08X}", v.plug_name_hash),
                var_index: v.var_index,
                direction: v.direction,
                authored_direction: v.authored_direction,
            })
            .collect(),
        declared_inputs: di.to_vec(),
        declared_outputs: do_.to_vec(),
        declared_vars: dv.to_vec(),
    }
}

fn subgraph_entries(g: &ScriptGraph) -> Vec<SubgraphEntry> {
    let mut out: Vec<SubgraphEntry> = Vec::with_capacity(g.stats.subgraphs);
    for id in 0..g.stats.subgraphs as u32 {
        let members: Vec<&_> = g.nodes.iter().filter(|n| n.subgraph == id).collect();
        let roots: Vec<&&_> = members.iter().filter(|n| n.in_edges.is_empty()).collect();
        let label = roots
            .iter()
            .take(3)
            .filter_map(|n| n.type_name.clone())
            .collect::<Vec<_>>()
            .join(" / ");
        out.push(SubgraphEntry {
            id,
            node_count: members.len(),
            root_count: roots.len(),
            label: if label.is_empty() {
                format!("{} nodes", members.len())
            } else {
                label
            },
        });
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.node_count));
    out
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

/// One zone's script graph. `subgraph` restricts the node list to one connected
/// component; `node_limit` caps it (default 4 000, 0 means no cap). The
/// component index is always returned in full, so the UI can offer a picker
/// before paying for a big component.
#[tauri::command]
pub async fn level_zone_script_graph(
    game_dir: String,
    zone_path: String,
    hashes_path: Option<String>,
    subgraph: Option<u32>,
    node_limit: Option<usize>,
) -> Result<ScriptGraphDto> {
    let idx = index(&game_dir, hashes_path)?;
    let (_, g) = graph_of(&idx, &zone_path)?;
    let limit = node_limit.unwrap_or(4_000);

    let picked: Vec<usize> = g
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| subgraph.is_none() || subgraph == Some(n.subgraph))
        .map(|(i, _)| i)
        .collect();
    let truncated = limit > 0 && picked.len() > limit;
    let kept: Vec<usize> = if truncated {
        picked[..limit].to_vec()
    } else {
        picked
    };
    let keep: std::collections::HashSet<u32> = kept.iter().map(|&i| i as u32).collect();

    Ok(ScriptGraphDto {
        zone_path,
        subgraphs: subgraph_entries(&g),
        nodes: kept.iter().map(|&i| node_dto(&g, i)).collect(),
        edges: g
            .edges
            .iter()
            .filter(|e| keep.contains(&e.from_node) || keep.contains(&e.to_node))
            .cloned()
            .collect(),
        vars: g.vars.clone(),
        stats: g.stats.clone(),
        truncated,
    })
}

/// The same graph as a text listing, for a copy-paste / diff view.
#[tauri::command]
pub async fn level_zone_script_dump(
    game_dir: String,
    zone_path: String,
    hashes_path: Option<String>,
    subgraph: Option<u32>,
) -> Result<String> {
    let idx = index(&game_dir, hashes_path)?;
    let (_, g) = graph_of(&idx, &zone_path)?;
    Ok(g.dump_text(&zone_path, subgraph))
}

/// One node with its edges, its bound variables and its raw prius blob.
#[tauri::command]
pub async fn level_zone_script_node(
    game_dir: String,
    zone_path: String,
    node_index: u32,
    hashes_path: Option<String>,
) -> Result<NodeDetailDto> {
    let idx = index(&game_dir, hashes_path)?;
    let (zone, g) = graph_of(&idx, &zone_path)?;
    let i = node_index as usize;
    if i >= g.nodes.len() {
        return Err(ToolkitError::Parse(format!(
            "node {node_index} out of range ({} nodes)",
            g.nodes.len()
        )));
    }
    let n = &g.nodes[i];
    let prius_hex = zone_script::prius_blob(&zone, n).map(|b| {
        let mut s = String::with_capacity(b.len() * 2);
        for x in b {
            s.push_str(&format!("{x:02X}"));
        }
        s
    });
    Ok(NodeDetailDto {
        in_edges: n.in_edges.iter().map(|&e| g.edges[e as usize].clone()).collect(),
        out_edges: n.out_edges.iter().map(|&e| g.edges[e as usize].clone()).collect(),
        vars: n
            .vars
            .iter()
            .map(|v| g.vars.get(v.var_index as usize).cloned())
            .collect(),
        prius_hex,
        node: node_dto(&g, i),
    })
}
