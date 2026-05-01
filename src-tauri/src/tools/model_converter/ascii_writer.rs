use crate::core::error::{Result, ToolkitError};
use crate::core::math::{quat_mul, rotate_vec, vec_add};
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

const TAG_INDEXES:   u32 = 0x0859863D;
const TAG_MATERIALS: u32 = 0x3250BB80;

fn pretty(n: f32) -> String {
    let s = format!("{:.6}", n);
    let _dot = s.find('.').unwrap_or(s.len());
    let trimmed = s.trim_end_matches('0');
    let trimmed = if trimmed.ends_with('.') { &trimmed[..trimmed.len() - 1] } else { trimmed };
    let result = trimmed.to_string();
    if result == "-0" { "0".to_string() } else { result }
}

pub fn model_to_ascii(model: &ModelFile, look: usize) -> Result<String> {
    model_to_ascii_for_looks(model, &[look])
}

pub fn model_to_ascii_for_looks(model: &ModelFile, looks: &[usize]) -> Result<String> {
    let dat1 = &model.dat1;

    // Sections
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
    let msmr_skin: Option<Vec<VertexWeights>> = {
        if let (Some(raw), Some(batch_data)) = (dat1.get_section_data(TAG_SKIN_DATA), dat1.get_section_data(TAG_SKIN_BATCH)) {
            let batches = SkinBatch::parse_all(batch_data)?;
            Some(decode_skin_data(raw, &batches))
        } else { None }
    };

    let rcra_skin: Option<Vec<VertexWeights>> = dat1.get_section_data(TAG_RCRA_SKIN).map(|d| {
        decode_rcra_skin(&RcraSkinEntry::parse_all(d))
    });

    let has_bones_section = dat1.get_section_data(TAG_JOINTS).is_some();

    // Meshes to export (LOD 0, selected look(s))
    let lod = 0;
    let mut mesh_set = std::collections::BTreeSet::new();
    for &look in looks {
        if let Some(l) = look_sec.looks.get(look) {
            if let Some(lod_entry) = l.lods.get(lod) {
                for mi in lod_entry.start..(lod_entry.start + lod_entry.count) {
                    mesh_set.insert(mi as usize);
                }
            }
        }
    }
    let mesh_indices: Vec<usize> = mesh_set.into_iter().filter(|&i| i < meshes.len()).collect();

    // Materials
    let get_material_path = |mat_idx: u16| -> String {
        if let Some(mat_data) = dat1.get_section_data(TAG_MATERIALS) {
            // The section contains a series of (offset32, offset32) pairs
            // first is the path offset, second is the name offset
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

    // Max bone groups across mesh
    let groups_count_for_mesh = |mesh: &MeshDefinition| -> usize {
        let skin = if mesh.is_rcra_skinned() { rcra_skin.as_deref() } else { msmr_skin.as_deref() };
        let mut gc = 4;
        if let Some(sw) = skin {
            for vi in (mesh.vertex_start as usize)..(mesh.vertex_start as usize + mesh.vertex_count as usize) {
                if vi < sw.len() { gc = gc.max(sw[vi].len()); }
            }
        }
        gc
    };

    // Build output
    let mut out = String::new();

    // Bones
    write_bones(&mut out, dat1, has_bones_section)?;

    // Meshes
    out.push_str(&format!("{}\n", mesh_indices.len()));
    for &mi in &mesh_indices {
        let mesh = &meshes[mi];
        let mat_path = get_material_path(mesh.material_index);
        out.push_str(&format!("sm{:02}_{}\n", mi, mat_path));
        out.push_str("1\n");   // uv_layers
        out.push_str("0\n");   // textures

        let gc = groups_count_for_mesh(mesh);
        let skin_to_use = if mesh.is_rcra_skinned() { rcra_skin.as_deref() } else { msmr_skin.as_deref() };
        let weight_offset = if mesh.is_rcra_skinned() { mesh.first_weight_index as usize } else { mesh.vertex_start as usize };

        out.push_str(&format!("{}\n", mesh.vertex_count));
        for vi in (mesh.vertex_start as usize)..(mesh.vertex_start as usize + mesh.vertex_count as usize) {
            let v = &vertexes[vi];
            out.push_str(&format!("{} {} {}\n", pretty(v.x), pretty(v.y), pretty(v.z)));
            if let Some(rn) = v.raw_normal {
                out.push_str(&format!("{} {} {} #nrm:{:08X}\n", pretty(v.nx), pretty(v.ny), pretty(v.nz), rn));
            } else {
                out.push_str(&format!("{} {} {}\n", pretty(v.nx), pretty(v.ny), pretty(v.nz)));
            }
            out.push_str("0 0 0 0\n");

            let (u, vv) = if let Some(ref uv1) = uv1_sec {
                let (ru, rv) = uv1.uvs[vi];
                (ru as f32 * built_uv_scale, rv as f32 * built_uv_scale)
            } else {
                (v.u, v.v)
            };
            out.push_str(&format!("{} {}\n", pretty(u), pretty(vv)));

            if has_bones_section {
                let wi = vi - mesh.vertex_start as usize + weight_offset;
                let (groups_str, weights_str) = get_weights(wi, skin_to_use, gc);
                out.push_str(&format!("{}\n{}\n", groups_str, weights_str));
            }
        }

        // Faces
        let vc_offset = if mesh.has_relative_indices() { 0u16 } else { mesh.vertex_start as u16 };
        let face_count = mesh.index_count / 3;
        out.push_str(&format!("{}\n", face_count));
        for f in 0..face_count as usize {
            let base = mesh.index_start as usize + f * 3;
            let i0 = indexes[base + 2].wrapping_sub(vc_offset);
            let i1 = indexes[base + 1].wrapping_sub(vc_offset);
            let i2 = indexes[base + 0].wrapping_sub(vc_offset);
            out.push_str(&format!("{} {} {}\n", i0, i1, i2));
        }
    }

    Ok(out)
}

fn write_bones(out: &mut String, dat1: &crate::core::dat1::Dat1, has_bones: bool) -> Result<()> {
    let joints_data = dat1.get_section_data(TAG_JOINTS);
    let transform_data = dat1.get_section_data(TAG_JOINTS_TRANSFORM);

    if !has_bones || joints_data.is_none() || transform_data.is_none() {
        out.push_str("0\n");
        return Ok(());
    }

    let joints = Joint::parse_all(joints_data.unwrap())?;
    let transforms = JointsTransform::parse(transform_data.unwrap())?;

    // Accumulate world-space transforms
    let mut world: Vec<Option<([f32; 3], [f32; 4])>> = vec![None; joints.len()];

    fn get_world(
        i: usize,
        joints: &[Joint],
        transforms: &JointsTransform,
        world: &mut Vec<Option<([f32; 3], [f32; 4])>>,
    ) -> ([f32; 3], [f32; 4]) {
        if let Some(w) = world[i] { return w; }

        let (lx, ly, lz) = transforms.get_position(i);
        let (qx, qy, qz, qw) = transforms.get_quaternion(i);

        let result = if joints[i].parent == -1 {
            ([lx, ly, lz], [qx, qy, qz, qw])
        } else {
            let pi = joints[i].parent as usize;
            let (pp, pq) = get_world(pi, joints, transforms, world);
            let pq64 = (pq[0] as f64, pq[1] as f64, pq[2] as f64, pq[3] as f64);
            let lq64 = (qx as f64, qy as f64, qz as f64, qw as f64);
            let wq = quat_mul(pq64, lq64);
            let lv = (lx as f64, ly as f64, lz as f64);
            let pp64 = (pp[0] as f64, pp[1] as f64, pp[2] as f64);
            let rotated = rotate_vec(lv, pq64);
            let wp = vec_add(pp64, rotated);
            (
                [wp.0 as f32, wp.1 as f32, wp.2 as f32],
                [wq.0 as f32, wq.1 as f32, wq.2 as f32, wq.3 as f32],
            )
        };
        world[i] = Some(result);
        result
    }

    out.push_str(&format!("{}\n", joints.len()));
    for i in 0..joints.len() {
        let name = dat1.get_string(joints[i].string_offset).unwrap_or_default();
        let (pos, rot) = get_world(i, &joints, &transforms, &mut world);
        out.push_str(&format!("{}\n", name));
        out.push_str(&format!("{}\n", joints[i].parent));
        out.push_str(&format!("{} {} {} {} {} {} {}\n",
            pretty(pos[0]), pretty(pos[1]), pretty(pos[2]),
            pretty(rot[0]), pretty(rot[1]), pretty(rot[2]), pretty(rot[3]),
        ));
    }
    Ok(())
}

fn get_weights(wi: usize, skin: Option<&[VertexWeights]>, groups_count: usize) -> (String, String) {
    let empty: VertexWeights = Vec::new();
    let vertex = skin.and_then(|s| s.get(wi)).unwrap_or(&empty);

    let mut groups = String::new();
    let mut weights_str = String::new();
    for j in 0..groups_count {
        if !groups.is_empty() { groups.push(' '); weights_str.push(' '); }
        let (g, w) = vertex.get(j).copied().unwrap_or((0, 0.0));
        groups.push_str(&g.to_string());
        weights_str.push_str(&pretty(w));
    }
    (groups, weights_str)
}
