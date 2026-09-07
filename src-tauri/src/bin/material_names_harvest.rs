//! Recovers material slot names by hashing identifiers harvested from
//! `.materialgraph` assets against the slot hashes used by materials.
//!
//! Slot ids are the Insomniac CRC32 variant, so they can only be matched, not
//! inverted. The graphs embed shader reflection tables naming the same slots
//! with a different suffix (`BaseMap2D_Func` ↔ `BaseMap2D_Texture`), which is
//! what makes the match possible.
//!
//! usage:
//!   material_names_harvest <toc> <archives dir> <hashes> [--out FILE]
//!                          [--materials] [--limit N] [--extra FILE]...

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

use rayon::prelude::*;

use omnitool_lib::core::crc32;
use omnitool_lib::core::material::{MaterialFile, MaterialSerialized, TAG_MATERIAL_SERIALIZED};
use omnitool_lib::core::material_graph::MaterialTemplate;
use omnitool_lib::core::toc::{Toc, TocAsset};

/// Where a slot hash was seen. Sampler slots take texture paths, constants
/// take floats — used to sanity-check a recovered name.
#[derive(Default, Clone, Copy)]
struct Kind {
    sampler: u32,
    constant: u32,
    /// Widest float count seen for a constant slot — 1 is a scalar, 2 a UV
    /// pair, 3/4 a color. Narrows what a guessed name should mean.
    arity: u32,
}

impl Kind {
    fn label(&self) -> &'static str {
        match (self.sampler > 0, self.constant > 0) {
            (true, false) => "sampler",
            (false, true) => "constant",
            (true, true) => "both",
            _ => "unknown",
        }
    }
}

/// Identifier-shaped byte runs, the way shader reflection stores names.
fn tokens_of(bytes: &[u8]) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut start: Option<usize> = None;
    for i in 0..=bytes.len() {
        let is_word = i < bytes.len()
            && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_');
        match (is_word, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                let run = &bytes[s..i];
                if run.len() >= 4
                    && run.len() <= 64
                    && (run[0].is_ascii_alphabetic() || run[0] == b'_')
                {
                    if let Ok(text) = std::str::from_utf8(run) {
                        out.insert(text.to_string());
                    }
                }
                start = None;
            }
            _ => {}
        }
    }
    out
}

const STRIP_SUFFIXES: &[&str] = &["_Func", "_Sampler", "_Texture", "_Tex", "_Map", "_Value"];
const STRIP_PREFIXES: &[&str] = &["m_", "g_", "s_", "k"];
const ADD_SUFFIXES: &[&str] = &[
    "",
    "_Texture",
    "_Sampler",
    "_Func",
    "_Color",
    "_Colour",
    "_Map",
    "_Value",
    "2D_Texture",
];

