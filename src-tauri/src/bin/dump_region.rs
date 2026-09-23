//! Dev helper: dump a WHOLE region (planet) in one pass, exactly as the Level
//! Editor's Tauri commands would hand it to the front end, so the real viewport
//! can be driven against real game data without a Tauri host.
//!
//!   cargo run --release --bin dump_region -- "<game dir>" <region key> <out dir>
//!
//! For every loadable zone of the region it writes
//!   scenes/z<NNN>.json   the exact `level_load_zone` JSON (compact)
//!   block2/z<NNN>.bin    every block-2 mesh of that zone (kind 3 decals and
//!                        kind 6 impostor shells) concatenated, the same RCMS
//!                        buffers `level_zone_geometry` returns, with the byte
//!                        spans recorded per scene-object offset in the manifest
//! and, once per distinct model the region uses,
//!   meshes/<id>.bin      the RCMS buffer `level_model_mesh` returns
//!
//! plus `manifest.json` (zone list, per-zone block-2 index, totals) and
//! `links.json` (the level asset's named links with world positions, so a
//! camera can be aimed at a real place).
//!
//! The asset name list comes from $RCRA_HASHES, else OmniTool's app-dir file.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;
use std::time::Instant;

use rayon::prelude::*;
use serde_json::json;

use omnitool_lib::commands_level_editor as cle;
use omnitool_lib::core::{level, level_asset::LevelAsset};

