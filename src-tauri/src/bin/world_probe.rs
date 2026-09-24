//! World structure of a shipped `.level`: the region tree, the region game
//! data, instance placement, and hibernation.
//!
//!   cargo run --release --bin world_probe -- "<game dir>" [level path] [flags]
//!
//! Flags: `--no-zones` skips every measurement that has to open zone assets
//! (sections 3 and 5), which turns a three-minute run into a one-second one.
//!
//! The four questions it answers, in order:
//!
//!   1. **What does the game-data table actually hold?** Every row's name hash
//!      is re-derived, every payload is DDL-decoded, and every field name in the
//!      decoded tree is checked against its own field id — the level carries the
//!      field names in its own string pool, so no external schema is needed.
//!   2. **What does each region-data type contain?** One report per type, with
//!      the values, not just the shape.
//!   3. **Is an instance region placed anywhere in the world?** Measured three
//!      ways: the bytes of the region record, the world box `PlanetRegionData`
//!      stores for the pause-menu map, and the actual bounds of the region's own
//!      zones. If a placement existed, the map box of an instance region would
//!      sit out in the open world while its zones sat at their own origin.
//!   4. **What do the 87,778 hibernate instances describe?** Which zone owns a
//!      cluster, whether their world positions agree with the zone's own placed
//!      instances, and which Euler order reproduces the authored matrix.

// `commands_level_world.rs` is not declared in `lib.rs` yet — wiring happens
// after this run — so it is compiled here instead, against a shim that makes the
// `crate::core::…` paths it uses resolve inside this binary. That way the module
// is type-checked by the same `cargo check --bins` gate as everything else, and
// the shim can go once the module is declared in the library.
#[path = "../commands_level_world.rs"]
mod commands_level_world;
mod core {
    pub use omnitool_lib::core::*;
}

use omnitool_lib::core::crc32;
use omnitool_lib::core::crc64;
use omnitool_lib::core::level;
use omnitool_lib::core::level_asset::{self as la, LevelAsset};
use omnitool_lib::core::region_data::{self as rd, DataOwner, RegionData, RegionDataKind};
use omnitool_lib::core::toc::Toc;
use omnitool_lib::core::zone::Zone;
use rayon::prelude::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

const DEFAULT_LEVEL: &str = "levels/i29/i29.level";

// ---------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Aabb {
    min: [f32; 3],
    max: [f32; 3],
    n: usize,
}

impl Aabb {
    fn new() -> Self {
        Self {
            min: [f32::INFINITY; 3],
            max: [f32::NEG_INFINITY; 3],
            n: 0,
        }
    }
    fn add_box(&mut self, min: [f32; 3], max: [f32; 3]) {
        for i in 0..3 {
            self.min[i] = self.min[i].min(min[i]);
            self.max[i] = self.max[i].max(max[i]);
        }
        self.n += 1;
    }
    fn valid(&self) -> bool {
        self.n > 0 && self.min.iter().all(|v| v.is_finite())
    }
    fn centre(&self) -> [f32; 3] {
        [
            (self.min[0] + self.max[0]) * 0.5,
            (self.min[1] + self.max[1]) * 0.5,
            (self.min[2] + self.max[2]) * 0.5,
        ]
    }
    fn span(&self) -> [f32; 3] {
        [
            self.max[0] - self.min[0],
            self.max[1] - self.min[1],
            self.max[2] - self.min[2],
        ]
    }
    fn overlaps_xz(&self, o: &Aabb) -> bool {
        self.min[0] <= o.max[0]
            && o.min[0] <= self.max[0]
            && self.min[2] <= o.max[2]
            && o.min[2] <= self.max[2]
    }
    /// Fraction of `self`'s XZ footprint that `o` covers.
    fn xz_overlap_fraction(&self, o: &Aabb) -> f32 {
        let w = (self.max[0].min(o.max[0]) - self.min[0].max(o.min[0])).max(0.0);
        let d = (self.max[2].min(o.max[2]) - self.min[2].max(o.min[2])).max(0.0);
        let area = (self.max[0] - self.min[0]) * (self.max[2] - self.min[2]);
        if area <= 0.0 {
            0.0
        } else {
            w * d / area
        }
    }
    fn fmt(&self) -> String {
        if !self.valid() {
            return "(empty)".into();
        }
        let c = self.centre();
        let s = self.span();
        format!(
            "centre {:>8.0},{:>7.0},{:>8.0}  span {:>7.0},{:>6.0},{:>7.0}",
            c[0], c[1], c[2], s[0], s[1], s[2]
        )
    }
}

fn short(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n - 1).collect::<String>() + "~"
    }
}

// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flags: HashSet<&str> = args
        .iter()
        .filter(|a| a.starts_with("--"))
        .map(|s| s.as_str())
        .collect();
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let game = positional
        .first()
        .map(|s| s.to_string())
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let level_path = positional
        .get(1)
        .map(|s| s.to_string())
        .unwrap_or_else(|| DEFAULT_LEVEL.into());
    let want_zones = !flags.contains("--no-zones");

    let game_path = PathBuf::from(&game);
    let toc_bytes = std::fs::read(game_path.join("toc")).expect("read toc");
    let toc = Toc::parse(&toc_bytes).expect("parse toc");
    let by_id: HashMap<u64, _> = toc
        .assets()
        .iter()
        .map(|a| (a.asset_id, a.clone()))
        .collect();
    let entry = by_id
        .get(&crc64::hash(&level_path))
        .unwrap_or_else(|| panic!("{level_path} not in the TOC"));
    let raw = toc.extract_asset(entry, &game_path).expect("extract level");
    let level = LevelAsset::parse(&raw).expect("parse level");

    println!("== {level_path} ==");
    println!(
        "  {} bytes, DAT1 version {:#010X} ({}), {} regions, {} zones, {} links, {} game-data rows",
        raw.len(),
        level.dat1.unk1,
        if level.version_known() { "known" } else { "UNKNOWN" },
        level.regions.len(),
        level.zones.len(),
        level.named_links.len(),
        level.game_data.len(),
    );
    for p in level.validate() {
        println!("  VALIDATE: {p}");
    }

    let blocks = rd::decode_all(&level);
    section_1_table(&level, &blocks);
    section_2_content(&level, &blocks);
    let zone_boxes = if want_zones {
        measure_zones(&game, &level)
    } else {
        HashMap::new()
    };
    let region_boxes = section_3_placement(&level, &blocks, &zone_boxes);
    section_4_hibernate(&level, &region_boxes);
    if want_zones {
        section_5_hibernate_vs_zone(&game, &level);
    }
    section_6_what_the_tree_buys(&level, &blocks);
}

// ---------------------------------------------------------------------------
// 1. the table itself
// ---------------------------------------------------------------------------

