//! Verification harness for `core::zone_script` against the shipped PC files.
//!
//! ```text
//!   cargo run --bin script_probe -- "<game dir>" [hashes.txt] survey
//!   cargo run --bin script_probe -- "<game dir>" [hashes.txt] dump <zone path> [subgraph]
//!   cargo run --bin script_probe -- "<game dir>" [hashes.txt] layout
//!   cargo run --bin script_probe -- "<game dir>" [hashes.txt] orient
//!   cargo run --bin script_probe -- "<game dir>" [hashes.txt] varlayout
//! ```
//!
//! `survey` is the run whose numbers are quoted in `core::zone_script`. It
//! walks every zone in the index and asserts the invariants that pin the
//! layout:
//!
//!   * the plug array is tiled exactly by the out- and var-plug slices;
//!   * `+0x04` of every out-plug row is a valid `Zone Script Actions` row, and
//!     counting inbound edges that way reproduces every node's `in_degree`;
//!   * `+0x08` of every var-plug row is `crc32("In")` or `crc32("Out")`, and
//!     (read as the plug on the *variable*, so inverted from the node's point of
//!   * `Zone Script Vars` divides by 32 and every var-plug index is in range;
//!   * `crc32` of each `Zone Script Strings` entry equals the parallel
//!     `Zone Script String Hashes` word.
//!
//! `layout`, `orient` and `varlayout` are the three measurement passes that
//! *derived* the layout in the first place; they are kept so the derivation can
//! be re-run rather than taken on faith.

use byteorder::{ByteOrder, LE};
use omnitool_lib::core::crc32;
use omnitool_lib::core::level::{self, LevelIndex};
use omnitool_lib::core::zone::{Zone, NO_PLUG, TAG_SCRIPT_ACTIONS, TAG_SCRIPT_PLUGS, TAG_SCRIPT_PRIUSES};
use omnitool_lib::core::zone_script::{
    self, ScriptGraph, NODE_PLUGS, PLUG_NAMES, TAG_SCRIPT_HASHES,
    TAG_SCRIPT_STRINGS, TAG_SCRIPT_STRING_HASHES, TAG_SCRIPT_VARS, VAR_ROW,
};
use std::collections::{BTreeMap, HashMap};

// `commands_level_script.rs` is not declared in `lib.rs` yet -- the exact
// registration lines are in this run's handoff and get wired afterwards -- so
// nothing else would type-check it. Pulling it in here does, and `crate::core`
// inside it resolves through the re-export. Delete both lines once the module is
// declared in `lib.rs`.
pub use omnitool_lib::core;
#[path = "../commands_level_script.rs"]
#[allow(dead_code)]
mod commands_level_script;

const HASHES: &str = r"E:\RCRALevelEditorViewer\refs\RCRA-Forge\hashes.txt";

