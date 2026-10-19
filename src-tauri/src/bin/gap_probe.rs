//! What a level editor still cannot see: a census of every zone DAT1 section
//! the backend does not read, plus the asset classes it has no decoder for.
//!
//!   cargo run --release --bin gap_probe -- "<game dir>"
//!
//! The "read" set is the list of `pub const T_*` section tags declared across
//! `core/`, which is exactly what the decoders address. Anything present in a
//! shipped zone and absent from that list is unread by definition, and the
//! byte totals say how much of the level is dark.
//!
//! Read-only. Nothing is written to the game directory.

use std::collections::{BTreeMap, HashMap};

use omnitool_lib::core::level;
use omnitool_lib::core::zone::Zone;

/// Section tags some decoder in `core/` addresses, harvested from the
/// `pub const T_*` declarations. Regenerate with:
///   grep -rhE "pub const T_[A-Z_0-9]+: u32 = 0x[0-9A-Fa-f_]+;" src/core/*.rs
const READ_TAGS: &[(u32, &str)] = &[
    (0x0277_95C5, "TAG_ENVPROBE_ASSETS"),
    (0x0410_71EF, "TAG_ACTOR_GROUP_MEMBERS"),
    (0x04C1_9E69, "TAG_NAV_CONTAINER"),
    (0x06AB_CAB2, "TAG_SCENE_OBJECTS"),
    (0x07C7_5341, "TAG_HIBERNATE_RAW_DATA"),
    (0x0CF5_8A6E, "TAG_ACTOR_GROUPS"),
    (0x101A_2196, "TAG_LIGHT_BIN_HEADER_DATA"),
    (0x13F4_AF3B, "TAG_LIGHT_GRID_SAMPLE_DATA"),
    (0x1951_BA1F, "TAG_LEVEL_LIGHT_NAME"),
    (0x1999_D0E3, "TAG_LIGHTING_CONDITION_REFS"),
    (0x2236_C47A, "TAG_LINK_NAMES"),
    (0x2300_D240, "TAG_SCRIPT_PRIUSES"),
    (0x2720_4B67, "TAG_LIGHT_BIN_UNNAMED"),
    (0x2BA3_3702, "TAG_ZONE_NAMES"),
    (0x3395_AEC1, "TAG_LINKS_BUILT"),
    (0x339C_970E, "TAG_REGION_GAME_DATA"),
    (0x396F_9418, "TAG_REGIONS_BUILT"),
    (0x3D5E_2FEF, "TAG_VFX_NAMES"),
    (0x4130_D903, "TAG_REGION_NAMES"),
    (0x4188_7FB3, "TAG_REGION_GAME_STRUCT"),
    (0x4E02_3760, "TAG_ZONES_BUILT"),
    (0x50ED_C53D, "TAG_ACTOR_PRIUSES"),
    (0x5570_13E9, "TAG_ENVPROBE_LIST"),
    (0x5736_A46F, "TAG_VERTEX_PAINT_INFO"),
    (0x57D2_5F50, "TAG_LIGHT_GRID_POSITIONS"),
    (0x5E54_ACCF, "TAG_SCRIPT_ACTIONS"),
    (0x611F_490D, "TAG_HIBERNATE_DATA_DESC"),
    (0x6251_A0BF, "TAG_HIBERNATE_VFX_NAMES"),
    (0x6575_12BB, "TAG_DECAL_GEOMETRY"),
    (0x6987_F172, "TAG_MODEL_INSTS"),
    (0x7068_2CB8, "TAG_ACTORS"),
    (0x7077_E5F5, "TAG_HIBERNATE_MODEL_NAMES"),
    (0x758B_AFBD, "TAG_ACTOR_VERTEX_PAINT_INFO"),
    (0x7868_4035, "TAG_ACTOR_ASSETS"),
    (0x7A60_75BE, "TAG_LEVEL_LIGHT_NAME_UNUSED"),
    (0x7CA7_267D, "TAG_LEVEL_BUILT"),
    (0x80D2_9828, "TAG_SCRIPT_STRING_HASHES"),
    (0x8199_9057, "TAG_ACTOR_PRIUS_POOL"),
    (0x927C_4EC3, "TAG_VFX_INSTS"),
    (0x95F9_1E24, "TAG_ZONE_REFERENCES"),
    (0x97FF_6EB5, "TAG_LIGHTS"),
    (0x9CCA_A06F, "TAG_VERTEX_PAINT_COLOR"),
    (0xA903_D8F1, "TAG_NAV_TILECACHE"),
    (0xB6A0_B72A, "TAG_ATMOSPHERE_NAME"),
    (0xBCF4_3558, "TAG_DECALS"),
    (0xBDAB_2B0D, "TAG_VOLUMES"),
    (0xBEAB_52E7, "TAG_SCRIPT_PLUGS"),
    (0xC30D_92B6, "TAG_REGION_LOD_BUILT"),
    (0xC496_8A44, "TAG_ACTOR_GROUP_NAMES"),
    (0xC6A5_905E, "TAG_MODEL_NAMES"),
    (0xC72A_514C, "TAG_LIGHT_GRID_DATA_OFFSETS"),
    (0xD124_430B, "TAG_NAV_UNIT"),
    (0xD266_157E, "TAG_SCRIPT_HASHES"),
    (0xD86A_7934, "TAG_SCRIPT_VARS"),
    (0xDA9E_4CDF, "TAG_ENVPROBE_DRAW_LISTS"),
    (0xDC31_1FC3, "TAG_IMPOSTORS"),
    (0xDC62_5B3D, "TAG_ACTOR_NAMES"),
    (0xE14D_4601, "TAG_LIGHT_BIN_ASSET_UNUSED"),
    (0xEDFA_607D, "TAG_LIGHT_BIN_ASSET"),
    (0xEF86_37D5, "TAG_SCRIPT_STRINGS"),
    (0xF435_AE9C, "TAG_ACTOR_VERTEX_PAINT_COLOR"),
];

