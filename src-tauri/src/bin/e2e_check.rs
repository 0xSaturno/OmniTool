//! End-to-end acceptance test for the Level Editor backend.
//!
//!   cargo run --release --bin e2e_check -- "<game dir>" [hashes file]
//!
//! Drives **every registered `level_*` / `prius_*` / `world_*` Tauri command**
//! the way the UI drives it -- the command function itself, through
//! `tauri::async_runtime::block_on`, with the same argument names -- against a
//! fixed set of real shipped assets: a busy art zone, a gameplay zone with
//! actors and scripts, a lighting zone, the two navdata zones of a region, an
//! impostor zone, a mission nodegraph, a conduit and `i29.level`.
//!
//! Every command gets at least one assertion about the *content* of what it
//! returned -- counts in range, ids that resolve, positions in plausible world
//! metres, packed buffers whose declared counts match their byte length, text
//! that mentions what it is about -- never "it returned Ok". Where empty is the
//! honest answer the emptiness itself is asserted to be well formed.
//!
//! One line per command, `PASS` or `FAIL`, and a non-zero exit on any failure.
//! Read-only apart from one throwaway Stager project, which it deletes again.

use std::fmt::Display;
use std::time::{Duration, Instant};

use omnitool_lib::commands_level_editor as ed;
use omnitool_lib::commands_level_entities as ent;
use omnitool_lib::commands_level_graphs as gr;
use omnitool_lib::commands_level_lighting as lt;
use omnitool_lib::commands_level_prius as pr;
use omnitool_lib::commands_level_script as sc;
use omnitool_lib::commands_level_world as wd;
use omnitool_lib::core::prius::{Edit, FieldView};
use tauri::async_runtime::block_on;
use tauri::ipc::{InvokeResponseBody, IpcResponse, Response};


/// The region every zone fixture is taken from.
const REGION: &str = "i29/openarea/sargasso";
/// A zone of distant skyline whose geometry lives only in its own block 2.
const IMPOSTOR_ZONE: &str =
    "levels/i29/openarea/megalopolis/i29_megalopolis_impostors_default_n19_art.zone";
const LEVEL: &str = "levels/i29/i29.level";
const PROJECT: &str = "e2e_check_scratch";

/// Nothing in this game is further than this from the origin; a coordinate
/// outside it means a decode is reading the wrong bytes, not a big level.
const WORLD_LIMIT: f32 = 100_000.0;

fn plausible(p: [f32; 3]) -> bool {
    p.iter().all(|c| c.is_finite() && c.abs() < WORLD_LIMIT)
}

// ---------------------------------------------------------------------------
// reporting
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Ck {
    problems: Vec<String>,
    notes: Vec<String>,
}

impl Ck {
    fn ok(&mut self, cond: bool, msg: impl Display) {
        if !cond {
            self.problems.push(msg.to_string());
        }
    }
    fn note(&mut self, msg: impl Display) {
        self.notes.push(msg.to_string());
    }
    /// Record a hard error and stop asserting -- the caller returns after this.
    fn err(&mut self, msg: impl Display) {
        self.problems.push(msg.to_string());
    }
}

#[derive(Default)]
struct Report {
    pass: usize,
    fail: usize,
    failed: Vec<String>,
    total: Duration,
}

