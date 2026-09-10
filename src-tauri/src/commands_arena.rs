//! Arena Editor commands: read a gameplay `.zone`, expose its script/actor
//! prius blobs and actor asset table, and write the edits back.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::Value;

use crate::core::error::ToolkitError;
use crate::core::zone as zone_mod;
use crate::core::zone::{Zone, ZoneEdits};

#[derive(serde::Serialize)]
pub struct ArenaPrius {
    pub id: usize,
    pub label: String,
    pub owners: Vec<String>,
    pub json: String,
    pub size: u32,
}

#[derive(serde::Serialize)]
pub struct ArenaActorAsset {
    pub index: usize,
    pub path: String,
    pub asset_id: String,
    pub instances: Vec<String>,
    pub is_enemy: bool,
    pub prius_ids: Vec<usize>,
}

#[derive(serde::Serialize)]
pub struct ArenaAssetRef {
    pub index: usize,
    pub path: String,
    pub asset_id: String,
    pub ext_hash: String,
}

/// Where one spawner takes its spawn points from, and the variable that decides it.
#[derive(serde::Serialize)]
pub struct ArenaSpawnBinding {
    /// Script var to rewrite in order to repoint this spawner.
    pub var: usize,
    /// "actor" (one spawn point) or "group" (a pool an ActorPick draws from).
    pub kind: String,
    pub id: String,
    pub label: String,
    pub asset: String,
    pub style: String,
    /// How the binding is reached — "direct", or the chain of picker nodes.
    pub via: String,
}

/// A spawn point the zone already contains, offered as a retarget option.
#[derive(serde::Serialize)]
pub struct ArenaSpawnTarget {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub asset: String,
    pub count: usize,
    pub style: String,
}

#[derive(serde::Serialize)]
pub struct ArenaSpawner {
    pub node: usize,
    /// Enemy actor instance the spawner's factory builds.
    pub template: String,
    pub num_spawns: Option<f64>,
    /// Script var backing `NumSpawns` — the editable handle for the count.
    pub num_spawns_var: Option<usize>,
    pub max_simultaneous: Option<f64>,
    pub prius_id: Option<usize>,
    pub groups: Vec<String>,
    pub locations: Vec<ArenaSpawnBinding>,
}

#[derive(serde::Serialize)]
pub struct ArenaWave {
    pub number: u32,
    pub signals: Vec<String>,
    pub spawners: Vec<ArenaSpawner>,
    pub total: f64,
}

#[derive(serde::Serialize)]
pub struct ArenaZoneData {
    pub zone_name: String,
    pub action_count: usize,
    pub actor_count: usize,
    pub waves: Vec<ArenaWave>,
    pub node_type_counts: Vec<(String, usize)>,
    pub actor_groups: Vec<String>,
    pub script_priuses: Vec<ArenaPrius>,
    pub actor_priuses: Vec<ArenaPrius>,
    pub actor_assets: Vec<ArenaActorAsset>,
    pub model_names: Vec<String>,
    pub asset_refs: Vec<ArenaAssetRef>,
    pub spawn_targets: Vec<ArenaSpawnTarget>,
}

