//! Runs every structural cross-check from `docs/MODEL_FORMAT.md` over a whole
//! corpus of models, so claims in the doc are backed by more than the handful of
//! samples they were reversed from.
//!
//! usage:
//!   model_audit <dir>                        walk a directory of .model files
//!   model_audit --toc <toc> --archives <dir> walk every model in the archives
//!
//! Options: --limit N, --failures (only print failing assets).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use omnitool_lib::core::crc32;
use omnitool_lib::core::dat1::Dat1;
use omnitool_lib::core::toc::Toc;
use omnitool_lib::tools::model_converter::model::ModelFile;
use omnitool_lib::tools::model_converter::sections::*;

#[derive(Default)]
struct Stats {
    models: usize,
    /// check name -> (passes, failures)
    checks: BTreeMap<&'static str, (usize, usize)>,
    failures: Vec<(String, String)>,
    /// feature flag value -> set of section tags present, intersected as we go
    /// (bit, tag) -> (set&has, set&!has, !set&has, !set&!has)
    bit_section: BTreeMap<(u32, u32), (usize, usize, usize, usize)>,
    unk60_morph: BTreeMap<u32, (usize, usize)>,
    flag_counts: BTreeMap<u32, usize>,
    unk60: BTreeMap<u32, usize>,
    unknown_tags: BTreeMap<u32, usize>,
    uv_shift: BTreeMap<u32, usize>,
    pos_scale: BTreeMap<String, usize>,
}

impl Stats {
    fn check(&mut self, name: &'static str, ok: bool, who: &str, detail: impl FnOnce() -> String) {
        let e = self.checks.entry(name).or_insert((0, 0));
        if ok {
            e.0 += 1;
        } else {
            e.1 += 1;
            if self.failures.len() < 200 {
                self.failures.push((who.to_string(), format!("{name}: {}", detail())));
            }
        }
    }
}

const KNOWN_TAGS: &[u32] = &[
    0x283D0383, 0xA98BE69B, 0x16F3BA18, 0x6B855EED, 0xCCBAFF15, 0xDCA379A2, 0xC61B1FF5,
    0x5240C82B, 0x0859863D, 0x78D9CBDE, 0x3250BB80, 0xDCC88A19, 0x90CDB60C, 0x15DF9D3B,
    0x0AD3A708, 0xEE31971C, 0x9F614FAB, 0x731CBC2E, 0xC5354B60, 0x5CBA9DE9, 0xEFD92E68,
    0x5E709570, 0xA600C108, 0x380A5744, 0xADD1CBD3, 0x06EB7EFC, 0x811902D7, 0x4CCEA4AD,
    0xDF9FDF12, 0xB7380E8C, 0x27CA5246, 0x3C9DABDF, 0xBB7303D5, 0x707F1B58, 0x9A434B29,
    0x5A39FAB7, 0xB25B3163, 0x45079BC5, 0x237D59F1, 0xCD903318, 0x42349A17, 0x244E5823,
    0x5796FEF6, 0xF4CB2F37, 0xBCE86B01, 0x0BA45069, 0x00823787, 0x14D8B13C, 0x5D5CF541,
    0x3F70F60D, 0xC5354B61,
];