/// Every string worth hashing for one harvested token.
fn candidates(token: &str) -> Vec<String> {
    let mut stems: Vec<&str> = vec![token];
    for suffix in STRIP_SUFFIXES {
        if let Some(stem) = token.strip_suffix(suffix) {
            if stem.len() >= 3 {
                stems.push(stem);
            }
        }
    }
    let mut prefixed = Vec::new();
    for stem in &stems {
        for prefix in STRIP_PREFIXES {
            if let Some(rest) = stem.strip_prefix(prefix) {
                if rest.len() >= 3 {
                    prefixed.push(rest);
                }
            }
        }
    }
    stems.extend(prefixed);

    let mut out = Vec::with_capacity(stems.len() * ADD_SUFFIXES.len());
    for stem in stems {
        for suffix in ADD_SUFFIXES {
            out.push(format!("{stem}{suffix}"));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Casing styles seen in confirmed slot names (`Spec_Reflectance`,
/// `world_displacement`, `BaseColor`, `Emissive`).
fn styles(parts: &[&str], out: &mut Vec<String>) {
    let title: Vec<String> = parts
        .iter()
        .map(|p| {
            let mut c = p.chars();
            match c.next() {
                Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
                None => String::new(),
            }
        })
        .collect();
    out.push(title.join("_"));
    out.push(parts.join("_"));
    out.push(title.concat());
    out.push(parts.concat());
    if parts.len() > 1 {
        out.push(format!("{}_{}", title[0], parts[1..].join("_")));
    }
}

const GUESS_SUFFIXES: &[&str] = &[
    "", "_Texture", "_Sampler", "_Value", "_Amount", "_Scale", "_Strength",
];

/// Vocabulary brute force over the slots nothing else could name. Prints one
/// line per hit; the caller decides what to believe.
fn guess(unresolved_path: &str, vocab_path: &str) {
    let mut targets: HashMap<u32, (String, u32, u32)> = HashMap::new();
    for line in std::fs::read_to_string(unresolved_path).expect("read unresolved").lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() < 4 {
            continue;
        }
        let Ok(hash) = u32::from_str_radix(cols[0].trim_start_matches("0x"), 16) else {
            continue;
        };
        targets.insert(
            hash,
            (
                cols[1].to_string(),
                cols[2].parse().unwrap_or(0),
                cols[3].parse().unwrap_or(0),
            ),
        );
    }

    let vocab_text = std::fs::read_to_string(vocab_path).expect("read vocab");
    let vocab: Vec<&str> = vocab_text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let head = &vocab[..vocab.len().min(170)];
    println!(
        "{} targets, {} words (head {}) — searching",
        targets.len(),
        vocab.len(),
        head.len()
    );

    let start = std::time::Instant::now();
    let tried = std::sync::atomic::AtomicU64::new(0);
    let hits: Vec<(u32, String)> = vocab
        .par_iter()
        .flat_map_iter(|a| {
            let mut bases: Vec<String> = Vec::new();
            styles(&[a], &mut bases);
            for b in &vocab {
                styles(&[a, b], &mut bases);
            }
            if head.contains(a) {
                for b in head {
                    for c in head {
                        styles(&[a, b, c], &mut bases);
                    }
                }
            }
            let mut local = Vec::new();
            let mut count = 0u64;
            for base in &bases {
                for suffix in GUESS_SUFFIXES {
                    let cand = format!("{base}{suffix}");
                    count += 1;
                    if targets.contains_key(&crc32::hash(&cand)) {
                        local.push((crc32::hash(&cand), cand));
                    }
                }
            }
            tried.fetch_add(count, std::sync::atomic::Ordering::Relaxed);
            local.into_iter()
        })
        .collect();

    let mut by_hash: HashMap<u32, Vec<String>> = HashMap::new();
    for (h, name) in hits {
        by_hash.entry(h).or_default().push(name);
    }
    println!(
        "tried {:.1}M candidates in {:.1}s — {} of {} slots got a match",
        tried.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e6,
        start.elapsed().as_secs_f64(),
        by_hash.len(),
        targets.len()
    );

    let mut rows: Vec<(u32, Vec<String>)> = by_hash.into_iter().collect();
    rows.sort_by_key(|(h, _)| std::cmp::Reverse(targets.get(h).map(|t| t.1).unwrap_or(0)));
    for (hash, mut names) in rows {
        names.sort_by_key(|n| (n.len(), n.clone()));
        names.dedup();
        let (kind, uses, arity) = targets.get(&hash).cloned().unwrap_or_default();
        let texture_ish = names[0].ends_with("_Texture") || names[0].ends_with("_Sampler");
        let flag = if kind == "sampler" && !texture_ish {
            "  SUSPICIOUS(sampler wants a texture name)"
        } else if kind == "constant" && texture_ish {
            "  SUSPICIOUS(constant with texture name)"
        } else {
            ""
        };
        let shown: Vec<&String> = names.iter().take(6).collect();
        println!(
            "{hash:#010X} {kind:9} uses={uses:5} arity={arity} -> {shown:?}{flag}"
        );
    }
}

/// Words the shipped strings don't carry but artists still use.
const CURATED: &str = "\
ambient occlusion displacement strength intensity amount scale bias offset \
tiling tile coord rotation angle speed power exponent factor multiplier mult \
opacity alpha transparency blend mask threshold cutoff clip fade falloff \
metallic metalness roughness smoothness glossiness gloss specular reflectance \
reflection refraction fresnel rim sheen anisotropy translucency subsurface \
emissive emission glow brightness contrast saturation hue gamma exposure \
color colour tint palette primary secondary tertiary index slot count steps \
detail overlay layer weight height depth thickness width radius size length \
normal bump parallax curvature wrap wind flow distortion warp wave \
frequency amplitude phase time animation frame variation seed random noise \
edge outline highlight shadow shading light lighting spec diffuse base map \
min max start end near far enable enabled toggle value default world local";

/// Split a path or identifier into lowercase words.
fn words_of(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, ch) in chars.iter().enumerate() {
        let boundary = !ch.is_ascii_alphanumeric()
            || (ch.is_ascii_uppercase()
                && i > 0
                && chars[i - 1].is_ascii_lowercase());
        if boundary {
            if cur.len() >= 3 {
                out.push(cur.to_ascii_lowercase());
            }
            cur.clear();
        }
        if ch.is_ascii_alphanumeric() {
            cur.push(*ch);
        }
    }
    if cur.len() >= 3 {
        out.push(cur.to_ascii_lowercase());
    }
    out
}

/// Per-graph search: vocabulary from the graph's own path and its sampler
/// default textures, targets limited to the slots that graph declares. Far
/// fewer candidates per target than the global sweep, so a hit means much
/// more.
fn guess_local(
    toc: &Toc,
    cache: &omnitool_lib::core::toc::ArchiveCache,
    graphs: &[(TocAsset, String)],
    known: &HashSet<u32>,
    out_path: &str,
) {
    use std::io::Write;

    let curated: Vec<String> = CURATED.split_whitespace().map(|s| s.to_string()).collect();
    let start = std::time::Instant::now();
    let mut results: Vec<(u32, String, String, u32, String)> = Vec::new();
    let mut total_candidates: u64 = 0;
    let mut scanned = 0usize;
    let mut skipped = 0usize;

    for (index, (asset, path)) in graphs.iter().enumerate() {
        let Ok(raw) = toc.extract_asset_with_cache(asset, cache) else {
            continue;
        };
        let Ok(template) = MaterialTemplate::parse(&raw) else {
            continue;
        };

        // Only the slots nothing has named yet.
        let mut wanted: HashMap<u32, (String, u32)> = HashMap::new();
        for s in &template.samplers {
            if !known.contains(&s.name_hash) {
                wanted.insert(s.name_hash, ("sampler".to_string(), 0));
            }
        }
        for c in &template.constants {
            if !known.contains(&c.name_hash) {
                wanted.insert(
                    c.name_hash,
                    ("constant".to_string(), c.default_values.len() as u32),
                );
            }
        }
        if wanted.is_empty() {
            skipped += 1;
            continue;
        }
        scanned += 1;

        // Vocabulary: this graph's path, its default textures, curated words.
        let mut vocab: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut push = |w: String, vocab: &mut Vec<String>, seen: &mut HashSet<String>| {
            if w.len() >= 3 && w.len() <= 20 && seen.insert(w.clone()) {
                vocab.push(w);
            }
        };
        for w in words_of(path) {
            push(w, &mut vocab, &mut seen);
        }
        for s in &template.samplers {
            for w in words_of(&s.default_path) {
                push(w, &mut vocab, &mut seen);
            }
        }
        for w in &curated {
            push(w.clone(), &mut vocab, &mut seen);
        }

        let head_len = vocab.len().min(60);
        let hits: Vec<(u32, String)> = vocab
            .par_iter()
            .flat_map_iter(|a| {
                let mut bases: Vec<String> = Vec::new();
                styles(&[a], &mut bases);
                for b in &vocab {
                    styles(&[a, b], &mut bases);
                }
                if vocab.iter().take(head_len).any(|h| h == a) {
                    for b in vocab.iter().take(head_len) {
                        for c in vocab.iter().take(head_len) {
                            styles(&[a, b, c], &mut bases);
                        }
                    }
                }
                let mut local = Vec::new();
                for base in &bases {
                    for suffix in GUESS_SUFFIXES {
                        let cand = format!("{base}{suffix}");
                        let h = crc32::hash(&cand);
                        if wanted.contains_key(&h) {
                            local.push((h, cand));
                        }
                    }
                }
                local.into_iter()
            })
            .collect();

        let v = vocab.len() as u64;
        let h = head_len as u64;
        total_candidates += (v + v * v + h * h * h) * 5 * GUESS_SUFFIXES.len() as u64;

        let short = path.rsplit(['/', '\\']).next().unwrap_or(path).to_string();
        if !hits.is_empty() {
            let mut by_hash: HashMap<u32, Vec<String>> = HashMap::new();
            for (h, name) in hits {
                let e = by_hash.entry(h).or_default();
                if !e.contains(&name) {
                    e.push(name);
                }
            }
            for (hash, mut names) in by_hash {
                names.sort_by_key(|n| (n.len(), n.clone()));
                let (kind, arity) = wanted.get(&hash).cloned().unwrap_or_default();
                println!(
                    "  [{:>4}/{}] {short}: {hash:#010X} {kind} arity={arity} -> {:?}",
                    index + 1,
                    graphs.len(),
                    names.iter().take(4).collect::<Vec<_>>()
                );
                let _ = std::io::stdout().flush();
                results.push((hash, names[0].clone(), kind, arity, short.clone()));
            }
        }

        if (index + 1) % 25 == 0 {
            println!(
                "[{:>4}/{}] {scanned} graphs searched, {skipped} already named, \
                 {} hits, {:.1}M candidates, {:.0}s",
                index + 1,
                graphs.len(),
                results.len(),
                total_candidates as f64 / 1e6,
                start.elapsed().as_secs_f64()
            );
            let _ = std::io::stdout().flush();
        }
    }

    // One name per hash; a hash found in several graphs is a good sign.
    let mut counts: HashMap<u32, usize> = HashMap::new();
    for (hash, ..) in &results {
        *counts.entry(*hash).or_default() += 1;
    }
    let mut seen: HashSet<u32> = HashSet::new();
    let mut text = String::from(
        "# Material slot names guessed per graph (local vocabulary).\n\
         # graphs=N means N different graphs produced the same name for the\n\
         # hash — collisions do not repeat, so N>1 is strong evidence.\n\n",
    );
    let mut rows: Vec<&(u32, String, String, u32, String)> = results.iter().collect();
    rows.sort_by_key(|(h, ..)| std::cmp::Reverse(counts.get(h).copied().unwrap_or(0)));
    for (hash, name, kind, arity, graph) in rows {
        if !seen.insert(*hash) {
            continue;
        }
        text.push_str(&format!(
            "{name} # {hash:#010X} {kind} arity={arity} graphs={} from={graph} source=local\n",
            counts.get(hash).copied().unwrap_or(0)
        ));
    }
    std::fs::write(out_path, text).expect("write results");

    println!(
        "\ndone: {} graphs searched ({} had nothing left to name), \
         {:.1}M candidates in {:.0}s, {} distinct slots named → {out_path}",
        scanned,
        skipped,
        total_candidates as f64 / 1e6,
        start.elapsed().as_secs_f64(),
        seen.len()
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // `--guess <unresolved.csv> <vocab.txt>` runs standalone, no TOC needed.
    if let Some(i) = args.iter().position(|a| a == "--guess") {
        let (Some(unresolved), Some(vocab)) = (args.get(i + 1), args.get(i + 2)) else {
            eprintln!("usage: material_names_harvest --guess <unresolved.csv> <vocab.txt>");
            std::process::exit(1);
        };
        guess(unresolved, vocab);
        return;
    }

    if args.len() < 3 {
        eprintln!(
            "usage: material_names_harvest <toc> <archives dir> <hashes> \
             [--out FILE] [--materials] [--limit N]"
        );
        std::process::exit(1);
    }
    // `--unresolved FILE`: every slot hash no candidate named, with the kind,
    // float count and use count — the input for an external guessing pass.
    let unresolved_path = args
        .iter()
        .position(|a| a == "--unresolved")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let out_path = args
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "material_names.txt".to_string());
    let scan_materials = args.iter().any(|a| a == "--materials");
    let limit: Option<usize> = args
        .iter()
        .position(|a| a == "--limit")
        .and_then(|i| args.get(i + 1))
        .and_then(|n| n.parse().ok());
    // Extra dictionary files — the exe, `dag`, ddl dumps, anything with
    // identifiers in it. Targets still come only from the assets.
    let extras: Vec<String> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| a.as_str() == "--extra")
        .filter_map(|(i, _)| args.get(i + 1).cloned())
        .collect();

    // `--explain <path substring>`: dump one graph's declared slots and every
    // identifier it contains, to see what a stubborn hash has to work with.
    let explain: Option<String> = args
        .iter()
        .position(|a| a == "--explain")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.to_ascii_lowercase());

    let toc_bytes = std::fs::read(&args[0]).expect("read toc");
    let toc = Toc::parse(&toc_bytes).expect("parse toc");
    let archives_dir = PathBuf::from(&args[1]);

    // The hash list tells us which asset is a graph and which is a material.
    let mut graph_ids: HashSet<u64> = HashSet::new();
    let mut material_ids: HashSet<u64> = HashSet::new();
    let mut explain_ids: HashMap<u64, String> = HashMap::new();
    let mut graph_paths: HashMap<u64, String> = HashMap::new();
    for line in std::fs::read_to_string(&args[2]).expect("read hashes").lines() {
        let mut parts = line.splitn(3, ',');
        let (Some(hex), Some(path)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Ok(id) = u64::from_str_radix(hex.trim(), 16) else {
            continue;
        };
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".materialgraph") {
            graph_ids.insert(id);
            graph_paths.insert(id, path.to_string());
            if let Some(needle) = &explain {
                if lower.contains(needle.as_str()) {
                    explain_ids.insert(id, path.to_string());
                }
            }
        } else if lower.ends_with(".material") {
            material_ids.insert(id);
        }
    }
    println!(
        "{} materialgraph ids, {} material ids in the hash list",
        graph_ids.len(),
        material_ids.len()
    );

    let assets = toc.assets();
    let pick = |ids: &HashSet<u64>| -> Vec<TocAsset> {
        let mut seen = HashSet::new();
        let mut out: Vec<TocAsset> = assets
            .iter()
            .filter(|a| a.span_index == 0 && ids.contains(&a.asset_id) && seen.insert(a.asset_id))
            .cloned()
            .collect();
        if let Some(n) = limit {
            out.truncate(n);
        }
        out
    };
    let graphs = pick(&graph_ids);
    let materials = if scan_materials { pick(&material_ids) } else { Vec::new() };
    println!(
        "scanning {} graphs{}",
        graphs.len(),
        if scan_materials {
            format!(" and {} materials", materials.len())
        } else {
            String::new()
        }
    );

    let cache = toc.archive_cache(&archives_dir);

    // `--guess-local <known names file>`: per-graph vocabulary search.
    if let Some(i) = args.iter().position(|a| a == "--guess-local") {
        let known_path = args.get(i + 1).expect("--guess-local needs a names file");
        let mut known: HashSet<u32> = HashSet::new();
        for line in std::fs::read_to_string(known_path)
            .expect("read known names")
            .lines()
        {
            let name = line.split('#').next().unwrap_or("").trim();
            if !name.is_empty() {
                known.insert(crc32::hash(name));
            }
        }
        let with_paths: Vec<(TocAsset, String)> = graphs
            .iter()
            .filter_map(|a| graph_paths.get(&a.asset_id).map(|p| (a.clone(), p.clone())))
            .collect();
        println!(
            "{} graphs, {} names already known — searching each graph's own vocabulary",
            with_paths.len(),
            known.len()
        );
        guess_local(&toc, &cache, &with_paths, &known, &out_path);
        return;
    }

    if !explain_ids.is_empty() {
        for asset in assets.iter().filter(|a| a.span_index == 0) {
            let Some(path) = explain_ids.get(&asset.asset_id) else {
                continue;
            };
            let Ok(raw) = toc.extract_asset_with_cache(asset, &cache) else {
                continue;
            };
            println!("\n=== {path} ({} bytes)", raw.len());
            if let Ok(template) = MaterialTemplate::parse(&raw) {
                for s in &template.samplers {
                    println!(
                        "  sampler  {:#010X} slot={} default={}",
                        s.name_hash, s.slot_index, s.default_path
                    );
                }
                for c in &template.constants {
                    println!("  constant {:#010X} default={:?}", c.name_hash, c.default_values);
                }
            }
            let mut tokens: Vec<String> = tokens_of(&raw).into_iter().collect();
            tokens.sort();
            println!("  {} tokens:", tokens.len());
            for t in &tokens {
                println!("    {t}");
            }
        }
        return;
    }

    // Pass 1 — slot hashes to look for, and the token dictionary, both out of
    // the graphs (one extraction each).
    let targets: Mutex<HashMap<u32, Kind>> = Mutex::new(HashMap::new());
    let dictionary: Mutex<HashSet<String>> = Mutex::new(HashSet::new());

    graphs.par_iter().for_each(|asset| {
        let Ok(raw) = toc.extract_asset_with_cache(asset, &cache) else {
            return;
        };
        let tokens = tokens_of(&raw);
        if let Ok(mut d) = dictionary.lock() {
            d.extend(tokens);
        }
        if let Ok(template) = MaterialTemplate::parse(&raw) {
            if let Ok(mut t) = targets.lock() {
                for s in &template.samplers {
                    t.entry(s.name_hash).or_default().sampler += 1;
                }
                for c in &template.constants {
                    let e = t.entry(c.name_hash).or_default();
                    e.constant += 1;
                    e.arity = e.arity.max(c.default_values.len() as u32);
                }
            }
        }
    });

    if scan_materials {
        materials.par_iter().for_each(|asset| {
            let Ok(raw) = toc.extract_asset_with_cache(asset, &cache) else {
                return;
            };
            let Ok(material) = MaterialFile::parse(&raw) else {
                return;
            };
            let Some(sec) = material.dat1.get_section_data(TAG_MATERIAL_SERIALIZED) else {
                return;
            };
            let Ok(parsed) = MaterialSerialized::parse(sec) else {
                return;
            };
            if let Ok(mut t) = targets.lock() {
                for s in &parsed.samplers {
                    t.entry(s.name_hash).or_default().sampler += 1;
                }
                for c in &parsed.constants {
                    let e = t.entry(c.name_hash).or_default();
                    e.constant += 1;
                    e.arity = e.arity.max(c.values.len() as u32);
                }
            }
        });
    }

    let targets = targets.into_inner().unwrap();
    let mut dictionary = dictionary.into_inner().unwrap();

    for path in &extras {
        match std::fs::read(path) {
            Ok(bytes) => {
                let before = dictionary.len();
                dictionary.extend(tokens_of(&bytes));
                println!("+{} tokens from {path}", dictionary.len() - before);
            }
            Err(e) => eprintln!("skip {path}: {e}"),
        }
    }
    println!(
        "{} distinct slot hashes to name, {} harvested tokens",
        targets.len(),
        dictionary.len()
    );

    // Pass 2 — expand each token into candidate names and hash them.
    let tokens: Vec<&String> = dictionary.iter().collect();
    let hits: Vec<(u32, String)> = tokens
        .par_iter()
        .flat_map_iter(|token| {
            let mut local = Vec::new();
            for candidate in candidates(token) {
                let h = crc32::hash(&candidate);
                if targets.contains_key(&h) {
                    local.push((h, candidate));
                }
            }
            local
        })
        .collect();

    // Group by hash: several candidate strings can hash the same (real
    // synonyms, or a collision) — keep them all, shortest first.
    let mut by_hash: HashMap<u32, Vec<String>> = HashMap::new();
    for (h, name) in hits {
        let entry = by_hash.entry(h).or_default();
        if !entry.contains(&name) {
            entry.push(name);
        }
    }
    for names in by_hash.values_mut() {
        names.sort_by_key(|n| (n.len(), n.clone()));
    }

    let mut lines: Vec<(u32, String)> = Vec::new();
    let mut suspicious = 0usize;
    let mut ambiguous = 0usize;
    for (hash, names) in &by_hash {
        let kind = targets.get(hash).copied().unwrap_or_default();
        let best = &names[0];
        let texture_ish = best.ends_with("_Texture")
            || best.ends_with("_Sampler")
            || best.contains("Map2D");
        let mismatch = (kind.sampler > 0 && kind.constant == 0 && !texture_ish)
            || (kind.constant > 0 && kind.sampler == 0 && texture_ish);
        if mismatch {
            suspicious += 1;
        }
        let mut note = format!(
            "{best} # {hash:#010X} {} uses={}",
            kind.label(),
            kind.sampler + kind.constant
        );
        if names.len() > 1 {
            ambiguous += 1;
            note.push_str(&format!(" ambiguous={}", names[1..].join("|")));
        }
        if mismatch {
            note.push_str(" SUSPICIOUS-kind-mismatch");
        }
        lines.push((*hash, note));
    }
    lines.sort_by(|a, b| a.1.cmp(&b.1));

    let mut text = String::new();
    text.push_str("# Material slot names recovered by material_names_harvest.\n");
    text.push_str("# One name per line; everything after '#' is a comment.\n");
    text.push_str("# Lines marked SUSPICIOUS-kind-mismatch named a sampler slot with a\n");
    text.push_str("# non-texture string (or vice versa) — verify before trusting.\n\n");
    for (_, line) in &lines {
        text.push_str(line);
        text.push('\n');
    }
    std::fs::write(&out_path, text).expect("write output");

    if let Some(path) = &unresolved_path {
        let mut rows: Vec<(u32, Kind)> = targets
            .iter()
            .filter(|(h, _)| !by_hash.contains_key(h))
            .map(|(h, k)| (*h, *k))
            .collect();
        rows.sort_by_key(|(_, k)| std::cmp::Reverse(k.sampler + k.constant));
        let mut text = String::from("# hash,kind,uses,arity\n");
        for (hash, kind) in &rows {
            text.push_str(&format!(
                "{hash:#010X},{},{},{}\n",
                kind.label(),
                kind.sampler + kind.constant,
                kind.arity
            ));
        }
        std::fs::write(path, text).expect("write unresolved list");
        println!("{} unresolved slot hashes → {path}", rows.len());
    }

    println!(
        "resolved {}/{} slot hashes ({} ambiguous, {} kind-mismatched) → {}",
        by_hash.len(),
        targets.len(),
        ambiguous,
        suspicious,
        out_path
    );
}
