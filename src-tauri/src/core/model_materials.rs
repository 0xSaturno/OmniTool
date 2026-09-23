//! Resolving a model's material slots down to the textures they bind.
//!
//! The chain the level viewport needs, end to end:
//!
//! ```text
//! .model  Model Material (0x3250BB80)   slot i -> material path + Maya mapping name + asset id
//!   -> .material  Material Serialized Data (0xF5260180)   sampler hash -> bound .texture path
//!        Material Template Samplers     (0x1CAFE804)      sampler hash -> DEFAULT texture path
//!   -> .texture   Texture Header        (0x4EDE3593)      width / height / DXGI format
//! ```
//!
//! `slot i` is exactly what the RCMS submesh `materialIndex` indexes, so the
//! viewport can go from a draw call to a texture without another lookup table.
//!
//! **Where a slot name comes from.** The human-readable HLSL sampler name is
//! *not* shipped: the compiled `RDEF` blocks only carry `textureUnit8` /
//! `textureSampler8`, and the serialized sampler records are keyed by a CRC32
//! in a hash space of their own. What *is* shipped is the template's **default**
//! texture path per sampler, and its suffix names the semantic
//! (`…_c` albedo, `…_n` normal, `…_g` gloss, `…_m`/`…_mp` mask …). A
//! `.material` embeds a copy of its template's sampler table, so that mapping
//! is readable without opening the `.materialgraph` at all — and every sampler
//! hash seen anywhere is remembered in a process-wide table, so a material that
//! ships without the template section still gets real names as soon as any
//! other material has declared the same hash. Only when both of those fail does
//! this fall back to guessing from the *bound* texture's own suffix, which
//! [`SlotName::Guessed`] marks so the caller can tell the two apart.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use dashmap::DashMap;
use log::debug;
use serde::Serialize;

use crate::core::crc64;
use crate::core::error::{Result, ToolkitError};
use crate::core::level::{self, LevelIndex};
use crate::core::material::{MaterialFile, MaterialSerialized, TAG_MATERIAL_SERIALIZED};
use crate::core::material_graph::MaterialTemplate;
use crate::core::material_names;
use crate::core::texture_serve::{self, TexInfo};
use crate::tools::model_converter::sections::looks::{MaterialSection, TAG_MATERIAL};

/// How many resolved models to keep. A planet has ~890 distinct models and each
/// record is a few hundred bytes, so the whole planet fits comfortably.
const MODEL_CACHE_CAP: usize = 4096;

