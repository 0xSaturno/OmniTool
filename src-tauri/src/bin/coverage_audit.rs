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
//!  * `--models` — a census of the `Model Built` header over every `.model` in
//!    the archives: how often each of the 32 flag bits is set, what the newly
//!    named `0x60` word and `0x74` content flags hold, whether any model asks
//!    for the `CenteredVerts` vertex offset, and how much of a zone's triangle
//!    budget belongs to collision-only models.
//!
//! Read-only: it never writes into the game folder and never opens a project.

use std::collections::{BTreeMap, HashMap, HashSet};

use omnitool_lib::core::level::{
    self, decode_model_mesh, parse_model_asset, LevelIndex, ModelBuilt, MODEL_FLAG_NAMES,
};
use omnitool_lib::core::zone::{Zone, TAG_SCENE_OBJECTS};
use omnitool_lib::tools::model_converter::sections::built::{Built, TAG_BUILT};
use omnitool_lib::tools::model_converter::sections::look::{LookSection, TAG_LOOK};
use omnitool_lib::tools::model_converter::sections::meshes::{MeshDefinition, TAG_MESHES};
use rayon::prelude::*;

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
        // Model Subset +0x2E low nibble = the LOD this subset belongs to;
        // upstream's `lod_proxy_id` is that same field, named.
        if m.lod_id() == 0 {
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

/// One model's `Model Built` header, reduced to what the census aggregates.
struct BuiltRow {
    id: u64,
    flags: u32,
    content_flags: u32,
    strand_subset_count: u8,
    shadow_casting_lod: i16,
    mesh_centre: [f32; 3],
    /// `max |p - bsphere.centre| / bsphere.radius` over the decoded LOD-0
    /// vertices, without and with the `CenteredVerts` offset applied. `None`
    /// when the model did not decode or has no radius to divide by.
    fit_plain: Option<f32>,
    fit_offset: Option<f32>,
    triangles: usize,
}

/// Every `.model` asset that is both named and present in the archives.
fn all_model_ids(index: &LevelIndex) -> Vec<u64> {
    let mut ids: Vec<u64> = index
        .names
        .iter()
        .filter(|(id, path)| path.ends_with(".model") && index.asset(**id).is_some())
        .map(|(id, _)| *id)
        .collect();
    ids.sort_unstable();
    ids
}

/// How far the decoded vertices stray from the model's own bounding sphere,
/// with and without the `CenteredVerts` offset added.
fn measure_fit(index: &LevelIndex, id: u64, b: &ModelBuilt) -> (Option<f32>, Option<f32>, usize) {
    let Ok(raw) = index.extract(id) else {
        return (None, None, 0);
    };
    let Ok(model) = parse_model_asset(&raw) else {
        return (None, None, 0);
    };
    let Ok(mesh) = decode_model_mesh(&model, 0) else {
        return (None, None, 0);
    };
    let tris = mesh.indices.len() / 3;
    if mesh.positions.is_empty() || !(b.bsphere[3] > 1e-6) {
        return (None, None, tris);
    }
    let c = [b.bsphere[0], b.bsphere[1], b.bsphere[2]];
    // `decode_model_mesh` has already applied the offset when the flag is set,
    // so undo it to get the "plain" reading and leave it in for the other.
    let applied = b.vertex_offset();
    let (mut plain, mut offset) = (0f32, 0f32);
    for p in mesh.positions.chunks_exact(3) {
        let d = |extra: [f32; 3]| {
            let v: [f32; 3] =
                std::array::from_fn(|k| p[k] - applied[k] + extra[k] - c[k]);
            (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
        };
        plain = plain.max(d([0.0; 3]));
        offset = offset.max(d(b.mesh_centre));
    }
    let r = b.bsphere[3];
    (Some(plain / r), Some(offset / r), tris)
}

/// Census of the `Model Built` header over every shipped model.
fn model_census(index: &LevelIndex, decode_stride: usize) {
    let ids = all_model_ids(index);
    println!("\n== (f) Model Built census over {} shipped .model assets ==", ids.len());
    let t = std::time::Instant::now();

    let rows: Vec<BuiltRow> = ids
        .par_iter()
        .enumerate()
        .filter_map(|(i, &id)| {
            let raw = index.extract(id).ok()?;
            let model = parse_model_asset(&raw).ok()?;
            let b = ModelBuilt::of(&model)?;
            // Decoding every model would re-extract and re-decode the whole
            // library; decode a stride sample plus every model that actually
            // asks for the CenteredVerts offset, which is the case that can
            // move geometry.
            let want = b.centered_verts() || i % decode_stride == 0;
            let (fit_plain, fit_offset, triangles) = if want {
                measure_fit(index, id, &b)
            } else {
                (None, None, 0)
            };
            Some(BuiltRow {
                id,
                flags: b.flags,
                content_flags: b.content_flags,
                strand_subset_count: b.strand_subset_count,
                shadow_casting_lod: b.shadow_casting_lod,
                mesh_centre: b.mesh_centre,
                fit_plain,
                fit_offset,
                triangles,
            })
        })
        .collect();

    println!(
        "  {} of {} parsed a 120-byte Built header in {:?}",
        rows.len(),
        ids.len(),
        t.elapsed()
    );
    if rows.is_empty() {
        return;
    }

    println!("\n  -- flag-bit frequency --");
    println!("  {:>3}  {:<24} {:>8}  {:>6}", "bit", "name", "models", "%");
    for bit in 0..32u32 {
        let n = rows.iter().filter(|r| r.flags >> bit & 1 == 1).count();
        let name = if MODEL_FLAG_NAMES[bit as usize].is_empty() {
            "(unused)"
        } else {
            MODEL_FLAG_NAMES[bit as usize]
        };
        println!(
            "  {bit:>3}  {name:<24} {n:>8}  {:>5.1}%",
            n as f32 * 100.0 / rows.len() as f32
        );
    }

    println!("\n  -- 0x60 word --");
    let mut lods: BTreeMap<i16, usize> = BTreeMap::new();
    let mut strands: BTreeMap<u8, usize> = BTreeMap::new();
    for r in &rows {
        *lods.entry(r.shadow_casting_lod).or_insert(0) += 1;
        *strands.entry(r.strand_subset_count).or_insert(0) += 1;
    }
    println!("  shadow-casting LOD (-1 = all): {lods:?}");
    println!("  strand subset count:           {strands:?}");
    // The bytes used to be read together as "high half = morph presence".
    let strandy = rows.iter().filter(|r| r.strand_subset_count != 0).count();
    let haircards = rows
        .iter()
        .filter(|r| r.flags >> 21 & 1 == 1)
        .count();
    let both = rows
        .iter()
        .filter(|r| r.strand_subset_count != 0 && r.flags >> 21 & 1 == 1)
        .count();
    let morphy = rows.iter().filter(|r| r.content_flags & 1 == 1).count();
    let strand_and_morph = rows
        .iter()
        .filter(|r| r.strand_subset_count != 0 && r.content_flags & 1 == 1)
        .count();
    println!(
        "  strand count != 0: {strandy}; HasHairCardsLook (bit 21): {haircards}; both: {both}"
    );
    println!(
        "  AnimMorph in content flags: {morphy}; models with BOTH a strand count and AnimMorph: {strand_and_morph}"
    );
    // Name the models that carry a non-zero 0x62, so the byte can be checked
    // against what the asset obviously is rather than taken on faith.
    let named: Vec<String> = rows
        .iter()
        .filter(|r| r.strand_subset_count != 0)
        .take(10)
        .map(|r| {
            format!(
                "{}={}",
                index
                    .name_of(r.id)
                    .unwrap_or("?")
                    .rsplit('/')
                    .next()
                    .unwrap_or("?"),
                r.strand_subset_count
            )
        })
        .collect();
    println!("  models with a non-zero 0x62: {named:?}");

    println!("\n  -- 0x74 content flags --");
    for (bit, name) in [(0, "AnimMorph"), (1, "AnimGeom"), (2, "AnimZiva"), (3, "AnimVert")] {
        let n = rows.iter().filter(|r| r.content_flags >> bit & 1 == 1).count();
        println!("  {name:<12} {n:>8}");
    }
    let stray: usize = rows.iter().filter(|r| r.content_flags & !0xF != 0).count();
    println!("  content flags with bits above 3 set: {stray}");

    println!("\n  -- CenteredVerts (flag bit 23) and the 0x1C mesh centre --");
    let centered = rows.iter().filter(|r| r.flags >> 23 & 1 == 1).count();
    let centre_nz = rows
        .iter()
        .filter(|r| r.mesh_centre.iter().any(|c| *c != 0.0))
        .count();
    let both_c = rows
        .iter()
        .filter(|r| r.flags >> 23 & 1 == 1 && r.mesh_centre.iter().any(|c| *c != 0.0))
        .count();
    println!("  models with bit 23 set:                 {centered}");
    println!("  models with a non-zero mesh centre:     {centre_nz}");
    println!("  models with both:                       {both_c}");
    if centre_nz > 0 {
        let worst = rows
            .iter()
            .filter(|r| r.mesh_centre.iter().any(|c| *c != 0.0))
            .map(|r| r.mesh_centre.iter().fold(0f32, |a, c| a.max(c.abs())))
            .fold(0f32, f32::max);
        println!("  largest non-zero mesh-centre component: {worst}");
    }

    // Does the decoded geometry land inside the model's own bounding sphere,
    // with and without the offset? Both columns are reported so the correct
    // choice is visible rather than assumed.
    let fits: Vec<&BuiltRow> = rows.iter().filter(|r| r.fit_plain.is_some()).collect();
    let inside = |v: Option<f32>| v.is_some_and(|x| x <= 1.01);
    let (in_plain, in_off) = (
        fits.iter().filter(|r| inside(r.fit_plain)).count(),
        fits.iter().filter(|r| inside(r.fit_offset)).count(),
    );
    let worst_plain = fits.iter().filter_map(|r| r.fit_plain).fold(0f32, f32::max);
    let worst_off = fits.iter().filter_map(|r| r.fit_offset).fold(0f32, f32::max);
    println!(
        "  decoded {} models: inside the Built bounding sphere without the offset {in_plain}, \
with it {in_off}"
    , fits.len());
    println!("  worst radius ratio: without {worst_plain:.4}, with {worst_off:.4}");

    println!("\n  -- bits 13 / 14 --");
    // Reported apart, because the two turn out to mean very different things:
    // see the cross-tab below and the per-zone table in (g).
    let decoded: Vec<&BuiltRow> = rows.iter().filter(|r| r.fit_plain.is_some()).collect();
    let sample_tris: usize = decoded.iter().map(|r| r.triangles).sum();
    for (bit, name) in [(13u32, "StaticCollOnly"), (14, "CollisionOnly")] {
        let set: Vec<&&BuiltRow> = decoded.iter().filter(|r| r.flags >> bit & 1 == 1).collect();
        let n_all = rows.iter().filter(|r| r.flags >> bit & 1 == 1).count();
        let tris: usize = set.iter().map(|r| r.triangles).sum();
        let uv1 = set.iter().filter(|r| r.flags >> 17 & 1 == 1).count();
        let col = set.iter().filter(|r| r.flags >> 18 & 1 == 1).count();
        println!(
            "  bit {bit:>2} {name:<15} {n_all:>6} models; in the decoded sample {:>5} models, \
{tris:>9} tris ({:>5.1}% of sample), VertHasUv1 {uv1}, VertHasColor {col}",
            set.len(),
            tris as f32 * 100.0 / sample_tris.max(1) as f32
        );
    }
    let both13_14 = rows
        .iter()
        .filter(|r| r.flags >> 13 & 1 == 1 && r.flags >> 14 & 1 == 1)
        .count();
    let only14 = rows
        .iter()
        .filter(|r| r.flags >> 13 & 1 == 0 && r.flags >> 14 & 1 == 1)
        .count();
    println!("  both 13 and 14: {both13_14}; 14 without 13: {only14}");
    for (bit, name) in [(6u32, "SkipImpostor"), (12, "DelayStreaming")] {
        let n = rows.iter().filter(|r| r.flags >> bit & 1 == 1).count();
        println!("  bit {bit:>2} {name:<15} {n:>6} models");
    }
    // Any bit the adopted table calls unused but the shipped data sets.
    for bit in 0..32u32 {
        if MODEL_FLAG_NAMES[bit as usize].is_empty() {
            let n = rows.iter().filter(|r| r.flags >> bit & 1 == 1).count();
            println!("  bit {bit:>2} (unnamed)      {n:>6} models");
        }
    }
}

/// How many of a zone's drawn instances and triangles belong to models the
/// engine flags as collision or navigation geometry.
fn collision_in_zones(index: &LevelIndex) {
    println!("\n== (g) bits 13 / 14 among the models zones actually place ==");
    println!(
        "  {:<44} {:>7} {:>11} | {:>7} {:>11} {:>6} | {:>7} {:>11} {:>6}",
        "zone", "insts", "tris", "b13inst", "b13tris", "%", "b14inst", "b14tris", "%"
    );
    let mut flags_of: HashMap<u64, u32> = HashMap::new();
    let mut tris_of: HashMap<u64, usize> = HashMap::new();
    for zp in ZONES {
        let id = omnitool_lib::core::crc64::hash(zp);
        let Ok(raw) = index.extract(id) else { continue };
        let Ok(zone) = Zone::parse(&raw) else { continue };
        let (mut insts, mut tris) = (0usize, 0usize);
        let (mut i13, mut t13, mut i14, mut t14) = (0usize, 0usize, 0usize, 0usize);
        for o in zone.model_instances() {
            let mid = o.model_id;
            if mid == 0 {
                continue;
            }
            let flags = *flags_of.entry(mid).or_insert_with(|| {
                index
                    .extract(mid)
                    .ok()
                    .and_then(|r| parse_model_asset(&r).ok())
                    .and_then(|m| ModelBuilt::of(&m))
                    .map(|b| b.flags)
                    .unwrap_or(0)
            });
            // Decoded directly, not through `model_mesh_bytes`: that path now
            // skips CollisionOnly models, and this table has to be able to
            // measure exactly what the skip removes.
            let t = *tris_of.entry(mid).or_insert_with(|| {
                index
                    .extract(mid)
                    .ok()
                    .and_then(|r| parse_model_asset(&r).ok())
                    .and_then(|m| decode_model_mesh(&m, 0).ok())
                    .map(|o| o.indices.len() / 3)
                    .unwrap_or(0)
            });
            insts += 1;
            tris += t;
            if flags >> 13 & 1 == 1 {
                i13 += 1;
                t13 += t;
            }
            if flags >> 14 & 1 == 1 {
                i14 += 1;
                t14 += t;
            }
        }
        println!(
            "  {:<44} {insts:>7} {tris:>11} | {i13:>7} {t13:>11} {:>5.1}% | {i14:>7} {t14:>11} {:>5.1}%",
            zp.rsplit('/').next().unwrap(),
            t13 as f32 * 100.0 / tris.max(1) as f32,
            t14 as f32 * 100.0 / tris.max(1) as f32
        );
    }
    // Where do the CollisionOnly models actually live? A viewer that stops
    // drawing them should be able to say what disappears.
    let mut names: Vec<&str> = flags_of
        .iter()
        .filter(|(_, f)| *f >> 14 & 1 == 1)
        .filter_map(|(id, _)| index.name_of(*id))
        .collect();
    names.sort_unstable();
    println!(
        "  distinct CollisionOnly models placed by these zones: {} e.g. {:?}",
        names.len(),
        &names[..names.len().min(6)]
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let game = positional
        .first()
        .map(|s| s.to_string())
        .unwrap_or_else(|| DEFAULT_GAME.into());
    let hashes = positional
        .get(1)
        .map(|s| s.to_string())
        .unwrap_or_else(|| HASHES.into());
    let index = level::get_index(&game, Some(&hashes)).expect("index");

    // `--models` is the Model Built census; it extracts every shipped model, so
    // it is opt-in rather than part of the default zone-funnel run.
    if flag("--models") {
        let stride = args
            .iter()
            .find_map(|a| a.strip_prefix("--stride="))
            .and_then(|s| s.parse().ok())
            .unwrap_or(8usize);
        model_census(&index, stride);
        collision_in_zones(&index);
        return;
    }

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
            .get_section_data(TAG_SCENE_OBJECTS)
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
        // "Not drawn" splits two ways now: a model the decoder could not read,
        // and a CollisionOnly model `model_mesh_bytes` deliberately skips.
        let (mut bad, mut skipped): (Vec<&str>, Vec<&str>) = (Vec::new(), Vec::new());
        for path in kind0
            .iter()
            .filter(|o| o.model_id != 0 && !good.contains(&o.model_id))
            .map(|o| (o.model_id, o.model_path.as_str()))
            .collect::<HashSet<_>>()
        {
            let coll = index
                .extract(path.0)
                .ok()
                .and_then(|r| parse_model_asset(&r).ok())
                .and_then(|m| ModelBuilt::of(&m))
                .is_some_and(|b| b.collision_only());
            if coll {
                skipped.push(path.1);
            } else {
                bad.push(path.1);
            }
        }
        if !skipped.is_empty() {
            println!(
                "      CollisionOnly models skipped on purpose ({}): {:?}",
                skipped.len(),
                &skipped[..skipped.len().min(6)]
            );
        }
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
