//! Extracts shipped `.model` assets for offline analysis, optionally only those carrying given sections.
//!
//! usage: model_extract <toc> <archives dir> <dag> <out dir> [--tags 0xAAAA,0xBBBB] [--limit N]

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use omnitool_lib::core::dag::Dag;
use omnitool_lib::core::toc::Toc;
use omnitool_lib::tools::model_converter::model::ModelFile;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 4 {
        eprintln!("usage: model_extract <toc> <archives dir> <dag> <out dir> [--tags 0xAAAA,0xBBBB] [--limit N]");
        std::process::exit(1);
    }
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let tags: Vec<u32> = get("--tags")
        .map(|v| {
            v.split(',')
                .filter_map(|t| u32::from_str_radix(t.trim().trim_start_matches("0x"), 16).ok())
                .collect()
        })
        .unwrap_or_default();
    let limit: usize = get("--limit").and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
    let out_dir = PathBuf::from(&args[3]);
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    let toc = Toc::parse(&std::fs::read(&args[0]).expect("read toc")).expect("parse toc");
    let dag = Dag::load(Path::new(&args[2])).expect("read dag");
    let names = toc.archive_filenames();
    let cache = toc.archive_cache(Path::new(&args[1]));

    let mut seen = HashSet::new();
    let mut written = 0usize;
    for asset in toc.assets() {
        if written >= limit {
            break;
        }
        let is_model_archive = names
            .get(asset.archive_index as usize)
            .is_some_and(|n| n.to_lowercase().contains("model"));
        if !is_model_archive || !seen.insert(asset.asset_id) {
            continue;
        }
        let Ok(raw) = toc.extract_asset_with_cache(&asset, &cache) else { continue };
        let Ok(model) = ModelFile::parse(&raw) else { continue };
        if !tags.is_empty() && !tags.iter().any(|t| model.dat1.get_section_data(*t).is_some()) {
            continue;
        }
        let stem = dag
            .index_of(asset.asset_id)
            .and_then(|i| dag.name(i))
            .and_then(|n| n.rsplit(['/', '\\']).next())
            .map(|n| n.trim_end_matches(".model").to_string())
            .unwrap_or_default();
        std::fs::write(out_dir.join(format!("{:016X}_{stem}.model", asset.asset_id)), &raw).expect("write");
        written += 1;
    }
    println!("wrote {written} models to {}", out_dir.display());
}
