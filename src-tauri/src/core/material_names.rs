//! CRC32 name lookup for material constant / sampler slots.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::core::crc32;
use crate::core::filesystem;

/// Names confirmed by hashing candidate strings against shipped material assets.
const SEED_NAMES: &[&str] = &[
    "BaseMap2D_Texture",
    "NormalMap2D_Texture",
    "GlossMap2D_Texture",
    "MaskMap2D_Texture",
    "Color",
    "Colour",
    "DiffuseColor",
    "EmissiveColor",
    "Emissive",
    "Glossiness",
    "Ambient_Occlusion",
    "Spec_Reflectance",
    "Specular_Reflectance",
    "Light_Wrap",
    "Displacement_Strength",
    "Distress_Tiling",
    "Health",
    "kNone",
    "kMetalMedium",
];

/// Optional user dictionary: one candidate name per line, `#` starts a comment.
pub const USER_DICTIONARY_FILE: &str = "material_names.txt";

fn table() -> &'static HashMap<u32, String> {
    static TABLE: OnceLock<HashMap<u32, String>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut map = HashMap::new();
        for name in SEED_NAMES {
            map.insert(crc32::hash(name), (*name).to_string());
        }
        if let Ok(path) = filesystem::app_dir().map(|d| d.join(USER_DICTIONARY_FILE)) {
            if let Ok(text) = std::fs::read_to_string(path) {
                for line in text.lines() {
                    let name = line.split('#').next().unwrap_or("").trim();
                    if !name.is_empty() {
                        map.insert(crc32::hash(name), name.to_string());
                    }
                }
            }
        }
        map
    })
}

pub fn resolve(hash: u32) -> Option<String> {
    table().get(&hash).cloned()
}

pub fn label(hash: u32) -> String {
    resolve(hash).unwrap_or_else(|| format!("{hash:#010X}"))
}