fn section_1_table(level: &LevelAsset, blocks: &[RegionData]) {
    println!("\n== 1. region game-data table ==");
    let stride_rows = level.game_data_raw.len() / la::GAME_DATA_STRIDE;
    println!(
        "  Level Region Game Struct Built: {} bytes / {} = {stride_rows} rows; header declares {}",
        level.game_data_raw.len(),
        la::GAME_DATA_STRIDE,
        level.built.game_data_count
    );
    println!(
        "  Level Region Game Struct Data:  {} bytes, block offset {:#X}",
        level.game_data_payload.len(),
        level.game_data_payload_base
    );

    // Rows are back to back and every payload must land inside the payload
    // section: that is what makes the {offset, size} pair trustworthy.
    let mut hash_ok = 0;
    let mut in_range = 0;
    let mut contiguous = true;
    let mut cursor = level.game_data_payload_base;
    for row in &level.game_data {
        if crc32::hash(&row.name) == row.name_hash {
            hash_ok += 1;
        }
        if level.game_data_bytes(row).is_some() {
            in_range += 1;
        }
        if row.payload_offset != cursor {
            contiguous = false;
        }
        cursor = row.payload_offset + row.payload_size;
    }
    println!(
        "  {hash_ok}/{} rows reproduce crc32(struct name) == name_hash; {in_range} payloads inside the section; back-to-back: {contiguous}",
        level.game_data.len()
    );
    println!(
        "  payload section used up to {:#X}, section ends at {:#X}",
        cursor,
        level.game_data_payload_base + level.game_data_payload.len() as u32
    );

    // Field names come from the level's own string pool. Hashing them back to
    // the field id is the proof that the pool entry is the field's name.
    let mut fields_ok = 0;
    let mut fields_total = 0;
    let mut empty = 0;
    let mut failed = 0;
    for b in blocks {
        let (ok, total) = rd::field_names_verify(&b.fields);
        fields_ok += ok;
        fields_total += total;
        if b.empty {
            empty += 1;
        }
        if b.parse_failed {
            failed += 1;
            let why = level
                .game_data
                .get(b.row)
                .and_then(|row| level.game_data_bytes(row))
                .map(|bytes| match omnitool_lib::core::ddl::parse(bytes, &level.dat1) {
                    Ok(_) => "parses on a retry".to_string(),
                    Err(e) => e.to_string(),
                })
                .unwrap_or_else(|| "payload outside the section".into());
            // The DDL header's own size word against the row's size: a short row
            // is a different problem from a wire-format case we misread.
            if let Some(bytes) = level.game_data.get(b.row).and_then(|r| level.game_data_bytes(r)) {
                let w = |i: usize| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
                println!(
                    "      header: guard {:#X} magic {:#X} fields {} size word {} vs row size {}; payload section has {} bytes left after this row",
                    w(0),
                    w(1),
                    w(2),
                    w(3),
                    b.payload_size,
                    level.game_data_payload.len() as u32
                        - (level.game_data[b.row].payload_offset - level.game_data_payload_base)
                        - b.payload_size
                );
                // The top-level field table, read by hand. A field type or array
                // kind that appears here and nowhere else in the level is the
                // thing `core::ddl` misreads.
                let n = w(2) as usize;
                for f in 0..n.min(32) {
                    let id = w(4 + f * 2);
                    let meta = w(5 + f * 2);
                    let name_off = w(4 + n * 2 + f);
                    let kind = (meta >> 24) as u8;
                    println!(
                        "        field {:08X} type {:>2} ({:?}) count {} array {:?} name {:?}",
                        id,
                        kind,
                        omnitool_lib::core::ddl::DdlTypeKind::from_byte(kind),
                        (meta >> 4) & 0xF_FFFF,
                        omnitool_lib::core::ddl::DdlArrayKind::from_byte(meta as u8),
                        level.dat1.get_string(name_off)
                    );
                }
                // How far past its own end does the reader run? The smallest
                // padding that lets it finish is the size of the overshoot, which
                // is the number to hand to whoever fixes `core::ddl`.
                let mut overshoot = None;
                for pad in (4..=8192).step_by(4) {
                    let mut padded = bytes.to_vec();
                    padded.resize(bytes.len() + pad, 0);
                    if omnitool_lib::core::ddl::parse(&padded, &level.dat1).is_ok() {
                        overshoot = Some(pad);
                        break;
                    }
                }
                match overshoot {
                    Some(pad) => println!(
                        "        the reader wants {pad} bytes more than the row declares, so it drifts inside a nested struct"
                    ),
                    None => println!("        padding up to 8 KiB does not let the reader finish"),
                }
            }
            println!(
                "      row {} {} ({} bytes) on region {:?} does not parse as DDL: {why}",
                b.row,
                b.struct_name,
                b.payload_size,
                b.owner_region()
                    .and_then(|i| level.regions.get(i))
                    .map(|r| r.name.clone())
            );
        }
    }
    println!(
        "  {fields_ok}/{fields_total} field names in the decoded payloads hash to their own field id"
    );
    println!(
        "  {failed} payloads fail to parse as DDL; {empty} parse to an object with no fields (a 16-byte header, which is legitimate)"
    );

    let mut per_kind: BTreeMap<&str, (usize, BTreeSet<&'static str>, u32, u32, usize)> =
        BTreeMap::new();
    for b in blocks {
        let e = per_kind
            .entry(b.kind.label())
            .or_insert((0, BTreeSet::new(), u32::MAX, 0, 0));
        e.0 += 1;
        if let Some(ri) = b.owner_region() {
            if let Some(r) = level.regions.get(ri) {
                e.1.insert(r.type_name);
            }
        }
        e.2 = e.2.min(b.payload_size);
        e.3 = e.3.max(b.payload_size);
        if b.is_override() {
            e.4 += 1;
        }
    }
    println!(
        "  {:<28} {:>6} {:>14} {:>10}  owning region types",
        "struct", "blocks", "payload bytes", "overrides"
    );
    for (k, (n, types, lo, hi, ov)) in &per_kind {
        println!(
            "  {k:<28} {n:>6} {:>14} {ov:>10}  {}",
            if lo == hi {
                format!("{lo}")
            } else {
                format!("{lo}..{hi}")
            },
            types.iter().copied().collect::<Vec<_>>().join(", ")
        );
    }
    let unowned = blocks
        .iter()
        .filter(|b| matches!(b.owner, DataOwner::Region(i) if i == usize::MAX))
        .count();
    let by_link = blocks
        .iter()
        .filter(|b| matches!(b.owner, DataOwner::Link(_)))
        .count();
    println!("  rows claimed by a named link: {by_link}; rows no region or link claims: {unowned}");
}

// ---------------------------------------------------------------------------
// 2. what each type contains
// ---------------------------------------------------------------------------

fn section_2_content(level: &LevelAsset, blocks: &[RegionData]) {
    println!("\n== 2. what each region-data type contains ==");
    let region_name = |b: &RegionData| -> String {
        b.owner_region()
            .and_then(|i| level.regions.get(i))
            .map(|r| format!("{} [{}]", trunc(short(&r.name), 40), r.type_name))
            .unwrap_or_else(|| "(unowned)".into())
    };

    // ---- PlanetRegionData -------------------------------------------------
    println!("\n  -- PlanetRegionData: planet identity, ship menu, and the pause-menu map boxes --");
    println!(
        "  {:<40} {:<14} {:<16} {:<6} {:>4} {:>4} {:>4}  {}",
        "region", "planet", "level enum", "main", "GB", "RP", "VP", "hero / checkpoint"
    );
    for b in blocks.iter().filter(|b| b.kind == RegionDataKind::Planet) {
        let p = b.planet().unwrap();
        println!(
            "  {:<40} {:<14} {:<16} {:<6} {:>4} {:>4} {:>4}  {} / {}",
            trunc(
                &b.owner_region()
                    .and_then(|i| level.regions.get(i))
                    .map(|r| short(&r.name).to_string())
                    .unwrap_or_default(),
                40
            ),
            trunc(&p.planet_name, 14),
            trunc(&p.level_enum, 16),
            p.is_main_region,
            p.gold_bolts,
            p.ryno_plans,
            p.vanity_pickups,
            trunc(&p.hero_type, 12),
            trunc(&p.checkpoint, 34),
        );
        for m in &p.maps {
            println!(
                "      map {:<24} x {:>7.0} .. {:>7.0}   z {:>7.0} .. {:>7.0}   zoom {:>5.0}/{:>5.0}  {}",
                trunc(&m.map, 24),
                m.min[0],
                m.max[0],
                m.min[2],
                m.max[2],
                m.zoom_default,
                m.zoom_max,
                trunc(short(&m.texture), 30)
            );
        }
    }

    // ---- DimensionRegionData ---------------------------------------------
    println!("\n  -- DimensionRegionData: pocket dimensions --");
    for b in blocks.iter().filter(|b| b.kind == RegionDataKind::Dimension) {
        let d = b.dimension().unwrap();
        println!(
            "  {:<44} code {:<28} {} checkpoints, {} portals",
            trunc(&region_name(b), 44),
            trunc(&d.code_name, 28),
            d.checkpoints.len(),
            d.portals.len()
        );
        for a in d.checkpoints.iter().chain(d.portals.iter()) {
            println!("      actor {:016X}  {}", a.actor_uid, trunc(&a.name, 66));
        }
    }
    // A dimension's actor uids should be actors placed in a zone (bit 63) —
    // the same convention the named links follow.
    let dim_actors: Vec<u64> = blocks
        .iter()
        .filter_map(|b| b.dimension())
        .flat_map(|d| {
            d.checkpoints
                .into_iter()
                .chain(d.portals)
                .map(|a| a.actor_uid)
        })
        .collect();
    let placed = dim_actors.iter().filter(|u| **u >> 63 == 1).count();
    println!(
        "  {} dimension actor uids, {placed} carry the \"placed in a zone\" bit 63",
        dim_actors.len()
    );

    // ---- AutoShadowZoneOverlayData ---------------------------------------
    println!("\n  -- AutoShadowZoneOverlayData: the zone an overlay shadows --");
    let mut resolved = 0;
    let mut unresolved: Vec<String> = Vec::new();
    let mut per_zone: BTreeMap<String, usize> = BTreeMap::new();
    for b in blocks
        .iter()
        .filter(|b| b.kind == RegionDataKind::AutoShadowZone)
    {
        let z = b.shadow_zone().unwrap();
        match level.zone_index_by_asset(z.asset_id) {
            Some(_) => resolved += 1,
            None => {
                if unresolved.len() < 6 {
                    unresolved.push(z.path.clone());
                }
            }
        }
        *per_zone.entry(short(&z.path).to_string()).or_default() += 1;
    }
    println!(
        "  {resolved} of {} shadow-zone paths resolve to a zone row of this level (asset id = crc64 of the path)",
        blocks
            .iter()
            .filter(|b| b.kind == RegionDataKind::AutoShadowZone)
            .count()
    );
    for u in &unresolved {
        println!("      unresolved: {u}");
    }
    println!("  most-shadowed zones:");
    let mut zs: Vec<_> = per_zone.iter().collect();
    zs.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (z, n) in zs.iter().take(6) {
        println!("      {n:>3} overlays shadow {z}");
    }

    // ---- CustomOverlayRegionData -----------------------------------------
    println!("\n  -- CustomOverlayRegionData: the expression that switches an overlay on --");
    let mut prius_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut depth_max = 0usize;
    let mut cond_kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut region_refs = 0usize;
    for b in blocks
        .iter()
        .filter(|b| b.kind == RegionDataKind::CustomOverlay)
    {
        let Some(t) = b.overlay_trigger() else { continue };
        for n in &t.nodes {
            *prius_counts.entry(n.prius.clone()).or_default() += 1;
            depth_max = depth_max.max(n.depth);
            for c in &n.conditions {
                let key = c.split('=').next().unwrap_or("?").to_string();
                if key == "region" {
                    region_refs += 1;
                }
                *cond_kinds.entry(key).or_default() += 1;
            }
        }
    }
    println!("  trigger prius types across every overlay (count of nodes):");
    let mut pc: Vec<_> = prius_counts.iter().collect();
    pc.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (p, n) in pc {
        println!("      {n:>5}  {p}");
    }
    println!("  deepest trigger nesting: {depth_max}");
    println!("  condition slots used: {cond_kinds:?}");
    println!("  triggers that name a .region path: {region_refs}");
    for b in blocks
        .iter()
        .filter(|b| b.kind == RegionDataKind::CustomOverlay)
        .take(3)
    {
        println!("  example {} :", trunc(&region_name(b), 60));
        for n in &b.overlay_trigger().unwrap().nodes {
            println!(
                "      {}{} {}",
                "  ".repeat(n.depth),
                n.prius,
                n.conditions.join(" ")
            );
        }
    }

    // ---- the small ones ---------------------------------------------------
    println!("\n  -- the rest --");
    for b in blocks.iter().filter(|b| {
        matches!(
            b.kind,
            RegionDataKind::PerfSpec
                | RegionDataKind::AutoGeneratedOverlay
                | RegionDataKind::ClankPuzzle
                | RegionDataKind::Other
        )
    }) {
        let detail = match b.kind {
            RegionDataKind::PerfSpec => {
                let (a, c) = b.perf_spec().unwrap();
                format!("impostor draw distance 30 Hz {a} m, 60 Hz {c} m")
            }
            RegionDataKind::AutoGeneratedOverlay => {
                format!("zone suffixes {:?}", b.zone_suffixes())
            }
            RegionDataKind::ClankPuzzle => format!(
                "{} bytes, {} fields — the block's presence is the whole signal",
                b.payload_size,
                b.fields.fields.len()
            ),
            _ => format!("{} fields", b.fields.fields.len()),
        };
        println!(
            "  {:<44} {:<26} {detail}",
            trunc(&region_name(b), 44),
            b.struct_name
        );
    }

    // Override wrappers.
    println!("\n  -- blocks stored as an override wrapper --");
    for b in blocks.iter().filter(|b| b.is_override()) {
        println!(
            "  {:<44} row {:>3} declares {:?}, {} overridden fields",
            trunc(&region_name(b), 44),
            b.row,
            b.override_of,
            b.fields.fields.len()
        );
    }
}

// ---------------------------------------------------------------------------
// zone bounds, measured once
// ---------------------------------------------------------------------------

/// What one zone asset says about where it is.
#[derive(Clone, Copy)]
struct ZoneMeasure {
    /// Bounds over every scene object, as the zone itself reports them.
    aabb: Aabb,
    /// 5th..95th percentile box of the placed models' positions.
    core: Aabb,
    /// Mean position of the placed models.
    centroid: [f32; 3],
    model_instances: usize,
}

/// Measurements for every zone row of the level whose asset is present and
/// parses, keyed by zone row index.
fn measure_zones(game: &str, level: &LevelAsset) -> HashMap<usize, ZoneMeasure> {
    let idx = level::get_index(game, None).expect("zone index");
    let t0 = std::time::Instant::now();
    let rows: Vec<(usize, u64)> = level
        .zones
        .iter()
        .enumerate()
        .map(|(i, z)| (i, z.asset_id))
        .collect();
    let measured: Vec<(usize, Option<ZoneMeasure>)> = rows
        .par_iter()
        .map(|(i, id)| {
            let Ok(raw) = idx.extract(*id) else {
                return (*i, None);
            };
            let Ok(zone) = Zone::parse(&raw) else {
                return (*i, None);
            };
            let mut bb = Aabb::new();
            if let Some((min, max)) = zone.bounds() {
                bb.add_box(min, max);
            }
            // The centroid of the placed models, and a 5th..95th percentile box
            // per axis. A raw AABB is decided by one stray object — a sky shell
            // or a volume parked at the origin — so every claim about "where
            // this region is" is made on these instead.
            let mut xs: Vec<f32> = Vec::new();
            let mut ys: Vec<f32> = Vec::new();
            let mut zs: Vec<f32> = Vec::new();
            let mut sum = [0.0f64; 3];
            for o in zone.scene_objects.iter().filter(|o| o.model_id != 0) {
                let p = [o.matrix[12], o.matrix[13], o.matrix[14]];
                if !p.iter().all(|v| v.is_finite()) {
                    continue;
                }
                xs.push(p[0]);
                ys.push(p[1]);
                zs.push(p[2]);
                for k in 0..3 {
                    sum[k] += p[k] as f64;
                }
            }
            let n = xs.len();
            let mut core = Aabb::new();
            let mut centroid = [0.0f32; 3];
            if n > 0 {
                for v in [&mut xs, &mut ys, &mut zs] {
                    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                }
                let lo = n * 5 / 100;
                let hi = (n * 95 / 100).max(lo);
                core.add_box(
                    [xs[lo], ys[lo], zs[lo]],
                    [xs[hi.min(n - 1)], ys[hi.min(n - 1)], zs[hi.min(n - 1)]],
                );
                for k in 0..3 {
                    centroid[k] = (sum[k] / n as f64) as f32;
                }
            }
            (
                *i,
                Some(ZoneMeasure {
                    aabb: bb,
                    core,
                    centroid,
                    model_instances: n,
                }),
            )
        })
        .collect();
    let out: HashMap<usize, ZoneMeasure> = measured
        .into_iter()
        .filter_map(|(i, v)| v.map(|v| (i, v)))
        .collect();
    println!(
        "\n  [measured {} of {} zone rows in {:.1} s]",
        out.len(),
        level.zones.len(),
        t0.elapsed().as_secs_f32()
    );
    out
}

// ---------------------------------------------------------------------------
// 3. placement
// ---------------------------------------------------------------------------

/// Everything measured about one region's own content.
#[derive(Clone, Copy)]
struct RegionSpace {
    /// Union of the raw zone bounds — what a naive "where is this region" gives.
    raw: Aabb,
    /// Union of the zones' 5th..95th percentile boxes: the region's real footprint.
    core: Aabb,
    /// Model-instance-weighted mean of its zones' centroids.
    dense: [f32; 3],
    zones: usize,
    instances: usize,
}

impl RegionSpace {
    fn empty() -> Self {
        Self {
            raw: Aabb::new(),
            core: Aabb::new(),
            dense: [0.0; 3],
            zones: 0,
            instances: 0,
        }
    }
    fn valid(&self) -> bool {
        self.core.valid() && self.instances > 0
    }
    fn dense_radius_xz(&self) -> f32 {
        (self.dense[0] * self.dense[0] + self.dense[2] * self.dense[2]).sqrt()
    }
}

fn section_3_placement(
    level: &LevelAsset,
    blocks: &[RegionData],
    zone_boxes: &HashMap<usize, ZoneMeasure>,
) -> Vec<RegionSpace> {
    println!("\n== 3. is an instance region placed anywhere? ==");
    println!(
        "  LevelRegionBuilt is {} bytes and every one of them is a known field (asset id, type,",
        la::REGION_STRIDE
    );
    println!("  name, lod index, parent, first child + count, two zone-ref ranges, links, game");
    println!("  data): a matrix, a quaternion or a translation cannot hide in the record itself.");
    println!(
        "  The only spatial record attached to a region is Level Region Lod Built, {} sets for the {} kOpenUnit regions,",
        level.region_lods.len(),
        level
            .regions
            .iter()
            .filter(|r| r.type_name == "kOpenUnit")
            .count()
    );
    println!("  and section 2 shows no game-data struct carries a transform either.");

    // Per region, from the level's own zone refs — not from a path heuristic.
    // Primary refs only: secondary refs are low-LOD stand-ins or shadow zones.
    let mut spaces: Vec<RegionSpace> = Vec::with_capacity(level.regions.len());
    for r in &level.regions {
        let mut s = RegionSpace::empty();
        let mut sum = [0.0f64; 3];
        for zi in level.region_zone_indices(r) {
            if *zi < 0 {
                continue;
            }
            let Some(m) = zone_boxes.get(&(*zi as usize)) else { continue };
            s.zones += 1;
            if m.model_instances == 0 {
                continue;
            }
            if m.aabb.valid() {
                s.raw.add_box(m.aabb.min, m.aabb.max);
            }
            if m.core.valid() {
                s.core.add_box(m.core.min, m.core.max);
            }
            s.instances += m.model_instances;
            for k in 0..3 {
                sum[k] += m.centroid[k] as f64 * m.model_instances as f64;
            }
        }
        if s.instances > 0 {
            for k in 0..3 {
                s.dense[k] = (sum[k] / s.instances as f64) as f32;
            }
        }
        spaces.push(s);
    }
    if zone_boxes.is_empty() {
        println!("  (--no-zones: zone-derived measurements skipped)");
        return spaces;
    }

    // A container's space is the union of its own zones and its children's.
    let mut merged = spaces.clone();
    for (i, r) in level.regions.iter().enumerate() {
        if r.child_count <= 0 {
            continue;
        }
        let mut s = spaces[i];
        let mut sum = [0.0f64; 3];
        let mut weight = 0usize;
        if s.instances > 0 {
            for k in 0..3 {
                sum[k] += s.dense[k] as f64 * s.instances as f64;
            }
            weight += s.instances;
        }
        for c in r.child_start..(r.child_start + r.child_count) {
            let Some(cs) = spaces.get(c as usize) else { continue };
            if !cs.valid() {
                continue;
            }
            s.raw.add_box(cs.raw.min, cs.raw.max);
            s.core.add_box(cs.core.min, cs.core.max);
            s.zones += cs.zones;
            for k in 0..3 {
                sum[k] += cs.dense[k] as f64 * cs.instances as f64;
            }
            weight += cs.instances;
            s.instances += cs.instances;
        }
        if weight > 0 {
            for k in 0..3 {
                s.dense[k] = (sum[k] / weight as f64) as f32;
            }
        }
        merged[i] = s;
    }

    // -- the map box against the zones ------------------------------------
    println!("\n  -- PlanetRegionData map box vs. the region's own content --");
    println!("  The map box is the world rectangle the pause-menu map texture covers, stored in");
    println!("  the level. If the level placed a region's zones somewhere, the box and the zones");
    println!("  would disagree by exactly that placement.");
    println!(
        "  {:<30} {:<9} {:>20} {:>20} {:>9} {:>9}",
        "region", "type", "map box centre xz", "content centre xz", "distance", "in box"
    );
    let mut near = 0;
    let mut checked = 0;
    for b in blocks.iter().filter(|b| b.kind == RegionDataKind::Planet) {
        let Some(ri) = b.owner_region() else { continue };
        let Some(r) = level.regions.get(ri) else { continue };
        let p = b.planet().unwrap();
        let s = merged[ri];
        if !s.valid() {
            continue;
        }
        for m in &p.maps {
            let mut mb = Aabb::new();
            mb.add_box(m.min, m.max);
            checked += 1;
            let c = mb.centre();
            let d = ((c[0] - s.dense[0]).powi(2) + (c[2] - s.dense[2]).powi(2)).sqrt();
            let inside = s.dense[0] >= m.min[0]
                && s.dense[0] <= m.max[0]
                && s.dense[2] >= m.min[2]
                && s.dense[2] <= m.max[2];
            if inside {
                near += 1;
            }
            println!(
                "  {:<30} {:<9} {:>20} {:>20} {:>8.0} m {:>9}",
                trunc(short(&r.name), 30),
                trunc(r.type_name.trim_start_matches('k'), 9),
                format!("{:.0},{:.0}", c[0], c[2]),
                format!("{:.0},{:.0}", s.dense[0], s.dense[2]),
                d,
                inside
            );
        }
    }
    println!(
        "  {near} of {checked} map boxes contain their own region's content centre. A region whose"
    );
    println!("  zones were moved into the world by the level could not manage that.");

    // -- where each region type's content sits ----------------------------
    println!("\n  -- is a region's content authored around the world origin, or somewhere? --");
    let mut per_type: BTreeMap<&str, (usize, usize, f32, f32)> = BTreeMap::new();
    for (i, r) in level.regions.iter().enumerate() {
        let s = merged[i];
        if !s.valid() {
            continue;
        }
        let e = per_type.entry(r.type_name).or_insert((0, 0, 0.0, 0.0));
        e.0 += 1;
        if s.dense_radius_xz() < 500.0 {
            e.1 += 1;
        }
        e.2 = e.2.max(s.dense_radius_xz());
        e.3 += s.dense_radius_xz();
    }
    println!(
        "  {:<20} {:>8} {:>18} {:>16} {:>14}",
        "region type", "measured", "centred < 500 m", "furthest", "mean |centre|"
    );
    for (t, (n, nearo, far, sum)) in &per_type {
        println!(
            "  {t:<20} {n:>8} {nearo:>18} {far:>13.0} m {:>12.0} m",
            *sum / *n as f32
        );
    }

    // -- do regions of a type stack on each other? ------------------------
    println!("\n  -- how often two regions of the same type cover the same ground --");
    let pick = |ty: &str| -> Vec<(usize, Aabb)> {
        level
            .regions
            .iter()
            .enumerate()
            .filter(|(i, r)| r.type_name == ty && merged[*i].valid())
            .map(|(i, _)| (i, merged[i].core))
            .collect()
    };
    for ty in [
        "kInstanceContainer",
        "kInstanceUnit",
        "kOpenContainer",
        "kOpenUnit",
        "kOverlay",
    ] {
        let list = pick(ty);
        let mut over = 0usize;
        let mut total = 0usize;
        for i in 0..list.len() {
            for j in i + 1..list.len() {
                total += 1;
                if list[i].1.overlaps_xz(&list[j].1) {
                    over += 1;
                }
            }
        }
        println!(
            "  {ty:<20} {:>4} regions, {over} of {total} pairs overlap in XZ ({:.0}%)",
            list.len(),
            if total == 0 {
                0.0
            } else {
                over as f64 * 100.0 / total as f64
            }
        );
    }
    println!("  Instance regions that overlap are alternate versions of one place — different acts,");
    println!("  a demo cut, a destroyed variant — and the game never loads two of them at once.");

    // -- the two Nefarious Cities -----------------------------------------
    println!("\n  -- Nefarious City, both spellings, measured from the level's own zone refs --");
    println!(
        "  {:<40} {:<20} {:>6} {:>9} {:<22} content centre / core span",
        "region", "type", "zones", "instances", "parent"
    );
    let mut rows: Vec<usize> = (0..level.regions.len())
        .filter(|i| {
            let n = level.regions[*i].name.to_lowercase();
            (n.contains("nefariouscity") || n.contains("nefarious_city")) && merged[*i].valid()
        })
        .collect();
    rows.sort_by_key(|i| std::cmp::Reverse(merged[*i].instances));
    for i in rows.iter().take(14) {
        let r = &level.regions[*i];
        let s = merged[*i];
        let parent = (r.parent_index >= 0)
            .then(|| level.regions.get(r.parent_index as usize))
            .flatten()
            .map(|p| short(&p.name).trim_end_matches(".region").to_string())
            .unwrap_or_else(|| "(root)".into());
        println!(
            "  {:<40} {:<20} {:>6} {:>9} {:<22} {:>7.0},{:>6.0},{:>7.0}   {:>6.0} x {:>6.0} x {:>6.0}",
            trunc(short(&r.name), 40),
            r.type_name,
            s.zones,
            s.instances,
            trunc(&parent, 22),
            s.dense[0],
            s.dense[1],
            s.dense[2],
            s.core.span()[0],
            s.core.span()[1],
            s.core.span()[2],
        );
    }
    // The open tile grid against the instance hub: do they cover the same ground?
    let find = |needle: &str, ty: &str| -> Option<usize> {
        level
            .regions
            .iter()
            .position(|r| short(&r.name).to_lowercase() == needle && r.type_name == ty)
    };
    if let (Some(open), Some(inst)) = (
        find("nefarious_city.region", "kOpenContainer"),
        find("nefariouscity.region", "kInstanceUnit"),
    ) {
        let a = merged[open];
        let b = merged[inst];
        println!(
            "  open-area tile grid content centre {:.0},{:.0},{:.0}; instance hub {:.0},{:.0},{:.0}; {:.0} m apart",
            a.dense[0],
            a.dense[1],
            a.dense[2],
            b.dense[0],
            b.dense[1],
            b.dense[2],
            ((a.dense[0] - b.dense[0]).powi(2) + (a.dense[2] - b.dense[2]).powi(2)).sqrt()
        );
        println!(
            "  their core boxes overlap over {:.0}% of the instance hub's footprint",
            b.core.xz_overlap_fraction(&a.core) * 100.0
        );
    }

    // -- named links -------------------------------------------------------
    println!("\n  -- named links: spawn points in their own region's space --");
    let mut inside = 0usize;
    let mut outside = 0usize;
    let mut load_region_matches_owner = 0usize;
    let mut samples: Vec<String> = Vec::new();
    for (ri, r) in level.regions.iter().enumerate() {
        let s = merged[ri];
        for l in level.region_named_links(r) {
            if l.load_region_index as usize == ri {
                load_region_matches_owner += 1;
            }
            if !s.valid() {
                continue;
            }
            let pad = 150.0f32;
            let within = (0..3)
                .all(|k| l.position[k] >= s.core.min[k] - pad && l.position[k] <= s.core.max[k] + pad);
            if within {
                inside += 1;
            } else {
                outside += 1;
                if samples.len() < 6 {
                    samples.push(format!(
                        "      {:<30} link {:<26} at {:.0},{:.0},{:.0}  core {}",
                        trunc(short(&r.name), 30),
                        trunc(l.short_name(), 26),
                        l.position[0],
                        l.position[1],
                        l.position[2],
                        s.core.fmt()
                    ));
                }
            }
        }
    }
    println!(
        "  {inside} links land inside their owning region's core box (+/-150 m), {outside} do not"
    );
    println!(
        "  {load_region_matches_owner} of {} links name their own owning region as the region to load",
        level.named_links.len()
    );
    for s in &samples {
        println!("{s}");
    }

    // -- the open-unit LOD reference point --------------------------------
    // An open unit's LOD point is where the container decides to stream it in.
    // If it lands on the unit's own content, open-unit coordinates are world
    // coordinates — the same claim the map boxes make for the containers.
    let mut lod_err: Vec<f32> = Vec::new();
    for (i, r) in level.regions.iter().enumerate() {
        let Some(lod) = level.region_lod(r) else { continue };
        let s = merged[i];
        if !s.valid() {
            continue;
        }
        lod_err.push(
            ((lod.position[0] - s.dense[0]).powi(2) + (lod.position[2] - s.dense[2]).powi(2)).sqrt(),
        );
    }
    if !lod_err.is_empty() {
        lod_err.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "\n  open-unit LOD reference point vs. the unit's own content centre, over {} units:",
            lod_err.len()
        );
        println!(
            "      median {:.0} m, 90th percentile {:.0} m, worst {:.0} m (the lattice step is 128 m)",
            lod_err[lod_err.len() / 2],
            lod_err[lod_err.len() * 9 / 10],
            lod_err[lod_err.len() - 1]
        );
    }

    merged
}

