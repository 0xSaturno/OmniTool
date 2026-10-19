//! Verification harness for `core::level_asset` against the shipped game files.
//!
//!   cargo run --bin level_asset_check -- "F:/SteamLibrary/steamapps/common/Ratchet & Clank - Rift Apart"
//!
//! What it checks and reports, on `levels/i29/i29.level`:
//!   * asset header / block split, the DAT1 version word, and every section's
//!     byte size against the record stride it is read at
//!   * the `Level Built` counts against the parsed row counts, and every index
//!     range (children, zone refs, secondary zone refs, named links, LOD
//!     controls, game data)
//!   * that every `LevelZoneBuilt` asset id resolves to a `.zone` in the TOC,
//!     and that every zone/region name hashes back to its stored asset id
//!   * the region tree: roots, depth, types, zones owned per region
//!   * named links: name-hash agreement and world positions
//!   * hibernation: containers, clusters, instances, and whether the decoded
//!     transforms land in a plausible world-space envelope
//!   * the level asset's region/zone grouping against the path heuristic in
//!     `core::level`, which guesses regions from the zone asset path
//!
//! Exit status is non-zero when a structural check fails. Facts about the
//! shipped data rather than about our reader — zones the PC build does not
//! ship, for instance — are printed as NOTE and do not fail the run.

use omnitool_lib::core::level_asset as la;
use omnitool_lib::core::level_asset::{LevelAsset, LevelRegion};
use omnitool_lib::core::toc::Toc;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

const LEVEL_PATH: &str = "levels/i29/i29.level";

