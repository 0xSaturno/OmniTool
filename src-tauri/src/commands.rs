use std::path::{Path, PathBuf};
use std::time::Instant;

use log::{debug, info, warn};
use walkdir::WalkDir;

use crate::core::config::ConfigFile;
use crate::core::dat1::Dat1;
use crate::core::error::ToolkitError;
use crate::core::filesystem;
use crate::core::toc::Toc;
use crate::tools::model_converter::{
    ascii_reader::{inject_ascii, parse_ascii},
    ascii_writer::model_to_ascii_for_looks as do_model_to_ascii_for_looks,
    model::ModelFile,
    sections::{
        look::{LookSection, TAG_LOOK},
        meshes::{MeshDefinition, TAG_MESHES},
    },
};
use crate::tools::texture_converter::{extract_texture, replace_texture, get_texture_info, get_dds_info, get_texture_preview, get_dds_preview, TextureInfo};

#[tauri::command]
pub async fn tauri_get_texture_info(path: String) -> Result<TextureInfo, ToolkitError> {
    get_texture_info(&path).map_err(|e| ToolkitError::Parse(e))
}

#[tauri::command]
pub async fn tauri_get_dds_info(path: String) -> Result<TextureInfo, ToolkitError> {
    get_dds_info(&path).map_err(|e| ToolkitError::Parse(e))
}

#[tauri::command]
pub async fn tauri_get_texture_preview(path: String) -> Result<String, ToolkitError> {
    get_texture_preview(&path).map_err(|e| ToolkitError::Parse(e))
}

#[tauri::command]
pub async fn tauri_get_dds_preview(path: String) -> Result<String, ToolkitError> {
    get_dds_preview(&path).map_err(|e| ToolkitError::Parse(e))
}

#[tauri::command]
pub async fn tauri_extract_texture(
    source_path: String,
    output_dir: Option<String>,
) -> Result<String, ToolkitError> {
    extract_texture(&source_path, output_dir, None)
        .map_err(|e| ToolkitError::Parse(e))
}

#[tauri::command]
pub async fn tauri_replace_texture(
    source_path: String,
    dds_path: String,
    output_dir: Option<String>,
    ignore_format: bool,
) -> Result<String, ToolkitError> {
    replace_texture(&source_path, &dds_path, output_dir, ignore_format, None, None, None)
        .map_err(|e| ToolkitError::Parse(e))
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct TextureJob {
    pub base_name: String,
    pub sd_path: Option<String>,
    pub hd_path: Option<String>,
    pub dds_path: Option<String>,
}

#[tauri::command]
pub async fn tauri_scan_stager_textures(
    project_dir: String,
    dds_dir: Option<String>,
) -> Result<Vec<TextureJob>, ToolkitError> {
    use std::collections::HashMap;

    #[derive(Default)]
    struct JobEntry {
        textures: Vec<PathBuf>,
        dds: Option<PathBuf>,
    }

    let mut map: HashMap<String, JobEntry> = HashMap::new();

    for entry in WalkDir::new(&project_dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file() {
            let name_lower = path.to_string_lossy().to_lowercase();
            if name_lower.ends_with(".hd.texture") {
                let stem = path.file_stem().unwrap().to_string_lossy().to_lowercase().replace(".hd", "");
                map.entry(stem).or_default().textures.push(path.to_path_buf());
            } else if name_lower.ends_with(".texture") {
                let stem = path.file_stem().unwrap().to_string_lossy().to_lowercase();
                map.entry(stem).or_default().textures.push(path.to_path_buf());
            } else if name_lower.ends_with(".dds") && dds_dir.is_none() {
                if name_lower.contains(".a") && name_lower.len() > 6 {
                    let parts: Vec<&str> = name_lower.split('.').collect();
                    if parts.len() >= 3 && parts[parts.len()-2].starts_with('a') {
                        continue;
                    }
                }
                let stem = path.file_stem().unwrap().to_string_lossy().to_lowercase();
                map.entry(stem).or_default().dds = Some(path.to_path_buf());
            }
        }
    }

    if let Some(dir) = dds_dir {
        for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_file() {
                let name_lower = path.to_string_lossy().to_lowercase();
                if name_lower.ends_with(".dds") {
                    if name_lower.contains(".a") && name_lower.len() > 6 {
                        let parts: Vec<&str> = name_lower.split('.').collect();
                        if parts.len() >= 3 && parts[parts.len()-2].starts_with('a') {
                            continue;
                        }
                    }
                    let stem = path.file_stem().unwrap().to_string_lossy().to_lowercase();
                    map.entry(stem).or_default().dds = Some(path.to_path_buf());
                }
            }
        }
    }

    let mut jobs = Vec::new();
    for (base_name, entry) in map {
        let mut sd_path = None;
        let mut hd_path = None;

        for t in entry.textures {
            if t.to_string_lossy().to_lowercase().ends_with(".hd.texture") {
                hd_path = Some(t.to_string_lossy().into_owned());
            } else {
                if sd_path.is_none() {
                    sd_path = Some(t.to_string_lossy().into_owned());
                } else {
                    let size1 = std::fs::metadata(sd_path.as_ref().unwrap()).map(|m| m.len()).unwrap_or(0);
                    let size2 = std::fs::metadata(&t).map(|m| m.len()).unwrap_or(0);
                    if size2 > size1 {
                        hd_path = Some(t.to_string_lossy().into_owned());
                    } else {
                        hd_path = sd_path;
                        sd_path = Some(t.to_string_lossy().into_owned());
                    }
                }
            }
        }

        let mut rel_base = base_name.clone();
        if let Some(ref sd) = sd_path {
            let sd_path_obj = Path::new(sd);
            if let Ok(rel) = sd_path_obj.strip_prefix(&project_dir) {
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                if rel_str.to_lowercase().ends_with(".texture") {
                    rel_base = rel_str[..rel_str.len() - 8].to_string();
                } else {
                    rel_base = rel_str;
                }
            }
        }

        jobs.push(TextureJob {
            base_name: rel_base,
            sd_path,
            hd_path,
            dds_path: entry.dds.map(|p| p.to_string_lossy().into_owned()),
        });
    }

    jobs.sort_by(|a, b| a.base_name.cmp(&b.base_name));
    Ok(jobs)
}

#[tauri::command]
pub async fn tauri_batch_replace_textures(
    jobs: Vec<TextureJob>,
    output_dir: Option<String>,
    ignore_format: bool,
    project_dir: String,
) -> Result<String, ToolkitError> {
    let mut success_count = 0;
    let mut errors = Vec::new();

    for job in jobs {
        if job.sd_path.is_none() || job.dds_path.is_none() {
            continue;
        }
        let sd = job.sd_path.unwrap();
        let dds = job.dds_path.unwrap();
        let hd = job.hd_path.as_deref();

        let mut explicit_out_sd = None;
        let mut explicit_out_hd = None;
        
        let out_sd_path;
        let out_hd_path;

        if let Some(ref out_dir) = output_dir {
            let base_path = Path::new(&project_dir);
            let sd_relative = Path::new(&sd).strip_prefix(base_path).unwrap_or(Path::new(""));
            out_sd_path = Path::new(out_dir).join(sd_relative);
            if let Some(parent) = out_sd_path.parent() {
                std::fs::create_dir_all(parent).unwrap_or(());
            }
            explicit_out_sd = Some(out_sd_path.to_str().unwrap().to_string());

            if let Some(ref hd_p) = job.hd_path {
                let hd_relative = Path::new(hd_p).strip_prefix(base_path).unwrap_or(Path::new(""));
                out_hd_path = Path::new(out_dir).join(hd_relative);
                if let Some(parent) = out_hd_path.parent() {
                    std::fs::create_dir_all(parent).unwrap_or(());
                }
                explicit_out_hd = Some(out_hd_path.to_str().unwrap().to_string());
            }
        } else {
            explicit_out_sd = Some(sd.clone());
            explicit_out_hd = job.hd_path.clone();
        }

        match replace_texture(
            &sd,
            &dds,
            None,
            ignore_format,
            hd,
            explicit_out_sd.as_deref(),
            explicit_out_hd.as_deref(),
        ) {
            Ok(_) => success_count += 1,
            Err(e) => errors.push(format!("{}: {}", job.base_name, e)),
        }
    }

    if errors.is_empty() {
        Ok(format!("Successfully replaced {} textures.", success_count))
    } else {
        Ok(format!("Replaced {} textures. Errors:\n{}", success_count, errors.join("\n")))
    }
}

#[tauri::command]
pub async fn tauri_batch_extract_textures(
    jobs: Vec<TextureJob>,
    output_dir: Option<String>,
    project_dir: String,
) -> Result<String, ToolkitError> {
    let mut success_count = 0;
    let mut errors = Vec::new();

    for job in jobs {
        if job.sd_path.is_none() {
            continue;
        }
        let sd = job.sd_path.unwrap();
        
        let out_dds_path;
        let explicit_out_dir;

        if let Some(ref out_dir) = output_dir {
            let base_path = Path::new(&project_dir);
            let sd_relative = Path::new(&sd).strip_prefix(base_path).unwrap_or(Path::new(""));
            out_dds_path = Path::new(out_dir).join(sd_relative).with_extension("dds");
            if let Some(parent) = out_dds_path.parent() {
                std::fs::create_dir_all(parent).unwrap_or(());
                explicit_out_dir = parent.to_str().map(|s| s.to_string());
            } else {
                explicit_out_dir = out_dds_path.to_str().map(|s| s.to_string());
            }
        } else {
            // If no output dir, we pass None to let extract_texture use default path next to the file
            explicit_out_dir = None;
        }

        match extract_texture(&sd, explicit_out_dir, job.hd_path.as_deref()) {
            Ok(_) => success_count += 1,
            Err(e) => errors.push(format!("{}: {}", job.base_name, e)),
        }
    }

    if errors.is_empty() {
        Ok(format!("Successfully extracted {} textures.", success_count))
    } else {
        Ok(format!("Extracted {} textures. Errors:\n{}", success_count, errors.join("\n")))
    }
}


