//! Measures the zone lighting model against the shipped PC build.
//!
//!   cargo run --release --bin light_probe -- "<game dir>" [zone substring] [--limit N]
//!
//! Everything printed here is a count over real files. It checks, in order:
//!
//!  * that a kind-1 scene entry is exactly 0xF0 bytes plus its inline asset path;
//!  * the light type / flags / shadow / spec / lighting-condition histograms;
//!  * that the +0xBC..+0xD8 union reads sanely for both the scalar and the
//!    volume-light interpretation;
//!  * that the cone half-angles are only non-zero on spot and pyramid lights;
//!  * the lighting-condition asset blocks (atmosphere, level light, light bin,
//!    env probe assets) and their five condition hashes;
//!  * the `Zone EnvProbe List` row size;
//!  * which lighting sections actually appear, and how often.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use omnitool_lib::core::crc32;
use omnitool_lib::core::level;
use omnitool_lib::core::zone::{Zone, T_SCENE_OBJECTS};
use omnitool_lib::core::zone_lighting as zl;
use rayon::prelude::*;

#[derive(Default)]
struct Acc {
    zones: usize,
    zones_with_lights: usize,
    lights: usize,
    /// entry size histogram
    entry_sizes: BTreeMap<u32, usize>,
    type_counts: BTreeMap<u8, usize>,
    /// type -> (casts shadows count)
    shadow_by_type: BTreeMap<u8, usize>,
    shadow_cast_hist: BTreeMap<u8, usize>,
    shadow_res_hist: BTreeMap<u8, usize>,
    spec_type_hist: BTreeMap<u8, usize>,
    flag_bit_hist: BTreeMap<u8, usize>,
    flags_above_9: usize,
    cond_hist: BTreeMap<u8, usize>,
    /// invariants
    color_negative: usize,
    color_zero: usize,
    inner_gt_outer: usize,
    cone_on_coneless_type: usize,
    cone_zero_on_spot: usize,
    atten_zero: usize,
    atten_negative: usize,
    volume_link_set: usize,
    asset_paths: usize,
    asset_path_bad: usize,
    asset_path_samples: Vec<String>,
    platform_mask_hist: BTreeMap<u32, usize>,
    /// per-type stats for numeric fields: (min, max, sum, n) of a few fields
    stats: BTreeMap<(u8, &'static str), (f32, f32, f64, usize)>,
    /// values collected for medians
    med: BTreeMap<(u8, &'static str), Vec<f32>>,
    /// section presence
    sections: BTreeMap<u32, usize>,
    /// condition block observations
    cond_blocks: BTreeMap<(u32, u32), usize>,
    cond_hashes: BTreeMap<u32, [u32; 5]>,
    atmos_uniform: usize,
    atmos_varying: usize,
    atmos_examples: Vec<String>,
    lightbin_examples: Vec<String>,
    envprobe_zones: usize,
    envprobes: usize,
    envprobe_shape_hist: BTreeMap<u8, usize>,
    envprobe_flag_hist: BTreeMap<u8, usize>,
    envprobe_asset_index_bad: usize,
    envprobe_list_tail: usize,
    envprobe_weight_min: f32,
    envprobe_weight_max: f32,
    lgpos_zones: usize,
    near_fade_set: usize,
    volfog_set: usize,
    gobo_set: usize,
    saturation_samples: Vec<f32>,
    /// gobo word 0 / word 1 raw histograms, and the same words read as f32.
    gobo0_hist: BTreeMap<u32, usize>,
    gobo1_hist: BTreeMap<u32, usize>,
    /// tag -> how many zones hold a u64 equal to a known asset id of that family
    ref_hits: BTreeMap<(&'static str, u32), usize>,
    /// zone path suffix -> how many of the light-bearing zones use it
    light_zone_suffix: BTreeMap<String, usize>,
    top_light_zones: Vec<(usize, String)>,
    extent_samples: Vec<[f32; 3]>,
    /// Condition index -> how often the referenced asset's file name carries
    /// that condition's own word. The end-to-end proof that row i really is
    /// condition i.
    cond_name_match: [usize; 5],
    cond_name_miss: [usize; 5],
    cond_name_null: [usize; 5],
    cond_name_examples: Vec<String>,
    suffix_hist: BTreeMap<u32, usize>,
    /// env probe asset-index diagnostics
    probe_asset_index_hist: BTreeMap<i16, usize>,
    probe_asset_rows: BTreeMap<usize, usize>,
    probe_asset_examples: Vec<String>,
}

fn note(acc: &mut Acc, t: u8, name: &'static str, v: f32) {
    if !v.is_finite() {
        return;
    }
    let e = acc
        .stats
        .entry((t, name))
        .or_insert((f32::INFINITY, f32::NEG_INFINITY, 0.0, 0));
    e.0 = e.0.min(v);
    e.1 = e.1.max(v);
    e.2 += v as f64;
    e.3 += 1;
    let m = acc.med.entry((t, name)).or_default();
    if m.len() < 200_000 {
        m.push(v);
    }
}

fn median(v: &mut Vec<f32>) -> f32 {
    if v.is_empty() {
        return f32::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[v.len() / 2]
}

fn merge(a: &mut Acc, b: Acc) {
    a.zones += b.zones;
    a.zones_with_lights += b.zones_with_lights;
    a.lights += b.lights;
    for (k, v) in b.entry_sizes {
        *a.entry_sizes.entry(k).or_default() += v;
    }
    for (k, v) in b.type_counts {
        *a.type_counts.entry(k).or_default() += v;
    }
    for (k, v) in b.shadow_by_type {
        *a.shadow_by_type.entry(k).or_default() += v;
    }
    for (k, v) in b.shadow_cast_hist {
        *a.shadow_cast_hist.entry(k).or_default() += v;
    }
    for (k, v) in b.shadow_res_hist {
        *a.shadow_res_hist.entry(k).or_default() += v;
    }
    for (k, v) in b.spec_type_hist {
        *a.spec_type_hist.entry(k).or_default() += v;
    }
    for (k, v) in b.flag_bit_hist {
        *a.flag_bit_hist.entry(k).or_default() += v;
    }
    for (k, v) in b.cond_hist {
        *a.cond_hist.entry(k).or_default() += v;
    }
    for (k, v) in b.platform_mask_hist {
        *a.platform_mask_hist.entry(k).or_default() += v;
    }
    for (k, v) in b.sections {
        *a.sections.entry(k).or_default() += v;
    }
    for (k, v) in b.cond_blocks {
        *a.cond_blocks.entry(k).or_default() += v;
    }
    for (k, v) in b.cond_hashes {
        a.cond_hashes.entry(k).or_insert(v);
    }
    for (k, v) in b.envprobe_shape_hist {
        *a.envprobe_shape_hist.entry(k).or_default() += v;
    }
    for (k, v) in b.envprobe_flag_hist {
        *a.envprobe_flag_hist.entry(k).or_default() += v;
    }
    for (k, (mn, mx, sum, n)) in b.stats {
        let e = a
            .stats
            .entry(k)
            .or_insert((f32::INFINITY, f32::NEG_INFINITY, 0.0, 0));
        e.0 = e.0.min(mn);
        e.1 = e.1.max(mx);
        e.2 += sum;
        e.3 += n;
    }
    for (k, mut v) in b.med {
        let e = a.med.entry(k).or_default();
        if e.len() < 400_000 {
            e.append(&mut v);
        }
    }
    a.flags_above_9 += b.flags_above_9;
    a.color_negative += b.color_negative;
    a.color_zero += b.color_zero;
    a.inner_gt_outer += b.inner_gt_outer;
    a.cone_on_coneless_type += b.cone_on_coneless_type;
    a.cone_zero_on_spot += b.cone_zero_on_spot;
    a.atten_zero += b.atten_zero;
    a.atten_negative += b.atten_negative;
    a.volume_link_set += b.volume_link_set;
    a.asset_paths += b.asset_paths;
    a.asset_path_bad += b.asset_path_bad;
    a.atmos_uniform += b.atmos_uniform;
    a.atmos_varying += b.atmos_varying;
    a.envprobe_zones += b.envprobe_zones;
    a.envprobes += b.envprobes;
    a.envprobe_asset_index_bad += b.envprobe_asset_index_bad;
    a.envprobe_list_tail += b.envprobe_list_tail;
    a.lgpos_zones += b.lgpos_zones;
    a.near_fade_set += b.near_fade_set;
    a.volfog_set += b.volfog_set;
    a.gobo_set += b.gobo_set;
    if b.envprobe_weight_min.is_finite() {
        a.envprobe_weight_min = a.envprobe_weight_min.min(b.envprobe_weight_min);
    }
    if b.envprobe_weight_max.is_finite() {
        a.envprobe_weight_max = a.envprobe_weight_max.max(b.envprobe_weight_max);
    }
    for s in b.asset_path_samples {
        if a.asset_path_samples.len() < 20 && !a.asset_path_samples.contains(&s) {
            a.asset_path_samples.push(s);
        }
    }
    for s in b.atmos_examples {
        if a.atmos_examples.len() < 14 && !a.atmos_examples.contains(&s) {
            a.atmos_examples.push(s);
        }
    }
    for s in b.lightbin_examples {
        if a.lightbin_examples.len() < 10 && !a.lightbin_examples.contains(&s) {
            a.lightbin_examples.push(s);
        }
    }
    for v in b.saturation_samples {
        if a.saturation_samples.len() < 500 {
            a.saturation_samples.push(v);
        }
    }
    for (k, v) in b.gobo0_hist {
        *a.gobo0_hist.entry(k).or_default() += v;
    }
    for (k, v) in b.gobo1_hist {
        *a.gobo1_hist.entry(k).or_default() += v;
    }
    for (k, v) in b.ref_hits {
        *a.ref_hits.entry(k).or_default() += v;
    }
    for (k, v) in b.light_zone_suffix {
        *a.light_zone_suffix.entry(k).or_default() += v;
    }
    a.top_light_zones.extend(b.top_light_zones);
    a.top_light_zones.sort_by_key(|r| std::cmp::Reverse(r.0));
    a.top_light_zones.truncate(15);
    for v in b.extent_samples {
        if a.extent_samples.len() < 40 {
            a.extent_samples.push(v);
        }
    }
    for i in 0..5 {
        a.cond_name_match[i] += b.cond_name_match[i];
        a.cond_name_miss[i] += b.cond_name_miss[i];
        a.cond_name_null[i] += b.cond_name_null[i];
    }
    for s in b.cond_name_examples {
        if a.cond_name_examples.len() < 12 {
            a.cond_name_examples.push(s);
        }
    }
    for (k, v) in b.suffix_hist {
        *a.suffix_hist.entry(k).or_default() += v;
    }
    for (k, v) in b.probe_asset_index_hist {
        *a.probe_asset_index_hist.entry(k).or_default() += v;
    }
    for (k, v) in b.probe_asset_rows {
        *a.probe_asset_rows.entry(k).or_default() += v;
    }
    for s in b.probe_asset_examples {
        if a.probe_asset_examples.len() < 10 {
            a.probe_asset_examples.push(s);
        }
    }
}

/// Which asset family an id belongs to, by the path extension in the name list.
fn family(path: &str) -> Option<&'static str> {
    let p = path.to_ascii_lowercase();
    if p.ends_with(".atmosphere") {
        Some("atmosphere")
    } else if p.ends_with(".zonelightbin") {
        Some("zonelightbin")
    } else if p.ends_with(".levellight") {
        Some("levellight")
    } else if p.ends_with(".lightgrid") {
        Some("lightgrid")
    } else if p.ends_with(".envprobe") {
        Some("envprobe")
    } else {
        None
    }
}

const LIGHTING_SECTIONS: &[(u32, &str)] = &[
    (0x97FF_6EB5, "Zone Lights"),
    (0xB6A0_B72A, "Zone Atmosphere Name"),
    (0x7A60_75BE, "Zone Level Light Name"),
    (0xE14D_4601, "Zone Light Bin Asset"),
    (0x0277_95C5, "Zone EnvProbe Assets"),
    (0x5570_13E9, "Zone EnvProbe List"),
    (0xDA9E_4CDF, "Zone Env Probe Draw Lists"),
    (0xBADA_4DC7, "Zone Env Probe Assets (alt tag)"),
    (0xF207_7E73, "Zone Env Probe List (alt tag)"),
    (0x57D2_5F50, "Zone Light Grid Positions"),
    (0xA17A_F09B, "Zone Light Bin"),
    (0x1999_D0E3, "Lighting Condition References"),
    (0x0C0F_F507, "Light Grid Mod"),
    (0x2E5D_6024, "Light Grid Mod Config Path"),
    (0xE250_5EC5, "Lighting Zones Enabled"),
    (0x3363_33A4, "Lighting Condition"),
];

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let game = a
        .first()
        .filter(|s| !s.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let want = a
        .get(1)
        .filter(|s| !s.starts_with("--"))
        .cloned()
        .unwrap_or_default();
    let limit = a
        .iter()
        .position(|s| s == "--limit")
        .and_then(|i| a.get(i + 1))
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(usize::MAX);

    let idx = level::get_index(&game, None).expect("index");

    // `--sect <hex tag> <zone substring>`: hex-dump one section of the first
    // matching zone, for identifying a block the name list does not cover.
    if let Some(i) = a.iter().position(|s| s == "--sect") {
        let tag = u32::from_str_radix(a[i + 1].trim_start_matches("0x"), 16).expect("hex tag");
        let sub = a.get(i + 2).cloned().unwrap_or_default();
        let mut shown = 0;
        for z in &idx.zones {
            if !sub.is_empty() && !z.path.contains(&sub) {
                continue;
            }
            let Ok(raw) = idx.extract(z.id) else { continue };
            let Ok(zone) = Zone::parse(&raw) else { continue };
            let Some(sec) = zone.dat1.get_section_data(tag) else {
                continue;
            };
            println!("\n{} :: {tag:#010X} ({} bytes)", z.path, sec.len());
            for (r, row) in sec.chunks(16).take(24).enumerate() {
                let hex: Vec<String> = row.iter().map(|b| format!("{b:02X}")).collect();
                let txt: String = row
                    .iter()
                    .map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { '.' })
                    .collect();
                println!("  +{:04X}  {:<48}  {txt}", r * 16, hex.join(" "));
            }
            // decode as { u64 asset_id, u32 string_offset, u32 suffix } rows
            for (k, c) in sec.chunks_exact(16).take(10).enumerate() {
                let id = u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
                let so = u32::from_le_bytes([c[8], c[9], c[10], c[11]]);
                let sh = u32::from_le_bytes([c[12], c[13], c[14], c[15]]);
                println!(
                    "   as AssetRef[{k}] id={id:016X} strOff={so} suffix={sh:#010X} name={:?} pool={:?}",
                    idx.name_of(id).unwrap_or(""),
                    zone.dat1.get_string(so).unwrap_or_default()
                );
            }
            shown += 1;
            if shown >= 3 {
                break;
            }
        }
        return;
    }

    println!(
        "== light_probe ==\ngame: {game}\nTOC assets: {}   named: {}   zones in level: {}",
        idx.assets.len(),
        idx.names.len(),
        idx.zones.len()
    );

    // ---- the DDL item-name hashes the zone data should carry -------------
    let want_hashes: Vec<(String, u32)> = ["kDay", "kOvercast", "kSunset", "kNight", "kBlackout"]
        .iter()
        .map(|n| (n.to_string(), crc32::hash(n)))
        .collect();
    println!("\n-- LightingConditions item-name hashes (crc32 of the DDL item name) --");
    for (n, h) in &want_hashes {
        println!("   {n:<11} {h:#010X}");
    }

    // ---- asset census by extension ---------------------------------------
    let mut census: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut known_ids: HashMap<u64, &'static str> = HashMap::new();
    for (id, path) in &idx.names {
        if let Some(f) = family(path) {
            *census.entry(f).or_default() += 1;
            known_ids.insert(*id, f);
        }
    }
    println!("\n-- lighting asset census (from the 169k name list) --");
    for (f, n) in &census {
        println!("   {f:<14} {n}");
    }
    let zlb_examples: Vec<&String> = idx
        .names
        .values()
        .filter(|p| p.to_ascii_lowercase().ends_with(".zonelightbin"))
        .take(6)
        .collect();
    println!("-- zonelightbin path examples --");
    for p in zlb_examples {
        println!("   {p}");
    }
    let ll_examples: Vec<&String> = idx
        .names
        .values()
        .filter(|p| p.to_ascii_lowercase().ends_with(".levellight"))
        .take(6)
        .collect();
    println!("-- levellight path examples --");
    for p in ll_examples {
        println!("   {p}");
    }

    let zones: Vec<(String, u64)> = idx
        .zones
        .iter()
        .filter(|z| want.is_empty() || z.path.contains(&want))
        .take(limit)
        .map(|z| (z.path.clone(), z.id))
        .collect();
    println!("\nscanning {} zones", zones.len());

    let out = Mutex::new(Acc {
        envprobe_weight_min: f32::INFINITY,
        envprobe_weight_max: f32::NEG_INFINITY,
        ..Default::default()
    });

    zones.par_chunks(64).for_each(|chunk| {
        let mut acc = Acc {
            envprobe_weight_min: f32::INFINITY,
            envprobe_weight_max: f32::NEG_INFINITY,
            ..Default::default()
        };
        for (path, id) in chunk {
            let Ok(raw) = idx.extract(*id) else { continue };
            let Ok(zone) = Zone::parse(&raw) else { continue };
            acc.zones += 1;

            for (tag, _) in LIGHTING_SECTIONS {
                if zone.dat1.get_section_data(*tag).is_some() {
                    *acc.sections.entry(*tag).or_default() += 1;
                }
            }

            // Which section of a zone holds a reference to a lighting asset?
            // Scan every section for an 8-byte-aligned u64 that is a known
            // atmosphere / zonelightbin / levellight / envprobe asset id.
            for s in &zone.dat1.sections {
                let Some(data) = zone.dat1.get_section_data(s.tag) else {
                    continue;
                };
                let mut seen: Vec<&'static str> = Vec::new();
                for c in data.chunks_exact(8) {
                    let v = u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
                    if v == 0 {
                        continue;
                    }
                    if let Some(f) = known_ids.get(&v) {
                        if !seen.contains(f) {
                            seen.push(f);
                        }
                    }
                }
                for f in seen {
                    *acc.ref_hits.entry((f, s.tag)).or_default() += 1;
                }
            }

            let scene = zone.dat1.get_section_data(T_SCENE_OBJECTS).unwrap_or(&[]);
            let lights = zl::read_zone_lights(&zone);
            if !lights.is_empty() {
                acc.zones_with_lights += 1;
                let suffix = path
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .rsplit_once('_')
                    .map(|(_, s)| s.to_string())
                    .unwrap_or_else(|| "(none)".into());
                *acc.light_zone_suffix.entry(suffix).or_default() += 1;
                acc.top_light_zones.push((lights.len(), path.clone()));
                acc.top_light_zones.sort_by_key(|r| std::cmp::Reverse(r.0));
                acc.top_light_zones.truncate(15);
            }
            // Raw offsets, so a decode that was rejected still gets counted.
            let raw_offsets: Vec<u32> = zone
                .dat1
                .get_section_data(zl::T_LIGHTS)
                .map(|s| {
                    s.chunks_exact(4)
                        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                        .collect()
                })
                .unwrap_or_default();
            if raw_offsets.len() != lights.len() {
                // kind mismatch or truncation: record it as a bad entry size 0
                *acc.entry_sizes.entry(0).or_default() += raw_offsets.len() - lights.len();
            }

            for l in &lights {
                acc.lights += 1;
                *acc.entry_sizes.entry(l.entry_size).or_default() += 1;
                *acc.type_counts.entry(l.light_type).or_default() += 1;
                if l.casts_shadows {
                    *acc.shadow_by_type.entry(l.light_type).or_default() += 1;
                }
                *acc.shadow_cast_hist.entry(l.shadow_cast_type).or_default() += 1;
                *acc.shadow_res_hist.entry(l.shadow_res).or_default() += 1;
                *acc.spec_type_hist.entry(l.spec_type).or_default() += 1;
                for b in 0..16u8 {
                    if l.flags & (1 << b) != 0 {
                        *acc.flag_bit_hist.entry(b).or_default() += 1;
                    }
                }
                if l.flags & !zl::LIGHT_FLAG_SERIALIZED_MASK != 0 {
                    acc.flags_above_9 += 1;
                }
                *acc.cond_hist.entry(l.lighting_conditions).or_default() += 1;
                *acc.platform_mask_hist.entry(l.platform_mask).or_default() += 1;

                let c = l.linear_rgb;
                if c.iter().any(|v| *v < 0.0) {
                    acc.color_negative += 1;
                }
                if c.iter().all(|v| *v == 0.0) {
                    acc.color_zero += 1;
                }
                if zl::has_cone(l.light_type) {
                    if l.inner_cone_radians > l.outer_cone_radians {
                        acc.inner_gt_outer += 1;
                    }
                    if l.outer_cone_radians == 0.0 {
                        acc.cone_zero_on_spot += 1;
                    }
                } else if l.inner_cone_radians != 0.0 || l.outer_cone_radians != 0.0 {
                    acc.cone_on_coneless_type += 1;
                }
                if !zl::is_volume_light(l.light_type) {
                    if l.attenuation_radius == 0.0 {
                        acc.atten_zero += 1;
                    }
                    if l.attenuation_radius < 0.0 {
                        acc.atten_negative += 1;
                    }
                } else {
                    if acc.saturation_samples.len() < 500 {
                        acc.saturation_samples.push(l.saturation);
                    }
                }
                if l.volume_link != 0 {
                    acc.volume_link_set += 1;
                }
                if l.near_fade_start_dist != 0.0 {
                    acc.near_fade_set += 1;
                }
                if l.volumetric_fog_intensity != 0.0 {
                    acc.volfog_set += 1;
                }
                if l.gobo_custom_data != [0, 0] {
                    acc.gobo_set += 1;
                }
                *acc.gobo0_hist.entry(l.gobo_custom_data[0]).or_default() += 1;
                *acc.gobo1_hist.entry(l.gobo_custom_data[1]).or_default() += 1;
                if zl::is_volume_light(l.light_type) && acc.extent_samples.len() < 40 {
                    acc.extent_samples.push(l.extents);
                }
                if l.asset_offset != 0 {
                    if l.asset_path.is_empty()
                        || !l
                            .asset_path
                            .bytes()
                            .all(|b| b.is_ascii_graphic() || b == b' ')
                    {
                        acc.asset_path_bad += 1;
                    } else {
                        acc.asset_paths += 1;
                        if acc.asset_path_samples.len() < 20 {
                            acc.asset_path_samples.push(l.asset_path.clone());
                        }
                    }
                }

                let t = l.light_type;
                note(&mut acc, t, "luminance", l.luminance);
                note(&mut acc, t, "effective_radius", l.effective_radius);
                if !zl::is_volume_light(t) {
                    note(&mut acc, t, "fade_out_dist", l.fade_out_dist);
                    note(&mut acc, t, "attenuation_radius", l.attenuation_radius);
                    note(&mut acc, t, "cut_off_dist", l.cut_off_dist);
                    note(&mut acc, t, "cut_on_dist", l.cut_on_dist);
                    note(&mut acc, t, "spec_intensity", l.spec_intensity);
                    note(&mut acc, t, "shadow_fade_out_dist", l.shadow_fade_out_dist);
                    note(&mut acc, t, "bulb_radius", l.bulb_radius);
                    note(&mut acc, t, "bulb_length", l.bulb_length);
                } else {
                    note(&mut acc, t, "saturation", l.saturation);
                    note(&mut acc, t, "falloff_neg_x", l.falloff_dist_neg[0]);
                    note(&mut acc, t, "falloff_pos_x", l.falloff_dist_pos[0]);
                    note(&mut acc, t, "extent_x", l.extents[0]);
                }
                if zl::has_cone(t) {
                    note(&mut acc, t, "inner_deg", l.inner_cone_degrees);
                    note(&mut acc, t, "outer_deg", l.outer_cone_degrees);
                }
                let _ = scene;
            }

            // ---- lighting-condition blocks --------------------------------
            for tag in [
                zl::T_ATMOSPHERE_NAME,
                zl::T_LEVEL_LIGHT_NAME,
                zl::T_LIGHT_BIN_ASSET,
            ] {
                if let Some(b) = zl::read_condition_block(&zone.dat1, tag, 1) {
                    *acc.cond_blocks.entry((tag, b.condition_count)).or_default() += 1;
                    if b.condition_hashes.len() == 5 {
                        let mut h = [0u32; 5];
                        h.copy_from_slice(&b.condition_hashes);
                        acc.cond_hashes.entry(tag).or_insert(h);
                    }
                    if tag == zl::T_ATMOSPHERE_NAME {
                        if b.uniform {
                            acc.atmos_uniform += 1;
                        } else {
                            acc.atmos_varying += 1;
                        }
                        if acc.atmos_examples.len() < 14 {
                            let names: Vec<String> = b
                                .per_condition
                                .iter()
                                .map(|r| {
                                    r.first()
                                        .map(|x| {
                                            x.path
                                                .rsplit('/')
                                                .next()
                                                .unwrap_or("")
                                                .to_string()
                                        })
                                        .unwrap_or_default()
                                })
                                .collect();
                            acc.atmos_examples
                                .push(format!("{path}  ->  [{}]", names.join(", ")));
                        }
                    }
                    for row in &b.per_condition {
                        for r in row {
                            *acc.suffix_hist.entry(r.suffix_hash).or_default() += 1;
                        }
                    }
                    // End-to-end: does row i name an asset whose file name
                    // carries condition i's own word?
                    if b.condition_count == 5 {
                        for (i, row) in b.per_condition.iter().enumerate().take(5) {
                            let name = row
                                .first()
                                .map(|r| {
                                    if r.path.is_empty() {
                                        idx.name_of(r.asset_id).unwrap_or("").to_string()
                                    } else {
                                        r.path.clone()
                                    }
                                })
                                .unwrap_or_default()
                                .to_ascii_lowercase();
                            if name.is_empty() {
                                acc.cond_name_null[i] += 1;
                                continue;
                            }
                            let words = ["day", "overcast", "sunset", "night", "blackout"];
                            // Only judge names that mention any condition word.
                            if words.iter().any(|w| name.contains(w)) {
                                if name.contains(words[i]) {
                                    acc.cond_name_match[i] += 1;
                                } else {
                                    acc.cond_name_miss[i] += 1;
                                    if acc.cond_name_examples.len() < 12 {
                                        acc.cond_name_examples.push(format!(
                                            "{tag:#010X} row {i} ({}) -> {name}",
                                            words[i]
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    if tag == zl::T_LIGHT_BIN_ASSET && acc.lightbin_examples.len() < 10 {
                        let names: Vec<String> = b
                            .per_condition
                            .iter()
                            .map(|r| {
                                r.first()
                                    .map(|x| x.path.clone())
                                    .unwrap_or_default()
                            })
                            .collect();
                        acc.lightbin_examples
                            .push(format!("{path}  ->  [{}]", names.join(", ")));
                    }
                }
            }

            let probes = zl::read_env_probes(&zone.dat1);
            if !probes.is_empty() {
                acc.envprobe_zones += 1;
                acc.envprobes += probes.len();
                if let Some(sec) = zone.dat1.get_section_data(zl::T_ENVPROBE_LIST) {
                    acc.envprobe_list_tail += sec.len() % zl::ENV_PROBE_ROW_LEN;
                }
                let assets = zl::read_env_probe_assets(&zone.dat1);
                let asset_rows = assets
                    .as_ref()
                    .map(|b| b.total_texture_count)
                    .unwrap_or(0);
                *acc.probe_asset_rows.entry(asset_rows).or_default() += 1;
                if acc.probe_asset_examples.len() < 10 {
                    if let Some(b) = assets.as_ref() {
                        let names: Vec<String> = b
                            .per_condition
                            .per_condition
                            .first()
                            .map(|r| r.iter().take(3).map(|x| x.path.clone()).collect())
                            .unwrap_or_default();
                        acc.probe_asset_examples.push(format!(
                            "{} probes / {} asset refs  {:?}",
                            probes.len(),
                            asset_rows,
                            names
                        ));
                    }
                }
                for p in &probes {
                    *acc.envprobe_shape_hist.entry(p.volume_shape).or_default() += 1;
                    *acc.envprobe_flag_hist.entry(p.flags).or_default() += 1;
                    *acc.probe_asset_index_hist.entry(p.asset_index).or_default() += 1;
                    // -1 means "this probe has no cube map of its own"; only an
                    // index at or past the runtime array length is a real error.
                    if p.asset_index >= 0 && asset_rows > 0 && p.asset_index as usize >= asset_rows
                    {
                        acc.envprobe_asset_index_bad += 1;
                    }
                    acc.envprobe_weight_min = acc.envprobe_weight_min.min(p.weight);
                    acc.envprobe_weight_max = acc.envprobe_weight_max.max(p.weight);
                }
                if let Some(b) = assets {
                    *acc.cond_blocks
                        .entry((zl::T_ENVPROBE_ASSETS, b.per_condition.condition_count))
                        .or_default() += 1;
                }
            }
            if zone.dat1.get_section_data(zl::T_LIGHT_GRID_POSITIONS).is_some() {
                acc.lgpos_zones += 1;
            }
        }
        merge(&mut out.lock().unwrap(), acc);
    });

    let mut acc = out.into_inner().unwrap();

    println!("\n================ ZONE LIGHTS ================");
    println!(
        "{} zones parsed, {} carry a Zone Lights section, {} lights decoded",
        acc.zones, acc.zones_with_lights, acc.lights
    );
    println!("\n-- entry size (0xF0 = 240 = header 0x80 + body 0x70) --");
    for (sz, n) in acc.entry_sizes.iter().take(20) {
        let tag = if *sz == 240 {
            "  <- exactly the light struct"
        } else if *sz > 240 {
            "  <- + inline asset path"
        } else {
            "  <- REJECTED / short"
        };
        println!("   {sz:>6} bytes x {n}{tag}");
    }

    println!("\n-- light type ( order) --");
    for (t, n) in &acc.type_counts {
        let sh = acc.shadow_by_type.get(t).copied().unwrap_or(0);
        println!(
            "   {t} {:<14} {n:>7}   shadow casters {sh:>6} ({:.1}%)",
            zl::light_type_name(*t),
            100.0 * sh as f64 / *n as f64
        );
    }

    println!("\n-- shadow cast type --");
    for (k, n) in &acc.shadow_cast_hist {
        println!(
            "   {k} {:<22} {n}",
            zl::SHADOW_CAST_NAMES.get(*k as usize).copied().unwrap_or("INVALID")
        );
    }
    println!("-- shadow resolution --");
    for (k, n) in &acc.shadow_res_hist {
        println!(
            "   {k} {:<12} {n}",
            zl::SHADOW_RES_NAMES.get(*k as usize).copied().unwrap_or("INVALID")
        );
    }
    println!("-- spec type (flag bits 0..1) --");
    for (k, n) in &acc.spec_type_hist {
        println!(
            "   {k} {:<14} {n}",
            zl::SPEC_TYPE_NAMES.get(*k as usize).copied().unwrap_or("INVALID")
        );
    }

    println!("\n-- flag bits set --");
    for (b, n) in &acc.flag_bit_hist {
        println!(
            "   bit {b:>2} {:<26} {n:>7} ({:.1}%)",
            zl::LIGHT_FLAG_NAMES[*b as usize],
            100.0 * *n as f64 / acc.lights.max(1) as f64
        );
    }
    println!(
        "   flags with a runtime-only bit (>bit 9) set on disk: {}",
        acc.flags_above_9
    );

    println!("\n-- lighting-condition bitmask on the light --");
    for (m, n) in &acc.cond_hist {
        println!(
            "   {m:#04X} [{:<34}] {n:>7}",
            zl::lighting_condition_names(*m).join(","),

        );
    }

    println!("\n-- platform mask --");
    for (m, n) in acc.platform_mask_hist.iter().take(10) {
        println!("   {m:#010X} x {n}");
    }

    println!("\n-- invariants --");
    println!("   negative colour channel           {}", acc.color_negative);
    println!("   all-zero colour                   {}", acc.color_zero);
    println!("   inner cone > outer (spot/pyramid)  {}", acc.inner_gt_outer);
    println!("   outer cone == 0 on spot/pyramid    {}", acc.cone_zero_on_spot);
    println!("   cone set on a coneless type        {}", acc.cone_on_coneless_type);
    println!("   attenuation_radius == 0 (derived)  {}", acc.atten_zero);
    println!("   attenuation_radius < 0             {}", acc.atten_negative);
    println!("   volume_link != 0                   {}", acc.volume_link_set);
    println!("   near fade authored                 {}", acc.near_fade_set);
    println!("   volumetric fog intensity != 0      {}", acc.volfog_set);
    println!("   gobo custom data != 0              {}", acc.gobo_set);
    println!("   inline asset path, readable        {}", acc.asset_paths);
    println!("   inline asset path, unreadable      {}", acc.asset_path_bad);
    for s in acc.asset_path_samples.iter().take(12) {
        println!("       {s}");
    }

    println!("\n-- per-type field ranges (min / median / mean / max) --");
    let keys: Vec<(u8, &'static str)> = acc.stats.keys().copied().collect();
    let mut last_t = 255u8;
    for k in keys {
        if k.0 != last_t {
            println!("   [{} {}]", k.0, zl::light_type_name(k.0));
            last_t = k.0;
        }
        let (mn, mx, sum, n) = acc.stats[&k];
        let med = acc.med.get_mut(&k).map(median).unwrap_or(f32::NAN);
        println!(
            "     {:<22} {mn:>12.4} {med:>12.4} {:>12.4} {mx:>12.4}   n={n}",
            k.1,
            sum / n as f64
        );
    }

    println!("\n================ LIGHTING CONDITIONS ================");
    println!("-- condition_count seen per block tag --");
    for ((tag, count), n) in &acc.cond_blocks {
        println!("   {:#010X} condition_count={count:<3} x {n}", tag);
    }
    println!("-- serialized condition name hashes --");
    for (tag, h) in &acc.cond_hashes {
        println!("   {tag:#010X}:");
        for (i, v) in h.iter().enumerate() {
            let expect = crc32::hash(
                ["kDay", "kOvercast", "kSunset", "kNight", "kBlackout"][i],
            );
            println!(
                "     [{i}] {v:#010X}  crc32(\"{}\") = {expect:#010X}  {}",
                ["kDay", "kOvercast", "kSunset", "kNight", "kBlackout"][i],
                if *v == expect { "MATCH" } else { "differs" }
            );
        }
    }
    println!(
        "\natmosphere blocks: {} name one asset for every condition, {} vary by condition",
        acc.atmos_uniform, acc.atmos_varying
    );
    println!("-- atmosphere per condition [day, overcast, sunset, night, blackout] --");
    for s in acc.atmos_examples.iter().take(14) {
        println!("   {s}");
    }
    println!("-- zonelightbin references --");
    for s in acc.lightbin_examples.iter().take(8) {
        println!("   {s}");
    }

    println!("\n================ ENV PROBES ================");
    println!(
        "{} zones hold {} probes; Zone EnvProbe List remainder mod 0x80 = {}",
        acc.envprobe_zones, acc.envprobes, acc.envprobe_list_tail
    );
    println!("   volume shape: {:?}", acc.envprobe_shape_hist);
    println!("   flags:        {:?}", acc.envprobe_flag_hist);
    println!("   asset_index >= runtime texture array length: {}", acc.envprobe_asset_index_bad);
    println!(
        "   weight range: {:.4} .. {:.4}",
        acc.envprobe_weight_min, acc.envprobe_weight_max
    );
    println!("   zones with Zone Light Grid Positions: {}", acc.lgpos_zones);
    println!("   asset_index histogram (first 12): {:?}", acc.probe_asset_index_hist.iter().take(12).collect::<Vec<_>>());
    println!("   asset refs per zone histogram:    {:?}", acc.probe_asset_rows);
    for s in acc.probe_asset_examples.iter().take(6) {
        println!("   {s}");
    }

    println!("\n-- AssetRef suffix hash (crc32 of the extension incl. dot) --");
    for (h, n) in &acc.suffix_hist {
        println!("   {h:#010X} {:<16} x {n}", zl::suffix_name(*h));
    }
    println!("\n-- row index == lighting-condition index? (names that mention a condition word) --");
    for i in 0..5 {
        println!(
            "   row {i} {:<9} match {:>5}   mismatch {:>3}   no asset {:>4}",
            zl::LIGHTING_CONDITIONS[i],
            acc.cond_name_match[i],
            acc.cond_name_miss[i],
            acc.cond_name_null[i]
        );
    }
    for s in acc.cond_name_examples.iter().take(10) {
        println!("     mismatch: {s}");
    }

    println!("\n================ SECTION PRESENCE ================");
    let named: HashMap<u32, &str> = LIGHTING_SECTIONS.iter().copied().collect();
    let mut rows: Vec<(u32, usize)> = acc.sections.iter().map(|(k, v)| (*k, *v)).collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1));
    for (tag, n) in rows {
        println!(
            "   {tag:#010X} {:<34} {n:>6} / {} zones",
            named.get(&tag).copied().unwrap_or("?"),
            acc.zones
        );
    }

    println!("\n================ GOBO CUSTOM DATA (+0xB4, +0xB8) ================");
    println!("   distinct word0 values {}, word1 {}", acc.gobo0_hist.len(), acc.gobo1_hist.len());
    let mut g0: Vec<(u32, usize)> = acc.gobo0_hist.iter().map(|(k, v)| (*k, *v)).collect();
    g0.sort_by_key(|r| std::cmp::Reverse(r.1));
    for (v, n) in g0.iter().take(8) {
        println!(
            "   word0 {v:#010X} x {n:<6} as f32 {:>12.4}   as 2 halves {:.3}/{:.3}",
            f32::from_bits(*v),
            zl::half_to_f32((*v & 0xFFFF) as u16),
            zl::half_to_f32((*v >> 16) as u16)
        );
    }
    let mut g1: Vec<(u32, usize)> = acc.gobo1_hist.iter().map(|(k, v)| (*k, *v)).collect();
    g1.sort_by_key(|r| std::cmp::Reverse(r.1));
    for (v, n) in g1.iter().take(8) {
        println!(
            "   word1 {v:#010X} x {n:<6} as f32 {:>12.4}   as 2 halves {:.3}/{:.3}",
            f32::from_bits(*v),
            zl::half_to_f32((*v & 0xFFFF) as u16),
            zl::half_to_f32((*v >> 16) as u16)
        );
    }

    println!("\n================ WHERE LIGHTS LIVE ================");
    println!("-- light-bearing zone filename suffixes --");
    let mut sfx: Vec<(String, usize)> = acc.light_zone_suffix.into_iter().collect();
    sfx.sort_by_key(|r| std::cmp::Reverse(r.1));
    for (s, n) in sfx.iter().take(12) {
        println!("   ..._{s:<22} {n}");
    }
    println!("-- busiest light zones --");
    for (n, p) in acc.top_light_zones.iter().take(12) {
        println!("   {n:>5} lights  {p}");
    }

    println!("\n================ WHICH SECTION REFERENCES WHICH ASSET FAMILY ================");
    let mut hits: Vec<((&str, u32), usize)> = acc.ref_hits.into_iter().collect();
    hits.sort_by_key(|r| std::cmp::Reverse(r.1));
    for ((f, tag), n) in hits.iter().take(30) {
        println!(
            "   {f:<14} in {tag:#010X} {:<34} {n:>6} zones",
            named.get(tag).copied().unwrap_or("?")
        );
    }

    if !acc.extent_samples.is_empty() {
        println!("\n-- volume-light extents (matrix row lengths, metres) --");
        for e in acc.extent_samples.iter().take(10) {
            println!("   [{:.2}, {:.2}, {:.2}]", e[0], e[1], e[2]);
        }
    }

    atmosphere_pass(&idx);
    zonelightbin_pass(&idx, 400);

    if !acc.saturation_samples.is_empty() {
        let mut s = acc.saturation_samples.clone();
        println!(
            "\nvolume-light saturation samples: median {:.3}, n={}",
            median(&mut s),
            s.len()
        );
    }
}

// ---------------------------------------------------------------------------
// atmosphere reader still calls `unk3_*`.
// ---------------------------------------------------------------------------

const ATMOSPHERE_HEADER: u32 = 0x02F0_6D4E;
const ATMOSPHERE_STRINGS: u32 = 0x72F2_8658;
const WRAPPER: usize = 36;

const KEYLIGHT_FIELDS: &[(usize, &str, &str)] = &[
    (112, "KeyLightColorLinear.x", "f"),
    (116, "KeyLightColorLinear.y", "f"),
    (120, "KeyLightColorLinear.z", "f"),
    (124, "KeyLightIntensity", "f"),
    (128, "KeyLightAzimuth (deg)", "f"),
    (132, "KeyLightElevation (deg)", "f"),
    (136, "SunDiskOffsetAzimuth", "f"),
    (140, "SunDiskOffsetElevation", "f"),
    (144, "ShadowDrawDist (m)", "f"),
    (148, "CsmLodCount", "i"),
    (152, "TimeOfDay", "f"),
    (156, "SunDiameter", "f"),
    (160, "KeylightMaterialScale", "f"),
    (164, "CasterRangeLower (m)", "f"),
    (168, "CasterRangeUpper (m)", "f"),
    (172, "FarLodCaching (bool)", "i"),
    (176, "FarLodTransitionDist", "f"),
    (180, "FarLodPerfModeOpts (bool)", "i"),
];

fn atmosphere_pass(idx: &level::LevelIndex) {
    let ids: Vec<(u64, String)> = idx
        .names
        .iter()
        .filter(|(_, p)| p.to_ascii_lowercase().ends_with(".atmosphere"))
        .map(|(i, p)| (*i, p.clone()))
        .collect();
    println!("\n================ ATMOSPHERE ================");
    println!("{} named .atmosphere assets", ids.len());

    let mut header_sizes: BTreeMap<usize, usize> = BTreeMap::new();
    let mut stats: Vec<(f32, f32, f64, usize, usize)> = vec![(f32::INFINITY, f32::NEG_INFINITY, 0.0, 0, 0); KEYLIGHT_FIELDS.len()];
    let mut ok = 0usize;
    let mut examples: Vec<String> = Vec::new();
    let mut bad_az = 0usize;
    let mut bad_el = 0usize;
    let mut bad_csm = 0usize;
    let mut bad_bool = 0usize;

    for (id, path) in &ids {
        let Ok(raw) = idx.extract(*id) else { continue };
        if raw.len() <= WRAPPER {
            continue;
        }
        let Ok(dat1) = omnitool_lib::core::dat1::Dat1::parse(&raw[WRAPPER..]) else {
            continue;
        };
        let Some(h) = dat1.get_section_data(ATMOSPHERE_HEADER) else {
            continue;
        };
        *header_sizes.entry(h.len()).or_default() += 1;
        if h.len() < 184 {
            continue;
        }
        ok += 1;
        for (k, (off, _, kind)) in KEYLIGHT_FIELDS.iter().enumerate() {
            let w = u32::from_le_bytes([h[*off], h[off + 1], h[off + 2], h[off + 3]]);
            let v = if *kind == "f" { f32::from_bits(w) } else { w as i32 as f32 };
            if !v.is_finite() {
                stats[k].4 += 1;
                continue;
            }
            stats[k].0 = stats[k].0.min(v);
            stats[k].1 = stats[k].1.max(v);
            stats[k].2 += v as f64;
            stats[k].3 += 1;
        }
        let f = |o: usize| f32::from_bits(u32::from_le_bytes([h[o], h[o + 1], h[o + 2], h[o + 3]]));
        let i = |o: usize| i32::from_le_bytes([h[o], h[o + 1], h[o + 2], h[o + 3]]);
        let az = f(128);
        let el = f(132);
        if !(-360.0..=360.0).contains(&az) {
            bad_az += 1;
        }
        if !(-90.0..=90.0).contains(&el) {
            bad_el += 1;
        }
        if !(0..=8).contains(&i(148)) {
            bad_csm += 1;
        }
        if !(0..=1).contains(&i(172)) || !(0..=1).contains(&i(180)) {
            bad_bool += 1;
        }
        if examples.len() < 10 {
            // The engine's own sun direction: the direction the light travels.
            let (azr, elr) = (az.to_radians(), el.to_radians());
            let dir = [
                azr.cos() * -elr.cos(),
                -elr.sin(),
                azr.sin() * -elr.cos(),
            ];
            let lum = zl::luminance([f(112), f(116), f(120)]);
            examples.push(format!(
                "{:<46} az {az:>7.1} el {el:>6.1} -> dir [{:>6.3},{:>6.3},{:>6.3}]  colour [{:.3},{:.3},{:.3}] lum {lum:.3} inten {:.3}  shadowDist {:.0} csm {}  strings {}",
                path.rsplit('/').next().unwrap_or(""),
                dir[0], dir[1], dir[2],
                f(112), f(116), f(120), f(124),
                f(144), i(148),
                dat1.get_section_data(ATMOSPHERE_STRINGS).map(|s| s.len()).unwrap_or(0)
            ));
        }
    }

    println!("header section size histogram: {header_sizes:?}");
    println!("{ok} atmospheres with a >=184-byte header\n");
    println!("-- key-light block at +112 (72 bytes), min / mean / max --");
    for (k, (off, name, kind)) in KEYLIGHT_FIELDS.iter().enumerate() {
        let (mn, mx, sum, n, nan) = stats[k];
        println!(
            "   +{off:<4} {name:<26} {kind}  {mn:>12.4} {:>12.4} {mx:>12.4}  n={n} nonfinite={nan}",
            sum / n.max(1) as f64
        );
    }
    println!("\n   azimuth outside [-360,360]: {bad_az}   elevation outside [-90,90]: {bad_el}");
    println!("   CsmLodCount outside 0..8:   {bad_csm}   bool fields outside 0..1:    {bad_bool}");
    println!("\n-- sun direction a viewer can use straight away --");
    for e in &examples {
        println!("   {e}");
    }
}

// ---------------------------------------------------------------------------
// ZONELIGHTBIN: what is actually in a baked light grid, and can a viewer use it?
// ---------------------------------------------------------------------------

const ZLB_WRAPPER_MAGIC: u32 = 0xFA8D_90B3;

fn zonelightbin_pass(idx: &level::LevelIndex, limit: usize) {
    let ids: Vec<(u64, String)> = idx
        .names
        .iter()
        .filter(|(_, p)| p.to_ascii_lowercase().ends_with(".zonelightbin"))
        .map(|(i, p)| (*i, p.clone()))
        .collect();
    println!("\n================ ZONELIGHTBIN ================");
    println!("{} named .zonelightbin assets", ids.len());

    let mut got = 0usize;
    let mut missing = 0usize;
    let mut wrapper_ok = 0usize;
    let mut sample_sizes: BTreeMap<usize, usize> = BTreeMap::new();
    let mut blocks_hist: BTreeMap<usize, usize> = BTreeMap::new();
    let mut tag_hist: BTreeMap<u32, usize> = BTreeMap::new();
    let mut type_hist: BTreeMap<u8, usize> = BTreeMap::new();
    let mut radlog_hist: BTreeMap<i32, usize> = BTreeMap::new();
    let mut lum_min = f32::INFINITY;
    let mut lum_max = f32::NEG_INFINITY;
    let mut neg_channel = 0usize;
    let mut decoded = 0usize;
    let mut examples: Vec<String> = Vec::new();
    let mut header_dump: Option<String> = None;
    let mut secondary_dump: Option<String> = None;
    let mut offsets_rowsize: BTreeMap<usize, usize> = BTreeMap::new();
    let mut declared_vs_stored: Vec<(usize, usize, usize)> = Vec::new();

    for (id, path) in ids.iter().take(limit) {
        let Ok(raw) = idx.extract(*id) else {
            missing += 1;
            continue;
        };
        got += 1;
        if raw.len() < 40 {
            continue;
        }
        let magic = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        if magic == ZLB_WRAPPER_MAGIC {
            wrapper_ok += 1;
        }
        let Ok(dat1) = omnitool_lib::core::dat1::Dat1::parse(&raw[36..]) else {
            continue;
        };
        for s in &dat1.sections {
            *tag_hist.entry(s.tag).or_default() += 1;
        }
        let rows_hint = dat1
            .get_section_data(zl::T_LIGHT_BIN_UNNAMED)
            .map(|t| t.len() / 12)
            .unwrap_or(0);
        // The sample data lives in a SECOND DAT1 further into the file; the
        // first only carries the header and the offset table. Find it by magic.
        let body = &raw[36..];
        // The second DAT1 starts inside block 1 and DECLARES more bytes than the
        // file stores (the tail is streamed), so parse its header by hand.
        let mut sec2: Vec<(u32, u32, u32)> = Vec::new();
        let mut sec2_at = 0usize;
        let mut probe_at = 0usize;
        while probe_at + 16 <= body.len() {
            if u32::from_le_bytes([
                body[probe_at],
                body[probe_at + 1],
                body[probe_at + 2],
                body[probe_at + 3],
            ]) == 0x4441_5431
                && probe_at > 0
            {
                let n = u16::from_le_bytes([body[probe_at + 12], body[probe_at + 13]]) as usize;
                if n > 0 && n < 64 && probe_at + 16 + n * 12 <= body.len() {
                    for i in 0..n {
                        let o = probe_at + 16 + i * 12;
                        sec2.push((
                            u32::from_le_bytes([body[o], body[o + 1], body[o + 2], body[o + 3]]),
                            u32::from_le_bytes([body[o + 4], body[o + 5], body[o + 6], body[o + 7]]),
                            u32::from_le_bytes([body[o + 8], body[o + 9], body[o + 10], body[o + 11]]),
                        ));
                    }
                    sec2_at = probe_at;
                    break;
                }
            }
            probe_at += 1;
        }
        for (t, _, _) in &sec2 {
            *tag_hist.entry(*t).or_default() += 1;
        }
        if secondary_dump.is_none() && !sec2.is_empty() {
            let avail = body.len().saturating_sub(sec2_at);
            let mut lines = vec![format!(
                "{}
     second DAT1 at block-1 offset +{} ({} bytes of it stored)",
                path,
                sec2_at - dat1.total_size as usize,
                avail
            )];
            for (t, o, sz) in &sec2 {
                lines.push(format!(
                    "       {t:#010X} {:<28} off {o:>10} declared {sz:>10}  stored {:>10}",
                    omnitool_lib::core::section_names::section_name(*t).unwrap_or("?"),
                    avail.saturating_sub(*o as usize).min(*sz as usize)
                ));
            }
            secondary_dump = Some(lines.join("
"));
        }
        // Per-block offsets, when the file carries them.
        if let Some((_, o, sz)) = sec2.iter().find(|(t, _, _)| *t == zl::T_LIGHT_GRID_DATA_OFFSETS) {
            let a = sec2_at + *o as usize;
            let b = (a + *sz as usize).min(body.len());
            if a < b {
                *offsets_rowsize.entry((b - a) / rows_hint.max(1)).or_default() += 1;
            }
        }
        if header_dump.is_none() {
            if let Some(h) = dat1.get_section_data(zl::T_LIGHT_BIN_HEADER_DATA) {
                let take = h.len().min(96);
                let hex: Vec<String> = h[..take].iter().map(|b| format!("{b:02X}")).collect();
                header_dump = Some(format!(
                    "{}  Light Bin Header Data = {} bytes
     {}",
                    path, h.len(),
                    hex.chunks(16).map(|c| c.join(" ")).collect::<Vec<_>>().join("
     ")
                ));
            }
        }
        let Some((_, so, ssz)) = sec2
            .iter()
            .find(|(t, _, _)| *t == zl::T_LIGHT_GRID_SAMPLE_DATA)
            .copied()
        else {
            continue;
        };
        declared_vs_stored.push((
            ssz as usize,
            body.len().saturating_sub(sec2_at + so as usize),
            rows_hint,
        ));
        let a = sec2_at + so as usize;
        let b = body.len();
        if a >= b {
            continue;
        }
        let samples = &body[a..b];
        sample_sizes
            .entry(samples.len() % zl::LIGHT_GRID_BLOCK_BYTES)
            .and_modify(|c| *c += 1)
            .or_insert(1);
        let blocks = samples.len() / zl::LIGHT_GRID_BLOCK_BYTES;
        *blocks_hist.entry(blocks).or_default() += 1;

        // Decode a stride of samples out of the first block.
        let n = samples.len() / zl::LIGHT_GRID_CHANNELS;
        let stride = (n / 512).max(1);
        let mut first_useful: Option<String> = None;
        for k in (0..n).step_by(stride) {
            let off = k * zl::LIGHT_GRID_CHANNELS;
            let Some(s) = zl::decode_light_grid_sample(&samples[off..off + 16]) else {
                continue;
            };
            decoded += 1;
            *type_hist.entry(s.sample_type).or_default() += 1;
            *radlog_hist.entry(s.radiance_log).or_default() += 1;
            let a = s.average();
            let l = zl::luminance(a);
            if l.is_finite() {
                lum_min = lum_min.min(l);
                lum_max = lum_max.max(l);
            }
            if a.iter().any(|c| *c < -1e-6) {
                neg_channel += 1;
            }
            if first_useful.is_none() && s.sample_type >= 2 && l > 0.0 {
                let up = s.evaluate([0.0, 1.0, 0.0]);
                let down = s.evaluate([0.0, -1.0, 0.0]);
                first_useful = Some(format!(
                    "avg [{:.4},{:.4},{:.4}]  up [{:.4},{:.4},{:.4}]  down [{:.4},{:.4},{:.4}]  radLog {:>3} default {:.2} {}",
                    a[0], a[1], a[2], up[0], up[1], up[2], down[0], down[1], down[2],
                    s.radiance_log, s.defaultness,
                    zl::LIGHT_GRID_SAMPLE_TYPES[s.sample_type as usize]
                ));
            }
        }
        if examples.len() < 8 {
            if let Some(u) = first_useful {
                examples.push(format!(
                    "{:<58} {blocks:>4} blocks  {u}",
                    path.rsplit('/').next().unwrap_or("")
                ));
            }
        }
    }

    println!("extracted {got}, missing from the TOC {missing}, wrapper magic FA8D90B3 on {wrapper_ok}");

    // Is the streamed tail in another TOC span of the same asset id?
    {
        let mut spans: BTreeMap<u8, usize> = BTreeMap::new();
        let mut multi = 0usize;
        let mut sample: Option<String> = None;
        for (id, path) in ids.iter().take(limit) {
            let mut seen: Vec<(u8, u32)> = Vec::new();
            for a in idx.toc.assets().iter().filter(|a| a.asset_id == *id) {
                seen.push((a.span_index, a.size));
                *spans.entry(a.span_index).or_default() += 1;
            }
            if seen.len() > 1 {
                multi += 1;
                if sample.is_none() {
                    sample = Some(format!("{path} -> {seen:?}"));
                }
            }
        }
        println!(
            "   TOC entries per zonelightbin id: span histogram {spans:?}; {multi} ids appear in more than one span"
        );
        if let Some(s) = sample {
            println!("     e.g. {s}");
        }
    }
    for (id, path) in ids.iter().take(3) {
        let Ok(raw) = idx.extract(*id) else { continue };
        println!("
   FILE {path}  ({} bytes)", raw.len());
        let hdr: Vec<String> = raw[..36.min(raw.len())].iter().map(|b| format!("{b:02X}")).collect();
        println!("     wrapper: {}", hdr.join(" "));
        if raw.len() > 36 {
            if let Ok(d) = omnitool_lib::core::dat1::Dat1::parse(&raw[36..]) {
                println!(
                    "     DAT1 type {:#010X} declared total {} bytes, {} available, {} sections, strings {}",
                    d.unk1, d.total_size, raw.len() - 36, d.sections.len(), d.strings_pool.len()
                );
                for sh in &d.sections {
                    println!(
                        "       {:#010X} {:<32} off {:>10} size {:>10}",
                        sh.tag,
                        omnitool_lib::core::section_names::section_name(sh.tag).unwrap_or("?"),
                        sh.offset, sh.size
                    );
                }
            } else {
                println!("     first DAT1 did not parse");
            }
        }
    }

    // 0x27204B67 as 12-byte rows against the raw block-1 payload.
    println!("
-- 0x27204B67 as {{u32,u32,u32}} rows vs the raw block 1 --");
    let mut row12_ok = 0usize;
    let mut row12_bad = 0usize;
    let mut cover_exact = 0usize;
    let mut cover_off = 0usize;
    let mut printed = 0usize;
    for (id, path) in ids.iter().take(limit) {
        let Ok(raw) = idx.extract(*id) else { continue };
        if raw.len() < 44 {
            continue;
        }
        let b0 = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]) as usize;
        let b1 = u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]) as usize;
        if 36 + b0 + b1 != raw.len() {
            continue;
        }
        let Ok(d) = omnitool_lib::core::dat1::Dat1::parse(&raw[36..36 + b0]) else {
            continue;
        };
        let Some(tbl) = d.get_section_data(zl::T_LIGHT_BIN_UNNAMED) else {
            continue;
        };
        if tbl.len() % 12 == 0 {
            row12_ok += 1;
        } else {
            row12_bad += 1;
            continue;
        }
        let rows = tbl.len() / 12;
        let mut max_end = 0usize;
        let mut inside = true;
        for r in 0..rows {
            let a = u32::from_le_bytes([tbl[r * 12], tbl[r * 12 + 1], tbl[r * 12 + 2], tbl[r * 12 + 3]]);
            let bb = u32::from_le_bytes([tbl[r * 12 + 4], tbl[r * 12 + 5], tbl[r * 12 + 6], tbl[r * 12 + 7]]);
            let c = u32::from_le_bytes([tbl[r * 12 + 8], tbl[r * 12 + 9], tbl[r * 12 + 10], tbl[r * 12 + 11]]);
            let pos = [f32::from_bits(a), f32::from_bits(bb), f32::from_bits(c)];
            if printed < 2 && r < 6 {
                println!(
                    "     {} row {r}: [{:>9.1}, {:>9.1}, {:>9.1}]",
                    path.rsplit('/').next().unwrap_or(""), pos[0], pos[1], pos[2]
                );
            }
            // Every grid-block position should sit on the 16 m lattice.
            for v in pos {
                if !v.is_finite() || (v / 8.0).fract().abs() > 1e-4 {
                    inside = false;
                }
            }
            max_end = max_end.max(1);
        }
        if printed < 2 {
            println!(
                "     ^ {rows} rows, block1 = {b1} raw bytes ({:.0} B per block on average; an uncompressed 16^3 block would be {})",
                b1 as f64 / rows.max(1) as f64, zl::LIGHT_GRID_BLOCK_BYTES
            );
            let head: Vec<String> = raw[36 + b0..(36 + b0 + 32).min(raw.len())]
                .iter().map(|x| format!("{x:02X}")).collect();
            println!("       block 1 head: {}", head.join(" "));
            printed += 1;
        }
        let _ = max_end;
        if inside {
            cover_exact += 1;
        } else {
            cover_off += 1;
        }
    }
    println!("   table size divisible by 12 (Vec3 rows): {row12_ok} yes / {row12_bad} no");
    println!("   every position a multiple of 8 m:       {cover_exact} yes / {cover_off} no");
    println!("-- sections present --");
    let mut tags: Vec<(u32, usize)> = tag_hist.into_iter().collect();
    tags.sort_by_key(|r| std::cmp::Reverse(r.1));
    for (t, n) in tags.iter().take(12) {
        let name = omnitool_lib::core::section_names::section_name(*t).unwrap_or("?");
        println!("   {t:#010X} {name:<32} x {n}");
    }
    let _ = (&sample_sizes, &blocks_hist);
    println!(
        "-- blind 16-byte decode of the STORED stream, {decoded} reads --\n   \
         A SANITY CHECK ONLY, not a real unpack: the stored bytes are compressed,\n   \
         so a uniform 'explicit-occluder' verdict and any negative channel below is\n   \
         evidence the stream is NOT an array of runtime samples."
    );
    for (t, n) in &type_hist {
        println!(
            "   type {t} {:<20} {n:>8} ({:.1}%)",
            zl::LIGHT_GRID_SAMPLE_TYPES[*t as usize],
            100.0 * *n as f64 / decoded.max(1) as f64
        );
    }
    println!("   radiance exponent histogram: {radlog_hist:?}");
    println!("   average-radiance luminance range {lum_min:.6} .. {lum_max:.4}; negative channels {neg_channel}");
    if let Some(h) = &secondary_dump {
        println!("-- second DAT1 (the sample data) --
   {h}");
    }
    if !declared_vs_stored.is_empty() {
        let n = declared_vs_stored.len();
        let full = declared_vs_stored.iter().filter(|(d, s, _)| s >= d).count();
        let ratio: f64 = declared_vs_stored
            .iter()
            .map(|(d, _, r)| *d as f64 / (*r).max(1) as f64)
            .sum::<f64>()
            / n as f64;
        println!(
            "-- Light Grid Sample Data: {n} files, {full} store every declared byte --
   mean declared bytes per 16 m block: {ratio:.0}  (uncompressed would be {})",
            zl::LIGHT_GRID_BLOCK_BYTES
        );
    }
    if !offsets_rowsize.is_empty() {
        println!("-- Light Grid Data Offsets: bytes per grid block --
   {offsets_rowsize:?}");
    }
    if let Some(h) = &header_dump {
        println!("-- Light Bin Header Data --
   {h}");
    }
    println!("-- what a viewer gets per sample --");
    for e in &examples {
        println!("   {e}");
    }
}
