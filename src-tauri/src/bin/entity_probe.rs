//! Verification harness for the non-model zone entity kinds: volumes, curves
//! (splines/paths), VFX instances, decals and navigation data.
//!
//! Every mode re-measures [`omnitool_lib::core::zone_entities`] against the
//! shipped PC archives and prints counts, so a claim in that module's docs can
//! be reproduced from the command line.
//!
//! ```text
//!   entity_probe audit  [substr]   every kind, every matching zone, with checks
//!   entity_probe volumes [substr]  volume shapes, flags, extents, owners
//!   entity_probe curves  [substr]  spline layout, types, owners, lengths
//!   entity_probe vfx     [substr]  effect name resolution
//!   entity_probe decals  [substr]  decal field ranges
//!   entity_probe nav     [substr]  container / unit / tile-cache decode
//!   entity_probe navmesh <substr>  one unit zone's triangles, in detail
//!   entity_probe raw <substr> <kind>  hex of the first few entries of a kind
//! ```

use byteorder::{ByteOrder, LittleEndian as LE};
use omnitool_lib::core::zone_entities as ent;
use omnitool_lib::core::{level, section_names, zone::Zone};
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::sync::Mutex;

const GAME: &str = r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart";

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mode = a.first().cloned().unwrap_or_else(|| "audit".into());
    let arg = a.get(1).cloned().unwrap_or_default();
    let arg2 = a.get(2).cloned().unwrap_or_default();
    let game = std::env::var("RCRA_GAME").unwrap_or_else(|_| GAME.into());
    let idx = level::get_index(&game, None).expect("index");

    match mode.as_str() {
        "audit" => audit(&idx, &arg, "all"),
        "volumes" => audit(&idx, &arg, "volumes"),
        "curves" => audit(&idx, &arg, "curves"),
        "vfx" => audit(&idx, &arg, "vfx"),
        "decals" => audit(&idx, &arg, "decals"),
        "nav" => nav(&idx, &arg),
        "navmesh" => navmesh(&idx, &arg),
        "raw" => raw(&idx, &arg, &arg2),
        "sections" => sections(&idx, &arg),
        "navid" => navid(&idx, &arg),
        m => eprintln!("unknown mode {m:?}"),
    }
}

fn zones_matching<'a>(idx: &'a level::LevelIndex, want: &str) -> Vec<&'a level::ZoneRec> {
    idx.zones
        .iter()
        .filter(|z| want.is_empty() || z.path.contains(want))
        .collect()
}

// ---------------------------------------------------------------------------
// the whole-game audit
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Tally {
    zones_read: usize,
    vol: usize,
    vol_zones: usize,
    vol_shapes: BTreeMap<&'static str, usize>,
    vol_flags: [usize; 24],
    vol_light: BTreeMap<u8, usize>,
    vol_inv_exact: usize,
    vol_inv_off: usize,
    vol_plain_size_ok: usize,
    vol_plain_size_bad: usize,
    vol_cyl_ok: usize,
    vol_cyl_n: usize,
    vol_water: usize,
    vol_post: usize,
    vol_trans: usize,
    vol_owner: BTreeMap<String, usize>,
    vol_purpose: BTreeMap<String, usize>,
    vol_ext_eq_base: usize,
    cur: usize,
    cur_zones: usize,
    cur_layout_ok: usize,
    cur_empty: usize,
    cur_layout_bad: usize,
    cur_loop: usize,
    cur_normals: usize,
    cur_types: BTreeMap<&'static str, usize>,
    cur_points: usize,
    cur_len_total: f64,
    cur_owner: BTreeMap<String, usize>,
    cur_pc_hist: BTreeMap<u16, usize>,
    cur_seg_ok: usize,
    cur_seg_n: usize,
    vfx: usize,
    vfx_zones: usize,
    vfx_resolved: usize,
    vfx_unresolved: usize,
    vfx_sizes: BTreeMap<u32, usize>,
    vfx_lc: BTreeMap<u8, usize>,
    vfx_tint_white: usize,
    vfx_top: BTreeMap<String, usize>,
    vfx_max_index: u32,
    vfx_8c_ff: usize,
    vfx_94_one: usize,
    dec: usize,
    dec_zones: usize,
    dec_sizes: BTreeMap<u32, usize>,
    dec_with_geo: usize,
    dec_no_geo: usize,
    dec_types: BTreeMap<u8, usize>,
    dec_flags: [usize; 6],
    dec_fade: (f32, f32),
    dec_lod: (f32, f32),
    dec_proj: (f32, f32),
    dec_alpha: BTreeMap<u8, usize>,
    dec_mask_all: usize,
    dec_path_ok: usize,
    dec_path_bad: usize,
    samples: Vec<String>,
}

