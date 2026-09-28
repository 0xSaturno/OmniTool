//! Measures and verifies the navigation data of the shipped PC build.
//!
//!   cargo run --release --bin nav_probe -- "<game dir>" [mode] [filter]
//!
//! Modes (the trailing number is a limit, and `art` samples evenly rather than
//! taking a prefix, because the zone list is sorted by path):
//!   census     every section of every `*navdata.zone`, with byte totals
//!   sections   parse all four nav sections on every zone that has them
//!   sizes      re-derive the element sizes from the payload-size identity alone
//!   fields     per-record field checks: winding, adjacency, bspheres, clues
//!   art        the geometric test: does the mesh sit on the drawn art?
//!   pair       what art there is to test one tile against, and how the
//!              footprints line up -- run this before trusting `art`
//!   tails      what the second polygon class is, measured against the first
//!   units      one line per unit, and which units have no reachable polygon
//!   container  every container, both shapes, with its implied slot count
//!   clue       raw clue records with their group, for reading the layout off
//!   cmd        drive every \`level_nav_*\` Tauri command and check its buffers
//!              (needs \`--features nav-commands\` and the lib.rs \`mod\` line)
//!   dump       one zone, in numbers and hex
//!
//! `art` is memory-hungry: it holds the art of a 3x3 tile block per worker.
//! Cap it with `RAYON_NUM_THREADS=8` on a 16-thread desktop.
//!
//! Read-only. Nothing is written to the game directory.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use byteorder::{ByteOrder, LE};
use omnitool_lib::core::level;
use omnitool_lib::core::nav::{self, EdgeType, NavUnit};
use omnitool_lib::core::zone::{self, Zone};
use rayon::prelude::*;

fn hex(d: &[u8], base: usize, n: usize) {
    let n = n.min(d.len());
    let mut i = 0;
    while i < n {
        let end = (i + 16).min(n);
        print!("  {:08X}  ", base + i);
        for j in i..i + 16 {
            if j < end {
                print!("{:02X} ", d[j]);
            } else {
                print!("   ");
            }
        }
        print!(" |");
        for j in i..end {
            let c = d[j];
            print!("{}", if (0x20..0x7f).contains(&c) { c as char } else { b'.' as char });
        }
        print!("| ");
        for j in (i..end).step_by(4) {
            if j + 4 <= end {
                let u = LE::read_u32(&d[j..]);
                let f = f32::from_bits(u);
                if f != 0.0 && f.abs() > 1e-6 && f.abs() < 1e7 {
                    print!("{:>12.3} ", f);
                } else {
                    print!("{:>12} ", u);
                }
            }
        }
        println!();
        i += 16;
    }
}

/// A counter that prints as `hits/total (pct)`.
#[derive(Default, Clone, Copy)]
struct Tally {
    hit: u64,
    tot: u64,
}
impl Tally {
    fn add(&mut self, ok: bool) {
        self.tot += 1;
        self.hit += ok as u64;
    }
    fn merge(&mut self, o: Tally) {
        self.hit += o.hit;
        self.tot += o.tot;
    }
}
impl std::fmt::Display for Tally {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.tot == 0 {
            write!(f, "{}/{}", self.hit, self.tot)
        } else {
            write!(
                f,
                "{}/{} ({:.3}%)",
                self.hit,
                self.tot,
                100.0 * self.hit as f64 / self.tot as f64
            )
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let game = args
        .next()
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let mode = args.next().unwrap_or_else(|| "census".into());
    let filter = args.next().unwrap_or_default();
    let limit: usize = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX);

    let idx = level::get_index(&game, None).expect("level index");

    let navs: Vec<&level::ZoneRec> = idx
        .zones
        .iter()
        .filter(|z| z.path.contains("navdata"))
        .filter(|z| filter.is_empty() || z.path.contains(&filter))
        .collect();

    match mode.as_str() {
        "census" => census(&idx, &navs),
        "sections" => sections(&idx, &navs),
        "sizes" => sizes(&idx, &navs),
        "fields" => fields(&idx, &navs, limit),
        "art" => art(&idx, &navs, limit),
        "dump" => dump(&idx, &navs),
        "pair" => pair(&idx, &navs, limit),
        "tails" => tails(&idx, &navs, limit),
        "units" => units(&idx, &navs, limit),
        "clue" => clue(&idx, &navs, limit),
        #[cfg(feature = "nav-commands")]
        "cmd" => cmd(&game),
        #[cfg(not(feature = "nav-commands"))]
        "cmd" => println!(
            "build with --features nav-commands, and add `pub mod commands_level_nav;` to lib.rs"
        ),
        "container" => container(&idx, &navs, limit),
        "which" => {
            for z in &navs {
                let Ok(bytes) = idx.extract(z.id) else { continue };
                let Ok(zone) = Zone::parse(&bytes) else { continue };
                let tags: Vec<String> = zone.dat1.sections.iter().map(|s| format!("{:08X}", s.tag)).collect();
                println!("{}  {}  {:016X}  {}", z.path, z.kind, z.id, tags.join(","));
            }
        }
        _ => println!("modes: census sections sizes fields art dump"),
    }
}

// ---------------------------------------------------------------------------

fn census(idx: &level::LevelIndex, navs: &[&level::ZoneRec]) {
    let mut per_tag: BTreeMap<u32, (u64, u64)> = BTreeMap::new();
    let mut names: BTreeMap<String, usize> = BTreeMap::new();
    let mut combos: BTreeMap<String, usize> = BTreeMap::new();
    for z in navs {
        *names
            .entry(z.path.rsplit('/').next().unwrap_or("").to_string())
            .or_default() += 1;
        let Ok(bytes) = idx.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&bytes) else { continue };
        let mut tags: Vec<String> = Vec::new();
        for s in &zone.dat1.sections {
            let e = per_tag.entry(s.tag).or_default();
            e.0 += 1;
            e.1 += s.size as u64;
            tags.push(format!("{:08X}", s.tag));
        }
        tags.sort();
        *combos.entry(tags.join("+")).or_default() += 1;
    }
    println!("{} nav zones", navs.len());
    for (n, c) in &names {
        println!("  {c:5} x {n}");
    }
    println!("sections:");
    for (t, (c, b)) in &per_tag {
        println!("  {t:08X}  {c:5} zones  {b:>12} bytes  {}", tag_name(*t));
    }
    println!("section sets:");
    for (k, c) in &combos {
        println!("  {c:5} x {k}");
    }
}

fn tag_name(t: u32) -> &'static str {
    match t {
        nav::NAV_CONTAINER_TAG => "kZoneNavBuiltContainer",
        nav::NAV_UNIT_TAG => "kZoneNavBuiltUnit",
        nav::NAV_TILE_CACHE_TAG => "nav tile cache (Detour dtCompressedTile stream)",
        nav::NAV_3D_TAG => "nav 3D (built region + SVO nodes)",
        nav::NAV_VERSION_TAG => "four bytes, name unknown",
        _ => "",
    }
}

// ---------------------------------------------------------------------------

#[derive(Default)]
struct SectionStats {
    container_ok: u64,
    container_bad: Vec<String>,
    container_layers: BTreeMap<i32, u64>,
    container_slots: Tally,
    container_scale: BTreeMap<i16, u64>,
    unit_ok: u64,
    unit_bad: Vec<String>,
    unit_layers: BTreeMap<usize, u64>,
    unit_scale: BTreeMap<i16, u64>,
    unit_problems: Vec<String>,
    unit_self_id: Tally,
    unit_id_is_container: Tally,
    unit_container_named: Tally,
    unit_id_in_toc: Tally,
    unit_cid_in_toc: Tally,
    tc_ok: u64,
    tc_bad: Vec<String>,
    tc_tiles: u64,
    tc_magic: Tally,
    tc_version: BTreeMap<u32, u64>,
    tc_layers: BTreeMap<usize, u64>,
    tc_extent: Tally,
    tc_group_match: Tally,
    n3_ok: u64,
    n3_bad: Vec<String>,
    n3_header: BTreeMap<usize, u64>,
    n3_nodes_div: BTreeMap<usize, u64>,
    n3_extent: Tally,
    version_word: BTreeMap<u32, u64>,
    totals: nav::NavTotals,
}