// ---------------------------------------------------------------------------
// 4. hibernation, from the level alone
// ---------------------------------------------------------------------------

fn section_4_hibernate(level: &LevelAsset, region_boxes: &[RegionSpace]) {
    println!("\n== 4. hibernation ==");
    println!(
        "  {} descriptors / containers, {} model names, {} vfx names, {} instances in {} clusters",
        level.hibernate_refs.len(),
        level.hibernate_model_names.len(),
        level.hibernate_vfx_names.len(),
        level.hibernate_object_count(),
        level
            .hibernate_zones
            .iter()
            .map(|z| z.model_clusters.len() + z.effect_clusters.len())
            .sum::<usize>(),
    );

    // Does a container name the same zone its descriptor points at?
    let mut id_match = 0usize;
    let mut id_mismatch: Vec<String> = Vec::new();
    for h in &level.hibernate_zones {
        let row = level.zones.get(h.zone_index.max(0) as usize);
        match row {
            Some(z) if z.asset_id == h.zone_asset_id => id_match += 1,
            Some(z) => {
                if id_mismatch.len() < 4 {
                    id_mismatch.push(format!(
                        "      zone_index {} -> {:016X} but container says {:016X} ({})",
                        h.zone_index,
                        z.asset_id,
                        h.zone_asset_id,
                        short(&z.name)
                    ));
                }
            }
            None => {
                if id_mismatch.len() < 4 {
                    id_mismatch.push(format!("      zone_index {} out of range", h.zone_index));
                }
            }
        }
    }
    println!(
        "  {id_match} of {} containers agree with their descriptor's zone row on the zone asset id",
        level.hibernate_zones.len()
    );
    for m in &id_mismatch {
        println!("{m}");
    }
    // Each zone appears at most once, and the model names are real assets.
    let distinct: BTreeSet<i32> = level.hibernate_zones.iter().map(|h| h.zone_index).collect();
    println!(
        "  {} distinct zone rows carry hibernate data ({} descriptors), so a zone owns at most one container",
        distinct.len(),
        level.hibernate_refs.len()
    );

    // Which regions own the hibernating zones?
    let mut zone_to_regions: HashMap<i16, Vec<usize>> = HashMap::new();
    for (ri, r) in level.regions.iter().enumerate() {
        for zi in level.region_zone_indices(r) {
            zone_to_regions.entry(*zi).or_default().push(ri);
        }
    }
    let mut secondary: HashMap<i16, Vec<usize>> = HashMap::new();
    for (ri, r) in level.regions.iter().enumerate() {
        for zi in level.region_secondary_zone_indices(r) {
            secondary.entry(*zi).or_default().push(ri);
        }
    }
    let mut owner_types: BTreeMap<&str, usize> = BTreeMap::new();
    let mut as_secondary = 0usize;
    let mut orphan = 0usize;
    let mut per_region: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
    for h in &level.hibernate_zones {
        let zi = h.zone_index as i16;
        let owners = zone_to_regions.get(&zi);
        match owners {
            Some(list) => {
                for ri in list {
                    *owner_types
                        .entry(level.regions[*ri].type_name)
                        .or_default() += 1;
                    let e = per_region.entry(*ri).or_insert((0, 0));
                    e.0 += 1;
                    e.1 += h.model_objects.len() + h.effect_objects.len();
                }
            }
            None => orphan += 1,
        }
        if secondary.contains_key(&zi) {
            as_secondary += 1;
        }
    }
    println!("  region types that own a hibernating zone (by primary zone ref): {owner_types:?}");
    println!(
        "  hibernating zones that are also some region's secondary (low-LOD / shadow) ref: {as_secondary}; owned by no region: {orphan}"
    );
    let mut top: Vec<_> = per_region.iter().collect();
    top.sort_by_key(|(_, (_, inst))| std::cmp::Reverse(*inst));
    println!("  regions with the most hibernated instances:");
    for (ri, (zones, inst)) in top.iter().take(8) {
        let r = &level.regions[**ri];
        println!(
            "      {inst:>7} instances over {zones:>4} zones  {:<40} [{}]",
            trunc(short(&r.name), 40),
            r.type_name
        );
    }

    // Do the instances sit where their region's zones sit?
    let mut inside = 0usize;
    let mut total = 0usize;
    for h in &level.hibernate_zones {
        let zi = h.zone_index as i16;
        let Some(owners) = zone_to_regions.get(&zi) else { continue };
        let Some(ri) = owners.first() else { continue };
        let Some(bb) = region_boxes.get(*ri) else { continue };
        if !bb.valid() {
            continue;
        }
        for o in h.model_objects.iter().chain(h.effect_objects.iter()) {
            total += 1;
            let pad = 250.0f32;
            if (0..3)
                .all(|k| o.position[k] >= bb.core.min[k] - pad && o.position[k] <= bb.core.max[k] + pad)
            {
                inside += 1;
            }
        }
    }
    if total > 0 {
        println!(
            "  {inside} of {total} instances fall inside their owning region's core box (+/-250 m) = {:.1}%",
            100.0 * inside as f64 / total as f64
        );
    }

    // Cluster geometry: how a viewer would cull them.
    let clusters: Vec<&la::HibernateCluster> = level
        .hibernate_zones
        .iter()
        .flat_map(|z| z.model_clusters.iter().chain(z.effect_clusters.iter()))
        .collect();
    let mut draw: BTreeMap<i64, usize> = BTreeMap::new();
    let mut radius_sum = 0.0f64;
    let mut count_sum = 0usize;
    let mut visible_equal = 0usize;
    for c in &clusters {
        *draw.entry(c.draw_distance as i64).or_default() += 1;
        radius_sum += c.radius as f64;
        count_sum += c.object_count as usize;
        if c.visible_count == c.object_count {
            visible_equal += 1;
        }
    }
    println!(
        "  {} clusters: mean radius {:.1} m, mean {:.1} instances each",
        clusters.len(),
        radius_sum / clusters.len() as f64,
        count_sum as f64 / clusters.len() as f64,
    );
    println!("  draw_distance values (metres -> clusters): {draw:?}");
    println!(
        "  clusters whose visible_count equals object_count: {visible_equal} of {}",
        clusters.len()
    );
    let per_container_clusters: Vec<usize> = level
        .hibernate_zones
        .iter()
        .map(|z| z.model_clusters.len())
        .collect();
    println!(
        "  clusters per container: min {} max {} mean {:.1}",
        per_container_clusters.iter().min().unwrap_or(&0),
        per_container_clusters.iter().max().unwrap_or(&0),
        per_container_clusters.iter().sum::<usize>() as f64 / per_container_clusters.len() as f64
    );

    // Model reuse: what a viewer has to load.
    let mut used: BTreeMap<i16, usize> = BTreeMap::new();
    for h in &level.hibernate_zones {
        for o in &h.model_objects {
            *used.entry(o.asset_index).or_default() += 1;
        }
    }
    let mut mv: Vec<_> = used.iter().collect();
    mv.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    println!(
        "  {} of {} hibernate model names are used; busiest:",
        used.len(),
        level.hibernate_model_names.len()
    );
    for (i, n) in mv.iter().take(6) {
        println!(
            "      {n:>6}  {}",
            level
                .hibernate_model_names
                .get((**i).max(0) as usize)
                .map(|s| short(s))
                .unwrap_or("?")
        );
    }
    let missing = used
        .keys()
        .filter(|i| level.hibernate_model_names.get((**i).max(0) as usize).is_none())
        .count();
    println!("  instances whose asset_index has no name row: {missing}");

    println!("\n  what a viewer needs to draw these:");
    println!("    * the model asset named by asset_index -> Level Hibernate Model Names, whose asset");
    println!("      id is the crc64 of that path — the same models the zone viewer already loads, and");
    println!("      only 353 distinct ones for the whole planet;");
    println!("    * HibernateObject::matrix(), which is in the same world space and the same row-major");
    println!("      layout as a zone scene object's, so both go through one instanced draw;");
    println!("    * the per-instance tint from rgba (sqrt-encoded, squared back to linear) and the");
    println!("      visible / long-range flags;");
    println!("    * the owning cluster's centre, radius and draw distance if it wants ");
    println!("      culling: a cluster is a contiguous run of instances with one bounding sphere.");
    let with_over = level
        .hibernate_zones
        .iter()
        .flat_map(|z| z.model_objects.iter().chain(z.effect_objects.iter()))
        .filter(|o| o.mat_override_count > 0)
        .count();
    let over_containers = level
        .hibernate_zones
        .iter()
        .filter(|z| z.mat_override_count > 0)
        .count();
    println!(
        "    Still open: {with_over} of {} instances point into their container's material-override array",
        level.hibernate_object_count()
    );
    println!(
        "    ({over_containers} of {} containers declare one) and that array is not decoded yet, so a",
        level.hibernate_zones.len()
    );
    println!("    viewer drawing those instances uses the model's own materials. There are no");
    println!("    visual-effect instances in this level at all, so that half of every container is idle.");
}