fn audit(idx: &level::LevelIndex, want: &str, only: &str) {
    let zones = zones_matching(idx, want);
    println!("{} zones match {want:?}; auditing {only}", zones.len());
    let t = Mutex::new(Tally {
        dec_fade: (f32::MAX, f32::MIN),
        dec_lod: (f32::MAX, f32::MIN),
        dec_proj: (f32::MAX, f32::MIN),
        ..Default::default()
    });

    zones.par_iter().for_each(|z| {
        let Ok(raw) = idx.extract(z.id) else { return };
        let Ok(zone) = Zone::parse(&raw) else { return };

        let vols = if only == "all" || only == "volumes" { ent::volumes(&zone) } else { Vec::new() };
        let curs = if only == "all" || only == "curves" { ent::curves(&zone) } else { Vec::new() };
        let vfxs = if only == "all" || only == "vfx" { ent::vfx_instances(&zone) } else { Vec::new() };
        let decs = if only == "all" || only == "decals" { ent::decals(&zone) } else { Vec::new() };
        let scene = zone.dat1.get_section_data(ent::TAG_SCENE_OBJECTS).unwrap_or(&[]);

        let mut a = t.lock().unwrap();
        a.zones_read += 1;

        // -- volumes --------------------------------------------------------
        if !vols.is_empty() {
            a.vol_zones += 1;
        }
        for v in &vols {
            a.vol += 1;
            *a.vol_shapes.entry(v.shape_name).or_default() += 1;
            for b in 0..24 {
                if v.flags & (1 << b) != 0 {
                    a.vol_flags[b] += 1;
                }
            }
            *a.vol_light.entry(v.lighting_conditions).or_default() += 1;
            let exact = (0..3).all(|i| {
                let e = v.extents[i];
                e != 0.0 && ((v.inv_extents[i] - 1.0 / e).abs() <= (1.0 / e).abs() * 1e-5)
            });
            if exact {
                a.vol_inv_exact += 1;
            } else {
                a.vol_inv_off += 1;
            }
            let plain = !v.has_water && !v.has_post_effects && !v.has_translucent_shadow;
            if plain {
                if v.size == 0xB0 {
                    a.vol_plain_size_ok += 1;
                } else {
                    a.vol_plain_size_bad += 1;
                }
            }
            if v.extents[1] != 0.0 {
                a.vol_cyl_n += 1;
                let pred = 1.0 - 0.5 / v.extents[1];
                if (v.unit_space_cylinder_height - pred).abs() <= pred.abs() * 1e-3 + 1e-4 {
                    a.vol_cyl_ok += 1;
                }
            }
            if (0..3).all(|i| (v.extents[i] - v.base_extents[i]).abs() < 1e-3) {
                a.vol_ext_eq_base += 1;
            }
            if v.has_water {
                a.vol_water += 1;
            }
            if v.has_post_effects {
                a.vol_post += 1;
            }
            if v.has_translucent_shadow {
                a.vol_trans += 1;
            }
            *a.vol_owner
                .entry(if v.actor_asset.is_empty() { "(none)".into() } else { v.actor_asset.to_lowercase() })
                .or_default() += 1;
            *a.vol_purpose.entry(v.purpose.clone()).or_default() += 1;
            if a.samples.len() < 24 && v.shape != 1 {
                a.samples.push(format!(
                    "  VOL {:<10} ext=[{:.2},{:.2},{:.2}] cyl={:.3} [{}] {:?} {}",
                    v.shape_name, v.extents[0], v.extents[1], v.extents[2],
                    v.unit_space_cylinder_height, v.flag_names.join("|"), v.actor_name, z.path
                ));
            }
        }

        // -- curves ---------------------------------------------------------
        if !curs.is_empty() {
            a.cur_zones += 1;
        }
        for c in &curs {
            a.cur += 1;
            a.cur_points += c.points.len();
            a.cur_len_total += c.length as f64;
            *a.cur_pc_hist.entry(c.point_count).or_default() += 1;
            *a.cur_types.entry(c.spline_type_name).or_default() += 1;
            if c.looping {
                a.cur_loop += 1;
            }
            if c.normals.is_some() {
                a.cur_normals += 1;
            }
            if c.layout_exact {
                a.cur_layout_ok += 1;
            } else if c.point_count == 0 && c.size == 0xB4 {
                a.cur_empty += 1;
            } else {
                a.cur_layout_bad += 1;
            }
            // For a Linear spline the stored segment length must be the plain
            // distance between consecutive control points.
            if c.spline_type == 2 && c.segment_lengths.len() + 1 == c.points.len() {
                a.cur_seg_n += 1;
                let ok = c.segment_lengths.iter().enumerate().all(|(i, &l)| {
                    let p = c.points[i];
                    let q = c.points[i + 1];
                    let d = ((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2) + (q[2] - p[2]).powi(2)).sqrt();
                    (d - l).abs() <= l.abs() * 1e-3 + 1e-3
                });
                if ok {
                    a.cur_seg_ok += 1;
                }
            }
            *a.cur_owner
                .entry(if c.actor_asset.is_empty() { "(none)".into() } else { c.actor_asset.to_lowercase() })
                .or_default() += 1;
        }

        // -- vfx ------------------------------------------------------------
        if !vfxs.is_empty() {
            a.vfx_zones += 1;
        }
        let names_len = ent::vfx_names(&zone).0.len();
        for v in &vfxs {
            a.vfx += 1;
            *a.vfx_sizes.entry(v.size).or_default() += 1;
            *a.vfx_lc.entry(v.byte_9f).or_default() += 1;
            a.vfx_max_index = a.vfx_max_index.max(v.asset_index);
            if v.asset_path.is_empty() || v.asset_index as usize >= names_len {
                a.vfx_unresolved += 1;
            } else {
                a.vfx_resolved += 1;
                *a.vfx_top.entry(v.asset_path.clone()).or_default() += 1;
            }
            if v.tint == [1.0, 1.0, 1.0] {
                a.vfx_tint_white += 1;
            }
            if v.slot_8c == u32::MAX {
                a.vfx_8c_ff += 1;
            }
            if v.scale_94 == 1.0 {
                a.vfx_94_one += 1;
            }
        }

        // -- decals ---------------------------------------------------------
        if !decs.is_empty() {
            a.dec_zones += 1;
        }
        for d in &decs {
            a.dec += 1;
            *a.dec_sizes.entry(d.size).or_default() += 1;
            *a.dec_types.entry(d.asset_type).or_default() += 1;
            for b in 0..6 {
                if d.decal_flags & (1 << b) != 0 {
                    a.dec_flags[b] += 1;
                }
            }
            if d.geometry_row.is_some() {
                a.dec_with_geo += 1;
            } else {
                a.dec_no_geo += 1;
            }
            a.dec_fade = (a.dec_fade.0.min(d.fade_distance), a.dec_fade.1.max(d.fade_distance));
            a.dec_lod = (a.dec_lod.0.min(d.lod_distance), a.dec_lod.1.max(d.lod_distance));
            a.dec_proj = (a.dec_proj.0.min(d.projection_limit), a.dec_proj.1.max(d.projection_limit));
            *a.dec_alpha.entry(d.alpha).or_default() += 1;
            if d.av_material_mask_lo == u64::MAX && d.av_material_mask_hi == u64::MAX {
                a.dec_mask_all += 1;
            }
            let b = d.offset as usize + 0x80;
            let terminated = scene.get(b..b + 512).is_some_and(|s| s.contains(&0));
            let ext_ok = match d.asset_type {
                3 => d.material_path.ends_with(".texture"),
                1 => d.material_path.ends_with(".materialgraph"),
                _ => d.material_path.ends_with(".material"),
            };
            if terminated && ext_ok {
                a.dec_path_ok += 1;
            } else {
                a.dec_path_bad += 1;
            }
        }
    });

    let a = t.into_inner().unwrap();
    println!("\nzones read: {}", a.zones_read);

    if a.vol > 0 {
        println!("\n== VOLUMES: {} in {} zones ==", a.vol, a.vol_zones);
        println!("  shapes: {:?}", a.vol_shapes);
        println!("  +0xA4 is the exact reciprocal of +0x98: {} / off {}", a.vol_inv_exact, a.vol_inv_off);
        println!("  entry is exactly 0xB0 when it carries no sub-struct: {} ok / {} not", a.vol_plain_size_ok, a.vol_plain_size_bad);
        println!("  +0x94 == 1 - 0.5/extents.y: {} / {}", a.vol_cyl_ok, a.vol_cyl_n);
        println!("  shape extents == the base header's +0x50 extents: {} / {}", a.vol_ext_eq_base, a.vol);
        println!("  sub-structs: water={} postEffects={} translucentShadow={}", a.vol_water, a.vol_post, a.vol_trans);
        println!("  lighting-condition mask: {:?}", a.vol_light);
        println!("  flag bits:");
        for (i, c) in a.vol_flags.iter().enumerate() {
            if *c > 0 {
                println!("    bit {i:2} {:<30} {c}", ent::VOLUME_FLAG_NAMES[i]);
            }
        }
        top("  owning actor asset", &a.vol_owner, 18);
        top("  inferred purpose", &a.vol_purpose, 18);
    }

    if a.cur > 0 {
        println!("\n== CURVES: {} in {} zones ==", a.cur, a.cur_zones);
        println!("  exact array layout: {} ok / {} empty (0xB4, no points) / {} bad", a.cur_layout_ok, a.cur_empty, a.cur_layout_bad);
        println!("  control points total: {}  looping: {}  with normals: {}", a.cur_points, a.cur_loop, a.cur_normals);
        println!("  total authored path length: {:.0} m", a.cur_len_total);
        println!("  Linear-curve segment lengths match point distance: {} / {}", a.cur_seg_ok, a.cur_seg_n);
        println!("  spline types: {:?}", a.cur_types);
        let mut h: Vec<_> = a.cur_pc_hist.iter().collect();
        h.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        println!("  point-count histogram (top 10): {:?}", &h[..h.len().min(10)]);
        top("  owning actor asset", &a.cur_owner, 18);
    }

    if a.vfx > 0 {
        println!("\n== VFX: {} in {} zones ==", a.vfx, a.vfx_zones);
        println!("  entry sizes: {:?}", a.vfx_sizes);
        println!("  asset index resolves: {} / fails {}   max index seen: {}", a.vfx_resolved, a.vfx_unresolved, a.vfx_max_index);
        println!("  tint is pure white: {} / {}", a.vfx_tint_white, a.vfx);
        println!("  +0x8C is 0xFFFFFFFF: {} / {}   +0x94 is 1.0: {} / {}", a.vfx_8c_ff, a.vfx, a.vfx_94_one, a.vfx);
        println!("  byte at +0x9F: {:?}", a.vfx_lc);
        top("  most-used effects", &a.vfx_top, 12);
    }

    if a.dec > 0 {
        println!("\n== DECALS: {} in {} zones ==", a.dec, a.dec_zones);
        println!("  entry sizes: {:?}", a.dec_sizes);
        println!("  inline path is NUL-terminated with the extension its asset type implies: {} ok / {} not", a.dec_path_ok, a.dec_path_bad);
        println!("  with a Zone Decal Geometry row: {} / without: {}", a.dec_with_geo, a.dec_no_geo);
        println!("  asset type (low 2 bits of +0x2B7): {:?}", a.dec_types);
        for (i, c) in a.dec_flags.iter().enumerate() {
            if *c > 0 {
                println!("    flag bit {i} {:<28} {c}", ent::DECAL_FLAG_NAMES[i]);
            }
        }
        println!("  fade {:.2}..{:.2} m   lod {:.2}..{:.2} m   projection limit {:.3}..{:.3}",
            a.dec_fade.0, a.dec_fade.1, a.dec_lod.0, a.dec_lod.1, a.dec_proj.0, a.dec_proj.1);
        println!("  AV material mask is all-ones: {} / {}", a.dec_mask_all, a.dec);
        let mut al: Vec<_> = a.dec_alpha.iter().collect();
        al.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        println!("  alpha byte (top 6): {:?}", &al[..al.len().min(6)]);
    }

    if !a.samples.is_empty() {
        println!("\nnon-cuboid volume samples:");
        for s in &a.samples {
            println!("{s}");
        }
    }
}

