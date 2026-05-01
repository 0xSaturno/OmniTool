use crate::core::error::{Result, ToolkitError};
use crate::tools::model_converter::model::ModelFile;
use crate::tools::model_converter::sections::{
    geo::{TAG_VERTEXES, TAG_UV1, TAG_INDEXES, Vertex, VertexesSection, Uv1Section, IndexesSection},
    meshes::{TAG_MESHES, MeshDefinition},
    skin::{TAG_SKIN_BATCH, TAG_SKIN_DATA, TAG_RCRA_SKIN, SkinBatch, RcraSkinEntry},
    look::{TAG_LOOK, LookSection},
    built::{get_uv_scale, get_position_scale},
};

const TAG_BUILT:     u32 = 0x283D0383;
const TAG_MUSCLEDEF: u32 = 0x380A5744;


// ASCII Data structures

pub struct AsciiVertex {
    pub position: (f32, f32, f32),
    pub normal:   (f32, f32, f32),
    pub raw_normal: Option<u32>,
    pub uv:       Option<(f32, f32)>,
    pub groups:   Vec<u8>,
    pub weights:  Vec<f32>,
}

pub struct AsciiMesh {
    pub name:     String,
    pub vertexes: Vec<AsciiVertex>,
    pub faces:    Vec<(u32, u32, u32)>,
}

pub struct AsciiModel {
    pub bones: Vec<(String, i32, (f32, f32, f32))>,
    pub meshes: Vec<AsciiMesh>,
}

// Parser

pub fn parse_ascii(text: &str) -> Result<AsciiModel> {
    // Keep both raw lines (for #nrm: tags) and stripped lines (for value parsing).
    let mut lines: Vec<&str> = Vec::new();
    let mut raw_lines: Vec<&str> = Vec::new();
    for l in text.lines() {
        let stripped = if let Some(i) = l.find('#') { &l[..i] } else { l };
        let stripped = stripped.trim();
        if stripped.is_empty() { continue; }
        lines.push(stripped);
        raw_lines.push(l.trim());
    }

    let mut ptr = 0;

    let bones = parse_bones(&lines, &mut ptr)?;
    let meshes = parse_meshes(&lines, &raw_lines, &mut ptr, bones.len())?;

    Ok(AsciiModel { bones, meshes })
}

fn read_int(lines: &[&str], ptr: &mut usize) -> Result<i64> {
    let line = lines.get(*ptr).ok_or_else(|| ToolkitError::AsciiFormat { line: *ptr, message: "unexpected EOF".into() })?;
    *ptr += 1;
    line.parse::<i64>().map_err(|_| ToolkitError::AsciiFormat { line: *ptr - 1, message: format!("expected int, got {:?}", line) })
}

fn read_split<'a>(lines: &[&'a str], ptr: &mut usize) -> Result<Vec<&'a str>> {
    let line = lines.get(*ptr).ok_or_else(|| ToolkitError::AsciiFormat { line: *ptr, message: "unexpected EOF".into() })?;
    *ptr += 1;
    Ok(line.split_whitespace().collect())
}

fn read_line<'a>(lines: &[&'a str], ptr: &mut usize) -> Result<&'a str> {
    let line = lines.get(*ptr).ok_or_else(|| ToolkitError::AsciiFormat { line: *ptr, message: "unexpected EOF".into() })?;
    *ptr += 1;
    Ok(line)
}

fn parse_bones(lines: &[&str], ptr: &mut usize) -> Result<Vec<(String, i32, (f32, f32, f32))>> {
    let count = read_int(lines, ptr)? as usize;
    let mut bones = Vec::with_capacity(count);
    for _ in 0..count {
        let name = read_line(lines, ptr)?.to_string();
        let parent = read_int(lines, ptr)? as i32;
        let parts = read_split(lines, ptr)?;
        let x = parts.get(0).and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
        let y = parts.get(1).and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
        let z = parts.get(2).and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
        // 4-6 are quaternion, we skip for now (not needed for injection)
        bones.push((name, parent, (x, y, z)));
    }
    Ok(bones)
}

fn extract_nrm_tag(raw_line: &str) -> Option<u32> {
    if let Some(pos) = raw_line.find("#nrm:") {
        let hex = &raw_line[pos + 5..pos + 5 + 8.min(raw_line.len() - pos - 5)];
        u32::from_str_radix(hex.trim(), 16).ok()
    } else {
        None
    }
}

fn parse_meshes(lines: &[&str], raw_lines: &[&str], ptr: &mut usize, bones_count: usize) -> Result<Vec<AsciiMesh>> {
    let count = read_int(lines, ptr)? as usize;
    let mut meshes = Vec::with_capacity(count);
    for _ in 0..count {
        let name = read_line(lines, ptr)?.to_string();
        let uv_layers = read_int(lines, ptr)? as usize;
        let textures = read_int(lines, ptr)? as usize;
        for _ in 0..(textures * 2) { *ptr += 1; }

        let vert_count = read_int(lines, ptr)? as usize;
        let mut vertexes = Vec::with_capacity(vert_count);
        for _ in 0..vert_count {
            let pos_p = read_split(lines, ptr)?;
            let nor_line_idx = *ptr;
            let nor_p = read_split(lines, ptr)?;
            let raw_normal = raw_lines.get(nor_line_idx).and_then(|l| extract_nrm_tag(l));
            let _col  = read_split(lines, ptr)?;

            let uv = if uv_layers > 0 {
                let uv_p = read_split(lines, ptr)?;
                for _ in 1..uv_layers { *ptr += 1; }
                Some((
                    uv_p.get(0).and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0),
                    uv_p.get(1).and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0),
                ))
            } else { None };

            let (groups, weights) = if bones_count > 0 {
                let gp = read_split(lines, ptr)?;
                let wp = read_split(lines, ptr)?;
                (
                    gp.iter().filter_map(|s| s.parse::<u8>().ok()).collect(),
                    wp.iter().filter_map(|s| s.parse::<f32>().ok()).collect(),
                )
            } else { (vec![], vec![]) };

            vertexes.push(AsciiVertex {
                position: (
                    pos_p.get(0).and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0),
                    pos_p.get(1).and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0),
                    pos_p.get(2).and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0),
                ),
                normal: (
                    nor_p.get(0).and_then(|s| s.parse::<f32>().ok()).unwrap_or(1.0),
                    nor_p.get(1).and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0),
                    nor_p.get(2).and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0),
                ),
                raw_normal, uv, groups, weights,
            });
        }

        let face_count = read_int(lines, ptr)? as usize;
        let mut faces = Vec::with_capacity(face_count);
        for _ in 0..face_count {
            let parts = read_split(lines, ptr)?;
            let a = parts.get(0).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            let b = parts.get(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            let c = parts.get(2).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            faces.push((a, b, c));
        }

        meshes.push(AsciiMesh { name, vertexes, faces });
    }
    Ok(meshes)
}

