//! Verification harness for `core::zone` against the shipped game files.
//!
//!   cargo run --bin zone_check -- "F:/SteamLibrary/steamapps/common/Ratchet & Clank - Rift Apart"
//!
//! Checks, on real zones of every kind:
//!   * the asset header / block split and the DAT1 type id
//!   * scene-object heap traversal and kind census
//!   * model-instance resolution (model index -> Zone Model Names -> a TOC asset)
//!   * actor rows, names and asset refs
//!   * byte-identical re-serialisation of an untouched zone
//!   * an in-place transform edit followed by undo restoring the original bytes
//!
//! Then a survey over a few thousand shipped zones, which is where the numbers
//! quoted in `core::zone`'s comments come from. It asserts, and prints the
//! distribution of:
//!   * actor flag words and the padding half-word behind them
//!   * the placed bit (63) on actor uids and scene-object instance ids
//!   * actor-group slices: members in range, ranges chaining, no orphan slot
//!   * script-action plug slices tiling Zone Script Plugs, prius blobs in range,
//!     and `crc32(action name) == action_name_hash`
//!   * vertex-paint rows: uid resolves to a model instance, count equals that
//!     model's vertex count, runs tile the colour section

use omnitool_lib::core::level::{self, LevelIndex};
use omnitool_lib::core::toc::Toc;
use omnitool_lib::core::zone::{
    actor_flag_names, kind_name, Zone, ACTOR_FLAG_NAMES, KIND_MODEL, T_ACTORS,
    NO_PLUG, T_ACTOR_GROUP_DATA, T_ACTOR_GROUP_NAMES, T_SCRIPT_ACTIONS, T_SCRIPT_PLUGS,
    T_SCRIPT_PRIUSES,
};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

const HASHES: &str = r"E:\RCRALevelEditorViewer\refs\RCRA-Forge\hashes.txt";
/// `Zone Script Strings`: a `u32[]` of string offsets the action rows index.
const T_SCRIPT_STRINGS: u32 = 0xEF86_37D5;

const ZONES: &[&str] = &[
    "levels/i29/openarea/sargasso/tile_n19/tile_n19_art.zone",
    "levels/i29/openarea/nefarious_city/tile_e18/tile_e18_art.zone",
    "levels/i29/openarea/megalopolis/tile_o37/tile_o37_art.zone",
    "levels/i29/instance/pocketdimensions/pocketdimension_10/pd_10_geo.zone",
    "levels/i29/openarea/zordoom/tile_e22/tile_e22_gp.zone",
    "levels/i29/openarea/sargasso/tile_n19/tile_n19_gp_crates.zone",
    "levels/i29/openarea/blizar_intact/tile_b15/tile_b15_vfx.zone",
    "levels/i29/openarea/megalopolis/tile_q31/tile_q31_lgt.zone",
];

fn crc64(path: &str) -> u64 {
    omnitool_lib::core::crc64::hash(path)
}