fn audit(name: &str, data: &[u8], st: &mut Stats) {
    let Ok(model) = ModelFile::parse(data) else { return };
    let dat1 = &model.dat1;
    if dat1.get_section_data(built::TAG_BUILT).is_none() {
        return;
    }
    st.models += 1;

    let tags: Vec<u32> = dat1.sections.iter().map(|s| s.tag).collect();
    for t in &tags {
        if !KNOWN_TAGS.contains(t) {
            *st.unknown_tags.entry(*t).or_insert(0) += 1;
        }
    }

    // ---- Built ----
    let Some(b) = dat1.get_section_data(built::TAG_BUILT).and_then(Built::parse) else { return };
    *st.flag_counts.entry(b.feature_flags).or_insert(0) += 1;
    for bit in 0..32u32 {
        let set = b.feature_flags >> bit & 1 == 1;
        for t in KNOWN_TAGS {
            let e = st.bit_section.entry((bit, *t)).or_insert((0, 0, 0, 0));
            match (set, tags.contains(t)) {
                (true, true) => e.0 += 1,
                (true, false) => e.1 += 1,
                (false, true) => e.2 += 1,
                (false, false) => e.3 += 1,
            }
        }
    }
    let has_morph = tags.contains(&morph::TAG_ANIM_MORPH_INFO);
    let m = st.unk60_morph.entry(b.unk_0x60 >> 16).or_insert((0, 0));
    m.0 += has_morph as usize;
    m.1 += 1;
    *st.unk60.entry(b.unk_0x60 >> 16).or_insert(0) += 1;
    *st.uv_shift.entry(b.uv_shifts).or_insert(0) += 1;
    *st.pos_scale.entry(format!("1/{:.0}", 1.0 / b.position_scale)).or_insert(0) += 1;

    if let Some(v) = dat1.get_section_data(geo::TAG_VERTEXES) {
        st.check("built.vertex_count == StdVert/16", v.len() / 16 == b.vertex_count as usize, name,
            || format!("{} vs {}", b.vertex_count, v.len() / 16));
    }
    if let Some(i) = dat1.get_section_data(geo::TAG_INDEXES) {
        st.check("built.index_count == Index/2", i.len() / 2 == b.index_count as usize, name,
            || format!("{} vs {}", b.index_count, i.len() / 2));
    }
    // The UV shift should put the largest raw component near 1.0 or above, never
    // far below — a too-small shift would mean the model's UVs never reach 1.
    if let Some(v) = dat1.get_section_data(geo::TAG_VERTEXES) {
        let mut max_raw = 0i32;
        for c in v.chunks_exact(16) {
            for o in [12, 14] {
                let x = i16::from_le_bytes([c[o], c[o + 1]]) as i32;
                max_raw = max_raw.max(x.abs());
            }
        }
        if max_raw > 0 {
            let uv = max_raw as f32 * b.uv0_scale();
            st.check("uv shift plausible (max |uv| >= 0.5)", uv >= 0.5, name, || format!("max |uv| = {uv:.3}"));
        }
    }

    // ---- materials: every slot resolves in the sorted id table ----
    if let Some(m) = dat1
        .get_section_data(looks::TAG_MATERIAL)
        .and_then(|d| MaterialSection::parse(d).ok())
    {
        let mut bad = 0;
        for slot in &m.slots {
            let n = dat1.get_string(slot.name_offset as u32).unwrap_or_default();
            if m.id_for_name(&n).is_none() {
                bad += 1;
            }
        }
        st.check("material slot resolves by ihash(lower(name))", bad == 0, name,
            || format!("{bad}/{} slots unresolved", m.slots.len()));
        st.check("material id table sorted by hash",
            m.ids.windows(2).all(|w| w[0].name_hash <= w[1].name_hash), name, || "unsorted".into());
        // Is ids[i] actually the row for slots[i]?
        let mis = m
            .slots
            .iter()
            .enumerate()
            .filter(|(i, slot)| {
                let n = dat1.get_string(slot.name_offset as u32).unwrap_or_default();
                m.ids[*i].name_hash != crc32::hash(&n.to_ascii_lowercase())
            })
            .count();
        st.check("material ids[i] belongs to slots[i]", mis == 0, name,
            || format!("{mis}/{} slots misaligned", m.slots.len()));
    }

    // ---- looks ----
    let look_count = dat1.get_section_data(look::TAG_LOOK).map(|d| d.len() / 32).unwrap_or(0);
    let subset_count = dat1.get_section_data(meshes::TAG_MESHES).map(|d| d.len() / 64).unwrap_or(0);
    if let Some(lb) = dat1
        .get_section_data(looks::TAG_LOOK_BUILT)
        .and_then(|d| LookBuiltSection::parse(d, look_count).ok())
    {
        let mut mask_bad = 0;
        for l in &lb.looks {
            if expand_mask(&l.bsphere_mask) != l.bspheres {
                mask_bad += 1;
            }
        }
        st.check("look bsphere mask == index list", mask_bad == 0, name, || format!("{mask_bad} looks"));
    }
    if let Some(ls) = dat1.get_section_data(look::TAG_LOOK).and_then(|d| LookSection::parse(d).ok()) {
        let mut over = 0;
        for l in &ls.looks {
            for lod in &l.lods {
                if lod.count > 0 && (lod.start as usize + lod.count as usize) > subset_count {
                    over += 1;
                }
            }
        }
        st.check("look LOD ranges inside subset table", over == 0, name, || format!("{over} ranges"));
    }

    // ---- skeleton ----
    let joint_count = dat1.get_section_data(joints::TAG_JOINTS).map(|d| d.len() / 16).unwrap_or(0);
    if let Some(h) = dat1
        .get_section_data(skeleton::TAG_JOINT_HIERARCHY)
        .and_then(|d| JointHierarchy::parse(d).ok())
    {
        let mirror = dat1.get_section_data(skeleton::TAG_MIRROR_IDS).map(|d| d.len() / 4).unwrap_or(0);
        let leaf = dat1.get_section_data(skeleton::TAG_LEAF_IDS).map(|d| d.len() / 2).unwrap_or(0);
        st.check("hierarchy.joint_count == Joint/16", h.joint_count as usize == joint_count, name,
            || format!("{} vs {joint_count}", h.joint_count));
        st.check("hierarchy.field2 == MirrorIds/4", h.mirror_count as usize == mirror, name,
            || format!("{} vs {mirror}", h.mirror_count));
        st.check("hierarchy.field3 == LeafIds/2", h.leaf_count as usize == leaf, name,
            || format!("{} vs {leaf}", h.leaf_count));
    }
    if let Some(bs) = dat1
        .get_section_data(skeleton::TAG_JOINT_BSPHERES)
        .and_then(|d| JointBspheres::parse(d).ok())
    {
        let bad = (0..bs.spheres.len())
            .filter(|&i| bs.spheres[i].joint % JointBspheres::JOINT_STRIDE != 0
                || bs.joint_index(i) as usize >= joint_count)
            .count();
        st.check("bsphere joint offset / 64 in range", bad == 0, name, || format!("{bad} spheres"));
    }

    // ---- morphs ----
    if let Some(mi) = dat1
        .get_section_data(morph::TAG_ANIM_MORPH_INFO)
        .and_then(|d| AnimMorphInfo::parse(d).ok())
    {
        let data_s = dat1.get_section_data(morph::TAG_ANIM_MORPH_DATA).unwrap_or(&[]);
        let idx_s = dat1.get_section_data(morph::TAG_ANIM_MORPH_INDICES).unwrap_or(&[]);
        let subs = dat1
            .get_section_data(meshes::TAG_MESHES)
            .and_then(|d| MeshDefinition::parse_all(d).ok())
            .unwrap_or_default();
        let (mut size_bad, mut count_bad, mut oob) = (0usize, 0usize, 0usize);
        for e in &mi.entries {
            let mut total = 0usize;
            for s in 0..e.subset_count as usize {
                let (lo, hi) = e.subset_chunks(s);
                total += e.chunks[lo..hi].iter().map(|c| e.chunk_bytes(c.0)).sum::<usize>();
                let d = AnimMorphInfo::decode_subset(e, s, data_s, idx_s);
                if d.len() != e.subset_vertex_counts[s] as usize {
                    count_bad += 1;
                }
                if let Some(md) = subs.get(e.subset_ids[s] as usize) {
                    if d.iter().any(|x| x.vertex >= md.vertex_count) {
                        oob += 1;
                    }
                }
            }
            if total != e.vertex_size as usize {
                size_bad += 1;
            }
        }
        st.check("morph chunk sizes == vertex_size", size_bad == 0, name, || format!("{size_bad} morphs"));
        st.check("morph index run counts == vertex counts", count_bad == 0, name, || format!("{count_bad} subsets"));
        st.check("morph vertex ids in range", oob == 0, name, || format!("{oob} subsets"));
    }

    // ---- splines ----
    if let Some(sp) = dat1.get_section_data(splines::TAG_SPLINES).and_then(|d| parse_splines(d).ok()) {
        let pts = dat1.get_section_data(splines::TAG_SPLINE_POINTS).map(|d| d.len() / 8).unwrap_or(0);
        let sum: u32 = sp.iter().map(|s| s.point_count as u32).sum();
        st.check("spline point_count sum == point records", sum as usize == pts, name,
            || format!("{sum} vs {pts}"));
        if let Some(ss) = dat1
            .get_section_data(splines::TAG_SPLINE_SUBSETS)
            .and_then(|d| SplineSubsets::parse(d).ok())
        {
            st.check("spline groups tile the strand array",
                ss.strand_total() as usize == sp.len(), name,
                || format!("{} vs {}", ss.strand_total(), sp.len()));
        }
    }

    // ---- subsets ----
    if let Some(subs) = dat1
        .get_section_data(meshes::TAG_MESHES)
        .and_then(|d| MeshDefinition::parse_all(d).ok())
    {
        let mat_count = dat1.get_section_data(looks::TAG_MATERIAL).map(|d| d.len() / 32).unwrap_or(0);
        let vtotal = dat1.get_section_data(geo::TAG_VERTEXES).map(|d| d.len() / 16).unwrap_or(0);
        let mut bad_mat = 0;
        let mut bad_range = 0;
        for m in &subs {
            if (m.material_index as usize) >= mat_count {
                bad_mat += 1;
            }
            if (m.vertex_start + m.vertex_count) as usize > vtotal {
                bad_range += 1;
            }
        }
        st.check("subset material index in range", bad_mat == 0, name, || format!("{bad_mat} subsets"));
        st.check("subset vertex range inside StdVert", bad_range == 0, name, || format!("{bad_range} subsets"));
    }
}

