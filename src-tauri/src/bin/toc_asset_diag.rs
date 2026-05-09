use std::path::Path;

use omnitool_lib::core::toc::Toc;

fn parse_hex_u64(s: &str) -> Result<u64, String> {
    let trimmed = s.trim();
    let no_prefix = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    u64::from_str_radix(no_prefix, 16).map_err(|e| format!("invalid hex asset id '{s}': {e}"))
}

fn read_u32_le(data: &[u8], off: usize) -> Option<u32> {
    if off + 4 > data.len() {
        return None;
    }
    Some(u32::from_le_bytes(data[off..off + 4].try_into().ok()?))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("Usage: toc_asset_diag <toc_path> <archives_dir> <asset_id_hex> [asset_id_hex ...]");
        std::process::exit(1);
    }

    let toc_path = &args[1];
    let archives_dir = Path::new(&args[2]);

    let toc_bytes = match std::fs::read(toc_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("failed to read TOC '{}': {e}", toc_path);
            std::process::exit(1);
        }
    };

    let toc = match Toc::parse(&toc_bytes) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("failed to parse TOC '{}': {e}", toc_path);
            std::process::exit(1);
        }
    };

    let assets = toc.assets();
    let archive_names = toc.archive_filenames();
    println!(
        "TOC loaded: {} total asset records, {} archives",
        assets.len(),
        toc.archive_count()
    );

    for query in &args[3..] {
        let asset_id = match parse_hex_u64(query) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("{e}");
                continue;
            }
        };

        let matching: Vec<_> = assets
            .iter()
            .enumerate()
            .filter(|(_, a)| a.asset_id == asset_id)
            .collect();

        println!("\nasset {:016X}: {} record(s)", asset_id, matching.len());
        if matching.is_empty() {
            continue;
        }

        for (global_idx, asset) in matching {
            let archive_name = archive_names
                .get(asset.archive_index as usize)
                .cloned()
                .unwrap_or_else(|| format!("<archive {} out of range>", asset.archive_index));
            println!(
                "  idx={} span={} archive={} ({}) offset={} size={} header_offset={}",
                global_idx,
                asset.span_index,
                asset.archive_index,
                archive_name,
                asset.offset,
                asset.size,
                asset.header_offset
            );

            match toc.extract_asset(asset, archives_dir) {
                Ok(raw) => {
                    let outer_magic = read_u32_le(&raw, 0).unwrap_or(0);
                    let outer_size = read_u32_le(&raw, 4).unwrap_or(0);
                    let dat1_magic = read_u32_le(&raw, 36).unwrap_or(0);
                    println!(
                        "    extracted_len={} outer_magic=0x{:08X} outer_size={} dat1_magic@36=0x{:08X}",
                        raw.len(),
                        outer_magic,
                        outer_size,
                        dat1_magic
                    );
                    if raw.len() >= 16 {
                        let head = raw
                            .iter()
                            .take(16)
                            .map(|b| format!("{:02X}", b))
                            .collect::<Vec<_>>()
                            .join(" ");
                        println!("    head16={head}");
                    }
                }
                Err(e) => {
                    println!("    extraction_error={e}");
                }
            }
        }
    }
}
