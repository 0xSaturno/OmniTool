//! Extracts shipped assets whose dag path contains any of the given texts, keeping their paths.
//!
//! usage: asset_extract <toc> <archives dir> <dag> <out dir> <text> [<text> ...]   (archive:<name> for a whole archive)

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use omnitool_lib::core::dag::Dag;
use omnitool_lib::core::toc::Toc;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 5 {
        eprintln!("usage: asset_extract <toc> <archives dir> <dag> <out dir> <text> [<text> ...]");
        std::process::exit(1);
    }
    let out_dir = PathBuf::from(&args[3]);
    let texts: Vec<String> = args[4..].iter().map(|t| t.to_lowercase()).collect();

    let toc = Toc::parse(&std::fs::read(&args[0]).expect("read toc")).expect("parse toc");
    let dag = Dag::load(Path::new(&args[2])).expect("read dag");
    let cache = toc.archive_cache(Path::new(&args[1]));

    // "archive:<name>" takes every asset of archives whose file name contains <name>, named or not.
    let archives = toc.archive_filenames();
    let archive_texts: Vec<&str> = texts.iter().filter_map(|t| t.strip_prefix("archive:")).collect();
    let mut seen = HashSet::new();
    let mut written = 0usize;
    for asset in toc.assets() {
        let in_archive = archives
            .get(asset.archive_index as usize)
            .is_some_and(|a| archive_texts.iter().any(|t| a.to_lowercase().contains(t)));
        let name = dag.index_of(asset.asset_id).and_then(|i| dag.name(i)).map(str::to_string);
        let lower = match &name {
            Some(n) => n.replace('\\', "/").to_lowercase(),
            None if in_archive => format!("unnamed/{:016X}", asset.asset_id),
            None => continue,
        };
        if !(in_archive || texts.iter().any(|t| lower.contains(t.as_str()))) || !seen.insert(asset.asset_id) {
            continue;
        }
        let raw = match toc.extract_asset_with_cache(&asset, &cache) {
            Ok(raw) => raw,
            Err(e) => {
                eprintln!("skip {lower}: {e}");
                continue;
            }
        };
        let file = out_dir.join(&lower);
        std::fs::create_dir_all(file.parent().unwrap()).expect("create folder");
        std::fs::write(&file, &raw).expect("write");
        println!("{:016X} {:>8} {lower}", asset.asset_id, raw.len());
        written += 1;
    }
    println!("wrote {written} assets to {}", out_dir.display());
}
