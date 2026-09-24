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
//!   slices               header-only census of ARRAY SLICES: how many textures
//!                        carry more than one, and whether
//!                        `slices * one mip chain == sd_len` on every one of
//!                        them (the slice-major layout test)
//!   blend <zone path>    the blend layers of one zone: how many colour slots
//!                        are four-slice arrays, the cost of serving slices 1-3
//!                        on top of slice 0, and a height-field test on
//!                        slice 2's green channel
//!   uv1 <region key>     how many models of a region carry UV1, and what the
//!                        emitted RCMS buffer costs with it
//!
//! The asset name list comes from $RCRA_HASHES, else OmniTool's app-dir file.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use image::ImageEncoder;
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
        "region" | "uv1" => "i29/openarea/nefarious_city".into(),
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
        "slices" => slice_census(&idx),
        "blend" => blend_slices(&idx, &arg, &out_dir),
        "uv1" => uv1_census(&idx, &arg),
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

// ---------------------------------------------------------------------------
// Array slices
// ---------------------------------------------------------------------------

/// Header-only census of array slices, and the slice-major layout test.
///
/// The test: `slices * one resident mip chain` must equal the header's own
/// `sd_len`. A chain is `sum over m < sd_mips of mip_level_size(sd_w >> m,
/// sd_h >> m)`, computed from nothing but the header's width/height/format/mip
/// count, so an agreement is a statement about the shipped bytes and not a
/// tautology.
fn slice_census(idx: &level::LevelIndex) {
    let ids: Vec<u64> = idx
        .names
        .iter()
        .filter(|(_, p)| p.ends_with(".texture"))
        .map(|(id, _)| *id)
        .collect();
    println!("{} .texture assets named in the TOC", ids.len());

    let t = Instant::now();
    let rows: Vec<(u64, texture_serve::TexInfo)> = ids
        .par_iter()
        .filter_map(|&id| texture_serve::texture_info(idx, id).map(|i| (id, i)))
        .collect();
    println!(
        "{} headers parsed in {:?}\n",
        rows.len(),
        t.elapsed()
    );

    let mut by_slices: BTreeMap<u32, usize> = BTreeMap::new();
    let mut exact = 0usize;
    let mut unknown_format = 0usize;
    let mut mismatched: Vec<(u64, u32, usize, u32)> = Vec::new();
    let mut multi_by_format: BTreeMap<String, usize> = BTreeMap::new();
    let mut multi_by_suffix: BTreeMap<String, usize> = BTreeMap::new();
    let mut four_slice = 0usize;
    let mut extra_bytes = 0u64;

    for (id, i) in &rows {
        let slices = i.slices();
        *by_slices.entry(slices).or_default() += 1;
        match i.resident_chain_bytes() {
            None => unknown_format += 1,
            Some(chain) => {
                if chain * slices as usize == i.sd_len as usize {
                    exact += 1;
                } else {
                    mismatched.push((*id, slices, chain, i.sd_len));
                }
            }
        }
        if slices > 1 {
            *multi_by_format
                .entry(texture_serve::format_name(i.format))
                .or_default() += 1;
            let name = idx.names.get(id).cloned().unwrap_or_default();
            let stem = name
                .rsplit('/')
                .next()
                .unwrap_or("")
                .trim_end_matches(".texture");
            let suffix = stem.rsplit('_').next().unwrap_or("").to_string();
            *multi_by_suffix.entry(suffix).or_default() += 1;
            if slices == 4 {
                four_slice += 1;
            }
            extra_bytes += i.sd_len as u64 - i.sd_len as u64 / slices as u64;
        }
    }

    println!("--- slices per texture ---");
    for (s, n) in &by_slices {
        println!("  {s:3} slice(s): {n:6}");
    }
    println!("  4-slice textures: {four_slice}");

    println!("\n--- slice-major layout test (slices * one mip chain == sd_len) ---");
    println!(
        "  exact: {exact} / {} ({:.2}%)",
        rows.len(),
        pct(exact, rows.len())
    );
    println!("  formats with no known block size: {unknown_format}");
    println!("  mismatched: {}", mismatched.len());
    for (id, slices, chain, sd_len) in mismatched.iter().take(10) {
        let name = idx.names.get(id).cloned().unwrap_or_default();
        println!(
            "    {slices} x {chain} = {} vs sd_len {sd_len}   {name}",
            chain * *slices as usize
        );
    }

    println!("\n--- multi-slice textures by format ---");
    let mut f: Vec<_> = multi_by_format.into_iter().collect();
    f.sort_by(|a, b| b.1.cmp(&a.1));
    for (name, n) in f.iter().take(10) {
        println!("  {n:6}  {name}");
    }
    println!("--- multi-slice textures by name suffix ---");
    let mut s: Vec<_> = multi_by_suffix.into_iter().collect();
    s.sort_by(|a, b| b.1.cmp(&a.1));
    for (name, n) in s.iter().take(14) {
        println!("  {n:6}  _{name}");
    }
    println!(
        "\nresident bytes past slice 0 across every multi-slice texture: {:.1} MB",
        extra_bytes as f64 / 1e6
    );
}