// ---------------------------------------------------------------------------
// 5. hibernate instances vs the zone they came from
// ---------------------------------------------------------------------------

/// Candidate Euler orders. `Rx*Ry*Rz` means the matrix that rotates a row
/// vector is applied X first, and the composition is written in that order.
const ORDERS: [(&str, [usize; 3]); 6] = [
    ("XYZ", [0, 1, 2]),
    ("XZY", [0, 2, 1]),
    ("YXZ", [1, 0, 2]),
    ("YZX", [1, 2, 0]),
    ("ZXY", [2, 0, 1]),
    ("ZYX", [2, 1, 0]),
];

fn axis_matrix(axis: usize, a: f32) -> [[f32; 3]; 3] {
    let (s, c) = a.sin_cos();
    match axis {
        0 => [[1.0, 0.0, 0.0], [0.0, c, s], [0.0, -s, c]],
        1 => [[c, 0.0, -s], [0.0, 1.0, 0.0], [s, 0.0, c]],
        _ => [[c, s, 0.0], [-s, c, 0.0], [0.0, 0.0, 1.0]],
    }
}

fn mat_mul(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut o = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            o[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    o
}

/// The basis a zone scene object's row-major matrix carries, with the scale
/// divided out.
fn basis_of(m: &[f32; 16]) -> Option<[[f32; 3]; 3]> {
    let mut out = [[0.0f32; 3]; 3];
    for r in 0..3 {
        let row = [m[r * 4], m[r * 4 + 1], m[r * 4 + 2]];
        let len = (row[0] * row[0] + row[1] * row[1] + row[2] * row[2]).sqrt();
        if !len.is_finite() || len < 1e-4 {
            return None;
        }
        for c in 0..3 {
            out[r][c] = row[c] / len;
        }
    }
    Some(out)
}

fn basis_error(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> f32 {
    let mut e = 0.0f32;
    for i in 0..3 {
        for j in 0..3 {
            e += (a[i][j] - b[i][j]).abs();
        }
    }
    e / 9.0
}

fn transpose(m: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut o = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            o[i][j] = m[j][i];
        }
    }
    o
}