#[tauri::command]
pub async fn model_to_ascii(
    model_path: String,
    ascii_path: Option<String>,
    look: Option<usize>,
    looks: Option<Vec<usize>>,
) -> Result<String, ToolkitError> {
    let start = Instant::now();
    eprintln!("[model_to_ascii] loading model from {}", model_path);
    let model_data = std::fs::read(&model_path)?;
    let model = ModelFile::parse(&model_data)?;
    
    let selected_looks: Vec<usize> = if let Some(ls) = looks {
        if ls.is_empty() {
            vec![look.unwrap_or(0)]
        } else {
            let mut unique = std::collections::BTreeSet::new();
            for l in ls {
                unique.insert(l);
            }
            unique.into_iter().collect()
        }
    } else {
        vec![look.unwrap_or(0)]
    };
    eprintln!("[model_to_ascii] converting model to ASCII (looks={:?})", selected_looks);
    let ascii = do_model_to_ascii_for_looks(&model, &selected_looks)?;

    let out_path = ascii_path
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let base = Path::new(&model_path);
            base.with_extension("ascii")
        });

    std::fs::write(&out_path, &ascii)?;
    eprintln!("[model_to_ascii] saved ASCII to {} in {:?}", out_path.display(), start.elapsed());
    Ok(out_path.to_string_lossy().into_owned())
}

#[derive(serde::Serialize)]
pub struct LookGroupInfo {
    pub index: usize,
    pub name: String,
}

#[tauri::command]
pub async fn list_model_lookgroups(model_path: String) -> Result<Vec<LookGroupInfo>, ToolkitError> {
    let model_data = std::fs::read(&model_path)?;
    let model = ModelFile::parse(&model_data)?;

    // Determine how many look groups exist from TAG_LOOK (visibility ranges).
    let look_count = model
        .dat1
        .get_section_data(TAG_LOOK)
        .map(|d| LookSection::parse(d))
        .transpose()?
        .map(|ls| ls.looks.len())
        .unwrap_or(0);

    // Resolve names from TAG_LOOK_BUILT (ModelLookBuilt) when present.
    let mut names: Vec<String> = Vec::new();
    if let Some(lb_data) = model.dat1.get_section_data(TAG_LOOK_BUILT) {
        if lb_data.len() >= 4 {
            let size1 = u32::from_le_bytes(lb_data[0..4].try_into().unwrap()) as usize;
            let region_end = size1.min(lb_data.len());
            let entry_size = 80;
            let n = region_end / entry_size;
            for i in 0..n {
                let base = i * entry_size;
                let string_off =
                    u32::from_le_bytes(lb_data[base + 76..base + 80].try_into().unwrap());
                let name = model.dat1.get_string(string_off).unwrap_or_default();
                names.push(name);
            }
        }
    }

    let total = look_count.max(names.len());
    let mut out = Vec::with_capacity(total);
    for i in 0..total {
        let name = names.get(i).cloned().unwrap_or_else(|| format!("look_{i}"));
        out.push(LookGroupInfo { index: i, name });
    }
    Ok(out)
}

#[tauri::command]
pub async fn ascii_to_model(
    ascii_path: String,
    src_model_path: String,
    out_path: Option<String>,
) -> Result<String, ToolkitError> {
    let start = Instant::now();
    eprintln!("[ascii_to_model] loading ascii from {}", ascii_path);
    let ascii_text = std::fs::read_to_string(&ascii_path)?;
    let ascii = parse_ascii(&ascii_text)?;
    eprintln!("[ascii_to_model] parsed ascii ({} meshes, {} bones)", ascii.meshes.len(), ascii.bones.len());

    eprintln!("[ascii_to_model] loading source model from {}", src_model_path);
    let model_data = std::fs::read(&src_model_path)?;
    let mut model = ModelFile::parse(&model_data)?;

    eprintln!("[ascii_to_model] injecting ascii data into model");
    inject_ascii(&mut model, &ascii)?;

    let output_path = out_path
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let base = Path::new(&src_model_path);
            let stem = base.file_stem().unwrap_or_default().to_string_lossy();
            let ext = base.extension().unwrap_or_default().to_string_lossy();
            base.with_file_name(format!("{}_modified.{}", stem, ext))
        });

    let out_bytes = model.save();
    std::fs::write(&output_path, &out_bytes)?;
    eprintln!("[ascii_to_model] saved modified model to {} in {:?}", output_path.display(), start.elapsed());
    Ok(output_path.to_string_lossy().into_owned())
}

// ===========================================================================
// Material Editor
// ===========================================================================

#[tauri::command]
pub async fn read_model_materials(model_path: String) -> Result<ModelMaterialData, ToolkitError> {
    eprintln!("[material_remapper] opening {}", model_path);
    let model_data = std::fs::read(&model_path)?;
    let model = ModelFile::parse(&model_data)?;

    // --- Materials (RCRA / i29 layout) ---
    // Section has TWO halves, each `count * 16` bytes:
    //   First half:  `count` × (u64 matfile_off, u64 matname_off) pairs
    //   Second half: `count` × (u64 crc64, u32 crc32_name, u32 ?) triples
    // String offsets are u64 but always fit in u32 in practice (strings pool is small).
    let mat_data = model
        .dat1
        .get_section_data(TAG_MATERIALS)
        .ok_or_else(|| ToolkitError::SectionNotFound(TAG_MATERIALS))?;
    const ENTRY_SIZE: usize = 16;
    let mat_count = mat_data.len() / 2 / ENTRY_SIZE;
    let mut materials = Vec::with_capacity(mat_count);
    for i in 0..mat_count {
        let base = i * ENTRY_SIZE;
        let matfile_off = u64::from_le_bytes(mat_data[base..base + 8].try_into().unwrap()) as u32;
        let matname_off =
            u64::from_le_bytes(mat_data[base + 8..base + 16].try_into().unwrap()) as u32;
        let path = model.dat1.get_string(matfile_off).unwrap_or_default();
        let name = model.dat1.get_string(matname_off).unwrap_or_default();
        materials.push(MaterialSlotInfo {
            index: i,
            path,
            name,
        });
    }

    // --- Look names (ModelLookBuilt section) ---
    // Section layout: leading u32 `size1` = byte length of the LookBuilt array,
    // followed by entries of 80 bytes each. Per-entry tail (bytes 64..80) is
    // (u32 count, u32 crc32_orig, u32 crc32_lower, u32 string_off).
    let mut look_names: Vec<String> = Vec::new();
    if let Some(lb_data) = model.dat1.get_section_data(TAG_LOOK_BUILT) {
        if lb_data.len() >= 4 {
            let size1 = u32::from_le_bytes(lb_data[0..4].try_into().unwrap()) as usize;
            let region_end = size1.min(lb_data.len());
            let entry_size = 80;
            let n = region_end / entry_size;
            for i in 0..n {
                let base = i * entry_size;
                let string_off =
                    u32::from_le_bytes(lb_data[base + 76..base + 80].try_into().unwrap());
                let name = model.dat1.get_string(string_off).unwrap_or_default();
                look_names.push(name);
            }
        }
    }

    // --- Meshes ---
    let mesh_data = model
        .dat1
        .get_section_data(TAG_MESHES)
        .ok_or_else(|| ToolkitError::SectionNotFound(TAG_MESHES))?;
    let mesh_defs = MeshDefinition::parse_all(mesh_data)?;

    // --- Looks (visibility ranges) ---
    let look_section = model
        .dat1
        .get_section_data(TAG_LOOK)
        .map(|d| LookSection::parse(d))
        .transpose()?;

    let mut submeshes = Vec::with_capacity(mesh_defs.len());
    for (i, m) in mesh_defs.iter().enumerate() {
        let mut look_indices = Vec::new();
        if let Some(ref ls) = look_section {
            for (li, look) in ls.looks.iter().enumerate() {
                for lod in &look.lods {
                    let start = lod.start as usize;
                    let end = start + lod.count as usize;
                    if i >= start && i < end {
                        look_indices.push(li);
                        break;
                    }
                }
            }
        }
        submeshes.push(SubmeshInfo {
            index: i,
            material_index: m.material_index,
            vertex_count: m.vertex_count,
            face_count: m.index_count / 3,
            look_indices,
        });
    }

    eprintln!(
        "[material_remapper] read: {} materials, {} submeshes, {} looks from {}",
        materials.len(),
        submeshes.len(),
        look_names.len(),
        model_path
    );
    Ok(ModelMaterialData {
        materials,
        submeshes,
        look_names,
    })
}

#[tauri::command]
pub async fn save_model_materials(
    model_path: String,
    materials: Vec<MaterialEdit>,
    out_path: Option<String>,
) -> Result<String, ToolkitError> {
    use crate::core::crc64;

    let model_data = std::fs::read(&model_path)?;
    let mut model = ModelFile::parse(&model_data)?;

    // RCRA MATERIALS layout: first half is `count` × 16-byte (u64 matfile_off, u64 matname_off)
    // pairs, second half is `count` × 16-byte (u64 crc64, u32 crc32, u32 ?) triples.
    let mat_data = model
        .dat1
        .get_section_data(TAG_MATERIALS)
        .ok_or_else(|| ToolkitError::SectionNotFound(TAG_MATERIALS))?;
    const ENTRY_SIZE: usize = 16;
    let count = mat_data.len() / 2 / ENTRY_SIZE;

    // Preserve every byte so we only patch the fields we need.
    let mut pairs: Vec<[u8; 16]> = Vec::with_capacity(count);
    let mut triples: Vec<[u8; 16]> = Vec::with_capacity(count);
    for i in 0..count {
        let pb = i * ENTRY_SIZE;
        let tb = count * ENTRY_SIZE + i * ENTRY_SIZE;
        let mut p = [0u8; 16];
        p.copy_from_slice(&mat_data[pb..pb + 16]);
        let mut t = [0u8; 16];
        t.copy_from_slice(&mat_data[tb..tb + 16]);
        pairs.push(p);
        triples.push(t);
    }

    let header_end = model.dat1.header_end() as u64;

    for edit in &materials {
        if edit.index >= count {
            return Err(ToolkitError::Parse(format!(
                "material index {} out of range (model has {})",
                edit.index, count
            )));
        }

        // Append new path string to the pool and get its absolute offset.
        let new_off_in_pool = model.dat1.strings_pool.len() as u64;
        model
            .dat1
            .strings_pool
            .extend_from_slice(edit.path.as_bytes());
        model.dat1.strings_pool.push(0);
        let new_matfile_off = header_end + new_off_in_pool;

        // Patch matfile_off (first u64 of the pair).
        pairs[edit.index][0..8].copy_from_slice(&new_matfile_off.to_le_bytes());

        // Update crc64 (first u64 of the corresponding triple) so the game's
        // lookup-by-hash still resolves to the new material path.
        let new_crc = crc64::hash(&edit.path);
        triples[edit.index][0..8].copy_from_slice(&new_crc.to_le_bytes());

        eprintln!(
            "[material_remapper] slot {} -> {:?} (crc64={:016X}, off={})",
            edit.index, edit.path, new_crc, new_matfile_off
        );
    }

    // Reassemble: all pairs first, then all triples — preserves original layout.
    let mut new_mat_data = Vec::with_capacity(count * ENTRY_SIZE * 2);
    for p in &pairs {
        new_mat_data.extend_from_slice(p);
    }
    for t in &triples {
        new_mat_data.extend_from_slice(t);
    }
    model.dat1.set_section_data(TAG_MATERIALS, new_mat_data)?;

    let output_path = out_path
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let base = Path::new(&model_path);
            let stem = base.file_stem().unwrap_or_default().to_string_lossy();
            let ext = base.extension().unwrap_or_default().to_string_lossy();
            base.with_file_name(format!("{}_matmod.{}", stem, ext))
        });

    let out_bytes = model.save();
    std::fs::write(&output_path, &out_bytes)?;
    eprintln!(
        "[material_remapper] saved {} edit(s) to {}",
        materials.len(),
        output_path.display()
    );
    Ok(output_path.to_string_lossy().into_owned())
}

