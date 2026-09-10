//! `read_model_info` — a read-only view over every reversed `.model` section

use crate::core::crc32;
use crate::core::dat1::Dat1;
use crate::core::error::ToolkitError;
use crate::tools::model_converter::model::ModelFile;
use crate::tools::model_converter::sections::*;

#[derive(serde::Serialize)]
pub struct SectionView {
    pub tag: u32,
    pub name: String,
    pub size: usize,
    pub known: bool,
}

#[derive(serde::Serialize)]
pub struct BuiltView {
    pub bounds: Vec<f32>,
    pub position_offset: (f32, f32, f32),
    pub position_scale: f32,
    pub uv0_scale: f32,
    pub uv1_scale: f32,
    pub uv_shift_field: u32,
    pub lod_distances: Vec<f32>,
    pub vertex_count: u32,
    pub index_count: u32,
    pub feature_flags: u32,
    pub feature_names: Vec<String>,
    pub av_material_hash: u32,
}

#[derive(serde::Serialize)]
pub struct MaterialSlotView {
    pub index: usize,
    pub name: String,
    pub path: String,
    pub asset_id: String,
    pub name_hash: u32,
    /// False when the stored hash does not match `ihash(name)` — never seen in
    /// shipped assets, so it flags a corrupt or hand-edited table.
    pub hash_ok: bool,
}

#[derive(serde::Serialize)]
pub struct LodRangeView {
    pub first_subset: u16,
    pub subset_count: u16,
}

#[derive(serde::Serialize)]
pub struct LookView {
    pub index: usize,
    pub name: Option<String>,
    pub name_hash: u32,
    pub lods: Vec<LodRangeView>,
    pub bspheres: Vec<u16>,
    /// The bitmask copy agrees with the explicit list.
    pub mask_ok: bool,
    pub bvh_nodes: Option<u32>,
    pub bvh_depth: Option<u32>,
}

#[derive(serde::Serialize)]
pub struct SubsetView {
    pub index: usize,
    pub material: u16,
    pub material_name: Option<String>,
    pub vertex_start: u32,
    pub vertex_count: u32,
    pub index_start: u32,
    pub index_count: u32,
    pub flags: u16,
    pub skinned: bool,
    pub uv_density: (f32, f32),
}

#[derive(serde::Serialize)]
pub struct JointView {
    pub index: usize,
    pub name: String,
    pub parent: i16,
    pub hash: u32,
}

#[derive(serde::Serialize)]
pub struct LocatorView {
    pub index: usize,
    pub name: String,
    pub hash: u32,
    pub joint: Option<u32>,
    pub joint_name: Option<String>,
}

#[derive(serde::Serialize)]
pub struct BsphereView {
    pub index: usize,
    pub joint: u32,
    pub joint_name: Option<String>,
    pub center: (f32, f32, f32),
    pub radius: f32,
}

#[derive(serde::Serialize)]
pub struct MorphView {
    pub id: u32,
    pub name: String,
    pub element_count: u8,
    pub component_bits: u8,
    pub subsets: Vec<u8>,
    pub vertex_total: u32,
    pub mirror_of: Option<String>,
}

#[derive(serde::Serialize)]
pub struct IkChainView {
    pub effector_hash: u32,
    pub effector_name: Option<String>,
    pub tolerance: f32,
    pub joints: Vec<String>,
}

#[derive(serde::Serialize)]
pub struct BindChainView {
    pub joint: String,
    pub parent: Option<String>,
    pub chain: Vec<String>,
}

#[derive(serde::Serialize)]
pub struct HairGroupView {
    pub name: String,
    pub name_hash: u32,
    pub strand_count: u32,
    pub first_strand: u32,
    pub point_count: u32,
    pub bounds_min: (f32, f32, f32),
    pub bounds_max: (f32, f32, f32),
}

#[derive(serde::Serialize)]
pub struct PhysicsView {
    pub havok_tagfile: bool,
    pub sdk_version: Option<String>,
    pub classes: Vec<String>,
}

#[derive(serde::Serialize)]
pub struct ModelInfo {
    pub path: String,
    pub sections: Vec<SectionView>,
    pub built: Option<BuiltView>,
    pub materials: Vec<MaterialSlotView>,
    pub looks: Vec<LookView>,
    pub subsets: Vec<SubsetView>,
    pub joints: Vec<JointView>,
    pub locators: Vec<LocatorView>,
    pub bspheres: Vec<BsphereView>,
    pub morphs: Vec<MorphView>,
    pub morph_pair_count: usize,
    pub ik_chains: Vec<IkChainView>,
    pub bind_chains: Vec<BindChainView>,
    pub hair_groups: Vec<HairGroupView>,
    pub physics: Option<PhysicsView>,
    pub warnings: Vec<String>,
}

