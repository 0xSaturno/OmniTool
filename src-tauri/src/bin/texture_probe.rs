//! Dev harness: measure the model -> material -> texture chain on the shipped
//! archives, the way `level_model_materials` and `level_texture` will serve it.
//!
//!   cargo run --release --bin texture_probe -- [game dir] [mode] [arg] [out dir]
//!
//! Modes:
//!   zone <zone path>     every model of one zone: slot resolution, distinct
//!                        textures, decode cost at 512 and 1024, PNG samples
//!   region <region key>  every distinct model of a whole region (planet)
//!   census               header-only census of every `.texture` in the TOC
//!
//! The asset name list comes from $RCRA_HASHES, else OmniTool's app-dir file.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use rayon::prelude::*;

use omnitool_lib::core::model_materials::{
    self, model_materials, ModelMaterials,
};
use omnitool_lib::core::texture_serve;
use omnitool_lib::core::{crc64, level};

const DEFAULT_ZONE: &str = "levels/i29/openarea/nefarious_city/tile_e18/tile_e18_art.zone";

fn main() {
    let mut args = std::env::args().skip(1);
    let game = args
        .next()
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let mode = args.next().unwrap_or_else(|| "zone".into());
    let arg = args.next().unwrap_or_else(|| match mode.as_str() {
        "region" => "i29/openarea/nefarious_city".into(),
        _ => DEFAULT_ZONE.into(),
    });
    let out_dir = args.next().unwrap_or_else(|| "tex_png".into());

    let hashes = std::env::var("RCRA_HASHES").unwrap_or_else(|_| {
        omnitool_lib::core::filesystem::hashes_path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    });

    let t0 = Instant::now();
    let idx = level::get_index(&game, Some(&hashes)).expect("index");
    idx.require_names().expect("hashes file");
    println!("index ready in {:?}\n", t0.elapsed());

    match mode.as_str() {
        "census" => census(&idx),
        "region" => {
            let models = region_models(&idx, &arg);
            println!("region {arg}: {} distinct models", models.len());
            run(&idx, &models, &out_dir, 8);
        }
        _ => {
            let models = zone_models(&idx, &arg);
            println!("zone {arg}: {} distinct models", models.len());
            run(&idx, &models, &out_dir, 24);
        }
    }
}

// ---------------------------------------------------------------------------

fn zone_models(idx: &level::LevelIndex, zone_path: &str) -> Vec<u64> {
    let path = crc64::normalize_path(zone_path);
    let id = crc64::hash_raw(path.as_bytes());
    let raw = idx.extract(id).expect("extract zone");
    let zone = omnitool_lib::core::zone::Zone::parse(&raw).expect("parse zone");
    let mut seen = HashSet::new();
    zone.model_instances()
        .filter(|o| seen.insert(o.model_id))
        .map(|o| o.model_id)
        .collect()
}

fn region_models(idx: &level::LevelIndex, region: &str) -> Vec<u64> {
    let zones: Vec<u64> = idx
        .zones
        .iter()
        .filter(|z| z.region == region)
        .map(|z| z.id)
        .collect();
    let mut seen = HashSet::new();
    for zid in zones {
        let Ok(raw) = idx.extract(zid) else { continue };
        let Ok(zone) = omnitool_lib::core::zone::Zone::parse(&raw) else {
            continue;
        };
        for o in zone.model_instances() {
            seen.insert(o.model_id);
        }
    }
    seen.into_iter().collect()
}

// ---------------------------------------------------------------------------

