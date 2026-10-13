//! Rivet Hook's `rivet.toml` (next to RiftApart.exe), edited through toml_edit so comments and the address cache survive.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use toml_edit::{Array, DocumentMut, Item, Table, Value};

use crate::core::error::ToolkitError;

const CONFIG_NAME: &str = "rivet.toml";
const GAME_EXE: &str = "RiftApart.exe";
/// The hook itself; `hid.dll` is only its loader and can be another proxy.
const HOOK_DLL: &str = "rivet_hook.dll";
/// Pointer cache the hook keeps for itself; never shown or edited.
const HIDDEN_SECTIONS: &[&str] = &["address_cache"];
/// Folder under the game dir that installed mods are exported to.
const MOD_DIR: &str = "mods";

#[derive(Serialize)]
pub struct RivetEntry {
    key: String,
    /// `bool`, `int`, `float`, `string`, `key` (a virtual-key name) or `list` (strings).
    kind: &'static str,
    value: Json,
    comment: String,
}

#[derive(Serialize)]
pub struct RivetSection {
    name: String,
    entries: Vec<RivetEntry>,
}

#[derive(Serialize)]
pub struct RivetMod {
    path: String,
    enabled: bool,
    exists: bool,
}

#[derive(Serialize)]
pub struct RivetConfig {
    config_path: String,
    exists: bool,
    game_running: bool,
    sections: Vec<RivetSection>,
    /// Loaded mods in `assets.paths` order (first is lowest priority), then unlisted mods on disk.
    mods: Vec<RivetMod>,
}

#[derive(Deserialize)]
pub struct RivetEdit {
    section: String,
    key: String,
    value: Json,
}

#[derive(Serialize)]
pub struct RivetInstall {
    entry: String,
    note: String,
}

#[tauri::command]
pub async fn rivet_hook_installed(game_dir: String) -> bool {
    !game_dir.is_empty() && Path::new(&game_dir).join(HOOK_DLL).is_file()
}

#[tauri::command]
pub async fn read_rivet_config(game_dir: String) -> Result<RivetConfig, ToolkitError> {
    let game = PathBuf::from(&game_dir);
    let path = game.join(CONFIG_NAME);
    let exists = path.is_file();
    let doc = load_doc(&path)?;

    let mut sections = Vec::new();
    for (name, item) in doc.as_table().iter() {
        let Some(table) = item.as_table() else { continue };
        if HIDDEN_SECTIONS.contains(&name) {
            continue;
        }
        let entries = table
            .iter()
            .filter_map(|(key, item)| entry(table, key, item))
            .collect();
        sections.push(RivetSection { name: name.to_string(), entries });
    }

    let enabled = mod_paths(&doc);
    let mut mods: Vec<RivetMod> = enabled
        .iter()
        .map(|p| RivetMod { path: p.clone(), enabled: true, exists: resolve(&game, p).exists() })
        .collect();
    for path in discover_mods(&game, &enabled) {
        mods.push(RivetMod { path, enabled: false, exists: true });
    }

    Ok(RivetConfig {
        config_path: path.to_string_lossy().into_owned(),
        exists,
        game_running: game_running(),
        sections,
        mods,
    })
}

#[tauri::command]
pub async fn write_rivet_config(
    game_dir: String,
    edits: Vec<RivetEdit>,
    mods: Option<Vec<String>>,
) -> Result<String, ToolkitError> {
    ensure_game_closed()?;
    let path = PathBuf::from(&game_dir).join(CONFIG_NAME);
    let mut doc = load_doc(&path)?;
    for edit in &edits {
        let table = section_mut(&mut doc, &edit.section)?;
        let current = table.get(&edit.key).and_then(Item::as_value);
        let value = to_toml(&edit.value, current)?;
        set_value(table, &edit.key, value);
    }
    if let Some(paths) = mods {
        set_mod_paths(&mut doc, &paths)?;
    }
    save_doc(&path, &doc)?;
    Ok(path.to_string_lossy().into_owned())
}

