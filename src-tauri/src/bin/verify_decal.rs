//! TEMPORARY diagnostic — delete when done. Tests the Zone Decal Geometry
//! row <-> scene-object mapping across many shipped zones.

use omnitool_lib::core::level;
use omnitool_lib::core::zone::{Zone, KIND_DECAL, T_DECAL_GEOMETRY, T_DECALS};

/// How many of a zone's decals have their own mesh inside their own row's
/// bounding sphere, under `row i <- list[i + shift]`.
fn score(zone: &Zone, shift: usize) -> (usize, usize, f64) {
    let list: Vec<u32> = zone
        .dat1
        .get_section_data(T_DECALS)
        .unwrap_or(&[])
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    let geo = zone
        .dat1
        .get_section_data(T_DECAL_GEOMETRY)
        .unwrap_or(&[])
        .to_vec();
    let rows = geo.len() / 88;
    let block = zone.block(2).unwrap_or(&[]);
    let mut ok = 0;
    let mut n = 0;
    let mut worst = 0.0f64;

    for (i, &off) in list.iter().enumerate() {
        if i < shift || i - shift >= rows {
            continue;
        }
        let r = &geo[(i - shift) * 88..(i - shift) * 88 + 88];
        let f = |o: usize| f32::from_le_bytes(r[o..o + 4].try_into().unwrap()) as f64;
        let c = [f(0x40), f(0x44), f(0x48)];
        let rad = f(0x4c).max(1e-6);
        let vc = u16::from_le_bytes(r[0..2].try_into().unwrap()) as usize;
        let vo = u32::from_le_bytes(r[0x50..0x54].try_into().unwrap()) as usize;
        let Some(e) = zone.find(off) else { continue };
        if vc == 0 || vo + vc * 16 > block.len() {
            continue;
        }
        n += 1;
        // Decode positions straight out of block 2 (Model Std Vert, i16 x 4 at
        // 1/32768) so this does not depend on the decoder under test.
        let mut far = 0.0f64;
        for k in 0..vc {
            let b = &block[vo + k * 16..vo + k * 16 + 16];
            let q = |o: usize| i16::from_le_bytes(b[o..o + 2].try_into().unwrap()) as f64 / 32768.0;
            let (x, y, z) = (q(0), q(2), q(4));
            let m = &e.matrix;
            let w = [
                x * m[0] as f64 + y * m[4] as f64 + z * m[8] as f64 + m[12] as f64,
                x * m[1] as f64 + y * m[5] as f64 + z * m[9] as f64 + m[13] as f64,
                x * m[2] as f64 + y * m[6] as f64 + z * m[10] as f64 + m[14] as f64,
            ];
            let d = ((w[0] - c[0]).powi(2) + (w[1] - c[1]).powi(2) + (w[2] - c[2]).powi(2)).sqrt();
            far = far.max(d / rad);
        }
        worst = worst.max(far);
        if far <= 1.05 {
            ok += 1;
        }
    }
    (ok, n, worst)
}

fn main() {
    let game = std::env::args()
        .nth(1)
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let hashes = std::env::var("RCRA_HASHES").unwrap_or_else(|_| {
        omnitool_lib::core::filesystem::hashes_path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let index = level::get_index(&game, Some(&hashes)).expect("index");

    let mut zones: Vec<(String, u64)> = index
        .zones
        .iter()
        .filter(|z| z.kind == "art" && z.path.contains("/openarea/"))
        .map(|z| (z.path.clone(), z.id))
        .collect();
    zones.sort();
    // 60 zones spread over every planet, deterministically.
    let want: usize = std::env::args()
        .nth(2)
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    let step = (zones.len() / want).max(1);
    println!("{} art zones in index, sampling every {step}", zones.len());
    let picks: Vec<_> = zones.into_iter().step_by(step).collect();

    let (mut cur_ok, mut cur_n) = (0usize, 0usize);
    let (mut fix_ok, mut fix_n) = (0usize, 0usize);
    let mut zones_with_deficit = 0;
    let mut zones_scanned = 0;
    let mut deficit_decals = 0usize;
    let mut total_decals = 0usize;
    let mut cur_worst = 0.0f64;
    let mut fix_worst = 0.0f64;

    for (path, id) in &picks {
        let Ok(raw) = index.extract(*id) else { continue };
        let Ok(zone) = Zone::parse(&raw) else { continue };
        let ndec = zone
            .dat1
            .get_section_data(T_DECALS)
            .map(|s| s.len() / 4)
            .unwrap_or(0);
        let nrow = zone
            .dat1
            .get_section_data(T_DECAL_GEOMETRY)
            .map(|s| s.len() / 88)
            .unwrap_or(0);
        if ndec == 0 || nrow == 0 {
            continue;
        }
        zones_scanned += 1;
        total_decals += ndec;
        let deficit = ndec.saturating_sub(nrow);
        if deficit > 0 {
            zones_with_deficit += 1;
            deficit_decals += ndec;
        }
        let (a, an, aw) = score(&zone, 0);
        let (b, bn, bw) = score(&zone, deficit);
        cur_ok += a;
        cur_n += an;
        fix_ok += b;
        fix_n += bn;
        cur_worst = cur_worst.max(aw);
        fix_worst = fix_worst.max(bw);
        if deficit > 0 && zones_with_deficit <= 3 {
            let list: Vec<u32> = zone
                .dat1
                .get_section_data(T_DECALS)
                .unwrap_or(&[])
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
                .collect();
            for (k, &off) in list.iter().enumerate() {
                if k > deficit + 2 {
                    break;
                }
                if let Some(e) = zone.find(off) {
                    println!(
                        "     list[{k}] off {off} flags 0x{:08X} size {} scale {:?} path {:?}{}",
                        e.flags,
                        e.size,
                        e.scale,
                        e.asset_path,
                        if k < deficit { "   <- no row under the shift rule" } else { "" }
                    );
                }
            }
        }
        if deficit > 0 && zones_with_deficit <= 8 {
            println!(
                "  {path}: decals {ndec} rows {nrow} deficit {deficit} -> shipped mapping {a}/{an}, shifted {b}/{bn}"
            );
        }
        let _ = path;
    }
    println!(
        "\n{zones_scanned} art zones with decals ({total_decals} decals); {zones_with_deficit} of them have fewer geometry rows than decals ({deficit_decals} decals live in those zones)"
    );
    println!("  shipped mapping (row i <- list[i]):            {cur_ok}/{cur_n} decal meshes inside their own row's sphere (worst reach {cur_worst:.1} r)");
    println!("  shifted mapping (row i <- list[i + deficit]):  {fix_ok}/{fix_n} (worst reach {fix_worst:.1} r)");
    let _ = KIND_DECAL;
}