fn main() {
    let game = std::env::args()
        .nth(1)
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let hashes = std::env::args().nth(2).unwrap_or_else(|| HASHES.to_string());
    let mode = std::env::args().nth(3).unwrap_or_else(|| "survey".into());
    let index = level::get_index(&game, Some(&hashes)).expect("level index");
    println!("index: {} zones\n", index.zones.len());

    let failures = match mode.as_str() {
        "survey" => survey(&index),
        "dump" => {
            dump(&index);
            0
        }
        "layout" => {
            layout(&index);
            0
        }
        "orient" => {
            orient(&index);
            0
        }
        "varlayout" => {
            varlayout(&index);
            0
        }
        other => {
            println!("unknown mode {other}");
            1
        }
    };
    if failures == 0 {
        println!("\nall checks passed");
    } else {
        println!("\n{failures} check(s) FAILED");
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// survey
// ---------------------------------------------------------------------------

fn survey(index: &LevelIndex) -> usize {
    let mut fails = 0usize;
    let mut zones = 0usize;
    let mut scripted = 0usize;
    let mut nodes = 0usize;
    let mut edges = 0usize;
    let mut dangling = 0usize;
    let mut var_rows = 0usize;
    let mut bindings = 0usize;
    let mut subgraphs = 0usize;
    let mut in_ok = 0usize;
    let mut in_bad = 0usize;
    let mut ty_ok = 0usize;
    let mut out_named = 0usize;
    let mut in_named = 0usize;
    let mut var_named = 0usize;
    let mut dir_agree = 0usize;
    let mut dir_disagree = 0usize;
    let mut dir_bad_word = 0usize;
    let mut str_hash_ok = 0usize;
    let mut str_hash_bad = 0usize;
    let mut plug_tiling_ok = 0usize;
    let mut plug_tiling_bad = 0usize;
    let mut var_div_ok = 0usize;
    let mut var_div_bad = 0usize;
    let mut var_idx_oob = 0usize;
    let mut prius_in_range = 0usize;
    let mut prius_oob = 0usize;
    let mut zone_script_hashes_present = 0usize;

    let mut types: BTreeMap<String, usize> = BTreeMap::new();
    let mut types_with_table = 0usize;
    let mut var_types: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut named_vars = 0usize;
    let mut subgraph_sizes: BTreeMap<usize, usize> = BTreeMap::new();
    let mut unresolved_out: BTreeMap<u32, usize> = BTreeMap::new();
    let mut unresolved_var: BTreeMap<u32, usize> = BTreeMap::new();
    let mut biggest: (usize, String) = (0, String::new());
    // Candidate "one small complete graph" zones for the worked example.
    let mut small: Vec<(usize, usize, String)> = Vec::new();

    let table: HashMap<&str, ()> = NODE_PLUGS.iter().map(|p| (p.0, ())).collect();
    // uid vs template_id: how prefab instancing shows up in the flattened zone.
    let mut uid_eq_graph = 0usize;
    let mut uid_eq_graph_bit63 = 0usize;
    let mut uid_other = 0usize;
    let mut uid_bit63 = 0usize;
    let mut graph_bit63 = 0usize;
    let mut graph_reuse: BTreeMap<usize, usize> = BTreeMap::new();
    let mut max_reuse = (0usize, String::new(), 0u64);
    let mut repeated_var_plug = 0usize;
    let mut repeated_out_plug = 0usize;
    // which types own the hashes we cannot name
    let mut unnamed_by_type: BTreeMap<String, usize> = BTreeMap::new();
    let mut types_without_table: BTreeMap<String, usize> = BTreeMap::new();
    let mut declared_lookup_bad = 0usize;

    for z in index.zones.iter() {
        let Ok(raw) = index.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&raw) else { continue };
        zones += 1;
        let Some(g) = ScriptGraph::parse(&zone) else { continue };
        scripted += 1;

        let s = &g.stats;
        nodes += s.nodes;
        edges += s.edges;
        dangling += s.dangling_edges;
        var_rows += s.var_rows;
        bindings += s.var_bindings;
        subgraphs += s.subgraphs;
        in_ok += s.in_count_match;
        in_bad += s.in_count_mismatch;
        ty_ok += s.type_names_resolved;
        out_named += s.out_plug_names_resolved;
        in_named += s.in_plug_names_resolved;
        var_named += s.var_plug_names_resolved;
        dir_agree += s.direction_agree;
        dir_disagree += s.direction_disagree;

        if s.nodes > biggest.0 {
            biggest = (s.nodes, z.path.clone());
        }
        if s.subgraphs == 1 && (6..=16).contains(&s.nodes) {
            small.push((s.nodes, s.var_rows, z.path.clone()));
        }
        for sg in 0..s.subgraphs as u32 {
            *subgraph_sizes
                .entry(g.nodes.iter().filter(|n| n.subgraph == sg).count())
                .or_insert(0) += 1;
        }

        let mut by_graph: BTreeMap<u64, usize> = BTreeMap::new();
        for n in &g.nodes {
            *by_graph.entry(n.template_id).or_insert(0) += 1;
            const B63: u64 = 1 << 63;
            if n.node_id & B63 != 0 {
                uid_bit63 += 1;
            }
            if n.template_id & B63 != 0 {
                graph_bit63 += 1;
            }
            if n.node_id == n.template_id {
                uid_eq_graph += 1;
            } else if n.node_id == n.template_id ^ B63 {
                uid_eq_graph_bit63 += 1;
            } else {
                uid_other += 1;
            }
            let mut seen_v: BTreeMap<u32, usize> = BTreeMap::new();
            for v in &n.vars {
                *seen_v.entry(v.plug_name_hash).or_insert(0) += 1;
            }
            repeated_var_plug += seen_v.values().filter(|&&c| c > 1).count();
            let mut seen_o: BTreeMap<u32, usize> = BTreeMap::new();
            for &e in &n.out_edges {
                *seen_o.entry(g.edges[e as usize].from_plug_hash).or_insert(0) += 1;
            }
            repeated_out_plug += seen_o.values().filter(|&&c| c > 1).count();
            if let Some(t) = &n.type_name {
                *types.entry(t.clone()).or_insert(0) += 1;
                if table.contains_key(t.as_str()) {
                    types_with_table += 1;
                    // NODE_PLUGS is binary-searched by name in the library, so
                    // check the sort order agrees with a plain map lookup.
                    if n.declared_plugs().is_none() {
                        declared_lookup_bad += 1;
                    }
                } else {
                    *types_without_table.entry(t.clone()).or_insert(0) += 1;
                }
                let bad = n.vars.iter().filter(|v| v.plug_name.is_none()).count()
                    + n.out_edges.iter().filter(|&&e| g.edges[e as usize].from_plug.is_none()).count();
                if bad > 0 {
                    *unnamed_by_type.entry(t.clone()).or_insert(0) += bad;
                }
            }
            for v in &n.vars {
                if v.plug_name.is_none() {
                    *unresolved_var.entry(v.plug_name_hash).or_insert(0) += 1;
                }
                if v.direction == "?" {
                    dir_bad_word += 1;
                }
            }
        }
        for (_, c) in by_graph.iter() {
            *graph_reuse.entry(*c).or_insert(0) += 1;
            if *c > max_reuse.0 {
                max_reuse = (*c, z.path.clone(), 0);
            }
        }
        for e in &g.edges {
            if e.from_plug.is_none() {
                *unresolved_out.entry(e.from_plug_hash).or_insert(0) += 1;
            }
        }
        for v in &g.vars {
            *var_types.entry(v.ty.name()).or_insert(0) += 1;
            if v.global_name.is_some() {
                named_vars += 1;
            }
        }

        // ---- raw invariants ---------------------------------------------------
        let act = zone.dat1.get_section_data(TAG_SCRIPT_ACTIONS).unwrap_or(&[]);
        let n_act = act.len() / 40;
        let plugs = zone.dat1.get_section_data(TAG_SCRIPT_PLUGS).unwrap_or(&[]);
        let n_plug = plugs.len() / 12;
        let vars = zone.dat1.get_section_data(TAG_SCRIPT_VARS).unwrap_or(&[]);
        let n_var = vars.len() / VAR_ROW;
        let priuses = zone.dat1.get_section_data(TAG_SCRIPT_PRIUSES).unwrap_or(&[]);
        if zone.dat1.get_section_data(TAG_SCRIPT_HASHES).is_some() {
            zone_script_hashes_present += 1;
        }
        if vars.is_empty() || vars.len() % VAR_ROW == 0 {
            var_div_ok += 1;
        } else {
            var_div_bad += 1;
        }

        // plug slices tile the plug array exactly
        let mut claimed = vec![0u8; n_plug];
        for i in 0..n_act {
            let r = &act[i * 40..i * 40 + 40];
            for (idx, cnt) in [
                (LE::read_u16(&r[18..20]), LE::read_u16(&r[20..22])),
                (LE::read_u16(&r[22..24]), LE::read_u16(&r[24..26])),
            ] {
                if idx == NO_PLUG {
                    continue;
                }
                for p in 0..cnt as usize {
                    if let Some(c) = claimed.get_mut(idx as usize + p) {
                        *c += 1;
                    }
                }
            }
            let po = LE::read_u32(&r[32..36]) as usize;
            let ps = LE::read_u32(&r[36..40]) as usize;
            if po + ps <= priuses.len() {
                prius_in_range += 1;
            } else {
                prius_oob += 1;
            }
        }
        if claimed.iter().all(|&c| c == 1) {
            plug_tiling_ok += 1;
        } else {
            plug_tiling_bad += 1;
        }

        for n in &g.nodes {
            for v in &n.vars {
                if v.var_index as usize >= n_var {
                    var_idx_oob += 1;
                }
            }
        }

        // Zone Script String Hashes parallel to Zone Script Strings
        let strs = zone.dat1.get_section_data(TAG_SCRIPT_STRINGS).unwrap_or(&[]);
        let sh = zone
            .dat1
            .get_section_data(TAG_SCRIPT_STRING_HASHES)
            .unwrap_or(&[]);
        if sh.len() == strs.len() {
            let table = zone_script::string_table(&zone);
            for (i, hb) in sh.chunks_exact(4).enumerate() {
                match table.get(i) {
                    Some(s) if crc32::hash(s) == LE::read_u32(hb) => str_hash_ok += 1,
                    _ => str_hash_bad += 1,
                }
            }
        } else {
            str_hash_bad += 1;
        }
    }

    println!("== zone script survey ==");
    println!("zones parsed                    {zones}");
    println!("zones carrying a script graph   {scripted}");
    println!("nodes                           {nodes}");
    println!("edges                           {edges}  ({dangling} dangling)");
    println!("variables                       {var_rows}  ({named_vars} global/named)");
    println!("var bindings                    {bindings}");
    println!("connected subgraphs             {subgraphs}");
    println!();
    println!("CONNECTIVITY");
    println!(
        "  in_degree == rebuilt inbound edges   {in_ok}/{} ({:.4}%)  mismatches {in_bad}",
        in_ok + in_bad,
        100.0 * in_ok as f64 / (in_ok + in_bad).max(1) as f64
    );
    println!("  out-plug rows pointing outside the action table   {dangling}");
    println!(
        "  var-plug rows pointing outside Zone Script Vars   {var_idx_oob}"
    );
    println!(
        "  var-plug direction word is crc32(\"In\")/(\"Out\")   {}/{} (other: {dir_bad_word})",
        bindings - dir_bad_word,
        bindings
    );
    println!(
        "  direction agrees with the authored label   {dir_agree} agree / {dir_disagree} disagree"
    );
    println!();
    println!("LAYOUT INVARIANTS");
    println!("  plug slices tile Zone Script Plugs exactly   {plug_tiling_ok} zones ok / {plug_tiling_bad} bad");
    println!("  Zone Script Vars length divides by 32        {var_div_ok} zones ok / {var_div_bad} bad");
    println!("  prius span inside Zone Script Priuses        {prius_in_range} ok / {prius_oob} bad");
    println!("  crc32(script string) == parallel hash word   {str_hash_ok} ok / {str_hash_bad} bad");
    println!("  zones carrying 'Zone Script Hashes' 0xD266157E   {zone_script_hashes_present}");
    println!();
    println!("NAMES");
    println!(
        "  node type names resolved from the zone's own strings   {ty_ok}/{nodes} ({:.2}%), {} distinct",
        100.0 * ty_ok as f64 / nodes.max(1) as f64,
        types.len()
    );
    println!(
        "  node types with a per-type plug table ({} types known)  {types_with_table}/{nodes} ({:.2}%)",
        NODE_PLUGS.len(),
        100.0 * types_with_table as f64 / nodes.max(1) as f64
    );
    println!(
        "  out-plug (source) names resolved from the {} name list  {out_named}/{edges} ({:.2}%)",
        PLUG_NAMES.len(),
        100.0 * out_named as f64 / edges.max(1) as f64
    );
    println!(
        "  in-plug (target) names resolved                          {in_named}/{edges} ({:.2}%)",
        100.0 * in_named as f64 / edges.max(1) as f64
    );
    println!(
        "  var-plug names resolved                                  {var_named}/{bindings} ({:.2}%)",
        100.0 * var_named as f64 / bindings.max(1) as f64
    );
    println!("  unresolved distinct hashes: {} out-plug, {} var-plug", unresolved_out.len(), unresolved_var.len());
    println!();
    println!("variable types: {var_types:?}");
    println!("largest graph: {} nodes in {}", biggest.0, biggest.1);
    let mut sizes: Vec<_> = subgraph_sizes.into_iter().filter(|(k, _)| *k > 0).collect();
    sizes.sort();
    let small_count: usize = sizes.iter().filter(|(k, _)| *k <= 4).map(|(_, v)| v).sum();
    let big_count: usize = sizes.iter().filter(|(k, _)| *k > 4).map(|(_, v)| v).sum();
    println!("subgraph size histogram: {small_count} of size <=4, {big_count} larger, largest {}",
        sizes.last().map(|(k, _)| *k).unwrap_or(0));
    small.sort();
    println!("\nsmall single-subgraph zones (candidates for a worked example):");
    for (n, v, p) in small.iter().take(12) {
        println!("  {n:3} nodes  {v:3} vars  {p}");
    }
    println!();
    println!("NODE IDENTITY (uid vs graph uid)");
    println!(
        "  uid == template_id                       {uid_eq_graph}/{nodes} ({:.2}%)",
        100.0 * uid_eq_graph as f64 / nodes.max(1) as f64
    );
    println!(
        "  uid == template_id with bit 63 flipped   {uid_eq_graph_bit63}/{nodes} ({:.2}%)",
        100.0 * uid_eq_graph_bit63 as f64 / nodes.max(1) as f64
    );
    println!(
        "  unrelated                              {uid_other}/{nodes} ({:.2}%)",
        100.0 * uid_other as f64 / nodes.max(1) as f64
    );
    println!("  uid has bit 63 (placed) {uid_bit63}/{nodes}, template_id has it {graph_bit63}/{nodes}");
    let reuse_1 = graph_reuse.get(&1).copied().unwrap_or(0);
    let reuse_n: usize = graph_reuse.iter().filter(|(k, _)| **k > 1).map(|(_, v)| *v).sum();
    println!(
        "  graph_uids used once in their zone {reuse_1}, reused {reuse_n}, max reuse {} (in {})",
        max_reuse.0, max_reuse.1
    );
    println!("  repeated plug names on one node: {repeated_var_plug} var, {repeated_out_plug} out");
    println!("  NODE_PLUGS binary search missed a type a map lookup found: {declared_lookup_bad}");
    println!();
    println!(
        "top 12 node types with no plug table ({} distinct):",
        types_without_table.len()
    );
    let mut tw: Vec<_> = types_without_table.iter().collect();
    tw.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
    for (n, c) in tw.iter().take(12) {
        println!("  {c:>8}  {n}");
    }
    println!("top 12 node types owning unnamed plug hashes:");
    let mut un: Vec<_> = unnamed_by_type.iter().collect();
    un.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
    for (n, c) in un.iter().take(12) {
        println!("  {c:>8}  {n}");
    }
    println!("\ntop 25 node types:");
    let mut tv: Vec<_> = types.iter().collect();
    tv.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
    for (n, c) in tv.iter().take(25) {
        println!("  {c:>8}  {n}");
    }

    if in_bad != 0 {
        println!("FAIL: {in_bad} nodes whose in_degree does not match the rebuilt edges");
        fails += 1;
    }
    if dir_bad_word != 0 {
        println!("FAIL: {dir_bad_word} var plugs whose direction word is neither In nor Out");
        fails += 1;
    }
    if dir_disagree != 0 {
        println!("FAIL: {dir_disagree} var plugs contradict their authored direction label");
        fails += 1;
    }
    if plug_tiling_bad != 0 || var_div_bad != 0 || prius_oob != 0 || str_hash_bad != 0 {
        println!("FAIL: a layout invariant broke");
        fails += 1;
    }
    if declared_lookup_bad != 0 {
        println!("FAIL: NODE_PLUGS is not sorted by type name ({declared_lookup_bad} misses)");
        fails += 1;
    }
    if var_idx_oob != 0 || dangling != 0 {
        println!("FAIL: {var_idx_oob} var / {dangling} node references out of range");
        fails += 1;
    }
    fails
}

// ---------------------------------------------------------------------------
// dump
// ---------------------------------------------------------------------------

fn dump(index: &LevelIndex) {
    let path = std::env::args().nth(4).unwrap_or_default();
    let only: Option<u32> = std::env::args().nth(5).and_then(|s| s.parse().ok());
    let Some(z) = index.zone(&path) else {
        println!("zone not found: {path}");
        return;
    };
    let raw = index.extract(z.id).expect("extract");
    let zone = Zone::parse(&raw).expect("parse");
    match ScriptGraph::parse(&zone) {
        Some(g) => print!("{}", g.dump_text(&z.path, only)),
        None => println!("{}: no script graph", z.path),
    }
}

// ---------------------------------------------------------------------------
// layout / orient / varlayout: the derivation passes
// ---------------------------------------------------------------------------

fn layout(index: &LevelIndex) {
    let mut zones_with_scripts = 0usize;
    let mut actions = 0usize;
    let mut out_rows = 0usize;
    let mut var_rows = 0usize;
    let mut out_idx_candidates = [0usize; 3];
    let mut var_idx_candidates = [0usize; 3];
    let mut inplug_match = [0usize; 3];
    let mut inplug_total = 0usize;
    let mut word_named = [0usize; 3];

    for z in index.zones.iter() {
        let Ok(raw) = index.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&raw) else { continue };
        let Some(act) = zone.dat1.get_section_data(TAG_SCRIPT_ACTIONS) else { continue };
        if act.is_empty() {
            continue;
        }
        zones_with_scripts += 1;
        let n_act = act.len() / 40;
        actions += n_act;
        let plugs = zone.dat1.get_section_data(TAG_SCRIPT_PLUGS).unwrap_or(&[]);
        let vars = zone.dat1.get_section_data(TAG_SCRIPT_VARS).unwrap_or(&[]);

        let mut declared = vec![0u32; n_act];
        let mut seen = [vec![0u32; n_act], vec![0u32; n_act], vec![0u32; n_act]];
        for i in 0..n_act {
            let r = &act[i * 40..i * 40 + 40];
            declared[i] = LE::read_u16(&r[16..18]) as u32;
            let (oi, oc) = (LE::read_u16(&r[18..20]), LE::read_u16(&r[20..22]));
            let (vi, vc) = (LE::read_u16(&r[22..24]), LE::read_u16(&r[24..26]));
            if oi != NO_PLUG {
                for p in 0..oc as usize {
                    let b = (oi as usize + p) * 12;
                    if b + 12 > plugs.len() {
                        continue;
                    }
                    out_rows += 1;
                    for k in 0..3 {
                        let w = LE::read_u32(&plugs[b + k * 4..b + k * 4 + 4]);
                        if (w as usize) < n_act {
                            out_idx_candidates[k] += 1;
                            seen[k][w as usize] += 1;
                        }
                        if zone_script::plug_name(w).is_some() {
                            word_named[k] += 1;
                        }
                    }
                }
            }
            if vi != NO_PLUG {
                for p in 0..vc as usize {
                    let b = (vi as usize + p) * 12;
                    if b + 12 > plugs.len() {
                        continue;
                    }
                    var_rows += 1;
                    for k in 0..3 {
                        let w = LE::read_u32(&plugs[b + k * 4..b + k * 4 + 4]) as usize;
                        if w < (vars.len() / VAR_ROW).max(1) {
                            var_idx_candidates[k] += 1;
                        }
                    }
                }
            }
        }
        for i in 0..n_act {
            inplug_total += 1;
            for k in 0..3 {
                if seen[k][i] == declared[i] {
                    inplug_match[k] += 1;
                }
            }
        }
    }

    println!("zones with script actions {zones_with_scripts}, action rows {actions}");
    println!("out-plug rows {out_rows}, var-plug rows {var_rows}");
    println!("\nOUT PLUG: word is a valid node index (< action count)");
    for k in 0..3 {
        println!("  +0x{:02X}: {}/{}", k * 4, out_idx_candidates[k], out_rows);
    }
    println!("OUT PLUG: word resolves in the {} plug names", PLUG_NAMES.len());
    for k in 0..3 {
        println!("  +0x{:02X}: {}/{}", k * 4, word_named[k], out_rows);
    }
    println!("in_degree reconstruction, per candidate target-index word");
    for k in 0..3 {
        println!("  +0x{:02X}: {}/{} ({:.4}%)", k * 4, inplug_match[k], inplug_total,
            100.0 * inplug_match[k] as f64 / inplug_total.max(1) as f64);
    }
    println!("VAR PLUG: word small enough to be a var index");
    for k in 0..3 {
        println!("  +0x{:02X}: {}/{}", k * 4, var_idx_candidates[k], var_rows);
    }
}