/// Green-channel statistics of one decoded slice.
///
/// `mad / sigma` is the discriminator. For independent noise the mean absolute
/// difference of adjacent samples is `sigma * 2 / sqrt(pi)` = 1.128 sigma; for a
/// smooth field it is far below sigma because neighbours are correlated. A
/// shuffled copy of the very same texels is measured alongside as the in-sample
/// noise baseline, so the comparison needs no assumption about the distribution.
struct ChannelStats {
    min: u8,
    max: u8,
    mean: f64,
    sigma: f64,
    mad: f64,
    mad_shuffled: f64,
}

fn channel_stats(rgba: &[u8], w: usize, h: usize, ch: usize) -> ChannelStats {
    let v: Vec<u8> = (0..w * h).map(|i| rgba[i * 4 + ch]).collect();
    let n = v.len().max(1) as f64;
    let mean = v.iter().map(|&x| x as f64).sum::<f64>() / n;
    let sigma = (v.iter().map(|&x| (x as f64 - mean).powi(2)).sum::<f64>() / n).sqrt();

    let mut pairs = 0f64;
    let mut sum = 0f64;
    for y in 0..h {
        for x in 1..w {
            sum += (v[y * w + x] as f64 - v[y * w + x - 1] as f64).abs();
            pairs += 1.0;
        }
    }
    let mad = if pairs > 0.0 { sum / pairs } else { 0.0 };

    // Deterministic shuffle (a 64-bit LCG), so the baseline is reproducible.
    let mut s = v.clone();
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    for i in (1..s.len()).rev() {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let j = (state >> 33) as usize % (i + 1);
        s.swap(i, j);
    }
    let mut sum2 = 0f64;
    let mut pairs2 = 0f64;
    for y in 0..h {
        for x in 1..w {
            sum2 += (s[y * w + x] as f64 - s[y * w + x - 1] as f64).abs();
            pairs2 += 1.0;
        }
    }
    ChannelStats {
        min: *v.iter().min().unwrap_or(&0),
        max: *v.iter().max().unwrap_or(&0),
        mean,
        sigma,
        mad,
        mad_shuffled: if pairs2 > 0.0 { sum2 / pairs2 } else { 0.0 },
    }
}

fn decode_png_rgba(png: &[u8]) -> Option<(Vec<u8>, usize, usize)> {
    let img = image::load_from_memory(png).ok()?.to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    Some((img.into_raw(), w, h))
}

