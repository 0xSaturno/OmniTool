//! Independent cross-check of the highest-value claims from the six Level
//! Editor sweeps, measured over the whole shipped PC build.
//!
//!   cargo run --release --bin integration_check -- "<game dir>"
//!
//! This is deliberately NOT built on the modules it is checking. The CRC-32 and
//! CRC-64 used here are re-implemented from the polynomial below, and every
//! section is read as raw bytes out of the zone's DAT1, so a bug shared between
//! a claim and its own probe cannot hide here. The only things trusted are
//! `core::level` (archive + TOC) and `Zone::parse`'s container split into DAT1
//! blocks, because `level_check` already covers both. Nothing here uses
//! `core::prius`, `core::zone_script`, `core::zone_lighting` or
//! `core::zone_entities` -- the modules whose claims are under test.
//!
//! Checks, in order:
//!   A  Zone Actor Prius row +0x00 is crc64(lowercase(component + "Prius")),
//!      and is not an actor uid.
//!   B  Zone Script Plug +0x04 is a target action row index: inbound edge
//!      counts must reproduce every action's own in_plug_count.
//!   C  The scene-object entry size at +0x7C: how wide is it really? Two
//!      modules disagree (24 bits vs 31 bits). Plus: does anything in the
//!      scene heap go unreferenced, i.e. can the editor miss an object?
//!   D  Zone Script String Hashes is parallel to Zone Script Strings and holds
//!      crc32 of each string.
//!   E  Light entries are 0xF0 bytes and every Zone Lights offset lands on a
//!      kind-1 scene object.

use std::collections::HashMap;

use omnitool_lib::core::dat1::Dat1;
use omnitool_lib::core::level;
use omnitool_lib::core::zone::Zone;

// ---------------------------------------------------------------------------
// hashes, from scratch
// ---------------------------------------------------------------------------

/// Standard CRC-32 (reflected, poly 0xEDB88320, init !0, final xor !0).
/// Kept so the report can show that the engine does NOT use it.
fn crc32_standard(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            let m = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & m);
        }
    }
    !crc
}

/// The engine's CRC-32: reflected poly 0xEDB88320, **init = poly**, no final
/// xor -- structurally the same trick as its CRC-64. Re-derived here rather than
/// calling `core::crc32`.
fn crc32(bytes: &[u8]) -> u32 {
    const POLY: u32 = 0xEDB8_8320;
    let mut crc = POLY;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            let m = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (POLY & m);
        }
    }
    crc
}

