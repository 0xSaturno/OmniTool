//! Runs the structural checks behind `docs/MATERIAL_FORMAT.md` over every shipped
//! `.material` and `.materialgraph`, picked from the dag by asset type.
//!
//! usage: material_audit <game_dir> [--limit N] [--show NAME_SUBSTRING [--save DIR]]
//!
//! `--show` skips the audit and dumps the slots of every material whose path contains the string;
//! `--save` also writes those materials and their templates to DIR.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use omnitool_lib::core::crc32;
use omnitool_lib::core::crc64;
use omnitool_lib::core::dag::Dag;
use omnitool_lib::core::dat1::Dat1;
use omnitool_lib::core::material::{
    flags, MaterialFile, MaterialFurInfo, MaterialHeaderSection, MaterialSerialized,
    MaterialWaterInfo, NO_PATH, TAG_MATERIAL_FUR, TAG_MATERIAL_HEADER, TAG_MATERIAL_SERIALIZED,
    TAG_MATERIAL_VARIATION, TAG_MATERIAL_WATER,
};
use omnitool_lib::core::material_graph::{MaterialTemplate, TAG_TEMPLATE_CONSTANTS, TAG_TEMPLATE_SAMPLERS};
use omnitool_lib::core::material_names::{self, av_material_name};
use omnitool_lib::core::toc::{Toc, TocAsset};

const TYPE_MATERIAL: u8 = 9;
const TYPE_MATERIAL_GRAPH: u8 = 10;
const FAILURES_PER_CHECK: usize = 25;

#[derive(Default)]
struct Stats {
    materials: usize,
    graphs: usize,
    checks: BTreeMap<&'static str, (usize, usize)>,
    failures: BTreeMap<&'static str, Vec<String>>,
    hist: BTreeMap<&'static str, BTreeMap<String, usize>>,
    flag_bits: [usize; 32],
    /// template asset id -> slot tables
    templates: HashMap<u64, Slots>,
    /// (material name, template id, overrides)
    overrides: Vec<(String, u64, Slots)>,
}

/// Sampler and constant name hashes, plus the samplers a template locks (UserExposed == 0).
#[derive(Clone, Default)]
struct Slots {
    samplers: Vec<u32>,
    constants: Vec<u32>,
    locked: Vec<u32>,
}

impl Slots {
    fn of_template(t: &MaterialTemplate) -> Self {
        Self {
            samplers: t.samplers.iter().map(|s| s.name_hash).collect(),
            constants: t.constants.iter().map(|c| c.name_hash).collect(),
            locked: t.samplers.iter().filter(|s| !s.user_exposed).map(|s| s.name_hash).collect(),
        }
    }
}

impl Stats {
    fn check(&mut self, name: &'static str, ok: bool, who: &str, detail: impl FnOnce() -> String) {
        let e = self.checks.entry(name).or_insert((0, 0));
        if ok {
            e.0 += 1;
            return;
        }
        e.1 += 1;
        let list = self.failures.entry(name).or_default();
        if list.len() < FAILURES_PER_CHECK {
            list.push(format!("{who}: {}", detail()));
        }
    }

    fn count(&mut self, what: &'static str, key: impl Into<String>) {
        *self.hist.entry(what).or_default().entry(key.into()).or_insert(0) += 1;
    }

    /// Overrides must name template slots, and textures only apply to exposed ones.
    fn check_overrides(&mut self, name: &str, o: &Slots, t: &Slots) {
        let label = |v: Vec<u32>| v.iter().map(|h| material_names::label(*h)).collect::<Vec<_>>().join(",");
        let orphan_s: Vec<u32> = o.samplers.iter().copied().filter(|h| !t.samplers.contains(h)).collect();
        let orphan_c: Vec<u32> = o.constants.iter().copied().filter(|h| !t.constants.contains(h)).collect();
        let locked: Vec<u32> = o.samplers.iter().copied().filter(|h| t.locked.contains(h)).collect();
        self.count("override targets", format!("samplers not in template: {}", !orphan_s.is_empty()));
        self.count("override targets", format!("constants not in template: {}", !orphan_c.is_empty()));
        self.check("sampler overrides target UserExposed slots", locked.is_empty(), name, || label(locked.clone()));
        for h in orphan_s.iter().chain(&orphan_c) {
            self.count("override names missing from template", material_names::label(*h));
        }
    }
}

