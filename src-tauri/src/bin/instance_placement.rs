//! Is there a world placement transform for an instance region?
//!
//!   cargo run --release --bin instance_placement -- "<game dir>" [region substring]
//!
//! The level editor can load zones from any region, but before it lets two
//! regions into one view it has to know whether their coordinates share a
//! space. `zone_bounds` found 385 of 3,597 zones "containing" one world point,
//! which is what you get when many regions are each authored around their own
//! origin. This harness settles the question on the shipped data.
//!
//! It reports four things, in the order they rule a transform out:
//!
//!   1. **Where the bytes could hide one.** `LevelRegionBuilt` is 36 bytes and
//!      every one of them is a known field, so a per-region transform would
//!      have to live in the region's game-data DDL blocks or be implied by a
//!      named link. The harness prints every distinct game-data struct name,
//!      how many regions of each type carry one, and scans each payload for
//!      any float triple that could pass for a world translation.
//!   2. **Where each region's zones actually sit.** The world AABB over a
//!      region's loadable zones, so an instance region that is authored around
//!      zero is visible as such next to an open area that is not.
//!   3. **How much the instance regions overlap each other.** If instance
//!      regions were placed in a shared world they would not all pile onto the
//!      same few hundred metres.
//!   4. **The named links.** A link's world position is the spawn point inside
//!      its own region; the harness prints it next to that region's AABB to
//!      show the link lands inside the region's own space rather than
//!      somewhere in the open world.

use omnitool_lib::core::level;
use omnitool_lib::core::level_asset::{self as la, LevelAsset};
use omnitool_lib::core::toc::Toc;
use omnitool_lib::core::zone::Zone;
use rayon::prelude::*;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

const LEVEL_PATH: &str = "levels/i29/i29.level";

/// Floats outside this envelope cannot be a Rift Apart world position: the
/// biggest open area measures a few thousand metres across.
const WORLD_LIMIT: f32 = 100_000.0;