fn tag_name(tag: u32) -> Option<&'static str> {
    Some(match tag {
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
        0x707F1B58 => "Joint Bind Chains (unnamed)",
        0x9A434B29 => "IK Chains (unnamed)",
        0x5A39FAB7 => "Joint Set (unnamed)",
        0xB25B3163 => "Spline Points (unnamed)",
        _ => return None,
    })
}

fn s(dat1: &Dat1, offset: u64) -> String {
    dat1.get_string(offset as u32).unwrap_or_default()
}

#[tauri::command]
pub async fn read_model_info(model_path: String) -> Result<ModelInfo, ToolkitError> {
    let data = std::fs::read(&model_path)?;
    let model = ModelFile::parse(&data)?;
    let dat1 = &model.dat1;
    let mut warnings = Vec::new();

    let mut sections: Vec<SectionView> = dat1
        .sections
        .iter()
        .enumerate()
        .map(|(i, sec)| SectionView {
            tag: sec.tag,
            name: tag_name(sec.tag).unwrap_or("unknown").to_string(),
            size: dat1.section_data[i].len(),
            known: tag_name(sec.tag).is_some(),
        })
        .collect();
    sections.sort_by(|a, b| b.size.cmp(&a.size));

    // ---- Built ----
    let built_raw = dat1.get_section_data(built::TAG_BUILT);
    let built = built_raw.and_then(Built::parse).map(|b| {
        let mut names = Vec::new();
        for (n, bit) in [
            ("UV1 + Colour streams", built::flags::UV1_AND_COLOR),
            ("Morph targets + splines", built::flags::MORPH_AND_SPLINES),
            ("IK chains", built::flags::IK_CHAINS),
            ("Anim dynamics", built::flags::ANIM_DYNAMICS),
        ] {
            if b.feature_flags & bit != 0 {
                names.push(n.to_string());
            }
        }
        BuiltView {
            bounds: b.bounds.to_vec(),
            position_offset: b.position_offset,
            position_scale: b.position_scale,
            uv0_scale: b.uv0_scale(),
            uv1_scale: b.uv1_scale(),
            uv_shift_field: b.uv_shifts,
            lod_distances: b.lod_distances.to_vec(),
            vertex_count: b.vertex_count,
            index_count: b.index_count,
            feature_flags: b.feature_flags,
            feature_names: names,
            av_material_hash: b.unk_0x6c,
        }
    });
    let position_scale = built
        .as_ref()
        .map(|b| b.position_scale)
        .unwrap_or(1.0 / 4096.0);

    // Cross-check the Built counts against the actual stream sizes.
    if let Some(b) = &built {
        if let Some(v) = dat1.get_section_data(geo::TAG_VERTEXES) {
            if v.len() / 16 != b.vertex_count as usize {
                warnings.push(format!(
                    "Built vertex_count {} disagrees with Std Vert ({} vertices)",
                    b.vertex_count,
                    v.len() / 16
                ));
            }
        }
        if let Some(i) = dat1.get_section_data(geo::TAG_INDEXES) {
            if i.len() / 2 != b.index_count as usize {
                warnings.push(format!(
                    "Built index_count {} disagrees with Model Index ({} indices)",
                    b.index_count,
                    i.len() / 2
                ));
            }
        }
    }

    // ---- materials ----
    let mat_section = dat1
        .get_section_data(looks::TAG_MATERIAL)
        .and_then(|d| MaterialSection::parse(d).ok());
    let materials: Vec<MaterialSlotView> = mat_section
        .as_ref()
        .map(|m| {
            m.slots
                .iter()
                .enumerate()
                .map(|(i, slot)| {
                    let name = s(dat1, slot.name_offset);
                    // The id table is sorted by hash, so it is looked up by
                    // name rather than read at the slot's own index.
                    let id = m.id_for_name(&name);
                    MaterialSlotView {
                        index: i,
                        hash_ok: id.is_some(),
                        asset_id: format!("{:016X}", id.map(|x| x.asset_id).unwrap_or(0)),
                        name_hash: crc32::hash(&name.to_ascii_lowercase()),
                        name,
                        path: s(dat1, slot.path_offset),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    for m in &materials {
        if !m.hash_ok {
            warnings.push(format!(
                "Material slot {} ({}) has no entry in the asset-id table",
                m.index, m.name
            ));
        }
    }

    // ---- joints / locators ----
    let joints_raw = dat1
        .get_section_data(joints::TAG_JOINTS)
        .and_then(|d| Joint::parse_all(d).ok())
        .unwrap_or_default();
    let joint_names: Vec<String> = joints_raw
        .iter()
        .map(|j| s(dat1, j.string_offset as u64))
        .collect();
    let joints: Vec<JointView> = joints_raw
        .iter()
        .enumerate()
        .map(|(i, j)| JointView {
            index: i,
            name: joint_names[i].clone(),
            parent: j.parent,
            hash: j.hash,
        })
        .collect();
    let jname = |i: usize| joint_names.get(i).cloned();

    let locators: Vec<LocatorView> = dat1
        .get_section_data(skeleton::TAG_LOCATOR)
        .and_then(|d| Locator::parse_all(d).ok())
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(i, l)| LocatorView {
            index: i,
            name: s(dat1, l.string_offset as u64),
            hash: l.hash,
            joint: l.joint,
            joint_name: l.joint.and_then(|j| jname(j as usize)),
        })
        .collect();

    let bs = dat1
        .get_section_data(skeleton::TAG_JOINT_BSPHERES)
        .and_then(|d| JointBspheres::parse(d).ok())
        .unwrap_or_default();
    let bspheres: Vec<BsphereView> = (0..bs.spheres.len())
        .map(|i| {
            let j = bs.joint_index(i);
            BsphereView {
                index: i,
                joint: j,
                joint_name: jname(j as usize),
                center: bs.spheres[i].center,
                radius: bs.spheres[i].radius,
            }
        })
        .collect();

    // ---- subsets ----
    let subs = dat1
        .get_section_data(meshes::TAG_MESHES)
        .and_then(|d| MeshDefinition::parse_all(d).ok())
        .unwrap_or_default();
    let subsets: Vec<SubsetView> = subs
        .iter()
        .enumerate()
        .map(|(i, m)| SubsetView {
            index: i,
            material: m.material_index,
            material_name: materials
                .get(m.material_index as usize)
                .map(|x| x.name.clone()),
            vertex_start: m.vertex_start,
            vertex_count: m.vertex_count,
            index_start: m.index_start,
            index_count: m.index_count,
            flags: m.flags,
            skinned: m.is_skinned() || m.is_rcra_skinned(),
            uv_density: (m.uv_density_u, m.uv_density_v),
        })
        .collect();

    // ---- looks ----
    let look_sec = dat1
        .get_section_data(look::TAG_LOOK)
        .and_then(|d| LookSection::parse(d).ok());
    let look_count = look_sec.as_ref().map(|l| l.looks.len()).unwrap_or(0);
    let built_looks = dat1
        .get_section_data(looks::TAG_LOOK_BUILT)
        .and_then(|d| LookBuiltSection::parse(d, look_count).ok());
    let bvh = dat1
        .get_section_data(looks::TAG_LOOK_BVH_INFO)
        .and_then(|d| parse_look_bvh_info(d).ok())
        .unwrap_or_default();

    let mut looks = Vec::new();
    for i in 0..look_count {
        let lb = built_looks.as_ref().and_then(|b| b.looks.get(i));
        let mask_ok = lb
            .map(|l| expand_mask(&l.bsphere_mask) == l.bspheres)
            .unwrap_or(true);
        if !mask_ok {
            warnings.push(format!(
                "Look {i}: bsphere bitmask disagrees with its index list"
            ));
        }
        looks.push(LookView {
            index: i,
            name: lb.and_then(|l| resolve_look_name(dat1, l.name_hash)),
            name_hash: lb.map(|l| l.name_hash).unwrap_or(0),
            lods: look_sec
                .as_ref()
                .and_then(|s| s.looks.get(i))
                .map(|l| {
                    l.lods
                        .iter()
                        .map(|d| LodRangeView {
                            first_subset: d.start,
                            subset_count: d.count,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            bspheres: lb.map(|l| l.bspheres.clone()).unwrap_or_default(),
            mask_ok,
            bvh_nodes: bvh.get(i).map(|b| b.node_count),
            bvh_depth: bvh.get(i).map(|b| b.depth),
        });
    }

    // ---- morphs ----
    let morph_info = dat1
        .get_section_data(morph::TAG_ANIM_MORPH_INFO)
        .and_then(|d| AnimMorphInfo::parse(d).ok());
    let mut morphs = Vec::new();
    let mut morph_pair_count = 0;
    if let Some(mi) = &morph_info {
        morph_pair_count = mi.pairs.len();
        let name_of = |id: u32| {
            mi.entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| s(dat1, e.name_offset as u64))
        };
        for e in &mi.entries {
            let mirror = mi
                .pairs
                .iter()
                .find_map(|&(l, r)| {
                    if l == e.id {
                        Some(r)
                    } else if r == e.id {
                        Some(l)
                    } else {
                        None
                    }
                })
                .and_then(name_of);
            morphs.push(MorphView {
                id: e.id,
                name: s(dat1, e.name_offset as u64),
                element_count: e.element_count,
                component_bits: e.component_bit_size,
                subsets: e.subset_ids.clone(),
                vertex_total: e.subset_vertex_counts.iter().map(|&c| c as u32).sum(),
                mirror_of: mirror,
            });
        }
        morphs.sort_by(|a, b| a.name.cmp(&b.name));
    }

    // ---- rig runtime ----
    let ik_chains: Vec<IkChainView> = dat1
        .get_section_data(dynamics::TAG_IK_CHAINS)
        .and_then(|d| IkChains::parse(d).ok())
        .map(|k| {
            k.solvers
                .iter()
                .map(|sv| IkChainView {
                    effector_hash: sv.locator_hash,
                    effector_name: locators
                        .iter()
                        .find(|l| l.hash == sv.locator_hash)
                        .map(|l| l.name.clone()),
                    tolerance: sv.tolerance,
                    joints: k
                        .joints_of(sv)
                        .iter()
                        .map(|&j| jname(j as usize).unwrap_or_else(|| j.to_string()))
                        .collect(),
                })
                .collect()
        })
        .unwrap_or_default();

    let bind_chains: Vec<BindChainView> = dat1
        .get_section_data(dynamics::TAG_JOINT_BIND_CHAINS)
        .and_then(|d| JointBindChains::parse(d).ok())
        .map(|b| {
            b.entries
                .iter()
                .map(|e| BindChainView {
                    joint: jname(e.joint as usize).unwrap_or_default(),
                    parent: if e.parent == u16::MAX {
                        None
                    } else {
                        jname(e.parent as usize)
                    },
                    chain: b
                        .chain_of(e)
                        .iter()
                        .map(|&j| jname(j as usize).unwrap_or_else(|| j.to_string()))
                        .collect(),
                })
                .collect()
        })
        .unwrap_or_default();

    // ---- hair ----
    let mut hair_groups = Vec::new();
    if let Some(ss) = dat1
        .get_section_data(splines::TAG_SPLINE_SUBSETS)
        .and_then(|d| SplineSubsets::parse(d).ok())
    {
        let strands = dat1
            .get_section_data(splines::TAG_SPLINES)
            .and_then(|d| parse_splines(d).ok())
            .unwrap_or_default();
        let points = dat1
            .get_section_data(splines::TAG_SPLINE_POINTS)
            .and_then(|d| parse_spline_points(d).ok())
            .unwrap_or_default();
        let offsets = spline_point_offsets(&strands);
        if points.len() as u32 != offsets.last().copied().unwrap_or(0) {
            warnings.push("Spline point count disagrees with the per-strand totals".into());
        }
        for g in &ss.subsets {
            let (a, b) = (
                g.first_strand as usize,
                (g.first_strand + g.strand_count) as usize,
            );
            let (p0, p1) = (
                offsets.get(a).copied().unwrap_or(0) as usize,
                offsets.get(b).copied().unwrap_or(0) as usize,
            );
            let mut lo = [f32::MAX; 3];
            let mut hi = [f32::MIN; 3];
            for p in points.get(p0..p1).unwrap_or(&[]) {
                let d = p.decode(position_scale);
                for (k, v) in [d.0, d.1, d.2].into_iter().enumerate() {
                    lo[k] = lo[k].min(v);
                    hi[k] = hi[k].max(v);
                }
            }
            let empty = p1 <= p0;
            hair_groups.push(HairGroupView {
                name: s(dat1, g.name_offset as u64),
                name_hash: g.name_hash,
                strand_count: g.strand_count,
                first_strand: g.first_strand,
                point_count: (p1 - p0.min(p1)) as u32,
                bounds_min: if empty {
                    (0.0, 0.0, 0.0)
                } else {
                    (lo[0], lo[1], lo[2])
                },
                bounds_max: if empty {
                    (0.0, 0.0, 0.0)
                } else {
                    (hi[0], hi[1], hi[2])
                },
            });
        }
    }

    let physics = dat1
        .get_section_data(physics::TAG_PHYSICS_DATA)
        .and_then(|d| PhysicsData::parse(d).ok())
        .map(|p| PhysicsView {
            havok_tagfile: p.is_havok_tagfile,
            sdk_version: p.sdk_version,
            classes: p.classes,
        });

    Ok(ModelInfo {
        path: model_path,
        sections,
        built,
        materials,
        looks,
        subsets,
        joints,
        locators,
        bspheres,
        morphs,
        morph_pair_count,
        ik_chains,
        bind_chains,
        hair_groups,
        physics,
        warnings,
    })
}

/// Look names are stored only as hashes. Every look name seen so far also
/// appears verbatim in the model's own string pool, so a scan of the pool
/// resolves them without needing an external name list.
fn resolve_look_name(dat1: &Dat1, hash: u32) -> Option<String> {
    if hash == 0 {
        return None;
    }
    for raw in dat1.strings_pool.split(|&b| b == 0) {
        if raw.is_empty() {
            continue;
        }
        if let Ok(t) = std::str::from_utf8(raw) {
            if crc32::hash(t) == hash {
                return Some(t.to_string());
            }
        }
    }
    None
}