// ---------------------------------------------------------------------------
// DTOs — the `level_model_materials` contract
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureRef {
    /// The sampler's semantic: `albedo` | `normal` | `roughness` | `mask` |
    /// `height` | `ao` | `id` | `emissive` | `detail` | `other`.
    pub slot: String,
    /// `0x`-prefixed 16-hex asset id, `"0x0"` when the path is empty.
    pub texture_id: String,
    /// Lowercase forward-slash asset path, `""` when unknown.
    pub texture_path: String,
    /// Native size — the first streamed mip when one exists, else the resident
    /// base. `0` when the asset is not in the archives.
    pub width: u32,
    pub height: u32,
    /// `DXGI_FORMAT` name, `""` when the asset is not in the archives.
    pub format: String,
    /// True when `slot` came from the material's own template table rather than
    /// from guessing at the bound texture's filename.
    pub named: bool,
    /// True when this binds one of the shipped placeholder textures
    /// (`default_pink_shader`, `mo_white_h`, `mo_black`, `default_grid`).
    pub placeholder: bool,
    /// True when the decoded texels are sRGB-encoded.
    pub srgb: bool,
    /// True when the texture asset was found in the TOC and its header parsed.
    pub resolved: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaterialSlot {
    /// Matches the RCMS submesh `materialIndex`.
    pub index: u32,
    /// `0x`-prefixed 16-hex asset id of the `.material`; `"0x0"` when the model
    /// names no material for this slot at all.
    pub material_id: String,
    /// Lowercase forward-slash path, `""` when unknown.
    pub material_path: String,
    /// The Maya mapping name, for display.
    pub mapping_name: String,
    /// The `.materialgraph` the material instances, `""` when it names none.
    pub template: String,
    /// True when the `.material` asset was found in the TOC and parsed.
    pub resolved: bool,
    /// **The viewport's fallback signal.** True when this slot is a shipped
    /// placeholder (`gl_default_grid`, `gl_default`, `default_pink`), when the
    /// material is missing from the archives, or when every texture it binds is
    /// a placeholder. Draw these with the neutral debug colour instead of
    /// papering a pink grid over the level.
    pub placeholder: bool,
    pub textures: Vec<TextureRef>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelMaterials {
    pub model_id: String,
    pub slots: Vec<MaterialSlot>,
}

// ---------------------------------------------------------------------------
// Slot semantics
// ---------------------------------------------------------------------------

/// Where a slot name came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotName {
    /// From the material's own template sampler table (or a sampler hash some
    /// other material declared).
    Declared,
    /// From the bound texture's own filename suffix.
    Guessed,
}

/// Filename suffix to semantic. Returns `None` for a suffix that says nothing.
fn semantic_of_path(path: &str) -> Option<&'static str> {
    let file = path.rsplit(['/', '\\']).next()?.to_ascii_lowercase();
    let stem = file.strip_suffix(".texture").unwrap_or(&file);
    let suffix = stem.rsplit('_').next()?;
    Some(match suffix {
        "c" | "col" | "color" | "colour" | "albedo" | "basecolor" | "d" | "diff" => "albedo",
        "n" | "nrm" | "norm" | "normal" => "normal",
        "g" | "gloss" | "glossiness" | "r" | "rough" | "roughness" => "roughness",
        "m" | "mp" | "mask" | "masks" => "mask",
        "h" | "height" => "height",
        "ao" | "occl" => "ao",
        "id" | "ids" => "id",
        "e" | "emiss" | "emissive" | "glow" => "emissive",
        "det" | "detail" => "detail",
        _ => return None,
    })
}

/// The few slot names recovered by dictionary attack live in
/// [`material_names`]; fold them into the same vocabulary.
fn semantic_of_known_name(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    Some(if n.starts_with("basemap") {
        "albedo"
    } else if n.starts_with("normalmap") {
        "normal"
    } else if n.starts_with("glossmap") {
        "roughness"
    } else if n.starts_with("maskmap") {
        "mask"
    } else {
        return None;
    })
}

/// Last resort: the texture's own `content_type` tag. Measured over a busy art
/// zone it is a clean discriminator — every `_n` map carries `0x02` and nothing
/// else does, every `_c` carries `0x01` — so it rescues the bindings whose file
/// name ends in something uninformative (`trim_master_001`, `grunge_001`).
fn semantic_of_content(info: Option<TexInfo>) -> Option<&'static str> {
    match info?.content_type {
        texture_serve::CONTENT_COLOR => Some("albedo"),
        texture_serve::CONTENT_NORMAL => Some("normal"),
        _ => None,
    }
}

/// Process-wide `sampler name_hash -> semantic`, learned from every template
/// sampler table this process has parsed.
///
/// A `.material` embeds its own template's table, so this fills itself as
/// materials are read; it exists so that the handful of materials that ship
/// *without* the section still resolve, and so a second model reusing a shared
/// sampler hash never has to guess.
fn semantic_table() -> &'static DashMap<u32, &'static str> {
    static TABLE: OnceLock<DashMap<u32, &'static str>> = OnceLock::new();
    TABLE.get_or_init(DashMap::new)
}

fn learn_template(template: &MaterialTemplate) {
    for s in &template.samplers {
        if let Some(sem) = semantic_of_path(&s.default_path) {
            semantic_table().entry(s.name_hash).or_insert(sem);
        }
    }
}

/// Distinct sampler hashes the semantic table currently knows, for the probe.
pub fn semantic_table_len() -> usize {
    semantic_table().len()
}

// ---------------------------------------------------------------------------
// Placeholders
// ---------------------------------------------------------------------------

/// Placeholder `.material` paths — the ones the game itself renders as a pink
/// grid when an artist has not assigned anything yet.
pub fn is_placeholder_material(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.contains("gl_default_grid") || p.contains("gl_default") || p.contains("default_pink")
}

