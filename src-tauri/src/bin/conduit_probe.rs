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
//!   keyprobe               the Events map key, entry by entry, against the pool string
//!   keycheck               key == crc32(decimal(crc32(Name))) over every shipped event
//!   root                   the root fields of a Conduit Built section
//!   sounds                 Wwise id == fnv1(lowercase(name)) over every name/id pair
//!   ngplugs                plug aliases, duplicate plug ids, ApproximatePlugCount
//!   ngbad                  unnameable-slot nodes and unresolvable connections
//!   ngcross                do unresolvable endpoints live in another shipped graph?
//!   ngindex                node index vs decoded nodes, missing payloads, id uniqueness
//!   ngsub                  FieldNameHash == buffer tag, and which graphs ref a nodegraph
//!   ngdebug                Node Graph Built Debug: what the flattened ids resolve to
//!   ngassetid              is every Debug row's OriginalAssetId the graph's own id?
//!   ngobj                  what an ObjectiveNodeGameDef carries, over all 606
//!   hashes                 crc32 preimages of the section names
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
            println!("  Events map key == crc32(decimal(crc32(Name))): {keys_ok} of {keys_total}");
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
                for (name, n) in v.iter().take(30) {
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

        "sounds" => {
            // Is the numeric id beside every Wwise name derivable from the name?
            // FNV-1 (multiply then xor) of the lowercased name, as in core::zone_refs.
            let fnv1 = |s: &str| -> u32 {
                let mut h: u32 = 2166136261;
                for b in s.as_bytes() { h = h.wrapping_mul(16777619); h ^= *b as u32; }
                h
            };
            let mut pairs: BTreeMap<&str, (&str, u64, u64)> = BTreeMap::new();
            pairs.insert("kSound", ("SoundEventName/SoundEventId", 0, 0));
            pairs.insert("kWWise:action", ("WwiseActionName/WwiseActionId", 0, 0));
            pairs.insert("kWWise:group", ("GroupName/GroupId", 0, 0));
            let mut mism: Vec<String> = Vec::new();
            for (id, name) in ship_c.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Ok(c) = Conduit::from_dat1(&d1) else { continue };
                for (_, _, r) in c.responses() {
                    for (key, nf, idf) in [
                        ("kSound", "SoundEventName", "SoundEventId"),
                        ("kWWise:action", "WwiseActionName", "WwiseActionId"),
                        ("kWWise:group", "GroupName", "GroupId"),
                    ] {
                        if !key.starts_with(&r.kind) { continue }
                        let (Some(n), Some(v)) = (r.payload(nf), r.payload(idf)) else { continue };
                        let e = pairs.get_mut(key).unwrap();
                        e.2 += 1;
                        let want = fnv1(&n.to_ascii_lowercase()) as u64;
                        match v.parse::<u64>() {
                            Ok(got) if got == want => e.1 += 1,
                            other => if mism.len() < 8 { mism.push(format!("  {} {nf}={n} {idf}={v} want {want} parsed {other:?}", name.rsplit('/').next().unwrap())) },
                        }
                    }
                }
            }
            for (k, (what, ok, tot)) in pairs.iter() {
                println!("{k:<14} {what:<32} id == fnv1(lowercase(name)): {ok} of {tot}");
            }
            for m in mism { println!("{m}") }
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

        "ngbad" => {
            // nodes whose FieldNameHash this build cannot name, and connections
            // whose endpoints do not resolve inside the asset.
            for (id, name) in ship_g.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Ok(g) = NodeGraph::from_dat1(&d1) else { continue };
                let short = name.rsplit('/').next().unwrap();
                for (i, n) in g.nodes.iter().enumerate() {
                    if n.slot.is_none() {
                        println!("--- {short} node #{i} slot {:#010X} buffer {:#010X}", n.slot_hash, n.buffer_tag);
                        println!("{}", serde_json::to_string(&n.params).unwrap());
                    }
                }
                let by_id = g.node_index();
                let plugs = g.plug_owners();
                for c in &g.connections {
                    let sn = by_id.contains_key(&c.source_node);
                    let tn = by_id.contains_key(&c.target_node);
                    let sp = plugs.contains_key(&c.source_plug);
                    let tp = plugs.contains_key(&c.target_plug);
                    if sn && tn && sp && tp { continue }
                    println!("--- {short} conn {:016X} src node {:016X}({}) plug {:016X}({}) -> dst node {:016X}({}) plug {:016X}({})",
                        c.id, c.source_node, sn, c.source_plug, sp, c.target_node, tn, c.target_plug, tp);
                    for m in g.id_mappings.iter().filter(|m| m.flattened_id == c.source_node || m.flattened_id == c.target_node || m.flattened_id == c.source_plug || m.flattened_id == c.target_plug) {
                        println!("      debug map flat {:016X} -> asset {:016X} id {:016X} embedded {:016X}",
                            m.flattened_id, m.original_asset_id, m.original_id, m.original_embedded_graph_id);
                    }
                }
                // do the subgraph node payloads name the missing ids?
                for n in g.nodes.iter().filter(|n| matches!(n.slot, Some("SubGraphs") | Some("EmbeddedSubGraphs"))) {
                    println!("--- {short} subgraph node {:#010X} {}", n.slot_hash, serde_json::to_string(&n.params).unwrap());
                }
            }
        }

        "ngplugs" => {
            // does the builder really emit a duplicate "<plug> <i>" field per element?
            let (mut arr, mut idxd, mut dup_id) = (0usize, 0usize, 0usize);
            let mut detailed = 0usize;
            for (id, name) in ship_g.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Ok(g) = NodeGraph::from_dat1(&d1) else { continue };
                let mut sum_plugs = 0usize;
                for n in &g.nodes {
                    let mut ids: BTreeMap<u64, Vec<String>> = BTreeMap::new();
                    for p in &n.plugs {
                        if p.label.rsplit(' ').next().is_some_and(|t| t.parse::<u32>().is_ok()) { idxd += 1 } else { arr += 1 }
                        if p.detail.is_some() { detailed += 1 }
                        ids.entry(p.id).or_default().push(p.label.clone());
                    }
                    dup_id += ids.values().filter(|v| v.len() > 1).count();
                    sum_plugs += n.plugs.len();
                }
                if sum_plugs as u64 != g.approximate_plug_count {
                    println!("  {:<52} plugs read {sum_plugs} vs ApproximatePlugCount {}", name.rsplit('/').next().unwrap(), g.approximate_plug_count);
                }
            }
            println!("plug fields with an array-element name (\"Inputs 0\"): {idxd}, plain: {arr}");
            println!("plugs carrying a SyncLabel: {}", detailed);
            println!("plug ids carried by more than one field on the same node: {dup_id}");
        }

        "ngcross" => {
            // Do the unresolvable endpoints live in a *different* shipped graph?
            let mut all_nodes: BTreeMap<u64, String> = BTreeMap::new();
            let mut all_plugs: BTreeMap<u64, String> = BTreeMap::new();
            let mut parsed: Vec<(String, NodeGraph)> = Vec::new();
            for (id, name) in ship_g.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Ok(g) = NodeGraph::from_dat1(&d1) else { continue };
                let short = name.rsplit('/').next().unwrap().to_string();
                for n in &g.nodes {
                    all_nodes.insert(n.id, short.clone());
                    for pl in &n.plugs { all_plugs.insert(pl.id, format!("{short}:{}", pl.label)); }
                }
                parsed.push((short, g));
            }
            let (mut elsewhere, mut nowhere) = (0usize, 0usize);
            for (short, g) in parsed.iter() {
                let local = g.node_index();
                let lp = g.plug_owners();
                for c in &g.connections {
                    if local.contains_key(&c.source_node) && local.contains_key(&c.target_node)
                        && lp.contains_key(&c.source_plug) && lp.contains_key(&c.target_plug) { continue }
                    let s = all_nodes.get(&c.source_node);
                    let t = all_nodes.get(&c.target_node);
                    let sp = all_plugs.get(&c.source_plug);
                    let tp = all_plugs.get(&c.target_plug);
                    if s.is_some() && t.is_some() { elsewhere += 1 } else { nowhere += 1 }
                    println!("{short}: src {:?}/{:?} dst {:?}/{:?}", s, sp, t, tp);
                    println!("   refs of {short}: {:?}", g.refs.iter().map(|r| r.path.rsplit('/').next().unwrap().to_string()).collect::<Vec<_>>());
                }
            }
            println!("dangling endpoints found in another shipped graph: {elsewhere}, found nowhere: {nowhere}");
        }

        "ngindex" => {
            use omnitool_lib::core::prius::{self, NameCtx, PValue};
            let (mut rows, mut missing, mut dupe_id, mut zero_id) = (0usize, 0usize, 0usize, 0usize);
            for (id, name) in ship_g.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let ctx = NameCtx::new(Some(&d1), None);
                let Some(sec) = d1.get_section_data(nodegraph::NG_NODES_TAG) else { continue };
                let Ok(index) = prius::decode(sec, 0, None, &ctx) else { continue };
                let mut n_rows = 0usize;
                let mut miss: Vec<u32> = Vec::new();
                if let Some(f) = index.field_named("Nodes") {
                    for v in &f.values {
                        let PValue::Struct(o) = v else { continue };
                        n_rows += 1;
                        let tag = o.field_named("BuiltNodeDataBufferNameHash")
                            .and_then(|x| x.values.first())
                            .and_then(|v| match v { PValue::U32(t) => Some(*t), PValue::U64(t) => Some(*t as u32), PValue::U16(t) => Some(*t as u32), PValue::U8(t) => Some(*t as u32), _ => None })
                            .unwrap_or(0);
                        if d1.get_section_data(tag).is_none() { miss.push(tag) }
                    }
                }
                rows += n_rows;
                missing += miss.len();
                let Ok(g) = NodeGraph::from_dat1(&d1) else { continue };
                if n_rows != g.nodes.len() {
                    println!("  {:<52} index rows {n_rows} decoded {} missing payload {:?}", name.rsplit('/').next().unwrap(), g.nodes.len(), miss.iter().map(|t| format!("{t:#010X}")).collect::<Vec<_>>());
                }
                let mut seen: BTreeSet<u64> = BTreeSet::new();
                for n in &g.nodes {
                    if n.id == 0 { zero_id += 1 }
                    if !seen.insert(n.id) { dupe_id += 1 }
                }
            }
            println!("node index rows {rows}, rows whose payload section is absent {missing}, nodes with Id==0 {zero_id}, duplicate node Ids {dupe_id}");
        }

        "ngsub" => {
            let (mut eq, mut neq, mut eq_named) = (0usize, 0usize, 0usize);
            let mut refkinds: BTreeMap<String, BTreeMap<u32, usize>> = BTreeMap::new();
            for (id, name) in ship_g.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Ok(g) = NodeGraph::from_dat1(&d1) else { continue };
                let short = name.rsplit('/').next().unwrap().to_string();
                for n in &g.nodes {
                    if n.slot_hash == n.buffer_tag {
                        eq += 1;
                        if n.slot.is_some() { eq_named += 1 }
                    } else { neq += 1 }
                }
                let e = refkinds.entry(short).or_default();
                for r in &g.refs { *e.entry(r.type_id).or_default() += 1 }
            }
            println!("nodes with FieldNameHash == BuiltNodeDataBufferNameHash: {eq} (of which the slot table names {eq_named}); other nodes {neq}");
            for (k, v) in refkinds.iter().filter(|(_, v)| v.contains_key(&0x53B8EA03u32)) {
                println!("  {k:<52} {:?}", v.iter().map(|(t, n)| (format!("{t:#010X}"), n)).collect::<Vec<_>>());
            }
        }

        "ngdebug" => {
            let (mut rows, mut emb, mut noemb, mut hit_node, mut hit_plug, mut hit_other) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
            let mut odd_slot_in_map = (0usize, 0usize);
            let mut distinct_assets: BTreeSet<u64> = BTreeSet::new();
            for (id, name) in ship_g.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Ok(g) = NodeGraph::from_dat1(&d1) else { continue };
                let by_id = g.node_index();
                let plugs = g.plug_owners();
                for m in &g.id_mappings {
                    rows += 1;
                    distinct_assets.insert(m.original_asset_id);
                    if m.original_embedded_graph_id != u64::MAX { emb += 1 } else { noemb += 1 }
                    if by_id.contains_key(&m.flattened_id) { hit_node += 1 }
                    else if plugs.contains_key(&m.flattened_id) { hit_plug += 1 }
                    else { hit_other += 1 }
                }
                // are the FieldNameHash==buffer nodes flagged as embedded in the map?
                for n in g.nodes.iter().filter(|n| n.slot_hash == n.buffer_tag) {
                    match g.id_mappings.iter().find(|m| m.flattened_id == n.id) {
                        Some(m) if m.original_embedded_graph_id != u64::MAX => odd_slot_in_map.0 += 1,
                        _ => odd_slot_in_map.1 += 1,
                    }
                }
                if name.contains("mis_ard_01_first_visit") {
                    println!("  {} debug rows {} nodes {}", name.rsplit('/').next().unwrap(), g.id_mappings.len(), g.nodes.len());
                    for m in g.id_mappings.iter().take(4) {
                        println!("     flat {:016X} asset {:016X} orig {:016X} emb {:016X}", m.flattened_id, m.original_asset_id, m.original_id, m.original_embedded_graph_id);
                    }
                    for n in g.nodes.iter().filter(|n| n.slot_hash == n.buffer_tag) {
                        let m = g.id_mappings.iter().find(|m| m.flattened_id == n.id);
                        println!("     odd node {:016X} -> {:?}", n.id, m.map(|m| (format!("{:016X}", m.original_asset_id), format!("{:016X}", m.original_id), format!("{:016X}", m.original_embedded_graph_id))));
                    }
                }
            }
            println!("Debug rows {rows}: embedded-graph id set {emb}, unset {noemb}");
            println!("  flattened_id is a node id {hit_node}, a plug id {hit_plug}, neither {hit_other}");
            println!("  FieldNameHash==buffer nodes flagged embedded in the map: {} of {}", odd_slot_in_map.0, odd_slot_in_map.0 + odd_slot_in_map.1);
            println!("  distinct OriginalAssetId values {}", distinct_assets.len());
        }

        "ngassetid" => {
            let (mut ok, mut tot) = (0usize, 0usize);
            for (id, name) in ship_g.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Ok(g) = NodeGraph::from_dat1(&d1) else { continue };
                let all_self = !g.id_mappings.is_empty() && g.id_mappings.iter().all(|m| m.original_asset_id == *id);
                tot += 1;
                if all_self { ok += 1 } else {
                    println!("  {} asset {:016X} first map asset {:?}", name.rsplit('/').next().unwrap(), id,
                        g.id_mappings.first().map(|m| format!("{:016X}", m.original_asset_id)));
                }
            }
            println!("graphs where every Debug row's OriginalAssetId is the graph's own asset id: {ok} of {tot}");
        }

        "ngobj" => {
            // What an ObjectiveNodeGameDef actually carries, over all 606.
            let mut fields: BTreeMap<String, usize> = BTreeMap::new();
            let mut taskmode: BTreeMap<String, usize> = BTreeMap::new();
            let mut uimode: BTreeMap<String, usize> = BTreeMap::new();
            let (mut n_obj, mut with_cp, mut seg, mut with_load) = (0usize, 0usize, 0usize, 0usize);
            for (id, _) in ship_g.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Ok(g) = NodeGraph::from_dat1(&d1) else { continue };
                for n in g.nodes.iter().filter(|n| n.class == Some("ObjectiveNodeGameDef")) {
                    n_obj += 1;
                    if let Some(m) = n.params.as_object() {
                        for k in m.keys() { *fields.entry(k.clone()).or_default() += 1 }
                    }
                    if let Some(v) = n.param("TaskMode") { *taskmode.entry(v).or_default() += 1 }
                    if let Some(v) = n.param("UIMode") { *uimode.entry(v).or_default() += 1 }
                    if n.param("Checkpoint").is_some() { with_cp += 1 }
                    if n.param("SegmentComplete").as_deref() == Some("true") { seg += 1 }
                    if n.params.get("DebugLoadInfo").is_some() { with_load += 1 }
                }
            }
            println!("ObjectiveNodeGameDef nodes: {n_obj}");
            println!("  with a Checkpoint {with_cp}, SegmentComplete true {seg}, with DebugLoadInfo {with_load}");
            top("payload fields present", &fields, 30);
            top("TaskMode", &taskmode, 10);
            top("UIMode", &uimode, 10);
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

        "root" => {
            use omnitool_lib::core::prius::{self, NameCtx};
            let mut n = 0;
            for (id, name) in ship_c.iter() {
                if !arg.is_empty() && !name.contains(&arg) { continue }
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Some(sec) = d1.get_section_data(CONDUIT_BUILT_TAG) else { continue };
                let ctx = NameCtx::new(Some(&d1), None);
                let Ok(root) = prius::decode(sec, 0, None, &ctx) else { continue };
                println!("=== {name} (asset id {:016X}) ===", id);
                for f in &root.fields {
                    println!("   id={:#010X} name={:?} type={} count={} vals={:?}",
                        f.id, f.name, f.encoding_type, f.count,
                        f.values.iter().take(3).map(|v| format!("{v:?}").chars().take(90).collect::<String>()).collect::<Vec<_>>());
                }
                n += 1;
                if n >= 3 { break }
            }
        }

        "keycheck" => {
            use omnitool_lib::core::prius::{self, NameCtx, PValue};
            let (mut ok, mut tot, mut bad) = (0u64, 0u64, 0u64);
            for (id, name) in ship_c.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Some(sec) = d1.get_section_data(CONDUIT_BUILT_TAG) else { continue };
                let ctx = NameCtx::new(Some(&d1), None);
                let Ok(root) = prius::decode(sec, 0, None, &ctx) else { continue };
                let Some(ev) = root.field_named("Events") else { continue };
                let Some(PValue::Struct(map)) = ev.values.first() else { continue };
                for f in &map.fields {
                    tot += 1;
                    let mut ename = String::new();
                    if let Some(PValue::Struct(e)) = f.values.first() {
                        if let Some(PValue::Str { value, .. }) = e.field_named("Name").and_then(|x| x.values.first()) {
                            ename = value.clone();
                        }
                    }
                    let decimal = format!("{}", crc32::hash(&ename));
                    if crc32::hash(&decimal) == f.id { ok += 1 } else {
                        bad += 1;
                        if bad <= 5 { println!("  MISS {name} key={:#010X} name={ename:?} decimal={decimal}", f.id) }
                    }
                }
            }
            println!("Events map key == crc32(decimal text of crc32(Name)): {ok} of {tot}");
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
                    println!("   key={:#010X} pool={:?} crc32(pool)={:#010X} name={:?} stored_crc32={:#010X} crc32(name)={:#010X} dec(crc32(name))={} crc64={:#018X}",
                        f.id, pool, crc32::hash(&pool), ename, ehash, crc32::hash(&ename), crc32::hash(&ename), echk);
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