fn sections(idx: &level::LevelIndex, navs: &[&level::ZoneRec]) {
    // Every region's containernavdata.zone, by region key, so a unit's +0x0C can
    // be checked against the thing it is supposed to name.
    let container_of: HashMap<String, u64> = idx
        .zones
        .iter()
        .filter(|z| z.path.ends_with("containernavdata.zone"))
        .map(|z| (z.region.clone(), z.id))
        .collect();
    let st: SectionStats = navs
        .par_iter()
        .fold(SectionStats::default, |mut st, z| {
            let Ok(bytes) = idx.extract(z.id) else { return st };
            let Ok(zone) = Zone::parse(&bytes) else { return st };

            if let Some(w) = nav::nav_version_word(&zone) {
                *st.version_word.entry(w).or_default() += 1;
            }

            if let Some(d) = zone.dat1.get_section_data(nav::NAV_CONTAINER_TAG) {
                match nav::NavContainer::parse(d) {
                    Ok(c) => {
                        st.container_ok += 1;
                        *st.container_layers.entry(c.layer_count).or_default() += 1;
                        *st.container_scale.entry(c.log_scale).or_default() += 1;
                        st.container_slots.add(c.slots == c.implied_slots());
                    }
                    Err(e) => st.container_bad.push(format!("{}: {e}", z.path)),
                }
            }

            if let Some(d) = zone.dat1.get_section_data(nav::NAV_UNIT_TAG) {
                match NavUnit::parse(d) {
                    Ok(u) => {
                        st.unit_ok += 1;
                        *st.unit_layers.entry(u.layers).or_default() += 1;
                        *st.unit_scale.entry(u.log_scale).or_default() += 1;
                        st.unit_self_id.add(u.asset_id == z.id);
                        st.unit_id_is_container.add(u.asset_id == u.container_id);
                        // Does +0x0C name the containernavdata.zone of this
                        // zone's own region?
                        if let Some(cid) = container_of.get(&z.region) {
                            st.unit_container_named.add(u.container_id == *cid);
                        }
                        // Are the two words asset ids the TOC knows at all?
                        st.unit_id_in_toc.add(idx.asset(u.asset_id).is_some());
                        st.unit_cid_in_toc.add(idx.asset(u.container_id).is_some());
                        let t = u.totals();
                        st.totals.groups += t.groups;
                        st.totals.empty_groups += t.empty_groups;
                        st.totals.verts += t.verts;
                        st.totals.polys += t.polys;
                        st.totals.disconnected += t.disconnected;
                        for i in 0..4 {
                            st.totals.edges[i] += t.edges[i];
                        }
                        for p in u.validate() {
                            if st.unit_problems.len() < 40 {
                                st.unit_problems.push(format!("{}: {p}", z.path));
                            }
                        }
                        // Each group's own position must equal the grid slot it
                        // occupies, which is what proves the layer-major order.
                        let (nx, _nz) = u.grid();
                        let cells = nx * (u.max_tile[1] - u.min_tile[1]).max(0) as usize;
                        for g in &u.groups {
                            if g.is_empty() || cells == 0 {
                                continue;
                            }
                            let within = g.index % cells;
                            let gx = u.min_tile[0] as i32 + (within % nx) as i32;
                            let gz = u.min_tile[1] as i32 + (within / nx) as i32;
                            if gx * nav::TILE_SIZE_M != g.x || gz * nav::TILE_SIZE_M != g.z {
                                if st.unit_problems.len() < 40 {
                                    st.unit_problems.push(format!(
                                        "{}: group {} at ({},{}) but slot says ({},{})",
                                        z.path,
                                        g.index,
                                        g.x,
                                        g.z,
                                        gx * nav::TILE_SIZE_M,
                                        gz * nav::TILE_SIZE_M
                                    ));
                                }
                            }
                        }
                    }
                    Err(e) => st.unit_bad.push(format!("{}: {e}", z.path)),
                }
            }

            if let Some(d) = zone.dat1.get_section_data(nav::NAV_TILE_CACHE_TAG) {
                match nav::tile_cache_entries(d) {
                    Ok((tiles, layers)) => {
                        st.tc_ok += 1;
                        st.tc_tiles += tiles.len() as u64;
                        *st.tc_layers.entry(layers).or_default() += 1;
                        // Every tile must be a Detour tile-cache layer whose
                        // AABB is its own 16 m column.
                        let mut groups: BTreeSet<(i32, i32)> = BTreeSet::new();
                        if let Some(du) = zone.dat1.get_section_data(nav::NAV_UNIT_TAG) {
                            if let Ok(u) = NavUnit::parse(du) {
                                for g in &u.groups {
                                    if !g.is_empty() {
                                        groups.insert((g.x, g.z));
                                    }
                                }
                            }
                        }
                        for t in &tiles {
                            st.tc_magic.add(t.magic == nav::TILE_CACHE_MAGIC);
                            *st.tc_version.entry(t.version).or_default() += 1;
                            st.tc_extent.add(
                                (t.bmax[0] - t.bmin[0] - nav::TILE_SIZE_M as f32).abs() < 1e-3
                                    && (t.bmax[2] - t.bmin[2] - nav::TILE_SIZE_M as f32).abs()
                                        < 1e-3
                                    && (t.bmin[0] - (t.tx * nav::TILE_SIZE_M) as f32).abs() < 1e-3
                                    && (t.bmin[2] - (t.tz * nav::TILE_SIZE_M) as f32).abs() < 1e-3,
                            );
                            st.tc_group_match.add(groups.contains(&(
                                t.tx * nav::TILE_SIZE_M,
                                t.tz * nav::TILE_SIZE_M,
                            )));
                        }
                    }
                    Err(e) => st.tc_bad.push(format!("{}: {e}", z.path)),
                }
            }

            if let Some(d) = zone.dat1.get_section_data(nav::NAV_3D_TAG) {
                match nav::Nav3D::parse(d) {
                    Ok(n) => {
                        st.n3_ok += 1;
                        *st.n3_header.entry(n.header_size).or_default() += 1;
                        for s in [8usize, 12, 16, 20, 24, 28, 32, 48, 64] {
                            if n.node_bytes % s == 0 {
                                *st.n3_nodes_div.entry(s).or_default() += 1;
                            }
                        }
                        st.n3_extent.add(
                            n.extent.iter().all(|&v| v > 0.0 && v < 100_000.0)
                                && n.min.iter().all(|&v| v.abs() < 100_000.0),
                        );
                    }
                    Err(e) => st.n3_bad.push(format!("{}: {e}", z.path)),
                }
            }
            st
        })
        .reduce(SectionStats::default, |mut a, b| {
            macro_rules! m {
                ($($f:ident)+) => { $( a.$f += b.$f; )+ };
            }
            m!(container_ok unit_ok tc_ok tc_tiles n3_ok);
            a.container_bad.extend(b.container_bad);
            a.unit_bad.extend(b.unit_bad);
            a.tc_bad.extend(b.tc_bad);
            a.n3_bad.extend(b.n3_bad);
            a.unit_problems.extend(b.unit_problems);
            a.container_slots.merge(b.container_slots);
            a.unit_self_id.merge(b.unit_self_id);
            a.unit_id_is_container.merge(b.unit_id_is_container);
            a.unit_container_named.merge(b.unit_container_named);
            a.unit_id_in_toc.merge(b.unit_id_in_toc);
            a.unit_cid_in_toc.merge(b.unit_cid_in_toc);
            a.tc_magic.merge(b.tc_magic);
            a.tc_extent.merge(b.tc_extent);
            a.tc_group_match.merge(b.tc_group_match);
            a.n3_extent.merge(b.n3_extent);
            for (k, v) in b.container_layers {
                *a.container_layers.entry(k).or_default() += v;
            }
            for (k, v) in b.container_scale {
                *a.container_scale.entry(k).or_default() += v;
            }
            for (k, v) in b.unit_layers {
                *a.unit_layers.entry(k).or_default() += v;
            }
            for (k, v) in b.unit_scale {
                *a.unit_scale.entry(k).or_default() += v;
            }
            for (k, v) in b.tc_version {
                *a.tc_version.entry(k).or_default() += v;
            }
            for (k, v) in b.tc_layers {
                *a.tc_layers.entry(k).or_default() += v;
            }
            for (k, v) in b.n3_header {
                *a.n3_header.entry(k).or_default() += v;
            }
            for (k, v) in b.n3_nodes_div {
                *a.n3_nodes_div.entry(k).or_default() += v;
            }
            for (k, v) in b.version_word {
                *a.version_word.entry(k).or_default() += v;
            }
            let t = b.totals;
            a.totals.groups += t.groups;
            a.totals.empty_groups += t.empty_groups;
            a.totals.verts += t.verts;
            a.totals.polys += t.polys;
            a.totals.disconnected += t.disconnected;
            for i in 0..4 {
                a.totals.edges[i] += t.edges[i];
            }
            a
        });

    println!("== 3D8DBDB8 (4 bytes) ==");
    for (w, c) in &st.version_word {
        println!("  {w:08X}  x{c}");
    }

    println!("\n== 04C19E69  kZoneNavBuiltContainer ==");
    println!("  parsed {} / failed {}", st.container_ok, st.container_bad.len());
    for b in st.container_bad.iter().take(5) {
        println!("    {b}");
    }
    println!("  layer count    {:?}", st.container_layers);
    println!("  log2 unit size {:?}", st.container_scale);
    println!("  stored slot count == implied_slots  {}", st.container_slots);

    println!("\n== D124430B  kZoneNavBuiltUnit ==");
    println!("  parsed {} / failed {}", st.unit_ok, st.unit_bad.len());
    for b in st.unit_bad.iter().take(5) {
        println!("    {b}");
    }
    println!("  inferred layers   {:?}", st.unit_layers);
    println!("  log2 tile size    {:?}", st.unit_scale);
    println!("  +0x04 == own asset id           {}", st.unit_self_id);
    println!("  +0x04 == +0x0C                  {}", st.unit_id_is_container);
    println!(
        "  +0x0C == the region containernavdata.zone id  {}",
        st.unit_container_named
    );
    println!("  +0x04 names an asset in the TOC  {}", st.unit_id_in_toc);
    println!("  +0x0C names an asset in the TOC  {}", st.unit_cid_in_toc);
    println!("  structural problems: {}", st.unit_problems.len());
    for p in st.unit_problems.iter().take(10) {
        println!("    {p}");
    }
    let t = st.totals;
    println!(
        "  groups {} with data, {} empty; verts {} polys {} (+{} disconnected)",
        t.groups, t.empty_groups, t.verts, t.polys, t.disconnected
    );
    println!(
        "  edges internal {} external {} custom {} border {}",
        t.edges[0], t.edges[1], t.edges[2], t.edges[3]
    );

    println!("\n== A903D8F1  nav tile cache ==");
    println!("  framed {} / failed {}", st.tc_ok, st.tc_bad.len());
    for b in st.tc_bad.iter().take(5) {
        println!("    {b}");
    }
    println!("  tiles {}", st.tc_tiles);
    println!("  tile magic == 'RLTD'   {}", st.tc_magic);
    println!("  tile header version    {:?}", st.tc_version);
    println!("  layer terminators      {:?}", st.tc_layers);
    println!("  AABB is the tile's own 16 m column  {}", st.tc_extent);
    println!("  tile coords match a non-empty nav group  {}", st.tc_group_match);

    println!("\n== 339AEC45  nav 3D ==");
    println!("  parsed {} / failed {}", st.n3_ok, st.n3_bad.len());
    for b in st.n3_bad.iter().take(5) {
        println!("    {b}");
    }
    println!("  sizeof(built region) {:?}", st.n3_header);
    println!("  node bytes divisible by {:?}", st.n3_nodes_div);
    println!("  min/extent plausible world metres  {}", st.n3_extent);
}

// ---------------------------------------------------------------------------

/// Re-derive every element size from nothing but the payload-size identity.
fn sizes(idx: &level::LevelIndex, navs: &[&level::ZoneRec]) {
    let a16 = |n: usize| (n + 15) & !15usize;
    let sols: Vec<(Vec<usize>, Vec<usize>, Vec<usize>, Vec<usize>, Vec<usize>, u64)> = navs
        .par_iter()
        .fold(
            || {
                (
                    (1..=64).collect::<Vec<usize>>(),
                    (1..=64).collect::<Vec<usize>>(),
                    (1..=64).collect::<Vec<usize>>(),
                    (1..=128).collect::<Vec<usize>>(),
                    (1..=64).collect::<Vec<usize>>(),
                    0u64,
                )
            },
            |mut acc, z| {
                let Ok(bytes) = idx.extract(z.id) else { return acc };
                let Ok(zone) = Zone::parse(&bytes) else { return acc };
                let Some(d) = zone.dat1.get_section_data(nav::NAV_UNIT_TAG) else { return acc };
                let Ok(u) = NavUnit::parse(d) else { return acc };
                for g in &u.groups {
                    if g.is_empty() {
                        continue;
                    }
                    acc.5 += 1;
                    let vs = g.edge_offset - g.vert_offset;
                    let es = g.poly_offset - g.edge_offset;
                    let ps = g.clue_offset - g.poly_offset;
                    let cs = g.bsphere_offset - g.clue_offset;
                    let bs = g.data_size - (g.bsphere_offset - g.vert_offset);
                    let et = g.edge_count();
                    let tp = g.total_poly_count();
                    let cc = g.edge_counts[EdgeType::Custom as usize];
                    acc.0.retain(|&s| g.vert_count == 0 || a16(g.vert_count * s) == vs);
                    acc.1.retain(|&s| et == 0 || a16(et * s) == es);
                    acc.2.retain(|&s| tp == 0 || a16(tp * s) == ps);
                    acc.3.retain(|&s| cc == 0 || a16(cc * s) == cs);
                    acc.4.retain(|&s| g.poly_count == 0 || a16(g.poly_count * s) == bs);
                }
                acc
            },
        )
        .collect();
    let mut v = sols[0].clone();
    for s in &sols[1..] {
        v.0.retain(|x| s.0.contains(x));
        v.1.retain(|x| s.1.contains(x));
        v.2.retain(|x| s.2.contains(x));
        v.3.retain(|x| s.3.contains(x));
        v.4.retain(|x| s.4.contains(x));
        v.5 += s.5;
    }
    println!("{} non-empty groups constrain the five strides:", v.5);
    println!("  ec3)               {:?}", v.0);
    println!("  gNavEdge)          {:?}", v.1);
    println!("  gNavPoly)          {:?}", v.2);
    println!("  lueData)           {:?}", v.3);
    println!("  ntersection::Sphere) {:?}", v.4);
}

