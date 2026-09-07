//! Dumps every reversed section of a .model asset in readable form.
//!
//! usage: model_dump <file.model> [section...]
//! With no section names it prints a summary of all of them.

use omnitool_lib::core::dat1::Dat1;
use omnitool_lib::tools::model_converter::model::ModelFile;
use omnitool_lib::tools::model_converter::sections::*;

fn tag_name(tag: u32) -> &'static str {
    match tag {
        0x283D0383 => "Model Built",
        0xA98BE69B => "Model Std Vert",
        0x16F3BA18 => "Model Tex Vert",
        0x6B855EED => "Model UV1 Vert",
        0xCCBAFF15 => "Model GPU Skin",
        0xDCA379A2 => "Model Skin Data",
        0xC61B1FF5 => "Model Skin Batch",
        0x5240C82B => "Model Skin Joint Remap",
        0x0859863D => "Model Index",
        0x78D9CBDE => "Model Subset",
        0x3250BB80 => "Model Material",
        0xDCC88A19 => "Model Bind Pose",
        0x90CDB60C => "Model Joint Hierarchy",
        0x15DF9D3B => "Model Joint",
        0x0AD3A708 => "Model Joint Bspheres",
        0xEE31971C => "Model Joint Lookup",
        0x9F614FAB => "Model Locator",
        0x731CBC2E => "Model Locator Lookup",
        0xC5354B60 => "Model Mirror Ids",
        0x5CBA9DE9 => "Model Col Vert",
        0xEFD92E68 => "Model Physics Data",
        0x5E709570 => "Model Anim Morph Data",
        0xA600C108 => "Model Anim Morph Indices",
        0x380A5744 => "Model Anim Morph Info",
        0xADD1CBD3 => "Model Anim Dynamics Def",
        0x06EB7EFC => "Model Look",
        0x811902D7 => "Model Look Built",
        0x4CCEA4AD => "Model Look Group",
        0xDF9FDF12 => "Model Look BVH Info",
        0xB7380E8C => "Model Leaf Ids",
        0x27CA5246 => "Model Splines",
        0x3C9DABDF => "Model Spline Subsets",
        0xBB7303D5 => "Model Spline Skin Binding",
        0x707F1B58 => "(unnamed) Joint Bind Chains",
        0x9A434B29 => "(unnamed) IK Chains",
        0x5A39FAB7 => "(unnamed) Joint Set",
        0xB25B3163 => "(unnamed) Spline Points",
        _ => "?",
    }
}

fn s(dat1: &Dat1, offset: u64) -> String {
    dat1.get_string(offset as u32).unwrap_or_default()
}

