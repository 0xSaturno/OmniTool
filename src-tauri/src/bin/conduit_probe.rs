//! Verification probe for `.conduit` and `.nodegraph` assets.
//!
//! Every claim in `core::conduit` and `core::nodegraph` is re-measured here
//! against the shipped PC build. Modes:
//!
//! ```text
//!   census                 what ships, what parses, section totals, struct totals
//!   events                 event-name / filter / selector census
//!   responses              response-type census + the destination fields each uses
//!   refs                   Conduit Asset Refs: file rows vs Wwise event rows
//!   find <text>            conduits whose constants mention <text>
//!   dump <substr>          one conduit, decoded, as a modder would read it
//!   cmpddl                 generic ddl reader vs prius reader, on conduits
//!   ngcensus               nodegraph: per-asset node/edge counts and integrity
//!   ngslots                nodegraph: FieldNameHash histogram and what resolves
//!   ngdump <substr>        one nodegraph, outlined
//!   ngnode <substr> <n>    one node's full payload
//! ```

use std::collections::{BTreeMap, BTreeSet};

use omnitool_lib::core::conduit::{Conduit, CONDUIT_ASSET_REFS_TAG, CONDUIT_BUILT_TAG};
use omnitool_lib::core::crc32;
use omnitool_lib::core::dat1::{Dat1, DAT1_MAGIC};
use omnitool_lib::core::level;
use omnitool_lib::core::nodegraph::{self, NodeGraph};

