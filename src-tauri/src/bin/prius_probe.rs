//! Verification harness for `core::prius` + `core::ddl_schema` against the
//! shipped PC build.
//!
//!   cargo run --release --bin prius_probe -- <game dir> <mode> [arg] [arg2]
//!
//! Modes:
//!   survey            decode every actor prius blob of every zone. Census of
//!                     row invariants, DDL types, encoding words, name sources,
//!                     schema hit rate, and byte-identical re-serialisation.
//!   components [n]    the commonest component priuses and their schema status
//!   show <zone> [substr]   decoded property sheets for one zone's actors
//!   edit <zone>       patch a float / a bool / a same-length enum in place and
//!                     prove an untouched blob is byte-identical
//!   script            the Zone Script Priuses side
//!
//! The type database is optional: point `OMNITOOL_DDL_SCHEMA` at
//! `rcra_ddl.json`. Without it the decoder still runs on pool-resolved names.

use std::collections::BTreeMap;

use omnitool_lib::core::ddl_schema::SchemaDb;
use omnitool_lib::core::level;
use omnitool_lib::core::prius::{
    self, Edit, NameCtx, PObject, PValue, PriusBlob, PriusTable, TAG_SCRIPT_PRIUSES,
};
use omnitool_lib::core::zone::Zone;

const HASHES: &str = r"E:\RCRALevelEditorViewer\refs\RCRA-Forge\hashes.txt";
const DEFAULT_SCHEMA: &str = r"E:\RCRALevelEditorViewer\refs\rivet\docs\ddl\rcra_ddl.json";
/// `Zone Script Strings`: a `u32[]` of string offsets the action rows index.
const TAG_SCRIPT_STRINGS: u32 = 0xEF86_37D5;