fn top(label: &str, m: &BTreeMap<String, usize>, n: usize) {
    let mut v: Vec<_> = m.iter().collect();
    v.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
    println!("{label} (top {n} of {}):", m.len());
    for (k, c) in v.iter().take(n) {
        println!("    {c:>7}  {k}");
    }
}

// ---------------------------------------------------------------------------
// navigation
// ---------------------------------------------------------------------------

#[derive(Default)]
struct NavAcc {
    containers: usize,
    container_size_exact: usize,
    container_log: BTreeMap<i32, usize>,
    container_tiled: usize,
    container_tiled_exact: usize,
    container_bad: Vec<String>,
    units: usize,
    unit_magic: BTreeMap<u32, usize>,
    container_magic: BTreeMap<u32, usize>,
    unit_id_matches_zone: usize,
    unit_id_differs: usize,
    container_links: BTreeMap<String, usize>,
    layers: BTreeMap<usize, usize>,
    layers_match_tilecache: usize,
    layers_mismatch: usize,
    log_scales: BTreeMap<i32, usize>,
    groups: usize,
    plain: usize,
    link: usize,
    verts: usize,
    tris: usize,
    tri_index_ok: usize,
    tri_index_bad: usize,
    vert_in_group: usize,
    vert_out_group: usize,
    tiles: usize,
    tile_magic_ok: usize,
    tile_bmin_hit: usize,
    tile_bmin_n: usize,
    y: (f32, f32),
    biggest: Vec<(usize, String)>,
}