fn det(m: [[f32; 3]; 3]) -> f32 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

/// Every composition an "three Euler angles" record could mean: the six
/// orders, each with the rotation written for row vectors or for column
/// vectors, and with the angles taken as stored or negated. 24 candidates, and
/// only the right one reproduces an authored basis.
fn euler_variants() -> Vec<(String, usize, bool, bool)> {
    let mut out = Vec::new();
    for (oi, (name, _)) in ORDERS.iter().enumerate() {
        for &tr in &[false, true] {
            for &neg in &[false, true] {
                out.push((
                    format!(
                        "{name}{}{}",
                        if tr { "^T" } else { "  " },
                        if neg { " -a" } else { "   " }
                    ),
                    oi,
                    tr,
                    neg,
                ));
            }
        }
    }
    out
}

fn euler_basis(rot: [f32; 3], oi: usize, tr: bool, neg: bool) -> [[f32; 3]; 3] {
    let axes = ORDERS[oi].1;
    let s = if neg { -1.0 } else { 1.0 };
    let m = mat_mul(
        mat_mul(
            axis_matrix(axes[0], s * rot[axes[0]]),
            axis_matrix(axes[1], s * rot[axes[1]]),
        ),
        axis_matrix(axes[2], s * rot[axes[2]]),
    );
    if tr {
        transpose(m)
    } else {
        m
    }
}

