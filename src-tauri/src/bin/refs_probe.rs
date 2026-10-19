//! Measurement harness for Zone Asset References (0x30DADA09).
//!
//!   cargo run --release --bin refs_probe -- "<game dir>" <mode>
//!
//! Modes:
//!   census   -- layout, type tags, id families, ordering, string-pool slack
//!   dag      -- how the table relates to the shipped `dag` flattened deps
//!   grow     -- add one reference to a real zone and re-verify the result
//!
//! Read-only with respect to the game directory. Nothing is ever written there.

use std::collections::{BTreeMap, HashSet};

use omnitool_lib::core::level;
use omnitool_lib::core::zone::Zone;
use omnitool_lib::core::zone_refs::{self, ZoneRefs, TAG_ZONE_ASSET_REFS};

/// FNV-1 32 (multiply first, then xor) over the lowercased bytes -- the hash
/// the Wwise event ids in this table are built with.
fn fnv1_32_lower(s: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for b in s.to_ascii_lowercase().bytes() {
        h = h.wrapping_mul(16777619);
        h ^= b as u32;
    }
    h
}

fn main() {
    let game = std::env::args()
        .nth(1)
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let mode = std::env::args().nth(2).unwrap_or_else(|| "census".into());

    match mode.as_str() {
        "census" => census(&game),
        "dag" => dag_mode(&game),
        "grow" => grow(&game, std::env::args().nth(3)),
        other => eprintln!("unknown mode {other}"),
    }
}

// ---------------------------------------------------------------------------
// census
// ---------------------------------------------------------------------------