fn main() {
    let game = std::env::args()
        .nth(1)
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let game = PathBuf::from(game);
    let toc_bytes = std::fs::read(game.join("toc")).expect("read toc");
    let toc = Toc::parse(&toc_bytes).expect("parse toc");
    // TOC archive names already include the "d\" prefix, so the archive root is
    // the game directory itself.
    let archives: &Path = &game;
    println!("toc: {} assets, {} archives\n", toc.asset_ids().len(), toc.archive_count());

    let assets = toc.assets();
    let by_id: BTreeMap<u64, _> = assets.iter().map(|a| (a.asset_id, a.clone())).collect();

    let mut failures = 0usize;
    for path in ZONES {
        let id = crc64(path);
        let Some(asset) = by_id.get(&id) else {
            println!("MISSING  {path}");
            failures += 1;
            continue;
        };
        let raw = match toc.extract_asset(asset, archives) {
            Ok(r) => r,
            Err(e) => {
                println!("EXTRACT FAIL {path}: {e}");
                failures += 1;
                continue;
            }
        };
        let mut zone = match Zone::parse(&raw) {
            Ok(z) => z,
            Err(e) => {
                println!("PARSE FAIL {path}: {e}");
                failures += 1;
                continue;
            }
        };

        let mut kinds: BTreeMap<u8, usize> = BTreeMap::new();
        for o in &zone.scene_objects {
            *kinds.entry(o.kind).or_default() += 1;
        }
        let resolved = zone.model_instances().count();
        let unresolved = zone
            .scene_objects
            .iter()
            .filter(|o| o.kind == KIND_MODEL && o.model_id == 0)
            .count();
        let in_toc = zone
            .model_instances()
            .filter(|o| by_id.contains_key(&o.model_id))
            .count();

        let short = path.rsplit('/').next().unwrap();
        println!("{short}");
        println!(
            "   blocks {:?}  sections {}  objects {}  actors {}  models(table) {}",
            zone.sizes,
            zone.dat1.sections.len(),
            zone.scene_objects.len(),
            zone.actors.len(),
            zone.model_ids.len()
        );
        let census: Vec<String> = kinds
            .iter()
            .map(|(k, n)| format!("{}={}", kind_name(*k), n))
            .collect();
        println!("   kinds: {}", census.join(" "));
        if resolved + unresolved > 0 {
            println!(
                "   model instances: {resolved} resolved ({in_toc} present in TOC), {unresolved} unresolved"
            );
            if resolved != in_toc {
                println!("   !! some resolved ids are not TOC assets");
                failures += 1;
            }
        }
        for a in zone.actors.iter().take(3) {
            println!(
                "   actor[{}] {:?} scene@{} id {:#018X} ref {}",
                a.index,
                a.name,
                a.scene_offset,
                a.instance_id,
                if a.asset_path.is_empty() { "-" } else { &a.asset_path }
            );
        }
        if let Some((min, max)) = zone.bounds() {
            println!(
                "   bounds  x[{:.1},{:.1}] y[{:.1},{:.1}] z[{:.1},{:.1}]",
                min[0], max[0], min[1], max[1], min[2], max[2]
            );
        }

        // round trip
        let rebuilt = zone.to_bytes();
        if rebuilt == raw {
            println!("   round trip: byte-identical ({} bytes)", rebuilt.len());
        } else {
            println!(
                "   !! round trip DIFFERS: {} vs {} bytes",
                rebuilt.len(),
                raw.len()
            );
            let n = rebuilt.len().min(raw.len());
            if let Some(i) = (0..n).find(|&i| rebuilt[i] != raw[i]) {
                println!("      first difference at byte {i}");
            }
            failures += 1;
        }

        // edit + undo
        if let Some(first) = zone.scene_objects.first().map(|o| (o.offset, o.matrix)) {
            let (off, m) = first;
            let mut moved = m;
            moved[12] += 1.0;
            zone.set_matrix(off, moved).expect("set matrix");
            let edited = zone.to_bytes();
            let changed = edited.iter().zip(raw.iter()).filter(|(a, b)| a != b).count();
            zone.undo();
            let restored = zone.to_bytes();
            println!(
                "   edit +1m on object @{off}: {changed} bytes differ; undo restores original: {}",
                restored == raw
            );
            if restored != raw || changed == 0 || changed > 4 {
                println!("   !! unexpected edit/undo behaviour");
                failures += 1;
            }
        }
        println!();
    }

    let hashes = std::env::args().nth(2).unwrap_or_else(|| HASHES.to_string());
    match level::get_index(&game.to_string_lossy(), Some(&hashes)) {
        Ok(index) => failures += survey(&index),
        Err(e) => println!("survey skipped (no level index: {e})"),
    }

    if failures == 0 {
        println!("all checks passed");
    } else {
        println!("{failures} check(s) FAILED");
        std::process::exit(1);
    }
}

