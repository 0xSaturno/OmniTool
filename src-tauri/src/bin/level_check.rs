//! Verification harness for the Level Editor Rust side.
//!
//!   cargo run --bin level_check -- "F:/SteamLibrary/steamapps/common/Ratchet & Clank - Rift Apart" [hashes file]
//!
//! Drives the five `level_*` Tauri commands directly — same functions the UI
//! calls, same arguments — and reports:
//!   * the region list with zone counts, grouped the way the sidebar groups it
//!   * `hasObjects` checked against a direct parse, plus the cost of measuring
//!     it for a whole planet
//!   * object / entity / model counts and bounds for three zones of different kinds
//!   * decoded vertex / index counts and position bounds for an art zone's top
//!     three models, each checked against the model's own Built bounding box
//!   * a save round trip through `level_save_zone`: the written asset re-parses
//!     and differs from the original only inside the edited field
//!   * timings for the index load, zone loads and mesh decodes
//!
//! Reads the game folder only. Writes one throwaway Stager project, which it
//! deletes again on the way out.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use omnitool_lib::commands_level_editor as cmd;
use omnitool_lib::core::level::{self, LevelIndex};
use omnitool_lib::core::zone::Zone;
use tauri::async_runtime::block_on;

const DEFAULT_GAME: &str = r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart";
const HASHES: &str = r"E:\RCRALevelEditorViewer\refs\RCRA-Forge\hashes.txt";
const SAMPLE_REGION: &str = "i29/openarea/sargasso";
const PROJECT: &str = "level_check_scratch";

fn load(index: &LevelIndex, path: &str) -> Result<Zone, String> {
    let id = omnitool_lib::core::crc64::hash(path);
    let raw = index.extract(id).map_err(|e| e.to_string())?;
    Zone::parse(&raw).map_err(|e| e.to_string())
}