// ===========================================================================
// Filesystem / Setup
// ===========================================================================

#[tauri::command]
pub async fn get_app_dir() -> Result<String, ToolkitError> {
    let dir = filesystem::app_dir()?;
    info!("get_app_dir: {}", dir.display());
    Ok(dir.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn get_hashes_path() -> Result<String, ToolkitError> {
    let path = filesystem::hashes_path()?;
    let exists = path.exists();
    info!("get_hashes_path: {} (exists={})", path.display(), exists);
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn hashes_exist() -> Result<bool, ToolkitError> {
    Ok(filesystem::hashes_path()?.exists())
}

#[tauri::command]
pub async fn load_hashes() -> Result<Vec<(String, String)>, ToolkitError> {
    let start = Instant::now();
    let path = filesystem::hashes_path()?;
    eprintln!("[asset_browser] reading hashes from {}", path.display());
    let text = std::fs::read_to_string(&path)?;

    let mut hashes = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, ',');
        let Some(hex_str) = parts.next() else {
            continue;
        };
        let Some(path_str) = parts.next() else {
            continue;
        };
        // Validate hex but keep as string to avoid u64 precision loss in JS
        if u64::from_str_radix(hex_str, 16).is_err() {
            debug!("load_hashes: skipping malformed hex {:?}", hex_str);
            continue;
        }
        hashes.push((hex_str.to_uppercase(), path_str.to_string()));
    }

    eprintln!(
        "[asset_browser] loaded {} hashes in {:?}",
        hashes.len(),
        start.elapsed()
    );
    Ok(hashes)
}

// ===========================================================================
// TOC / Asset Browser
// ===========================================================================

#[derive(serde::Serialize)]
pub struct TocInfo {
    pub asset_count: usize,
    pub archive_count: usize,
    pub archive_names: Vec<String>,
    pub span_count: usize,
}

#[derive(serde::Serialize)]
pub struct AssetInfo {
    pub id: String,
    pub archive_index: u32,
    pub offset: u32,
    pub size: u32,
    pub span: u8,
}

const TAG_MATERIALS: u32 = 0x3250BB80;
const TAG_LOOK_BUILT: u32 = 0x811902D7;

#[derive(serde::Serialize)]
pub struct MaterialSlotInfo {
    pub index: usize,
    pub path: String,
    pub name: String,
}

#[derive(serde::Serialize)]
pub struct SubmeshInfo {
    pub index: usize,
    pub material_index: u16,
    pub vertex_count: u32,
    pub face_count: u32,
    pub look_indices: Vec<usize>,
}

#[derive(serde::Serialize)]
pub struct ModelMaterialData {
    pub materials: Vec<MaterialSlotInfo>,
    pub submeshes: Vec<SubmeshInfo>,
    pub look_names: Vec<String>,
}

#[derive(serde::Deserialize)]
pub struct MaterialEdit {
    pub index: usize,
    pub path: String,
}

#[tauri::command]
pub async fn load_toc(toc_path: String) -> Result<TocInfo, ToolkitError> {
    let start = Instant::now();
    eprintln!("[asset_browser] reading TOC {}", toc_path);
    let data = std::fs::read(&toc_path)?;
    let toc = Toc::parse(&data)?;

    let assets = toc.assets();
    let archives = toc.archive_filenames();
    let info = TocInfo {
        asset_count: assets.len(),
        archive_count: archives.len(),
        span_count: assets
            .iter()
            .map(|a| a.span_index)
            .max()
            .map_or(0, |m| m as usize + 1),
        archive_names: archives,
    };

    eprintln!(
        "[asset_browser] TOC loaded: {} assets, {} archives in {:?}",
        info.asset_count,
        info.archive_count,
        start.elapsed()
    );
    Ok(info)
}

#[tauri::command]
pub async fn list_toc_assets(toc_path: String) -> Result<Vec<AssetInfo>, ToolkitError> {
    let start = Instant::now();
    eprintln!("[asset_browser] listing assets from {}", toc_path);
    let data = std::fs::read(&toc_path)?;
    let toc = Toc::parse(&data)?;

    let assets: Vec<AssetInfo> = toc
        .assets()
        .into_iter()
        .map(|a| AssetInfo {
            id: format!("{:016X}", a.asset_id),
            archive_index: a.archive_index,
            offset: a.offset,
            size: a.size,
            span: a.span_index,
        })
        .collect();

    eprintln!(
        "[asset_browser] returning {} assets in {:?}",
        assets.len(),
        start.elapsed()
    );
    Ok(assets)
}

#[tauri::command]
pub async fn extract_asset_to_project(
    toc_path: String,
    asset_id: String,
    archives_dir: String,
    project_name: String,
    asset_path: String,
) -> Result<String, ToolkitError> {
    let start = Instant::now();
    info!(
        "extract_asset_to_project: asset={} project={} path={}",
        asset_id, project_name, asset_path
    );

    let id = u64::from_str_radix(&asset_id, 16)
        .map_err(|e| ToolkitError::Parse(format!("invalid asset id hex: {e}")))?;

    let data = std::fs::read(&toc_path)?;
    let toc = Toc::parse(&data)?;

    // Collect ALL spans for this asset (e.g. span 0 = SD, span 1 = HD for textures).
    let matching: Vec<_> = toc
        .assets()
        .into_iter()
        .filter(|a| a.asset_id == id)
        .collect();
    if matching.is_empty() {
        return Err(ToolkitError::Parse(format!(
            "asset {asset_id} not found in TOC"
        )));
    }

    let project_base = filesystem::projects_dir()?.join(&project_name);
    let mut summary = Vec::new();

    for asset in &matching {
        let raw = toc.extract_asset(asset, Path::new(&archives_dir))?;
        // Each span lives in its own subfolder: 0/ for SD, 1/ for HD, etc.
        let out_path = project_base
            .join(asset.span_index.to_string())
            .join(&asset_path);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out_path, &raw)?;
        summary.push(format!("{} (span {}→{}B)", asset_path, asset.span_index, raw.len()));
        debug!(
            "extract_asset_to_project: span {} wrote {} bytes to {}",
            asset.span_index,
            raw.len(),
            out_path.display()
        );
    }

    info!(
        "extract_asset_to_project: {} span(s) in {:?}",
        matching.len(),
        start.elapsed()
    );
    Ok(summary.join(", "))
}

#[tauri::command]
pub async fn extract_asset_to_path(
    toc_path: String,
    asset_id: String,
    archives_dir: String,
    output_dir: String,
    asset_path: String,
) -> Result<String, ToolkitError> {
    let id = u64::from_str_radix(&asset_id, 16)
        .map_err(|e| ToolkitError::Parse(format!("invalid asset id hex: {e}")))?;

    let data = std::fs::read(&toc_path)?;
    let toc = Toc::parse(&data)?;

    let matching: Vec<_> = toc
        .assets()
        .into_iter()
        .filter(|a| a.asset_id == id)
        .collect();
    if matching.is_empty() {
        return Err(ToolkitError::Parse(format!(
            "asset {asset_id} not found in TOC"
        )));
    }

    let base = PathBuf::from(output_dir);
    let mut summary = Vec::new();

    for asset in &matching {
        let raw = toc.extract_asset(asset, Path::new(&archives_dir))?;
        let out_path = base
            .join(asset.span_index.to_string())
            .join(&asset_path);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out_path, &raw)?;
        summary.push(format!(
            "{} (span {}→{}B)",
            out_path.display(),
            asset.span_index,
            raw.len()
        ));
    }

    Ok(summary.join(", "))
}

#[tauri::command]
pub async fn extract_asset_as_dds(
    toc_path: String,
    asset_id: String,
    archives_dir: String,
    output_dir: String,
    asset_path: String,
) -> Result<String, ToolkitError> {
    let id = u64::from_str_radix(&asset_id, 16)
        .map_err(|e| ToolkitError::Parse(format!("invalid asset id hex: {e}")))?;

    let data = std::fs::read(&toc_path)?;
    let toc = Toc::parse(&data)?;

    let matching: Vec<_> = toc
        .assets()
        .into_iter()
        .filter(|a| a.asset_id == id)
        .collect();
    if matching.is_empty() {
        return Err(ToolkitError::Parse(format!(
            "asset {asset_id} not found in TOC"
        )));
    }

    let temp_dir = std::env::temp_dir().join("omnitool_dds_export");
    std::fs::create_dir_all(&temp_dir)?;

    let base_name = Path::new(&asset_path).file_name().unwrap().to_string_lossy().to_string();
    let sd_path = temp_dir.join(&base_name);
    let mut hd_path = sd_path.clone();
    hd_path.set_file_name(base_name.replace(".texture", ".hd.texture"));

    for asset in &matching {
        let raw = toc.extract_asset(asset, Path::new(&archives_dir))?;
        if asset.span_index == 0 {
            std::fs::write(&sd_path, &raw)?;
        } else if asset.span_index == 1 {
            std::fs::write(&hd_path, &raw)?;
        }
    }

    let hd_explicit = if hd_path.exists() {
        Some(hd_path.to_string_lossy().to_string())
    } else {
        None
    };
    let result = extract_texture(&sd_path.to_string_lossy(), Some(output_dir), hd_explicit.as_deref())
        .map_err(|e| ToolkitError::Parse(e))?;

    let _ = std::fs::remove_file(&sd_path);
    let _ = std::fs::remove_file(&hd_path);

    Ok(result)
}