// ---------------------------------------------------------------------------

#[derive(Default)]
struct FieldStats {
    // vertices
    vert_in_column: Tally,
    vmin: [f32; 3],
    vmax: [f32; 3],
    // polys, split connected / disconnected
    p_vert_ok: [Tally; 2],
    p_edge_ok: [Tally; 2],
    p_edge_unset: [Tally; 2],
    p_island_zero: [Tally; 2],
    p_flags: [BTreeMap<u8, u64>; 2],
    p_pad_zero: [Tally; 2],
    p_handle_zero: [Tally; 2],
    p_degenerate: [Tally; 2],
    p_upright: [Tally; 2],
    p_area: Vec<f64>,
    // edge -> poly -> edge closure
    e_type_matches_rank: Tally,
    e_owner_in_range: Tally,
    e_owner_lists_edge: Tally,
    e_verts_are_poly_verts: Tally,
    e_verts_consecutive: Tally,
    e_internal_target_ok: Tally,
    e_border_target_unset: Tally,
    e_external_target_adjacent: Tally,
    e_target_group_dirs: BTreeMap<(i32, i32), u64>,
    // bspheres
    bs_covers_poly: Tally,
    bs_is_circumcircle: Tally,
    bs_slack: Vec<f64>,
    // clues
    clue_in_column: Tally,
    clue_in_column_exact: Tally,
    clue_w_zero: Tally,
    clue_uid_high: BTreeMap<u32, u64>,
    clue_dist: Vec<f64>,
    clue_ability: BTreeMap<u32, u64>,
    clue_entity: BTreeMap<u32, u64>,
    clue_uids: BTreeSet<u64>,
    zones: u64,
}

fn merge_map<K: Ord, V: std::ops::AddAssign + Default>(a: &mut BTreeMap<K, V>, b: BTreeMap<K, V>) {
    for (k, v) in b {
        *a.entry(k).or_default() += v;
    }
}

fn fields(idx: &level::LevelIndex, navs: &[&level::ZoneRec], limit: usize) {
    let picked: Vec<&&level::ZoneRec> = navs.iter().take(limit).collect();
    let st = picked
        .par_iter()
        .fold(
            || FieldStats {
                vmin: [f32::MAX; 3],
                vmax: [f32::MIN; 3],
                ..Default::default()
            },
            |mut st, z| {
                let Ok(bytes) = idx.extract(z.id) else { return st };
                let Ok(zone) = Zone::parse(&bytes) else { return st };
                let Some(d) = zone.dat1.get_section_data(nav::NAV_UNIT_TAG) else { return st };
                let Ok(u) = NavUnit::parse(d) else { return st };
                st.zones += 1;
                for g in &u.groups {
                    if g.is_empty() {
                        continue;
                    }
                    let verts = u.verts(g);
                    let polys = u.polys(g);
                    let edges = u.edges(g);
                    let clues = u.clues(g);
                    let bs = u.bspheres(g);
                    let (gx, gz) = (g.x as f32, g.z as f32);
                    let t = nav::TILE_SIZE_M as f32;

                    for v in &verts {
                        st.vert_in_column.add(
                            v[0] >= gx - 0.01
                                && v[0] <= gx + t + 0.01
                                && v[2] >= gz - 0.01
                                && v[2] <= gz + t + 0.01,
                        );
                        for i in 0..3 {
                            st.vmin[i] = st.vmin[i].min(v[i]);
                            st.vmax[i] = st.vmax[i].max(v[i]);
                        }
                    }

                    for (pi, p) in polys.iter().enumerate() {
                        let k = usize::from(pi >= g.poly_count);
                        st.p_vert_ok[k]
                            .add(p.verts.iter().all(|&v| (v as usize) < verts.len()));
                        st.p_edge_ok[k].add(p.edges.iter().all(|&e| {
                            e == nav::INVALID_INDEX || (e as usize) < edges.len()
                        }));
                        st.p_edge_unset[k]
                            .add(p.edges.iter().all(|&e| e == nav::INVALID_INDEX));
                        st.p_island_zero[k].add(p.island == 0);
                        *st.p_flags[k].entry(p.flags).or_default() += 1;
                        st.p_pad_zero[k].add(p.pad == 0);
                        st.p_handle_zero[k].add(p.modify_handle == 0);
                        if p.verts.iter().all(|&v| (v as usize) < verts.len()) {
                            let a = verts[p.verts[0] as usize];
                            let b = verts[p.verts[1] as usize];
                            let c = verts[p.verts[2] as usize];
                            let n = cross(sub(b, a), sub(c, a));
                            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                            st.p_degenerate[k].add(len < 1e-9);
                            st.p_upright[k].add(n[1] > 0.0);
                            if st.p_area.len() < 200_000 {
                                st.p_area.push(0.5 * len as f64);
                            }
                        }
                    }

                    for kind in [
                        EdgeType::Internal,
                        EdgeType::External,
                        EdgeType::Custom,
                        EdgeType::Border,
                    ] {
                        for ei in g.edge_range(kind) {
                            let Some(e) = edges.get(ei) else { continue };
                            st.e_type_matches_rank.add(e.edge_type == kind as u16);
                            let owner = e.poly as usize;
                            st.e_owner_in_range.add(owner < polys.len());
                            if owner >= polys.len() {
                                continue;
                            }
                            let p = &polys[owner];
                            st.e_owner_lists_edge
                                .add(p.edges.iter().any(|&x| x as usize == ei));
                            if kind == EdgeType::Custom {
                                continue;
                            }
                            let (a, b) = (e.vert_a, e.vert_b);
                            st.e_verts_are_poly_verts
                                .add(p.verts.contains(&a) && p.verts.contains(&b));
                            let mut consec = false;
                            for i in 0..3 {
                                if p.verts[i] == a && p.verts[(i + 1) % 3] == b {
                                    consec = true;
                                }
                            }
                            st.e_verts_consecutive.add(consec);
                            match kind {
                                EdgeType::Internal => {
                                    st.e_internal_target_ok.add(
                                        (e.target_poly as usize) < polys.len()
                                            && e.target_group() == Some((g.x, g.z)),
                                    );
                                }
                                EdgeType::Border => {
                                    st.e_border_target_unset.add(
                                        e.target_poly == nav::INVALID_INDEX
                                            && e.target_group().is_none(),
                                    );
                                }
                                EdgeType::External => {
                                    let tg = e.target_group();
                                    let ok = match tg {
                                        None => false,
                                        Some((tx, tz)) => {
                                            let dx = (tx - g.x) / nav::TILE_SIZE_M;
                                            let dz = (tz - g.z) / nav::TILE_SIZE_M;
                                            (tx - g.x) % nav::TILE_SIZE_M == 0
                                                && (tz - g.z) % nav::TILE_SIZE_M == 0
                                                && dx.abs() <= 1
                                                && dz.abs() <= 1
                                                && (dx == 0 || dz == 0)
                                        }
                                    };
                                    st.e_external_target_adjacent.add(ok);
                                    if let Some((tx, tz)) = tg {
                                        let d = (
                                            (tx - g.x) / nav::TILE_SIZE_M,
                                            (tz - g.z) / nav::TILE_SIZE_M,
                                        );
                                        *st.e_target_group_dirs.entry(d).or_default() += 1;
                                    }
                                }
                                EdgeType::Custom => {}
                            }
                        }
                    }

                    for (pi, s) in bs.iter().enumerate() {
                        let Some(p) = polys.get(pi) else { continue };
                        if p.verts.iter().any(|&v| (v as usize) >= verts.len()) {
                            continue;
                        }
                        let mut worst = 0f32;
                        for &vi in &p.verts {
                            let v = verts[vi as usize];
                            let d = dist(v, s.centre);
                            worst = worst.max(d);
                        }
                        st.bs_covers_poly.add(worst <= s.radius + 1e-3);
                        st.bs_is_circumcircle.add((worst - s.radius).abs() <= 1e-3);
                        if st.bs_slack.len() < 200_000 {
                            st.bs_slack.push((s.radius - worst) as f64);
                        }
                    }

                    for c in &clues {
                        // Two bars: the group's own 16 m column, and the column
                        // plus a tile of slack either way. A clue actor stands
                        // near the edge it links across, so the loose bar is the
                        // one that says the field is a position at all.
                        st.clue_in_column_exact.add(
                            c.position[0] >= gx - 0.01
                                && c.position[0] <= gx + t + 0.01
                                && c.position[2] >= gz - 0.01
                                && c.position[2] <= gz + t + 0.01,
                        );
                        st.clue_in_column.add(
                            c.position[0] >= gx - t
                                && c.position[0] <= gx + 2.0 * t
                                && c.position[2] >= gz - t
                                && c.position[2] <= gz + 2.0 * t,
                        );
                        st.clue_w_zero.add(c.required_entity == 0);
                        *st.clue_uid_high
                            .entry((c.actor_uid >> 32) as u32)
                            .or_default() += 1;
                        if c.distance.is_finite() {
                            st.clue_dist.push(c.distance as f64);
                        }
                        *st.clue_ability.entry(c.required_ability).or_default() += 1;
                        *st.clue_entity.entry(c.required_entity).or_default() += 1;
                        if c.actor_uid != 0 {
                            st.clue_uids.insert(c.actor_uid);
                        }
                    }
                }
                st
            },
        )
        .reduce(
            || FieldStats {
                vmin: [f32::MAX; 3],
                vmax: [f32::MIN; 3],
                ..Default::default()
            },
            |mut a, b| {
                a.zones += b.zones;
                a.vert_in_column.merge(b.vert_in_column);
                for i in 0..3 {
                    a.vmin[i] = a.vmin[i].min(b.vmin[i]);
                    a.vmax[i] = a.vmax[i].max(b.vmax[i]);
                }
                for k in 0..2 {
                    a.p_vert_ok[k].merge(b.p_vert_ok[k]);
                    a.p_edge_ok[k].merge(b.p_edge_ok[k]);
                    a.p_edge_unset[k].merge(b.p_edge_unset[k]);
                    a.p_island_zero[k].merge(b.p_island_zero[k]);
                    a.p_pad_zero[k].merge(b.p_pad_zero[k]);
                    a.p_handle_zero[k].merge(b.p_handle_zero[k]);
                    a.p_degenerate[k].merge(b.p_degenerate[k]);
                    a.p_upright[k].merge(b.p_upright[k]);
                }
                for (k, m) in b.p_flags.into_iter().enumerate() {
                    merge_map(&mut a.p_flags[k], m);
                }
                a.p_area.extend(b.p_area.into_iter().take(200_000));
                a.e_type_matches_rank.merge(b.e_type_matches_rank);
                a.e_owner_in_range.merge(b.e_owner_in_range);
                a.e_owner_lists_edge.merge(b.e_owner_lists_edge);
                a.e_verts_are_poly_verts.merge(b.e_verts_are_poly_verts);
                a.e_verts_consecutive.merge(b.e_verts_consecutive);
                a.e_internal_target_ok.merge(b.e_internal_target_ok);
                a.e_border_target_unset.merge(b.e_border_target_unset);
                a.e_external_target_adjacent.merge(b.e_external_target_adjacent);
                merge_map(&mut a.e_target_group_dirs, b.e_target_group_dirs);
                a.bs_covers_poly.merge(b.bs_covers_poly);
                a.bs_is_circumcircle.merge(b.bs_is_circumcircle);
                a.bs_slack.extend(b.bs_slack.into_iter().take(200_000));
                a.clue_in_column.merge(b.clue_in_column);
                a.clue_in_column_exact.merge(b.clue_in_column_exact);
                a.clue_w_zero.merge(b.clue_w_zero);
                merge_map(&mut a.clue_uid_high, b.clue_uid_high);
                a.clue_dist.extend(b.clue_dist);
                merge_map(&mut a.clue_ability, b.clue_ability);
                merge_map(&mut a.clue_entity, b.clue_entity);
                a.clue_uids.extend(b.clue_uids);
                a
            },
        );

    println!("{} unit sections", st.zones);
    println!("\n-- vertices --");
    println!("  inside their own group's 16 m column  {}", st.vert_in_column);
    println!("  bounds min {:?}  max {:?}", st.vmin, st.vmax);

    for (k, label) in [(0usize, "connected"), (1, "disconnected")] {
        println!("\n-- polys, {label} --");
        println!("  vertex indices in range     {}", st.p_vert_ok[k]);
        println!("  edge indices in range       {}", st.p_edge_ok[k]);
        println!("  all three edges unset       {}", st.p_edge_unset[k]);
        println!("  island == 0                 {}", st.p_island_zero[k]);
        println!("  +0x0F padding zero          {}", st.p_pad_zero[k]);
        println!("  +0x10 handle zero           {}", st.p_handle_zero[k]);
        println!("  degenerate (zero area)      {}", st.p_degenerate[k]);
        println!("  winding gives +Y normal     {}", st.p_upright[k]);
        let mut f: Vec<_> = st.p_flags[k].iter().collect();
        f.sort_by_key(|x| std::cmp::Reverse(*x.1));
        println!("  +0x0E flag byte             {:?}", &f[..f.len().min(8)]);
    }
    if !st.p_area.is_empty() {
        let mut a = st.p_area.clone();
        a.sort_by(|x, y| x.partial_cmp(y).unwrap());
        println!(
            "\n  triangle area m^2: p5 {:.3}  median {:.3}  p95 {:.3}  max {:.3}",
            a[a.len() / 20],
            a[a.len() / 2],
            a[a.len() * 19 / 20],
            a[a.len() - 1]
        );
    }

    println!("\n-- edges --");
    println!("  +0x00 type == array rank         {}", st.e_type_matches_rank);
    println!("  +0x02 owner poly in range        {}", st.e_owner_in_range);
    println!("  owner poly lists this edge back  {}", st.e_owner_lists_edge);
    println!("  +0x04/+0x06 are owner's verts    {}", st.e_verts_are_poly_verts);
    println!("  ...and consecutive in winding    {}", st.e_verts_consecutive);
    println!("  internal: target in own group    {}", st.e_internal_target_ok);
    println!("  border:   no target at all       {}", st.e_border_target_unset);
    println!("  external: target is a 4-neighbour or self  {}", st.e_external_target_adjacent);
    let mut dirs: Vec<_> = st.e_target_group_dirs.iter().collect();
    dirs.sort_by_key(|x| std::cmp::Reverse(*x.1));
    println!("  external target deltas, in tiles {:?}", &dirs[..dirs.len().min(10)]);

    println!("\n-- bounding spheres (connected polys only) --");
    println!("  contains all three verts         {}", st.bs_covers_poly);
    println!("  radius == farthest vert, +-1 mm  {}", st.bs_is_circumcircle);
    if !st.bs_slack.is_empty() {
        let mut s = st.bs_slack.clone();
        s.sort_by(|x, y| x.partial_cmp(y).unwrap());
        println!(
            "  radius - farthest vert: min {:.5} median {:.5} max {:.5}",
            s[0],
            s[s.len() / 2],
            s[s.len() - 1]
        );
    }

    println!("\n-- clues (custom edges) --");
    println!("  position in its own 16 m column  {}", st.clue_in_column_exact);
    println!("  ...within one tile of it         {}", st.clue_in_column);
    println!("  +0x14 required entity zero       {}", st.clue_w_zero);
    println!("  distinct actor uids              {}", st.clue_uids.len());
    let mut h: Vec<_> = st.clue_uid_high.iter().collect();
    h.sort_by_key(|x| std::cmp::Reverse(*x.1));
    println!("  actor uid high dword             {:?}", &h[..h.len().min(6)]);
    if !st.clue_dist.is_empty() {
        let mut d = st.clue_dist.clone();
        d.sort_by(|x, y| x.partial_cmp(y).unwrap());
        println!(
            "  +0x18 distance: min {:.3} median {:.3} max {:.3}",
            d[0],
            d[d.len() / 2],
            d[d.len() - 1]
        );
    }
    let fmax = st.clue_dist.iter().filter(|d| **d > 3.0e38).count();
    println!(
        "  +0x18 == FLT_MAX                 {}/{}",
        fmax,
        st.clue_dist.len()
    );
    let mut ab: Vec<_> = st.clue_ability.iter().collect();
    ab.sort_by_key(|x| std::cmp::Reverse(*x.1));
    println!("  +0x10 required-ability mask     {:?}", &ab[..ab.len().min(8)]);
    println!("  distinct required entities       {}", st.clue_entity.len());
    let mut en: Vec<_> = st.clue_entity.iter().collect();
    en.sort_by_key(|x| std::cmp::Reverse(*x.1));
    println!("  +0x14 required entity          {:?}", &en[..en.len().min(8)]);

    // Do the clue uids name real actors? Every actor instance id in the whole
    // level index is the population to look them up in.
    let mut actor_uids: BTreeSet<u64> = BTreeSet::new();
    for z in idx.zones.iter().filter(|z| !z.path.contains("navdata")) {
        let Ok(bytes) = idx.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&bytes) else { continue };
        for o in &zone.scene_objects {
            if o.instance_id != 0 {
                actor_uids.insert(o.instance_id);
            }
        }
    }
    let hits = st.clue_uids.iter().filter(|u| actor_uids.contains(u)).count();
    println!(
        "  clue actor uids that name a scene-object instance id: {}/{} ({:.1}%) out of {} ids",
        hits,
        st.clue_uids.len(),
        100.0 * hits as f64 / st.clue_uids.len().max(1) as f64,
        actor_uids.len()
    );
}