/// Decides which of `+0x00` / `+0x08` is the source plug and which the target
/// plug, by scoring both assignments against the per-node-type vocabularies.
fn orient(index: &LevelIndex) {
    let mut w0_src_out = (0usize, 0usize);
    let mut w0_dst_in = (0usize, 0usize);
    let mut w2_src_out = (0usize, 0usize);
    let mut w2_dst_in = (0usize, 0usize);
    let mut w0_var = (0usize, 0usize);
    let mut var_dir: BTreeMap<&str, usize> = BTreeMap::new();
    let mut var_row_size: BTreeMap<usize, usize> = BTreeMap::new();

    let tbl: HashMap<&'static str, [&'static [&'static str]; 3]> = NODE_PLUGS
        .iter()
        .map(|&(t, i, o, v)| (t, [i, o, v]))
        .collect();

    for z in index.zones.iter() {
        let Ok(raw) = index.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&raw) else { continue };
        let Some(g) = ScriptGraph::parse(&zone) else { continue };
        let vars = zone.dat1.get_section_data(TAG_SCRIPT_VARS).unwrap_or(&[]);

        for e in &g.edges {
            let src = g.nodes[e.from_node as usize].type_name.as_deref();
            let dst = g
                .nodes
                .get(e.to_node as usize)
                .and_then(|n| n.type_name.as_deref());
            score(&mut w0_src_out, &tbl, src, 1, e.from_plug_hash);
            score(&mut w0_dst_in, &tbl, dst, 0, e.from_plug_hash);
            score(&mut w2_src_out, &tbl, src, 1, e.to_plug_hash);
            score(&mut w2_dst_in, &tbl, dst, 0, e.to_plug_hash);
        }
        let mut max_var = 0usize;
        for n in &g.nodes {
            for v in &n.vars {
                score(&mut w0_var, &tbl, n.type_name.as_deref(), 2, v.plug_name_hash);
                max_var = max_var.max(v.var_index as usize);
                *var_dir.entry(v.direction).or_insert(0) += 1;
            }
        }
        if !vars.is_empty() {
            let mut best = None;
            for d in [16usize, 20, 24, 28, 32, 36, 40, 44, 48, 56, 64, 72, 80] {
                if vars.len() % d == 0 && vars.len() / d > max_var {
                    best = Some(d);
                }
            }
            if let Some(d) = best {
                *var_row_size.entry(d).or_insert(0) += 1;
            }
        }
    }

    let pct = |(a, b): (usize, usize)| 100.0 * a as f64 / (a + b).max(1) as f64;
    println!("OUT PLUG orientation (scored only where the node type has a table):");
    println!("  +0x00 in SOURCE Outputs : {}/{} ({:.2}%)", w0_src_out.0, w0_src_out.0 + w0_src_out.1, pct(w0_src_out));
    println!("  +0x00 in TARGET Inputs  : {}/{} ({:.2}%)", w0_dst_in.0, w0_dst_in.0 + w0_dst_in.1, pct(w0_dst_in));
    println!("  +0x08 in SOURCE Outputs : {}/{} ({:.2}%)", w2_src_out.0, w2_src_out.0 + w2_src_out.1, pct(w2_src_out));
    println!("  +0x08 in TARGET Inputs  : {}/{} ({:.2}%)", w2_dst_in.0, w2_dst_in.0 + w2_dst_in.1, pct(w2_dst_in));
    println!("  var +0x00 in OWNER Vars : {}/{} ({:.2}%)", w0_var.0, w0_var.0 + w0_var.1, pct(w0_var));
    println!("var-plug directions (node point of view): {var_dir:?}");
    println!("largest var row size that still fits every index: {var_row_size:?}");
}