// ===========================================================================
// Stager / Project Management
// ===========================================================================

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ProjectInfo {
    pub name: String,
    pub game: String,
    pub author: String,
    #[serde(default = "default_version")]
    pub version: String,
}

fn default_version() -> String {
    "1.0.0".to_string()
}

#[tauri::command]
pub async fn create_project(
    name: String,
    game: String,
    author: String,
) -> Result<String, ToolkitError> {
    let project_dir = filesystem::projects_dir()?.join(&name);
    let content_dir = project_dir.join("0");
    std::fs::create_dir_all(&content_dir)?;

    let info = ProjectInfo {
        name: name.clone(),
        game,
        author,
        version: "1.0.0".to_string(),
    };
    let json = serde_json::to_string_pretty(&info)
        .map_err(|e| ToolkitError::Parse(format!("failed to serialize info.json: {e}")))?;
    std::fs::write(project_dir.join("info.json"), &json)?;

    eprintln!(
        "[stager] created project {:?} at {}",
        name,
        project_dir.display()
    );
    Ok(project_dir.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn list_projects() -> Result<Vec<ProjectInfo>, ToolkitError> {
    let dir = filesystem::projects_dir()?;
    eprintln!("[stager] scanning projects in {}", dir.display());
    let mut projects = Vec::new();

    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let info_path = entry.path().join("info.json");
        match std::fs::read_to_string(&info_path) {
            Ok(text) => match serde_json::from_str::<ProjectInfo>(&text) {
                Ok(info) => projects.push(info),
                Err(e) => warn!(
                    "list_projects: invalid info.json in {:?}: {e}",
                    entry.path()
                ),
            },
            Err(e) => warn!("list_projects: no info.json in {:?}: {e}", entry.path()),
        }
    }

    eprintln!("[stager] found {} projects", projects.len());
    Ok(projects)
}

#[tauri::command]
pub async fn delete_project(name: String) -> Result<(), ToolkitError> {
    let project_dir = filesystem::projects_dir()?.join(&name);
    if !project_dir.exists() {
        return Err(ToolkitError::Parse(format!(
            "project {:?} does not exist",
            name
        )));
    }
    std::fs::remove_dir_all(&project_dir)?;
    eprintln!("[stager] deleted project {:?}", name);
    Ok(())
}

#[tauri::command]
pub async fn list_project_assets(name: String) -> Result<Vec<String>, ToolkitError> {
    let project_dir = filesystem::projects_dir()?.join(&name);
    eprintln!(
        "[stager] listing assets for project {:?} ({})",
        name,
        project_dir.display()
    );
    if !project_dir.exists() {
        return Ok(Vec::new());
    }

    let mut paths = Vec::new();
    for entry in WalkDir::new(&project_dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            if let Ok(rel) = entry.path().strip_prefix(&project_dir) {
                paths.push(rel.to_string_lossy().into_owned().replace('\\', "/"));
            }
        }
    }

    eprintln!("[stager] found {} assets in project {:?}", paths.len(), name);
    Ok(paths)
}

#[tauri::command]
pub async fn export_stage(name: String, output_path: String) -> Result<String, ToolkitError> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    let start = Instant::now();
    let project_dir = filesystem::projects_dir()?.join(&name);
    if !project_dir.exists() {
        return Err(ToolkitError::Parse(format!(
            "project {:?} does not exist",
            name
        )));
    }

    let file = std::fs::File::create(&output_path)?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let map_zip = |e: zip::result::ZipError| ToolkitError::Parse(format!("zip error: {e}"));

    // Write info.json at root
    let info_bytes = std::fs::read(project_dir.join("info.json"))?;
    zip.start_file("info.json", options).map_err(map_zip)?;
    zip.write_all(&info_bytes)?;

    // Walk the entire project dir so all span subfolders (0/, 1/, …) are included.
    for entry in WalkDir::new(&project_dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        let rel = path.strip_prefix(&project_dir).unwrap();
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        // Skip the root itself and info.json (already written above).
        if rel_str.is_empty() || rel_str == "info.json" {
            continue;
        }

        if entry.file_type().is_dir() {
            zip.add_directory(&rel_str, options).map_err(map_zip)?;
        } else {
            let data = std::fs::read(path)?;
            zip.start_file(&rel_str, options).map_err(map_zip)?;
            zip.write_all(&data)?;
        }
    }

    zip.finish().map_err(map_zip)?;
    eprintln!(
        "[stager] exported project {:?} to {} in {:?}",
        name,
        output_path,
        start.elapsed()
    );

    // Open the folder and select the newly created package
    let _ = std::process::Command::new("explorer")
        .arg("/select,")
        .arg(&output_path)
        .spawn();

    Ok(output_path)
}

#[tauri::command]
pub async fn compute_crc64(input: String) -> Result<String, ToolkitError> {
    let hash = crate::core::crc64::hash(&input);
    debug!("compute_crc64: {:?} -> {:016X}", input, hash);
    Ok(format!("{:016X}", hash))
}

#[tauri::command]
pub fn get_project_path(name: String) -> Result<String, ToolkitError> {
    let path = filesystem::projects_dir()?.join(&name);
    debug!("get_project_path: {:?} -> {}", name, path.display());
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
pub fn open_project_in_explorer(name: String) -> Result<(), ToolkitError> {
    let dir = filesystem::projects_dir()?.join(&name);
    info!("open_project_in_explorer: {}", dir.display());
    #[cfg(target_os = "windows")]
    std::process::Command::new("explorer")
        .arg(&dir)
        .spawn()
        .map_err(|e| ToolkitError::Parse(e.to_string()))?;
    Ok(())
}

#[tauri::command]
pub fn update_project_version(name: String, version: String) -> Result<(), ToolkitError> {
    let info_path = filesystem::projects_dir()?.join(&name).join("info.json");
    if !info_path.exists() {
        return Err(ToolkitError::Parse(format!(
            "project {:?} info.json missing",
            name
        )));
    }

    let text = std::fs::read_to_string(&info_path)?;
    let mut info: ProjectInfo =
        serde_json::from_str(&text).map_err(|e| ToolkitError::Parse(e.to_string()))?;

    info.version = version.clone();
    let new_json =
        serde_json::to_string_pretty(&info).map_err(|e| ToolkitError::Parse(e.to_string()))?;

    std::fs::write(&info_path, new_json)?;
    eprintln!("[stager] updated project {:?} version to {:?}", name, version);
    Ok(())
}

#[tauri::command]
pub fn rename_project_asset(
    name: String,
    old_path: String,
    new_path: String,
) -> Result<(), ToolkitError> {
    let base = filesystem::projects_dir()?.join(&name);
    let old = base.join(&old_path);
    let new = base.join(&new_path);
    if let Some(parent) = new.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&old, &new)?;
    eprintln!(
        "[stager] renamed {:?} asset: {} -> {}",
        name, old_path, new_path
    );
    Ok(())
}

#[tauri::command]
pub fn delete_project_asset(name: String, path: String) -> Result<(), ToolkitError> {
    let target = filesystem::projects_dir()?.join(&name).join(&path);
    eprintln!("[stager] deleting {:?} asset: {}", name, path);
    if target.is_dir() {
        std::fs::remove_dir_all(target)?;
    } else {
        std::fs::remove_file(target)?;
    }
    Ok(())
}

#[tauri::command]
pub fn import_assets_to_project(
    name: String,
    paths: Vec<String>,
    target_folder: String,
) -> Result<(), ToolkitError> {
    eprintln!("[stager] importing {} path(s) into project {:?} folder {:?}", paths.len(), name, target_folder);
    let base = filesystem::projects_dir()?.join(&name).join(&target_folder);
    std::fs::create_dir_all(&base)?;
    let num_paths = paths.len();
    for p in paths {
        let src = Path::new(&p);
        if src.is_file() {
            let dest = base.join(src.file_name().unwrap_or_default());
            eprintln!("[stager] copying file {} -> {}", src.display(), dest.display());
            std::fs::copy(src, dest)?;
        } else if src.is_dir() {
            let dest_dir = base.join(src.file_name().unwrap_or_default());
            eprintln!("[stager] copying directory {} -> {}", src.display(), dest_dir.display());
            for entry in walkdir::WalkDir::new(src).into_iter().filter_map(|e| e.ok()) {
                let rel = entry.path().strip_prefix(src).unwrap();
                let dest = dest_dir.join(rel);
                if entry.file_type().is_dir() {
                    std::fs::create_dir_all(&dest)?;
                } else {
                    if let Some(par) = dest.parent() { std::fs::create_dir_all(par)?; }
                    std::fs::copy(entry.path(), &dest)?;
                }
            }
        }
    }
    eprintln!("[stager] imported {} source path(s) into project {:?}", num_paths, name);
    Ok(())
}

// ===========================================================================
// Config Editor
// ===========================================================================

#[derive(serde::Serialize)]
pub struct ConfigData {
    pub config_type: String,
    pub content_json: String,
    pub can_save: bool,
}

#[tauri::command]
pub async fn read_config(config_path: String) -> Result<ConfigData, ToolkitError> {
    let start = Instant::now();
    eprintln!("[config_editor] reading {}", config_path);
    let data = std::fs::read(&config_path)?;
    let cfg = ConfigFile::parse(&data)?;
    let content_json = serde_json::to_string_pretty(&cfg.content)
        .map_err(|e| ToolkitError::Parse(format!("failed to serialize content to JSON: {e}")))?;
    let can_save = !cfg.config_type.starts_with("ReadOnly_");
    eprintln!(
        "[config_editor] loaded type={:?} in {:?}",
        cfg.config_type,
        start.elapsed()
    );
    Ok(ConfigData { config_type: cfg.config_type, content_json, can_save })
}