// Injector

pub fn inject_ascii(model: &mut ModelFile, ascii: &AsciiModel) -> Result<()> {
    clear_muscle_deformation(model);
    change_lod_distances(model);
    update_look_groups(model)?;

    let mesh_updates = inject_vertexes(model, ascii)?;
    update_meshes(model, &mesh_updates)?;

    Ok(())
}

fn clear_muscle_deformation(model: &mut ModelFile) {
    if let Some(data) = model.dat1.get_section_data(TAG_MUSCLEDEF).map(|d| d.to_vec()) {
        if data.len() >= 0x50 {
            let mut d = data;
            // struct.pack("<6i", 0, 0, 0, 0x40, 0x48, 0)
            d[0..24].fill(0);
            d[12..16].copy_from_slice(&0x40u32.to_le_bytes());
            d[16..20].copy_from_slice(&0x48u32.to_le_bytes());
            // struct.pack("<2q", -1, -1) at 0x40..0x50
            d[0x40..0x50].fill(0xFF);
            let _ = model.dat1.set_section_data(TAG_MUSCLEDEF, d);
        }
    }
}

fn change_lod_distances(model: &mut ModelFile) {
    if let Some(data) = model.dat1.get_section_data(TAG_BUILT).map(|d| d.to_vec()) {
        let mut d = data;
        let base = 0x34;
        for i in 0..5 {
            let offset = base + i * 4;
            if offset + 4 <= d.len() {
                d[offset..offset + 4].copy_from_slice(&4096.0f32.to_le_bytes());
            }
        }
        let _ = model.dat1.set_section_data(TAG_BUILT, d);
    }
}

fn update_look_groups(model: &mut ModelFile) -> Result<()> {
    let look_data = model.dat1.get_section_data(TAG_LOOK)
        .ok_or_else(|| ToolkitError::SectionNotFound(TAG_LOOK))?.to_vec();
    let mut look_sec = LookSection::parse(&look_data)?;

    // Collapse all LODs to LOD0 for look 0 only
    let lod = 0;
    if let Some(look) = look_sec.looks.get_mut(0) {
        if let Some(lod0) = look.lods.get(lod).copied() {
            for (i, l) in look.lods.iter_mut().enumerate() {
                if i != lod && (l.start != 0 || l.count != 0) {
                    l.start = lod0.start;
                    l.count = lod0.count;
                }
            }
        }
    }
    model.dat1.set_section_data(TAG_LOOK, look_sec.save())?;
    Ok(())
}

struct MeshUpdate {
    mesh_index: usize,
    vertex_start: u32,
    vertex_count: u32,
    index_start: u32,
    index_count: u32,
    first_skin_batch: u16,
    first_weight_index: u32,
    skin_batches_count: u16,
    force_relative: bool,
    /// false for untouched meshes — skip the flag-strip so their original flags are preserved
    strip_flags: bool,
}

fn parse_mesh_index_from_ascii_name(name: &str) -> Option<usize> {
    // Expected names include "smNN_...", but Blender edits may prepend prefixes
    // (for example "5_sm08_..."). Parse the first "sm<digits>" token anywhere.
    let bytes = name.as_bytes();
    for i in 0..bytes.len().saturating_sub(2) {
        if (bytes[i] == b's' || bytes[i] == b'S') && (bytes[i + 1] == b'm' || bytes[i + 1] == b'M') {
            let mut j = i + 2;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 2 {
                return name[i + 2..j].parse::<usize>().ok();
            }
        }
    }
    None
}

fn build_ascii_mesh_index_map(ascii: &AsciiModel, mesh_count: usize) -> Result<std::collections::HashMap<usize, usize>> {
    let mut ascii_by_index: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::with_capacity(ascii.meshes.len());
    for (ascii_i, mesh_ascii) in ascii.meshes.iter().enumerate() {
        let mesh_index = parse_mesh_index_from_ascii_name(&mesh_ascii.name).unwrap_or(ascii_i);
        if mesh_index >= mesh_count {
            return Err(ToolkitError::Parse(format!(
                "mesh '{}' resolved to index {} but model has only {} meshes",
                mesh_ascii.name,
                mesh_index,
                mesh_count
            )));
        }
        if let Some(prev_ascii_i) = ascii_by_index.insert(mesh_index, ascii_i) {
            return Err(ToolkitError::Parse(format!(
                "duplicate ASCII mesh mapping for model mesh #{}: '{}' and '{}'. \
                 Export must contain exactly one mesh per smNN index",
                mesh_index,
                ascii.meshes[prev_ascii_i].name,
                mesh_ascii.name
            )));
        }
    }
    Ok(ascii_by_index)
}

