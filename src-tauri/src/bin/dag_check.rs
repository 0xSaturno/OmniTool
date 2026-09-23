//! Verification harness for the section-name table and the `dag` reader.
//!
//!   cargo run --bin dag_check -- "F:/SteamLibrary/.../Ratchet & Clank - Rift Apart" [hashes file]
//!
//! Reads the game folder only; writes nothing. It reports, in order:
//!
//!   1. the section-name table: size, and that every name still hashes to its
//!      own tag with `core::crc32`, checked against the tags we had already
//!      identified by hand;
//!   2. a census of the section tags that actually occur in shipped zone and
//!      model assets, and how many of them the table can name;
//!   3. the `dag`: de-transposed asset ids checked against `crc64(name)` for
//!      every entry, the same block read as a plain `u64[]` for comparison,
//!      whether the ids are ascending, and the shape of the link arrays;
//!   4. name coverage of the TOC from the `dag` versus from `hashes.txt`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

use omnitool_lib::core::crc32;
use omnitool_lib::core::crc64;
use omnitool_lib::core::dag::{self, Dag};
use omnitool_lib::core::dat1::Dat1;
use omnitool_lib::core::level::{self, LevelIndex};
use omnitool_lib::core::section_names;

const DEFAULT_GAME: &str = r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart";
const DEFAULT_HASHES: &str = r"E:\RCRALevelEditorViewer\refs\RCRA-Forge\hashes.txt";

/// How many zones and how many models to open for the tag census.
const ZONE_SAMPLE: usize = 400;
const MODEL_SAMPLE: usize = 400;

fn rule(title: &str) {
    println!("\n=== {title} ===");
}

