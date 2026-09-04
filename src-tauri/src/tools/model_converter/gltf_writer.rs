use crate::core::error::{Result, ToolkitError};
use crate::tools::model_converter::model::ModelFile;
use crate::tools::model_converter::sections::{
    geo::{TAG_VERTEXES, TAG_UV1, VertexesSection, Uv1Section},
    meshes::{TAG_MESHES, MeshDefinition},
    joints::{TAG_JOINTS, TAG_JOINTS_TRANSFORM, Joint, JointsTransform},
    look::{TAG_LOOK, LookSection},
    skin::{TAG_SKIN_BATCH, TAG_SKIN_DATA, TAG_RCRA_SKIN, SkinBatch, RcraSkinEntry,
           decode_skin_data, decode_rcra_skin, VertexWeights},
    built::{TAG_BUILT, get_uv_scale, get_position_scale},
};

use glam::{Mat4, Quat, Vec3};
use gltf::json as gj;
use gj::validation::{Checked, USize64};

const TAG_INDEXES:   u32 = 0x0859863D;
const TAG_MATERIALS: u32 = 0x3250BB80;

/// Real binary glTF (.glb) export, distinct from the XNALara-style ASCII
/// export in `ascii_writer.rs`. Every mesh's attributes live in their own
/// tightly-packed bufferViews (no interleaving), 4-byte aligned. Joint node
/// hierarchy comes directly from `JointsTransform` local TRS (no world-space
/// recomposition needed on export), matching what `gltf_reader.rs` expects
/// when it re-reads the file: skin joint names matched by string against
/// `SECTION_JOINTS`, and JOINTS_0 indices equal to the source bone's raw
/// index (since `skin.joints` is emitted in the same order as the model's
/// joint table).
pub fn model_to_glb(model: &ModelFile, look: usize) -> Result<Vec<u8>> {
    model_to_glb_for_looks(model, &[look])
}

struct Buf {
    bytes: Vec<u8>,
}

impl Buf {
    fn new() -> Self { Buf { bytes: Vec::new() } }

    fn align4(&mut self) {
        while self.bytes.len() % 4 != 0 { self.bytes.push(0); }
    }

    fn push_vec3(&mut self, v: &[[f32; 3]]) -> (usize, usize) {
        self.align4();
        let start = self.bytes.len();
        for e in v {
            for c in e { self.bytes.extend_from_slice(&c.to_le_bytes()); }
        }
        (start, self.bytes.len() - start)
    }

    fn push_vec2(&mut self, v: &[[f32; 2]]) -> (usize, usize) {
        self.align4();
        let start = self.bytes.len();
        for e in v {
            for c in e { self.bytes.extend_from_slice(&c.to_le_bytes()); }
        }
        (start, self.bytes.len() - start)
    }

    fn push_vec4_f32(&mut self, v: &[[f32; 4]]) -> (usize, usize) {
        self.align4();
        let start = self.bytes.len();
        for e in v {
            for c in e { self.bytes.extend_from_slice(&c.to_le_bytes()); }
        }
        (start, self.bytes.len() - start)
    }

    fn push_vec4_u8(&mut self, v: &[[u8; 4]]) -> (usize, usize) {
        self.align4();
        let start = self.bytes.len();
        for e in v { self.bytes.extend_from_slice(e); }
        (start, self.bytes.len() - start)
    }

    fn push_u16(&mut self, v: &[u16]) -> (usize, usize) {
        self.align4();
        let start = self.bytes.len();
        for c in v { self.bytes.extend_from_slice(&c.to_le_bytes()); }
        (start, self.bytes.len() - start)
    }

    fn push_mat4(&mut self, v: &[[f32; 16]]) -> (usize, usize) {
        self.align4();
        let start = self.bytes.len();
        for m in v {
            for c in m { self.bytes.extend_from_slice(&c.to_le_bytes()); }
        }
        (start, self.bytes.len() - start)
    }
}

#[allow(clippy::too_many_arguments)]
fn add_accessor(
    root: &mut gj::Root,
    buffer_view: gj::Index<gj::buffer::View>,
    component_type: gj::accessor::ComponentType,
    type_: gj::accessor::Type,
    count: usize,
    min: Option<Vec<f32>>,
    max: Option<Vec<f32>>,
) -> gj::Index<gj::Accessor> {
    root.push(gj::Accessor {
        buffer_view: Some(buffer_view),
        byte_offset: None,
        count: USize64(count as u64),
        component_type: Checked::Valid(gj::accessor::GenericComponentType(component_type)),
        extensions: None,
        extras: Default::default(),
        type_: Checked::Valid(type_),
        min: min.map(|v| serde_json::to_value(v).unwrap()),
        max: max.map(|v| serde_json::to_value(v).unwrap()),
        name: None,
        normalized: false,
        sparse: None,
    })
}

