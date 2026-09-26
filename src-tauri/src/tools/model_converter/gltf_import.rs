//! glTF import planner. Maps primitives onto subsets by material slot and geometry, adds and
//! removes subsets and material slots, re-encodes morph targets from shape keys, then hands the
//! geometry to the injector.
//!
//! Each primitive that keeps a subset's identity is classed by what changed:
//! - `Unchanged`: same topology and attributes; the subset is left byte-for-byte as shipped.
//! - `Moved`: same topology, new positions / normals / UVs / weights; morphs and skin carry over.
//! - `Rebuilt`: new topology in an existing subset; skin and morphs come from the glTF.
//! - `New`: a primitive with no subset to replace; it becomes a new subset of the primary look.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::core::error::{Result, ToolkitError};
use crate::core::{crc32, crc64};
use super::gltf_reader::{inject_prepared, GltfMesh, GltfModel, GltfVertex, InjectPlan};
use super::model::ModelFile;
use super::morph_build::{self, Delta, MorphDef, MorphSet};
use super::sections::{
    built::{get_position_scale, get_uv1_scale, get_uv_scale, Built},
    dynamics::TAG_CLOTH_META_DATA,
    geo::{IndexesSection, Uv1Section, Vertex, VertexesSection, DEFAULT_UV_SCALE, TAG_INDEXES, TAG_UV1, TAG_VERTEXES},
    joints::{Joint, TAG_JOINTS},
    look::{LookSection, TAG_LOOK},
    looks::{sync_lod_subset_bits, MaterialSection, TAG_MATERIAL},
    meshes::{subset_flags, MeshDefinition, TAG_MESHES},
    morph::{TAG_ANIM_MORPH_DATA, TAG_ANIM_MORPH_INDICES, TAG_ANIM_MORPH_INFO},
    skin::{SkinBatch, SkinSource, TAG_RCRA_SKIN, TAG_SKIN_BATCH},
    splines::{SplineSubsets, TAG_SPLINES, TAG_SPLINE_CVS, TAG_SPLINE_SKIN_BINDING, TAG_SPLINE_SUBSETS},
};

const TAG_BUILT: u32 = 0x283D0383;
/// Vertices one subset can address with 16-bit local indices.
const MAX_SUBSET_VERTICES: usize = 65536;
/// Look Built's per-LOD bitfields hold this many subsets.
const MAX_SUBSETS: usize = 2048;
/// Bits per component for shape keys the model did not have.
const NEW_MORPH_BITS: u8 = 8;
/// Largest normal component difference still read as the same normal (10-bit packing).
const NORMAL_TOLERANCE: f32 = 0.004;
/// Largest weight difference still read as the same weight (8-bit packing).
const WEIGHT_TOLERANCE: f32 = 2.5 / 255.0;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ImportOptions {
    /// Looks whose LOD 0 the glTF replaces; empty = the looks recorded at export, else look 0.
    pub looks: Vec<usize>,
    /// Slot name -> `.material` path: required for new slots, repoints existing ones.
    pub materials: BTreeMap<String, String>,
    /// Subset flag overrides per slot name.
    pub slot_flags: BTreeMap<String, FlagOverride>,
    /// Remove subsets of the replaced looks that no primitive maps to. Unset: remove, unless every
    /// primitive is named `smNN` (files from before slot-based import).
    pub remove_missing: Option<bool>,
    /// Ignore shape keys and keep the model's morphs wherever the geometry allows.
    pub keep_morphs: bool,
    /// Drop the model's hair / fur strands. Unset: keep them.
    pub strip_hair: Option<bool>,
}