// ---------------------------------------------------------------------------
// The geometric test
// ---------------------------------------------------------------------------

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    let d = sub(a, b);
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

/// One art triangle in world space.
type Tri = [[f32; 3]; 3];

/// Every Y at which the art surface passes under `(x, z)`, in no order.
///
/// Unlike a "nearest within a window" test this separates the two questions the
/// cross-check has to keep apart: *is there art under this vertex at all* and
/// *how far is the nav from it*. The caller picks the nearest Y itself.
fn surface_ys(tris: &[Tri], x: f32, z: f32, out: &mut Vec<f32>) {
    out.clear();
    for t in tris {
        let (x1, z1) = (t[0][0], t[0][2]);
        let (x2, z2) = (t[1][0], t[1][2]);
        let (x3, z3) = (t[2][0], t[2][2]);
        let den = (z2 - z3) * (x1 - x3) + (x3 - x2) * (z1 - z3);
        if den.abs() < 1e-12 {
            continue;
        }
        let l1 = ((z2 - z3) * (x - x3) + (x3 - x2) * (z - z3)) / den;
        let l2 = ((z3 - z1) * (x - x3) + (x1 - x3) * (z - z3)) / den;
        let l3 = 1.0 - l1 - l2;
        let eps = -1e-4;
        if l1 < eps || l2 < eps || l3 < eps {
            continue;
        }
        out.push(l1 * t[0][1] + l2 * t[1][1] + l3 * t[2][1]);
    }
}

/// The surface Y nearest `probe_y` in the 3x3 cell block around `(x, z)`.
fn nearest_surface(
    grid: &HashMap<(i32, i32), Vec<Tri>>,
    x: f32,
    z: f32,
    probe_y: f32,
    scratch: &mut Vec<f32>,
) -> Option<f32> {
    let cx = (x / 16.0).floor() as i32;
    let cz = (z / 16.0).floor() as i32;
    let mut best: Option<f32> = None;
    for dx in -1..=1 {
        for dz in -1..=1 {
            let Some(list) = grid.get(&(cx + dx, cz + dz)) else { continue };
            surface_ys(list, x, z, scratch);
            for &y in scratch.iter() {
                if best.is_none_or(|b: f32| (y - probe_y).abs() < (b - probe_y).abs()) {
                    best = Some(y);
                }
            }
        }
    }
    best
}

/// Art triangles of one zone, in the zone's own (region) space.
fn art_tris(idx: &level::LevelIndex, z: &level::ZoneRec, budget: usize) -> Vec<Tri> {
    let mut out: Vec<Tri> = Vec::new();
    let Ok(bytes) = idx.extract(z.id) else { return out };
    let Ok(zone) = Zone::parse(&bytes) else { return out };
    let mut cache: HashMap<u64, std::sync::Arc<Vec<u8>>> = HashMap::new();
    for o in &zone.scene_objects {
        if o.kind != zone::KIND_MODEL || o.model_id == 0 {
            continue;
        }
        if out.len() > budget {
            break;
        }
        let packed = cache
            .entry(o.model_id)
            .or_insert_with(|| level::model_mesh_bytes(idx, o.model_id, 0))
            .clone();
        let Some((pos, idxs)) = unpack_rcms(&packed) else { continue };
        let m = &o.matrix;
        for tri in idxs.chunks_exact(3) {
            let mut t: Tri = [[0.0; 3]; 3];
            for (k, &vi) in tri.iter().enumerate() {
                let v = &pos[vi as usize * 3..vi as usize * 3 + 3];
                for c in 0..3 {
                    t[k][c] = v[0] * m[c] + v[1] * m[4 + c] + v[2] * m[8 + c] + m[12 + c];
                }
            }
            out.push(t);
        }
    }
    out
}

fn unpack_rcms(b: &[u8]) -> Option<(Vec<f32>, Vec<u32>)> {
    if b.len() < 48 || LE::read_u32(&b[0..]) != level::MESH_MAGIC {
        return None;
    }
    let vc = LE::read_u32(&b[8..]) as usize;
    let ic = LE::read_u32(&b[12..]) as usize;
    let pos_at = 48;
    let idx_at = pos_at + vc * 12 * 2 + vc * 8;
    if idx_at + ic * 4 > b.len() {
        return None;
    }
    let pos = b[pos_at..pos_at + vc * 12]
        .chunks_exact(4)
        .map(LE::read_f32)
        .collect();
    let idxs = b[idx_at..idx_at + ic * 4]
        .chunks_exact(4)
        .map(LE::read_u32)
        .collect();
    Some((pos, idxs))
}