impl Report {
    fn check(&mut self, name: &str, body: impl FnOnce(&mut Ck)) {
        let mut c = Ck::default();
        let t = Instant::now();
        body(&mut c);
        let dt = t.elapsed();
        self.total += dt;
        let ms = dt.as_secs_f64() * 1000.0;
        if c.problems.is_empty() {
            self.pass += 1;
            println!("PASS {name:<28} {ms:>8.1} ms  {}", c.notes.join("; "));
        } else {
            self.fail += 1;
            self.failed.push(name.to_string());
            println!("FAIL {name:<28} {ms:>8.1} ms  {}", c.notes.join("; "));
            for p in &c.problems {
                println!("       - {p}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// packed-buffer readers (what the front end has to do with these bytes)
// ---------------------------------------------------------------------------

fn raw(r: Response) -> Vec<u8> {
    match <Response as IpcResponse>::body(r) {
        Ok(InvokeResponseBody::Raw(v)) => v,
        Ok(InvokeResponseBody::Json(s)) => s.into_bytes(),
        Err(e) => panic!("response body: {e}"),
    }
}

fn u32at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn f32at(b: &[u8], at: usize) -> f32 {
    f32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

struct Rcms {
    vertices: u32,
    indices: u32,
    submeshes: u32,
    flags: u32,
    min: [f32; 3],
    max: [f32; 3],
    /// `(firstIndex, indexCount, materialIndex, flags)` per submesh.
    subs: Vec<[u32; 4]>,
    index_values: Vec<u32>,
    positions: Vec<[f32; 3]>,
}

/// Decode an `"RCMS"` buffer exactly as the viewport's `decodeRcms` must, and
/// complain about anything that would make it produce garbage.
fn parse_rcms(b: &[u8], c: &mut Ck, what: &str) -> Option<Rcms> {
    if b.len() < 48 {
        c.err(format!("{what}: {} bytes, below the 48-byte header", b.len()));
        return None;
    }
    if u32at(b, 0) != omnitool_lib::core::level::MESH_MAGIC {
        c.err(format!("{what}: magic {:#010X} is not RCMS", u32at(b, 0)));
        return None;
    }
    if u32at(b, 4) != omnitool_lib::core::level::MESH_VERSION {
        c.err(format!("{what}: version {}", u32at(b, 4)));
    }
    let (vc, ic, sc, flags) = (u32at(b, 8), u32at(b, 12), u32at(b, 16), u32at(b, 20));
    let has_uv1 = flags & omnitool_lib::core::level::mesh_flag::HAS_UV1 != 0;
    let want = 48
        + vc as usize * 32
        + if has_uv1 { vc as usize * 8 } else { 0 }
        + ic as usize * 4
        + sc as usize * 16;
    if b.len() != want {
        c.err(format!(
            "{what}: declares {vc} verts / {ic} indices / {sc} submeshes (uv1 {has_uv1}) \
             = {want} bytes, buffer is {}",
            b.len()
        ));
        return None;
    }
    let min = [f32at(b, 24), f32at(b, 28), f32at(b, 32)];
    let max = [f32at(b, 36), f32at(b, 40), f32at(b, 44)];
    let pos_at = 48;
    let idx_at = 48 + vc as usize * 32;
    let sub_at = idx_at + ic as usize * 4;
    let positions = (0..vc as usize)
        .map(|i| {
            [
                f32at(b, pos_at + i * 12),
                f32at(b, pos_at + i * 12 + 4),
                f32at(b, pos_at + i * 12 + 8),
            ]
        })
        .collect();
    let index_values = (0..ic as usize).map(|i| u32at(b, idx_at + i * 4)).collect();
    let subs = (0..sc as usize)
        .map(|i| {
            [
                u32at(b, sub_at + i * 16),
                u32at(b, sub_at + i * 16 + 4),
                u32at(b, sub_at + i * 16 + 8),
                u32at(b, sub_at + i * 16 + 12),
            ]
        })
        .collect();
    Some(Rcms {
        vertices: vc,
        indices: ic,
        submeshes: sc,
        flags,
        min,
        max,
        subs,
        index_values,
        positions,
    })
}

/// The invariants any drawable RCMS buffer has to satisfy.
fn check_rcms(m: &Rcms, c: &mut Ck, what: &str) {
    c.ok(
        m.indices % 3 == 0,
        format!("{what}: {} indices is not a triangle list", m.indices),
    );
    let bad = m.index_values.iter().filter(|&&i| i >= m.vertices).count();
    c.ok(
        bad == 0,
        format!("{what}: {bad} indices point past the {} vertices", m.vertices),
    );
    let covered: u32 = m.subs.iter().map(|s| s[1]).sum();
    c.ok(
        covered == m.indices,
        format!(
            "{what}: submeshes cover {covered} of {} indices",
            m.indices
        ),
    );
    for s in &m.subs {
        c.ok(
            (s[0] + s[1]) as usize <= m.indices as usize,
            format!("{what}: submesh run {}..{} leaves the index buffer", s[0], s[0] + s[1]),
        );
    }
    if m.vertices > 0 {
        c.ok(
            plausible(m.min) && plausible(m.max),
            format!("{what}: bounds {:?}..{:?} are not world metres", m.min, m.max),
        );
        for i in 0..3 {
            c.ok(
                m.min[i] <= m.max[i],
                format!("{what}: bounds inverted on axis {i}"),
            );
        }
        let outside = m
            .positions
            .iter()
            .filter(|p| {
                (0..3).any(|i| p[i] < m.min[i] - 1e-2 || p[i] > m.max[i] + 1e-2)
            })
            .count();
        c.ok(
            outside == 0,
            format!("{what}: {outside} vertices outside the declared bounds"),
        );
    }
}

/// First `Edit::Scalar` leaf with a byte span, depth-first, as a dotted path.
fn first_scalar(fields: &[FieldView], prefix: &str) -> Option<(String, f64, f64)> {
    for f in fields {
        let path = if prefix.is_empty() {
            f.name.clone()
        } else {
            format!("{prefix}.{}", f.name)
        };
        if f.edit == Edit::Scalar
            && f.count == 1
            && f.offset.is_some()
            && f.children.is_empty()
            && f.min.is_some()
        {
            return Some((path, f.min.unwrap_or(0.0), f.max.unwrap_or(0.0)));
        }
        if let Some(hit) = first_scalar(&f.children, &path) {
            return Some(hit);
        }
    }
    None
}

/// A spread of zones across the whole build: at most `per_region` zones of each
/// region matching `kind` (any kind when `kind` is `None`), smallest first so a
/// sweep does not pay for a 16 MB art zone, capped at `cap` zones in all.
///
/// A sweep over a spread like this is how a "this field is empty for every
/// input" claim gets made without reading all 9,046 zones.
fn spread(
    idx: &omnitool_lib::core::level::LevelIndex,
    kind: Option<&str>,
    per_region: usize,
    cap: usize,
    max_bytes: u32,
) -> Vec<String> {
    let mut by_region: std::collections::BTreeMap<&str, Vec<&omnitool_lib::core::level::ZoneRec>> =
        Default::default();
    for z in &idx.zones {
        if kind.is_some_and(|k| z.kind != k) || z.size > max_bytes || z.size == 0 {
            continue;
        }
        by_region.entry(z.region.as_str()).or_default().push(z);
    }
    let mut out = Vec::new();
    for (_, mut v) in by_region {
        v.sort_by_key(|z| std::cmp::Reverse(z.size));
        for z in v.into_iter().take(per_region) {
            out.push(z.path.clone());
            if out.len() >= cap {
                return out;
            }
        }
    }
    out
}

/// Where a DDL schema might be found when `ddl_schema::default_paths` does not
/// find one: the third command-line argument, then `refs/rivet/docs/ddl` walked
/// up from the working directory and from the executable.
///
/// The UI reaches this file through `prius_load_schema` with a path the user
/// picked, so a harness that only ever sees "no schema" never tests the typed
/// side of the property sheet at all.
fn schema_candidates() -> Vec<std::path::PathBuf> {
    const REL: &str = "refs/rivet/docs/ddl/rcra_ddl.json";
    let mut out = Vec::new();
    if let Some(a) = std::env::args().nth(3) {
        out.push(std::path::PathBuf::from(a));
    }
    let mut roots = vec![std::env::current_dir().ok()];
    roots.push(std::env::current_exe().ok().and_then(|p| p.parent().map(|q| q.to_path_buf())));
    for root in roots.into_iter().flatten() {
        let mut d = root.as_path();
        for _ in 0..7 {
            out.push(d.join(REL));
            match d.parent() {
                Some(p) => d = p,
                None => break,
            }
        }
    }
    out.retain(|p| p.is_file());
    out.dedup();
    out
}

fn main() {
    // No machine-specific defaults. The game directory is required; the asset
    // name list is optional, and without it names come from the shipped dag.
    let Some(game) = std::env::args().nth(1) else {
        eprintln!("usage: e2e_check <game directory> [asset name list]");
        eprintln!();
        eprintln!("  <game directory>   the Rift Apart install (the folder holding the toc)");
        eprintln!("  [asset name list]  optional; omit to take names from the shipped dag");
        std::process::exit(2);
    };
    let hashes = std::env::args().nth(2).unwrap_or_default();
    let h = || Some(hashes.clone());
    let mut r = Report::default();
    let t_all = Instant::now();

    println!("game   {game}");
    println!("hashes {hashes}\n");

    // ---- fixtures, chosen from the shipped index ---------------------------
    let mut art = String::new();
    let mut gp = String::new();
    let mut lgt = String::new();
    let mut vfx_zone = String::new();
    let mut curve_zone = String::new();
    let script_zone;
    // `(zone, row, (field path, min, max), is a script prius)`
    let mut plan: Option<(String, usize, (String, f64, f64), bool)> = None;
    let mut gp_candidates: Vec<String> = Vec::new();
    let mut lgt_candidates: Vec<String> = Vec::new();
    let mut curve_candidates: Vec<String> = Vec::new();

    r.check("level_list_planets", |c| {
        let regions = match block_on(ed::level_list_planets(game.clone(), h())) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(regions.len() > 100, format!("only {} regions", regions.len()));
        c.ok(
            regions.iter().any(|x| x.key == REGION),
            format!("{REGION} is not in the region list"),
        );
        let bad_group = regions
            .iter()
            .filter(|x| !["openarea", "overlay", "instance", "other"].contains(&x.group.as_str()))
            .count();
        c.ok(bad_group == 0, format!("{bad_group} regions with an unknown group"));
        let bad_kinds = regions
            .iter()
            .filter(|x| x.kinds.values().sum::<usize>() != x.zone_count)
            .count();
        c.ok(
            bad_kinds == 0,
            format!("{bad_kinds} regions whose kind histogram does not add up to zoneCount"),
        );
        let empty = regions.iter().filter(|x| x.zone_count == 0).count();
        c.ok(empty == 0, format!("{empty} regions with no zones"));
        let zones: usize = regions.iter().map(|x| x.zone_count).sum();
        c.note(format!("{} regions, {zones} zones", regions.len()));
        // Warm: the index is memoised, so a second call must cost nothing.
        let t = Instant::now();
        let _ = block_on(ed::level_list_planets(game.clone(), h()));
        c.ok(
            t.elapsed().as_millis() < 50,
            format!("warm call took {:?}; the index is not being memoised", t.elapsed()),
        );
    });

    r.check("level_list_zones", |c| {
        let zones = match block_on(ed::level_list_zones(game.clone(), REGION.into(), h())) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(zones.len() > 50, format!("{REGION} has only {} zones", zones.len()));
        let off_region = zones
            .iter()
            .filter(|z| !z.path.starts_with("levels/i29/openarea/sargasso/"))
            .count();
        c.ok(off_region == 0, format!("{off_region} zones outside the region's tree"));
        let bad_id = zones
            .iter()
            .filter(|z| {
                let want = omnitool_lib::core::crc64::hash(&z.path);
                u64::from_str_radix(z.id.trim_start_matches("0x"), 16) != Ok(want)
            })
            .count();
        c.ok(bad_id == 0, format!("{bad_id} zone ids are not the CRC-64 of the path"));
        let with = zones.iter().filter(|z| z.has_objects).count();
        c.ok(with > 0, "no zone in the region reports hasObjects");
        c.ok(with < zones.len(), "every zone reports hasObjects, which is wrong for a region full of stubs");
        // Biggest-first candidate lists per kind; the fixture inside each kind
        // is then chosen by what the command under test actually needs, not by
        // byte size, so a command is never declared broken because the zone it
        // was handed had nothing of its kind in it.
        let biggest = |kind: &str, n: usize| {
            let mut v: Vec<&ed::ZoneEntry> = zones
                .iter()
                .filter(|z| z.kind == kind && z.has_objects)
                .collect();
            v.sort_by_key(|z| std::cmp::Reverse(z.size));
            v.into_iter().take(n).map(|z| z.path.clone()).collect::<Vec<_>>()
        };
        art = biggest("art", 1).first().cloned().unwrap_or_default();
        gp_candidates = biggest("gp", 16);
        lgt_candidates = biggest("lgt", 16);
        gp = gp_candidates.first().cloned().unwrap_or_default();
        lgt = lgt_candidates.first().cloned().unwrap_or_default();
        vfx_zone = biggest("vfx", 1).first().cloned().unwrap_or_default();
        // Curves live in only 349 of the 9,046 shipped zones, so the search for
        // one is region-wide rather than by kind.
        let mut any: Vec<&ed::ZoneEntry> = zones
            .iter()
            .filter(|z| z.has_objects && z.size < 8 << 20)
            .collect();
        any.sort_by_key(|z| std::cmp::Reverse(z.size));
        curve_candidates = any.into_iter().take(60).map(|z| z.path.clone()).collect();
        for (k, p) in [("art", &art), ("gp", &gp), ("lgt", &lgt)] {
            c.ok(!p.is_empty(), format!("{REGION} has no {k} zone to test with"));
        }
        match block_on(ed::level_list_zones(game.clone(), "no/such/region".into(), h())) {
            Ok(v) => c.ok(v.is_empty(), format!("an unknown region returned {} zones", v.len())),
            Err(e) => c.err(format!("an unknown region errored instead of returning empty: {e}")),
        }
        c.note(format!(
            "{} zones, {with} with objects; art={}, {} gp / {} lgt candidates",
            zones.len(),
            art.rsplit('/').next().unwrap_or(""),
            gp_candidates.len(),
            lgt_candidates.len()
        ));
    });

    if art.is_empty() || gp.is_empty() {
        println!("\nFATAL: no zone fixtures; nothing else can be measured.");
        std::process::exit(1);
    }

    // ---- refine the fixtures: each command gets a zone that has its data ---
    {
        let t = Instant::now();
        // Gameplay zone: the one with the most prius rows, which is also the
        // one with actors, scripts and components worth inspecting.
        let mut best = (0usize, gp.clone());
        for z in &gp_candidates {
            if let Ok(s) = block_on(pr::prius_zone_summary(game.clone(), z.clone(), h())) {
                if s.rows > best.0 {
                    best = (s.rows, z.clone());
                }
            }
        }
        gp = best.1;
        // Lighting zone: the one that actually names an atmosphere, so
        // level_atmosphere_lighting has an input.
        let mut best_l = (0usize, lgt.clone());
        for z in &lgt_candidates {
            if let Ok(d) =
                block_on(lt::level_zone_lighting(game.clone(), z.clone(), Some(0), Some(true), h()))
            {
                let score = d.lights.len() + 1000 * d.atmospheres.iter().filter(|a| !a.path.is_empty()).count();
                if score > best_l.0 {
                    best_l = (score, z.clone());
                }
            }
        }
        lgt = best_l.1;
        // A zone that carries a script graph -- a different population from the
        // zones with the most priuses, so it gets its own fixture rather than
        // making the script commands look dead.
        let mut best_s = (0usize, String::new());
        for z in &gp_candidates {
            if let Ok(g) = block_on(sc::level_zone_script_graph(game.clone(), z.clone(), h(), None, Some(1))) {
                if g.stats.nodes > best_s.0 {
                    best_s = (g.stats.nodes, z.clone());
                }
            }
        }
        script_zone = best_s.1;
        // A zone that authors a curve.
        for z in curve_candidates.iter().chain(std::iter::once(&gp)) {
            if let Ok(v) = block_on(ent::level_zone_paths(game.clone(), z.clone(), h())) {
                if !v.is_empty() {
                    curve_zone = z.clone();
                    break;
                }
            }
        }
        println!(
            "fixtures chosen in {:?}: gp={} ({} prius rows), script={} ({} nodes), lgt={}, curves={}",
            t.elapsed(),
            gp.rsplit('/').next().unwrap_or(""),
            best.0,
            if script_zone.is_empty() { "none" } else { script_zone.rsplit('/').next().unwrap_or("") },
            best_s.0,
            lgt.rsplit('/').next().unwrap_or(""),
            if curve_zone.is_empty() { "none in sample" } else { curve_zone.rsplit('/').next().unwrap_or("") },
        );
    }

    // ---- level_load_zone ---------------------------------------------------
    let mut art_model = String::new();
    let mut art_models: Vec<String> = Vec::new();
    let mut art_geo: Vec<(u32, u8, u32)> = Vec::new();
    let mut art_offsets: Vec<u32> = Vec::new();
    let mut gp_actor_with_prius: Option<usize> = None;
    let mut prius_actors: Vec<usize> = Vec::new();

    r.check("level_load_zone", |c| {
        let s = match block_on(ed::level_load_zone(game.clone(), art.clone(), h(), None)) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(s.objects.len() > 100, format!("art zone has only {} objects", s.objects.len()));
        c.ok(!s.models.is_empty(), "art zone lists no models");
        c.ok(!s.dirty, "a shipped zone reports itself dirty");
        c.ok(!s.sections.is_empty(), "no DAT1 sections reported");
        match &s.bounds {
            None => c.err("art zone has no bounds"),
            Some(b) => {
                c.ok(
                    plausible(b.min) && plausible(b.max),
                    format!("bounds {:?}..{:?} are not world metres", b.min, b.max),
                );
                c.ok(
                    (0..3).all(|i| b.min[i] <= b.max[i]),
                    "bounds are inverted",
                );
            }
        }
        // Every instanced model id must appear in the models[] catalogue, and
        // the per-model instance counts must add up to the instances.
        let ids: std::collections::HashSet<&str> = s.models.iter().map(|m| m.id.as_str()).collect();
        let hex = |v: u64| format!("{v:#018X}");
        let instanced: Vec<_> = s.objects.iter().filter(|o| o.model_id != 0).collect();
        let missing = instanced.iter().filter(|o| !ids.contains(hex(o.model_id).as_str())).count();
        c.ok(missing == 0, format!("{missing} instances name a model the catalogue omits"));
        let summed: usize = s.models.iter().map(|m| m.instance_count).sum();
        c.ok(
            summed == instanced.len(),
            format!("instanceCount sums to {summed}, {} instances exist", instanced.len()),
        );
        // Placement: a shipped scene object is an affine row-major matrix.
        let bad_m = s
            .objects
            .iter()
            .filter(|o| (o.matrix[15] - 1.0).abs() > 1e-4 || !plausible(o.position()))
            .count();
        c.ok(bad_m == 0, format!("{bad_m} objects with a non-affine or out-of-world matrix"));
        let named = s.models.iter().filter(|m| !m.path.starts_with("0x")).count();
        c.ok(named > 0, "no model id resolved to a name");
        // Prefer a multi-slot model: a single-slot model would not exercise the
        // materialIndex -> slot mapping the viewport depends on.
        art_model = s.models.first().map(|m| m.id.clone()).unwrap_or_default();
        let mut best = 0usize;
        for m in s.models.iter().take(12) {
            if let Ok(mm) = block_on(ed::level_model_materials(game.clone(), m.id.clone())) {
                if mm.slots.len() > best {
                    best = mm.slots.len();
                    art_model = m.id.clone();
                }
            }
        }
        art_offsets = s.objects.iter().take(4).map(|o| o.offset).collect();
        art_models = s.models.iter().take(40).map(|m| m.id.clone()).collect();
        art_geo = s
            .objects
            .iter()
            .filter(|o| o.geometry.is_some())
            .take(12)
            .map(|o| (o.offset, o.kind, o.geometry.unwrap().vertex_count))
            .collect();
        c.note(format!(
            "{} objects, {} models ({named} named), {} actors, {} sections, block geometry {}",
            s.objects.len(),
            s.models.len(),
            s.actors.len(),
            s.sections.len(),
            s.block_geometry_count
        ));
        // This is the command every other one stands on, so it gets a
        // build-wide spread rather than one zone: a decode that fails on a
        // whole planet's worth of zones is the difference between a viewer and
        // a demo.
        let idx = match omnitool_lib::core::level::get_index(&game, Some(&hashes)) {
            Ok(i) => i,
            Err(e) => return c.err(e),
        };
        let sample = spread(&idx, None, 2, 400, 4 << 20);
        let (mut okz, mut errz, mut objs, mut bad_m, mut no_bounds, mut dirty, mut orphan) =
            (0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
        let mut first_err = String::new();
        for path in &sample {
            match block_on(ed::level_load_zone(game.clone(), path.clone(), h(), None)) {
                Err(e) => {
                    errz += 1;
                    if first_err.is_empty() {
                        first_err = format!("{path}: {e}");
                    }
                }
                Ok(z) => {
                    okz += 1;
                    objs += z.objects.len();
                    if z.dirty {
                        dirty += 1;
                    }
                    if !z.objects.is_empty() && z.bounds.is_none() {
                        no_bounds += 1;
                    }
                    bad_m += z
                        .objects
                        .iter()
                        .filter(|o| (o.matrix[15] - 1.0).abs() > 1e-4 || !plausible(o.position()))
                        .count();
                    let have: std::collections::HashSet<&str> =
                        z.models.iter().map(|m| m.id.as_str()).collect();
                    orphan += z
                        .objects
                        .iter()
                        .filter(|o| o.model_id != 0 && !have.contains(format!("{:#018X}", o.model_id).as_str()))
                        .count();
                }
            }
        }
        c.ok(errz == 0, format!("{errz} of {} zones failed to parse; first: {first_err}", sample.len()));
        c.ok(bad_m == 0, format!("{bad_m} objects across the spread have a bad matrix"));
        c.ok(orphan == 0, format!("{orphan} instances across the spread name a model the zone does not catalogue"));
        c.ok(no_bounds == 0, format!("{no_bounds} zones hold objects but report no bounds"));
        c.ok(dirty == 0, format!("{dirty} shipped zones report themselves dirty"));
        c.note(format!(
            "spread of {} zones: {okz} parsed, {errz} failed, {objs} scene objects",
            sample.len()
        ));
    });

    r.check("level_load_zone (gameplay)", |c| {
        let s = match block_on(ed::level_load_zone(game.clone(), gp.clone(), h(), None)) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(!s.actors.is_empty(), "gameplay zone has no actors");
        let named = s.actors.iter().filter(|a| !a.name.is_empty()).count();
        c.ok(named > 0, "no actor carries a name");
        let with_asset = s.actors.iter().filter(|a| !a.asset_path.is_empty()).count();
        c.ok(with_asset > 0, "no actor names an asset path");
        // scene_offset has to land on a real scene object.
        let offsets: std::collections::HashSet<u32> =
            s.objects.iter().map(|o| o.offset).collect();
        let dangling = s
            .actors
            .iter()
            .filter(|a| a.scene_offset != 0 && !offsets.contains(&a.scene_offset))
            .count();
        c.ok(dangling == 0, format!("{dangling} actors point at a scene offset that does not exist"));
        let flagged = s.actors.iter().filter(|a| !a.flag_names.is_empty()).count();
        c.ok(flagged > 0, "no actor has a single decoded flag");
        prius_actors = s
            .actors
            .iter()
            .enumerate()
            .filter(|(_, a)| a.prius_count > 0)
            .map(|(i, _)| i)
            .collect();
        gp_actor_with_prius = prius_actors.first().copied();
        c.ok(gp_actor_with_prius.is_some(), "no actor in the gameplay zone carries a prius");
        c.note(format!(
            "{} actors ({named} named, {with_asset} with assets), {} objects",
            s.actors.len(),
            s.objects.len()
        ));
    });

    // ---- geometry / materials / textures -----------------------------------
    let mut texture_id = String::new();

    r.check("level_model_mesh", |c| {
        if art_model.is_empty() {
            return c.err("no model fixture");
        }
        let b = raw(match block_on(ed::level_model_mesh(game.clone(), art_model.clone(), Some(0))) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        });
        let Some(m) = parse_rcms(&b, c, "mesh") else { return };
        c.ok(m.vertices > 0, "the zone's most-instanced model decodes to no geometry");
        c.ok(m.submeshes > 0, "no submeshes, so nothing can pick a material");
        check_rcms(&m, c, "mesh");
        c.note(format!(
            "{} verts, {} tris, {} submeshes, uv1 {}",
            m.vertices,
            m.indices / 3,
            m.submeshes,
            m.flags & 1 != 0
        ));
        // The `lod` parameter is the only way a viewport can trade detail for
        // throughput, so each level has to come back well formed, and the chain
        // has to actually get coarser.
        let mut chain = Vec::new();
        for lod in 0..4u32 {
            let b = raw(
                match block_on(ed::level_model_mesh(game.clone(), art_model.clone(), Some(lod))) {
                    Ok(v) => v,
                    Err(e) => {
                        c.err(format!("lod {lod}: {e}"));
                        continue;
                    }
                },
            );
            let Some(m) = parse_rcms(&b, c, &format!("lod {lod}")) else { continue };
            check_rcms(&m, c, &format!("lod {lod}"));
            chain.push((lod, m.vertices, m.indices / 3));
        }
        c.ok(chain.len() == 4, "a LOD level did not return a well-formed buffer");
        let nonzero: Vec<_> = chain.iter().filter(|x| x.1 > 0).collect();
        c.ok(!nonzero.is_empty(), "no LOD of the model decodes any geometry");
        c.ok(
            nonzero.windows(2).all(|w| w[0].1 >= w[1].1),
            format!("the LOD chain does not get coarser: {chain:?}"),
        );
        c.note(format!("lod chain (lod, verts, tris) {chain:?}"));
        // A LOD past the end of the chain does not error and does not come back
        // empty: `core::level::decode_model_mesh` falls back to LOD 0 on
        // purpose. Pin that, because it is the opposite of what a caller asking
        // for a coarse LOD wants and the response carries no way to tell.
        let far = raw(
            block_on(ed::level_model_mesh(game.clone(), art_model.clone(), Some(99)))
                .expect("an absent LOD must not error"),
        );
        let lod0 = raw(
            block_on(ed::level_model_mesh(game.clone(), art_model.clone(), Some(0)))
                .expect("lod 0 must not error"),
        );
        if let Some(m) = parse_rcms(&far, c, "lod 99") {
            c.ok(
                far == lod0,
                format!(
                    "LOD 99 returned {} verts, which is neither empty nor the documented \
                     LOD-0 fallback ({} verts)",
                    m.vertices,
                    lod0.len()
                ),
            );
            c.note("lod 99 silently falls back to lod 0 (documented, but indistinguishable to a caller)");
        }
        // Every model the art zone places: an empty buffer is a hole in the
        // viewport, so the rate is the number that matters.
        let (mut good, mut empt, mut tris) = (0usize, 0usize, 0u64);
        for id in &art_models {
            let b = raw(
                match block_on(ed::level_model_mesh(game.clone(), id.clone(), Some(0))) {
                    Ok(v) => v,
                    Err(e) => {
                        c.err(format!("{id}: {e}"));
                        continue;
                    }
                },
            );
            let Some(m) = parse_rcms(&b, c, id) else { continue };
            check_rcms(&m, c, id);
            if m.vertices == 0 {
                empt += 1;
            } else {
                good += 1;
                tris += (m.indices / 3) as u64;
            }
        }
        c.ok(
            empt * 5 <= art_models.len(),
            format!("{empt} of {} art-zone models decode to nothing", art_models.len()),
        );
        c.note(format!(
            "{} zone models: {good} with geometry ({tris} triangles), {empt} empty",
            art_models.len()
        ));
        // A model that does not exist must come back as the empty buffer, not
        // an error: the viewport uploads a whole zone in one loop.
        let miss = raw(
            block_on(ed::level_model_mesh(game.clone(), "0xDEADBEEFDEADBEEF".into(), Some(0)))
                .expect("a missing model must not error"),
        );
        if let Some(e) = parse_rcms(&miss, c, "missing model") {
            c.ok(e.vertices == 0 && e.indices == 0, "a missing model returned geometry");
        }
    });

    r.check("level_model_materials", |c| {
        if art_model.is_empty() {
            return c.err("no model fixture");
        }
        let mm = match block_on(ed::level_model_materials(game.clone(), art_model.clone())) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(!mm.slots.is_empty(), "the model declares no material slots");
        let idx: Vec<u32> = mm.slots.iter().map(|s| s.index).collect();
        let mut sorted = idx.clone();
        sorted.sort_unstable();
        sorted.dedup();
        c.ok(sorted.len() == idx.len(), "duplicate materialIndex across slots");
        let resolved = mm.slots.iter().filter(|s| s.resolved).count();
        c.ok(resolved > 0, "not one material slot resolved against the archives");
        let with_tex: Vec<_> = mm
            .slots
            .iter()
            .flat_map(|s| s.textures.iter())
            .filter(|t| t.texture_id != "0x0" && t.width > 0)
            .collect();
        c.ok(!with_tex.is_empty(), "no slot binds a texture present in the archives");
        texture_id = with_tex.first().map(|t| t.texture_id.clone()).unwrap_or_default();
        c.note(format!(
            "{} slots, {resolved} resolved, {} placeholder, {} textures",
            mm.slots.len(),
            mm.slots.iter().filter(|s| s.placeholder).count(),
            with_tex.len()
        ));
        // Every model of the art zone, not just one: the viewport needs a slot
        // for every submesh materialIndex of every model it draws, and the
        // blend recipe is the feature two thirds of an art tile's slots use.
        let mut slots = 0usize;
        let mut res = 0usize;
        let mut ph = 0usize;
        let mut blends = 0usize;
        let mut texs = 0usize;
        let mut models = 0usize;
        let mut no_slots = 0usize;
        for id in &art_models {
            let Ok(mm) = block_on(ed::level_model_materials(game.clone(), id.clone())) else {
                c.err(format!("{id}: materials would not resolve"));
                continue;
            };
            models += 1;
            if mm.slots.is_empty() {
                no_slots += 1;
            }
            slots += mm.slots.len();
            res += mm.slots.iter().filter(|s| s.resolved).count();
            ph += mm.slots.iter().filter(|s| s.placeholder).count();
            blends += mm.slots.iter().filter(|s| s.blend.is_some()).count();
            texs += mm.slots.iter().map(|s| s.textures.len()).sum::<usize>();
            // The slot table has to cover the mesh's own material indices, or
            // a submesh draws with no material at all.
            if let Ok(resp) = block_on(ed::level_model_mesh(game.clone(), id.clone(), Some(0))) {
                let b = raw(resp);
                if let Some(m) = parse_rcms(&b, c, "mesh for slot coverage") {
                    let have: std::collections::HashSet<u32> = mm.slots.iter().map(|s| s.index).collect();
                    let miss = m.subs.iter().filter(|x| !have.contains(&x[2])).count();
                    c.ok(
                        miss == 0,
                        format!("{id}: {miss} submeshes name a materialIndex with no slot"),
                    );
                }
            }
            if models >= 40 {
                break;
            }
        }
        c.ok(no_slots == 0, format!("{no_slots} of {models} models declare no slot at all"));
        c.ok(res > 0, "no slot of any model in the zone resolved");
        c.ok(
            blends > 0,
            format!("not one of {slots} slots across {models} art-zone models carries a blend recipe"),
        );
        c.note(format!(
            "{models} models: {slots} slots, {res} resolved, {ph} placeholder, {texs} textures,              {blends} with a blend recipe"
        ));
    });

    r.check("level_texture", |c| {
        if texture_id.is_empty() {
            return c.err("no texture fixture");
        }
        let png = raw(
            match block_on(ed::level_texture(game.clone(), texture_id.clone(), Some(256))) {
                Ok(v) => v,
                Err(e) => return c.err(e),
            },
        );
        c.ok(png.len() > 100, format!("decoded PNG is {} bytes", png.len()));
        c.ok(
            png.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
            "the buffer is not a PNG",
        );
        if png.len() > 24 {
            let w = u32::from_be_bytes(png[16..20].try_into().unwrap());
            let hh = u32::from_be_bytes(png[20..24].try_into().unwrap());
            c.ok(w > 0 && hh > 0 && w <= 256 && hh <= 256,
                format!("PNG is {w}x{hh}; maxSize 256 was asked for"));
            c.note(format!("{} bytes, {w}x{hh}", png.len()));
        }
        // Missing texture: empty buffer, not an error.
        let miss = raw(
            block_on(ed::level_texture(game.clone(), "0xDEADBEEFDEADBEEF".into(), Some(256)))
                .expect("a missing texture must not error"),
        );
        c.ok(miss.is_empty(), format!("a missing texture returned {} bytes", miss.len()));
        // One texture proves the path; the zone's whole texture set proves the
        // viewport can actually dress it. An empty buffer is "undecodable", and
        // the viewport draws grey for it, so the rate matters.
        let mut want: Vec<String> = Vec::new();
        for id in art_models.iter().take(24) {
            if let Ok(mm) = block_on(ed::level_model_materials(game.clone(), id.clone())) {
                for t in mm.slots.iter().flat_map(|s| s.textures.iter()) {
                    if t.texture_id != "0x0" && !want.contains(&t.texture_id) {
                        want.push(t.texture_id.clone());
                    }
                }
            }
        }
        want.truncate(48);
        let mut okn = 0usize;
        let mut empty = 0usize;
        let mut bytes = 0usize;
        for t in &want {
            let b = raw(
                match block_on(ed::level_texture(game.clone(), t.clone(), Some(128))) {
                    Ok(v) => v,
                    Err(e) => {
                        c.err(format!("{t}: {e}"));
                        continue;
                    }
                },
            );
            if b.is_empty() {
                empty += 1;
            } else if b.starts_with(&[0x89, b'P', b'N', b'G']) {
                okn += 1;
                bytes += b.len();
            } else {
                c.err(format!("{t}: {} bytes that are not a PNG", b.len()));
            }
        }
        c.ok(
            !want.is_empty(),
            "no texture ids could be collected from the zone's materials",
        );
        c.ok(
            empty * 4 <= want.len(),
            format!("{empty} of {} zone textures decoded to an empty buffer", want.len()),
        );
        c.note(format!(
            "{} zone textures: {okn} PNG ({} KiB), {empty} undecodable",
            want.len(), bytes / 1024
        ));
    });

    r.check("level_zone_geometry", |c| {
        let s = match block_on(ed::level_load_zone(game.clone(), IMPOSTOR_ZONE.into(), h(), None)) {
            Ok(v) => v,
            Err(e) => return c.err(format!("{IMPOSTOR_ZONE}: {e}")),
        };
        c.ok(
            s.block_geometry_count > 0,
            "the impostor zone reports no block-2 geometry",
        );
        let owners: Vec<&omnitool_lib::core::zone::SceneObject> =
            s.objects.iter().filter(|o| o.geometry.is_some()).collect();
        let mut tris = 0u64;
        let mut decoded = 0usize;
        for o in owners.iter().take(12) {
            let b = raw(
                match block_on(ed::level_zone_geometry(game.clone(), IMPOSTOR_ZONE.into(), o.offset, None)) {
                    Ok(v) => v,
                    Err(e) => {
                        c.err(format!("offset {}: {e}", o.offset));
                        continue;
                    }
                },
            );
            let Some(m) = parse_rcms(&b, c, &format!("impostor @{}", o.offset)) else { continue };
            check_rcms(&m, c, &format!("impostor @{}", o.offset));
            if m.vertices > 0 {
                decoded += 1;
                tris += (m.indices / 3) as u64;
                let g = o.geometry.unwrap();
                c.ok(
                    m.vertices == g.vertex_count,
                    format!(
                        "offset {}: decoded {} verts, the entry declares {}",
                        o.offset, m.vertices, g.vertex_count
                    ),
                );
            }
        }
        c.ok(decoded > 0, "no impostor entry decoded to any geometry");
        // An offset that owns no geometry must come back empty, not error.
        let bogus = s.objects.iter().find(|o| o.geometry.is_none()).map(|o| o.offset);
        if let Some(off) = bogus {
            let b = raw(
                block_on(ed::level_zone_geometry(game.clone(), IMPOSTOR_ZONE.into(), off, None))
                    .expect("an offset with no geometry must not error"),
            );
            if let Some(m) = parse_rcms(&b, c, "no-geometry offset") {
                c.ok(m.vertices == 0, "an offset with no geometry returned vertices");
            }
        }
        c.note(format!(
            "impostor zone: {} entries own block-2 geometry, {decoded} of the first 12 decoded,              {tris} triangles",
            owners.len()
        ));
        // Kind 6 is the impostor shell; kind 3 is a projected decal mesh, a
        // different decoder in the same command. The art zone has those.
        let mut dec = 0usize;
        let mut dec_tris = 0u64;
        let mut kinds: std::collections::BTreeSet<u8> = Default::default();
        for &(off, kind, declared) in &art_geo {
            let b = raw(
                match block_on(ed::level_zone_geometry(game.clone(), art.clone(), off, None)) {
                    Ok(v) => v,
                    Err(e) => {
                        c.err(format!("art zone @{off}: {e}"));
                        continue;
                    }
                },
            );
            let Some(m) = parse_rcms(&b, c, &format!("art zone @{off}")) else { continue };
            check_rcms(&m, c, &format!("art zone @{off}"));
            if m.vertices > 0 {
                dec += 1;
                dec_tris += (m.indices / 3) as u64;
                kinds.insert(kind);
                c.ok(
                    m.vertices == declared,
                    format!("art zone @{off}: decoded {} verts, the entry declares {declared}", m.vertices),
                );
            }
        }
        c.ok(
            art_geo.is_empty() || dec > 0,
            format!("{} art-zone entries own block-2 geometry but none decoded", art_geo.len()),
        );
        c.note(format!(
            "art zone: {dec} of {} sampled entries decoded, kinds {kinds:?}, {dec_tris} triangles",
            art_geo.len()
        ));
    });

    // ---- entities ----------------------------------------------------------
    r.check("level_zone_entities", |c| {
        let e = match block_on(ent::level_zone_entities(game.clone(), gp.clone(), h())) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        let total = e.volumes.len() + e.curves.len() + e.vfx.len() + e.decals.len();
        c.ok(total > 0, "the gameplay zone yields no entity of any kind");
        for v in &e.volumes {
            c.ok(
                v.extents.iter().all(|x| x.is_finite() && *x >= 0.0 && *x < WORLD_LIMIT),
                format!("volume @{} has extents {:?}", v.offset, v.extents),
            );
            c.ok(!v.shape_name.is_empty(), format!("volume @{} has no shape name", v.offset));
        }
        for v in &e.vfx {
            c.ok(
                !v.asset_path.is_empty() || v.asset_id != 0,
                format!("vfx @{} names nothing", v.offset),
            );
        }
        c.note(format!(
            "{} volumes, {} curves, {} vfx, {} decals, nav {}",
            e.volumes.len(),
            e.curves.len(),
            e.vfx.len(),
            e.decals.len(),
            e.nav_unit.is_some()
        ));
        // The four entity kinds do not share a zone: effects are in the vfx
        // zone, decals in the art zone. Check each where it lives, so an
        // always-empty decoder cannot hide behind the wrong fixture.
        if !vfx_zone.is_empty() {
            match block_on(ent::level_zone_entities(game.clone(), vfx_zone.clone(), h())) {
                Ok(v) => {
                    c.ok(!v.vfx.is_empty(), "the region's biggest vfx zone holds no effect instance");
                    let unnamed = v.vfx.iter().filter(|x| x.asset_path.is_empty()).count();
                    c.ok(unnamed == 0, format!("{unnamed} vfx instances with no asset path"));
                    c.note(format!("vfx zone: {} vfx instances", v.vfx.len()));
                }
                Err(e) => c.err(format!("vfx zone: {e}")),
            }
        }
        match block_on(ent::level_zone_entities(game.clone(), art.clone(), h())) {
            Ok(v) => {
                c.ok(
                    !v.decals.is_empty(),
                    "the busiest art zone holds no decal, though its scene objects own decal geometry",
                );
                let nomat = v.decals.iter().filter(|d| d.material_path.is_empty()).count();
                c.ok(nomat == 0, format!("{nomat} decals naming no material"));
                let bad = v
                    .decals
                    .iter()
                    .filter(|d| !d.fade_distance.is_finite() || !d.lod_distance.is_finite())
                    .count();
                c.ok(bad == 0, format!("{bad} decals with a non-finite distance"));
                c.note(format!(
                    "art zone: {} decals, {} volumes, {} curves",
                    v.decals.len(), v.volumes.len(), v.curves.len()
                ));
            }
            Err(e) => c.err(format!("art zone: {e}")),
        }
    });

    // The row is read as `{u32 name_index, u32 asset_index, u32 scene_offset,
    // i32 prius_index, i32 prius_count, u16 flags, u16 pad, u64 uid}`. Which of
    // those words are signed is not a matter of taste: a word read as `u32`
    // that really carries -1 silently becomes 4 294 967 295 and indexes
    // nothing. Measured over all 3 597 loadable zones of the build (144 414
    // rows): name_index, asset_index and prius_count are never negative,
    // prius_index is -1 on 103 053 rows (71.4 %), and the half-word after the
    // flags is zero on every row. This check re-measures the same four claims
    // from the raw section bytes over a spread of regions, so the struct's own
    // types cannot be what makes it pass.
    // A zone the editor cannot open at all is a worse answer than a zone with
    // one unreadable prius sheet, so an undecodable DDL blob is recorded on the
    // blob and reported in `warnings` instead of failing the parse. This is the
    // one shipped zone where that happens, and it is checked by name: the zone
    // must load, the bad blob must be named, and the other 237 blobs of the
    // same zone must decode.
    r.check("zone_partial_prius_decode", |c| {
        const ZONE: &str =
            "levels/i29/overlay/storymission/mis_blizar/i29_blizar_startingarea_gp.zone";
        let scene = match block_on(ed::level_load_zone(game.clone(), ZONE.into(), h(), None)) {
            Ok(v) => v,
            Err(e) => return c.err(format!("{ZONE} will not load at all: {e}")),
        };
        c.ok(scene.objects.len() > 100, format!("only {} scene objects", scene.objects.len()));
        c.ok(
            scene.warnings.len() == 1,
            format!("expected exactly 1 warning, got {:?}", scene.warnings),
        );
        c.ok(
            scene.warnings.iter().any(|w| w.contains("did not decode")),
            "the warning does not say what went wrong",
        );
        let idx = match omnitool_lib::core::level::get_index(&game, Some(&hashes)) {
            Ok(i) => i,
            Err(e) => return c.err(e),
        };
        let Some(rec) = idx.zones.iter().find(|z| z.path == ZONE) else {
            return c.err(format!("{ZONE} is not in this build"));
        };
        let Ok(raw) = idx.extract(rec.id) else { return c.err("extract failed") };
        let Ok(mut z) = omnitool_lib::core::zone::Zone::parse(&raw) else {
            return c.err("Zone::parse failed")
        };
        let blobs = z.script_priuses.len() + z.actor_priuses.len();
        let bad: Vec<(u32, String)> = z
            .undecoded_priuses()
            .into_iter()
            .map(|(o, w)| (o, w.to_string()))
            .collect();
        c.ok(bad.len() == 1, format!("{} blobs did not decode, expected 1", bad.len()));
        c.ok(
            blobs - bad.len() >= 200,
            format!("only {} of {blobs} blobs decoded", blobs - bad.len()),
        );
        // The unread blob is written back from its original bytes, so a zone
        // holding one still round-trips byte for byte.
        c.ok(!z.is_dirty(), "parsing alone marked the zone dirty");
        // `to_asset_bytes` drops the 36-byte header the TOC prepends.
        let round = z.to_asset_bytes();
        let want = &raw[omnitool_lib::core::zone::ASSET_HEADER_LEN..];
        c.ok(
            round == want,
            format!(
                "round trip changed the file: {} bytes in, {} out",
                want.len(),
                round.len()
            ),
        );
        c.note(format!(
            "{} objects, {blobs} prius blobs, 1 undecoded at offset {} -- round trip byte-identical over {} bytes",
            scene.objects.len(),
            bad.first().map(|(o, _)| *o).unwrap_or(0),
            want.len()
        ));
    });

    // The word at +0x7C of a scene-object entry carries the entry's own byte
    // length and at least one flag. Measured over 162 103 entries in 209 zones:
    // its top byte is 0x00 or 0x80 and nothing else, bit 31 is set on 81.6 %,
    // and bits 24..30 are zero everywhere -- which is why `& 0x00FF_FFFF` and
    // `& 0x7FFF_FFFF` agree on everything shipped. `core::zone` uses the 24-bit
    // mask and preserves the top byte when it writes the word back. If a build
    // ever puts something in bits 24..30, the two masks diverge and the heap
    // walk silently reads the wrong entry, so it is asserted rather than
    // assumed.
    // REGRESSION GUARD. Zone Scene Objects is optional -- the build ships
    // `_art` zones with no heap at all -- and `build_index` used to `return`
    // the moment it was absent, skipping every plain section table below it as
    // well as the heap walk. Because `save` re-emits those sections from the
    // vectors `build_index` fills, and `put` only declines an empty body when
    // the section is *absent*, a zone with a script but no heap parsed to zero
    // actions and was then written back with its Zone Script Actions and Zone
    // Script Priuses bodies zeroed. This asserts the readers are not gated: the
    // decoded row count must equal what the section's own byte length implies,
    // on every zone, heap or no heap.
    r.check("zone_tables_ungated", |c| {
        let idx = match omnitool_lib::core::level::get_index(&game, Some(&hashes)) {
            Ok(i) => i,
            Err(e) => return c.err(e),
        };
        use omnitool_lib::core::zone as zm;
        let tables: [(&str, u32, usize, fn(&zm::Zone) -> usize); 4] = [
            ("Zone Script Actions", zm::TAG_SCRIPT_ACTIONS, 40, |z| z.actions.len()),
            ("Zone Actors", zm::TAG_ACTORS, 32, |z| z.actors.len()),
            ("Zone Actor Groups", zm::TAG_ACTOR_GROUPS, 24, |z| z.actor_groups.len()),
            ("Zone Model Names", zm::TAG_MODEL_NAMES, 12, |z| z.model_names.len()),
        ];
        let mut zones = 0usize;
        let mut heapless = 0usize;
        let mut heapless_with_script = 0usize;
        let mut mismatches: Vec<String> = Vec::new();
        let mut victim: Option<(String, Vec<u8>)> = None;
        for path in spread(&idx, None, 4, 320, 8 << 20) {
            let Some(rec) = idx.zones.iter().find(|z| z.path == path) else { continue };
            let Ok(raw) = idx.extract(rec.id) else { continue };
            let Ok(z) = zm::Zone::parse(&raw) else { continue };
            zones += 1;
            let has_heap = z.dat1.get_section_data(zm::TAG_SCENE_OBJECTS).is_some();
            if !has_heap {
                heapless += 1;
            }
            for (name, tag, stride, count_of) in tables {
                let Some(sec) = z.dat1.get_section_data(tag) else { continue };
                let want = sec.len() / stride;
                let got = count_of(&z);
                if tag == zm::TAG_SCRIPT_ACTIONS && !has_heap && want > 0 {
                    heapless_with_script += 1;
                    if victim.is_none() {
                        victim = Some((path.clone(), raw.clone()));
                    }
                }
                if got != want && mismatches.len() < 6 {
                    mismatches.push(format!(
                        "{path}: {name} is {} bytes ({want} rows) but parsed to {got}{}",
                        sec.len(),
                        if has_heap { "" } else { " [no heap -- the gate is back]" }
                    ));
                }
            }
        }
        c.ok(zones > 50, format!("only {zones} zones sampled"));
        c.ok(
            mismatches.is_empty(),
            format!("a section table decoded to the wrong row count: {}", mismatches.join(" | ")),
        );
        if heapless == 0 {
            c.note("WARNING: no heapless zone in this sample, so the gate regression was not exercised");
        }
        if let Some((path, raw)) = victim {
            match zm::Zone::parse(&raw) {
                Ok(mut z) => {
                    let out = z.to_asset_bytes();
                    let want = &raw[zm::ASSET_HEADER_LEN..];
                    c.ok(
                        out == want,
                        format!(
                            "{path} has a script but no heap and does not round-trip: {} bytes in, {} out",
                            want.len(),
                            out.len()
                        ),
                    );
                }
                Err(e) => c.err(format!("{path}: {e}")),
            }
        }
        c.note(format!(
            "{zones} zones: {heapless} with no scene-object heap, {heapless_with_script} of those carrying a script; all four tables decoded to their declared row counts"
        ));
    });

    // The three decode facts the whole tool rests on, each asserted against the
    // WRONG reading so the check cannot pass by coincidence. None of these was
    // asserted anywhere before.
    r.check("zone_decode_discriminators", |c| {
        let idx = match omnitool_lib::core::level::get_index(&game, Some(&hashes)) {
            Ok(i) => i,
            Err(e) => return c.err(e),
        };
        use omnitool_lib::core::crc64;
        use omnitool_lib::core::zone as zm;
        let mut entries = 0u64;
        let mut with_extras = 0u64;
        let mut u32_would_break = 0u64;
        let mut len_agrees = 0u64;
        let mut f0_zero = 0u64;
        let mut soa_hit = 0u64;
        let mut soa_rows = 0u64;
        let mut inter_hit = 0u64;
        let mut inter_rows = 0u64;
        for path in spread(&idx, None, 3, 240, 4 << 20) {
            let Some(rec) = idx.zones.iter().find(|z| z.path == path) else { continue };
            let Ok(raw) = idx.extract(rec.id) else { continue };
            let Ok(z) = zm::Zone::parse(&raw) else { continue };
            let slots = z.model_names.len().max(1);
            if let Some(heap) = z.dat1.get_section_data(zm::TAG_SCENE_OBJECTS) {
                for o in z.scene_objects.iter().filter(|o| o.kind == zm::KIND_MODEL) {
                    let off = o.offset as usize;
                    let Some(e) = heap.get(off..off + 0x140) else { continue };
                    let rd16 = |a: usize| u16::from_le_bytes(e[a..a + 2].try_into().unwrap());
                    let rd32 = |a: usize| u32::from_le_bytes(e[a..a + 4].try_into().unwrap());
                    entries += 1;
                    let extras = rd16(0x112);
                    if extras != 0 {
                        with_extras += 1;
                    }
                    if zm::Zone::entry_len(heap, off) == Some(0x140 + 0x20 * extras as usize) {
                        len_agrees += 1;
                    }
                    if rd32(0x110) as usize >= slots {
                        u32_would_break += 1;
                    }
                    if rd32(0xF0) == 0 {
                        f0_zero += 1;
                    }
                }
            }
            for tag in [zm::TAG_MODEL_NAMES, zm::TAG_ACTOR_ASSETS] {
                let Some(sec) = z.dat1.get_section_data(tag).map(<[u8]>::to_vec) else { continue };
                let n = sec.len() / 12;
                for i in 0..n {
                    let rd64 = |a: usize| u64::from_le_bytes(sec[a..a + 8].try_into().unwrap());
                    let rd32 = |a: usize| u32::from_le_bytes(sec[a..a + 4].try_into().unwrap());
                    soa_rows += 1;
                    let name = z.dat1.get_string(rd32(n * 8 + i * 4)).unwrap_or_default();
                    if !name.is_empty() && crc64::hash(&name) == rd64(i * 8) {
                        soa_hit += 1;
                    }
                    if sec.len() >= i * 12 + 12 {
                        inter_rows += 1;
                        let nm = z.dat1.get_string(rd32(i * 12 + 8)).unwrap_or_default();
                        if !nm.is_empty() && crc64::hash(&nm) == rd64(i * 12) {
                            inter_hit += 1;
                        }
                    }
                }
            }
        }
        c.ok(entries > 10_000, format!("only {entries} model entries sampled"));
        c.ok(
            with_extras > 0,
            "no entry in this sample carries extra records, so the u16/u32 question is not exercised",
        );
        c.ok(
            len_agrees == entries,
            format!("entry length != 0x140 + 0x20 * u16 at +0x112 on {} of {entries}", entries - len_agrees),
        );
        c.ok(
            u32_would_break == with_extras,
            format!("a u32 at +0x110 breaks {u32_would_break} entries but {with_extras} carry extras -- must be the same set"),
        );
        c.ok(
            f0_zero == entries,
            format!("+0xF0 is non-zero on {} of {entries}, so it could be the model index after all", entries - f0_zero),
        );
        c.ok(soa_rows > 5_000, format!("only {soa_rows} name rows sampled"));
        c.ok(
            soa_hit * 100 >= soa_rows * 99,
            format!("struct-of-arrays only matches crc64(name) on {soa_hit}/{soa_rows}"),
        );
        c.ok(
            inter_hit * 100 < inter_rows * 5,
            format!("the interleaved reading matches on {inter_hit}/{inter_rows} -- too close to call the layout proven"),
        );
        c.note(format!(
            "{entries} model entries: {with_extras} with extras, exactly the {u32_would_break} a u32 at +0x110 would corrupt; +0xF0 zero on {f0_zero}/{entries}; names crc64-match {soa_hit}/{soa_rows} struct-of-arrays vs {inter_hit}/{inter_rows} interleaved"
        ));
    });

    r.check("scene_entry_size_word", |c| {
        let idx = match omnitool_lib::core::level::get_index(&game, Some(&hashes)) {
            Ok(i) => i,
            Err(e) => return c.err(e),
        };
        let mut entries = 0u64;
        let mut top_byte: std::collections::BTreeMap<u8, u64> = Default::default();
        let mut bit31 = 0u64;
        let mut low24_ok = 0u64;
        let mut low31_ok = 0u64;
        let mut zones = 0usize;
        for path in spread(&idx, None, 3, 240, 4 << 20) {
            let Some(rec) = idx.zones.iter().find(|z| z.path == path) else { continue };
            let Ok(raw) = idx.extract(rec.id) else { continue };
            let Ok(z) = omnitool_lib::core::zone::Zone::parse(&raw) else { continue };
            let Some(heap) = z.dat1.get_section_data(omnitool_lib::core::zone::TAG_SCENE_OBJECTS)
            else {
                continue;
            };
            zones += 1;
            // Walk the heap from each entry the object table already resolved,
            // so this never guesses where an entry starts.
            for o in &z.scene_objects {
                let off = o.offset as usize;
                if off + 0x80 > heap.len() {
                    continue;
                }
                let w = u32::from_le_bytes(heap[off + 0x7C..off + 0x80].try_into().unwrap());
                entries += 1;
                *top_byte.entry((w >> 24) as u8).or_insert(0) += 1;
                bit31 += (w >> 31) as u64;
                let l24 = (w & 0x00FF_FFFF) as usize;
                let l31 = (w & 0x7FFF_FFFF) as usize;
                low24_ok += (l24 >= 0x80 && off + l24 <= heap.len()) as u64;
                low31_ok += (l31 >= 0x80 && off + l31 <= heap.len()) as u64;
            }
        }
        c.ok(entries > 10_000, format!("only {entries} entries sampled"));
        c.ok(
            low24_ok == entries,
            format!("the 24-bit mask leaves the heap on {} of {entries} entries", entries - low24_ok),
        );
        c.ok(
            low31_ok == entries,
            format!("the 31-bit mask leaves the heap on {} of {entries} entries", entries - low31_ok),
        );
        let stray: Vec<String> = top_byte
            .keys()
            .filter(|b| **b != 0x00 && **b != 0x80)
            .map(|b| format!("{b:#04X}"))
            .collect();
        c.ok(
            stray.is_empty(),
            format!("bits 24..30 are not unused after all -- top bytes {stray:?} appear"),
        );
        c.ok(bit31 > 0, "bit 31 is never set in this sample, so it is not the flag documented");
        let tops: Vec<String> = top_byte
            .iter()
            .map(|(b, n)| format!("{b:#04X}x{n}"))
            .take(8)
            .collect();
        c.note(format!(
            "{entries} entries over {zones} zones: bit 31 set on {bit31} ({:.1}%),              24-bit length in range {low24_ok}, 31-bit length in range {low31_ok},              top byte {}",
            bit31 as f64 * 100.0 / entries.max(1) as f64,
            tops.join(" ")
        ));
    });

    r.check("zone_actor_row_signs", |c| {
        let idx = match omnitool_lib::core::level::get_index(&game, Some(&hashes)) {
            Ok(i) => i,
            Err(e) => return c.err(e),
        };
        let mut rows = 0u64;
        let mut name_neg = 0u64;
        let mut asset_neg = 0u64;
        let mut prius_neg = 0u64;
        let mut count_neg = 0u64;
        let mut pad_nz = 0u64;
        let mut zones = 0usize;
        for path in spread(&idx, None, 3, 240, 4 << 20) {
            let Some(rec) = idx.zones.iter().find(|z| z.path == path) else { continue };
            let Ok(raw) = idx.extract(rec.id) else { continue };
            let Ok(z) = omnitool_lib::core::zone::Zone::parse(&raw) else { continue };
            let Some(sec) = z.dat1.get_section_data(omnitool_lib::core::zone::TAG_ACTORS) else {
                continue;
            };
            zones += 1;
            for r in sec.chunks_exact(32) {
                let w = |o: usize| i32::from_le_bytes(r[o..o + 4].try_into().unwrap());
                rows += 1;
                name_neg += (w(0) < 0) as u64;
                asset_neg += (w(4) < 0) as u64;
                prius_neg += (w(12) < 0) as u64;
                count_neg += (w(16) < 0) as u64;
                pad_nz += (u16::from_le_bytes(r[22..24].try_into().unwrap()) != 0) as u64;
            }
        }
        c.ok(rows > 1000, format!("only {rows} actor rows found; the spread is too thin to say anything"));
        c.ok(name_neg == 0, format!("name_index is negative on {name_neg} of {rows} rows, so reading it as u32 is wrong"));
        c.ok(asset_neg == 0, format!("asset_index is negative on {asset_neg} of {rows} rows, so reading it as u32 is wrong"));
        c.ok(count_neg == 0, format!("prius_count is negative on {count_neg} of {rows} rows"));
        c.ok(pad_nz == 0, format!("the half-word after the flags is non-zero on {pad_nz} of {rows} rows, so it is not padding"));
        c.ok(prius_neg > 0, "prius_index is never negative in this sample, so the -1 sentinel is unproven here");
        c.note(format!(
            "{rows} rows over {zones} zones: prius_index -1 on {prius_neg} ({:.1}%), name/asset/count never negative, pad always zero",
            prius_neg as f64 * 100.0 / rows.max(1) as f64
        ));
    });

    r.check("level_zone_paths", |c| {
        let mut z = if curve_zone.is_empty() { gp.clone() } else { curve_zone.clone() };
        let lines = match block_on(ent::level_zone_paths(game.clone(), z.clone(), h())) {
            Ok(v) => v,
            Err(e) => return c.err(format!("{z}: {e}")),
        };
        let lines = if !lines.is_empty() {
            lines
        } else {
            // Nothing in this region. Curves ship in only 349 of the 9,046
            // zones, so widen to a build-wide spread before calling the
            // command dead -- and if the spread finds nothing either, that is
            // a finding, not a fixture problem.
            let idx = match omnitool_lib::core::level::get_index(&game, Some(&hashes)) {
                Ok(i) => i,
                Err(e) => return c.err(e),
            };
            let sample = spread(&idx, None, 2, 400, 4 << 20);
            let mut hit = Vec::new();
            let mut zones_with = 0usize;
            let mut total = 0usize;
            for p in &sample {
                if let Ok(v) = block_on(ent::level_zone_paths(game.clone(), p.clone(), h())) {
                    if !v.is_empty() {
                        zones_with += 1;
                        total += v.len();
                        if hit.is_empty() {
                            z = p.clone();
                            hit = v;
                        }
                    }
                }
            }
            c.note(format!(
                "none in {REGION}; build-wide spread of {} zones: {zones_with} with curves,                  {total} curves",
                sample.len()
            ));
            c.ok(
                zones_with > 0,
                format!("not one of {} zones across the whole build authors a curve", sample.len()),
            );
            hit
        };
        if lines.is_empty() {
            return;
        }
        for l in &lines {
            c.ok(l.points.len() >= 2, format!("curve @{} has {} points", l.offset, l.points.len()));
            c.ok(
                l.points.iter().all(|&p| plausible(p)),
                format!("curve @{} has a point outside the world", l.offset),
            );
            c.ok(!l.name.is_empty(), format!("curve @{} has no label", l.offset));
            if let Some(ns) = &l.normals {
                c.ok(
                    ns.len() + usize::from(l.looping) >= l.points.len(),
                    format!("curve @{}: {} normals for {} points", l.offset, ns.len(), l.points.len()),
                );
            }
            // Stored arc length must agree with the polyline it came with.
            let walk: f32 = l
                .points
                .windows(2)
                .map(|w| {
                    let d = [w[1][0] - w[0][0], w[1][1] - w[0][1], w[1][2] - w[0][2]];
                    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
                })
                .sum();
            if l.length > 0.1 {
                c.ok(
                    walk >= l.length * 0.5 && walk <= l.length * 2.0 + 1.0,
                    format!(
                        "curve @{}: stored length {:.2} m, polyline walks {:.2} m",
                        l.offset, l.length, walk
                    ),
                );
            }
        }
        c.note(format!("{}: {} curves, longest {:.1} m",
            z.rsplit('/').next().unwrap_or(""), lines.len(),
            lines.iter().map(|l| l.length).fold(0.0f32, f32::max)));
    });

    // ---- navigation --------------------------------------------------------
    let mut nav_unit_zone = String::new();
    let mut nav_container_zone = String::new();

    // ---- lighting ----------------------------------------------------------
    let mut atmosphere = String::new();

    r.check("level_zone_lighting", |c| {
        let d = match block_on(lt::level_zone_lighting(game.clone(), lgt.clone(), Some(0), Some(true), h())) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(!d.lights.is_empty(), format!("{lgt} decodes no lights"));
        c.ok(
            d.type_counts.iter().sum::<u32>() as usize == d.lights.len(),
            format!("type histogram sums to {}, {} lights", d.type_counts.iter().sum::<u32>(), d.lights.len()),
        );
        c.ok(d.condition_name == "day", format!("condition 0 is named {:?}", d.condition_name));
        let bad_pos = d.lights.iter().filter(|l| !plausible(l.position)).count();
        c.ok(bad_pos == 0, format!("{bad_pos} lights outside the world"));
        let bad_dir = d
            .lights
            .iter()
            .filter(|l| {
                let n = (l.direction[0].powi(2) + l.direction[1].powi(2) + l.direction[2].powi(2)).sqrt();
                !(0.9..1.1).contains(&n)
            })
            .count();
        c.ok(bad_dir == 0, format!("{bad_dir} lights with a non-unit direction"));
        let bad_rad = d
            .lights
            .iter()
            .filter(|l| !l.effective_radius.is_finite() || l.effective_radius < 0.0 || l.effective_radius > 10_000.0)
            .count();
        c.ok(bad_rad == 0, format!("{bad_rad} lights with an impossible radius"));
        let lit = d.lights.iter().filter(|l| l.luminance > 0.0).count();
        c.ok(lit > 0, "every light has zero luminance");
        let unnamed = d.lights.iter().filter(|l| l.light_type_name.is_empty()).count();
        c.ok(unnamed == 0, format!("{unnamed} lights with no type name"));
        // The `condition` parameter is what the day/night switch drives.
        let mut names = Vec::new();
        for k in 0..5usize {
            match block_on(lt::level_zone_lighting(game.clone(), lgt.clone(), Some(k), Some(false), h())) {
                Ok(x) => {
                    c.ok(x.condition == k, format!("asked for condition {k}, got {}", x.condition));
                    c.ok(!x.condition_name.is_empty(), format!("condition {k} has no name"));
                    c.ok(
                        x.lights.len() == d.lights.len(),
                        "the light list changed with the condition; it is meant to carry every light with its own mask",
                    );
                    names.push(x.condition_name);
                }
                Err(e) => c.err(format!("condition {k}: {e}")),
            }
        }
        c.ok(names.len() == 5, "a lighting condition could not be selected");
        let mut uniq = names.clone();
        uniq.sort();
        uniq.dedup();
        c.ok(uniq.len() == names.len(), format!("conditions share a name: {names:?}"));
        // Out of range must clamp, not panic or error.
        match block_on(lt::level_zone_lighting(game.clone(), lgt.clone(), Some(99), Some(false), h())) {
            Ok(x) => c.ok(x.condition == 4, format!("condition 99 clamped to {}", x.condition)),
            Err(e) => c.err(format!("condition 99 errored instead of clamping: {e}")),
        }
        atmosphere = d
            .atmospheres
            .iter()
            .find(|a| !a.path.is_empty())
            .map(|a| a.path.clone())
            .unwrap_or_default();
        c.note(format!(
            "{} lights {:?}, {} shadow casters, {} condition-dependent, {} atmospheres, \
             {} env probes, {} grid positions",
            d.lights.len(),
            d.type_counts,
            d.shadow_casters,
            d.condition_dependent_lights,
            d.atmospheres.len(),
            d.env_probes.len(),
            d.light_grid_positions
        ));
        // An art zone normally carries no lights: empty must still be valid.
        match block_on(lt::level_zone_lighting(game.clone(), art.clone(), Some(0), Some(false), h())) {
            Ok(a) => c.ok(
                a.type_counts.iter().sum::<u32>() as usize == a.lights.len(),
                "art zone histogram does not match its light list",
            ),
            Err(e) => c.err(format!("art zone: {e}")),
        }
        // Five of this DTO's fields are reference lists the lighting panel is
        // meant to offer. One lighting zone showing them empty says nothing; a
        // spread across the whole build showing them empty says the field is
        // dead weight. Sweep it and report, per field, how many zones populate it.
        let idx = match omnitool_lib::core::level::get_index(&game, Some(&hashes)) {
            Ok(i) => i,
            Err(e) => return c.err(e),
        };
        let sample = spread(&idx, Some("lgt"), 2, 120, 8 << 20);
        let mut t = [0usize; 6];
        let mut lights = 0usize;
        let mut zones_with_lights = 0usize;
        for p in &sample {
            let Ok(d) = block_on(lt::level_zone_lighting(game.clone(), p.clone(), Some(0), Some(true), h()))
            else {
                c.err(format!("{p}: lighting would not decode"));
                continue;
            };
            if !d.lights.is_empty() {
                zones_with_lights += 1;
                lights += d.lights.len();
            }
            t[0] += usize::from(!d.atmospheres.is_empty());
            t[1] += usize::from(!d.level_lights.is_empty());
            t[2] += usize::from(!d.light_bins.is_empty());
            t[3] += usize::from(!d.env_probe_textures.is_empty());
            t[4] += usize::from(!d.env_probes.is_empty());
            t[5] += usize::from(d.light_grid_positions > 0);
            c.ok(
                d.type_counts.iter().sum::<u32>() as usize == d.lights.len(),
                format!("{p}: type histogram does not match the light list"),
            );
        }
        c.ok(zones_with_lights > 0, "no lighting zone in the spread decodes a light");
        c.note(format!(
            "spread of {} lgt zones: {zones_with_lights} with lights ({lights} total);              populated -- atmospheres {}, levelLights {}, lightBins {}, envProbeTextures {},              envProbes {}, lightGrid {}",
            sample.len(), t[0], t[1], t[2], t[3], t[4], t[5]
        ));
    });

    r.check("level_atmosphere_lighting", |c| {
        if atmosphere.is_empty() {
            // No zone in the sample names one; take any shipped `.atmosphere`
            // out of the TOC so the command is still measured, and say so.
            match omnitool_lib::core::level::get_index(&game, Some(&hashes)) {
                Ok(idx) => {
                    let mut names: Vec<&String> = idx
                        .names
                        .iter()
                        .filter(|(id, n)| {
                            n.to_ascii_lowercase().ends_with(".atmosphere")
                                && idx.asset(**id).is_some_and(|a| a.size > 0)
                        })
                        .map(|(_, n)| n)
                        .collect();
                    names.sort();
                    atmosphere = names.first().map(|s| s.to_string()).unwrap_or_default();
                    c.note(format!(
                        "no sampled lighting zone named an atmosphere; used the TOC ({} shipped)",
                        names.len()
                    ));
                }
                Err(e) => return c.err(e),
            }
        }
        if atmosphere.is_empty() {
            return c.err("no .atmosphere asset could be found at all");
        }
        let a = match block_on(lt::level_atmosphere_lighting(game.clone(), atmosphere.clone(), h())) {
            Ok(v) => v,
            Err(e) => return c.err(format!("{atmosphere}: {e}")),
        };
        c.ok(a.header_size >= 184, format!("header is {} bytes", a.header_size));
        let n = (a.key_light_direction[0].powi(2)
            + a.key_light_direction[1].powi(2)
            + a.key_light_direction[2].powi(2))
        .sqrt();
        c.ok((0.99..1.01).contains(&n), format!("key light direction is not unit: {n}"));
        c.ok(
            a.key_light_intensity.is_finite() && a.key_light_intensity >= 0.0,
            format!("key light intensity {}", a.key_light_intensity),
        );
        c.ok(
            a.key_light_linear.iter().all(|v| v.is_finite() && *v >= 0.0),
            format!("linear key colour {:?}", a.key_light_linear),
        );
        c.ok(
            (0.0..=24.0).contains(&a.time_of_day) || a.time_of_day == 0.0,
            format!("time of day {}", a.time_of_day),
        );
        c.ok(a.csm_lod_count >= 0 && a.csm_lod_count <= 8, format!("{} CSM LODs", a.csm_lod_count));
        c.note(format!(
            "{}: intensity {:.3}, dir {:?}, az/el {:.1}/{:.1}, {} CSM LODs, {} strings",
            a.path.rsplit('/').next().unwrap_or(&a.path),
            a.key_light_intensity,
            a.key_light_direction.map(|v| (v * 100.0).round() / 100.0),
            a.key_light_azimuth_deg,
            a.key_light_elevation_deg,
            a.csm_lod_count,
            a.strings.len()
        ));
    });

    r.check("level_lighting_conditions", |c| {
        let cs = match block_on(lt::level_lighting_conditions()) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(cs.len() == 5, format!("{} conditions, the engine has 5", cs.len()));
        for (i, x) in cs.iter().enumerate() {
            c.ok(x.index == i, format!("row {i} reports index {}", x.index));
            c.ok(x.bit == 1u8 << i, format!("row {i} reports bit {:#04X}", x.bit));
            c.ok(!x.name.is_empty() && x.item_name.starts_with('k'), format!("row {i} names {:?}/{:?}", x.name, x.item_name));
            c.ok(x.name_hash != "0x00000000", format!("row {i} has a zero name hash"));
        }
        c.note(cs.iter().map(|x| x.name.clone()).collect::<Vec<_>>().join(", "));
    });

    // ---- prius -------------------------------------------------------------
    let mut schema_loaded = false;
    let mut prius_row: Option<usize> = None;
    let mut prius_actor: Option<usize> = None;
    let mut prius_field: Option<(String, f64, f64)> = None;
    let mut script_action: Option<usize> = None;

    r.check("prius_schema_status", |c| {
        let s = match block_on(pr::prius_schema_status()) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(!s.searched.is_empty(), "the status reports no search path, so a UI cannot tell the user where to put a schema");
        let present: Vec<&String> = s
            .searched
            .iter()
            .filter(|p| std::path::Path::new(p).exists())
            .collect();
        if s.loaded {
            c.ok(s.types > 0, "loaded, but zero types");
            c.ok(!s.source.is_empty(), "loaded, but no source named");
        } else {
            c.ok(
                present.is_empty(),
                format!("a schema exists at {present:?} but nothing loaded it"),
            );
            // The schema that ships with this repo lives under refs/; if the
            // default paths miss it, say so, because the UI then starts with
            // untyped sheets until the user browses for the file by hand.
            let near = schema_candidates();
            c.note(format!(
                "no default path resolved; {} schema file(s) found near the repo: {:?}",
                near.len(),
                near.iter().map(|p| p.display().to_string()).collect::<Vec<_>>()
            ));
        }
        c.note(format!(
            "loaded {}, {} types, {} enums, {} names; searched {:?}",
            s.loaded, s.types, s.enums, s.names, s.searched
        ));
    });

    r.check("prius_load_schema", |c| {
        // A bad path must be refused rather than silently reporting success.
        c.ok(
            block_on(pr::prius_load_schema("Z:/no/such/schema.ddl".into())).is_err(),
            "loading a path that does not exist returned Ok",
        );
        let s = match block_on(pr::prius_schema_status()) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        let mut found: Vec<std::path::PathBuf> = s
            .searched
            .iter()
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_file())
            .collect();
        found.extend(schema_candidates());
        let Some(p) = found.first().map(|p| p.display().to_string()) else {
            return c.err(
                "no DDL schema could be found on any default path or next to the repo,                  so every typed property sheet is running without member sets or defaults",
            );
        };
        let l = match block_on(pr::prius_load_schema(p.clone())) {
            Ok(v) => v,
            Err(e) => return c.err(format!("{p}: {e}")),
        };
        c.ok(l.loaded, format!("{p} loaded but reports loaded = false"));
        c.ok(l.types > 1000, format!("{p} loaded only {} types", l.types));
        c.ok(l.enums > 0, "the schema declares no enums, so no select field can list members");
        c.ok(l.names > 0, "the schema has no field-name index");
        c.ok(
            block_on(pr::prius_schema_status()).map(|x| x.types).unwrap_or(0) == l.types,
            "the status command does not see the schema that was just loaded",
        );
        schema_loaded = true;
        c.note(format!(
            "{} types, {} enums, {} names from {}",
            l.types, l.enums, l.names,
            p.replace('\\', "/").rsplit('/').next().unwrap_or("schema").to_string()
        ));
    });

    r.check("prius_zone_summary", |c| {
        let s = match block_on(pr::prius_zone_summary(game.clone(), gp.clone(), h())) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(s.actor_count > 0, "no actors");
        c.ok(s.rows > 0, "no prius rows");
        c.ok(s.pool_bytes > 0, "the prius pool is empty");
        c.ok(!s.components.is_empty(), "no components");
        let summed: usize = s.components.iter().map(|x| x.count).sum();
        c.ok(summed == s.rows, format!("component counts sum to {summed}, {} rows", s.rows));
        c.ok(
            s.actors_with_priuses > 0 && s.actors_with_priuses <= s.actor_count,
            format!("{} of {} actors carry priuses", s.actors_with_priuses, s.actor_count),
        );
        let unnamed = s.components.iter().filter(|x| x.component.is_empty()).count();
        c.ok(unnamed == 0, format!("{unnamed} components with no name"));
        if schema_loaded {
            let typed = s.components.iter().filter(|x| x.type_name.is_some()).count();
            c.ok(
                typed > 0,
                "a schema is loaded but not one component resolved to a schema type name",
            );
            c.note(format!("{typed} of {} components typed by the schema", s.components.len()));
        }
        let mismatched = s.components.iter().filter(|x| !x.type_uid_matches_name).count();
        c.note(format!(
            "{} actors ({} with priuses), {} rows, {} pool bytes, {} script prius bytes, \
             {} components, top {:?}, {mismatched} with a uid that is not crc of the name",
            s.actor_count, s.actors_with_priuses, s.rows, s.pool_bytes, s.script_prius_bytes,
            s.components.len(),
            s.components.first().map(|x| (x.component.clone(), x.count))
        ));
    });

    r.check("prius_actor_sheet", |c| {
        if prius_actors.is_empty() {
            return c.err("no actor with a prius was found");
        }
        // Every prius-bearing actor of the zone, not just the first: a single
        // component with an empty sheet would otherwise look like a dead
        // command, and an empty sheet across all of them would otherwise hide.
        let mut sheets = 0usize;
        let mut empty_sheets = 0usize;
        let mut fields = 0usize;
        let mut named = 0usize;
        let mut best = (0usize, 0usize); // (fields, actor index)
        for &ai in &prius_actors {
            let a = match block_on(pr::prius_actor_sheet(game.clone(), gp.clone(), ai, h())) {
                Ok(v) => v,
                Err(e) => {
                    c.err(format!("actor {ai}: {e}"));
                    continue;
                }
            };
            c.ok(a.actor_index == ai, format!("actor {ai}: the sheet reports {}", a.actor_index));
            c.ok(!a.sheets.is_empty(), format!("actor {ai} carries priuses but produced no sheets"));
            c.ok(a.failures.is_empty(), format!("actor {ai} decode failures: {:?}", a.failures));
            let mut actor_fields = 0usize;
            for s in &a.sheets {
                sheets += 1;
                c.ok(s.blob_size > 0, format!("row {} has a zero-size blob", s.row));
                c.ok(!s.view.component.is_empty(), format!("row {} has no component name", s.row));
                c.ok(
                    s.view.size as usize == s.blob_size,
                    format!("row {}: view says {} bytes, blob is {}", s.row, s.view.size, s.blob_size),
                );
                if s.view.total_fields == 0 {
                    empty_sheets += 1;
                }
                actor_fields += s.view.total_fields;
                fields += s.view.total_fields;
                named += s.view.named_fields;
                if prius_field.is_none() {
                    if let Some(f) = first_scalar(&s.view.fields, "") {
                        prius_row = Some(s.row);
                        prius_field = Some(f);
                        prius_actor = Some(ai);
                    }
                }
            }
            if actor_fields > best.0 {
                best = (actor_fields, ai);
            }
        }
        gp_actor_with_prius = Some(best.1);
        c.ok(fields > 0, format!("{sheets} sheets across {} actors decoded not one field", prius_actors.len()));
        c.ok(named > 0, "not one field resolved to a name");
        c.ok(
            empty_sheets * 2 <= sheets,
            format!("{empty_sheets} of {sheets} sheets decoded zero fields"),
        );
        c.note(format!(
            "{} actors, {sheets} sheets ({empty_sheets} with no fields), {fields} fields ({named} named); \
             richest actor {} with {} fields",
            prius_actors.len(), best.1, best.0
        ));
        // Out of range must be an error, not a silent empty sheet.
        c.ok(
            block_on(pr::prius_actor_sheet(game.clone(), gp.clone(), 1_000_000, h())).is_err(),
            "an out-of-range actor index returned Ok",
        );
        // The project's claim is 50,341 of 50,341 actor prius blobs decode.
        // Sweep the gameplay candidates and count the failures the command
        // itself reports, which is the only place a UI would see them.
        let (mut zsw, mut sh2, mut fails, mut flds) = (0usize, 0usize, 0usize, 0usize);
        let mut first = String::new();
        for z in &gp_candidates {
            let Ok(zs) = block_on(ed::level_load_zone(game.clone(), z.clone(), h(), None)) else { continue };
            zsw += 1;
            for (ai, _) in zs.actors.iter().enumerate().filter(|(_, a)| a.prius_count > 0).take(200) {
                let Ok(a) = block_on(pr::prius_actor_sheet(game.clone(), z.clone(), ai, h())) else {
                    c.err(format!("{z} actor {ai}: sheet failed"));
                    continue;
                };
                sh2 += a.sheets.len();
                flds += a.sheets.iter().map(|x| x.view.total_fields).sum::<usize>();
                fails += a.failures.len();
                if !a.failures.is_empty() && first.is_empty() {
                    first = format!("{z} actor {ai}: {:?}", a.failures);
                }
            }
        }
        c.ok(fails == 0, format!("{fails} prius rows would not decode; first {first}"));
        c.ok(sh2 > 0, "no sheet at all across the gameplay candidates");
        c.note(format!("{zsw} zones swept: {sh2} sheets, {flds} fields, {fails} decode failures"));
    });

    r.check("prius_script_sheet", |c| {
        // Script priuses live with the script graph, not with the actors.
        let z = if script_zone.is_empty() { gp.clone() } else { script_zone.clone() };
        let mut found = None;
        let mut errs = 0usize;
        for i in 0..128usize {
            match block_on(pr::prius_script_sheet(game.clone(), z.clone(), i, h())) {
                Ok(s) => {
                    if found.is_none() {
                        found = Some((i, s));
                    }
                }
                Err(_) => errs += 1,
            }
        }
        let Some((i, s)) = found else {
            return c.err(format!(
                "not one of the first 128 script actions of {z} yields a prius sheet                  ({errs} refused)"
            ));
        };
        script_action = Some(i);
        c.ok(s.row == i, "the sheet reports a different action index");
        c.ok(s.blob_size > 0, "zero-size script prius blob");
        c.ok(s.view.total_fields > 0, "the script prius decoded no fields");
        c.ok(
            s.view.size as usize == s.blob_size,
            format!("view says {} bytes, blob is {}", s.view.size, s.blob_size),
        );
        c.note(format!(
            "{}: action {i} {:?}, {} bytes at {}, {} fields ({} named)",
            z.rsplit('/').next().unwrap_or(""),
            s.view.component, s.blob_size, s.blob_offset, s.view.total_fields, s.view.named_fields
        ));
        c.ok(
            block_on(pr::prius_script_sheet(game.clone(), z.clone(), 1_000_000, h())).is_err(),
            "an out-of-range action index returned Ok",
        );
    });

    r.check("prius_plan_edit", |c| {
        // Hunt for an editable scalar leaf now, with whatever schema is
        // loaded: the field names a sheet reports depend on it.
        'hunt: for z in &gp_candidates {
            let Ok(s) = block_on(ed::level_load_zone(game.clone(), z.clone(), h(), None)) else { continue };
            for (ai, _) in s.actors.iter().enumerate().filter(|(_, a)| a.prius_count > 0).take(60) {
                let Ok(a) = block_on(pr::prius_actor_sheet(game.clone(), z.clone(), ai, h())) else { continue };
                for sh in &a.sheets {
                    if let Some(f) = first_scalar(&sh.view.fields, "") {
                        plan = Some((z.clone(), sh.row, f, false));
                        break 'hunt;
                    }
                }
            }
        }
        let script_fallback = plan.is_none();
        if script_fallback && !script_zone.is_empty() {
            for i in 0..128usize {
                let Ok(sh) = block_on(pr::prius_script_sheet(game.clone(), script_zone.clone(), i, h())) else {
                    continue;
                };
                if let Some(f) = first_scalar(&sh.view.fields, "") {
                    plan = Some((script_zone.clone(), i, f, true));
                    break;
                }
            }
        }
        c.ok(
            !script_fallback,
            "no actor prius in any sampled gameplay zone has an editable scalar field;              only script priuses do",
        );
        let Some((z, row, (path, lo, hi), is_script)) = plan.clone() else {
            return c.err(
                "no Edit::Scalar leaf was found in any sampled actor or script prius,                  so the one write path the editor has cannot be exercised at all",
            );
        };
        // Re-setting a field to its own value must produce the bytes already
        // there: equal length, identical content. That is the whole contract.
        fn find(fs: &[FieldView], p: &str, pre: &str) -> Option<f64> {
            for f in fs {
                let full = if pre.is_empty() { f.name.clone() } else { format!("{pre}.{}", f.name) };
                if full == p {
                    return f
                        .value
                        .as_f64()
                        .or_else(|| f.value.as_bool().map(|b| b as i32 as f64));
                }
                if let Some(v) = find(&f.children, p, &full) {
                    return Some(v);
                }
            }
            None
        }
        let current = if is_script {
            block_on(pr::prius_script_sheet(game.clone(), z.clone(), row, h()))
                .ok()
                .and_then(|s| find(&s.view.fields, &path, ""))
        } else {
            block_on(ed::level_load_zone(game.clone(), z.clone(), h(), None))
                .ok()
                .and_then(|zs| {
                    zs.actors
                        .iter()
                        .enumerate()
                        .filter(|(_, a)| a.prius_count > 0)
                        .find_map(|(ai, _)| {
                            let a = block_on(pr::prius_actor_sheet(game.clone(), z.clone(), ai, h())).ok()?;
                            a.sheets
                                .iter()
                                .find(|s| s.row == row)
                                .and_then(|s| find(&s.view.fields, &path, ""))
                        })
                })
        }
        .unwrap_or(lo);
        let p = match block_on(pr::prius_plan_edit(
            game.clone(), z.clone(), row, path.clone(), Some(current), None, Some(is_script), h(),
        )) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(p.ok, format!("planning {path} = {current} failed: {:?}", p.error));
        if p.ok {
            let (Some(nb), Some(ob)) = (&p.bytes, &p.old_bytes) else {
                return c.err("ok, but no bytes returned");
            };
            c.ok(nb.len() == ob.len(), format!("patch is {} bytes, the slot is {}", nb.len(), ob.len()));
            c.ok(nb == ob, "re-setting a field to its own value changed its bytes");
            c.ok(!nb.is_empty(), "an empty patch");
            c.ok(p.section_tag.is_some() && p.section_offset.is_some(), "no section tag or offset");
            c.ok(p.edit.is_some(), "no edit class returned");
            c.ok(p.old_text.is_some(), "no old text for an undo entry");
            c.note(format!(
                "{} row {row} {path} = {current} (range {lo}..{hi}) -> {} byte patch at {} of {}",
                z.rsplit('/').next().unwrap_or(""),
                nb.len(),
                p.section_offset.unwrap_or(0),
                p.section_tag.clone().unwrap_or_default()
            ));
            // Out of range must be refused, not clamped or written.
            let over = hi + 1e9;
            match block_on(pr::prius_plan_edit(
                game.clone(), z.clone(), row, path.clone(), Some(over), None, Some(is_script), h(),
            )) {
                Ok(b) => c.ok(!b.ok, format!("{over} was accepted into a slot whose range is {lo}..{hi}")),
                Err(e) => c.err(format!("an out-of-range value errored instead of refusing: {e}")),
            }
        }
        // A field that does not exist must be refused, not accepted.
        match block_on(pr::prius_plan_edit(
            game.clone(), z.clone(), row, "NoSuchFieldAtAll".into(), Some(1.0), None, Some(is_script), h(),
        )) {
            Ok(b) => {
                c.ok(!b.ok, "a nonexistent field path was accepted");
                c.ok(b.error.is_some(), "refused without saying why");
            }
            Err(e) => c.err(format!("a bad field path errored instead of refusing: {e}")),
        }
        // Neither value, and both values, must both be refused.
        match block_on(pr::prius_plan_edit(
            game.clone(), z.clone(), row, path.clone(), None, None, Some(is_script), h(),
        )) {
            Ok(b) => c.ok(!b.ok, "an edit with no value at all was accepted"),
            Err(e) => c.err(format!("no value errored instead of refusing: {e}")),
        }
        let _ = script_action;
    });

    // ---- script graph ------------------------------------------------------
    let mut script_nodes = 0usize;

    r.check("level_zone_script_graph", |c| {
        let z = if script_zone.is_empty() { gp.clone() } else { script_zone.clone() };
        let g = match block_on(sc::level_zone_script_graph(game.clone(), z.clone(), h(), None, None)) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(g.stats.nodes > 0, format!("{z} decodes no script nodes"));
        script_nodes = g.stats.nodes;
        c.ok(!g.nodes.is_empty(), "stats claim nodes but none were returned");
        c.ok(
            g.nodes.len() == g.stats.nodes || g.truncated,
            format!("{} nodes returned of {} without the truncated flag", g.nodes.len(), g.stats.nodes),
        );
        c.ok(g.stats.dangling_edges == 0, format!("{} dangling edges", g.stats.dangling_edges));
        let bad_edge = g
            .edges
            .iter()
            .filter(|e| e.from_node as usize >= g.stats.nodes || e.to_node as usize >= g.stats.nodes)
            .count();
        c.ok(bad_edge == 0, format!("{bad_edge} edges name a node outside the table"));
        c.ok(!g.subgraphs.is_empty(), "no connected components reported");
        let summed: usize = g.subgraphs.iter().map(|s| s.node_count).sum();
        c.ok(summed == g.stats.nodes, format!("subgraph membership sums to {summed}, {} nodes", g.stats.nodes));
        c.ok(
            g.stats.type_names_resolved > 0,
            "not one script node type resolved to a name",
        );
        c.ok(
            g.stats.in_count_mismatch == 0,
            format!("{} nodes whose stored in-plug count disagrees with the rebuilt edges", g.stats.in_count_mismatch),
        );
        // A subgraph filter must return a subset.
        if let Some(s0) = g.subgraphs.first() {
            match block_on(sc::level_zone_script_graph(game.clone(), z.clone(), h(), Some(s0.id), None)) {
                Ok(one) => c.ok(
                    one.nodes.len() == s0.node_count,
                    format!("subgraph {} has {} nodes, the filter returned {}", s0.id, s0.node_count, one.nodes.len()),
                ),
                Err(e) => c.err(format!("subgraph filter: {e}")),
            }
        }
        c.note(format!(
            "{}: {} nodes, {} edges, {} vars ({} bindings), {} subgraphs, types resolved {}/{}",
            z.rsplit('/').next().unwrap_or(""), g.stats.nodes, g.stats.edges, g.stats.var_rows, g.stats.var_bindings,
            g.stats.subgraphs, g.stats.type_names_resolved, g.stats.nodes
        ));
    });

    r.check("level_zone_script_dump", |c| {
        let z = if script_zone.is_empty() { gp.clone() } else { script_zone.clone() };
        let s = match block_on(sc::level_zone_script_dump(game.clone(), z.clone(), h(), None)) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(!s.is_empty(), "the dump is empty");
        c.ok(s.contains(&z), "the dump does not name the zone it is about");
        let lines = s.lines().count();
        c.ok(
            lines >= script_nodes,
            format!("{lines} lines for {script_nodes} nodes"),
        );
        c.note(format!("{lines} lines, {} bytes", s.len()));
    });

    r.check("level_zone_script_node", |c| {
        let z = if script_zone.is_empty() { gp.clone() } else { script_zone.clone() };
        let d = match block_on(sc::level_zone_script_node(game.clone(), z.clone(), 0, h())) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(d.node.index == 0, "asked for node 0, got another");
        c.ok(d.node.node_id != "0000000000000000", "node 0 has a zero id");
        c.ok(
            d.in_edges.len() == d.node.in_edges.len() && d.out_edges.len() == d.node.out_edges.len(),
            "the resolved edge lists do not match the node's own edge indices",
        );
        c.ok(
            d.in_edges.iter().all(|e| e.to_node == 0) && d.out_edges.iter().all(|e| e.from_node == 0),
            "an edge in node 0's lists does not touch node 0",
        );
        c.ok(d.vars.len() == d.node.vars.len(), "var list length differs from the bindings");
        if d.node.prius_size > 0 {
            match &d.prius_hex {
                None => c.err("the node declares a prius but none was returned"),
                Some(x) => c.ok(
                    x.len() == d.node.prius_size as usize * 2,
                    format!("{} hex chars for a {}-byte prius", x.len(), d.node.prius_size),
                ),
            }
        }
        c.note(format!(
            "node 0 {:?}: {} in, {} out, {} vars, prius {} bytes",
            d.node.type_name, d.in_edges.len(), d.out_edges.len(), d.vars.len(), d.node.prius_size
        ));
        c.ok(
            block_on(sc::level_zone_script_node(game.clone(), z.clone(), 1_000_000, h())).is_err(),
            "an out-of-range node index returned Ok",
        );
        // Node 0 has no prius on this zone, so the hex-blob branch would never
        // be touched. Find the nodes that do own one and check them.
        let Ok(g) = block_on(sc::level_zone_script_graph(game.clone(), z.clone(), h(), None, Some(0))) else {
            return c.err("the graph would not reload to find a node with a prius");
        };
        let with: Vec<u32> = g.nodes.iter().filter(|n| n.prius_size > 0).map(|n| n.index).collect();
        c.ok(!with.is_empty(), "no node of this zone's script graph owns a prius blob");
        let mut checked = 0usize;
        for &i in with.iter().take(16) {
            let Ok(dd) = block_on(sc::level_zone_script_node(game.clone(), z.clone(), i, h())) else {
                c.err(format!("node {i} would not load"));
                continue;
            };
            match &dd.prius_hex {
                None => c.err(format!("node {i} declares a {}-byte prius but none came back", dd.node.prius_size)),
                Some(x) => {
                    c.ok(
                        x.len() == dd.node.prius_size as usize * 2,
                        format!("node {i}: {} hex chars for {} bytes", x.len(), dd.node.prius_size),
                    );
                    c.ok(
                        x.chars().all(|ch| ch.is_ascii_hexdigit()),
                        format!("node {i}: the prius blob is not hex"),
                    );
                    checked += 1;
                }
            }
        }
        c.note(format!("{} nodes own a prius; {checked} of the first 16 returned a blob", with.len()));
    });

    // ---- nodegraphs and conduits ------------------------------------------
    let mut graph_path = String::new();
    let mut graph_nodes = 0usize;
    let mut conduit_path = String::new();

    r.check("level_list_graphs", |c| {
        let gs = match block_on(gr::level_list_graphs(game.clone(), h())) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(gs.len() >= 10, format!("only {} nodegraphs found", gs.len()));
        let empty = gs.iter().filter(|g| g.nodes == 0).count();
        c.ok(empty * 2 < gs.len(), format!("{empty} of {} graphs decode no nodes", gs.len()));
        let unknown = gs.iter().filter(|g| g.graph_type == "unknown").count();
        c.ok(unknown == 0, format!("{unknown} graphs with an unrecognised type"));
        let bad = gs.iter().filter(|g| g.edges > g.connections).count();
        c.ok(bad == 0, format!("{bad} graphs resolve more edges than they have connections"));
        let zero_bytes = gs.iter().filter(|g| g.bytes == 0).count();
        c.ok(zero_bytes == 0, format!("{zero_bytes} graphs report zero size"));
        let best = gs.iter().max_by_key(|g| g.nodes);
        if let Some(b) = best {
            graph_path = b.path.clone();
            graph_nodes = b.nodes;
        }
        c.note(format!(
            "{} graphs, {} nodes total, {} stale edges, biggest {} with {graph_nodes} nodes",
            gs.len(),
            gs.iter().map(|g| g.nodes).sum::<usize>(),
            gs.iter().map(|g| g.stale_edges).sum::<usize>(),
            graph_path.rsplit('/').next().unwrap_or("")
        ));
    });

    r.check("level_nodegraph", |c| {
        if graph_path.is_empty() {
            return c.err("no nodegraph fixture");
        }
        let g = match block_on(gr::level_nodegraph(game.clone(), graph_path.clone(), h())) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(
            g.nodes.len() == graph_nodes,
            format!("the listing said {graph_nodes} nodes, the decode returned {}", g.nodes.len()),
        );
        c.ok(!g.graph_type.is_empty() && g.graph_type != "unknown", format!("graph type {:?}", g.graph_type));
        let bad = g
            .edges
            .iter()
            .filter(|e| e.from >= g.nodes.len() || e.to >= g.nodes.len())
            .count();
        c.ok(bad == 0, format!("{bad} edges name a node outside the table"));
        c.ok(
            g.roots.iter().all(|&i| i < g.nodes.len()),
            "a root index is outside the node table",
        );
        c.ok(
            g.plugs == g.nodes.iter().map(|n| n.plugs.len()).sum::<usize>(),
            "the plug total does not match the nodes' plug lists",
        );
        let named = g.nodes.iter().filter(|n| !n.name.is_empty()).count();
        c.ok(named > 0, "not one node carries a name");
        let classed: usize = g.class_counts.iter().map(|(_, n)| n).sum();
        c.ok(classed == g.nodes.len(), format!("class histogram sums to {classed}, {} nodes", g.nodes.len()));
        c.ok(g.unclaimed_sections.is_empty(), format!("unread sections: {:?}", g.unclaimed_sections));
        c.note(format!(
            "{:?}: {} nodes ({named} named), {} edges, {} plugs, {} roots, {} refs, {} stale",
            g.graph_type, g.nodes.len(), g.edges.len(), g.plugs, g.roots.len(), g.refs.len(), g.stale_edges.len()
        ));
    });

    r.check("level_nodegraph_node", |c| {
        if graph_path.is_empty() {
            return c.err("no nodegraph fixture");
        }
        let d = match block_on(gr::level_nodegraph_node(game.clone(), graph_path.clone(), 0, h())) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(d.node.index == 0, "asked for node 0, got another");
        c.ok(!d.node.kind.is_empty(), "node 0 has no class name");
        c.ok(!d.params.is_null(), "node 0 returned a null payload, so an inspector has nothing to show");
        c.ok(
            d.incoming.iter().all(|e| e.to == 0) && d.outgoing.iter().all(|e| e.from == 0),
            "an edge in node 0's lists does not touch node 0",
        );
        c.ok(d.buffer_tag != "0x00000000", "node 0 has no payload section tag");
        c.note(format!(
            "node 0 {:?} {:?}: {} in, {} out, {} plugs, payload {} keys",
            d.node.kind, d.node.name, d.incoming.len(), d.outgoing.len(), d.node.plugs.len(),
            d.params.as_object().map(|o| o.len()).unwrap_or(0)
        ));
        c.ok(
            block_on(gr::level_nodegraph_node(game.clone(), graph_path.clone(), 1_000_000, h())).is_err(),
            "an out-of-range node index returned Ok",
        );
        // One node is not a decoder. Walk the graph and check every node's
        // detail against the graph's own edge list.
        let Ok(g) = block_on(gr::level_nodegraph(game.clone(), graph_path.clone(), h())) else {
            return c.err("the graph would not reload for the node sweep");
        };
        let mut nulls = 0usize;
        let mut mismatched = 0usize;
        for i in 0..g.nodes.len() {
            let Ok(dd) = block_on(gr::level_nodegraph_node(game.clone(), graph_path.clone(), i, h())) else {
                c.err(format!("node {i} would not load"));
                continue;
            };
            if dd.params.is_null() {
                nulls += 1;
            }
            let want_in = g.edges.iter().filter(|e| e.to == i).count();
            let want_out = g.edges.iter().filter(|e| e.from == i).count();
            if dd.incoming.len() != want_in || dd.outgoing.len() != want_out {
                mismatched += 1;
            }
            if dd.node.index != i {
                c.err(format!("asked for node {i}, got {}", dd.node.index));
            }
        }
        c.ok(mismatched == 0, format!("{mismatched} nodes whose detail edges disagree with the graph"));
        c.ok(nulls == 0, format!("{nulls} of {} nodes return a null payload", g.nodes.len()));
        c.note(format!("swept all {} nodes: {nulls} null payloads", g.nodes.len()));
    });

    r.check("level_nodegraph_outline", |c| {
        if graph_path.is_empty() {
            return c.err("no nodegraph fixture");
        }
        let s = match block_on(gr::level_nodegraph_outline(game.clone(), graph_path.clone(), h())) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(s.contains(&graph_path), "the outline does not name the graph");
        c.ok(
            s.contains(&format!("nodes {graph_nodes}")),
            format!("the outline header does not say 'nodes {graph_nodes}'"),
        );
        let lines = s.lines().count();
        c.ok(lines > 2, format!("{lines} lines of outline"));
        c.note(format!("{lines} lines, {} bytes", s.len()));
    });

    r.check("level_list_conduits", |c| {
        let cs = match block_on(gr::level_list_conduits(game.clone(), h(), None)) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(cs.len() > 100, format!("only {} conduits found", cs.len()));
        let empty = cs.iter().filter(|x| x.events == 0).count();
        c.ok(empty * 2 < cs.len(), format!("{empty} of {} conduits decode no events", cs.len()));
        let bad = cs
            .iter()
            .filter(|x| x.response_counts.iter().map(|(_, n)| n).sum::<usize>() != x.responses)
            .count();
        c.ok(bad == 0, format!("{bad} conduits whose response histogram does not match the total"));
        conduit_path = cs
            .iter()
            .max_by_key(|x| x.responses)
            .map(|x| x.path.clone())
            .unwrap_or_default();
        // The filter must actually filter.
        let f = block_on(gr::level_list_conduits(game.clone(), h(), Some("weapon".into())));
        match f {
            Ok(f) => {
                c.ok(f.len() < cs.len(), "the filter returned everything");
                c.ok(
                    f.iter().all(|x| x.path.to_ascii_lowercase().contains("weapon")),
                    "the filter let through a path that does not match",
                );
                c.note(format!("filter \"weapon\" -> {} of {}", f.len(), cs.len()));
            }
            Err(e) => c.err(format!("filtered listing: {e}")),
        }
        c.note(format!(
            "{} conduits, {} events, {} responses, {} vfx refs, biggest {}",
            cs.len(),
            cs.iter().map(|x| x.events).sum::<usize>(),
            cs.iter().map(|x| x.responses).sum::<usize>(),
            cs.iter().map(|x| x.visual_effects).sum::<usize>(),
            conduit_path.rsplit('/').next().unwrap_or("")
        ));
    });

    r.check("level_conduit", |c| {
        if conduit_path.is_empty() {
            return c.err("no conduit fixture");
        }
        let d = match block_on(gr::level_conduit(game.clone(), conduit_path.clone(), h())) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(!d.events.is_empty(), "the busiest conduit decodes no events");
        c.ok(
            d.decoded_bytes == d.built_bytes,
            format!("decoded {} of {} bytes of Conduit Built", d.decoded_bytes, d.built_bytes),
        );
        let bad_key = d.events.iter().filter(|e| !e.key_ok).count();
        c.ok(bad_key == 0, format!("{bad_key} events whose map key is not derived from the name"));
        let responses: usize = d
            .events
            .iter()
            .flat_map(|e| e.filters.iter())
            .map(|f| f.responses.len())
            .sum();
        c.ok(responses > 0, "no responses, so the routing table says nothing");
        let unnamed = d.events.iter().filter(|e| e.name.is_empty()).count();
        c.ok(unnamed == 0, format!("{unnamed} events with no name"));
        let bad_ref = d.refs.iter().filter(|x| !x.id_ok && !x.path.is_empty()).count();
        c.ok(bad_ref == 0, format!("{bad_ref} asset refs whose id is not the hash of the path"));
        c.note(format!(
            "{}: {} events, {responses} responses, {} refs, {} vfx, {} sounds, notes {:?}",
            d.path.rsplit('/').next().unwrap_or(""),
            d.events.len(), d.refs.len(), d.visual_effects.len(), d.sound_events.len(), d.notes
        ));
    });

    r.check("level_conduit_search", |c| {
        let hits = match block_on(gr::level_conduit_search(
            game.clone(), ".visualeffect".into(), h(), Some(50), Some(false),
        )) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(!hits.is_empty(), "searching for \".visualeffect\" found nothing");
        c.ok(hits.len() <= 50, format!("limit 50 returned {} hits", hits.len()));
        let off = hits
            .iter()
            .filter(|x| !x.value.to_ascii_lowercase().contains(".visualeffect"))
            .count();
        c.ok(off == 0, format!("{off} hits whose value does not contain the needle"));
        c.ok(
            hits.iter().all(|x| !x.path.is_empty() && !x.event.is_empty()),
            "a hit does not say which conduit or event it came from",
        );
        c.ok(
            block_on(gr::level_conduit_search(game.clone(), String::new(), h(), None, None)).is_err(),
            "an empty needle was accepted",
        );
        // A needle that cannot match must come back empty, not error.
        match block_on(gr::level_conduit_search(
            game.clone(), "zzz_no_such_constant_zzz".into(), h(), Some(10), Some(true),
        )) {
            Ok(n) => c.ok(n.is_empty(), format!("a nonsense needle matched {} times", n.len())),
            Err(e) => c.err(format!("a nonsense needle errored: {e}")),
        }
        c.note(format!("{} hits for \".visualeffect\" (capped at 50)", hits.len()));
    });

    // ---- world / level -----------------------------------------------------
    let mut hib_zone = String::new();
    let mut hib_zones: Vec<String> = Vec::new();
    let mut data_region: Option<usize> = None;
    let mut data_regions: Vec<usize> = Vec::new();

    r.check("world_open_level", |c| {
        let w = match block_on(wd::world_open_level(game.clone(), LEVEL.into(), h())) {
            Ok(v) => v,
            Err(e) => return c.err(format!("{LEVEL}: {e}")),
        };
        c.ok(w.version_known, "the level version is not one this build knows");
        c.ok(w.regions.len() > 100, format!("only {} regions", w.regions.len()));
        c.ok(w.zones.len() > 1000, format!("only {} zone rows", w.zones.len()));
        c.ok(!w.roots.is_empty(), "no root region");
        c.ok(
            w.roots.iter().all(|&i| i < w.regions.len()),
            "a root index is outside the region table",
        );
        let bad_parent = w
            .regions
            .iter()
            .filter(|x| x.parent.is_some_and(|p| p >= w.regions.len()))
            .count();
        c.ok(bad_parent == 0, format!("{bad_parent} regions with a parent outside the table"));
        let bad_child = w
            .regions
            .iter()
            .filter(|x| x.children.iter().any(|&k| k >= w.regions.len()))
            .count();
        c.ok(bad_child == 0, format!("{bad_child} regions with a child outside the table"));
        let bad_link = w
            .links
            .iter()
            .filter(|l| !plausible(l.position) || l.load_region.is_some_and(|k| k >= w.regions.len()))
            .count();
        c.ok(bad_link == 0, format!("{bad_link} links with a bad position or region"));
        let present = w.zones.iter().filter(|z| z.present).count();
        c.ok(present > 0, "not one zone row resolves to an asset in the archives");
        c.ok(w.hibernate_instances > 0, "the level holds no far-LOD instances");
        c.ok(
            w.hibernate_zones > 0 && w.hibernate_zones <= w.zones.len(),
            format!("{} hibernate zones of {} rows", w.hibernate_zones, w.zones.len()),
        );
        hib_zones = w
            .zones
            .iter()
            .filter(|z| z.hibernate_instances > 0)
            .map(|z| z.path.clone())
            .collect();
        hib_zone = w
            .zones
            .iter()
            .filter(|z| z.hibernate_instances > 0 && z.present)
            .max_by_key(|z| z.hibernate_instances)
            .map(|z| z.path.clone())
            .unwrap_or_default();
        data_regions = w
            .regions
            .iter()
            .enumerate()
            .filter(|(_, x)| !x.game_data.is_empty())
            .map(|(i, _)| i)
            .collect();
        data_region = data_regions.first().copied();
        c.ok(data_region.is_some(), "no region carries any game data");
        let typed = w.regions.iter().filter(|x| !x.region_type.is_empty()).count();
        c.ok(typed == w.regions.len(), "a region has no type name");
        c.note(format!(
            "{} regions ({typed} typed), {} zones ({present} present), {} links, \
             {} hibernate zones / {} instances, warnings {:?}",
            w.regions.len(), w.zones.len(), w.links.len(),
            w.hibernate_zones, w.hibernate_instances, w.warnings
        ));
    });

    r.check("world_region_data", |c| {
        let Some(ri) = data_region else {
            return c.err("no region with game data was found");
        };
        let bs = match block_on(wd::world_region_data(game.clone(), LEVEL.into(), ri, h())) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(!bs.is_empty(), format!("region {ri} said it had game data but returned no blocks"));
        for b in &bs {
            c.ok(!b.struct_name.is_empty(), format!("row {} has no struct name", b.row));
            c.ok(b.payload_size > 0, format!("row {} declares a zero payload", b.row));
            c.ok(b.name_hash != "0x00000000", format!("row {} has a zero name hash", b.row));
            c.ok(!b.fields.is_null(), format!("row {} decoded no fields at all", b.row));
        }
        let typed = bs
            .iter()
            .filter(|b| {
                b.planet.is_some()
                    || b.dimension.is_some()
                    || b.shadow_zone.is_some()
                    || b.overlay_trigger.is_some()
                    || b.impostor_draw_distance.is_some()
            })
            .count();
        c.note(format!(
            "region {ri}: {} blocks ({typed} with a typed view) {:?}",
            bs.len(),
            bs.iter().map(|b| b.struct_name.clone()).collect::<Vec<_>>()
        ));
        c.ok(
            block_on(wd::world_region_data(game.clone(), LEVEL.into(), 1_000_000, h())).is_err(),
            "an out-of-range region index returned Ok",
        );
        // One region proves the path; the whole set proves the typed views are
        // not a single lucky struct. Sweep every region that claims game data.
        let mut swept = 0usize;
        let mut blocks = 0usize;
        let mut typed = 0usize;
        let mut names: std::collections::BTreeMap<String, usize> = Default::default();
        for &ri in data_regions.iter().take(400) {
            let Ok(bs) = block_on(wd::world_region_data(game.clone(), LEVEL.into(), ri, h())) else {
                c.err(format!("region {ri}: game data would not decode"));
                continue;
            };
            swept += 1;
            c.ok(!bs.is_empty(), format!("region {ri} claims game data but returned no block"));
            for b in &bs {
                blocks += 1;
                *names.entry(b.struct_name.clone()).or_default() += 1;
                c.ok(b.payload_size > 0, format!("region {ri} row {} has a zero payload", b.row));
                c.ok(!b.struct_name.is_empty(), format!("region {ri} row {} is unnamed", b.row));
                if b.planet.is_some()
                    || b.dimension.is_some()
                    || b.shadow_zone.is_some()
                    || b.overlay_trigger.is_some()
                    || b.impostor_draw_distance.is_some()
                {
                    typed += 1;
                }
            }
        }
        c.ok(blocks > 0, "no region in the level yielded a game-data block");
        c.ok(names.len() > 1, format!("every block is the same struct: {names:?}"));
        c.ok(
            typed > 0,
            "not one block in the whole level got a typed view, so the decoders are unused",
        );
        c.note(format!(
            "{swept} of {} regions with game data: {blocks} blocks, {typed} typed, structs {names:?}",
            data_regions.len()
        ));
    });

    r.check("world_zone_hibernate", |c| {
        if hib_zone.is_empty() {
            return c.err("no zone with far-LOD instances was found");
        }
        let v = match block_on(wd::world_zone_hibernate(game.clone(), LEVEL.into(), hib_zone.clone(), h())) {
            Ok(Some(v)) => v,
            Ok(None) => return c.err(format!("{hib_zone} reported instances in the level summary but none here")),
            Err(e) => return c.err(e),
        };
        c.ok(!v.instances.is_empty(), "no instances");
        c.ok(!v.clusters.is_empty(), "instances but no clusters");
        c.ok(plausible(v.centre) && v.radius > 0.0, format!("centre {:?} radius {}", v.centre, v.radius));
        let bad = v
            .instances
            .iter()
            .filter(|i| !plausible(i.position) || (i.matrix[15] - 1.0).abs() > 1e-4)
            .count();
        c.ok(bad == 0, format!("{bad} instances with a bad position or matrix"));
        let unnamed = v.instances.iter().filter(|i| i.model.is_empty()).count();
        c.ok(unnamed == 0, format!("{unnamed} instances naming no model"));
        let far = v
            .instances
            .iter()
            .filter(|i| {
                let d = [i.position[0] - v.centre[0], i.position[1] - v.centre[1], i.position[2] - v.centre[2]];
                (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() > v.radius * 1.5 + 10.0
            })
            .count();
        c.ok(far == 0, format!("{far} instances sit outside the container's own bounding sphere"));
        let covered: usize = v.clusters.iter().map(|k| k.instance_count).sum();
        c.ok(
            covered == v.instances.len(),
            format!("clusters cover {covered} of {} instances", v.instances.len()),
        );
        let bad_cluster = v
            .clusters
            .iter()
            .filter(|k| !plausible(k.centre) || k.radius <= 0.0 || k.first_instance + k.instance_count > v.instances.len())
            .count();
        c.ok(bad_cluster == 0, format!("{bad_cluster} clusters with bad bounds or ranges"));
        c.note(format!(
            "{}: {} instances in {} clusters, centre {:?} r {:.0} m",
            hib_zone.rsplit('/').next().unwrap_or(""),
            v.instances.len(), v.clusters.len(),
            v.centre.map(|x| x.round()), v.radius
        ));
        // 613 of the level's zones have a hibernate container. Sweep them:
        // the matrices are drawn by the same pipeline as a zone's own objects,
        // so a bad one is a visible ghost in the wrong place.
        let (mut zsw, mut insts, mut bad2, mut nocluster, mut unnamed2) =
            (0usize, 0usize, 0usize, 0usize, 0usize);
        for zp in hib_zones.iter().take(200) {
            match block_on(wd::world_zone_hibernate(game.clone(), LEVEL.into(), zp.clone(), h())) {
                Err(e) => c.err(format!("{zp}: {e}")),
                Ok(None) => c.err(format!("{zp} is listed with instances but returned None")),
                Ok(Some(v)) => {
                    zsw += 1;
                    insts += v.instances.len();
                    bad2 += v
                        .instances
                        .iter()
                        .filter(|i| !plausible(i.position) || (i.matrix[15] - 1.0).abs() > 1e-4)
                        .count();
                    unnamed2 += v.instances.iter().filter(|i| i.model.is_empty()).count();
                    let covered: usize = v.clusters.iter().map(|k| k.instance_count).sum();
                    if covered != v.instances.len() {
                        nocluster += 1;
                    }
                }
            }
        }
        c.ok(bad2 == 0, format!("{bad2} far-LOD instances with a bad matrix or position"));
        c.ok(unnamed2 == 0, format!("{unnamed2} far-LOD instances naming no model"));
        c.ok(nocluster == 0, format!("{nocluster} zones whose clusters do not cover their instances"));
        c.note(format!("{zsw} hibernate zones swept, {insts} instances"));
        // A zone with no container must be Ok(None), not an error.
        match block_on(wd::world_zone_hibernate(game.clone(), LEVEL.into(), art.clone(), h())) {
            Ok(_) => {}
            Err(e) => c.err(format!("a zone with no container errored: {e}")),
        }
    });

    r.check("world_hibernate_ghost", |c| {
        if hib_zone.is_empty() {
            return c.err("no hibernate zone fixture");
        }
        // Find a placed instance of that zone which the level really does keep
        // a far-LOD copy of -- the folded uid is the only link between the two,
        // so matching it here is what makes a `true` answer provable.
        let s = block_on(ed::level_load_zone(game.clone(), hib_zone.clone(), h(), None));
        let Ok(s) = s else {
            return c.err(format!("{hib_zone} would not load"));
        };
        let view = block_on(wd::world_zone_hibernate(game.clone(), LEVEL.into(), hib_zone.clone(), h()));
        let ghosts: std::collections::HashSet<String> = match view {
            Ok(Some(v)) => v.instances.iter().map(|i| i.parent_id.clone()).collect(),
            _ => Default::default(),
        };
        let matched = s.objects.iter().find(|o| {
            o.model_id != 0
                && ghosts.contains(&format!(
                    "{:08X}",
                    omnitool_lib::core::level_asset::fold_instance_uid(o.instance_id)
                ))
        });
        c.ok(
            matched.is_some(),
            format!(
                "none of {hib_zone}'s {} instances folds onto one of the {} far-LOD records, \
                 so the ghost check can never answer yes",
                s.objects.len(),
                ghosts.len()
            ),
        );
        let uid = match matched.or_else(|| s.objects.iter().find(|o| o.model_id != 0)) {
            Some(o) => format!("{:016X}", o.instance_id),
            None => return c.err("the hibernate zone places no model instance to ask about"),
        };
        let expect_copy = matched.is_some();
        let g = match block_on(wd::world_hibernate_ghost(game.clone(), LEVEL.into(), hib_zone.clone(), uid.clone(), h())) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(g.parent_id.len() == 8, format!("folded uid {:?} is not 8 hex digits", g.parent_id));
        c.ok(
            g.has_far_lod_copy == expect_copy,
            format!(
                "the folded uid {} is {}in the level's far-LOD records, the command says {}",
                g.parent_id,
                if expect_copy { "" } else { "not " },
                g.has_far_lod_copy
            ),
        );
        if g.has_far_lod_copy {
            c.ok(!g.model.is_empty(), "a far-LOD copy was found but names no model");
            c.ok(g.position.is_some_and(plausible), format!("ghost position {:?}", g.position));
        } else {
            c.ok(g.model.is_empty() && g.position.is_none(),
                "no far-LOD copy, but a model or position came back anyway");
        }
        // A uid that cannot exist must answer no, not error.
        match block_on(wd::world_hibernate_ghost(
            game.clone(), LEVEL.into(), hib_zone.clone(), "FFFFFFFFFFFFFFFF".into(), h(),
        )) {
            Ok(n) => c.ok(!n.has_far_lod_copy, "an impossible uid matched a far-LOD copy"),
            Err(e) => c.err(format!("an impossible uid errored: {e}")),
        }
        // A zone that is not in this level must be refused.
        c.ok(
            block_on(wd::world_hibernate_ghost(
                game.clone(), LEVEL.into(), "levels/no/such.zone".into(), uid.clone(), h(),
            ))
            .is_err(),
            "a zone outside the level was accepted",
        );
        c.note(format!(
            "uid {uid} -> far-LOD copy {} {:?}, folded {}",
            g.has_far_lod_copy, g.model.rsplit('/').next().unwrap_or(""), g.parent_id
        ));
    });

    // ---- save round trip ---------------------------------------------------
    r.check("level_save_zone", |c| {
        if art_offsets.is_empty() {
            return c.err("no scene object offsets to edit");
        }
        let before = block_on(ed::level_load_zone(game.clone(), art.clone(), h(), None));
        let Ok(before) = before else { return c.err("could not re-read the zone") };
        let target = before.objects.iter().find(|o| o.offset == art_offsets[0]).cloned();
        let Some(t) = target else { return c.err("the chosen offset vanished") };
        let mut m = t.matrix;
        m[13] += 5.0;
        let saved = match block_on(ed::level_save_zone(
            game.clone(),
            art.clone(),
            PROJECT.into(),
            vec![ed::ZoneEditDto { offset: t.offset, matrix: Some(m.to_vec()), scale: None }],
        )) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(saved.bytes_written > 0, "nothing was written");
        c.ok(!saved.asset_path.is_empty(), "no asset path reported");
        let f = std::path::Path::new(&saved.project_file);
        c.ok(f.exists(), format!("{} does not exist", saved.project_file));
        if f.exists() {
            let raw = std::fs::read(f).unwrap_or_default();
            c.ok(
                raw.len() == saved.bytes_written,
                format!("reported {} bytes, file is {}", saved.bytes_written, raw.len()),
            );
            // The rebuilt asset must parse and carry exactly the edit.
            match omnitool_lib::core::zone::Zone::parse(&raw) {
                Ok(z) => {
                    let o = z.scene_objects.iter().find(|o| o.offset == t.offset);
                    match o {
                        None => c.err("the edited object is gone from the rebuilt zone"),
                        Some(o) => {
                            c.ok(
                                (o.matrix[13] - m[13]).abs() < 1e-3,
                                format!("row 3 Y is {} after writing {}", o.matrix[13], m[13]),
                            );
                            let others = z
                                .scene_objects
                                .iter()
                                .zip(before.objects.iter())
                                .filter(|(a, b)| a.offset != t.offset && a.matrix != b.matrix)
                                .count();
                            c.ok(others == 0, format!("{others} other objects moved"));
                            c.ok(
                                z.scene_objects.len() == before.objects.len(),
                                "the object count changed",
                            );
                        }
                    }
                }
                Err(e) => c.err(format!("the written asset does not re-parse: {e}")),
            }
        }
        c.note(format!("{} bytes into {}", saved.bytes_written, saved.asset_path));
        // Reject a malformed edit rather than writing nonsense.
        c.ok(
            block_on(ed::level_save_zone(
                game.clone(), art.clone(), PROJECT.into(),
                vec![ed::ZoneEditDto { offset: t.offset, matrix: Some(vec![0.0; 9]), scale: None }],
            ))
            .is_err(),
            "a 9-element matrix was accepted",
        );
        // Clean up the scratch project, and the projects directory itself when
        // the save created it and nothing else is in it.
        if let Ok(d) = omnitool_lib::core::filesystem::projects_dir() {
            let _ = std::fs::remove_dir_all(d.join(PROJECT));
            let _ = std::fs::remove_dir(&d);
        }
    });

    // ---- the whole-build sweep, last because it is the expensive one -------
    r.check("level_zone_stats", |c| {
        let s = match block_on(ed::level_zone_stats(game.clone(), h())) {
            Ok(v) => v,
            Err(e) => return c.err(e),
        };
        c.ok(s.total_zones > 5000, format!("only {} zones", s.total_zones));
        c.ok(s.regions > 100, format!("only {} regions", s.regions));
        c.ok(s.with_objects > 0, "no zone in the build carries objects");
        c.ok(
            s.with_objects < s.total_zones,
            "every zone carries objects, which cannot be right",
        );
        c.note(format!(
            "{} zones in {} regions, {} with objects ({:.1}%)",
            s.total_zones, s.regions, s.with_objects,
            100.0 * s.with_objects as f64 / s.total_zones as f64
        ));
    });

    println!(
        "\n{} commands checked: {} PASS, {} FAIL in {:?} (wall {:?})",
        r.pass + r.fail,
        r.pass,
        r.fail,
        r.total,
        t_all.elapsed()
    );
    if !r.failed.is_empty() {
        println!("failed: {}", r.failed.join(", "));
    }
    if r.fail > 0 {
        std::process::exit(1);
    }
}