fn schema() -> Option<SchemaDb> {
    let p = std::env::var("OMNITOOL_DDL_SCHEMA").unwrap_or_else(|_| DEFAULT_SCHEMA.into());
    let path = std::path::PathBuf::from(&p);
    if !path.is_file() {
        eprintln!("no type database at {p} — decoding on pool names only");
        return None;
    }
    match SchemaDb::load(&path) {
        Ok(db) => {
            println!(
                "schema: {} types, {} enums, {} names indexed",
                db.type_count(),
                db.enum_count(),
                db.name_index.len()
            );
            Some(db)
        }
        Err(e) => {
            eprintln!("schema load failed: {e}");
            None
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let game = args
        .next()
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let mode = args.next().unwrap_or_else(|| "survey".into());
    let arg = args.next().unwrap_or_default();
    let arg2 = args.next().unwrap_or_default();

    let idx = level::get_index(&game, Some(HASHES)).expect("index");
    idx.require_names().expect("hashes");
    let db = schema();

    match mode.as_str() {
        "survey" => survey(&idx, db.as_ref()),
        "components" => components(&idx, db.as_ref(), arg.parse().unwrap_or(40)),
        "show" => show(&idx, db.as_ref(), &arg, &arg2),
        "edit" => edit(&idx, db.as_ref(), &arg),
        "script" => script(&idx, db.as_ref()),
        "uid" => uid_probe(&idx),
        "bycomp" => bycomp(&idx, db.as_ref(), &arg, arg2.parse().unwrap_or(1)),
        _ => eprintln!("unknown mode {mode}"),
    }
}

// ---------------------------------------------------------------------------

#[derive(Default)]
struct Stats {
    zones: usize,
    zones_with: usize,
    rows: usize,
    blob_ok: usize,
    blob_span_bad: usize,
    decode_fail: usize,
    decode_errs: BTreeMap<String, usize>,
    roundtrip_ok: usize,
    roundtrip_bad: usize,
    size_mismatch: usize,
    declared_size_ok: usize,

    row_reserved_nonzero: usize,
    row_tail_not_minus1: usize,
    row_name_hash_bad: usize,
    row_type_uid_bad: usize,
    row_uid_unplaced: usize,

    fields: usize,
    low_nibble_nonzero: usize,
    enc_type: BTreeMap<u8, usize>,
    ddl_types: BTreeMap<u8, usize>,
    name_src: BTreeMap<&'static str, usize>,
    magic: BTreeMap<u32, usize>,
    counts_gt1: usize,
    max_count: u32,
    struct_depth: usize,

    schema_typed: usize,
    schema_untyped: BTreeMap<String, usize>,
    schema_field_known: usize,
    schema_field_unknown: usize,
    unknown_field_names: BTreeMap<String, usize>,

    edit_class: BTreeMap<&'static str, usize>,
    /// For every field whose *wire* type is `string`, what the schema calls it.
    wire_string_schema: BTreeMap<String, usize>,
    /// `schema type -> wire type -> count`. The builder narrows numbers.
    cross: BTreeMap<(u8, u8), usize>,
    /// For schema-float fields stored as an integer: is the value integral?
    narrowed_integral: usize,
    narrowed_fractional: usize,
    tuids: usize,
    tuid_in_toc: usize,
    tuid_zero: usize,
    tuid_actor_same_zone: usize,
    tuid_scene_same_zone: usize,
    tuid_actor_any_zone: usize,
    tuid_unresolved: usize,
    tuid_values: usize,
    /// The names of `tuid` fields, to show what they reference.
    tuid_names: BTreeMap<String, usize>,
    type_uid_bad_names: BTreeMap<String, usize>,

    actors_with_prius: usize,
    actor_slice_uid_ok: usize,
    actor_slice_uid_bad: usize,
    orphan_rows: usize,
    identity_ok: usize,
    identity_bad: usize,
    identity_refused: usize,
    /// Stored DDLString hash vs what we would recompute.
    str_values: usize,
    str_hash_exact: usize,
    str_hash_lower: usize,
    str_hash_zero: usize,
    str_hash_other: usize,
    str_ck_exact: usize,
    str_ck_lower: usize,
    str_ck_zero: usize,
    str_ck_other: usize,
    str_mismatch_samples: Vec<String>,
    ck_raw_only: Vec<String>,
    zero_hash_samples: Vec<String>,
    zero_hash_empty: usize,
    toc: std::collections::HashSet<u64>,
    /// Every actor uid and scene-object instance id in the whole game.
    all_uids: std::collections::HashSet<u64>,
    /// The current zone's own uids, set per zone.
    zone_actor_uids: std::collections::HashSet<u64>,
    zone_scene_ids: std::collections::HashSet<u64>,
    empty_bytes: BTreeMap<u8, usize>,
    empty_counts: BTreeMap<u32, usize>,
}

fn edit_name(e: Edit) -> &'static str {
    match e {
        Edit::Scalar => "scalar",
        Edit::SelectSameLen => "select",
        Edit::StringSameLen => "string",
        Edit::Reference => "reference",
        Edit::NotEditable => "not-editable",
    }
}

fn walk(obj: &PObject, s: &mut Stats, db: Option<&SchemaDb>, depth: usize) {
    *s.magic.entry(obj.magic).or_default() += 1;
    s.struct_depth = s.struct_depth.max(depth);
    let known = db.and_then(|d| d.ty(obj.type_id));
    for f in &obj.fields {
        s.fields += 1;
        *s.enc_type.entry(f.encoding_type).or_default() += 1;
        *s.ddl_types.entry(f.ddl_type).or_default() += 1;
        *s.name_src.entry(f.name_source).or_default() += 1;
        *s.edit_class.entry(edit_name(f.edit)).or_default() += 1;
        if f.count > 1 {
            s.counts_gt1 += 1;
        }
        s.max_count = s.max_count.max(f.count);
        if let Some(st) = f.schema_type {
            *s.cross.entry((st, f.ddl_type)).or_default() += 1;
            if st == prius::DT_F32 && f.ddl_type != prius::DT_F32 {
                for v in &f.values {
                    match v.as_f64() {
                        Some(x) if x.fract() == 0.0 => s.narrowed_integral += 1,
                        Some(_) => s.narrowed_fractional += 1,
                        None => {}
                    }
                }
            }
        }
        if f.ddl_type == prius::DT_EMPTY_ARRAY {
            *s.empty_counts.entry(f.count).or_default() += 1;
            for v in &f.values {
                if let PValue::EmptyArray(b) = v {
                    *s.empty_bytes.entry(*b).or_default() += 1;
                }
            }
        }
        for v in &f.values {
            if let PValue::Str { value, hash, checksum } = v {
                s.str_values += 1;
                let h = omnitool_lib::core::crc32::hash(value);
                let hl = omnitool_lib::core::crc32::hash(&value.to_lowercase());
                if *hash == h {
                    s.str_hash_exact += 1;
                } else if *hash == hl {
                    s.str_hash_lower += 1;
                } else if *hash == 0 {
                    s.str_hash_zero += 1;
                } else {
                    s.str_hash_other += 1;
                    if s.str_mismatch_samples.len() < 8 {
                        s.str_mismatch_samples.push(format!(
                            "{}.{} = {value:?}  stored hash {hash:#010X}  crc32 {h:#010X}  crc32(lower) {hl:#010X}",
                            obj.type_name.clone().unwrap_or_default(),
                            f.name
                        ));
                    }
                }
                let c = omnitool_lib::core::crc64::hash_raw(value.as_bytes());
                let cl = omnitool_lib::core::crc64::hash(value);
                if *checksum == cl {
                    s.str_ck_lower += 1;
                } else if *checksum == c {
                    s.str_ck_exact += 1;
                    if s.ck_raw_only.len() < 8 {
                        s.ck_raw_only.push(format!("{:?}", value));
                    }
                } else if *checksum == 0 {
                    s.str_ck_zero += 1;
                } else {
                    s.str_ck_other += 1;
                }
                if *hash == 0 || *checksum == 0 {
                    if s.zero_hash_samples.len() < 8 {
                        s.zero_hash_samples.push(format!(
                            "{:?} (len {}) h={hash:#X} ck={checksum:#X}",
                            value,
                            value.len()
                        ));
                    }
                    if value.is_empty() {
                        s.zero_hash_empty += 1;
                    }
                }
            }
        }
        if f.ddl_type == prius::DT_STRING {
            let k = match f.schema_type {
                Some(t) => prius::type_label(t).to_string(),
                None => "<not in schema>".to_string(),
            };
            *s.wire_string_schema.entry(k).or_default() += 1;
        }
        if f.ddl_type == prius::DT_TUID {
            s.tuids += 1;
            *s.tuid_names.entry(f.name.clone()).or_default() += 1;
            for v in &f.values {
                if let PValue::Tuid(x) = v {
                    s.tuid_values += 1;
                    if *x == 0 {
                        s.tuid_zero += 1;
                    } else {
                        if s.toc.contains(x) {
                            s.tuid_in_toc += 1;
                        }
                        let a = s.zone_actor_uids.contains(x);
                        let c = s.zone_scene_ids.contains(x);
                        if a {
                            s.tuid_actor_same_zone += 1;
                        }
                        if c {
                            s.tuid_scene_same_zone += 1;
                        }
                        if !a && !c {
                            if s.all_uids.contains(x) {
                                s.tuid_actor_any_zone += 1;
                            } else {
                                s.tuid_unresolved += 1;
                            }
                        }
                    }
                }
            }
        }
        if let Some(t) = known {
            if t.field(f.id).is_some() {
                s.schema_field_known += 1;
            } else {
                s.schema_field_unknown += 1;
                *s.unknown_field_names
                    .entry(format!("{}.{}", t.name, f.name))
                    .or_default() += 1;
            }
        }
        for v in &f.values {
            if let PValue::Struct(o) = v {
                walk(o, s, db, depth + 1);
            }
        }
    }
}

fn survey(idx: &level::LevelIndex, db: Option<&SchemaDb>) {
    let mut s = Stats::default();
    s.toc = idx.assets.keys().copied().collect();
    // Pre-pass: every actor uid and scene-object instance id in the game, so a
    // tuid that points outside its own zone can still be recognised.
    for z in &idx.zones {
        let Ok(raw) = idx.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&raw) else { continue };
        for a in &zone.actors {
            s.all_uids.insert(a.instance_id);
        }
        for o in &zone.scene_objects {
            s.all_uids.insert(o.instance_id);
        }
    }
    println!("pre-pass: {} distinct actor/scene uids in the game", s.all_uids.len());
    for z in &idx.zones {
        let Ok(raw) = idx.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&raw) else { continue };
        s.zones += 1;
        let table = PriusTable::from_dat1(&zone.dat1);
        if table.rows.is_empty() {
            continue;
        }
        s.zones_with += 1;
        s.zone_actor_uids = zone.actors.iter().map(|a| a.instance_id).collect();
        s.zone_scene_ids = zone.scene_objects.iter().map(|o| o.instance_id).collect();
        let ctx = NameCtx::new(Some(&zone.dat1), db);

        for r in &table.rows {
            s.rows += 1;
            if r.reserved != 0 {
                s.row_reserved_nonzero += 1;
            }
            if r.tail != -1 {
                s.row_tail_not_minus1 += 1;
            }
            if !r.name_hash_ok() {
                s.row_name_hash_bad += 1;
            }
            if !r.type_uid_ok() {
                s.row_type_uid_bad += 1;
                *s.type_uid_bad_names
                    .entry(format!("{} ({:#018X} vs {:#018X})", r.component, r.type_uid, r.expected_type_uid()))
                    .or_default() += 1;
            }
            if !omnitool_lib::core::zone::is_placed(r.type_uid) {
                s.row_uid_unplaced += 1;
            }
        }

        // Actor slices own every row exactly once, and every row in a slice
        // carries the owning actor's uid.
        let mut owned = vec![false; table.rows.len()];
        for a in &zone.actors {
            if a.prius_count <= 0 {
                continue;
            }
            s.actors_with_prius += 1;
            let lo = a.prius_index.max(0) as usize;
            let hi = (lo + a.prius_count as usize).min(table.rows.len());
            let mut ok = true;
            for i in lo..hi {
                owned[i] = true;
                if table.rows[i].type_uid != a.instance_id {
                    ok = false;
                }
            }
            if ok {
                s.actor_slice_uid_ok += 1;
            } else {
                s.actor_slice_uid_bad += 1;
            }
        }
        s.orphan_rows += owned.iter().filter(|o| !**o).count();

        for i in 0..table.rows.len() {
            let Some(blob) = table.blob(i) else {
                s.blob_span_bad += 1;
                continue;
            };
            // Low-nibble check on raw bytes, so a decode failure still counts.
            let fc = u32::from_le_bytes(blob[8..12].try_into().unwrap()) as usize;
            if 16 + fc * 8 <= blob.len() {
                for k in 0..fc {
                    let at = 16 + k * 8 + 4;
                    let enc = u32::from_le_bytes(blob[at..at + 4].try_into().unwrap());
                    if enc & 0xF != 0 {
                        s.low_nibble_nonzero += 1;
                    }
                }
            }

            let comp = table.rows[i].component.clone();
            match PriusBlob::new(blob, &comp, table.rows[i].type_uid, &ctx, db) {
                Ok(b) => {
                    s.blob_ok += 1;
                    if b.tree.span.1 == blob.len() {
                        s.declared_size_ok += 1;
                    } else {
                        s.size_mismatch += 1;
                    }
                    if prius::reserialize(&b.tree) == blob {
                        s.roundtrip_ok += 1;
                    } else {
                        s.roundtrip_bad += 1;
                    }
                    if let Some(d) = db {
                        match d.prius_of_component(&comp) {
                            Some(_) => s.schema_typed += 1,
                            None => *s.schema_untyped.entry(comp.clone()).or_default() += 1,
                        }
                    }
                    walk(&b.tree, &mut s, db, 0);

                    // Every editable leaf, re-set to the value it already has,
                    // must reproduce the original bytes. This exercises the
                    // whole patch path (numeric coercion, DDLString rewrite with
                    // fresh crc32/crc64, padding) against real data.
                    let mut leaves = Vec::new();
                    collect(&b.tree, "", &mut leaves);
                    for (path, f) in &leaves {
                        let same = match f.edit {
                            Edit::Scalar => f
                                .values
                                .first()
                                .and_then(|v| v.as_f64())
                                .and_then(|x| prius::coerce_number(f.ddl_type, x).ok()),
                            Edit::SelectSameLen | Edit::StringSameLen => {
                                f.values.first().cloned()
                            }
                            _ => None,
                        };
                        let Some(v) = same else { continue };
                        let mut c = PriusBlob::new(blob, &comp, table.rows[i].type_uid, &ctx, db).unwrap();
                        match c.patch(path, &v) {
                            Ok(()) => {
                                if c.to_bytes() == blob {
                                    s.identity_ok += 1;
                                } else {
                                    s.identity_bad += 1;
                                }
                            }
                            Err(_) => s.identity_refused += 1,
                        }
                    }
                }
                Err(e) => {
                    s.decode_fail += 1;
                    let key = e.to_string();
                    let key = key.split(" at ").next().unwrap_or(&key).to_string();
                    *s.decode_errs.entry(key).or_default() += 1;
                }
            }
        }
    }

    println!("\n=== rows and blobs");
    println!("zones parsed              {}", s.zones);
    println!("zones with actor priuses  {}", s.zones_with);
    println!("prius rows                {}", s.rows);
    println!("  blob span out of pool   {}", s.blob_span_bad);
    println!("  decoded                 {}  ({:.4}%)", s.blob_ok, pct(s.blob_ok, s.rows));
    println!("  decode failures         {}", s.decode_fail);
    for (k, v) in &s.decode_errs {
        println!("      {v:6}  {k}");
    }
    println!(
        "  consumed exactly data_size {}   short/long {}",
        s.declared_size_ok, s.size_mismatch
    );
    println!(
        "  re-serialised byte-identical {}  ({:.4}%)   differing {}",
        s.roundtrip_ok,
        pct(s.roundtrip_ok, s.blob_ok),
        s.roundtrip_bad
    );

    println!(
        "  every editable leaf re-set to its own value reproduced the bytes: {}   differed {}   refused {}",
        s.identity_ok, s.identity_bad, s.identity_refused
    );

    println!("\n=== DDLString hash/checksum provenance ({} values)", s.str_values);
    println!(
        "  crc32 field: == crc32(value) {}   == crc32(lower) {}   zero {}   other {}",
        s.str_hash_exact, s.str_hash_lower, s.str_hash_zero, s.str_hash_other
    );
    println!(
        "  crc64 field: == crc64::hash(value) (normalised) {}   only == crc64_raw(literal) {}   zero {}   other {}",
        s.str_ck_lower, s.str_ck_exact, s.str_ck_zero, s.str_ck_other
    );
    for x in &s.ck_raw_only {
        println!("    literal-only checksum: {x}");
    }
    println!("  values with a zero hash: {} of which empty strings {}", s.str_hash_zero, s.zero_hash_empty);
    for x in &s.zero_hash_samples {
        println!("    {x}");
    }
    for x in &s.str_mismatch_samples {
        println!("    {x}");
    }

    println!("\n=== row invariants");
    println!("+0x10 != 0                {}", s.row_reserved_nonzero);
    println!("+0x1C != -1               {}", s.row_tail_not_minus1);
    println!("crc32(component) != hash  {}", s.row_name_hash_bad);
    println!("+0x00 != crc64(lower(component+\"Prius\"))  {}", s.row_type_uid_bad);
    println!("+0x00 without the placed bit {}  (it is a type hash, not a uid)", s.row_uid_unplaced);
    println!("actors carrying a prius   {}", s.actors_with_prius);
    println!(
        "  actors whose slice rows all carry the actor's own uid: {}  (expected 0 -- +0x00 is a type id)",
        s.actor_slice_uid_ok
    );
    println!("rows owned by no actor     {}", s.orphan_rows);

    println!("\n=== wire format");
    println!("fields decoded            {}", s.fields);
    println!("encoding low nibble != 0  {}", s.low_nibble_nonzero);
    print!("encoding_type:");
    for (k, v) in &s.enc_type {
        print!("  {k}={v}");
    }
    println!();
    print!("magic:");
    for (k, v) in &s.magic {
        print!("  {k:#010X}={v}");
    }
    println!();
    println!("fields with count > 1     {}  (max count {})", s.counts_gt1, s.max_count);
    println!("deepest nested struct     {}", s.struct_depth);
    println!("--- DDL type census");
    let mut tv: Vec<_> = s.ddl_types.iter().collect();
    tv.sort_by(|a, b| b.1.cmp(a.1));
    for (t, n) in tv {
        println!("  {:>12}  {n:8}  ({:5.2}%)", prius::type_label(*t), pct(*n, s.fields));
    }

    println!("\n=== where field names came from");
    for (k, v) in &s.name_src {
        println!("  {k:8} {v:8}  ({:.3}%)", pct(*v, s.fields));
    }

    println!("\n=== schema coverage");
    println!(
        "blobs whose component resolved to a DDL type {}  ({:.3}%)",
        s.schema_typed,
        pct(s.schema_typed, s.blob_ok)
    );
    let mut uv: Vec<_> = s.schema_untyped.iter().collect();
    uv.sort_by(|a, b| b.1.cmp(a.1));
    println!("components with no type in the database: {}", uv.len());
    for (n, c) in uv.iter().take(20) {
        println!("  {c:6}  {n}");
    }
    println!(
        "root fields present in the resolved type {}  absent {}  ({:.2}% known)",
        s.schema_field_known,
        s.schema_field_unknown,
        pct(s.schema_field_known, s.schema_field_known + s.schema_field_unknown)
    );
    let mut fv: Vec<_> = s.unknown_field_names.iter().collect();
    fv.sort_by(|a, b| b.1.cmp(a.1));
    for (n, c) in fv.iter().take(15) {
        println!("  {c:6}  {n}");
    }

    println!("\n=== the wire erases `select`: what the schema calls a wire `string`");
    let wire_strings = s.ddl_types.get(&prius::DT_STRING).copied().unwrap_or(0);
    let mut ws: Vec<_> = s.wire_string_schema.iter().collect();
    ws.sort_by(|a, b| b.1.cmp(a.1));
    for (k, v) in ws {
        println!("  {k:16} {v:8}  ({:5.2}% of wire strings)", pct(*v, wire_strings));
    }

    println!(
        "\n=== type-19 empty arrays: declared count -> {:?}, payload byte -> {:?}",
        s.empty_counts, s.empty_bytes
    );

    println!("\n=== schema type x wire type (the builder narrows numbers)");
    let mut byschema: BTreeMap<u8, Vec<(u8, usize)>> = BTreeMap::new();
    for ((st, wt), n) in &s.cross {
        byschema.entry(*st).or_default().push((*wt, *n));
    }
    for (st, mut v) in byschema {
        v.sort_by(|a, b| b.1.cmp(&a.1));
        let total: usize = v.iter().map(|(_, n)| n).sum();
        let cells: Vec<String> = v
            .iter()
            .map(|(wt, n)| format!("{}={} ({:.1}%)", prius::type_label(*wt), n, pct(*n, total)))
            .collect();
        println!("  schema {:>12} ({total:6}) -> {}", prius::type_label(st), cells.join("  "));
    }
    println!(
        "  schema-float values stored as an integer: {} integral, {} fractional",
        s.narrowed_integral, s.narrowed_fractional
    );

    println!("\n=== tuid fields");
    println!(
        "tuid fields {}  values {}   value 0 {}   value is a TOC asset id {}",
        s.tuids, s.tuid_values, s.tuid_zero, s.tuid_in_toc
    );
    println!(
        "  resolves to an actor uid in the SAME zone {}   to a scene object in the same zone {}",
        s.tuid_actor_same_zone, s.tuid_scene_same_zone
    );
    println!(
        "  resolves only somewhere else in the game {}   unresolved {}",
        s.tuid_actor_any_zone, s.tuid_unresolved
    );
    let mut tn: Vec<_> = s.tuid_names.iter().collect();
    tn.sort_by(|a, b| b.1.cmp(a.1));
    for (n, c) in tn.iter().take(12) {
        println!("  {c:6}  {n}");
    }

    println!("\n=== rows whose +0x00 is not crc64(lower(component+\"Prius\"))");
    let mut bv: Vec<_> = s.type_uid_bad_names.iter().collect();
    bv.sort_by(|a, b| b.1.cmp(a.1));
    for (n, c) in bv.iter().take(15) {
        println!("  {c:6}  {n}");
    }

    println!("\n=== what an editor may offer");
    let mut ev: Vec<_> = s.edit_class.iter().collect();
    ev.sort_by(|a, b| b.1.cmp(a.1));
    for (k, v) in ev {
        println!("  {k:14} {v:8}  ({:5.2}%)", pct(*v, s.fields));
    }
}

