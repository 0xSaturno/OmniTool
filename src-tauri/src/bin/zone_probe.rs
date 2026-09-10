use std::path::Path;

use omnitool_lib::core::crc64;
use omnitool_lib::core::dat1::{Dat1, DAT1_MAGIC};
use omnitool_lib::core::toc::Toc;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: zone_probe <game_dir> <asset_path>...");
        eprintln!("       zone_probe <game_dir> --stdin   (one asset path per line)");
        std::process::exit(1);
    }

    let game_dir = Path::new(&args[1]);
    let toc = Toc::parse(&std::fs::read(game_dir.join("toc")).unwrap()).unwrap();
    let archives = game_dir.to_path_buf();
    let assets = toc.assets();

    let show_refs = args.iter().any(|a| a == "--refs");
    let save_dir = args
        .iter()
        .position(|a| a == "--save")
        .and_then(|p| args.get(p + 1).cloned());
    let paths: Vec<String> = if args.iter().any(|a| a == "--stdin") {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf).unwrap();
        buf.lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    } else {
        let skip = save_dir.as_deref();
        args[2..]
            .iter()
            .filter(|a| !a.starts_with("--") && Some(a.as_str()) != skip)
            .cloned()
            .collect()
    };

    for path in paths {
        let id = crc64::hash(&path);
        let Some(asset) = assets.iter().find(|a| a.asset_id == id) else {
            println!("{path}: not in TOC ({id:016X})");
            continue;
        };
        let raw = match toc.extract_asset(asset, &archives) {
            Ok(r) => r,
            Err(e) => {
                println!("{path}: extract failed — {e}");
                continue;
            }
        };

        if let Some(dir) = &save_dir {
            let out = Path::new(dir).join(path.rsplit('/').next().unwrap_or("asset"));
            std::fs::create_dir_all(dir).ok();
            std::fs::write(&out, &raw).unwrap();
            println!("  saved {}", out.display());
        }

        let body = if raw.len() > 40
            && u32::from_le_bytes(raw[36..40].try_into().unwrap()) == DAT1_MAGIC
        {
            &raw[36..]
        } else {
            &raw[..]
        };
        match Dat1::parse(body) {
            Ok(d) => {
                let tags: Vec<String> = d
                    .sections
                    .iter()
                    .map(|s| format!("{:08X}:{}", s.tag, s.size))
                    .collect();
                println!("{path}  [{} bytes]  {}", raw.len(), tags.join(" "));
                if show_refs {
                    let refs = omnitool_lib::core::references::extract_references(&d);
                    let structured: Vec<_> = refs
                        .iter()
                        .filter(|r| r.source != "Strings Block")
                        .collect();
                    println!(
                        "  {} structured ref(s), {} from strings pool",
                        structured.len(),
                        refs.len() - structured.len()
                    );
                    for r in structured.iter().take(10) {
                        println!(
                            "      {:016X}  {}  {}",
                            r.asset_id,
                            r.source,
                            r.filename.as_deref().unwrap_or("<id only>")
                        );
                    }
                }
            }
            Err(e) => println!("{path}: not DAT1 — {e}"),
        }
    }
}