fn main() {
    let game = std::env::args()
        .nth(1)
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let game = PathBuf::from(game);
    let toc_bytes = std::fs::read(game.join("toc")).expect("read toc");
    let toc = Toc::parse(&toc_bytes).expect("parse toc");
    let archives: &Path = &game;

    let assets = toc.assets();
    let by_id: HashMap<u64, _> = assets.iter().map(|a| (a.asset_id, a.clone())).collect();
    println!("toc: {} assets", by_id.len());

    let level_id = omnitool_lib::core::crc64::hash(LEVEL_PATH);
    let Some(entry) = by_id.get(&level_id) else {
        println!("FAIL: {LEVEL_PATH} ({level_id:#018X}) not in the TOC");
        std::process::exit(1);
    };
    let raw = toc.extract_asset(entry, archives).expect("extract i29.level");
    println!("{LEVEL_PATH}: {} bytes extracted", raw.len());

    let level = match LevelAsset::parse(&raw) {
        Ok(l) => l,
        Err(e) => {
            println!("FAIL parse: {e}");
            std::process::exit(1);
        }
    };
    let mut failures = 0usize;

    // ---------------------------------------------------------------- header
    println!("\n== asset header ==");
    println!("  header present: {}", level.header.is_some());
    println!("  size words: {:?}", level.all_sizes);
    println!(
        "  DAT1 version word {:#010X} ({})",
        level.dat1.unk1,
        if level.version_known() {
            "the level builder version we know"
        } else {
            "UNKNOWN - the game may have been patched"
        }
    );
    if !level.version_known() {
        failures += 1;
        println!("  FAIL DAT1 version word is not the level builder version we know");
    }

    println!("\n== sections ==");
    for (i, s) in level.dat1.sections.iter().enumerate() {
        println!(
            "  {:#010X}  file off {:>8}  size {:>9}  {}",
            s.tag,
            s.offset,
            level.dat1.section_data[i].len(),
            section_role(s.tag).unwrap_or("(role not established)")
        );
    }

    // A wrong stride nearly always shows up as a non-zero remainder, and the
    // row count has to equal what the header declares.
    let h = &level.built;
    let strides: &[(u32, &str, usize, i32)] = &[
        (la::TAG_REGIONS_BUILT, "regions built", la::REGION_STRIDE, h.region_count),
        (la::TAG_ZONES_BUILT, "zones built", la::ZONE_STRIDE, h.zone_count),
        (la::TAG_LINKS_BUILT, "links built", la::LINK_STRIDE, h.named_link_count),
        (
            la::TAG_REGION_LOD_BUILT,
            "region LOD controls",
            la::REGION_LOD_STRIDE,
            h.lod_control_count,
        ),
        (la::TAG_ZONE_REFERENCES, "zone references", 2, h.zone_ref_count),
        (la::TAG_REGION_NAMES, "region names", 4, h.region_count),
        (la::TAG_ZONE_NAMES, "zone names", 4, h.zone_count),
        (la::TAG_LINK_NAMES, "link names", 4, h.named_link_count),
        (la::TAG_REGION_GAME_STRUCT, "game struct table", 16, h.game_data_count),
    ];
    println!("\n== record strides vs the header counts ==");
    for (tag, name, stride, want) in strides {
        match level.dat1.get_section_data(*tag) {
            Some(s) => {
                let rows = s.len() / stride;
                let rem = s.len() % stride;
                let ok = rem == 0 && rows as i32 == *want;
                println!(
                    "  {:<20} {:>8} bytes = {rows} x {stride} + {rem}   header says {want}  {}",
                    name,
                    s.len(),
                    if ok { "ok" } else { "MISMATCH" }
                );
                if !ok {
                    failures += 1;
                }
            }
            None => {
                failures += 1;
                println!("  {name}: FAIL absent");
            }
        }
    }

    println!("\n== Level Built ==");
    println!("  default_named_link_id {:#018X}", h.default_named_link_id);
    println!("  global_region_index   {}", h.global_region_index);
    println!("  region_count          {}", h.region_count);
    println!("  zone_ref_count        {}", h.zone_ref_count);
    println!("  lod_control_count     {}", h.lod_control_count);
    println!("  zone_count            {}", h.zone_count);
    println!("  named_link_count      {}", h.named_link_count);
    println!("  game_data_count       {}", h.game_data_count);

    let problems = level.validate();
    println!("\n== structural validation ==");
    if problems.is_empty() {
        println!("  every index range is in bounds and every count agrees with the header");
    } else {
        for p in problems.iter().take(30) {
            failures += 1;
            println!("  FAIL {p}");
        }
        if problems.len() > 30 {
            failures += problems.len() - 30;
            println!("  ... and {} more", problems.len() - 30);
        }
    }

    // ------------------------------------------------------------ region tree
    let depths = level.region_depths();
    let max_depth = depths.iter().flatten().copied().max().unwrap_or(0);
    let roots = level.root_regions();
    let mut depth_hist: BTreeMap<usize, usize> = BTreeMap::new();
    for d in depths.iter().flatten() {
        *depth_hist.entry(*d).or_default() += 1;
    }
    println!("\n== region tree ==");
    println!("  {} regions, {} roots", level.regions.len(), roots.len());
    println!("  max depth (roots = 0): {max_depth}; regions per depth {depth_hist:?}");
    if depths.iter().any(|d| d.is_none()) {
        failures += 1;
        println!("  FAIL a region's parent chain does not terminate");
    }

    let mut by_type: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &level.regions {
        *by_type.entry(r.type_name).or_default() += 1;
    }
    println!("  region types: {by_type:?}");

    // The evidence the RegionType ordinal names rest on.
    if let Some(g) = level.regions.get(h.global_region_index.max(0) as usize) {
        println!(
            "  global region [{}] is type {} ({}) - {}",
            g.index, g.type_id, g.type_name, g.name
        );
        if g.type_id != 0 {
            failures += 1;
            println!("  FAIL the always-resident region is not type 0, so the type table is wrong");
        }
    }
    let types_with = |pred: &dyn Fn(&LevelRegion) -> bool| -> BTreeSet<i16> {
        level
            .regions
            .iter()
            .filter(|r| pred(r))
            .map(|r| r.type_id)
            .collect()
    };
    println!(
        "  types that carry LOD controls:  {:?}  (expect only kOpenUnit = 5)",
        types_with(&|r| r.lod_controls_index >= 0)
    );
    println!(
        "  types with secondary zone refs: {:?}  (expect only kOverlay = 6)",
        types_with(&|r| r.secondary_zone_refs_count > 0)
    );
    println!(
        "  types that hold named links:    {:?}  (expect kInstanceUnit = 3, kOpenUnit = 5)",
        types_with(&|r| r.named_link_count > 0)
    );

    let mut zone_counts: Vec<i16> = level.regions.iter().map(|r| r.zone_refs_count).collect();
    zone_counts.sort_unstable();
    let total_refs: i64 = zone_counts.iter().map(|c| *c as i64).sum();
    println!(
        "  zones owned per region: min {} median {} max {} total {}",
        zone_counts.first().copied().unwrap_or(0),
        zone_counts[zone_counts.len() / 2],
        zone_counts.last().copied().unwrap_or(0),
        total_refs
    );
    println!(
        "  regions that own no zone at all: {}",
        zone_counts.iter().filter(|c| **c == 0).count()
    );

    println!("\n  top-level regions (depth 0) and their children");
    println!("  index  region                                   type               zones  2nd  links  kids");
    for &i in roots.iter() {
        let r = &level.regions[i];
        println!(
            "  [{:>4}] {:<40} {:<18} {:>5} {:>4} {:>6} {:>5}",
            r.index,
            trunc(short(&r.name), 40),
            r.type_name,
            r.zone_refs_count,
            r.secondary_zone_refs_count,
            r.named_link_count,
            r.child_count
        );
        if r.child_count > 0 {
            let s = r.child_start.max(0) as usize;
            let e = (s + r.child_count as usize).min(level.regions.len());
            let kids: Vec<String> = level.regions[s..e]
                .iter()
                .take(5)
                .map(|c| format!("{} ({})", short(&c.name), c.zone_refs_count))
                .collect();
            println!(
                "         children: {}{}",
                kids.join(", "),
                if r.child_count as usize > kids.len() {
                    format!(", +{} more", r.child_count as usize - kids.len())
                } else {
                    String::new()
                }
            );
        }
    }

    // ---------------------------------------------------- region LOD controls
    println!("\n== region LOD controls ({}) ==", level.region_lods.len());
    let mut settings: BTreeMap<(u32, u32, u32), usize> = BTreeMap::new();
    for l in &level.region_lods {
        *settings
            .entry((l.tile_radius, l.load_distance, l.unload_distance))
            .or_default() += 1;
    }
    println!("  distinct (tile_radius, load, unload) settings:");
    for ((tr, ld, ul), n) in &settings {
        println!("    tile_radius {tr:>4}  load {ld:>5}  unload {ul:>5}   x{n}");
    }
    println!(
        "  LOD reference-point bounds: {}",
        fmt_bounds(&bounds(level.region_lods.iter().map(|l| l.position)))
    );
    let ys: BTreeSet<i32> = level
        .region_lods
        .iter()
        .map(|l| l.position[1] as i32)
        .collect();
    println!("  distinct integer Y among LOD points: {ys:?} (a 2D streaming grid)");
    for r in level
        .regions
        .iter()
        .filter(|r| r.lod_controls_index >= 0)
        .take(5)
    {
        let lod = level.region_lod(r).unwrap();
        println!(
            "    [{:>4}] {:<28} pos ({:8.1},{:6.1},{:8.1}) radius {} load {} unload {}",
            r.index,
            trunc(short(&r.name), 28),
            lod.position[0],
            lod.position[1],
            lod.position[2],
            lod.tile_radius,
            lod.load_distance,
            lod.unload_distance
        );
    }

    // ------------------------------------------------------------------ zones
    println!("\n== zones ==");
    let mut zone_missing: Vec<usize> = Vec::new();
    let mut zone_name_mismatch = 0usize;
    for z in &level.zones {
        if !by_id.contains_key(&z.asset_id) {
            zone_missing.push(z.index);
        }
        if !z.name.is_empty() && omnitool_lib::core::crc64::hash(&z.name) != z.asset_id {
            zone_name_mismatch += 1;
        }
    }
    println!(
        "  {} zones; {} resolve to a TOC asset, {} do not",
        level.zones.len(),
        level.zones.len() - zone_missing.len(),
        zone_missing.len()
    );
    println!("  {zone_name_mismatch} zone names whose crc64 does not reproduce the stored asset id");
    if zone_name_mismatch > 0 {
        failures += 1;
        println!("  FAIL a stored zone asset id is not the crc64 of its stored name");
    }
    if !zone_missing.is_empty() {
        println!("  NOTE the level lists zones the PC build does not ship, so a level zone row is");
        println!("       a reference, not a guarantee that the asset is present.");
        for i in zone_missing.iter().take(5) {
            let z = &level.zones[*i];
            println!("       [{}] {:#018X} {}", z.index, z.asset_id, z.name);
        }
    }
    println!(
        "  {} of {} region names hash back to their stored asset id",
        level
            .regions
            .iter()
            .filter(|r| omnitool_lib::core::crc64::hash(&r.name) == r.asset_id)
            .count(),
        level.regions.len()
    );
    println!(
        "  {} of {} region asset ids exist in the TOC (.region is an authoring-only type)",
        level
            .regions
            .iter()
            .filter(|r| by_id.contains_key(&r.asset_id))
            .count(),
        level.regions.len()
    );

    let mut owned = vec![0usize; level.zones.len()];
    let mut secondary = vec![0usize; level.zones.len()];
    for r in &level.regions {
        for &zi in level.region_zone_indices(r) {
            if zi >= 0 {
                owned[zi as usize] += 1;
            }
        }
        for &zi in level.region_secondary_zone_indices(r) {
            if zi >= 0 {
                secondary[zi as usize] += 1;
            }
        }
    }
    println!(
        "  zones no region references: {}; zones owned by more than one region: {}",
        owned
            .iter()
            .zip(&secondary)
            .filter(|(a, b)| **a == 0 && **b == 0)
            .count(),
        owned.iter().filter(|c| **c > 1).count()
    );

    // ---------------------------------------------------- secondary zone refs
    println!("\n== secondary zone refs ==");
    let with_secondary: Vec<&LevelRegion> = level
        .regions
        .iter()
        .filter(|r| r.secondary_zone_refs_count > 0)
        .collect();
    let parallel = with_secondary
        .iter()
        .filter(|r| r.secondary_zone_refs_count == r.zone_refs_count)
        .count();
    println!(
        "  {} regions carry secondary zone refs; {parallel} have exactly as many as primary refs,",
        with_secondary.len()
    );
    println!("  so the secondary list runs parallel to the primary one, entry i covering zone i");
    let absent = level.zone_refs.iter().filter(|z| **z < 0).count();
    let absent_primary: usize = level
        .regions
        .iter()
        .map(|r| {
            level
                .region_zone_indices(r)
                .iter()
                .filter(|z| **z < 0)
                .count()
        })
        .sum();
    println!(
        "  {absent} of {} zone-ref entries are -1 (no stand-in); {absent_primary} of those sit in a PRIMARY list",
        level.zone_refs.len()
    );
    if absent_primary > 0 {
        failures += 1;
        println!("  FAIL a primary zone ref is -1, which the parallel-array reading does not allow");
    }
    for r in with_secondary.iter().take(4) {
        let named: Vec<String> = level
            .region_secondary_zone_indices(r)
            .iter()
            .map(|i| {
                if *i < 0 {
                    "-".into()
                } else {
                    level
                        .zones
                        .get(*i as usize)
                        .map(|z| short(&z.name).to_string())
                        .unwrap_or_default()
                }
            })
            .take(4)
            .collect();
        println!(
            "    [{:>4}] {:<32} {:<10} primary {:>3}  secondary {:?}",
            r.index,
            trunc(short(&r.name), 32),
            r.type_name,
            r.zone_refs_count,
            named
        );
    }

    // -------------------------------------------------------- region game data
    println!("\n== region game data ==");
    match (
        level.dat1.get_section_data(la::TAG_REGION_GAME_STRUCT),
        level.dat1.get_section_data(la::TAG_REGION_GAME_DATA),
    ) {
        (Some(t), Some(p)) => {
            let payload_off = level
                .dat1
                .sections
                .iter()
                .find(|s| s.tag == la::TAG_REGION_GAME_DATA)
                .map(|s| s.offset)
                .unwrap_or(0);
            println!(
                "  table {} bytes ({} rows), payload {} bytes at file offset {:#X}",
                t.len(),
                t.len() / 16,
                p.len(),
                payload_off
            );
            let rows = t.len() / 16;
            let mut contiguous = true;
            let mut cursor = 0u32;
            let mut in_payload = 0usize;
            for i in 0..rows {
                let r = &t[i * 16..i * 16 + 16];
                let w: Vec<u32> = (0..4)
                    .map(|k| {
                        u32::from_le_bytes([r[k * 4], r[k * 4 + 1], r[k * 4 + 2], r[k * 4 + 3]])
                    })
                    .collect();
                if i > 0 && w[2] != cursor {
                    contiguous = false;
                }
                cursor = w[2] + w[3];
                if w[2] >= payload_off
                    && (w[2] + w[3]) as usize <= payload_off as usize + p.len()
                {
                    in_payload += 1;
                }
                if i < 4 {
                    println!(
                        "    row {i}: w0 {:#010X} w1 {:#010X} off {:#010X} size {:#X}   w0 as a string {:?}",
                        w[0],
                        w[1],
                        w[2],
                        w[3],
                        level.dat1.get_string(w[0]).unwrap_or_default()
                    );
                }
            }
            println!(
                "  {in_payload} of {rows} rows have (offset, size) inside the payload section; rows contiguous: {contiguous}"
            );
            println!("  reading: w0 name string offset, w1 name hash, w2 file offset, w3 byte size");
            println!(
                "  {} regions and {} named links point at game data blocks",
                level.regions.iter().filter(|r| r.game_data_count > 0).count(),
                level
                    .named_links
                    .iter()
                    .filter(|l| l.game_data_count > 0)
                    .count()
            );
        }
        _ => println!("  one of the two game-data sections is absent"),
    }

    // ------------------------------------------------------------ named links
    println!("\n== named links ({}) ==", level.named_links.len());
    let hash_ok = level
        .named_links
        .iter()
        .filter(|l| l.name_hash_matches())
        .count();
    println!(
        "  {hash_ok} of {} link names reproduce their stored name_hash under the engine CRC-32",
        level.named_links.len()
    );
    if hash_ok != level.named_links.len() {
        failures += 1;
        println!("  FAIL a link name does not hash to its stored name_hash");
    }
    match level
        .named_links
        .iter()
        .find(|l| l.id == h.default_named_link_id)
    {
        Some(l) => println!(
            "  default (entry point): [{}] {} at ({:.1}, {:.1}, {:.1}), loads region {} ({})",
            l.index,
            l.name,
            l.position[0],
            l.position[1],
            l.position[2],
            l.load_region_index,
            level
                .regions
                .get(l.load_region_index.max(0) as usize)
                .map(|r| short(&r.name))
                .unwrap_or("-")
        ),
        None => {
            failures += 1;
            println!("  FAIL default_named_link_id matches no link row");
        }
    }
    for l in level.named_links.iter().take(10) {
        println!(
            "  [{:>3}] {:<38} ({:9.1},{:8.1},{:9.1})  loads {:<26} actor {:#018X}",
            l.index,
            trunc(l.short_name(), 38),
            l.position[0],
            l.position[1],
            l.position[2],
            trunc(
                level
                    .regions
                    .get(l.load_region_index.max(0) as usize)
                    .map(|r| short(&r.name))
                    .unwrap_or("-"),
                26
            ),
            l.actor_id
        );
    }
    println!("  ... {} more", level.named_links.len().saturating_sub(10));
    println!(
        "  link position bounds: {}",
        fmt_bounds(&bounds(level.named_links.iter().map(|l| l.position)))
    );
    println!(
        "  {} of {} link actor uids carry the engine's 'placed in a zone' bit 63",
        level
            .named_links
            .iter()
            .filter(|l| l.actor_id & 0x8000_0000_0000_0000 != 0)
            .count(),
        level.named_links.len()
    );

    // ------------------------------------------------------------ hibernation
    println!("\n== hibernation ==");
    println!(
        "  {} descriptors, {} containers decoded, {} model names, {} vfx names",
        level.hibernate_refs.len(),
        level.hibernate_zones.len(),
        level.hibernate_model_names.len(),
        level.hibernate_vfx_names.len()
    );
    if level.hibernate_refs.len() != level.hibernate_zones.len() {
        failures += 1;
        println!("  FAIL a hibernate descriptor points outside the raw data block");
    }
    let raw_len = level
        .dat1
        .get_section_data(la::TAG_HIBERNATE_RAW_DATA)
        .map(|s| s.len())
        .unwrap_or(0);
    let model_objs: usize = level.hibernate_zones.iter().map(|z| z.model_objects.len()).sum();
    let fx_objs: usize = level.hibernate_zones.iter().map(|z| z.effect_objects.len()).sum();
    let model_clusters: usize = level.hibernate_zones.iter().map(|z| z.model_clusters.len()).sum();
    let fx_clusters: usize = level.hibernate_zones.iter().map(|z| z.effect_clusters.len()).sum();
    let total_objs = model_objs + fx_objs;
    println!("  raw data block {raw_len} bytes");
    println!(
        "  {model_objs} model instances in {model_clusters} clusters; {fx_objs} effect instances in {fx_clusters} clusters"
    );
    println!("  total hibernated instances: {total_objs}");
    let declared: usize = level
        .hibernate_zones
        .iter()
        .map(|z| z.models.object_count as usize + z.effects.object_count as usize)
        .sum();
    if declared != total_objs {
        failures += 1;
        println!("  FAIL declared instance count {declared} != decoded {total_objs}");
    }
    let optimized = level
        .hibernate_zones
        .iter()
        .filter(|z| z.models.optimized != 0 || z.effects.optimized != 0)
        .count();
    println!("  containers already in the packed runtime form: {optimized} (0 expected on disk)");
    if optimized != 0 {
        failures += 1;
        println!("  FAIL shipped data is not in the half-float form the reader assumes");
    }
    let all = || {
        level
            .hibernate_zones
            .iter()
            .flat_map(|z| z.model_objects.iter().chain(z.effect_objects.iter()))
    };
    let orphans = all().filter(|o| o.cluster == usize::MAX).count();
    println!("  instances not covered by any cluster: {orphans}");
    if orphans > 0 {
        failures += 1;
        println!("  FAIL the cluster ranges do not tile the instance array");
    }

    println!(
        "  instance world positions: {}",
        fmt_bounds(&bounds(all().map(|o| o.position)))
    );
    println!(
        "  cluster centres:          {}",
        fmt_bounds(&bounds(
            level
                .hibernate_zones
                .iter()
                .flat_map(|z| z.model_clusters.iter().chain(z.effect_clusters.iter()))
                .map(|c| c.centre)
        ))
    );
    let max_off = all()
        .map(|o| o.offset[0].abs().max(o.offset[1].abs()).max(o.offset[2].abs()))
        .fold(0f32, f32::max);
    let max_radius = level
        .hibernate_zones
        .iter()
        .flat_map(|z| z.model_clusters.iter().chain(z.effect_clusters.iter()))
        .map(|c| c.radius)
        .fold(0f32, f32::max);
    println!(
        "  largest cluster-relative offset {max_off:.1} m, largest cluster radius {max_radius:.1} m"
    );
    let non_finite = all()
        .filter(|o| o.position.iter().any(|v| !v.is_finite()))
        .count();
    println!("  instances with a non-finite position: {non_finite}");
    if non_finite > 0 {
        failures += 1;
        println!("  FAIL the half-float decode produced a non-finite position");
    }
    // Every instance should sit inside the bounding sphere its own cluster
    // declares — that is the strongest single check on the offset decode.
    let mut outside_sphere = 0usize;
    for z in &level.hibernate_zones {
        for (objs, clusters) in [
            (&z.model_objects, &z.model_clusters),
            (&z.effect_objects, &z.effect_clusters),
        ] {
            for o in objs {
                if let Some(c) = clusters.get(o.cluster) {
                    let d = (o.offset[0] * o.offset[0]
                        + o.offset[1] * o.offset[1]
                        + o.offset[2] * o.offset[2])
                        .sqrt();
                    if d > c.radius * 1.05 + 0.5 {
                        outside_sphere += 1;
                    }
                }
            }
        }
    }
    println!(
        "  instances further from their cluster centre than the cluster radius: {outside_sphere} of {total_objs}"
    );
    if outside_sphere * 100 > total_objs {
        failures += 1;
        println!("  FAIL more than 1% of instances fall outside their own cluster sphere");
    }
    let rot_max = all().flat_map(|o| o.rotation).map(f32::abs).fold(0f32, f32::max);
    let rot_sane = all()
        .filter(|o| o.rotation.iter().all(|r| r.abs() <= std::f32::consts::TAU))
        .count();
    println!(
        "  Euler angles: largest magnitude {rot_max:.3} rad; {rot_sane} of {total_objs} stay inside +/- 2pi"
    );
    let scales: Vec<f32> = all().flat_map(|o| o.scale).collect();
    if !scales.is_empty() {
        let min = scales.iter().copied().fold(f32::INFINITY, f32::min);
        let max = scales.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let ones = scales.iter().filter(|s| (**s - 1.0).abs() < 1e-6).count();
        let uniform = all()
            .filter(|o| o.scale.iter().all(|s| (*s - 1.0).abs() < 1e-6))
            .count();
        println!(
            "  scale range {min:.3} .. {max:.3}; {:.1}% of axes are exactly 1.0; {uniform} instances are unscaled",
            100.0 * ones as f32 / scales.len() as f32
        );
        println!("  (an exact 1.0 only falls out of the half-float reading, so that is the decode check)");
    }
    println!(
        "  flags: {} visible, {} long range; low bits 0-4 seen on disk: {:#07b}",
        all().filter(|o| o.visible()).count(),
        all().filter(|o| o.long_range()).count(),
        all().fold(0u8, |acc, o| acc | (o.flags & 0x1F))
    );
    println!(
        "  {} instances carry a material override slice",
        all().filter(|o| o.mat_override_count > 0).count()
    );

    let mut busiest: Vec<(usize, String, [f32; 3])> = level
        .hibernate_zones
        .iter()
        .map(|z| {
            let name = level
                .zones
                .get(z.zone_index.max(0) as usize)
                .map(|zz| short(&zz.name).to_string())
                .unwrap_or_default();
            (
                z.model_objects.len() + z.effect_objects.len(),
                name,
                z.models.centre,
            )
        })
        .collect();
    busiest.sort_by(|a, b| b.0.cmp(&a.0));
    println!("  busiest hibernate zones (instances, zone, container centre):");
    for (n, name, c) in busiest.iter().take(6) {
        println!(
            "    {n:>6}  {:<46} ({:8.1},{:8.1},{:8.1})",
            trunc(name, 46),
            c[0],
            c[1],
            c[2]
        );
    }
    println!(
        "  containers holding exactly 2048 instances: {}",
        busiest.iter().filter(|(n, _, _)| *n == 2048).count()
    );
    for m in level.hibernate_model_names.iter().take(3) {
        println!("  sample hibernate model: {m}");
    }
    if let Some(z) = level
        .hibernate_zones
        .iter()
        .find(|z| !z.model_objects.is_empty())
    {
        let o = &z.model_objects[0];
        println!(
            "  worked example: zone {}",
            level
                .zones
                .get(z.zone_index.max(0) as usize)
                .map(|zz| zz.name.as_str())
                .unwrap_or("")
        );
        println!(
            "    model {:?} at ({:.2},{:.2},{:.2}) scale ({:.3},{:.3},{:.3}) flags {:#04X}",
            level
                .hibernate_model_names
                .get(o.asset_index.max(0) as usize)
                .map(|s| short(s))
                .unwrap_or(""),
            o.position[0],
            o.position[1],
            o.position[2],
            o.scale[0],
            o.scale[1],
            o.scale[2],
            o.flags
        );
    }

    // ----------------------------------------- cross-check vs the path heuristic
    println!("\n== level asset vs the path heuristic in core::level ==");
    // Group zones two ways and compare the partitions. Regions are keyed by
    // INDEX, not by name: many regions share a short name (every planet has a
    // `tile_d19.region`), and merging them would invent disagreements.
    let mut heuristic: HashMap<String, BTreeSet<usize>> = HashMap::new();
    for z in &level.zones {
        heuristic
            .entry(heuristic_region_key(&z.name))
            .or_default()
            .insert(z.index);
    }
    let mut owner: HashMap<usize, BTreeSet<usize>> = HashMap::new();
    for r in &level.regions {
        for &zi in level.region_zone_indices(r) {
            if zi >= 0 {
                owner.entry(zi as usize).or_default().insert(r.index);
            }
        }
    }
    println!(
        "  path heuristic: {} groups over {} zones; level asset: {} regions, {} of which own zones",
        heuristic.len(),
        level.zones.len(),
        level.regions.len(),
        level.regions.iter().filter(|r| r.zone_refs_count > 0).count()
    );

    let mut split_groups = 0usize;
    let mut examples: Vec<String> = Vec::new();
    for (key, zones) in &heuristic {
        let regions: BTreeSet<usize> = zones
            .iter()
            .flat_map(|z| owner.get(z).cloned().unwrap_or_default())
            .collect();
        if regions.len() > 1 {
            split_groups += 1;
            if examples.len() < 6 {
                let names: Vec<&str> = regions
                    .iter()
                    .take(5)
                    .map(|i| short(&level.regions[*i].name))
                    .collect();
                examples.push(format!(
                    "    {key} ({} zones) -> {} level regions: {names:?}",
                    zones.len(),
                    regions.len()
                ));
            }
        }
    }
    println!("  heuristic groups whose zones belong to more than one level region: {split_groups}");
    for e in &examples {
        println!("{e}");
    }

    let mut split_regions = 0usize;
    let mut rex: Vec<String> = Vec::new();
    for r in &level.regions {
        let zones = level.region_zone_indices(r);
        let keys: BTreeSet<String> = zones
            .iter()
            .filter(|z| **z >= 0)
            .filter_map(|z| level.zones.get(*z as usize))
            .map(|z| heuristic_region_key(&z.name))
            .collect();
        if keys.len() > 1 {
            split_regions += 1;
            if rex.len() < 6 {
                rex.push(format!(
                    "    [{}] {} ({} zones) -> heuristic groups {:?}",
                    r.index,
                    short(&r.name),
                    zones.len(),
                    keys.iter().take(4).collect::<Vec<_>>()
                ));
            }
        }
    }
    println!("  level regions the heuristic splits across several groups: {split_regions}");
    for e in &rex {
        println!("{e}");
    }
    println!(
        "  zones owned by more than one region, which no single-parent grouping can express: {}",
        owner.values().filter(|v| v.len() > 1).count()
    );
    println!(
        "  zone names outside levels/, keyed by directory instead: {}",
        level
            .zones
            .iter()
            .filter(|z| !z.name.to_ascii_lowercase().starts_with("levels/"))
            .count()
    );

    // ------------------------------------------------- every other .level asset
    // The reader has to work on more than one file. Sweep whatever other level
    // assets the name list knows about; skip quietly if the list is not there.
    println!("\n== other .level assets ==");
    match omnitool_lib::core::filesystem::hashes_path() {
        Ok(hp) if hp.is_file() => {
            let text = std::fs::read_to_string(&hp).unwrap_or_default();
            let mut levels: Vec<(u64, String)> = Vec::new();
            for line in text.lines() {
                let mut it = line.trim().splitn(3, ',');
                let (Some(hex), Some(path)) = (it.next(), it.next()) else {
                    continue;
                };
                if !path.to_ascii_lowercase().ends_with(".level") {
                    continue;
                }
                if let Ok(id) = u64::from_str_radix(hex, 16) {
                    levels.push((id, path.replace('\\', "/")));
                }
            }
            levels.sort();
            levels.dedup_by_key(|(id, _)| *id);
            let present: Vec<(u64, String)> = levels
                .into_iter()
                .filter(|(id, _)| by_id.contains_key(id))
                .collect();
            println!("  {} .level assets present in the TOC", present.len());
            for (id, path) in &present {
                let Some(e) = by_id.get(id) else { continue };
                let bytes = match toc.extract_asset(e, archives) {
                    Ok(b) => b,
                    Err(err) => {
                        failures += 1;
                        println!("  FAIL extract {path}: {err}");
                        continue;
                    }
                };
                match LevelAsset::parse(&bytes) {
                    Ok(l) => {
                        let bad = l.validate();
                        println!(
                            "  {:<40} {:>4} regions {:>5} zones {:>4} links {:>6} hibernated  {}",
                            trunc(short(path), 40),
                            l.regions.len(),
                            l.zones.len(),
                            l.named_links.len(),
                            l.hibernate_object_count(),
                            if bad.is_empty() {
                                "ok".to_string()
                            } else {
                                format!("{} PROBLEMS: {}", bad.len(), bad[0])
                            }
                        );
                        if !bad.is_empty() {
                            failures += 1;
                        }
                    }
                    Err(err) => {
                        failures += 1;
                        println!("  FAIL parse {path}: {err}");
                    }
                }
            }
        }
        _ => println!("  NOTE no asset name list next to the executable; sweep skipped"),
    }

    println!(
        "\n{}",
        if failures == 0 {
            "OK".to_string()
        } else {
            format!("{failures} FAILURES")
        }
    );
    if failures > 0 {
        std::process::exit(1);
    }
}

