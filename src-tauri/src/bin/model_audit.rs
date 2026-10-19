//! Runs every structural cross-check from `docs/MODEL_FORMAT.md` over a whole
//! corpus of models, so claims in the doc are backed by more than the handful of
//! samples they were reversed from.
//!
//! usage:
//!   model_audit <dir>                        walk a directory of .model files
//!   model_audit --toc <toc> --archives <dir> walk every model in the archives
//!
//! Options: --limit N, --failures (only print failing assets).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use omnitool_lib::core::crc32;
use omnitool_lib::core::dat1::Dat1;
use omnitool_lib::core::toc::Toc;
use omnitool_lib::tools::model_converter::model::ModelFile;
use omnitool_lib::tools::model_converter::sections::*;

#[derive(Default)]
struct Stats {
    models: usize,
    /// check name -> (passes, failures)
    checks: BTreeMap<&'static str, (usize, usize)>,
    failures: Vec<(String, String)>,
    /// feature flag value -> set of section tags present, intersected as we go
    /// (bit, tag) -> (set&has, set&!has, !set&has, !set&!has)
    bit_section: BTreeMap<(u32, u32), (usize, usize, usize, usize)>,
    flag_counts: BTreeMap<u32, usize>,
    unknown_tags: BTreeMap<u32, usize>,
    uv_shift: BTreeMap<u32, usize>,
    pos_scale: BTreeMap<String, usize>,
    /// (subsets, bounds mismatches, area mismatches, uv-density mismatches) for the recompute check.
    subset_stat_counts: (usize, usize, usize, usize),
    /// log2(stored / recomputed) uv density on mismatching subsets.
    density_log2_ratios: BTreeMap<String, usize>,
}

impl Stats {
    fn check(&mut self, name: &'static str, ok: bool, who: &str, detail: impl FnOnce() -> String) {
        let e = self.checks.entry(name).or_insert((0, 0));
        if ok {
            e.0 += 1;
        } else {
            e.1 += 1;
            if self.failures.len() < 200 {
                self.failures.push((who.to_string(), format!("{name}: {}", detail())));
            }
        }
    }
}

const KNOWN_TAGS: &[u32] = &[
    0x283D0383, 0xA98BE69B, 0x16F3BA18, 0x6B855EED, 0xCCBAFF15, 0xDCA379A2, 0xC61B1FF5,
    0x5240C82B, 0x0859863D, 0x78D9CBDE, 0x3250BB80, 0xDCC88A19, 0x90CDB60C, 0x15DF9D3B,
    0x0AD3A708, 0xEE31971C, 0x9F614FAB, 0x731CBC2E, 0xC5354B60, 0x5CBA9DE9, 0xEFD92E68,
    0x5E709570, 0xA600C108, 0x380A5744, 0xADD1CBD3, 0x06EB7EFC, 0x811902D7, 0x4CCEA4AD,
    0xDF9FDF12, 0xB7380E8C, 0x27CA5246, 0x3C9DABDF, 0xBB7303D5, 0x707F1B58, 0x9A434B29,
    0x5A39FAB7, 0xB25B3163, 0x45079BC5, 0x237D59F1, 0xCD903318, 0x42349A17, 0x244E5823,
    0x5796FEF6, 0xF4CB2F37, 0xBCE86B01, 0x0BA45069, 0x00823787, 0x14D8B13C, 0x5D5CF541,
    0x3F70F60D, 0xC5354B61, 0x665DA362, 0x7CA37DA0,
];