fn walk_dir(root: &Path, limit: usize, st: &mut Stats) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            if st.models >= limit {
                return;
            }
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|s| s.to_str()) == Some("model") {
                if let Ok(bytes) = std::fs::read(&p) {
                    audit(&p.file_name().unwrap().to_string_lossy(), &bytes, st);
                }
            }
        }
    }
}

fn walk_archives(toc_path: &Path, archives: &Path, limit: usize, st: &mut Stats) {
    let data = match std::fs::read(toc_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cannot read toc {}: {e}", toc_path.display());
            return;
        }
    };
    let toc = match Toc::parse(&data) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot parse toc: {e}");
            return;
        }
    };
    // Models live only in archives whose name mentions "model", so the other
    // ~30 GB never has to be decompressed.
    let names = toc.archive_filenames();
    let model_archives: Vec<u32> = names
        .iter()
        .enumerate()
        .filter(|(_, n)| n.to_lowercase().contains("model"))
        .map(|(i, _)| i as u32)
        .collect();
    let all_assets = toc.assets();
    let assets: Vec<_> = all_assets
        .into_iter()
        .filter(|a| model_archives.contains(&a.archive_index))
        .collect();
    eprintln!(
        "{} assets in {} model archives ({:?})",
        assets.len(),
        model_archives.len(),
        model_archives.iter().map(|&i| names[i as usize].clone()).collect::<Vec<_>>()
    );
    let cache = toc.archive_cache(archives);
    let mut scanned = 0usize;
    for a in &assets {
        if st.models >= limit {
            break;
        }
        scanned += 1;
        if scanned % 5000 == 0 {
            eprintln!("  {scanned}/{} assets, {} models", assets.len(), st.models);
        }
        let Ok(bytes) = toc.extract_asset_with_cache(a, &cache) else { continue };
        if std::env::var("AUDIT_MAGIC").is_ok() && bytes.len() >= 4 {
            let m = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
            *st.unknown_tags.entry(m).or_insert(0) += 1;
            continue;
        }
        // Cheap pre-filter: only DAT1 payloads carrying a Model Built section.
        if !looks_like_model(&bytes) {
            continue;
        }
        audit(&format!("{:016X}", a.asset_id), &bytes, st);
    }
}

