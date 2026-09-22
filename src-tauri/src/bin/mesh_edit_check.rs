//! Edits a model through the glTF path the way a mod would, then checks the rebuilt file against
//! the format rules a replaced mesh has to satisfy.
//!
//! usage: mesh_edit_check <file.model> [--scenario S] [--mesh N] [--wide-joint J]
//!
//! Scenarios (default `grow`):
//! - `unchanged`: re-import the untouched export; the file must come back byte-identical.
//! - `shuffle`: every mesh lists its vertices and triangles in reverse, as a DCC may; still identical.
//! - `grow`: a skinned mesh gains a fan of triangles pushed 50% beyond its bounds, so its skin is
//!   rebuilt. With `--wide-joint J` the new vertices blend joint 0 with joint J.
//! - `add`: a copy of that mesh, moved aside, with a new material slot, becomes a new subset.
//! - `drop`: that mesh is left out of the glTF, so its subset is removed.
//! - `morph-edit`: one shape key of a morph mesh is scaled 1.5x; topology unchanged.
//! - `morph-grow`: a morph mesh grows like `grow`; its shape keys must survive the rebuild.

use std::collections::BTreeMap;

use omnitool_lib::tools::model_converter::{
    gltf_import::{import_gltf, ImportOptions, ImportReport, Tier},
    gltf_reader::{parse_gltf, GltfMesh, GltfModel, GltfVertex},
    gltf_writer::model_to_glb_for_looks,
    model::ModelFile,
    morph_build::{encode, MorphSet},
    sections::{
        built::Built,
        geo::{TAG_INDEXES, TAG_VERTEXES},
        look::{LookSection, TAG_LOOK},
        looks::{LookBuiltSection, MaterialSection, TAG_LOOK_BUILT, TAG_MATERIAL},
        meshes::{MeshDefinition, TAG_MESHES},
        morph::{TAG_ANIM_MORPH_DATA, TAG_ANIM_MORPH_INDICES, TAG_ANIM_MORPH_INFO},
        skin::{SkinSource, SKIN_BATCH_JOINT_MAX, SKIN_BATCH_VERT_MAX},
    },
};

const TAG_BUILT: u32 = 0x283D0383;
const TAG_JOINTS: u32 = 0x15DF9D3B;

fn subsets(m: &ModelFile) -> Vec<MeshDefinition> {
    MeshDefinition::parse_all(m.dat1.get_section_data(TAG_MESHES).unwrap()).unwrap()
}

fn positions(m: &ModelFile, mpu: f32) -> Vec<[f32; 3]> {
    m.dat1.get_section_data(TAG_VERTEXES).unwrap()
        .chunks_exact(16)
        .map(|c| [0, 2, 4].map(|o| i16::from_le_bytes([c[o], c[o + 1]]) as f32 * mpu))
        .collect()
}

fn morphs(m: &ModelFile) -> Option<MorphSet> {
    let d = &m.dat1;
    let skin = SkinSource::from_dat1(d)?;
    MorphSet::decode(
        d.get_section_data(TAG_ANIM_MORPH_INFO)?,
        d.get_section_data(TAG_ANIM_MORPH_DATA)?,
        d.get_section_data(TAG_ANIM_MORPH_INDICES)?,
        &subsets(m),
        &skin.batches,
        |o| d.get_string(o),
    )
    .ok()
}

fn slot_names(m: &ModelFile) -> Vec<String> {
    let mat = MaterialSection::parse(m.dat1.get_section_data(TAG_MATERIAL).unwrap()).unwrap();
    mat.slots.iter().map(|s| m.dat1.get_string(s.name_offset as u32).unwrap_or_default()).collect()
}

/// Stored tangent of one vertex: X from the normal word, Y from |W|, Z sign from bit 30.
fn stored_tangent(raw: &[u8]) -> [f32; 3] {
    let w = i16::from_le_bytes([raw[6], raw[7]]) as i32;
    let nt = u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]);
    let s2 = std::f32::consts::SQRT_2;
    let ex = ((nt >> 20) & 0x3FF) as f32 / 1023.0 * (4.0 / s2) - 2.0 / s2;
    let ey = (w.abs() & 0x3FF) as f32 / 1023.0 * (4.0 / s2) - 2.0 / s2;
    let f = ex * ex + ey * ey;
    let k = (1.0 - f * 0.25).max(0.0).sqrt();
    let z = (1.0 - f * 0.5).abs();
    [ex * k, ey * k, if nt >> 30 & 1 == 1 { z } else { -z }]
}

