//! Dev harness: the `gbl_rgb_blend` template family and the materials that use it.
//!
//!   cargo run --release --bin blend_probe -- <game dir> <mode> [arg] [arg2]
//!
//! Modes:
//!   list <substr>              every named asset whose path contains <substr>
//!   template <graph path>      one template's full declared interface
//!   family <substr>            every `.materialgraph` matching <substr>, side by side
//!   material <material path>   one material: interface, bindings, derived recipe
//!   tabulate <substr>          every `.material` in the TOC whose template matches
//!                              <substr>: slot / constant occupancy, clustered by
//!                              the number of colour layers actually bound
//!   zone <zone path>           the same tabulation over one zone's models
//!   masks <zone path>          decode every splat map a zone binds, channel stats
//!   bychannel [region]         splat channel usage per template variant
//!   hsl [region]               HSL triple overrides vs which layers are bound
//!   hslchan [region]           HSL deviation vs splat channel coverage
//!   correlate <region>         layer slot x splat channel contingency
//!   sections <asset path>      DAT1 section list of any material-ish asset
//!   hexdump <asset path> <tag> raw bytes of one section
//!
//! An empty `[region]` means every `.material` in the TOC, which takes minutes.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use omnitool_lib::core::dat1::Dat1;
use omnitool_lib::core::material::{MaterialFile, MaterialSerialized, TAG_MATERIAL_SERIALIZED};
use omnitool_lib::core::material_graph::MaterialTemplate;
use omnitool_lib::core::model_materials;
use omnitool_lib::core::section_names::section_name;
use omnitool_lib::core::texture_serve;
use omnitool_lib::core::{crc64, level};