#[tauri::command]
pub async fn write_config(
    config_path: String,
    config_type: String,
    content_json: String,
    out_path: Option<String>,
) -> Result<String, ToolkitError> {
    let start = Instant::now();
    eprintln!("[config_editor] writing config type={:?}", config_type);

    let content: serde_json::Value = serde_json::from_str(&content_json)
        .map_err(|e| ToolkitError::Parse(format!("invalid JSON: {e}")))?;

    // Load original to preserve the magic and unk bytes
    let data = std::fs::read(&config_path)?;
    let mut cfg = ConfigFile::parse(&data)?;
    if cfg.config_type.starts_with("ReadOnly_") {
        return Err(ToolkitError::Unsupported(
            "saving is not supported for this file format yet".to_string(),
        ));
    }
    cfg.config_type = config_type;
    cfg.content = content;
    let bytes = cfg.save()?;

    let output = out_path
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let base = Path::new(&config_path);
            let stem = base.file_stem().unwrap_or_default().to_string_lossy();
            let ext = base.extension().unwrap_or_default().to_string_lossy();
            base.with_file_name(format!("{}_edited.{}", stem, ext))
        });

    std::fs::write(&output, &bytes)?;
    eprintln!(
        "[config_editor] saved {} bytes to {} in {:?}",
        bytes.len(),
        output.display(),
        start.elapsed()
    );
    Ok(output.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn export_config_json(content_json: String, out_path: String) -> Result<String, ToolkitError> {
    let content: serde_json::Value = serde_json::from_str(&content_json)
        .map_err(|e| ToolkitError::Parse(format!("invalid JSON: {e}")))?;
    let pretty = serde_json::to_string_pretty(&content)
        .map_err(|e| ToolkitError::Parse(format!("failed to serialize JSON: {e}")))?;
    let pretty_crlf = pretty.replace('\n', "\r\n");
    std::fs::write(&out_path, pretty_crlf)?;
    Ok(out_path)
}

// ===========================================================================
// Atmosphere Inspector
// ===========================================================================

const ATMOSPHERE_MAGIC_RCRA: u32 = 0x21D5E72C;
const ATMOSPHERE_SECTION_HEADER: u32 = 0x02F06D4E;
const ATMOSPHERE_SECTION_TEXTURE: u32 = 0x71C168B4;
const ATMOSPHERE_SECTION_STRINGS: u32 = 0x72F28658;

#[derive(serde::Serialize)]
pub struct AtmosphereSectionInfo {
    pub tag: String,
    pub offset: u32,
    pub size: u32,
}

#[derive(serde::Serialize)]
pub struct AtmosphereKnownValue {
    pub name: String,
    pub offset: u32,
    pub value_type: String,
    pub value: String,
}

#[derive(serde::Deserialize)]
pub struct AtmosphereValueEdit {
    pub offset: u32,
    pub value_type: String,
    pub value: String,
}

#[derive(serde::Serialize)]
pub struct AtmosphereData {
    pub file_path: String,
    pub outer_magic: String,
    pub outer_size: u32,
    pub dat1_magic: String,
    pub dat1_type_magic: String,
    pub dat1_total_size: u32,
    pub sections: Vec<AtmosphereSectionInfo>,
    pub known_values: Vec<AtmosphereKnownValue>,
    pub strings: Vec<String>,
    pub notes: Vec<String>,
}

fn read_u32_at(data: &[u8], off: usize) -> Option<u32> {
    let bytes = data.get(off..off + 4)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn read_f32_at(data: &[u8], off: usize) -> Option<f32> {
    let bytes = data.get(off..off + 4)?;
    Some(f32::from_le_bytes(bytes.try_into().ok()?))
}

fn read_i32_at(data: &[u8], off: usize) -> Option<i32> {
    let bytes = data.get(off..off + 4)?;
    Some(i32::from_le_bytes(bytes.try_into().ok()?))
}

fn push_f32(values: &mut Vec<AtmosphereKnownValue>, data: &[u8], name: &str, off: usize) {
    if let Some(v) = read_f32_at(data, off) {
        values.push(AtmosphereKnownValue {
            name: name.to_string(),
            offset: off as u32,
            value_type: "float".to_string(),
            value: format!("{v:.6}"),
        });
    }
}

fn push_u32(values: &mut Vec<AtmosphereKnownValue>, data: &[u8], name: &str, off: usize) {
    if let Some(v) = read_u32_at(data, off) {
        values.push(AtmosphereKnownValue {
            name: name.to_string(),
            offset: off as u32,
            value_type: "u32".to_string(),
            value: v.to_string(),
        });
    }
}

fn push_i32(values: &mut Vec<AtmosphereKnownValue>, data: &[u8], name: &str, off: usize) {
    if let Some(v) = read_i32_at(data, off) {
        values.push(AtmosphereKnownValue {
            name: name.to_string(),
            offset: off as u32,
            value_type: "i32".to_string(),
            value: v.to_string(),
        });
    }
}

fn expected_value_type_for_offset(off: usize) -> Option<&'static str> {
    match off {
        36 | 72 | 76 | 80 | 84 | 88 | 92 | 96 | 100 | 104 | 108 | 112 | 116 | 120 | 124
        | 128 | 132 | 144 | 152 | 156 | 160 | 164 | 168 | 176 | 180 | 184 | 188 | 192
        | 196 | 200 | 204 => Some("float"),
        32 | 40 | 44 | 136 | 140 | 148 => Some("u32"),
        172 => Some("i32"),
        _ => None,
    }
}

#[tauri::command]
pub async fn read_atmosphere(atmosphere_path: String) -> Result<AtmosphereData, ToolkitError> {
    let bytes = std::fs::read(&atmosphere_path)?;
    if bytes.len() < 36 {
        return Err(ToolkitError::Parse(format!(
            "atmosphere file too small: {} bytes",
            bytes.len()
        )));
    }

    let outer_magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let outer_size = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let logical_end = 36usize + outer_size as usize;
    if bytes.len() < logical_end {
        return Err(ToolkitError::Parse(format!(
            "atmosphere file truncated: wrapper size {} exceeds file length {}",
            outer_size,
            bytes.len()
        )));
    }

    let dat1_bytes = &bytes[36..logical_end];
    let dat1 = Dat1::parse(dat1_bytes)?;

    let mut notes = Vec::new();
    if outer_magic != ATMOSPHERE_MAGIC_RCRA {
        notes.push(format!(
            "outer magic is {outer_magic:#010X}, expected RCRA {ATMOSPHERE_MAGIC_RCRA:#010X}"
        ));
    }

    let mut sections = Vec::with_capacity(dat1.sections.len());
    for s in &dat1.sections {
        sections.push(AtmosphereSectionInfo {
            tag: format!("{:08X}", s.tag),
            offset: s.offset,
            size: s.size,
        });
    }

    let mut known_values = Vec::new();
    if let Some(header) = dat1.get_section_data(ATMOSPHERE_SECTION_HEADER) {
        push_u32(&mut known_values, header, "z1", 32);
        push_f32(&mut known_values, header, "time_of_day", 36);
        push_u32(&mut known_values, header, "z2", 40);
        push_u32(&mut known_values, header, "z3", 44);

        push_f32(&mut known_values, header, "curve_pair_0_x", 72);
        push_f32(&mut known_values, header, "curve_pair_0_y", 76);
        push_f32(&mut known_values, header, "curve_pair_1_x", 80);
        push_f32(&mut known_values, header, "curve_pair_1_y", 84);
        push_f32(&mut known_values, header, "curve_pair_2_x", 88);
        push_f32(&mut known_values, header, "curve_pair_2_y", 92);
        push_f32(&mut known_values, header, "curve_pair_3_x", 96);
        push_f32(&mut known_values, header, "curve_pair_3_y", 100);
        push_f32(&mut known_values, header, "curve_pair_4_x", 104);
        push_f32(&mut known_values, header, "curve_pair_4_y", 108);

        push_f32(&mut known_values, header, "sun_rgba_r", 112);
        push_f32(&mut known_values, header, "sun_rgba_g", 116);
        push_f32(&mut known_values, header, "sun_rgba_b", 120);
        push_f32(&mut known_values, header, "sun_rgba_a", 124);
        push_f32(&mut known_values, header, "sun_rot", 128);
        push_f32(&mut known_values, header, "sun_elev", 132);
        push_u32(&mut known_values, header, "sun_a", 136);
        push_u32(&mut known_values, header, "sun_b", 140);
        push_f32(&mut known_values, header, "sun_c", 144);
        push_u32(&mut known_values, header, "sun_radius", 148);

        push_f32(&mut known_values, header, "unk3_f0", 152);
        push_f32(&mut known_values, header, "unk3_f1", 156);
        push_f32(&mut known_values, header, "unk3_f2", 160);
        push_f32(&mut known_values, header, "unk3_f3", 164);
        push_f32(&mut known_values, header, "unk3_f4", 168);
        push_i32(&mut known_values, header, "unk3_i0", 172);
        push_f32(&mut known_values, header, "unk3_f5", 176);
        push_f32(&mut known_values, header, "unk3_f6", 180);
        push_f32(&mut known_values, header, "unk3_f7", 184);
        push_f32(&mut known_values, header, "unk3_f8", 188);

        push_f32(&mut known_values, header, "ambience_rgba_r", 192);
        push_f32(&mut known_values, header, "ambience_rgba_g", 196);
        push_f32(&mut known_values, header, "ambience_rgba_b", 200);
        push_f32(&mut known_values, header, "ambience_rgba_a", 204);
    } else {
        notes.push(format!(
            "missing section {ATMOSPHERE_SECTION_HEADER:#010X} (atmosphere header/content)"
        ));
    }

    if dat1.get_section_data(ATMOSPHERE_SECTION_TEXTURE).is_none() {
        notes.push(format!(
            "missing section {ATMOSPHERE_SECTION_TEXTURE:#010X} (texture DAT1/reference)"
        ));
    }

    let strings = if let Some(data) = dat1.get_section_data(ATMOSPHERE_SECTION_STRINGS) {
        data.split(|b| *b == 0)
            .filter(|chunk| !chunk.is_empty())
            .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
            .collect()
    } else {
        Vec::new()
    };

    if strings.is_empty() {
        notes.push("no strings section data found (valid for some files)".to_string());
    }

    let trailing = bytes.len().saturating_sub(logical_end);
    if trailing > 0 {
        notes.push(format!(
            "file has {} trailing byte(s) after wrapper payload; preserving them on save",
            trailing
        ));
    }

    Ok(AtmosphereData {
        file_path: atmosphere_path,
        outer_magic: format!("{:08X}", outer_magic),
        outer_size,
        dat1_magic: format!("{:08X}", dat1.magic),
        dat1_type_magic: format!("{:08X}", dat1.unk1),
        dat1_total_size: dat1.total_size,
        sections,
        known_values,
        strings,
        notes,
    })
}

#[tauri::command]
pub async fn write_atmosphere(
    atmosphere_path: String,
    values: Vec<AtmosphereValueEdit>,
    strings: Option<Vec<String>>,
    out_path: Option<String>,
) -> Result<String, ToolkitError> {
    let bytes = std::fs::read(&atmosphere_path)?;
    if bytes.len() < 36 {
        return Err(ToolkitError::Parse(format!(
            "atmosphere file too small: {} bytes",
            bytes.len()
        )));
    }

    let outer_size = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let logical_end = 36usize + outer_size as usize;
    if bytes.len() < logical_end {
        return Err(ToolkitError::Parse(format!(
            "atmosphere file truncated: wrapper size {} exceeds file length {}",
            outer_size,
            bytes.len()
        )));
    }

    let mut dat1 = Dat1::parse(&bytes[36..logical_end])?;
    let mut header_data = dat1
        .get_section_data(ATMOSPHERE_SECTION_HEADER)
        .ok_or_else(|| ToolkitError::SectionNotFound(ATMOSPHERE_SECTION_HEADER))?
        .to_vec();

    for edit in values {
        let off = edit.offset as usize;
        let expected = expected_value_type_for_offset(off).ok_or_else(|| {
            ToolkitError::Parse(format!("offset {} is not editable in phase 2", edit.offset))
        })?;

        if !edit.value_type.eq_ignore_ascii_case(expected) {
            return Err(ToolkitError::Parse(format!(
                "offset {} expected type {}, got {}",
                edit.offset, expected, edit.value_type
            )));
        }

        if off + 4 > header_data.len() {
            return Err(ToolkitError::Parse(format!(
                "offset {} out of bounds for header section size {}",
                edit.offset,
                header_data.len()
            )));
        }

        if expected == "float" {
            let parsed = edit.value.parse::<f32>().map_err(|e| {
                ToolkitError::Parse(format!(
                    "invalid float at offset {}: {} ({e})",
                    edit.offset, edit.value
                ))
            })?;
            header_data[off..off + 4].copy_from_slice(&parsed.to_le_bytes());
        } else if expected == "i32" {
            let parsed = edit.value.parse::<i32>().map_err(|e| {
                ToolkitError::Parse(format!(
                    "invalid i32 at offset {}: {} ({e})",
                    edit.offset, edit.value
                ))
            })?;
            header_data[off..off + 4].copy_from_slice(&parsed.to_le_bytes());
        } else {
            let parsed = edit.value.parse::<u32>().map_err(|e| {
                ToolkitError::Parse(format!(
                    "invalid u32 at offset {}: {} ({e})",
                    edit.offset, edit.value
                ))
            })?;
            header_data[off..off + 4].copy_from_slice(&parsed.to_le_bytes());
        }
    }

    dat1.set_section_data(ATMOSPHERE_SECTION_HEADER, header_data)?;

    if let Some(string_list) = strings {
        if dat1.get_section_data(ATMOSPHERE_SECTION_STRINGS).is_none() {
            return Err(ToolkitError::SectionNotFound(ATMOSPHERE_SECTION_STRINGS));
        }

        let mut strings_blob = Vec::new();
        for s in string_list {
            if s.is_empty() {
                continue;
            }
            strings_blob.extend_from_slice(s.as_bytes());
            strings_blob.push(0);
        }
        if strings_blob.is_empty() {
            strings_blob.push(0);
        }

        dat1.set_section_data(ATMOSPHERE_SECTION_STRINGS, strings_blob)?;
    }

    let new_dat1 = dat1.save();
    let trailing = if logical_end < bytes.len() {
        &bytes[logical_end..]
    } else {
        &[][..]
    };

    let mut output_bytes = Vec::with_capacity(36 + new_dat1.len() + trailing.len());
    output_bytes.extend_from_slice(&bytes[0..4]);
    output_bytes.extend_from_slice(&(new_dat1.len() as u32).to_le_bytes());
    output_bytes.extend_from_slice(&bytes[8..36]);
    output_bytes.extend_from_slice(&new_dat1);
    output_bytes.extend_from_slice(trailing);

    let output = out_path
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let base = Path::new(&atmosphere_path);
            let stem = base.file_stem().unwrap_or_default().to_string_lossy();
            let ext = base.extension().unwrap_or_default().to_string_lossy();
            base.with_file_name(format!("{}_edited.{}", stem, ext))
        });

    std::fs::write(&output, &output_bytes)?;
    Ok(output.to_string_lossy().into_owned())
}