fn run(idx: &level::LevelIndex, models: &[u64], out_dir: &str, samples: usize) {
    // ---- slot resolution ---------------------------------------------------
    let _ = model_materials::take_counters();
    let t = Instant::now();
    let records: Vec<Arc<ModelMaterials>> = models
        .par_iter()
        .map(|&m| model_materials(idx, m))
        .collect();
    let resolve_time = t.elapsed();
    let c = model_materials::take_counters();

    println!("--- slot resolution ---");
    println!(
        "{} models -> {} slots in {:?} ({:.2} ms/model)",
        models.len(),
        c.slots,
        resolve_time,
        resolve_time.as_secs_f64() * 1000.0 / models.len().max(1) as f64
    );
    println!(
        "  slots with a real .material in the TOC : {} / {} ({:.1}%)",
        c.slots_resolved,
        c.slots,
        pct(c.slots_resolved, c.slots)
    );
    println!(
        "  slots the viewport must fall back on   : {} / {} ({:.1}%)",
        c.slots_placeholder,
        c.slots,
        pct(c.slots_placeholder, c.slots)
    );
    println!(
        "  texture bindings                       : {} ({} resolved, {} missing)",
        c.bindings, c.textures_resolved, c.textures_missing
    );
    println!(
        "  slot names: {} declared ({:.1}%), {} guessed from the bound file ({:.1}%), {} unnamed ({:.1}%)",
        c.bindings_declared,
        pct(c.bindings_declared, c.bindings),
        c.bindings_guessed,
        pct(c.bindings_guessed, c.bindings),
        c.bindings_unnamed,
        pct(c.bindings_unnamed, c.bindings),
    );
    println!(
        "  sampler hashes learned from templates  : {}",
        model_materials::semantic_table_len()
    );

    // ---- how many submesh material indices land on a real albedo -----------
    let mut slot_hist: BTreeMap<String, usize> = BTreeMap::new();
    let mut with_albedo = 0usize;
    let mut with_any_texture = 0usize;
    let mut placeholder_materials: BTreeMap<String, usize> = BTreeMap::new();
    for r in &records {
        for s in &r.slots {
            if s.placeholder {
                let key = if s.material_path.is_empty() {
                    "<no path>".to_string()
                } else if !s.resolved {
                    format!("<missing> {}", s.material_path)
                } else {
                    s.material_path.clone()
                };
                *placeholder_materials.entry(key).or_default() += 1;
            }
            if !s.textures.is_empty() {
                with_any_texture += 1;
            }
            if s.textures
                .iter()
                .any(|t| t.slot == "albedo" && t.resolved && !t.placeholder)
            {
                with_albedo += 1;
            }
            for t in &s.textures {
                *slot_hist.entry(t.slot.clone()).or_default() += 1;
            }
        }
    }
    println!(
        "  slots binding any texture              : {} / {} ({:.1}%)",
        with_any_texture,
        c.slots,
        pct(with_any_texture, c.slots)
    );
    println!(
        "  slots binding a REAL albedo            : {} / {} ({:.1}%)",
        with_albedo,
        c.slots,
        pct(with_albedo, c.slots)
    );
    print!("  semantics:");
    for (k, n) in &slot_hist {
        print!(" {k}={n}");
    }
    println!();
    println!("  top placeholder / unresolved materials:");
    let mut ph: Vec<_> = placeholder_materials.into_iter().collect();
    ph.sort_by(|a, b| b.1.cmp(&a.1));
    for (p, n) in ph.iter().take(8) {
        println!("    {n:5}  {p}");
    }

    // ---- UV sanity ---------------------------------------------------------
    // Textures are only as good as the coordinates they are sampled at, and
    // that `decode_model_mesh` really reads it rather than falling back to
    // `DEFAULT_UV_SCALE`, and that the decoded UVs land in a sane range.
    let mut nibbles: BTreeMap<u32, usize> = BTreeMap::new();
    let mut missing_built = 0usize;
    let mut uv_min = f32::INFINITY;
    let mut uv_max = f32::NEG_INFINITY;
    let mut wild = 0usize;
    for &m in models {
        let Ok(raw) = idx.extract(m) else { continue };
        let Ok(model) = level::parse_model_asset(&raw) else {
            continue;
        };
        match level::ModelBuilt::of(&model) {
            Some(b) => *nibbles.entry(b.uv_log_scales & 0xF).or_default() += 1,
            None => missing_built += 1,
        }
        let bytes = level::model_mesh_bytes(idx, m, 0);
        for uv in read_rcms_uvs(&bytes) {
            uv_min = uv_min.min(uv);
            uv_max = uv_max.max(uv);
            if !(-64.0..=64.0).contains(&uv) {
                wild += 1;
            }
        }
    }
    println!("\n--- UV scaling ---");
    print!("  Built UV0 log-scale nibble:");
    for (n, c) in &nibbles {
        print!(" {n}->x{:.4}={c}", (1u32 << n) as f32 / 16384.0);
    }
    println!();
    println!(
        "  models with no Built header (would use DEFAULT_UV_SCALE): {missing_built}"
    );
    println!("  decoded UV range [{uv_min:.3}, {uv_max:.3}], {wild} outside +/-64");

    // ---- distinct textures -------------------------------------------------
    let mut distinct: HashMap<u64, (String, u32, u32, String)> = HashMap::new();
    for r in &records {
        for s in &r.slots {
            for t in &s.textures {
                if !t.resolved {
                    continue;
                }
                let id = model_materials::parse_hex_id(&t.texture_id).unwrap_or(0);
                distinct.insert(
                    id,
                    (t.texture_path.clone(), t.width, t.height, t.format.clone()),
                );
            }
        }
    }
    println!("\n--- textures ---");
    println!("{} distinct textures", distinct.len());

    let native_bytes: u64 = distinct
        .values()
        .map(|(_, w, h, _)| (*w as u64) * (*h as u64) * 4)
        .sum();
    println!(
        "  at native size, RGBA8, they would be {:.1} MB",
        native_bytes as f64 / 1e6
    );

    let ids: Vec<u64> = distinct.keys().copied().collect();

    // How much of the cap the RESIDENT mips can actually satisfy — i.e. how
    // often chasing the streamed HD tiles would buy the viewport anything.
    for cap in [256u32, 512, 1024] {
        let mut met = 0usize;
        let mut short = 0usize;
        for &id in &ids {
            let Some(i) = texture_serve::texture_info(idx, id) else {
                continue;
            };
            if i.sd_w.max(i.sd_h) >= cap {
                met += 1;
            } else {
                short += 1;
            }
        }
        println!(
            "  cap {cap:4}: resident chain reaches it on {met}/{} ({:.1}%), falls short on {short}",
            met + short,
            pct(met, met + short)
        );
    }

    for cap in [256u32, 512, 1024] {
        let t = Instant::now();
        let results: Vec<(u64, usize, u32, u32)> = ids
            .par_iter()
            .map(|&id| match texture_serve::decode_texture(idx, id, cap) {
                Ok((png, st)) => (id, png.len(), st.width, st.height),
                Err(_) => (id, 0, 0, 0),
            })
            .collect();
        let dt = t.elapsed();
        let ok = results.iter().filter(|r| r.1 > 0).count();
        let png_bytes: usize = results.iter().map(|r| r.1).sum();
        let gpu_bytes: u64 = results
            .iter()
            .map(|r| (r.2 as u64) * (r.3 as u64) * 4)
            .sum();
        println!(
            "  cap {cap:4}: {ok}/{} decoded in {:?} ({:.2} ms each, {:.0} tex/s) \
             -> {:.1} MB PNG, {:.1} MB RGBA8 on the GPU",
            ids.len(),
            dt,
            dt.as_secs_f64() * 1000.0 / ids.len().max(1) as f64,
            ids.len() as f64 / dt.as_secs_f64(),
            png_bytes as f64 / 1e6,
            gpu_bytes as f64 / 1e6,
        );
        let failed: Vec<u64> = results.iter().filter(|r| r.1 == 0).map(|r| r.0).collect();
        if !failed.is_empty() {
            println!("    {} failed, e.g.:", failed.len());
            for id in failed.iter().take(5) {
                let (p, w, h, f) = &distinct[id];
                println!("      {p} {w}x{h} {f}");
            }
        }
    }

    // ---- the served path, cold then warm -----------------------------------
    let t = Instant::now();
    let cold: usize = ids
        .par_iter()
        .map(|&id| texture_serve::texture_png_bytes(idx, id, 512).len())
        .sum();
    let cold_t = t.elapsed();
    let t = Instant::now();
    let warm: usize = ids
        .par_iter()
        .map(|&id| texture_serve::texture_png_bytes(idx, id, 512).len())
        .sum();
    let warm_t = t.elapsed();
    println!(
        "  level_texture @512: cold {:?} ({:.1} MB), warm {:?} ({}x faster)",
        cold_t,
        cold as f64 / 1e6,
        warm_t,
        (cold_t.as_secs_f64() / warm_t.as_secs_f64().max(1e-9)) as u64
    );
    assert_eq!(cold, warm, "the LRU must serve identical bytes");

    // ---- PNG samples -------------------------------------------------------
    std::fs::create_dir_all(out_dir).expect("out dir");
    let mut sample: Vec<(&u64, &(String, u32, u32, String))> = distinct
        .iter()
        .filter(|(_, (p, ..))| !model_materials::is_placeholder_texture(p))
        .collect();
    sample.sort_by(|a, b| {
        (b.1 .1 as u64 * b.1 .2 as u64)
            .cmp(&(a.1 .1 as u64 * a.1 .2 as u64))
            .then(a.1 .0.cmp(&b.1 .0))
    });
    println!("\n--- samples written to {out_dir} ---");
    let mut written = 0usize;
    for (id, (path, w, h, fmt)) in sample.iter() {
        if written >= samples {
            break;
        }
        let Ok((png, st)) = texture_serve::decode_texture(idx, **id, 512) else {
            continue;
        };
        let stem = path
            .rsplit('/')
            .next()
            .unwrap_or("tex")
            .trim_end_matches(".texture");
        let file = format!("{out_dir}/{written:02}_{stem}.png");
        std::fs::write(&file, &png).expect("write png");
        let ct = texture_serve::texture_info(idx, **id)
            .map(|i| i.content_type)
            .unwrap_or(0);
        println!(
            "  {written:02}_{stem}.png  ({fmt} native {w}x{h} -> mip {} {}x{}, {} B, content {ct:#04X}{})",
            st.mip,
            st.width,
            st.height,
            png.len(),
            if ct & 0x02 != 0 { " NORMAL" } else { "" },
        );
        written += 1;
    }

    let (n, hits) = model_materials::model_cache_stats();
    let (pn, pb) = texture_serve::png_cache_stats();
    println!(
        "\ncaches: {n} models ({hits} hits), {pn} PNGs / {:.1} MB, {} texture headers",
        pb as f64 / 1e6,
        texture_serve::info_cache_len()
    );
}

