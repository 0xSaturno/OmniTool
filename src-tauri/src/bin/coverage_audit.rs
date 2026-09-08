//! Coverage audit for the Level Editor / Viewer backend.
//!
//!   cargo run --bin coverage_audit -- [game dir] [hashes file]
//!
//! Answers, with numbers measured from the shipped archives:
//!
//!  * the entry funnel per zone — heap entries walked exhaustively, entries each
//!    instance table references, entries `Zone::build_index` exposes, and how
//!    many of those end up drawable (kind 0, model resolved, geometry non-empty);
//!  * the LOOK / LOD audit — for every distinct model used by the sampled zones,
//!    what `decode_model_mesh` emits against the model's own subset table
//!    (all subsets, and the subsets whose `+0x2E` low nibble says LOD 0) and
//!    against the selection `gltf_writer` makes for the same model.
//!
//! Read-only: it never writes into the game folder and never opens a project.

use std::collections::{BTreeMap, HashMap, HashSet};

use omnitool_lib::core::level::{self, decode_model_mesh, parse_model_asset, LevelIndex};
use omnitool_lib::core::zone::{Zone, T_SCENE_OBJECTS};
use omnitool_lib::tools::model_converter::sections::built::{Built, TAG_BUILT};
use omnitool_lib::tools::model_converter::sections::look::{LookSection, TAG_LOOK};
use omnitool_lib::tools::model_converter::sections::meshes::{MeshDefinition, TAG_MESHES};

const DEFAULT_GAME: &str = r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart";
const HASHES: &str = r"E:\RCRALevelEditorViewer\refs\RCRA-Forge\hashes.txt";

const ZONES: &[&str] = &[
    "levels/i29/openarea/sargasso/tile_n19/tile_n19_art.zone",
    "levels/i29/openarea/sargasso/tile_o17/tile_o17_art.zone",
    "levels/i29/openarea/megalopolis/tile_m37/tile_m37_art.zone",
    "levels/i29/openarea/megalopolis/tile_l37/tile_l37_art.zone",
    "levels/i29/openarea/megalopolis/global_art.zone",
    "levels/i29/openarea/sargasso/global_art.zone",
    "levels/i29/openarea/megalopolis/i29_megalopolis_impostors_default_n19_art.zone",
    "levels/i29/openarea/sargasso/tile_o17/tile_o17_gp.zone",
    "levels/i29/openarea/sargasso/tile_o17/tile_o17_lgt.zone",
];

/// Impostor zones, whose whole content is kind-6 entries + block 2.
const IMPOSTOR_ZONES: &[&str] = &[
    "levels/i29/openarea/megalopolis/i29_megalopolis_impostors_default_n19_art.zone",
    "levels/i29/openarea/megalopolis/i29_megalopolis_impostors_default_j23_art.zone",
    "levels/i29/openarea/megalopolis/i29_megalopolis_impostors_default_zv21_art.zone",
    "levels/i29/openarea/megalopolis/i29_megalopolis_impostors_default_g40_art.zone",
    "levels/i29/openarea/sargasso/i29_sargasso_impostors_one_district_r16_art.zone",
    "levels/i29/openarea/sargasso/i29_sargasso_impostors_one_district_o19_art.zone",
];

/// Exhaustive forward walk of the heap, independent of the tables.
fn walk_heap(scene: &[u8]) -> BTreeMap<u32, (u32, u8)> {
    let mut out = BTreeMap::new();
    let mut o = 0usize;
    while o + 0x80 <= scene.len() {
        let size = u32::from_le_bytes(scene[o + 0x7C..o + 0x80].try_into().unwrap()) & 0x00FF_FFFF;
        let kind = scene[o + 0x5F];
        let m33 = f32::from_le_bytes(scene[o + 0x3C..o + 0x40].try_into().unwrap());
        let ok = size >= 0x80 && o + size as usize <= scene.len() && kind <= 6 && m33 == 1.0;
        if ok {
            out.insert(o as u32, (size, kind));
            o = (o + size as usize + 7) & !7;
        } else {
            o += 8;
        }
    }
    out
}