fn census(game: &str) {
    let idx = level::get_index(game, None).expect("level index");
    println!("zones in index: {}", idx.zones.len());

    let mut zones_with = 0u64;
    let mut rows_total = 0u64;
    let mut tags: BTreeMap<u32, u64> = BTreeMap::new();
    let mut tag_ext: BTreeMap<u32, BTreeMap<String, u64>> = BTreeMap::new();
    let mut crc_match = 0u64;
    let mut crc_rows = 0u64;
    let mut wwise_rows = 0u64;
    let mut wwise_fnv1 = 0u64;
    let mut wwise_fnv1a = 0u64;
    // ordering hypotheses
    let (mut ord_id, mut ord_name, mut ord_tag_runs, mut ord_off) = (0u64, 0u64, 0u64, 0u64);
    // presence in the TOC
    let (mut in_toc, mut not_in_toc) = (0u64, 0u64);
    let mut missing_by_tag: BTreeMap<u32, u64> = BTreeMap::new();
    // do the strings live contiguously at the end of the pool?
    let mut pool_tail_used = 0u64;
    let mut slack: BTreeMap<usize, u64> = BTreeMap::new();

    for z in idx.zones.iter() {
        let Ok(bytes) = idx.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&bytes) else { continue };
        let Some(refs) = ZoneRefs::read(&zone.dat1) else { continue };
        zones_with += 1;
        rows_total += refs.rows.len() as u64;

        for r in &refs.rows {
            *tags.entry(r.type_tag).or_default() += 1;
            let ext = r
                .name
                .rsplit_once('.')
                .map(|(_, e)| format!(".{}", e.to_ascii_lowercase()))
                .unwrap_or_else(|| "<none>".into());
            *tag_ext.entry(r.type_tag).or_default().entry(ext).or_default() += 1;

            if r.type_tag == zone_refs::TAG_WWISE_EVENT {
                wwise_rows += 1;
                if (r.asset_id >> 32) as u32 == 0xE000_0000
                    && (r.asset_id as u32) == fnv1_32_lower(&r.name)
                {
                    wwise_fnv1 += 1;
                }
                if (r.asset_id as u32) == omnitool_lib::core::fnv::hash_string(&r.name) {
                    wwise_fnv1a += 1;
                }
            } else {
                crc_rows += 1;
                if omnitool_lib::core::crc64::hash(&r.name) == r.asset_id {
                    crc_match += 1;
                }
                if idx.asset(r.asset_id).is_some() {
                    in_toc += 1;
                } else {
                    not_in_toc += 1;
                    *missing_by_tag.entry(r.type_tag).or_default() += 1;
                }
            }
        }

        // ordering
        if refs.rows.windows(2).all(|w| w[0].asset_id <= w[1].asset_id) {
            ord_id += 1;
        }
        if refs
            .rows
            .windows(2)
            .all(|w| w[0].name.to_ascii_lowercase() <= w[1].name.to_ascii_lowercase())
        {
            ord_name += 1;
        }
        if refs.rows.windows(2).all(|w| w[0].name_offset <= w[1].name_offset) {
            ord_off += 1;
        }
        // are equal tags contiguous?
        let mut seen: HashSet<u32> = HashSet::new();
        let mut runs_ok = true;
        let mut prev: Option<u32> = None;
        for r in &refs.rows {
            if prev != Some(r.type_tag) && !seen.insert(r.type_tag) {
                runs_ok = false;
            }
            prev = Some(r.type_tag);
        }
        if runs_ok {
            ord_tag_runs += 1;
        }

        // where do this table's own strings sit in the pool?
        let pool_len = zone.dat1.strings_pool.len();
        let he = zone.dat1.header_end();
        let max_end = refs
            .rows
            .iter()
            .map(|r| r.name_offset as usize - he + r.name.len() + 1)
            .max()
            .unwrap_or(0);
        let mut trailing = 0usize;
        for &b in zone.dat1.strings_pool.iter().rev() {
            if b == 0 {
                trailing += 1
            } else {
                break;
            }
        }
        *slack.entry(trailing.min(32)).or_default() += 1;
        if max_end + trailing >= pool_len {
            pool_tail_used += 1;
        }
    }

    println!("\n=== Zone Asset References (0x{TAG_ZONE_ASSET_REFS:08X}) census ===");
    println!("zones carrying the section : {zones_with} of {}", idx.zones.len());
    println!("rows                       : {rows_total}");
    println!(
        "crc64(name) == id          : {crc_match}/{crc_rows} non-Wwise rows"
    );
    println!(
        "wwise: hi==E0000000 && lo==FNV-1(lower(name)) : {wwise_fnv1}/{wwise_rows}   (FNV-1a would give {wwise_fnv1a})"
    );
    println!("asset id present in the TOC: {in_toc} yes / {not_in_toc} no (non-Wwise rows)");
    if !missing_by_tag.is_empty() {
        for (t, c) in &missing_by_tag {
            println!("    missing under tag {t:08X}: {c}");
        }
    }
    println!("\n--- row order, zones satisfying each hypothesis (of {zones_with}) ---");
    println!("  ascending asset id        : {ord_id}");
    println!("  ascending name            : {ord_name}");
    println!("  ascending name_offset     : {ord_off}");
    println!("  equal type tags contiguous: {ord_tag_runs}");

    println!("\n--- type tags ({} distinct) ---", tags.len());
    let mut tv: Vec<_> = tags.into_iter().collect();
    tv.sort_by(|a, b| b.1.cmp(&a.1));
    for (t, c) in &tv {
        let exts = tag_ext
            .get(t)
            .map(|m| {
                let mut v: Vec<_> = m.iter().collect();
                v.sort_by(|a, b| b.1.cmp(a.1));
                v.iter()
                    .take(3)
                    .map(|(e, n)| format!("{e} x{n}"))
                    .collect::<Vec<_>>()
                    .join("  ")
            })
            .unwrap_or_default();
        let label = zone_refs::type_tag_label(*t).unwrap_or("?");
        println!("  {t:08X}  {c:>7}  {label:<16} {exts}");
    }

    println!("\n--- zones whose ref strings reach the end of the pool: {pool_tail_used}/{zones_with}");
    println!("--- trailing NUL run in the string pool (capped 32) ---");
    let mut sv: Vec<_> = slack.into_iter().collect();
    sv.sort_by(|a, b| b.1.cmp(&a.1));
    for (s, c) in sv.iter().take(8) {
        println!("  {s:>3} bytes : {c} zones");
    }
}

// ---------------------------------------------------------------------------
// dag
// ---------------------------------------------------------------------------

