//! Tauri commands for the actor/script **prius property sheet**.
//!
//! | command                     | answers                                                |
//! |-----------------------------|--------------------------------------------------------|
//! | `prius_schema_status`       | is a DDL type database loaded, and how big is it       |
//! | `prius_load_schema`         | load one from a path the user picked                   |
//! | `prius_zone_summary`        | which component priuses a zone carries, and how many   |
//! | `prius_actor_sheet`         | every component prius of one placed actor, typed       |
//! | `prius_script_sheet`        | the prius of one script action, typed                  |
//! | `prius_plan_edit`           | validate one field edit and return the byte patch      |
//!
//! ## Why `prius_plan_edit` returns bytes instead of saving
//!
//! The zone save path only ever applies **equal-length in-place patches**, so
//! that an untouched zone re-serialises byte-identically. A prius edit fits that
//! model exactly: it rewrites a fixed byte range inside the
//! `Zone Actor Prius Data` (`0x81999057`) or `Zone Script Priuses`
//! (`0x2300D240`) section and nothing else. So this command does the typing,
//! bounds-checking and encoding, and hands back
//! `{ sectionTag, sectionOffset, bytes }` for the zone session to splice in.
//! Nothing here mutates a zone, and nothing here writes to the game directory.
//!
//! The measurements behind the validation live in [`crate::core::prius`]; the
//! short version is that over the whole shipped PC build, 50 341 of 50 341 actor
//! prius blobs and 112 886 of 112 886 script prius blobs decode and
//! re-serialise byte-identically, and 172 584 of 172 584 editable leaves re-set
//! to their own value reproduce the original bytes.

use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;

use crate::core::ddl_schema::{self, SchemaDb};
use crate::core::error::ToolkitError;
use crate::core::level::{self, LevelIndex};
use crate::core::prius::{
    self, Edit, NameCtx, PValue, PriusBlob, PriusTable, PriusView, TAG_ACTOR_PRIUS_POOL,
    TAG_SCRIPT_PRIUSES,
};
use crate::core::zone::Zone;

type Result<T> = std::result::Result<T, ToolkitError>;

// ---------------------------------------------------------------------------
// Schema handle
// ---------------------------------------------------------------------------

/// The database the UI is currently using. `None` until something loads one;
/// the decoder works without it, just without enum member sets and defaults.
fn current() -> &'static Mutex<Option<Arc<SchemaDb>>> {
    static C: OnceLock<Mutex<Option<Arc<SchemaDb>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

