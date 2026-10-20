//! DDL **type database** — the schema side of prius/config decoding.
//!
//! A prius blob on disk is *self-describing enough to walk* (every field says
//! its DDL type and element count, and carries a string-pool offset for its
//! name) but it says nothing about
//!
//!   * which named struct it is an instance of beyond the component name on the
//!     owning table row,
//!   * the legal member set of a `select` (enum) field,
//!   * the authored default a field would have had if it were not overridden,
//!   * the designer-facing label of a field.
//!
//! All four live in a type database that ships *outside* the game. This module
//! loads one at runtime and indexes it; nothing here is embedded in the binary,
//! and the decoder in [`crate::core::prius`] degrades to pool-resolved field
//! names when no database is configured.
//!
//! ## Database shape
//!
//! The database is a JSON document with four arrays:
//!
//! ```text
//! { "types":   [ { id, name, parent_id, size, component_id,
//!                  fields: [ { id, name, label, type, type_id,
//!                              array_type, map_type, fized_size,
//!                              offset, default } ] } ],
//!   "enums":   [ { id, id2, values: [ { id, name, label } ] } ],
//!   "bitsets": [ { id, values: [ { id, name, value } ] } ],
//!   "roots":   [ { id, name, parent: { id, name } } ] }
//! ```
//!
//! Every `id` is `crc32(name)` in the Insomniac variant (init `0xEDB88320`, no
//! final xor) — see [`crate::core::crc32`]. **VERIFIED** on the shipped PC
//! build: the component name `"GameHeroSingleSpawnPoint"` on a
//! `sargasso/tile_n19_gp.zone` prius row hashes to the `0x042BB0B8` stored
//! beside it, and `crc32("GameHeroSingleSpawnPointPrius") == 0x48D69B0C`, which
//! is the `types[]` id of that struct. Field ids in blobs likewise match
//! `crc32(field name)`.
//!
//! `types[].fields` is already **flattened over inheritance** in the databases
//! we have: `GameHeroSingleSpawnPointPrius` (6 fields) repeats the 3 fields of
//! its `parent_id` `HeroSingleSpawnPointPrius`. `parent_id` is kept anyway so a
//! UI can group inherited fields, and [`SchemaDb::fields_of`] walks parents for
//! databases that do not flatten.
//!
//! ## The database is *advisory*, never authoritative
//!
//! The type DB we have is a dump of a different build than the shipped PC game.
//! Measured example: the shipped `GameHeroSingleSpawnPoint` prius carries field
//! `0x11872F38` (`Character`, a `select` holding `"kRatchette"`), which is
//! absent from `GameHeroSingleSpawnPointPrius` in the database. So:
//!
//!   * a field the database does not know is still decoded, from the blob's own
//!     type/count words and its pool-resolved name;
//!   * the blob's own DDL type id always wins over the database's `type`;
//!   * the database supplies names, labels, enum member sets and defaults only.
//!
//! ## Provenance of names
//!
//! [`SchemaDb::name_index`] is a global `crc32 -> name` table harvested from
//! every type name, field name and enum/bitset member name in the database. It
//! is the fallback that names a field whose string-pool offset is missing or out
//! of range, and it is how a bare hash in a nested struct becomes readable.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Deserialize;
use serde_json::Value;

use crate::core::error::{Result, ToolkitError};

// ---------------------------------------------------------------------------
// Raw JSON shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RawDb {
    #[serde(default)]
    types: Vec<RawType>,
    #[serde(default)]
    enums: Vec<RawEnum>,
    #[serde(default)]
    bitsets: Vec<RawBitset>,
    #[serde(default)]
    roots: Vec<RawRoot>,
}

#[derive(Deserialize)]
struct RawType {
    id: i64,
    name: String,
    #[serde(default)]
    parent_id: i64,
    #[serde(default)]
    size: i64,
    #[serde(default)]
    component_id: i64,
    #[serde(default)]
    fields: Vec<RawField>,
}