/// Placeholder `.texture` paths — 2x2 pink, 16x16 white, flat black, the grid.
pub fn is_placeholder_texture(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.contains("default_pink_shader")
        || p.contains("default_grid")
        || p.ends_with("mo_white_h.texture")
        || p.contains("/mo_black")
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

fn hex_id(id: u64) -> String {
    format!("{id:#018X}")
}

/// One material's textures plus the template it instances.
struct ResolvedMaterial {
    template: String,
    textures: Vec<TextureRef>,
}

/// Counters the probe reports; every resolution bumps them.
#[derive(Debug, Default, Clone, Copy)]
pub struct ResolveCounters {
    pub slots: usize,
    pub slots_resolved: usize,
    pub slots_placeholder: usize,
    pub bindings: usize,
    pub bindings_declared: usize,
    pub bindings_guessed: usize,
    pub bindings_unnamed: usize,
    pub textures_resolved: usize,
    pub textures_missing: usize,
}

fn counters() -> &'static Mutex<ResolveCounters> {
    static C: OnceLock<Mutex<ResolveCounters>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(ResolveCounters::default()))
}

/// Read and zero the counters — call it around a measured pass.
pub fn take_counters() -> ResolveCounters {
    let mut g = counters().lock().unwrap();
    std::mem::take(&mut *g)
}

fn bump(f: impl FnOnce(&mut ResolveCounters)) {
    f(&mut counters().lock().unwrap());
}

/// Parse one `.material` and resolve every texture it binds.
fn resolve_material(index: &LevelIndex, material_id: u64) -> Result<ResolvedMaterial> {
    let raw = index.extract(material_id)?;
    let mat = MaterialFile::parse(&raw)?;
    let template_path = mat.template_path().unwrap_or_default();

    // The material embeds a copy of its template's sampler table; learn from it
    // whether or not this particular material binds every slot.
    let template = MaterialTemplate::from_dat1(&mat.dat1).unwrap_or_default();
    learn_template(&template);

    let serialized = match mat.dat1.get_section_data(TAG_MATERIAL_SERIALIZED) {
        Some(sec) => MaterialSerialized::parse(sec)?,
        None => MaterialSerialized::default(),
    };

    let mut textures = Vec::with_capacity(serialized.samplers.len());
    for s in &serialized.samplers {
        let path = crc64::normalize_path(&s.path);

        let texture_id = if path.is_empty() {
            0
        } else {
            crc64::hash_raw(path.as_bytes())
        };
        let info: Option<TexInfo> = if texture_id == 0 {
            None
        } else {
            texture_serve::texture_info(index, texture_id)
        };

        // 1. this material's own template table, 2. anything another material
        // has declared for the same hash, 3. a recovered literal slot name,
        // 4. the bound texture's own suffix, 5. the texture header's own
        //    content-type tag.
        let (slot, source) = template
            .samplers
            .iter()
            .find(|t| t.name_hash == s.name_hash)
            .and_then(|t| semantic_of_path(&t.default_path))
            .map(|sem| (sem, SlotName::Declared))
            .or_else(|| {
                semantic_table()
                    .get(&s.name_hash)
                    .map(|sem| (*sem, SlotName::Declared))
            })
            .or_else(|| {
                material_names::resolve(s.name_hash)
                    .as_deref()
                    .and_then(semantic_of_known_name)
                    .map(|sem| (sem, SlotName::Declared))
            })
            .or_else(|| semantic_of_path(&path).map(|sem| (sem, SlotName::Guessed)))
            .or_else(|| semantic_of_content(info).map(|sem| (sem, SlotName::Guessed)))
            .unwrap_or(("other", SlotName::Guessed));

        let (w, h) = info.map(|i| i.native()).unwrap_or((0, 0));

        bump(|c| {
            c.bindings += 1;
            match (source, slot) {
                (_, "other") => c.bindings_unnamed += 1,
                (SlotName::Declared, _) => c.bindings_declared += 1,
                (SlotName::Guessed, _) => c.bindings_guessed += 1,
            }
            if info.is_some() {
                c.textures_resolved += 1;
            } else {
                c.textures_missing += 1;
            }
        });

        textures.push(TextureRef {
            slot: slot.to_string(),
            texture_id: hex_id(texture_id),
            texture_path: path.clone(),
            width: w,
            height: h,
            format: info.map(|i| texture_serve::format_name(i.format)).unwrap_or_default(),
            named: source == SlotName::Declared,
            placeholder: is_placeholder_texture(&path),
            srgb: info.map(|i| texture_serve::is_srgb(i.format)).unwrap_or(false),
            resolved: info.is_some(),
        });
    }

    Ok(ResolvedMaterial {
        template: template_path,
        textures,
    })
}