fn av_label(h: u32) -> String {
    match h {
        0 => "0".into(),
        _ => av_material_name(h).map(str::to_string).unwrap_or_else(|| format!("unresolved {h:#010X}")),
    }
}

fn is_sorted(v: &[u32]) -> bool {
    v.windows(2).all(|w| w[0] < w[1])
}

/// `Some(None)` for [`NO_PATH`], `Some(Some(path))` for a pool string, `None` if it doesn't resolve.
fn path_field(dat1: &Dat1, off: u32) -> Option<Option<String>> {
    if off == NO_PATH {
        return Some(None);
    }
    (off as usize >= dat1.header_end()).then(|| dat1.get_string(off)).flatten().map(Some)
}

fn audit_material(name: &str, bytes: &[u8], st: &mut Stats) {
    let Ok(mut file) = MaterialFile::parse(bytes) else {
        st.check("parses", false, name, || "MaterialFile::parse failed".into());
        return;
    };
    st.materials += 1;
    st.check("file round-trips byte-identical", file.save() == bytes, name, || "save() differs".into());
    let dat1 = &file.dat1;

    let Some(sec) = dat1.get_section_data(TAG_MATERIAL_HEADER) else {
        st.check("has header", false, name, || "no header".into());
        return;
    };
    st.check("header is 40 bytes", sec.len() == 40, name, || sec.len().to_string());
    let Ok(h) = MaterialHeaderSection::parse(sec) else { return };
    st.check("header round-trips", h.build() == sec, name, || "build differs".into());
    st.check("template pointer slot is 0 on disk", h.template_ptr == 0, name, || format!("{:#X}", h.template_ptr));
    let builtin = h.flags & (flags::FUR | flags::WATER) != 0;

    let header_off = dat1.sections.iter().find(|s| s.tag == TAG_MATERIAL_HEADER).unwrap().offset;
    let fixups = dat1.fixup_pairs();
    st.check("exactly one fixup, from header +0", fixups.len() == 1 && fixups[0].0 == header_off, name,
        || format!("{fixups:X?} header at {header_off:#X}"));
    let template = file.template_path();
    st.check("fixup targets a .materialgraph (empty for fur/water)",
        if builtin { template.is_none() } else { template.as_deref().is_some_and(|t| t.ends_with(".materialgraph")) },
        name, || format!("{template:?}"));
    let order: Vec<u32> = {
        let mut s: Vec<_> = dat1.sections.iter().collect();
        s.sort_by_key(|s| s.offset);
        s.iter().map(|s| s.tag).collect()
    };
    if let Some(ser) = order.iter().position(|&t| t == TAG_MATERIAL_SERIALIZED) {
        st.check("header precedes serialized data", order.iter().position(|&t| t == TAG_MATERIAL_HEADER) < Some(ser),
            name, || format!("{order:08X?}"));
    }

    // A/V at 0x0C is written raw (0 when unset); audio at 0x10 falls back to A/V, then kNone.
    let knone = crc32::hash("kNone");
    st.count("audio (0x10) given A/V (0x0C)", match (h.av_material_hash, h.audio_material_hash) {
        (0, x) if x == knone => "A/V unset -> audio kNone",
        (0, _) => "A/V unset -> audio set",
        (a, b) if a == b => "A/V X -> audio X",
        (_, x) if x == knone => "A/V X -> audio kNone",
        _ => "A/V X -> audio Y",
    });
    st.check("audio material is never 0", h.audio_material_hash != 0, name, String::new);
    for hash in [h.av_material_hash, h.audio_material_hash].into_iter().filter(|&x| x != 0) {
        st.check("A/V and audio hashes resolve to AV material names", av_material_name(hash).is_some(), name, || av_label(hash));
    }
    st.count("A/V material (0x0C)", av_label(h.av_material_hash));
    st.count("Alpha (0x14)", format!("{}", h.alpha));
    st.count("AlphaTest (0x18)", format!("{}", h.alpha_test));
    st.count("LodDist (0x1C)", format!("{}", h.lod_dist));
    st.count("VoxelizationOrderBias (0x20)", format!("{}", h.voxelization_order_bias));
    if h.pad != [0; 7] {
        st.count("nonzero header bytes 0x21..0x28", format!("{:02X?}", h.pad));
    }

    let f = h.flags;
    for bit in 0..32 {
        st.flag_bits[bit] += (f >> bit & 1) as usize;
    }
    st.check("runtime flag bits 25-31 clear", f & flags::RUNTIME_MASK == 0, name, || format!("{f:#X}"));
    let blend = f >> 7 & 0xF;
    st.check("blend bits 7-10 one-hot", blend.count_ones() <= 1, name, || format!("{f:#X}"));
    st.check("SSRDisabled(12) and SSRFidelityOnly(24) exclusive", f >> 12 & f >> 24 & 1 == 0, name, || format!("{f:#X}"));
    st.check("OverlapColorOnly(16) and OverlapNormalOnly(20) exclusive", f >> 16 & f >> 20 & 1 == 0, name, || format!("{f:#X}"));
    let blend_name = ["opaque", "Additive", "Alpha", "", "Modulate", "", "", "", "Hybrid"][blend.min(8) as usize];
    st.count("AlphaLit(11) by blend", format!("{blend_name} alphalit={}", f >> 11 & 1));

    let fur = dat1.get_section_data(TAG_MATERIAL_FUR);
    let water = dat1.get_section_data(TAG_MATERIAL_WATER);
    st.check("Fur(14) <=> Material Fur Info", (f & flags::FUR != 0) == fur.is_some(), name, || format!("{f:#X}"));
    st.check("Water(23) <=> Material Water Info", (f & flags::WATER != 0) == water.is_some(), name, || format!("{f:#X}"));
    if let Some(sec) = fur {
        st.check("fur info is 52 bytes", sec.len() == 52, name, || sec.len().to_string());
        if let Ok(info) = MaterialFurInfo::parse(sec) {
            st.check("fur info round-trips", info.build() == sec, name, String::new);
            st.count("fur layer count", info.layer_count.to_string());
            for off in info.map_offsets {
                match path_field(dat1, off) {
                    Some(Some(p)) => st.check("fur map is a pool .texture path", p.ends_with(".texture"), name, || p),
                    Some(None) => st.count("fur maps", "none"),
                    None => st.check("fur map is a pool .texture path", false, name, || format!("{off:#X}")),
                }
            }
        }
    }
    if let Some(sec) = water {
        st.check("water info is 76 bytes", sec.len() == 76, name, || sec.len().to_string());
        if let Ok(info) = MaterialWaterInfo::parse(sec) {
            st.check("water info round-trips", info.build() == sec, name, String::new);
            st.check("water flow map resolves", path_field(dat1, info.flow_map_offset).is_some(), name,
                || format!("{:#X}", info.flow_map_offset));
        }
    }

    let embedded = file.has_embedded_template();
    if let Some(v) = dat1.get_section_data(TAG_MATERIAL_VARIATION) {
        st.check("variation info is 520 bytes", v.len() == 520, name, || v.len().to_string());
        let path = String::from_utf8_lossy(&v[..v.iter().position(|&b| b == 0).unwrap_or(v.len())]).into_owned();
        let id = u64::from_le_bytes(v[v.len() - 8..].try_into().unwrap());
        st.check("variation info: id == crc64(path)", id == crc64::hash(&path), name, || format!("{path} {id:016X}"));
        st.check("variation info names a .materialgraph", path.ends_with(".materialgraph"), name, || path.clone());
    }
    let Some(sec) = dat1.get_section_data(TAG_MATERIAL_SERIALIZED) else { return };
    let s = match MaterialSerialized::parse(sec) {
        Ok(s) => s,
        Err(e) => return st.check("serialized section parses", false, name, || e.to_string()),
    };
    st.check("serialized section round-trips", s.build() == sec, name, || "build differs".into());
    if embedded {
        st.check("embedded template => variations or AccurateAlphaVelocity(1)",
            !s.variations.is_empty() || f & flags::ACCURATE_ALPHA_VELOCITY != 0, name, String::new);
    }
    st.count("variations present", format!("{} (embedded template: {embedded})", !s.variations.is_empty()));
    let sh: Vec<u32> = s.samplers.iter().map(|x| x.name_hash).collect();
    let ch: Vec<u32> = s.constants.iter().map(|x| x.name_hash).collect();
    let vh: Vec<u32> = s.variations.iter().map(|x| x.name_hash).collect();
    st.check("serialized samplers sorted by hash", is_sorted(&sh), name, || format!("{sh:08X?}"));
    st.check("serialized constants sorted by hash", is_sorted(&ch), name, || format!("{ch:08X?}"));
    st.check("variations sorted by hash", is_sorted(&vh), name, || format!("{vh:08X?}"));
    st.check("variation values are 0/1", s.variations.iter().all(|v| v.value <= 1), name, String::new);
    for c in &s.constants {
        st.count("constant sizes (bytes)", (c.values.len() * 4).to_string());
    }

    let o = Slots { samplers: sh, constants: ch, locked: vec![] };
    if embedded {
        let t = Slots::of_template(&MaterialTemplate::from_dat1(dat1).unwrap_or_default());
        st.check_overrides(name, &o, &t);
    } else if let Some(t) = &template {
        st.overrides.push((name.to_string(), crc64::hash(t), o));
    }
}

