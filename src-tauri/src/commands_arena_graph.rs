//! Arena Editor graph view: a zone's script as nodes, links and sections.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::OnceLock;

use serde_json::Value;

use crate::commands_arena::{short_signal, wave_number};
use crate::core::error::ToolkitError;
use crate::core::zone::{ScriptVar, Zone};
use crate::core::zone_pins::PIN_NAMES;

#[derive(serde::Serialize)]
pub struct GraphParam {
    pub pin: String,
    pub var: usize,
    pub name: Option<String>,
    pub value: String,
    /// The node writes this variable rather than reading it.
    pub write: bool,
}

#[derive(serde::Serialize)]
pub struct GraphNode {
    pub index: usize,
    pub kind: String,
    pub label: Option<String>,
    /// Signal relays only: "emit", "listen", "both" or "idle".
    pub relay: Option<&'static str>,
    pub global: bool,
    pub prius_id: Option<usize>,
    /// Index into `ArenaGraph::signals` for relays.
    pub signal: Option<usize>,
    pub params: Vec<GraphParam>,
    pub section: usize,
    pub node_id: String,
    pub template_id: String,
}

#[derive(serde::Serialize)]
pub struct GraphLink {
    pub from: usize,
    pub from_pin: String,
    pub to: usize,
    pub to_pin: String,
}

#[derive(serde::Serialize)]
pub struct GraphSection {
    pub key: String,
    pub label: String,
    /// "wave", "chunk" or "loose".
    pub kind: &'static str,
    pub nodes: Vec<usize>,
}

/// Relays sharing a name; the engine pairs them by that name, not by wiring.
#[derive(serde::Serialize)]
pub struct GraphSignal {
    pub name: String,
    pub global: bool,
    pub emitters: Vec<usize>,
    pub listeners: Vec<usize>,
}

#[derive(serde::Serialize)]
pub struct ArenaGraph {
    pub nodes: Vec<GraphNode>,
    pub links: Vec<GraphLink>,
    pub sections: Vec<GraphSection>,
    pub signals: Vec<GraphSignal>,
    pub pins_named: usize,
    pub pins_total: usize,
}

#[tauri::command]
pub async fn read_arena_graph(zone_path: String) -> Result<ArenaGraph, ToolkitError> {
    let bytes = std::fs::read(&zone_path)?;
    let zone = Zone::parse(&bytes)?;
    Ok(build_graph(&zone))
}

fn pin_table() -> &'static HashMap<u32, &'static str> {
    static TABLE: OnceLock<HashMap<u32, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| PIN_NAMES.iter().map(|n| (crate::core::crc32::hash(n), *n)).collect())
}

fn pin_name(hash: u32) -> String {
    pin_table()
        .get(&hash)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{hash:08X}"))
}