/// Resolve every material slot of one model.
///
/// Never fails on a per-slot basis: a slot whose material is missing from the
/// archives comes back `resolved: false, placeholder: true` with no textures,
/// which is the viewport's cue to draw its neutral debug colour.
pub fn resolve_model(index: &LevelIndex, model_id: u64) -> Result<ModelMaterials> {
    let raw = index.extract(model_id)?;
    let model = level::parse_model_asset(&raw)?;

    let Some(section) = model.dat1.get_section_data(TAG_MATERIAL) else {
        return Ok(ModelMaterials {
            model_id: hex_id(model_id),
            slots: Vec::new(),
        });
    };
    let table = MaterialSection::parse(section)?;

    let mut slots = Vec::with_capacity(table.slots.len());
    for (i, slot) in table.slots.iter().enumerate() {
        let material_path = model
            .dat1
            .get_string(slot.path_offset as u32)
            .map(|s| crc64::normalize_path(&s))
            .unwrap_or_default();
        let mapping_name = model
            .dat1
            .get_string(slot.name_offset as u32)
            .unwrap_or_default();

        // The path is the identity: hashing it gives the TOC id directly. The
        // `ids` table is sorted by the mapping-name hash, not parallel to the
        // slot array, so it is consulted by name and only as a fallback.
        let by_path = if material_path.is_empty() {
            0
        } else {
            crc64::hash_raw(material_path.as_bytes())
        };
        let by_table = table
            .id_for_name(&mapping_name)
            .map(|e| e.asset_id)
            .unwrap_or(0);
        let material_id = if by_path != 0 && index.asset(by_path).is_some() {
            by_path
        } else if by_table != 0 && index.asset(by_table).is_some() {
            by_table
        } else if by_path != 0 {
            by_path
        } else {
            by_table
        };

        let known_placeholder = is_placeholder_material(&material_path);
        let resolved = material_id != 0 && index.asset(material_id).is_some();

        let (template, textures) = if resolved && !known_placeholder {
            match resolve_material(index, material_id) {
                Ok(r) => (r.template, r.textures),
                Err(e) => {
                    debug!("material {material_id:016X} ({material_path}): {e}");
                    (String::new(), Vec::new())
                }
            }
        } else {
            (String::new(), Vec::new())
        };

        // A slot that binds nothing usable is as good as a placeholder to the
        // viewport: 27% of shipped materials bind only placeholder textures.
        let all_placeholder = !textures.is_empty()
            && textures
                .iter()
                .all(|t| t.placeholder || !t.resolved);
        let placeholder =
            known_placeholder || !resolved || textures.is_empty() || all_placeholder;

        bump(|c| {
            c.slots += 1;
            if resolved {
                c.slots_resolved += 1;
            }
            if placeholder {
                c.slots_placeholder += 1;
            }
        });

        slots.push(MaterialSlot {
            index: i as u32,
            material_id: if material_id == 0 {
                "0x0".to_string()
            } else {
                hex_id(material_id)
            },
            material_path,
            mapping_name,
            template,
            resolved,
            placeholder,
            textures,
        });
    }

    Ok(ModelMaterials {
        model_id: hex_id(model_id),
        slots,
    })
}

// ---------------------------------------------------------------------------
// Model LRU
// ---------------------------------------------------------------------------

struct ModelCache {
    map: DashMap<u64, Arc<ModelMaterials>>,
    order: Mutex<Vec<u64>>,
    hits: AtomicUsize,
}

fn model_cache() -> &'static ModelCache {
    static CACHE: OnceLock<ModelCache> = OnceLock::new();
    CACHE.get_or_init(|| ModelCache {
        map: DashMap::new(),
        order: Mutex::new(Vec::new()),
        hits: AtomicUsize::new(0),
    })
}