fn nav(idx: &level::LevelIndex, want: &str) {
    let zones: Vec<_> = zones_matching(idx, want)
        .into_iter()
        .filter(|z| z.path.contains("navdata") || z.path.contains("nav"))
        .collect();
    println!("{} nav-ish zones", zones.len());

    // Pass 1: every container's own asset id, so a unit's container id can be
    // resolved without any name list.
    let containers: BTreeMap<u64, String> = zones
        .par_iter()
        .filter_map(|z| {
            let raw = idx.extract(z.id).ok()?;
            let zone = Zone::parse(&raw).ok()?;
            let c = ent::nav_container(&zone)?;
            Some((c.asset_id, z.path.clone()))
        })
        .collect();
    println!("containers by their own asset id: {}", containers.len());

    let acc = Mutex::new(NavAcc { y: (f32::MAX, f32::MIN), ..Default::default() });

    zones.par_iter().for_each(|z| {
        let Ok(raw) = idx.extract(z.id) else { return };
        let Ok(zone) = Zone::parse(&raw) else { return };
        if !ent::has_nav(&zone) {
            return;
        }
        let u_sec = zone.dat1.get_section_data(ent::TAG_NAV_UNIT).map(<[u8]>::to_vec);
        let container = ent::nav_container(&zone);
        let unit = ent::nav_unit(&zone);
        let (tiles, terms) = ent::nav_tiles(&zone);
        let mut a = acc.lock().unwrap();

        if let Some(c) = &container {
            a.containers += 1;
            *a.container_magic.entry(c.magic).or_default() += 1;
            if c.size_exact {
                a.container_size_exact += 1;
            }
            *a.container_log.entry(c.log_scale).or_default() += 1;
            if !c.size_exact && a.container_bad.len() < 20 {
                a.container_bad.push(format!(
                    "    section {} vs expected {} (layers {} grid x[{}..{}] z[{}..{}] log2 {}) {}",
                    c.section_len, 0x20 + 8 * c.tile_count, c.layer_count,
                    c.min_x, c.max_x, c.min_z, c.max_z, c.log_scale, z.path
                ));
            }
            if c.log_scale >= 0 {
                a.container_tiled += 1;
                if c.size_exact {
                    a.container_tiled_exact += 1;
                }
            }
        }
        if let (Some(u), Some(sec)) = (&unit, &u_sec) {
            a.units += 1;
            *a.unit_magic.entry(u.magic).or_default() += 1;
            if u.asset_id == z.id {
                a.unit_id_matches_zone += 1;
            } else {
                a.unit_id_differs += 1;
            }
            let cname = containers
                .get(&u.container_id)
                .cloned()
                .unwrap_or_else(|| "(no container zone in this scan)".into());
            *a.container_links.entry(cname).or_default() += 1;
            *a.layers.entry(u.layer_count).or_default() += 1;
            *a.log_scales.entry(u.log_scale).or_default() += 1;
            if terms == u.layer_count {
                a.layers_match_tilecache += 1;
            } else {
                a.layers_mismatch += 1;
            }
            let tile = u.group_size_m.max(1);
            let mut ztris = 0usize;
            for g in &u.groups {
                a.groups += 1;
                if g.plain {
                    a.plain += 1;
                } else {
                    a.link += 1;
                }
                a.verts += g.vert_count as usize;
                a.tris += g.poly_count as usize;
                ztris += g.poly_count as usize;
                let vb = g.vert_offset as usize;
                let pb = g.poly_offset as usize;
                let vc = g.vert_count as usize;
                let pc = g.poly_count as usize;
                if vb + vc * 12 > sec.len() || pb + pc * 24 > sec.len() {
                    continue;
                }
                for i in 0..vc {
                    let mut v = [0f32; 3];
                    LE::read_f32_into(&sec[vb + i * 12..vb + i * 12 + 12], &mut v);
                    a.y = (a.y.0.min(v[1]), a.y.1.max(v[1]));
                    let inside = v[0] >= g.x as f32 - 1.0
                        && v[0] <= (g.x + tile) as f32 + 1.0
                        && v[2] >= g.z as f32 - 1.0
                        && v[2] <= (g.z + tile) as f32 + 1.0;
                    if inside {
                        a.vert_in_group += 1;
                    } else {
                        a.vert_out_group += 1;
                    }
                }
                for i in 0..pc {
                    let o = pb + i * 24;
                    let ok = (0..3).all(|k| (LE::read_u16(&sec[o + k * 2..o + k * 2 + 2]) as usize) < vc);
                    if ok {
                        a.tri_index_ok += 1;
                    } else {
                        a.tri_index_bad += 1;
                    }
                }
            }
            a.biggest.push((ztris, z.path.clone()));
            for t in tiles.iter().take(64) {
                a.tile_bmin_n += 1;
                if u.groups.iter().any(|g| {
                    (t.bmin[0] - g.x as f32).abs() < 0.5 && (t.bmin[2] - g.z as f32).abs() < 0.5
                }) {
                    a.tile_bmin_hit += 1;
                }
            }
        }
        a.tiles += tiles.len();
        a.tile_magic_ok += tiles.iter().filter(|t| t.magic_ok && t.version == 1).count();
    });

    let mut a = acc.into_inner().unwrap();
    let hex = |m: &BTreeMap<u32, usize>| {
        m.iter().map(|(k, v)| (format!("{k:08X}"), *v)).collect::<Vec<_>>()
    };
    println!("\ncontainers: {} (section length matches the computed tile count: {})", a.containers, a.container_size_exact);
    println!("container magic: {:?}", hex(&a.container_magic));
    println!("container grid log2: {:?}  (of the {} with a non-negative log2, {} have an exact section length)", a.container_log, a.container_tiled, a.container_tiled_exact);
    for b in &a.container_bad {
        println!("{b}");
    }
    println!("units: {}  magic: {:?}", a.units, hex(&a.unit_magic));
    println!("unit's +0x04 asset id equals the zone's own TOC id: {} / differs {}", a.unit_id_matches_zone, a.unit_id_differs);
    println!("nav layers per unit: {:?}  (agrees with the tile-cache terminator count: {} / {} mismatched)", a.layers, a.layers_match_tilecache, a.layers_mismatch);
    println!("group size log2: {:?}", a.log_scales);
    println!("groups with data: {}  plain (verts+tris only): {}  with link arrays: {}", a.groups, a.plain, a.link);
    println!("vertices: {}  triangles: {}", a.verts, a.tris);
    println!("triangle vertex indices in range: {} / out of range: {}", a.tri_index_ok, a.tri_index_bad);
    println!("vertices inside their own group footprint: {} / outside {}", a.vert_in_group, a.vert_out_group);
    println!("vertex y range: {:.1} .. {:.1}", a.y.0, a.y.1);
    println!("tile-cache records: {}  with 'RLTD' magic and version 1: {}", a.tiles, a.tile_magic_ok);
    println!("tile bmin lands on a group's SW corner: {} / {}", a.tile_bmin_hit, a.tile_bmin_n);
    top("units by container", &a.container_links, 10);
    a.biggest.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
    println!("biggest nav units:");
    for (n, p) in a.biggest.iter().take(10) {
        println!("    {n:>9} tris  {p}");
    }
}