fn pct(a: usize, b: usize) -> f64 {
    if b == 0 {
        0.0
    } else {
        a as f64 * 100.0 / b as f64
    }
}

// ---------------------------------------------------------------------------

fn components(idx: &level::LevelIndex, db: Option<&SchemaDb>, n: usize) {
    let mut count: BTreeMap<String, usize> = BTreeMap::new();
    let mut fields: BTreeMap<String, usize> = BTreeMap::new();
    for z in &idx.zones {
        let Ok(raw) = idx.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&raw) else { continue };
        let table = PriusTable::from_dat1(&zone.dat1);
        let ctx = NameCtx::new(Some(&zone.dat1), db);
        for i in 0..table.rows.len() {
            let comp = table.rows[i].component.clone();
            *count.entry(comp.clone()).or_default() += 1;
            if let Some(blob) = table.blob(i) {
                if let Ok(b) = PriusBlob::new(blob, &comp, table.rows[i].type_uid, &ctx, db) {
                    let (_, t) = prius::count_named(&b.tree);
                    *fields.entry(comp).or_default() += t;
                }
            }
        }
    }
    let mut v: Vec<_> = count.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    println!("\n{:>7}  {:>6}  {:<38} {}", "rows", "flds/r", "component", "DDL type");
    for (c, n_) in v.iter().take(n) {
        let ty = db
            .and_then(|d| d.prius_of_component(c))
            .map(|t| format!("{} ({} declared)", t.name, t.fields.len()))
            .unwrap_or_else(|| "-- not in database --".into());
        let f = *fields.get(c).unwrap_or(&0) as f64 / *n_ as f64;
        println!("{n_:7}  {f:6.1}  {c:<38} {ty}");
    }
}