/// Resolve the schema once per call: an explicitly loaded one, else the first
/// of [`ddl_schema::default_paths`] that exists.
fn schema() -> Option<Arc<SchemaDb>> {
    if let Some(db) = current().lock().unwrap().clone() {
        return Some(db);
    }
    let db = ddl_schema::get_default();
    if db.is_empty() {
        return None;
    }
    *current().lock().unwrap() = Some(db.clone());
    Some(db)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaStatus {
    pub loaded: bool,
    pub source: String,
    pub types: usize,
    pub enums: usize,
    pub names: usize,
    /// Paths that would be tried, so the UI can tell the user where to put it.
    pub searched: Vec<String>,
}

fn status_of(db: Option<&SchemaDb>) -> SchemaStatus {
    let searched = ddl_schema::default_paths()
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    match db {
        Some(db) => SchemaStatus {
            loaded: !db.is_empty(),
            source: db.source.clone(),
            types: db.type_count(),
            enums: db.enum_count(),
            names: db.name_index.len(),
            searched,
        },
        None => SchemaStatus {
            loaded: false,
            source: String::new(),
            types: 0,
            enums: 0,
            names: 0,
            searched,
        },
    }
}

#[tauri::command]
pub async fn prius_schema_status() -> Result<SchemaStatus> {
    let db = schema();
    Ok(status_of(db.as_deref()))
}

#[tauri::command]
pub async fn prius_load_schema(path: String) -> Result<SchemaStatus> {
    let db = ddl_schema::get(std::path::Path::new(&path))?;
    *current().lock().unwrap() = Some(db.clone());
    Ok(status_of(Some(&db)))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn index(game_dir: &str, hashes_path: Option<String>) -> Result<Arc<LevelIndex>> {
    level::get_index(game_dir, hashes_path.as_deref())
}

fn load_zone(idx: &LevelIndex, zone_path: &str) -> Result<Zone> {
    let path = crate::core::crc64::normalize_path(zone_path);
    let id = crate::core::crc64::hash_raw(path.as_bytes());
    Ok(Zone::parse(&idx.extract(id)?)?)
}

// ---------------------------------------------------------------------------
// Zone summary
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentCount {
    pub component: String,
    pub count: usize,
    /// `crc32(component)`, as the row stores it.
    pub name_hash: String,
    /// The DDL struct the rows point at, when the schema knows it.
    pub type_name: Option<String>,
    /// `crc64` type id from the row, hex.
    pub type_uid: String,
    /// True when `type_uid` is `crc64(lower(component + "Prius"))`. False for
    /// the 277 shipped rows whose struct is a base class instead.
    pub type_uid_matches_name: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZonePriusSummary {
    pub zone_path: String,
    pub actor_count: usize,
    pub actors_with_priuses: usize,
    pub rows: usize,
    pub pool_bytes: usize,
    pub script_prius_bytes: usize,
    pub components: Vec<ComponentCount>,
    pub schema: SchemaStatus,
}

#[tauri::command]
pub async fn prius_zone_summary(
    game_dir: String,
    zone_path: String,
    hashes_path: Option<String>,
) -> Result<ZonePriusSummary> {
    let idx = index(&game_dir, hashes_path)?;
    let zone = load_zone(&idx, &zone_path)?;
    let db = schema();
    let table = PriusTable::from_dat1(&zone.dat1);

    let mut seen: std::collections::BTreeMap<String, (usize, &prius::PriusRow)> =
        std::collections::BTreeMap::new();
    for r in &table.rows {
        let e = seen.entry(r.component.clone()).or_insert((0, r));
        e.0 += 1;
    }
    let mut components: Vec<ComponentCount> = seen
        .iter()
        .map(|(name, (count, r))| ComponentCount {
            component: name.clone(),
            count: *count,
            name_hash: format!("{:#010X}", r.name_hash),
            type_name: db
                .as_ref()
                .and_then(|d| {
                    d.ty_by_uid(r.type_uid)
                        .or_else(|| d.prius_of_component(name))
                })
                .map(|t| t.name.clone()),
            type_uid: format!("{:#018X}", r.type_uid),
            type_uid_matches_name: r.type_uid_ok(),
        })
        .collect();
    components.sort_by(|a, b| b.count.cmp(&a.count));

    Ok(ZonePriusSummary {
        zone_path,
        actor_count: zone.actors.len(),
        actors_with_priuses: zone.actors.iter().filter(|a| a.prius_count > 0).count(),
        rows: table.rows.len(),
        pool_bytes: table.pool.len(),
        script_prius_bytes: zone
            .dat1
            .get_section_data(TAG_SCRIPT_PRIUSES)
            .map(|s| s.len())
            .unwrap_or(0),
        components,
        schema: status_of(db.as_deref()),
    })
}

// ---------------------------------------------------------------------------
// One actor's property sheets
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PriusSheet {
    /// Row index in Zone Actor Priuses — the handle `prius_plan_edit` wants.
    pub row: usize,
    /// Pool-relative byte offset of this blob, for the patch arithmetic.
    pub blob_offset: usize,
    pub blob_size: usize,
    pub view: PriusView,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorSheets {
    pub actor_index: usize,
    pub actor_name: String,
    pub actor_asset: String,
    pub uid: String,
    pub flags: u16,
    pub flag_names: Vec<&'static str>,
    pub is_template: bool,
    pub starts_active: bool,
    pub sheets: Vec<PriusSheet>,
    /// Rows that would not decode, with the reason. Empty on every shipped zone
    /// measured.
    pub failures: Vec<String>,
}

#[tauri::command]
pub async fn prius_actor_sheet(
    game_dir: String,
    zone_path: String,
    actor_index: usize,
    hashes_path: Option<String>,
) -> Result<ActorSheets> {
    let idx = index(&game_dir, hashes_path)?;
    let zone = load_zone(&idx, &zone_path)?;
    let db = schema();
    let table = PriusTable::from_dat1(&zone.dat1);

    let a = zone
        .actors
        .get(actor_index)
        .ok_or_else(|| ToolkitError::Parse(format!("no actor {actor_index} in {zone_path}")))?;

    let ap = prius::actor_priuses(
        &table,
        &zone.dat1,
        db.as_deref(),
        a.instance_id,
        a.prius_index,
        a.prius_count,
    );

    let sheets = ap
        .blobs
        .iter()
        .zip(&ap.row_indices)
        .map(|(b, &row)| PriusSheet {
            row,
            blob_offset: table.blob_at(row).unwrap_or(0),
            blob_size: b.original.len(),
            view: prius::view(
                &b.component,
                table.rows[row].type_uid,
                b.original.len() as u32,
                &b.tree,
            ),
        })
        .collect();

    Ok(ActorSheets {
        actor_index,
        actor_name: a.name.clone(),
        actor_asset: a.asset_path.clone(),
        uid: format!("{:#018X}", a.instance_id),
        flags: a.flags,
        flag_names: a.flag_names.clone(),
        is_template: a.is_template(),
        starts_active: a.starts_active(),
        sheets,
        failures: ap.failures.iter().map(|(i, e)| format!("row {i}: {e}")).collect(),
    })
}

// ---------------------------------------------------------------------------
// One script action's prius
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn prius_script_sheet(
    game_dir: String,
    zone_path: String,
    action_index: usize,
    hashes_path: Option<String>,
) -> Result<PriusSheet> {
    let idx = index(&game_dir, hashes_path)?;
    let zone = load_zone(&idx, &zone_path)?;
    let db = schema();

    let a = zone.actions.get(action_index).ok_or_else(|| {
        ToolkitError::Parse(format!("no script action {action_index} in {zone_path}"))
    })?;
    let pool = zone
        .dat1
        .get_section_data(TAG_SCRIPT_PRIUSES)
        .ok_or_else(|| ToolkitError::Parse("zone has no Zone Script Priuses".into()))?;
    let lo = a.prius_offset as usize;
    let hi = lo
        .checked_add(a.prius_size as usize)
        .filter(|h| *h <= pool.len())
        .ok_or_else(|| ToolkitError::Parse("script prius blob leaves its section".into()))?;

    let action = script_action_name(&zone, a);
    let ctx = NameCtx::new(Some(&zone.dat1), db.as_deref());
    let b = PriusBlob::new(&pool[lo..hi], &action, 0, &ctx, db.as_deref())?;
    Ok(PriusSheet {
        row: action_index,
        blob_offset: lo,
        blob_size: hi - lo,
        view: prius::view(&action, a.node_id, (hi - lo) as u32, &b.tree),
    })
}

/// `Zone Script Strings`: a `u32[]` of string offsets the action rows index.
const TAG_SCRIPT_STRINGS: u32 = 0xEF86_37D5;

fn script_action_name(zone: &Zone, a: &crate::core::zone::ScriptAction) -> String {
    let strings = zone.dat1.get_section_data(TAG_SCRIPT_STRINGS).unwrap_or(&[]);
    let i = a.type_index as usize;
    if i * 4 + 4 <= strings.len() {
        let so = u32::from_le_bytes(strings[i * 4..i * 4 + 4].try_into().unwrap());
        zone.dat1.get_string(so).unwrap_or_default()
    } else {
        String::new()
    }
}

// ---------------------------------------------------------------------------
// Planning one edit
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditPlan {
    pub ok: bool,
    /// Why not, when `ok` is false — safe to show verbatim; the messages name
    /// the slot width and the reason.
    pub error: Option<String>,
    /// DAT1 section the patch belongs to.
    pub section_tag: Option<String>,
    /// Byte offset **inside that section**, ready for an equal-length in-place
    /// patch.
    pub section_offset: Option<usize>,
    /// The replacement bytes. Always the same length as the range they replace.
    pub bytes: Option<Vec<u8>>,
    /// The field's current value, for an undo entry.
    pub old_bytes: Option<Vec<u8>>,
    pub old_text: Option<String>,
    pub new_text: Option<String>,
    /// What class of edit this was, so a UI can label the undo.
    pub edit: Option<Edit>,
}

fn fail(e: impl std::fmt::Display) -> EditPlan {
    EditPlan {
        ok: false,
        error: Some(e.to_string()),
        section_tag: None,
        section_offset: None,
        bytes: None,
        old_bytes: None,
        old_text: None,
        new_text: None,
        edit: None,
    }
}

/// Validate and encode one field edit, returning the byte patch to apply.
///
/// `number` and `text` are alternatives: pass `number` for anything numeric or
/// boolean (the value is coerced into whatever narrow type the blob actually
/// stores, and refused when it cannot be), `text` for a `select` member or a
/// free string (refused unless the new value occupies the same slot).
///
/// `scriptAction` switches from Zone Actor Priuses to Zone Script Priuses, with
/// `row` meaning the script-action index.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn prius_plan_edit(
    game_dir: String,
    zone_path: String,
    row: usize,
    field_path: String,
    number: Option<f64>,
    text: Option<String>,
    script_action: Option<bool>,
    hashes_path: Option<String>,
) -> Result<EditPlan> {
    let idx = index(&game_dir, hashes_path)?;
    let zone = load_zone(&idx, &zone_path)?;
    let db = schema();
    let ctx = NameCtx::new(Some(&zone.dat1), db.as_deref());
    let script = script_action.unwrap_or(false);

    // Locate the blob and the section it lives in.
    let (blob, base, tag, label, type_uid) = if script {
        let Some(a) = zone.actions.get(row) else {
            return Ok(fail(format!("no script action {row}")));
        };
        let Some(pool) = zone.dat1.get_section_data(TAG_SCRIPT_PRIUSES) else {
            return Ok(fail("zone has no Zone Script Priuses"));
        };
        let lo = a.prius_offset as usize;
        let hi = lo + a.prius_size as usize;
        if hi > pool.len() || a.prius_size < 16 {
            return Ok(fail("script prius blob leaves its section"));
        }
        (
            pool[lo..hi].to_vec(),
            lo,
            TAG_SCRIPT_PRIUSES,
            script_action_name(&zone, a),
            0u64,
        )
    } else {
        let table = PriusTable::from_dat1(&zone.dat1);
        let Some(r) = table.rows.get(row) else {
            return Ok(fail(format!("no prius row {row}")));
        };
        let Some(bytes) = table.blob(row) else {
            return Ok(fail("prius blob leaves the data pool"));
        };
        (
            bytes.to_vec(),
            table.blob_at(row).unwrap_or(0),
            TAG_ACTOR_PRIUS_POOL,
            r.component.clone(),
            r.type_uid,
        )
    };

    let mut b = match PriusBlob::new(&blob, &label, type_uid, &ctx, db.as_deref()) {
        Ok(b) => b,
        Err(e) => return Ok(fail(e)),
    };

    // Find the field so we can report the span and the old value.
    let Some(fv) = find_view(&b, &field_path) else {
        return Ok(fail(format!("no field {field_path}")));
    };
    let (span_at, span_len, old_text, edit) = fv;

    let outcome = match (number, text) {
        (Some(x), None) => b.patch_number(&field_path, x),
        (None, Some(t)) => b.patch(
            &field_path,
            &PValue::Str { value: t, hash: 0, checksum: 0 },
        ),
        (Some(_), Some(_)) => Err(ToolkitError::Parse(
            "pass either a number or a text value, not both".into(),
        )),
        (None, None) => Err(ToolkitError::Parse("no value given".into())),
    };
    if let Err(e) = outcome {
        return Ok(fail(e));
    }

    let new_bytes = b.to_bytes()[span_at..span_at + span_len].to_vec();
    let new_text = find_view(&b, &field_path).map(|f| f.2);
    Ok(EditPlan {
        ok: true,
        error: None,
        section_tag: Some(format!("{tag:#010X}")),
        section_offset: Some(base + span_at),
        bytes: Some(new_bytes),
        old_bytes: Some(blob[span_at..span_at + span_len].to_vec()),
        old_text: Some(old_text),
        new_text,
        edit: Some(edit),
    })
}

/// `(span start, span len, value text, edit class)` of one dotted field path.
fn find_view(b: &PriusBlob, path: &str) -> Option<(usize, usize, String, Edit)> {
    fn walk(
        obj: &prius::PObject,
        steps: &[&str],
    ) -> Option<(usize, usize, String, Edit)> {
        let (name, idx) = match steps[0].split_once('[') {
            Some((n, rest)) => (n, rest.trim_end_matches(']').parse::<usize>().unwrap_or(0)),
            None => (steps[0], 0),
        };
        let f = obj.fields.iter().find(|f| f.name == name)?;
        if steps.len() == 1 {
            let (s, l) = f.span(idx)?;
            let t = f.values.get(idx).map(|v| v.text()).unwrap_or_default();
            return Some((s, l, t, f.edit));
        }
        match f.values.get(idx)? {
            PValue::Struct(o) => walk(o, &steps[1..]),
            _ => None,
        }
    }
    let steps: Vec<&str> = path.split('.').collect();
    walk(&b.tree, &steps)
}