fn audit_graph(name: &str, id: u64, bytes: &[u8], st: &mut Stats) {
    let Ok(file) = MaterialFile::parse(bytes) else {
        st.check("graph parses", false, name, || "parse failed".into());
        return;
    };
    st.graphs += 1;
    let dat1 = &file.dat1;
    for s in &dat1.sections {
        st.count("graph sections (tag: graphs)", format!("{:08X}", s.tag));
    }
    if let Some(sec) = dat1.get_section_data(TAG_TEMPLATE_SAMPLERS) {
        st.check("template samplers are 16-byte records", sec.len() % 16 == 0, name, || sec.len().to_string());
    }
    let Ok(t) = MaterialTemplate::from_dat1(dat1) else { return };
    for s in &t.samplers {
        st.count("template sampler UserExposed", s.user_exposed.to_string());
        st.count("template sampler type", material_names::label(s.type_hash));
    }
    let mut slots: Vec<u16> = t.samplers.iter().map(|s| s.slot_index).collect();
    slots.sort_unstable();
    st.check("template sampler slot indices are 0..n", slots.iter().enumerate().all(|(i, &s)| s as usize == i), name,
        || format!("{slots:?}"));
    let t = Slots::of_template(&t);
    st.check("template samplers sorted by hash", is_sorted(&t.samplers), name, String::new);
    if dat1.get_section_data(TAG_TEMPLATE_CONSTANTS).is_some() {
        st.check("template constants sorted by hash", is_sorted(&t.constants), name, String::new);
    }
    st.templates.insert(id, t);
}