fn u32s(b: &[u8]) -> Vec<u32> {
    b.chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

struct ModelStats {
    ours_v: usize,
    ours_i: usize,
    all_v: usize,
    all_i: usize,
    lod0_v: usize,
    lod0_i: usize,
    gltf_v: usize,
    gltf_i: usize,
    built_v: u32,
    built_i: u32,
    looks: usize,
    subsets: usize,
    err: Option<String>,
}

fn model_stats(index: &LevelIndex, id: u64) -> ModelStats {
    let mut s = ModelStats {
        ours_v: 0,
        ours_i: 0,
        all_v: 0,
        all_i: 0,
        lod0_v: 0,
        lod0_i: 0,
        gltf_v: 0,
        gltf_i: 0,
        built_v: 0,
        built_i: 0,
        looks: 0,
        subsets: 0,
        err: None,
    };
    let raw = match index.extract(id) {
        Ok(r) => r,
        Err(e) => {
            s.err = Some(format!("extract: {e}"));
            return s;
        }
    };
    let model = match parse_model_asset(&raw) {
        Ok(m) => m,
        Err(e) => {
            s.err = Some(format!("parse: {e}"));
            return s;
        }
    };
    let dat1 = &model.dat1;
    if let Some(b) = dat1.get_section_data(TAG_BUILT).and_then(Built::parse) {
        s.built_v = b.vertex_count;
        s.built_i = b.index_count;
    }
    let meshes = match dat1.get_section_data(TAG_MESHES).map(MeshDefinition::parse_all) {
        Some(Ok(m)) => m,
        _ => {
            s.err = Some("no Model Subset".into());
            return s;
        }
    };
    s.subsets = meshes.len();
    for m in &meshes {
        s.all_v += m.vertex_count as usize;
        s.all_i += m.index_count as usize;
        // Model Subset +0x2E low nibble = the LOD this subset belongs to.
        if (m.unk10 & 0xF) == 0 {
            s.lod0_v += m.vertex_count as usize;
            s.lod0_i += m.index_count as usize;
        }
    }
    // What gltf_writer selects for look 0 / LOD 0 — the export oracle.
    if let Some(ld) = dat1.get_section_data(TAG_LOOK) {
        if let Ok(look) = LookSection::parse(ld) {
            s.looks = look.looks.len();
            if let Some(l0) = look.looks.first() {
                if let Some(e) = l0.lods.first() {
                    for mi in e.start..e.start + e.count {
                        if let Some(m) = meshes.get(mi as usize) {
                            s.gltf_v += m.vertex_count as usize;
                            s.gltf_i += m.index_count as usize;
                        }
                    }
                }
            }
        }
    }
    match decode_model_mesh(&model, 0) {
        Ok(o) => {
            s.ours_v = o.vertex_count();
            s.ours_i = o.indices.len();
        }
        Err(e) => s.err = Some(format!("decode: {e}")),
    }
    s
}

fn main() {
    let game = std::env::args().nth(1).unwrap_or_else(|| DEFAULT_GAME.into());
    let hashes = std::env::args().nth(2).unwrap_or_else(|| HASHES.into());
    let index = level::get_index(&game, Some(&hashes)).expect("index");

    let mut used_models: HashMap<u64, usize> = HashMap::new();

    println!("== (a) per-zone funnel ==");
    println!(
        "{:<58} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}",
        "zone", "heap", "tables", "exposed", "kind0", "modelok", "inTOC", "drawn"
    );
    for zp in ZONES {
        let id = omnitool_lib::core::crc64::hash(zp);
        let raw = match index.extract(id) {
            Ok(r) => r,
            Err(e) => {
                println!("{zp}: extract failed: {e}");
                continue;
            }
        };
        let zone = match Zone::parse(&raw) {
            Ok(z) => z,
            Err(e) => {
                println!("{zp}: parse failed: {e}");
                continue;
            }
        };
        let scene = zone
            .dat1
            .get_section_data(T_SCENE_OBJECTS)
            .map(|s| s.to_vec())
            .unwrap_or_default();
        let heap = walk_heap(&scene);

        let mut refs: HashSet<u32> = HashSet::new();
        for tag in [
            0x6987_F172u32,
            0x97FF_6EB5,
            0xBDAB_2B0D,
            0xBCF4_3558,
            0x927C_4EC3,
            0xDC31_1FC3,
        ] {
            if let Some(s) = zone.dat1.get_section_data(tag) {
                refs.extend(u32s(s));
            }
        }
        for a in &zone.actors {
            refs.insert(a.scene_offset);
        }

        let exposed = zone.scene_objects.len();
        let kind0: Vec<_> = zone
            .scene_objects
            .iter()
            .filter(|o| o.kind == 0)
            .collect();
        let model_ok = kind0.iter().filter(|o| o.model_id != 0).count();
        let mut in_toc = 0usize;
        let mut drawn = 0usize;
        let mut per_zone_models: HashSet<u64> = HashSet::new();
        for o in &kind0 {
            if o.model_id == 0 {
                continue;
            }
            per_zone_models.insert(o.model_id);
            if index.asset(o.model_id).is_some() {
                in_toc += 1;
            }
        }
        let mut good: HashSet<u64> = HashSet::new();
        for &m in &per_zone_models {
            *used_models.entry(m).or_insert(0) += 1;
            let bytes = level::model_mesh_bytes(&index, m, 0);
            let vc = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
            if vc > 0 {
                good.insert(m);
            }
        }
        for o in &kind0 {
            if good.contains(&o.model_id) {
                drawn += 1;
            }
        }
        let short = zp.rsplit('/').next().unwrap();
        println!(
            "{:<58} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}",
            short,
            heap.len(),
            refs.len(),
            exposed,
            kind0.len(),
            model_ok,
            in_toc,
            drawn
        );
        // explain the funnel losses
        let unref: Vec<_> = heap.keys().filter(|k| !refs.contains(k)).collect();
        if !unref.is_empty() {
            let mut ks: BTreeMap<u8, usize> = BTreeMap::new();
            for k in &unref {
                ks.entry(heap[k].1).and_modify(|c| *c += 1).or_insert(1);
            }
            println!("      heap entries no table references: {} {:?}", unref.len(), ks);
        }
        let missed: Vec<u32> = refs
            .iter()
            .copied()
            .filter(|r| !zone.scene_objects.iter().any(|o| o.offset == *r))
            .collect();
        if !missed.is_empty() {
            println!(
                "      referenced but NOT exposed: {} e.g. {:?}",
                missed.len(),
                &missed[..missed.len().min(5)]
            );
        }
        let bad: Vec<&str> = kind0
            .iter()
            .filter(|o| o.model_id != 0 && !good.contains(&o.model_id))
            .map(|o| o.model_path.as_str())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if !bad.is_empty() {
            println!(
                "      models that decode to nothing ({}): {:?}",
                bad.len(),
                &bad[..bad.len().min(6)]
            );
        }
    }

    // ---- (d) impostors + (c) decal geometry --------------------------------
    println!("
== (d) block-2 geometry now decoded ==");
    let mut zone_geo: Vec<&str> = IMPOSTOR_ZONES.to_vec();
    zone_geo.extend_from_slice(&[
        "levels/i29/openarea/sargasso/tile_n19/tile_n19_art.zone",
        "levels/i29/openarea/megalopolis/tile_m37/tile_m37_art.zone",
    ]);
    for zp in zone_geo {
        let id = omnitool_lib::core::crc64::hash(zp);
        let Ok(raw) = index.extract(id) else { continue };
        let Ok(zone) = Zone::parse(&raw) else { continue };
        let mut n = 0usize;
        let (mut tris, mut verts, mut fails) = (0usize, 0usize, 0usize);
        let mut kinds: BTreeMap<u8, usize> = BTreeMap::new();
        for o in zone.scene_objects.iter().filter(|o| o.geometry.is_some()) {
            n += 1;
            *kinds.entry(o.kind).or_insert(0) += 1;
            match level::decode_zone_geometry(&zone, o.offset) {
                Ok(m) => {
                    tris += m.indices.len() / 3;
                    verts += m.vertex_count();
                }
                Err(_) => fails += 1,
            }
        }
        println!(
            "  {:<58} objects {:>5}  meshes {:>4} {:?}  {} tris / {} verts  fails {}",
            zp.rsplit('/').next().unwrap(),
            zone.scene_objects.len(),
            n,
            kinds,
            tris,
            verts,
            fails
        );
    }

    // ---- (e) LOOK / LOD audit ---------------------------------------------
    println!("\n== (e) look/LOD audit over {} distinct models ==", used_models.len());
    let mut ids: Vec<u64> = used_models.keys().copied().collect();
    ids.sort();
    let (mut under, mut over, mut exact, mut empty, mut errs) = (0, 0, 0, 0, 0);
    let mut examples: Vec<String> = Vec::new();
    let mut multi_look = 0;
    let mut no_look = 0;
    let mut lod0_mismatch = 0;
    for id in &ids {
        let s = model_stats(&index, *id);
        if s.err.is_some() && s.ours_v == 0 {
            errs += 1;
            if examples.len() < 12 {
                examples.push(format!(
                    "  ERR {:016X} {} {:?}",
                    id,
                    index.name_of(*id).unwrap_or("?"),
                    s.err
                ));
            }
            continue;
        }
        if s.looks == 0 {
            no_look += 1;
        }
        if s.looks > 1 {
            multi_look += 1;
        }
        if s.ours_v == 0 {
            empty += 1;
        }
        if s.looks > 0 && (s.gltf_v != s.lod0_v) {
            lod0_mismatch += 1;
        }
        let target = if s.looks > 0 { s.gltf_v } else { s.all_v };
        if s.ours_v < target {
            under += 1;
            if examples.len() < 12 {
                examples.push(format!(
                    "  UNDER {:016X} ours {}v/{}i  gltf {}v/{}i  lod0 {}v  all {}v  built {}v  looks {} subsets {}  {}",
                    id, s.ours_v, s.ours_i, s.gltf_v, s.gltf_i, s.lod0_v, s.all_v, s.built_v,
                    s.looks, s.subsets,
                    index.name_of(*id).unwrap_or("?")
                ));
            }
        } else if s.ours_v > target {
            over += 1;
        } else {
            exact += 1;
        }
    }
    println!(
        "  vs the gltf_writer selection: exact {exact}, under {under}, over {over}, \
         empty {empty}, undecodable {errs}"
    );
    println!("  models with no Model Look section: {no_look}; with >1 look: {multi_look}");
    println!("  look0/LOD0 vertex total != LOD0-by-subset-nibble total: {lod0_mismatch}");
    // A model whose look 0 / LOD 0 selects FEWER vertices than the subset
    // table's own LOD-0 nibble says would be genuinely under-emitted.
    let mut short = 0usize;
    for id in &ids {
        let s = model_stats(&index, *id);
        if s.looks > 0 && s.gltf_v < s.lod0_v {
            short += 1;
            if short <= 6 {
                println!(
                    "  SHORT {:016X} look0/LOD0 {}v < LOD0-by-nibble {}v (all {}v, {} looks) {}",
                    id, s.gltf_v, s.lod0_v, s.all_v, s.looks,
                    index.name_of(*id).unwrap_or("?")
                );
            }
        }
    }
    println!("  models where look 0 / LOD 0 selects fewer vertices than the subset table's LOD 0: {short}");
    for e in &examples {
        println!("{e}");
    }
}