fn dag_mode(game: &str) {
    use omnitool_lib::core::dag::Dag;
    let idx = level::get_index(game, None).expect("level index");
    let dag = Dag::load(std::path::Path::new(game)).expect("dag");
    println!("dag assets: {}", dag.len());

    let mut zones_checked = 0u64;
    let mut rows = 0u64;
    let mut row_in_dagdeps = 0u64;
    let mut dep_total = 0u64;
    let mut models_total = 0u64;
    let mut models_in_refs = 0u64;
    let mut models_in_dagdeps = 0u64;
    let mut printed = 0;

    for z in idx.zones.iter() {
        let Ok(bytes) = idx.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&bytes) else { continue };
        let Some(refs) = ZoneRefs::read(&zone.dat1) else { continue };
        let Some(di) = dag.index_of(z.id) else { continue };
        zones_checked += 1;

        let deps: HashSet<u64> = dag.deps(di).iter().map(|&i| dag.ids[i]).collect();
        dep_total += deps.len() as u64;

        let ref_ids: HashSet<u64> = refs.rows.iter().map(|r| r.asset_id).collect();
        rows += refs.rows.len() as u64;
        row_in_dagdeps += refs.rows.iter().filter(|r| deps.contains(&r.asset_id)).count() as u64;

        for &m in &zone.model_ids {
            models_total += 1;
            if ref_ids.contains(&m) {
                models_in_refs += 1;
            }
            if deps.contains(&m) {
                models_in_dagdeps += 1;
            }
        }

        if printed < 3 && !refs.rows.is_empty() && !zone.model_ids.is_empty() {
            printed += 1;
            println!(
                "\n-- {} : {} ref rows, {} models, {} dag deps",
                z.path,
                refs.rows.len(),
                zone.model_ids.len(),
                deps.len()
            );
        }
    }

    println!("\n=== 0x30DADA09 vs the shipped dag ===");
    println!("zones with both            : {zones_checked}");
    println!("dag dependency edges total : {dep_total}");
    println!("ref rows in dag deps       : {row_in_dagdeps}/{rows}");
    println!("zone model ids in ref table: {models_in_refs}/{models_total}");
    println!("zone model ids in dag deps : {models_in_dagdeps}/{models_total}");
}

// ---------------------------------------------------------------------------
// grow
// ---------------------------------------------------------------------------

fn grow(game: &str, zone_path: Option<String>) {
    let idx = level::get_index(game, None).expect("level index");
    let path = zone_path.unwrap_or_else(|| {
        // Pick the first zone that has the section, some models and a modest size.
        idx.zones
            .iter()
            .find(|z| {
                idx.extract(z.id)
                    .ok()
                    .and_then(|b| Zone::parse(&b).ok())
                    .map(|zz| {
                        zz.dat1.get_section_data(TAG_ZONE_ASSET_REFS).is_some()
                            && !zz.model_ids.is_empty()
                    })
                    .unwrap_or(false)
            })
            .map(|z| z.path.clone())
            .expect("no candidate zone")
    });

    let rec = idx.zone(&path).expect("zone not in index");
    let original = idx.extract(rec.id).expect("extract");
    println!("zone   : {path}");
    println!("id     : {:016X}", rec.id);
    println!("bytes  : {}", original.len());

    let before = Zone::parse(&original).expect("parse");
    let refs_before = ZoneRefs::read(&before.dat1).expect("refs");
    println!("refs   : {} rows", refs_before.rows.len());
    println!(
        "pool   : {} bytes, header_end {}, sections {}",
        before.dat1.strings_pool.len(),
        before.dat1.header_end(),
        before.dat1.sections.len()
    );

    // 1) Control: re-serialising an untouched zone must reproduce the bytes.
    let mut ctrl = Zone::parse(&original).expect("parse");
    let ctrl_bytes = ctrl.to_bytes();
    println!(
        "\ncontrol round trip      : {} ({} vs {} bytes)",
        if ctrl_bytes == original { "IDENTICAL" } else { "DIFFERS" },
        ctrl_bytes.len(),
        original.len()
    );

    // 2) Pick a target the zone does not already list: a model from some other
    //    zone, so the id really is in the TOC and really is a .model.
    let existing: HashSet<u64> = refs_before.rows.iter().map(|r| r.asset_id).collect();
    let mut target: Option<(u64, String)> = None;
    for z in idx.zones.iter() {
        if z.path == path {
            continue;
        }
        let Ok(b) = idx.extract(z.id) else { continue };
        let Ok(zz) = Zone::parse(&b) else { continue };
        for (i, &mid) in zz.model_ids.iter().enumerate() {
            if !existing.contains(&mid) && idx.asset(mid).is_some() {
                if let Some(name) = idx.name_of(mid) {
                    target = Some((mid, name.to_string()));
                    break;
                }
            }
            let _ = i;
        }
        if target.is_some() {
            break;
        }
    }
    let (new_id, new_name) = target.expect("no target model found");
    println!("\nadding  : {new_name}");
    println!("      id: {new_id:016X}  crc64(name)={:016X}", omnitool_lib::core::crc64::hash(&new_name));

    // 3) Grow.
    let mut grown = Zone::parse(&original).expect("parse");
    let plan = zone_refs::add_reference(&mut grown, &new_name, zone_refs::TAG_MODEL)
        .expect("add_reference");
    println!("\nplan:");
    println!("  {plan}");

    let out = grown.to_bytes();
    println!("\nresult bytes: {} (was {})", out.len(), original.len());

    // 4) Verify.
    let report = zone_refs::verify_grown(&original, &out, new_id, &new_name);
    println!("\n{report}");
}