// ---------------------------------------------------------------------------

fn print_tree(obj: &PObject, indent: usize, db: Option<&SchemaDb>) {
    for f in &obj.fields {
        let pad = " ".repeat(indent);
        let label = f.label.as_deref().map(|l| format!("  \"{l}\"")).unwrap_or_default();
        let dflt = f.default.as_ref().map(|d| format!("   was {d}")).unwrap_or_default();
        let src = if f.name_source == "pool" { "" } else { "*" };
        if f.ddl_type == prius::DT_STRUCT {
            println!("{pad}{}{src} : struct x{}{label}", f.name, f.values.len());
            for v in &f.values {
                if let PValue::Struct(o) = v {
                    println!("{pad}  ({})", o.type_name.clone().unwrap_or_else(|| "?".into()));
                    print_tree(o, indent + 4, db);
                }
            }
            continue;
        }
        let vals: Vec<String> = f.values.iter().map(|v| v.text()).collect();
        let shown = if f.count == 1 {
            vals.first().cloned().unwrap_or_default()
        } else {
            format!("[{}]", vals.join(", "))
        };
        let opts = match (f.enum_values.as_ref(), f.values.first()) {
            (Some(_), Some(PValue::Str { value, .. })) => {
                let e = db
                    .and_then(|d| d.ty(obj.type_id))
                    .and_then(|t| t.field(f.id))
                    .and_then(|fd| db.and_then(|d| d.enum_def(fd.type_id)));
                match e {
                    Some(e) => {
                        let (fits, _) = prius::select_options(e, value);
                        format!(
                            "   [{} members, {} swap in place: {}]",
                            e.values.len(),
                            fits.len(),
                            fits.join("|")
                        )
                    }
                    None => String::new(),
                }
            }
            _ => String::new(),
        };
        println!(
            "{pad}{}{src} : {} = {shown}{label}{dflt}   <{}>{opts}",
            f.name,
            prius::type_label(f.ddl_type),
            edit_name(f.edit)
        );
    }
}