/// A tile directory name split into its two lattice indices, so the eight
/// neighbours of a tile can be named. `tile_f22` is column `f`, row `22`;
/// a nav vertex exactly on a tile edge belongs to the neighbour's floor.
fn tile_key(t: &str) -> Option<(i64, i64)> {
    let s = t.strip_prefix("tile_")?;
    let cut = s.find(|c: char| c.is_ascii_digit())?;
    if cut == 0 {
        return None;
    }
    let col = s[..cut]
        .bytes()
        .fold(0i64, |a, b| a * 26 + (b.to_ascii_lowercase() - b'a') as i64 + 1);
    Some((col, s[cut..].parse::<i64>().ok()?))
}

/// Accumulate the triangles of one art zone into `grid`, keeping only those
/// whose XZ footprint touches a cell in `want`. Returns (kept, seen).
fn art_into_grid(
    idx: &level::LevelIndex,
    z: &level::ZoneRec,
    want: &BTreeSet<(i32, i32)>,
    grid: &mut HashMap<(i32, i32), Vec<Tri>>,
) -> (usize, usize) {
    let (mut kept, mut seen) = (0usize, 0usize);
    for t in art_tris(idx, z, usize::MAX) {
        seen += 1;
        let (mut xl, mut xh, mut zl, mut zh) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for c in t.iter() {
            xl = xl.min(c[0]);
            xh = xh.max(c[0]);
            zl = zl.min(c[2]);
            zh = zh.max(c[2]);
        }
        if !xl.is_finite() || !xh.is_finite() || !zl.is_finite() || !zh.is_finite() {
            continue;
        }
        // Every cell the triangle's XZ box touches, so a triangle wider than a
        // cell is still found from the middle of it.
        let (i0, i1) = ((xl / 16.0).floor() as i32, (xh / 16.0).floor() as i32);
        let (j0, j1) = ((zl / 16.0).floor() as i32, (zh / 16.0).floor() as i32);
        if (i1 as i64 - i0 as i64) * (j1 as i64 - j0 as i64) > 4096 {
            continue; // a degenerate or world-sized triangle
        }
        let mut cells: BTreeSet<(i32, i32)> = BTreeSet::new();
        for i in i0..=i1 {
            for j in j0..=j1 {
                if want.contains(&(i, j)) {
                    cells.insert((i, j));
                }
            }
        }
        if cells.is_empty() {
            continue;
        }
        kept += 1;
        for c in cells {
            grid.entry(c).or_default().push(t);
        }
    }
    (kept, seen)
}

/// A list of signed errors, printed as a distribution of absolute values.
#[derive(Default)]
struct Dist {
    v: Vec<f64>,
}
impl Dist {
    /// A cap, because the whole art test runs in parallel over zones and every
    /// worker keeps its own samples until the reduce. 400k is already far more
    /// than any percentile needs.
    const CAP: usize = 400_000;

    fn push(&mut self, x: f64) {
        if self.v.len() < Self::CAP {
            self.v.push(x);
        }
    }
    fn merge(&mut self, o: Dist) {
        for x in o.v {
            self.push(x);
        }
    }
    fn report(&mut self, label: &str) {
        if self.v.is_empty() {
            println!("  {label:<18} no samples");
            return;
        }
        let mut a: Vec<f64> = self.v.iter().map(|x| x.abs()).collect();
        a.sort_by(|x, y| x.partial_cmp(y).unwrap());
        let frac = |t: f64| a.iter().filter(|&&x| x <= t).count() as f64 / a.len() as f64 * 100.0;
        println!(
            "  {label:<18} n {:>9}  median {:>9.3}  p90 {:>9.3}  p99 {:>9.3}   <=0.1 m {:>5.1}%  <=0.5 m {:>5.1}%  <=2 m {:>5.1}%",
            a.len(),
            a[a.len() / 2],
            a[a.len() * 9 / 10],
            a[a.len() * 99 / 100],
            frac(0.1),
            frac(0.5),
            frac(2.0),
        );
    }
}

/// Does the nav mesh sit on the art?
///
/// Only openarea tile units are tested: a tile's nav unit and the art of the
/// same `tile_<col><row>` directory are authored in one region space, so the
/// pairing is exact, and the eight neighbouring tiles' art is loaded too
/// because a nav vertex on a tile edge belongs to the neighbour's floor.
///
/// Three numbers come out, and the test only means something if all three do:
///
/// * **coverage** - the fraction of nav vertices with any art triangle over or
///   under their XZ. This measures how complete the drawn art is, not the
///   decode: a nav vertex over a hole in the art can never match.
/// * **|dy|, real** - for the covered vertices, the distance to the nearest art
///   surface directly above or below. This is the decode.
/// * **|dy|, displaced control** - the same vertices against the same art, but
///   every vertex moved a fixed 3 cells (48 m) in X and Z first. Art density is
///   unchanged and the correspondence is destroyed, so a decode that really is
///   on the floor separates from the control by orders of magnitude. A test in
///   which the control scores as well as the real case proves nothing.
fn art(idx: &level::LevelIndex, navs: &[&level::ZoneRec], limit: usize) {
    /// The control displacement, in metres. Three nav cells: far enough that no
    /// vertex can land on its own triangle, near enough to stay inside the art
    /// that was loaded.
    const SHIFT: f32 = 48.0;

    let mut art_by_tile: HashMap<(String, String), Vec<&level::ZoneRec>> = HashMap::new();
    for z in &idx.zones {
        if z.kind == "art" {
            if let Some(t) = &z.tile {
                art_by_tile
                    .entry((z.region.clone(), t.clone()))
                    .or_default()
                    .push(z);
            }
        }
    }
    // Tile directory name by (region, col, row), for the neighbour lookup.
    let mut tile_name: HashMap<(String, i64, i64), String> = HashMap::new();
    for (region, tile) in art_by_tile.keys() {
        if let Some((c, r)) = tile_key(tile) {
            tile_name.insert((region.clone(), c, r), tile.clone());
        }
    }

    let eligible: Vec<&&level::ZoneRec> = navs
        .iter()
        .filter(|z| {
            z.tile
                .as_ref()
                .is_some_and(|t| art_by_tile.contains_key(&(z.region.clone(), t.clone())))
        })
        .collect();
    // Sample evenly rather than taking a prefix: the zone list is sorted by
    // path, so a prefix would be one planet.
    let step = if limit == 0 || limit >= eligible.len() {
        1
    } else {
        eligible.len() / limit
    };
    let picked: Vec<&&level::ZoneRec> = eligible
        .iter()
        .step_by(step.max(1))
        .take(limit)
        .copied()
        .collect();
    println!(
        "openarea tile nav zones with art in the same tile: {} (testing every {}th, {})",
        eligible.len(),
        step,
        picked.len()
    );

    #[derive(Default)]
    struct R {
        zones: u64,
        art_kept: u64,
        art_seen: u64,
        verts: u64,
        covered: u64,
        ctl_covered: u64,
        cents: u64,
        cent_covered: u64,
        cent_ctl_covered: u64,
        tails: u64,
        tail_covered: u64,
        tail_ctl_covered: u64,
        yshuf_covered: u64,
        real: Dist,
        ctl: Dist,
        cent: Dist,
        cent_ctl: Dist,
        tail: Dist,
        tail_ctl: Dist,
        yshuf: Dist,
        signed: Dist,
        worst: Vec<(f64, u64, String)>,
    }

    let r = picked
        .par_iter()
        .fold(R::default, |mut r, z| {
            let tile = z.tile.clone().unwrap_or_default();
            let Some((col, row)) = tile_key(&tile) else { return r };
            let Ok(bytes) = idx.extract(z.id) else { return r };
            let Ok(zone) = Zone::parse(&bytes) else { return r };
            let Some(d) = zone.dat1.get_section_data(nav::NAV_UNIT_TAG) else { return r };
            let Ok(u) = NavUnit::parse(d) else { return r };

            // Two probe sets. A vertex sits on the rim of the walkable area,
            // where Detour's contour simplification puts it on a voxel edge that
            // can overhang a cliff by a cell; a connected polygon's centroid is
            // well inside the floor and is the honest test of "is the mesh on
            // the art".
            let mut verts: Vec<[f32; 3]> = Vec::new();
            let mut cents: Vec<[f32; 3]> = Vec::new();
            let mut tails: Vec<[f32; 3]> = Vec::new();
            for g in &u.groups {
                if g.is_empty() {
                    continue;
                }
                let gv = u.verts(g);
                for (pi, poly) in u.polys(g).iter().enumerate() {
                    if poly.verts.iter().any(|&i| i as usize >= gv.len()) {
                        continue;
                    }
                    let mut c = [0.0f32; 3];
                    for &i in &poly.verts {
                        for k in 0..3 {
                            c[k] += gv[i as usize][k] / 3.0;
                        }
                    }
                    if pi < g.poly_count {
                        cents.push(c);
                    } else {
                        tails.push(c);
                    }
                }
                verts.extend(gv);
            }
            if verts.is_empty() {
                return r;
            }

            // The cells the test will read: around each vertex and around each
            // displaced vertex, one ring wider so the 3x3 search never misses.
            let mut want: BTreeSet<(i32, i32)> = BTreeSet::new();
            for v in verts.iter().chain(cents.iter()).chain(tails.iter()) {
                for (x, zz) in [(v[0], v[2]), (v[0] + SHIFT, v[2] + SHIFT)] {
                    let (cx, cz) = ((x / 16.0).floor() as i32, (zz / 16.0).floor() as i32);
                    for dx in -1..=1 {
                        for dz in -1..=1 {
                            want.insert((cx + dx, cz + dz));
                        }
                    }
                }
            }

            let mut grid: HashMap<(i32, i32), Vec<Tri>> = HashMap::new();
            for dc in -1..=1i64 {
                for dr in -1..=1i64 {
                    let Some(name) = tile_name.get(&(z.region.clone(), col + dc, row + dr)) else {
                        continue;
                    };
                    let Some(zones) = art_by_tile.get(&(z.region.clone(), name.clone())) else {
                        continue;
                    };
                    for a in zones {
                        let (k, s) = art_into_grid(idx, a, &want, &mut grid);
                        r.art_kept += k as u64;
                        r.art_seen += s as u64;
                    }
                }
            }
            if grid.is_empty() {
                return r;
            }
            r.zones += 1;

            let mut scratch: Vec<f32> = Vec::new();
            let mut worst = 0.0f64;
            let mut zone_miss = 0u64;
            for v in &verts {
                r.verts += 1;
                if let Some(y) = nearest_surface(&grid, v[0], v[2], v[1], &mut scratch) {
                    r.covered += 1;
                    r.real.push((y - v[1]) as f64);
                }
                if let Some(y) =
                    nearest_surface(&grid, v[0] + SHIFT, v[2] + SHIFT, v[1], &mut scratch)
                {
                    r.ctl_covered += 1;
                    r.ctl.push((y - v[1]) as f64);
                }
            }
            for c in &cents {
                r.cents += 1;
                if let Some(y) = nearest_surface(&grid, c[0], c[2], c[1], &mut scratch) {
                    r.cent_covered += 1;
                    let dy = (y - c[1]) as f64;
                    r.cent.push(dy);
                    r.signed.push(dy);
                    if dy.abs() > 0.5 {
                        zone_miss += 1;
                    }
                    worst = worst.max(dy.abs());
                }
                if let Some(y) =
                    nearest_surface(&grid, c[0] + SHIFT, c[2] + SHIFT, c[1], &mut scratch)
                {
                    r.cent_ctl_covered += 1;
                    r.cent_ctl.push((y - c[1]) as f64);
                }
            }
            // Second control: the real XZ, but the height of a different
            // centroid of the same unit. It asks whether the Y component is
            // the height of this point or just a height from this level.
            for (i, c) in cents.iter().enumerate() {
                let fake_y = cents[(i * 7919 + 104_729) % cents.len()][1];
                if let Some(y) = nearest_surface(&grid, c[0], c[2], fake_y, &mut scratch) {
                    r.yshuf_covered += 1;
                    r.yshuf.push((y - fake_y) as f64);
                }
            }
            for c in &tails {
                r.tails += 1;
                if let Some(y) = nearest_surface(&grid, c[0], c[2], c[1], &mut scratch) {
                    r.tail_covered += 1;
                    r.tail.push((y - c[1]) as f64);
                }
                if let Some(y) =
                    nearest_surface(&grid, c[0] + SHIFT, c[2] + SHIFT, c[1], &mut scratch)
                {
                    r.tail_ctl_covered += 1;
                    r.tail_ctl.push((y - c[1]) as f64);
                }
            }
            r.worst.push((worst, zone_miss, z.path.clone()));
            r
        })
        .reduce(R::default, |mut a, b| {
            a.zones += b.zones;
            a.art_kept += b.art_kept;
            a.art_seen += b.art_seen;
            a.verts += b.verts;
            a.covered += b.covered;
            a.ctl_covered += b.ctl_covered;
            a.cents += b.cents;
            a.cent_covered += b.cent_covered;
            a.cent_ctl_covered += b.cent_ctl_covered;
            a.tails += b.tails;
            a.tail_covered += b.tail_covered;
            a.tail_ctl_covered += b.tail_ctl_covered;
            a.yshuf_covered += b.yshuf_covered;
            a.real.merge(b.real);
            a.ctl.merge(b.ctl);
            a.cent.merge(b.cent);
            a.cent_ctl.merge(b.cent_ctl);
            a.tail.merge(b.tail);
            a.tail_ctl.merge(b.tail_ctl);
            a.yshuf.merge(b.yshuf);
            a.signed.merge(b.signed);
            a.worst.extend(b.worst);
            a
        });

    let mut r = r;
    println!(
        "\n{} of {} picked tile units produced a test (the rest have no vertex, or no art in the 3x3 tile block)",
        r.zones,
        picked.len()
    );
    println!(
        "{} tile units tested, {} nav vertices, {} art triangles kept of {} loaded",
        r.zones, r.verts, r.art_kept, r.art_seen
    );
    println!(
        "coverage: art lies over or under the vertex XZ on {}/{} ({:.1}%); displaced {}/{} ({:.1}%)",
        r.covered,
        r.verts,
        100.0 * r.covered as f64 / r.verts.max(1) as f64,
        r.ctl_covered,
        r.verts,
        100.0 * r.ctl_covered as f64 / r.verts.max(1) as f64
    );
    println!(
        "         polygon centroids: {}/{} ({:.1}%); displaced {}/{} ({:.1}%)",
        r.cent_covered,
        r.cents,
        100.0 * r.cent_covered as f64 / r.cents.max(1) as f64,
        r.cent_ctl_covered,
        r.cents,
        100.0 * r.cent_ctl_covered as f64 / r.cents.max(1) as f64
    );
    println!("|dy| to the nearest art surface above or below, in metres:");
    r.real.report("vertices");
    r.ctl.report("  displaced 48 m");
    r.cent.report("centroids");
    r.cent_ctl.report("  displaced 48 m");
    r.yshuf.report("  Y from another");
    println!(
        "         tail-class centroids: {}/{} ({:.1}%); displaced {}/{} ({:.1}%)",
        r.tail_covered,
        r.tails,
        100.0 * r.tail_covered as f64 / r.tails.max(1) as f64,
        r.tail_ctl_covered,
        r.tails,
        100.0 * r.tail_ctl_covered as f64 / r.tails.max(1) as f64
    );
    r.tail.report("tail centroids");
    r.tail_ctl.report("  displaced 48 m");
    if !r.signed.v.is_empty() {
        let near: Vec<f64> = r.signed.v.iter().copied().filter(|d| d.abs() <= 2.0).collect();
        if !near.is_empty() {
            println!(
                "  mean signed art Y minus nav Y over the {} centroids inside 2 m: {:+.4} m",
                near.len(),
                near.iter().sum::<f64>() / near.len() as f64
            );
        }
    }
    r.worst.sort_by(|a, b| b.1.cmp(&a.1));
    println!("tiles with the most vertices further than 0.5 m from the art:");
    for (d, miss, p) in r.worst.iter().take(8) {
        println!("  {miss:>7} over 0.5 m, worst {d:>9.2} m  {p}");
    }
}