fn main() {
    let game = std::env::args().nth(1).unwrap_or_else(|| DEFAULT_GAME.into());
    let hashes = std::env::args().nth(2).unwrap_or_else(|| HASHES.into());
    let mut failures = 0usize;
    fn fail(msg: &str, failures: &mut usize) {
        println!("  FAIL: {msg}");
        *failures += 1;
    }

    // ---- index / level_list_planets ----------------------------------------
    level::clear_index_cache();
    let t0 = Instant::now();
    let regions = match block_on(cmd::level_list_planets(game.clone(), Some(hashes.clone()))) {
        Ok(r) => r,
        Err(e) => {
            println!("FATAL level_list_planets: {e}");
            std::process::exit(1);
        }
    };
    let cold = t0.elapsed();
    let t1 = Instant::now();
    let _ = block_on(cmd::level_list_planets(game.clone(), Some(hashes.clone()))).unwrap();
    let warm = t1.elapsed();

    let index = level::get_index(&game, Some(&hashes)).unwrap();
    let t = Instant::now();
    let stats = block_on(cmd::level_zone_stats(game.clone(), Some(hashes.clone()))).unwrap();
    let stats_cold = t.elapsed();
    let t = Instant::now();
    let _ = block_on(cmd::level_zone_stats(game.clone(), Some(hashes.clone()))).unwrap();
    println!(
        "index: {} TOC assets, {} zones, {} regions   cold {:?}, warm {:?}",
        index.assets.len(),
        stats.total_zones,
        stats.regions,
        cold,
        warm
    );
    println!(
        "level_zone_stats: {} of {} zones carry objects — cold {:?} (probes every zone), warm {:?}",
        stats.with_objects, stats.total_zones, stats_cold, t.elapsed()
    );
    if warm.as_millis() > 5 {
        fail("warm level_list_planets should be instant", &mut failures);
    }

    let mut by_group: HashMap<&str, (usize, usize)> = HashMap::new();
    for r in &regions {
        let e = by_group.entry(r.group.as_str()).or_insert((0, 0));
        e.0 += 1;
        e.1 += r.zone_count;
    }
    println!("\n== regions by group ==");
    let mut groups: Vec<_> = by_group.into_iter().collect();
    groups.sort();
    for (g, (n, z)) in groups {
        println!("  {g:<9} {n:>4} regions  {z:>6} zones");
    }

    println!("\n== 12 largest regions ==");
    let mut sorted: Vec<_> = regions.iter().collect();
    sorted.sort_by(|a, b| b.zone_count.cmp(&a.zone_count));
    for r in sorted.iter().take(12) {
        let mut kinds: Vec<_> = r.kinds.iter().collect();
        kinds.sort();
        let kinds = kinds
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "  {:<52} {:<9} {:>5} zones  [{kinds}]",
            r.key, r.group, r.zone_count
        );
    }

    // ---- hasObjects --------------------------------------------------------
    // `level_list_zones` measures this instead of guessing from size or kind;
    // check the measurement against a direct parse of a spread of zones.
    println!("\n== hasObjects (sample of 400 zones) ==");
    let step = (index.zones.len() / 400).max(1);
    let (mut sampled, mut wrong) = (0usize, 0usize);
    let mut by_kind: HashMap<&str, (usize, usize, u32, u32)> = HashMap::new();
    for z in index.zones.iter().step_by(step) {
        let Ok(zone) = load(&index, &z.path) else {
            continue;
        };
        sampled += 1;
        let has = !zone.scene_objects.is_empty();
        let e = by_kind.entry(z.kind).or_insert((0, 0, u32::MAX, 0));
        if has {
            e.0 += 1;
            e.2 = e.2.min(z.size);
        } else {
            e.1 += 1;
            e.3 = e.3.max(z.size);
        }
        if has != level::zone_has_objects(&index, z.id) {
            wrong += 1;
        }
    }
    let mut ks: Vec<_> = by_kind.into_iter().collect();
    ks.sort();
    for (k, (withobj, empty, smallest, largest)) in ks {
        println!(
            "  {k:<6} with objects {withobj:>4} (smallest {} B)   without {empty:>4} (largest {largest} B)",
            if smallest == u32::MAX { 0 } else { smallest }
        );
    }
    println!("  sampled {sampled}, disagreements with a direct parse: {wrong}");
    if wrong > 0 {
        fail(
            "zone_has_objects disagrees with parsing the zone",
            &mut failures,
        );
    }

    // ---- level_list_zones on the biggest region ----------------------------
    let biggest = sorted[0];
    level::clear_probe_cache();
    let t = Instant::now();
    let listed = block_on(cmd::level_list_zones(
        game.clone(),
        biggest.key.clone(),
        Some(hashes.clone()),
    ))
    .unwrap();
    let first_pass = t.elapsed();
    let t = Instant::now();
    let again = block_on(cmd::level_list_zones(
        game.clone(),
        biggest.key.clone(),
        Some(hashes.clone()),
    ))
    .unwrap();
    println!(
        "\n== level_list_zones {} ==\n  {} zones, {} carry objects — first call {:?}, memoised call {:?}",
        biggest.key,
        listed.len(),
        listed.iter().filter(|z| z.has_objects).count(),
        first_pass,
        t.elapsed()
    );
    if listed.len() != biggest.zone_count || again.len() != listed.len() {
        fail("zone count disagrees with the region record", &mut failures);
    }

    // ---- three zones of different kinds ------------------------------------
    // The largest art / gp / lgt zone of one planet, so the sample is real
    // content rather than a hand-picked path that may go stale.
    println!("\n== level_load_zone ==");
    let mut picks: Vec<String> = Vec::new();
    for kind in ["art", "gp", "lgt"] {
        if let Some(z) = index
            .zones
            .iter()
            .filter(|z| z.region == SAMPLE_REGION && z.kind == kind)
            .max_by_key(|z| z.size)
        {
            picks.push(z.path.clone());
        }
    }

    for path in &picks {
        let t = Instant::now();
        let scene = match block_on(cmd::level_load_zone(
            game.clone(),
            path.clone(),
            Some(hashes.clone()),
        )) {
            Ok(s) => s,
            Err(e) => {
                println!("  {path}");
                fail(&e.to_string(), &mut failures);
                continue;
            }
        };
        let elapsed = t.elapsed();
        let zone = load(&index, path).unwrap();
        let entities = zone.entities().count();
        let mut kinds: HashMap<String, usize> = HashMap::new();
        for o in &scene.objects {
            *kinds.entry(o.kind_name.clone()).or_insert(0) += 1;
        }
        let mut kinds: Vec<_> = kinds.into_iter().collect();
        kinds.sort();
        println!("  {path}");
        println!(
            "    {} objects ({} model instances, {} entities), {} distinct models, {} actors, {} sections   load {:?}",
            scene.objects.len(),
            scene.objects.len() - entities,
            entities,
            scene.models.len(),
            scene.actors.len(),
            scene.sections.len(),
            elapsed
        );
        println!(
            "    kinds: {}",
            kinds
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        match &scene.bounds {
            Some(b) => println!(
                "    bounds  min [{:.1} {:.1} {:.1}]  max [{:.1} {:.1} {:.1}]",
                b.min[0], b.min[1], b.min[2], b.max[0], b.max[1], b.max[2]
            ),
            None => println!("    bounds  (none)"),
        }
        // Every model instance should resolve to a TOC asset. A handful of
        // shipped zones reference models that are not in the archives at all;
        // the viewport draws nothing for those, so they are reported rather
        // than treated as a decode failure.
        let unresolved: Vec<u64> = zone
            .model_instances()
            .map(|o| o.model_id)
            .filter(|id| index.asset(*id).is_none())
            .collect();
        let distinct_missing: HashSet<u64> = unresolved.iter().copied().collect();
        if !unresolved.is_empty() {
            let mut ids: Vec<u64> = distinct_missing.iter().copied().collect();
            ids.sort();
            println!(
                "    {} instance(s) of {} model id(s) absent from the TOC: {}",
                unresolved.len(),
                ids.len(),
                ids.iter()
                    .take(4)
                    .map(|i| format!("{i:#018X}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            for id in ids.iter().take(4) {
                println!(
                    "      {:#018X} -> {}",
                    id,
                    index.name_of(*id).unwrap_or("<no name>")
                );
            }
        }
        let ratio = unresolved.len() as f32 / zone.model_instances().count().max(1) as f32;
        if ratio > 0.02 {
            fail(
                &format!("{:.1}% of model instances do not resolve", ratio * 100.0),
                &mut failures,
            );
        }
        // The DTO must carry every scene object, not just the drawable ones.
        if scene.objects.len() != zone.scene_objects.len() {
            fail("ZoneScene.objects is missing entries", &mut failures);
        }
        let distinct: HashSet<u64> = zone.model_instances().map(|o| o.model_id).collect();
        if scene.models.len() != distinct.len() {
            fail(
                "ZoneScene.models does not match the distinct model ids",
                &mut failures,
            );
        }
    }

    // ---- mesh decode -------------------------------------------------------
    let art = picks.first().cloned().expect("an art zone");
    println!("\n== level_model_mesh: top 3 models of {art} ==");
    let scene = block_on(cmd::level_load_zone(
        game.clone(),
        art.clone(),
        Some(hashes.clone()),
    ))
    .unwrap();

    for m in scene.models.iter().take(3) {
        // Time the command itself, cold then memoised. Its ArrayBuffer body is
        // opaque here, so the bytes are read back afterwards through the same
        // decoder it calls.
        let t = Instant::now();
        if let Err(e) = block_on(cmd::level_model_mesh(game.clone(), m.id.clone(), Some(0))) {
            fail(&format!("level_model_mesh({}): {e}", m.id), &mut failures);
            continue;
        }
        let decode = t.elapsed();
        let t = Instant::now();
        let _ = block_on(cmd::level_model_mesh(game.clone(), m.id.clone(), Some(0)));
        let cached = t.elapsed();

        let mid = u64::from_str_radix(m.id.trim_start_matches("0x"), 16).unwrap();
        let bytes = level::model_mesh_bytes(&index, mid, 0);

        let u32_at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
        let f32_at = |o: usize| f32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
        let (vc, ic, sc) = (u32_at(8), u32_at(12), u32_at(16));
        let bmin = [f32_at(24), f32_at(28), f32_at(32)];
        let bmax = [f32_at(36), f32_at(40), f32_at(44)];

        println!("  {}", m.path);
        println!(
            "    {} instances, {vc} verts, {ic} indices, {sc} submeshes, {} B   decode {:?}, cached {:?}",
            m.instance_count,
            bytes.len(),
            decode,
            cached
        );
        println!(
            "    decoded bounds  min [{:.3} {:.3} {:.3}]  max [{:.3} {:.3} {:.3}]",
            bmin[0], bmin[1], bmin[2], bmax[0], bmax[1], bmax[2]
        );

        if u32_at(0) != level::MESH_MAGIC || u32_at(4) != level::MESH_VERSION {
            fail("bad magic/version", &mut failures);
            continue;
        }
        let expect_len = 48 + vc as usize * 32 + ic as usize * 4 + sc as usize * 16;
        if bytes.len() != expect_len {
            fail(
                &format!("buffer is {} B, header implies {expect_len}", bytes.len()),
                &mut failures,
            );
        }
        if vc == 0 || ic == 0 {
            fail("empty geometry", &mut failures);
            continue;
        }
        let ibase = 48 + vc as usize * 32;
        let bad = (0..ic as usize)
            .filter(|i| u32_at(ibase + i * 4) >= vc)
            .count();
        if bad > 0 {
            fail(&format!("{bad} out-of-range indices"), &mut failures);
        }
        let sbase = ibase + ic as usize * 4;
        let covered: u32 = (0..sc as usize).map(|s| u32_at(sbase + s * 16 + 4)).sum();
        if covered != ic {
            fail(
                &format!("submeshes cover {covered} of {ic} indices"),
                &mut failures,
            );
        }
        // Decoded positions must sit inside the model's own Built AABB.
        let raw = index.extract(mid).expect("extract model");
        let model = level::parse_model_asset(&raw).expect("parse model");
        match level::built_box(&model) {
            Some((bbmin, bbmax)) => {
                println!(
                    "    Built AABB      min [{:.3} {:.3} {:.3}]  max [{:.3} {:.3} {:.3}]",
                    bbmin[0], bbmin[1], bbmin[2], bbmax[0], bbmax[1], bbmax[2]
                );
                // A millimetre plus 1% of the extent of slack for the quantised
                // positions.
                let inside = (0..3).all(|k| {
                    let slack = 1e-3 + (bbmax[k] - bbmin[k]).abs() * 0.01;
                    bmin[k] >= bbmin[k] - slack && bmax[k] <= bbmax[k] + slack
                });
                println!("    inside Built AABB: {}", if inside { "yes" } else { "NO" });
                if !inside {
                    fail(
                        "decoded positions escape the model's bounding box",
                        &mut failures,
                    );
                }
            }
            None => println!("    Built AABB      (no Built section)"),
        }
    }
    let (n, b) = level::mesh_cache_stats();
    println!("  mesh cache: {n} entries, {} KiB", b / 1024);

    // ---- level_save_zone round trip ----------------------------------------
    println!("\n== level_save_zone round trip ==");
    let target_path = art.clone();
    let original = index
        .extract(omnitool_lib::core::crc64::hash(&target_path))
        .expect("extract zone");
    let base = Zone::parse(&original).expect("parse zone");
    let obj = base.scene_objects.first().expect("a scene object").clone();
    let mut moved: Vec<f32> = obj.matrix.to_vec();
    moved[12] += 1.0;

    let saved = block_on(cmd::level_save_zone(
        game.clone(),
        target_path.clone(),
        PROJECT.to_string(),
        vec![cmd::ZoneEditDto {
            offset: obj.offset,
            matrix: Some(moved),
            scale: None,
        }],
    ))
    .expect("level_save_zone");
    println!(
        "  asset {} -> {} ({} B)",
        saved.asset_path, saved.project_file, saved.bytes_written
    );

    let back = std::fs::read(&saved.project_file).expect("read back");
    match Zone::parse(&back) {
        Ok(z) => {
            let after = z.find(obj.offset).expect("edited object still present");
            let dx = after.matrix[12] - obj.matrix[12];
            println!(
                "  re-parsed: {} objects, {} actors, edited object moved {dx:+.3} on X",
                z.scene_objects.len(),
                z.actors.len()
            );
            if (dx - 1.0).abs() > 1e-4 {
                fail("translation did not survive the round trip", &mut failures);
            }
        }
        Err(e) => fail(
            &format!("written asset does not re-parse: {e}"),
            &mut failures,
        ),
    }

    if back.len() != original.len() {
        fail(
            &format!("length changed {} -> {}", original.len(), back.len()),
            &mut failures,
        );
    } else {
        let diff: Vec<usize> = (0..back.len()).filter(|&i| back[i] != original[i]).collect();
        println!(
            "  {} byte(s) differ, at {}..={} — inside the 4-byte matrix[12] field",
            diff.len(),
            diff.first().copied().unwrap_or(0),
            diff.last().copied().unwrap_or(0)
        );
        let contiguous = diff.windows(2).all(|w| w[1] == w[0] + 1);
        if diff.is_empty() || diff.len() > 4 || !contiguous {
            fail(
                "expected one contiguous run of at most 4 changed bytes",
                &mut failures,
            );
        }
    }

    // Saving with no edits must reproduce the shipped asset byte for byte.
    let untouched = block_on(cmd::level_save_zone(
        game.clone(),
        target_path.clone(),
        PROJECT.to_string(),
        Vec::new(),
    ))
    .expect("level_save_zone (no edits)");
    if std::fs::read(&untouched.project_file).unwrap() == original {
        println!("  saving with no edits reproduces the shipped asset exactly");
    } else {
        fail("an unedited save is not byte-identical", &mut failures);
    }

    // The project must be laid out the way the Stager expects: info.json at the
    // root and the asset under the "0/" span folder.
    let project_dir = omnitool_lib::core::filesystem::projects_dir()
        .unwrap()
        .join(PROJECT);
    let expected = project_dir.join("0").join(&target_path);
    println!(
        "  project layout: info.json {}, asset at 0/<path> {}",
        project_dir.join("info.json").exists(),
        expected.exists()
    );
    if !project_dir.join("info.json").exists() || !expected.exists() {
        fail("project layout does not match the Stager's", &mut failures);
    }
    let _ = std::fs::remove_dir_all(&project_dir);

    println!(
        "\n{}",
        if failures == 0 {
            "ALL CHECKS PASSED".to_string()
        } else {
            format!("{failures} CHECK(S) FAILED")
        }
    );
    std::process::exit(if failures == 0 { 0 } else { 1 });
}