const GAME: &str = r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart";

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mode = a.first().cloned().unwrap_or_else(|| "census".into());
    let arg = a.get(1).cloned().unwrap_or_default();
    let arg2 = a.get(2).cloned().unwrap_or_default();
    let game = std::env::var("RCRA_GAME").unwrap_or_else(|_| GAME.into());
    let idx = level::get_index(&game, None).expect("index");

    let mut conduits: Vec<(u64, String)> = Vec::new();
    let mut graphs: Vec<(u64, String)> = Vec::new();
    for (id, name) in idx.names.iter() {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".conduit") {
            conduits.push((*id, name.clone()));
        } else if lower.ends_with(".nodegraph") {
            graphs.push((*id, name.clone()));
        }
    }
    conduits.sort_by(|x, y| x.1.cmp(&y.1));
    graphs.sort_by(|x, y| x.1.cmp(&y.1));
    let shipped = |v: &Vec<(u64, String)>| -> Vec<(u64, String)> {
        v.iter()
            .filter(|c| idx.asset(c.0).is_some_and(|a| a.size > 0))
            .cloned()
            .collect()
    };
    let ship_c = shipped(&conduits);
    let ship_g = shipped(&graphs);

    match mode.as_str() {
        "census" => {
            println!("named .conduit {}   in TOC {}", conduits.len(), ship_c.len());
            println!("named .nodegraph {}   in TOC {}", graphs.len(), ship_g.len());

            // ---- conduits -------------------------------------------------
            let mut secs: BTreeMap<u32, (u64, u64)> = BTreeMap::new();
            let mut tot = (0usize, 0usize, 0usize, 0usize, 0usize);
            let (mut ok, mut full, mut keys_ok, mut keys_total) = (0, 0, 0u64, 0u64);
            let mut no_refs: Vec<String> = Vec::new();
            let mut stack_sizes: BTreeMap<u64, usize> = BTreeMap::new();
            let mut id64_ok = 0u64;
            for (id, name) in ship_c.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                for s in &d1.sections {
                    let e = secs.entry(s.tag).or_insert((0, 0));
                    e.0 += 1;
                    e.1 += s.size as u64;
                }
                let Ok(c) = Conduit::from_dat1(&d1) else {
                    println!("  PARSE FAIL {name}");
                    continue;
                };
                ok += 1;
                if c.decoded_bytes == c.built_bytes {
                    full += 1;
                }
                if c.asset_id64 == *id {
                    id64_ok += 1;
                }
                *stack_sizes.entry(c.built_value_stack_size).or_default() += 1;
                let n = c.counts();
                tot.0 += n.0;
                tot.1 += n.1;
                tot.2 += n.2;
                tot.3 += n.3;
                tot.4 += n.4;
                for e in &c.events {
                    keys_total += 1;
                    if e.key_matches_name() {
                        keys_ok += 1;
                    }
                }
                if c.asset_refs.is_empty() {
                    no_refs.push(name.clone());
                }
            }
            println!("\nconduit sections:");
            for (t, (n, b)) in secs.iter() {
                let label = match *t {
                    CONDUIT_BUILT_TAG => "Conduit Built",
                    CONDUIT_ASSET_REFS_TAG => "Conduit Asset Refs",
                    _ => "?",
                };
                println!("  {t:#010X} {label:<20} in {n:>5} assets  {b:>12} bytes");
            }
            println!("\nconduits decoded {ok} of {}", ship_c.len());
            println!("  decoder consumed the whole Conduit Built section: {full} of {ok}");
            println!("  AssetId64 == the asset's own id: {id64_ok} of {ok}");
            println!("  BuiltValueStackSize values: {stack_sizes:?}");
            println!(
                "  ECMEvent {}  ECMFilter {}  ECMSelector {}  ECMResponse {}  ECMFieldMap {}  ECMField {}",
                tot.0,
                tot.1,
                tot.2,
                tot.3,
                tot.4,
                tot.4 * 2 + tot.2 * 2
            );
            println!("  Events map key == crc32(Name): {keys_ok} of {keys_total}");
            println!("  conduits with no Conduit Asset Refs section: {}", no_refs.len());

            // Are the refs-less ones really value-only?
            let mut only_value = 0;
            for name in no_refs.iter() {
                let id = conduits.iter().find(|c| &c.1 == name).unwrap().0;
                let Ok(bytes) = idx.extract(id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Ok(c) = Conduit::from_dat1(&d1) else { continue };
                if c.responses()
                    .iter()
                    .all(|(_, _, r)| matches!(r.kind.as_str(), "kConstant" | "kMaterial"))
                {
                    only_value += 1;
                }
            }
            println!("    of those, every response is kConstant/kMaterial: {only_value}");
            for n in no_refs.iter().take(4) {
                println!("      {n}");
            }

            // ---- nodegraphs ----------------------------------------------
            let mut ngsec: BTreeMap<u32, (u64, u64)> = BTreeMap::new();
            let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
            let (mut ng_ok, mut nodes, mut conns, mut edges, mut bad_n, mut bad_p) =
                (0, 0usize, 0usize, 0usize, 0usize, 0usize);
            let mut unclaimed = 0usize;
            for (id, name) in ship_g.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Ok(g) = NodeGraph::from_dat1(&d1) else {
                    println!("  NG PARSE FAIL {name}");
                    continue;
                };
                ng_ok += 1;
                for s in &d1.sections {
                    let key = if nodegraph::FIXED_SECTIONS.iter().any(|(t, _)| *t == s.tag) {
                        s.tag
                    } else {
                        0
                    };
                    let e = ngsec.entry(key).or_insert((0, 0));
                    e.0 += 1;
                    e.1 += s.size as u64;
                }
                *by_type
                    .entry(g.graph_type_name.unwrap_or("?").to_string())
                    .or_default() += 1;
                nodes += g.nodes.len();
                conns += g.connections.len();
                edges += g.edges().len();
                let (bn, bp) = g.dangling();
                bad_n += bn;
                bad_p += bp;
                unclaimed += g.unclaimed_sections.len();
            }
            println!("\nnodegraphs decoded {ng_ok} of {}", ship_g.len());
            println!("  by graph type: {by_type:?}");
            println!("  nodes {nodes}  connections {conns}  edges resolved {edges}");
            println!("  connections with an unresolvable node id: {bad_n}");
            println!("  connections with an unresolvable plug id:  {bad_p}");
            println!("  section tags that are neither fixed nor a node payload: {unclaimed}");
            println!("  nodegraph sections (tag 0 = per-node payloads):");
            for (t, (n, b)) in ngsec.iter() {
                let label = nodegraph::FIXED_SECTIONS
                    .iter()
                    .find(|(x, _)| x == t)
                    .map(|(_, l)| *l)
                    .unwrap_or("<per-node payload>");
                println!("    {t:#010X} {label:<32} in {n:>5}  {b:>10} bytes");
            }
        }

        "events" => {
            let mut scopes: BTreeMap<String, usize> = BTreeMap::new();
            let mut conts: BTreeMap<String, usize> = BTreeMap::new();
            let mut selops: BTreeMap<String, usize> = BTreeMap::new();
            let mut sel_lhs: BTreeMap<String, usize> = BTreeMap::new();
            let mut evnames: BTreeMap<String, usize> = BTreeMap::new();
            let mut invalid = (0usize, 0usize, 0usize);
            for (id, _) in ship_c.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Ok(c) = Conduit::from_dat1(&d1) else { continue };
                for e in &c.events {
                    *evnames.entry(e.name.clone()).or_default() += 1;
                    if !e.is_valid {
                        invalid.0 += 1;
                    }
                    for f in &e.filters {
                        *scopes.entry(f.scope.clone()).or_default() += 1;
                        *conts.entry(f.continue_type.clone()).or_default() += 1;
                        if !f.is_valid {
                            invalid.1 += 1;
                        }
                        for s in &f.selectors {
                            *selops.entry(s.op.clone()).or_default() += 1;
                            *sel_lhs.entry(s.lhs.value.clone()).or_default() += 1;
                            if !s.is_valid {
                                invalid.2 += 1;
                            }
                        }
                    }
                }
            }
            println!("distinct event names: {}", evnames.len());
            {
                let mut keys_of: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
                let mut names_of: BTreeMap<u32, BTreeSet<String>> = BTreeMap::new();
                for (id, _) in ship_c.iter() {
                    let Ok(bytes) = idx.extract(*id) else { continue };
                    let Some(d1) = parse_dat1(&bytes) else { continue };
                    let Ok(c) = Conduit::from_dat1(&d1) else { continue };
                    for e in &c.events {
                        keys_of.entry(e.name.clone()).or_default().insert(e.key);
                        names_of.entry(e.key).or_default().insert(e.name.clone());
                    }
                }
                let multi = keys_of.values().filter(|s| s.len() > 1).count();
                let multi2 = names_of.values().filter(|s| s.len() > 1).count();
                println!("event names mapping to more than one key: {multi}");
                println!("keys mapping to more than one name:      {multi2}");
                println!("distinct keys: {}", names_of.len());
                for (n, ks) in keys_of.iter().take(10) {
                    let v: Vec<String> = ks.iter().map(|k| format!("{k:#010X}")).collect();
                    println!("   {n:<44} {}", v.join(" "));
                }
                for (n, ks) in keys_of.iter().filter(|(_, s)| s.len() > 1).take(6) {
                    let v: Vec<String> = ks.iter().map(|k| format!("{k:#010X}")).collect();
                    println!("   MULTI {n:<38} {}", v.join(" "));
                }
            }
            top("most common event names", &evnames, 25);
            top("filter Scope", &scopes, 10);
            top("filter ContinueType", &conts, 10);
            top("selector Op", &selops, 10);
            top("selector left operand", &sel_lhs, 20);
            println!("\nIsValid == false: events {} filters {} selectors {}", invalid.0, invalid.1, invalid.2);
        }

        "responses" => {
            let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
            let mut dest_by_kind: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
            let mut srckind: BTreeMap<String, usize> = BTreeMap::new();
            let mut destkind: BTreeMap<String, usize> = BTreeMap::new();
            let mut ops: BTreeMap<String, usize> = BTreeMap::new();
            let mut groups: BTreeMap<u32, usize> = BTreeMap::new();
            for (id, _) in ship_c.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Ok(c) = Conduit::from_dat1(&d1) else { continue };
                for (_, _, r) in c.responses() {
                    *kinds.entry(r.kind.clone()).or_default() += 1;
                    *groups.entry(r.group_index).or_default() += 1;
                    let d = dest_by_kind.entry(r.kind.clone()).or_default();
                    for m in &r.field_map {
                        *d.entry(m.dest.value.clone()).or_default() += 1;
                        *srckind.entry(m.source.kind.clone()).or_default() += 1;
                        *destkind.entry(m.dest.kind.clone()).or_default() += 1;
                        *ops.entry(m.op.clone()).or_default() += 1;
                    }
                }
            }
            top("ECMResponse.Type", &kinds, 20);
            top("FieldMap Dest.Type", &destkind, 10);
            top("FieldMap Source.Type", &srckind, 10);
            top("FieldMap Op", &ops, 12);
            println!("\nGroupIndex histogram: {groups:?}");
            for (k, d) in dest_by_kind.iter() {
                let mut v: Vec<_> = d.iter().collect();
                v.sort_by(|x, y| y.1.cmp(x.1));
                let total: usize = d.values().sum();
                println!("\n{k}: {total} mappings over {} distinct destination fields", d.len());
                for (name, n) in v.iter().take(18) {
                    println!("   {name:<34} {n}");
                }
            }
        }

        "refs" => {
            let (mut rows, mut files, mut wwise, mut zero_id, mut crc_ok) = (0u64, 0u64, 0u64, 0u64, 0u64);
            let mut types: BTreeMap<u32, usize> = BTreeMap::new();
            let mut examples: Vec<String> = Vec::new();
            for (id, _) in ship_c.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Ok(c) = Conduit::from_dat1(&d1) else { continue };
                for r in &c.asset_refs {
                    rows += 1;
                    *types.entry(r.type_id).or_default() += 1;
                    if r.is_wwise_event() {
                        wwise += 1;
                    } else {
                        files += 1;
                        if omnitool_lib::core::crc64::hash(&r.path.to_ascii_lowercase()) == r.asset_id {
                            crc_ok += 1;
                        }
                    }
                    if r.asset_id == 0 {
                        zero_id += 1;
                    }
                    if examples.len() < 12 {
                        examples.push(format!("  {:016X} type {:#010X} {}", r.asset_id, r.type_id, r.path));
                    }
                }
            }
            println!("Conduit Asset Refs rows {rows}: file-shaped {files}, bare event names {wwise}, asset_id==0 {zero_id}");
            println!("file rows whose crc64(lowercased path) == asset_id: {crc_ok} of {files}");
            println!("type tags: {:?}", types.iter().map(|(t, n)| (format!("{t:#010X}"), n)).collect::<Vec<_>>());
            for e in examples {
                println!("{e}");
            }
        }

        "find" => {
            let needle = arg.to_ascii_lowercase();
            let mut hits = 0;
            for (id, name) in ship_c.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Ok(c) = Conduit::from_dat1(&d1) else { continue };
                for (e, f, r) in c.responses() {
                    for (dest, lit) in r.constants() {
                        if lit.to_ascii_lowercase().contains(&needle) {
                            println!("{name}\n   {} / {} / {} [{}]  {dest} = {lit}", e.name, f.name, r.name, r.kind);
                            hits += 1;
                        }
                    }
                }
                if hits > 40 {
                    break;
                }
            }
        }

        "dump" => {
            for (id, name) in ship_c.iter() {
                if !arg.is_empty() && !name.contains(&arg) {
                    continue;
                }
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Ok(c) = Conduit::from_dat1(&d1) else { continue };
                println!("=== {name} ===");
                println!(
                    "{} events, decoded {}/{} bytes, stack {}",
                    c.events.len(),
                    c.decoded_bytes,
                    c.built_bytes,
                    c.built_value_stack_size
                );
                for e in &c.events {
                    println!("\nevent {:#010X} {} (key==crc32(name): {})", e.key, e.name, e.key_matches_name());
                    for f in &e.filters {
                        println!("  filter \"{}\"  scope={} continue={}", f.name, f.scope_label(), f.continue_label());
                        for s in &f.selectors {
                            println!("    when {}", s.to_line());
                        }
                        for r in &f.responses {
                            println!("    do [{}] {}  group {}", r.kind_label(), r.name, r.group_index);
                            for m in &r.field_map {
                                println!("        {:<40} ({} <- {})", m.to_line(), m.dest.kind_label(), m.source.kind_label());
                            }
                        }
                    }
                }
                break;
            }
        }

        "cmpddl" => {
            use omnitool_lib::core::ddl;
            let (mut same, mut differ) = (0usize, 0usize);
            for (id, name) in ship_c.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Some(sec) = d1.get_section_data(CONDUIT_BUILT_TAG) else { continue };
                let Ok(o) = ddl::parse(sec, &d1) else { continue };
                let a = count_ddl(&o);
                let Ok(c) = Conduit::from_dat1(&d1) else { continue };
                let n = c.counts();
                let b = (n.0, n.1, n.2, n.3, n.4);
                if a == b {
                    same += 1;
                } else {
                    differ += 1;
                    if differ <= 6 {
                        println!("  DIFF {name}\n    ddl   {a:?}\n    prius {b:?}");
                    }
                }
            }
            println!("conduits where the two readers agree: {same}, disagree: {differ}");
            // and on nodegraph connections, where they do not
            for (id, name) in ship_g.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Some(sec) = d1.get_section_data(nodegraph::NG_CONNECTIONS_TAG) else { continue };
                let Ok(o) = ddl::parse(sec, &d1) else { continue };
                let ddl_n = o
                    .fields
                    .values()
                    .find(|f| f.name == "Connections")
                    .map(|f| {
                        f.values
                            .iter()
                            .filter(|v| matches!(v, ddl::DdlValue::Struct(s) if !s.fields.is_empty()))
                            .count()
                    })
                    .unwrap_or(0);
                let Ok(g) = NodeGraph::from_dat1(&d1) else { continue };
                if ddl_n != g.connections.len() {
                    println!(
                        "  NG {} ddl={} prius={}",
                        name.rsplit('/').next().unwrap(),
                        ddl_n,
                        g.connections.len()
                    );
                }
            }
        }

        "ngcensus" => {
            let mut classes: BTreeMap<String, usize> = BTreeMap::new();
            let mut plugkinds: BTreeMap<String, usize> = BTreeMap::new();
            let mut plugnames: BTreeMap<String, usize> = BTreeMap::new();
            let mut reftypes: BTreeMap<u32, usize> = BTreeMap::new();
            let mut refpaths: BTreeSet<String> = BTreeSet::new();
            for (id, name) in ship_g.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Ok(g) = NodeGraph::from_dat1(&d1) else { continue };
                let (bn, bp) = g.dangling();
                println!(
                    "{:<58} {:<22} nodes {:>3} conn {:>3} edges {:>3} roots {:>2} plugs~{:<4} refs {:>2} bad {bn}/{bp}",
                    name.rsplit('/').next().unwrap(),
                    g.graph_type_name.unwrap_or("?"),
                    g.nodes.len(),
                    g.connections.len(),
                    g.edges().len(),
                    g.roots().len(),
                    g.approximate_plug_count,
                    g.refs.len(),
                );
                for n in &g.nodes {
                    *classes.entry(n.kind_label()).or_default() += 1;
                    for p in &n.plugs {
                        *plugkinds.entry(p.kind.clone()).or_default() += 1;
                        *plugnames.entry(p.label.clone()).or_default() += 1;
                    }
                }
                for r in &g.refs {
                    *reftypes.entry(r.type_id).or_default() += 1;
                    refpaths.insert(r.path.clone());
                }
            }
            top("node classes", &classes, 40);
            top("plug Type", &plugkinds, 10);
            top("plug port labels", &plugnames, 25);
            println!("\nNode Graph Refs type tags: {:?}", reftypes.iter().map(|(t, n)| (format!("{t:#010X}"), n)).collect::<Vec<_>>());
            println!("distinct referenced assets: {}", refpaths.len());
            for p in refpaths.iter().take(8) {
                println!("   {p}");
            }
        }

        "ngslots" => {
            let mut slots: BTreeMap<u32, (usize, BTreeSet<String>)> = BTreeMap::new();
            for (id, name) in ship_g.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Ok(g) = NodeGraph::from_dat1(&d1) else { continue };
                for n in &g.nodes {
                    let e = slots.entry(n.slot_hash).or_default();
                    e.0 += 1;
                    e.1.insert(name.rsplit('/').next().unwrap().to_string());
                }
            }
            let (mut named, mut unnamed, mut nn, mut un) = (0, 0, 0usize, 0usize);
            let mut rows: Vec<_> = slots.iter().collect();
            rows.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
            for (h, (n, graphs_in)) in rows {
                match nodegraph::slot_name(*h) {
                    Some(name) => {
                        named += 1;
                        nn += n;
                        println!("  {h:#010X} n={n:<4} {name}");
                    }
                    None => {
                        unnamed += 1;
                        un += n;
                        let g: Vec<&str> = graphs_in.iter().map(|s| s.as_str()).collect();
                        println!("  {h:#010X} n={n:<4} <unresolved>  in {}", g.join(","));
                    }
                }
            }
            println!("\nslots resolved {named} ({nn} nodes), unresolved {unnamed} ({un} nodes)");
        }

        "ngdump" => {
            for (id, name) in ship_g.iter() {
                if !arg.is_empty() && !name.contains(&arg) {
                    continue;
                }
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Ok(g) = NodeGraph::from_dat1(&d1) else { continue };
                println!("=== {name} ===");
                println!(
                    "{} ({:#010X})  nodes {}  connections {}  plugs~{}",
                    g.graph_type_name.unwrap_or("?"),
                    g.graph_type,
                    g.nodes.len(),
                    g.connections.len(),
                    g.approximate_plug_count
                );
                for r in &g.refs {
                    println!("  ref {:016X} {}", r.asset_id, r.path);
                }
                for line in g.outline() {
                    println!("{line}");
                }
                break;
            }
        }

        "ngnode" => {
            let want: usize = arg2.parse().unwrap_or(0);
            for (id, name) in ship_g.iter() {
                if !arg.is_empty() && !name.contains(&arg) {
                    continue;
                }
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Ok(g) = NodeGraph::from_dat1(&d1) else { continue };
                let Some(n) = g.nodes.get(want) else { break };
                println!("=== {name} node #{want} ===");
                println!(
                    "slot {:#010X} {:?} class {:?} buffer {:#010X} at ({}, {})",
                    n.slot_hash, n.slot, n.class, n.buffer_tag, n.x, n.y
                );
                println!("{}", serde_json::to_string_pretty(&n.params).unwrap());
                break;
            }
        }

        "hashes" => {
            for s in [
                "Node Graph Built Header",
                "Node Graph Built Nodes",
                "Node Graph Built Connections",
                "Node Graph Built Debug",
                "Node Graph Refs",
                "Conduit Built",
                "Conduit Asset Refs",
            ] {
                println!("  {:#010X}  {s}", crc32::hash(s));
            }
        }

        "keyprobe" => {
            use omnitool_lib::core::prius::{self, NameCtx, PValue};
            let mut n = 0;
            for (id, name) in ship_c.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Some(sec) = d1.get_section_data(CONDUIT_BUILT_TAG) else { continue };
                let ctx = NameCtx::new(Some(&d1), None);
                let Ok(root) = prius::decode(sec, 0, None, &ctx) else { continue };
                let Some(ev) = root.field_named("Events") else { continue };
                let Some(PValue::Struct(map)) = ev.values.first() else { continue };
                println!("{} : Events field id={:#010X} count={} array/enc={} pool_name_off={}", name.rsplit('/').next().unwrap(), ev.id, ev.count, ev.encoding_type, ev.name_offset);
                for f in &map.fields {
                    let pool = d1.get_string(f.name_offset).unwrap_or_default();
                    let mut ename = String::new();
                    let mut ehash = 0u32;
                    let mut echk = 0u64;
                    if let Some(PValue::Struct(e)) = f.values.first() {
                        if let Some(nf) = e.field_named("Name") {
                            if let Some(PValue::Str { value, hash, checksum }) = nf.values.first() {
                                ename = value.clone();
                                ehash = *hash;
                                echk = *checksum;
                            }
                        }
                    }
                    println!("   key={:#010X} ({}) pool={:?} name={:?} stored_crc32={:#010X} crc32(name)={:#010X} crc64={:#018X}",
                        f.id, f.id, pool, ename, ehash, crc32::hash(&ename), echk);
                }
                n += 1;
                if n >= 3 { break }
            }
        }
        m => eprintln!("unknown mode {m}"),
    }
}