/// Object-space positions of the Std Vert stream.
fn positions(dat1: &Dat1, mpu: f32) -> Vec<[f32; 3]> {
    dat1.get_section_data(geo::TAG_VERTEXES)
        .map(|v| {
            v.chunks_exact(16)
                .map(|c| [0, 2, 4].map(|o| i16::from_le_bytes([c[o], c[o + 1]]) as f32 * mpu))
                .collect()
        })
        .unwrap_or_default()
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Half-size of the axis-aligned box around `pts`.
fn half_extents(pts: &[[f32; 3]]) -> [f32; 3] {
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for p in pts {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    [0, 1, 2].map(|k| (hi[k] - lo[k]) / 2.0)
}

fn audit(name: &str, data: &[u8], st: &mut Stats) {
    let Ok(model) = ModelFile::parse(data) else { return };
    let dat1 = &model.dat1;
    if dat1.get_section_data(built::TAG_BUILT).is_none() {
        return;
    }
    st.models += 1;

    let tags: Vec<u32> = dat1.sections.iter().map(|s| s.tag).collect();
    for t in &tags {
        if !KNOWN_TAGS.contains(t) {
            *st.unknown_tags.entry(*t).or_insert(0) += 1;
        }
    }

    // ---- Built ----
    let Some(b) = dat1.get_section_data(built::TAG_BUILT).and_then(Built::parse) else { return };
    *st.flag_counts.entry(b.flags).or_insert(0) += 1;
    for bit in 0..32u32 {
        let set = b.flags >> bit & 1 == 1;
        for t in KNOWN_TAGS {
            let e = st.bit_section.entry((bit, *t)).or_insert((0, 0, 0, 0));
            match (set, tags.contains(t)) {
                (true, true) => e.0 += 1,
                (true, false) => e.1 += 1,
                (false, true) => e.2 += 1,
                (false, false) => e.3 += 1,
            }
        }
    }
    let has_morph = tags.contains(&morph::TAG_ANIM_MORPH_INFO);
    *st.uv_shift.entry(b.uv_log_scales).or_insert(0) += 1;
    *st.pos_scale.entry(format!("1/{:.0}", 1.0 / b.meters_per_unit)).or_insert(0) += 1;

    let strand_subsets = dat1.get_section_data(splines::TAG_SPLINE_SUBSETS).map(|d| d.len() / 1256).unwrap_or(0);
    st.check("built.strand_subset_count == SplineSubsets/1256", b.strand_subset_count as usize == strand_subsets, name,
        || format!("{} vs {strand_subsets}", b.strand_subset_count));
    st.check("built AMBIENT_ANIMATION <=> ambient_animation != 0",
        (b.flags & built::flags::AMBIENT_ANIMATION != 0) == (b.ambient_animation != 0.0), name,
        || format!("flag {} value {}", b.flags & 1, b.ambient_animation));
    st.check("built SHADOW_HAS_FADE_DIST <=> shadow_fade_dist != 0",
        (b.flags & built::flags::SHADOW_HAS_FADE_DIST != 0) == (b.shadow_fade_dist != 0), name,
        || format!("dist {}", b.shadow_fade_dist));
    st.check("built content MORPH <=> Anim Morph Info",
        (b.content_flags & built::content_flags::MORPH != 0) == has_morph, name,
        || format!("content 0x{:X}", b.content_flags));

    let pos = positions(dat1, b.meters_per_unit);
    if !pos.is_empty() {
        let r = pos.iter().map(|p| dist(*p, b.bsphere_center)).fold(0f32, f32::max);
        st.check("built bsphere encloses every vertex", r <= b.bsphere_radius * 1.01 + 1e-3, name,
            || format!("{r:.3} > {:.3}", b.bsphere_radius));
        let half = half_extents(&pos);
        st.check("built aabb_extents cover the vertex extents",
            (0..3).all(|k| half[k] <= b.aabb_extents[k] * 1.001 + 2.0 * b.meters_per_unit), name,
            || format!("{half:?} vs {:?}", b.aabb_extents));
    }

    if let Some(v) = dat1.get_section_data(geo::TAG_VERTEXES) {
        st.check("built.vertex_count == StdVert/16", v.len() / 16 == b.vertex_count as usize, name,
            || format!("{} vs {}", b.vertex_count, v.len() / 16));
    }
    if let Some(i) = dat1.get_section_data(geo::TAG_INDEXES) {
        st.check("built.index_count == Index/2", i.len() / 2 == b.index_count as usize, name,
            || format!("{} vs {}", b.index_count, i.len() / 2));
    }
    // The UV shift should put the largest raw component near 1.0 or above, never
    // far below — a too-small shift would mean the model's UVs never reach 1.
    if let Some(v) = dat1.get_section_data(geo::TAG_VERTEXES) {
        let mut max_raw = 0i32;
        for c in v.chunks_exact(16) {
            for o in [12, 14] {
                let x = i16::from_le_bytes([c[o], c[o + 1]]) as i32;
                max_raw = max_raw.max(x.abs());
            }
        }
        if max_raw > 0 {
            let uv = max_raw as f32 * b.uv0_scale();
            st.check("uv shift plausible (max |uv| >= 0.5)", uv >= 0.5, name, || format!("max |uv| = {uv:.3}"));
        }
    }

    // ---- materials: every slot resolves in the sorted id table ----
    if let Some(m) = dat1
        .get_section_data(looks::TAG_MATERIAL)
        .and_then(|d| MaterialSection::parse(d).ok())
    {
        let mut bad = 0;
        for slot in &m.slots {
            let n = dat1.get_string(slot.name_offset as u32).unwrap_or_default();
            if m.id_for_name(&n).is_none() {
                bad += 1;
            }
        }
        st.check("material slot resolves by ihash(lower(name))", bad == 0, name,
            || format!("{bad}/{} slots unresolved", m.slots.len()));
        st.check("material id table sorted by hash",
            m.ids.windows(2).all(|w| w[0].name_hash <= w[1].name_hash), name, || "unsorted".into());
        // Is ids[i] actually the row for slots[i]?
        let mis = m
            .slots
            .iter()
            .enumerate()
            .filter(|(i, slot)| {
                let n = dat1.get_string(slot.name_offset as u32).unwrap_or_default();
                m.ids[*i].name_hash != crc32::hash(&n.to_ascii_lowercase())
            })
            .count();
        st.check("material ids[i] belongs to slots[i]", mis == 0, name,
            || format!("{mis}/{} slots misaligned", m.slots.len()));
    }

    // ---- looks ----
    let look_count = dat1.get_section_data(look::TAG_LOOK).map(|d| d.len() / 32).unwrap_or(0);
    let subset_count = dat1.get_section_data(meshes::TAG_MESHES).map(|d| d.len() / 64).unwrap_or(0);
    if let Some(lb) = dat1
        .get_section_data(looks::TAG_LOOK_BUILT)
        .and_then(|d| LookBuiltSection::parse(d, look_count).ok())
    {
        let ls = dat1.get_section_data(look::TAG_LOOK).and_then(|d| LookSection::parse(d).ok());
        let (mut mask_bad, mut name_bad, mut bits_bad) = (0, 0, 0);
        for (i, l) in lb.looks.iter().enumerate() {
            if expand_mask(&l.bsphere_mask) != l.bspheres {
                mask_bad += 1;
            }
            let n = dat1.get_string(l.name_offset).unwrap_or_default();
            if crc32::hash(&n) != l.name_hash || crc32::hash(&n.to_ascii_lowercase()) != l.name_hash_lower {
                name_bad += 1;
            }
            if let Some(look) = ls.as_ref().and_then(|s| s.looks.get(i)) {
                for (lod, r) in look.lods.iter().take(6).enumerate() {
                    if l.lod_subsets(lod) != (r.start..r.start + r.count).collect::<Vec<_>>() {
                        bits_bad += 1;
                    }
                }
            }
        }
        st.check("look bsphere mask == index list", mask_bad == 0, name, || format!("{mask_bad} looks"));
        st.check("look name_offset hashes to name_hash (+lower)", name_bad == 0, name, || format!("{name_bad} looks"));
        st.check("look LOD subset bitfield == LOD range", bits_bad == 0, name, || format!("{bits_bad} lods"));
    }
    if let Some(ls) = dat1.get_section_data(look::TAG_LOOK).and_then(|d| LookSection::parse(d).ok()) {
        let mut over = 0;
        for l in &ls.looks {
            for lod in &l.lods {
                if lod.count > 0 && (lod.start as usize + lod.count as usize) > subset_count {
                    over += 1;
                }
            }
        }
        st.check("look LOD ranges inside subset table", over == 0, name, || format!("{over} ranges"));
    }

    // ---- skeleton ----
    let joint_count = dat1.get_section_data(joints::TAG_JOINTS).map(|d| d.len() / 16).unwrap_or(0);
    if let Some(h) = dat1
        .get_section_data(skeleton::TAG_JOINT_HIERARCHY)
        .and_then(|d| JointHierarchy::parse(d).ok())
    {
        let mirror = dat1.get_section_data(skeleton::TAG_MIRROR_IDS).map(|d| d.len() / 4).unwrap_or(0);
        let leaf = dat1.get_section_data(skeleton::TAG_LEAF_IDS).map(|d| d.len() / 2).unwrap_or(0);
        st.check("hierarchy.joint_count == Joint/16", h.joint_count as usize == joint_count, name,
            || format!("{} vs {joint_count}", h.joint_count));
        st.check("hierarchy.field2 == MirrorIds/4", h.mirror_count as usize == mirror, name,
            || format!("{} vs {mirror}", h.mirror_count));
        st.check("hierarchy.field3 == LeafIds/2", h.leaf_count as usize == leaf, name,
            || format!("{} vs {leaf}", h.leaf_count));
    }
    if let Some(bs) = dat1
        .get_section_data(skeleton::TAG_JOINT_BSPHERES)
        .and_then(|d| JointBspheres::parse(d).ok())
    {
        let bad = (0..bs.spheres.len())
            .filter(|&i| bs.spheres[i].joint % JointBspheres::JOINT_STRIDE != 0
                || bs.joint_index(i) as usize >= joint_count)
            .count();
        st.check("bsphere joint offset / 64 in range", bad == 0, name, || format!("{bad} spheres"));
    }
    let joint_list = dat1
        .get_section_data(joints::TAG_JOINTS)
        .and_then(|d| Joint::parse_all(d).ok())
        .unwrap_or_default();
    if !joint_list.is_empty() {
        let n = joint_list.len();
        let mut desc = vec![0u16; n];
        for i in (0..n).rev() {
            let p = joint_list[i].parent;
            if p >= 0 && (p as usize) < i {
                desc[p as usize] += desc[i] + 1;
            }
        }
        let sub_bad = (0..n).filter(|&i| joint_list[i].subtree_count != desc[i]).count();
        st.check("joint subtree_count == descendant count", sub_bad == 0, name, || format!("{sub_bad} joints"));
        let sib_bad = (0..n)
            .filter(|&i| joint_list[i].parent >= 0)
            .filter(|&i| {
                let next = i + 1 + joint_list[i].subtree_count as usize;
                let has = next < n && joint_list[next].parent == joint_list[i].parent;
                has != (joint_list[i].flags & joint_flags::HAS_NEXT_SIBLING != 0)
            })
            .count();
        st.check("joint HAS_NEXT_SIBLING matches the tree", sib_bad == 0, name, || format!("{sib_bad} joints"));
    }
    if let Some(r) = dat1
        .get_section_data(dynamics::TAG_RAGDOLL_META_DATA)
        .and_then(|d| RagdollMetaData::parse(d).ok())
    {
        let bad = r
            .bodies
            .iter()
            .filter(|b| b.joint != u16::MAX)
            .filter(|b| {
                let mut chain = Vec::new();
                let mut p = joint_list.get(b.joint as usize).map(|j| j.parent).unwrap_or(-1);
                while p >= 0 && chain.len() <= joint_list.len() {
                    chain.push(p);
                    p = joint_list.get(p as usize).map(|j| j.parent).unwrap_or(-1);
                }
                r.ancestors_of(b) != chain.as_slice()
                    || joint_list.get(b.joint as usize).map(|j| j.hash) != Some(b.joint_hash)
            })
            .count();
        st.check("ragdoll body ancestors == joint parent chain", bad == 0, name, || format!("{bad} bodies"));
    }
    if let Some(k) = dat1
        .get_section_data(dynamics::TAG_IK_SETUP)
        .and_then(|d| IkSetup::parse(d).ok())
    {
        let tiled = k.chains.iter().try_fold(0usize, |acc, c| (c.joint_start as usize == acc).then(|| acc + c.joint_count as usize));
        st.check("IK chains tile the joint-info array", tiled == Some(k.joints.len()), name,
            || format!("{tiled:?} vs {}", k.joints.len()));
    }
    if let Some(bytes) = dat1.get_section_data(dynamics::TAG_ANIM_DYNAMICS_DEF) {
        let d = AnimDynamicsDef::parse(bytes).unwrap_or_default();
        let ok = d.chains.iter().all(|c| {
            c.links.0 as usize + c.links.1 as usize <= d.links.len()
                && c.points.0 as usize + c.points.1 as usize <= d.points.len()
                && c.bends.0 as usize + c.bends.1 as usize <= d.bends.len()
        });
        st.check("anim dynamics chain runs in range", ok && !d.chains.is_empty(), name, || "bad chain run".into());
    }

    // ---- morphs ----
    if let Some(mi) = dat1
        .get_section_data(morph::TAG_ANIM_MORPH_INFO)
        .and_then(|d| AnimMorphInfo::parse(d).ok())
    {
        let data_s = dat1.get_section_data(morph::TAG_ANIM_MORPH_DATA).unwrap_or(&[]);
        let idx_s = dat1.get_section_data(morph::TAG_ANIM_MORPH_INDICES).unwrap_or(&[]);
        let subs = dat1
            .get_section_data(meshes::TAG_MESHES)
            .and_then(|d| MeshDefinition::parse_all(d).ok())
            .unwrap_or_default();
        let batches = dat1
            .get_section_data(skin::TAG_SKIN_BATCH)
            .and_then(|d| SkinBatch::parse_all(d).ok())
            .unwrap_or_default();
        let (mut size_bad, mut count_bad, mut oob, mut chunks_bad, mut outside_batch) = (0usize, 0usize, 0usize, 0usize, 0usize);
        for e in &mi.entries {
            let mut total = 0usize;
            for s in 0..e.subset_count as usize {
                let (lo, hi) = e.subset_chunks(s);
                total += e.chunks[lo..hi].iter().map(|c| e.chunk_bytes(c.0)).sum::<usize>();
                let Some(md) = subs.get(e.subset_ids[s] as usize) else { continue };
                if hi - lo != md.anim_vert_batch_count() as usize {
                    chunks_bad += 1;
                }
                let bases = morph::chunk_bases(md.first_skin_batch, md.skin_batch_count(), &batches);
                let d = AnimMorphInfo::decode_subset(e, s, data_s, idx_s, &bases);
                if d.len() != e.subset_vertex_counts[s] as usize {
                    count_bad += 1;
                }
                if d.iter().any(|x| x.vertex >= md.vertex_count) {
                    oob += 1;
                }
                // Every delta of chunk k must land inside skin batch k.
                let mut next = 0usize;
                for (k, ci) in (lo..hi).enumerate() {
                    let n = e.chunks[ci].0 as usize;
                    let b = batches.get(md.first_skin_batch as usize + k);
                    let inside = d.get(next..next + n).unwrap_or(&[]).iter().all(|x| {
                        b.is_some_and(|b| x.vertex >= b.first_vertex as u32 && x.vertex < b.first_vertex as u32 + b.vertex_count as u32)
                    });
                    outside_batch += !inside as usize;
                    next += n;
                }
            }
            if total != e.vertex_size as usize {
                size_bad += 1;
            }
        }
        st.check("morph chunk sizes == vertex_size", size_bad == 0, name, || format!("{size_bad} morphs"));
        st.check("morph index run counts == vertex counts", count_bad == 0, name, || format!("{count_bad} subsets"));
        st.check("morph vertex ids in range", oob == 0, name, || format!("{oob} subsets"));
        st.check("morph chunks per subset == anim-vert batches", chunks_bad == 0, name, || format!("{chunks_bad} subsets"));
        st.check("morph chunk k lies inside skin batch k", outside_batch == 0, name, || format!("{outside_batch} chunks"));
    }

    // ---- splines ----
    if let Some(sp) = dat1.get_section_data(splines::TAG_SPLINES).and_then(|d| parse_splines(d).ok()) {
        let cvs = dat1.get_section_data(splines::TAG_SPLINE_CVS).map(|d| d.len() / 8).unwrap_or(0);
        let mut ranges: Vec<_> = sp.iter().map(|s| s.cv_range()).collect();
        ranges.sort_by_key(|r| r.start);
        let tiles = ranges.iter().try_fold(0usize, |acc, r| (r.start == acc).then_some(r.end));
        st.check("spline CV ranges tile the CV array", tiles == Some(cvs), name,
            || format!("{tiles:?} vs {cvs}"));
        if let Some(ss) = dat1
            .get_section_data(splines::TAG_SPLINE_SUBSETS)
            .and_then(|d| SplineSubsets::parse(d).ok())
        {
            st.check("spline groups tile the strand array",
                ss.strand_total() as usize == sp.len(), name,
                || format!("{} vs {}", ss.strand_total(), sp.len()));
        }
    }

    // ---- subsets ----
    if let Some(subs) = dat1
        .get_section_data(meshes::TAG_MESHES)
        .and_then(|d| MeshDefinition::parse_all(d).ok())
    {
        let mat_count = dat1.get_section_data(looks::TAG_MATERIAL).map(|d| d.len() / 32).unwrap_or(0);
        let vtotal = dat1.get_section_data(geo::TAG_VERTEXES).map(|d| d.len() / 16).unwrap_or(0);
        let batches = dat1
            .get_section_data(skin::TAG_SKIN_BATCH)
            .and_then(|d| SkinBatch::parse_all(d).ok())
            .unwrap_or_default();
        let mpu = b.meters_per_unit;
        let (mut bad_mat, mut bad_range, mut bad_sphere, mut bad_ext, mut bad_batch) = (0, 0, 0, 0, 0);
        for m in &subs {
            if (m.material_index as usize) >= mat_count {
                bad_mat += 1;
            }
            if (m.vertex_start + m.vertex_count) as usize > vtotal {
                bad_range += 1;
                continue;
            }
            let p = &pos[m.vertex_start as usize..(m.vertex_start + m.vertex_count) as usize];
            if !p.is_empty() {
                // Tolerances: 1% plus a few quanta, since both fields are stored as i16s.
                let r = p.iter().map(|v| dist(*v, m.bsphere_center)).fold(0f32, f32::max);
                if m.bsphere_radius != i16::MAX && r > m.bsphere_radius_m(mpu) * 1.01 + 8.0 * mpu {
                    bad_sphere += 1;
                }
                let half = half_extents(p);
                let e = m.aabb_extents_m(mpu);
                if (0..3).any(|k| (half[k] - e[k]).abs() > 0.01 * e[k].abs() + 4.0 * mpu && half[k] > e[k]) {
                    bad_ext += 1;
                }
            }
            if m.skin_batch_count() > 0 && !batches.is_empty() {
                let a = m.first_skin_batch as usize;
                let run = batches.get(a..a + m.skin_batch_count() as usize).unwrap_or(&[]);
                let mut next = 0u32;
                let contiguous = run.iter().all(|x| {
                    let ok = x.first_vertex as u32 == next;
                    next += x.vertex_count as u32;
                    ok
                });
                if !contiguous || next != m.vertex_count || run.iter().any(|x| x.vertex_count as usize > SKIN_BATCH_VERT_MAX) {
                    bad_batch += 1;
                }
            }
        }
        st.check("subset material index in range", bad_mat == 0, name, || format!("{bad_mat} subsets"));
        st.check("subset vertex range inside StdVert", bad_range == 0, name, || format!("{bad_range} subsets"));
        st.check("subset bsphere (r*mpu*2) encloses its vertices", bad_sphere == 0, name, || format!("{bad_sphere} subsets"));
        st.check("subset aabb_extents (*mpu) cover its vertices", bad_ext == 0, name, || format!("{bad_ext} subsets"));
        st.check("subset skin batches (low byte) cover exactly its vertices", bad_batch == 0, name, || format!("{bad_batch} subsets"));
        let mut first = std::collections::HashMap::new();
        let bad_proxy = subs.iter().enumerate().filter(|(i, m)| {
            let owner = *first.entry((m.vertex_start, m.vertex_count, m.index_start, m.index_count)).or_insert(*i);
            m.lod_ref() as usize != owner || m.is_lod_proxy() != (owner != *i)
        }).count();
        st.check("lod_proxy_id ref == first subset of its geometry block", bad_proxy == 0, name, || format!("{bad_proxy} subsets"));

        // The importer recomputes these for replaced geometry; they must reproduce the shipped values.
        if let Some(vd) = dat1.get_section_data(geo::TAG_VERTEXES) {
            let uv_scale = built::get_uv_scale(dat1.get_section_data(built::TAG_BUILT).unwrap_or(&[]));
            let verts = VertexesSection::parse_scaled(vd, mpu, uv_scale).map(|v| v.vertexes).unwrap_or_default();
            let idx = dat1.get_section_data(geo::TAG_INDEXES).and_then(|d| IndexesSection::parse(d).ok()).map(|i| i.values).unwrap_or_default();
            let (mut bounds_bad, mut area_bad, mut density_bad, mut n) = (0, 0, 0, 0);
            let near = |a: f32, b: f32, rel: f32, abs: f32| (a - b).abs() <= rel * a.abs().max(b.abs()) + abs;
            for m in &subs {
                let base = if m.has_relative_indices() { m.vertex_start as usize } else { 0 };
                let Some(s) = omnitool_lib::tools::model_converter::bounds::subset_stats(
                    &verts, &idx, m.vertex_start as usize, m.vertex_count as usize,
                    m.index_start as usize, m.index_count as usize, base, mpu,
                ) else { continue };
                n += 1;
                let ok_bounds = (0..3).all(|k| near(s.bsphere_center[k], m.bsphere_center[k], 0.0, 2.0 * mpu))
                    && (m.bsphere_radius == i16::MAX || near(s.bsphere_radius as f32, m.bsphere_radius as f32, 0.01, 2.0))
                    && (0..3).all(|k| near(s.aabb_extents[k] as f32, m.aabb_extents[k] as f32, 0.01, 2.0));
                bounds_bad += !ok_bounds as usize;
                if m.surface_area_sqrt != u16::MAX && m.index_count >= 3 {
                    area_bad += !near(s.surface_area_sqrt as f32, m.surface_area_sqrt as f32, 0.05, 2.0) as usize;
                }
                if m.uv_density_u > 0.0 && s.uv_density.0 > 0.0 {
                    // A per-subset streaming bias scales the stored value by a power of two.
                    let biased = |stored: f32, got: f32| {
                        let l = (stored / got).log2();
                        near(got, stored, 0.1, 0.0) || (l.round() != 0.0 && (l - l.round()).abs() < 0.05)
                    };
                    let off = !(biased(m.uv_density_u, s.uv_density.0) && biased(m.uv_density_v, s.uv_density.1));
                    density_bad += off as usize;
                    if off {
                        let r = |a: f32, b: f32| format!("{:.2}", (a / b).log2());
                        *st.density_log2_ratios
                            .entry(format!("u {} v {}", r(m.uv_density_u, s.uv_density.0), r(m.uv_density_v, s.uv_density.1)))
                            .or_insert(0) += 1;
                    }
                }
            }
            if n > 0 {
                st.check("recomputed subset bounds == stored (1% + 2 units)", bounds_bad == 0, name, || format!("{bounds_bad}/{n} subsets"));
                st.check("recomputed surface_area_sqrt == stored (5%)", area_bad == 0, name, || format!("{area_bad}/{n} subsets"));
                st.check("recomputed uv density == stored (10%, x2^k bias)", density_bad == 0, name, || format!("{density_bad}/{n} subsets"));
                st.subset_stat_counts.0 += n;
                st.subset_stat_counts.1 += bounds_bad;
                st.subset_stat_counts.2 += area_bad;
                st.subset_stat_counts.3 += density_bad;
            }
        }
    }
}

fn walk_dir(root: &Path, limit: usize, st: &mut Stats) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            if st.models >= limit {
                return;
            }
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|s| s.to_str()) == Some("model") {
                if let Ok(bytes) = std::fs::read(&p) {
                    audit(&p.file_name().unwrap().to_string_lossy(), &bytes, st);
                }
            }
        }
    }
}