/// The engine CRC-32 over raw bytes. `core::crc32::hash` takes a string; the
/// uid hypotheses need the same polynomial over eight bytes.
fn crc32_bytes(data: &[u8]) -> u32 {
    const POLY: u32 = 0xEDB8_8320;
    let mut crc: u32 = 0xEDB8_8320;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ POLY
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// `parent_id` is a 32-bit fold of the zone instance's 64-bit uid: the low word
/// exclusive-ored with the high word. This is the library's own function, so the
/// hypothesis table below tests what the library actually does.
fn fold_uid(uid: u64) -> u32 {
    la::fold_instance_uid(uid)
}

/// One hibernate instance matched to the zone scene object it came from.
struct Pair {
    /// What `HibernateObject::matrix()` produces for this record.
    built: [f32; 16],
    /// The zone's own authored matrix.
    authored: [f32; 16],
    hib_pos: [f32; 3],
    zone_pos: [f32; 3],
    rotation: [f32; 3],
    hib_scale: [f32; 3],
    /// Row lengths of the authored matrix — the scale the zone actually uses.
    row_scale: [f32; 3],
    /// Separate `scale` word of the scene-object entry (+0x70).
    entry_scale: [f32; 3],
    basis: [[f32; 3]; 3],
    mirrored_flag: bool,
    negative_det: bool,
    median_axis: usize,
    uniform: bool,
    same_model: bool,
    parent_id: u32,
    uid: u64,
    entry_offset: u32,
}

fn section_5_hibernate_vs_zone(game: &str, level: &LevelAsset) {
    println!("\n== 5. hibernate instances against the zones they came from ==");
    let idx = level::get_index(game, None).expect("zone index");

    // The busiest containers plus a spread of smaller ones, so nothing here is
    // an artefact of one zone.
    let mut order: Vec<usize> = (0..level.hibernate_zones.len()).collect();
    order.sort_by_key(|i| {
        std::cmp::Reverse(
            level.hibernate_zones[*i].model_objects.len()
                + level.hibernate_zones[*i].effect_objects.len(),
        )
    });
    let sample: Vec<usize> = order
        .iter()
        .take(12)
        .copied()
        .chain(order.iter().skip(12).step_by(17).copied().take(40))
        .collect();

    struct Stat {
        objects: usize,
        by_fold: usize,
        by_position: usize,
        fold_collisions: usize,
        zone_model_insts: usize,
        pairs: Vec<Pair>,
    }

    let stats: Vec<(usize, Option<Stat>)> = sample
        .par_iter()
        .map(|hi| {
            let h = &level.hibernate_zones[*hi];
            let Some(zrow) = level.zones.get(h.zone_index.max(0) as usize) else {
                return (*hi, None);
            };
            let Ok(raw) = idx.extract(zrow.asset_id) else {
                return (*hi, None);
            };
            let Ok(zone) = Zone::parse(&raw) else {
                return (*hi, None);
            };
            // Two independent indexes over the zone's placed models: by the
            // folded instance uid, and by (model, metre). Matching both ways and
            // comparing the answers is what makes the decode claims checkable.
            let mut by_fold: HashMap<u32, Vec<usize>> = HashMap::new();
            let mut by_key: HashMap<(u64, i32, i32, i32), usize> = HashMap::new();
            for (i, o) in zone.scene_objects.iter().enumerate() {
                if o.model_id == 0 {
                    continue;
                }
                by_fold.entry(fold_uid(o.instance_id)).or_default().push(i);
                let m = &o.matrix;
                by_key.insert(
                    (
                        o.model_id,
                        m[12].round() as i32,
                        m[13].round() as i32,
                        m[14].round() as i32,
                    ),
                    i,
                );
            }
            let mut st = Stat {
                objects: 0,
                by_fold: 0,
                by_position: 0,
                fold_collisions: by_fold.values().filter(|v| v.len() > 1).count(),
                zone_model_insts: by_key.len(),
                pairs: Vec::new(),
            };
            for o in &h.model_objects {
                st.objects += 1;
                let model_id = level
                    .hibernate_model_names
                    .get(o.asset_index.max(0) as usize)
                    .map(|n| crc64::hash(n))
                    .unwrap_or(0);
                // Match on the folded uid first: it is exact and does not care
                // about the half-float position.
                let by_uid = by_fold.get(&o.parent_id).and_then(|v| v.first().copied());
                if by_uid.is_some() {
                    st.by_fold += 1;
                }
                // ...and independently on (model, metre), with a metre of slack.
                let mut by_pos = None;
                for dx in -1i32..=1 {
                    for dy in -1i32..=1 {
                        for dz in -1i32..=1 {
                            if by_pos.is_some() {
                                continue;
                            }
                            let key = (
                                model_id,
                                o.position[0].round() as i32 + dx,
                                o.position[1].round() as i32 + dy,
                                o.position[2].round() as i32 + dz,
                            );
                            if let Some(i) = by_key.get(&key) {
                                by_pos = Some(*i);
                            }
                        }
                    }
                }
                if by_pos.is_some() {
                    st.by_position += 1;
                }
                let Some(si) = by_uid.or(by_pos) else { continue };
                let so = &zone.scene_objects[si];
                let m = &so.matrix;
                let row_len = |r: usize| {
                    (m[r * 4] * m[r * 4]
                        + m[r * 4 + 1] * m[r * 4 + 1]
                        + m[r * 4 + 2] * m[r * 4 + 2])
                        .sqrt()
                };
                if let Some(basis) = basis_of(m) {
                    st.pairs.push(Pair {
                        built: o.matrix(),
                        authored: *m,
                        hib_pos: o.position,
                        zone_pos: [m[12], m[13], m[14]],
                        rotation: o.rotation,
                        hib_scale: o.scale,
                        row_scale: [row_len(0), row_len(1), row_len(2)],
                        entry_scale: so.scale,
                        basis,
                        mirrored_flag: o.mirrored(),
                        negative_det: det(basis) < 0.0,
                        median_axis: o.median_scale_axis(),
                        uniform: o.uniform_scale(),
                        same_model: so.model_id == model_id,
                        parent_id: o.parent_id,
                        uid: so.instance_id,
                        entry_offset: so.offset,
                    });
                }
            }
            (*hi, Some(st))
        })
        .collect();

    let mut objects = 0usize;
    let mut by_fold_total = 0usize;
    let mut by_pos_total = 0usize;
    let mut collisions = 0usize;
    let mut zone_insts = 0usize;
    let mut pairs: Vec<&Pair> = Vec::new();
    let mut zones_done = 0usize;
    println!(
        "  {:<44} {:>7} {:>10} {:>10} {:>10}",
        "zone", "hib", "by uid", "by position", "zone insts"
    );
    for (hi, st) in &stats {
        let Some(st) = st else {
            println!("      container {hi}: zone asset missing from the PC build");
            continue;
        };
        zones_done += 1;
        let h = &level.hibernate_zones[*hi];
        let name = level
            .zones
            .get(h.zone_index.max(0) as usize)
            .map(|z| short(&z.name).to_string())
            .unwrap_or_default();
        if st.objects >= 150 {
            println!(
                "  {:<44} {:>7} {:>10} {:>10} {:>10}",
                trunc(&name, 44),
                st.objects,
                st.by_fold,
                st.by_position,
                st.zone_model_insts
            );
        }
        objects += st.objects;
        by_fold_total += st.by_fold;
        by_pos_total += st.by_position;
        collisions += st.fold_collisions;
        zone_insts += st.zone_model_insts;
        pairs.extend(st.pairs.iter());
    }
    let pct = |n: usize| 100.0 * n as f64 / objects.max(1) as f64;
    println!(
        "  totals over {zones_done} zones: {objects} hibernate instances; {by_fold_total} ({:.1}%) resolve through parent_id == (uid low ^ uid high); {by_pos_total} ({:.1}%) resolve on (model asset, metre)",
        pct(by_fold_total),
        pct(by_pos_total)
    );
    println!(
        "  those zones hold {zone_insts} placed model instances, so hibernation covers {:.1}% of them; {collisions} folded uids collide inside a zone",
        100.0 * by_fold_total as f64 / zone_insts.max(1) as f64
    );
    let same = pairs.iter().filter(|p| p.same_model).count();
    println!(
        "  {same} of {} matched pairs agree on the model asset as well",
        pairs.len()
    );

    // -- position ----------------------------------------------------------
    let mut perr: Vec<f32> = pairs
        .iter()
        .map(|p| {
            ((p.hib_pos[0] - p.zone_pos[0]).powi(2)
                + (p.hib_pos[1] - p.zone_pos[1]).powi(2)
                + (p.hib_pos[2] - p.zone_pos[2]).powi(2))
            .sqrt()
        })
        .collect();
    perr.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if !perr.is_empty() {
        println!(
            "\n  position: cluster centre + half-float offset vs. the zone's own translation — median {:.3} m, 90th {:.3} m, 99th {:.3} m, worst {:.2} m",
            perr[perr.len() / 2],
            perr[perr.len() * 9 / 10],
            perr[perr.len() * 99 / 100],
            perr[perr.len() - 1]
        );
        println!("  A hibernate record is drawn while its zone is unloaded, so its position is a world");
        println!("  position by definition. Agreeing with the zone's authored matrix to a fraction of");
        println!("  a metre is the proof that a zone's coordinates ARE world coordinates — for an");
        println!("  instance region exactly as for an open-world tile. The placement transform is");
        println!("  identity, and there is no other transform anywhere in the level.");
    }

    // -- the library's own matrix ------------------------------------------
    // The one check that matters for a viewer: what `HibernateObject::matrix()`
    // returns, element by element, against the matrix the zone authored.
    let mut merr: Vec<f32> = pairs
        .iter()
        .map(|p| {
            let mut e = 0.0f32;
            for r in 0..3 {
                for c in 0..3 {
                    e = e.max((p.built[r * 4 + c] - p.authored[r * 4 + c]).abs());
                }
            }
            e
        })
        .collect();
    merr.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if !merr.is_empty() {
        let tol = 0.02f32;
        let under = merr.iter().filter(|e| **e < tol).count();
        println!(
            "\n  HibernateObject::matrix() vs. the authored 3x3, worst element per pair: median {:.4}, 90th {:.4}, worst {:.3}; {:.1}% of pairs inside {tol}",
            merr[merr.len() / 2],
            merr[merr.len() * 9 / 10],
            merr[merr.len() - 1],
            100.0 * under as f64 / merr.len() as f64
        );
        if under * 100 < merr.len() * 90 {
            println!("  FAIL the library's matrix does not reproduce the authored one");
        }
        // Which pairs miss, and what is different about them.
        let worst_of = |p: &Pair| {
            let mut e = 0.0f32;
            for r in 0..3 {
                for c in 0..3 {
                    e = e.max((p.built[r * 4 + c] - p.authored[r * 4 + c]).abs());
                }
            }
            e
        };
        let bad: Vec<&&Pair> = pairs.iter().filter(|p| worst_of(p) >= tol).collect();
        let bad_mirror = bad.iter().filter(|p| p.mirrored_flag).count();
        let bad_scale = bad
            .iter()
            .filter(|p| {
                (0..3).any(|k| {
                    (p.hib_scale[k] - p.row_scale[k]).abs()
                        > 0.02 * p.row_scale[k].abs().max(1.0)
                })
            })
            .count();
        println!(
            "      {} pairs miss: {bad_mirror} of them mirrored, {bad_scale} of them disagree on scale",
            bad.len()
        );
        for p in bad.iter().take(6) {
            println!(
                "      miss: rot {:?} hib scale {:?} row scale {:?} entry scale {:?} mirror {} uniform {}",
                p.rotation.map(|v| (v * 1000.0).round() / 1000.0),
                p.hib_scale.map(|v| (v * 1000.0).round() / 1000.0),
                p.row_scale.map(|v| (v * 1000.0).round() / 1000.0),
                p.entry_scale.map(|v| (v * 1000.0).round() / 1000.0),
                p.mirrored_flag,
                p.uniform
            );
        }
    }

    // -- scale -------------------------------------------------------------
    let close = |a: f32, b: f32| (a - b).abs() <= 0.02 * a.abs().max(b.abs()).max(1.0);
    let row_agree = pairs
        .iter()
        .filter(|p| (0..3).all(|k| close(p.hib_scale[k], p.row_scale[k])))
        .count();
    let entry_agree = pairs
        .iter()
        .filter(|p| (0..3).all(|k| close(p.hib_scale[k], p.entry_scale[k])))
        .count();
    let uniform_ok = pairs
        .iter()
        .filter(|p| {
            p.uniform
                == (close(p.hib_scale[0], p.hib_scale[1]) && close(p.hib_scale[1], p.hib_scale[2]))
        })
        .count();
    let median_ok = pairs
        .iter()
        .filter(|p| {
            let s = p.hib_scale;
            let mut ord = [0usize, 1, 2];
            ord.sort_by(|a, b| s[*a].partial_cmp(&s[*b]).unwrap());
            ord[1] == p.median_axis
        })
        .count();
    let abs_agree = pairs
        .iter()
        .filter(|p| (0..3).all(|k| close(p.hib_scale[k].abs(), p.row_scale[k])))
        .count();
    let all_negative = pairs
        .iter()
        .filter(|p| p.hib_scale.iter().all(|s| *s < 0.0))
        .count();
    let some_negative = pairs
        .iter()
        .filter(|p| p.hib_scale.iter().any(|s| *s < 0.0))
        .count();
    println!(
        "\n  scale: matches the authored matrix's row lengths on {row_agree} of {} pairs signed, {abs_agree} in magnitude; the entry's own scale word on {entry_agree}",
        pairs.len()
    );
    println!(
        "  {some_negative} pairs store a negative scale axis and {all_negative} store all three negative — a mirrored instance is a"
    );
    println!("  point reflection carried in the sign of the scale triple, which is why bit 2 and a");
    println!("  negative determinant agree exactly and why no separate mirror step is needed.");
    println!(
        "  transform bits: bit 4 (uniform scale) agrees with the decoded triple on {uniform_ok}; bits 0-1 name the median-scale axis on {median_ok}"
    );

    // -- the rotation encoding --------------------------------------------
    let usable: Vec<&&Pair> = pairs.iter().filter(|p| !p.negative_det).collect();
    println!(
        "\n  rotation: {} of {} matched pairs have a right-handed authored basis",
        usable.len(),
        pairs.len()
    );
    let variants = euler_variants();
    let mut scored: Vec<(String, f64, f64, usize)> = variants
        .par_iter()
        .map(|(name, oi, tr, neg)| {
            let mut errs: Vec<f32> = Vec::with_capacity(usable.len());
            for p in &usable {
                errs.push(basis_error(euler_basis(p.rotation, *oi, *tr, *neg), p.basis));
            }
            let mean = errs.iter().map(|e| *e as f64).sum::<f64>() / errs.len().max(1) as f64;
            errs.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let median = errs.get(errs.len() / 2).copied().unwrap_or(0.0) as f64;
            let under = errs.iter().filter(|e| **e < 0.02).count();
            (name.clone(), mean, median, under)
        })
        .collect();
    scored.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap());
    println!(
        "  {:<14} {:>10} {:>10} {:>16}",
        "variant", "mean err", "median", "pairs under 0.02"
    );
    for (name, mean, median, under) in scored.iter().take(6) {
        println!(
            "  {name:<14} {mean:>10.4} {median:>10.4} {:>15.1}%",
            100.0 * *under as f64 / usable.len().max(1) as f64
        );
    }
    println!(
        "  best: {}. Read for row vectors that is basis = Rx(rx) . Ry(ry) . Rz(rz); the runner-up",
        scored[0].0.trim()
    );
    println!("  is the same rotation written for column vectors, so the two cannot be told apart.");

    // -- mirrored instances ------------------------------------------------
    // A mirrored instance cannot be a pure rotation. Find what the engine does
    // with the mirror: negate one scale axis, and if so which.
    let mirrored: Vec<&&Pair> = pairs.iter().filter(|p| p.negative_det).collect();
    println!(
        "\n  mirroring: {} of {} pairs have a negative-determinant authored basis; the instance's",
        mirrored.len(),
        pairs.len()
    );
    let flag_agree = pairs
        .iter()
        .filter(|p| p.mirrored_flag == p.negative_det)
        .count();
    println!("  bit-2 mirrored flag agrees with that on {flag_agree} of {}", pairs.len());
    if !mirrored.is_empty() {
        let (oi, tr, neg) = variants
            .iter()
            .find(|(n, ..)| *n == scored[0].0)
            .map(|(_, a, b, c)| (*a, *b, *c))
            .unwrap();
        println!(
            "  {:<28} {:>12} {:>18}",
            "mirror applied as", "median err", "pairs under 0.02"
        );
        // Try negating each axis on either side of the rotation, plus the whole
        // basis, and see which reproduces the authored matrix.
        for (label, pre, axis) in [
            ("negate row x, then rotate", true, 0usize),
            ("negate row y, then rotate", true, 1),
            ("negate row z, then rotate", true, 2),
            ("rotate, then negate col x", false, 0),
            ("rotate, then negate col y", false, 1),
            ("rotate, then negate col z", false, 2),
        ] {
            let mut errs: Vec<f32> = Vec::new();
            for p in &mirrored {
                let r = euler_basis(p.rotation, oi, tr, neg);
                let mut m = r;
                if pre {
                    for j in 0..3 {
                        m[axis][j] = -m[axis][j];
                    }
                } else {
                    for i in 0..3 {
                        m[i][axis] = -m[i][axis];
                    }
                }
                errs.push(basis_error(m, p.basis));
            }
            errs.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let under = errs.iter().filter(|e| **e < 0.02).count();
            println!(
                "  {label:<28} {:>12.4} {:>17.1}%",
                errs[errs.len() / 2],
                100.0 * under as f64 / errs.len() as f64
            );
        }
        println!("  (a mirrored instance's own scale triple is positive, so the sign has to come from");
        println!("  the flag — an editor that ignores bit 2 draws those models inside out)");

        // What is left over: B = R . X, so X = R^T . B. Rounding X to integers
        // and counting the patterns says what the mirror actually is.
        let mut patterns: BTreeMap<String, usize> = BTreeMap::new();
        let mut patterns_other: BTreeMap<String, usize> = BTreeMap::new();
        for p in &mirrored {
            let r = euler_basis(p.rotation, oi, tr, neg);
            let x = mat_mul(transpose(r), p.basis);
            let y = mat_mul(p.basis, transpose(r));
            let fmt3 = |m: [[f32; 3]; 3]| {
                let mut s = String::new();
                for i in 0..3 {
                    for j in 0..3 {
                        s.push_str(&format!("{:>3}", m[i][j].round() as i32));
                    }
                    if i < 2 {
                        s.push('|');
                    }
                }
                s
            };
            *patterns.entry(fmt3(x)).or_default() += 1;
            *patterns_other.entry(fmt3(y)).or_default() += 1;
        }
        let mut top: Vec<_> = patterns.iter().collect();
        top.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        println!("  residual R^T . B, rounded, for the mirrored pairs (top patterns):");
        for (k, n) in top.iter().take(5) {
            println!("      {n:>6}  [{k}]");
        }
        let mut top2: Vec<_> = patterns_other.iter().collect();
        top2.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        println!("  residual B . R^T, rounded (top patterns):");
        for (k, n) in top2.iter().take(5) {
            println!("      {n:>6}  [{k}]");
        }

        // Does the stored Euler triple describe the basis with its mirror
        // divided out — i.e. is |B| (each row unit, sign stripped per axis) the
        // rotation? Test the three single-axis reflections applied to B itself.
        println!("  a mirrored basis with one axis reflected out, against the stored Euler:");
        for axis in 0..3 {
            let mut errs: Vec<f32> = Vec::new();
            for p in &mirrored {
                let mut b = p.basis;
                for j in 0..3 {
                    b[axis][j] = -b[axis][j];
                }
                errs.push(basis_error(euler_basis(p.rotation, oi, tr, neg), b));
            }
            errs.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let under = errs.iter().filter(|e| **e < 0.02).count();
            println!(
                "      reflect row {axis}: median {:.4}, {:.1}% under 0.02",
                errs[errs.len() / 2],
                100.0 * under as f64 / errs.len() as f64
            );
        }
        for axis in 0..3 {
            let mut errs: Vec<f32> = Vec::new();
            for p in &mirrored {
                let mut b = p.basis;
                for i in 0..3 {
                    b[i][axis] = -b[i][axis];
                }
                errs.push(basis_error(euler_basis(p.rotation, oi, tr, neg), b));
            }
            errs.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let under = errs.iter().filter(|e| **e < 0.02).count();
            println!(
                "      reflect column {axis}: median {:.4}, {:.1}% under 0.02",
                errs[errs.len() / 2],
                100.0 * under as f64 / errs.len() as f64
            );
        }
        // Negating all three axes (a point reflection) is its own case.
        let mut errs: Vec<f32> = Vec::new();
        for p in &mirrored {
            let mut b = p.basis;
            for i in 0..3 {
                for j in 0..3 {
                    b[i][j] = -b[i][j];
                }
            }
            errs.push(basis_error(euler_basis(p.rotation, oi, tr, neg), b));
        }
        errs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "      negate every axis: median {:.4}, {:.1}% under 0.02",
            errs[errs.len() / 2],
            100.0 * errs.iter().filter(|e| **e < 0.02).count() as f64 / errs.len() as f64
        );
    }

    // Which scale triple do bits 0-1 rank? Try the authored matrix's row
    // lengths as well as the hibernate triple.
    let median_row_ok = pairs
        .iter()
        .filter(|p| {
            let s = p.row_scale;
            let mut ord = [0usize, 1, 2];
            ord.sort_by(|a, b| s[*a].partial_cmp(&s[*b]).unwrap());
            ord[1] == p.median_axis
        })
        .count();
    let largest_ok = pairs
        .iter()
        .filter(|p| {
            let s = p.hib_scale;
            let mut ord = [0usize, 1, 2];
            ord.sort_by(|a, b| s[*a].partial_cmp(&s[*b]).unwrap());
            ord[2] == p.median_axis
        })
        .count();
    println!(
        "  bits 0-1 vs. the median axis of the authored matrix's row lengths: {median_row_ok} of {}; vs. the largest hibernate axis: {largest_ok}",
        pairs.len()
    );
    let mut bits_dist: BTreeMap<usize, usize> = BTreeMap::new();
    for p in &pairs {
        *bits_dist.entry(p.median_axis).or_default() += 1;
    }
    let uniform_pairs = pairs.iter().filter(|p| p.uniform).count();
    println!(
        "  bits 0-1 values seen: {bits_dist:?} over {} pairs, {uniform_pairs} of which are uniformly scaled;",
        pairs.len()
    );
    // A uniformly scaled instance has no median axis to name, so the test only
    // means anything on the rest.
    let nonuniform: Vec<&&Pair> = pairs.iter().filter(|p| !p.uniform).collect();
    let nu_median = nonuniform
        .iter()
        .filter(|p| {
            let s = p.row_scale;
            let mut ord = [0usize, 1, 2];
            ord.sort_by(|a, b| s[*a].partial_cmp(&s[*b]).unwrap());
            ord[1] == p.median_axis
        })
        .count();
    let nu_max = nonuniform
        .iter()
        .filter(|p| {
            let s = p.row_scale;
            let mut ord = [0usize, 1, 2];
            ord.sort_by(|a, b| s[*a].partial_cmp(&s[*b]).unwrap());
            ord[2] == p.median_axis
        })
        .count();
    println!(
        "  restricted to the {} non-uniformly scaled pairs: median axis {nu_median}, largest axis {nu_max}",
        nonuniform.len()
    );
    println!("  so what those two bits rank is still open — they are not needed to draw an instance.");

    // -- parent_id ---------------------------------------------------------
    println!("\n  parent_id: which function of the zone entry reproduces it?");
    let hyp: Vec<(&str, Box<dyn Fn(&Pair) -> u32>)> = vec![
        ("uid low 32", Box::new(|p: &Pair| (p.uid & 0xFFFF_FFFF) as u32)),
        ("uid high 32", Box::new(|p: &Pair| (p.uid >> 32) as u32)),
        (
            "uid low ^ uid high",
            Box::new(|p: &Pair| ((p.uid & 0xFFFF_FFFF) ^ (p.uid >> 32)) as u32),
        ),
        (
            "crc32 of the uid bytes",
            Box::new(|p: &Pair| crc32_bytes(&p.uid.to_le_bytes())),
        ),
        ("scene-object offset", Box::new(|p: &Pair| p.entry_offset)),
    ];
    for (name, f) in &hyp {
        let hits = pairs.iter().filter(|p| f(p) == p.parent_id).count();
        println!("      {name:<26} {hits:>6} of {}", pairs.len());
    }
    if let Some(p) = pairs.first() {
        println!(
            "      example: parent_id {:08X}, zone uid {:016X} folds to {:08X}, entry offset {:#X}",
            p.parent_id,
            p.uid,
            fold_uid(p.uid),
            p.entry_offset
        );
    }
    println!("  That fold is what an editor needs: it is the only link from a far-LOD record back to");
    println!("  the zone instance it stands in for, so moving or deleting that instance can find and");
    println!("  fix (or flag) its hibernate copy instead of leaving a ghost at distance.");
}