// ---------------------------------------------------------------------------

fn dump(idx: &level::LevelIndex, navs: &[&level::ZoneRec]) {
    for z in navs.iter().take(3) {
        let Ok(bytes) = idx.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&bytes) else { continue };
        println!("\n=== {} (id {:016X}) ===", z.path, z.id);
        for s in &zone.dat1.sections {
            println!("  section {:08X} size {:>10}  {}", s.tag, s.size, tag_name(s.tag));
        }
        if let Some(d) = zone.dat1.get_section_data(nav::NAV_VERSION_TAG) {
            println!("  3D8DBDB8: ");
            hex(d, 0, 4);
        }
        if let Some(d) = zone.dat1.get_section_data(nav::NAV_CONTAINER_TAG) {
            println!("  04C19E69 head:");
            hex(d, 0, 64);
            if let Ok(c) = nav::NavContainer::parse(d) {
                println!(
                    "  container: layers {} tiles ({},{})..({},{}) log2 {} slots {} implied {}",
                    c.layer_count,
                    c.min_tile[0],
                    c.min_tile[1],
                    c.max_tile[0],
                    c.max_tile[1],
                    c.log_scale,
                    c.slots,
                    c.implied_slots()
                );
            }
        }
        if let Some(d) = zone.dat1.get_section_data(nav::NAV_3D_TAG) {
            println!("  339AEC45 head:");
            hex(d, 0, 96);
            if let Ok(n) = nav::Nav3D::parse(d) {
                println!("  nav3d: {n:?}");
            }
        }
        if let Some(d) = zone.dat1.get_section_data(nav::NAV_UNIT_TAG) {
            let Ok(u) = NavUnit::parse(d) else { continue };
            println!(
                "  unit: asset {:016X} container {:016X} tiles ({},{})..({},{}) log2 {} layers {} groups {}",
                u.asset_id,
                u.container_id,
                u.min_tile[0],
                u.min_tile[1],
                u.max_tile[0],
                u.max_tile[1],
                u.log_scale,
                u.layers,
                u.groups.len()
            );
            let t = u.totals();
            println!("  {t:?}");
            let problems = u.validate();
            println!("  validate: {} problems {:?}", problems.len(), &problems[..problems.len().min(3)]);
            for g in u.groups.iter().filter(|g| !g.is_empty()).take(2) {
                println!("\n  group {} layer {} at ({}, {})", g.index, g.layer, g.x, g.z);
                println!(
                    "    verts {} polys {} +{} edges {:?} clues {}",
                    g.vert_count,
                    g.poly_count,
                    g.disconnected_poly_count,
                    g.edge_counts,
                    g.edge_counts[2]
                );
                let verts = u.verts(g);
                for (i, v) in verts.iter().take(4).enumerate() {
                    println!("    v{i}  {:9.3} {:9.3} {:9.3}", v[0], v[1], v[2]);
                }
                for (i, p) in u.polys(g).iter().take(4).enumerate() {
                    println!("    p{i}  {p:?}");
                }
                for (i, e) in u.edges(g).iter().take(6).enumerate() {
                    println!("    e{i}  {e:?}");
                }
                for (i, c) in u.clues(g).iter().take(3).enumerate() {
                    println!("    c{i}  {c:?}");
                }
                for (i, s) in u.bspheres(g).iter().take(3).enumerate() {
                    println!("    s{i}  {s:?}");
                }
            }
        }
        if let Some(d) = zone.dat1.get_section_data(nav::NAV_TILE_CACHE_TAG) {
            println!("\n  A903D8F1 head:");
            hex(d, 0, 64);
            match nav::tile_cache_entries(d) {
                Ok((t, layers)) => {
                    println!("  {} tiles, {layers} layer terminators", t.len());
                    for e in t.iter().take(3) {
                        println!("    {e:?}");
                    }
                }
                Err(e) => println!("  framing failed: {e}"),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Pairing diagnostic
// ---------------------------------------------------------------------------

/// For each openarea tile that has a nav unit, print what art there is to test
/// against and how the two footprints line up. Answers "is the cross-check
/// short of art, or short of agreement?" before the cross-check is trusted.
fn pair(idx: &level::LevelIndex, navs: &[&level::ZoneRec], limit: usize) {
    let mut by_tile: HashMap<(String, Option<String>), Vec<&level::ZoneRec>> = HashMap::new();
    for z in &idx.zones {
        by_tile
            .entry((z.region.clone(), z.tile.clone()))
            .or_default()
            .push(z);
    }
    for z in navs.iter().filter(|z| z.tile.is_some()).take(limit) {
        let Ok(bytes) = idx.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&bytes) else { continue };
        let Some(d) = zone.dat1.get_section_data(nav::NAV_UNIT_TAG) else { continue };
        let Ok(u) = NavUnit::parse(d) else { continue };
        let mut nav_cells: BTreeSet<(i32, i32)> = BTreeSet::new();
        let mut nv = 0usize;
        let (mut nxl, mut nxh, mut nzl, mut nzh) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for g in &u.groups {
            if g.is_empty() {
                continue;
            }
            for v in u.verts(g) {
                nv += 1;
                nav_cells.insert(((v[0] / 16.0).floor() as i32, (v[2] / 16.0).floor() as i32));
                nxl = nxl.min(v[0]);
                nxh = nxh.max(v[0]);
                nzl = nzl.min(v[2]);
                nzh = nzh.max(v[2]);
            }
        }
        println!(
            "\n=== {} ===\n  unit tiles x {}..{} z {}..{} layers {} groups {}",
            z.path,
            u.min_tile[0],
            u.max_tile[0],
            u.min_tile[1],
            u.max_tile[1],
            u.layers,
            u.groups.iter().filter(|g| !g.is_empty()).count()
        );
        let t = u.totals();
        println!(
            "  nav verts {nv} in {} cells, x {nxl:.1}..{nxh:.1} z {nzl:.1}..{nzh:.1}",
            nav_cells.len()
        );
        println!(
            "  totals: verts {} polys {} disconnected {} edges {:?}",
            t.verts, t.polys, t.disconnected, t.edges
        );
        let mut art_cells: BTreeSet<(i32, i32)> = BTreeSet::new();
        let empty = Vec::new();
        let sibs = by_tile
            .get(&(z.region.clone(), z.tile.clone()))
            .unwrap_or(&empty);
        for s in sibs {
            if s.id == z.id {
                continue;
            }
            let tris = art_tris(idx, s, 4_000_000);
            let mut objs = 0usize;
            let mut models = 0usize;
            if let Ok(b) = idx.extract(s.id) {
                if let Ok(sz) = Zone::parse(&b) {
                    objs = sz.scene_objects.len();
                    models = sz
                        .scene_objects
                        .iter()
                        .filter(|o| o.kind == zone::KIND_MODEL && o.model_id != 0)
                        .count();
                }
            }
            let mut cells: BTreeSet<(i32, i32)> = BTreeSet::new();
            for t in &tris {
                for c in t.iter() {
                    cells.insert(((c[0] / 16.0).floor() as i32, (c[2] / 16.0).floor() as i32));
                }
            }
            println!(
                "  sib {:<6} objs {:>5} models {:>5} tris {:>8} cells {:>5}  {}",
                s.kind,
                objs,
                models,
                tris.len(),
                cells.len(),
                s.path.rsplit('/').next().unwrap_or("")
            );
            if s.kind == "art" {
                art_cells.extend(cells);
            }
        }
        let shared = nav_cells.intersection(&art_cells).count();
        println!(
            "  art cells {} ; nav cells covered by art {}/{} ({:.1}%)",
            art_cells.len(),
            shared,
            nav_cells.len(),
            100.0 * shared as f64 / nav_cells.len().max(1) as f64
        );
    }
}

// ---------------------------------------------------------------------------
// What the second polygon class is
// ---------------------------------------------------------------------------

/// Is `(x, z)` inside triangle `t` projected to XZ?
fn inside_xz(t: &Tri, x: f32, z: f32) -> bool {
    let (x1, z1) = (t[0][0], t[0][2]);
    let (x2, z2) = (t[1][0], t[1][2]);
    let (x3, z3) = (t[2][0], t[2][2]);
    let den = (z2 - z3) * (x1 - x3) + (x3 - x2) * (z1 - z3);
    if den.abs() < 1e-12 {
        return false;
    }
    let l1 = ((z2 - z3) * (x - x3) + (x3 - x2) * (z - z3)) / den;
    let l2 = ((z3 - z1) * (x - x3) + (x1 - x3) * (z - z3)) / den;
    let l3 = 1.0 - l1 - l2;
    l1 >= -1e-4 && l2 >= -1e-4 && l3 >= -1e-4
}

/// The polygon array of a group is `poly_count + disconnected_poly_count`
/// 24-byte records. Only the first `poly_count` carry an island, a flag byte, in
/// range edge indices and a bounding sphere. This asks what the rest are, by
/// measuring them against the first class inside the same group:
///
/// * do their centroids fall inside a first-class triangle in XZ (a height
///   detail subdivision would),
/// * do the two classes cover the same XZ area,
/// * are their vertex indices drawn from the same part of the vertex array.
fn tails(idx: &level::LevelIndex, navs: &[&level::ZoneRec], limit: usize) {
    #[derive(Default)]
    struct A {
        units: u64,
        head: u64,
        tail: u64,
        inside: Tally,
        head_area: f64,
        tail_area: f64,
        head_vmax: u64,
        tail_vmin: u64,
        vsplit: Tally,
        dy_inside: Vec<f64>,
        head_y: Vec<f64>,
        tail_y: Vec<f64>,
    }
    let a = navs
        .iter()
        .take(limit)
        .par_bridge()
        .fold(A::default, |mut a, z| {
            let Ok(bytes) = idx.extract(z.id) else { return a };
            let Ok(zone) = Zone::parse(&bytes) else { return a };
            let Some(d) = zone.dat1.get_section_data(nav::NAV_UNIT_TAG) else { return a };
            let Ok(u) = NavUnit::parse(d) else { return a };
            a.units += 1;
            for g in &u.groups {
                if g.is_empty() || g.disconnected_poly_count == 0 {
                    continue;
                }
                let gv = u.verts(g);
                let polys = u.polys(g);
                if polys.len() != g.total_poly_count() {
                    continue;
                }
                let tri = |p: &nav::NavPoly| -> Option<Tri> {
                    if p.verts.iter().any(|&i| i as usize >= gv.len()) {
                        return None;
                    }
                    Some([
                        gv[p.verts[0] as usize],
                        gv[p.verts[1] as usize],
                        gv[p.verts[2] as usize],
                    ])
                };
                let head: Vec<Tri> = polys[..g.poly_count].iter().filter_map(tri).collect();
                a.head += head.len() as u64;
                for t in &head {
                    let n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
                    a.head_area += 0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt() as f64;
                    for c in t.iter() {
                        if a.head_y.len() < 200_000 {
                            a.head_y.push(c[1] as f64);
                        }
                    }
                }
                // which slice of the vertex array each class indexes
                let hv = polys[..g.poly_count]
                    .iter()
                    .flat_map(|p| p.verts)
                    .max()
                    .unwrap_or(0);
                let tv = polys[g.poly_count..]
                    .iter()
                    .flat_map(|p| p.verts)
                    .min()
                    .unwrap_or(0);
                a.head_vmax = a.head_vmax.max(hv as u64);
                a.tail_vmin = a.tail_vmin.max(tv as u64);
                a.vsplit.add(g.poly_count == 0 || tv > hv);
                for p in &polys[g.poly_count..] {
                    a.tail += 1;
                    let Some(t) = tri(p) else { continue };
                    let n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
                    a.tail_area += 0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt() as f64;
                    let c = [
                        (t[0][0] + t[1][0] + t[2][0]) / 3.0,
                        (t[0][1] + t[1][1] + t[2][1]) / 3.0,
                        (t[0][2] + t[1][2] + t[2][2]) / 3.0,
                    ];
                    for v in t.iter() {
                        if a.tail_y.len() < 200_000 {
                            a.tail_y.push(v[1] as f64);
                        }
                    }
                    let mut best: Option<f32> = None;
                    for h in &head {
                        if inside_xz(h, c[0], c[2]) {
                            let (x1, z1) = (h[0][0], h[0][2]);
                            let (x2, z2) = (h[1][0], h[1][2]);
                            let (x3, z3) = (h[2][0], h[2][2]);
                            let den = (z2 - z3) * (x1 - x3) + (x3 - x2) * (z1 - z3);
                            let l1 = ((z2 - z3) * (c[0] - x3) + (x3 - x2) * (c[2] - z3)) / den;
                            let l2 = ((z3 - z1) * (c[0] - x3) + (x1 - x3) * (c[2] - z3)) / den;
                            let y = l1 * h[0][1] + l2 * h[1][1] + (1.0 - l1 - l2) * h[2][1];
                            if best.is_none_or(|b: f32| (y - c[1]).abs() < (b - c[1]).abs()) {
                                best = Some(y);
                            }
                        }
                    }
                    a.inside.add(best.is_some());
                    if let Some(y) = best {
                        if a.dy_inside.len() < 400_000 {
                            a.dy_inside.push((y - c[1]) as f64);
                        }
                    }
                }
            }
            a
        })
        .reduce(A::default, |mut x, y| {
            x.units += y.units;
            x.head += y.head;
            x.tail += y.tail;
            x.inside.merge(y.inside);
            x.head_area += y.head_area;
            x.tail_area += y.tail_area;
            x.head_vmax = x.head_vmax.max(y.head_vmax);
            x.tail_vmin = x.tail_vmin.max(y.tail_vmin);
            x.vsplit.merge(y.vsplit);
            x.dy_inside.extend(y.dy_inside.into_iter().take(400_000));
            x.head_y.extend(y.head_y.into_iter().take(200_000));
            x.tail_y.extend(y.tail_y.into_iter().take(200_000));
            x
        });

    println!("{} units, class A (first poly_count) {} tris, class B (the rest) {} tris", a.units, a.head, a.tail);
    println!(
        "  class B centroid lies inside a class A triangle in XZ: {}",
        a.inside
    );
    println!(
        "  XZ-projected area: class A {:.0} m^2, class B {:.0} m^2, ratio {:.3}",
        a.head_area,
        a.tail_area,
        a.tail_area / a.head_area.max(1.0)
    );
    println!(
        "  class B indexes only vertices above every class A index, per group: {}",
        a.vsplit
    );
    if !a.dy_inside.is_empty() {
        let mut v: Vec<f64> = a.dy_inside.iter().map(|x| x.abs()).collect();
        v.sort_by(|x, y| x.partial_cmp(y).unwrap());
        let frac = |t: f64| v.iter().filter(|&&x| x <= t).count() as f64 / v.len() as f64 * 100.0;
        println!(
            "  where it is inside: |dy| to the class A surface median {:.3} p90 {:.3} max {:.3}; <=0.05 m {:.1}%",
            v[v.len() / 2],
            v[v.len() * 9 / 10],
            v[v.len() - 1],
            frac(0.05)
        );
    }
}

// ---------------------------------------------------------------------------
// Per-unit census and the container
// ---------------------------------------------------------------------------

/// One line per unit section, then how many units carry no polygons at all.
fn units(idx: &level::LevelIndex, navs: &[&level::ZoneRec], limit: usize) {
    let mut rows: Vec<(String, usize, usize, usize, usize, usize, usize, usize)> = navs
        .par_iter()
        .filter_map(|z| {
            let bytes = idx.extract(z.id).ok()?;
            let zone = Zone::parse(&bytes).ok()?;
            let d = zone.dat1.get_section_data(nav::NAV_UNIT_TAG)?;
            let u = NavUnit::parse(d).ok()?;
            let t = u.totals();
            let (nx, nz) = u.grid();
            Some((
                z.path.clone(),
                u.layers,
                nx,
                nz,
                t.groups,
                t.verts,
                t.polys,
                t.disconnected,
            ))
        })
        .collect();
    rows.sort_by(|a, b| b.5.cmp(&a.5));
    println!(
        "{:<62} {:>2} {:>4}x{:<4} {:>6} {:>9} {:>8} {:>9}",
        "unit", "L", "nx", "nz", "groups", "verts", "polysA", "polysB"
    );
    for r in rows.iter().take(limit.min(40)) {
        println!(
            "{:<62} {:>2} {:>4}x{:<4} {:>6} {:>9} {:>8} {:>9}",
            r.0.trim_start_matches("levels/i29/"),
            r.1,
            r.2,
            r.3,
            r.4,
            r.5,
            r.6,
            r.7
        );
    }
    let no_a = rows.iter().filter(|r| r.6 == 0).count();
    let no_any = rows.iter().filter(|r| r.6 == 0 && r.7 == 0).count();
    let no_v = rows.iter().filter(|r| r.5 == 0).count();
    println!(
        "
{} units; {} with no class A polygon, {} with no polygon of either class, {} with no vertex",
        rows.len(),
        no_a,
        no_any,
        no_v
    );
    println!("units with vertices but no class A polygon:");
    for r in rows.iter().filter(|r| r.6 == 0 && r.5 > 0).take(20) {
        println!(
            "  verts {:>8} polysB {:>8} groups {:>5} L {} {}",
            r.5, r.7, r.4, r.1, r.0
        );
    }
}

/// Every container section, with the slot count its header implies and where the
/// region's mesh lives. The two shapes a container comes in are what an earlier
/// version of this module read as one.
fn container(idx: &level::LevelIndex, navs: &[&level::ZoneRec], limit: usize) {
    let mut shown = 0usize;
    let mut scale: BTreeMap<i16, usize> = BTreeMap::new();
    let mut fit: BTreeMap<&str, usize> = BTreeMap::new();
    let mut single_slots: BTreeMap<usize, usize> = BTreeMap::new();
    for z in navs {
        let Ok(bytes) = idx.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&bytes) else { continue };
        let Some(d) = zone.dat1.get_section_data(nav::NAV_CONTAINER_TAG) else { continue };
        let Ok(c) = nav::NavContainer::parse(d) else { continue };
        *scale.entry(c.log_scale).or_default() += 1;
        let words = (d.len() - 0x20) / 8;
        let key = if words == c.implied_slots() {
            "implied_slots"
        } else {
            "mismatch"
        };
        *fit.entry(key).or_default() += 1;
        if !c.is_tiled() {
            *single_slots.entry(words).or_default() += 1;
        }
        let has_unit = zone.dat1.get_section_data(nav::NAV_UNIT_TAG).is_some();
        if shown < limit {
            shown += 1;
            let (lo, hi) = c.bounds_metres();
            let (ux, uz) = c.unit_grid();
            println!(
                "  {:<66} {} layers {} units {}x{} x {}..{} z {}..{} m  slots {} implied {} mesh here {}",
                z.path.trim_start_matches("levels/i29/"),
                if c.is_tiled() { "tiled  " } else { "single " },
                c.layer_count,
                ux,
                uz,
                lo[0],
                hi[0],
                lo[1],
                hi[1],
                words,
                c.implied_slots(),
                has_unit
            );
        }
    }
    println!("\nlog2 unit size {scale:?}");
    println!("stored slot count == implied_slots {fit:?}");
    println!("single-unit containers by stored slot count {single_slots:?}");
}

/// Raw clue records with their group, so the 40-byte layout can be read off the
/// bytes instead of guessed.
fn clue(idx: &level::LevelIndex, navs: &[&level::ZoneRec], limit: usize) {
    let mut shown = 0usize;
    for z in navs {
        if shown >= limit {
            break;
        }
        let Ok(bytes) = idx.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&bytes) else { continue };
        let Some(d) = zone.dat1.get_section_data(nav::NAV_UNIT_TAG) else { continue };
        let Ok(u) = NavUnit::parse(d) else { continue };
        for g in &u.groups {
            if g.is_empty() || g.edge_counts[EdgeType::Custom as usize] == 0 {
                continue;
            }
            if shown >= limit {
                break;
            }
            shown += 1;
            let n = g.edge_counts[EdgeType::Custom as usize];
            println!(
                "\n{} group {} at ({}, {}) layer {} : {} clues",
                z.path.trim_start_matches("levels/i29/"),
                g.index,
                g.x,
                g.z,
                g.layer,
                n
            );
            let edges = u.edges(g);
            let cr = g.edge_range(EdgeType::Custom);
            for (k, ei) in cr.clone().enumerate().take(3) {
                let e = &edges[ei];
                println!(
                    "  custom edge {k}: type {} poly {} words {:04X} {:04X} {:04X} {:04X} {:04X}",
                    e.edge_type, e.poly, e.vert_a, e.vert_b, e.target_group_x_raw,
                    e.target_group_z_raw, e.target_poly
                );
            }
            let at = g.clue_offset;
            let take = (n.min(3)) * nav::CLUE_STRIDE;
            if at + take <= d.len() {
                hex(&d[at..at + take], at, take);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The command layer, driven directly
// ---------------------------------------------------------------------------

#[cfg(feature = "nav-commands")]
/// Poll an already-complete future once. Every `level_nav_*` command is an
/// `async fn` with a synchronous body -- it awaits nothing -- so a single poll
/// finishes it and no runtime is needed to drive it from a probe.
fn block_on<F: std::future::Future>(mut f: F) -> F::Output {
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
    unsafe fn noop(_: *const ()) {}
    unsafe fn clone(p: *const ()) -> RawWaker {
        RawWaker::new(p, &VTABLE)
    }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
    let mut cx = Context::from_waker(&waker);
    // Safety: the future is owned here and never moved after being pinned.
    let mut f = unsafe { std::pin::Pin::new_unchecked(&mut f) };
    loop {
        match f.as_mut().poll(&mut cx) {
            Poll::Ready(v) => return v,
            Poll::Pending => panic!("a level_nav_* command awaited something"),
        }
    }
}

#[cfg(feature = "nav-commands")]
fn raw_body(r: tauri::ipc::Response) -> Vec<u8> {
    use tauri::ipc::IpcResponse;
    match r.body().expect("body") {
        tauri::ipc::InvokeResponseBody::Raw(v) => v,
        tauri::ipc::InvokeResponseBody::Json(_) => panic!("expected a raw body"),
    }
}

#[cfg(feature = "nav-commands")]
fn rcms_counts(b: &[u8]) -> (usize, usize, usize) {
    (
        LE::read_u32(&b[8..]) as usize,
        LE::read_u32(&b[12..]) as usize,
        LE::read_u32(&b[16..]) as usize,
    )
}

#[cfg(feature = "nav-commands")]
/// Drive every `level_nav_*` command against the shipped archives and check the
/// buffers it hands the viewport against the counts the summary reports, so the
/// IPC layer is exercised before the front end ever calls it.
fn cmd(game: &str) {
    use omnitool_lib::commands_level_nav as c;
    let mut failures = 0usize;
    let mut check = |ok: bool, what: &str| {
        println!("  {} {what}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            failures += 1;
        }
    };

    println!("== level_nav_zones on i29/openarea/sargasso ==");
    let zones = block_on(c::level_nav_zones(
        game.to_string(),
        "i29/openarea/sargasso".into(),
        None,
    ))
    .expect("level_nav_zones");
    let units = zones.iter().filter(|z| z.has_unit).count();
    let containers = zones.iter().filter(|z| z.has_container).count();
    println!(
        "  {} nav zones: {units} with a unit, {containers} with the container",
        zones.len()
    );
    check(units > 100, "the region has many unit zones");
    check(containers == 1, "exactly one container zone");

    for path in [
        "levels/i29/openarea/sargasso/tile_f22/unitnavdata.zone",
        "levels/i29/instance/cordelion/containernavdata.zone",
        "levels/i29/instance/ardolis_passiveshift/containernavdata.zone",
    ] {
        println!("\n== {path} ==");
        let s = block_on(c::level_nav_summary(game.to_string(), path.into(), None))
            .expect("level_nav_summary");
        let u = s.unit.as_ref().expect("a unit section");
        println!(
            "  layers {} (from the container: {})  tiles {}x{}  clues {}",
            u.layers, u.layers_from_container, u.tiles_x, u.tiles_z, u.clues
        );
        for l in &u.per_layer {
            println!(
                "  layer {}: groups {} verts {} reachable {} unreachable {} edges {:?}",
                l.layer, l.groups, l.vertices, l.polygons, l.disconnected, l.edges
            );
        }
        println!("  bounds {:?} .. {:?}  tile-cache tiles {:?}", u.min, u.max, s.tile_cache_tiles);
        if let Some(k) = &s.container {
            println!(
                "  container: tiled {} layers {} slots {} implied {}",
                k.tiled, k.layer_count, k.slots, k.implied_slots
            );
        }
        check(s.problems.is_empty(), "validate() found nothing wrong");
        check(u.layers == 2, "two layers");
        check(u.log_tile_size == 4, "16 m tiles");

        let both = block_on(c::level_nav_surface(
            game.to_string(),
            path.into(),
            None,
            Some(true),
            None,
        ))
        .expect("level_nav_surface");
        let b = raw_body(both);
        let (vc, ic, sc) = rcms_counts(&b);
        let want: usize = u.per_layer.iter().map(|l| l.polygons + l.disconnected).sum();
        println!("  surface: {} bytes, {vc} verts, {ic} indices, {sc} submeshes (unit has {want} polygons)", b.len());
        check(ic == vc, "de-indexed: one index per vertex");
        check(ic / 3 <= want, "no more triangles than the unit has polygons");
        check(ic / 3 * 10 >= want * 9, "at least 90% of them survive the build");
        check(
            48 + vc * 32 + ic * 4 + sc * 16 == b.len(),
            "the buffer is exactly its header plus its arrays",
        );

        let only = block_on(c::level_nav_surface(
            game.to_string(),
            path.into(),
            None,
            Some(false),
            None,
        ))
        .expect("level_nav_surface");
        let ob = raw_body(only);
        let (_, oic, osc) = rcms_counts(&ob);
        let reachable: usize = u.per_layer.iter().map(|l| l.polygons).sum();
        println!("  reachable only: {} triangles in {osc} submeshes (poly_count sums to {reachable})", oic / 3);
        check(oic / 3 <= reachable, "reachable-only is bounded by poly_count");
        check(oic <= ic, "reachable-only is a subset of both classes");

        let lines = block_on(c::level_nav_edges(game.to_string(), path.into(), None, None))
            .expect("level_nav_edges");
        let lb = raw_body(lines);
        let seg = LE::read_u32(&lb[8..]) as usize;
        let non_custom: usize = u
            .per_layer
            .iter()
            .map(|l| l.edges[0] + l.edges[1] + l.edges[3])
            .sum();
        println!("  edges: {} bytes, {seg} segments (unit has {non_custom} non-custom)", lb.len());
        check(LE::read_u32(&lb[0..]) == 0x4C4E_4352, "RCNL magic");
        check(seg == non_custom, "one segment per non-custom edge");
        check(40 + seg * 28 == lb.len(), "the line buffer is exactly sized");

        let clues = block_on(c::level_nav_clues(game.to_string(), path.into(), None))
            .expect("level_nav_clues");
        check(clues.len() == u.clues, "one clue per custom edge");
        // A clue actor stands over the edge it links across, so the bar is the
        // group's column plus a tile of slack; `fields` reports the exact figure.
        let inside = clues
            .iter()
            .filter(|k| {
                let dx = k.position[0] - k.group_x as f32;
                let dz = k.position[2] - k.group_z as f32;
                (-16.0..=32.0).contains(&dx) && (-16.0..=32.0).contains(&dz)
            })
            .count();
        println!("  clues: {}, within a tile of their own group {inside}", clues.len());
        check(inside == clues.len(), "every clue is near its own group");
    }

    println!();
    if failures == 0 {
        println!("ALL CHECKS PASSED");
    } else {
        println!("{failures} CHECKS FAILED");
        std::process::exit(1);
    }
}