#[tauri::command]
pub async fn read_arena_zone(zone_path: String) -> Result<ArenaZoneData, ToolkitError> {
    let start = Instant::now();
    eprintln!("[arena_editor] reading {zone_path}");
    let bytes = std::fs::read(&zone_path)?;
    let zone = Zone::parse(&bytes)?;

    let mut node_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for a in &zone.actions {
        *node_counts.entry(zone.action_type(a)).or_insert(0) += 1;
    }
    let mut node_type_counts: Vec<(String, usize)> = node_counts
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    node_type_counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let script_priuses: Vec<ArenaPrius> = zone
        .script_priuses
        .iter()
        .enumerate()
        .map(|(id, blob)| {
            let mut types: Vec<String> = blob
                .owners
                .iter()
                .filter_map(|i| zone.actions.get(*i))
                .map(|a| zone.action_type(a).to_string())
                .collect();
            types.sort();
            types.dedup();
            ArenaPrius {
                id,
                label: types.join(" / "),
                owners: blob.owners.iter().map(|i| format!("#{i}")).collect(),
                json: pretty(&blob.json),
                size: blob.size,
            }
        })
        .collect();

    let actor_priuses: Vec<ArenaPrius> = zone
        .actor_priuses
        .iter()
        .enumerate()
        .map(|(id, blob)| {
            let mut names: Vec<String> = blob
                .owners
                .iter()
                .filter_map(|i| zone.actor_prius_index.get(*i))
                .filter_map(|e| zone.dat1.get_string(e.name_offset))
                .collect();
            names.sort();
            names.dedup();
            ArenaPrius {
                id,
                label: if names.is_empty() {
                    format!("prius {id}")
                } else {
                    names.join(" / ")
                },
                owners: owners_of_actor_prius(&zone, id),
                json: pretty(&blob.json),
                size: blob.size,
            }
        })
        .collect();

    let actor_assets: Vec<ArenaActorAsset> = zone
        .actor_asset_paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let instances: Vec<String> = zone
                .instances_of_asset(index as u32)
                .into_iter()
                .map(str::to_string)
                .collect();
            let prius_ids = prius_ids_for_asset(&zone, index as u32);
            ArenaActorAsset {
                index,
                path: path.clone(),
                asset_id: format!(
                    "{:016X}",
                    zone.actor_asset_ids.get(index).copied().unwrap_or(0)
                ),
                is_enemy: is_enemy(path, &instances),
                instances,
                prius_ids,
            }
        })
        .collect();

    let asset_refs: Vec<ArenaAssetRef> = zone
        .asset_refs
        .iter()
        .enumerate()
        .map(|(index, r)| ArenaAssetRef {
            index,
            path: zone.dat1.get_string(r.name_offset).unwrap_or_default(),
            asset_id: format!("{:016X}", r.asset_id),
            ext_hash: format!("{:08X}", r.ext_hash),
        })
        .collect();

    let data = ArenaZoneData {
        zone_name: Path::new(&zone_path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        action_count: zone.actions.len(),
        actor_count: zone.actors.len(),
        waves: detect_waves(&zone),
        node_type_counts,
        actor_groups: string_list(&zone, crate::core::zone::TAG_ACTOR_GROUP_NAMES),
        script_priuses,
        actor_priuses,
        actor_assets,
        model_names: zone.model_names.clone(),
        asset_refs,
        spawn_targets: spawn_targets(&zone),
    };

    eprintln!(
        "[arena_editor] {} actions, {} actors, {} waves in {:?}",
        data.action_count,
        data.actor_count,
        data.waves.len(),
        start.elapsed()
    );
    Ok(data)
}

#[tauri::command]
pub async fn write_arena_zone(
    zone_path: String,
    edits_json: String,
    out_path: Option<String>,
) -> Result<String, ToolkitError> {
    let start = Instant::now();
    let edits: ZoneEdits = serde_json::from_str(&edits_json)
        .map_err(|e| ToolkitError::Parse(format!("invalid edit payload: {e}")))?;

    let bytes = std::fs::read(&zone_path)?;
    let zone = Zone::parse(&bytes)?;
    let out_bytes = zone.save(&edits)?;

    let output = out_path
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let base = Path::new(&zone_path);
            let stem = base.file_stem().unwrap_or_default().to_string_lossy();
            base.with_file_name(format!("{stem}_edited.zone"))
        });

    std::fs::write(&output, &out_bytes)?;
    eprintln!(
        "[arena_editor] wrote {} bytes to {} in {:?}",
        out_bytes.len(),
        output.display(),
        start.elapsed()
    );
    Ok(output.to_string_lossy().into_owned())
}

