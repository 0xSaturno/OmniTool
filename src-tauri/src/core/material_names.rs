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
    "sampler1D",
    "sampler2D",
    "sampler3D",
    "samplerCUBE",
];

/// Physics A/V material names, from the CC0 Rivet DDL dump (`rcra_ddl.json`).
pub const AV_MATERIALS: &[&str] = &[
    "kNone", "kAcid", "kAsphalt", "kBlood", "kBuilding", "kCanvas", "kCarpet",
    "kCarpetIndustrial", "kCarpetSoft", "kConcrete", "kConcreteDirty", "kConcreteWet", "kCloud",
    "kDirt", "kEnergy", "kFlesh", "kFleshExotic", "kFoliage", "kFoliageThick",
    "kGenericManMade", "kGlassBroken", "kGlassExoticUnbreakable", "kGlassMedium", "kGlassThick",
    "kGlassThin", "kGlassUnbreakable", "kGrass", "kGrassThick", "kGravel", "kGravelWet",
    "kGrindRail", "kHoverboardAutoKill", "kIce", "kIceThick", "kIceThin", "kIceWall",
    "kLavaActive", "kLavaLazy", "kMagBoot", "kMetalCable", "kMetalGrate", "kMetalHollow",
    "kMetalMedium", "kMetalPipe", "kMetalThick", "kMetalThin", "kMetalWet", "kMetalFloorGrate",
    "kMetalFloorHollow", "kMetalFloorMedium", "kMetalFloorThick", "kMetalFloorThin", "kMud",
    "kMudWet", "kNarrowPlatform", "kOil", "kPaper", "kPlasticHard", "kPlasticSoft", "kRubber",
    "kSand", "kSnow", "kSnowDeep", "kSnowHard", "kStoneBrittle", "kStoneMedium", "kStoneSolid",
    "kTar", "kTarp", "kVinyl", "kWaterAnkleHard", "kWaterAnkleSoft", "kWaterDeep", "kWaterfall",
    "kWaterOcean", "kWaterPuddleHard", "kWaterPuddleSoft", "kWaterWaistDeep", "kWoodCreaky",
    "kWoodHard", "kWoodHollow", "kWoodMedium", "kWoodThick", "kWoodThin",
];

pub fn av_material_name(hash: u32) -> Option<&'static str> {
    AV_MATERIALS.iter().copied().find(|n| crc32::hash(n) == hash)
}

/// Harvested slot names, compiled in. Same format as the user dictionary.
const BUNDLED_NAMES: &str = include_str!("material_names.txt");

/// Optional user dictionary: one candidate name per line, `#` starts a comment.
pub const USER_DICTIONARY_FILE: &str = "material_names.txt";

fn add_dictionary(map: &mut HashMap<u32, String>, text: &str) {
    for line in text.lines() {
        let name = line.split('#').next().unwrap_or("").trim();
        if !name.is_empty() {
            map.insert(crc32::hash(name), name.to_string());
        }
    }
}

fn table() -> &'static HashMap<u32, String> {
    static TABLE: OnceLock<HashMap<u32, String>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut map = HashMap::new();
        for name in SEED_NAMES.iter().chain(AV_MATERIALS) {
            map.insert(crc32::hash(name), (*name).to_string());
        }
        add_dictionary(&mut map, BUNDLED_NAMES);
        if let Ok(path) = filesystem::app_dir().map(|d| d.join(USER_DICTIONARY_FILE)) {
            if let Ok(text) = std::fs::read_to_string(path) {
                add_dictionary(&mut map, &text);
            }
        }
        map
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_names_include_the_palette_constants() {
        let mut map = HashMap::new();
        add_dictionary(&mut map, BUNDLED_NAMES);
        for (hash, name) in [(0x2F53_BB17, "Color_ID_Primary"), (0xA9C2_766A, "Color_ID_Emissive"), (0x7107_A2D8, "GlossMap2D_UseColor")] {
            assert_eq!(map.get(&hash).map(String::as_str), Some(name));
        }
    }
}

pub fn resolve(hash: u32) -> Option<String> {
    table().get(&hash).cloned()
}

pub fn label(hash: u32) -> String {
    resolve(hash).unwrap_or_else(|| format!("{hash:#010X}"))
}