fn main() {
    let mut args = std::env::args().skip(1);
    let game = args.next().unwrap_or_else(|| DEFAULT_GAME.to_string());
    let hashes = args.next().unwrap_or_else(|| DEFAULT_HASHES.to_string());

    let mut failures = 0usize;

    failures += part1_table();
    let index = match level::get_index(&game, Some(&hashes)) {
        Ok(i) => i,
        Err(e) => {
            println!("\nFATAL: could not build the level index: {e}");
            std::process::exit(1);
        }
    };
    failures += part2_census(&index);
    let t_dag = Instant::now();
    let dag = match Dag::load(Path::new(&game)) {
        Ok(d) => d,
        Err(e) => {
            println!("\nFATAL: could not read the dag: {e}");
            std::process::exit(1);
        }
    };
    let dag_load = t_dag.elapsed();
    failures += part3_dag(&dag, dag_load);
    failures += part4_names(&index, &dag, Path::new(&hashes));

    rule("RESULT");
    if failures == 0 {
        println!("all checks passed");
    } else {
        println!("{failures} check(s) FAILED");
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// 1. The section-name table
// ---------------------------------------------------------------------------

/// Every tag `level::zone_section_name` names today, with the label it gives.
///
/// Two jobs: the subset whose labels we worked out independently pins down the
/// CRC-32 seed and final-xor, and the whole list tells us whether the table can
/// replace the hand-written match arm without losing a name.
const HAND_NAMED: &[(u32, &str)] = &[
    (0x06AB_CAB2, "Zone Scene Objects"),
    (0x6987_F172, "Zone Model Insts"),
    (0xC6A5_905E, "Zone Model Names"),
    (0x7068_2CB8, "Zone Actors"),
    (0xDC62_5B3D, "Zone Actor Names"),
    (0x7868_4035, "Actor Asset Refs"),
    (0x50ED_C53D, "Zone Actor Priuses"),
    (0x0CF5_8A6E, "Zone Actor Groups"),
    (0x97FF_6EB5, "Zone Lights"),
    (0xBDAB_2B0D, "Zone Volumes"),
    (0xBCF4_3558, "Zone Decals"),
    (0x91DE_11D9, "Zone Decal Assets"),
    (0x6575_12BB, "Zone Decal Geometry"),
    (0x927C_4EC3, "Zone Visual Effect Insts"),
    (0x3D5E_2FEF, "Zone Visual Effect Names"),
    (0x30DA_DA09, "Zone Asset References"),
    (0xB6A0_B72A, "Zone Atmosphere Name"),
    (0x0277_95C5, "Zone EnvProbe Assets"),
    (0x5570_13E9, "Zone EnvProbe List"),
    (0xDC31_1FC3, "Zone Impostors"),
    (0xCB8D_34F9, "Zone Impostors Atlas"),
    (0x8E40_1376, "Zone Impostors BVH Info"),
    (0x5736_A46F, "Zone Instance Vertex Data Index"),
    (0x9CCA_A06F, "Zone Instance Vertex Data"),
    (0xDE61_C274, "Zone Local Frames"),
    (0xE415_8AC3, "Zone Material Overrides"),
    (0x17AF_FFCE, "Zone Override Tris"),
    (0x5E54_ACCF, "Zone Script Actions"),
    (0xBEAB_52E7, "Zone Script Plugs"),
    (0x2300_D240, "Zone Script Priuses"),
    (0xEF86_37D5, "Zone Script Strings"),
    (0xD86A_7934, "Zone Script Vars"),
    (0x7CA7_267D, "Level Built"),
    (0x2BA3_3702, "Level Zone Names"),
    (0x4E02_3760, "Level Zones Built"),
    (0x4130_D903, "Level Region Names"),
    (0x396F_9418, "Level Regions Built"),
];

/// The subset of [`HAND_NAMED`] we are confident is spelled the way the engine
/// spells it; used to confirm the CRC-32 parameters.
const SEED_PROBE: &[(u32, &str)] = &[
    (0x06AB_CAB2, "Zone Scene Objects"),
    (0x6987_F172, "Zone Model Insts"),
    (0xC6A5_905E, "Zone Model Names"),
    (0x7068_2CB8, "Zone Actors"),
    (0xBCF4_3558, "Zone Decals"),
    (0x97FF_6EB5, "Zone Lights"),
    (0xEF86_37D5, "Zone Script Strings"),
    (0x7CA7_267D, "Level Built"),
];

const FORMERLY_UNKNOWN: &[(u32, &str)] = &[
    (0x2FFB_5E42, "Zone Decal BVH Info"),
    (0x5736_A46F, "Zone Vertex Paint Info"),
    (0x9CCA_A06F, "Zone Vertex Paint Color"),
    (0x758B_AFBD, "Zone Actor Vertex Paint Info"),
    (0xF435_AE9C, "Zone Actor Vertex Paint Color"),
    (0x58F8_61B6, "Zone Delay Streaming Model Count"),
    (0xC515_00F1, "Zone Delay Streaming Model Inst Count"),
    (0x80D2_9828, "Zone Script String Hashes"),
];

fn part1_table() -> usize {
    rule("1. section-name table");
    let mut bad = 0;

    // Seed/xor of our CRC-32 confirmed against tags we identified independently.
    for &(tag, name) in SEED_PROBE {
        if crc32::hash(name) != tag {
            println!("  FAIL crc32({name:?}) = {:08X}, expected {tag:08X}", crc32::hash(name));
            bad += 1;
        }
    }
    println!(
        "  crc32 seed/xor confirmed on {} hand-identified tags: {}",
        SEED_PROBE.len(),
        if bad == 0 { "ok" } else { "MISMATCH" }
    );

    let mut rehash_bad = 0;
    for &(tag, name) in section_names::SECTION_NAMES {
        if crc32::hash(name) != tag {
            if rehash_bad < 10 {
                println!("  FAIL {name:?} hashes to {:08X}, table says {tag:08X}", crc32::hash(name));
            }
            rehash_bad += 1;
        }
    }
    println!(
        "  table entries: {} — re-hashed with core::crc32, {} reproduce, {} dropped",
        section_names::len(),
        section_names::len() - rehash_bad,
        rehash_bad
    );
    bad += rehash_bad;

    let unsorted = section_names::SECTION_NAMES.windows(2).filter(|w| w[0].0 >= w[1].0).count();
    if unsorted > 0 {
        println!("  FAIL table is not strictly sorted ({unsorted} bad pairs)");
        bad += unsorted;
    }

    let mut missing = 0;
    let mut respelled = Vec::new();
    for &(tag, want) in HAND_NAMED {
        match section_names::section_name(tag) {
            Some(got) if got == want => {}
            Some(got) => respelled.push((tag, want, got)),
            None => {
                missing += 1;
                bad += 1;
                println!("  FAIL {tag:08X}: the viewer names it {want:?}, the table does not");
            }
        }
    }
    println!(
        "  the {} tags the viewer names today: {} covered by the table, {missing} not",
        HAND_NAMED.len(),
        HAND_NAMED.len() - missing
    );
    println!(
        "  of those, {} are spelled differently — the table's spelling is the hash-verified one:",
        respelled.len()
    );
    for (tag, want, got) in &respelled {
        println!("    {tag:08X}  viewer {want:<32} table {got}");
    }

    println!("  the eight tags the viewer showed as \"unknown\":");
    for &(tag, want) in FORMERLY_UNKNOWN {
        let got = section_names::section_name(tag);
        let ok = got == Some(want);
        if !ok {
            bad += 1;
        }
        println!(
            "    {tag:08X} -> {:<40} {}",
            got.unwrap_or("<STILL UNKNOWN>"),
            if ok { "ok" } else { "FAIL" }
        );
    }
    bad
}

// ---------------------------------------------------------------------------
// 2. Which tags actually occur in shipped assets
// ---------------------------------------------------------------------------

fn collect_tags(bytes: &[u8], into: &mut BTreeMap<u32, usize>) {
    // Assets come out of the TOC with their 36-byte header in front; the DAT1
    // container starts right after it.
    for start in [36usize, 0] {
        if bytes.len() > start {
            if let Ok(d) = Dat1::parse(&bytes[start..]) {
                for s in &d.sections {
                    *into.entry(s.tag).or_default() += 1;
                }
                return;
            }
        }
    }
}

fn part2_census(index: &LevelIndex) -> usize {
    rule("2. section tags in shipped zones and models");
    if index.names_missing {
        println!("  SKIPPED: no hashes file, cannot pick assets by path");
        return 0;
    }

    let mut zone_ids: Vec<u64> = index.zones.iter().map(|z| z.id).collect();
    zone_ids.sort_unstable();
    zone_ids.truncate(ZONE_SAMPLE);

    let mut model_ids: Vec<u64> = index
        .names
        .iter()
        .filter(|(id, p)| p.ends_with(".model") && index.asset(**id).is_some())
        .map(|(id, _)| *id)
        .collect();
    model_ids.sort_unstable();
    model_ids.truncate(MODEL_SAMPLE);

    let t0 = Instant::now();
    let mut zone_tags: BTreeMap<u32, usize> = BTreeMap::new();
    let mut zones_read = 0;
    for id in &zone_ids {
        if let Ok(b) = index.extract(*id) {
            collect_tags(&b, &mut zone_tags);
            zones_read += 1;
        }
    }
    let mut model_tags: BTreeMap<u32, usize> = BTreeMap::new();
    let mut models_read = 0;
    for id in &model_ids {
        if let Ok(b) = index.extract(*id) {
            collect_tags(&b, &mut model_tags);
            models_read += 1;
        }
    }
    println!(
        "  read {zones_read} zones and {models_read} models in {:.1}s",
        t0.elapsed().as_secs_f32()
    );

    let mut all: BTreeMap<u32, usize> = zone_tags.clone();
    for (t, n) in &model_tags {
        *all.entry(*t).or_default() += n;
    }

    let named_either =
        |t: u32| section_names::section_name(t).is_some() || level::zone_section_name(t).is_some();
    let report = |label: &str, tags: &BTreeMap<u32, usize>| -> Vec<u32> {
        let old = tags.keys().filter(|t| level::zone_section_name(**t).is_some()).count();
        let new = tags.keys().filter(|t| section_names::section_name(**t).is_some()).count();
        let union = tags.keys().filter(|t| named_either(**t)).count();
        println!(
            "  {label:<22} {:>4} distinct tags — viewer names {:>3}, table names {:>3}, union {:>3}",
            tags.len(),
            old,
            new,
            union
        );
        tags.keys().copied().filter(|t| !named_either(*t)).collect()
    };
    report("zones", &zone_tags);
    report("models", &model_tags);
    let unnamed = report("zones + models", &all);

    // Tags the viewer can name but the table cannot: those are the reason a
    // rewired lookup has to consult both, not just the table.
    let only_viewer: Vec<u32> = all
        .keys()
        .copied()
        .filter(|t| section_names::section_name(*t).is_none() && level::zone_section_name(*t).is_some())
        .collect();
    println!("  named by the viewer but NOT in the table ({}):", only_viewer.len());
    for t in &only_viewer {
        println!("    {t:08X}  {}", level::zone_section_name(*t).unwrap_or("-"));
    }

    println!("  still unnamed by either ({}):", unnamed.len());
    for t in &unnamed {
        println!("    {t:08X}  seen in {:>5} of the sampled assets", all[t]);
    }
    0
}

// ---------------------------------------------------------------------------
// 3. The dag itself
// ---------------------------------------------------------------------------

fn part3_dag(dag: &Dag, load: std::time::Duration) -> usize {
    rule("3. dag");
    let mut bad = 0;
    let n = dag.len();
    println!("  {n} assets, read and fully decoded in {:.0} ms", load.as_secs_f32() * 1000.0);
    println!(
        "  ids ascending (binary-searchable): {}",
        if dag.ids_ascending { "yes" } else { "NO" }
    );
    if !dag.ids_ascending {
        bad += 1;
    }

    let flags: BTreeSet<u64> = dag.ids.iter().map(|id| id >> 62).collect();
    println!("  top-2-bit values across all ids: {flags:?}  (2 = Shipped)");

    let plain = dag::plain_u64(dag.raw_id_block());
    let t0 = Instant::now();
    let (mut de_match, mut plain_match, mut empty) = (0usize, 0usize, 0usize);
    let mut shown = 0;
    for i in 0..n {
        let Some(name) = dag.name(i) else {
            empty += 1;
            continue;
        };
        if name.is_empty() {
            empty += 1;
            continue;
        }
        let h = crc64::hash(name);
        if h == dag.ids[i] {
            de_match += 1;
        } else if shown < 5 {
            println!("    MISMATCH #{i}: id {:016X}, crc64(name) {h:016X}, {name}", dag.ids[i]);
            shown += 1;
        }
        if h == plain[i] {
            plain_match += 1;
        }
    }
    println!(
        "  crc64(name) vs de-transposed ids : {de_match}/{n} in {:.1}s",
        t0.elapsed().as_secs_f32()
    );
    println!("  crc64(name) vs plain u64[] ids   : {plain_match}/{n}");
    println!("  entries with no name             : {empty}");
    if de_match + empty != n {
        bad += 1;
    }

    let mut types: BTreeMap<u8, usize> = BTreeMap::new();
    for &t in &dag.types {
        *types.entry(t).or_default() += 1;
    }
    let top: Vec<String> = {
        let mut v: Vec<_> = types.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        v.iter().take(8).map(|(t, c)| format!("{t}:{c}")).collect()
    };
    println!("  asset types: {} distinct, most common {}", types.len(), top.join(" "));

    let heads = dag.link_heads.iter().filter(|&&h| h >= 0).count();
    println!(
        "  link heads: {heads} assets have dependencies, {} have none",
        n - heads
    );
    println!("  links array: {} i32 slots", dag.links.len());
    println!(
        "  lighting-condition link heads: {} ({} groups of {})",
        dag.lc_link_heads.len(),
        dag.lc_link_heads.len() / dag::LC_GROUP,
        dag::LC_GROUP
    );

    // Every link slot should be reachable by walking the runs from the asset
    // link heads and then from the lighting-condition link heads. A run ends at
    // the first negative value, and that terminator slot counts as reached.
    // Nothing left over is what proves this reading of the block is complete.
    let mut seen = vec![false; dag.links.len()];
    let walk = |start: usize, seen: &mut Vec<bool>| {
        let mut j = start;
        while j < seen.len() {
            seen[j] = true;
            if dag.links[j] < 0 {
                return;
            }
            j += 1;
        }
    };
    for &h in &dag.link_heads {
        if h >= 0 {
            walk(h as usize, &mut seen);
        }
    }
    let after_nodes = seen.iter().filter(|&&s| s).count();
    for &h in &dag.lc_link_heads {
        if h >= 0 {
            walk(h as usize, &mut seen);
        }
    }
    let after_lc = seen.iter().filter(|&&s| s).count();
    println!(
        "  link slots reached: {after_nodes} from asset link heads, {after_lc}/{} once the lighting-condition heads are walked too",
        dag.links.len()
    );
    if after_lc != dag.links.len() {
        println!("  NOTE {} link slots unaccounted for", dag.links.len() - after_lc);
        bad += 1;
    }

    let edges: usize = (0..n).map(|i| dag.deps(i).len()).sum();
    let lc_assets = (0..n).filter(|&i| dag.lc_marker(i).is_some()).count();
    let lc_edges: usize = (0..n).flat_map(|i| dag.lc_deps(i)).map(|(_, v)| v.len()).sum();
    println!(
        "  dependency edges: {edges} unconditional, {lc_edges} lighting-condition-only across {lc_assets} assets"
    );

    // A lighting-condition example, so that line has something behind it.
    if let Some(i) = (0..n).find(|&i| dag.lc_deps(i).iter().any(|(_, v)| !v.is_empty())) {
        println!("  lighting-condition example: {}", dag.name(i).unwrap_or("?"));
        for (slot, v) in dag.lc_deps(i) {
            for d in v.iter().take(2) {
                println!("      condition {slot} -> {}", dag.name(*d).unwrap_or("?"));
            }
        }
    }

    // Spot check: a lookup by id round-trips to the same name.
    if let Some(name) = dag.name(n / 2) {
        let id = crc64::hash(name);
        let ok = dag.index_of(id) == Some(n / 2);
        println!("  index_of(crc64(name)) round trip: {}", if ok { "ok" } else { "FAIL" });
        if !ok {
            bad += 1;
        }
    }

    // A worked example, so the numbers above have something concrete behind them.
    if let Some(i) = (0..n).find(|&i| dag.deps(i).len() >= 3) {
        println!("  example: {}", dag.name(i).unwrap_or("?"));
        for d in dag.deps(i).iter().take(3) {
            println!("      -> [{}] {}", dag.types[*d], dag.name(*d).unwrap_or("?"));
        }
    }
    bad
}

// ---------------------------------------------------------------------------
// 4. dag names versus hashes.txt
// ---------------------------------------------------------------------------

fn load_hashes(path: &Path) -> HashMap<u64, String> {
    let mut map = HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return map;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, ',');
        let (Some(hex), Some(p)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Ok(id) = u64::from_str_radix(hex, 16) else {
            continue;
        };
        map.entry(id).or_insert_with(|| crc64::normalize_path(p));
    }
    map
}

fn part4_names(index: &LevelIndex, dag: &Dag, hashes_path: &Path) -> usize {
    rule("4. name coverage: dag vs hashes.txt");
    let toc_ids: BTreeSet<u64> = index.toc.asset_ids().iter().copied().collect();
    println!("  TOC: {} distinct asset ids", toc_ids.len());
    // Only ids with the Shipped flag (top two bits `0b10`) are derived from a
    // path at all; `0b11` marks a 32-bit external / Wwise hash, which no name
    // list can ever resolve. Coverage is only meaningful against the former.
    let shipped: usize = toc_ids.iter().filter(|id| *id >> 62 == 2).count();
    println!(
        "    of which {shipped} carry the Shipped flag (path-derived) and {} are external/Wwise hashes",
        toc_ids.len() - shipped
    );

    let hashes = load_hashes(hashes_path);
    println!(
        "  hashes.txt: {} entries ({})",
        hashes.len(),
        PathBuf::from(hashes_path).display()
    );
    println!("  dag: {} entries", dag.len());

    let (mut both, mut dag_only, mut hash_only, mut neither) = (0usize, 0usize, 0usize, 0usize);
    let mut disagree = 0usize;
    let mut disagree_sample = Vec::new();
    let mut dag_only_sample = Vec::new();
    let mut hash_only_sample = Vec::new();
    for &id in &toc_ids {
        let d = dag.name_of_id(id).map(crc64::normalize_path);
        let h = hashes.get(&id);
        match (&d, h) {
            (Some(dn), Some(hn)) => {
                both += 1;
                if dn != hn {
                    disagree += 1;
                    if disagree_sample.len() < 3 {
                        disagree_sample.push((id, dn.clone(), hn.clone()));
                    }
                }
            }
            (Some(dn), None) => {
                dag_only += 1;
                if dag_only_sample.len() < 5 {
                    dag_only_sample.push((id, dn.clone()));
                }
            }
            (None, Some(hn)) => {
                hash_only += 1;
                if hash_only_sample.len() < 5 {
                    hash_only_sample.push((id, hn.clone()));
                }
            }
            (None, None) => neither += 1,
        }
    }
    let total = toc_ids.len();
    let pct = |n: usize| 100.0 * n as f64 / total as f64;
    let pcts = |n: usize| 100.0 * n as f64 / shipped as f64;
    println!(
        "  named by dag        : {:>7} ({:.2}% of all ids, {:.3}% of the Shipped ones)",
        both + dag_only,
        pct(both + dag_only),
        pcts(both + dag_only)
    );
    println!(
        "  named by hashes.txt : {:>7} ({:.2}% of all ids, {:.3}% of the Shipped ones)",
        both + hash_only,
        pct(both + hash_only),
        pcts(both + hash_only)
    );
    println!("  both                : {both:>7}   (disagreeing paths: {disagree})");
    println!("  dag only            : {dag_only:>7}");
    println!("  hashes.txt only     : {hash_only:>7}");
    println!("  neither             : {neither:>7} ({:.2}%)", pct(neither));
    for (id, d, h) in &disagree_sample {
        println!("    DISAGREE {id:016X}\n      dag   {d}\n      hashes {h}");
    }
    for (id, n) in &dag_only_sample {
        println!("    dag only    {id:016X} {n}");
    }
    for (id, n) in &hash_only_sample {
        println!("    hashes only {id:016X} {:?}", n);
    }

    // hashes.txt carries entries whose path is blank; those are ids, not names,
    // so they do not count as coverage the dag is missing.
    let hash_only_real: Vec<(u64, &String)> = toc_ids
        .iter()
        .filter(|id| dag.name_of_id(**id).is_none())
        .filter_map(|id| hashes.get(id).map(|p| (*id, p)))
        .filter(|(_, p)| !p.is_empty())
        .collect();
    println!(
        "  of the {hash_only} hashes.txt-only ids, {} have a BLANK path and {} a real one:",
        hash_only - hash_only_real.len(),
        hash_only_real.len()
    );
    for (id, p) in &hash_only_real {
        println!("    {id:016X} {p}  (crc64 of that path: {:016X})", crc64::hash(p));
    }
    let blank_total = hashes.values().filter(|p| p.is_empty()).count();
    println!("  hashes.txt entries with a blank path overall: {blank_total}");

    // Where the rest of the dag goes: entries whose id the TOC does not ship.
    let not_in_toc: Vec<usize> =
        (0..dag.len()).filter(|&i| !toc_ids.contains(&dag.ids[i])).collect();
    println!(
        "  dag entries whose id is not in the TOC: {} of {}",
        not_in_toc.len(),
        dag.len()
    );
    let mut ext: BTreeMap<String, usize> = BTreeMap::new();
    for &i in &not_in_toc {
        let n = dag.name(i).unwrap_or("");
        let e = n.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        *ext.entry(e).or_default() += 1;
    }
    let mut ext: Vec<_> = ext.into_iter().collect();
    ext.sort_by(|a, b| b.1.cmp(&a.1));
    println!(
        "    by extension: {}",
        ext.iter().take(8).map(|(e, c)| format!("{e}:{c}")).collect::<Vec<_>>().join(" ")
    );

    // And the other direction: TOC ids nothing names at all.
    let unnamed_toc: Vec<u64> = toc_ids
        .iter()
        .copied()
        .filter(|id| dag.name_of_id(*id).is_none() && !hashes.contains_key(id))
        .collect();
    let flags: BTreeMap<u64, usize> = unnamed_toc.iter().fold(BTreeMap::new(), |mut m, id| {
        *m.entry(id >> 62).or_default() += 1;
        m
    });
    println!("  TOC ids nothing names: {} — top-2-bit histogram {flags:?}", unnamed_toc.len());

    // How many of the ids only hashes.txt covers are actually reachable — i.e.
    // whether dropping hashes.txt would cost the viewer anything real.
    let dag_zone_names = dag
        .ids
        .iter()
        .enumerate()
        .filter(|(i, id)| {
            toc_ids.contains(id) && dag.name(*i).map(|n| n.ends_with(".zone")).unwrap_or(false)
        })
        .count();
    println!("  .zone assets the dag names and the TOC ships: {dag_zone_names}");
    println!("  .zone assets in the current index (hashes.txt): {}", index.zones.len());
    0
}