/// How does the asset id a nav unit stores relate to the zone's own TOC id?
fn navid(idx: &level::LevelIndex, want: &str) {
    let mut shown = 0;
    for z in idx.zones.iter().filter(|z| z.path.contains("navdata")) {
        if !want.is_empty() && !z.path.contains(want) {
            continue;
        }
        let Ok(raw) = idx.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&raw) else { continue };
        let Some(u) = ent::nav_unit(&zone) else { continue };
        let own = hashes(&z.path);
        println!("\n{}", z.path);
        println!("  toc id            {:016X}", z.id);
        println!("  crc64 of the path {own:016X}");
        println!("  unit +0x04        {:016X}   swapped {:016X}", u.asset_id, u.asset_id.swap_bytes());
        println!("  unit +0x0C        {:016X}   swapped {:016X}", u.container_id, u.container_id.swap_bytes());
        for cand in [u.asset_id, u.asset_id.swap_bytes(), u.asset_id & 0x0000_FFFF_FFFF_FFFF] {
            if let Some(n) = idx.name_of(cand) {
                println!("  +0x04 {cand:016X} names {n}");
            }
        }
        for cand in [u.container_id, u.container_id.swap_bytes(), u.container_id & 0x0000_FFFF_FFFF_FFFF] {
            if let Some(n) = idx.name_of(cand) {
                println!("  +0x0C {cand:016X} names {n}");
            }
        }
        shown += 1;
        if shown >= 4 {
            return;
        }
    }
}

