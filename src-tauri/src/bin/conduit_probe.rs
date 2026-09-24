//! Scratch exploration of `.conduit` / `.nodegraph` assets.

use omnitool_lib::core::dat1::Dat1;
use omnitool_lib::core::level;
use std::collections::BTreeMap;

const GAME: &str = r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart";

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mode = a.first().cloned().unwrap_or_else(|| "census".into());
    let arg = a.get(1).cloned().unwrap_or_default();
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
    conduits.sort_by(|a, b| a.1.cmp(&b.1));
    graphs.sort_by(|a, b| a.1.cmp(&b.1));

    match mode.as_str() {
        "census" => {
            println!("named .conduit: {}", conduits.len());
            println!("named .nodegraph: {}", graphs.len());
            let in_toc_c = conduits.iter().filter(|c| idx.asset(c.0).is_some()).count();
            let in_toc_g = graphs.iter().filter(|c| idx.asset(c.0).is_some()).count();
            println!("in TOC: conduit {in_toc_c}, nodegraph {in_toc_g}");
            for (id, n) in graphs.iter() {
                println!(
                    "  {:016X} {:>9} {}",
                    id,
                    idx.asset(*id).map(|a| a.size).unwrap_or(0),
                    n
                );
            }
            println!("\nfirst 20 conduits:");
            for (id, n) in conduits.iter().take(20) {
                println!(
                    "  {:016X} {:>9} {}",
                    id,
                    idx.asset(*id).map(|a| a.size).unwrap_or(0),
                    n
                );
            }
        }
        "sections" => {
            let list = if arg == "nodegraph" { &graphs } else { &conduits };
            let mut census: BTreeMap<u32, (u64, u64)> = BTreeMap::new();
            let mut ok = 0;
            for (id, _) in list.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                ok += 1;
                for s in &d1.sections {
                    let e = census.entry(s.tag).or_insert((0, 0));
                    e.0 += 1;
                    e.1 += s.size as u64;
                }
            }
            println!("parsed {ok} of {}", list.len());
            for (t, (n, b)) in census.iter() {
                println!("  {t:#010X}  in {n:>5} assets  {b:>12} bytes");
            }
        }
        "hex" => {
            let list = if arg == "nodegraph" { &graphs } else { &conduits };
            let (id, name) = &list[0];
            let bytes = idx.extract(*id).unwrap();
            println!("{name} ({} bytes)", bytes.len());
            for (i, ch) in bytes.chunks(16).take(40).enumerate() {
                let hex: Vec<String> = ch.iter().map(|b| format!("{b:02X}")).collect();
                let asc: String = ch
                    .iter()
                    .map(|&b| if (32..127).contains(&b) { b as char } else { '.' })
                    .collect();
                println!("  {:04X}  {:<48}  {asc}", i * 16, hex.join(" "));
            }
        }
        "ddl" => {
            let list = if arg.is_empty() {
                vec![conduits[0].clone()]
            } else {
                conduits
                    .iter()
                    .filter(|c| c.1.contains(&arg))
                    .cloned()
                    .take(1)
                    .collect()
            };
            for (id, name) in list {
                let bytes = idx.extract(id).unwrap();
                let d1 = parse_dat1(&bytes).unwrap();
                let sec = d1.get_section_data(0xCEB3_0E68).unwrap();
                let obj = omnitool_lib::core::ddl::parse(sec, &d1).unwrap();
                println!("=== {name} ===");
                println!(
                    "{}",
                    serde_json::to_string_pretty(&omnitool_lib::core::ddl::collapse_to_json(&obj))
                        .unwrap()
                );
            }
        }
        "ngddl" => {
            let (id, name) = graphs
                .iter()
                .filter(|g| idx.asset(g.0).is_some_and(|a| a.size > 0))
                .find(|g| arg.is_empty() || g.1.contains(&arg))
                .unwrap()
                .clone();
            let bytes = idx.extract(id).unwrap();
            let d1 = parse_dat1(&bytes).unwrap();
            println!("=== {name} ===");
            for s in &d1.sections {
                println!("  section {:#010X} size {}", s.tag, s.size);
            }
            let tags: Vec<u32> = [0xE506_5650u32, 0xD811_0E80, 0x7884_E530, 0xA989_4CDB]
                .into_iter()
                .chain(d1.sections.iter().map(|s| s.tag).filter(|t| ![0xE506_5650u32, 0xD811_0E80, 0x7884_E530, 0xA989_4CDB, 0xFBD4_96D6].contains(t)))
                .collect();
            for tag in tags {
                let Some(sec) = d1.get_section_data(tag) else {
                    continue;
                };
                println!("\n--- {tag:#010X} ({} bytes) ---", sec.len());
                let obj = omnitool_lib::core::ddl::parse(sec, &d1).unwrap();
                let j = omnitool_lib::core::ddl::collapse_to_json(&obj);
                let s = serde_json::to_string_pretty(&j).unwrap();
                println!("{}", &s[..s.len().min(4000)]);
            }
        }
        "shape" => {
            use omnitool_lib::core::ddl::{self, DdlObject, DdlValue};
            // path -> field name -> (count, kinds seen)
            let mut shape: BTreeMap<String, BTreeMap<String, (u64, std::collections::BTreeSet<String>)>> =
                BTreeMap::new();
            let mut enums: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
            fn walk(
                obj: &DdlObject,
                path: &str,
                shape: &mut BTreeMap<String, BTreeMap<String, (u64, std::collections::BTreeSet<String>)>>,
                enums: &mut BTreeMap<String, BTreeMap<String, u64>>,
            ) {
                for id in &obj.field_order {
                    let f = &obj.fields[id];
                    let key = if f.name.chars().all(|c| c.is_ascii_digit()) {
                        "<maphash>".to_string()
                    } else {
                        f.name.clone()
                    };
                    let e = shape.entry(path.to_string()).or_default();
                    let slot = e.entry(key.clone()).or_default();
                    slot.0 += 1;
                    slot.1.insert(format!(
                        "{}{}",
                        f.type_kind.label(),
                        match f.array_kind {
                            ddl::DdlArrayKind::None => "",
                            ddl::DdlArrayKind::Fixed => "[fixed]",
                            ddl::DdlArrayKind::Dynamic => "[dyn]",
                            ddl::DdlArrayKind::Map => "[map]",
                        }
                    ));
                    let _ = &slot;
                    if f.type_kind == ddl::DdlTypeKind::Enum {
                        for v in &f.values {
                            if let DdlValue::String { value, .. } = v {
                                *enums
                                    .entry(format!("{path}.{key}"))
                                    .or_default()
                                    .entry(value.clone())
                                    .or_default() += 1;
                            }
                        }
                    }
                    for v in &f.values {
                        if let DdlValue::Struct(inner) = v {
                            walk(inner, &format!("{path}.{key}"), shape, enums);
                        }
                    }
                }
            }
            let mut n = 0;
            for (id, _) in conduits.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                let Some(sec) = d1.get_section_data(0xCEB3_0E68) else {
                    continue;
                };
                let Ok(obj) = ddl::parse(sec, &d1) else { continue };
                walk(&obj, "ECM", &mut shape, &mut enums);
                n += 1;
            }
            println!("walked {n} conduits\n=== struct shapes ===");
            for (p, fields) in &shape {
                println!("{p}");
                for (name, (c, kinds)) in fields {
                    let k: Vec<&str> = kinds.iter().map(|s| s.as_str()).collect();
                    println!("   {name:<24} n={c:<8} {}", k.join(","));
                }
            }
            println!("\n=== enum members seen ===");
            for (p, vals) in &enums {
                println!("{p}");
                let mut v: Vec<_> = vals.iter().collect();
                v.sort_by(|a, b| b.1.cmp(a.1));
                for (name, c) in v {
                    println!("   {name:<34} {c}");
                }
            }
        }
        "nghdr" => {
            use omnitool_lib::core::ddl::{self, DdlValue};
            let mut gth: BTreeMap<u64, usize> = BTreeMap::new();
            let mut fnh: BTreeMap<u64, usize> = BTreeMap::new();
            let mut ctype: BTreeMap<String, usize> = BTreeMap::new();
            let mut itype: BTreeMap<String, usize> = BTreeMap::new();
            let (mut nnodes, mut nconn, mut ngraphs, mut nsec) = (0usize, 0usize, 0usize, 0usize);
            let mut nodefields: BTreeMap<u64, BTreeMap<String, usize>> = BTreeMap::new();
            for (id, _) in graphs.iter() {
                let Ok(bytes) = idx.extract(*id) else { continue };
                let Some(d1) = parse_dat1(&bytes) else { continue };
                if d1.get_section_data(0xE506_5650).is_none() { continue }
                ngraphs += 1;
                nsec += d1.sections.len();
                let hdr = ddl::parse(d1.get_section_data(0xE506_5650).unwrap(), &d1).unwrap();
                for f in hdr.fields.values() {
                    if f.name == "GraphTypeHash" {
                        if let Some(v) = f.values.first() { *gth.entry(num(v)).or_default() += 1; }
                    }
                }
                if let Some(sec) = d1.get_section_data(0xD811_0E80) {
                    let o = ddl::parse(sec, &d1).unwrap();
                    for f in o.fields.values() {
                        if f.name != "Nodes" { continue }
                        for v in &f.values {
                            if let DdlValue::Struct(n) = v {
                                nnodes += 1;
                                let mut tag = 0u64; let mut fh = 0u64;
                                for g in n.fields.values() {
                                    if g.name == "BuiltNodeDataBufferNameHash" { tag = num(g.values.first().unwrap()); }
                                    if g.name == "FieldNameHash" { fh = num(g.values.first().unwrap()); }
                                }
                                *fnh.entry(fh).or_default() += 1;
                                if let Some(nd) = d1.get_section_data(tag as u32) {
                                    if let Ok(no) = ddl::parse(nd, &d1) {
                                        let e = nodefields.entry(fh).or_default();
                                        for g in no.fields.values() {
                                            *e.entry(g.name.clone()).or_default() += 1;
                                            if g.name == "Type" {
                                                if let Some(DdlValue::String{value,..}) = g.values.first() {
                                                    *itype.entry(value.clone()).or_default() += 1;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(sec) = d1.get_section_data(0x7884_E530) {
                    let o = ddl::parse(sec, &d1).unwrap();
                    for f in o.fields.values() {
                        if f.name != "Connections" { continue }
                        for v in &f.values {
                            if let DdlValue::Struct(c) = v {
                                if c.fields.is_empty() { continue }
                                nconn += 1;
                                for g in c.fields.values() {
                                    if g.name == "Type" {
                                        if let Some(DdlValue::String{value,..}) = g.values.first() {
                                            *ctype.entry(value.clone()).or_default() += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            println!("graphs parsed {ngraphs}, sections {nsec}, nodes {nnodes}, connections {nconn}");
            println!("
GraphTypeHash:");
            for (h, c) in &gth { println!("  {:#010X}  {c}", *h as u32); }
            println!("
Connection Type:");
            for (t, c) in &ctype { println!("  {t:<24} {c}"); }
            println!("
Node item Type:");
            for (t, c) in &itype { println!("  {t:<24} {c}"); }
            println!("
FieldNameHash (node class slot) x count, with payload fields:");
            let mut v: Vec<_> = fnh.iter().collect();
            v.sort_by(|a,b| b.1.cmp(a.1));
            for (h, c) in v {
                println!("  {:#010X}  n={c}", *h as u32);
                if let Some(fields) = nodefields.get(h) {
                    let mut fv: Vec<_> = fields.iter().collect();
                    fv.sort_by(|a,b| b.1.cmp(a.1));
                    let names: Vec<String> = fv.iter().map(|(n,c)| format!("{n}({c})")).collect();
                    println!("       {}", names.join(" "));
                }
            }
        }
        m => eprintln!("unknown mode {m}"),
    }
}

fn num(v: &omnitool_lib::core::ddl::DdlValue) -> u64 {
    use omnitool_lib::core::ddl::DdlValue as D;
    match v {
        D::U8(x) => *x as u64, D::U16(x) => *x as u64, D::U32(x) => *x as u64, D::U64(x) => *x,
        D::I8(x) => *x as u64, D::I16(x) => *x as u64, D::I32(x) => *x as u64, D::I64(x) => *x as u64,
        D::Identifier(x) | D::Asset(x) => *x,
        _ => 0,
    }
}

fn parse_dat1(bytes: &[u8]) -> Option<Dat1> {
    const HDR: usize = 36;
    if bytes.len() >= 4 && u32::from_le_bytes(bytes[0..4].try_into().ok()?) == omnitool_lib::core::dat1::DAT1_MAGIC {
        return Dat1::parse(bytes).ok();
    }
    if bytes.len() > HDR {
        if let Ok(d) = Dat1::parse(&bytes[HDR..]) {
            return Some(d);
        }
    }
    Dat1::parse(bytes).ok()
}
