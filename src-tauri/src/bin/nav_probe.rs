//! Measures and verifies the navigation data of the shipped PC build.
//!
//!   cargo run --release --bin nav_probe -- "<game dir>" [mode] [filter]
//!
//! Modes:
//!   census    every section of every `*navdata.zone`, with byte totals
//!   sections  parse all four nav sections on every zone that has them
//!   sizes     re-derive the element sizes from the payload-size identity alone
//!   fields    per-record field checks: winding, adjacency, bspheres, clues
//!   art       the geometric test: do nav vertices sit on the art surface?
//!   dump      one zone, in numbers and hex
//!
//! Read-only. Nothing is written to the game directory.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use byteorder::{ByteOrder, LE};
use omnitool_lib::core::level::{self, MeshOut};
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
    println!("  slot count == layers*nx*nz  {}", st.container_slots);

    println!("\n== D124430B  kZoneNavBuiltUnit ==");
    println!("  parsed {} / failed {}", st.unit_ok, st.unit_bad.len());
    for b in st.unit_bad.iter().take(5) {
        println!("    {b}");
    }
    println!("  inferred layers   {:?}", st.unit_layers);
    println!("  log2 tile size    {:?}", st.unit_scale);
    println!("  +0x04 == own asset id  {}", st.unit_self_id);
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
    clue_w_zero: Tally,
    clue_uid_high: BTreeMap<u32, u64>,
    clue_dist: Vec<f64>,
    clue_ability: BTreeMap<u16, u64>,
    clue_entity: BTreeMap<u16, u64>,
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
                        st.clue_in_column.add(
                            c.position[0] >= gx - t
                                && c.position[0] <= gx + 2.0 * t
                                && c.position[2] >= gz - t
                                && c.position[2] <= gz + 2.0 * t,
                        );
                        st.clue_w_zero.add(c.w == 0.0);
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
    println!("  position near its own group      {}", st.clue_in_column);
    println!("  +0x0C zero                       {}", st.clue_w_zero);
    println!("  distinct actor uids              {}", st.clue_uids.len());
    let mut h: Vec<_> = st.clue_uid_high.iter().collect();
    h.sort_by_key(|x| std::cmp::Reverse(*x.1));
    println!("  actor uid high dword             {:?}", &h[..h.len().min(6)]);
    if !st.clue_dist.is_empty() {
        let mut d = st.clue_dist.clone();
        d.sort_by(|x, y| x.partial_cmp(y).unwrap());
        println!(
            "  +0x20 f32: min {:.3} median {:.3} max {:.3}",
            d[0],
            d[d.len() / 2],
            d[d.len() - 1]
        );
    }
    let mut ab: Vec<_> = st.clue_ability.iter().collect();
    ab.sort_by_key(|x| std::cmp::Reverse(*x.1));
    println!("  +0x24 u16                        {:?}", &ab[..ab.len().min(8)]);
    let mut en: Vec<_> = st.clue_entity.iter().collect();
    en.sort_by_key(|x| std::cmp::Reverse(*x.1));
    println!("  +0x26 u16                        {:?}", &en[..en.len().min(8)]);

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