#[derive(Deserialize)]
struct RawField {
    id: i64,
    name: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    display_label: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(rename = "type", default)]
    ddl_type: i64,
    #[serde(default)]
    type_id: i64,
    #[serde(default)]
    array_type: i64,
    #[serde(default)]
    map_type: i64,
    /// Spelled `fized_size` in the dump — kept verbatim so the game matches.
    #[serde(default, alias = "fixed_size")]
    fized_size: i64,
    #[serde(default)]
    offset: i64,
    #[serde(default)]
    default: Option<Value>,
}

#[derive(Deserialize)]
struct RawEnum {
    id: i64,
    #[serde(default)]
    id2: i64,
    #[serde(default)]
    values: Vec<RawEnumValue>,
}

#[derive(Deserialize)]
struct RawEnumValue {
    id: i64,
    name: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    value: Option<i64>,
}

#[derive(Deserialize)]
struct RawBitset {
    id: i64,
    #[serde(default)]
    values: Vec<RawEnumValue>,
}

#[derive(Deserialize)]
struct RawRoot {
    id: i64,
    name: String,
}

// ---------------------------------------------------------------------------
// Indexed model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FieldDef {
    /// `crc32(name)`.
    pub id: u32,
    pub name: String,
    /// Designer-facing label when the dump carries one and it differs from
    /// `name`; `"NONE"` in the dump means "no label" and is dropped.
    pub label: Option<String>,
    pub description: Option<String>,
    /// DDL type id 0..20. Advisory: the blob's own word wins.
    pub ddl_type: u8,
    /// For `select`/`bitfield`: the enum id. For `struct`: the struct type id.
    pub type_id: u32,
    pub array_type: u8,
    pub map_type: u8,
    pub fixed_size: u32,
    /// Byte offset inside the *runtime* struct — not the wire offset.
    pub runtime_offset: u32,
    pub default: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct TypeDef {
    pub id: u32,
    pub name: String,
    pub parent_id: u32,
    pub size: u32,
    pub component_id: u32,
    pub fields: Vec<FieldDef>,
    by_field: HashMap<u32, usize>,
}

impl TypeDef {
    pub fn field(&self, id: u32) -> Option<&FieldDef> {
        self.by_field.get(&id).map(|&i| &self.fields[i])
    }
}

#[derive(Debug, Clone)]
pub struct EnumValue {
    /// `crc32(name)`.
    pub id: u32,
    pub name: String,
    pub label: Option<String>,
    /// Present for bitsets and for enums that carry explicit values.
    pub value: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct EnumDef {
    pub id: u32,
    /// The dump carries a second id per enum; kept so both hash spellings
    /// resolve.
    pub id2: u32,
    pub bitset: bool,
    pub values: Vec<EnumValue>,
}

impl EnumDef {
    pub fn has_member(&self, name: &str) -> bool {
        self.values.iter().any(|v| v.name == name)
    }
    pub fn member_names(&self) -> Vec<&str> {
        self.values.iter().map(|v| v.name.as_str()).collect()
    }
}

#[derive(Debug, Default)]
pub struct SchemaDb {
    pub source: String,
    types: HashMap<u32, TypeDef>,
    by_name: HashMap<String, u32>,
    /// `crc64_plain(lowercase(type name)) -> crc32(type name)`. This is the key
    /// a Zone Actor Prius row stores at `+0x00`, so it resolves a blob's struct
    /// *by hash* instead of by the `component + "Prius"` naming convention.
    by_uid: HashMap<u64, u32>,
    enums: HashMap<u32, EnumDef>,
    /// `crc32 -> name` over every type, field, enum and member name in the DB.
    pub name_index: HashMap<u32, String>,
    /// Config/prius root struct names.
    pub roots: HashMap<u32, String>,
}

impl SchemaDb {
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }
    pub fn type_count(&self) -> usize {
        self.types.len()
    }
    pub fn enum_count(&self) -> usize {
        self.enums.len()
    }