fn score(
    acc: &mut (usize, usize),
    tbl: &HashMap<&'static str, [&'static [&'static str]; 3]>,
    ty: Option<&str>,
    kind: usize,
    hash: u32,
) {
    let Some(ty) = ty else { return };
    let Some(sets) = tbl.get(ty) else { return };
    if sets[kind].is_empty() {
        return;
    }
    if sets[kind].iter().any(|n| crc32::hash(n) == hash) {
        acc.0 += 1;
    } else {
        acc.1 += 1;
    }
}

/// Finds which halfword of the 32-byte var row holds the type and which holds
/// the global-name index.
fn varlayout(index: &LevelIndex) {
    let mut type_slot = [0usize; 16];
    let mut namei_slot = [0usize; 16];
    let mut distinct: Vec<BTreeMap<i16, usize>> = (0..16).map(|_| BTreeMap::new()).collect();
    let mut rows = 0usize;

    for z in index.zones.iter() {
        let Ok(raw) = index.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&raw) else { continue };
        let Some(act) = zone.dat1.get_section_data(TAG_SCRIPT_ACTIONS) else { continue };
        if act.is_empty() {
            continue;
        }
        let vars = zone.dat1.get_section_data(TAG_SCRIPT_VARS).unwrap_or(&[]);
        if vars.is_empty() {
            continue;
        }
        let n_str = zone
            .dat1
            .get_section_data(TAG_SCRIPT_STRINGS)
            .map(|s| s.len() / 4)
            .unwrap_or(0);
        for row in vars.chunks_exact(VAR_ROW) {
            rows += 1;
            for s in 0..16 {
                let v = LE::read_i16(&row[s * 2..s * 2 + 2]);
                if (0..=5).contains(&v) {
                    type_slot[s] += 1;
                }
                if v == -1 || (v >= 0 && (v as usize) < n_str) {
                    namei_slot[s] += 1;
                }
                if distinct[s].len() < 64 {
                    *distinct[s].entry(v).or_insert(0) += 1;
                }
            }
        }
    }
    println!("var rows {rows}");
    for s in 0..16 {
        println!(
            "  i16 @ +0x{:02X}: in 0..=5 {:>8} ({:>6.2}%)   -1 or valid string index {:>8} ({:>6.2}%)   distinct(<=64) {}",
            s * 2,
            type_slot[s], 100.0 * type_slot[s] as f64 / rows.max(1) as f64,
            namei_slot[s], 100.0 * namei_slot[s] as f64 / rows.max(1) as f64,
            distinct[s].len()
        );
    }
}