fn main() {
    let game = std::env::args()
        .nth(1)
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());

    let idx = level::get_index(&game, None).expect("level index");
    let read: HashMap<u32, &str> = READ_TAGS.iter().copied().collect();

    // --- asset class census over the whole TOC ---------------------------
    let mut ext: BTreeMap<String, usize> = BTreeMap::new();
    for name in idx.names.values() {
        let e = name
            .rsplit_once('.')
            .map(|(_, e)| format!(".{}", e.to_ascii_lowercase()))
            .unwrap_or_else(|| "<no extension>".into());
        *ext.entry(e).or_default() += 1;
    }

    // --- zone section census --------------------------------------------
    // tag -> (zones containing it, total bytes)
    let mut census: BTreeMap<u32, (u64, u64)> = BTreeMap::new();
    let mut zones_ok = 0u64;
    for z in idx.zones.iter() {
        let Ok(bytes) = idx.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&bytes) else { continue };
        zones_ok += 1;
        for sh in &zone.dat1.sections {
            let e = census.entry(sh.tag).or_insert((0, 0));
            e.0 += 1;
            e.1 += sh.size as u64;
        }
    }

    println!("game  : {game}");
    println!("zones parsed: {zones_ok} of {}\n", idx.zones.len());

    println!("=== asset classes in the TOC (top 40 by count) ===");
    let mut ev: Vec<_> = ext.iter().collect();
    ev.sort_by(|a, b| b.1.cmp(a.1));
    for (e, n) in ev.iter().take(40) {
        println!("  {n:>7}  {e}");
    }
    for want in [
        ".conduit",
        ".nodegraph",
        ".scriptgraph",
        ".zone",
        ".level",
        ".actor",
        ".modelvariant",
        ".dynamicenum",
        ".breakable",
        ".terrain",
    ] {
        println!(
            "  [{}] {want}",
            ext.get(want).map(|n| n.to_string()).unwrap_or("0".into())
        );
    }

    println!("\n=== zone sections NOBODY READS, by bytes ===");
    let mut unread: Vec<(u32, u64, u64)> = census
        .iter()
        .filter(|(t, _)| !read.contains_key(t))
        .map(|(t, (z, b))| (*t, *z, *b))
        .collect();
    unread.sort_by(|a, b| b.2.cmp(&a.2));
    let total_unread: u64 = unread.iter().map(|u| u.2).sum();
    let total_all: u64 = census.values().map(|v| v.1).sum();
    println!(
        "  {} distinct unread tags, {total_unread} bytes of {total_all} ({:.1}% of zone bytes)\n",
        unread.len(),
        100.0 * total_unread as f64 / total_all.max(1) as f64
    );
    println!("  {:>10}  {:>6}  {:>13}  name", "tag", "zones", "bytes");
    for (t, z, b) in unread.iter().take(45) {
        let name = level::zone_section_name(*t).unwrap_or("<unnamed>");
        println!("  {t:#010X}  {z:>6}  {b:>13}  {name}");
    }

    println!("\n=== sections we DO read, for scale ===");
    let mut rd: Vec<(u32, u64, u64)> = census
        .iter()
        .filter(|(t, _)| read.contains_key(t))
        .map(|(t, (z, b))| (*t, *z, *b))
        .collect();
    rd.sort_by(|a, b| b.2.cmp(&a.2));
    println!("  {} distinct read tags", rd.len());
    for (t, z, b) in rd.iter().take(8) {
        println!(
            "  {t:#010X}  {z:>6}  {b:>13}  {}",
            level::zone_section_name(*t).unwrap_or("<unnamed>")
        );
    }

    // Any declared-read tag that never actually ships?
    let never: Vec<&str> = READ_TAGS
        .iter()
        .filter(|(t, _)| !census.contains_key(t))
        .map(|(_, n)| *n)
        .collect();
    println!(
        "\n  tags the code reads that ship in 0 zones ({}): {}",
        never.len(),
        never.join(", ")
    );
}