pub fn build_graph(zone: &Zone) -> ArenaGraph {
    let n = zone.actions.len();
    let plugs = zone.node_plugs();
    let write = crate::core::crc32::hash("In");

    let mut blob_of: HashMap<usize, usize> = HashMap::new();
    for (id, blob) in zone.script_priuses.iter().enumerate() {
        for owner in &blob.owners {
            blob_of.insert(*owner, id);
        }
    }
    let field = |node: usize, key: &str| -> Option<&Value> {
        let blob = zone.script_priuses.get(*blob_of.get(&node)?)?;
        blob.json.get(key)?.get("Value")
    };

    let actor_names: HashMap<u64, &str> = zone
        .actors
        .iter()
        .filter_map(|a| Some((a.instance_id, zone.actor_names.get(a.name_index as usize)?.as_str())))
        .collect();
    let group_names: HashMap<u64, &str> =
        zone.actor_groups.iter().map(|g| (g.id, g.name.as_str())).collect();

    let mut links = Vec::new();
    let mut in_degree = vec![0usize; n];
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut pins_seen = BTreeSet::new();
    for (from, np) in plugs.iter().enumerate() {
        for p in &np.links {
            let to = p.index as usize;
            if to >= n {
                continue;
            }
            in_degree[to] += 1;
            succ[from].push(to);
            pins_seen.insert(p.name_hash);
            pins_seen.insert(p.other_hash);
            links.push(GraphLink {
                from,
                from_pin: pin_name(p.name_hash),
                to,
                to_pin: pin_name(p.other_hash),
            });
        }
        for p in &np.params {
            pins_seen.insert(p.name_hash);
        }
    }

    let mut nodes: Vec<GraphNode> = Vec::with_capacity(n);
    let mut signals: BTreeMap<String, GraphSignal> = BTreeMap::new();
    let mut relay_names: Vec<(usize, String)> = Vec::new();
    for (i, action) in zone.actions.iter().enumerate() {
        let kind = zone.action_type(action).trim_end_matches("Action").to_string();
        let full_name = field(i, "Name").and_then(Value::as_str);
        let global = field(i, "IsGlobal").and_then(Value::as_bool).unwrap_or(false);
        let relay = (kind == "SignalRelay").then(|| {
            match (in_degree[i] > 0, !succ[i].is_empty()) {
                (true, false) => "emit",
                (false, true) => "listen",
                (true, true) => "both",
                (false, false) => "idle",
            }
        });
        if let (Some(role), Some(name)) = (relay, full_name) {
            relay_names.push((i, name.to_string()));
            let sig = signals.entry(name.to_string()).or_insert_with(|| GraphSignal {
                name: name.to_string(),
                global,
                emitters: Vec::new(),
                listeners: Vec::new(),
            });
            if role == "emit" || role == "both" {
                sig.emitters.push(i);
            }
            if role == "listen" || role == "both" {
                sig.listeners.push(i);
            }
        }

        let params = plugs[i]
            .params
            .iter()
            .map(|p| {
                let var = p.index as usize;
                GraphParam {
                    pin: pin_name(p.name_hash),
                    var,
                    name: own_var_name(zone, var).map(short_signal),
                    value: describe_var(zone, var, &actor_names, &group_names),
                    write: p.other_hash == write,
                }
            })
            .collect();

        nodes.push(GraphNode {
            index: i,
            kind,
            label: full_name.map(short_signal),
            relay,
            global,
            prius_id: blob_of.get(&i).copied(),
            signal: None,
            params,
            section: usize::MAX,
            node_id: format!("{:016X}", action.node_id),
            template_id: format!("{:016X}", action.template_id),
        });
    }

    let signals: Vec<GraphSignal> = signals.into_values().collect();
    let signal_index: HashMap<&str, usize> =
        signals.iter().enumerate().map(|(k, s)| (s.name.as_str(), k)).collect();
    for (i, name) in &relay_names {
        nodes[*i].signal = signal_index.get(name.as_str()).copied();
    }

    let sections = build_sections(zone, &mut nodes, &links, &in_degree, &succ);
    let pins_named = pins_seen.iter().filter(|h| pin_table().contains_key(h)).count();
    ArenaGraph {
        nodes,
        links,
        sections,
        signals,
        pins_named,
        pins_total: pins_seen.len(),
    }
}

/// The var's own name — `Zone::var_name` falls back to a string var's value.
fn own_var_name(zone: &Zone, index: usize) -> Option<&str> {
    let v = zone.vars.get(index)?;
    if v.name_index == u16::MAX {
        return None;
    }
    zone.script_types.get(v.name_index as usize).map(String::as_str).filter(|s| !s.is_empty())
}

fn describe_var(
    zone: &Zone,
    index: usize,
    actors: &HashMap<u64, &str>,
    groups: &HashMap<u64, &str>,
) -> String {
    let Some(var) = zone.vars.get(index) else {
        return "?".into();
    };
    match var.value_type {
        ScriptVar::TYPE_INT | ScriptVar::TYPE_FLOAT => {
            var.as_number().map(format_number).unwrap_or_default()
        }
        ScriptVar::TYPE_STRING => zone
            .string_value(index)
            .map(|s| format!("\"{s}\""))
            .unwrap_or_else(|| "runtime".into()),
        5 => match var.id_value() {
            0 => "runtime".into(),
            id => actors
                .get(&id)
                .map(|a| format!("actor {a}"))
                .or_else(|| groups.get(&id).map(|g| format!("group {g}")))
                .unwrap_or_else(|| format!("{id:016X}")),
        },
        _ => "runtime".into(),
    }
}