/// Running tallies of the structure checks, over every surveyed zone.
#[derive(Default)]
struct Survey {
    zones: usize,
    // actors
    actors: usize,
    flag_hist: BTreeMap<u16, usize>,
    flag_bits: [usize; 16],
    actor_pad_nonzero: usize,
    actor_name_oob: usize,
    actor_asset_oob: usize,
    actor_prius_oob: usize,
    actor_uid_placed: usize,
    actor_uid_zero: usize,
    actor_scene_oob: usize,
    // scene objects
    objects: usize,
    obj_placed: usize,
    obj_id_zero: usize,
    obj_by_kind: BTreeMap<u8, usize>,
    obj_unplaced_by_kind: BTreeMap<u8, usize>,
    obj_zero_by_kind: BTreeMap<u8, usize>,
    // groups
    zones_with_groups: usize,
    groups: usize,
    group_named: usize,
    group_name_is_row: usize,
    group_global: usize,
    group_color_hist: BTreeMap<u32, usize>,
    group_members: usize,
    group_member_oob: usize,
    group_range_oob: usize,
    group_chain_breaks: usize,
    group_data_unclaimed: usize,
    group_uid_placed: usize,
    // script actions
    zones_with_scripts: usize,
    actions: usize,
    action_in_hist: BTreeMap<u16, usize>,
    action_out_oob: usize,
    action_var_oob: usize,
    action_name_oob: usize,
    action_name_hash_ok: usize,
    action_name_hash_checked: usize,
    action_prius_oob: usize,
    action_plug_overlap: usize,
    action_plug_none: usize,
    action_plug_empty: usize,
    action_plug_sentinel_count: usize,
    plug_rows: usize,
    plug_unclaimed: usize,
    // vertex paint
    zones_with_paint: usize,
    paint_rows: usize,
    paint_actor_rows: usize,
    paint_uid_matched: usize,
    paint_uid_model: usize,
    paint_span_oob: usize,
    paint_vertices: usize,
    paint_color_bytes: usize,
    paint_covered_bytes: usize,
    paint_count_is_model_verts: usize,
    paint_count_mismatch: usize,
}

