use std::path::{Path, PathBuf};
use std::time::Instant;

use log::{debug, info, warn};
use walkdir::WalkDir;

use crate::core::config::ConfigFile;
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