fn show(idx: &level::LevelIndex, db: Option<&SchemaDb>, zone_path: &str, filter: &str) {
    let Some(rec) = idx.zone(zone_path) else {
        eprintln!("no such zone {zone_path}");
        return;
    };
    let raw = idx.extract(rec.id).expect("extract");
    let zone = Zone::parse(&raw).expect("parse");
    let table = PriusTable::from_dat1(&zone.dat1);
    println!(
        "\n{zone_path}\n  {} actors, {} prius rows, {} pool bytes",
        zone.actors.len(),
        table.rows.len(),
        table.pool.len()
    );

    let mut shown = 0usize;
    for a in &zone.actors {
        if a.prius_count <= 0 {
            continue;
        }
        let name = a.name.clone();
        if !filter.is_empty() && !name.to_lowercase().contains(&filter.to_lowercase()) {
            continue;
        }
        if shown >= 12 {
            break;
        }
        shown += 1;
        println!(
            "\n--- actor {:?}  uid {:#018X}  flags {:#06X}  {} priuses",
            name, a.instance_id, a.flags, a.prius_count
        );
        let ap = prius::actor_priuses(&table, &zone.dat1, db, a.instance_id, a.prius_index, a.prius_count);
        for b in &ap.blobs {
            let (named, total) = prius::count_named(&b.tree);
            println!(
                "  [{}]  type {}  {}/{} fields named",
                b.component,
                b.tree
                    .type_name
                    .clone()
                    .unwrap_or_else(|| format!("{:#010X} (unknown)", b.tree.type_id)),
                named,
                total
            );
            print_tree(&b.tree, 4, db);
        }
        for (i, e) in &ap.failures {
            println!("  row {i} FAILED: {e}");
        }
    }
}

// ---------------------------------------------------------------------------

