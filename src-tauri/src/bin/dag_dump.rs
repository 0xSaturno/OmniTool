use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::Instant;

use omnitool_lib::core::asset_id::AssetIdFlags;
use omnitool_lib::core::crc64;
use omnitool_lib::core::dag::{asset_type_name, Dag};
use omnitool_lib::core::toc::Toc;

const LIST_LIMIT: usize = 12;

fn resolve(dag: &Dag, query: &str) -> Option<usize> {
    let hex = query.strip_prefix("0x").or_else(|| query.strip_prefix("0X"));
    match hex.and_then(|h| u64::from_str_radix(h, 16).ok()) {
        Some(id) => dag.index_of(id),
        None => dag.index_of(crc64::hash(query)),
    }
}

fn label(dag: &Dag, index: usize) -> String {
    format!("{:016X} {:<13} {}", dag.id(index), dag.type_name(index), dag.name(index).unwrap_or("<unnamed>"))
}

fn print_list(title: &str, dag: &Dag, indices: &[usize]) {
    println!("  {title}: {}", indices.len());
    for &i in indices.iter().take(LIST_LIMIT) {
        println!("    {}", label(dag, i));
    }
    if indices.len() > LIST_LIMIT {
        println!("    ... {} more", indices.len() - LIST_LIMIT);
    }
}

fn coverage(dag: &Dag, toc_path: &str) {
    let toc = match std::fs::read(toc_path).map_err(|e| e.to_string()).and_then(|b| Toc::parse(&b).map_err(|e| e.to_string())) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("failed to load toc: {e}");
            return;
        }
    };
    let toc_ids: HashSet<u64> = toc.asset_ids().iter().copied().collect();

    let mut rows: BTreeMap<String, [usize; 2]> = BTreeMap::new();
    for &id in &toc_ids {
        let row = rows.entry(format!("{:?}", AssetIdFlags::from_id(id))).or_default();
        row[0] += 1;
        row[1] += dag.index_of(id).is_some() as usize;
    }
    println!("\ntoc coverage ({} unique ids)", toc_ids.len());
    println!("  {:<8} {:>8} {:>8}", "flags", "toc", "dag");
    for (flag, r) in &rows {
        println!("  {:<8} {:>8} {:>8}", flag, r[0], r[1]);
    }
    let not_in_toc = dag.asset_ids().iter().filter(|id| !toc_ids.contains(id)).count();
    println!("  dag entries not in toc (virtual): {not_in_toc}");

    let archives = toc.archive_filenames();
    let mod_records: Vec<_> = toc
        .assets()
        .into_iter()
        .filter(|a| {
            archives
                .get(a.archive_index as usize)
                .is_some_and(|n| n.replace('/', "\\").to_ascii_lowercase().starts_with("d\\mods\\"))
        })
        .collect();
    let mod_ids: HashSet<u64> = mod_records.iter().map(|a| a.asset_id).collect();
    let replaced = mod_ids.iter().filter(|id| dag.index_of(**id).is_some()).count();
    println!(
        "  mod records: {} ({} assets: {} replace shipped assets, {} new)",
        mod_records.len(),
        mod_ids.len(),
        replaced,
        mod_ids.len() - replaced
    );
}

fn main() {
    let mut args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: dag_dump <dag_path> [--toc <toc>] [asset_path | 0xASSETID ...]");
        std::process::exit(1);
    }
    let toc_path = args.iter().position(|a| a == "--toc").and_then(|pos| {
        let value = args.get(pos + 1).cloned();
        args.drain(pos..(pos + 2).min(args.len()));
        value
    });

    let start = Instant::now();
    let dag = match Dag::load(Path::new(&args[1])) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("failed to load dag: {e}");
            std::process::exit(1);
        }
    };
    println!("{} assets, version 0x{:08X}, parsed in {:?}", dag.len(), dag.version, start.elapsed());

    let mut by_type: BTreeMap<u8, usize> = BTreeMap::new();
    let (mut named, mut hash_ok) = (0usize, 0usize);
    for i in 0..dag.len() {
        *by_type.entry(dag.type_byte(i)).or_default() += 1;
        if let Some(name) = dag.name(i) {
            named += 1;
            hash_ok += (crc64::hash(name) == dag.id(i)) as usize;
        }
    }
    println!("named {named}, id == crc64(name) for {hash_ok}");
    for (t, n) in &by_type {
        println!("  type {t:>2} {:<13} {n}", asset_type_name(*t));
    }

    if let Some(toc_path) = &toc_path {
        coverage(&dag, toc_path);
    }

    if args.len() < 3 {
        return;
    }

    let start = Instant::now();
    let dependents = dag.dependents_index(None);
    println!("reverse index built in {:?}", start.elapsed());

    for query in &args[2..] {
        let Some(index) = resolve(&dag, query) else {
            println!("\n{query}: not in dag");
            continue;
        };
        println!("\n{}", label(&dag, index));
        print_list("direct dependencies", &dag, &dag.direct_dependencies(index, None));
        print_list("all dependencies", &dag, &dag.dependencies(&[index], None, true));
        let direct: Vec<usize> = dependents.direct(index).iter().map(|&p| p as usize).collect();
        print_list("loaded directly by", &dag, &direct);
        let zones: Vec<usize> = dependents
            .transitive(index)
            .into_iter()
            .filter(|&i| dag.type_name(i) == "Zone")
            .collect();
        print_list("zones that load it", &dag, &zones);
    }
}
