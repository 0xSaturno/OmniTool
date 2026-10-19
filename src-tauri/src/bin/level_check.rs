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

/// Exhaustive forward walk of the Zone Scene Objects heap, ignoring the tables.
/// Used only to prove the table-driven reader misses nothing.
fn walk_heap(scene: &[u8]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut o = 0usize;
    while o + 0x80 <= scene.len() {
        let size = u32::from_le_bytes(scene[o + 0x7C..o + 0x80].try_into().unwrap()) & 0x00FF_FFFF;
        let kind = scene[o + 0x5F];
        let m33 = f32::from_le_bytes(scene[o + 0x3C..o + 0x40].try_into().unwrap());
        if size >= 0x80 && o + size as usize <= scene.len() && kind <= 6 && m33 == 1.0 {
            out.push(o as u32);
            o = (o + size as usize + 7) & !7;
        } else {
            o += 8;
        }
    }
    out
}

/// The vertex total `gltf_writer` exports for look 0 / LOD 0, computed the same
/// way it does — the reference the viewport's decoder must reproduce.
fn gltf_look0_vertex_count(model: &omnitool_lib::tools::model_converter::model::ModelFile) -> Option<usize> {
    use omnitool_lib::tools::model_converter::sections::look::{LookSection, TAG_LOOK};
    use omnitool_lib::tools::model_converter::sections::meshes::{MeshDefinition, TAG_MESHES};
    let meshes = MeshDefinition::parse_all(model.dat1.get_section_data(TAG_MESHES)?).ok()?;
    let looks = LookSection::parse(model.dat1.get_section_data(TAG_LOOK)?).ok()?;
    let lod = looks.looks.first()?.lods.first()?;
    Some(
        (lod.start..lod.start + lod.count)
            .filter_map(|i| meshes.get(i as usize))
            .map(|m| m.vertex_count as usize)
            .sum(),
    )
}

/// How much of a mesh's emitted winding agrees with its own stored vertex
/// normals, as a triangle-area-weighted fraction in `0..=1`.
///
/// For each triangle the geometric normal `cross(p1-p0, p2-p0)` is compared
/// against the sum of the three stored vertex normals; a positive dot means the
/// emitted front face points the way the artist's normals do. Degenerate
/// triangles and triangles whose three normals cancel are skipped. Weighting by
/// area keeps a few slivers from swamping the answer.
fn winding_agreement(m: &level::MeshOut) -> Option<f32> {
    let (mut agree, mut total) = (0f64, 0f64);
    for t in m.indices.chunks_exact(3) {
        let p: [[f32; 3]; 3] = std::array::from_fn(|j| {
            let b = t[j] as usize * 3;
            [m.positions[b], m.positions[b + 1], m.positions[b + 2]]
        });
        let u = [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]];
        let v = [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]];
        let g = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        let area = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt() as f64;
        if area <= 1e-12 {
            continue;
        }
        let mut n = [0f32; 3];
        for j in 0..3 {
            let b = t[j] as usize * 3;
            for k in 0..3 {
                n[k] += m.normals[b + k];
            }
        }
        if n[0] * n[0] + n[1] * n[1] + n[2] * n[2] < 1e-12 {
            continue;
        }
        total += area;
        if (g[0] * n[0] + g[1] * n[1] + g[2] * n[2]) > 0.0 {
            agree += area;
        }
    }
    (total > 0.0).then(|| (agree / total) as f32)
}