fn main() {
    let mut args = std::env::args().skip(1);
    let game = args
        .next()
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let mode = args.next().unwrap_or_else(|| "family".into());
    let arg = args.next().unwrap_or_else(|| "gbl_rgb_blend".into());
    let arg2 = args.next().unwrap_or_default();

    let hashes = std::env::var("RCRA_HASHES").unwrap_or_else(|_| {
        omnitool_lib::core::filesystem::hashes_path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let idx = level::get_index(&game, Some(&hashes)).expect("index");
    idx.require_names().expect("hashes file");

    match mode.as_str() {
        "list" => list(&idx, &arg),
        "template" => template(&idx, &arg),
        "family" => family(&idx, &arg),
        "material" => material(&idx, &arg),
        "tabulate" => tabulate(&idx, &arg, None),
        "zone" => zone(&idx, &arg),
        "masks" => masks(&idx, &arg),
        "correlate" => correlate(&idx, &arg),
        "bychannel" => bychannel(&idx, if arg == "gbl_rgb_blend" { "" } else { &arg }),
        "hslchan" => hslchan(&idx, if arg == "gbl_rgb_blend" { "" } else { &arg }),
        "hsl" => hsl(&idx, if arg == "gbl_rgb_blend" { "" } else { &arg }),
        "sections" => sections(&idx, &arg),
        "hexdump" => hexdump_mode(&idx, &arg, &arg2),
        _ => eprintln!("unknown mode {mode}"),
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn load(idx: &level::LevelIndex, path: &str) -> Option<Dat1> {
    let p = crc64::normalize_path(path);
    let id = crc64::hash_raw(p.as_bytes());
    let raw = idx.extract(id).ok()?;
    MaterialFile::parse(&raw).ok().map(|m| m.dat1)
}

fn matching(idx: &level::LevelIndex, pat: &str, ext: &str) -> Vec<(u64, String)> {
    let pat = pat.to_ascii_lowercase();
    let mut v: Vec<(u64, String)> = idx
        .names
        .iter()
        .filter(|(id, n)| {
            let n = n.to_ascii_lowercase();
            n.contains(&pat) && n.ends_with(ext) && idx.asset(**id).is_some()
        })
        .map(|(id, n)| (*id, n.to_ascii_lowercase()))
        .collect();
    v.sort_by(|a, b| a.1.cmp(&b.1));
    v
}

fn stem(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// The filename suffix after the last `_`, minus the extension.
fn suffix(path: &str) -> String {
    let f = stem(path).to_ascii_lowercase();
    let s = f.strip_suffix(".texture").unwrap_or(&f);
    s.rsplit('_').next().unwrap_or("").to_string()
}

// ---------------------------------------------------------------------------
// list / sections / hexdump
// ---------------------------------------------------------------------------

fn list(idx: &level::LevelIndex, pat: &str) {
    let pat = pat.to_ascii_lowercase();
    let mut v: Vec<(&u64, &String)> = idx
        .names
        .iter()
        .filter(|(_, n)| n.to_ascii_lowercase().contains(&pat))
        .collect();
    v.sort_by(|a, b| a.1.cmp(b.1));
    for (id, n) in v {
        println!("{id:016X} {} {n}", if idx.asset(*id).is_some() { "+" } else { "-" });
    }
}

fn sections(idx: &level::LevelIndex, path: &str) {
    let Some(d) = load(idx, path) else {
        eprintln!("cannot load {path}");
        return;
    };
    println!("== {path}");
    for s in &d.sections {
        println!(
            "  {:08X} {:>9} bytes  {}",
            s.tag,
            s.size,
            section_name(s.tag).unwrap_or("?")
        );
    }
}

fn hexdump_mode(idx: &level::LevelIndex, path: &str, tag: &str) {
    let Some(d) = load(idx, path) else {
        eprintln!("cannot load {path}");
        return;
    };
    let tag = u32::from_str_radix(tag.trim_start_matches("0x"), 16).unwrap_or(0);
    let Some(sec) = d.get_section_data(tag) else {
        eprintln!("no section {tag:08X}");
        return;
    };
    for (i, chunk) in sec.chunks(16).take(64).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|c| format!("{c:02X}")).collect();
        let asc: String = chunk
            .iter()
            .map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '.' })
            .collect();
        println!("{:06X}  {:<48}  {asc}", i * 16, hex.join(" "));
    }
    println!("({} bytes total)", sec.len());
}

// ---------------------------------------------------------------------------
// template
// ---------------------------------------------------------------------------

fn print_template(name: &str, t: &MaterialTemplate) {
    println!("== {name}");
    println!(
        "   {} samplers, {} constants, {} variations",
        t.samplers.len(),
        t.constants.len(),
        t.variations.len()
    );
    let mut s: Vec<_> = t.samplers.iter().collect();
    s.sort_by_key(|x| x.slot_index);
    println!("   -- samplers (by texture unit) --");
    for x in s {
        println!(
            "   t{:<2} hash={:08X} type={:08X} exposed={} default={}",
            x.slot_index,
            x.name_hash,
            x.type_hash,
            x.user_exposed,
            x.default_path.replace('\\', "/")
        );
    }
    let mut c: Vec<_> = t.constants.iter().collect();
    c.sort_by_key(|x| x.buffer_offset);
    println!("   -- constants (by constant-buffer offset) --");
    for x in c {
        println!(
            "   c{:<3} +{:<4} {}f  hash={:08X}  default={:?}",
            x.buffer_offset / 16,
            x.buffer_offset,
            x.buffer_size / 4,
            x.name_hash,
            x.default_values
        );
    }
    let mut v: Vec<_> = t.variations.iter().collect();
    v.sort_by_key(|x| x.name_hash);
    println!("   -- variations (static switches) --");
    for x in v {
        println!("   hash={:08X} default={}", x.name_hash, x.value);
    }
}

fn template(idx: &level::LevelIndex, path: &str) {
    let Some(d) = load(idx, path) else {
        eprintln!("cannot load {path}");
        return;
    };
    let t = MaterialTemplate::from_dat1(&d).expect("template");
    print_template(path, &t);
}

/// Every matching `.materialgraph`, then a cross-variant table of which
/// sampler / constant hashes each one declares.
fn family(idx: &level::LevelIndex, pat: &str) {
    let graphs = matching(idx, pat, ".materialgraph");
    let mut loaded: Vec<(String, MaterialTemplate)> = Vec::new();
    for (id, name) in &graphs {
        let Ok(raw) = idx.extract(*id) else { continue };
        let Ok(f) = MaterialFile::parse(&raw) else { continue };
        let Ok(t) = MaterialTemplate::from_dat1(&f.dat1) else { continue };
        loaded.push((name.clone(), t));
    }

    for (name, t) in &loaded {
        print_template(name, t);
        println!();
    }

    println!("== cross-variant sampler hashes ==");
    let mut all: BTreeSet<u32> = BTreeSet::new();
    for (_, t) in &loaded {
        all.extend(t.samplers.iter().map(|s| s.name_hash));
    }
    print!("{:<10}", "hash");
    for (n, _) in &loaded {
        print!("{:>10}", short(n));
    }
    println!("  default");
    for h in &all {
        print!("{h:08X}  ");
        let mut def = String::new();
        for (_, t) in &loaded {
            match t.sampler_by_hash(*h) {
                Some(s) => {
                    print!("{:>10}", format!("t{}", s.slot_index));
                    if def.is_empty() {
                        def = stem(&s.default_path.replace('\\', "/")).to_string();
                    }
                }
                None => print!("{:>10}", "."),
            }
        }
        println!("  {def}");
    }

    println!("\n== cross-variant constant hashes ==");
    let mut allc: BTreeMap<u32, (u16, Vec<f32>)> = BTreeMap::new();
    for (_, t) in &loaded {
        for c in &t.constants {
            allc.entry(c.name_hash)
                .or_insert((c.buffer_size, c.default_values.clone()));
        }
    }
    print!("{:<10}", "hash");
    for (n, _) in &loaded {
        print!("{:>10}", short(n));
    }
    println!("  size default");
    for (h, (size, def)) in &allc {
        print!("{h:08X}  ");
        for (_, t) in &loaded {
            match t.constant_by_hash(*h) {
                Some(c) => print!("{:>10}", format!("+{}", c.buffer_offset)),
                None => print!("{:>10}", "."),
            }
        }
        println!("  {}f {:?}", size / 4, def);
    }
}

fn short(path: &str) -> String {
    let s = stem(path);
    let s = s.strip_suffix(".materialgraph").unwrap_or(s);
    let s = s.strip_prefix("gbl_rgb_blend").unwrap_or(s);
    let s = s.trim_start_matches('_');
    if s.is_empty() { "base".into() } else { s.chars().take(9).collect() }
}

// ---------------------------------------------------------------------------
// one material
// ---------------------------------------------------------------------------

struct Inst {
    path: String,
    template_path: String,
    template: MaterialTemplate,
    serialized: MaterialSerialized,
}

fn read_material(idx: &level::LevelIndex, id: u64, path: &str) -> Option<Inst> {
    let raw = idx.extract(id).ok()?;
    let f = MaterialFile::parse(&raw).ok()?;
    let template = MaterialTemplate::from_dat1(&f.dat1).ok()?;
    let serialized = match f.dat1.get_section_data(TAG_MATERIAL_SERIALIZED) {
        Some(s) => MaterialSerialized::parse(s).ok()?,
        None => MaterialSerialized::default(),
    };
    Some(Inst {
        path: path.to_string(),
        template_path: f.template_path().unwrap_or_default().to_ascii_lowercase(),
        template,
        serialized,
    })
}

fn material(idx: &level::LevelIndex, path: &str) {
    let p = crc64::normalize_path(path);
    let id = crc64::hash_raw(p.as_bytes());
    let Some(inst) = read_material(idx, id, &p) else {
        eprintln!("cannot load {path}");
        return;
    };
    println!("== {}\n   template {}", inst.path, inst.template_path);
    print_template("(embedded template copy)", &inst.template);

    println!("\n   -- bound samplers --");
    for s in &inst.serialized.samplers {
        let t = inst.template.sampler_by_hash(s.name_hash);
        println!(
            "   {} hash={:08X}  {}",
            t.map(|t| format!("t{}", t.slot_index)).unwrap_or_else(|| "t?".into()),
            s.name_hash,
            s.path.replace('\\', "/")
        );
    }
    println!("   -- overridden constants --");
    let mut cs: Vec<_> = inst.serialized.constants.iter().collect();
    cs.sort_by_key(|c| inst.template.constant_by_hash(c.name_hash).map(|t| t.buffer_offset));
    for c in cs {
        let t = inst.template.constant_by_hash(c.name_hash);
        println!(
            "   {:>6} hash={:08X}  {:?}   (default {:?})",
            t.map(|t| format!("+{}", t.buffer_offset)).unwrap_or_default(),
            c.name_hash,
            c.values,
            t.map(|t| t.default_values.clone()).unwrap_or_default()
        );
    }
    println!("   -- overridden variations --");
    for v in &inst.serialized.aux {
        println!("   hash={:08X} value={}", v.name_hash, v.value);
    }

    println!("\n   -- derived recipe --");
    match model_materials::derive_blend(&inst.template, &inst.serialized, &inst.template_path) {
        Some(b) => println!("{}", serde_json::to_string_pretty(&b).unwrap()),
        None => println!("   (not a gbl_rgb_blend material)"),
    }
}

// ---------------------------------------------------------------------------
// tabulation
// ---------------------------------------------------------------------------

fn tabulate(idx: &level::LevelIndex, pat: &str, only: Option<&[u64]>) {
    let mats: Vec<(u64, String)> = match only {
        Some(ids) => ids
            .iter()
            .filter_map(|id| idx.name_of(*id).map(|n| (*id, n.to_ascii_lowercase())))
            .collect(),
        None => matching(idx, "", ".material"),
    };
    println!("scanning {} materials for template ~ {pat}", mats.len());

    let pat = pat.to_ascii_lowercase();
    let mut per_template: BTreeMap<String, usize> = BTreeMap::new();
    // sampler name hash -> suffix -> count  (the hash is stable across variants,
    // the texture unit is not)
    let mut unit_suffix: BTreeMap<u32, BTreeMap<String, usize>> = BTreeMap::new();
    let mut unit_bound: BTreeMap<u32, usize> = BTreeMap::new();
    let mut unit_units: BTreeMap<u32, BTreeSet<u16>> = BTreeMap::new();
    // constant hash -> (times overridden, distinct value sets, min, max)
    let mut const_stats: BTreeMap<u32, ConstStat> = BTreeMap::new();
    // variation hash -> value -> count
    let mut var_stats: BTreeMap<u32, BTreeMap<u32, usize>> = BTreeMap::new();
    // cluster: number of colour-suffixed textures bound
    let mut clusters: BTreeMap<usize, usize> = BTreeMap::new();
    let mut total = 0usize;

    for (id, name) in &mats {
        let Some(inst) = read_material(idx, *id, name) else { continue };
        if !inst.template_path.contains(&pat) {
            continue;
        }
        total += 1;
        *per_template.entry(inst.template_path.clone()).or_default() += 1;

        let mut colour_layers = 0;
        for s in &inst.serialized.samplers {
            let Some(t) = inst.template.sampler_by_hash(s.name_hash) else { continue };
            *unit_bound.entry(s.name_hash).or_default() += 1;
            unit_units.entry(s.name_hash).or_default().insert(t.slot_index);
            let sfx = suffix(&s.path);
            if sfx == "c" && LAYER_SAMPLERS.contains(&s.name_hash) {
                colour_layers += 1;
            }
            *unit_suffix
                .entry(s.name_hash)
                .or_default()
                .entry(sfx)
                .or_default() += 1;
        }
        *clusters.entry(colour_layers).or_default() += 1;

        for c in &inst.serialized.constants {
            let e = const_stats.entry(c.name_hash).or_default();
            e.count += 1;
            e.observe(&c.values);
        }
        for v in &inst.serialized.aux {
            *var_stats.entry(v.name_hash).or_default().entry(v.value).or_default() += 1;
        }
    }

    println!("\n{total} materials use a template matching {pat:?}");
    for (t, n) in &per_template {
        println!("  {n:>5}  {t}");
    }

    println!("\n-- sampler slot -> bound filename suffix --");
    let mut us: Vec<_> = unit_suffix.iter().collect();
    us.sort_by_key(|(h, _)| std::cmp::Reverse(unit_bound.get(*h).copied().unwrap_or(0)));
    for (hash, sfx) in us {
        let bound = unit_bound.get(hash).copied().unwrap_or(0);
        let mut v: Vec<_> = sfx.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        let top: Vec<String> = v.iter().take(6).map(|(s, n)| format!("_{s}:{n}")).collect();
        let units: Vec<String> = unit_units
            .get(hash)
            .map(|u| u.iter().map(|x| format!("t{x}")).collect())
            .unwrap_or_default();
        println!(
            "  {hash:08X} {:<12} bound by {bound:>5} ({:>5.1}%)  {}",
            units.join("/"),
            100.0 * bound as f64 / total.max(1) as f64,
            top.join(" ")
        );
    }

    println!("\n-- colour layers bound per material (layer samplers, suffix _c) --");
    for (n, c) in &clusters {
        println!("  {n} layers: {c} materials");
    }

    println!("\n-- constants, by how often a material overrides them --");
    let mut cs: Vec<_> = const_stats.iter().collect();
    cs.sort_by_key(|(_, s)| std::cmp::Reverse(s.count));
    for (h, s) in cs {
        println!(
            "  {h:08X}  overridden {:>5} ({:>5.1}%)  {}f  range {}  distinct {}",
            s.count,
            100.0 * s.count as f64 / total.max(1) as f64,
            s.width,
            s.range(),
            s.distinct.len().min(9999)
        );
    }

    println!("\n-- variations overridden --");
    for (h, vals) in &var_stats {
        let v: Vec<String> = vals.iter().map(|(k, n)| format!("{k}:{n}")).collect();
        println!("  {h:08X}  {}", v.join(" "));
    }
}

/// The four layer-colour sampler slots of the `gbl_rgb_blend` family, by the
/// CRC32 the template declares them under. Stable across every variant; the
/// texture unit is not.
const LAYER_SAMPLERS: [u32; 4] = [0x54C8_3B26, 0x9E34_A894, 0x013D_D88D, 0xE4CF_0EAD];
/// The RGB splat / ID mask slot.
const SPLAT_SAMPLER: u32 = 0x205D_ECC3;

/// Decode every splat map a zone's `gbl_rgb_blend` materials bind and report
/// the channel statistics that decide how the mask weights layers.
fn masks(idx: &level::LevelIndex, zone_path: &str) {
    let ids = zone_material_ids(idx, zone_path);
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    println!(
        "{:<44} {:>9} {:>17} {:>17} {:>7} {:>7} {:>7}",
        "splat map", "size", "mean r,g,b", "sum r+g+b", "sum<=1", "binary", "maxch"
    );
    let mut agg = MaskAgg::default();
    for id in ids {
        let Some(inst) = read_material(idx, id, idx.name_of(id).unwrap_or("")) else { continue };
        if !inst.template_path.contains("gbl_rgb_blend") {
            continue;
        }
        let Some(s) = inst.serialized.samplers.iter().find(|s| s.name_hash == SPLAT_SAMPLER) else {
            continue;
        };
        let p = crc64::normalize_path(&s.path);
        let tid = crc64::hash_raw(p.as_bytes());
        if !seen.insert(tid) {
            continue;
        }
        match mask_stats(idx, tid) {
            Some(st) => {
                println!(
                    "{:<44} {:>4}x{:<4} {:>17} {:>17} {:>6.1}% {:>6.1}% {:>7}",
                    stem(&p).chars().take(44).collect::<String>(),
                    st.w,
                    st.h,
                    format!("{:.2},{:.2},{:.2}", st.mean[0], st.mean[1], st.mean[2]),
                    format!("{:.2} ({:.2}..{:.2})", st.sum_mean, st.sum_min, st.sum_max),
                    100.0 * st.sum_le_one,
                    100.0 * st.binary,
                    st.channels_used
                );
                agg.add(&st);
            }
            None => println!("{:<44} (no decode)", stem(&p)),
        }
    }
    agg.report();
}

#[derive(Default)]
struct MaskStat {
    w: u32,
    h: u32,
    mean: [f64; 3],
    sum_mean: f64,
    sum_min: f64,
    sum_max: f64,
    /// Fraction of texels whose R+G+B <= 1.02.
    sum_le_one: f64,
    /// Fraction of texels where every channel is within 0.1 of 0 or 1.
    binary: f64,
    /// How many of R,G,B have a mean above 1/255.
    channels_used: usize,
    /// Fraction of texels where all three channels are below 0.1 — where a
    /// "base" layer would show through.
    base_area: f64,
    /// Fraction of texels above 0.5, per channel.
    coverage: [f64; 3],
    alpha_mean: f64,
}

#[derive(Default)]
struct MaskAgg {
    n: usize,
    sum_le_one: f64,
    binary: f64,
    base_area: f64,
    sum_mean: f64,
    chan: [usize; 4],
}

impl MaskAgg {
    fn add(&mut self, s: &MaskStat) {
        self.n += 1;
        self.sum_le_one += s.sum_le_one;
        self.binary += s.binary;
        self.base_area += s.base_area;
        self.sum_mean += s.sum_mean;
        self.chan[s.channels_used.min(3)] += 1;
    }
    fn report(&self) {
        if self.n == 0 {
            return;
        }
        let n = self.n as f64;
        println!(
            "\n{} distinct splat maps: mean(R+G+B) {:.3}, {:.1}% of texels have R+G+B<=1, \
             {:.1}% are near-binary, {:.1}% are all-dark (base showing)",
            self.n,
            self.sum_mean / n,
            100.0 * self.sum_le_one / n,
            100.0 * self.binary / n,
            100.0 * self.base_area / n
        );
        println!(
            "channels actually used: 0ch {} · 1ch {} · 2ch {} · 3ch {}",
            self.chan[0], self.chan[1], self.chan[2], self.chan[3]
        );
    }
}

fn mask_stats(idx: &level::LevelIndex, tid: u64) -> Option<MaskStat> {
    let (png, st) = texture_serve::decode_texture(idx, tid, 256).ok()?;
    let img = image::load_from_memory(&png).ok()?.to_rgba8();
    let mut s = MaskStat { w: st.width, h: st.height, ..Default::default() };
    let n = (img.width() * img.height()) as f64;
    let (mut le, mut bin, mut dark) = (0usize, 0usize, 0usize);
    let mut cover = [0f64; 3];
    s.sum_min = f64::MAX;
    for px in img.pixels() {
        let c = [
            px.0[0] as f64 / 255.0,
            px.0[1] as f64 / 255.0,
            px.0[2] as f64 / 255.0,
        ];
        for i in 0..3 {
            s.mean[i] += c[i];
        }
        s.alpha_mean += px.0[3] as f64 / 255.0;
        let sum = c[0] + c[1] + c[2];
        s.sum_mean += sum;
        s.sum_min = s.sum_min.min(sum);
        s.sum_max = s.sum_max.max(sum);
        if sum <= 1.02 {
            le += 1;
        }
        if c.iter().all(|v| *v < 0.1 || *v > 0.9) {
            bin += 1;
        }
        if c.iter().all(|v| *v < 0.1) {
            dark += 1;
        }
        for i in 0..3 {
            if c[i] > 0.5 {
                cover[i] += 1.0;
            }
        }
    }
    for i in 0..3 {
        s.mean[i] /= n;
    }
    s.alpha_mean /= n;
    s.sum_mean /= n;
    s.sum_le_one = le as f64 / n;
    s.binary = bin as f64 / n;
    s.base_area = dark as f64 / n;
    for i in 0..3 {
        s.coverage[i] = cover[i] / n;
    }
    s.channels_used = s.mean.iter().filter(|m| **m > 1.0 / 255.0).count();
    Some(s)
}

/// For every `gbl_rgb_blend` material in a region: decode its splat map, note
/// which of R/G/B is actually painted, and note which layer slots carry a
/// texture of their own. If a slot is driven by one channel, it will only ever
/// carry its own texture when that channel is painted — so the 4x3 table below
/// names the slots without a single guess about the shader.
fn correlate(idx: &level::LevelIndex, region: &str) {
    let mats = region_material_ids(idx, region);
    println!("{} candidate materials in region {region}", mats.len());

    struct Rec {
        used: [bool; 3],
        unique: [bool; 4],
        present: [bool; 4],
    }
    let mut recs: Vec<Rec> = Vec::new();
    let mut stats_cache: HashMap<u64, Option<[f64; 3]>> = HashMap::new();

    for id in mats {
        let Some(inst) = read_material(idx, id, idx.name_of(id).unwrap_or("")) else { continue };
        if !inst.template_path.contains("gbl_rgb_blend") {
            continue;
        }
        let Some(sp) = inst.serialized.samplers.iter().find(|s| s.name_hash == SPLAT_SAMPLER)
        else {
            continue;
        };
        let p = crc64::normalize_path(&sp.path);
        let tid = crc64::hash_raw(p.as_bytes());
        let mean = *stats_cache
            .entry(tid)
            .or_insert_with(|| mask_stats(idx, tid).map(|s| s.mean));
        let Some(mean) = mean else { continue };

        let mut paths: BTreeMap<u32, String> = BTreeMap::new();
        for s in &inst.serialized.samplers {
            if LAYER_SAMPLERS.contains(&s.name_hash) {
                paths.insert(s.name_hash, crc64::normalize_path(&s.path));
            }
        }
        let mut unique = [false; 4];
        let mut present = [false; 4];
        for (i, h) in LAYER_SAMPLERS.iter().enumerate() {
            if let Some(path) = paths.get(h) {
                present[i] = true;
                unique[i] = paths.values().filter(|p| *p == path).count() == 1;
            }
        }
        recs.push(Rec {
            used: [mean[0] > 0.01, mean[1] > 0.01, mean[2] > 0.01],
            unique,
            present,
        });
    }

    let n = recs.len();
    println!("
{n} materials with a decodable splat map");
    println!("channel painted: R {} · G {} · B {}",
        recs.iter().filter(|r| r.used[0]).count(),
        recs.iter().filter(|r| r.used[1]).count(),
        recs.iter().filter(|r| r.used[2]).count());
    println!("
P(slot carries its own texture | channel painted) minus");
    println!("P(slot carries its own texture | channel blank), per slot x channel:");
    println!("{:<10} {:>8} {:>18} {:>18} {:>18}", "slot", "present", "R", "G", "B");
    for (i, h) in LAYER_SAMPLERS.iter().enumerate() {
        print!("{h:08X}   {:>8}", recs.iter().filter(|r| r.present[i]).count());
        for c in 0..3 {
            let (mut a, mut an, mut b, mut bn) = (0usize, 0usize, 0usize, 0usize);
            for r in &recs {
                if r.used[c] {
                    an += 1;
                    if r.unique[i] {
                        a += 1;
                    }
                } else {
                    bn += 1;
                    if r.unique[i] {
                        b += 1;
                    }
                }
            }
            let pa = if an > 0 { a as f64 / an as f64 } else { 0.0 };
            let pb = if bn > 0 { b as f64 / bn as f64 } else { 0.0 };
            print!("{:>18}", format!("{:+.2} ({:.2}/{:.2})", pa - pb, pa, pb));
        }
        println!();
    }
    println!("
(Read the table, not a conclusion: the diagonal is only meaningful when the
sample is large enough for the off-diagonal entries to sit near zero.)");
}

/// The four `{Hue, Saturation, Luminance}` triples the template declares, named
/// by inverting the CRC32 of the builder's `"<node>_<port>"` naming. The full
/// strings are `"{Base,Red,Green,Blue}_HSL_{Hue,Saturation,Luminance}"`, and all
/// twelve hash exactly, so the triple-to-layer mapping below is read off the
/// names, not inferred. [`hsl`] measures how well shipped materials agree with
/// it — the answer is "not at all", the correlation is noise, which is why the
/// names are the only evidence that counts here.
const HSL_TRIPLES: [(&str, [u32; 3]); 4] = [
    ("green", [0x679C_068F, 0x33AF_D592, 0x0F89_833E]),
    ("red", [0x18D8_DA14, 0x25B3_6543, 0x7FF6_17E1]),
    ("blue", [0x4EE4_4C3D, 0x3E1E_E9AB, 0xD3DF_84B4]),
    ("base", [0x886E_717D, 0x6A81_9148, 0x92F0_4E42]),
];

/// Over every material of the base template (the one that declares all four
/// layers): is a HSL triple only touched when its layer carries a texture?
fn hsl(idx: &level::LevelIndex, region: &str) {
    let mats = if region.is_empty() {
        matching(idx, "", ".material").into_iter().map(|(i, _)| i).collect()
    } else {
        region_material_ids(idx, region)
    };
    struct Rec {
        bound: [bool; 4],
        unique: [bool; 4],
        touched: [bool; 4],
    }
    let mut recs = Vec::new();
    for id in mats {
        let Some(inst) = read_material(idx, id, idx.name_of(id).unwrap_or("")) else { continue };
        // Only the variant that declares all four layers, so an unbound slot
        // really means "the artist left it out".
        if !inst.template_path.ends_with("gbl_rgb_blend.materialgraph") {
            continue;
        }
        let mut paths: BTreeMap<u32, String> = BTreeMap::new();
        for s in &inst.serialized.samplers {
            if LAYER_SAMPLERS.contains(&s.name_hash) {
                paths.insert(s.name_hash, crc64::normalize_path(&s.path));
            }
        }
        let mut bound = [false; 4];
        let mut unique = [false; 4];
        for (i, h) in LAYER_SAMPLERS.iter().enumerate() {
            if let Some(p) = paths.get(h) {
                bound[i] = !p.is_empty() && !model_materials::is_placeholder_texture(p);
                unique[i] = paths.values().filter(|q| *q == p).count() == 1;
            }
        }
        // "Touched" means pushed off the neutral 0.5, not merely present in the
        // override table: artists re-save the whole constant block, so presence
        // carries almost no signal while deviation does.
        let mut touched = [false; 4];
        for (k, (_, tri)) in HSL_TRIPLES.iter().enumerate() {
            touched[k] = inst.serialized.constants.iter().any(|c| {
                tri.contains(&c.name_hash)
                    && c.values.first().map(|v| (v - 0.5).abs() > 0.02).unwrap_or(false)
            });
        }
        recs.push(Rec { bound, unique, touched });
    }
    let n = recs.len();
    println!("{n} materials on the 4-layer gbl_rgb_blend template");
    println!("layer slots, texture of their own / bound at all:");
    for (i, h) in LAYER_SAMPLERS.iter().enumerate() {
        println!(
            "  {h:08X}  unique {:>5}  bound {:>5}",
            recs.iter().filter(|r| r.unique[i]).count(),
            recs.iter().filter(|r| r.bound[i]).count()
        );
    }
    println!("
P(triple overridden | layer has its own texture) - P(.. | it does not):");
    print!("{:<8}", "triple");
    for h in LAYER_SAMPLERS.iter() {
        print!("{:>22}", format!("{h:08X}"));
    }
    println!();
    for (k, (name, _)) in HSL_TRIPLES.iter().enumerate() {
        print!("{name:<8}");
        for i in 0..4 {
            let (mut a, mut an, mut b, mut bn) = (0, 0, 0, 0);
            for r in &recs {
                if r.unique[i] {
                    an += 1;
                    a += usize::from(r.touched[k]);
                } else {
                    bn += 1;
                    b += usize::from(r.touched[k]);
                }
            }
            let pa = if an > 0 { a as f64 / an as f64 } else { 0.0 };
            let pb = if bn > 0 { b as f64 / bn as f64 } else { 0.0 };
            print!("{:>22}", format!("{:+.3} ({:.2}/{:.2})", pa - pb, pa, pb));
        }
        println!();
    }
}

/// Which RGB channels the splat maps of each template variant actually paint.
/// `gbl_rgb_blend_optimized` declares one layer besides the base, `_red_green`
/// and `_red_blue` two; if the variant names tell the truth their masks will
/// only paint the channels they name, which is what fixes channel -> slot.
fn bychannel(idx: &level::LevelIndex, region: &str) {
    let mats = if region.is_empty() {
        matching(idx, "", ".material").into_iter().map(|(i, _)| i).collect()
    } else {
        region_material_ids(idx, region)
    };
    let mut per: BTreeMap<String, (usize, [f64; 3], [f64; 3])> = BTreeMap::new();
    let mut cache: HashMap<u64, Option<MaskStat>> = HashMap::new();
    for id in mats {
        let Some(inst) = read_material(idx, id, idx.name_of(id).unwrap_or("")) else { continue };
        if !inst.template_path.contains("blend") {
            continue;
        }
        let Some(sp) = inst.serialized.samplers.iter().find(|s| s.name_hash == SPLAT_SAMPLER)
        else {
            continue;
        };
        let p = crc64::normalize_path(&sp.path);
        if p.is_empty() || model_materials::is_placeholder_texture(&p) {
            continue;
        }
        let tid = crc64::hash_raw(p.as_bytes());
        let st = cache.entry(tid).or_insert_with(|| mask_stats(idx, tid));
        let Some(st) = st else { continue };
        let key = stem(&inst.template_path).to_string();
        let e = per.entry(key).or_insert((0, [0.0; 3], [0.0; 3]));
        e.0 += 1;
        for i in 0..3 {
            e.1[i] += st.mean[i];
            e.2[i] += st.coverage[i];
        }
    }
    println!(
        "{:<52} {:>5} {:>22} {:>22}",
        "template variant", "n", "mean R,G,B", "coverage(>0.5) R,G,B"
    );
    for (k, (n, m, c)) in &per {
        let d = *n as f64;
        println!(
            "{:<52} {n:>5} {:>22} {:>22}",
            k.chars().take(52).collect::<String>(),
            format!("{:.3},{:.3},{:.3}", m[0] / d, m[1] / d, m[2] / d),
            format!("{:.3},{:.3},{:.3}", c[0] / d, c[1] / d, c[2] / d)
        );
    }
}

/// Correlate, over shipped materials, how far each HSL triple is pushed off its
/// neutral default against how much of each splat channel is painted. A layer
/// nobody can see is a layer nobody tunes, so the triple that belongs to a
/// channel should track that channel's coverage.
fn hslchan(idx: &level::LevelIndex, region: &str) {
    let mats = if region.is_empty() {
        matching(idx, "", ".material").into_iter().map(|(i, _)| i).collect()
    } else {
        region_material_ids(idx, region)
    };
    let mut rows: Vec<([f64; 3], [f64; 4])> = Vec::new();
    let mut cache: HashMap<u64, Option<MaskStat>> = HashMap::new();
    for id in mats {
        let Some(inst) = read_material(idx, id, idx.name_of(id).unwrap_or("")) else { continue };
        if !inst.template_path.contains("rgb_blend") && !inst.template_path.contains("rgb_hsl") {
            continue;
        }
        let Some(sp) = inst.serialized.samplers.iter().find(|s| s.name_hash == SPLAT_SAMPLER)
        else {
            continue;
        };
        let p = crc64::normalize_path(&sp.path);
        if p.is_empty() || model_materials::is_placeholder_texture(&p) {
            continue;
        }
        let tid = crc64::hash_raw(p.as_bytes());
        let st = cache.entry(tid).or_insert_with(|| mask_stats(idx, tid));
        let Some(st) = st else { continue };
        let mut dev = [0.0f64; 4];
        for (k, (_, tri)) in HSL_TRIPLES.iter().enumerate() {
            let mut d = 0.0;
            for c in &inst.serialized.constants {
                if let Some(pos) = tri.iter().position(|h| *h == c.name_hash) {
                    let v = c.values.first().copied().unwrap_or(0.5) as f64;
                    let neutral = 0.5f64;
                    // hue/sat/lum all default to 0.5 in every variant
                    let _ = pos;
                    d += (v - neutral).abs();
                }
            }
            dev[k] = d;
        }
        rows.push((st.coverage, dev));
    }
    println!("{} materials sampled", rows.len());
    let pearson = |xs: &[f64], ys: &[f64]| -> f64 {
        let n = xs.len() as f64;
        if n < 3.0 {
            return 0.0;
        }
        let mx = xs.iter().sum::<f64>() / n;
        let my = ys.iter().sum::<f64>() / n;
        let mut sxy = 0.0;
        let mut sxx = 0.0;
        let mut syy = 0.0;
        for i in 0..xs.len() {
            let a = xs[i] - mx;
            let b = ys[i] - my;
            sxy += a * b;
            sxx += a * a;
            syy += b * b;
        }
        if sxx <= 0.0 || syy <= 0.0 {
            0.0
        } else {
            sxy / (sxx * syy).sqrt()
        }
    };
    println!("Pearson r between channel coverage and |HSL - 0.5| per triple:");
    println!("{:<8} {:>10} {:>10} {:>10}", "triple", "R", "G", "B");
    for (k, (name, _)) in HSL_TRIPLES.iter().enumerate() {
        print!("{name:<8}");
        for c in 0..3 {
            let xs: Vec<f64> = rows.iter().map(|(cov, _)| cov[c]).collect();
            let ys: Vec<f64> = rows.iter().map(|(_, d)| d[k]).collect();
            print!("{:>10}", format!("{:+.3}", pearson(&xs, &ys)));
        }
        println!();
    }
}

fn region_material_ids(idx: &level::LevelIndex, region: &str) -> Vec<u64> {
    let zones: Vec<u64> = idx
        .zones
        .iter()
        .filter(|z| z.region == region)
        .map(|z| z.id)
        .collect();
    let mut models = BTreeSet::new();
    for zid in zones {
        let Ok(raw) = idx.extract(zid) else { continue };
        let Ok(z) = omnitool_lib::core::zone::Zone::parse(&raw) else { continue };
        for o in z.model_instances() {
            models.insert(o.model_id);
        }
    }
    let mut mats: BTreeSet<u64> = BTreeSet::new();
    for m in &models {
        for s in &model_materials::model_materials(idx, *m).slots {
            if let Ok(id) = model_materials::parse_hex_id(&s.material_id) {
                if id != 0 {
                    mats.insert(id);
                }
            }
        }
    }
    mats.into_iter().collect()
}

fn zone_material_ids(idx: &level::LevelIndex, zone_path: &str) -> Vec<u64> {
    let p = crc64::normalize_path(zone_path);
    let zid = crc64::hash_raw(p.as_bytes());
    let Ok(raw) = idx.extract(zid) else { return Vec::new() };
    let Ok(z) = omnitool_lib::core::zone::Zone::parse(&raw) else { return Vec::new() };
    let mut models = BTreeSet::new();
    for o in z.model_instances() {
        models.insert(o.model_id);
    }
    let mut mats: BTreeSet<u64> = BTreeSet::new();
    for m in &models {
        for s in &model_materials::model_materials(idx, *m).slots {
            if let Ok(id) = model_materials::parse_hex_id(&s.material_id) {
                if id != 0 {
                    mats.insert(id);
                }
            }
        }
    }
    mats.into_iter().collect()
}

#[derive(Default)]
struct ConstStat {
    count: usize,
    width: usize,
    min: Vec<f32>,
    max: Vec<f32>,
    distinct: BTreeSet<String>,
}

impl ConstStat {
    fn observe(&mut self, v: &[f32]) {
        self.width = v.len();
        if self.min.len() != v.len() {
            self.min = v.to_vec();
            self.max = v.to_vec();
        }
        for (i, x) in v.iter().enumerate() {
            if *x < self.min[i] {
                self.min[i] = *x;
            }
            if *x > self.max[i] {
                self.max[i] = *x;
            }
        }
        if self.distinct.len() < 4096 {
            self.distinct
                .insert(v.iter().map(|x| format!("{x:.4}")).collect::<Vec<_>>().join(","));
        }
    }
    fn range(&self) -> String {
        let lo: Vec<String> = self.min.iter().map(|x| format!("{x:.3}")).collect();
        let hi: Vec<String> = self.max.iter().map(|x| format!("{x:.3}")).collect();
        format!("[{}]..[{}]", lo.join(","), hi.join(","))
    }
}

fn zone(idx: &level::LevelIndex, zone_path: &str) {
    let p = crc64::normalize_path(zone_path);
    let zid = crc64::hash_raw(p.as_bytes());
    let raw = idx.extract(zid).expect("extract zone");
    let z = omnitool_lib::core::zone::Zone::parse(&raw).expect("parse zone");
    let mut models = BTreeSet::new();
    for o in z.model_instances() {
        models.insert(o.model_id);
    }
    // Every material any of those models names.
    let mut mats: BTreeSet<u64> = BTreeSet::new();
    let mut by_template: HashMap<String, usize> = HashMap::new();
    for m in &models {
        let rec = model_materials::model_materials(idx, *m);
        for s in &rec.slots {
            *by_template.entry(s.template.to_ascii_lowercase()).or_default() += 1;
            if let Ok(id) = model_materials::parse_hex_id(&s.material_id) {
                if id != 0 {
                    mats.insert(id);
                }
            }
        }
    }
    println!(
        "zone {zone_path}: {} models, {} distinct materials",
        models.len(),
        mats.len()
    );
    let mut t: Vec<_> = by_template.into_iter().collect();
    t.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    println!("-- slots per template --");
    for (name, n) in t.iter().take(20) {
        println!("  {n:>5}  {name}");
    }
    let ids: Vec<u64> = mats.into_iter().collect();
    tabulate(idx, "gbl_rgb_blend", Some(&ids));
}