// ---------------------------------------------------------------------------

/// Header-only pass over every `.texture` the name list knows about.
fn census(idx: &level::LevelIndex) {
    let ids: Vec<u64> = idx
        .names
        .iter()
        .filter(|(_, p)| p.ends_with(".texture"))
        .map(|(id, _)| *id)
        .collect();
    println!("{} .texture assets named in the TOC", ids.len());

    let t = Instant::now();
    let infos: Vec<Option<texture_serve::TexInfo>> = ids
        .par_iter()
        .map(|&id| texture_serve::texture_info(idx, id))
        .collect();
    let dt = t.elapsed();
    let ok = infos.iter().filter(|i| i.is_some()).count();
    println!(
        "{ok} headers parsed in {:?} ({:.3} ms each)",
        dt,
        dt.as_secs_f64() * 1000.0 / ids.len().max(1) as f64
    );

    let mut fmts: BTreeMap<String, usize> = BTreeMap::new();
    let mut streamed = 0usize;
    let mut sd_bytes = 0u64;
    let mut hd_bytes = 0u64;
    for i in infos.iter().flatten() {
        *fmts.entry(texture_serve::format_name(i.format)).or_default() += 1;
        if i.hd_mips > 0 {
            streamed += 1;
        }
        sd_bytes += i.sd_len as u64;
        hd_bytes += i.hd_len as u64;
    }
    println!(
        "{streamed} have streamed mips; resident {:.0} MB, streamed {:.0} MB",
        sd_bytes as f64 / 1e6,
        hd_bytes as f64 / 1e6
    );
    let mut f: Vec<_> = fmts.into_iter().collect();
    f.sort_by(|a, b| b.1.cmp(&a.1));
    for (name, n) in f.iter().take(14) {
        println!("  {n:6}  {name}");
    }
}

/// The `uvs` span of an RCMS buffer: header 24 B, bounds 24 B, then
/// `3V` positions, `3V` normals, `2V` uvs.
fn read_rcms_uvs(b: &[u8]) -> Vec<f32> {
    if b.len() < 48 {
        return Vec::new();
    }
    let vc = u32::from_le_bytes(b[8..12].try_into().unwrap()) as usize;
    let start = 48 + vc * 3 * 4 * 2;
    let end = start + vc * 2 * 4;
    if end > b.len() {
        return Vec::new();
    }
    b[start..end]
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn pct(a: usize, b: usize) -> f64 {
    if b == 0 {
        0.0
    } else {
        a as f64 * 100.0 / b as f64
    }
}