/// The key `core::level::classify` builds for a zone path, lower-cased so a
/// path's casing does not split a group on its own. Copied here on purpose:
/// the harness compares against the heuristic's *behaviour*, and a local copy
/// keeps that comparison honest if the heuristic changes.
fn heuristic_region_key(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 3 || !parts[0].eq_ignore_ascii_case("levels") {
        let dir = parts[..parts.len().saturating_sub(1)].join("/");
        return if dir.is_empty() {
            "misc".into()
        } else {
            dir.to_ascii_lowercase()
        };
    }
    let level = parts[1];
    let rest = &parts[2..parts.len() - 1];
    if rest.is_empty() {
        return format!("{level}/(root)").to_ascii_lowercase();
    }
    let group_dir = rest[0];
    let mut region_dirs = &rest[1..];
    if let Some(last) = region_dirs.last() {
        if last.to_ascii_lowercase().starts_with("tile_") {
            region_dirs = &region_dirs[..region_dirs.len() - 1];
        }
    }
    let sub = if region_dirs.is_empty() {
        group_dir.to_string()
    } else {
        region_dirs.join("/")
    };
    format!("{level}/{group_dir}/{sub}").to_ascii_lowercase()
}

fn section_role(tag: u32) -> Option<&'static str> {
    Some(match tag {
        la::TAG_LEVEL_BUILT => "Level Built",
        la::TAG_REGIONS_BUILT => "Level Regions Built",
        la::TAG_ZONES_BUILT => "Level Zones Built",
        la::TAG_LINKS_BUILT => "links built (role fixed by size)",
        la::TAG_REGION_LOD_BUILT => "region LOD controls (role fixed by size)",
        la::TAG_ZONE_REFERENCES => "zone references (role fixed by size)",
        la::TAG_REGION_NAMES => "Level Region Names",
        la::TAG_ZONE_NAMES => "Level Zone Names",
        la::TAG_LINK_NAMES => "Level Link Names",
        la::TAG_REGION_GAME_STRUCT => "game struct table (role fixed by size)",
        la::TAG_REGION_GAME_DATA => "game struct payload (role fixed by size)",
        la::TAG_HIBERNATE_MODEL_NAMES => "Level Hibernate Model Names",
        la::TAG_HIBERNATE_VFX_NAMES => "Level Hibernate VFX Names",
        la::TAG_HIBERNATE_RAW_DATA => "Level Hibernate Raw Data",
        la::TAG_HIBERNATE_DATA_DESC => "Level Hibernate Data Desc",
        _ => return None,
    })
}

fn short(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).collect::<String>() + "~"
    }
}

fn bounds(it: impl Iterator<Item = [f32; 3]>) -> Option<([f32; 3], [f32; 3])> {
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    let mut any = false;
    for p in it {
        any = true;
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    any.then_some((lo, hi))
}

fn fmt_bounds(b: &Option<([f32; 3], [f32; 3])>) -> String {
    match b {
        Some((lo, hi)) => format!(
            "x {:.1}..{:.1}  y {:.1}..{:.1}  z {:.1}..{:.1}",
            lo[0], hi[0], lo[1], hi[1], lo[2], hi[2]
        ),
        None => "(empty)".into(),
    }
}