/// Appends a fan of copies of every 7th vertex pushed 50% further from the centroid.
fn grow(m: &mut GltfMesh, wide: Option<usize>) -> usize {
    let n = m.vertexes.len() as f32;
    let c = m.vertexes.iter().fold([0f32; 3], |a, v| [a[0] + v.position.0 / n, a[1] + v.position.1 / n, a[2] + v.position.2 / n]);
    let picks: Vec<usize> = (0..m.vertexes.len()).step_by(7).take(3000).collect();
    let base = m.vertexes.len() as u32;
    for &i in &picks {
        let v = &m.vertexes[i];
        let p = [v.position.0, v.position.1, v.position.2];
        let mut nv = GltfVertex {
            position: (c[0] + (p[0] - c[0]) * 1.5, c[1] + (p[1] - c[1]) * 1.5, c[2] + (p[2] - c[2]) * 1.5),
            normal: v.normal,
            raw_normal: None,
            uv: v.uv,
            uv1: v.uv1,
            groups: v.groups.clone(),
            weights: v.weights.clone(),
        };
        if let Some(j) = wide {
            nv.groups = vec![0, j as u16, 0, 0];
            nv.weights = vec![0.5, 0.5, 0.0, 0.0];
        }
        m.vertexes.push(nv);
    }
    for k in 0..(picks.len() as u32).saturating_sub(2) {
        m.faces.push((base, base + k + 1, base + k + 2));
    }
    picks.len()
}

struct Checks {
    fails: usize,
}

impl Checks {
    fn check(&mut self, name: &str, ok: bool, detail: impl FnOnce() -> String) {
        println!("  {} {name}{}", if ok { "ok  " } else { "FAIL" }, if ok { String::new() } else { format!(": {}", detail()) });
        self.fails += !ok as usize;
    }
}