/// Peeks at the DAT1 section table without decompressing anything else.
fn looks_like_model(bytes: &[u8]) -> bool {
    let at = if bytes.len() > 36 && u32::from_le_bytes(bytes[0..4].try_into().unwrap()) != 0x44415431
    {
        36
    } else {
        0
    };
    if bytes.len() < at + 16 {
        return false;
    }
    if u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) != 0x44415431 {
        return false;
    }
    let n = u16::from_le_bytes(bytes[at + 12..at + 14].try_into().unwrap()) as usize;
    (0..n).any(|i| {
        let o = at + 16 + i * 12;
        bytes.len() >= o + 4
            && u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap()) == built::TAG_BUILT
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| {
        args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned()
    };
    let limit: usize = get("--limit").and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
    let only_failures = args.iter().any(|a| a == "--failures");

    let mut st = Stats::default();
    let t0 = std::time::Instant::now();

    if let (Some(toc), Some(arch)) = (get("--toc"), get("--archives")) {
        walk_archives(Path::new(&toc), Path::new(&arch), limit, &mut st);
    } else if let Some(dir) = args.first().filter(|a| !a.starts_with("--")) {
        walk_dir(&PathBuf::from(dir), limit, &mut st);
    } else {
        eprintln!("usage: model_audit <dir> | --toc <toc> --archives <dir> [--limit N] [--failures]");
        std::process::exit(1);
    }

    println!("\n=== audited {} models in {:.1}s ===", st.models, t0.elapsed().as_secs_f32());
    if std::env::var("AUDIT_MAGIC").is_ok() {
        let mut v: Vec<_> = st.unknown_tags.iter().collect();
        v.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
        println!("-- asset magic histogram (top 20) --");
        for (m, c) in v.into_iter().take(20) {
            println!("  0x{m:08X}: {c}");
        }
        return;
    }
    if st.models == 0 {
        return;
    }

    println!("\n-- cross-checks --");
    let mut failed_any = false;
    for (name, (ok, bad)) in &st.checks {
        let flag = if *bad == 0 { "ok  " } else { "FAIL" };
        if *bad > 0 {
            failed_any = true;
        }
        println!("  {flag} {name:<46} {ok} passed, {bad} failed");
    }

    if !st.failures.is_empty() {
        println!("\n-- first {} failures --", st.failures.len());
        for (who, what) in &st.failures {
            println!("  {who}  {what}");
        }
    }

    if only_failures {
        return;
    }

    println!("\n-- Built feature flags (value: count) --");
    for (v, c) in &st.flag_counts {
        println!("  0x{v:08X}: {c}");
    }

    // For each bit, the section its state predicts best, as
    // P(section | bit set) vs P(section | bit clear). Statistical rather than
    // all-or-nothing, so a few outliers among 11k models cannot hide a signal.
    println!("
-- feature bit -> best-predicted section --");
    for bit in 0..32u32 {
        let mut best: Option<(f64, u32)> = None;
        for t in KNOWN_TAGS {
            let Some(&(sh, sn, ch, cn)) = st.bit_section.get(&(bit, *t)) else { continue };
            if sh + sn == 0 || ch + cn == 0 {
                continue;
            }
            let sep = sh as f64 / (sh + sn) as f64 - ch as f64 / (ch + cn) as f64;
            if best.map_or(true, |x| sep > x.0) {
                best = Some((sep, *t));
            }
        }
        if let Some((sep, tag)) = best {
            if sep > 0.9 {
                let &(sh, sn, ch, cn) = st.bit_section.get(&(bit, tag)).unwrap();
                println!(
                    "  bit {bit:2} (0x{:08X}) -> 0x{tag:08X}  P(sec|set)={:.3} ({}/{})  P(sec|clear)={:.3} ({}/{})",
                    1u32 << bit,
                    sh as f64 / (sh + sn) as f64, sh, sh + sn,
                    ch as f64 / (ch + cn) as f64, ch, ch + cn
                );
            }
        }
    }

    println!("\n-- Built 0x60 high u16 --");
    for (v, c) in &st.unk60 {
        let m = st.unk60_morph.get(v).copied().unwrap_or((0, 0));
        println!("  {v}: {c} models, {} with morph info", m.0);
    }
    println!("\n-- UV shift field --");
    for (v, c) in &st.uv_shift {
        println!("  0x{v:X}: {c}");
    }
    println!("\n-- position scale --");
    for (v, c) in &st.pos_scale {
        println!("  {v}: {c}");
    }
    if !st.unknown_tags.is_empty() {
        println!("\n-- section tags not in the doc --");
        for (t, c) in &st.unknown_tags {
            println!("  0x{t:08X}: {c} models  (crc name unknown)");
        }
    }

    if failed_any {
        std::process::exit(2);
    }
}

#[allow(dead_code)]
fn unused(_: &Dat1, _: fn(&str) -> u32) {
    let _ = crc32::hash;
}
