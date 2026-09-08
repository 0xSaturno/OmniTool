//! Why is a zone's block-2 geometry (impostor / decal meshes) not appearing?
//!
//!   cargo run --bin block2_probe -- "<game dir>" <region substring>
//!
//! For every loadable zone of the region it reports how many scene objects claim
//! block-2 geometry, how many actually decode, and the triangle totals, so a
//! "0 / N" counter in the viewport can be pinned on the backend or the frontend.

use omnitool_lib::core::level;
use omnitool_lib::core::zone::Zone;

fn main() {
    let mut args = std::env::args().skip(1);
    let game = args
        .next()
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let want = args.next().unwrap_or_else(|| "nefarious_city".into());
    let hashes = std::env::var("RCRA_HASHES")
        .unwrap_or_else(|_| r"E:\RCRALevelEditorViewer\refs\RCRA-Forge\hashes.txt".into());

    let idx = level::get_index(&game, Some(&hashes)).expect("index");

    let mut zones: Vec<String> = idx
        .zones
        .iter()
        .filter(|z| z.path.contains(&want))
        .map(|z| z.path.clone())
        .collect();
    zones.sort();
    println!("{} zones matching {want:?}\n", zones.len());

    let (mut tot_claim, mut tot_ok, mut tot_tris, mut tot_empty, mut tot_err) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut shown = 0usize;

    for path in &zones {
        let Some(rec) = idx.zone(path) else { continue };
        let Ok(raw) = idx.extract(rec.id) else { continue };
        let Ok(zone) = Zone::parse(&raw) else { continue };

        let mut claim = 0usize;
        let mut ok = 0usize;
        let mut tris = 0usize;
        let mut empty = 0usize;
        let mut err = 0usize;

        for o in &zone.scene_objects {
            // Only impostors (kind 6) and decals (kind 3) keep geometry in block 2.
            if o.kind != 3 && o.kind != 6 {
                continue;
            }
            claim += 1;
            match level::decode_zone_geometry(&zone, o.offset) {
                Ok(m) => {
                    let n = m.indices.len() / 3;
                    if n == 0 {
                        empty += 1;
                    } else {
                        ok += 1;
                        tris += n;
                    }
                }
                Err(_) => err += 1,
            }
        }

        tot_claim += claim;
        tot_ok += ok;
        tot_tris += tris;
        tot_empty += empty;
        tot_err += err;

        if claim > 0 && shown < 25 {
            shown += 1;
            println!(
                "{:<70} claim {:>4}  decoded {:>4}  empty {:>3}  err {:>3}  tris {:>8}",
                path.rsplit('/').next().unwrap(),
                claim,
                ok,
                empty,
                err,
                tris
            );
        }
    }

    println!(
        "\nTOTAL over {} zones: {} objects claim block-2 geometry, {} decoded ({} tris), {} empty, {} errored",
        zones.len(),
        tot_claim,
        tot_ok,
        tot_tris,
        tot_empty,
        tot_err
    );
    if tot_claim > 0 && tot_ok == 0 {
        println!("=> the BACKEND returns nothing; the viewport counter is honest.");
    } else if tot_ok > 0 {
        println!("=> the backend decodes fine; a 0/N counter in the viewport is a FRONTEND problem.");
    }
}