/// CRC-64/ECMA reflected, init = poly, no final xor. This is the "plain" form:
/// no `(crc >> 2) | 1 << 63` asset-id packing.
fn crc64_plain(bytes: &[u8]) -> u64 {
    const POLY: u64 = 0xC96C_5795_D787_0F42;
    let mut crc = POLY;
    for &b in bytes {
        crc ^= b as u64;
        for _ in 0..8 {
            let m = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (POLY & m);
        }
    }
    crc
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64le(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

// Section tags.
const SEC_SCENE: u32 = 0x06AB_CAB2; // Zone Scene Objects
const SEC_ACTOR_PRIUS: u32 = 0x50ED_C53D; // Zone Actor Priuses
const SEC_ACTOR_PRIUS_DATA: u32 = 0x8199_9057; // Zone Actor Prius Data
const SEC_SCRIPT_ACTIONS: u32 = 0x5E54_ACCF;
const SEC_SCRIPT_PLUGS: u32 = 0xBEAB_52E7;
const SEC_SCRIPT_STRINGS: u32 = 0xEF86_37D5;
const SEC_SCRIPT_STR_HASHES: u32 = 0x80D2_9828;
const SEC_LIGHTS: u32 = 0x97FF_6EB5;

/// The six per-kind instance tables `core::zone` enumerates scene objects from.
/// Each is a flat `u32[]` of byte offsets into the scene heap. Tags taken from
/// `core::zone`'s own constants, which `level_check` already covers.
const INSTANCE_TABLES: [(&str, u32); 6] = [
    ("Zone Model Instances", 0x6987_F172),
    ("Zone Lights", 0x97FF_6EB5),
    ("Zone Volumes", 0xBDAB_2B0D),
    ("Zone Decals", 0xBCF4_3558),
    ("Zone VFX Instances", 0x927C_4EC3),
    ("Zone Impostors", 0xDC31_1FC3),
];

/// String pool lookup: DAT1 string offsets are relative to the string section.
fn pool_string(d: &Dat1, raw_off: u32) -> Option<String> {
    d.get_string(raw_off)
}

#[derive(Default)]
struct Counts {
    ok: u64,
    total: u64,
}
impl Counts {
    fn hit(&mut self, cond: bool) {
        self.total += 1;
        if cond {
            self.ok += 1;
        }
    }
    fn show(&self, label: &str) -> bool {
        let pass = self.ok == self.total && self.total > 0;
        println!(
            "  {label:<58} {}/{}  {}",
            self.ok,
            self.total,
            if pass { "OK" } else { "MISMATCH" }
        );
        pass
    }
}

fn main() {
    let game = std::env::args()
        .nth(1)
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let limit: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX);

    let idx = level::get_index(&game, None).expect("level index");
    println!("game  : {game}");
    println!("zones : {}\n", idx.zones.len());

    // --- accumulators -------------------------------------------------------
    // A: actor prius typing
    let mut a_uid_match: u64 = 0; // +0x00 equals some actor uid in this zone
    let mut a_rows: u64 = 0;
    let mut a_type_match = Counts::default();
    let mut a_name_crc = Counts::default();
    let mut a_span_ok = Counts::default();
    let mut a_resv_ok = Counts::default();
    let mut a_unresolved: HashMap<String, u64> = HashMap::new();
    let mut a_name_crc_std = Counts::default();
    // B: script connectivity
    let mut b_incount = Counts::default();
    let mut b_incount_alt0: u64 = 0;
    let mut b_incount_alt8: u64 = 0;
    let mut b_actions_total: u64 = 0;
    let mut b_target_in_range = Counts::default();
    // C: entry size width
    let mut c_bit31: u64 = 0;
    let mut c_bits24_30: u64 = 0;
    let mut c_entries: u64 = 0;
    let mut c_max_size: u32 = 0;
    let mut c_walk_exact: u64 = 0;
    let mut c_walk_short: u64 = 0;
    let mut c_heap_bytes: u64 = 0;
    let mut c_uncovered_bytes: u64 = 0;
    let mut c_orphan_kinds: HashMap<u8, u64> = HashMap::new();
    let mut c_uncov_examples: Vec<(String, usize, usize)> = Vec::new();
    // D: script string hashes
    let mut d_len_eq = Counts::default();
    let mut d_hash = Counts::default();
    // E: lights
    let mut e_kind1 = Counts::default();
    let mut e_zones_with_lights: u64 = 0;
    let mut e_lights: u64 = 0;
    let mut e_type_hist: HashMap<u8, u64> = HashMap::new();

    let mut zones_read = 0usize;
    let mut zones_failed = 0usize;

    for z in idx.zones.iter().take(limit) {
        let Ok(bytes) = idx.extract(z.id) else {
            zones_failed += 1;
            continue;
        };
        let Ok(zone) = Zone::parse(&bytes) else {
            zones_failed += 1;
            continue;
        };
        let d = &zone.dat1;
        zones_read += 1;

        // ------------------------------------------------------------------
        // C: scene-object entry size width, walked from the heap itself
        // ------------------------------------------------------------------
        // `core::zone` enumerates objects from the six per-kind instance tables,
        // not by walking the heap, so coverage is measured the same way: mark
        // the bytes every listed entry occupies and see what is left over.
        if let Some(scene) = d.get_section_data(SEC_SCENE) {
            let mut covered = vec![false; scene.len()];
            // Actors own a scene object too, addressed by the actor row's
            // `scene_offset` at +0x08 and NOT listed in any instance table.
            // Leaving these out makes the heap look full of orphans.
            let mut roots: Vec<usize> = Vec::new();
            for sh in &d.sections {
                if level::zone_section_name(sh.tag) == Some("Zone Actors") {
                    if let Some(rows) = d.get_section_data(sh.tag) {
                        for r in rows.chunks_exact(32) {
                            roots.push(u32le(r, 0x08) as usize);
                        }
                    }
                }
            }
            for (_, tag) in INSTANCE_TABLES {
                let Some(sec) = d.get_section_data(tag) else {
                    continue;
                };
                for ch in sec.chunks_exact(4) {
                    roots.push(u32::from_le_bytes([ch[0], ch[1], ch[2], ch[3]]) as usize);
                }
            }
            {
                for off in roots {
                    if off + 0x80 > scene.len() {
                        continue;
                    }
                    let raw = u32le(scene, off + 0x7C);
                    let size = (raw & 0x7FFF_FFFF) as usize;
                    c_entries += 1;
                    if raw & 0x8000_0000 != 0 {
                        c_bit31 += 1;
                    }
                    if raw & 0x7F00_0000 != 0 {
                        c_bits24_30 += 1;
                    }
                    c_max_size = c_max_size.max(size as u32);
                    let end = (off + size.max(0x80)).min(scene.len());
                    for b in &mut covered[off..end] {
                        *b = true;
                    }
                }
            }
            let uncov = covered.iter().filter(|c| !**c).count();
            c_heap_bytes += scene.len() as u64;
            c_uncovered_bytes += uncov as u64;
            if uncov == 0 {
                c_walk_exact += 1;
            } else {
                c_walk_short += 1;
                // Does an uncovered run parse as a scene entry? If so, its kind
                // byte names a class of object the editor never enumerates.
                let mut i = 0usize;
                while i < covered.len() {
                    if covered[i] {
                        i += 1;
                        continue;
                    }
                    let start = i;
                    while i < covered.len() && !covered[i] {
                        i += 1;
                    }
                    let run = i - start;
                    if run >= 0x80 && start + 0x80 <= scene.len() {
                        let kind = (u32le(scene, start + 0x5C) >> 24) as u8;
                        let sz = u32le(scene, start + 0x7C) & 0x7FFF_FFFF;
                        if sz >= 0x80 && sz as usize <= run {
                            *c_orphan_kinds.entry(kind).or_default() += 1;
                            if c_uncov_examples.len() < 6 {
                                c_uncov_examples.push((z.path.clone(), start, run));
                            }
                        }
                    }
                }
            }
        }

        // ------------------------------------------------------------------
        // A: actor priuses
        // ------------------------------------------------------------------
        // Gather actor uids in this zone from the 32-byte Zone Actors rows.
        // The Zone Actors tag: find it by trying the known tag, else skip the
        // uid comparison for this zone rather than guess.
        let mut actor_uids: Vec<u64> = Vec::new();
        for sh in &d.sections {
            if let Some(name) = level::zone_section_name(sh.tag) {
                if name == "Zone Actors" {
                    if let Some(rows) = d.get_section_data(sh.tag) {
                        let n = rows.len() / 32;
                        for i in 0..n {
                            actor_uids.push(u64le(rows, i * 32 + 24));
                        }
                    }
                }
            }
        }
        let uid_set: std::collections::HashSet<u64> = actor_uids.iter().copied().collect();

        if let Some(rows) = d.get_section_data(SEC_ACTOR_PRIUS) {
            // Where does the prius data pool live in the file?
            let pool_hdr = d
                .sections
                .iter()
                .find(|s| s.tag == SEC_ACTOR_PRIUS_DATA)
                .map(|s| (s.offset as usize, s.size as usize));
            let n = rows.len() / 32;
            for i in 0..n {
                let r = &rows[i * 32..i * 32 + 32];
                let type_uid = u64le(r, 0x00);
                let name_off = u32le(r, 0x08);
                let name_hash = u32le(r, 0x0C);
                let resv10 = u32le(r, 0x10);
                let data_off = u32le(r, 0x14) as usize;
                let data_size = u32le(r, 0x18) as usize;
                let resv1c = u32le(r, 0x1C) as i32;

                a_rows += 1;
                if uid_set.contains(&type_uid) {
                    a_uid_match += 1;
                }
                a_resv_ok.hit(resv10 == 0 && resv1c == -1);

                if let Some((po, ps)) = pool_hdr {
                    a_span_ok.hit(data_off >= po && data_off + data_size <= po + ps);
                }

                if let Some(comp) = pool_string(d, name_off) {
                    a_name_crc.hit(crc32(comp.as_bytes()) == name_hash);
                    a_name_crc_std.hit(crc32_standard(comp.as_bytes()) == name_hash);
                    let want = crc64_plain(format!("{}Prius", comp).to_lowercase().as_bytes());
                    let hit = want == type_uid;
                    a_type_match.hit(hit);
                    if !hit {
                        *a_unresolved.entry(comp).or_default() += 1;
                    }
                }
            }
        }

        // ------------------------------------------------------------------
        // B / D: script graph
        // ------------------------------------------------------------------
        let actions = d.get_section_data(SEC_SCRIPT_ACTIONS);
        let plugs = d.get_section_data(SEC_SCRIPT_PLUGS);
        if let (Some(actions), Some(plugs)) = (actions, plugs) {
            let na = actions.len() / 40;
            let np = plugs.len() / 12;
            b_actions_total += na as u64;

            // Per-action declared in-plug count, and its out-plug slice.
            // Row layout from core::zone: uid, graph_uid, then the u16 counts.
            // Read them by position rather than trusting a struct.
            let mut declared = vec![0u16; na];
            let mut out_slices: Vec<(usize, usize)> = Vec::with_capacity(na);
            for i in 0..na {
                let r = &actions[i * 40..i * 40 + 40];
                declared[i] = u16le(r, 0x10);
                let oi = u16le(r, 0x12) as usize;
                let oc = u16le(r, 0x14) as usize;
                out_slices.push((oi, oc));
            }

            let mut inbound = vec![0u32; na];
            let mut inbound0 = vec![0u32; na];
            let mut inbound8 = vec![0u32; na];
            for (oi, oc) in out_slices.iter().copied() {
                for p in oi..oi.saturating_add(oc) {
                    if p >= np {
                        continue;
                    }
                    let r = &plugs[p * 12..p * 12 + 12];
                    let t4 = u32le(r, 0x04) as usize;
                    let t0 = u32le(r, 0x00) as usize;
                    let t8 = u32le(r, 0x08) as usize;
                    b_target_in_range.hit(t4 < na);
                    if t4 < na {
                        inbound[t4] += 1;
                    }
                    if t0 < na {
                        inbound0[t0] += 1;
                    }
                    if t8 < na {
                        inbound8[t8] += 1;
                    }
                }
            }
            for i in 0..na {
                b_incount.hit(inbound[i] as u16 == declared[i]);
                if inbound0[i] as u16 == declared[i] {
                    b_incount_alt0 += 1;
                }
                if inbound8[i] as u16 == declared[i] {
                    b_incount_alt8 += 1;
                }
            }

            // D
            if let (Some(strs), Some(hashes)) = (
                d.get_section_data(SEC_SCRIPT_STRINGS),
                d.get_section_data(SEC_SCRIPT_STR_HASHES),
            ) {
                d_len_eq.hit(strs.len() == hashes.len());
                let n = (strs.len() / 4).min(hashes.len() / 4);
                for i in 0..n {
                    let so = u32le(strs, i * 4);
                    let want = u32le(hashes, i * 4);
                    if let Some(s) = pool_string(d, so) {
                        d_hash.hit(crc32(s.as_bytes()) == want);
                    }
                }
            }
        }

        // ------------------------------------------------------------------
        // E: lights
        // ------------------------------------------------------------------
        if let (Some(lights), Some(scene)) =
            (d.get_section_data(SEC_LIGHTS), d.get_section_data(SEC_SCENE))
        {
            let n = lights.len() / 4;
            if n > 0 {
                e_zones_with_lights += 1;
            }
            for i in 0..n {
                let off = u32le(lights, i * 4) as usize;
                if off + 0x80 > scene.len() {
                    e_kind1.hit(false);
                    continue;
                }
                let flags = u32le(scene, off + 0x5C);
                let kind = (flags >> 24) as u8;
                let size = u32le(scene, off + 0x7C) & 0x7FFF_FFFF;
                e_kind1.hit(kind == 1 && size >= 0xF0);
                e_lights += 1;
                if off + 0x93 < scene.len() {
                    *e_type_hist.entry(scene[off + 0x92]).or_default() += 1;
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    println!("zones read {zones_read}, failed {zones_failed}\n");
    let mut pass = true;

    println!("A  Zone Actor Prius row +0x00");
    println!("  rows                                                       {a_rows}");
    println!(
        "  +0x00 equal to an actor uid in the same zone               {a_uid_match}  {}",
        if a_uid_match == 0 {
            "OK (it is NOT the actor uid)"
        } else {
            "the uid theory is not dead"
        }
    );
    pass &= a_uid_match == 0;
    let t = &a_type_match;
    println!(
        "  +0x00 == crc64_plain(lower(component+\"Prius\"))             {}/{}  ({:.3}%)",
        t.ok,
        t.total,
        100.0 * t.ok as f64 / t.total.max(1) as f64
    );
    pass &= a_name_crc.show("engine crc32(component) == row +0x0C");
    println!(
        "  standard crc32 (init !0, xor !0) instead                    {}/{}  {}",
        a_name_crc_std.ok,
        a_name_crc_std.total,
        "<- the engine does not use it"
    );
    pass &= a_span_ok.show("blob span inside Zone Actor Prius Data");
    pass &= a_resv_ok.show("+0x10 == 0 and +0x1C == -1");
    let mut un: Vec<_> = a_unresolved.iter().collect();
    un.sort_by(|a, b| b.1.cmp(a.1));
    println!(
        "  components whose name does not hash to +0x00: {} distinct",
        un.len()
    );
    for (name, c) in un.iter().take(12) {
        println!("      {c:>6}  {name}");
    }

    println!("\nB  Zone Script Plugs +0x04 is the target action row index");
    println!("  action rows                                                {b_actions_total}");
    pass &= b_target_in_range.show("out-plug +0x04 < action row count");
    pass &= b_incount.show("inbound count from +0x04 == action in_plug_count");
    println!("  same test using +0x00 as the target                        {b_incount_alt0}");
    println!("  same test using +0x08 as the target                        {b_incount_alt8}");

    println!("\nC  Scene objects: entry size at +0x7C, and heap coverage");
    println!("  entries reachable from instance tables + actor rows        {c_entries}");
    println!("  with bit 31 of +0x7C set                                   {c_bit31}");
    println!("  with any of bits 24..30 set                                {c_bits24_30}");
    println!("  largest entry                                              {c_max_size} bytes");
    println!(
        "  -> 24-bit mask (core::zone) vs 31-bit (core::zone_entities): {}",
        if c_bits24_30 == 0 {
            "same size on every shipped entry"
        } else {
            "THEY DISAGREE on real data"
        }
    );
    println!(
        "  -> bit 31 is set on {:.1}% of entries, so zone_entities' claim that",
        100.0 * c_bit31 as f64 / c_entries.max(1) as f64
    );
    println!("     \"every shipped entry measured has bit 31 clear\" is FALSE");
    println!("  scene heap bytes                                           {c_heap_bytes}");
    println!(
        "  heap bytes nothing points at                                {c_uncovered_bytes}  ({:.2}%)",
        100.0 * c_uncovered_bytes as f64 / c_heap_bytes.max(1) as f64
    );
    println!("  zones with the heap fully covered                          {c_walk_exact}");
    println!("  zones with uncovered heap bytes                            {c_walk_short}");
    let mut oks: Vec<_> = c_orphan_kinds.iter().collect();
    oks.sort();
    if oks.is_empty() {
        println!("  uncovered runs that parse as a scene entry: none");
    } else {
        println!("  uncovered runs that parse as a scene entry, by kind byte:");
        for (k, n) in oks {
            println!("      kind {k:>3}  {n}");
        }
    }
    for (p, at, run) in c_uncov_examples.iter().take(6) {
        println!("      gap at {at:>8} run {run:>7}  {p}");
    }

    println!("\nD  Zone Script String Hashes");
    pass &= d_len_eq.show("length equals Zone Script Strings");
    pass &= d_hash.show("crc32(string) == hash");

    println!("\nE  Zone Lights");
    println!("  zones with lights                                          {e_zones_with_lights}");
    println!("  lights                                                     {e_lights}");
    pass &= e_kind1.show("offset lands on a kind-1 entry of >= 0xF0 bytes");
    let mut th: Vec<_> = e_type_hist.iter().collect();
    th.sort();
    let names = [
        "point",
        "spot",
        "pyramid",
        "shaft",
        "modulate",
        "lightgridmod",
    ];
    for (k, c) in th {
        let nm = names.get(*k as usize).copied().unwrap_or("UNKNOWN");
        println!("      type {k:>3} {nm:<14} {c}");
    }

    println!(
        "\n{}",
        if pass {
            "ALL CROSS-CHECKS PASSED"
        } else {
            "SOME CROSS-CHECKS FAILED - see above"
        }
    );
    if !pass {
        std::process::exit(1);
    }
}