/// Format rules every written model must satisfy, whatever was edited.
fn validate(rt: &ModelFile, c: &mut Checks) {
    let d = &rt.dat1;
    let subs = subsets(rt);
    let built = Built::parse(d.get_section_data(TAG_BUILT).unwrap()).unwrap();
    let verts = d.get_section_data(TAG_VERTEXES).unwrap().len() / 16;
    let idx = d.get_section_data(TAG_INDEXES).unwrap().len() / 2;
    c.check("Built vertex / index counts match the streams", built.vertex_count as usize == verts && built.index_count as usize == idx,
        || format!("{}/{} vs {verts}/{idx}", built.vertex_count, built.index_count));
    let past = subs.iter().filter(|m| (m.vertex_start + m.vertex_count) as usize > verts || (m.index_start + m.index_count) as usize > idx).count();
    c.check("every subset lies inside the streams", past == 0, || format!("{past} subsets"));

    let looks = LookSection::parse(d.get_section_data(TAG_LOOK).unwrap()).unwrap();
    if let Some(lb) = d.get_section_data(TAG_LOOK_BUILT) {
        let lb = LookBuiltSection::parse(lb, looks.looks.len()).unwrap();
        let mut bad = 0;
        for (look, built) in looks.looks.iter().zip(&lb.looks) {
            for (k, r) in look.lods.iter().take(6).enumerate() {
                let want: Vec<u16> = (r.start..r.start + r.count).collect();
                bad += (built.lod_subsets(k) != want) as usize;
            }
        }
        c.check("Look Built LOD bitfields match the look ranges", bad == 0, || format!("{bad} LODs"));
    }
    let out = looks.looks.iter().flat_map(|l| l.lods.iter().take(6)).filter(|r| (r.start + r.count) as usize > subs.len()).count();
    c.check("look ranges stay inside the subset table", out == 0, || format!("{out} ranges"));

    let mut first = std::collections::HashMap::new();
    let bad_proxy = subs.iter().enumerate().filter(|(i, m)| {
        let owner = *first.entry((m.vertex_start, m.vertex_count, m.index_start, m.index_count)).or_insert(*i);
        m.lod_ref() as usize != owner || m.is_lod_proxy() != (owner != *i)
    }).count();
    c.check("LOD proxy refs point at the first subset of each block", bad_proxy <= 1, || format!("{bad_proxy} subsets"));

    let mat = MaterialSection::parse(d.get_section_data(TAG_MATERIAL).unwrap()).unwrap();
    let bad_slot = subs.iter().filter(|m| m.material_index as usize >= mat.slots.len()).count();
    c.check("every subset's material slot exists", bad_slot == 0, || format!("{bad_slot} subsets"));
    c.check("material ids sorted by name hash", mat.ids.windows(2).all(|w| w[0].name_hash <= w[1].name_hash), String::new);

    if let Some(s) = SkinSource::from_dat1(d) {
        let joint_count = d.get_section_data(TAG_JOINTS).map(|j| j.len() / 16).unwrap_or(0);
        let (mut tiling, mut limits, mut joints) = (0, 0, 0);
        for m in subs.iter().filter(|m| m.skin_batch_count() > 0) {
            let run = &s.batches[m.first_skin_batch as usize..][..m.skin_batch_count() as usize];
            let mut next = 0u32;
            for b in run {
                tiling += (b.first_vertex as u32 != next) as usize;
                next += b.vertex_count as u32;
                limits += (b.vertex_count as usize > SKIN_BATCH_VERT_MAX || b.joint_remap_count as usize > SKIN_BATCH_JOINT_MAX) as usize;
            }
            tiling += (next != m.vertex_count) as usize;
            joints += s.subset_weights(m).iter().flatten().filter(|(j, _)| *j as usize >= joint_count).count();
        }
        let flagged = subs.iter().filter(|m| m.is_skinned() != (m.skin_batch_count() > 0)).count();
        c.check("skinned flag <=> skin batches", flagged == 0, || format!("{flagged} subsets"));
        c.check("skin batches tile every skinned subset", tiling == 0, || format!("{tiling} breaks"));
        c.check("batches within 2560 vertices / 256 joints", limits == 0, || format!("{limits} batches"));
        c.check("every decoded joint exists", joints == 0, || format!("{joints} influences"));

        if let Some(set) = morphs(rt) {
            let mut outside = 0;
            for m in &set.morphs {
                for (&sid, deltas) in &m.subsets {
                    let sub = &subs[sid as usize];
                    let first = sub.first_skin_batch as usize;
                    let prefix: u32 = s.batches[first..first + sub.anim_vert_batch_count() as usize].iter().map(|b| b.vertex_count as u32).sum();
                    outside += deltas.iter().filter(|dl| dl.0 >= prefix).count();
                }
            }
            c.check("morph deltas lie in their subset's morphing batches", outside == 0, || format!("{outside} deltas"));
            let enc = encode(&set, &subs, &s.batches, |_| 0).unwrap();
            let same = enc.data == d.get_section_data(TAG_ANIM_MORPH_DATA).unwrap()
                && enc.indices == d.get_section_data(TAG_ANIM_MORPH_INDICES).unwrap();
            c.check("morph streams decode and re-encode identically", same && enc.dropped == 0, || format!("dropped {}", enc.dropped));
        }
    }
}

/// Subset-level checks on an edited subset: bounds and tangents.
fn check_edited(rt: &ModelFile, target: usize, c: &mut Checks) {
    let subs = subsets(rt);
    let built = Built::parse(rt.dat1.get_section_data(TAG_BUILT).unwrap()).unwrap();
    let mpu = built.meters_per_unit;
    let pos = positions(rt, mpu);
    let m = &subs[target];
    let p = &pos[m.vertex_start as usize..(m.vertex_start + m.vertex_count) as usize];
    let d = |a: [f32; 3], b: [f32; 3]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
    let reach = p.iter().map(|v| d(*v, m.bsphere_center)).fold(0f32, f32::max);
    c.check("edited subset bsphere encloses its vertices", reach <= m.bsphere_radius_m(mpu) + 2.0 * mpu,
        || format!("{reach:.3} > {:.3}", m.bsphere_radius_m(mpu)));
    let reach = pos.iter().map(|v| d(*v, built.bsphere_center)).fold(0f32, f32::max);
    c.check("model bsphere encloses every vertex", reach <= built.bsphere_radius * 1.001 + mpu,
        || format!("{reach:.3} > {:.3}", built.bsphere_radius));

    let vb = rt.dat1.get_section_data(TAG_VERTEXES).unwrap();
    let ib: Vec<u16> = rt.dat1.get_section_data(TAG_INDEXES).unwrap().chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let uv_scale = built.uv0_scale();
    let uv = |i: usize| [0, 2].map(|o| i16::from_le_bytes([vb[i * 16 + 12 + o], vb[i * 16 + 13 + o]]) as f32 * uv_scale);
    let base = if m.has_relative_indices() { m.vertex_start as usize } else { 0 };
    let (mut agree, mut total) = (0, 0);
    for t in ib[m.index_start as usize..(m.index_start + m.index_count) as usize].chunks_exact(3) {
        let [a, b, cc] = [t[0], t[1], t[2]].map(|i| base + i as usize);
        let (e1, e2) = ([0, 1, 2].map(|k| pos[b][k] - pos[a][k]), [0, 1, 2].map(|k| pos[cc][k] - pos[a][k]));
        let (d1, d2) = ([uv(b)[0] - uv(a)[0], uv(b)[1] - uv(a)[1]], [uv(cc)[0] - uv(a)[0], uv(cc)[1] - uv(a)[1]]);
        let det = d1[0] * d2[1] - d2[0] * d1[1];
        if det.abs() < 1e-6 {
            continue;
        }
        let tg = [0, 1, 2].map(|k| (e1[k] * d2[1] - e2[k] * d1[1]) / det);
        let st = stored_tangent(&vb[a * 16..a * 16 + 16]);
        total += 1;
        agree += (tg[0] * st[0] + tg[1] * st[1] + tg[2] * st[2] > 0.0) as usize;
    }
    c.check("edited mesh tangents follow UV0 (>=95% of triangles)", total > 0 && agree * 100 >= total * 95, || format!("{agree}/{total}"));
}