fn should_rebuild_skin(meshes: &[MeshDefinition], ascii: &AsciiModel) -> Result<bool> {
    for (i, mesh_data_ascii) in ascii.meshes.iter().enumerate() {
        let mesh_index = parse_mesh_index_from_ascii_name(&mesh_data_ascii.name).unwrap_or(i);
        let mesh = meshes.get(mesh_index).ok_or_else(|| {
            ToolkitError::Parse(format!(
                "mesh '{}' resolved to index {} but model has only {} meshes",
                mesh_data_ascii.name,
                mesh_index,
                meshes.len()
            ))
        })?;

        // Preserve original skin blobs unless topology changed on a skinned mesh.
        if mesh.is_skinned() && mesh.vertex_count as usize != mesh_data_ascii.vertexes.len() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn align_skin_data_16(skin_data: &mut Option<Vec<u8>>, cur_offset: &mut usize) -> Result<()> {
    let sd = skin_data.as_mut().ok_or_else(|| {
        ToolkitError::Parse("missing skin_data section while rebuilding skinned mesh".into())
    })?;
    let pad = (16 - (sd.len() & 0x0F)) & 0x0F;
    if pad != 0 {
        sd.resize(sd.len() + pad, 0);
    }
    *cur_offset = sd.len();
    Ok(())
}

fn inject_vertexes(model: &mut ModelFile, ascii: &AsciiModel) -> Result<Vec<MeshUpdate>> {
    let mesh_data = model.dat1.get_section_data(TAG_MESHES)
        .ok_or_else(|| ToolkitError::SectionNotFound(TAG_MESHES))?.to_vec();
    let meshes = MeshDefinition::parse_all(&mesh_data)?;

    let built_pos_scale: f32 = model.dat1.get_section_data(TAG_BUILT)
        .map(get_position_scale).unwrap_or(1.0 / 4096.0);

    let vert_data = model.dat1.get_section_data(TAG_VERTEXES)
        .ok_or_else(|| ToolkitError::SectionNotFound(TAG_VERTEXES))?.to_vec();
    let mut vert_sec = VertexesSection::parse_scaled(&vert_data, built_pos_scale)?;

    let idx_data = model.dat1.get_section_data(TAG_INDEXES)
        .ok_or_else(|| ToolkitError::SectionNotFound(TAG_INDEXES))?.to_vec();
    let mut idx_sec = IndexesSection::parse(&idx_data)?;

    let uv1_data: Option<Vec<u8>> = model.dat1.get_section_data(TAG_UV1).map(|d| d.to_vec());
    let mut uv1_sec: Option<Uv1Section> = uv1_data.as_deref().map(|d| Uv1Section::parse(d).ok()).flatten();

    // Built UV scale — same value writer uses, so round-trip is exact.
    let built_uv_scale: f32 = model.dat1.get_section_data(crate::tools::model_converter::sections::built::TAG_BUILT)
        .map(get_uv_scale).unwrap_or(1.0 / 16384.0);

    // Skin data
    let _has_skin_batch = model.dat1.get_section_data(TAG_SKIN_BATCH).is_some();
    let mut skin_data: Option<Vec<u8>> = model.dat1.get_section_data(TAG_SKIN_DATA).map(|d| d.to_vec());
    let mut skin_batches: Option<Vec<SkinBatch>> = model.dat1.get_section_data(TAG_SKIN_BATCH)
        .map(|d| SkinBatch::parse_all(d).ok()).flatten();
    let mut rcra_entries: Option<Vec<RcraSkinEntry>> = model.dat1.get_section_data(TAG_RCRA_SKIN)
        .map(|d| Some(RcraSkinEntry::parse_all(d))).flatten();
    let original_skin_data = skin_data.clone();
    let original_skin_batches = skin_batches.clone();
    let original_rcra_entries = rcra_entries.clone();
    let rebuild_skin = should_rebuild_skin(&meshes, ascii)?;
    let skin_batch_templates = if rebuild_skin { skin_batches.clone() } else { None };
    if rebuild_skin {
        if let Some(ref mut sd) = skin_data {
            sd.clear();
        }
        if let Some(ref mut sb) = skin_batches {
            sb.clear();
        }
        if let Some(ref mut re) = rcra_entries {
            re.clear();
        }
    }

    let mut cur_skin_batch = 0usize;
    let mut cur_skin_offset = 0usize;
    let mut cur_rcra_weight = 0usize;

    // Contiguous relayout pre-pass
    // Lay out every mesh contiguously at new positions: ASCII-injected meshes
    // use their ASCII sizes, untouched meshes keep their original sizes.
    // A grown mesh's added tail gets its own space instead of overflowing into
    // the next mesh. Untouched meshes' bytes are copied verbatim into the new
    // buffer — their per-mesh data stays identical; only their `vertex_start`
    // pointer shifts. (The old global-repack glitch came from not copying
    // untouched-mesh bytes into the new layout; see HANDOFF_mesh_glitch.md.)
    let n_meshes = meshes.len();
    let ascii_by_index = build_ascii_mesh_index_map(ascii, n_meshes)?;
    let mut new_vstart = vec![0u32; n_meshes];
    let mut new_istart = vec![0u32; n_meshes];
    let mut force_relative = vec![false; n_meshes];
    let orig_vertexes_for_raw_w: Vec<Vertex>;
    {
        let mut cv: u64 = 0;
        let mut ci: u64 = 0;
        for mi in 0..n_meshes {
            new_vstart[mi] = u32::try_from(cv)
                .map_err(|_| ToolkitError::Parse(format!("vertex_start overflow for mesh {}", mi)))?;
            new_istart[mi] = u32::try_from(ci)
                .map_err(|_| ToolkitError::Parse(format!("index_start overflow for mesh {}", mi)))?;
            let (vc, ic) = if let Some(&ai) = ascii_by_index.get(&mi) {
                let am = &ascii.meshes[ai];
                (am.vertexes.len() as u64, (am.faces.len() * 3) as u64)
            } else {
                (meshes[mi].vertex_count as u64, meshes[mi].index_count as u64)
            };
            cv += vc;
            ci += ic;
        }
        for mi in 0..n_meshes {
            if meshes[mi].has_relative_indices() {
                continue;
            }
            let vc = if let Some(&ai) = ascii_by_index.get(&mi) {
                ascii.meshes[ai].vertexes.len() as u64
            } else {
                meshes[mi].vertex_count as u64
            };
            if vc == 0 {
                continue;
            }
            let max_abs_index = new_vstart[mi] as u64 + vc - 1;
            if max_abs_index > u16::MAX as u64 {
                force_relative[mi] = true;
                eprintln!(
                    "[inject_ascii] mesh #{} absolute indices would overflow (max_abs_index={}) -> forcing relative indices",
                    mi,
                    max_abs_index
                );
            }
        }
        let new_vert_total = cv as usize;
        let new_idx_total = ci as usize;
        let mut new_vertexes = vec![Vertex::zero(); new_vert_total];
        let mut new_indices = vec![0u16; new_idx_total];
        let mut new_uvs: Option<Vec<(i16, i16)>> =
            uv1_sec.as_ref().map(|_| vec![(0i16, 0i16); new_vert_total]);
        // Copy untouched meshes' slices from originals into the new layout.
        // ASCII meshes' slots are left zeroed; the per-mesh loop below fills
        // them from ASCII data.
        for mi in 0..n_meshes {
            if ascii_by_index.contains_key(&mi) { continue; }
            let om = &meshes[mi];
            let old_vs = om.vertex_start as usize;
            let old_vc = om.vertex_count as usize;
            let nvs = new_vstart[mi] as usize;
            let copy_vc = old_vc
                .min(vert_sec.vertexes.len().saturating_sub(old_vs))
                .min(new_vertexes.len().saturating_sub(nvs));
            new_vertexes[nvs..nvs + copy_vc]
                .clone_from_slice(&vert_sec.vertexes[old_vs..old_vs + copy_vc]);
            if let (Some(ref mut nu), Some(ref uv)) = (new_uvs.as_mut(), uv1_sec.as_ref()) {
                let copy_uv = copy_vc.min(uv.uvs.len().saturating_sub(old_vs));
                nu[nvs..nvs + copy_uv].clone_from_slice(&uv.uvs[old_vs..old_vs + copy_uv]);
            }
            let old_is = om.index_start as usize;
            let old_ic = om.index_count as usize;
            let nis = new_istart[mi] as usize;
            let copy_ic = old_ic
                .min(idx_sec.values.len().saturating_sub(old_is))
                .min(new_indices.len().saturating_sub(nis));
            if om.has_relative_indices() {
                new_indices[nis..nis + copy_ic]
                    .clone_from_slice(&idx_sec.values[old_is..old_is + copy_ic]);
            } else if force_relative[mi] {
                // Convert absolute source indices into relative indices.
                for k in 0..copy_ic {
                    let v = idx_sec.values[old_is + k] as i64 - old_vs as i64;
                    if !(0..=u16::MAX as i64).contains(&v) {
                        return Err(ToolkitError::Parse(format!(
                            "relative index conversion out of range for mesh {}: {}",
                            mi,
                            v
                        )));
                    }
                    new_indices[nis + k] = v as u16;
                }
            } else {
                // Absolute indices reference mesh.vertex_start — shift them by
                // (new_vs - old_vs) so they keep pointing at this mesh's verts
                // in the new layout.
                let shift: i64 = nvs as i64 - old_vs as i64;
                for k in 0..copy_ic {
                    let v = idx_sec.values[old_is + k] as i64 + shift;
                    if !(0..=u16::MAX as i64).contains(&v) {
                        return Err(ToolkitError::Parse(format!(
                            "index overflow for mesh {} while shifting absolute indices: {}",
                            mi,
                            v
                        )));
                    }
                    new_indices[nis + k] = v as u16;
                }
            }
        }
        // Snapshot originals so ASCII mesh writes below can still read raw_w
        // from the mesh's vanilla slot. After the swap below, vert_sec holds
        // the new buffer (untouched meshes already populated, ASCII slots
        // zeroed pending the per-mesh loop).
        orig_vertexes_for_raw_w = std::mem::take(&mut vert_sec.vertexes);
        vert_sec.vertexes = new_vertexes;
        idx_sec.values = new_indices;
        if let (Some(uv), Some(nu)) = (uv1_sec.as_mut(), new_uvs) {
            uv.uvs = nu;
        }
    }

    let mut updates = Vec::with_capacity(ascii.meshes.len());

    for (i, mesh_data_ascii) in ascii.meshes.iter().enumerate() {
        let mesh_index = parse_mesh_index_from_ascii_name(&mesh_data_ascii.name).unwrap_or(i);
        let mesh = meshes.get(mesh_index).ok_or_else(|| {
            ToolkitError::Parse(format!(
                "mesh '{}' resolved to index {} but model has only {} meshes",
                mesh_data_ascii.name,
                mesh_index,
                meshes.len()
            ))
        })?;
        eprintln!(
            "[inject_ascii] mesh '{}' -> model mesh #{} (verts={}, faces={})",
            mesh_data_ascii.name,
            mesh_index,
            mesh_data_ascii.vertexes.len(),
            mesh_data_ascii.faces.len()
        );
        let has_skin = mesh.is_skinned();
        let has_rcra_skin = mesh.is_rcra_skinned();
        // Rebuild skin payload only for meshes whose topology changed.
        // For unchanged-count meshes, preserve original skin bytes to avoid
        // introducing deformation from editor-side weight re-normalization.
        let mesh_skin_changed = has_skin
            && mesh.vertex_count as usize != mesh_data_ascii.vertexes.len();
        // Positions come from the contiguous-relayout pre-pass. This is
        // critical for grown meshes — their added tail would otherwise
        // overflow into the next mesh's slot and get clobbered.
        let orig_vertex_start = mesh.vertex_start as usize;
        let orig_vertex_count = mesh.vertex_count as usize;
        let vertex_start = new_vstart[mesh_index];
        let index_start  = new_istart[mesh_index];
        let mut cur_vertex = vertex_start as usize;
        let mut cur_index = index_start as usize;
        let first_skin_batch = if rebuild_skin {
            u16::try_from(cur_skin_batch).map_err(|_| {
                ToolkitError::Parse(format!(
                    "too many skin batches while rebuilding '{}' (index {})",
                    mesh_data_ascii.name,
                    mesh_index
                ))
            })?
        } else {
            mesh.first_skin_batch
        };
        // Python uses vertex_start for first_weight_index on all RCRA models
        let first_weight_index = if rebuild_skin {
            if has_rcra_skin {
                u32::try_from(cur_rcra_weight).map_err(|_| {
                    ToolkitError::Parse(format!(
                        "rcra weight index overflow while rebuilding '{}' (index {})",
                        mesh_data_ascii.name,
                        mesh_index
                    ))
                })?
            } else {
                mesh.first_weight_index
            }
        } else {
            mesh.first_weight_index
        };

        // Start skin batch tracking
        let batch_vertex_count = mesh_data_ascii.vertexes.len();
        let mut batch_vertex_index = 0usize;
        let mut sum_batch_vertex_index = 0usize;
        let mut fallback_weight_count = 0usize;
        let mut fallback_reused_prev_count = 0usize;
        let mut last_valid_weights: Option<(Vec<u8>, Vec<f32>)> = None;
        let mut next_mesh_template_index = 1usize;
        let mesh_batch_templates = if has_skin {
            let start_batch = mesh.first_skin_batch as usize;
            let batch_count = mesh.skin_batches_count as usize;
            original_skin_batches
                .as_ref()
                .and_then(|all| all.get(start_batch..start_batch + batch_count))
        } else {
            None
        };
        if has_skin && rebuild_skin {
            if skin_data.is_none() || skin_batches.is_none() {
                return Err(ToolkitError::Parse(format!(
                    "mesh '{}' is skinned but model is missing skin_data/skin_batch sections",
                    mesh_data_ascii.name
                )));
            }
            if has_rcra_skin && rcra_entries.is_none() {
                return Err(ToolkitError::Parse(format!(
                    "mesh '{}' uses rcra skin but model is missing rcra_skin section",
                    mesh_data_ascii.name
                )));
            }
            if mesh_skin_changed {
                align_skin_data_16(&mut skin_data, &mut cur_skin_offset)?;
                if let Some(ref mut batches) = skin_batches {
                    let mut batch = mesh_batch_templates
                        .and_then(|templates| templates.get(0))
                        .cloned()
                        .or_else(|| {
                            skin_batch_templates
                                .as_ref()
                                .and_then(|templates| templates.get(cur_skin_batch))
                                .cloned()
                        })
                        .unwrap_or_default();
                    batch.offset = cur_skin_offset as u32;
                    batch.vertex_count = 0;
                    batch.first_vertex = 0;
                    // rebuilt skin batches get unk1=0
                    batch.unk1 = 0;
                    batches.push(batch);
                }
            } else {
                let orig_skin_data_ref = original_skin_data.as_ref().ok_or_else(|| {
                    ToolkitError::Parse(format!(
                        "mesh '{}' is skinned but source model has no skin_data",
                        mesh_data_ascii.name
                    ))
                })?;
                let orig_skin_batches_ref = original_skin_batches.as_ref().ok_or_else(|| {
                    ToolkitError::Parse(format!(
                        "mesh '{}' is skinned but source model has no skin_batch",
                        mesh_data_ascii.name
                    ))
                })?;

                let start_batch = mesh.first_skin_batch as usize;
                let batch_count = mesh.skin_batches_count as usize;
                let end_batch = start_batch + batch_count;
                if end_batch > orig_skin_batches_ref.len() {
                    return Err(ToolkitError::Parse(format!(
                        "mesh '{}' skin batch range out of bounds: {}..{} of {}",
                        mesh_data_ascii.name,
                        start_batch,
                        end_batch,
                        orig_skin_batches_ref.len()
                    )));
                }

                if let Some(ref mut batches) = skin_batches {
                    for bi in start_batch..end_batch {
                        let orig_batch = &orig_skin_batches_ref[bi];
                        let start = orig_batch.offset as usize;
                        let end = if bi + 1 < orig_skin_batches_ref.len() {
                            orig_skin_batches_ref[bi + 1].offset as usize
                        } else {
                            orig_skin_data_ref.len()
                        };
                        if start > end || end > orig_skin_data_ref.len() {
                            return Err(ToolkitError::Parse(format!(
                                "mesh '{}' has invalid skin_data slice for batch {}: {}..{} of {}",
                                mesh_data_ascii.name,
                                bi,
                                start,
                                end,
                                orig_skin_data_ref.len()
                            )));
                        }

                        align_skin_data_16(&mut skin_data, &mut cur_skin_offset)?;
                        let sd = skin_data.as_mut().ok_or_else(|| {
                            ToolkitError::Parse("missing skin_data section while rebuilding skinned mesh".into())
                        })?;

                        let mut copied_batch = orig_batch.clone();
                        copied_batch.offset = cur_skin_offset as u32;
                        batches.push(copied_batch);

                        sd.extend_from_slice(&orig_skin_data_ref[start..end]);
                        cur_skin_offset = sd.len();
                        cur_skin_batch += 1;
                    }
                }

                if has_rcra_skin {
                    let orig_rcra_entries_ref = original_rcra_entries.as_ref().ok_or_else(|| {
                        ToolkitError::Parse(format!(
                            "mesh '{}' uses rcra skin but source model has no rcra_skin",
                            mesh_data_ascii.name
                        ))
                    })?;
                    let start = mesh.first_weight_index as usize;
                    let end = start + mesh.vertex_count as usize;
                    if end > orig_rcra_entries_ref.len() {
                        return Err(ToolkitError::Parse(format!(
                            "mesh '{}' rcra weight range out of bounds: {}..{} of {}",
                            mesh_data_ascii.name,
                            start,
                            end,
                            orig_rcra_entries_ref.len()
                        )));
                    }
                    if let Some(ref mut entries) = rcra_entries {
                        entries.extend_from_slice(&orig_rcra_entries_ref[start..end]);
                    }
                    cur_rcra_weight += mesh.vertex_count as usize;
                }
            }
        }

        let mut weights_group: Vec<(Vec<u8>, Vec<f32>)> = Vec::new();

        let vertex_count = mesh_data_ascii.vertexes.len();
        for (vi, av) in mesh_data_ascii.vertexes.iter().enumerate() {
            // Write vertex
            let mut new_v = Vertex::zero();
            // raw_w is read from the mesh's ORIGINAL vanilla slot (not from
            // the swapped-in buffer, which is zeroed at ASCII-mesh positions).
            // Added tail vertices (vi >= orig_vertex_count) have no vanilla
            // counterpart and keep raw_w=0.
            if vi < orig_vertex_count {
                let src = orig_vertex_start + vi;
                if src < orig_vertexes_for_raw_w.len() {
                    new_v.raw_w = orig_vertexes_for_raw_w[src].raw_w;
                }
            }
            new_v.x = av.position.0;
            new_v.y = av.position.1;
            new_v.z = av.position.2;
            new_v.nx = av.normal.0;
            new_v.ny = av.normal.1;
            new_v.nz = av.normal.2;
            new_v.raw_normal = av.raw_normal;
            if let Some((u, v)) = av.uv {
                let ru = (u / built_uv_scale).round() as i16;
                let rv = (v / built_uv_scale).round() as i16;
                new_v.u = ru as f32 / 32768.0;
                new_v.v = rv as f32 / 32768.0;
                if i == 0 && vi < 3 {
                    eprintln!(
                        "[inject_ascii] uv sample mesh#{} v{}: ascii=({:.6},{:.6}) raw=({}, {}) vertex_uv=({:.6},{:.6}) scale={:.8}",
                        mesh_index,
                        vi,
                        u,
                        v,
                        ru,
                        rv,
                        new_v.u,
                        new_v.v,
                        built_uv_scale
                    );
                }
                if let Some(ref mut uv1) = uv1_sec {
                    if cur_vertex < uv1.uvs.len() {
                        uv1.uvs[cur_vertex] = (ru, rv);
                    }
                }
            }
            if cur_vertex < vert_sec.vertexes.len() {
                vert_sec.vertexes[cur_vertex] = new_v;
            }

            // Skin weights
            if has_skin && rebuild_skin && mesh_skin_changed {
                let (mut w, used_fallback) = normalize_weights(&av.groups, &av.weights);
                if used_fallback {
                    fallback_weight_count += 1;
                    if let Some(prev) = last_valid_weights.clone() {
                        w = prev;
                        fallback_reused_prev_count += 1;
                    }
                } else {
                    last_valid_weights = Some(w.clone());
                }
                if has_rcra_skin {
                    if let Some(ref mut entries) = rcra_entries {
                        let mut bs = [1u8, 0, 0, 0];
                        let mut ws = [255u8, 0, 0, 0];
                        for j in 0..w.0.len().min(4) {
                            bs[j] = w.0[j];
                            ws[j] = (w.1.get(j).copied().unwrap_or(0.0) * 256.0).clamp(0.0, 255.0) as u8;
                        }
                        entries.push(RcraSkinEntry { bones: bs, weights: ws });
                    }
                    cur_rcra_weight += 1;
                }
                weights_group.push(w);

                if weights_group.len() == 16 || (vi == vertex_count - 1 && !weights_group.is_empty()) {
                    flush_weights_group(
                        &mut weights_group, &mut skin_data, &mut cur_skin_offset,
                        &mut skin_batches, &mut cur_skin_batch,
                        &mut batch_vertex_index, &mut sum_batch_vertex_index, batch_vertex_count,
                        mesh_batch_templates,
                        &mut next_mesh_template_index,
                        skin_batch_templates.as_deref(),
                    )?;
                }
            }

            cur_vertex += 1;
        }

        if has_skin && rebuild_skin && mesh_skin_changed {
            eprintln!(
                "[inject_ascii] mesh #{} fallback_weights={}/{} reused_prev={} unresolved={}",
                mesh_index,
                fallback_weight_count,
                vertex_count,
                fallback_reused_prev_count,
                fallback_weight_count.saturating_sub(fallback_reused_prev_count)
            );
        }

        // Write faces
        let use_relative_indices = mesh.has_relative_indices() || force_relative[mesh_index];
        let vc_offset = if use_relative_indices { 0 } else { vertex_start };
        for face in &mesh_data_ascii.faces {
            if cur_index + 2 < idx_sec.values.len() {
                idx_sec.values[cur_index + 0] = (face.2 + vc_offset) as u16;
                idx_sec.values[cur_index + 1] = (face.1 + vc_offset) as u16;
                idx_sec.values[cur_index + 2] = (face.0 + vc_offset) as u16;
            }
            cur_index += 3;
        }

        let skin_batches_count = if has_skin {
            if rebuild_skin {
                let cur_skin_batch_u16 = u16::try_from(cur_skin_batch).map_err(|_| {
                    ToolkitError::Parse(format!(
                        "too many skin batches while rebuilding '{}' (index {})",
                        mesh_data_ascii.name,
                        mesh_index
                    ))
                })?;
                cur_skin_batch_u16 - first_skin_batch
            } else {
                mesh.skin_batches_count
            }
        } else {
            0
        };

        updates.push(MeshUpdate {
            mesh_index,
            vertex_start,
            vertex_count: mesh_data_ascii.vertexes.len() as u32,
            index_start,
            index_count: (mesh_data_ascii.faces.len() * 3) as u32,
            first_skin_batch,
            first_weight_index,
            skin_batches_count,
            force_relative: force_relative[mesh_index],
            strip_flags: true,
        });
    }

    // When rebuild_skin is true, copy skin data for meshes NOT present in the
    // ASCII. Without this, untouched meshes (e.g. claws, LODs) lose their
    // skinning and render with displaced/glitched vertices.
    if rebuild_skin {
        let injected_set: std::collections::HashSet<usize> = updates.iter().map(|u| u.mesh_index).collect();
        for (mi, mesh) in meshes.iter().enumerate() {
            if injected_set.contains(&mi) { continue; }
            if !mesh.is_skinned() { continue; }

            let new_first_skin_batch = cur_skin_batch as u16;
            let mut new_skin_batches_count = 0u16;
            let mut new_first_weight_index = mesh.first_weight_index;

            if let (Some(ref orig_sd), Some(ref orig_sb)) =
                (original_skin_data.as_ref(), original_skin_batches.as_ref())
            {
                let start_batch = mesh.first_skin_batch as usize;
                let batch_count = mesh.skin_batches_count as usize;
                let end_batch = start_batch + batch_count;
                if end_batch <= orig_sb.len() {
                    if let Some(ref mut batches) = skin_batches {
                        for bi in start_batch..end_batch {
                            let orig_batch = &orig_sb[bi];
                            let start = orig_batch.offset as usize;
                            let end = if bi + 1 < orig_sb.len() {
                                orig_sb[bi + 1].offset as usize
                            } else {
                                orig_sd.len()
                            };
                            if start <= end && end <= orig_sd.len() {
                                align_skin_data_16(&mut skin_data, &mut cur_skin_offset)?;
                                let sd = skin_data.as_mut().ok_or_else(|| {
                                    ToolkitError::Parse("missing skin_data section while rebuilding skinned mesh".into())
                                })?;

                                let mut copied_batch = orig_batch.clone();
                                copied_batch.offset = cur_skin_offset as u32;
                                batches.push(copied_batch);
                                sd.extend_from_slice(&orig_sd[start..end]);
                                cur_skin_offset = sd.len();
                                cur_skin_batch += 1;
                                new_skin_batches_count += 1;
                            }
                        }
                    }
                }
            }

            if mesh.is_rcra_skinned() {
                if let Some(ref orig_re) = original_rcra_entries {
                    let start = mesh.first_weight_index as usize;
                    let end = start + mesh.vertex_count as usize;
                    if end <= orig_re.len() {
                        if let Some(ref mut entries) = rcra_entries {
                            new_first_weight_index = entries.len() as u32;
                            entries.extend_from_slice(&orig_re[start..end]);
                        }
                    }
                }
            }

            // Add an update so update_meshes fixes the skin batch pointers
            // for this untouched mesh. vertex_start/index_start come from the
            // contiguous-relayout pre-pass — untouched mesh bytes were copied
            // into the new layout above, so mesh.vertex_start is now stale.
            updates.push(MeshUpdate {
                mesh_index: mi,
                vertex_start: new_vstart[mi],
                vertex_count: mesh.vertex_count,
                index_start: new_istart[mi],
                index_count: mesh.index_count,
                first_skin_batch: new_first_skin_batch,
                first_weight_index: new_first_weight_index,
                skin_batches_count: new_skin_batches_count,
                force_relative: force_relative[mi],
                strip_flags: false,
            });
        }
    }

    // Compute per-vertex tangent + bitangent for every mesh we just touched.
    // Must run *before* vert_sec.save(), because Vertex::save_rcra() reads the
    // computed tangents and packs them into the top bits of the normal u32 and
    // the i16 W field.
    calculate_tangents(&mut vert_sec, uv1_sec.as_ref(), &idx_sec, &updates, &meshes, built_uv_scale);

    // Refresh BUILT total vertex/index counts to match the rebuilt sections.
    // The game validates section sizes against these — if we grow VERTEXES /
    // INDEXES and leave BUILT pointing at vanilla counts, every mesh renders
    // broken (not just the grown ones).
    if let Some(built) = model.dat1.get_section_data(TAG_BUILT).map(|d| d.to_vec()) {
        let mut built = built;
        crate::tools::model_converter::sections::built::set_counts(
            &mut built,
            vert_sec.vertexes.len() as u32,
            idx_sec.values.len() as u32,
        );
        model.dat1.set_section_data(TAG_BUILT, built)?;
    }

    // Save back all modified sections
    model.dat1.set_section_data(TAG_VERTEXES, vert_sec.save_scaled(built_pos_scale))?;
    model.dat1.set_section_data(TAG_INDEXES, idx_sec.save())?;
    if let Some(ref uv1) = uv1_sec {
        model.dat1.set_section_data(TAG_UV1, uv1.save())?;
    }
    if rebuild_skin {
        if let Some(sd) = skin_data {
            model.dat1.set_section_data(TAG_SKIN_DATA, sd)?;
        }
        if let Some(sb) = skin_batches {
            model.dat1.set_section_data(TAG_SKIN_BATCH, SkinBatch::save_all(&sb))?;
        }
        if let Some(re) = rcra_entries {
            model.dat1.set_section_data(TAG_RCRA_SKIN, RcraSkinEntry::save_all(&re))?;
        }
    }

    Ok(updates)
}

fn normalize_weights(groups: &[u8], weights: &[f32]) -> ((Vec<u8>, Vec<f32>), bool) {
    let mut ng: Vec<u8> = Vec::new();
    let mut nw: Vec<f32> = Vec::new();
    let mut sum = 0.0f32;
    for (&g, &w) in groups.iter().zip(weights.iter()) {
        if sum >= 1.0 { break; }
        if w == 0.0 { continue; }
        ng.push(g); nw.push(w); sum += w;
    }
    if sum > 1.0 { nw.iter_mut().for_each(|w| *w /= sum); }
    let used_fallback = ng.is_empty();
    if used_fallback { ng.push(0); nw.push(1.0); }
    ((ng, nw), used_fallback)
}

fn flush_weights_group(
    group: &mut Vec<(Vec<u8>, Vec<f32>)>,
    skin_data: &mut Option<Vec<u8>>,
    cur_offset: &mut usize,
    skin_batches: &mut Option<Vec<SkinBatch>>,
    cur_batch: &mut usize,
    batch_vertex_index: &mut usize,
    sum_batch_vertex_index: &mut usize,
    batch_vertex_count: usize,
    mesh_batch_templates: Option<&[SkinBatch]>,
    next_mesh_template_index: &mut usize,
    skin_batch_templates: Option<&[SkinBatch]>,
) -> Result<()> {
    if group.is_empty() { return Ok(()); }
    let max_groups = group.iter().map(|w| w.0.len()).max().unwrap_or(1);

    let sd = skin_data.as_mut().ok_or_else(|| {
        ToolkitError::Parse("missing skin_data section while rebuilding skinned mesh".into())
    })?;
    sd.push((max_groups - 1) as u8);
    for w in group.iter() {
        if max_groups == 1 {
            sd.push(*w.0.first().unwrap_or(&1));
        } else {
            let mut bone_ids = vec![0u8; max_groups];
            let mut iweights = vec![0i32; max_groups];
            bone_ids[0] = *w.0.first().unwrap_or(&1);
            iweights[0] = 256;
            for k in 1..w.0.len().min(max_groups) {
                bone_ids[k] = w.0[k];
                let bw = (w.1.get(k).copied().unwrap_or(0.0) * 256.0).clamp(0.0, 255.0).round() as i32;
                iweights[k] = bw;
                iweights[0] -= bw;
            }
            if iweights[0] < 0 {
                iweights[0] = 0;
            } else if iweights[0] > 255 {
                iweights[0] = 255;
                iweights[1] = 1;
                bone_ids[1] = bone_ids[0];
            }
            for k in 0..max_groups {
                if k > 0 && iweights[k] == 0 { bone_ids[k] = bone_ids[k - 1]; }
                sd.push(bone_ids[k]);
                sd.push(iweights[k] as u8);
            }
        }
        *batch_vertex_index += 1;
    }
    *cur_offset = sd.len();

    group.clear();

    // End current sub-batch and start next when all mesh verts are processed or sub-batch is full
    let finished_mesh = *sum_batch_vertex_index + *batch_vertex_index == batch_vertex_count;
    if finished_mesh || *batch_vertex_index == 2560 {
        let batches = skin_batches.as_mut().ok_or_else(|| {
            ToolkitError::Parse("missing skin_batch section while rebuilding skinned mesh".into())
        })?;
        if *cur_batch >= batches.len() {
            return Err(ToolkitError::Parse(format!(
                "internal skin_batch indexing error: batch {} out of {}",
                *cur_batch,
                batches.len()
            )));
        }

        let vertex_count_u16 = u16::try_from(*batch_vertex_index).map_err(|_| {
            ToolkitError::Parse(format!(
                "skin batch vertex_count overflow: {}",
                *batch_vertex_index
            ))
        })?;
        let first_vertex_u16 = u16::try_from(*sum_batch_vertex_index).map_err(|_| {
            ToolkitError::Parse(format!(
                "skin batch first_vertex overflow: {}",
                *sum_batch_vertex_index
            ))
        })?;

        batches[*cur_batch].vertex_count = vertex_count_u16;
        batches[*cur_batch].first_vertex = first_vertex_u16;
        *cur_batch += 1;

        *sum_batch_vertex_index += *batch_vertex_index;
        *batch_vertex_index = 0;

        if !finished_mesh {
            align_skin_data_16(skin_data, cur_offset)?;
            let mut next_batch = mesh_batch_templates
                .and_then(|templates| templates.get(*next_mesh_template_index))
                .cloned()
                .or_else(|| {
                    batches
                        .get(*cur_batch - 1)
                        .cloned()
                })
                .or_else(|| {
                    skin_batch_templates
                        .and_then(|templates| templates.get(*cur_batch))
                        .cloned()
                })
                .unwrap_or_default();
            next_batch.offset = *cur_offset as u32;
            next_batch.vertex_count = 0;
            next_batch.first_vertex = 0;
            // See initial-batch push: rebuilt continuation batches use
            // unk1=0
            next_batch.unk1 = 0;
            batches.push(next_batch);
            *next_mesh_template_index += 1;
        }
    }

    Ok(())
}

/// Compute per-vertex tangent + bitangent from triangle positions and UVs,
/// accumulating across all faces of every mesh we re-wrote. The results are
/// stored on each `Vertex` so `save_rcra()` packs them into the nxyz u32 + W
/// i16 fields (the actual on-disk tangent encoding for RCRA vertices).
fn calculate_tangents(
    vert_sec: &mut VertexesSection,
    uv1_sec: Option<&Uv1Section>,
    idx_sec: &IndexesSection,
    updates: &[MeshUpdate],
    meshes: &[MeshDefinition],
    built_uv_scale: f32,
) {
    let n = vert_sec.vertexes.len();
    if n == 0 { return; }
    let mut tangents = vec![[0.0f64; 3]; n];
    let mut bitangents = vec![[0.0f64; 3]; n];

    let uv_scale = built_uv_scale as f64;

    for u in updates {
        // Skip untouched meshes copied verbatim from vanilla — their tangents
        // are already baked into the preserved vertex bytes, and recomputing
        // here would overwrite them with values that don't match Insomniac's
        // baked tangents (causing normal-map glitches on shoulder pads/claws).
        if !u.strip_flags { continue; }
        let mesh = meshes.get(u.mesh_index);
        let relative = u.force_relative || mesh.map(|m| m.has_relative_indices()).unwrap_or(true);
        let vs = u.vertex_start as usize;
        let is = u.index_start as usize;
        let ic = u.index_count as usize;
        let vc_offset: usize = if relative { 0 } else { vs };

        let uv_at = |abs_vi: usize| -> (f64, f64) {
            if let Some(uv1) = uv1_sec {
                if abs_vi < uv1.uvs.len() {
                    let (ru, rv) = uv1.uvs[abs_vi];
                    return (ru as f64 * uv_scale, rv as f64 * uv_scale);
                }
            }
            if abs_vi < vert_sec.vertexes.len() {
                let v = &vert_sec.vertexes[abs_vi];
                (v.u as f64, v.v as f64)
            } else {
                (0.0, 0.0)
            }
        };

        let face_count = ic / 3;
        for f in 0..face_count {
            let base = is + f * 3;
            if base + 2 >= idx_sec.values.len() { break; }
            // Faces were written as (face.2, face.1, face.0) in injection, so
            // use reverse order when walking them.
            let i0 = idx_sec.values[base + 2] as usize;
            let i1 = idx_sec.values[base + 1] as usize;
            let i2 = idx_sec.values[base + 0] as usize;
            let a = vs + i0.wrapping_sub(vc_offset);
            let b = vs + i1.wrapping_sub(vc_offset);
            let c = vs + i2.wrapping_sub(vc_offset);
            if a >= n || b >= n || c >= n { continue; }

            let v0 = &vert_sec.vertexes[a];
            let v1 = &vert_sec.vertexes[b];
            let v2 = &vert_sec.vertexes[c];
            let p0 = [v0.x as f64, v0.y as f64, v0.z as f64];
            let p1 = [v1.x as f64, v1.y as f64, v1.z as f64];
            let p2 = [v2.x as f64, v2.y as f64, v2.z as f64];
            let (u0, w0) = uv_at(a);
            let (u1, w1) = uv_at(b);
            let (u2, w2) = uv_at(c);

            let e1 = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
            let e2 = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
            let du1 = u1 - u0;
            let dv1 = w1 - w0;
            let du2 = u2 - u0;
            let dv2 = w2 - w0;
            let d = du1 * dv2 - du2 * dv1;
            if d.abs() < 1e-20 { continue; }
            let r = 1.0 / d;

            let t = [
                (dv2 * e1[0] - dv1 * e2[0]) * r,
                (dv2 * e1[1] - dv1 * e2[1]) * r,
                (dv2 * e1[2] - dv1 * e2[2]) * r,
            ];
            let bt = [
                (-du2 * e1[0] + du1 * e2[0]) * r,
                (-du2 * e1[1] + du1 * e2[1]) * r,
                (-du2 * e1[2] + du1 * e2[2]) * r,
            ];

            for idx in [a, b, c] {
                tangents[idx][0] += t[0];
                tangents[idx][1] += t[1];
                tangents[idx][2] += t[2];
                bitangents[idx][0] += bt[0];
                bitangents[idx][1] += bt[1];
                bitangents[idx][2] += bt[2];
            }
        }
    }

    // Store on every vertex we touched. We also clear raw_normal for those
    // vertices so save_rcra falls through to the tangent-aware encoder.
    for u in updates {
        let vs = u.vertex_start as usize;
        let vc = u.vertex_count as usize;
        for i in vs..(vs + vc).min(n) {
            vert_sec.vertexes[i].tangent = Some((
                tangents[i][0] as f32,
                tangents[i][1] as f32,
                tangents[i][2] as f32,
            ));
            vert_sec.vertexes[i].bitangent = Some((
                bitangents[i][0] as f32,
                bitangents[i][1] as f32,
                bitangents[i][2] as f32,
            ));
            // If the ASCII provided a verbatim raw normal (#nrm tag), we
            // trust it over the recomputed tangent. Otherwise drop it so the
            // tangent-aware encoder runs in save_rcra.
            if vert_sec.vertexes[i].raw_normal.is_none() {
                // already None -- keep it so encoder picks the tangent path
            }
        }
    }
}

fn update_meshes(model: &mut ModelFile, updates: &[MeshUpdate]) -> Result<()> {
    let mesh_data = model.dat1.get_section_data(TAG_MESHES)
        .ok_or_else(|| ToolkitError::SectionNotFound(TAG_MESHES))?.to_vec();
    let mut meshes = MeshDefinition::parse_all(&mesh_data)?;

    for u in updates.iter() {
        if let Some(m) = meshes.get_mut(u.mesh_index) {
            m.vertex_start       = u.vertex_start;
            m.vertex_count       = u.vertex_count;
            m.index_start        = u.index_start;
            m.index_count        = u.index_count;
            m.first_skin_batch   = u.first_skin_batch;
            m.skin_batches_count = u.skin_batches_count;
            if m.is_rcra_skinned() {
                m.first_weight_index = u.first_weight_index;
            }
            if u.strip_flags { m.flags = m.flags & 0x111; }
            if u.force_relative { m.flags |= 0x10; }
            eprintln!(
                "[inject_ascii] updated mesh #{}: v_start={} v_count={} i_start={} i_count={} first_weight={} skin_batches={}",
                u.mesh_index,
                u.vertex_start,
                u.vertex_count,
                u.index_start,
                u.index_count,
                u.first_weight_index,
                u.skin_batches_count
            );
        }
    }

    model.dat1.set_section_data(TAG_MESHES, MeshDefinition::save_all(&meshes))?;
    Ok(())
}