fn joint_names(dat1: &Dat1) -> Vec<String> {
    dat1.get_section_data(0x15DF9D3B)
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

    println!("== sections ({}) ==", dat1.sections.len());
    let mut ordered: Vec<_> = dat1.sections.iter().enumerate().collect();
    ordered.sort_by_key(|(_, s)| s.offset);
    for (i, sec) in ordered {
        println!(
            "  0x{:08X}  {:<28}  size={}",
            sec.tag,
            tag_name(sec.tag),
            dat1.section_data[i].len()
        );
    }

    if show("built") {
        if let Some(b) = dat1.get_section_data(built::TAG_BUILT).and_then(Built::parse) {
            println!("\n== Built ==");
            println!("  bounds          {:?}", b.bounds);
            println!("  position_offset {:?}", b.position_offset);
            println!("  position_scale  {} (1/{})", b.position_scale, 1.0 / b.position_scale);
            println!(
                "  uv scales       uv0={} uv1={} (field 0x{:X})",
                b.uv0_scale(),
                b.uv1_scale(),
                b.uv_shifts
            );
            println!("  lod_distances   {:?}", b.lod_distances);
            println!("  counts          verts={} indices={}", b.vertex_count, b.index_count);
            println!("  feature_flags   0x{:08X}", b.feature_flags);
            for (name, bit) in [
                ("UV1+COLOR", built::flags::UV1_AND_COLOR),
                ("MORPH+SPLINES", built::flags::MORPH_AND_SPLINES),
                ("IK_CHAINS", built::flags::IK_CHAINS),
                ("ANIM_DYNAMICS", built::flags::ANIM_DYNAMICS),
            ] {
                if b.feature_flags & bit != 0 {
                    println!("                  + {name}");
                }
            }
            println!("  unk 0x60={:#X} 0x6C={:#X} 0x70={:#X} 0x74={:#X}", b.unk_0x60, b.unk_0x6c, b.unk_0x70, b.unk_0x74);
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
                    "  [{i:3}] aid={:016X}{}  {}  <- {}",
                    id.map(|x| x.asset_id).unwrap_or(0),
                    if id.is_some() { "" } else { " (no id entry)" },
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
                println!("  hash={:08X} unk=0x{:X} looks={:?}", g.name_hash, g.unk, g.looks);
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
                let expanded = expand_mask(&l.bsphere_mask);
                let mask_ok = expanded == l.bspheres;
                println!(
                    "  [{i}] name={:08X}/{:08X} unk=0x{:X} subcounts={:?} bspheres({})={:?} mask_matches={}",
                    l.name_hash, l.name_hash_lower, l.unk, l.sub_counts, l.bsphere_count, l.bspheres, mask_ok
                );
                for (k, a) in l.sub_arrays.iter().enumerate() {
                    if !a.is_empty() {
                        println!("        sub{k} = {a:?}");
                    }
                }
            }
        }
        if let Some(v) = dat1
            .get_section_data(looks::TAG_LOOK_BVH_INFO)
            .and_then(|d| parse_look_bvh_info(d).ok())
        {
            println!("\n== Look BVH Info ==");
            for (i, e) in v.iter().enumerate() {
                println!("  [{i}] nodes={} depth={} unk=({},{})", e.node_count, e.depth, e.unk0, e.unk2);
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
                let owner = loc
                    .joint
                    .and_then(|j| jn.get(j as usize).cloned())
                    .unwrap_or_else(|| "-".into());
                println!(
                    "  [{i:3}] {:08X} {:<34} joint={owner}",
                    loc.hash,
                    s(dat1, loc.string_offset as u64)
                );
            }
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
                    jn.get(j).cloned().unwrap_or_default()
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
                println!(
                    "  {:>3} {:<28} <-> {:>3} {:<28} flags={:X}",
                    e.joint,
                    jn.get(e.joint as usize).cloned().unwrap_or_default(),
                    e.mirror_joint,
                    jn.get(e.mirror_joint as usize).cloned().unwrap_or_default(),
                    e.flags
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

            // Decode every morph and report the structural self-checks, which
            // is what pins the format down: per-chunk index counts have to hit
            // the stored vertex counts exactly, and every id must land inside
            // its own subset's vertex range.
            let (data, idx, subsets) = (
                dat1.get_section_data(morph::TAG_ANIM_MORPH_DATA),
                dat1.get_section_data(morph::TAG_ANIM_MORPH_INDICES),
                dat1.get_section_data(meshes::TAG_MESHES),
            );
            if let (Some(data), Some(idx), Some(sub)) = (data, idx, subsets) {
                let subs = MeshDefinition::parse_all(sub).unwrap_or_default();
                let (mut count_ok, mut count_bad, mut oob) = (0usize, 0usize, 0usize);
                let mut sample = Vec::new();
                for e in &m.entries {
                    for si in 0..e.subset_count as usize {
                        let deltas = AnimMorphInfo::decode_subset(e, si, data, idx);
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

    if show("ik") || show("chain") {
        if let Some(b) = dat1
            .get_section_data(dynamics::TAG_JOINT_BIND_CHAINS)
            .and_then(|d| JointBindChains::parse(d).ok())
        {
            println!("\n== Joint Bind Chains ({}) ==", b.entries.len());
            for e in &b.entries {
                let chain: Vec<String> = b
                    .chain_of(e)
                    .iter()
                    .map(|&j| jn.get(j as usize).cloned().unwrap_or_else(|| j.to_string()))
                    .collect();
                println!(
                    "  {:<26} parent={:<26} chain={}",
                    jn.get(e.joint as usize).cloned().unwrap_or_default(),
                    if e.parent == u16::MAX {
                        "-".into()
                    } else {
                        jn.get(e.parent as usize).cloned().unwrap_or_default()
                    },
                    chain.join(" > ")
                );
            }
        }
        if let Some(k) = dat1
            .get_section_data(dynamics::TAG_IK_CHAINS)
            .and_then(|d| IkChains::parse(d).ok())
        {
            println!("\n== IK Chains ({} solvers, {} limits) ==", k.solvers.len(), k.limits.len());
            for sv in &k.solvers {
                let chain: Vec<String> = k
                    .joints_of(sv)
                    .iter()
                    .map(|&j| jn.get(j as usize).cloned().unwrap_or_else(|| j.to_string()))
                    .collect();
                println!(
                    "  effector={:08X} tol={} tag={:?} chain={}",
                    sv.locator_hash,
                    sv.tolerance,
                    sv.tag,
                    chain.join(" > ")
                );
            }
            for l in &k.limits {
                println!("    limit axis={} range=[{}, {}] value={}", l.axis, l.min, l.max, l.value);
            }
        }
        if let Some(js) = dat1
            .get_section_data(dynamics::TAG_JOINT_SET)
            .and_then(|d| JointSet::parse(d).ok())
        {
            println!("\n== Joint Set (kind={}, {} joints, map {} entries) ==", js.kind, js.joints.len(), js.map.len());
            for j in &js.joints {
                println!("  {j:>3} {}", jn.get(*j as usize).cloned().unwrap_or_default());
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
                .map(|d| d.len() / 12)
                .unwrap_or(0);
            println!(
                "\n== Spline Subsets ({} groups, {} strands in section, {} implied) ==",
                ss.subsets.len(),
                strands,
                ss.strand_total()
            );
            let splines = dat1
                .get_section_data(splines::TAG_SPLINES)
                .and_then(|d| parse_splines(d).ok())
                .unwrap_or_default();
            let points = dat1
                .get_section_data(splines::TAG_SPLINE_POINTS)
                .and_then(|d| parse_spline_points(d).ok())
                .unwrap_or_default();
            let offsets = spline_point_offsets(&splines);
            let pos_scale = dat1
                .get_section_data(built::TAG_BUILT)
                .map(built::get_position_scale)
                .unwrap_or(1.0 / 4096.0);
            println!(
                "  point records {}, sum of per-strand counts {} -> {}",
                points.len(),
                offsets.last().copied().unwrap_or(0),
                if points.len() as u32 == offsets.last().copied().unwrap_or(0) {
                    "match"
                } else {
                    "MISMATCH"
                }
            );
            for g in &ss.subsets {
                let (a, b) = (g.first_strand as usize, (g.first_strand + g.strand_count) as usize);
                let (p0, p1) = (
                    offsets.get(a).copied().unwrap_or(0) as usize,
                    offsets.get(b).copied().unwrap_or(0) as usize,
                );
                let mut lo = [f32::MAX; 3];
                let mut hi = [f32::MIN; 3];
                for p in points.get(p0..p1).unwrap_or(&[]) {
                    let d = p.decode(pos_scale);
                    for (k, v) in [d.0, d.1, d.2].into_iter().enumerate() {
                        lo[k] = lo[k].min(v);
                        hi[k] = hi[k].max(v);
                    }
                }
                let bbox = if p1 > p0 {
                    format!(
                        "x[{:6.3},{:6.3}] y[{:6.3},{:6.3}] z[{:6.3},{:6.3}]",
                        lo[0], hi[0], lo[1], hi[1], lo[2], hi[2]
                    )
                } else {
                    "(empty)".into()
                };
                println!(
                    "  {:08X} {:<28} strands {:5}..{:<5} pts {:6}..{:<6} {}",
                    g.name_hash,
                    s(dat1, g.name_offset as u64),
                    g.first_strand,
                    g.first_strand + g.strand_count,
                    p0,
                    p1,
                    bbox
                );
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