// ===========================================================================
// Cross-tool routing
// ===========================================================================

/// Extract a single asset (span 0) to the OS temp dir and return its path.
/// Used by "Send To" to hand off a file to another tool without a project.
#[tauri::command]
pub async fn extract_to_temp(
    toc_path: String,
    asset_id: String,
    archives_dir: String,
    filename: String,
) -> Result<String, ToolkitError> {
    let temp_dir = std::env::temp_dir().join("omnitool");
    std::fs::create_dir_all(&temp_dir)?;

    let id = u64::from_str_radix(&asset_id, 16)
        .map_err(|e| ToolkitError::Parse(format!("invalid asset id hex: {e}")))?;

    let data = std::fs::read(&toc_path)?;
    let toc = Toc::parse(&data)?;

    let asset = toc
        .assets()
        .into_iter()
        .find(|a| a.asset_id == id && a.span_index == 0)
        .ok_or_else(|| ToolkitError::Parse(format!("asset {asset_id} not found in TOC")))?;

    let raw = toc.extract_asset(&asset, Path::new(&archives_dir))?;

    let fname = Path::new(&filename)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("{asset_id}"));

    let out_path = temp_dir.join(&fname);
    std::fs::write(&out_path, &raw)?;
    info!("extract_to_temp: {} → {}", asset_id, out_path.display());
    Ok(out_path.to_string_lossy().into_owned())
}

/// Download the hashes file from the SpaceDepot release and save it next to the exe.
#[tauri::command]
pub async fn download_hashes() -> Result<String, ToolkitError> {
    use std::io::Read;

    const URL: &str =
        "https://github.com/SpaceDepot/rcra-depot/releases/download/hashes/hashes";

    let path = filesystem::hashes_path()?;

    let bytes = tauri::async_runtime::spawn_blocking(|| -> std::result::Result<Vec<u8>, String> {
        let response = ureq::get(URL)
            .timeout(std::time::Duration::from_secs(120))
            .call()
            .map_err(|e| format!("request failed: {e}"))?;

        let mut buf = Vec::new();
        response
            .into_reader()
            .read_to_end(&mut buf)
            .map_err(|e| format!("read failed: {e}"))?;
        Ok(buf)
    })
    .await
    .map_err(|e| ToolkitError::Parse(format!("task error: {e}")))?
    .map_err(ToolkitError::Parse)?;

    std::fs::write(&path, &bytes)?;
    info!("download_hashes: {} bytes → {}", bytes.len(), path.display());
    Ok(format!("{} bytes", bytes.len()))
}

/// Copy a file directly into a project at an exact relative path (no rename suffix).
/// Used by tools to send their output straight into a staging project.
#[tauri::command]
pub fn import_file_to_project(
    name: String,
    source_path: String,
    target_path: String,
) -> Result<(), ToolkitError> {
    let dest = filesystem::projects_dir()?.join(&name).join(&target_path);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&source_path, &dest)?;
    info!("import_file_to_project: {} → {}/{}", source_path, name, target_path);
    Ok(())
}

// ===========================================================================
// ZoneLightBin Inspector / Editor
// ===========================================================================

const ZLB_WRAPPER_MAGIC: u32 = 0xFA8D90B3;
const ZLB_WRAPPER_HEADER_LEN: usize = 36;

const ZLB_PRIMARY_TAG_A: u32 = 0x27204B67;
const ZLB_PRIMARY_TAG_B: u32 = 0x101A2196;
const ZLB_SECONDARY_TAG_A: u32 = 0x13F4AF3B;
const ZLB_SECONDARY_TAG_B: u32 = 0xC72A514C;

#[derive(serde::Serialize, Clone)]
pub struct ZlbSectionInfo {
    pub tag: String,
    pub offset: u32,
    pub declared_size: u32,
    pub available_size: u32,
    pub crc32: u32,
    pub truncated: bool,
}

#[derive(serde::Serialize, Clone)]
pub struct ZlbDat1Info {
    pub magic: String,
    pub type_magic: String,
    pub declared_total_size: u32,
    pub available_size: u32,
    pub start_offset: u64,
    pub sections: Vec<ZlbSectionInfo>,
    pub truncated: bool,
}

#[derive(serde::Serialize)]
pub struct ZoneLightBinData {
    pub file_path: String,
    pub file_size: u64,
    pub wrapper_magic: String,
    pub wrapper_size: u32,
    pub primary: ZlbDat1Info,
    pub bridge_bytes_hex: String,
    pub bridge_offset: u64,
    pub bridge_length: u32,
    pub secondary: Option<ZlbDat1Info>,
    pub trailing_after_secondary: u64,
    pub notes: Vec<String>,
}

#[derive(serde::Deserialize, Default)]
pub struct ZlbCopyOptions {
    #[serde(default)]
    pub primary_27204b67: bool,
    #[serde(default)]
    pub primary_101a2196: bool,
    #[serde(default)]
    pub secondary_13f4af3b: bool,
    #[serde(default)]
    pub secondary_c72a514c: bool,
}

#[derive(serde::Serialize)]
pub struct ZlbDiffSection {
    pub tag: String,
    pub layer: String, // "primary" or "secondary"
    pub base_size: Option<u32>,
    pub reference_size: Option<u32>,
    pub equal: bool,
    pub byte_diffs: Option<u32>,
}