fn top(title: &str, m: &BTreeMap<String, usize>, n: usize) {
    let mut v: Vec<_> = m.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let total: usize = m.values().sum();
    println!("\n{title} ({} distinct, {total} total):", m.len());
    for (k, c) in v.into_iter().take(n) {
        println!("   {k:<40} {c}");
    }
}

/// The same five counts `Conduit::counts` returns, but off the generic reader.
fn count_ddl(root: &omnitool_lib::core::ddl::DdlObject) -> (usize, usize, usize, usize, usize) {
    use omnitool_lib::core::ddl::DdlValue;
    let mut c = (0, 0, 0, 0, 0);
    let nonempty = |v: &DdlValue| matches!(v, DdlValue::Struct(s) if !s.fields.is_empty());
    let Some(map) = root.fields.values().find(|f| f.name == "Events") else { return c };
    for v in &map.values {
        let DdlValue::Struct(m) = v else { continue };
        for entry in m.fields.values() {
            for ev in entry.values.iter().filter(|v| nonempty(v)) {
                let DdlValue::Struct(ev) = ev else { continue };
                c.0 += 1;
                for fl in ev.fields.values().filter(|f| f.name == "Filters") {
                    for f in fl.values.iter().filter(|v| nonempty(v)) {
                        let DdlValue::Struct(f) = f else { continue };
                        c.1 += 1;
                        for sel in f.fields.values().filter(|x| x.name == "Selector") {
                            c.2 += sel.values.iter().filter(|v| nonempty(v)).count();
                        }
                        for rs in f.fields.values().filter(|x| x.name == "Response") {
                            for r in rs.values.iter().filter(|v| nonempty(v)) {
                                let DdlValue::Struct(r) = r else { continue };
                                c.3 += 1;
                                for fm in r.fields.values().filter(|x| x.name == "FieldMap") {
                                    c.4 += fm.values.iter().filter(|v| nonempty(v)).count();
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    c
}

fn parse_dat1(bytes: &[u8]) -> Option<Dat1> {
    const HDR: usize = 36;
    if bytes.len() >= 4 && u32::from_le_bytes(bytes[0..4].try_into().ok()?) == DAT1_MAGIC {
        return Dat1::parse(bytes).ok();
    }
    if bytes.len() > HDR {
        if let Ok(d) = Dat1::parse(&bytes[HDR..]) {
            return Some(d);
        }
    }
    Dat1::parse(bytes).ok()
}