/// Resolved slots for a model, computed at most once per id.
///
/// Never fails: a model that cannot be extracted or parsed yields an empty slot
/// list rather than an error, so one bad asset does not abort a zone's loop.
pub fn model_materials(index: &LevelIndex, model_id: u64) -> Arc<ModelMaterials> {
    let cache = model_cache();
    if let Some(hit) = cache.map.get(&model_id) {
        cache.hits.fetch_add(1, Ordering::Relaxed);
        return Arc::clone(&hit);
    }

    let resolved = resolve_model(index, model_id).unwrap_or_else(|e| {
        debug!("model {model_id:016X} materials: {e}");
        ModelMaterials {
            model_id: hex_id(model_id),
            slots: Vec::new(),
        }
    });

    let arc = Arc::new(resolved);
    if cache.map.insert(model_id, Arc::clone(&arc)).is_none() {
        let mut order = cache.order.lock().unwrap();
        order.push(model_id);
        while order.len() > MODEL_CACHE_CAP {
            let victim = order.remove(0);
            cache.map.remove(&victim);
        }
    }
    arc
}

/// `(entries, hits)` of the resolved-model cache, for diagnostics.
pub fn model_cache_stats() -> (usize, usize) {
    let c = model_cache();
    (c.map.len(), c.hits.load(Ordering::Relaxed))
}

/// Parse an id as produced by [`hex_id`]; the `0x` prefix is optional.
pub fn parse_hex_id(s: &str) -> Result<u64> {
    let t = s.trim();
    let t = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    u64::from_str_radix(t, 16)
        .map_err(|e| ToolkitError::Parse(format!("invalid asset id {s:?}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffixes_name_the_semantic() {
        assert_eq!(
            semantic_of_path("textures/default_pink_shader/default_pink_shader_c.texture"),
            Some("albedo")
        );
        assert_eq!(semantic_of_path("a/b/solar_panels_002_n.texture"), Some("normal"));
        assert_eq!(semantic_of_path("a/b/solar_panels_002_g.texture"), Some("roughness"));
        assert_eq!(semantic_of_path("a/b/default_pink_mp.texture"), Some("mask"));
        assert_eq!(semantic_of_path("a/b/mo_white_h.texture"), Some("height"));
        assert_eq!(semantic_of_path("a/b/meg_arch_trim_02_id.texture"), Some("id"));
        assert_eq!(semantic_of_path("a/b/trim_master_001.texture"), None);
    }

    #[test]
    fn placeholders_are_recognised_on_both_sides() {
        assert!(is_placeholder_material(
            "materials/gl_default_grid/grid_light_pink.material"
        ));
        assert!(is_placeholder_texture(
            "textures/default_pink_shader/default_pink_shader_c.texture"
        ));
        assert!(is_placeholder_texture(
            "textures/environment/global/masks/mo_gbl_masks/mo_white_h.texture"
        ));
        assert!(!is_placeholder_material(
            "materials/environment/megalopolis/meg_arch_steel_trim_001.material"
        ));
        assert!(!is_placeholder_texture("textures/sbs/metal/steel_001_c.texture"));
    }

    #[test]
    fn doubled_separators_collapse_to_the_shipped_id() {
        // Authored paths sometimes carry `\\`; only the collapsed single-slash
        // form is in the TOC, so both spellings must hash the same.
        let a = crc64::normalize_path(r"textures\\default_pink_shader\\x.texture");
        let b = crc64::normalize_path("textures/default_pink_shader/x.texture");
        assert_eq!(a, b);
        assert_eq!(crc64::hash_raw(a.as_bytes()), crc64::hash_raw(b.as_bytes()));
    }

    #[test]
    fn hex_ids_round_trip() {
        let id = 0x8123_4567_89AB_CDEFu64;
        assert_eq!(parse_hex_id(&hex_id(id)).unwrap(), id);
        assert_eq!(parse_hex_id("0x0").unwrap(), 0);
    }

    #[test]
    fn known_slot_names_fold_into_the_vocabulary() {
        assert_eq!(semantic_of_known_name("BaseMap2D_Texture"), Some("albedo"));
        assert_eq!(semantic_of_known_name("NormalMap2D_Texture"), Some("normal"));
        assert_eq!(semantic_of_known_name("Glossiness"), None);
    }
}