// ---------------------------------------------------------------------------
// 6. what the region tree buys the editor
// ---------------------------------------------------------------------------

fn section_6_what_the_tree_buys(level: &LevelAsset, blocks: &[RegionData]) {
    println!("\n== 6. what the region tree plus game data offers that a path grouping cannot ==");

    // Zones in more than one region: a path grouping has to pick one.
    let mut count: HashMap<i16, usize> = HashMap::new();
    for r in &level.regions {
        for zi in level.region_zone_indices(r) {
            *count.entry(*zi).or_default() += 1;
        }
    }
    let shared = count.values().filter(|n| **n > 1).count();
    println!("  zones owned by more than one region: {shared}");

    // Secondary refs: the low-LOD / shadow pairing a path cannot express.
    let overlay_secondary: usize = level
        .regions
        .iter()
        .filter(|r| r.type_name == "kOverlay")
        .map(|r| level.region_secondary_zone_indices(r).len())
        .sum();
    println!("  overlay shadow-zone entries: {overlay_secondary}");

    // Streaming: what the editor can show per open unit.
    let lods = &level.region_lods;
    let mut settings: BTreeMap<(u32, u32, u32), usize> = BTreeMap::new();
    for l in lods {
        *settings
            .entry((l.tile_radius, l.load_distance, l.unload_distance))
            .or_default() += 1;
    }
    println!("  open-unit streaming settings (tile radius, load, unload): {settings:?}");

    // Game data per region type.
    let mut by_type: BTreeMap<&str, usize> = BTreeMap::new();
    for b in blocks {
        if let Some(r) = b.owner_region().and_then(|i| level.regions.get(i)) {
            *by_type.entry(r.type_name).or_default() += 1;
        }
    }
    println!("  game-data blocks per region type: {by_type:?}");

    // The planet-level grouping the game itself uses.
    let mut planets: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for b in blocks.iter().filter(|b| b.kind == RegionDataKind::Planet) {
        let p = b.planet().unwrap();
        let e = planets.entry(p.planet_name.clone()).or_insert((0, 0));
        e.0 += 1;
        if let Some(r) = b.owner_region().and_then(|i| level.regions.get(i)) {
            e.1 += level.region_zone_indices(r).len();
        }
    }
    println!("  planets named by PlanetRegionData, with region count and zones:");
    for (p, (n, z)) in &planets {
        println!("      {p:<16} {n:>2} regions, {z:>5} zone refs");
    }
    println!(
        "  {} named links across {} regions give the editor the game's own spawn points",
        level.named_links.len(),
        level
            .regions
            .iter()
            .filter(|r| r.named_link_count > 0)
            .count()
    );
}
