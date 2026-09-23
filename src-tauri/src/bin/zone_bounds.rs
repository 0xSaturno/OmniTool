//! Which zones cover a given world point?
//!
//!   cargo run --release --bin zone_bounds -- "<game dir>" <x> <y> <z> [substring]
//!
//! Prints every loadable zone whose scene-object bounds contain the point, so a
//! "there is nothing here" report can be turned into "this zone holds it".

use omnitool_lib::core::level;
use omnitool_lib::core::zone::Zone;
use rayon::prelude::*;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let game = a
        .first()
        .cloned()
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let p = [
        a.get(1).and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0),
        a.get(2).and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0),
        a.get(3).and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0),
    ];
    let want = a.get(4).cloned().unwrap_or_default();
    let pad = 60.0f32;

    let idx = level::get_index(&game, None).expect("index");
    let zones: Vec<_> = idx
        .zones
        .iter()
        .filter(|z| (want.is_empty() || z.path.contains(&want)) && level::zone_has_objects(&idx, z.id))
        .map(|z| (z.path.clone(), z.id))
        .collect();
    println!("point ({:.1}, {:.1}, {:.1}); {} loadable zones scanned\n", p[0], p[1], p[2], zones.len());

    let mut hits: Vec<(String, usize, [f32; 3], [f32; 3])> = zones
        .par_iter()
        .filter_map(|(path, id)| {
            let raw = idx.extract(*id).ok()?;
            let zone = Zone::parse(&raw).ok()?;
            let (min, max) = zone.bounds()?;
            let inside = (0..3).all(|i| p[i] >= min[i] - pad && p[i] <= max[i] + pad);
            if inside {
                Some((path.clone(), zone.scene_objects.len(), min, max))
            } else {
                None
            }
        })
        .collect();
    hits.sort_by_key(|h| std::cmp::Reverse(h.1));

    println!("{} zones contain it (with a {pad:.0} m margin):", hits.len());
    for (path, n, min, max) in hits.iter().take(25) {
        println!(
            "  {:>6} objects  x[{:.0},{:.0}] y[{:.0},{:.0}] z[{:.0},{:.0}]  {}",
            n, min[0], max[0], min[1], max[1], min[2], max[2], path
        );
    }
}