/// Every `gbl_rgb_blend` colour layer of one zone: slice counts, the cost of
/// serving slices 1-3 on top of slice 0, and the height-field test.
fn blend_slices(idx: &level::LevelIndex, zone_path: &str, out_dir: &str) {
    let models = zone_models(idx, zone_path);
    println!("zone {zone_path}: {} distinct models", models.len());

    let records: Vec<Arc<ModelMaterials>> = models
        .par_iter()
        .map(|&m| model_materials(idx, m))
        .collect();

    // Distinct layer / mask / shared-normal textures of every blend slot.
    let mut layer_tex: BTreeMap<u64, String> = BTreeMap::new();
    let mut mask_tex: BTreeMap<u64, String> = BTreeMap::new();
    let mut shared_tex: BTreeMap<u64, String> = BTreeMap::new();
    let mut blend_slots = 0usize;
    let mut plain_slots = 0usize;
    let mut plain_tex: BTreeMap<u64, String> = BTreeMap::new();
    for r in &records {
        for s in &r.slots {
            match &s.blend {
                Some(b) => {
                    blend_slots += 1;
                    for l in &b.layers {
                        if let Ok(id) = model_materials::parse_hex_id(&l.texture_id) {
                            layer_tex.insert(id, l.texture_path.clone());
                        }
                    }
                    if let Ok(id) = model_materials::parse_hex_id(&b.mask_texture_id) {
                        mask_tex.insert(id, b.mask_texture_path.clone());
                    }
                    for (idh, p) in [
                        (&b.normal_texture_id, &b.normal_texture_path),
                        (&b.misc_texture_id, &b.misc_texture_path),
                    ] {
                        if let Ok(id) = model_materials::parse_hex_id(idh) {
                            shared_tex.insert(id, p.clone());
                        }
                    }
                }
                None => {
                    plain_slots += 1;
                    for t in &s.textures {
                        if let Ok(id) = model_materials::parse_hex_id(&t.texture_id) {
                            plain_tex.insert(id, t.texture_path.clone());
                        }
                    }
                }
            }
        }
    }
    println!(
        "  {blend_slots} blend slots, {plain_slots} plain slots\n  \
         {} distinct colour-layer textures, {} masks, {} shared normal/misc, {} plain-slot textures",
        layer_tex.len(),
        mask_tex.len(),
        shared_tex.len(),
        plain_tex.len()
    );

    // ---- how many of each kind are arrays -----------------------------------
    println!("\n--- slices per texture, by role ---");
    for (label, set) in [
        ("blend colour layer", &layer_tex),
        ("blend splat mask", &mask_tex),
        ("blend shared normal/misc", &shared_tex),
        ("plain-material texture", &plain_tex),
    ] {
        let mut hist: BTreeMap<u32, usize> = BTreeMap::new();
        for id in set.keys() {
            hist.entry(texture_serve::texture_slices(idx, *id))
                .and_modify(|n| *n += 1)
                .or_insert(1);
        }
        let total: usize = hist.values().sum();
        print!("  {label:26} {total:4} textures:");
        for (s, n) in &hist {
            print!(" {s}sl={n}");
        }
        println!();
    }

    // ---- cost of serving each slice -----------------------------------------
    let ids: Vec<u64> = layer_tex.keys().copied().collect();
    let four: Vec<u64> = ids
        .iter()
        .copied()
        .filter(|&id| texture_serve::texture_slices(idx, id) >= 4)
        .collect();
    println!(
        "\n--- cost per slice over the {} four-slice colour layers (cap 512) ---",
        four.len()
    );
    let mut cumulative_png = 0usize;
    for slice in 0..4u32 {
        let t = Instant::now();
        let res: Vec<(usize, usize, u32, u32)> = four
            .par_iter()
            .map(
                |&id| match texture_serve::decode_texture_slice(idx, id, slice, 512) {
                    Ok((png, st)) => (png.len(), st.read_bytes, st.width, st.height),
                    Err(_) => (0, 0, 0, 0),
                },
            )
            .collect();
        let dt = t.elapsed();
        let ok = res.iter().filter(|r| r.0 > 0).count();
        let png: usize = res.iter().map(|r| r.0).sum();
        let read: usize = res.iter().map(|r| r.1).sum();
        let gpu: u64 = res.iter().map(|r| r.2 as u64 * r.3 as u64 * 4).sum();
        cumulative_png += png;
        println!(
            "  slice {slice}: {ok}/{} decoded in {:?} ({:.2} ms each) -> \
             {:.2} MB PNG, {:.2} MB archive read, {:.1} MB RGBA8 on the GPU",
            four.len(),
            dt,
            dt.as_secs_f64() * 1000.0 / four.len().max(1) as f64,
            png as f64 / 1e6,
            read as f64 / 1e6,
            gpu as f64 / 1e6,
        );
    }
    println!(
        "  all four slices of every four-slice layer: {:.2} MB PNG",
        cumulative_png as f64 / 1e6
    );

    // ---- one read for all four, vs four reads ------------------------------
    // The single-slice call narrows its read to "up to the end of the slice I
    // want", so slice 3 re-reads 0-2 on the way past. Serve them together and
    // that happens once.
    let read_separately: usize = (0..4u32)
        .map(|s| {
            four.iter()
                .map(|&id| {
                    texture_serve::decode_texture_slice(idx, id, s, 512)
                        .map(|(_, st)| st.read_bytes)
                        .unwrap_or(0)
                })
                .sum::<usize>()
        })
        .sum();
    let read_together: usize = four
        .iter()
        .map(|&id| {
            texture_serve::decode_texture_slice(idx, id, 3, 512)
                .map(|(_, st)| st.read_bytes)
                .unwrap_or(0)
        })
        .sum();
    texture_serve::png_cache_clear();
    let t = Instant::now();
    let multi: usize = four
        .par_iter()
        .map(|&id| {
            texture_serve::texture_png_bytes_multi(idx, id, &[0, 1, 2, 3], 512)
                .iter()
                .map(|p| p.len())
                .sum::<usize>()
        })
        .sum();
    let multi_t = t.elapsed();
    println!(
        "  archive bytes read: {:.2} MB as four separate calls, {:.2} MB as one \
         ({:.1}x less)",
        read_separately as f64 / 1e6,
        read_together as f64 / 1e6,
        read_separately as f64 / read_together.max(1) as f64
    );
    println!(
        "  texture_png_bytes_multi([0,1,2,3]) cold, one read each: {:?}, {:.2} MB PNG",
        multi_t,
        multi as f64 / 1e6
    );
    assert_eq!(
        multi, cumulative_png,
        "the batch path must return the same bytes as four single calls"
    );

    // What a diffuse viewport would actually ask for: albedo, the height it
    // evaluates the blend weights at, and the per-layer tangent normal. The
    // gloss slice has no consumer yet.
    texture_serve::png_cache_clear();
    let t = Instant::now();
    let want: Vec<usize> = four
        .par_iter()
        .map(|&id| {
            texture_serve::texture_png_bytes_multi(idx, id, &[0, 2, 3], 512)
                .iter()
                .map(|p| p.len())
                .sum::<usize>()
        })
        .collect();
    let want_t = t.elapsed();
    println!(
        "  what a viewport needs, slices [0,2,3] of all {} layers: {:?}, {:.2} MB PNG \
         ({:.1} MB RGBA8 on the GPU)",
        four.len(),
        want_t,
        want.iter().sum::<usize>() as f64 / 1e6,
        four.len() as f64 * 3.0 * 512.0 * 512.0 * 4.0 / 1e6,
    );

    // ---- the height-field test ---------------------------------------------
    // Sort by path so the sample is the same on every run.
    let mut sample: Vec<u64> = four.clone();
    sample.sort_by_key(|id| layer_tex.get(id).cloned().unwrap_or_default());
    std::fs::create_dir_all(out_dir).ok();

    println!("\n--- slice 2 green channel: height field or noise? ---");
    println!(
        "  {:<40} {:>10} {:>7} {:>7} {:>7} {:>7}",
        "texture (slice 2, green)", "range", "mean", "sigma", "mad", "mad/sig"
    );
    let mut smooth = 0usize;
    let mut tested = 0usize;
    let mut ratio_sum = 0f64;
    let mut shuffled_sum = 0f64;
    for (n, id) in sample.iter().enumerate() {
        let Ok((png, st)) = texture_serve::decode_texture_slice(idx, *id, 2, 512) else {
            continue;
        };
        let Some((rgba, w, h)) = decode_png_rgba(&png) else {
            continue;
        };
        let g = channel_stats(&rgba, w, h, 1);
        let ratio = if g.sigma > 0.0 { g.mad / g.sigma } else { 0.0 };
        let shuf = if g.sigma > 0.0 {
            g.mad_shuffled / g.sigma
        } else {
            0.0
        };
        tested += 1;
        ratio_sum += ratio;
        shuffled_sum += shuf;
        // Smooth: adjacent texels are far closer than two unrelated ones, and
        // the channel actually uses its range.
        if ratio < 0.5 * shuf && (g.max as i32 - g.min as i32) > 64 {
            smooth += 1;
        }
        if n < 16 {
            let path = layer_tex.get(id).cloned().unwrap_or_default();
            let stem = path
                .rsplit('/')
                .next()
                .unwrap_or("tex")
                .trim_end_matches(".texture");
            println!(
                "  {:<40} {:>4}..{:<4} {:>7.1} {:>7.2} {:>7.2} {:>7.3}",
                &stem[..stem.len().min(40)],
                g.min,
                g.max,
                g.mean,
                g.sigma,
                g.mad,
                ratio
            );
            // Write every slice of the first few, so the eye can check too.
            if n < 4 {
                for s in 0..st.slices.min(4) {
                    if let Ok((p, _)) = texture_serve::decode_texture_slice(idx, *id, s, 512) {
                        let _ = std::fs::write(format!("{out_dir}/{n:02}_{stem}_s{s}.png"), &p);
                    }
                }
                // Slice 2's green channel on its own — the height field the
                // blend weights are meant to be evaluated at.
                let grey: Vec<u8> = (0..w * h).map(|i| rgba[i * 4 + 1]).collect();
                let mut out = Vec::new();
                if image::codecs::png::PngEncoder::new(&mut out)
                    .write_image(
                        &grey,
                        w as u32,
                        h as u32,
                        image::ExtendedColorType::L8,
                    )
                    .is_ok()
                {
                    let _ = std::fs::write(format!("{out_dir}/{n:02}_{stem}_s2_height.png"), &out);
                }
            }
        }
    }
    println!(
        "\n  {smooth}/{tested} slice-2 green channels are smooth AND full-range\n  \
         mean mad/sigma: {:.3} as stored, {:.3} for the same texels shuffled \
         (independent noise would be 1.128)",
        ratio_sum / tested.max(1) as f64,
        shuffled_sum / tested.max(1) as f64
    );

    // Same statistic on slice 0's green, as a control: an albedo is smooth too,
    // so the test alone does not prove slice 2 IS the height — what it rules
    // out is that slice 2 is a fragment of another mip or of tiled/noise data.
    let mut s0_ratio = 0f64;
    let mut s0_n = 0usize;
    for id in sample.iter().take(64) {
        let Ok((png, _)) = texture_serve::decode_texture_slice(idx, *id, 0, 512) else {
            continue;
        };
        let Some((rgba, w, h)) = decode_png_rgba(&png) else {
            continue;
        };
        let g = channel_stats(&rgba, w, h, 1);
        if g.sigma > 0.0 {
            s0_ratio += g.mad / g.sigma;
            s0_n += 1;
        }
    }
    println!(
        "  control, slice 0 green over {s0_n} of the same textures: mean mad/sigma {:.3}",
        s0_ratio / s0_n.max(1) as f64
    );
    println!("  per-slice PNGs written to {out_dir}");
}