#[derive(Default, Clone, Copy)]
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
    fn add(&mut self, min: [f32; 3], max: [f32; 3]) {
        for i in 0..3 {
            self.min[i] = self.min[i].min(min[i]);
            self.max[i] = self.max[i].max(max[i]);
        }
        self.n += 1;
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
    /// Horizontal distance of the centre from the world origin — the single
    /// number that says "authored around its own origin" or not.
    fn centre_radius_xz(&self) -> f32 {
        let c = self.centre();
        (c[0] * c[0] + c[2] * c[2]).sqrt()
    }
    fn overlaps_xz(&self, o: &Aabb) -> bool {
        self.min[0] <= o.max[0]
            && o.min[0] <= self.max[0]
            && self.min[2] <= o.max[2]
            && o.min[2] <= self.max[2]
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let game = args
        .first()
        .cloned()
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let want = args.get(1).cloned().unwrap_or_default();
    let game_path = PathBuf::from(&game);

    // -------------------------------------------------------------- level asset
    let toc_bytes = std::fs::read(game_path.join("toc")).expect("read toc");
    let toc = Toc::parse(&toc_bytes).expect("parse toc");
    let assets = toc.assets();
    let by_id: HashMap<u64, _> = assets.iter().map(|a| (a.asset_id, a.clone())).collect();
    let level_id = omnitool_lib::core::crc64::hash(LEVEL_PATH);
    let entry = by_id.get(&level_id).expect("i29.level in the TOC");
    let raw = toc
        .extract_asset(entry, &game_path)
        .expect("extract i29.level");
    let level = LevelAsset::parse(&raw).expect("parse i29.level");

    println!("== {LEVEL_PATH} ==");
    println!(
        "  {} regions, {} zones, {} named links, {} game-data blocks ({} bytes of payload)",
        level.regions.len(),
        level.zones.len(),
        level.named_links.len(),
        level.game_data.len(),
        level.game_data_payload.len()
    );
    for problem in level.validate() {
        println!("  VALIDATE: {problem}");
    }

    section_1_game_data(&level);
    let region_boxes = section_2_region_bounds(&game, &level, &want);
    section_3_instance_overlap(&level, &region_boxes);
    section_4_named_links(&level, &region_boxes);
    section_5_shared_space(&game);
    section_6_search(&game, if want.is_empty() { "nefariouscity" } else { &want });
    verdict(&level, &region_boxes);
}

// ---------------------------------------------------------------------------
// 1. the only bytes that could hold a transform
// ---------------------------------------------------------------------------

fn section_1_game_data(level: &LevelAsset) {
    println!("\n== 1. region game data — could a placement live in a DDL block? ==");

    // Which struct names exist, and which region types carry them.
    let mut by_name: BTreeMap<String, (usize, BTreeSet<&'static str>, u32, u32)> = BTreeMap::new();
    let mut regions_with_data = 0usize;
    for r in &level.regions {
        let rows = level.region_game_data(r);
        if !rows.is_empty() {
            regions_with_data += 1;
        }
        for row in rows {
            let e = by_name
                .entry(row.name.clone())
                .or_insert((0, BTreeSet::new(), u32::MAX, 0));
            e.0 += 1;
            e.1.insert(r.type_name);
            e.2 = e.2.min(row.payload_size);
            e.3 = e.3.max(row.payload_size);
        }
    }
    let mut link_rows = 0usize;
    for l in &level.named_links {
        link_rows += level.link_game_data(l).len();
    }

    println!(
        "  {regions_with_data} of {} regions carry game data; named links claim {link_rows} rows",
        level.regions.len()
    );
    println!("  {:<34} {:>6}  {:>13}  region types", "struct", "blocks", "payload bytes");
    for (name, (count, types, lo, hi)) in &by_name {
        let size = if lo == hi {
            format!("{lo}")
        } else {
            format!("{lo}..{hi}")
        };
        println!(
            "  {:<34} {count:>6}  {size:>13}  {}",
            if name.is_empty() { "(unnamed)" } else { name },
            types.iter().copied().collect::<Vec<_>>().join(", ")
        );
    }

    // Do the instance regions carry anything at all?
    let inst_with_data = level
        .regions
        .iter()
        .filter(|r| {
            (r.type_name == "kInstanceUnit" || r.type_name == "kInstanceContainer")
                && !level.region_game_data(r).is_empty()
        })
        .count();
    let inst_total = level
        .regions
        .iter()
        .filter(|r| r.type_name == "kInstanceUnit" || r.type_name == "kInstanceContainer")
        .count();
    println!("  instance regions carrying game data: {inst_with_data} of {inst_total}");

    // Scan every payload for a float triple that could be a world translation.
    // A placement transform would have to show up here as three plausible
    // metres in a row; anything the scan finds is printed so it can be judged
    // rather than silently dismissed.
    println!("\n  float-triple scan over every game-data payload");
    let mut hits = 0usize;
    let mut scanned = 0usize;
    let mut nonzero_examples: Vec<String> = Vec::new();
    for r in &level.regions {
        for row in level.region_game_data(r) {
            let Some(bytes) = level.game_data_bytes(row) else {
                continue;
            };
            scanned += 1;
            let words = bytes.len() / 4;
            for w in 0..words.saturating_sub(2) {
                let f = |k: usize| f32::from_le_bytes(bytes[(w + k) * 4..(w + k) * 4 + 4].try_into().unwrap());
                let (x, y, z) = (f(0), f(1), f(2));
                let plausible = |v: f32| v.is_finite() && v.abs() > 1.0 && v.abs() < WORLD_LIMIT;
                // Require all three to look like metres AND at least one to be
                // large enough that it could only be a position, so integers
                // and small tuning constants do not flood the report.
                if plausible(x)
                    && plausible(y)
                    && plausible(z)
                    && (x.abs() > 50.0 || y.abs() > 50.0 || z.abs() > 50.0)
                {
                    hits += 1;
                    if nonzero_examples.len() < 12 {
                        nonzero_examples.push(format!(
                            "    {} / {} word {w}: ({x:.2}, {y:.2}, {z:.2})",
                            r.name.rsplit('/').next().unwrap_or(&r.name),
                            row.name,
                            w = w
                        ));
                    }
                }
            }
        }
    }
    println!("    {scanned} payloads scanned, {hits} candidate triples");
    for e in &nonzero_examples {
        println!("{e}");
    }
    if hits == 0 {
        println!("    none — no game-data payload holds anything shaped like a world position");
    }
}

// ---------------------------------------------------------------------------
// 2. where each region's zones actually sit
// ---------------------------------------------------------------------------

fn section_2_region_bounds(
    game: &str,
    _level: &LevelAsset,
    want: &str,
) -> BTreeMap<String, (Aabb, usize, usize)> {
    println!("\n== 2. world bounds per region, measured from the zones themselves ==");

    let idx = level::get_index(game, None).expect("zone index");

    // The level asset names regions by `.region` asset path; the tool's own
    // browser groups zones by the path heuristic in `core::level`. Measuring
    // on the browser's own grouping is what matters here, because that is the
    // thing the UI will offer to load.
    let mut zones_by_region: BTreeMap<String, Vec<(u64, String)>> = BTreeMap::new();
    for z in &idx.zones {
        if !want.is_empty() && !z.region.contains(want) && !z.path.contains(want) {
            continue;
        }
        if !level::zone_has_objects(&idx, z.id) {
            continue;
        }
        zones_by_region
            .entry(z.region.clone())
            .or_default()
            .push((z.id, z.path.clone()));
    }

    let measured: Vec<(String, Aabb, usize, usize)> = zones_by_region
        .par_iter()
        .map(|(region, zones)| {
            let mut bb = Aabb::new();
            let mut objects = 0usize;
            let mut ok = 0usize;
            for (id, _) in zones {
                let Ok(raw) = idx.extract(*id) else { continue };
                let Ok(zone) = Zone::parse(&raw) else { continue };
                objects += zone.scene_objects.len();
                if let Some((min, max)) = zone.bounds() {
                    bb.add(min, max);
                    ok += 1;
                }
            }
            let _ = ok;
            (region.clone(), bb, zones.len(), objects)
        })
        .collect();

    let mut out: BTreeMap<String, (Aabb, usize, usize)> = BTreeMap::new();
    for (region, bb, zones, objects) in measured {
        if bb.n > 0 {
            out.insert(region, (bb, zones, objects));
        }
    }

    let mut rows: Vec<_> = out.iter().collect();
    rows.sort_by(|a, b| {
        a.1 .0
            .centre_radius_xz()
            .partial_cmp(&b.1 .0.centre_radius_xz())
            .unwrap()
    });

    println!(
        "  {:>5} {:>9}  {:>10}  {:>26}  {:>22}  region",
        "zones", "objects", "|centre|", "centre (x,y,z)", "span (x,y,z)"
    );
    let show = |label: &str, bb: &Aabb, zones: usize, objects: usize| {
        let c = bb.centre();
        let s = bb.span();
        println!(
            "  {zones:>5} {objects:>9}  {:>10.0}  {:>26}  {:>22}  {label}",
            bb.centre_radius_xz(),
            format!("{:.0},{:.0},{:.0}", c[0], c[1], c[2]),
            format!("{:.0},{:.0},{:.0}", s[0], s[1], s[2]),
        );
    };

    // Print the extremes: the regions closest to the origin and the ones
    // furthest from it. The interesting claim is that the first list is almost
    // entirely instances and overlays and the second almost entirely open
    // areas.
    println!("  -- 20 regions whose content is closest to the world origin --");
    for (region, (bb, zones, objects)) in rows.iter().take(20) {
        show(region, bb, *zones, *objects);
    }
    println!("  -- 10 regions furthest from the world origin --");
    for (region, (bb, zones, objects)) in rows.iter().rev().take(10) {
        show(region, bb, *zones, *objects);
    }

    // The two Nefarious Cities, side by side, whatever the filter was.
    println!("  -- every region whose name mentions nefarious --");
    for (region, (bb, zones, objects)) in rows.iter() {
        if region.to_lowercase().contains("nefarious") || region.to_lowercase().contains("nefcity") {
            show(region, bb, *zones, *objects);
        }
    }

    // How the three groups compare on "is it authored around zero".
    let mut per_group: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for (region, (bb, _, _)) in out.iter() {
        let group = region.split('/').nth(1).unwrap_or("?");
        let e = per_group.entry(match group {
            "openarea" => "openarea",
            "instance" => "instance",
            "overlay" => "overlay",
            _ => "other",
        })
        .or_insert((0, 0));
        e.0 += 1;
        if bb.centre_radius_xz() < 500.0 {
            e.1 += 1;
        }
    }
    println!("  -- regions whose content is centred within 500 m of the world origin --");
    for (group, (total, near)) in &per_group {
        println!("    {group:<10} {near:>4} of {total:>4}");
    }

    out
}

// ---------------------------------------------------------------------------
// 3. do instance regions collide with each other?
// ---------------------------------------------------------------------------

fn section_3_instance_overlap(_level: &LevelAsset, boxes: &BTreeMap<String, (Aabb, usize, usize)>) {
    println!("\n== 3. do the instance regions share a world, or all sit on the origin? ==");

    let instances: Vec<(&String, &Aabb)> = boxes
        .iter()
        .filter(|(k, _)| k.split('/').nth(1) == Some("instance"))
        .map(|(k, v)| (k, &v.0))
        .collect();
    let opens: Vec<(&String, &Aabb)> = boxes
        .iter()
        .filter(|(k, _)| k.split('/').nth(1) == Some("openarea"))
        .map(|(k, v)| (k, &v.0))
        .collect();

    let pairs = |list: &[(&String, &Aabb)]| {
        let mut over = 0usize;
        let mut total = 0usize;
        for i in 0..list.len() {
            for j in i + 1..list.len() {
                total += 1;
                if list[i].1.overlaps_xz(list[j].1) {
                    over += 1;
                }
            }
        }
        (over, total)
    };

    let (io, it) = pairs(&instances);
    let (oo, ot) = pairs(&opens);
    println!(
        "  instance x instance: {io} of {it} pairs overlap in XZ ({:.0}%)",
        if it == 0 { 0.0 } else { io as f64 * 100.0 / it as f64 }
    );
    println!(
        "  openarea x openarea: {oo} of {ot} pairs overlap in XZ ({:.0}%)",
        if ot == 0 { 0.0 } else { oo as f64 * 100.0 / ot as f64 }
    );
    println!(
        "  Two regions that really shared a world would overlap rarely — a level is laid out, not"
    );
    println!("  stacked. A high instance-to-instance overlap rate means they do not share one.");
}

// ---------------------------------------------------------------------------
// 4. named links
// ---------------------------------------------------------------------------

fn section_4_named_links(level: &LevelAsset, boxes: &BTreeMap<String, (Aabb, usize, usize)>) {
    println!("\n== 4. named links — a spawn point, or a placement? ==");

    // A link's position is stored in the level, so if a transform existed
    // anywhere this is where it would be implied: an instance region's link
    // would sit out in the open world, and the region's own zones would need
    // moving to meet it. Check instead whether the link lands INSIDE that
    // region's own measured bounds.
    let mut inside = 0usize;
    let mut outside = 0usize;
    let mut samples: Vec<String> = Vec::new();

    for r in &level.regions {
        let links = level.region_named_links(r);
        if links.is_empty() {
            continue;
        }
        // The level names regions by `.region` path; match it to the browser's
        // region key by the trailing name component.
        // Level region names are `.region` asset paths ("…/nefariouscity.region");
        // browser region keys end in the same leaf without the extension.
        let leaf = r
            .name
            .rsplit('/')
            .next()
            .unwrap_or(&r.name)
            .trim_end_matches(".region")
            .to_lowercase();
        let Some((key, (bb, _, _))) = boxes
            .iter()
            .find(|(k, _)| k.rsplit('/').next().map(|s| s.to_lowercase()) == Some(leaf.clone()))
        else {
            continue;
        };
        for l in links {
            let p = l.position;
            let pad = 200.0f32;
            let within = (0..3).all(|i| p[i] >= bb.min[i] - pad && p[i] <= bb.max[i] + pad);
            if within {
                inside += 1;
            } else {
                outside += 1;
            }
            if samples.len() < 14 && r.type_name == "kInstanceUnit" {
                samples.push(format!(
                    "    {:<40} link {:<28} at ({:.0},{:.0},{:.0})  region centre ({:.0},{:.0},{:.0}) {}",
                    key,
                    l.short_name(),
                    p[0],
                    p[1],
                    p[2],
                    bb.centre()[0],
                    bb.centre()[1],
                    bb.centre()[2],
                    if within { "INSIDE" } else { "outside" }
                ));
            }
        }
    }

    println!("  {inside} links land inside their own region's measured bounds, {outside} do not");
    for s in &samples {
        println!("{s}");
    }
    println!(
        "  A link inside its own region's bounds is a spawn point in that region's own space."
    );
    println!("  It is not a transform: it says where the player appears, not where the region is.");
}

// ---------------------------------------------------------------------------
// 5. do two named regions share one coordinate space?
// ---------------------------------------------------------------------------

/// Every placed model instance of a region, as `(model asset id, position
/// rounded to the metre)`. Two regions authored in the same world space and
/// holding the same content will agree on a large share of these; two regions
/// each authored around their own origin will agree on almost none, because a
/// coincidence needs the same model AND the same metre in all three axes.
fn region_instance_keys(
    idx: &level::LevelIndex,
    region_key: &str,
) -> (BTreeSet<(u64, i32, i32, i32)>, usize) {
    let zones: Vec<u64> = idx
        .zones
        .iter()
        .filter(|z| z.region == region_key && level::zone_has_objects(idx, z.id))
        .map(|z| z.id)
        .collect();

    let sets: Vec<BTreeSet<(u64, i32, i32, i32)>> = zones
        .par_iter()
        .map(|id| {
            let mut out = BTreeSet::new();
            let Ok(raw) = idx.extract(*id) else { return out };
            let Ok(zone) = Zone::parse(&raw) else { return out };
            for o in &zone.scene_objects {
                if o.model_id == 0 {
                    continue;
                }
                let m = &o.matrix;
                out.insert((
                    o.model_id,
                    m[12].round() as i32,
                    m[13].round() as i32,
                    m[14].round() as i32,
                ));
            }
            out
        })
        .collect();

    let mut all = BTreeSet::new();
    let mut total = 0usize;
    for s in sets {
        total += s.len();
        all.extend(s);
    }
    (all, total)
}

fn section_5_shared_space(game: &str) {
    println!("
== 5. do the two Nefarious Cities share one coordinate space? ==");
    let idx = level::get_index(game, None).expect("zone index");

    // The pairs worth asking about: the two spellings of Nefarious City, and a
    // control pair that cannot possibly share a space (two different planets).
    let pairs = [
        (
            "i29/openarea/nefarious_city",
            "i29/instance/nefariouscity/nefariouscity",
        ),
        (
            "i29/openarea/nefarious_city",
            "i29/instance/nefariouscity_actone/nefariouscity_actone",
        ),
        (
            "i29/openarea/nefarious_city",
            "i29/instance/nefariouscity_tower/nefariouscity_tower",
        ),
        // control: two regions that are certainly different places
        ("i29/openarea/nefarious_city", "i29/openarea/savali"),
        (
            "i29/instance/nefariouscity/nefariouscity",
            "i29/instance/cordelion/cordelion_a_main",
        ),
    ];

    let mut cache: BTreeMap<&str, (BTreeSet<(u64, i32, i32, i32)>, usize)> = BTreeMap::new();
    for (a, b) in pairs {
        for k in [a, b] {
            if !cache.contains_key(k) {
                cache.insert(k, region_instance_keys(&idx, k));
            }
        }
    }

    println!(
        "  {:>9} {:>9} {:>9} {:>7}  pair",
        "A insts", "B insts", "shared", "of A"
    );
    for (a, b) in pairs {
        let (sa, _) = &cache[a];
        let (sb, _) = &cache[b];
        let shared = sa.intersection(sb).count();
        println!(
            "  {:>9} {:>9} {:>9} {:>6.1}%  {a}
  {:>9} {:>9} {:>9} {:>7}  vs {b}",
            sa.len(),
            sb.len(),
            shared,
            if sa.is_empty() {
                0.0
            } else {
                shared as f64 * 100.0 / sa.len() as f64
            },
            "",
            "",
            "",
            ""
        );
    }
    println!(
        "  A high share means the same models sit at the same world metres in both regions, i.e."
    );
    println!("  one space. Near zero means each region is its own space and the two must not be");
    println!("  drawn together without a transform we do not have.");
}

// ---------------------------------------------------------------------------
// 6. the rows the zone browser will show for a name search
// ---------------------------------------------------------------------------

/// The browser's search normaliser: lowercase, and drop every character that
/// is not a letter or a digit. `nefarious_city` and `nefariouscity` collapse to
/// the same string, which is the whole point — the two Nefarious Cities differ
/// by exactly one underscore.
fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn section_6_search(game: &str, query: &str) {
    println!("
== 6. what a cross-group search for \"{query}\" turns up ==");
    let idx = level::get_index(game, None).expect("zone index");
    let needle = squash(query);

    #[derive(Default)]
    struct Row {
        group: &'static str,
        zones: usize,
        loadable: usize,
        objects: usize,
        model_instances: usize,
        kinds: BTreeMap<String, usize>,
    }

    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    for z in &idx.zones {
        if !squash(&z.region).contains(&needle) {
            continue;
        }
        let r = rows.entry(z.region.clone()).or_default();
        r.group = level::group_of(z.region.split('/').nth(1).unwrap_or(""));
        r.zones += 1;
        *r.kinds.entry(z.kind.to_string()).or_default() += 1;
    }

    let keys: Vec<String> = rows.keys().cloned().collect();
    for key in &keys {
        let zones: Vec<u64> = idx
            .zones
            .iter()
            .filter(|z| &z.region == key && level::zone_has_objects(&idx, z.id))
            .map(|z| z.id)
            .collect();
        let counted: Vec<(usize, usize)> = zones
            .par_iter()
            .map(|id| {
                let Ok(raw) = idx.extract(*id) else {
                    return (0, 0);
                };
                let Ok(zone) = Zone::parse(&raw) else {
                    return (0, 0);
                };
                (
                    zone.scene_objects.len(),
                    zone.scene_objects.iter().filter(|o| o.model_id != 0).count(),
                )
            })
            .collect();
        let r = rows.get_mut(key).unwrap();
        r.loadable = zones.len();
        for (o, m) in counted {
            r.objects += o;
            r.model_instances += m;
        }
    }

    let mut sorted: Vec<(&String, &Row)> = rows.iter().collect();
    sorted.sort_by_key(|(_, r)| std::cmp::Reverse(r.model_instances));
    println!(
        "  {:<10} {:>6} {:>9} {:>10} {:>16}  region",
        "group", "zones", "loadable", "objects", "model instances"
    );
    for (key, r) in &sorted {
        println!(
            "  {:<10} {:>6} {:>9} {:>10} {:>16}  {key}",
            r.group, r.zones, r.loadable, r.objects, r.model_instances
        );
    }
    let total_zones: usize = sorted.iter().map(|(_, r)| r.zones).sum();
    let total_inst: usize = sorted.iter().map(|(_, r)| r.model_instances).sum();
    println!(
        "  {} regions across {} groups, {total_zones} zones, {total_inst} model instances",
        sorted.len(),
        sorted
            .iter()
            .map(|(_, r)| r.group)
            .collect::<BTreeSet<_>>()
            .len()
    );
    for (key, r) in &sorted {
        if r.zones > 0 {
            let kinds: Vec<String> = r.kinds.iter().map(|(k, n)| format!("{k} {n}")).collect();
            println!("    {key}: {}", kinds.join(", "));
        }
    }
}

// ---------------------------------------------------------------------------
// verdict
// ---------------------------------------------------------------------------

fn verdict(level: &LevelAsset, boxes: &BTreeMap<String, (Aabb, usize, usize)>) {
    println!("\n== verdict ==");
    println!(
        "  LevelRegionBuilt is {} bytes and every field is accounted for: asset id, type, name,",
        la::REGION_STRIDE
    );
    println!("  lod index, parent, children, zone refs, secondary zone refs, links, game data.");
    println!("  There is no room for a matrix, a quaternion or a translation.");
    println!(
        "  Level Region Lod Built is the only spatial record attached to a region, and it exists"
    );
    println!(
        "  only for kOpenUnit ({} sets for {} regions) — it is the streaming reference point, not",
        level.region_lods.len(),
        level
            .regions
            .iter()
            .filter(|r| r.type_name == "kOpenUnit")
            .count()
    );
    println!("  a placement: an open unit's zones are already in world space.");

    let inst_near = boxes
        .iter()
        .filter(|(k, v)| k.split('/').nth(1) == Some("instance") && v.0.centre_radius_xz() < 500.0)
        .count();
    let inst_total = boxes
        .iter()
        .filter(|(k, _)| k.split('/').nth(1) == Some("instance"))
        .count();
    println!(
        "  {inst_near} of {inst_total} instance regions are centred within 500 m of the world origin."
    );
}