/// Export a Stager project into `<game>/mods` and list it; a listed older version is swapped in place.
#[tauri::command]
pub async fn install_rivet_mod(
    game_dir: String,
    project: String,
    file_name: String,
) -> Result<RivetInstall, ToolkitError> {
    ensure_game_closed()?;
    let file_name = Path::new(&file_name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| n.to_ascii_lowercase().ends_with(".stage"))
        .ok_or_else(|| ToolkitError::Parse(format!("not a .stage file name: {file_name}")))?;

    let game = PathBuf::from(&game_dir);
    let dir = game.join(MOD_DIR);
    std::fs::create_dir_all(&dir)?;
    crate::commands::export_stage(project.clone(), dir.join(&file_name).to_string_lossy().into_owned())
        .await?;

    let entry = format!("{MOD_DIR}/{file_name}");
    let path = game.join(CONFIG_NAME);
    let mut doc = load_doc(&path)?;
    let mut paths = mod_paths(&doc);
    let note = if paths.iter().any(|p| norm(p) == norm(&entry)) {
        "Re-exported; already in the load list".to_string()
    } else if let Some(i) = paths.iter().position(|p| is_version_of(p, &project)) {
        let old = std::mem::replace(&mut paths[i], entry.clone());
        format!("Replaced {old} in the load list")
    } else {
        paths.push(entry.clone());
        "Added to the end of the load list (highest priority)".to_string()
    };
    set_mod_paths(&mut doc, &paths)?;
    save_doc(&path, &doc)?;
    Ok(RivetInstall { entry, note })
}

fn load_doc(path: &Path) -> Result<DocumentMut, ToolkitError> {
    if !path.is_file() {
        return Ok(DocumentMut::new());
    }
    std::fs::read_to_string(path)?
        .parse::<DocumentMut>()
        .map_err(|e| ToolkitError::Parse(format!("{} is not valid TOML: {e}", path.display())))
}

fn save_doc(path: &Path, doc: &DocumentMut) -> Result<(), ToolkitError> {
    if path.is_file() {
        std::fs::copy(path, path.with_extension("toml.bak"))?;
    }
    let tmp = path.with_extension("toml.omnitool.tmp");
    std::fs::write(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn entry(table: &Table, key: &str, item: &Item) -> Option<RivetEntry> {
    let (kind, value) = match item.as_value()? {
        Value::Boolean(b) => ("bool", Json::Bool(*b.value())),
        Value::Integer(i) => ("int", Json::from(*i.value())),
        Value::Float(f) => ("float", Json::from(*f.value())),
        Value::String(s) if key.ends_with("_key") => ("key", Json::from(s.value().as_str())),
        Value::String(s) => ("string", Json::from(s.value().as_str())),
        Value::Array(a) if a.iter().all(|v| v.is_str()) => (
            "list",
            Json::Array(a.iter().filter_map(|v| v.as_str()).map(Json::from).collect()),
        ),
        _ => return None,
    };
    Some(RivetEntry { key: key.to_string(), kind, value, comment: comment(table, key) })
}

/// The `#` lines written above a key, joined into one line.
fn comment(table: &Table, key: &str) -> String {
    let prefix = table
        .key(key)
        .and_then(|k| k.leaf_decor().prefix())
        .and_then(|p| p.as_str())
        .unwrap_or("");
    prefix
        .lines()
        .filter_map(|l| l.trim().strip_prefix('#'))
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn section_mut<'a>(doc: &'a mut DocumentMut, name: &str) -> Result<&'a mut Table, ToolkitError> {
    doc.as_table_mut()
        .entry(name)
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| ToolkitError::Parse(format!("[{name}] in rivet.toml is not a table")))
}

/// Replace a value but keep the spacing around it; the key's comment lives on the key.
fn set_value(table: &mut Table, key: &str, value: Value) {
    match table.get_mut(key).and_then(Item::as_value_mut) {
        Some(slot) => {
            let decor = slot.decor().clone();
            *slot = value;
            *slot.decor_mut() = decor;
        }
        None => {
            table.insert(key, Item::Value(value));
        }
    }
}

