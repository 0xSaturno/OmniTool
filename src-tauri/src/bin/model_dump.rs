//! Dumps every reversed section of a .model asset in readable form.
//!
//! usage: model_dump <file.model> [section...]
//! With no section names it prints a summary of all of them.

use omnitool_lib::core::dat1::Dat1;
use omnitool_lib::tools::model_converter::model::ModelFile;
use omnitool_lib::tools::model_converter::sections::*;

fn tag_name(tag: u32) -> String {
    match section_name(tag) {
        Some((n, true)) => n.to_string(),
        Some((n, false)) => format!("{n} (inferred)"),
        None => "?".into(),
    }
}

fn s(dat1: &Dat1, offset: u64) -> String {
    dat1.get_string(offset as u32).unwrap_or_default()
}

fn joint_names(dat1: &Dat1) -> Vec<String> {
    dat1.get_section_data(joints::TAG_JOINTS)
        .map(|d| {
            Joint::parse_all(d)
                .unwrap_or_default()
                .iter()
                .map(|j| dat1.get_string(j.string_offset).unwrap_or_default())
                .collect()
        })
        .unwrap_or_default()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: model_dump <file.model> [section...]");
        std::process::exit(1);
    }
    let want: Vec<String> = args[1..].iter().map(|a| a.to_lowercase()).collect();
    let show = |k: &str| want.is_empty() || want.iter().any(|w| k.contains(w.as_str()));

    let data = std::fs::read(&args[0]).expect("read model");
    let model = ModelFile::parse(&data).expect("parse model");
    let dat1 = &model.dat1;
    let jn = joint_names(dat1);
    let jname = |j: usize| jn.get(j).cloned().unwrap_or_else(|| j.to_string());

    println!("== sections ({}) ==", dat1.sections.len());
    let mut ordered: Vec<_> = dat1.sections.iter().enumerate().collect();
    ordered.sort_by_key(|(_, s)| s.offset);
    for (i, sec) in ordered {
        println!(
            "  0x{:08X}  {:<36}  size={}",
            sec.tag,
            tag_name(sec.tag),
            dat1.section_data[i].len()
        );
    }

    let built = dat1.get_section_data(built::TAG_BUILT).and_then(Built::parse);
    let mpu = built.as_ref().map(|b| b.meters_per_unit).unwrap_or(1.0 / 4096.0);

    if show("built") {
        if let Some(b) = &built {
            println!("\n== Built ==");
            println!("  bsphere         c={:?} r={}", b.bsphere_center, b.bsphere_radius);
            println!("  aabb_extents    {:?}", b.aabb_extents);
            println!("  mesh_center     {:?}", b.mesh_center);
            println!("  meters_per_unit {} (1/{})", b.meters_per_unit, 1.0 / b.meters_per_unit);
            println!(
                "  uv scales       uv0={} uv1={} (field 0x{:X})",
                b.uv0_scale(),
                b.uv1_scale(),
                b.uv_log_scales
            );
            println!("  lod_distances   {:?}", b.lod_distances);
            println!("  counts          verts={} indices={}", b.vertex_count, b.index_count);
            println!("  flags           0x{:08X} {:?}", b.flags, b.flag_names());
            println!("  content_flags   0x{:X}", b.content_flags);
            println!(
                "  anim            ambient={} max_disp={} dyn_force=[{}, {}]",
                b.ambient_animation, b.max_displacement, b.min_dynamic_force, b.max_dynamic_force
            );
            println!(
                "  render          z_bias={} alpha_sort_bias={} fade_out={} shadow_fade={} shadow_lod={}",
                b.z_bias(),
                b.alpha_sort_bias(),
                b.fade_out_dist,
                b.shadow_fade_dist,
                b.shadow_casting_lod
            );
            println!(
                "  hashes          av=0x{:08X} audio=0x{:08X}  strand_subsets={}",
                b.av_material_hash, b.audio_material_hash, b.strand_subset_count
            );
        }
    }

    if show("subset") {
        if let Some(subs) = dat1
            .get_section_data(meshes::TAG_MESHES)
            .and_then(|d| MeshDefinition::parse_all(d).ok())
        {
            println!("\n== Subsets ({}) ==", subs.len());
            for (i, m) in subs.iter().enumerate() {
                println!(
                    "  [{i:4}] v {:7}+{:<6} i {:8}+{:<7} mat {:3} flags 0x{:04X} lod {} ref {}{} skin {}+{} (anim {}) r={:.3} area={:.3} fade={} mat_lod={}",
                    m.vertex_start,
                    m.vertex_count,
                    m.index_start,
                    m.index_count,
                    m.material_index,
                    m.flags,
                    m.lod_id(),
                    m.lod_ref(),
                    if m.is_lod_proxy() { " proxy" } else { "" },
                    m.first_skin_batch,
                    m.skin_batch_count(),
                    m.anim_vert_batch_count(),
                    m.bsphere_radius_m(mpu),
                    m.surface_area_m2(),
                    m.fade_out_dist,
                    m.material_lod_dist
                );
            }
        }
    }

    if show("material") {
        if let Some(m) = dat1
            .get_section_data(looks::TAG_MATERIAL)
            .and_then(|d| MaterialSection::parse(d).ok())
        {
            println!("\n== Material ({} slots) ==", m.slots.len());
            for (i, slot) in m.slots.iter().enumerate() {
                let name = s(dat1, slot.name_offset);
                let id = m.id_for_name(&name);
                println!(
                    "  [{i:3}] aid={:016X}{} flags=0x{:X}  {}  <- {}",
                    id.map(|x| x.asset_id).unwrap_or(0),
                    if id.is_some() { "" } else { " (no id entry)" },
                    id.map(|x| x.flags).unwrap_or(0),
                    name,
                    s(dat1, slot.path_offset)
                );
            }
        }
    }

    if show("look") {
        let groups = dat1
            .get_section_data(looks::TAG_LOOK_GROUP)
            .and_then(|d| parse_look_groups(d).ok())
            .unwrap_or_default();
        if !groups.is_empty() {
            println!("\n== Look Groups ==");
            for g in &groups {
                println!("  {:<24} hash={:08X} looks={:?}", s(dat1, g.name_offset as u64), g.name_hash, g.looks);
            }
        }
        let look_count = dat1
            .get_section_data(look::TAG_LOOK)
            .map(|d| d.len() / 32)
            .unwrap_or(0);
        if let Some(lb) = dat1
            .get_section_data(looks::TAG_LOOK_BUILT)
            .and_then(|d| LookBuiltSection::parse(d, look_count).ok())
        {
            println!("\n== Look Built ({} looks) ==", lb.looks.len());
            for (i, l) in lb.looks.iter().enumerate() {
                let mask_ok = expand_mask(&l.bsphere_mask) == l.bspheres;
                println!(
                    "  [{i}] {:<28} bodies={:?} cloths={:?} cloth_joints={} collidables={} bspheres={:?} mask_matches={} lod0_subsets={}",
                    s(dat1, l.name_offset as u64),
                    l.rigid_bodies,
                    l.cloths,
                    l.cloth_joints.len(),
                    l.cloth_collidables.len(),
                    l.bspheres,
                    mask_ok,
                    l.lod_subsets(0).len()
                );
            }
        }
        if let Some(v) = dat1
            .get_section_data(looks::TAG_LOOK_BVH_INFO)
            .and_then(|d| parse_look_bvh_info(d).ok())
        {
            println!("\n== Look BVH Info ==");
            for (i, e) in v.iter().enumerate() {
                println!("  [{i}] usage=0x{:X} lod={}", e.usage, e.lod);
            }
        }
    }

    if show("locator") {
        if let Some(l) = dat1
            .get_section_data(skeleton::TAG_LOCATOR)
            .and_then(|d| Locator::parse_all(d).ok())
        {
            println!("\n== Locators ({}) ==", l.len());
            for (i, loc) in l.iter().enumerate() {
                let owner = loc.joint.map(|j| jname(j as usize)).unwrap_or_else(|| "-".into());
                println!(
                    "  [{i:3}] {:08X} {:<34} joint={owner} pos=({:.3},{:.3},{:.3})",
                    loc.hash,
                    s(dat1, loc.string_offset as u64),
                    loc.transform[9],
                    loc.transform[10],
                    loc.transform[11]
                );
            }
        }
    }

    if show("joint") {
        if let Some(js) = dat1
            .get_section_data(joints::TAG_JOINTS)
            .and_then(|d| Joint::parse_all(d).ok())
        {
            println!("\n== Joints ({}) ==", js.len());
            for (i, j) in js.iter().enumerate() {
                let f = j.flags;
                let tags: Vec<&str> = [
                    (joint_flags::MIRROR_PAIRED_LEFT, "L"),
                    (joint_flags::MIRROR_PAIRED_RIGHT, "R"),
                    (joint_flags::END_JOINT, "end"),
                    (joint_flags::CLOTH_JOINT, "cloth"),
                    (joint_flags::HAS_NEXT_SIBLING, "sib"),
                    (joint_flags::SEGMENT_SCALE_COMPENSATE, "ssc"),
                ]
                .into_iter()
                .filter(|(b, _)| f & b != 0)
                .map(|(_, n)| n)
                .collect();
                println!(
                    "  [{i:3}] {:<30} parent={:<4} subtree={:<3} {}",
                    jname(i),
                    j.parent,
                    j.subtree_count,
                    tags.join(",")
                );
            }
        }
        if let Some(h) = dat1
            .get_section_data(skeleton::TAG_JOINT_HIERARCHY)
            .and_then(|d| JointHierarchy::parse(d).ok())
        {
            println!(
                "  hierarchy: flags={} joints={} mirrors={} leaves={} spline_root=0x{:X} spline_joints={} spline_radius={}",
                h.flags, h.joint_count, h.mirror_count, h.leaf_count, h.spline_root, h.spline_joint_count, h.spline_radius
            );
        }
    }

    if show("bsphere") {
        if let Some(b) = dat1
            .get_section_data(skeleton::TAG_JOINT_BSPHERES)
            .and_then(|d| JointBspheres::parse(d).ok())
        {
            println!("\n== Joint Bspheres ({}) ==", b.spheres.len());
            for i in 0..b.spheres.len() {
                let sp = b.spheres[i];
                let j = b.joint_index(i) as usize;
                println!(
                    "  [{i:3}] c=({:8.4},{:8.4},{:8.4}) r={:.4}  joint {j} {}",
                    sp.center.0,
                    sp.center.1,
                    sp.center.2,
                    sp.radius,
                    jname(j)
                );
            }
        }
    }

    if show("mirror") {
        if let Some(m) = dat1
            .get_section_data(skeleton::TAG_MIRROR_IDS)
            .and_then(|d| parse_mirror_ids(d).ok())
        {
            println!("\n== Mirror Ids ({}) ==", m.len());
            for e in &m {
                let kind = match e.kind {
                    mirror_kind::PAIRED => "paired",
                    mirror_kind::PAIRED_ATTACH => "paired-attach",
                    mirror_kind::PAIRED_LINK => "paired-link",
                    mirror_kind::UNPAIRED => "unpaired",
                    _ => "?",
                };
                println!(
                    "  {:>3} {:<28} <-> {:>3} {:<28} {kind}",
                    e.joint,
                    jname(e.joint as usize),
                    e.mirror_joint,
                    jname(e.mirror_joint as usize)
                );
            }
        }
    }

    if show("morph") {
        if let Some(m) = dat1
            .get_section_data(morph::TAG_ANIM_MORPH_INFO)
            .and_then(|d| AnimMorphInfo::parse(d).ok())
        {
            println!(
                "\n== Anim Morph Info ({} morphs, {} LF/RT pairs, data_size={}) ==",
                m.header.lookup_count, m.header.pairs_count, m.header.data_size
            );
            for e in &m.entries {
                println!(
                    "  {:08X} {:<34} elems={} ebits={} cbits={} subsets={} vsize={} isize={}",
                    e.id,
                    s(dat1, e.name_offset as u64),
                    e.element_count,
                    e.element_bit_size,
                    e.component_bit_size,
                    e.subset_count,
                    e.vertex_size,
                    e.index_size
                );
            }
            println!("  -- pairs --");
            for (l, r) in &m.pairs {
                println!("  {l:08X} <-> {r:08X}");
            }

            // Structural self-checks: per-chunk index counts must hit the stored
            // vertex counts exactly, and every id must land inside its subset.
            let (data, idx, subsets) = (
                dat1.get_section_data(morph::TAG_ANIM_MORPH_DATA),
                dat1.get_section_data(morph::TAG_ANIM_MORPH_INDICES),
                dat1.get_section_data(meshes::TAG_MESHES),
            );
            if let (Some(data), Some(idx), Some(sub)) = (data, idx, subsets) {
                let subs = MeshDefinition::parse_all(sub).unwrap_or_default();
                let batches = dat1
                    .get_section_data(skin::TAG_SKIN_BATCH)
                    .and_then(|d| SkinBatch::parse_all(d).ok())
                    .unwrap_or_default();
                let (mut count_ok, mut count_bad, mut oob) = (0usize, 0usize, 0usize);
                let mut sample = Vec::new();
                for e in &m.entries {
                    for si in 0..e.subset_count as usize {
                        let bases = subs
                            .get(e.subset_ids[si] as usize)
                            .map(|md| morph::chunk_bases(md.first_skin_batch, md.skin_batch_count(), &batches))
                            .unwrap_or_default();
                        let deltas = AnimMorphInfo::decode_subset(e, si, data, idx, &bases);
                        if deltas.len() == e.subset_vertex_counts[si] as usize {
                            count_ok += 1;
                        } else {
                            count_bad += 1;
                        }
                        if let Some(md) = subs.get(e.subset_ids[si] as usize) {
                            if deltas.iter().any(|d| d.vertex >= md.vertex_count) {
                                oob += 1;
                            }
                        }
                        if sample.len() < 3 && !deltas.is_empty() {
                            let d = &deltas[deltas.len() / 2];
                            sample.push(format!(
                                "    {} subset {} vtx {} pos_delta=({:+.5},{:+.5},{:+.5})",
                                s(dat1, e.name_offset as u64),
                                e.subset_ids[si],
                                d.vertex,
                                d.elements[0][0],
                                d.elements[0][1],
                                d.elements[0][2]
                            ));
                        }
                    }
                }
                println!(
                    "  -- decode: {count_ok} subsets with exact vertex counts, {count_bad} mismatched, {oob} with out-of-range ids --"
                );
                for l in sample {
                    println!("{l}");
                }
            }
        }
    }

    if show("ragdoll") || show("rig") {
        if let Some(r) = dat1
            .get_section_data(dynamics::TAG_RAGDOLL_META_DATA)
            .and_then(|d| RagdollMetaData::parse(d).ok())
        {
            println!("\n== Ragdoll Meta Data ({} bodies, flags=0x{:X}) ==", r.bodies.len(), r.flags);
            for b in &r.bodies {
                let chain: Vec<String> = r.ancestors_of(b).iter().map(|&j| jname(j as usize)).collect();
                println!(
                    "  {:<26} parent={:<26} ancestors={}",
                    if b.joint == u16::MAX { format!("(no joint {:08X})", b.joint_hash) } else { jname(b.joint as usize) },
                    if b.parent_joint == u16::MAX { "-".into() } else { jname(b.parent_joint as usize) },
                    chain.join(" > ")
                );
            }
        }
    }

    if show("ik") || show("rig") {
        if let Some(k) = dat1
            .get_section_data(dynamics::TAG_IK_SETUP)
            .and_then(|d| IkSetup::parse(d).ok())
        {
            println!(
                "\n== IK Setup ({} chains, {} joint infos, {} sticks) ==",
                k.chains.len(),
                k.joints.len(),
                k.sticks.len()
            );
            for c in &k.chains {
                let chain: Vec<String> = k.chain_joints(c).iter().map(|&j| jname(j as usize)).collect();
                println!(
                    "  goal={:08X} err={} iters={} flags=0x{:X} sticks={} chain={}",
                    c.goal_locator_hash,
                    c.solver_error,
                    c.max_iterations,
                    c.flags,
                    c.stick_count,
                    chain.join(" > ")
                );
            }
        }
    }

    if show("cloth") || show("rig") {
        if let Some(c) = dat1
            .get_section_data(dynamics::TAG_CLOTH_META_DATA)
            .and_then(|d| ClothMetaData::parse(d).ok())
        {
            println!(
                "\n== Cloth Meta Data (flags={}, {} instances, {} collidables, {} vertex-cloth subsets) ==",
                c.flags,
                c.instance_count,
                c.collidable_count,
                c.vertex_cloth_subsets.len()
            );
            let names: Vec<String> = c.influence_joints.iter().map(|&j| jname(j as usize)).collect();
            println!("  influence joints: {}", names.join(", "));
            for (i, ids) in c.instance_collidables.iter().enumerate() {
                println!("  instance {i} collidables {ids:?}");
            }
        }
    }

    if show("dynamics") || show("rig") {
        if let Some(d) = dat1
            .get_section_data(dynamics::TAG_ANIM_DYNAMICS_DEF)
            .and_then(|d| AnimDynamicsDef::parse(d).ok())
        {
            println!(
                "\n== Anim Dynamics Def ({} points, {} links, {} attaches, {} bends, {} chains, {} joint elems) ==",
                d.points.len(),
                d.links.len(),
                d.attaches.len(),
                d.bends.len(),
                d.chains.len(),
                d.joint_elems.len()
            );
            for c in &d.chains {
                println!(
                    "  {:<24} type={} links {}+{} points {}+{} bends {}+{} tethers={:?} gravity={:?} damping={}",
                    s(dat1, c.name_offset as u64),
                    c.constraint_type,
                    c.links.0,
                    c.links.1,
                    c.points.0,
                    c.points.1,
                    c.bends.0,
                    c.bends.1,
                    c.tether_types,
                    c.gravity,
                    c.damping
                );
            }
        }
    }

    if show("spline") || show("hair") {
        if let Some(ss) = dat1
            .get_section_data(splines::TAG_SPLINE_SUBSETS)
            .and_then(|d| SplineSubsets::parse(d).ok())
        {
            let strands = dat1
                .get_section_data(splines::TAG_SPLINES)
                .and_then(|d| parse_splines(d).ok())
                .unwrap_or_default();
            let cvs = dat1
                .get_section_data(splines::TAG_SPLINE_CVS)
                .and_then(|d| parse_spline_cvs(d).ok())
                .unwrap_or_default();
            let referenced: usize = strands.iter().map(|s| s.cv_count as usize).sum();
            println!(
                "\n== Spline Subsets ({} groups, {} strands, {} implied) ==",
                ss.subsets.len(),
                strands.len(),
                ss.strand_total()
            );
            println!(
                "  cv records {}, referenced by strands {} -> {}",
                cvs.len(),
                referenced,
                if cvs.len() == referenced { "match" } else { "MISMATCH" }
            );
            for g in &ss.subsets {
                let mut lo = [f32::MAX; 3];
                let mut hi = [f32::MIN; 3];
                let (a, b) = (g.first_strand as usize, (g.first_strand + g.strand_count) as usize);
                for st in strands.get(a..b).unwrap_or(&[]) {
                    for p in cvs.get(st.cv_range()).unwrap_or(&[]) {
                        let d = p.decode(mpu);
                        for (k, v) in [d.0, d.1, d.2].into_iter().enumerate() {
                            lo[k] = lo[k].min(v);
                            hi[k] = hi[k].max(v);
                        }
                    }
                }
                let bbox = if lo[0] <= hi[0] {
                    format!("x[{:6.3},{:6.3}] y[{:6.3},{:6.3}] z[{:6.3},{:6.3}]", lo[0], hi[0], lo[1], hi[1], lo[2], hi[2])
                } else {
                    "(empty)".into()
                };
                println!(
                    "  {:08X} {:<28} strands {:5}..{:<5} lod {}/{} tess {}..{} {}",
                    g.name_hash,
                    s(dat1, g.name_offset as u64),
                    g.first_strand,
                    g.first_strand + g.strand_count,
                    g.lod_distance,
                    g.lod_reduction,
                    g.tess_min,
                    g.tess_max,
                    bbox
                );
                println!("      config {}", s(dat1, g.config_path_offset as u64));
                for (k, slot) in SPLINE_TEXTURE_SLOTS.iter().enumerate() {
                    if g.texture_ids[k] != 0 {
                        println!("      {slot:<14} {:016X} {}", g.texture_ids[k], s(dat1, g.texture_path_offsets[k] as u64));
                    }
                }
            }
        }
    }

    if show("render") || show("override") || show("shadow") || show("perf") {
        if let Some(v) = dat1.get_section_data(render::TAG_RENDER_OVERRIDES).and_then(|d| parse_render_overrides(d).ok()) {
            println!("\n== Render Overrides ({}) ==", v.len());
            for o in &v {
                println!(
                    "  param={:08X} slot={:08X} value={:?} texture={:016X}",
                    o.name_hash, o.mapping_name_hash, o.value, o.texture_id
                );
            }
        }
        if let Some(v) = dat1.get_section_data(render::TAG_TEXTURE_OVERRIDES).and_then(|d| parse_texture_overrides(d).ok()) {
            println!("\n== Texture Overrides ({}) ==", v.len());
            for o in &v {
                println!("  {}", s(dat1, *o as u64));
            }
        }
        if let Some(v) = dat1.get_section_data(render::TAG_AMBIENT_SHADOW_PRIMS).and_then(|d| parse_ambient_shadow_prims(d).ok()) {
            println!("\n== Ambient Shadow Prims ({}) ==", v.len());
            for p in &v {
                println!(
                    "  c={:?} r={} a={:?} b={:?} joint/fade=0x{:X} last={}",
                    p.center, p.radius, p.offset_a, p.offset_b, p.joint_or_fade, p.is_terminator != 0
                );
            }
        }
        if let Some(v) = dat1.get_section_data(render::TAG_PERF_PROFILE_MAX_LOD).and_then(|d| parse_perf_profile_max_lod(d).ok()) {
            println!("\n== Perf Profile Max LoD ==");
            for p in &v {
                println!("  {:<14} max_lod={}", p.spec_name().unwrap_or("?"), p.max_lod);
            }
        }
    }

    if show("physics") {
        if let Some(p) = dat1
            .get_section_data(physics::TAG_PHYSICS_DATA)
            .and_then(|d| PhysicsData::parse(d).ok())
        {
            println!("\n== Physics Data ==");
            println!(
                "  havok tagfile={} sdk={:?} classes={:?}",
                p.is_havok_tagfile, p.sdk_version, p.classes
            );
        }
    }
}