fn walk_archives(toc_path: &Path, archives: &Path, limit: usize, st: &mut Stats) {
    let data = match std::fs::read(toc_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cannot read toc {}: {e}", toc_path.display());
            return;
        }
    };
    let toc = match Toc::parse(&data) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot parse toc: {e}");
            return;
        }
    };
    // Models live only in archives whose name mentions "model", so the other
    // ~30 GB never has to be decompressed.
    let names = toc.archive_filenames();
    let model_archives: Vec<u32> = names
        .iter()
        .enumerate()
        .filter(|(_, n)| n.to_lowercase().contains("model"))
        .map(|(i, _)| i as u32)
        .collect();
    let all_assets = toc.assets();
    let assets: Vec<_> = all_assets
        .into_iter()
        .filter(|a| model_archives.contains(&a.archive_index))
        .collect();
    eprintln!(
        "{} assets in {} model archives ({:?})",
        assets.len(),
        model_archives.len(),
        model_archives.iter().map(|&i| names[i as usize].clone()).collect::<Vec<_>>()
    );
    let cache = toc.archive_cache(archives);
    let mut scanned = 0usize;
    for a in &assets {
        if st.models >= limit {
            break;
        }
        scanned += 1;
        if scanned % 5000 == 0 {
            eprintln!("  {scanned}/{} assets, {} models", assets.len(), st.models);
        }
        let Ok(bytes) = toc.extract_asset_with_cache(a, &cache) else { continue };
        if std::env::var("AUDIT_MAGIC").is_ok() && bytes.len() >= 4 {
            let m = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
            *st.unknown_tags.entry(m).or_insert(0) += 1;
            continue;
        }
        // Cheap pre-filter: only DAT1 payloads carrying a Model Built section.
        if !looks_like_model(&bytes) {
            continue;
        }
        audit(&format!("{:016X}", a.asset_id), &bytes, st);
    }
}

