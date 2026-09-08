//! TEMPORARY adversarial verification harness — delete when done.
//!
//! Independent of `level_check` / `coverage_audit`: it re-implements every
//! measurement (winding agreement, watertightness, signed volume, decal
//! placement, triangle budget) from the decoder output alone.
//!
//!   cargo run --bin verify_audit -- "<game dir>"

use std::collections::HashMap;

use omnitool_lib::core::level::{self, MeshOut};
use omnitool_lib::core::zone::{Zone, KIND_DECAL, KIND_IMPOSTOR, KIND_MODEL, T_DECAL_GEOMETRY};

// ---------------------------------------------------------------------------
// geometry measurements (written from scratch here on purpose)
// ---------------------------------------------------------------------------

struct MeshStats {
    tris: usize,
    /// unweighted fraction of triangles whose CCW face normal agrees with the
    /// averaged stored vertex normal
    agree: f64,
    /// same, weighted by triangle area
    agree_area: f64,
    sampled: usize,
    has_normals: bool,
    /// every undirected edge used exactly twice, in opposite directions
    watertight: bool,
    /// 6 * signed volume of the closed mesh (only meaningful if watertight)
    signed_volume: f64,
}

fn measure(m: &MeshOut) -> MeshStats {
    let vc = m.positions.len() / 3;
    let tris = m.indices.len() / 3;
    let has_normals = m.normals.iter().any(|v| *v != 0.0);

    // Weld by quantised position: decoded meshes duplicate vertices at UV /
    // normal seams, so an index-based edge test would call every closed mesh
    // open. 1e-4 of a metre is far below the smallest authored feature.
    let mut weld: HashMap<(i64, i64, i64), u32> = HashMap::new();
    let mut welded: Vec<u32> = Vec::with_capacity(vc);
    for i in 0..vc {
        let q = (
            (m.positions[i * 3] as f64 * 10_000.0).round() as i64,
            (m.positions[i * 3 + 1] as f64 * 10_000.0).round() as i64,
            (m.positions[i * 3 + 2] as f64 * 10_000.0).round() as i64,
        );
        let n = weld.len() as u32;
        welded.push(*weld.entry(q).or_insert(n));
    }

    let p = |i: u32| -> [f64; 3] {
        let i = i as usize * 3;
        [
            m.positions[i] as f64,
            m.positions[i + 1] as f64,
            m.positions[i + 2] as f64,
        ]
    };
    let n = |i: u32| -> [f64; 3] {
        let i = i as usize * 3;
        [
            m.normals[i] as f64,
            m.normals[i + 1] as f64,
            m.normals[i + 2] as f64,
        ]
    };

    let mut agree = 0usize;
    let mut sampled = 0usize;
    let mut area_agree = 0.0f64;
    let mut area_total = 0.0f64;
    let mut volume = 0.0f64;
    // directed edge -> count
    let mut edges: HashMap<(u32, u32), i32> = HashMap::new();
    let mut ok_edges = true;

    for t in 0..tris {
        let (i0, i1, i2) = (m.indices[t * 3], m.indices[t * 3 + 1], m.indices[t * 3 + 2]);
        if i0 as usize >= vc || i1 as usize >= vc || i2 as usize >= vc {
            ok_edges = false;
            continue;
        }
        let (w0, w1, w2) = (welded[i0 as usize], welded[i1 as usize], welded[i2 as usize]);
        if w0 == w1 || w1 == w2 || w0 == w2 {
            continue; // degenerate after welding: no edge, no area
        }
        let (a, b, c) = (p(i0), p(i1), p(i2));
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let f = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        let flen = (f[0] * f[0] + f[1] * f[1] + f[2] * f[2]).sqrt();

        // 6 * signed volume of the tetra (origin, a, b, c)
        volume += a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
            + a[2] * (b[0] * c[1] - b[1] * c[0]);

        for (x, y) in [(w0, w1), (w1, w2), (w2, w0)] {
            *edges.entry((x, y)).or_insert(0) += 1;
        }

        if has_normals && flen > 1e-12 {
            let na = n(i0);
            let nb = n(i1);
            let nc = n(i2);
            let s = [na[0] + nb[0] + nc[0], na[1] + nb[1] + nc[1], na[2] + nb[2] + nc[2]];
            if s[0] * s[0] + s[1] * s[1] + s[2] * s[2] > 1e-12 {
                sampled += 1;
                area_total += flen;
                if f[0] * s[0] + f[1] * s[1] + f[2] * s[2] > 0.0 {
                    agree += 1;
                    area_agree += flen;
                }
            }
        }
    }

    // watertight and consistently oriented: every directed edge appears once
    // and its opposite appears once too.
    let mut watertight = ok_edges && !edges.is_empty();
    if watertight {
        for (&(x, y), &c) in &edges {
            if c != 1 || edges.get(&(y, x)).copied().unwrap_or(0) != 1 {
                watertight = false;
                break;
            }
        }
    }

    MeshStats {
        tris,
        agree: if sampled == 0 { -1.0 } else { agree as f64 / sampled as f64 },
        agree_area: if area_total == 0.0 { -1.0 } else { area_agree / area_total },
        sampled,
        has_normals,
        watertight,
        signed_volume: volume / 6.0,
    }
}

