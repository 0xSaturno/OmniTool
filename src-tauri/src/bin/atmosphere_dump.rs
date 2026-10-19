//! Bulk-extracts every `.atmosphere` asset and its header section, and checks each header against
//! `core::atmosphere::FIELDS`: values have the right shape, padding is zero, and every asset id
//! or path is one of the atmosphere's own dag dependencies.
//!
//! usage: atmosphere_dump <toc> <archives dir> <dag> <out dir>

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use omnitool_lib::core::atmosphere::{FieldKind, FIELDS, HEADER_SIZE};
use omnitool_lib::core::crc64;
use omnitool_lib::core::dag::Dag;
use omnitool_lib::core::dat1::Dat1;
use omnitool_lib::core::toc::Toc;

const ATMOSPHERE_SECTION_HEADER: u32 = 0x02F0_6D4E;
const ATMOSPHERE_SECTION_TEXTURE: u32 = 0x71C1_68B4;
const ATMOSPHERE_SECTION_STRINGS: u32 = 0x72F2_8658;
const WRAPPER_SIZE: usize = 36;

/// `(field, problem)` for every value in `header` that does not fit the layout.
fn check_header(header: &[u8], dat1: &[u8], deps: &HashSet<u64>) -> Vec<(String, &'static str)> {
    let mut issues = Vec::new();
    let mut covered = vec![false; header.len()];
    let u32_at = |o: usize| u32::from_le_bytes(header[o..o + 4].try_into().unwrap());
    let check_id = |id: u64, issues: &mut Vec<(String, &'static str)>, name: &str| {
        if id != 0 && id != u64::MAX && !deps.contains(&id) {
            issues.push((name.to_string(), "id is not a dag dependency"));
        }
    };
    for f in FIELDS {
        let (o, size) = (f.offset, f.kind.size());
        if o + size > header.len() {
            issues.push((f.name.to_string(), "past end of header"));
            continue;
        }
        covered[o..o + size].fill(true);
        let bytes = &header[o..o + size];
        match f.kind {
            FieldKind::F32 | FieldKind::Vec3 | FieldKind::Vec4 => {
                if bytes.chunks_exact(4).any(|c| {
                    let v = f32::from_le_bytes(c.try_into().unwrap());
                    !v.is_finite() || v.abs() > 1e7
                }) {
                    issues.push((f.name.to_string(), "float out of range"));
                }
            }
            FieldKind::U8 if bytes[0] > 16 => issues.push((f.name.to_string(), "byte out of range")),
            FieldKind::U8 | FieldKind::U32 => {}
            FieldKind::AssetRef => {
                if bytes[8..20].iter().any(|&b| b != 0) {
                    issues.push((f.name.to_string(), "reference record has non-zero runtime bytes"));
                }
                check_id(u64::from_le_bytes(bytes[..8].try_into().unwrap()), &mut issues, f.name);
            }
            FieldKind::AssetId => check_id(u64::from_le_bytes(bytes.try_into().unwrap()), &mut issues, f.name),
            FieldKind::PathOffset => {
                let raw = u32_at(o);
                if raw == u32::MAX {
                    continue;
                }
                let Some(tail) = dat1.get(raw as usize..) else {
                    issues.push((f.name.to_string(), "path offset past end of file"));
                    continue;
                };
                let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
                let path = String::from_utf8_lossy(&tail[..end]);
                if !path.is_empty() && !path.ends_with(".atmosphere") {
                    check_id(crc64::hash(&path), &mut issues, f.name);
                }
            }
        }
    }
    if header.iter().zip(&covered).any(|(&b, &c)| !c && b != 0) {
        issues.push(("<padding>".to_string(), "non-zero padding byte"));
    }
    issues
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 4 {
        eprintln!("usage: atmosphere_dump <toc> <archives dir> <dag> <out dir>");
        std::process::exit(1);
    }
    let out_dir = PathBuf::from(&args[3]);
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    let toc_bytes = std::fs::read(&args[0]).expect("read toc");
    let toc = Toc::parse(&toc_bytes).expect("parse toc");
    let archives_dir = PathBuf::from(&args[1]);

    let dag = Dag::load(Path::new(&args[2])).expect("read dag");
    let paths: HashMap<u64, String> = dag
        .named_assets()
        .filter(|(_, path)| path.to_ascii_lowercase().ends_with(".atmosphere"))
        .collect();
    println!("{} atmosphere ids in the dag", paths.len());

    let cache = toc.archive_cache(&archives_dir);
    let mut sizes: HashMap<u32, usize> = HashMap::new();
    let mut written = 0usize;
    let mut skipped = 0usize;
    let mut clean = 0usize;
    let mut issue_counts: BTreeMap<(String, &'static str), usize> = BTreeMap::new();

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

        let deps: HashSet<u64> = dag
            .index_of(asset.asset_id)
            .map(|i| dag.direct_dependencies(i, None).into_iter().map(|d| dag.id(d)).collect())
            .unwrap_or_default();
        let issues = check_header(header, &raw[WRAPPER_SIZE..], &deps);
        clean += issues.is_empty() as usize;
        for issue in issues {
            *issue_counts.entry(issue).or_default() += 1;
        }

        let stem = path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("atmosphere")
            .trim_end_matches(".atmosphere");
        std::fs::write(out_dir.join(format!("{stem}.atmosphere")), &raw).expect("write asset");
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
    println!("layout check ({HEADER_SIZE}-byte layout): {clean}/{written} headers clean");
    for ((field, problem), count) in &issue_counts {
        println!("  {field}: {problem} × {count}");
    }
}