fn navmesh(idx: &level::LevelIndex, want: &str) {
    let Some(z) = idx.zones.iter().find(|z| z.path.contains(want)) else {
        println!("no zone matching {want:?}");
        return;
    };
    let raw = idx.extract(z.id).expect("extract");
    let zone = Zone::parse(&raw).expect("parse");
    println!("{}  {} bytes", z.path, raw.len());
    if let Some(c) = ent::nav_container(&zone) {
        println!(
            "container: magic={:08X} asset={} layers={} grid x[{}..{}] z[{}..{}] tile {} m, {} slots, size exact {}",
            c.magic, c.asset_id, c.layer_count, c.min_x, c.max_x, c.min_z, c.max_z,
            c.tile_size_m, c.tile_count, c.size_exact
        );
    }
    let Some(u) = ent::nav_unit(&zone) else {
        println!("no nav unit section");
        return;
    };
    println!(
        "unit: asset={} container={} grid x[{}..{}] z[{}..{}] {} m groups, {} layers x {} = {} rows, {} with data",
        u.asset_id, u.container_id, u.min_x, u.max_x, u.min_z, u.max_z, u.group_size_m,
        u.layer_count, u.groups_per_layer, u.layer_count * u.groups_per_layer, u.groups.len()
    );
    for l in 0..u.layer_count {
        let m = ent::nav_mesh(&zone, &u, Some(l));
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for v in &m.verts {
            for i in 0..3 {
                min[i] = min[i].min(v[i]);
                max[i] = max[i].max(v[i]);
            }
        }
        let area: f32 = m
            .tris
            .iter()
            .map(|t| {
                let (p, q, r) = (
                    m.verts[t[0] as usize],
                    m.verts[t[1] as usize],
                    m.verts[t[2] as usize],
                );
                let e0 = [q[0] - p[0], q[1] - p[1], q[2] - p[2]];
                let e1 = [r[0] - p[0], r[1] - p[1], r[2] - p[2]];
                let n = [
                    e0[1] * e1[2] - e0[2] * e1[1],
                    e0[2] * e1[0] - e0[0] * e1[2],
                    e0[0] * e1[1] - e0[1] * e1[0],
                ];
                0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt()
            })
            .sum();
        println!(
            "  layer {l}: {} verts {} tris, walkable area {:.0} m^2, bounds x[{:.1},{:.1}] y[{:.1},{:.1}] z[{:.1},{:.1}]",
            m.verts.len(), m.tris.len(), area, min[0], max[0], min[1], max[1], min[2], max[2]
        );
    }
    let (tiles, terms) = ent::nav_tiles(&zone);
    println!("tile cache: {} records, {} layer terminators", tiles.len(), terms);
    for t in tiles.iter().take(5) {
        println!(
            "  tx={} ty={} layer={} bmin=[{:.1},{:.1},{:.1}] bmax=[{:.1},{:.1},{:.1}] grid {}x{} used x[{}..{}] y[{}..{}] {} compressed bytes",
            t.tx, t.ty, t.tlayer, t.bmin[0], t.bmin[1], t.bmin[2], t.bmax[0], t.bmax[1], t.bmax[2],
            t.width, t.height, t.minx, t.maxx, t.miny, t.maxy, t.compressed_size
        );
    }
    if let Ok(dir) = std::env::var("ENTITY_PROBE_OUT") {
        let m = ent::nav_mesh(&zone, &u, None);
        let mut s = String::new();
        for v in &m.verts {
            s.push_str(&format!("v {} {} {}\n", v[0], v[1], v[2]));
        }
        for t in &m.tris {
            s.push_str(&format!("f {} {} {}\n", t[0] + 1, t[1] + 1, t[2] + 1));
        }
        let p = format!("{dir}/navmesh.obj");
        if std::fs::write(&p, s).is_ok() {
            println!("wrote {p} ({} verts, {} tris)", m.verts.len(), m.tris.len());
        }
    }
}