/// Flag names from `subset_flags::OVERRIDABLE`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FlagOverride {
    pub set: Vec<String>,
    pub clear: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Unchanged,
    Moved,
    Rebuilt,
    New,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubsetReport {
    /// Index in the written model.
    pub subset: usize,
    /// Index in the source model.
    pub source: Option<usize>,
    pub primitive: String,
    pub slot: String,
    pub tier: Tier,
    pub vertices: usize,
    pub triangles: usize,
    pub skin_rebuilt: bool,
    pub morph_vertices: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ImportReport {
    pub looks: Vec<usize>,
    pub subsets: Vec<SubsetReport>,
    /// Source indices of removed subsets.
    pub removed: Vec<usize>,
    /// Hair groups dropped with `strip_hair`.
    pub hair_removed: usize,
    /// Source indices of replaced-look subsets the glTF lacks, left untouched.
    pub kept: Vec<usize>,
    pub new_slots: Vec<String>,
    pub repointed_slots: Vec<String>,
    /// Morph targets written, when the morph sections were re-encoded.
    pub morphs: Option<usize>,
    pub morph_deltas_dropped: usize,
    pub warnings: Vec<String>,
}

impl ImportReport {
    fn warn(&mut self, msg: String) {
        log::warn!("[import_gltf] {msg}");
        self.warnings.push(msg);
    }
}

fn err(msg: String) -> ToolkitError {
    ToolkitError::Parse(msg)
}

/// A glTF material that is not a slot of the model yet.
#[derive(Debug, Clone, Serialize)]
pub struct SlotRequest {
    pub name: String,
    /// From the import options or the material's `rcra_material_path`, when known.
    pub path: Option<String>,
    /// False when every mesh using it names its old subset, whose slot it keeps without a path.
    pub required: bool,
    pub meshes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExistingSlot {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SlotPlan {
    pub new_slots: Vec<SlotRequest>,
    pub existing: Vec<ExistingSlot>,
    /// The model's hair / fur strands, when it has any.
    pub hair: Option<HairInfo>,
    /// The saved `strip_hair` choice.
    pub strip_hair: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HairInfo {
    pub groups: Vec<String>,
    pub strands: usize,
}

const HAIR_SECTIONS: [u32; 4] = [TAG_SPLINE_SUBSETS, TAG_SPLINES, TAG_SPLINE_CVS, TAG_SPLINE_SKIN_BINDING];

fn hair_info(model: &ModelFile) -> Option<HairInfo> {
    let d = &model.dat1;
    let subsets = SplineSubsets::parse(d.get_section_data(TAG_SPLINE_SUBSETS)?).ok()?;
    let strands = d.get_section_data(TAG_SPLINES).map_or(0, |s| s.len() / 12);
    if subsets.subsets.is_empty() && strands == 0 {
        return None;
    }
    let groups = subsets
        .subsets
        .iter()
        .map(|s| d.get_string(s.name_offset).unwrap_or_else(|| format!("{:08X}", s.name_hash)))
        .collect();
    Some(HairInfo { groups, strands })
}

/// Removes the spline sections and zeroes Built's strand group count. The Spline model flag does
/// not track these sections in the corpus, so it is left alone.
fn strip_hair(model: &mut ModelFile) -> Result<usize> {
    let groups = hair_info(model).map_or(0, |h| h.groups.len());
    if model.dat1.remove_sections(&HAIR_SECTIONS) == 0 {
        return Ok(0);
    }
    if let Some(mut b) = model.dat1.get_section_data(TAG_BUILT).map(|b| b.to_vec()) {
        if b.len() >= Built::SIZE {
            b[0x62] = 0;
            model.dat1.set_section_data(TAG_BUILT, b)?;
        }
    }
    Ok(groups)
}

/// The model's material slots and the glTF materials an import would add as new slots.
pub fn material_slot_plan(model: &ModelFile, gltf: &GltfModel, opts: &ImportOptions) -> Result<SlotPlan> {
    let d = &model.dat1;
    let n = d.get_section_data(TAG_MESHES).map_or(0, |m| m.len() / 64);
    let existing: Vec<ExistingSlot> = d
        .get_section_data(TAG_MATERIAL)
        .map(MaterialSection::parse)
        .transpose()?
        .map(|m| {
            m.slots
                .iter()
                .map(|s| ExistingSlot {
                    name: d.get_string(s.name_offset as u32).unwrap_or_default(),
                    path: d.get_string(s.path_offset as u32).unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    let names: Vec<String> = existing.iter().map(|s| s.name.clone()).collect();
    let mut new_slots: Vec<SlotRequest> = Vec::new();
    for m in &gltf.meshes {
        let Some(name) = m.material.as_deref() else { continue };
        if find_slot(name, &names).is_some() {
            continue;
        }
        let hinted = m.subset_hint.or(name_hint(&m.name)).is_some_and(|h| h < n);
        let path = lookup_ci(&opts.materials, name).cloned().or_else(|| m.material_path.clone());
        match new_slots.iter_mut().find(|s| s.name == name) {
            Some(s) => {
                s.required |= !hinted;
                s.path = s.path.take().or(path);
                s.meshes.push(m.name.clone());
            }
            None => new_slots.push(SlotRequest { name: name.to_string(), path, required: !hinted, meshes: vec![m.name.clone()] }),
        }
    }
    Ok(SlotPlan { new_slots, existing, hair: hair_info(model), strip_hair: opts.strip_hair })
}

/// Material paths are stored the way the game writes them: backslash-separated.
fn game_path(path: &str) -> String {
    path.trim().replace('/', "\\")
}

/// The source model, read once before anything is rewritten.
struct Source {
    meshes: Vec<MeshDefinition>,
    looks: LookSection,
    slot_names: Vec<String>,
    slot_paths: Vec<String>,
    pos_scale: f32,
    uv_scale: f32,
    uv1_scale: f32,
    vertexes: Vec<Vertex>,
    indices: Vec<u16>,
    uv1: Option<Vec<(i16, i16)>>,
    skin: Option<SkinSource>,
    bones: HashMap<String, u16>,
    morphs: Option<MorphSet>,
    gpu_skin: bool,
}

impl Source {
    fn read(model: &ModelFile) -> Result<Self> {
        let d = &model.dat1;
        let section = |tag: u32| d.get_section_data(tag).ok_or(ToolkitError::SectionNotFound(tag));
        let meshes = MeshDefinition::parse_all(section(TAG_MESHES)?)?;
        let looks = LookSection::parse(section(TAG_LOOK)?)?;
        let mat = d.get_section_data(TAG_MATERIAL).and_then(|m| MaterialSection::parse(m).ok());
        let (slot_names, slot_paths) = mat
            .map(|m| {
                let s = |o: u64| d.get_string(o as u32).unwrap_or_default();
                (m.slots.iter().map(|x| s(x.name_offset)).collect(), m.slots.iter().map(|x| s(x.path_offset)).collect())
            })
            .unwrap_or_default();
        let built = d.get_section_data(TAG_BUILT);
        let pos_scale = built.map(get_position_scale).unwrap_or(1.0 / 4096.0);
        let uv_scale = built.map(get_uv_scale).unwrap_or(DEFAULT_UV_SCALE);
        let uv1_scale = built.map(get_uv1_scale).unwrap_or(DEFAULT_UV_SCALE);
        let vertexes = VertexesSection::parse_scaled(section(TAG_VERTEXES)?, pos_scale, uv_scale)?.vertexes;
        let indices = IndexesSection::parse(section(TAG_INDEXES)?)?.values;
        let uv1 = d.get_section_data(TAG_UV1).and_then(|u| Uv1Section::parse(u).ok()).map(|u| u.uvs);
        let skin = SkinSource::from_dat1(d);
        let mut bones = HashMap::new();
        if let Some(j) = d.get_section_data(TAG_JOINTS).and_then(|j| Joint::parse_all(j).ok()) {
            for (i, joint) in j.iter().enumerate() {
                if let Some(name) = d.get_string(joint.string_offset) {
                    bones.insert(name, i as u16);
                }
            }
        }
        let morphs = match (
            d.get_section_data(TAG_ANIM_MORPH_INFO),
            d.get_section_data(TAG_ANIM_MORPH_DATA),
            d.get_section_data(TAG_ANIM_MORPH_INDICES),
            &skin,
        ) {
            (Some(info), Some(data), Some(idx), Some(skin)) => {
                Some(MorphSet::decode(info, data, idx, &meshes, &skin.batches, |o| d.get_string(o))?)
            }
            _ => None,
        };
        let gpu_skin = d.get_section_data(TAG_RCRA_SKIN).is_some() && meshes.iter().any(|m| m.is_rcra_skinned());
        Ok(Self {
            meshes,
            looks,
            slot_names,
            slot_paths,
            pos_scale,
            uv_scale,
            uv1_scale,
            vertexes,
            indices,
            uv1,
            skin,
            bones,
            morphs,
            gpu_skin,
        })
    }

    fn lod0(&self, look: usize) -> (usize, usize) {
        self.looks.looks[look].lods.first().map(|r| (r.start as usize, r.count as usize)).unwrap_or((0, 0))
    }

    fn vertices(&self, m: &MeshDefinition) -> Option<&[Vertex]> {
        self.vertexes.get(m.vertex_start as usize..(m.vertex_start + m.vertex_count) as usize)
    }

    /// The subset's triangles reversed from game order, as `parse_gltf` stores them.
    fn faces(&self, m: &MeshDefinition) -> Option<Vec<(u32, u32, u32)>> {
        let base = if m.has_relative_indices() { 0 } else { m.vertex_start };
        let idx = self.indices.get(m.index_start as usize..(m.index_start + m.index_count) as usize)?;
        Some(
            idx.chunks_exact(3)
                .map(|t| {
                    let v = |k: usize| (t[k] as u32).wrapping_sub(base);
                    (v(2), v(1), v(0))
                })
                .collect(),
        )
    }

    /// Vertices at the head of the subset that its morphing skin batches cover.
    fn anim_prefix(&self, m: &MeshDefinition) -> usize {
        let Some(skin) = &self.skin else { return 0 };
        let first = m.first_skin_batch as usize;
        let count = (m.anim_vert_batch_count() as usize).min(m.skin_batch_count() as usize);
        skin.batches.get(first..first + count).map_or(0, |b| b.iter().map(|b| b.vertex_count as usize).sum())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SlotRef {
    Existing(u16),
    New(String),
}

struct Prim {
    mesh: GltfMesh,
    hint: Option<usize>,
    named: bool,
    slot: SlotRef,
    source: Option<usize>,
    identity: Option<Identity>,
    tier: Tier,
    skinned: bool,
    skin_rebuilt: bool,
    morph_vertices: usize,
}

/// A primitive recognised as a source subset with the same topology.
#[derive(Clone)]
struct Identity {
    /// New vertex order (`order[source vertex] = primitive vertex`) when the primitive lists them differently.
    order: Option<Vec<u32>>,
    /// Summed squared position difference, to rank candidates.
    distance: f64,
}

/// `smNN` / `smNN_…`, optionally behind the numeric `N_` prefix Blender adds on duplicates.
fn name_hint(name: &str) -> Option<usize> {
    let rest = name.trim_start_matches(|c: char| c.is_ascii_digit());
    let rest = if rest.len() < name.len() { rest.strip_prefix('_')? } else { rest };
    let rest = rest.strip_prefix("sm").or_else(|| rest.strip_prefix("SM"))?;
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let after = &rest[digits..];
    if digits == 0 || !(after.is_empty() || after.starts_with('_') || after.starts_with('.')) {
        return None;
    }
    rest[..digits].parse().ok()
}

/// `body.001` -> `body`: Blender's suffix for duplicated names.
fn strip_blender_suffix(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((base, n)) if n.len() == 3 && n.bytes().all(|c| c.is_ascii_digit()) => base,
        _ => name,
    }
}

fn find_slot(name: &str, slot_names: &[String]) -> Option<u16> {
    let by = |n: &str| {
        slot_names
            .iter()
            .position(|s| s == n)
            .or_else(|| slot_names.iter().position(|s| s.eq_ignore_ascii_case(n)))
    };
    let unnamed = || {
        let i: usize = name.strip_prefix("mat")?.parse().ok()?;
        slot_names.get(i).filter(|s| s.is_empty()).map(|_| i)
    };
    by(name)
        .or_else(|| by(strip_blender_suffix(name)))
        .or_else(unnamed)
        .map(|i| i as u16)
}

fn lookup_ci<'a>(map: &'a BTreeMap<String, String>, key: &str) -> Option<&'a String> {
    map.get(key).or_else(|| map.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v))
}

/// Reorders a primitive's vertices: `new_to_old[new] = old`. Faces and shape keys follow.
fn permute(p: &mut GltfMesh, new_to_old: &[u32]) {
    let mut old_to_new = vec![0u32; new_to_old.len()];
    for (new, &old) in new_to_old.iter().enumerate() {
        old_to_new[old as usize] = new as u32;
    }
    p.vertexes = new_to_old.iter().map(|&o| p.vertexes[o as usize].clone()).collect();
    for f in &mut p.faces {
        *f = (old_to_new[f.0 as usize], old_to_new[f.1 as usize], old_to_new[f.2 as usize]);
    }
    for t in &mut p.targets {
        for d in &mut t.deltas {
            d.0 = old_to_new[d.0 as usize];
        }
        t.deltas.sort_by_key(|d| d.0);
    }
}

/// Splits a primitive into pieces of at most `MAX_SUBSET_VERTICES` vertices.
fn split_primitive(p: GltfMesh) -> Vec<GltfMesh> {
    if p.vertexes.len() <= MAX_SUBSET_VERTICES {
        return vec![p];
    }
    let mut parts = Vec::new();
    let mut local: HashMap<u32, u32> = HashMap::new();
    let mut order: Vec<u32> = Vec::new();
    let mut faces = Vec::new();
    let finish = |order: &mut Vec<u32>, faces: &mut Vec<(u32, u32, u32)>, local: &mut HashMap<u32, u32>, parts: &mut Vec<GltfMesh>| {
        let mut part = p.clone();
        part.vertexes = order.iter().map(|&o| p.vertexes[o as usize].clone()).collect();
        part.faces = std::mem::take(faces);
        for t in &mut part.targets {
            t.deltas = t.deltas.iter().filter_map(|d| local.get(&d.0).map(|&l| (l, d.1, d.2))).collect();
            t.deltas.sort_by_key(|d| d.0);
        }
        parts.push(part);
        order.clear();
        local.clear();
    };
    for f in &p.faces {
        let fresh = [f.0, f.1, f.2].iter().collect::<HashSet<_>>().iter().filter(|v| !local.contains_key(v)).count();
        if order.len() + fresh > MAX_SUBSET_VERTICES {
            finish(&mut order, &mut faces, &mut local, &mut parts);
        }
        let mut map = |v: u32| {
            *local.entry(v).or_insert_with(|| {
                order.push(v);
                order.len() as u32 - 1
            })
        };
        faces.push((map(f.0), map(f.1), map(f.2)));
    }
    if !faces.is_empty() {
        finish(&mut order, &mut faces, &mut local, &mut parts);
    }
    parts
}

/// Recognises `p` as subset `m` when both have the same triangles, in order or up to a vertex
/// permutation found by position and UV.
fn identity(p: &GltfMesh, m: &MeshDefinition, src: &Source) -> Option<Identity> {
    let n = m.vertex_count as usize;
    if p.vertexes.len() != n || p.faces.len() * 3 != m.index_count as usize {
        return None;
    }
    let orig = src.vertices(m)?;
    let faces = src.faces(m)?;
    if faces == p.faces {
        let distance = p
            .vertexes
            .iter()
            .zip(orig)
            .map(|(a, o)| {
                let d = [a.position.0 - o.x, a.position.1 - o.y, a.position.2 - o.z];
                (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]) as f64
            })
            .sum();
        return Some(Identity { order: None, distance });
    }

    // Vertices equal in position, UVs and normal form a class; seams and hard edges make classes
    // of several. The triangles must match class for class, and members pair up in order.
    let (ps, us, u1s) = (src.pos_scale, src.uv_scale, src.uv1_scale);
    let q = |x: f32, s: f32| (x / s).round() as i32;
    let key = |pos: [f32; 3], uv: (f32, f32), uv1: (i32, i32), nrm: [f32; 3]| {
        [q(pos[0], ps), q(pos[1], ps), q(pos[2], ps), q(uv.0, us), q(uv.1, us), uv1.0, uv1.1, q(nrm[0], 1.0 / 64.0), q(nrm[1], 1.0 / 64.0), q(nrm[2], 1.0 / 64.0)]
    };
    let mut class: HashMap<[i32; 10], usize> = HashMap::new();
    let mut orig_members: Vec<Vec<u32>> = Vec::new();
    let mut orig_class = Vec::with_capacity(n);
    for (i, o) in orig.iter().enumerate() {
        let uv1 = src.uv1.as_ref().and_then(|t| t.get(m.vertex_start as usize + i)).map_or((0, 0), |&(a, b)| (a as i32, b as i32));
        let c = *class.entry(key([o.x, o.y, o.z], (o.u, o.v), uv1, [o.nx, o.ny, o.nz])).or_insert_with(|| {
            orig_members.push(Vec::new());
            orig_members.len() - 1
        });
        orig_members[c].push(i as u32);
        orig_class.push(c);
    }
    let mut in_members: Vec<Vec<u32>> = vec![Vec::new(); orig_members.len()];
    let mut in_class = Vec::with_capacity(n);
    for (vi, v) in p.vertexes.iter().enumerate() {
        let uv = v.uv?;
        let uv1 = if src.uv1.is_some() { v.uv1.unwrap_or(uv) } else { (0.0, 0.0) };
        let uv1 = if src.uv1.is_some() { (q(uv1.0, u1s), q(uv1.1, u1s)) } else { (0, 0) };
        let c = *class.get(&key([v.position.0, v.position.1, v.position.2], uv, uv1, [v.normal.0, v.normal.1, v.normal.2]))?;
        in_members[c].push(vi as u32);
        in_class.push(c);
    }
    if in_members.iter().zip(&orig_members).any(|(a, b)| a.len() != b.len()) {
        return None;
    }
    let canon = |t: [usize; 3]| {
        let k = (0..3).min_by_key(|&k| t[k]).unwrap();
        [t[k], t[(k + 1) % 3], t[(k + 2) % 3]]
    };
    let mut a: Vec<_> = p.faces.iter().map(|f| canon([f.0, f.1, f.2].map(|v| in_class[v as usize]))).collect();
    let mut b: Vec<_> = faces.iter().map(|f| canon([f.0, f.1, f.2].map(|v| orig_class.get(v as usize).copied().unwrap_or(usize::MAX)))).collect();
    a.sort_unstable();
    b.sort_unstable();
    if a != b {
        return None;
    }
    // Within a class, pair members by exact normal and skin weights.
    let fine = |n: [f32; 3]| n.map(|c| (c * 1024.0).round() as i32);
    let sign = |w: &[(u16, f32)]| {
        let mut s: Vec<(u16, i32)> = w.iter().filter(|x| x.1 > 0.0).map(|&(j, x)| (j, (x * 255.0).round() as i32)).collect();
        s.sort_unstable();
        s
    };
    let weighted = src.skin.is_some() && m.is_skinned() && has_weights(p);
    let orig_w = match &src.skin {
        Some(skin) if weighted => skin.subset_weights(m),
        _ => Vec::new(),
    };
    let orig_key: Vec<_> = orig
        .iter()
        .enumerate()
        .map(|(i, o)| (fine([o.nx, o.ny, o.nz]), orig_w.get(i).map(|w| sign(w)).unwrap_or_default()))
        .collect();
    let in_key: Vec<_> = p
        .vertexes
        .iter()
        .map(|v| {
            let w = if weighted { sign(&influences(v, p.joint_names.as_deref(), &src.bones)) } else { Vec::new() };
            (fine([v.normal.0, v.normal.1, v.normal.2]), w)
        })
        .collect();
    let mut order = vec![0u32; n];
    for (om, im) in orig_members.iter_mut().zip(in_members.iter_mut()) {
        if om.len() > 1 {
            om.sort_by(|a, b| orig_key[*a as usize].cmp(&orig_key[*b as usize]));
            im.sort_by(|a, b| in_key[*a as usize].cmp(&in_key[*b as usize]));
        }
        for (&o, &i) in om.iter().zip(im.iter()) {
            order[o as usize] = i;
        }
    }
    Some(Identity { order: Some(order), distance: 0.0 })
}

/// Every attribute the glTF carries equals the subset's, at the model's quantisation.
fn same_attributes(p: &GltfMesh, m: &MeshDefinition, src: &Source) -> bool {
    let Some(orig) = src.vertices(m) else { return false };
    let q = |x: f32, s: f32| (x / s).round() as i32;
    let (ps, us, u1s) = (src.pos_scale, src.uv_scale, src.uv1_scale);
    p.vertexes.iter().zip(orig).enumerate().all(|(i, (a, o))| {
        let Some((u, v)) = a.uv else { return false };
        let pos = q(a.position.0, ps) == q(o.x, ps) && q(a.position.1, ps) == q(o.y, ps) && q(a.position.2, ps) == q(o.z, ps);
        let uv0 = q(u, us) == q(o.u, us) && q(v, us) == q(o.v, us);
        let uv1 = match &src.uv1 {
            Some(t) => {
                let (u1, v1) = a.uv1.unwrap_or((u, v));
                t.get(m.vertex_start as usize + i).is_some_and(|&(ru, rv)| q(u1, u1s) == ru as i32 && q(v1, u1s) == rv as i32)
            }
            None => true,
        };
        let nrm = (a.normal.0 - o.nx).abs() <= NORMAL_TOLERANCE
            && (a.normal.1 - o.ny).abs() <= NORMAL_TOLERANCE
            && (a.normal.2 - o.nz).abs() <= NORMAL_TOLERANCE;
        pos && uv0 && uv1 && nrm
    })
}

/// The vertex's influences as model joints, summed per joint and normalised; empty when it has none.
fn influences(v: &GltfVertex, joint_names: Option<&[String]>, bones: &HashMap<String, u16>) -> Vec<(u16, f32)> {
    let mut acc: BTreeMap<u16, f32> = BTreeMap::new();
    for (&g, &w) in v.groups.iter().zip(&v.weights) {
        if w <= 0.0 {
            continue;
        }
        let joint = joint_names
            .and_then(|n| n.get(g as usize))
            .and_then(|name| bones.get(name).copied())
            .unwrap_or(g);
        *acc.entry(joint).or_default() += w;
    }
    let sum: f32 = acc.values().sum();
    acc.into_iter().map(|(j, w)| (j, w / sum)).collect()
}

fn has_weights(p: &GltfMesh) -> bool {
    p.joint_names.is_some() && p.vertexes.iter().any(|v| v.weights.iter().any(|&w| w > 0.0))
}

/// The glTF weights match the subset's skin, or the glTF carries none.
fn same_weights(p: &GltfMesh, m: &MeshDefinition, src: &Source) -> bool {
    let Some(skin) = &src.skin else { return true };
    if !m.is_skinned() || !has_weights(p) {
        return true;
    }
    let orig = skin.subset_weights(m);
    p.vertexes.iter().zip(&orig).all(|(v, ow)| {
        let a: BTreeMap<u16, f32> = influences(v, p.joint_names.as_deref(), &src.bones).into_iter().collect();
        let mut b: BTreeMap<u16, f32> = BTreeMap::new();
        for &(j, w) in ow {
            *b.entry(j).or_default() += w;
        }
        a.keys().chain(b.keys()).all(|j| {
            (a.get(j).copied().unwrap_or(0.0) - b.get(j).copied().unwrap_or(0.0)).abs() <= WEIGHT_TOLERANCE
        })
    })
}

fn morph_vertices(p: &GltfMesh) -> BTreeSet<u32> {
    p.targets.iter().flat_map(|t| t.deltas.iter().map(|d| d.0)).collect()
}

/// Moves the morphing vertices to the front, keeping relative order. Returns how many there are.
fn morphing_first(p: &mut GltfMesh) -> usize {
    let mv = morph_vertices(p);
    if mv.is_empty() {
        return 0;
    }
    let n = p.vertexes.len() as u32;
    let order: Vec<u32> = mv.iter().copied().chain((0..n).filter(|v| !mv.contains(v))).collect();
    permute(p, &order);
    mv.len()
}

fn morph_id(name: &str, orig: Option<&MorphSet>) -> u32 {
    let id = crc32::hash(name);
    let known = |id: u32| orig.is_some_and(|o| o.morphs.iter().any(|m| m.id == id));
    if !known(id) && name.len() == 8 {
        if let Ok(hex) = u32::from_str_radix(name, 16) {
            if known(hex) {
                return hex;
            }
        }
    }
    id
}

fn parse_flags(names: &[String], slot: &str) -> Result<u16> {
    names.iter().try_fold(0u16, |acc, n| {
        subset_flags::OVERRIDABLE
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(n))
            .map(|(_, bit)| acc | bit)
            .ok_or_else(|| {
                let known: Vec<&str> = subset_flags::OVERRIDABLE.iter().map(|(k, _)| *k).collect();
                err(format!("slot '{slot}': unknown subset flag '{n}' (known: {})", known.join(", ")))
            })
    })
}

enum Entry {
    Orig(usize),
    New(usize),
}

pub fn import_gltf(model: &mut ModelFile, gltf: &GltfModel, opts: &ImportOptions) -> Result<ImportReport> {
    let src = Source::read(model)?;
    let n = src.meshes.len();
    let mut report = ImportReport::default();

    // Looks the glTF replaces; their LOD 0 subsets are the candidates for replacement.
    let looks: Vec<usize> = [&opts.looks, &gltf.looks]
        .into_iter()
        .find(|l| !l.is_empty())
        .cloned()
        .unwrap_or_else(|| vec![0]);
    if let Some(&bad) = looks.iter().find(|&&l| l >= src.looks.looks.len()) {
        return Err(err(format!("look {bad} does not exist; the model has {}", src.looks.looks.len())));
    }
    let range: BTreeSet<usize> = looks
        .iter()
        .flat_map(|&l| {
            let (s, c) = src.lod0(l);
            s..s + c
        })
        .filter(|&i| i < n)
        .collect();
    report.looks = looks.clone();

    // Primitives
    let mut prims: Vec<Prim> = Vec::new();
    for m in &gltf.meshes {
        if m.vertexes.is_empty() || m.faces.is_empty() {
            report.warn(format!("primitive '{}' has no triangles; skipped", m.name));
            continue;
        }
        if m.faces.iter().any(|f| f.0.max(f.1).max(f.2) as usize >= m.vertexes.len()) {
            return Err(err(format!("primitive '{}' has triangles past its {} vertices", m.name, m.vertexes.len())));
        }
        let named = name_hint(&m.name);
        let hint = m.subset_hint.or(named).filter(|&h| h < n);
        let parts = split_primitive(m.clone());
        if parts.len() > 1 {
            report.warn(format!(
                "primitive '{}' has {} vertices; split into {} subsets of at most {MAX_SUBSET_VERTICES}",
                m.name,
                m.vertexes.len(),
                parts.len()
            ));
        }
        for (k, mesh) in parts.into_iter().enumerate() {
            prims.push(Prim {
                mesh,
                hint: if k == 0 { hint } else { None },
                named: named.is_some(),
                slot: SlotRef::Existing(0),
                source: None,
                identity: None,
                tier: Tier::New,
                skinned: false,
                skin_rebuilt: false,
                morph_vertices: 0,
            });
        }
    }
    if prims.is_empty() {
        return Err(err("the glTF has no geometry".into()));
    }

    // Material slots: existing by name, new ones need a `.material` path.
    let mut new_slots: Vec<(String, String)> = Vec::new();
    let mut missing_paths: Vec<String> = Vec::new();
    for p in &mut prims {
        let name = p.mesh.material.clone();
        if let Some(i) = name.as_deref().and_then(|nm| find_slot(nm, &src.slot_names)) {
            p.slot = SlotRef::Existing(i);
            continue;
        }
        let path = name
            .as_deref()
            .and_then(|nm| lookup_ci(&opts.materials, nm).cloned())
            .or_else(|| p.mesh.material_path.clone())
            .map(|p| game_path(&p))
            .filter(|p| !p.is_empty());
        match (name, path, p.hint) {
            (Some(nm), Some(path), _) => {
                match new_slots.iter().find(|(s, _)| *s == nm) {
                    Some((_, prev)) if crc64::hash(prev) != crc64::hash(&path) => {
                        report.warn(format!("new slot '{nm}' has two paths; using '{prev}'"));
                    }
                    Some(_) => {}
                    None => new_slots.push((nm.clone(), path)),
                }
                p.slot = SlotRef::New(nm);
            }
            (name, _, Some(h)) => {
                let keep = src.meshes[h].material_index;
                report.warn(format!(
                    "primitive '{}': material {:?} is not a slot of this model and has no path; keeping subset {h}'s slot",
                    p.mesh.name, name
                ));
                p.slot = SlotRef::Existing(keep);
            }
            (name, _, None) => missing_paths.push(name.unwrap_or_else(|| format!("(none on '{}')", p.mesh.name))),
        }
    }
    if !missing_paths.is_empty() {
        missing_paths.sort();
        missing_paths.dedup();
        return Err(err(format!(
            "new material slots need a .material path: {}. Existing slots: {}",
            missing_paths.join(", "),
            src.slot_names.join(", ")
        )));
    }

    // Identity: subset hints first, then same-slot subsets with the same topology, then the
    // leftovers of each slot in order.
    let mut claimed: HashMap<usize, usize> = HashMap::new();
    let mut by_hint: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (pi, p) in prims.iter().enumerate() {
        if let Some(h) = p.hint {
            by_hint.entry(h).or_default().push(pi);
        }
    }
    for (h, ps) in by_hint {
        let scored: Vec<(usize, Option<Identity>)> =
            ps.iter().map(|&pi| (pi, identity(&prims[pi].mesh, &src.meshes[h], &src))).collect();
        let best = scored
            .iter()
            .min_by(|a, b| {
                let key = |x: &Option<Identity>| x.as_ref().map_or(f64::INFINITY, |i| i.distance);
                key(&a.1).total_cmp(&key(&b.1))
            })
            .map(|(pi, id)| (*pi, id.clone()));
        if let Some((pi, id)) = best {
            claimed.insert(h, pi);
            prims[pi].source = Some(h);
            prims[pi].identity = id;
        }
        if ps.len() > 1 {
            report.warn(format!("{} primitives name subset {h}; the closest keeps it, the rest become new subsets", ps.len()));
        }
    }
    for pi in 0..prims.len() {
        let SlotRef::Existing(slot) = prims[pi].slot else { continue };
        if prims[pi].source.is_some() {
            continue;
        }
        let best = range
            .iter()
            .filter(|&&s| !claimed.contains_key(&s) && src.meshes[s].material_index == slot)
            .filter_map(|&s| identity(&prims[pi].mesh, &src.meshes[s], &src).map(|id| (s, id)))
            .min_by(|a, b| a.1.distance.total_cmp(&b.1.distance));
        if let Some((s, id)) = best {
            claimed.insert(s, pi);
            prims[pi].source = Some(s);
            prims[pi].identity = Some(id);
        }
    }
    for pi in 0..prims.len() {
        let SlotRef::Existing(slot) = prims[pi].slot else { continue };
        if prims[pi].source.is_some() {
            continue;
        }
        if let Some(&s) = range.iter().find(|&&s| !claimed.contains_key(&s) && src.meshes[s].material_index == slot) {
            claimed.insert(s, pi);
            prims[pi].source = Some(s);
        }
    }

    // Tiers
    let has_skin = src.skin.is_some();
    for p in &mut prims {
        match p.source {
            Some(s) => {
                let m = &src.meshes[s];
                p.skinned = m.is_skinned();
                match p.identity.take() {
                    Some(id) => {
                        if let Some(order) = &id.order {
                            permute(&mut p.mesh, order);
                            p.mesh.faces = src.faces(m).unwrap_or_default();
                        }
                        let weights = same_weights(&p.mesh, m, &src);
                        p.tier = if same_attributes(&p.mesh, m, &src) && weights { Tier::Unchanged } else { Tier::Moved };
                        p.skin_rebuilt = p.skinned && !weights;
                        p.identity = Some(id);
                    }
                    None => {
                        p.tier = Tier::Rebuilt;
                        p.skin_rebuilt = p.skinned;
                    }
                }
            }
            None => {
                p.tier = Tier::New;
                p.skinned = has_skin && has_weights(&p.mesh);
                p.skin_rebuilt = p.skinned;
                if has_skin && !p.skinned {
                    report.warn(format!("primitive '{}' has no skin weights; it will not follow the skeleton", p.mesh.name));
                }
            }
        }
        if p.skin_rebuilt && !has_weights(&p.mesh) {
            report.warn(format!("primitive '{}' has no skin weights; its vertices are bound to joint 0", p.mesh.name));
        }
        if let Some(names) = &p.mesh.joint_names {
            let unknown: Vec<&str> = names.iter().filter(|n| !src.bones.contains_key(*n)).map(|s| s.as_str()).take(5).collect();
            if p.skin_rebuilt && !unknown.is_empty() {
                report.warn(format!("primitive '{}' is weighted to joints this model lacks: {}", p.mesh.name, unknown.join(", ")));
            }
        }
    }

    // Morphs: shape keys are authoritative once any primitive carries them. Morphing vertices
    // must sit in the subset's leading skin batches, so rebuilt subsets put them first.
    let any_targets = prims.iter().any(|p| !p.mesh.targets.is_empty());
    let gltf_morphs = src.morphs.is_some() && any_targets && !opts.keep_morphs;
    if any_targets && src.morphs.is_none() && !opts.keep_morphs {
        report.warn("the model has no morph sections (one cannot be added); shape keys are ignored".into());
    }
    for p in &mut prims {
        let pref = p.source.map_or(0, |s| src.anim_prefix(&src.meshes[s]));
        if !gltf_morphs || p.mesh.targets.is_empty() {
            p.mesh.targets.clear();
            if matches!(p.tier, Tier::Moved) {
                p.mesh.anim_prefix = pref;
            }
            continue;
        }
        if !p.skinned {
            report.warn(format!("primitive '{}' has shape keys but no skin; morphs need a skinned subset", p.mesh.name));
            p.mesh.targets.clear();
            continue;
        }
        let mv = morph_vertices(&p.mesh);
        p.morph_vertices = mv.len();
        if matches!(p.tier, Tier::Unchanged | Tier::Moved) {
            if mv.last().is_some_and(|&v| v as usize >= pref) {
                p.tier = Tier::Rebuilt;
                p.skin_rebuilt = true;
            } else {
                p.mesh.anim_prefix = pref;
                continue;
            }
        }
        p.mesh.anim_prefix = morphing_first(&mut p.mesh);
    }

    // New subset table: removed subsets drop out, new ones go at the end of the primary look's LOD 0.
    let all_named = prims.iter().all(|p| p.named);
    let remove = opts.remove_missing.unwrap_or(!all_named);
    let missing: Vec<usize> = range.iter().copied().filter(|s| !claimed.contains_key(s)).collect();
    let removed: BTreeSet<usize> = if remove { missing.iter().copied().collect() } else { BTreeSet::new() };
    if remove {
        report.removed = missing;
    } else {
        report.kept = missing;
    }
    let new_prims: Vec<usize> = (0..prims.len()).filter(|&pi| prims[pi].source.is_none()).collect();
    let (s0, c0) = src.lod0(looks[0]);
    if !new_prims.is_empty() && c0 == 0 {
        return Err(err(format!("look {} has no LOD 0 subsets to add new ones next to", looks[0])));
    }
    let insert_at = s0 + c0;
    let mut entries: Vec<Entry> = Vec::with_capacity(n + new_prims.len());
    for i in 0..=n {
        if i == insert_at {
            entries.extend(new_prims.iter().map(|&pi| Entry::New(pi)));
        }
        if i < n && !removed.contains(&i) {
            entries.push(Entry::Orig(i));
        }
    }
    if entries.len() > MAX_SUBSETS {
        return Err(err(format!("{} subsets; a model holds at most {MAX_SUBSETS}", entries.len())));
    }
    let structure_changed = !new_prims.is_empty() || !removed.is_empty();
    let mut old_to_new: Vec<Option<usize>> = vec![None; n];
    let mut prim_index: HashMap<usize, usize> = HashMap::new();
    for (f, e) in entries.iter().enumerate() {
        match *e {
            Entry::Orig(i) => {
                old_to_new[i] = Some(f);
                if let Some(&pi) = claimed.get(&i) {
                    prim_index.insert(pi, f);
                }
            }
            Entry::New(pi) => {
                prim_index.insert(pi, f);
            }
        }
    }

    // Material slots: insert new ones (sorted by name hash), repoint existing ones.
    let mut mat = model
        .dat1
        .get_section_data(TAG_MATERIAL)
        .map(MaterialSection::parse)
        .transpose()?
        .ok_or(ToolkitError::SectionNotFound(TAG_MATERIAL))?;
    let mut slot_map: Vec<u16> = (0..mat.slots.len() as u16).collect();
    for (name, path) in &new_slots {
        let hash = crc32::hash(&name.to_ascii_lowercase());
        if mat.ids.iter().any(|e| e.name_hash == hash) {
            return Err(err(format!("new slot '{name}' collides with an existing slot's name hash; rename it")));
        }
        let path_offset = model.dat1.intern_string(path);
        let name_offset = model.dat1.intern_string(name);
        let remap = mat.insert_slot(path_offset, name_offset, crc64::hash(path), name);
        slot_map.iter_mut().for_each(|s| *s = remap[*s as usize]);
        report.new_slots.push(name.clone());
    }
    for (name, path) in &opts.materials {
        let Some(i) = find_slot(name, &src.slot_names) else { continue };
        let path = game_path(path);
        if path.is_empty() || crc64::hash(&path) == crc64::hash(&src.slot_paths[i as usize]) {
            continue;
        }
        let at = slot_map[i as usize] as usize;
        mat.slots[at].path_offset = model.dat1.intern_string(&path) as u64;
        mat.ids[at].asset_id = crc64::hash(&path);
        report.repointed_slots.push(src.slot_names[i as usize].clone());
    }
    let slot_of = |r: &SlotRef| -> u16 {
        match r {
            SlotRef::Existing(i) => slot_map[*i as usize],
            SlotRef::New(name) => {
                let hash = crc32::hash(&name.to_ascii_lowercase());
                mat.ids.iter().position(|e| e.name_hash == hash).unwrap_or(0) as u16
            }
        }
    };
    let slot_name = |r: &SlotRef| -> String {
        match r {
            SlotRef::Existing(i) => src.slot_names[*i as usize].clone(),
            SlotRef::New(name) => name.clone(),
        }
    };

    // Subset records
    let mut overrides: HashMap<u16, (u16, u16)> = HashMap::new();
    for (name, o) in &opts.slot_flags {
        let slot = match find_slot(name, &src.slot_names) {
            Some(i) => SlotRef::Existing(i),
            None if new_slots.iter().any(|(s, _)| s == name) => SlotRef::New(name.clone()),
            None => {
                report.warn(format!("flag override for unknown slot '{name}' ignored"));
                continue;
            }
        };
        overrides.insert(slot_of(&slot), (parse_flags(&o.set, name)?, parse_flags(&o.clear, name)?));
    }
    let (total_v, total_i) = (src.vertexes.len() as u32, src.indices.len() as u32);
    let mut records: Vec<MeshDefinition> = Vec::with_capacity(entries.len());
    for e in &entries {
        let rec = match *e {
            Entry::Orig(i) => {
                let mut r = src.meshes[i].clone();
                r.material_index = slot_map.get(r.material_index as usize).copied().unwrap_or(r.material_index);
                if let Some(&pi) = claimed.get(&i) {
                    r.material_index = slot_of(&prims[pi].slot);
                }
                r
            }
            Entry::New(pi) => {
                let p = &prims[pi];
                let same_slot = match &p.slot {
                    SlotRef::Existing(s) => range.iter().copied().chain(0..n).find(|&i| src.meshes[i].material_index == *s),
                    SlotRef::New(_) => None,
                };
                let template = same_slot.or(range.iter().next().copied()).unwrap_or(0);
                let mut r = src.meshes.get(template).cloned().ok_or_else(|| err("the model has no subsets".into()))?;
                if same_slot.is_none() {
                    r.flags &= !subset_flags::OVERRIDABLE.iter().fold(0u16, |a, &(_, b)| a | b);
                }
                r.material_index = slot_of(&p.slot);
                r.vertex_start = total_v;
                r.vertex_count = 0;
                r.index_start = total_i;
                r.index_count = 0;
                r.first_skin_batch = 0;
                r.skin_batches_count = 0;
                r.first_weight_index = 0;
                r.lod_proxy_id = 0;
                r.flags &= !(subset_flags::IS_SKINNED | subset_flags::GPU_SKINNING | subset_flags::LOD_PROXY);
                r.flags |= subset_flags::LOCAL_INDICES;
                if p.skinned {
                    r.flags |= subset_flags::IS_SKINNED;
                    if src.gpu_skin {
                        r.flags |= subset_flags::GPU_SKINNING;
                    }
                }
                r
            }
        };
        records.push(rec);
    }
    for (f, r) in records.iter_mut().enumerate() {
        let covered = match entries[f] {
            Entry::Orig(i) => claimed.contains_key(&i),
            Entry::New(_) => true,
        };
        if let (true, Some(&(set, clear))) = (covered, overrides.get(&r.material_index)) {
            r.flags = (r.flags | set) & !clear;
        }
    }

    // Look ranges follow the new table; new subsets land in every range that ends at the insertion point.
    let key = |e: &Entry| match *e {
        Entry::Orig(i) => 2 * i,
        Entry::New(_) => 2 * insert_at - 1,
    };
    let keys: Vec<usize> = entries.iter().map(key).collect();
    let mut look_sec = LookSection::parse(model.dat1.get_section_data(TAG_LOOK).unwrap_or_default())?;
    let mut gained: Vec<usize> = Vec::new();
    for (li, look) in look_sec.looks.iter_mut().enumerate() {
        for (k, lod) in look.lods.iter_mut().take(6).enumerate() {
            let (s, e) = (2 * lod.start as usize, 2 * (lod.start as usize + lod.count as usize));
            let start = keys.partition_point(|&x| x < s);
            let count = keys.partition_point(|&x| x < e) - start;
            if k == 0 && lod.count > 0 && !new_prims.is_empty() && s < 2 * insert_at - 1 && 2 * insert_at - 1 < e && !looks.contains(&li) {
                gained.push(li);
            }
            lod.start = start as u16;
            lod.count = if lod.count == 0 { 0 } else { count as u16 };
        }
    }
    if !gained.is_empty() {
        report.warn(format!("looks {gained:?} share look {}'s range and also show the new subsets", looks[0]));
    }
    if !new_prims.is_empty() && looks.len() > 1 {
        report.warn(format!("new subsets are added to look {} only", looks[0]));
    }

    // Cloth: vertex-cloth entries name subsets by index.
    if let Some(cloth) = model.dat1.get_section_data(TAG_CLOTH_META_DATA).map(|c| c.to_vec()) {
        if cloth.len() >= 16 {
            let mut c = cloth;
            let off = u16::from_le_bytes([c[4], c[5]]) as usize * 16;
            let count = u16::from_le_bytes([c[6], c[7]]) as usize;
            for k in 0..count {
                let at = off + k * 8 + 4;
                let Some(b) = c.get(at..at + 2) else { break };
                let s = u16::from_le_bytes([b[0], b[1]]) as usize;
                match old_to_new.get(s).copied().flatten() {
                    Some(f) => c[at..at + 2].copy_from_slice(&(f as u16).to_le_bytes()),
                    None => report.warn(format!("vertex cloth on removed subset {s} now points at nothing")),
                }
            }
            model.dat1.set_section_data(TAG_CLOTH_META_DATA, c)?;
        }
    }

    if !opts.strip_hair.unwrap_or(false)
        && model.dat1.get_section_data(TAG_SPLINE_SKIN_BINDING).is_some()
        && (structure_changed || prims.iter().any(|p| matches!(p.tier, Tier::Rebuilt | Tier::New)))
    {
        report.warn(
            "this model has hair / fur strands bound to subset triangles; changed topology or subset order can detach them (strip_hair removes them)".into(),
        );
    }

    // Morph set in the new numbering, built before the tables are rewritten.
    let morphs_changed = src.morphs.is_some()
        && (gltf_morphs || structure_changed || prims.iter().any(|p| !matches!(p.tier, Tier::Unchanged)));
    let mut morph_set: Option<MorphSet> = None;
    if let (true, Some(orig)) = (morphs_changed, src.morphs.as_ref()) {
        let mut defs: BTreeMap<u32, MorphDef> = BTreeMap::new();
        let mut lost = 0usize;
        for m in &orig.morphs {
            let mut def = MorphDef { subsets: BTreeMap::new(), ..m.clone() };
            for (&s, deltas) in &m.subsets {
                let Some(f) = old_to_new.get(s as usize).copied().flatten() else { continue };
                let keep = match claimed.get(&(s as usize)) {
                    None => true,
                    Some(_) if gltf_morphs => false,
                    Some(&pi) => matches!(prims[pi].tier, Tier::Unchanged | Tier::Moved),
                };
                if keep {
                    def.subsets.insert(f as u16, deltas.clone());
                } else if !gltf_morphs {
                    lost += deltas.len();
                }
            }
            defs.insert(m.id, def);
        }
        if lost > 0 {
            report.warn(format!("{lost} morph deltas on rebuilt subsets were dropped; export shape keys to keep them"));
        }
        let mut new_names: Vec<String> = Vec::new();
        for (pi, p) in prims.iter().enumerate() {
            let Some(&f) = prim_index.get(&pi) else { continue };
            for t in &p.mesh.targets {
                let id = morph_id(&t.name, Some(orig));
                let def = defs.entry(id).or_insert_with(|| {
                    new_names.push(t.name.clone());
                    MorphDef {
                        id,
                        name: t.name.clone(),
                        name_offset: None,
                        component_bits: NEW_MORPH_BITS,
                        ranges: None,
                        subsets: BTreeMap::new(),
                    }
                });
                let deltas: Vec<Delta> = t
                    .deltas
                    .iter()
                    .map(|&(v, pos, nrm)| (v, pos, if t.has_normals { nrm } else { [0.0; 3] }))
                    .collect();
                def.subsets.insert(f as u16, deltas);
            }
        }
        let mut pairs = orig.pairs.clone();
        for name in &new_names {
            let Some(rest) = name.strip_prefix("LF_") else { continue };
            let mirror = format!("RT_{rest}");
            if defs.contains_key(&crc32::hash(&mirror)) {
                pairs.push((crc32::hash(name), crc32::hash(&mirror)));
            }
        }
        morph_set = Some(MorphSet { morphs: defs.into_values().collect(), pairs });
    }

    // Write the planned tables, then inject the geometry that changed.
    model.dat1.set_section_data(TAG_MATERIAL, mat.save())?;
    model.dat1.set_section_data(TAG_MESHES, MeshDefinition::save_all(&records))?;
    model.dat1.set_section_data(TAG_LOOK, look_sec.save())?;

    let mut prepared = GltfModel { bones: Vec::new(), meshes: Vec::new(), looks: Vec::new() };
    let mut skin_from_gltf = HashSet::new();
    for (pi, p) in prims.iter().enumerate() {
        let f = prim_index[&pi];
        if p.skin_rebuilt || matches!(p.tier, Tier::Rebuilt | Tier::New) {
            skin_from_gltf.insert(f);
        }
        if p.tier != Tier::Unchanged {
            let mut m = p.mesh.clone();
            m.name = format!("sm{f}");
            prepared.meshes.push(m);
        }
    }
    let plan = InjectPlan {
        skin_from_gltf: Some(skin_from_gltf),
        repack_skin: structure_changed,
        morphs_rebuilt: morph_set.is_some(),
        structure_changed,
    };
    let geometry_changed = if prepared.meshes.is_empty() && !structure_changed {
        false
    } else {
        inject_prepared(model, &prepared, &plan)?
    };

    if geometry_changed {
        relink_lod_proxies(model)?;
        sync_lod_subset_bits(&mut model.dat1)?;
    }

    if let Some(set) = morph_set {
        let subsets = MeshDefinition::parse_all(model.dat1.get_section_data(TAG_MESHES).unwrap_or_default())?;
        let batches = SkinBatch::parse_all(model.dat1.get_section_data(TAG_SKIN_BATCH).unwrap_or_default())?;
        let dat1 = &mut model.dat1;
        let enc = morph_build::encode(&set, &subsets, &batches, |name| dat1.intern_string(name))?;
        report.morphs = Some(set.morphs.len());
        report.morph_deltas_dropped = enc.dropped;
        if enc.dropped > 0 {
            report.warn(format!("{} morph deltas fell outside their subset's morphing skin batches and were dropped", enc.dropped));
        }
        model.dat1.set_section_data(TAG_ANIM_MORPH_INFO, enc.info)?;
        model.dat1.set_section_data(TAG_ANIM_MORPH_DATA, enc.data)?;
        model.dat1.set_section_data(TAG_ANIM_MORPH_INDICES, enc.indices)?;
    }

    // Last, so strings interned above sit behind the pool padding too.
    if opts.strip_hair.unwrap_or(false) {
        report.hair_removed = strip_hair(model)?;
    }

    for (pi, p) in prims.iter().enumerate() {
        report.subsets.push(SubsetReport {
            subset: prim_index[&pi],
            source: p.source,
            primitive: p.mesh.name.clone(),
            slot: slot_name(&p.slot),
            tier: p.tier,
            vertices: p.mesh.vertexes.len(),
            triangles: p.mesh.faces.len(),
            skin_rebuilt: p.skin_rebuilt,
            morph_vertices: p.morph_vertices,
        });
    }
    report.subsets.sort_by_key(|s| s.subset);
    Ok(report)
}

/// A subset sharing its geometry block with an earlier one points at it and is flagged a proxy;
/// the first user of a block points at itself (11,353 / 11,354 shipped models).
fn relink_lod_proxies(model: &mut ModelFile) -> Result<()> {
    let data = model.dat1.get_section_data(TAG_MESHES).ok_or(ToolkitError::SectionNotFound(TAG_MESHES))?;
    let mut meshes = MeshDefinition::parse_all(data)?;
    let mut first: HashMap<(u32, u32, u32, u32), usize> = HashMap::new();
    for i in 0..meshes.len() {
        let m = &meshes[i];
        let owner = *first.entry((m.vertex_start, m.vertex_count, m.index_start, m.index_count)).or_insert(i);
        let proxy = if owner != i { 0x8000 } else { 0 };
        meshes[i].lod_proxy_id = (m.lod_proxy_id & 0xF) | ((owner as u16 & 0x7FF) << 4) | proxy;
    }
    model.dat1.set_section_data(TAG_MESHES, MeshDefinition::save_all(&meshes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subset_names() {
        assert_eq!(name_hint("sm03_body"), Some(3));
        assert_eq!(name_hint("sm12"), Some(12));
        assert_eq!(name_hint("5_sm08_head.001"), Some(8));
        assert_eq!(name_hint("sm_body"), None);
        assert_eq!(name_hint("mtl_sm1"), None);
        assert_eq!(name_hint("sm1x"), None);
    }

    #[test]
    fn blender_suffix() {
        let slots = vec!["Body".to_string(), String::new(), "eyes".to_string()];
        assert_eq!(find_slot("body.001", &slots), Some(0));
        assert_eq!(find_slot("EYES", &slots), Some(2));
        assert_eq!(find_slot("mat1", &slots), Some(1));
        assert_eq!(find_slot("mat2", &slots), None);
        assert_eq!(find_slot("teeth", &slots), None);
    }
}