fn add_view(
    root: &mut gj::Root,
    buffer: gj::Index<gj::Buffer>,
    offset: usize,
    length: usize,
    target: Option<gj::buffer::Target>,
) -> gj::Index<gj::buffer::View> {
    root.push(gj::buffer::View {
        buffer,
        byte_length: USize64(length as u64),
        byte_offset: Some(USize64(offset as u64)),
        byte_stride: None,
        name: None,
        target: target.map(Checked::Valid),
        extensions: None,
        extras: Default::default(),
    })
}

pub fn model_to_glb_for_looks(model: &ModelFile, looks: &[usize]) -> Result<Vec<u8>> {
    let dat1 = &model.dat1;

    let built_pos_scale: f32 = dat1.get_section_data(TAG_BUILT).map(get_position_scale).unwrap_or(1.0 / 4096.0);

    let vert_data = dat1.get_section_data(TAG_VERTEXES)
        .ok_or_else(|| ToolkitError::SectionNotFound(TAG_VERTEXES))?;
    let vertexes_sec = VertexesSection::parse_scaled(vert_data, built_pos_scale)?;
    let vertexes = &vertexes_sec.vertexes;

    let mesh_data = dat1.get_section_data(TAG_MESHES)
        .ok_or_else(|| ToolkitError::SectionNotFound(TAG_MESHES))?;
    let meshes = MeshDefinition::parse_all(mesh_data)?;

    let idx_data = dat1.get_section_data(TAG_INDEXES)
        .ok_or_else(|| ToolkitError::SectionNotFound(TAG_INDEXES))?;
    let indexes_sec = crate::tools::model_converter::sections::geo::IndexesSection::parse(idx_data)?;
    let indexes = &indexes_sec.values;

    let look_data = dat1.get_section_data(TAG_LOOK)
        .ok_or_else(|| ToolkitError::SectionNotFound(TAG_LOOK))?;
    let look_sec = LookSection::parse(look_data)?;

    let uv1_sec: Option<Uv1Section> = dat1.get_section_data(TAG_UV1).map(|d| Uv1Section::parse(d).ok()).flatten();
    let built_uv_scale: f32 = dat1.get_section_data(TAG_BUILT).map(get_uv_scale).unwrap_or(1.0 / 16384.0);

    // Skin
    let batched_skin: Option<Vec<VertexWeights>> = {
        if let (Some(raw), Some(batch_data)) = (dat1.get_section_data(TAG_SKIN_DATA), dat1.get_section_data(TAG_SKIN_BATCH)) {
            let batches = SkinBatch::parse_all(batch_data)?;
            Some(decode_skin_data(raw, &batches))
        } else { None }
    };
    let rcra_skin: Option<Vec<VertexWeights>> = dat1.get_section_data(TAG_RCRA_SKIN).map(|d| {
        decode_rcra_skin(&RcraSkinEntry::parse_all(d))
    });

    let joints_data = dat1.get_section_data(TAG_JOINTS);
    let transform_data = dat1.get_section_data(TAG_JOINTS_TRANSFORM);
    let has_bones_section = joints_data.is_some() && transform_data.is_some();
    let joints: Vec<Joint> = if has_bones_section { Joint::parse_all(joints_data.unwrap())? } else { Vec::new() };
    let transforms: Option<JointsTransform> = if has_bones_section {
        Some(JointsTransform::parse(transform_data.unwrap())?)
    } else { None };

    // Meshes to export: LOD 0, selected look(s)
    let lod = 0;
    let mut mesh_set = std::collections::BTreeSet::new();
    for &lk in looks {
        if let Some(l) = look_sec.looks.get(lk) {
            if let Some(lod_entry) = l.lods.get(lod) {
                for mi in lod_entry.start..(lod_entry.start + lod_entry.count) {
                    mesh_set.insert(mi as usize);
                }
            }
        }
    }
    let mesh_indices: Vec<usize> = mesh_set.into_iter().filter(|&i| i < meshes.len()).collect();

    let get_material_path = |mat_idx: u16| -> String {
        if let Some(mat_data) = dat1.get_section_data(TAG_MATERIALS) {
            let entry_offset = mat_idx as usize * 8;
            if entry_offset + 4 <= mat_data.len() {
                let path_offset = u32::from_le_bytes(mat_data[entry_offset..entry_offset + 4].try_into().unwrap());
                if let Some(s) = dat1.get_string(path_offset) {
                    return s;
                }
            }
        }
        String::new()
    };

    let mut buf = Buf::new();
    let mut root = gj::Root::default();
    root.asset.generator = Some("RCRA ModdingToolkit".to_string());

    // One glTF material per unique game material path, so Blender gets a
    // material slot per submesh instead of leaving primitives unassigned.
    // No textures/PBR data is available from the .model, so these are named
    // placeholders (default white, non-metallic) — just enough for Blender
    // to create and label the slot; the reader ignores materials entirely.
    let mut material_indices: std::collections::HashMap<String, gj::Index<gj::Material>> = std::collections::HashMap::new();

    let buffer_index: gj::Index<gj::Buffer> = root.push(gj::Buffer {
        byte_length: USize64(0), // patched after all data is written
        name: None,
        uri: None,
        extensions: None,
        extras: Default::default(),
    });

    // Joint nodes: local TRS taken directly from JointsTransform (already
    // parent-relative), so no world-space recomposition is needed for the
    // node hierarchy itself — only for the inverse bind matrices below.
    let mut joint_node_indices: Vec<gj::Index<gj::scene::Node>> = Vec::with_capacity(joints.len());
    let mut joint_worlds: Vec<Mat4> = vec![Mat4::IDENTITY; joints.len()];
    if let Some(ref tf) = transforms {
        for i in 0..joints.len() {
            let (lx, ly, lz) = tf.get_position(i);
            let (qx, qy, qz, qw) = tf.get_quaternion(i);
            let name = dat1.get_string(joints[i].string_offset);

            let node = root.push(gj::scene::Node {
                translation: Some([lx, ly, lz]),
                rotation: Some(gj::scene::UnitQuaternion([qx, qy, qz, qw])),
                name,
                ..Default::default()
            });
            joint_node_indices.push(node);

            let local = Mat4::from_rotation_translation(Quat::from_xyzw(qx, qy, qz, qw), Vec3::new(lx, ly, lz));
            joint_worlds[i] = if joints[i].parent >= 0 {
                joint_worlds[joints[i].parent as usize] * local
            } else {
                local
            };
        }
        // Wire up parent -> children after all joint nodes exist.
        for i in 0..joints.len() {
            if joints[i].parent >= 0 {
                let p = joints[i].parent as usize;
                let child_idx = joint_node_indices[i];
                let parent_node_idx = joint_node_indices[p].value();
                root.nodes[parent_node_idx].children.get_or_insert_with(Vec::new).push(child_idx);
            }
        }
    }

    let skin_index: Option<gj::Index<gj::Skin>> = if !joints.is_empty() {
        let ibms: Vec<[f32; 16]> = joint_worlds.iter().map(|w| w.inverse().to_cols_array()).collect();
        let (off, len) = buf.push_mat4(&ibms);
        let view = add_view(&mut root, buffer_index, off, len, None);
        let accessor = add_accessor(&mut root, view, gj::accessor::ComponentType::F32, gj::accessor::Type::Mat4, joints.len(), None, None);
        Some(root.push(gj::Skin {
            inverse_bind_matrices: Some(accessor),
            joints: joint_node_indices.clone(),
            name: Some("Armature".to_string()),
            skeleton: None,
            extensions: None,
            extras: Default::default(),
        }))
    } else {
        None
    };

    let mut scene_nodes: Vec<gj::Index<gj::scene::Node>> = joint_node_indices
        .iter()
        .enumerate()
        .filter(|(i, _)| joints[*i].parent < 0)
        .map(|(_, n)| *n)
        .collect();

    for &mi in &mesh_indices {
        let mesh = &meshes[mi];
        let mat_path = get_material_path(mesh.material_index);
        let mesh_name = format!("sm{:02}_{}", mi, mat_path);
        let material_name = if mat_path.is_empty() {
            format!("mat{}", mesh.material_index)
        } else {
            mat_path.clone()
        };
        let material_index = *material_indices.entry(material_name.clone()).or_insert_with(|| {
            root.push(gj::Material {
                name: Some(material_name),
                ..Default::default()
            })
        });

        let vstart = mesh.vertex_start as usize;
        let vcount = mesh.vertex_count as usize;
        let mesh_has_skin = has_bones_section && (mesh.is_skinned() || mesh.is_rcra_skinned());
        let skin_to_use = if mesh.is_rcra_skinned() { rcra_skin.as_deref() } else { batched_skin.as_deref() };
        let weight_offset = if mesh.is_rcra_skinned() { mesh.first_weight_index as usize } else { vstart };

        let mut positions: Vec<[f32; 3]> = Vec::with_capacity(vcount);
        let mut normals: Vec<[f32; 3]> = Vec::with_capacity(vcount);
        let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(vcount);
        // The batched skin format (SKIN_DATA + SKIN_BATCH) stores a variable
        // number of influences per vertex — this game's models reach 7 — unlike
        // the RCRA_SKIN compact array, which is fixed at 4. glTF carries 4 per
        // JOINTS_n/WEIGHTS_n set, so we collect up to MAX_INFLUENCES and emit a
        // second set when needed; dropping them silently under-weights the
        // vertex and wrecks the deformation on reimport.
        const MAX_INFLUENCES: usize = 8;
        let mut joints_arr: Vec<[u8; MAX_INFLUENCES]> = Vec::with_capacity(vcount);
        let mut weights_arr: Vec<[f32; MAX_INFLUENCES]> = Vec::with_capacity(vcount);
        let mut max_influences = 0usize;
        let mut dropped_influences = 0usize;

        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];

        for vi in vstart..(vstart + vcount) {
            let v = &vertexes[vi];
            positions.push([v.x, v.y, v.z]);
            for k in 0..3 {
                let c = [v.x, v.y, v.z][k];
                if c < min[k] { min[k] = c; }
                if c > max[k] { max[k] = c; }
            }
            normals.push([v.nx, v.ny, v.nz]);

            let (u, vv) = if let Some(ref uv1) = uv1_sec {
                let (ru, rv) = uv1.uvs[vi];
                (ru as f32 * built_uv_scale, rv as f32 * built_uv_scale)
            } else {
                (v.u, v.v)
            };
            uvs.push([u, vv]);

            if mesh_has_skin {
                let wi = vi - vstart + weight_offset;
                let empty: VertexWeights = Vec::new();
                let vw = skin_to_use.and_then(|s| s.get(wi)).unwrap_or(&empty);
                let mut ja = [0u8; MAX_INFLUENCES];
                let mut wa = [0f32; MAX_INFLUENCES];
                let used = vw.len().min(MAX_INFLUENCES);
                for k in 0..used {
                    ja[k] = vw[k].0;
                    wa[k] = vw[k].1;
                }
                if vw.len() > MAX_INFLUENCES {
                    dropped_influences += vw.len() - MAX_INFLUENCES;
                }
                if used > max_influences {
                    max_influences = used;
                }
                joints_arr.push(ja);
                weights_arr.push(wa);
            }
        }

        let (pos_off, pos_len) = buf.push_vec3(&positions);
        let pos_view = add_view(&mut root, buffer_index, pos_off, pos_len, Some(gj::buffer::Target::ArrayBuffer));
        let pos_accessor = add_accessor(&mut root, pos_view, gj::accessor::ComponentType::F32, gj::accessor::Type::Vec3, positions.len(), Some(min.to_vec()), Some(max.to_vec()));

        let (nrm_off, nrm_len) = buf.push_vec3(&normals);
        let nrm_view = add_view(&mut root, buffer_index, nrm_off, nrm_len, Some(gj::buffer::Target::ArrayBuffer));
        let nrm_accessor = add_accessor(&mut root, nrm_view, gj::accessor::ComponentType::F32, gj::accessor::Type::Vec3, normals.len(), None, None);

        let (uv_off, uv_len) = buf.push_vec2(&uvs);
        let uv_view = add_view(&mut root, buffer_index, uv_off, uv_len, Some(gj::buffer::Target::ArrayBuffer));
        let uv_accessor = add_accessor(&mut root, uv_view, gj::accessor::ComponentType::F32, gj::accessor::Type::Vec2, uvs.len(), None, None);

        let mut attributes = std::collections::BTreeMap::new();
        attributes.insert(Checked::Valid(gj::mesh::Semantic::Positions), pos_accessor);
        attributes.insert(Checked::Valid(gj::mesh::Semantic::Normals), nrm_accessor);
        attributes.insert(Checked::Valid(gj::mesh::Semantic::TexCoords(0)), uv_accessor);

        if mesh_has_skin {
            // One glTF set per 4 influences. Every set must cover every vertex
            // (zero-padded), per spec.
            let sets = ((max_influences.max(1) + 3) / 4).min(MAX_INFLUENCES / 4);
            if dropped_influences > 0 {
                eprintln!(
                    "[model_to_glb] mesh '{}' dropped {} influences beyond {} per vertex",
                    mesh_name, dropped_influences, MAX_INFLUENCES
                );
            }
            for s in 0..sets {
                let base = s * 4;
                let j_set: Vec<[u8; 4]> = joints_arr
                    .iter()
                    .map(|j| [j[base], j[base + 1], j[base + 2], j[base + 3]])
                    .collect();
                let w_set: Vec<[f32; 4]> = weights_arr
                    .iter()
                    .map(|w| [w[base], w[base + 1], w[base + 2], w[base + 3]])
                    .collect();

                let (j_off, j_len) = buf.push_vec4_u8(&j_set);
                let j_view = add_view(&mut root, buffer_index, j_off, j_len, Some(gj::buffer::Target::ArrayBuffer));
                let j_accessor = add_accessor(&mut root, j_view, gj::accessor::ComponentType::U8, gj::accessor::Type::Vec4, j_set.len(), None, None);

                let (w_off, w_len) = buf.push_vec4_f32(&w_set);
                let w_view = add_view(&mut root, buffer_index, w_off, w_len, Some(gj::buffer::Target::ArrayBuffer));
                let w_accessor = add_accessor(&mut root, w_view, gj::accessor::ComponentType::F32, gj::accessor::Type::Vec4, w_set.len(), None, None);

                attributes.insert(Checked::Valid(gj::mesh::Semantic::Joints(s as u32)), j_accessor);
                attributes.insert(Checked::Valid(gj::mesh::Semantic::Weights(s as u32)), w_accessor);
            }
        }

        // Faces: mesh-local indices, matching gltf_reader.rs's inject_vertexes
        // convention (which re-adds vertex_start on the way back in).
        let vc_offset = if mesh.has_relative_indices() { 0u16 } else { mesh.vertex_start as u16 };
        let face_count = mesh.index_count / 3;
        let mut idx_arr: Vec<u16> = Vec::with_capacity(face_count as usize * 3);
        for f in 0..face_count as usize {
            let base = mesh.index_start as usize + f * 3;
            let i0 = indexes[base + 2].wrapping_sub(vc_offset);
            let i1 = indexes[base + 1].wrapping_sub(vc_offset);
            let i2 = indexes[base + 0].wrapping_sub(vc_offset);
            idx_arr.push(i0);
            idx_arr.push(i1);
            idx_arr.push(i2);
        }
        let (idx_off, idx_len) = buf.push_u16(&idx_arr);
        let idx_view = add_view(&mut root, buffer_index, idx_off, idx_len, Some(gj::buffer::Target::ElementArrayBuffer));
        let idx_accessor = add_accessor(&mut root, idx_view, gj::accessor::ComponentType::U16, gj::accessor::Type::Scalar, idx_arr.len(), None, None);

        let primitive = gj::mesh::Primitive {
            attributes,
            extensions: None,
            extras: Default::default(),
            indices: Some(idx_accessor),
            material: Some(material_index),
            mode: Default::default(),
            targets: None,
        };

        let gltf_mesh_index = root.push(gj::Mesh {
            extensions: None,
            extras: Default::default(),
            name: Some(mesh_name.clone()),
            primitives: vec![primitive],
            weights: None,
        });

        let node = root.push(gj::scene::Node {
            mesh: Some(gltf_mesh_index),
            skin: if mesh_has_skin { skin_index } else { None },
            name: Some(mesh_name),
            ..Default::default()
        });
        scene_nodes.push(node);
    }

    // Patch the buffer's declared length now that all data has been written.
    root.buffers[buffer_index.value()].byte_length = USize64(buf.bytes.len() as u64);

    let scene = root.push(gj::Scene {
        extensions: None,
        extras: Default::default(),
        name: Some("Scene".to_string()),
        nodes: scene_nodes,
    });
    root.scene = Some(scene);

    let json_bytes = root.to_vec()
        .map_err(|e| ToolkitError::Parse(format!("failed to serialize glTF JSON: {}", e)))?;

    let glb = gltf::binary::Glb {
        header: gltf::binary::Header {
            magic: *b"glTF",
            version: 2,
            length: 0, // recomputed by to_vec()
        },
        json: std::borrow::Cow::Owned(json_bytes),
        bin: Some(std::borrow::Cow::Owned(buf.bytes)),
    };

    glb.to_vec().map_err(|e| ToolkitError::Parse(format!("failed to write GLB: {}", e)))
}
