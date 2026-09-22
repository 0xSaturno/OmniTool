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
    /// False when the name is inferred from content rather than a matching hash.
    pub name_recovered: bool,
}

#[derive(serde::Serialize)]
pub struct BuiltView {
    pub bsphere_center: [f32; 3],
    pub bsphere_radius: f32,
    pub aabb_extents: [f32; 3],
    pub mesh_center: [f32; 3],
    pub meters_per_unit: f32,
    pub uv0_scale: f32,
    pub uv1_scale: f32,
    pub uv_log_scales: u32,
    pub lod_distances: Vec<f32>,
    pub vertex_count: u32,
    pub index_count: u32,
    pub flags: u32,
    pub flag_names: Vec<String>,
    pub content_flags: u32,
    pub ambient_animation: f32,
    pub fade_out_dist: u16,
    pub shadow_fade_dist: u16,
    pub shadow_casting_lod: i16,
    pub av_material_hash: u32,
    pub audio_material_hash: u32,
}

#[derive(serde::Serialize)]
pub struct MaterialSlotView {
    pub index: usize,
    pub name: String,
    pub path: String,
    pub asset_id: String,
    pub name_hash: u32,
    pub flags: u32,
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
    pub rigid_bodies: usize,
    pub cloths: usize,
    pub bvh_usage: Option<u32>,
    pub bvh_lod: Option<u32>,
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
    pub lod: u8,
    pub lod_proxy: bool,
    pub skin_batches: u8,
    pub anim_vert_batches: u8,
    pub bsphere_radius: f32,
    pub surface_area: f32,
    pub fade_out_dist: i16,
    pub material_lod_dist: i16,
    pub uv_density: (f32, f32),
}

#[derive(serde::Serialize)]
pub struct JointView {
    pub index: usize,
    pub name: String,
    pub parent: i16,
    pub hash: u32,
    pub subtree_count: u16,
    pub flags: Vec<&'static str>,
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
    pub solver_error: f32,
    pub max_iterations: u8,
    pub joints: Vec<String>,
}

#[derive(serde::Serialize)]
pub struct RagdollBodyView {
    pub joint: Option<String>,
    pub parent: Option<String>,
    pub ancestors: Vec<String>,
}

#[derive(serde::Serialize)]
pub struct ClothView {
    pub instances: u16,
    pub collidables: u8,
    pub influence_joints: Vec<String>,
}

#[derive(serde::Serialize)]
pub struct DynamicsChainView {
    pub name: String,
    pub constraint: &'static str,
    pub points: u16,
    pub links: u16,
    pub bends: u16,
}

#[derive(serde::Serialize)]
pub struct HairGroupView {
    pub name: String,
    pub name_hash: u32,
    pub strand_count: u32,
    pub first_strand: u32,
    pub point_count: u32,
    pub config: String,
    pub textures: Vec<String>,
    pub bounds_min: (f32, f32, f32),
    pub bounds_max: (f32, f32, f32),
}

#[derive(serde::Serialize)]
pub struct PerfLodView {
    pub spec: String,
    pub max_lod: u32,
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
    pub ragdoll_bodies: Vec<RagdollBodyView>,
    pub cloth: Option<ClothView>,
    pub dynamics_chains: Vec<DynamicsChainView>,
    pub hair_groups: Vec<HairGroupView>,
    pub perf_max_lod: Vec<PerfLodView>,
    pub render_override_count: usize,
    pub ambient_shadow_prim_count: usize,
    pub physics: Option<PhysicsView>,
    pub warnings: Vec<String>,
}

fn s(dat1: &Dat1, offset: u64) -> String {
    dat1.get_string(offset as u32).unwrap_or_default()
}