// ---------------------------------------------------------------------------
// UV1
// ---------------------------------------------------------------------------

/// How many models carry a second UV set, and what emitting it costs.
fn uv1_census(idx: &level::LevelIndex, region: &str) {
    use omnitool_lib::tools::model_converter::sections::geo::TAG_UV1;

    let models = if region.contains(".zone") {
        zone_models(idx, region)
    } else {
        region_models(idx, region)
    };
    println!("{region}: {} distinct models", models.len());

    #[derive(Default)]
    struct Tally {
        flag_and_section: usize,
        flag_only: usize,
        section_only: usize,
        neither: usize,
        no_built: usize,
        uv1_shorter_than_verts: usize,
        bytes_without: u64,
        bytes_with: u64,
        verts: u64,
        uv1_verts: u64,
        rcms_with_uv1: usize,
        rcms_flag_disagrees: usize,
        rcms_size_wrong: usize,
        uv1_nonfinite: usize,
        uv1_texels: u64,
        uv1_wild: u64,
        uv1_min: f32,
        uv1_max: f32,
        worst_abs: f32,
        worst_model: String,
        worst_nibbles: (u32, u32),
    }
    let mut t = Tally {
        uv1_min: f32::INFINITY,
        uv1_max: f32::NEG_INFINITY,
        ..Default::default()
    };
    let mut nibbles: BTreeMap<(u32, u32), usize> = BTreeMap::new();

    for &m in &models {
        let Ok(raw) = idx.extract(m) else { continue };
        let Ok(model) = level::parse_model_asset(&raw) else {
            continue;
        };
        let sec = model.dat1.get_section_data(TAG_UV1);
        let built = level::ModelBuilt::of(&model);
        let flag = match &built {
            Some(b) => {
                *nibbles
                    .entry((b.uv_log_scales & 0xF, (b.uv_log_scales >> 4) & 0xF))
                    .or_default() += 1;
                b.has_flag(level::model_flag::VERT_HAS_UV1)
            }
            None => {
                t.no_built += 1;
                false
            }
        };
        match (flag, sec.is_some()) {
            (true, true) => t.flag_and_section += 1,
            (true, false) => t.flag_only += 1,
            (false, true) => t.section_only += 1,
            (false, false) => t.neither += 1,
        }
        // Does the section cover every vertex the Built header declares?
        if let (Some(s), Some(b)) = (sec, built.as_ref()) {
            t.uv1_verts += (s.len() / 4) as u64;
            t.verts += b.vertex_count as u64;
            if s.len() / 4 < b.vertex_count as usize {
                t.uv1_shorter_than_verts += 1;
            }
        }
        let buf = level::model_mesh_bytes(idx, m, 0);
        let bytes = buf.len() as u64;
        t.bytes_with += bytes;
        // What the same buffer would be with no UV1 block: 8 bytes per vertex.
        let mesh_verts = if bytes >= 48 {
            u32::from_le_bytes(buf[8..12].try_into().unwrap()) as u64
        } else {
            0
        };
        t.bytes_without += bytes - if sec.is_some() { mesh_verts * 8 } else { 0 };

        // ---- what the RCMS buffer actually says ----------------------------
        if bytes >= 48 {
            let flags = u32::from_le_bytes(buf[20..24].try_into().unwrap());
            let has = flags & level::mesh_flag::HAS_UV1 != 0;
            if has != (sec.is_some() && mesh_verts > 0) {
                t.rcms_flag_disagrees += 1;
            }
            if has {
                t.rcms_with_uv1 += 1;
                let mut worst = 0f32;
                for uv in read_rcms_uv1(&buf) {
                    t.uv1_min = t.uv1_min.min(uv);
                    t.uv1_max = t.uv1_max.max(uv);
                    worst = worst.max(uv.abs());
                    t.uv1_texels += 1;
                    if !(-64.0..=64.0).contains(&uv) {
                        t.uv1_wild += 1;
                    }
                    if !uv.is_finite() {
                        t.uv1_nonfinite += 1;
                    }
                }
                if worst > t.worst_abs {
                    t.worst_abs = worst;
                    t.worst_model = idx.names.get(&m).cloned().unwrap_or_default();
                    t.worst_nibbles = built
                        .as_ref()
                        .map(|b| (b.uv_log_scales & 0xF, (b.uv_log_scales >> 4) & 0xF))
                        .unwrap_or((0, 0));
                }
                // The block must sit exactly at the end, right after the
                // submesh table, and be 2 floats per vertex.
                let ic = u32::from_le_bytes(buf[12..16].try_into().unwrap()) as u64;
                let sc = u32::from_le_bytes(buf[16..20].try_into().unwrap()) as u64;
                let expect = 48 + mesh_verts * 32 + ic * 4 + sc * 16 + mesh_verts * 8;
                if expect != bytes {
                    t.rcms_size_wrong += 1;
                }
            }
        }
    }

    println!("\n--- Model flag bit 17 (VertHasUv1) vs. the Model UV1 Vert section ---");
    println!("  flag set AND section present : {}", t.flag_and_section);
    println!("  flag set, NO section         : {}", t.flag_only);
    println!("  section present, flag CLEAR  : {}", t.section_only);
    println!("  neither                      : {}", t.neither);
    println!("  no Model Built header        : {}", t.no_built);
    println!(
        "  UV1 rows vs Built vertex_count: {} rows for {} vertices, {} models short",
        t.uv1_verts, t.verts, t.uv1_shorter_than_verts
    );
    println!("\n--- Built +0x30 UV log scales (uv0 nibble, uv1 nibble) ---");
    let mut n: Vec<_> = nibbles.into_iter().collect();
    n.sort_by(|a, b| b.1.cmp(&a.1));
    for ((n0, n1), c) in n.iter().take(12) {
        println!(
            "  uv0 {n0} (x{:.5}), uv1 {n1} (x{:.5}): {c}",
            (1u32 << n0) as f32 / 16384.0,
            (1u32 << n1) as f32 / 16384.0
        );
    }
    println!("\n--- what the emitted RCMS buffers say ---");
    println!("  buffers carrying a UV1 block             : {}", t.rcms_with_uv1);
    println!("  header flag disagrees with the section   : {}", t.rcms_flag_disagrees);
    println!("  buffer length != header+geom+idx+sub+uv1 : {}", t.rcms_size_wrong);
    println!(
        "  decoded UV1 range [{:.3}, {:.3}], {} non-finite, {} of {} texels outside +/-64",
        t.uv1_min, t.uv1_max, t.uv1_nonfinite, t.uv1_wild, t.uv1_texels
    );
    println!(
        "  largest |UV1| {:.1} on {} (uv0 nibble {}, uv1 nibble {})",
        t.worst_abs, t.worst_model, t.worst_nibbles.0, t.worst_nibbles.1
    );
    println!(
        "\n--- RCMS cost ---\n  {:.2} MB with UV1, {:.2} MB without (+{:.2}%)",
        t.bytes_with as f64 / 1e6,
        t.bytes_without as f64 / 1e6,
        (t.bytes_with as f64 / t.bytes_without.max(1) as f64 - 1.0) * 100.0
    );
}

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

/// The `uv1` span of an RCMS buffer: it sits at the very end, after the
/// submesh table, and only when bit 0 of the header word at `+0x14` is set.
fn read_rcms_uv1(b: &[u8]) -> Vec<f32> {
    if b.len() < 48 {
        return Vec::new();
    }
    let vc = u32::from_le_bytes(b[8..12].try_into().unwrap()) as usize;
    let ic = u32::from_le_bytes(b[12..16].try_into().unwrap()) as usize;
    let sc = u32::from_le_bytes(b[16..20].try_into().unwrap()) as usize;
    let start = 48 + vc * 32 + ic * 4 + sc * 16;
    let end = start + vc * 2 * 4;
    if end > b.len() {
        return Vec::new();
    }
    b[start..end]
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
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
