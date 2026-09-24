//! Measures the navigation blobs of the shipped PC build.
//!
//!   cargo run --release --bin nav_probe -- "<game dir>" [mode] [zone substring]
//!
//! Read-only. Nothing is written to the game directory.

use std::collections::BTreeMap;

use byteorder::{ByteOrder, LE};
use omnitool_lib::core::level;
use omnitool_lib::core::zone::Zone;

const T_NAV_A: u32 = 0x339A_EC45;
const T_NAV_B: u32 = 0xA903_D8F1;
const T_NAV_C: u32 = 0xD124_430B;

fn hex(d: &[u8], base: usize, n: usize) {
    let n = n.min(d.len());
    let mut i = 0;
    while i < n {
        let end = (i + 16).min(n);
        print!("  {:08X}  ", base + i);
        for j in i..i + 16 {
            if j < end {
                print!("{:02X} ", d[j]);
            } else {
                print!("   ");
            }
        }
        print!(" |");
        for j in i..end {
            let c = d[j];
            print!("{}", if (0x20..0x7f).contains(&c) { c as char } else { b'.' as char });
        }
        print!("| ");
        // u32 / f32 view
        for j in (i..end).step_by(4) {
            if j + 4 <= end {
                let u = LE::read_u32(&d[j..]);
                let f = f32::from_bits(u);
                if f != 0.0 && f.abs() > 1e-6 && f.abs() < 1e7 {
                    print!("{:>12.3} ", f);
                } else {
                    print!("{:>12} ", u);
                }
            }
        }
        println!();
        i += 16;
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let game = args
        .next()
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let mode = args.next().unwrap_or_else(|| "census".into());
    let filter = args.next().unwrap_or_default();

    let idx = level::get_index(&game, None).expect("level index");

    let navs: Vec<&level::ZoneRec> = idx
        .zones
        .iter()
        .filter(|z| z.path.contains("navdata"))
        .filter(|z| filter.is_empty() || z.path.contains(&filter))
        .collect();

    match mode.as_str() {
        "census" => {
            let mut per_tag: BTreeMap<u32, (u64, u64)> = BTreeMap::new();
            let mut names: BTreeMap<String, usize> = BTreeMap::new();
            let mut combos: BTreeMap<String, usize> = BTreeMap::new();
            for z in &navs {
                *names
                    .entry(z.path.rsplit('/').next().unwrap_or("").to_string())
                    .or_default() += 1;
                let Ok(bytes) = idx.extract(z.id) else { continue };
                let Ok(zone) = Zone::parse(&bytes) else { continue };
                let mut tags: Vec<String> = Vec::new();
                for s in &zone.dat1.sections {
                    let e = per_tag.entry(s.tag).or_default();
                    e.0 += 1;
                    e.1 += s.size as u64;
                    tags.push(format!("{:08X}", s.tag));
                }
                tags.sort();
                *combos.entry(tags.join("+")).or_default() += 1;
            }
            println!("{} nav zones", navs.len());
            for (n, c) in &names {
                println!("  {c:5} x {n}");
            }
            println!("sections:");
            for (t, (c, b)) in &per_tag {
                println!("  {t:08X}  {c:5} zones  {b:>12} bytes");
            }
            println!("section sets:");
            for (k, c) in &combos {
                println!("  {c:5} x {k}");
            }
        }
        "head" => {
            for z in navs.iter().take(6) {
                let Ok(bytes) = idx.extract(z.id) else { continue };
                let Ok(zone) = Zone::parse(&bytes) else { continue };
                println!("\n=== {} (id {:016X}) ===", z.path, z.id);
                for s in &zone.dat1.sections {
                    println!("  section {:08X} size {}", s.tag, s.size);
                }
                for tag in [T_NAV_A, T_NAV_B, T_NAV_C] {
                    if let Some(d) = zone.dat1.get_section_data(tag) {
                        println!("--- {tag:08X} ({} bytes) ---", d.len());
                        hex(d, 0, 256);
                    }
                }
            }
        }
        "heads2" => {
            // one zone that has every section
            let mut shown = 0;
            for z in &navs {
                let Ok(bytes) = idx.extract(z.id) else { continue };
                let Ok(zone) = Zone::parse(&bytes) else { continue };
                if zone.dat1.get_section_data(T_NAV_A).is_none() {
                    continue;
                }
                println!("\n=== {} ===", z.path);
                for s in &zone.dat1.sections {
                    println!("  section {:08X} size {}", s.tag, s.size);
                }
                for tag in [0x04C1_9E69, 0x3D8D_BDB8, T_NAV_A] {
                    if let Some(d) = zone.dat1.get_section_data(tag) {
                        println!("--- {tag:08X} ({} bytes) ---", d.len());
                        hex(d, 0, 320);
                    }
                }
                shown += 1;
                if shown >= 3 {
                    break;
                }
            }
            // and a container section on its own
            for z in navs.iter().filter(|z| z.path.contains("container")).take(2) {
                let Ok(bytes) = idx.extract(z.id) else { continue };
                let Ok(zone) = Zone::parse(&bytes) else { continue };
                println!("\n=== CONTAINER {} ===", z.path);
                for tag in [0x04C1_9E69u32, 0x3D8D_BDB8] {
                    if let Some(d) = zone.dat1.get_section_data(tag) {
                        println!("--- {tag:08X} ({} bytes) ---", d.len());
                        hex(d, 0, 256);
                    }
                }
            }
        }
        "unit" => {
            // Solve for element sizes from the per-group offset deltas.
            let a16 = |n: usize| (n + 15) & !15usize;
            // candidate -> still possible
            let mut cand_poly: Vec<usize> = (1..=64).collect();
            let mut cand_edge: Vec<usize> = (1..=64).collect();
            let mut cand_clue: Vec<usize> = (1..=128).collect();
            let mut cand_bsph: Vec<usize> = (1..=64).collect();
            let mut cand_vert: Vec<usize> = (1..=64).collect();
            let mut groups_seen = 0u64;
            let mut zones_ok = 0u64;
            let mut zones_bad = 0u64;
            let mut layer_hist: BTreeMap<i32, u64> = BTreeMap::new();
            let mut tail_hist: BTreeMap<u32, u64> = BTreeMap::new();
            let mut pad_hist: BTreeMap<u16, u64> = BTreeMap::new();
            let mut magic_hist: BTreeMap<u32, u64> = BTreeMap::new();
            let mut tot_v = 0u64;
            let mut tot_p = 0u64;
            let mut tot_d = 0u64;
            let mut tot_e = [0u64; 4];
            let mut tot_c = 0u64;
            for z in &navs {
                let Ok(bytes) = idx.extract(z.id) else { continue };
                let Ok(zone) = Zone::parse(&bytes) else { continue };
                let Some(d) = zone.dat1.get_section_data(T_NAV_C) else { continue };
                if d.len() < 0x20 {
                    zones_bad += 1;
                    continue;
                }
                *magic_hist.entry(LE::read_u32(&d[0..])).or_default() += 1;
                let minx = LE::read_i16(&d[0x14..]) as i64;
                let minz = LE::read_i16(&d[0x16..]) as i64;
                let maxx = LE::read_i16(&d[0x18..]) as i64;
                let maxz = LE::read_i16(&d[0x1A..]) as i64;
                let cell = ((maxx - minx).max(0) * (maxz - minz).max(0)) as usize;
                // find layer count: the first non-empty group's absolute vert
                // offset must equal ALIGN16(0x20 + 48*G), G = layers*cell.
                let mut layers = 0i32;
                for l in 1..=16usize {
                    let g = l * cell;
                    let arr_end = 0x20 + 48 * g;
                    if arr_end > d.len() {
                        break;
                    }
                    let base = a16(arr_end);
                    let mut first = usize::MAX;
                    let mut sum = 0usize;
                    let mut ok = true;
                    for i in 0..g {
                        let o = 0x20 + 48 * i;
                        let ds = LE::read_u32(&d[o + 0x28..]) as usize;
                        if ds == 0 {
                            continue;
                        }
                        let vo = o + LE::read_u32(&d[o + 0x14..]) as usize;
                        if vo + ds > d.len() {
                            ok = false;
                            break;
                        }
                        first = first.min(vo);
                        sum += ds;
                    }
                    if ok && first == base && base + sum == d.len() {
                        layers = l as i32;
                        break;
                    }
                }
                if layers == 0 {
                    zones_bad += 1;
                    continue;
                }
                zones_ok += 1;
                *layer_hist.entry(layers).or_default() += 1;
                let g = layers as usize * cell;
                for i in 0..g {
                    let o = 0x20 + 48 * i;
                    let ds = LE::read_u32(&d[o + 0x28..]) as usize;
                    if ds == 0 {
                        continue;
                    }
                    groups_seen += 1;
                    let v = LE::read_u16(&d[o + 0x04..]) as usize;
                    let p = LE::read_u16(&d[o + 0x06..]) as usize;
                    let dc = LE::read_u16(&d[o + 0x08..]) as usize;
                    *pad_hist.entry(LE::read_u16(&d[o + 0x12..])).or_default() += 1;
                    let mut e = [0usize; 4];
                    for k in 0..4 {
                        e[k] = LE::read_u16(&d[o + 0x0A + 2 * k..]) as usize;
                        tot_e[k] += e[k] as u64;
                    }
                    *tail_hist.entry(LE::read_u32(&d[o + 0x2C..])).or_default() += 1;
                    tot_v += v as u64;
                    tot_p += p as u64;
                    tot_d += dc as u64;
                    tot_c += e[2] as u64;
                    let vo = LE::read_u32(&d[o + 0x14..]) as usize;
                    let eo = LE::read_u32(&d[o + 0x18..]) as usize;
                    let po = LE::read_u32(&d[o + 0x1C..]) as usize;
                    let co = LE::read_u32(&d[o + 0x20..]) as usize;
                    let bo = LE::read_u32(&d[o + 0x24..]) as usize;
                    let vs = eo.wrapping_sub(vo);
                    let es = po.wrapping_sub(eo);
                    let ps = co.wrapping_sub(po);
                    let cs = bo.wrapping_sub(co);
                    let bs = ds.wrapping_sub(bo.wrapping_sub(vo));
                    let et = e[0] + e[1] + e[2] + e[3];
                    cand_vert.retain(|&s| v == 0 || a16(v * s) == vs);
                    cand_edge.retain(|&s| et == 0 || a16(et * s) == es);
                    cand_poly.retain(|&s| p + dc == 0 || a16((p + dc) * s) == ps);
                    cand_clue.retain(|&s| e[2] == 0 || a16(e[2] * s) == cs);
                    cand_bsph.retain(|&s| p == 0 || a16(p * s) == bs);
                }
            }
            println!("zones ok {zones_ok}  bad {zones_bad}  groups with data {groups_seen}");
            println!("magic   {magic_hist:08X?}");
            println!("layers  {layer_hist:?}");
            println!("+0x0A   {pad_hist:?}");
            println!(
                "+0x2C   {:?}",
                tail_hist.iter().take(8).collect::<Vec<_>>()
            );
            println!("totals  verts {tot_v} polys {tot_p} disconnected {tot_d} clues {tot_c}");
            println!("edge type totals int/ext/custom/border {tot_e:?}");
            println!("sizeof Vec3    candidates {cand_vert:?}");
            println!("sizeof Edge    candidates {cand_edge:?}");
            println!("sizeof Poly    candidates {cand_poly:?}");
            println!("sizeof Clue    candidates {cand_clue:?}");
            println!("sizeof BSphere candidates {cand_bsph:?}");
        }
        "fields" => {
            let a16 = |n: usize| (n + 15) & !15usize;
            let mut p_u16_lt_v = [0u64; 12];
            let mut p_u16_lt_e = [0u64; 12];
            let mut p_u16_lt_p = [0u64; 12];
            let mut p_u8_hist: Vec<BTreeMap<u8, u64>> = vec![BTreeMap::new(); 24];
            let mut e_u16_lt_v = [0u64; 7];
            let mut e_u16_lt_p = [0u64; 7];
            let mut e_u8_hist: Vec<BTreeMap<u8, u64>> = vec![BTreeMap::new(); 14];
            let mut etype: BTreeMap<(usize, u8), u64> = BTreeMap::new();
            let mut polys_seen = 0u64;
            let mut edges_seen = 0u64;
            let mut bsph_ok = 0u64;
            let mut bsph_bad = 0u64;
            let mut vmin = [f32::MAX; 3];
            let mut vmax = [f32::MIN; 3];
            let mut vert_in_group = 0u64;
            let mut vert_out_group = 0u64;
            for z in navs.iter().take(60) {
                let Ok(bytes) = idx.extract(z.id) else { continue };
                let Ok(zone) = Zone::parse(&bytes) else { continue };
                let Some(d) = zone.dat1.get_section_data(T_NAV_C) else { continue };
                if d.len() < 0x20 {
                    continue;
                }
                let minx = LE::read_i16(&d[0x14..]) as i64;
                let minz = LE::read_i16(&d[0x16..]) as i64;
                let maxx = LE::read_i16(&d[0x18..]) as i64;
                let maxz = LE::read_i16(&d[0x1A..]) as i64;
                let cell = ((maxx - minx).max(0) * (maxz - minz).max(0)) as usize;
                let mut layers = 0usize;
                for l in 1..=16usize {
                    let g = l * cell;
                    if 0x20 + 48 * g > d.len() {
                        break;
                    }
                    let base = a16(0x20 + 48 * g);
                    let mut first = usize::MAX;
                    let mut sum = 0usize;
                    let mut ok = true;
                    for i in 0..g {
                        let o = 0x20 + 48 * i;
                        let ds = LE::read_u32(&d[o + 0x28..]) as usize;
                        if ds == 0 {
                            continue;
                        }
                        let vo = o + LE::read_u32(&d[o + 0x14..]) as usize;
                        if vo + ds > d.len() {
                            ok = false;
                            break;
                        }
                        first = first.min(vo);
                        sum += ds;
                    }
                    if ok && first == base && base + sum == d.len() {
                        layers = l;
                        break;
                    }
                }
                if layers == 0 {
                    continue;
                }
                for i in 0..layers * cell {
                    let o = 0x20 + 48 * i;
                    if LE::read_u32(&d[o + 0x28..]) == 0 {
                        continue;
                    }
                    let gx = LE::read_u16(&d[o..]) as i32 - 0x8000;
                    let gz = LE::read_u16(&d[o + 2..]) as i32 - 0x8000;
                    let v = LE::read_u16(&d[o + 0x04..]) as usize;
                    let p = LE::read_u16(&d[o + 0x06..]) as usize;
                    let dc = LE::read_u16(&d[o + 0x08..]) as usize;
                    let mut ec = [0usize; 4];
                    for k in 0..4 {
                        ec[k] = LE::read_u16(&d[o + 0x0A + 2 * k..]) as usize;
                    }
                    let et: usize = ec.iter().sum();
                    let vo = o + LE::read_u32(&d[o + 0x14..]) as usize;
                    let eo = o + LE::read_u32(&d[o + 0x18..]) as usize;
                    let po = o + LE::read_u32(&d[o + 0x1C..]) as usize;
                    let bo = o + LE::read_u32(&d[o + 0x24..]) as usize;
                    for k in 0..v {
                        let x = LE::read_f32(&d[vo + 12 * k..]);
                        let y = LE::read_f32(&d[vo + 12 * k + 4..]);
                        let zz = LE::read_f32(&d[vo + 12 * k + 8..]);
                        for (a, b) in [(0usize, x), (1, y), (2, zz)] {
                            if b < vmin[a] {
                                vmin[a] = b;
                            }
                            if b > vmax[a] {
                                vmax[a] = b;
                            }
                        }
                        if x >= gx as f32 - 0.01
                            && x <= gx as f32 + 16.01
                            && zz >= gz as f32 - 0.01
                            && zz <= gz as f32 + 16.01
                        {
                            vert_in_group += 1;
                        } else {
                            vert_out_group += 1;
                        }
                    }
                    for k in 0..p + dc {
                        let b = &d[po + 24 * k..po + 24 * k + 24];
                        polys_seen += 1;
                        for s in 0..12 {
                            let u = LE::read_u16(&b[2 * s..]) as usize;
                            if u < v {
                                p_u16_lt_v[s] += 1;
                            }
                            if u < et {
                                p_u16_lt_e[s] += 1;
                            }
                            if u < p + dc {
                                p_u16_lt_p[s] += 1;
                            }
                        }
                        for s in 0..24 {
                            *p_u8_hist[s].entry(b[s]).or_default() += 1;
                        }
                    }
                    for k in 0..et {
                        let b = &d[eo + 14 * k..eo + 14 * k + 14];
                        edges_seen += 1;
                        let kind = if k < ec[0] {
                            0
                        } else if k < ec[0] + ec[1] {
                            1
                        } else if k < ec[0] + ec[1] + ec[2] {
                            2
                        } else {
                            3
                        };
                        *etype.entry((kind, b[0])).or_default() += 1;
                        for s in 0..7 {
                            let u = LE::read_u16(&b[2 * s..]) as usize;
                            if u < v {
                                e_u16_lt_v[s] += 1;
                            }
                            if u < p + dc {
                                e_u16_lt_p[s] += 1;
                            }
                        }
                        for s in 0..14 {
                            *e_u8_hist[s].entry(b[s]).or_default() += 1;
                        }
                    }
                    for k in 0..p {
                        let cx = LE::read_f32(&d[bo + 16 * k..]);
                        let cz = LE::read_f32(&d[bo + 16 * k + 8..]);
                        let r = LE::read_f32(&d[bo + 16 * k + 12..]);
                        if r > 0.0
                            && r < 64.0
                            && cx >= gx as f32 - 1.0
                            && cx <= gx as f32 + 17.0
                            && cz >= gz as f32 - 1.0
                            && cz <= gz as f32 + 17.0
                        {
                            bsph_ok += 1;
                        } else {
                            bsph_bad += 1;
                        }
                    }
                }
            }
            let top = |h: &BTreeMap<u8, u64>| {
                let mut v: Vec<_> = h.iter().map(|(a, b)| (*a, *b)).collect();
                v.sort_by_key(|x| std::cmp::Reverse(x.1));
                v.into_iter().take(5).collect::<Vec<_>>()
            };
            println!("polys {polys_seen} edges {edges_seen}");
            println!("vert bounds  min {vmin:?}  max {vmax:?}");
            println!("verts inside own 16 m cell {vert_in_group} outside {vert_out_group}");
            println!("bspheres plausible {bsph_ok} implausible {bsph_bad}");
            println!("POLY u16 slots  <vertcount  <edgecount  <polycount   (of {polys_seen})");
            for s in 0..12 {
                println!(
                    "  +{:02X}  {:>10} {:>10} {:>10}",
                    2 * s,
                    p_u16_lt_v[s],
                    p_u16_lt_e[s],
                    p_u16_lt_p[s]
                );
            }
            println!("POLY u8 slots:");
            for s in 0..24 {
                println!("  +{:02X}  distinct {:>5}  top {:?}", s, p_u8_hist[s].len(), top(&p_u8_hist[s]));
            }
            println!("EDGE u16 slots  <vertcount  <polycount  (of {edges_seen})");
            for s in 0..7 {
                println!("  +{:02X}  {:>10} {:>10}", 2 * s, e_u16_lt_v[s], e_u16_lt_p[s]);
            }
            println!("EDGE u8 slots:");
            for s in 0..14 {
                println!("  +{:02X}  distinct {:>5}  top {:?}", s, e_u8_hist[s].len(), top(&e_u8_hist[s]));
            }
            println!("edge (rank, byte0) -> count {etype:?}");
        }
        _ => println!("modes: census head heads2 unit fields"),
    }
}