fn final_index(report: &ImportReport, source: usize) -> usize {
    report.subsets.iter().find(|s| s.source == Some(source)).map(|s| s.subset).expect("subset in report")
}

/// Per morph name: (delta count, summed position-delta length) over one primitive's shape keys.
fn target_sums(m: &GltfMesh) -> BTreeMap<String, (usize, f32)> {
    m.targets
        .iter()
        .map(|t| {
            let len: f32 = t.deltas.iter().map(|d| (d.1[0].powi(2) + d.1[1].powi(2) + d.1[2].powi(2)).sqrt()).sum();
            (t.name.clone(), (t.deltas.len(), len))
        })
        .collect()
}

fn morph_sums(set: &MorphSet, subset: usize) -> BTreeMap<String, (usize, f32)> {
    set.morphs
        .iter()
        .filter_map(|m| {
            let deltas = m.subsets.get(&(subset as u16))?;
            let len: f32 = deltas.iter().map(|d| (d.1[0].powi(2) + d.1[1].powi(2) + d.1[2].powi(2)).sqrt()).sum();
            Some((m.name.clone(), (deltas.len(), len)))
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let num = |k: &str| get(k).and_then(|v| v.parse::<usize>().ok());
    let Some(path) = args.first() else {
        eprintln!("usage: mesh_edit_check <file.model> [--scenario unchanged|grow|add|drop|morph-edit|morph-grow] [--mesh N] [--wide-joint J]");
        std::process::exit(1);
    };
    let scenario = get("--scenario").unwrap_or_else(|| "grow".into());
    let data = std::fs::read(path).expect("read model");
    let orig = ModelFile::parse(&data).expect("parse model");
    let orig_subs = subsets(&orig);

    let glb = model_to_glb_for_looks(&orig, &[0]).expect("export");
    let glb_path = std::env::temp_dir().join("mesh_edit_check.glb");
    std::fs::write(&glb_path, &glb).unwrap();
    let mut gltf: GltfModel = parse_gltf(&glb_path.to_string_lossy()).expect("parse glb");

    let morphing = scenario.starts_with("morph");
    let gi = match num("--mesh") {
        Some(n) => gltf.meshes.iter().position(|m| m.subset_hint == Some(n)).expect("mesh not exported"),
        None => gltf
            .meshes
            .iter()
            .position(|m| m.subset_hint.is_some_and(|n| orig_subs[n].is_skinned()) && (!morphing || !m.targets.is_empty()))
            .expect(if morphing { "no morph mesh" } else { "no skinned mesh" }),
    };
    let source = gltf.meshes[gi].subset_hint.unwrap();
    let wide = num("--wide-joint");
    let mut opts = ImportOptions::default();
    let mut added = 0;
    let expect_targets = gltf.meshes[gi].clone();

    match scenario.as_str() {
        "unchanged" => {}
        "shuffle" => {
            for m in &mut gltf.meshes {
                let n = m.vertexes.len() as u32;
                m.vertexes.reverse();
                m.faces = m.faces.iter().rev().map(|f| (n - 1 - f.1, n - 1 - f.2, n - 1 - f.0)).collect();
                for t in &mut m.targets {
                    t.deltas.iter_mut().for_each(|d| d.0 = n - 1 - d.0);
                    t.deltas.sort_by_key(|d| d.0);
                }
            }
        }
        "grow" | "morph-grow" => added = grow(&mut gltf.meshes[gi], wide),
        "add" => {
            let mut copy = gltf.meshes[gi].clone();
            let (lo, hi) = copy.vertexes.iter().fold((f32::MAX, f32::MIN), |a, v| (a.0.min(v.position.0), a.1.max(v.position.0)));
            copy.vertexes.iter_mut().for_each(|v| v.position.0 += (hi - lo) * 1.2);
            copy.name = "omni_test".into();
            copy.material = Some("omni_test_slot".into());
            copy.material_path = None;
            copy.subset_hint = None;
            copy.targets.clear();
            let slots = MaterialSection::parse(orig.dat1.get_section_data(TAG_MATERIAL).unwrap()).unwrap();
            let src_path = orig.dat1.get_string(slots.slots[orig_subs[source].material_index as usize].path_offset as u32).unwrap_or_default();
            opts.materials.insert("omni_test_slot".into(), src_path);
            gltf.meshes.push(copy);
        }
        "drop" => {
            gltf.meshes.remove(gi);
        }
        "morph-edit" => {
            let t = &mut gltf.meshes[gi].targets[0];
            t.deltas.iter_mut().for_each(|d| d.1 = d.1.map(|x| x * 1.5));
            println!("  info scaled shape key '{}' ({} vertices)", t.name, t.deltas.len());
        }
        other => panic!("unknown scenario {other}"),
    }
    println!("{path}: {scenario} on mesh #{source}{}", if added > 0 { format!(" (+{added} vertices{})", wide.map(|j| format!(", joints 0+{j}")).unwrap_or_default()) } else { String::new() });

    let mut rebuilt = ModelFile::parse(&data).unwrap();
    let report = import_gltf(&mut rebuilt, &gltf, &opts).expect("import");
    let mut tiers: BTreeMap<String, usize> = BTreeMap::new();
    for s in &report.subsets {
        *tiers.entry(format!("{:?}", s.tier)).or_default() += 1;
    }
    println!("  info tiers {tiers:?}, removed {:?}, new slots {:?}, morphs {:?}", report.removed, report.new_slots, report.morphs);
    for w in &report.warnings {
        println!("  warn {w}");
    }
    let out = rebuilt.save();
    let rt = ModelFile::parse(&out).expect("reparse");
    let subs = subsets(&rt);
    let mut c = Checks { fails: 0 };
    validate(&rt, &mut c);

    match scenario.as_str() {
        "unchanged" | "shuffle" => {
            let vanilla = ModelFile::parse(&data).unwrap().save();
            c.check("every primitive is recognised as unchanged", report.subsets.iter().all(|s| s.tier == Tier::Unchanged), || format!("{tiers:?}"));
            let diff = vanilla.iter().zip(&out).position(|(a, b)| a != b);
            c.check("re-import is byte-identical", vanilla.len() == out.len() && diff.is_none(),
                || format!("len {} vs {}, first diff {diff:?}", vanilla.len(), out.len()));
        }
        "grow" | "morph-grow" => {
            let target = final_index(&report, source);
            c.check("grown mesh keeps its subset index", target == source, || format!("now #{target}"));
            let untouched = orig_subs.iter().zip(&subs).enumerate().filter(|(i, _)| *i != target)
                .filter(|(_, (a, b))| a.anim_vert_batch_count() != b.anim_vert_batch_count()).count();
            c.check("untouched subsets keep their anim-vert batch byte", untouched == 0, || format!("{untouched} changed"));
            let skin = SkinSource::from_dat1(&rt.dat1).unwrap();
            let got = skin.subset_weights(&subs[target]);
            if scenario == "grow" {
                let src = &gltf.meshes[gi];
                let mismatched = src.vertexes.iter().zip(&got).filter(|(v, w)| {
                    let top = v.groups.iter().zip(&v.weights).max_by(|a, b| a.1.total_cmp(b.1)).map(|(j, _)| *j);
                    let got_top = w.iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(|(j, _)| *j);
                    top.is_some() && top != got_top && wide.is_none()
                }).count();
                c.check("edited mesh: dominant joint of every vertex survives", mismatched == 0, || format!("{mismatched} vertices"));
                if let Some(j) = wide {
                    let tail = &got[got.len() - added..];
                    let both = tail.iter().filter(|w| w.iter().any(|x| x.0 == 0) && w.iter().any(|x| x.0 as usize == j)).count();
                    println!("  info wide-joint vertices keeping both joints: {both}/{added}");
                }
            } else {
                let set = morphs(&rt).expect("morphs");
                let (want, have) = (target_sums(&expect_targets), morph_sums(&set, target));
                let bad = want.iter().filter(|(k, w)| have.get(*k).is_none_or(|h| h.0 != w.0 || (h.1 - w.1).abs() > 0.02 * w.1.max(1e-3))).count();
                c.check("every shape key survives the rebuild (count and magnitude)", bad == 0, || format!("{bad}/{} keys differ", want.len()));
            }
            check_edited(&rt, target, &mut c);
        }
        "add" => {
            let new = report.subsets.iter().find(|s| s.tier == Tier::New).map(|s| s.subset);
            c.check("one new subset", subs.len() == orig_subs.len() + 1 && new.is_some(), || format!("{} -> {}", orig_subs.len(), subs.len()));
            let names = slot_names(&rt);
            c.check("one new material slot", names.len() == slot_names(&orig).len() + 1, || format!("{} slots", names.len()));
            if let Some(f) = new {
                c.check("new subset uses the new slot", names[subs[f].material_index as usize] == "omni_test_slot",
                    || names[subs[f].material_index as usize].clone());
                let look = LookSection::parse(rt.dat1.get_section_data(TAG_LOOK).unwrap()).unwrap();
                let r = look.looks[0].lods[0];
                c.check("new subset is in look 0 LOD 0", (r.start as usize..(r.start + r.count) as usize).contains(&f), || format!("{r:?}"));
                c.check("new subset is skinned like its template", subs[f].is_skinned() == orig_subs[source].is_skinned(), String::new);
                let orig_names = slot_names(&orig);
                let moved = orig_subs.iter().enumerate().filter(|(i, m)| {
                    let now = if *i >= f { i + 1 } else { *i };
                    names[subs[now].material_index as usize] != orig_names[m.material_index as usize]
                }).count();
                c.check("existing subsets keep their slots after the remap", moved == 0, || format!("{moved} subsets"));
                check_edited(&rt, f, &mut c);
            }
        }
        "drop" => {
            c.check("the dropped subset is removed", subs.len() + 1 == orig_subs.len() && report.removed == vec![source],
                || format!("{} -> {}, removed {:?}", orig_subs.len(), subs.len(), report.removed));
            let (names, orig_names) = (slot_names(&rt), slot_names(&orig));
            let moved = orig_subs.iter().enumerate().filter(|(i, _)| *i != source).filter(|(i, m)| {
                let now = if *i > source { i - 1 } else { *i };
                names[subs[now].material_index as usize] != orig_names[m.material_index as usize]
                    || subs[now].vertex_count != m.vertex_count
            }).count();
            c.check("the other subsets shift down intact", moved == 0, || format!("{moved} subsets"));
        }
        "morph-edit" => {
            let target = final_index(&report, source);
            let tier = report.subsets.iter().find(|s| s.subset == target).unwrap().tier;
            c.check("topology kept: subset stays unchanged", tier == Tier::Unchanged, || format!("{tier:?}"));
            let set = morphs(&rt).expect("morphs");
            let before = morphs(&orig).unwrap();
            let edited = &gltf.meshes[gi].targets[0];
            let m = set.morphs.iter().find(|m| m.name == edited.name).expect("edited morph");
            let got = m.subsets.get(&(target as u16)).cloned().unwrap_or_default();
            let step = m.ranges.map_or(0.0, |r| r[0].0);
            let off = edited.deltas.iter().zip(&got).filter(|(a, b)| a.0 != b.0 || (0..3).any(|k| (a.1[k] - b.1[k]).abs() > step * 0.51 + 1e-6)).count();
            c.check("edited shape key is written as edited", got.len() == edited.deltas.len() && off == 0,
                || format!("{} vs {} deltas, {off} off", got.len(), edited.deltas.len()));
            let others = before.morphs.iter().filter(|b| b.name != edited.name)
                .filter(|b| set.morphs.iter().find(|m| m.id == b.id).is_none_or(|m| m.subsets != b.subsets)).count();
            c.check("every other morph is unchanged", others == 0, || format!("{others} morphs"));
        }
        _ => {}
    }

    println!("{}", if c.fails == 0 { "all checks passed" } else { "SOME CHECKS FAILED" });
    std::process::exit(if c.fails == 0 { 0 } else { 2 });
}