// ---------------------------------------------------------------------------

#[derive(Default)]
struct Acc {
    n: usize,
    sum_area: f64,
    below_09: usize,
    below_04: usize,
    min: f64,
    worst: String,
    watertight: usize,
    neg_volume: usize,
    zero_agree: usize,
}

impl Acc {
    fn new() -> Self {
        Acc { min: 2.0, ..Default::default() }
    }
    fn add(&mut self, label: &str, s: &MeshStats) {
        if s.sampled == 0 {
            return;
        }
        self.n += 1;
        self.sum_area += s.agree_area;
        if s.agree_area < 0.9 {
            self.below_09 += 1;
        }
        if s.agree_area < 0.4 {
            self.below_04 += 1;
        }
        if s.agree_area == 0.0 {
            self.zero_agree += 1;
        }
        if s.agree_area < self.min {
            self.min = s.agree_area;
            self.worst = label.to_string();
        }
        if s.watertight {
            self.watertight += 1;
            if s.signed_volume < 0.0 {
                self.neg_volume += 1;
            }
        }
    }
    fn report(&self, what: &str) {
        if self.n == 0 {
            println!("  {what}: nothing measured");
            return;
        }
        println!(
            "  {what}: n={} mean_area_agreement={:.4} <0.9:{} <0.4:{} ==0.0:{} min={:.4} ({}) watertight={} neg_volume={}",
            self.n,
            self.sum_area / self.n as f64,
            self.below_09,
            self.below_04,
            self.zero_agree,
            self.min,
            self.worst,
            self.watertight,
            self.neg_volume
        );
    }
}

