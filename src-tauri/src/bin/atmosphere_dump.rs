//! Bulk-extracts the header section of every `.atmosphere` asset, so the
//! cooked layout can be solved against the whole shipped set rather than one
//! file.
//!
//! usage: atmosphere_dump <toc> <archives dir> <hashes> <out dir>

use std::collections::HashMap;
use std::path::PathBuf;

use omnitool_lib::core::dat1::Dat1;
use omnitool_lib::core::toc::Toc;

const ATMOSPHERE_SECTION_HEADER: u32 = 0x02F0_6D4E;
const ATMOSPHERE_SECTION_TEXTURE: u32 = 0x71C1_68B4;
const ATMOSPHERE_SECTION_STRINGS: u32 = 0x72F2_8658;
const WRAPPER_SIZE: usize = 36;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 4 {
        eprintln!("usage: atmosphere_dump <toc> <archives dir> <hashes> <out dir>");
        std::process::exit(1);
    }
    let out_dir = PathBuf::from(&args[3]);
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    let toc_bytes = std::fs::read(&args[0]).expect("read toc");
    let toc = Toc::parse(&toc_bytes).expect("parse toc");
    let archives_dir = PathBuf::from(&args[1]);

    let mut paths: HashMap<u64, String> = HashMap::new();
    for line in std::fs::read_to_string(&args[2]).expect("read hashes").lines() {
        let mut parts = line.splitn(3, ',');
        let (Some(hex), Some(path)) = (parts.next(), parts.next()) else {
            continue;
        };
        if path.to_ascii_lowercase().ends_with(".atmosphere") {
            if let Ok(id) = u64::from_str_radix(hex.trim(), 16) {
                paths.insert(id, path.to_string());
            }
        }
    }
    println!("{} atmosphere ids in the hash list", paths.len());

    let cache = toc.archive_cache(&archives_dir);
    let mut sizes: HashMap<u32, usize> = HashMap::new();
    let mut written = 0usize;
    let mut skipped = 0usize;

    for asset in toc.assets().iter().filter(|a| a.span_index == 0) {
        let Some(path) = paths.get(&asset.asset_id) else {
            continue;
        };
        let Ok(raw) = toc.extract_asset_with_cache(asset, &cache) else {
            skipped += 1;
            continue;
        };
        if raw.len() <= WRAPPER_SIZE {
            skipped += 1;
            continue;
        }
        let Ok(dat1) = Dat1::parse(&raw[WRAPPER_SIZE..]) else {
            skipped += 1;
            continue;
        };
        let Some(header) = dat1.get_section_data(ATMOSPHERE_SECTION_HEADER) else {
            skipped += 1;
            continue;
        };
        *sizes.entry(header.len() as u32).or_default() += 1;

        let stem = path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("atmosphere")
            .trim_end_matches(".atmosphere");
        std::fs::write(out_dir.join(format!("{stem}.header.bin")), header).expect("write header");
        if let Some(tex) = dat1.get_section_data(ATMOSPHERE_SECTION_TEXTURE) {
            std::fs::write(out_dir.join(format!("{stem}.textures.bin")), tex).ok();
        }
        if let Some(strings) = dat1.get_section_data(ATMOSPHERE_SECTION_STRINGS) {
            std::fs::write(out_dir.join(format!("{stem}.strings.bin")), strings).ok();
        }
        written += 1;
    }

    let mut hist: Vec<(u32, usize)> = sizes.into_iter().collect();
    hist.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    println!("wrote {written} header sections ({skipped} skipped) to {}", out_dir.display());
    println!("header size histogram:");
    for (size, count) in hist.iter().take(12) {
        println!("  {size} bytes × {count}");
    }
}