fn collect(obj: &PObject, prefix: &str, out: &mut Vec<(String, prius::PField)>) {
    for f in &obj.fields {
        let base = if prefix.is_empty() { f.name.clone() } else { format!("{prefix}.{}", f.name) };
        if f.ddl_type == prius::DT_STRUCT {
            for (k, v) in f.values.iter().enumerate() {
                if let PValue::Struct(o) = v {
                    let p = if f.count > 1 { format!("{base}[{k}]") } else { base.clone() };
                    collect(o, &p, out);
                }
            }
            continue;
        }
        out.push((base, f.clone()));
    }
}

fn leaf(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

fn edit(idx: &level::LevelIndex, db: Option<&SchemaDb>, zone_path: &str) {
    let Some(rec) = idx.zone(zone_path) else {
        eprintln!("no such zone {zone_path}");
        return;
    };
    let raw = idx.extract(rec.id).expect("extract");
    let zone = Zone::parse(&raw).expect("parse");
    let table = PriusTable::from_dat1(&zone.dat1);
    let ctx = NameCtx::new(Some(&zone.dat1), db);

    let mut done: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
    let mut identical = 0usize;
    let mut total = 0usize;
    let mut patched_ok = 0usize;
    let mut patch_refused = 0usize;

    for i in 0..table.rows.len() {
        let Some(blob) = table.blob(i) else { continue };
        let comp = table.rows[i].component.clone();
        let tu = table.rows[i].type_uid;
        let Ok(b) = PriusBlob::new(blob, &comp, tu, &ctx, db) else { continue };
        total += 1;
        if b.to_bytes() == blob && !b.is_dirty() {
            identical += 1;
        }
        let mut leaves = Vec::new();
        collect(&b.tree, "", &mut leaves);

        // Exhaustive sanity check: re-patching every scalar leaf with its own
        // current value must reproduce the original bytes exactly.
        for (path, f) in &leaves {
            if f.edit != Edit::Scalar {
                continue;
            }
            if let Some(x) = f.values.first().and_then(|v| v.as_f64()) {
                let mut c = PriusBlob::new(blob, &comp, tu, &ctx, db).unwrap();
                match c.patch_number(path, x) {
                    Ok(()) => {
                        if c.to_bytes() == blob {
                            patched_ok += 1;
                        } else {
                            println!("  !! identity patch changed bytes: {comp}.{path}");
                        }
                    }
                    Err(_) => patch_refused += 1,
                }
            }
        }

        for (path, f) in &leaves {
            let st = f.schema_type.map(prius::type_label).unwrap_or("-");
            match f.edit {
                Edit::Scalar if f.ddl_type == prius::DT_U8 && done.insert("u8") => {
                    let v = f.values[0].as_f64().unwrap_or(0.0);
                    let mut c = PriusBlob::new(blob, &comp, tu, &ctx, db).unwrap();
                    let ok = c.patch_number(path, v + 1.0);
                    let frac = PriusBlob::new(blob, &comp, tu, &ctx, db).unwrap().patch_number(path, v + 0.5);
                    let over = PriusBlob::new(blob, &comp, tu, &ctx, db).unwrap().patch_number(path, 4096.0);
                    println!("u8      {comp}.{path} (schema {st}) = {v}");
                    println!("          +1   -> {}   bytes changed {}", if ok.is_ok() { "ok" } else { "REFUSED" }, diff_count(blob, c.to_bytes()));
                    println!("          +0.5 -> {}", err_text(&frac));
                    println!("          4096 -> {}", err_text(&over));
                }
                Edit::Scalar if f.ddl_type == prius::DT_F32 && done.insert("f32") => {
                    let v = f.values[0].as_f64().unwrap_or(0.0);
                    let mut c = PriusBlob::new(blob, &comp, tu, &ctx, db).unwrap();
                    c.patch_number(path, v + 1.5).expect("patch f32");
                    let re = PriusBlob::new(c.to_bytes(), &comp, tu, &ctx, db).unwrap();
                    println!(
                        "f32     {comp}.{path} (schema {st}): {v} -> {}   len {} -> {}   bytes changed {}",
                        re.tree.field_named(leaf(path)).map(|x| x.values[0].text()).unwrap_or_default(),
                        blob.len(), c.to_bytes().len(), diff_count(blob, c.to_bytes())
                    );
                }
                Edit::Scalar if f.ddl_type == prius::DT_BOOL && done.insert("bool") => {
                    let v = matches!(f.values[0], PValue::Bool(true));
                    let mut c = PriusBlob::new(blob, &comp, tu, &ctx, db).unwrap();
                    c.patch(path, &PValue::Bool(!v)).expect("patch bool");
                    println!(
                        "bool    {comp}.{path}: {v} -> {}   len {} -> {}   bytes changed {}",
                        !v, blob.len(), c.to_bytes().len(), diff_count(blob, c.to_bytes())
                    );
                }
                Edit::Reference if done.insert("ref") => {
                    let mut c = PriusBlob::new(blob, &comp, tu, &ctx, db).unwrap();
                    let r = c.patch(path, &PValue::Tuid(1));
                    println!("ref     {comp}.{path} ({}, schema {st}) refused: {}", prius::type_label(f.ddl_type), err_text(&r));
                }
                Edit::SelectSameLen => {
                    let cur = f.values[0].text();
                    let opts = f.enum_values.clone().unwrap_or_default();
                    let same: Vec<&String> = opts.iter().filter(|m| prius::slot_len(m.len()) == prius::slot_len(cur.len()) && **m != cur).collect();
                    let diff: Vec<&String> = opts.iter().filter(|m| prius::slot_len(m.len()) != prius::slot_len(cur.len())).collect();
                    if let Some(alt) = same.first() {
                        if done.insert("select") {
                            let mut c = PriusBlob::new(blob, &comp, tu, &ctx, db).unwrap();
                            c.patch(path, &str_val(alt)).expect("patch select");
                            let re = PriusBlob::new(c.to_bytes(), &comp, tu, &ctx, db).unwrap();
                            let back = re.tree.field_named(leaf(path)).map(|x| x.values[0].text());
                            println!(
                                "select  {comp}.{path}: {cur:?} -> {alt:?} (read back {back:?})   len {} -> {}   bytes changed {}   {} of {} members fit",
                                blob.len(), c.to_bytes().len(), diff_count(blob, c.to_bytes()), same.len() + 1, opts.len()
                            );
                        }
                    }
                    if let Some(bad) = diff.first() {
                        if done.insert("select-reject") {
                            let mut c = PriusBlob::new(blob, &comp, tu, &ctx, db).unwrap();
                            let r = c.patch(path, &str_val(bad));
                            println!("reject  {comp}.{path} -> {bad:?}: {}   blob untouched {}", err_text(&r), c.to_bytes() == blob);
                        }
                    }
                }
                Edit::StringSameLen if done.insert("string") => {
                    let cur = f.values[0].text();
                    let same: String = cur.chars().rev().collect();
                    let mut c = PriusBlob::new(blob, &comp, tu, &ctx, db).unwrap();
                    let r = c.patch(path, &str_val(&same));
                    println!(
                        "string  {comp}.{path}: {cur:?} -> {same:?} ({})   len {} -> {}",
                        if r.is_ok() { "ok" } else { "refused" }, blob.len(), c.to_bytes().len()
                    );
                    let longer = format!("{cur}_much_longer_value");
                    let r2 = PriusBlob::new(blob, &comp, tu, &ctx, db).unwrap().patch(path, &str_val(&longer));
                    println!("          lengthened -> {}", err_text(&r2));
                }
                Edit::NotEditable if done.insert("none") => {
                    let mut c = PriusBlob::new(blob, &comp, tu, &ctx, db).unwrap();
                    let r = c.patch(path, &PValue::U8(0));
                    println!("none    {comp}.{path} ({}) refused: {}", prius::type_label(f.ddl_type), err_text(&r));
                }
                _ => {}
            }
        }
    }
    println!("\nblobs byte-identical when untouched: {identical} of {total}");
    println!("identity re-patch of every scalar leaf reproduced the original bytes: {patched_ok}  (refused {patch_refused})");
}



fn str_val(s: &str) -> PValue {
    PValue::Str { value: s.to_string(), hash: 0, checksum: 0 }
}

fn err_text<T>(r: &omnitool_lib::core::error::Result<T>) -> String {
    match r {
        Ok(_) => "ACCEPTED (unexpected)".into(),
        Err(e) => e.to_string(),
    }
}

fn diff_count(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).filter(|(x, y)| x != y).count()
}