const JOINT_FLAG_NAMES: [(u16, &str); 6] = [
    (joint_flags::MIRROR_PAIRED_LEFT, "L"),
    (joint_flags::MIRROR_PAIRED_RIGHT, "R"),
    (joint_flags::END_JOINT, "end"),
    (joint_flags::CLOTH_JOINT, "cloth"),
    (joint_flags::HAS_NEXT_SIBLING, "sibling"),
    (joint_flags::SEGMENT_SCALE_COMPENSATE, "ssc"),
];

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
        .map(|(i, sec)| {
            let named = section_name(sec.tag);
            SectionView {
                tag: sec.tag,
                name: named.map(|n| n.0).unwrap_or("unknown").to_string(),
                size: dat1.section_data[i].len(),
                known: named.is_some(),
                name_recovered: named.map(|n| n.1).unwrap_or(false),
            }
        })
        .collect();
    sections.sort_by(|a, b| b.size.cmp(&a.size));

    // ---- Built ----
    let built = dat1.get_section_data(built::TAG_BUILT).and_then(Built::parse).map(|b| BuiltView {
        bsphere_center: b.bsphere_center,
        bsphere_radius: b.bsphere_radius,
        aabb_extents: b.aabb_extents,
        mesh_center: b.mesh_center,
        meters_per_unit: b.meters_per_unit,
        uv0_scale: b.uv0_scale(),
        uv1_scale: b.uv1_scale(),
        uv_log_scales: b.uv_log_scales,
        lod_distances: b.lod_distances.to_vec(),
        vertex_count: b.vertex_count,
        index_count: b.index_count,
        flags: b.flags,
        flag_names: b.flag_names().into_iter().map(String::from).collect(),
        content_flags: b.content_flags,
        ambient_animation: b.ambient_animation,
        fade_out_dist: b.fade_out_dist,
        shadow_fade_dist: b.shadow_fade_dist,
        shadow_casting_lod: b.shadow_casting_lod,
        av_material_hash: b.av_material_hash,
        audio_material_hash: b.audio_material_hash,
    });
    let mpu = built.as_ref().map(|b| b.meters_per_unit).unwrap_or(1.0 / 4096.0);

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
                        flags: id.map(|x| x.flags).unwrap_or(0),
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
            subtree_count: j.subtree_count,
            flags: JOINT_FLAG_NAMES
                .iter()
                .filter(|(b, _)| j.flags & b != 0)
                .map(|(_, n)| *n)
                .collect(),
        })
        .collect();
    let jname = |i: usize| joint_names.get(i).cloned();
    let jname_or_index = |i: usize| jname(i).unwrap_or_else(|| i.to_string());

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
            lod: m.lod_id(),
            lod_proxy: m.is_lod_proxy(),
            skin_batches: m.skin_batch_count(),
            anim_vert_batches: m.anim_vert_batch_count(),
            bsphere_radius: m.bsphere_radius_m(mpu),
            surface_area: m.surface_area_m2(),
            fade_out_dist: m.fade_out_dist,
            material_lod_dist: m.material_lod_dist,
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
        let lods: Vec<LodRangeView> = look_sec
            .as_ref()
            .and_then(|s| s.looks.get(i))
            .map(|l| {
                l.lods
                    .iter()
                    .take(6)
                    .map(|d| LodRangeView {
                        first_subset: d.start,
                        subset_count: d.count,
                    })
                    .collect()
            })
            .unwrap_or_default();
        if let Some(l) = lb {
            for (lod, r) in lods.iter().enumerate() {
                let want: Vec<u16> = (r.first_subset..r.first_subset + r.subset_count).collect();
                if !l.lod_subset_bits.is_empty() && l.lod_subsets(lod) != want {
                    warnings.push(format!("Look {i}: LOD {lod} subset bitfield disagrees with its range"));
                }
            }
        }
        looks.push(LookView {
            index: i,
            name: lb
                .map(|l| s(dat1, l.name_offset as u64))
                .filter(|n| !n.is_empty())
                .or_else(|| lb.and_then(|l| resolve_look_name(dat1, l.name_hash))),
            name_hash: lb.map(|l| l.name_hash).unwrap_or(0),
            lods,
            bspheres: lb.map(|l| l.bspheres.clone()).unwrap_or_default(),
            mask_ok,
            rigid_bodies: lb.map(|l| l.rigid_bodies.len()).unwrap_or(0),
            cloths: lb.map(|l| l.cloths.len()).unwrap_or(0),
            bvh_usage: bvh.get(i).map(|b| b.usage),
            bvh_lod: bvh.get(i).map(|b| b.lod),
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
        .get_section_data(dynamics::TAG_IK_SETUP)
        .and_then(|d| IkSetup::parse(d).ok())
        .map(|k| {
            k.chains
                .iter()
                .map(|c| IkChainView {
                    effector_hash: c.goal_locator_hash,
                    effector_name: locators
                        .iter()
                        .find(|l| l.hash == c.goal_locator_hash)
                        .map(|l| l.name.clone()),
                    solver_error: c.solver_error,
                    max_iterations: c.max_iterations,
                    joints: k.chain_joints(c).iter().map(|&j| jname_or_index(j as usize)).collect(),
                })
                .collect()
        })
        .unwrap_or_default();

    let ragdoll_bodies: Vec<RagdollBodyView> = dat1
        .get_section_data(dynamics::TAG_RAGDOLL_META_DATA)
        .and_then(|d| RagdollMetaData::parse(d).ok())
        .map(|r| {
            r.bodies
                .iter()
                .map(|b| RagdollBodyView {
                    joint: (b.joint != u16::MAX).then(|| jname_or_index(b.joint as usize)),
                    parent: (b.parent_joint != u16::MAX).then(|| jname_or_index(b.parent_joint as usize)),
                    ancestors: r.ancestors_of(b).iter().map(|&j| jname_or_index(j as usize)).collect(),
                })
                .collect()
        })
        .unwrap_or_default();

    let cloth = dat1
        .get_section_data(dynamics::TAG_CLOTH_META_DATA)
        .and_then(|d| ClothMetaData::parse(d).ok())
        .map(|c| ClothView {
            instances: c.instance_count,
            collidables: c.collidable_count,
            influence_joints: c.influence_joints.iter().map(|&j| jname_or_index(j as usize)).collect(),
        });

    let dynamics_chains: Vec<DynamicsChainView> = dat1
        .get_section_data(dynamics::TAG_ANIM_DYNAMICS_DEF)
        .and_then(|d| AnimDynamicsDef::parse(d).ok())
        .map(|d| {
            d.chains
                .iter()
                .map(|c| DynamicsChainView {
                    name: s(dat1, c.name_offset as u64),
                    constraint: match c.constraint_type {
                        dynamics_constraint::SIMPLE => "simple",
                        dynamics_constraint::CONE => "cone",
                        dynamics_constraint::LIMITED_CONE => "limited cone",
                        _ => "?",
                    },
                    points: c.points.1,
                    links: c.links.1,
                    bends: c.bends.1,
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
        let cvs = dat1
            .get_section_data(splines::TAG_SPLINE_CVS)
            .and_then(|d| parse_spline_cvs(d).ok())
            .unwrap_or_default();
        let referenced: usize = strands.iter().map(|s| s.cv_count as usize).sum();
        if cvs.len() != referenced {
            warnings.push("Spline CV count disagrees with the per-strand totals".into());
        }
        for g in &ss.subsets {
            let (a, b) = (
                g.first_strand as usize,
                (g.first_strand + g.strand_count) as usize,
            );
            let mut lo = [f32::MAX; 3];
            let mut hi = [f32::MIN; 3];
            let mut point_count = 0u32;
            for st in strands.get(a..b).unwrap_or(&[]) {
                point_count += st.cv_count as u32;
                for p in cvs.get(st.cv_range()).unwrap_or(&[]) {
                    let d = p.decode(mpu);
                    for (k, v) in [d.0, d.1, d.2].into_iter().enumerate() {
                        lo[k] = lo[k].min(v);
                        hi[k] = hi[k].max(v);
                    }
                }
            }
            let empty = lo[0] > hi[0];
            hair_groups.push(HairGroupView {
                name: s(dat1, g.name_offset as u64),
                name_hash: g.name_hash,
                strand_count: g.strand_count,
                first_strand: g.first_strand,
                point_count,
                config: s(dat1, g.config_path_offset as u64),
                textures: g
                    .texture_path_offsets
                    .iter()
                    .map(|&o| s(dat1, o as u64))
                    .filter(|p| !p.is_empty())
                    .collect(),
                bounds_min: if empty { (0.0, 0.0, 0.0) } else { (lo[0], lo[1], lo[2]) },
                bounds_max: if empty { (0.0, 0.0, 0.0) } else { (hi[0], hi[1], hi[2]) },
            });
        }
    }

    // ---- render extras ----
    let perf_max_lod: Vec<PerfLodView> = dat1
        .get_section_data(render::TAG_PERF_PROFILE_MAX_LOD)
        .and_then(|d| parse_perf_profile_max_lod(d).ok())
        .unwrap_or_default()
        .iter()
        .map(|p| PerfLodView {
            spec: p.spec_name().map(String::from).unwrap_or_else(|| format!("{:08X}", p.spec_hash)),
            max_lod: p.max_lod,
        })
        .collect();
    let render_override_count = dat1
        .get_section_data(render::TAG_RENDER_OVERRIDES)
        .map(|d| d.len() / 32)
        .unwrap_or(0);
    let ambient_shadow_prim_count = dat1
        .get_section_data(render::TAG_AMBIENT_SHADOW_PRIMS)
        .map(|d| d.len() / 48)
        .unwrap_or(0);

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
        ragdoll_bodies,
        cloth,
        dynamics_chains,
        hair_groups,
        perf_max_lod,
        render_override_count,
        ambient_shadow_prim_count,
        physics,
        warnings,
    })
}

/// Fallback for looks whose name offset is empty: scan the model's string pool for a matching hash.
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