fn format_number(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        let s = format!("{x:.3}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Waves first (by their script segment), then the rest of the script split
/// into connected chunks named after the signal that starts them.
fn build_sections(
    zone: &Zone,
    nodes: &mut [GraphNode],
    links: &[GraphLink],
    in_degree: &[usize],
    succ: &[Vec<usize>],
) -> Vec<GraphSection> {
    let n = nodes.len();
    let mut section_of = vec![usize::MAX; n];
    let mut sections: Vec<GraphSection> = Vec::new();

    let waves: BTreeSet<u32> = nodes
        .iter()
        .filter(|g| g.relay.is_some())
        .filter_map(|g| g.label.as_deref().and_then(wave_number))
        .collect();
    for w in waves {
        let Ok(seg) = zone.wave_segment(w) else {
            continue;
        };
        let members: Vec<usize> =
            seg.nodes.into_iter().filter(|i| section_of[*i] == usize::MAX).collect();
        if members.is_empty() {
            continue;
        }
        for i in &members {
            section_of[*i] = sections.len();
        }
        sections.push(GraphSection {
            key: format!("wave{w}"),
            label: format!("Wave {w}"),
            kind: "wave",
            nodes: members,
        });
    }

    let mut undirected: Vec<Vec<usize>> = vec![Vec::new(); n];
    for l in links {
        undirected[l.from].push(l.to);
        undirected[l.to].push(l.from);
    }
    let mut seen = vec![false; n];
    let mut chunks: Vec<Vec<usize>> = Vec::new();
    let mut loose: Vec<usize> = Vec::new();
    for start in 0..n {
        if seen[start] || section_of[start] != usize::MAX {
            continue;
        }
        seen[start] = true;
        let mut comp = vec![start];
        let mut queue = VecDeque::from([start]);
        while let Some(x) = queue.pop_front() {
            for y in &undirected[x] {
                if !seen[*y] && section_of[*y] == usize::MAX {
                    seen[*y] = true;
                    comp.push(*y);
                    queue.push_back(*y);
                }
            }
        }
        if comp.len() < 3 {
            loose.extend(comp);
        } else {
            comp.sort_unstable();
            chunks.push(comp);
        }
    }
    chunks.sort_by(|a, b| b.len().cmp(&a.len()));

    for (k, comp) in chunks.into_iter().enumerate() {
        let label = chunk_label(nodes, &comp, in_degree, succ);
        for i in &comp {
            section_of[*i] = sections.len();
        }
        sections.push(GraphSection { key: format!("chunk{k}"), label, kind: "chunk", nodes: comp });
    }
    if !loose.is_empty() {
        loose.sort_unstable();
        for i in &loose {
            section_of[*i] = sections.len();
        }
        sections.push(GraphSection {
            key: "loose".into(),
            label: "Loose nodes".into(),
            kind: "loose",
            nodes: loose,
        });
    }

    for (i, g) in nodes.iter_mut().enumerate() {
        g.section = section_of[i];
    }
    sections
}

/// Name a chunk after the entry point that reaches most of it.
fn chunk_label(nodes: &[GraphNode], comp: &[usize], in_degree: &[usize], succ: &[Vec<usize>]) -> String {
    let inside: std::collections::HashSet<usize> = comp.iter().copied().collect();
    let roots: Vec<usize> = comp.iter().copied().filter(|i| in_degree[*i] == 0).collect();
    let reach = |root: usize| {
        let mut seen = std::collections::HashSet::from([root]);
        let mut stack = vec![root];
        while let Some(x) = stack.pop() {
            for y in &succ[x] {
                if inside.contains(y) && seen.insert(*y) {
                    stack.push(*y);
                }
            }
        }
        seen.len()
    };
    let best = roots
        .iter()
        .copied()
        .max_by_key(|r| (nodes[*r].label.is_some(), reach(*r), std::cmp::Reverse(*r)));
    let Some(best) = best else {
        return format!("{} loop", nodes[comp[0]].kind);
    };
    let kind = &nodes[best].kind;
    let is_event = kind.starts_with("On") || kind == "ScriptStarted" || kind.ends_with("Listener");
    let name = match &nodes[best].label {
        Some(l) => l.clone(),
        None if is_event => kind.clone(),
        // Nothing in the zone fires it.
        None => format!("{kind} (not triggered)"),
    };
    match roots.len() {
        0 | 1 => name,
        k => format!("{name} +{}", k - 1),
    }
}