// ---------------------------------------------------------------------------

fn script(idx: &level::LevelIndex, db: Option<&SchemaDb>) {
    let mut blobs = 0usize;
    let mut ok = 0usize;
    let mut rt = 0usize;
    let mut typed = 0usize;
    let mut errs: BTreeMap<String, usize> = BTreeMap::new();
    let mut names: BTreeMap<String, usize> = BTreeMap::new();
    let mut shown = 0usize;
    let mut sfields = 0usize;
    let mut senc: BTreeMap<u8, usize> = BTreeMap::new();
    let mut smagic: BTreeMap<u32, usize> = BTreeMap::new();
    let mut stypes: BTreeMap<u8, usize> = BTreeMap::new();
    let mut snames: BTreeMap<&str, usize> = BTreeMap::new();

    for z in &idx.zones {
        let Ok(raw) = idx.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&raw) else { continue };
        let Some(pool) = zone.dat1.get_section_data(TAG_SCRIPT_PRIUSES) else { continue };
        let pool = pool.to_vec();
        let strings = zone.dat1.get_section_data(TAG_SCRIPT_STRINGS).unwrap_or(&[]).to_vec();
        let ctx = NameCtx::new(Some(&zone.dat1), db);
        for a in &zone.actions {
            if a.prius_size < 16 {
                continue;
            }
            let lo = a.prius_offset as usize;
            let hi = lo + a.prius_size as usize;
            if hi > pool.len() {
                continue;
            }
            blobs += 1;
            let i = a.type_index as usize;
            let action = if i * 4 + 4 <= strings.len() {
                let so = u32::from_le_bytes(strings[i * 4..i * 4 + 4].try_into().unwrap());
                zone.dat1.get_string(so).unwrap_or_default()
            } else {
                String::new()
            };
            *names.entry(action.clone()).or_default() += 1;
            if db.map(|d| d.prius_of_component(&action).is_some()).unwrap_or(false) {
                typed += 1;
            }
            match PriusBlob::new(&pool[lo..hi], &action, 0, &ctx, db) {
                Ok(b) => {
                    ok += 1;
                    if prius::reserialize(&b.tree) == pool[lo..hi] {
                        rt += 1;
                    }
                    let mut st = Stats::default();
                    walk(&b.tree, &mut st, db, 0);
                    sfields += st.fields;
                    for (k, v) in st.enc_type {
                        *senc.entry(k).or_default() += v;
                    }
                    for (k, v) in st.magic {
                        *smagic.entry(k).or_default() += v;
                    }
                    for (k, v) in st.ddl_types {
                        *stypes.entry(k).or_default() += v;
                    }
                    for (k, v) in st.name_src {
                        *snames.entry(k).or_default() += v;
                    }
                    if shown < 3 && b.tree.fields.len() > 3 {
                        println!("\n--- script action {action:?} ({} fields)", b.tree.fields.len());
                        print_tree(&b.tree, 4, db);
                        shown += 1;
                    }
                }
                Err(e) => {
                    let k = e.to_string();
                    *errs.entry(k.split(" at ").next().unwrap_or(&k).to_string()).or_default() += 1;
                }
            }
        }
    }
    println!(
        "\nscript prius blobs {blobs}  decoded {ok}  byte-identical round trip {rt}  action name resolved to a DDL type {typed}"
    );
    for (k, v) in &errs {
        println!("  {v:6} {k}");
    }
    println!("fields {sfields}  encoding_type {senc:?}  magic {smagic:?}");
    println!("name sources {snames:?}");
    let mut tv: Vec<_> = stypes.iter().collect();
    tv.sort_by(|a, b| b.1.cmp(a.1));
    println!("types: {}", tv.iter().map(|(t, n)| format!("{}={n}", prius::type_label(**t))).collect::<Vec<_>>().join("  "));
    let mut nv: Vec<_> = names.into_iter().collect();
    nv.sort_by(|a, b| b.1.cmp(&a.1));
    println!("--- commonest script actions with a prius");
    for (n, c) in nv.iter().take(20) {
        let ty = db
            .and_then(|d| d.prius_of_component(n))
            .map(|t| t.name.clone())
            .unwrap_or_else(|| "--".into());
        println!("  {c:6}  {n:<44} {ty}");
    }
}