#[derive(serde::Serialize)]
pub struct ZlbDiffResult {
    pub base_path: String,
    pub reference_path: String,
    pub base_file_size: u64,
    pub reference_file_size: u64,
    pub sections: Vec<ZlbDiffSection>,
    pub notes: Vec<String>,
}

fn crc32_bytes(data: &[u8]) -> u32 {
    const POLY: u32 = 0xEDB8_8320;
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ POLY } else { crc >> 1 };
        }
    }
    !crc
}

/// Lightweight DAT1 view that does NOT require all section data to be present.
/// Some `.zonelightbin` files declare section sizes larger than the bytes
/// actually stored on disk (the rest is streamed at runtime). We tolerate
/// truncation: each section gets a slice of whatever bytes are available.
struct Dat1View<'a> {
    bytes: &'a [u8],
    start: usize,
    magic: u32,
    type_magic: u32,
    declared_total_size: u32,
    available_size: u32,
    sections: Vec<crate::core::dat1::SectionHeader>,
    truncated: bool,
}

impl<'a> Dat1View<'a> {
    fn parse(bytes: &'a [u8], start: usize) -> std::result::Result<Self, ToolkitError> {
        if start + 16 > bytes.len() {
            return Err(ToolkitError::Parse(format!(
                "DAT1 header out of bounds at offset {start}"
            )));
        }
        let magic = u32::from_le_bytes(bytes[start..start + 4].try_into().unwrap());
        if magic != crate::core::dat1::DAT1_MAGIC {
            return Err(ToolkitError::InvalidMagic {
                expected: crate::core::dat1::DAT1_MAGIC,
                got: magic,
            });
        }
        let type_magic = u32::from_le_bytes(bytes[start + 4..start + 8].try_into().unwrap());
        let declared_total_size = u32::from_le_bytes(bytes[start + 8..start + 12].try_into().unwrap());
        let sections_count = u16::from_le_bytes(bytes[start + 12..start + 14].try_into().unwrap()) as usize;
        let unknown_count = u16::from_le_bytes(bytes[start + 14..start + 16].try_into().unwrap()) as usize;

        let header_table_end = 16 + 12 * sections_count + 8 * unknown_count;
        if start + header_table_end > bytes.len() {
            return Err(ToolkitError::Parse(format!(
                "DAT1 section table at offset {start} extends past file end"
            )));
        }

        let mut sections = Vec::with_capacity(sections_count);
        for i in 0..sections_count {
            let sh = start + 16 + i * 12;
            sections.push(crate::core::dat1::SectionHeader {
                tag: u32::from_le_bytes(bytes[sh..sh + 4].try_into().unwrap()),
                offset: u32::from_le_bytes(bytes[sh + 4..sh + 8].try_into().unwrap()),
                size: u32::from_le_bytes(bytes[sh + 8..sh + 12].try_into().unwrap()),
            });
        }

        let available_end = (start + declared_total_size as usize).min(bytes.len());
        let available_size = (available_end - start) as u32;
        let truncated = (declared_total_size as usize) > (available_size as usize);

        Ok(Dat1View {
            bytes,
            start,
            magic,
            type_magic,
            declared_total_size,
            available_size,
            sections,
            truncated,
        })
    }

    fn section_slice(&self, tag: u32) -> Option<&'a [u8]> {
        let header = self.sections.iter().find(|s| s.tag == tag)?;
        let abs_start = self.start + header.offset as usize;
        let declared_end = abs_start + header.size as usize;
        if abs_start >= self.bytes.len() {
            return Some(&[]);
        }
        let actual_end = declared_end.min(self.bytes.len());
        Some(&self.bytes[abs_start..actual_end])
    }
}

fn dat1_view_to_info(view: &Dat1View<'_>) -> ZlbDat1Info {
    let mut sections = Vec::with_capacity(view.sections.len());
    for s in &view.sections {
        let data = view.section_slice(s.tag).unwrap_or(&[]);
        let available = data.len() as u32;
        sections.push(ZlbSectionInfo {
            tag: format!("{:08X}", s.tag),
            offset: s.offset,
            declared_size: s.size,
            available_size: available,
            crc32: crc32_bytes(data),
            truncated: available < s.size,
        });
    }
    ZlbDat1Info {
        magic: format!("{:08X}", view.magic),
        type_magic: format!("{:08X}", view.type_magic),
        declared_total_size: view.declared_total_size,
        available_size: view.available_size,
        start_offset: view.start as u64,
        sections,
        truncated: view.truncated,
    }
}

/// Locate a secondary DAT1 starting at `expected` (logical_end + 2). If not
/// present there, scan a small window for the DAT1 magic. Returns the
/// absolute start offset of the secondary DAT1, or `None` if not found.
fn find_secondary_dat1(bytes: &[u8], logical_end: usize) -> Option<usize> {
    let dat1_magic_le = crate::core::dat1::DAT1_MAGIC.to_le_bytes();

    // Preferred location per research: logical_end + 2.
    let preferred = logical_end + 2;
    if preferred + 4 <= bytes.len() && bytes[preferred..preferred + 4] == dat1_magic_le {
        return Some(preferred);
    }

    // Fallback: scan a small window after logical_end (up to 64 bytes).
    let scan_start = logical_end;
    let scan_end = (logical_end + 64).min(bytes.len().saturating_sub(4));
    for off in scan_start..=scan_end {
        if bytes[off..off + 4] == dat1_magic_le {
            return Some(off);
        }
    }
    None
}

#[tauri::command]
pub async fn read_zonelightbin(
    zlb_path: String,
) -> std::result::Result<ZoneLightBinData, ToolkitError> {
    let bytes = std::fs::read(&zlb_path)?;
    if bytes.len() < ZLB_WRAPPER_HEADER_LEN {
        return Err(ToolkitError::Parse(format!(
            "zonelightbin file too small: {} bytes",
            bytes.len()
        )));
    }

    let mut notes: Vec<String> = Vec::new();

    let wrapper_magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    if wrapper_magic != ZLB_WRAPPER_MAGIC {
        notes.push(format!(
            "outer magic is {wrapper_magic:#010X}, expected ZoneLightBinRcra {ZLB_WRAPPER_MAGIC:#010X}"
        ));
    }
    let wrapper_size = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let logical_end = ZLB_WRAPPER_HEADER_LEN + wrapper_size as usize;
    if bytes.len() < logical_end {
        return Err(ToolkitError::Parse(format!(
            "zonelightbin truncated: wrapper size {wrapper_size} exceeds file length {}",
            bytes.len()
        )));
    }

    // Primary DAT1 sits within the wrapper bounds; parse it as a view too so
    // truncated declarations don't blow up the inspector.
    let primary_view = Dat1View::parse(&bytes, ZLB_WRAPPER_HEADER_LEN)?;
    let primary_info = dat1_view_to_info(&primary_view);

    // Secondary DAT1 (optional)
    let mut secondary_info: Option<ZlbDat1Info> = None;
    let mut bridge_bytes_hex = String::new();
    let bridge_offset: u64 = logical_end as u64;
    let mut bridge_length: u32 = 0;
    let mut trailing_after_secondary: u64 = 0;

    if logical_end < bytes.len() {
        match find_secondary_dat1(&bytes, logical_end) {
            Some(sec_start) => {
                bridge_length = (sec_start - logical_end) as u32;
                bridge_bytes_hex = bytes[logical_end..sec_start]
                    .iter()
                    .map(|b| format!("{:02X}", b))
                    .collect::<Vec<_>>()
                    .join(" ");
                if bridge_length != 2 {
                    notes.push(format!(
                        "bridge between primary and secondary DAT1 is {bridge_length} byte(s), \
                         expected 2 in samples"
                    ));
                }
                let sec_view = Dat1View::parse(&bytes, sec_start)?;
                let declared_end = sec_start + sec_view.declared_total_size as usize;
                if declared_end > bytes.len() {
                    let missing = declared_end - bytes.len();
                    notes.push(format!(
                        "secondary DAT1 declares total_size {} but only {} byte(s) are present \
                         on disk ({} byte(s) appear to be streamed at runtime). The inspector \
                         will show available data.",
                        sec_view.declared_total_size,
                        sec_view.available_size,
                        missing
                    ));
                    trailing_after_secondary = 0;
                } else {
                    trailing_after_secondary = (bytes.len() - declared_end) as u64;
                    if trailing_after_secondary > 0 {
                        notes.push(format!(
                            "{} byte(s) of trailing data after secondary DAT1; preserving on save",
                            trailing_after_secondary
                        ));
                    }
                }
                secondary_info = Some(dat1_view_to_info(&sec_view));
            }
            None => {
                let trailing = bytes.len() - logical_end;
                bridge_length = trailing as u32;
                bridge_bytes_hex = bytes[logical_end..]
                    .iter()
                    .take(64)
                    .map(|b| format!("{:02X}", b))
                    .collect::<Vec<_>>()
                    .join(" ");
                notes.push(format!(
                    "no secondary DAT1 found after primary; {} trailing byte(s) preserved as-is",
                    trailing
                ));
            }
        }
    } else {
        notes.push("file has no trailing data after primary DAT1".to_string());
    }

    Ok(ZoneLightBinData {
        file_path: zlb_path,
        file_size: bytes.len() as u64,
        wrapper_magic: format!("{:08X}", wrapper_magic),
        wrapper_size,
        primary: primary_info,
        bridge_bytes_hex,
        bridge_offset,
        bridge_length,
        secondary: secondary_info,
        trailing_after_secondary,
        notes,
    })
}

/// Owning version of [`Dat1View`] used by the writer so we can hold both base
/// and reference loads at once without lifetime juggling.
struct ZlbLayout {
    bytes: Vec<u8>,
    primary_start: usize,
    primary_sections: Vec<crate::core::dat1::SectionHeader>,
    secondary: Option<ZlbSecondaryLayout>,
}

struct ZlbSecondaryLayout {
    start: usize,
    sections: Vec<crate::core::dat1::SectionHeader>,
}