    pub fn ty(&self, id: u32) -> Option<&TypeDef> {
        self.types.get(&id)
    }
    pub fn ty_by_name(&self, name: &str) -> Option<&TypeDef> {
        self.by_name.get(name).and_then(|id| self.types.get(id))
    }
    /// Resolve a struct from the 64-bit type id a prius row carries.
    pub fn ty_by_uid(&self, uid: u64) -> Option<&TypeDef> {
        self.by_uid.get(&uid).and_then(|id| self.types.get(id))
    }
    pub fn enum_def(&self, id: u32) -> Option<&EnumDef> {
        self.enums.get(&id)
    }
    pub fn name_of(&self, hash: u32) -> Option<&str> {
        self.name_index.get(&hash).map(|s| s.as_str())
    }

    /// The prius struct of a component named on a Zone Actor Prius row.
    ///
    /// The component is named (`"StackableCrate"`) and the struct is
    /// that name plus `"Prius"` (`"StackableCratePrius"`). **VERIFIED** for
    /// 100% of the distinct component names measured on the shipped build — see
    /// `prius_probe`. `name` alone and the `Component`-stripped spelling are
    /// tried as fallbacks so script-action and config callers can share this.
    pub fn prius_of_component(&self, component: &str) -> Option<&TypeDef> {
        let direct = format!("{component}Prius");
        if let Some(t) = self.ty_by_name(&direct) {
            return Some(t);
        }
        if let Some(stem) = component.strip_suffix("Component") {
            if let Some(t) = self.ty_by_name(&format!("{stem}Prius")) {
                return Some(t);
            }
        }
        self.ty_by_name(component)
    }

    /// Fields of `id`, walking `parent_id` for databases that do not flatten
    /// inheritance. Child fields shadow parent fields of the same id.
    pub fn fields_of(&self, id: u32) -> Vec<&FieldDef> {
        let mut out: Vec<&FieldDef> = Vec::new();
        let mut seen: HashMap<u32, ()> = HashMap::new();
        let mut cur = id;
        let mut guard = 0;
        while let Some(t) = self.types.get(&cur) {
            for f in &t.fields {
                if seen.insert(f.id, ()).is_none() {
                    out.push(f);
                }
            }
            if t.parent_id == 0 || t.parent_id == cur {
                break;
            }
            cur = t.parent_id;
            guard += 1;
            if guard > 32 {
                break;
            }
        }
        out
    }