/// The Y of the art surface under `(x, z)`, taking the highest triangle that is
/// no more than `above` metres over the probe height -- a nav vertex sits on the
/// floor, not on the ceiling above it.
fn surface_y(tris: &[Tri], x: f32, z: f32, near: f32, probe_y: f32) -> Option<f32> {
    let mut best: Option<f32> = None;
    for t in tris {
        // barycentric in XZ
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
        let y = l1 * t[0][1] + l2 * t[1][1] + l3 * t[2][1];
        if (y - probe_y).abs() > near {
            continue;
        }
        if best.is_none_or(|b: f32| (y - probe_y).abs() < (b - probe_y).abs()) {
            best = Some(y);
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

fn art(idx: &level::LevelIndex, navs: &[&level::ZoneRec], limit: usize) {
    // Pair each nav zone with the art zones of the same region and tile.
    let mut by_region_tile: HashMap<(String, Option<String>), Vec<&level::ZoneRec>> = HashMap::new();
    for z in &idx.zones {
        if z.kind == "art" {
            by_region_tile
                .entry((z.region.clone(), z.tile.clone()))
                .or_default()
                .push(z);
        }
    }
    // Any art zone of the region, for the shuffled control.
    let mut by_region: HashMap<String, Vec<&level::ZoneRec>> = HashMap::new();
    for z in &idx.zones {
        if z.kind == "art" {
            by_region.entry(z.region.clone()).or_default().push(z);
        }
    }

    let picked: Vec<&&level::ZoneRec> = navs
        .iter()
        .filter(|z| {
            by_region_tile
                .get(&(z.region.clone(), z.tile.clone()))
                .is_some_and(|v| !v.is_empty())
        })
        .take(limit)
        .collect();

    println!(
        "{} nav zones share a region and tile with at least one art zone; testing {}",
        navs.iter()
            .filter(|z| by_region_tile.contains_key(&(z.region.clone(), z.tile.clone())))
            .count(),
        picked.len()
    );

    #[derive(Default)]
    struct R {
        zones: u64,
        art_tris: u64,
        nav_verts: u64,
        matched: u64,
        dy: Vec<f64>,
        ctl_matched: u64,
        ctl_dy: Vec<f64>,
        ctl_verts: u64,
    }

    let r = picked
        .par_iter()
        .fold(R::default, |mut r, z| {
            let key = (z.region.clone(), z.tile.clone());
            let Some(arts) = by_region_tile.get(&key) else { return r };
            let mut tris: Vec<Tri> = Vec::new();
            for a in arts.iter().take(4) {
                tris.extend(art_tris(idx, a, 400_000));
            }
            if tris.is_empty() {
                return r;
            }
            // The control: art from a *different* tile of the same region.
            let mut ctl: Vec<Tri> = Vec::new();
            if let Some(all) = by_region.get(&z.region) {
                if let Some(other) = all
                    .iter()
                    .find(|a| a.tile != z.tile && !arts.iter().any(|b| b.id == a.id))
                {
                    ctl = art_tris(idx, other, 400_000);
                }
            }

            let Ok(bytes) = idx.extract(z.id) else { return r };
            let Ok(zone) = Zone::parse(&bytes) else { return r };
            let Some(d) = zone.dat1.get_section_data(nav::NAV_UNIT_TAG) else { return r };
            let Ok(u) = NavUnit::parse(d) else { return r };

            r.zones += 1;
            r.art_tris += tris.len() as u64;

            // Bucket art triangles by 16 m column so the search is local.
            let mut grid: HashMap<(i32, i32), Vec<Tri>> = HashMap::new();
            for t in &tris {
                let mut cells: BTreeSet<(i32, i32)> = BTreeSet::new();
                for c in t.iter() {
                    cells.insert((
                        (c[0] / 16.0).floor() as i32,
                        (c[2] / 16.0).floor() as i32,
                    ));
                }
                for c in cells {
                    grid.entry(c).or_default().push(*t);
                }
            }
            let mut ctl_grid: HashMap<(i32, i32), Vec<Tri>> = HashMap::new();
            for t in &ctl {
                let mut cells: BTreeSet<(i32, i32)> = BTreeSet::new();
                for c in t.iter() {
                    cells.insert((
                        (c[0] / 16.0).floor() as i32,
                        (c[2] / 16.0).floor() as i32,
                    ));
                }
                for c in cells {
                    ctl_grid.entry(c).or_default().push(*t);
                }
            }

            for g in &u.groups {
                if g.is_empty() {
                    continue;
                }
                let cell = (g.x / 16, g.z / 16);
                let verts = u.verts(g);
                for v in &verts {
                    r.nav_verts += 1;
                    // Search this cell and its eight neighbours.
                    let mut best: Option<f32> = None;
                    for dx in -1..=1 {
                        for dz in -1..=1 {
                            if let Some(list) = grid.get(&(cell.0 + dx, cell.1 + dz)) {
                                if let Some(y) = surface_y(list, v[0], v[2], 4.0, v[1]) {
                                    if best.is_none_or(|b: f32| {
                                        (y - v[1]).abs() < (b - v[1]).abs()
                                    }) {
                                        best = Some(y);
                                    }
                                }
                            }
                        }
                    }
                    if let Some(y) = best {
                        r.matched += 1;
                        if r.dy.len() < 400_000 {
                            r.dy.push((y - v[1]) as f64);
                        }
                    }
                    if !ctl_grid.is_empty() {
                        r.ctl_verts += 1;
                        let mut cb: Option<f32> = None;
                        for dx in -1..=1 {
                            for dz in -1..=1 {
                                if let Some(list) = ctl_grid.get(&(cell.0 + dx, cell.1 + dz)) {
                                    if let Some(y) = surface_y(list, v[0], v[2], 4.0, v[1]) {
                                        if cb.is_none_or(|b: f32| {
                                            (y - v[1]).abs() < (b - v[1]).abs()
                                        }) {
                                            cb = Some(y);
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(y) = cb {
                            r.ctl_matched += 1;
                            if r.ctl_dy.len() < 400_000 {
                                r.ctl_dy.push((y - v[1]) as f64);
                            }
                        }
                    }
                }
            }
            r
        })
        .reduce(R::default, |mut a, b| {
            a.zones += b.zones;
            a.art_tris += b.art_tris;
            a.nav_verts += b.nav_verts;
            a.matched += b.matched;
            a.ctl_matched += b.ctl_matched;
            a.ctl_verts += b.ctl_verts;
            a.dy.extend(b.dy.into_iter().take(400_000));
            a.ctl_dy.extend(b.ctl_dy.into_iter().take(400_000));
            a
        });

    println!(
        "\n{} nav zones tested against {} art triangles; {} nav vertices",
        r.zones, r.art_tris, r.nav_verts
    );
    let report = |label: &str, matched: u64, total: u64, dy: &[f64]| {
        println!(
            "\n{label}: an art triangle covers the vertex in XZ within 4 m of its Y on {}/{} ({:.1}%)",
            matched,
            total,
            100.0 * matched as f64 / total.max(1) as f64
        );
        if dy.is_empty() {
            return;
        }
        let mut a: Vec<f64> = dy.iter().map(|x| x.abs()).collect();
        a.sort_by(|x, y| x.partial_cmp(y).unwrap());
        let frac = |t: f64| {
            a.iter().filter(|&&x| x <= t).count() as f64 / a.len() as f64 * 100.0
        };
        println!(
            "  |dy|: median {:.3} m  p90 {:.3}  p99 {:.3}   within 0.1 m {:.1}%  0.5 m {:.1}%  1 m {:.1}%",
            a[a.len() / 2],
            a[a.len() * 9 / 10],
            a[a.len() * 99 / 100],
            frac(0.1),
            frac(0.5),
            frac(1.0)
        );
        let signed: f64 = dy.iter().sum::<f64>() / dy.len() as f64;
        println!("  mean signed (art Y minus nav Y): {signed:+.4} m");
    };
    report("same tile", r.matched, r.nav_verts, &r.dy);
    report(
        "control, art of a different tile in the same region",
        r.ctl_matched,
        r.ctl_verts,
        &r.ctl_dy,
    );
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