/// Peeks at the DAT1 section table without decompressing anything else.
fn looks_like_model(bytes: &[u8]) -> bool {
    let at = if bytes.len() > 36 && u32::from_le_bytes(bytes[0..4].try_into().unwrap()) != 0x44415431
    {
        36
    } else {
        0
    };
    if bytes.len() < at + 16 {
        return false;
    }
    if u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) != 0x44415431 {
        return false;
    }
    let n = u16::from_le_bytes(bytes[at + 12..at + 14].try_into().unwrap()) as usize;
    (0..n).any(|i| {
        let o = at + 16 + i * 12;
        bytes.len() >= o + 4
            && u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap()) == built::TAG_BUILT
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| {
        args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned()
    };
    let limit: usize = get("--limit").and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
    let only_failures = args.iter().any(|a| a == "--failures");

    let mut st = Stats::default();
    let t0 = std::time::Instant::now();

    if let (Some(toc), Some(arch)) = (get("--toc"), get("--archives")) {
        walk_archives(Path::new(&toc), Path::new(&arch), limit, &mut st);
    } else if let Some(dir) = args.first().filter(|a| !a.starts_with("--")) {
        walk_dir(&PathBuf::from(dir), limit, &mut st);
    } else {
        eprintln!("usage: model_audit <dir> | --toc <toc> --archives <dir> [--limit N] [--failures]");
        std::process::exit(1);
    }

    println!("\n=== audited {} models in {:.1}s ===", st.models, t0.elapsed().as_secs_f32());
    if std::env::var("AUDIT_MAGIC").is_ok() {
        let mut v: Vec<_> = st.unknown_tags.iter().collect();
        v.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
        println!("-- asset magic histogram (top 20) --");
        for (m, c) in v.into_iter().take(20) {
            println!("  0x{m:08X}: {c}");
        }
        return;
    }
    if st.models == 0 {
        return;
    }

    println!("\n-- cross-checks --");
    let mut failed_any = false;
    for (name, (ok, bad)) in &st.checks {
        let flag = if *bad == 0 { "ok  " } else { "FAIL" };
        if *bad > 0 {
            failed_any = true;
        }
        println!("  {flag} {name:<46} {ok} passed, {bad} failed");
    }

    let (n, b, a, d) = st.subset_stat_counts;
    if n > 0 {
        println!(
            "\n-- recomputed subset stats vs stored: {n} subsets; bounds off on {b}, area off on {a}, uv density off on {d} --"
        );
        let mut v: Vec<_> = st.density_log2_ratios.iter().collect();
        v.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
        for (k, c) in v.into_iter().take(12) {
            println!("  uv density log2(stored/recomputed) {k}: {c}");
        }
    }

    if !st.failures.is_empty() {
        println!("\n-- first {} failures --", st.failures.len());
        for (who, what) in &st.failures {
            println!("  {who}  {what}");
        }
    }

    if only_failures {
        return;
    }

    println!("\n-- Built feature flags (value: count) --");
    for (v, c) in &st.flag_counts {
        println!("  0x{v:08X}: {c}");
    }

    // For each bit, the section its state predicts best, as
    // P(section | bit set) vs P(section | bit clear). Statistical rather than
    // all-or-nothing, so a few outliers among 11k models cannot hide a signal.
    println!("
-- feature bit -> best-predicted section --");
    for bit in 0..32u32 {
        let mut best: Option<(f64, u32)> = None;
        for t in KNOWN_TAGS {
            let Some(&(sh, sn, ch, cn)) = st.bit_section.get(&(bit, *t)) else { continue };
            if sh + sn == 0 || ch + cn == 0 {
                continue;
            }
            let sep = sh as f64 / (sh + sn) as f64 - ch as f64 / (ch + cn) as f64;
            if best.map_or(true, |x| sep > x.0) {
                best = Some((sep, *t));
            }
        }
        if let Some((sep, tag)) = best {
            if sep > 0.9 {
                let &(sh, sn, ch, cn) = st.bit_section.get(&(bit, tag)).unwrap();
                println!(
                    "  bit {bit:2} (0x{:08X}) -> 0x{tag:08X}  P(sec|set)={:.3} ({}/{})  P(sec|clear)={:.3} ({}/{})",
                    1u32 << bit,
                    sh as f64 / (sh + sn) as f64, sh, sh + sn,
                    ch as f64 / (ch + cn) as f64, ch, ch + cn
                );
            }
        }
    }

    println!("\n-- UV shift field --");
    for (v, c) in &st.uv_shift {
        println!("  0x{v:X}: {c}");
    }
    println!("\n-- position scale --");
    for (v, c) in &st.pos_scale {
        println!("  {v}: {c}");
    }
    if !st.unknown_tags.is_empty() {
        println!("\n-- section tags not in the doc --");
        for (t, c) in &st.unknown_tags {
            println!("  0x{t:08X}: {c} models  (crc name unknown)");
        }
    }

    if failed_any {
        std::process::exit(2);
    }
}

#[allow(dead_code)]
fn unused(_: &Dat1, _: fn(&str) -> u32) {
    let _ = crc32::hash;
}
