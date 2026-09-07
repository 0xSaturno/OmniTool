//! Verification harness for `core::zone` against the shipped game files.
//!
//!   cargo run --bin zone_check -- "F:/SteamLibrary/steamapps/common/Ratchet & Clank - Rift Apart"
//!
//! Checks, on real zones of every kind:
//!   * the asset header / block split and the DAT1 type id
//!   * scene-object heap traversal and kind census
//!   * model-instance resolution (model index -> Zone Model Names -> a TOC asset)
//!   * actor rows, names and asset refs
//!   * byte-identical re-serialisation of an untouched zone
//!   * an in-place transform edit followed by undo restoring the original bytes

use omnitool_lib::core::toc::Toc;
use omnitool_lib::core::zone::{kind_name, Zone, KIND_MODEL};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const ZONES: &[&str] = &[
    "levels/i29/openarea/sargasso/tile_n19/tile_n19_art.zone",
    "levels/i29/openarea/nefarious_city/tile_e18/tile_e18_art.zone",
    "levels/i29/openarea/megalopolis/tile_o37/tile_o37_art.zone",
    "levels/i29/instance/pocketdimensions/pocketdimension_10/pd_10_geo.zone",
    "levels/i29/openarea/zordoom/tile_e22/tile_e22_gp.zone",
    "levels/i29/openarea/sargasso/tile_n19/tile_n19_gp_crates.zone",
    "levels/i29/openarea/blizar_intact/tile_b15/tile_b15_vfx.zone",
    "levels/i29/openarea/megalopolis/tile_q31/tile_q31_lgt.zone",
];

fn crc64(path: &str) -> u64 {
    omnitool_lib::core::crc64::hash(path)
}

fn main() {
    let game = std::env::args()
        .nth(1)
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let game = PathBuf::from(game);
    let toc_bytes = std::fs::read(game.join("toc")).expect("read toc");
    let toc = Toc::parse(&toc_bytes).expect("parse toc");
    // TOC archive names already include the "d\" prefix, so the archive root is
    // the game directory itself.
    let archives: &Path = &game;
    println!("toc: {} assets, {} archives\n", toc.asset_ids().len(), toc.archive_count());

    let assets = toc.assets();
    let by_id: BTreeMap<u64, _> = assets.iter().map(|a| (a.asset_id, a.clone())).collect();

    let mut failures = 0usize;
    for path in ZONES {
        let id = crc64(path);
        let Some(asset) = by_id.get(&id) else {
            println!("MISSING  {path}");
            failures += 1;
            continue;
        };
        let raw = match toc.extract_asset(asset, archives) {
            Ok(r) => r,
            Err(e) => {
                println!("EXTRACT FAIL {path}: {e}");
                failures += 1;
                continue;
            }
        };
        let mut zone = match Zone::parse(&raw) {
            Ok(z) => z,
            Err(e) => {
                println!("PARSE FAIL {path}: {e}");
                failures += 1;
                continue;
            }
        };

        let mut kinds: BTreeMap<u8, usize> = BTreeMap::new();
        for o in &zone.scene_objects {
            *kinds.entry(o.kind).or_default() += 1;
        }
        let resolved = zone.model_instances().count();
        let unresolved = zone
            .scene_objects
            .iter()
            .filter(|o| o.kind == KIND_MODEL && o.model_id == 0)
            .count();
        let in_toc = zone
            .model_instances()
            .filter(|o| by_id.contains_key(&o.model_id))
            .count();

        let short = path.rsplit('/').next().unwrap();
        println!("{short}");
        println!(
            "   blocks {:?}  sections {}  objects {}  actors {}  models(table) {}",
            zone.sizes,
            zone.dat1.sections.len(),
            zone.scene_objects.len(),
            zone.actors.len(),
            zone.model_ids.len()
        );
        let census: Vec<String> = kinds
            .iter()
            .map(|(k, n)| format!("{}={}", kind_name(*k), n))
            .collect();
        println!("   kinds: {}", census.join(" "));
        if resolved + unresolved > 0 {
            println!(
                "   model instances: {resolved} resolved ({in_toc} present in TOC), {unresolved} unresolved"
            );
            if resolved != in_toc {
                println!("   !! some resolved ids are not TOC assets");
                failures += 1;
            }
        }
        for a in zone.actors.iter().take(3) {
            println!(
                "   actor[{}] {:?} scene@{} id {:#018X} ref {}",
                a.index,
                a.name,
                a.scene_offset,
                a.instance_id,
                if a.asset_path.is_empty() { "-" } else { &a.asset_path }
            );
        }
        if let Some((min, max)) = zone.bounds() {
            println!(
                "   bounds  x[{:.1},{:.1}] y[{:.1},{:.1}] z[{:.1},{:.1}]",
                min[0], max[0], min[1], max[1], min[2], max[2]
            );
        }

        // round trip
        let rebuilt = zone.to_bytes();
        if rebuilt == raw {
            println!("   round trip: byte-identical ({} bytes)", rebuilt.len());
        } else {
            println!(
                "   !! round trip DIFFERS: {} vs {} bytes",
                rebuilt.len(),
                raw.len()
            );
            let n = rebuilt.len().min(raw.len());
            if let Some(i) = (0..n).find(|&i| rebuilt[i] != raw[i]) {
                println!("      first difference at byte {i}");
            }
            failures += 1;
        }

        // edit + undo
        if let Some(first) = zone.scene_objects.first().map(|o| (o.offset, o.matrix)) {
            let (off, m) = first;
            let mut moved = m;
            moved[12] += 1.0;
            zone.set_matrix(off, moved).expect("set matrix");
            let edited = zone.to_bytes();
            let changed = edited.iter().zip(raw.iter()).filter(|(a, b)| a != b).count();
            zone.undo();
            let restored = zone.to_bytes();
            println!(
                "   edit +1m on object @{off}: {changed} bytes differ; undo restores original: {}",
                restored == raw
            );
            if restored != raw || changed == 0 || changed > 4 {
                println!("   !! unexpected edit/undo behaviour");
                failures += 1;
            }
        }
        println!();
    }

    if failures == 0 {
        println!("all checks passed");
    } else {
        println!("{failures} check(s) FAILED");
        std::process::exit(1);
    }
}