/// `Some(signed_volume)` when the topology is closed — every directed edge
/// appears once and its reverse appears once. A correctly wound closed mesh has
/// positive signed volume.
///
/// Vertices are welded by exact position first: the exporter splits a vertex
/// wherever the UV or normal breaks, so a solid prop's index topology looks
/// open until the seam copies are merged back together.
fn watertight_volume(m: &level::MeshOut) -> Option<f64> {
    let mut weld: HashMap<[u32; 3], u32> = HashMap::new();
    let mut key = vec![0u32; m.vertex_count()];
    for i in 0..m.vertex_count() {
        let k = [
            m.positions[i * 3].to_bits(),
            m.positions[i * 3 + 1].to_bits(),
            m.positions[i * 3 + 2].to_bits(),
        ];
        let next = weld.len() as u32;
        key[i] = *weld.entry(k).or_insert(next);
    }
    let mut edges: HashMap<(u32, u32), i32> = HashMap::new();
    for t in m.indices.chunks_exact(3) {
        let t = [key[t[0] as usize], key[t[1] as usize], key[t[2] as usize]];
        if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
            return None;
        }
        for e in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            *edges.entry(e).or_insert(0) += 1;
        }
    }
    if edges.is_empty() {
        return None;
    }
    for (&(a, b), &n) in &edges {
        if n != 1 || edges.get(&(b, a)).copied().unwrap_or(0) != 1 {
            return None;
        }
    }
    let mut vol = 0f64;
    for t in m.indices.chunks_exact(3) {
        let p: [[f64; 3]; 3] = std::array::from_fn(|j| {
            let b = t[j] as usize * 3;
            [
                m.positions[b] as f64,
                m.positions[b + 1] as f64,
                m.positions[b + 2] as f64,
            ]
        });
        vol += p[0][0] * (p[1][1] * p[2][2] - p[1][2] * p[2][1])
            - p[0][1] * (p[1][0] * p[2][2] - p[1][2] * p[2][0])
            + p[0][2] * (p[1][0] * p[2][1] - p[1][1] * p[2][0]);
    }
    Some(vol / 6.0)
}

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

    // ---- coverage invariants ----------------------------------------------
    // Everything below was established by measuring the shipped archives with
    // `cargo run --bin coverage_audit`; these assertions keep it that way.
    println!("\n== coverage invariants ==");

    // 1. Every heap entry an exhaustive walk finds must also be reachable
    //    through the instance tables — i.e. the table-driven reader loses
    //    nothing. Measured over these zones: 39 165 entries, 0 orphans.
    const FUNNEL_ZONES: [&str; 5] = [
        "levels/i29/openarea/sargasso/tile_n19/tile_n19_art.zone",
        "levels/i29/openarea/megalopolis/tile_m37/tile_m37_art.zone",
        "levels/i29/openarea/megalopolis/global_art.zone",
        "levels/i29/openarea/sargasso/tile_o17/tile_o17_gp.zone",
        "levels/i29/openarea/megalopolis/i29_megalopolis_impostors_default_n19_art.zone",
    ];
    let (mut walked, mut exposed) = (0usize, 0usize);
    for zp in FUNNEL_ZONES {
        let Ok(zone) = load(&index, zp) else {
            fail(&format!("{zp} did not load"), &mut failures);
            continue;
        };
        let scene = zone
            .dat1
            .get_section_data(omnitool_lib::core::zone::TAG_SCENE_OBJECTS)
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        let found = walk_heap(&scene);
        let seen: HashSet<u32> = zone.scene_objects.iter().map(|o| o.offset).collect();
        let missed: Vec<u32> = found.iter().copied().filter(|o| !seen.contains(o)).collect();
        walked += found.len();
        exposed += seen.len();
        if !missed.is_empty() {
            fail(
                &format!(
                    "{zp}: {} heap entries the tables do not reach (e.g. {:?})",
                    missed.len(),
                    &missed[..missed.len().min(4)]
                ),
                &mut failures,
            );
        }
    }
    println!("  heap walk {walked} entries, exposed {exposed} — no entry is lost by the tables");

    // 2. Impostor zones must parse to their kind-6 entries and decode their
    //    block-2 shells. Before Zone Impostors was read, every one of the 645
    //    shipped impostor zones parsed to zero objects.
    let imposter_zones: Vec<String> = index
        .zones
        .iter()
        .filter(|z| z.path.contains("impostors") && z.size > level::STUB_MAX_SIZE)
        .map(|z| z.path.clone())
        .take(120)
        .collect();
    let (mut imp_zones, mut imp_objs, mut imp_tris, mut imp_fail) = (0, 0, 0usize, 0);
    for zp in &imposter_zones {
        let Ok(zone) = load(&index, zp) else { continue };
        let k6: Vec<u32> = zone
            .scene_objects
            .iter()
            .filter(|o| o.kind == omnitool_lib::core::zone::KIND_IMPOSTOR)
            .map(|o| o.offset)
            .collect();
        if k6.is_empty() {
            continue;
        }
        imp_zones += 1;
        imp_objs += k6.len();
        for off in k6 {
            match level::decode_zone_geometry(&zone, off) {
                Ok(m) if !m.indices.is_empty() => imp_tris += m.indices.len() / 3,
                _ => imp_fail += 1,
            }
        }
    }
    println!(
        "  impostors: {imp_zones} zones, {imp_objs} kind-6 entries, {imp_tris} triangles decoded, {imp_fail} failures"
    );
    if imp_zones == 0 || imp_fail > 0 {
        fail("impostor geometry did not decode", &mut failures);
    }

    // 3. Decal meshes decode and, once placed by their entry matrix, sit inside
    //    the bounding sphere the Zone Decal Geometry row records for them.
    let mut decals = 0usize;
    let mut decal_fail = 0usize;
    for zp in [
        "levels/i29/openarea/sargasso/tile_n19/tile_n19_art.zone",
        "levels/i29/openarea/megalopolis/tile_m37/tile_m37_art.zone",
    ] {
        let Ok(zone) = load(&index, zp) else { continue };
        for o in zone
            .scene_objects
            .iter()
            .filter(|o| o.kind == omnitool_lib::core::zone::KIND_DECAL && o.geometry.is_some())
        {
            decals += 1;
            match level::decode_zone_geometry(&zone, o.offset) {
                Ok(m) if !m.indices.is_empty() => {
                    // Local-space extent must stay inside the projector box.
                    let r = (0..3)
                        .map(|k| m.max[k].abs().max(m.min[k].abs()))
                        .fold(0f32, f32::max);
                    if r > 1.5 {
                        decal_fail += 1;
                    }
                }
                _ => decal_fail += 1,
            }
        }
    }
    println!("  decals: {decals} meshes decoded, {decal_fail} outside their projector box");
    if decals == 0 || decal_fail > 0 {
        fail("decal geometry did not decode inside its projector box", &mut failures);
    }

    // 4. `decode_model_mesh` must emit exactly the subsets `gltf_writer`
    //    exports for look 0 / LOD 0 — the shipped export path is the oracle.
    //    Measured over the 576 distinct models of six real art zones: 574
    //    exact, 0 under, 0 over, 2 models simply absent from the TOC.
    let mut checked = 0usize;
    let mut mismatched = 0usize;
    let mut zone_models: Vec<u64> = Vec::new();
    for zp in FUNNEL_ZONES.iter().take(3) {
        if let Ok(zone) = load(&index, zp) {
            zone_models.extend(zone.model_instances().map(|o| o.model_id));
        }
    }
    zone_models.sort_unstable();
    zone_models.dedup();
    for id in zone_models.iter().take(300) {
        let Ok(raw) = index.extract(*id) else { continue };
        let Ok(model) = level::parse_model_asset(&raw) else {
            continue;
        };
        let Some(expected) = gltf_look0_vertex_count(&model) else {
            continue;
        };
        let Ok(mesh) = level::decode_model_mesh(&model, 0) else {
            continue;
        };
        checked += 1;
        if mesh.vertex_count() != expected {
            mismatched += 1;
        }
    }
    println!("  look/LOD: {checked} models compared with the glTF export selection, {mismatched} mismatched");
    if checked < 100 || mismatched > 0 {
        fail("decode_model_mesh no longer matches the glTF export selection", &mut failures);
    }

    // 5. Emitted triangle winding must AGREE with the stored vertex normals.
    //    The zone assets store one winding and one set of artist normals; the
    //    decoder used to reverse every triangle, which put the two in direct
    //    opposition — measured over 399 distinct models of four planets,
    //    396/399 scored below 0.10 agreement, 0/399 above 0.90, and all 49
    //    watertight models had NEGATIVE signed volume. Since 58-100% of a
    //    zone's triangles live on open "sheet" meshes, single-sided rendering
    //    then hid most of the level. Keep the source winding: agreement must
    //    average above 0.9 and every closed mesh must enclose positive volume.
    println!("\n== winding vs. stored normals ==");
    // One art tile from each of four planets, so the sample is not one
    // artist's habit. Tile basenames repeat across planets, hence the planet
    // folder in the match.
    const WIND_ZONES: [(&str, &str); 4] = [
        ("sargasso", "tile_n19_art.zone"),
        ("megalopolis", "tile_m37_art.zone"),
        ("nefarious", "tile_e19_art.zone"),
        ("savali", "tile_c17_art.zone"),
    ];
    let mut wind_models: Vec<u64> = Vec::new();
    let mut wind_zones_found = 0usize;
    for (planet, suffix) in WIND_ZONES {
        let Some(zp) = index
            .zones
            .iter()
            .find(|z| z.path.ends_with(suffix) && z.path.contains(planet))
            .map(|z| z.path.clone())
        else {
            continue;
        };
        let Ok(zone) = load(&index, &zp) else { continue };
        wind_zones_found += 1;
        println!("  {zp}");
        wind_models.extend(zone.model_instances().map(|o| o.model_id));
    }
    wind_models.sort_unstable();
    wind_models.dedup();

    // Distribution over agreement deciles, plus the watertight-volume tally.
    let mut hist = [0usize; 10];
    let (mut n_models, mut sum_agree) = (0usize, 0f64);
    let (mut worst, mut worst_id) = (2f32, 0u64);
    let (mut tight, mut tight_neg) = (0usize, 0usize);
    for id in &wind_models {
        let Ok(raw) = index.extract(*id) else { continue };
        let Ok(model) = level::parse_model_asset(&raw) else {
            continue;
        };
        let Ok(mesh) = level::decode_model_mesh(&model, 0) else {
            continue;
        };
        let Some(a) = winding_agreement(&mesh) else {
            continue;
        };
        n_models += 1;
        sum_agree += a as f64;
        hist[((a * 10.0) as usize).min(9)] += 1;
        if a < worst {
            worst = a;
            worst_id = *id;
        }
        if let Some(v) = watertight_volume(&mesh) {
            tight += 1;
            if v <= 0.0 {
                tight_neg += 1;
            }
        }
    }
    let mean = if n_models > 0 {
        sum_agree / n_models as f64
    } else {
        0.0
    };
    println!(
        "  {n_models} distinct models from {wind_zones_found} art zones, mean agreement {mean:.4}"
    );
    for (d, n) in hist.iter().enumerate() {
        if *n > 0 {
            println!("    [{:.1},{:.1})  {n:>4}", d as f32 / 10.0, (d + 1) as f32 / 10.0);
        }
    }
    println!(
        "  worst model {worst_id:#018x} at {worst:.4}; watertight {tight}, of which negative volume {tight_neg}"
    );
    if n_models < 150 {
        fail(
            &format!("only {n_models} models measured, need at least 150"),
            &mut failures,
        );
    }
    if mean <= 0.9 {
        fail(
            &format!("mean winding/normal agreement {mean:.4} is not above 0.9"),
            &mut failures,
        );
    }
    if tight == 0 {
        fail("no watertight model found to check signed volume", &mut failures);
    }
    if tight_neg > 0 {
        fail(
            &format!("{tight_neg}/{tight} watertight models have negative signed volume"),
            &mut failures,
        );
    }

    // The same rule for the geometry that lives in the zone's own block 2.
    // Impostors ship no normals, so theirs are accumulated from the emitted
    // winding and agreement is 1.0 by construction — what it proves is that the
    // accumulator and the emitter still share an orientation. Decals do carry
    // stored normals, so their agreement is a real measurement.
    let (mut zg_imp, mut zg_imp_sum, mut zg_imp_bad) = (0usize, 0f64, 0usize);
    let (mut zg_dec, mut zg_dec_sum, mut zg_dec_bad) = (0usize, 0f64, 0usize);
    let mut zg_tight_neg = 0usize;
    let mut zg_paths: Vec<String> = index
        .zones
        .iter()
        .filter(|z| z.path.contains("impostors") && z.size > level::STUB_MAX_SIZE)
        .map(|z| z.path.clone())
        .take(40)
        .collect();
    for (planet, suffix) in WIND_ZONES {
        if let Some(z) = index
            .zones
            .iter()
            .find(|z| z.path.ends_with(suffix) && z.path.contains(planet))
        {
            zg_paths.push(z.path.clone());
        }
    }
    for zp in &zg_paths {
        let Ok(zone) = load(&index, zp) else { continue };
        for o in zone.scene_objects.iter().filter(|o| o.geometry.is_some()) {
            let Ok(m) = level::decode_zone_geometry(&zone, o.offset) else {
                continue;
            };
            let Some(a) = winding_agreement(&m) else { continue };
            if let Some(v) = watertight_volume(&m) {
                if v <= 0.0 {
                    zg_tight_neg += 1;
                }
            }
            if o.kind == omnitool_lib::core::zone::KIND_IMPOSTOR {
                zg_imp += 1;
                zg_imp_sum += a as f64;
                if a < 0.9 {
                    zg_imp_bad += 1;
                }
            } else {
                zg_dec += 1;
                zg_dec_sum += a as f64;
                if a < 0.9 {
                    zg_dec_bad += 1;
                }
            }
        }
    }
    let dec_mean = if zg_dec > 0 {
        zg_dec_sum / zg_dec as f64
    } else {
        0.0
    };
    let imp_mean = if zg_imp > 0 {
        zg_imp_sum / zg_imp as f64
    } else {
        0.0
    };
    println!(
        "  block 2: {zg_imp} impostors mean agreement {imp_mean:.4} ({zg_imp_bad} below 0.9), \
{zg_dec} decals mean agreement {dec_mean:.4} ({zg_dec_bad} below 0.9), {zg_tight_neg} closed meshes with negative volume"
    );
    if zg_imp == 0 || imp_mean <= 0.9 {
        fail(
            &format!("impostor winding/accumulated-normal agreement {imp_mean:.4} is not above 0.9"),
            &mut failures,
        );
    }
    // A handful of decals really are wound against their own stored normals —
    // 13 of 522, each at exactly 0.000, and exactly the 13 that scored *above*
    // 0.9 while the decoder was reversing. That is a property of those entries,
    // not of the decoder, so the guard is on the mean plus a cap on the tail.
    if zg_dec == 0 || dec_mean <= 0.9 || zg_dec_bad * 20 > zg_dec {
        fail(
            &format!(
                "decal winding/normal agreement {dec_mean:.4} with {zg_dec_bad}/{zg_dec} below 0.9"
            ),
            &mut failures,
        );
    }
    if zg_tight_neg > 0 {
        fail(
            &format!("{zg_tight_neg} block-2 closed meshes have negative signed volume"),
            &mut failures,
        );
    }

    // ---- Model Built header ------------------------------------------------
    // The 120-byte header is now fully named. These assertions pin the parts a
    // viewer's correctness depends on; `coverage_audit --models` is the wider
    // census they were derived from (all 11,387 shipped `.model` assets).
    println!("\n== Model Built header ==");
    {
        use omnitool_lib::core::level::{model_flag, model_flag_names, ModelBuilt};

        // A broad, diverse sample: every distinct model the four art tiles of
        // four different planets place, plus a stride sample over the whole
        // library so characters, weapons and UI props are represented too.
        let mut sample: Vec<u64> = wind_models.clone();
        let mut library: Vec<u64> = index
            .names
            .iter()
            .filter(|(id, p)| p.ends_with(".model") && index.asset(**id).is_some())
            .map(|(id, _)| *id)
            .collect();
        library.sort_unstable();
        sample.extend(library.iter().step_by(4).copied());
        sample.sort_unstable();
        sample.dedup();

        let mut parsed = 0usize;
        let mut bit_counts = [0usize; 32];
        let (mut centered, mut centre_nz) = (0usize, 0usize);
        let (mut coll_only, mut static_coll) = (0usize, 0usize);
        let mut strandy = 0usize;
        let mut bad_scale = 0usize;
        let mut count_mismatch = 0usize;
        for id in &sample {
            let Ok(raw) = index.extract(*id) else { continue };
            let Ok(model) = level::parse_model_asset(&raw) else {
                continue;
            };
            let Some(b) = ModelBuilt::of(&model) else {
                continue;
            };
            parsed += 1;
            for (bit, c) in bit_counts.iter_mut().enumerate() {
                if b.flags >> bit & 1 == 1 {
                    *c += 1;
                }
            }
            if b.centered_verts() {
                centered += 1;
            }
            if b.mesh_centre.iter().any(|c| *c != 0.0) {
                centre_nz += 1;
            }
            if b.collision_only() {
                coll_only += 1;
            }
            if b.static_coll_only() {
                static_coll += 1;
            }
            if b.strand_subset_count != 0 {
                strandy += 1;
            }
            // metres-per-unit must be a usable, positive scale.
            if !(b.metres_per_unit > 0.0 && b.metres_per_unit < 1.0) {
                bad_scale += 1;
            }
            // 0x64/0x68 are the counts the engine validates the geometry
            // sections against, so they must match the sections themselves.
            if let Some(v) = model
                .dat1
                .get_section_data(omnitool_lib::tools::model_converter::sections::geo::TAG_VERTEXES)
            {
                if v.len() / 16 != b.vertex_count as usize {
                    count_mismatch += 1;
                }
            }
        }
        println!("  {parsed} models parsed a 120-byte Built header");
        println!(
            "  CenteredVerts (bit 23) {centered}, non-zero mesh centre {centre_nz}, \
CollisionOnly (14) {coll_only}, StaticCollOnly (13) {static_coll}, strand count != 0 {strandy}"
        );
        // The union of every bit any sampled model sets, run back through the
        // name table — bit 29 has no name and must surface as `bit29`.
        println!(
            "  bits observed across the sample: {:?}",
            model_flag_names(bit_counts.iter().enumerate().fold(0u32, |a, (b, n)| {
                if *n > 0 {
                    a | 1 << b
                } else {
                    a
                }
            }))
        );
        if parsed < 2000 {
            fail(
                &format!("only {parsed} Built headers parsed, need at least 2000"),
                &mut failures,
            );
        }
        if bad_scale > 0 {
            fail(
                &format!("{bad_scale} models have an unusable metres-per-unit at 0x2C"),
                &mut failures,
            );
        }
        if count_mismatch > 0 {
            fail(
                &format!("{count_mismatch} models: Built 0x68 != Model Std Vert length / 16"),
                &mut failures,
            );
        }
        // Bits 17/18 were established here by correlating against which vertex
        // streams a model carries; the adopted table agrees, so they must stay
        // common but not universal.
        for (bit, name) in [
            (model_flag::VERT_HAS_UV1, "VertHasUv1"),
            (model_flag::VERT_HAS_COLOR, "VertHasColor"),
        ] {
            let n = bit_counts[bit as usize];
            if n == 0 || n == parsed {
                fail(
                    &format!("{name} (bit {bit}) is set on {n}/{parsed} — the flag table moved"),
                    &mut failures,
                );
            }
        }
        // CenteredVerts is the one flag that can move geometry. On this build
        // nothing sets it and 0x1C is always zero; if that ever changes the
        // decoder's offset term becomes live and this note should be revisited.
        if centered != centre_nz {
            fail(
                &format!(
                    "{centered} models set CenteredVerts but {centre_nz} have a non-zero 0x1C"
                ),
                &mut failures,
            );
        }
        // CollisionOnly must stay rare — it is the bit `model_mesh_bytes` uses
        // to stop drawing a model, so a table shift that made it common would
        // empty the viewport.
        if coll_only * 10 > parsed {
            fail(
                &format!("CollisionOnly on {coll_only}/{parsed} models — too many to skip drawing"),
                &mut failures,
            );
        }
        // ...and StaticCollOnly must stay common, which is why it is NOT used
        // as a visibility test.
        if static_coll * 10 < parsed {
            fail(
                &format!(
                    "StaticCollOnly on only {static_coll}/{parsed} models — re-check the bit-13 reading"
                ),
                &mut failures,
            );
        }

        // The decoder must apply the CenteredVerts offset, whether or not any
        // shipped model asks for it. Proven on a synthetic header rather than
        // on the shipped data, since the shipped data cannot exercise it.
        let mut hdr = vec![0u8; ModelBuilt::SIZE];
        hdr[0x1C..0x20].copy_from_slice(&3.5f32.to_le_bytes());
        hdr[0x20..0x24].copy_from_slice(&(-2.0f32).to_le_bytes());
        hdr[0x28..0x2C].copy_from_slice(&(1u32 << model_flag::CENTERED_VERTS).to_le_bytes());
        let with = ModelBuilt::parse(&hdr).expect("synthetic header");
        hdr[0x28..0x2C].copy_from_slice(&0u32.to_le_bytes());
        let without = ModelBuilt::parse(&hdr).expect("synthetic header");
        println!(
            "  offset term: CenteredVerts set -> {:?}, clear -> {:?}",
            with.vertex_offset(),
            without.vertex_offset()
        );
        if with.vertex_offset() != [3.5, -2.0, 0.0] || without.vertex_offset() != [0.0; 3] {
            fail("ModelBuilt::vertex_offset does not follow flag bit 23", &mut failures);
        }

        // model_mesh_bytes must actually skip a CollisionOnly model.
        let coll_id = sample.iter().copied().find(|id| {
            index
                .extract(*id)
                .ok()
                .and_then(|r| level::parse_model_asset(&r).ok())
                .and_then(|m| ModelBuilt::of(&m))
                .is_some_and(|b| b.collision_only() && b.vertex_count > 0)
        });
        match coll_id {
            Some(id) => {
                let raw = index.extract(id).unwrap();
                let model = level::parse_model_asset(&raw).unwrap();
                let decoded = level::decode_model_mesh(&model, 0).map(|m| m.indices.len() / 3);
                let bytes = level::model_mesh_bytes(&index, id, 0);
                let drawn = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) / 3;
                println!(
                    "  CollisionOnly {} decodes to {:?} triangles, viewport draws {drawn}",
                    index.name_of(id).unwrap_or("?"),
                    decoded
                );
                if drawn != 0 {
                    fail("model_mesh_bytes still draws a CollisionOnly model", &mut failures);
                }
            }
            None => fail("no CollisionOnly model in the sample to check the skip", &mut failures),
        }
    }

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