    // -----------------------------------------------------------------------
    // Loading
    // -----------------------------------------------------------------------

    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .map_err(|e| ToolkitError::Parse(format!("ddl schema {}: {e}", path.display())))?;
        let raw: RawDb = serde_json::from_slice(&bytes)
            .map_err(|e| ToolkitError::Parse(format!("ddl schema {}: {e}", path.display())))?;
        Ok(Self::from_raw(raw, path.display().to_string()))
    }

    fn from_raw(raw: RawDb, source: String) -> Self {
        fn clean(s: Option<String>) -> Option<String> {
            match s {
                Some(v) if v != "NONE" && !v.is_empty() => Some(v),
                _ => None,
            }
        }

        let mut db = SchemaDb { source, ..Default::default() };

        for t in raw.types {
            let id = t.id as u32;
            let mut fields = Vec::with_capacity(t.fields.len());
            let mut by_field = HashMap::with_capacity(t.fields.len());
            for f in t.fields {
                let fid = f.id as u32;
                let label = clean(f.label).or_else(|| {
                    clean(f.display_label).filter(|l| *l != f.name)
                });
                db.name_index.entry(fid).or_insert_with(|| f.name.clone());
                by_field.insert(fid, fields.len());
                fields.push(FieldDef {
                    id: fid,
                    name: f.name,
                    label,
                    description: clean(f.description),
                    ddl_type: (f.ddl_type as u64 & 0xFF) as u8,
                    type_id: f.type_id as u32,
                    array_type: (f.array_type as u64 & 0xFF) as u8,
                    map_type: (f.map_type as u64 & 0xFF) as u8,
                    fixed_size: f.fized_size as u32,
                    runtime_offset: f.offset as u32,
                    default: f.default,
                });
            }
            db.name_index.entry(id).or_insert_with(|| t.name.clone());
            db.by_uid
                .insert(crate::core::prius::ddl_type_uid(&t.name.to_lowercase()), id);
            db.by_name.insert(t.name.clone(), id);
            db.types.insert(
                id,
                TypeDef {
                    id,
                    name: t.name,
                    parent_id: t.parent_id as u32,
                    size: t.size as u32,
                    component_id: t.component_id as u32,
                    fields,
                    by_field,
                },
            );
        }

        let add_enum = |id: u32, id2: u32, bitset: bool, values: Vec<RawEnumValue>, db: &mut SchemaDb| {
            let vals: Vec<EnumValue> = values
                .into_iter()
                .map(|v| {
                    let vid = v.id as u32;
                    db.name_index.entry(vid).or_insert_with(|| v.name.clone());
                    EnumValue { id: vid, name: v.name, label: clean(v.label), value: v.value }
                })
                .collect();
            let def = EnumDef { id, id2, bitset, values: vals };
            if id2 != 0 && id2 != id {
                db.enums.insert(id2, def.clone());
            }
            db.enums.insert(id, def);
        };

        for e in raw.enums {
            add_enum(e.id as u32, e.id2 as u32, false, e.values, &mut db);
        }
        for b in raw.bitsets {
            add_enum(b.id as u32, 0, true, b.values, &mut db);
        }
        for r in raw.roots {
            let id = r.id as u32;
            db.name_index.entry(id).or_insert_with(|| r.name.clone());
            db.roots.insert(id, r.name);
        }
        db
    }
}

// ---------------------------------------------------------------------------
// Process-wide cache
// ---------------------------------------------------------------------------

type Cache = Mutex<HashMap<PathBuf, Arc<SchemaDb>>>;

fn cache() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Where to look for a type database, in order:
///
///  1. `$OMNITOOL_DDL_SCHEMA` (a file, or a directory holding `rcra_ddl.json`)
///  2. `<exe dir>/ddl/rcra_ddl.json`
///  3. `<exe dir>/../refs/rivet/docs/ddl/rcra_ddl.json`
///
/// The database is not shipped with OmniTool; a user points at their own copy.
pub fn default_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(v) = std::env::var("OMNITOOL_DDL_SCHEMA") {
        let p = PathBuf::from(&v);
        if p.is_dir() {
            out.push(p.join("rcra_ddl.json"));
        } else {
            out.push(p);
        }
    }
    if let Ok(dir) = crate::core::filesystem::app_dir() {
        out.push(dir.join("ddl").join("rcra_ddl.json"));
        out.push(dir.join("..").join("refs").join("rivet").join("docs").join("ddl").join("rcra_ddl.json"));
    }
    out
}

/// Load (and memoise) the database at `path`.
pub fn get(path: &Path) -> Result<Arc<SchemaDb>> {
    let key = path.to_path_buf();
    if let Some(hit) = cache().lock().unwrap().get(&key).cloned() {
        return Ok(hit);
    }
    let db = Arc::new(SchemaDb::load(path)?);
    cache().lock().unwrap().insert(key, db.clone());
    Ok(db)
}

/// Load the first database that exists among [`default_paths`], or an empty one.
///
/// An empty database is not an error: [`crate::core::prius`] still decodes
/// blobs, it just cannot offer enum member sets or authored defaults.
pub fn get_default() -> Arc<SchemaDb> {
    static EMPTY: OnceLock<Arc<SchemaDb>> = OnceLock::new();
    for p in default_paths() {
        if p.is_file() {
            if let Ok(db) = get(&p) {
                return db;
            }
        }
    }
    EMPTY
        .get_or_init(|| {
            Arc::new(SchemaDb { source: "<none>".into(), ..Default::default() })
        })
        .clone()
}
