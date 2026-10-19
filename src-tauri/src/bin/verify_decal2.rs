//! TEMPORARY diagnostic — delete when done. Validates `entry+0x2BC` as the
//! Zone Decal Geometry row index for kind-3 scene objects.

use std::collections::HashMap;

use omnitool_lib::core::level;
use omnitool_lib::core::zone::{Zone, KIND_DECAL, TAG_DECAL_GEOMETRY, TAG_DECALS, TAG_SCENE_OBJECTS};

fn main() {
    let game = std::env::args()
        .nth(1)
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let step: usize = std::env::args().nth(2).and_then(|v| v.parse().ok()).unwrap_or(3);
    let hashes = std::env::var("RCRA_HASHES").unwrap_or_else(|_| {
        omnitool_lib::core::filesystem::hashes_path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let index = level::get_index(&game, Some(&hashes)).expect("index");

    let mut zones: Vec<(String, u64)> = index
        .zones
        .iter()
        .filter(|z| z.path.contains("/openarea/") || z.path.contains("/overlay/"))
        .map(|z| (z.path.clone(), z.id))
        .collect();
    zones.sort();
    let picks: Vec<_> = zones.into_iter().step_by(step).collect();

    let mut zones_used = 0usize;
    let mut decals = 0usize;
    let mut with_row = 0usize;
    let mut sentinel: HashMap<i64, usize> = HashMap::new();
    let mut in_sphere = 0usize;
    let mut out_sphere = 0usize;
    let mut worst = 0.0f64;
    let mut dup_rows = 0usize;
    let mut list_mismatch = 0usize;
    let mut shift_agrees = 0usize;
    let mut shift_disagrees = 0usize;

    for (path, id) in &picks {
        let Ok(raw) = index.extract(*id) else { continue };
        let Ok(zone) = Zone::parse(&raw) else { continue };
        let Some(heap) = zone.dat1.get_section_data(TAG_SCENE_OBJECTS).map(<[u8]>::to_vec) else {
            continue;
        };
        let geo = zone
            .dat1
            .get_section_data(TAG_DECAL_GEOMETRY)
            .unwrap_or(&[])
            .to_vec();
        let rows = geo.len() / 88;
        let list: Vec<u32> = zone
            .dat1
            .get_section_data(TAG_DECALS)
            .unwrap_or(&[])
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        let kind3: Vec<&_> = zone
            .scene_objects
            .iter()
            .filter(|o| o.kind == KIND_DECAL)
            .collect();
        if kind3.is_empty() {
            continue;
        }
        zones_used += 1;
        if list.len() != kind3.len() {
            list_mismatch += 1;
        }
        let deficit = list.len().saturating_sub(rows);
        let block = zone.block(2).unwrap_or(&[]);
        let mut used: HashMap<u32, u32> = HashMap::new();

        for o in &kind3 {
            decals += 1;
            let b = o.offset as usize + 0x2BC;
            if b + 4 > heap.len() {
                continue;
            }
            let idx = u32::from_le_bytes(heap[b..b + 4].try_into().unwrap());
            if idx as usize >= rows {
                *sentinel.entry(idx as i64).or_insert(0) += 1;
                continue;
            }
            with_row += 1;
            if used.insert(idx, o.offset).is_some() {
                dup_rows += 1;
            }
            // agreement with the naive "shift by deficit" rule
            if let Some(li) = list.iter().position(|&x| x == o.offset) {
                if li >= deficit && (li - deficit) as u32 == idx {
                    shift_agrees += 1;
                } else {
                    shift_disagrees += 1;
                }
            }

            let r = &geo[idx as usize * 88..idx as usize * 88 + 88];
            let f = |x: usize| f32::from_le_bytes(r[x..x + 4].try_into().unwrap()) as f64;
            let c = [f(0x40), f(0x44), f(0x48)];
            let rad = f(0x4c).max(1e-6);
            let vc = u16::from_le_bytes(r[0..2].try_into().unwrap()) as usize;
            let vo = u32::from_le_bytes(r[0x50..0x54].try_into().unwrap()) as usize;
            if vc == 0 || vo + vc * 16 > block.len() {
                continue;
            }
            let mut far = 0.0f64;
            for k in 0..vc {
                let vb = &block[vo + k * 16..vo + k * 16 + 16];
                let q =
                    |x: usize| i16::from_le_bytes(vb[x..x + 2].try_into().unwrap()) as f64 / 32768.0;
                let (x, y, z) = (q(0), q(2), q(4));
                let m = &o.matrix;
                let w = [
                    x * m[0] as f64 + y * m[4] as f64 + z * m[8] as f64 + m[12] as f64,
                    x * m[1] as f64 + y * m[5] as f64 + z * m[9] as f64 + m[13] as f64,
                    x * m[2] as f64 + y * m[6] as f64 + z * m[10] as f64 + m[14] as f64,
                ];
                far = far.max(
                    ((w[0] - c[0]).powi(2) + (w[1] - c[1]).powi(2) + (w[2] - c[2]).powi(2)).sqrt()
                        / rad,
                );
            }
            worst = worst.max(far);
            if far <= 1.05 {
                in_sphere += 1;
            } else {
                out_sphere += 1;
                if out_sphere <= 5 {
                    println!("  OUT {path} @{} row {idx} reach {far:.2} r", o.offset);
                }
            }
        }
    }

    println!("\nzones with decals: {zones_used} (Zone Decals list length != kind-3 count in {list_mismatch})");
    println!("kind-3 entries: {decals}; entry+0x2BC names a real row for {with_row}");
    let mut s: Vec<_> = sentinel.into_iter().collect();
    s.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    println!("  out-of-range values (no geometry): {:?}", &s[..s.len().min(5)]);
    println!("  duplicate row claims: {dup_rows}");
    println!("  mesh inside its row's sphere: {in_sphere} in / {out_sphere} out (worst reach {worst:.2} r)");
    println!("  vs the naive shift-by-deficit rule: agrees {shift_agrees}, disagrees {shift_disagrees}");
}