fn mat_mul_point(m: &[f32; 16], p: [f32; 3]) -> [f64; 3] {
    // row-major, row 3 = translation
    let (x, y, z) = (p[0] as f64, p[1] as f64, p[2] as f64);
    [
        x * m[0] as f64 + y * m[4] as f64 + z * m[8] as f64 + m[12] as f64,
        x * m[1] as f64 + y * m[5] as f64 + z * m[9] as f64 + m[13] as f64,
        x * m[2] as f64 + y * m[6] as f64 + z * m[10] as f64 + m[14] as f64,
    ]
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

    // Pick zones independently: the biggest `art` zone of each openarea region,
    // plus the biggest impostor zone found anywhere.
    let mut per_region: HashMap<String, (&str, u32)> = HashMap::new();
    let mut impostor_zones: Vec<(&str, u32)> = Vec::new();
    for z in &index.zones {
        if z.path.contains("impostor") {
            impostor_zones.push((&z.path, z.size));
        }
        if z.kind != "art" || !z.path.contains("/openarea/") {
            continue;
        }
        let e = per_region.entry(z.region.clone()).or_insert((&z.path, 0));
        if z.size > e.1 {
            *e = (&z.path, z.size);
        }
    }
    impostor_zones.sort_by_key(|(_, s)| std::cmp::Reverse(*s));

    let mut picks: Vec<String> = per_region
        .values()
        .map(|(p, _)| (*p).to_string())
        .collect();
    picks.sort();
    for (p, _) in impostor_zones.iter().take(2) {
        picks.push((*p).to_string());
    }
    // Plus the four zones the other agents measured, so the numbers are comparable.
    for p in [
        "levels/i29/openarea/sargasso/tile_n19/tile_n19_art.zone",
        "levels/i29/openarea/megalopolis/tile_m37/tile_m37_art.zone",
        "levels/i29/openarea/nefarious_city/tile_e19/tile_e19_art.zone",
        "levels/i29/openarea/savali/tile_c17/tile_c17_art.zone",
    ] {
        if !picks.iter().any(|q| q == p) {
            picks.push(p.to_string());
        }
    }

    let mut models = Acc::new();
    let mut decals = Acc::new();
    let mut impostors = Acc::new();
    let mut seen_models: HashMap<u64, ()> = HashMap::new();
    let mut zone_rows: Vec<String> = Vec::new();
    let mut decal_placed_ok = 0usize;
    let mut decal_local_ok = 0usize;
    let mut decal_matched = 0usize;
    let mut model_decode_fail: Vec<u64> = Vec::new();
    let mut decal_placed_bad = 0usize;
    let mut decal_worst_r = 0.0f64;
    let mut impostor_placed_ok = 0usize;
    let mut impostor_placed_bad = 0usize;

    for path in &picks {
        let Some(rec) = index.zone(path) else {
            println!("!! zone not in index: {path}");
            continue;
        };
        let Ok(raw) = index.extract(rec.id) else {
            println!("!! extract failed: {path}");
            continue;
        };
        let Ok(zone) = Zone::parse(&raw) else {
            println!("!! parse failed: {path}");
            continue;
        };

        // -- model geometry -------------------------------------------------
        let mut model_tris_inst = 0u64;
        let mut model_inst = 0u64;
        let mut model_missing = 0u64;
        let mut unique_tris: HashMap<u64, usize> = HashMap::new();
        for o in zone.model_instances() {
            model_inst += 1;
            let tris = if let Some(t) = unique_tris.get(&o.model_id) {
                *t
            } else {
                let t = (|| -> Option<usize> {
                    let raw = index.extract(o.model_id).ok()?;
                    let mf = level::parse_model_asset(&raw).ok()?;
                    let mesh = level::decode_model_mesh(&mf, 0).ok()?;
                    let s = measure(&mesh);
                    if seen_models.insert(o.model_id, ()).is_none() {
                        models.add(&format!("0x{:016x}", o.model_id), &s);
                    }
                    Some(s.tris)
                })()
                .unwrap_or_else(|| {
                    if !model_decode_fail.contains(&o.model_id) {
                        model_decode_fail.push(o.model_id);
                    }
                    0
                });
                unique_tris.insert(o.model_id, t);
                t
            };
            if tris == 0 {
                model_missing += 1;
            }
            model_tris_inst += tris as u64;
        }

        // -- block-2 geometry ------------------------------------------------
        // Independent read of the Zone Decal Geometry table (88-byte rows) so
        // the placement check does not lean on the zone parser's own mapping.
        // Sphere keyed by the row index the entry itself names at +0x2BC.
        let heap = zone
            .dat1
            .get_section_data(omnitool_lib::core::zone::T_SCENE_OBJECTS)
            .unwrap_or(&[])
            .to_vec();
        let geo_sec = zone
            .dat1
            .get_section_data(T_DECAL_GEOMETRY)
            .unwrap_or(&[])
            .to_vec();
        let mut decal_spheres: HashMap<u32, ([f64; 3], f64, u32, u32)> = HashMap::new();
        for o in &zone.scene_objects {
            if o.kind != KIND_DECAL {
                continue;
            }
            let b = o.offset as usize + 0x2BC;
            if b + 4 > heap.len() {
                continue;
            }
            let row = u32::from_le_bytes(heap[b..b + 4].try_into().unwrap()) as usize;
            if (row + 1) * 88 > geo_sec.len() {
                continue;
            }
            let r = &geo_sec[row * 88..row * 88 + 88];
            let f = |x: usize| f32::from_le_bytes(r[x..x + 4].try_into().unwrap()) as f64;
            decal_spheres.insert(
                o.offset,
                (
                    [f(0x40), f(0x44), f(0x48)],
                    f(0x4c),
                    u32::from_le_bytes(r[0x50..0x54].try_into().unwrap()),
                    u16::from_le_bytes(r[0..2].try_into().unwrap()) as u32,
                ),
            );
        }
        let mut sphere_dump = 0;

        let mut geo_objects = 0usize;
        let mut geo_tris = 0u64;
        let mut geo_fail = 0usize;
        let mut imp_tris = 0u64;
        let mut dec_tris = 0u64;
        for o in &zone.scene_objects {
            if o.geometry.is_none() {
                continue;
            }
            geo_objects += 1;
            match level::decode_zone_geometry(&zone, o.offset) {
                Ok(mesh) => {
                    let s = measure(&mesh);
                    geo_tris += s.tris as u64;
                    let label = format!("{}@{}", path.rsplit('/').next().unwrap(), o.offset);
                    if o.kind == KIND_IMPOSTOR {
                        imp_tris += s.tris as u64;
                        impostors.add(&label, &s);
                        // Placement: the unit-box vertices, placed by the
                        // entry's own 4x4, must bracket the entry position.
                        let mut lo = [f64::INFINITY; 3];
                        let mut hi = [f64::NEG_INFINITY; 3];
                        for v in mesh.positions.chunks_exact(3) {
                            let w = mat_mul_point(&o.matrix, [v[0], v[1], v[2]]);
                            for k in 0..3 {
                                lo[k] = lo[k].min(w[k]);
                                hi[k] = hi[k].max(w[k]);
                            }
                        }
                        let p = o.position();
                        let inside = (0..3).all(|k| {
                            let pad = (hi[k] - lo[k]).max(1.0) * 0.05;
                            p[k] as f64 >= lo[k] - pad && p[k] as f64 <= hi[k] + pad
                        });
                        if inside {
                            impostor_placed_ok += 1;
                        } else {
                            impostor_placed_bad += 1;
                            if impostor_placed_bad < 4 {
                                println!(
                                    "   impostor {label} pos {:?} outside world box {:?}..{:?}",
                                    p, lo, hi
                                );
                            }
                        }
                    } else if o.kind == KIND_DECAL {
                        dec_tris += s.tris as u64;
                        decals.add(&label, &s);
                        let g = o.geometry.unwrap();
                        if let Some((c, r, vo, vc)) = decal_spheres.get(&o.offset).copied() {
                            if vo != g.vertex_offset || vc != g.vertex_count {
                                println!("   decal {label}: row/entry span mismatch");
                            }
                            // The sphere could be in world space or in the
                            // entry's local space — measure both and let the
                            // numbers say which.
                            let mut worst_w = 0.0f64;
                            let mut worst_l = 0.0f64;
                            for v in mesh.positions.chunks_exact(3) {
                                let w = mat_mul_point(&o.matrix, [v[0], v[1], v[2]]);
                                let dw = ((w[0] - c[0]).powi(2)
                                    + (w[1] - c[1]).powi(2)
                                    + (w[2] - c[2]).powi(2))
                                .sqrt();
                                worst_w = worst_w.max(dw / r.max(1e-6));
                                let dl = ((v[0] as f64 - c[0]).powi(2)
                                    + (v[1] as f64 - c[1]).powi(2)
                                    + (v[2] as f64 - c[2]).powi(2))
                                .sqrt();
                                worst_l = worst_l.max(dl / r.max(1e-6));
                            }
                            let worst = worst_w.min(worst_l);
                            if worst_w > 1.05 && sphere_dump < 3 {
                                sphere_dump += 1;
                                let p = o.position();
                                println!(
                                    "   decal {label}: entry pos {:.1?} sphere c {:.1?} r {r:.2} localbbox {:.2?}..{:.2?}",
                                    p,
                                    c,
                                    mesh.min,
                                    mesh.max
                                );
                            }
                            if worst_l <= 1.05 {
                                decal_local_ok += 1;
                            }
                            if worst_w <= 1.05 {
                                decal_placed_ok += 1;
                            } else {
                                decal_placed_bad += 1;
                                if decal_placed_bad < 4 {
                                    println!(
                                        "   decal {label} reaches {worst_w:.2} r in world, {worst_l:.2} r in local"
                                    );
                                }
                            }
                            decal_worst_r = decal_worst_r.max(worst);
                        }
                    }
                }
                Err(e) => {
                    geo_fail += 1;
                    if geo_fail < 3 {
                        println!("   geo decode fail {}@{}: {e}", path, o.offset);
                    }
                }
            }
        }

        let entities = zone
            .scene_objects
            .iter()
            .filter(|o| o.kind != KIND_MODEL && o.geometry.is_none())
            .count();
        let total_tris = model_tris_inst + geo_tris;
        zone_rows.push(format!(
            "{path}\n    objects={} kind0={} entities={} blockgeo={}({} fail)\n    tris: models(instanced)={} block2={} (impostor {} / decal {})  block2 share={:.1}%\n    model instances with no decoded mesh: {}",
            zone.scene_objects.len(),
            model_inst,
            entities,
            geo_objects,
            geo_fail,
            model_tris_inst,
            geo_tris,
            imp_tris,
            dec_tris,
            if total_tris > 0 { geo_tris as f64 * 100.0 / total_tris as f64 } else { 0.0 },
            model_missing
        ));
    }

    println!("\n=== ZONES ({}) ===", picks.len());
    for r in &zone_rows {
        println!("  {r}");
    }
    println!("\n=== WINDING vs STORED NORMALS (area-weighted) ===");
    models.report("model meshes (unique, lod0)");
    decals.report("block-2 decals");
    impostors.report("block-2 impostors");
    println!("\n=== PLACEMENT ===");
    println!(
        "  decals matched to a Zone Decal Geometry row: {decal_matched}; inside the sphere when placed by the entry 4x4: {decal_placed_ok} ok / {decal_placed_bad} outside; inside it in raw local space: {decal_local_ok} (best-of-both max reach {decal_worst_r:.3} r)"
    );
    println!(
        "  unique models that failed to decode: {} {:?}",
        model_decode_fail.len(),
        model_decode_fail
            .iter()
            .take(6)
            .map(|i| format!("0x{i:016x}"))
            .collect::<Vec<_>>()
    );
    println!("  impostor entry position inside its own world box: {impostor_placed_ok} ok / {impostor_placed_bad} outside");
    let _ = KIND_DECAL;
}