/// Re-emit the zone with no edits and report whether it is byte-identical.
#[tauri::command]
pub async fn verify_arena_zone_roundtrip(zone_path: String) -> Result<String, ToolkitError> {
    let bytes = std::fs::read(&zone_path)?;
    let zone = Zone::parse(&bytes)?;
    let out = zone.save(&ZoneEdits::default())?;
    if out == bytes {
        Ok(format!("byte-identical ({} bytes)", out.len()))
    } else {
        let diff = bytes
            .iter()
            .zip(out.iter())
            .position(|(a, b)| a != b)
            .map(|p| format!("first difference at byte {p}"))
            .unwrap_or_else(|| "sizes differ".to_string());
        Ok(format!(
            "MISMATCH: {} bytes in, {} bytes out — {diff}",
            bytes.len(),
            out.len()
        ))
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".into())
}

fn string_list(zone: &Zone, tag: u32) -> Vec<String> {
    zone.dat1
        .get_section_data(tag)
        .map(|d| {
            d.chunks_exact(4)
                .map(|c| {
                    let off = u32::from_le_bytes(c.try_into().unwrap());
                    zone.dat1.get_string(off).unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn owners_of_actor_prius(zone: &Zone, blob_id: usize) -> Vec<String> {
    let mut out: Vec<String> = zone
        .actors
        .iter()
        .filter(|a| zone.prius_blobs_of_actor(a).contains(&blob_id))
        .filter_map(|a| zone.actor_names.get(a.name_index as usize).cloned())
        .collect();
    out.sort();
    out.dedup();
    out
}

fn prius_ids_for_asset(zone: &Zone, asset_index: u32) -> Vec<usize> {
    let mut out: Vec<usize> = zone
        .actors
        .iter()
        .filter(|a| a.asset_index == asset_index)
        .flat_map(|a| zone.prius_blobs_of_actor(a))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

fn is_enemy(path: &str, instances: &[String]) -> bool {
    let p = path.to_ascii_lowercase().replace('\\', "/");
    p.contains("characters/enemy")
        || p.contains("characters/npc")
        || instances
            .iter()
            .any(|n| n.to_ascii_lowercase().starts_with("enm_"))
}

/// Waves are named by their signal relays (`wave_03_A_start`) and by the actor
/// group each spawner writes into (`wave_03_enemies`), so the wave set comes
/// from the numbers those names carry.
fn detect_waves(zone: &Zone) -> Vec<ArenaWave> {
    let mut signals: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for blob in &zone.script_priuses {
        let Some(name) = blob
            .json
            .get("Name")
            .and_then(|f| f.get("Value"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if let Some(n) = wave_number(name) {
            signals.entry(n).or_default().push(short_signal(name));
        }
    }

    let mut spawners: BTreeMap<u32, Vec<ArenaSpawner>> = BTreeMap::new();
    for (i, action) in zone.actions.iter().enumerate() {
        if zone.action_type(action) != "SpawnerScriptAction" {
            continue;
        }
        let spawner = read_spawner(zone, i, action);
        let Some(number) = spawner.groups.iter().find_map(|g| wave_number(g)) else {
            continue;
        };
        spawners.entry(number).or_default().push(spawner);
    }

    let numbers: std::collections::BTreeSet<u32> =
        signals.keys().chain(spawners.keys()).copied().collect();
    numbers
        .into_iter()
        .map(|number| {
            let mut sig = signals.remove(&number).unwrap_or_default();
            sig.sort();
            sig.dedup();
            let list = spawners.remove(&number).unwrap_or_default();
            let total = list.iter().filter_map(|s| s.num_spawns).sum();
            ArenaWave {
                number,
                signals: sig,
                spawners: list,
                total,
            }
        })
        .collect()
}

/// Classify a spawn-point actor by how the enemy arrives from it.
///
/// `staticvolume.actor` is deliberately its own style rather than a volume:
/// bosses do spawn from one, but the same asset is also every hero bounce
/// target and trigger volume in the zone, so it is never a safe default.
fn spawn_style(asset: &str) -> &'static str {
    let a = asset.to_ascii_lowercase().replace('\\', "/");
    if a.contains("spawn_portal_bot") {
        "portal"
    } else if a.contains("/animclue") {
        "animclue"
    } else if a.contains("spawn_volume") {
        "volume"
    } else if a.contains("staticvolume") {
        "static"
    } else {
        "other"
    }
}

/// Ordering for the retarget list: purpose-built spawn points first.
fn style_rank(style: &str) -> u8 {
    match style {
        "volume" => 0,
        "portal" => 1,
        "animclue" => 2,
        "static" => 3,
        _ => 4,
    }
}

/// Spawn points already present in the zone, offered as retarget options.
fn spawn_targets(zone: &Zone) -> Vec<ArenaSpawnTarget> {
    let mut out = Vec::new();
    for group in &zone.actor_groups {
        if group.members.is_empty() {
            continue;
        }
        let mut assets: Vec<&str> = group
            .members
            .iter()
            .filter_map(|m| zone.actors.get(*m as usize))
            .filter_map(|a| zone.actor_asset_paths.get(a.asset_index as usize))
            .map(String::as_str)
            .collect();
        assets.sort_unstable();
        assets.dedup();
        let style = assets.first().map(|a| spawn_style(a)).unwrap_or("other");
        if style == "other" {
            continue;
        }
        out.push(ArenaSpawnTarget {
            id: format!("{:016X}", group.id),
            kind: "group".into(),
            label: group.name.clone(),
            asset: assets.join(" + "),
            count: group.members.len(),
            style: style.into(),
        });
    }
    for (i, actor) in zone.actors.iter().enumerate() {
        let Some(asset) = zone.actor_asset_paths.get(actor.asset_index as usize) else {
            continue;
        };
        let style = spawn_style(asset);
        if style == "other" {
            continue;
        }
        let name = zone
            .actor_names
            .get(actor.name_index as usize)
            .cloned()
            .unwrap_or_default();
        out.push(ArenaSpawnTarget {
            id: format!("{:016X}", actor.instance_id),
            kind: "actor".into(),
            label: format!("{name} #{i}"),
            asset: asset.clone(),
            count: 1,
            style: style.into(),
        });
    }
    out.sort_by(|a, b| {
        style_rank(&a.style)
            .cmp(&style_rank(&b.style))
            .then(b.kind.cmp(&a.kind))
            .then(b.count.cmp(&a.count))
            .then(a.label.cmp(&b.label))
    });
    out
}

/// Follow a `Locations` binding to the variable that actually decides the
/// spawn point — either an actor instance id or an actor group id.
fn resolve_binding(zone: &Zone, var: usize, via: &mut Vec<String>, depth: u8) -> ArenaSpawnBinding {
    let id = zone.vars.get(var).map(|v| v.id_value()).unwrap_or(0);

    if let Some(actor) = zone.actors.iter().find(|a| a.instance_id == id && id != 0) {
        let asset = zone
            .actor_asset_paths
            .get(actor.asset_index as usize)
            .cloned()
            .unwrap_or_default();
        return ArenaSpawnBinding {
            var,
            kind: "actor".into(),
            id: format!("{id:016X}"),
            label: zone
                .actor_names
                .get(actor.name_index as usize)
                .cloned()
                .unwrap_or_default(),
            style: spawn_style(&asset).into(),
            asset,
            via: join_via(via),
        };
    }

    if let Some(group) = zone.actor_groups.iter().find(|g| g.id == id && id != 0) {
        let asset = group
            .members
            .first()
            .and_then(|m| zone.actors.get(*m as usize))
            .and_then(|a| zone.actor_asset_paths.get(a.asset_index as usize))
            .cloned()
            .unwrap_or_default();
        return ArenaSpawnBinding {
            var,
            kind: "group".into(),
            id: format!("{id:016X}"),
            label: format!("{} [{}]", group.name, group.members.len()),
            style: spawn_style(&asset).into(),
            asset,
            via: join_via(via),
        };
    }

    if depth < 4 {
        if let Some(writer) = zone.var_writer(var) {
            if let Some(node) = zone.actions.get(writer) {
                let pick = zone
                    .script_priuses
                    .iter()
                    .find(|b| b.owners.contains(&writer))
                    .and_then(|b| b.json.pointer("/PickType/Value"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                via.push(format!(
                    "{}({pick})",
                    zone.action_type(node).replace("Action", "")
                ));
                for name in ["Group", "Actors", "Locations", "Location"] {
                    if let Some(p) = zone.param(node, name) {
                        if p.other_hash != crate::core::crc32::hash("In") {
                            return resolve_binding(zone, p.index as usize, via, depth + 1);
                        }
                    }
                }
            }
        }
    }

    ArenaSpawnBinding {
        var,
        kind: "unresolved".into(),
        id: format!("{id:016X}"),
        label: "filled at runtime".into(),
        asset: String::new(),
        style: "other".into(),
        via: join_via(via),
    }
}

fn join_via(via: &[String]) -> String {
    if via.is_empty() {
        "direct".into()
    } else {
        via.join(" → ")
    }
}

/// Read one spawner node's count, target group and bot template out of the graph.
fn read_spawner(zone: &Zone, index: usize, action: &zone_mod::ScriptAction) -> ArenaSpawner {
    let num_plug = zone.param(action, "NumSpawns");
    let num_spawns_var = num_plug.map(|p| p.index as usize);
    let num_spawns = num_spawns_var.and_then(|v| zone.vars.get(v)).and_then(|v| v.as_number());

    let groups: Vec<String> = zone
        .plugs
        .get(action.param_range())
        .map(|ps| {
            ps.iter()
                .filter(|p| p.name_hash == crate::core::crc32::hash("SpawnIntoGroup"))
                .filter_map(|p| zone.var_name(p.index as usize))
                .map(short_signal)
                .collect()
        })
        .unwrap_or_default();

    let prius_id = zone
        .script_priuses
        .iter()
        .position(|b| b.owners.contains(&index));
    let max_simultaneous = prius_id
        .and_then(|id| zone.script_priuses.get(id))
        .and_then(|b| b.json.pointer("/MaxSimultaneousSpawns/Value"))
        .and_then(Value::as_f64);

    ArenaSpawner {
        node: index,
        template: spawner_template(zone, action).unwrap_or_default(),
        num_spawns,
        num_spawns_var,
        max_simultaneous,
        prius_id,
        groups,
        locations: spawn_bindings(zone, action),
    }
}

/// `Locations` lives on the factory for factory spawners and on the node
/// itself for direct ones.
fn spawn_bindings(zone: &Zone, action: &zone_mod::ScriptAction) -> Vec<ArenaSpawnBinding> {
    let holder = match zone.param(action, "Factories") {
        Some(f) => match zone
            .var_writer(f.index as usize)
            .and_then(|w| zone.actions.get(w))
        {
            Some(factory) => factory,
            None => action,
        },
        None => action,
    };
    let wants = [
        crate::core::crc32::hash("Locations"),
        crate::core::crc32::hash("Location"),
    ];
    let write = crate::core::crc32::hash("In");
    zone.plugs
        .get(holder.param_range())
        .map(|ps| {
            ps.iter()
                .filter(|p| wants.contains(&p.name_hash) && p.other_hash != write)
                .map(|p| resolve_binding(zone, p.index as usize, &mut Vec::new(), 0))
                .collect()
        })
        .unwrap_or_default()
}

/// `Factories` names a runtime var; the factory node that writes it carries the
/// `Templates` binding whose value is an actor instance id.
fn spawner_template(zone: &Zone, action: &zone_mod::ScriptAction) -> Option<String> {
    let factories = zone.param(action, "Factories")?;
    let writer = zone.var_writer(factories.index as usize)?;
    let factory = zone.actions.get(writer)?;
    let template = zone
        .param(factory, "Templates")
        .or_else(|| zone.param(factory, "Template"))?;
    let id = zone.vars.get(template.index as usize)?.id_value();
    zone.actors
        .iter()
        .find(|a| a.instance_id == id)
        .and_then(|a| zone.actor_names.get(a.name_index as usize).cloned())
}

/// Pull the digits out of the first `wave_<n>` / `WAVE_<n>` token in `name`.
fn wave_number(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(pos) = lower[from..].find("wave_") {
        let after = from + pos + "wave_".len();
        let digits: String = lower[after..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if !digits.is_empty() {
            return digits.parse().ok();
        }
        from = after;
    }
    None
}

/// Signal and var names are namespaced as `0x<hash>::<name>`; show the readable half.
fn short_signal(name: impl AsRef<str>) -> String {
    let name = name.as_ref();
    name.rsplit(':').next().unwrap_or(name).to_string()
}