// ---------------------------------------------------------------------------
// raw helpers
// ---------------------------------------------------------------------------

fn raw(idx: &level::LevelIndex, want: &str, kind_s: &str) {
    let kind: u8 = kind_s.parse().unwrap_or(2);
    // First zone that matches AND actually holds an entry of that kind.
    let mut found = None;
    for z in idx.zones.iter().filter(|z| z.path.contains(want)) {
        let Ok(rawb) = idx.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&rawb) else { continue };
        if zone.scene_objects.iter().any(|o| o.kind == kind) {
            found = Some((z.path.clone(), rawb, zone));
            break;
        }
    }
    let Some((path, _rawb, zone)) = found else {
        println!("no zone matching {want:?} holds a kind-{kind} entry");
        return;
    };
    let scene = zone.dat1.get_section_data(ent::TAG_SCENE_OBJECTS).unwrap_or(&[]);
    println!("{}  {} scene objects", path, zone.scene_objects.len());
    for o in zone.scene_objects.iter().filter(|o| o.kind == kind).take(3) {
        println!("\n-- offset {:#x} size {:#x} flags {:08X} actor={}", o.offset, o.size, o.flags, o.actor_index);
        let end = (o.offset as usize + o.size as usize).min(scene.len());
        hexdump(scene, o.offset as usize, end - o.offset as usize);
    }
}