fn main() {
    let mut args = std::env::args().skip(1);
    let game = args
        .next()
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let region_key = args
        .next()
        .unwrap_or_else(|| "i29/openarea/nefarious_city".into());
    let out_dir = args.next().unwrap_or_else(|| "devdata_region".into());
    // Optional 4th argument: only dump zones whose path contains this substring.
    let filter = args.next();

    let hashes = std::env::var("RCRA_HASHES").unwrap_or_else(|_| {
        omnitool_lib::core::filesystem::hashes_path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    });

    for sub in ["scenes", "block2", "meshes"] {
        std::fs::create_dir_all(format!("{out_dir}/{sub}")).expect("create out dir");
    }

    let t0 = Instant::now();
    let idx = level::get_index(&game, Some(&hashes)).expect("index");
    idx.require_names().expect("hashes file");
    println!("index ready in {:?}", t0.elapsed());

    // ---- region + zone list -------------------------------------------------
    if !idx.regions.iter().any(|r| r.key == region_key) {
        eprintln!("region {region_key:?} not found. Candidates:");
        for r in idx.regions.iter().filter(|r| r.group == "openarea") {
            eprintln!("  {} ({} zones)", r.key, r.zone_count);
        }
        std::process::exit(2);
    }

    let t_list = Instant::now();
    let zones = tauri::async_runtime::block_on(cle::level_list_zones(
        game.clone(),
        region_key.clone(),
        Some(hashes.clone()),
    ))
    .expect("list zones");
    let listed = zones.len();
    let loadable: Vec<&cle::ZoneEntry> = zones
        .iter()
        .filter(|z| z.has_objects)
        .filter(|z| filter.as_ref().is_none_or(|f| z.path.contains(f.as_str())))
        .collect();
    println!(
        "{region_key}: {listed} zones listed, {} loadable (has_objects) in {:?}",
        loadable.len(),
        t_list.elapsed()
    );

    // ---- per-zone dump ------------------------------------------------------
    let t_zones = Instant::now();
    let model_ids: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
    let rows: Mutex<Vec<serde_json::Value>> = Mutex::new(Vec::new());
    let totals = Mutex::new(Totals::default());

    loadable.par_iter().enumerate().for_each(|(i, entry)| {
        let zone = match level::get_index(&game, Some(&hashes))
            .ok()
            .and_then(|ix| load_zone(&ix, &entry.path))
        {
            Some(z) => z,
            None => {
                eprintln!("  FAILED to parse {}", entry.path);
                return;
            }
        };

        let scene = match tauri::async_runtime::block_on(cle::level_load_zone(
            game.clone(),
            entry.path.clone(),
            Some(hashes.clone()),
        )) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("  FAILED to load {}: {e}", entry.path);
                return;
            }
        };

        let scene_file = format!("scenes/z{i:04}.json");
        let bytes = serde_json::to_vec(&scene).expect("serialize scene");
        let scene_bytes = bytes.len();
        std::fs::write(format!("{out_dir}/{scene_file}"), &bytes).expect("write scene");

        {
            let mut m = model_ids.lock().unwrap();
            for md in &scene.models {
                m.insert(md.id.to_lowercase());
            }
        }

        // Block-2 geometry, packed one file per zone with a span index, so the
        // harness makes one fetch per zone instead of one per impostor.
        let mut pack: Vec<u8> = Vec::new();
        let mut index: BTreeMap<String, [usize; 2]> = BTreeMap::new();
        let mut claimed = 0usize;
        let mut decoded = 0usize;
        let mut b2_tris = 0usize;
        for o in &scene.objects {
            let has = o
                .geometry
                .as_ref()
                .map(|g| g.index_count >= 3 && g.vertex_count >= 3)
                .unwrap_or(false);
            if !has || (o.kind != 3 && o.kind != 6) {
                continue;
            }
            claimed += 1;
            match level::decode_zone_geometry(&zone, o.offset) {
                Ok(mesh) => {
                    let packed = mesh.pack();
                    b2_tris += mesh.indices.len() / 3;
                    index.insert(o.offset.to_string(), [pack.len(), packed.len()]);
                    pack.extend_from_slice(&packed);
                    decoded += 1;
                }
                Err(_) => { /* left out of the index: harness keeps the marker box */ }
            }
        }
        let block_file = if pack.is_empty() {
            None
        } else {
            let f = format!("block2/z{i:04}.bin");
            std::fs::write(format!("{out_dir}/{f}"), &pack).expect("write block2 pack");
            Some(f)
        };

        let model_instances = scene.objects.iter().filter(|o| o.kind == 0).count();
        {
            let mut t = totals.lock().unwrap();
            t.zones += 1;
            t.objects += scene.objects.len();
            t.model_instances += model_instances;
            t.actors += scene.actors.len();
            t.b2_claimed += claimed;
            t.b2_decoded += decoded;
            t.b2_triangles += b2_tris;
            t.b2_bytes += pack.len();
            t.scene_bytes += scene_bytes;
        }

        rows.lock().unwrap().push(json!({
            "i": i,
            "path": entry.path,
            "kind": entry.kind,
            "tile": entry.tile,
            "scene": scene_file,
            "block2": block_file,
            "block2Index": index,
            "objects": scene.objects.len(),
            "modelInstances": model_instances,
            "blockGeometryClaimed": claimed,
            "blockGeometryDecoded": decoded,
        }));
    });

    let mut rows = rows.into_inner().unwrap();
    rows.sort_by_key(|r| r["i"].as_u64().unwrap_or(0));
    let totals = totals.into_inner().unwrap();
    println!(
        "zones dumped in {:?}: {} zones, {} objects, {} model instances, {} actors",
        t_zones.elapsed(),
        totals.zones,
        totals.objects,
        totals.model_instances,
        totals.actors
    );
    println!(
        "  block-2: {}/{} decoded, {} triangles, {:.1} MiB packed",
        totals.b2_decoded,
        totals.b2_claimed,
        totals.b2_triangles,
        totals.b2_bytes as f64 / (1024.0 * 1024.0)
    );

    // ---- distinct model meshes ---------------------------------------------
    let t_mesh = Instant::now();
    let ids: Vec<String> = {
        let m = model_ids.into_inner().unwrap();
        let mut v: Vec<String> = m.into_iter().collect();
        v.sort();
        v
    };
    let mesh_stats: Mutex<(usize, usize, usize)> = Mutex::new((0, 0, 0)); // bytes, tris, empty
    let mesh_rows: Mutex<HashMap<String, usize>> = Mutex::new(HashMap::new());
    ids.par_iter().for_each(|id| {
        let num = u64::from_str_radix(id.trim_start_matches("0x"), 16).expect("model id");
        let bytes = level::model_mesh_bytes(&idx, num, 0);
        let tris = rcms_triangles(&bytes);
        std::fs::write(format!("{out_dir}/meshes/{id}.bin"), &*bytes).expect("write mesh");
        let mut s = mesh_stats.lock().unwrap();
        s.0 += bytes.len();
        s.1 += tris;
        if tris == 0 {
            s.2 += 1;
        }
        mesh_rows.lock().unwrap().insert(id.clone(), tris);
    });
    let (mesh_bytes, mesh_tris, mesh_empty) = mesh_stats.into_inner().unwrap();
    println!(
        "models dumped in {:?}: {} distinct, {:.1} MiB, {} unique triangles, {} decoded empty",
        t_mesh.elapsed(),
        ids.len(),
        mesh_bytes as f64 / (1024.0 * 1024.0),
        mesh_tris,
        mesh_empty
    );

    // ---- named links from the level asset ----------------------------------
    let links = dump_links(&idx, &region_key);
    std::fs::write(
        format!("{out_dir}/links.json"),
        serde_json::to_vec(&links).unwrap(),
    )
    .unwrap();
    println!("named links: {}", links.len());

    // ---- manifest -----------------------------------------------------------
    let manifest = json!({
        "region": region_key,
        "gameDir": game,
        "zonesListed": listed,
        "zonesDumped": totals.zones,
        "objects": totals.objects,
        "modelInstances": totals.model_instances,
        "actors": totals.actors,
        "blockGeometryClaimed": totals.b2_claimed,
        "blockGeometryDecoded": totals.b2_decoded,
        "blockGeometryTriangles": totals.b2_triangles,
        "distinctModels": ids.len(),
        "modelTriangles": mesh_tris,
        "modelsDecodedEmpty": mesh_empty,
        "sceneBytes": totals.scene_bytes,
        "meshBytes": mesh_bytes,
        "block2Bytes": totals.b2_bytes,
        "elapsedSeconds": t0.elapsed().as_secs_f64(),
        "zones": rows,
        "modelTriangleCounts": mesh_rows.into_inner().unwrap(),
    });
    std::fs::write(
        format!("{out_dir}/manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();

    println!("total {:?} -> {out_dir}/manifest.json", t0.elapsed());
}

#[derive(Default)]
struct Totals {
    zones: usize,
    objects: usize,
    model_instances: usize,
    actors: usize,
    b2_claimed: usize,
    b2_decoded: usize,
    b2_triangles: usize,
    b2_bytes: usize,
    scene_bytes: usize,
}

fn load_zone(idx: &level::LevelIndex, path: &str) -> Option<omnitool_lib::core::zone::Zone> {
    let norm = omnitool_lib::core::crc64::normalize_path(path);
    let id = omnitool_lib::core::crc64::hash_raw(norm.as_bytes());
    let raw = idx.extract(id).ok()?;
    omnitool_lib::core::zone::Zone::parse(&raw).ok()
}

/// `indexCount / 3` straight out of the RCMS header (magic, version, vertexCount,
/// indexCount, …) — see the layout note on `MeshOut::pack`.
fn rcms_triangles(bytes: &[u8]) -> usize {
    if bytes.len() < 16 {
        return 0;
    }
    let idx = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
    idx / 3
}

/// Named links of the level this region belongs to, with their world positions.
fn dump_links(idx: &level::LevelIndex, region_key: &str) -> Vec<serde_json::Value> {
    let level_name = region_key.split('/').next().unwrap_or("i29");
    let path = format!("levels/{level_name}/{level_name}.level");
    let id = omnitool_lib::core::crc64::hash(&path);
    let Ok(raw) = idx.extract(id) else {
        eprintln!("note: {path} not in the TOC, no named links dumped");
        return Vec::new();
    };
    let Ok(level) = LevelAsset::parse(&raw) else {
        eprintln!("note: {path} did not parse, no named links dumped");
        return Vec::new();
    };
    level
        .named_links
        .iter()
        .map(|l| {
            json!({
                "index": l.index,
                "name": l.name,
                "short": l.short_name(),
                "pos": l.position,
                "region": l.load_region_index,
            })
        })
        .collect()
}