fn load_zlb_layout(path: &str) -> std::result::Result<ZlbLayout, ToolkitError> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < ZLB_WRAPPER_HEADER_LEN {
        return Err(ToolkitError::Parse(format!(
            "zonelightbin file too small: {} bytes",
            bytes.len()
        )));
    }

    let wrapper_magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    if wrapper_magic != ZLB_WRAPPER_MAGIC {
        return Err(ToolkitError::InvalidMagic {
            expected: ZLB_WRAPPER_MAGIC,
            got: wrapper_magic,
        });
    }
    let wrapper_size = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let logical_end = ZLB_WRAPPER_HEADER_LEN + wrapper_size as usize;
    if bytes.len() < logical_end {
        return Err(ToolkitError::Parse(format!(
            "zonelightbin truncated: wrapper size {wrapper_size} exceeds file length {}",
            bytes.len()
        )));
    }

    let primary_view = Dat1View::parse(&bytes, ZLB_WRAPPER_HEADER_LEN)?;
    let primary_sections = primary_view.sections.clone();

    let secondary = if logical_end < bytes.len() {
        find_secondary_dat1(&bytes, logical_end).map(|sec_start| {
            // Best-effort: ignore parse error here so writer can still operate
            // on primary alone if secondary is malformed.
            let sections = Dat1View::parse(&bytes, sec_start)
                .map(|v| v.sections.clone())
                .unwrap_or_default();
            ZlbSecondaryLayout {
                start: sec_start,
                sections,
            }
        })
    } else {
        None
    };

    Ok(ZlbLayout {
        bytes,
        primary_start: ZLB_WRAPPER_HEADER_LEN,
        primary_sections,
        secondary,
    })
}

fn dat1_section_slice<'a>(
    bytes: &'a [u8],
    dat1_start: usize,
    sections: &[crate::core::dat1::SectionHeader],
    tag: u32,
) -> Option<&'a [u8]> {
    let header = sections.iter().find(|s| s.tag == tag)?;
    let abs_start = dat1_start + header.offset as usize;
    if abs_start >= bytes.len() {
        return Some(&[]);
    }
    let actual_end = (abs_start + header.size as usize).min(bytes.len());
    Some(&bytes[abs_start..actual_end])
}

/// Copy a section's raw bytes from `reference` into `base` at the same
/// absolute offset. Requires the section to exist in both layouts AND have
/// the same declared size in both files (so DAT1 offsets / total_size stay
/// valid). Returns the number of bytes written.
fn copy_section_inplace(
    base: &mut [u8],
    reference: &[u8],
    base_dat1_start: usize,
    base_sections: &[crate::core::dat1::SectionHeader],
    ref_dat1_start: usize,
    ref_sections: &[crate::core::dat1::SectionHeader],
    tag: u32,
) -> std::result::Result<usize, ToolkitError> {
    let base_h = base_sections
        .iter()
        .find(|s| s.tag == tag)
        .ok_or(ToolkitError::SectionNotFound(tag))?;
    let ref_h = ref_sections
        .iter()
        .find(|s| s.tag == tag)
        .ok_or(ToolkitError::SectionNotFound(tag))?;

    if base_h.size != ref_h.size {
        return Err(ToolkitError::Parse(format!(
            "section 0x{tag:08X} size differs: base={} reference={}; in-place copy requires \
             matching declared sizes (full DAT1 rebuild not yet supported)",
            base_h.size, ref_h.size
        )));
    }

    let base_abs = base_dat1_start + base_h.offset as usize;
    let ref_abs = ref_dat1_start + ref_h.offset as usize;
    let avail_base = base.len().saturating_sub(base_abs);
    let avail_ref = reference.len().saturating_sub(ref_abs);
    let copy_len = base_h.size as usize;
    let n = copy_len.min(avail_base).min(avail_ref);
    if n == 0 {
        return Err(ToolkitError::Parse(format!(
            "section 0x{tag:08X} has no bytes available in either file"
        )));
    }
    base[base_abs..base_abs + n].copy_from_slice(&reference[ref_abs..ref_abs + n]);
    Ok(n)
}

#[tauri::command]
pub async fn write_zonelightbin_sections(
    base_path: String,
    reference_path: String,
    options: ZlbCopyOptions,
    out_path: Option<String>,
) -> std::result::Result<String, ToolkitError> {
    let start_t = Instant::now();

    let base_layout = load_zlb_layout(&base_path)?;
    let reference_layout = load_zlb_layout(&reference_path)?;

    let want_primary = options.primary_27204b67 || options.primary_101a2196;
    let want_secondary = options.secondary_13f4af3b || options.secondary_c72a514c;
    if !want_primary && !want_secondary {
        return Err(ToolkitError::Parse(
            "no copy options selected; nothing to do".to_string(),
        ));
    }

    let mut out_bytes = base_layout.bytes.clone();
    let mut summary = Vec::new();

    if options.primary_27204b67 {
        let n = copy_section_inplace(
            &mut out_bytes,
            &reference_layout.bytes,
            base_layout.primary_start,
            &base_layout.primary_sections,
            reference_layout.primary_start,
            &reference_layout.primary_sections,
            ZLB_PRIMARY_TAG_A,
        )?;
        summary.push(format!("primary 0x27204B67 ({n} B)"));
    }
    if options.primary_101a2196 {
        let n = copy_section_inplace(
            &mut out_bytes,
            &reference_layout.bytes,
            base_layout.primary_start,
            &base_layout.primary_sections,
            reference_layout.primary_start,
            &reference_layout.primary_sections,
            ZLB_PRIMARY_TAG_B,
        )?;
        summary.push(format!("primary 0x101A2196 ({n} B)"));
    }

    if want_secondary {
        let base_sec = base_layout
            .secondary
            .as_ref()
            .ok_or_else(|| ToolkitError::Parse("base file has no secondary DAT1".to_string()))?;
        let ref_sec = reference_layout
            .secondary
            .as_ref()
            .ok_or_else(|| ToolkitError::Parse("reference file has no secondary DAT1".to_string()))?;

        if options.secondary_13f4af3b {
            let n = copy_section_inplace(
                &mut out_bytes,
                &reference_layout.bytes,
                base_sec.start,
                &base_sec.sections,
                ref_sec.start,
                &ref_sec.sections,
                ZLB_SECONDARY_TAG_A,
            )?;
            summary.push(format!("secondary 0x13F4AF3B ({n} B)"));
        }
        if options.secondary_c72a514c {
            let n = copy_section_inplace(
                &mut out_bytes,
                &reference_layout.bytes,
                base_sec.start,
                &base_sec.sections,
                ref_sec.start,
                &ref_sec.sections,
                ZLB_SECONDARY_TAG_B,
            )?;
            summary.push(format!("secondary 0xC72A514C ({n} B)"));
        }
    }

    let output = out_path
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let p = Path::new(&base_path);
            let stem = p.file_stem().unwrap_or_default().to_string_lossy();
            let ext = p.extension().unwrap_or_default().to_string_lossy();
            p.with_file_name(format!("{}_edited.{}", stem, ext))
        });
    std::fs::write(&output, &out_bytes)?;

    info!(
        "write_zonelightbin_sections: base={} ref={} -> {} ({} bytes) [{}] in {:?}",
        base_path,
        reference_path,
        output.display(),
        out_bytes.len(),
        summary.join(", "),
        start_t.elapsed()
    );
    Ok(output.to_string_lossy().into_owned())
}

fn diff_layer_section(
    base_bytes: &[u8],
    base_start: Option<usize>,
    base_sections: Option<&[crate::core::dat1::SectionHeader]>,
    ref_bytes: &[u8],
    ref_start: Option<usize>,
    ref_sections: Option<&[crate::core::dat1::SectionHeader]>,
    tag: u32,
    layer: &str,
) -> ZlbDiffSection {
    let base_data = match (base_start, base_sections) {
        (Some(s), Some(secs)) => dat1_section_slice(base_bytes, s, secs, tag),
        _ => None,
    };
    let ref_data = match (ref_start, ref_sections) {
        (Some(s), Some(secs)) => dat1_section_slice(ref_bytes, s, secs, tag),
        _ => None,
    };

    let base_size = base_data.map(|d| d.len() as u32);
    let reference_size = ref_data.map(|d| d.len() as u32);

    let (equal, byte_diffs) = match (base_data, ref_data) {
        (Some(a), Some(b)) if a.len() == b.len() => {
            let diffs: u32 = a.iter().zip(b.iter()).filter(|(x, y)| x != y).count() as u32;
            (diffs == 0, Some(diffs))
        }
        (Some(_), Some(_)) => (false, None),
        _ => (false, None),
    };

    ZlbDiffSection {
        tag: format!("{:08X}", tag),
        layer: layer.to_string(),
        base_size,
        reference_size,
        equal,
        byte_diffs,
    }
}

#[tauri::command]
pub async fn diff_zonelightbin(
    base_path: String,
    reference_path: String,
) -> std::result::Result<ZlbDiffResult, ToolkitError> {
    let base = load_zlb_layout(&base_path)?;
    let reference = load_zlb_layout(&reference_path)?;

    let mut sections = Vec::new();
    for tag in [ZLB_PRIMARY_TAG_A, ZLB_PRIMARY_TAG_B] {
        sections.push(diff_layer_section(
            &base.bytes,
            Some(base.primary_start),
            Some(&base.primary_sections),
            &reference.bytes,
            Some(reference.primary_start),
            Some(&reference.primary_sections),
            tag,
            "primary",
        ));
    }
    for tag in [ZLB_SECONDARY_TAG_A, ZLB_SECONDARY_TAG_B] {
        sections.push(diff_layer_section(
            &base.bytes,
            base.secondary.as_ref().map(|s| s.start),
            base.secondary.as_ref().map(|s| s.sections.as_slice()),
            &reference.bytes,
            reference.secondary.as_ref().map(|s| s.start),
            reference.secondary.as_ref().map(|s| s.sections.as_slice()),
            tag,
            "secondary",
        ));
    }

    let mut notes = Vec::new();
    if base.secondary.is_none() {
        notes.push("base file has no secondary DAT1".to_string());
    }
    if reference.secondary.is_none() {
        notes.push("reference file has no secondary DAT1".to_string());
    }

    Ok(ZlbDiffResult {
        base_path,
        reference_path,
        base_file_size: base.bytes.len() as u64,
        reference_file_size: reference.bytes.len() as u64,
        sections,
        notes,
    })
}