fn sections(idx: &level::LevelIndex, want: &str) {
    let zones = zones_matching(idx, want);
    println!("{} zones match {want:?}", zones.len());
    let tally = Mutex::new(BTreeMap::<u32, usize>::new());
    zones.par_iter().for_each(|z| {
        let Ok(raw) = idx.extract(z.id) else { return };
        let Ok(zone) = Zone::parse(&raw) else { return };
        let mut t = tally.lock().unwrap();
        for s in &zone.dat1.sections {
            *t.entry(s.tag).or_default() += 1;
        }
    });
    for (t, n) in tally.into_inner().unwrap() {
        println!("  {t:08X} x{n:<7} {}", section_names::section_name(t).unwrap_or("?"));
    }
}

fn hashes(path: &str) -> u64 {
    omnitool_lib::core::crc64::hash_raw(omnitool_lib::core::crc64::normalize_path(path).as_bytes())
}

fn hexdump(d: &[u8], start: usize, len: usize) {
    for off in (start..start + len).step_by(16) {
        let end = (off + 16).min(d.len());
        if off >= end {
            break;
        }
        let hex: String = d[off..end].iter().map(|b| format!("{b:02X} ")).collect();
        let asc: String = d[off..end]
            .iter()
            .map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' })
            .collect();
        println!("  {off:06X}  {hex:<48} |{asc}|");
    }
}