// ---------------------------------------------------------------------------
// What is the u64 at +0x00 of a Zone Actor Prius row?
// ---------------------------------------------------------------------------

fn uid_probe(idx: &level::LevelIndex) {
    use std::collections::{HashMap, HashSet};
    let mut per_component: HashMap<String, HashSet<u64>> = HashMap::new();
    let mut eq_actor = 0usize;
    let mut eq_actor_low32 = 0usize;
    let mut rows = 0usize;
    let mut placed = 0usize;
    let mut crc64_component = 0usize;
    let mut samples: Vec<String> = Vec::new();
    // Does the row uid equal the uid of ANY actor / scene object in the zone?
    let mut in_actor_set = 0usize;
    let mut monotone = 0usize;
    let mut zones = 0usize;

    for z in &idx.zones {
        let Ok(raw) = idx.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&raw) else { continue };
        let table = PriusTable::from_dat1(&zone.dat1);
        if table.rows.is_empty() {
            continue;
        }
        zones += 1;
        let actor_uids: HashSet<u64> = zone.actors.iter().map(|a| a.instance_id).collect();
        let mut prev = 0u64;
        let mut mono = true;
        for (i, r) in table.rows.iter().enumerate() {
            rows += 1;
            if omnitool_lib::core::zone::is_placed(r.type_uid) {
                placed += 1;
            }
            if actor_uids.contains(&r.type_uid) {
                in_actor_set += 1;
            }
            if r.type_uid_ok() {
                crc64_component += 1;
            }
            per_component.entry(r.component.clone()).or_default().insert(r.type_uid);
            if i > 0 && r.type_uid < prev {
                mono = false;
            }
            prev = r.type_uid;
        }
        if mono {
            monotone += 1;
        }
        // Print the first zone's actor/prius pairing in detail.
        if samples.len() < 24 {
            for a in zone.actors.iter().take(40) {
                if a.prius_count <= 0 {
                    continue;
                }
                let lo = a.prius_index as usize;
                for k in lo..(lo + a.prius_count as usize).min(table.rows.len()) {
                    let r = &table.rows[k];
                    samples.push(format!(
                        "  actor {:<34} uid {:#018X}   row[{k}] {:<26} uid {:#018X}  xor {:#018X}",
                        a.name, a.instance_id, r.component, r.type_uid, a.instance_id ^ r.type_uid
                    ));
                    if samples.len() >= 24 {
                        break;
                    }
                }
                if samples.len() >= 24 {
                    break;
                }
            }
        }
        let _ = &mut eq_actor;
        let _ = &mut eq_actor_low32;
    }
    println!("rows {rows} in {zones} zones");
    println!("  placed bit set            {placed}");
    println!("  uid is some actor's uid   {in_actor_set}");
    println!("  uid == crc64(lower(component+Prius))  {crc64_component}");
    let shared: usize = per_component.values().filter(|s| s.len() == 1).count();
    for (n, set) in per_component.iter() {
        if set.len() > 1 {
            println!("  !! {n} has {} distinct type uids", set.len());
        }
    }
    println!(
        "  components whose uid is constant across every row: {shared} of {}",
        per_component.len()
    );
    println!("  zones whose row uids ascend: {monotone} of {zones}");
    println!("--- sample pairings");
    for s in samples {
        println!("{s}");
    }
}

// ---------------------------------------------------------------------------
// One decoded example of a named component, wherever it first occurs.
// ---------------------------------------------------------------------------

fn bycomp(idx: &level::LevelIndex, db: Option<&SchemaDb>, want: &str, n: usize) {
    let mut shown = 0usize;
    for z in &idx.zones {
        let Ok(raw) = idx.extract(z.id) else { continue };
        let Ok(zone) = Zone::parse(&raw) else { continue };
        let table = PriusTable::from_dat1(&zone.dat1);
        if table.rows.is_empty() {
            continue;
        }
        let ctx = NameCtx::new(Some(&zone.dat1), db);
        // Prefer the row with the most fields, so the example is informative.
        let mut best: Option<(usize, usize)> = None;
        for i in 0..table.rows.len() {
            if table.rows[i].component != want {
                continue;
            }
            let Some(blob) = table.blob(i) else { continue };
            if let Ok(b) = PriusBlob::new(blob, want, table.rows[i].type_uid, &ctx, db) {
                let (_, t) = prius::count_named(&b.tree);
                if best.map(|(_, bt)| t > bt).unwrap_or(true) {
                    best = Some((i, t));
                }
            }
        }
        let Some((i, _)) = best else { continue };
        let blob = table.blob(i).unwrap();
        let b = PriusBlob::new(blob, want, table.rows[i].type_uid, &ctx, db).unwrap();
        // Which actor owns this row?
        let owner = zone.actors.iter().find(|a| {
            a.prius_count > 0
                && (a.prius_index as usize) <= i
                && i < (a.prius_index as usize + a.prius_count as usize)
        });
        println!("\n================ {want}   {}", z.path);
        if let Some(a) = owner {
            println!(
                "actor {:?}   asset {:?}\n  uid {:#018X}  flags {:#06X} {:?}  pos {:?}",
                a.name,
                a.asset_path,
                a.instance_id,
                a.flags,
                a.flag_names,
                zone.find(a.scene_offset).map(|o| o.position())
            );
        }
        println!(
            "row[{i}]  type_uid {:#018X} -> {}   blob {} bytes at pool+{}",
            table.rows[i].type_uid,
            b.tree.type_name.clone().unwrap_or_else(|| "<unknown struct>".into()),
            table.rows[i].data_size,
            table.blob_at(i).unwrap_or(0)
        );
        print_tree(&b.tree, 2, db);
        shown += 1;
        if shown >= n {
            return;
        }
    }
    if shown == 0 {
        println!("no zone carries a {want} prius");
    }
}