fn show(name: &str, bytes: &[u8], extract: &dyn Fn(u64) -> Option<Vec<u8>>, save: Option<&Path>) {
    let Ok(file) = MaterialFile::parse(bytes) else { return };
    let graph_bytes = file.template_path().and_then(|t| extract(crc64::hash(&t)));
    if let Some(dir) = save {
        let base = name.rsplit('\\').next().unwrap_or(name);
        std::fs::write(dir.join(base), bytes).ok();
        if let (Some(g), Some(t)) = (&graph_bytes, file.template_path()) {
            std::fs::write(dir.join(t.rsplit('\\').next().unwrap_or(&t)), g).ok();
        }
    }
    let template = if file.has_embedded_template() {
        MaterialTemplate::from_dat1(&file.dat1).ok()
    } else {
        graph_bytes.and_then(|b| MaterialTemplate::parse(&b).ok())
    };
    println!("\n{name}\n  template: {:?} (embedded: {})", file.template_path(), file.has_embedded_template());
    let overrides = file
        .dat1
        .get_section_data(TAG_MATERIAL_SERIALIZED)
        .and_then(|s| MaterialSerialized::parse(s).ok())
        .unwrap_or_default();
    let mark = |h: u32| if material_names::resolve(h).is_some() { " " } else { "?" };
    for c in &overrides.constants {
        println!("  {} const   {:08X} {:<36} {:?}", mark(c.name_hash), c.name_hash, material_names::label(c.name_hash), c.values);
    }
    for s in &overrides.samplers {
        println!("  {} sampler {:08X} {:<36} {}", mark(s.name_hash), s.name_hash, material_names::label(s.name_hash), s.path);
    }
    for v in &overrides.variations {
        println!("  {} variation {:08X} {:<34} {}", mark(v.name_hash), v.name_hash, material_names::label(v.name_hash), v.value);
    }
    if let Some(t) = template {
        for c in &t.constants {
            println!("  {} tmpl-const {:08X} {:<33} {:?}", mark(c.name_hash), c.name_hash, material_names::label(c.name_hash), c.default_values);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let Some(game) = args.first().filter(|a| !a.starts_with("--")) else {
        eprintln!("usage: material_audit <game_dir> [--limit N]");
        std::process::exit(1);
    };
    let limit: usize = get("--limit").and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
    let game = Path::new(game);
    let t0 = std::time::Instant::now();

    let dag = Dag::load(&game.join("dag")).expect("load dag");
    let toc = Toc::parse(&std::fs::read(game.join("toc")).expect("read toc")).expect("parse toc");
    let archives = toc.archive_filenames();
    let is_mod = |a: &TocAsset| {
        archives.get(a.archive_index as usize)
            .is_some_and(|n| n.replace('/', "\\").to_ascii_lowercase().starts_with("d\\mods\\"))
    };
    let mut by_id: HashMap<u64, TocAsset> = HashMap::new();
    for a in toc.assets() {
        if !is_mod(&a) {
            by_id.entry(a.asset_id).or_insert(a);
        }
    }
    let cache = toc.archive_cache(game);
    if let Some(needle) = get("--show") {
        let extract = |id: u64| by_id.get(&id).and_then(|a| toc.extract_asset_with_cache(a, &cache).ok());
        for i in (0..dag.len()).filter(|&i| dag.type_byte(i) == TYPE_MATERIAL) {
            let name = dag.name(i).unwrap_or("");
            if name.to_lowercase().contains(&needle.to_lowercase()) {
                if let Some(bytes) = extract(dag.id(i)) {
                    show(name, &bytes, &extract, get("--save").as_deref().map(Path::new));
                }
            }
        }
        return;
    }
    let mut st = Stats::default();

    // Graphs first, so materials can be checked against their templates.
    let mut dag_only = [0usize; 2];
    for (pass, ty) in [TYPE_MATERIAL_GRAPH, TYPE_MATERIAL].into_iter().enumerate() {
        let mut done = 0usize;
        for i in 0..dag.len() {
            if dag.type_byte(i) != ty || done >= limit {
                continue;
            }
            let name = dag.name(i).unwrap_or("<unnamed>").to_string();
            let Some(a) = by_id.get(&dag.id(i)) else {
                dag_only[pass] += 1;
                continue;
            };
            let Ok(bytes) = toc.extract_asset_with_cache(a, &cache) else {
                st.check("extracts", false, &name, || "extract failed".into());
                continue;
            };
            done += 1;
            if ty == TYPE_MATERIAL {
                audit_material(&name, &bytes, &mut st);
            } else {
                audit_graph(&name, dag.id(i), &bytes, &mut st);
            }
        }
    }

    for (name, tid, o) in std::mem::take(&mut st.overrides) {
        match st.templates.get(&tid).cloned() {
            Some(t) => st.check_overrides(&name, &o, &t),
            None => st.count("template not shipped", format!("{tid:016X}")),
        }
    }

    println!(
        "\n=== {} materials, {} graphs in {:.1}s (in dag but not in toc: {} materials, {} graphs) ===",
        st.materials, st.graphs, t0.elapsed().as_secs_f32(), dag_only[1], dag_only[0]
    );
    println!("\n-- checks --");
    for (name, (ok, bad)) in &st.checks {
        println!("  {} {name:<58} {ok} passed, {bad} failed", if *bad == 0 { "ok  " } else { "FAIL" });
    }
    println!("\n-- flag bits set (of {} materials) --", st.materials);
    for bit in 0..32 {
        if st.flag_bits[bit] > 0 {
            println!("  {bit:>2} {:<26} {}", flags::NAMES.get(bit).unwrap_or(&"(runtime)"), st.flag_bits[bit]);
        }
    }
    for (what, m) in &st.hist {
        let mut v: Vec<_> = m.iter().collect();
        v.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
        println!("\n-- {what} ({} distinct) --", v.len());
        for (k, c) in v.into_iter().take(16) {
            println!("  {c:>7}  {k}");
        }
    }
    for (check, list) in &st.failures {
        println!("\n-- failures: {check} --");
        for f in list {
            println!("  {f}");
        }
    }
}