fn u16s(b: &[u8]) -> Vec<u16> {
    b.chunks_exact(2)
        .map(|c| u16::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn u32s(b: &[u8]) -> Vec<u32> {
    b.chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

/// The vertex count a model's own `Model Built` header declares, memoised.
fn model_vertex_count(
    index: &LevelIndex,
    model_id: u64,
    cache: &mut std::collections::HashMap<u64, Option<u32>>,
) -> Option<u32> {
    if let Some(v) = cache.get(&model_id) {
        return *v;
    }
    let v = index
        .extract(model_id)
        .ok()
        .and_then(|raw| level::parse_model_asset(&raw).ok())
        .and_then(|m| {
            m.dat1
                .get_section_data(omnitool_lib::tools::model_converter::sections::built::TAG_BUILT)
                .and_then(omnitool_lib::tools::model_converter::sections::built::Built::parse)
                .map(|b| b.vertex_count)
        });
    cache.insert(model_id, v);
    v
}

/// Walk a large sample of shipped zones and measure every structure this module
/// parses. Everything printed here is a count taken from the PC game files.
fn survey(index: &LevelIndex) -> usize {
    let mut s = Survey::default();
    let model_verts = &mut std::collections::HashMap::new();
    let mut failures = 0usize;
    // Sample broadly but stay inside a couple of minutes: gameplay zones carry
    // the actors, scripts and groups, so take every one of those and step
    // through the rest.
    let mut picked: Vec<&level::ZoneRec> = Vec::new();
    for (i, z) in index.zones.iter().enumerate() {
        let interesting = z.kind == "gp" || z.kind == "other"
            || z.path.contains("_gp")
            || z.path.contains("_amb")
            || z.path.contains("_mis");
        if interesting || i % 7 == 0 {
            picked.push(z);
        }
    }
    println!("survey: {} of {} zones", picked.len(), index.zones.len());

    // Example rows, printed once, so the layout can be eyeballed against the
    // numbers rather than taken on faith.
    let mut shown_group = false;
    let mut shown_action = false;
    let mut shown_paint = false;

    for z in picked {
        let Ok(raw) = index.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&raw) else { continue };
        s.zones += 1;

        // ---- actors ----------------------------------------------------------------
        let actor_rows = zone
            .dat1
            .get_section_data(T_ACTORS)
            .map(|a| a.len() / 32)
            .unwrap_or(0);
        let name_count = zone
            .dat1
            .get_section_data(omnitool_lib::core::zone::T_ACTOR_NAMES)
            .map(|a| a.len() / 4)
            .unwrap_or(0);
        let asset_count = zone
            .dat1
            .get_section_data(omnitool_lib::core::zone::T_ACTOR_REFS)
            .map(|a| a.len() / 12)
            .unwrap_or(0);
        let prius_rows = zone
            .dat1
            .get_section_data(omnitool_lib::core::zone::T_ACTOR_PRIUSES)
            .map(|a| a.len() / 32)
            .unwrap_or(0);
        let scene_len = zone
            .dat1
            .get_section_data(omnitool_lib::core::zone::T_SCENE_OBJECTS)
            .map(<[u8]>::len)
            .unwrap_or(0);
        for a in &zone.actors {
            s.actors += 1;
            *s.flag_hist.entry(a.flags).or_default() += 1;
            for b in 0..16 {
                if a.flags & (1 << b) != 0 {
                    s.flag_bits[b] += 1;
                }
            }
            if a.pad != 0 {
                s.actor_pad_nonzero += 1;
            }
            if a.name_index < 0 || a.name_index as usize >= name_count {
                s.actor_name_oob += 1;
            }
            if a.asset_index < 0 || a.asset_index as usize >= asset_count {
                s.actor_asset_oob += 1;
            }
            if a.prius_count > 0
                && (a.prius_index < 0
                    || (a.prius_index + a.prius_count) as usize > prius_rows)
            {
                s.actor_prius_oob += 1;
            }
            if a.scene_offset as usize + 0x80 > scene_len {
                s.actor_scene_oob += 1;
            }
            if a.is_placed {
                s.actor_uid_placed += 1;
            }
            if a.instance_id == 0 {
                s.actor_uid_zero += 1;
            }
        }
        if actor_rows != zone.actors.len() {
            println!("   !! {} actor rows {} parsed {}", z.path, actor_rows, zone.actors.len());
            failures += 1;
        }

        // ---- scene-object ids ------------------------------------------------------
        for o in &zone.scene_objects {
            s.objects += 1;
            if o.instance_id == 0 {
                s.obj_id_zero += 1;
                *s.obj_zero_by_kind.entry(o.kind).or_default() += 1;
            } else if o.is_placed {
                s.obj_placed += 1;
            } else {
                *s.obj_unplaced_by_kind.entry(o.kind).or_default() += 1;
            }
            *s.obj_by_kind.entry(o.kind).or_default() += 1;
        }

        // ---- actor groups ----------------------------------------------------------
        if !zone.actor_groups.is_empty() {
            s.zones_with_groups += 1;
            let data_len = zone
                .dat1
                .get_section_data(T_ACTOR_GROUP_DATA)
                .map(|d| d.len() / 2)
                .unwrap_or(0);
            let name_count = zone
                .dat1
                .get_section_data(T_ACTOR_GROUP_NAMES)
                .map(|d| d.len() / 4)
                .unwrap_or(0);
            let mut claimed = vec![false; data_len];
            let mut next = 0i64;
            for g in &zone.actor_groups {
                s.groups += 1;
                if g.name_index >= 0 {
                    s.group_named += 1;
                    if g.name_index as u32 == g.index {
                        s.group_name_is_row += 1;
                    }
                    if g.name_index as usize >= name_count {
                        s.group_range_oob += 1;
                    }
                }
                if g.is_global {
                    s.group_global += 1;
                }
                if g.is_placed {
                    s.group_uid_placed += 1;
                }
                *s.group_color_hist.entry(g.debug_color).or_default() += 1;
                s.group_members += g.actor_count as usize;
                let start = g.first_actor_index;
                if start < 0 || start as usize + g.actor_count as usize > data_len {
                    s.group_range_oob += 1;
                } else {
                    if start as i64 != next {
                        s.group_chain_breaks += 1;
                    }
                    next = start as i64 + g.actor_count as i64;
                    for i in start as usize..start as usize + g.actor_count as usize {
                        claimed[i] = true;
                    }
                }
                if g.members.len() != g.actor_count as usize {
                    s.group_range_oob += 1;
                }
                for &m in &g.members {
                    if m as usize >= zone.actors.len() {
                        s.group_member_oob += 1;
                    }
                }
            }
            s.group_data_unclaimed += claimed.iter().filter(|c| !**c).count();
            if !shown_group {
                shown_group = true;
                println!("   groups sample from {}", z.path);
                for g in zone.actor_groups.iter().take(6) {
                    println!(
                        "      [{}] name_index {} {:?} first {} count {} global {} color {:#010X} uid {:#018X} members {:?}",
                        g.index, g.name_index, g.name, g.first_actor_index, g.actor_count,
                        g.is_global, g.debug_color, g.uid,
                        &g.members[..g.members.len().min(8)]
                    );
                }
            }
        }

        // ---- script actions --------------------------------------------------------
        if !zone.script_actions.is_empty() {
            s.zones_with_scripts += 1;
            let plug_rows = zone
                .dat1
                .get_section_data(T_SCRIPT_PLUGS)
                .map(|d| d.len() / 12)
                .unwrap_or(0);
            let prius_len = zone
                .dat1
                .get_section_data(T_SCRIPT_PRIUSES)
                .map(<[u8]>::len)
                .unwrap_or(0);
            let strings = zone
                .dat1
                .get_section_data(T_SCRIPT_STRINGS)
                .map(u32s)
                .unwrap_or_default();
            // Out and var plugs are slices of ONE array: they interleave, so the
            // test is that together they tile Zone Script Plugs without overlap.
            let mut claimed = vec![false; plug_rows];
            for a in &zone.script_actions {
                s.actions += 1;
                *s.action_in_hist.entry(a.in_plug_count).or_default() += 1;
                for (slice, index, count) in [
                    (a.out_plugs(), a.out_plug_index, a.out_plug_count),
                    (a.var_plugs(), a.var_plug_index, a.var_plug_count),
                ] {
                    if index == NO_PLUG {
                        s.action_plug_none += 1;
                        if count != 0 {
                            s.action_plug_sentinel_count += 1;
                        }
                        continue;
                    }
                    let Some(range) = slice else {
                        s.action_plug_empty += 1;
                        continue;
                    };
                    if range.end > plug_rows {
                        s.action_out_oob += 1;
                        continue;
                    }
                    for i in range {
                        if claimed[i] {
                            s.action_plug_overlap += 1;
                        }
                        claimed[i] = true;
                    }
                }
                if a.prius_offset as usize + a.prius_size as usize > prius_len {
                    s.action_prius_oob += 1;
                }
                if (a.action_name_index as usize) < strings.len() {
                    if let Some(name) = zone.dat1.get_string(strings[a.action_name_index as usize]) {
                        s.action_name_hash_checked += 1;
                        let h = omnitool_lib::core::crc32::hash(&name);
                        let hl = omnitool_lib::core::crc32::hash(&name.to_lowercase());
                        if h == a.action_name_hash || hl == a.action_name_hash {
                            s.action_name_hash_ok += 1;
                        }
                    }
                } else {
                    s.action_name_oob += 1;
                }
            }
            s.plug_rows += plug_rows;
            s.plug_unclaimed += claimed.iter().filter(|c| !**c).count();
            if !shown_action {
                shown_action = true;
                let raw = zone.dat1.get_section_data(T_SCRIPT_ACTIONS).unwrap();
                println!("   script actions sample from {} ({} strings, {} plug rows)", z.path, strings.len(), plug_rows);
                for (i, a) in zone.script_actions.iter().take(6).enumerate() {
                    let words = u16s(&raw[i * 40 + 16..i * 40 + 28]);
                    let name = strings
                        .get(a.action_name_index as usize)
                        .and_then(|&o| zone.dat1.get_string(o))
                        .unwrap_or_default();
                    println!(
                        "      [{}] uid {:#018X} graph {:#018X} u16s {:?} hash {:#010X} prius {}+{} name[{}] {:?}",
                        a.index, a.uid, a.graph_uid, words, a.action_name_hash,
                        a.prius_offset, a.prius_size, a.action_name_index, name
                    );
                }
            }
        }

        // ---- vertex paint ----------------------------------------------------------
        if !zone.vertex_paint.is_empty() {
            s.zones_with_paint += 1;
            let ids: HashSet<u64> = zone
                .scene_objects
                .iter()
                .filter(|o| o.instance_id != 0)
                .map(|o| o.instance_id)
                .collect();
            let model_ids: HashSet<u64> = zone
                .scene_objects
                .iter()
                .filter(|o| o.kind == KIND_MODEL && o.instance_id != 0)
                .map(|o| o.instance_id)
                .collect();
            for actor in [false, true] {
                let color_len = zone.vertex_paint_colors(actor).map(<[u8]>::len).unwrap_or(0);
                let mut covered = 0usize;
                for p in zone.vertex_paint.iter().filter(|p| p.actor == actor) {
                    s.paint_rows += 1;
                    if actor {
                        s.paint_actor_rows += 1;
                    }
                    if ids.contains(&p.uid) {
                        s.paint_uid_matched += 1;
                    }
                    if model_ids.contains(&p.uid) {
                        s.paint_uid_model += 1;
                        if let Some(obj) = zone.scene_objects.iter().find(|o| o.instance_id == p.uid)
                        {
                            match model_vertex_count(index, obj.model_id, model_verts) {
                                Some(v) if v as i32 == p.count => s.paint_count_is_model_verts += 1,
                                Some(_) => s.paint_count_mismatch += 1,
                                None => {}
                            }
                        }
                    }
                    let end = p.data_offset as usize + (p.count.max(0) as usize) * 4;
                    if p.count < 0 || end > color_len {
                        s.paint_span_oob += 1;
                    } else {
                        covered += p.count as usize * 4;
                    }
                    s.paint_vertices += p.count.max(0) as usize;
                }
                s.paint_color_bytes += color_len;
                s.paint_covered_bytes += covered;
            }
            if !shown_paint {
                shown_paint = true;
                println!(
                    "   vertex paint sample from {} ({} rows, color {} B)",
                    z.path,
                    zone.vertex_paint.len(),
                    zone.vertex_paint_colors(false).map(<[u8]>::len).unwrap_or(0)
                );
                for p in zone.vertex_paint.iter().take(6) {
                    let obj = zone.scene_objects.iter().find(|o| o.instance_id == p.uid);
                    println!(
                        "      uid {:#018X} count {} at {} actor {} -> {}",
                        p.uid,
                        p.count,
                        p.data_offset,
                        p.actor,
                        obj.map(|o| format!("{} {}", kind_name(o.kind), o.model_path))
                            .unwrap_or_else(|| "(no scene object)".into())
                    );
                }
            }
        }
    }

    println!("\n== survey over {} zones ==", s.zones);
    println!(
        "actors {}  pad!=0 {}  name oob {}  asset oob {}  prius oob {}  scene oob {}",
        s.actors, s.actor_pad_nonzero, s.actor_name_oob, s.actor_asset_oob, s.actor_prius_oob,
        s.actor_scene_oob
    );
    println!("actor flag values:");
    let mut hist: Vec<(&u16, &usize)> = s.flag_hist.iter().collect();
    hist.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (v, n) in hist.iter().take(12) {
        println!(
            "   {:#06X} x{:<8} {}",
            v,
            n,
            actor_flag_names(**v).join(" | ")
        );
    }
    let bits: Vec<String> = (0..16)
        .filter(|&b| s.flag_bits[b] > 0)
        .map(|b| format!("{}({})={}", b, ACTOR_FLAG_NAMES[b], s.flag_bits[b]))
        .collect();
    println!("   bits seen on disk: {}", bits.join("  "));
    println!(
        "placed bit: actor uids {}/{}  scene instance ids {}/{} (ids==0 {})",
        s.actor_uid_placed, s.actors, s.obj_placed, s.objects - s.obj_id_zero, s.obj_id_zero
    );
    let unplaced: Vec<String> = s
        .obj_unplaced_by_kind
        .iter()
        .map(|(k, n)| {
            format!("{}={}/{}", kind_name(*k), n, s.obj_by_kind.get(k).copied().unwrap_or(0))
        })
        .collect();
    let zeroed: Vec<String> = s
        .obj_zero_by_kind
        .iter()
        .map(|(k, n)| format!("{}={}", kind_name(*k), n))
        .collect();
    println!("   ids without the placed bit, by kind: {}", unplaced.join(" "));
    println!("   ids read as 0, by kind: {}", zeroed.join(" "));
    println!(
        "groups: {} in {} zones, {} named ({} with name_index == row), {} global, members {} (oob {}), range errors {}, chain breaks {}, unclaimed slots {}, placed uids {}",
        s.groups, s.zones_with_groups, s.group_named, s.group_name_is_row, s.group_global,
        s.group_members, s.group_member_oob, s.group_range_oob, s.group_chain_breaks,
        s.group_data_unclaimed, s.group_uid_placed
    );
    let mut colors: Vec<(&u32, &usize)> = s.group_color_hist.iter().collect();
    colors.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    let clist: Vec<String> = colors
        .iter()
        .take(6)
        .map(|(c, n)| format!("{:#010X}x{}", c, n))
        .collect();
    println!("   debug colors: {}", clist.join(" "));
    println!(
        "script actions: {} in {} zones, plug slice oob {}, name index oob {}, prius oob {}",
        s.actions, s.zones_with_scripts, s.action_out_oob + s.action_var_oob, s.action_name_oob,
        s.action_prius_oob
    );
    println!(
        "   plug slices: {} rows total, {} unclaimed, {} overlaps, {} empty slices ({} with the 0xFFFF sentinel, {} of those with a non-zero count)",
        s.plug_rows, s.plug_unclaimed, s.action_plug_overlap,
        s.action_plug_none + s.action_plug_empty, s.action_plug_none,
        s.action_plug_sentinel_count
    );
    println!(
        "   action name hash matches crc32(name from Zone Script Strings) on {}/{}",
        s.action_name_hash_ok, s.action_name_hash_checked
    );
    let ins: Vec<String> = s
        .action_in_hist
        .iter()
        .take(8)
        .map(|(v, n)| format!("{v}x{n}"))
        .collect();
    println!("   in_plug_count: {}", ins.join(" "));
    println!(
        "vertex paint: {} rows ({} actor) in {} zones, uid matches a scene object {} ({} a model instance), span out of range {}, {} painted vertices, {}/{} colour bytes covered; count == the model's own vertex count on {} rows ({} mismatches)",
        s.paint_rows, s.paint_actor_rows, s.zones_with_paint, s.paint_uid_matched,
        s.paint_uid_model, s.paint_span_oob, s.paint_vertices, s.paint_covered_bytes,
        s.paint_color_bytes, s.paint_count_is_model_verts, s.paint_count_mismatch
    );

    // Hard assertions: these must hold on every shipped zone.
    let mut bad = 0usize;
    for (what, n) in [
        ("actor pad half-word non-zero", s.actor_pad_nonzero),
        ("actor prius slice out of range", s.actor_prius_oob),
        ("actor scene offset out of range", s.actor_scene_oob),
        ("actor uid zero", s.actor_uid_zero),
        ("group member index out of range", s.group_member_oob),
        ("group slice out of range", s.group_range_oob),
        ("group data slot claimed by no group", s.group_data_unclaimed),
        ("script plug slice out of range", s.action_out_oob + s.action_var_oob),
        ("script plug slices overlapping", s.action_plug_overlap),
        ("0xFFFF plug index with a non-zero count", s.action_plug_sentinel_count),
        ("script action name index out of range", s.action_name_oob),
        ("script prius blob out of range", s.action_prius_oob),
        ("vertex paint span outside the colour section", s.paint_span_oob),
    ] {
        if n > 0 {
            println!("   !! {what}: {n}");
            bad += 1;
        }
    }
    if s.actors > 0 && s.actor_uid_placed != s.actors {
        println!(
            "   !! {} of {} actor uids lack the placed bit",
            s.actors - s.actor_uid_placed,
            s.actors
        );
        bad += 1;
    }
    if s.paint_rows > 0 && s.paint_uid_matched != s.paint_rows {
        println!(
            "   !! {} of {} vertex paint uids match no scene object",
            s.paint_rows - s.paint_uid_matched,
            s.paint_rows
        );
        bad += 1;
    }
    failures + bad
}