fn to_toml(value: &Json, current: Option<&Value>) -> Result<Value, ToolkitError> {
    Ok(match value {
        Json::Bool(b) => Value::from(*b),
        Json::Number(n) => match n.as_i64() {
            Some(i) if !matches!(current, Some(Value::Float(_))) => Value::from(i),
            _ => Value::from(n.as_f64().unwrap_or_default()),
        },
        Json::String(s) => Value::from(s.as_str()),
        Json::Array(items) => {
            let mut arr = Array::new();
            for item in items {
                arr.push(to_toml(item, None)?);
            }
            multiline(&mut arr);
            Value::Array(arr)
        }
        _ => return Err(ToolkitError::Parse(format!("unsupported rivet.toml value: {value}"))),
    })
}

/// One entry per line with a trailing comma, the way the hook writes its own arrays.
fn multiline(arr: &mut Array) {
    for v in arr.iter_mut() {
        v.decor_mut().set_prefix("\n    ");
        v.decor_mut().set_suffix("");
    }
    arr.set_trailing_comma(!arr.is_empty());
    arr.set_trailing(if arr.is_empty() { "" } else { "\n" });
}

fn mod_paths(doc: &DocumentMut) -> Vec<String> {
    doc.get("assets")
        .and_then(|a| a.get("paths"))
        .and_then(Item::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn set_mod_paths(doc: &mut DocumentMut, paths: &[String]) -> Result<(), ToolkitError> {
    let list = Json::Array(paths.iter().map(|p| Json::from(p.as_str())).collect());
    let value = to_toml(&list, None)?;
    set_value(section_mut(doc, "assets")?, "paths", value);
    Ok(())
}

fn norm(p: &str) -> String {
    p.replace('\\', "/").trim_start_matches("./").to_ascii_lowercase()
}

fn resolve(game: &Path, entry: &str) -> PathBuf {
    let p = Path::new(entry);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        game.join(p)
    }
}

/// `mods/<project>.stage` or `mods/<project>-<version>.stage`.
fn is_version_of(entry: &str, project: &str) -> bool {
    let entry = norm(entry);
    let Some(name) = entry.strip_prefix(&format!("{MOD_DIR}/")) else { return false };
    let project = project.to_ascii_lowercase();
    name == format!("{project}.stage")
        || (name.starts_with(&format!("{project}-")) && name.ends_with(".stage"))
}

/// Unlisted `.stage` files and Overstrike folders in `mods/` and in every folder a listed mod sits in.
fn discover_mods(game: &Path, listed: &[String]) -> Vec<String> {
    let mut folders = vec![MOD_DIR.to_string()];
    for p in listed {
        if let Some(parent) = Path::new(p).parent().map(|d| d.to_string_lossy().replace('\\', "/")) {
            if !parent.is_empty() && !folders.iter().any(|f| norm(f) == norm(&parent)) {
                folders.push(parent);
            }
        }
    }
    let known: std::collections::HashSet<String> = listed.iter().map(|p| norm(p)).collect();
    let mut found = Vec::new();
    for folder in folders {
        let Ok(dir) = std::fs::read_dir(resolve(game, &folder)) else { continue };
        let mut names: Vec<String> = dir
            .filter_map(Result::ok)
            .filter(|e| {
                let path = e.path();
                let is_stage = path.is_file()
                    && path.extension().is_some_and(|x| x.eq_ignore_ascii_case("stage"));
                is_stage || (path.is_dir() && path.join("info.json").is_file())
            })
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for name in names {
            let rel = format!("{folder}/{name}");
            if !known.contains(&norm(&rel)) && !found.iter().any(|f: &String| norm(f) == norm(&rel)) {
                found.push(rel);
            }
        }
    }
    found
}

fn game_running() -> bool {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::new(),
    );
    sys.processes().values().any(|p| p.name().eq_ignore_ascii_case(GAME_EXE))
}

/// The hook writes its settings back on exit, which would undo an edit made while the game runs.
fn ensure_game_closed() -> Result<(), ToolkitError> {
    if game_running() {
        return Err(ToolkitError::Parse(
            "Rift Apart is running. Rivet Hook rewrites rivet.toml when the game closes, so close the game first."
                .into(),
        ));
    }
    Ok(())
}
