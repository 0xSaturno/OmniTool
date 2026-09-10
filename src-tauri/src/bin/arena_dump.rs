use omnitool_lib::core::zone::{Zone, ZoneEdits};

fn describe_binding(zone: &Zone, var: usize, depth: u8) -> String {
    let id = zone.vars.get(var).map(|v| v.id_value()).unwrap_or(0);
    if id != 0 {
        if let Some(a) = zone.actors.iter().find(|a| a.instance_id == id) {
            return format!(
                "var{var}=actor:{}",
                zone.actor_asset_paths
                    .get(a.asset_index as usize)
                    .map(|p| p.rsplit('\\').next().unwrap_or(p).to_string())
                    .unwrap_or_default()
            );
        }
        if let Some(g) = zone.actor_groups.iter().find(|g| g.id == id) {
            return format!("var{var}=group:{}[{}]", g.name, g.members.len());
        }
    }
    if depth < 4 {
        if let Some(w) = zone.var_writer(var) {
            if let Some(node) = zone.actions.get(w) {
                for name in ["Group", "Actors", "Locations", "Location"] {
                    if let Some(p) = zone.param(node, name) {
                        return format!(
                            "{} -> {}",
                            zone.action_type(node),
                            describe_binding(zone, p.index as usize, depth + 1)
                        );
                    }
                }
            }
        }
    }
    format!("var{var}=runtime")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: arena_dump <file.zone> [--roundtrip]");
        std::process::exit(1);
    }

    let bytes = std::fs::read(&args[1]).unwrap();
    let mut zone = Zone::parse(&bytes).unwrap();

    println!(
        "actions={} actors={} actor_assets={} script_priuses={} actor_priuses={} asset_refs={}",
        zone.actions.len(),
        zone.actors.len(),
        zone.actor_asset_paths.len(),
        zone.script_priuses.len(),
        zone.actor_priuses.len(),
        zone.asset_refs.len(),
    );

    println!("\n=== waves ===");
    for (i, a) in zone.actions.iter().enumerate() {
        if zone.action_type(a) != "SpawnerScriptAction" {
            continue;
        }
        let num = zone
            .param(a, "NumSpawns")
            .map(|p| p.index as usize)
            .and_then(|v| zone.vars.get(v).map(|x| (v, x.as_number())));
        let groups: Vec<&str> = zone
            .plugs
            .get(a.param_range())
            .map(|ps| {
                ps.iter()
                    .filter(|p| p.name_hash == omnitool_lib::core::zone::name_hash("SpawnIntoGroup"))
                    .filter_map(|p| zone.var_name(p.index as usize))
                    .collect()
            })
            .unwrap_or_default();
        let template = zone
            .param(a, "Factories")
            .and_then(|p| zone.var_writer(p.index as usize))
            .and_then(|w| zone.actions.get(w))
            .and_then(|f| zone.param(f, "Templates").or_else(|| zone.param(f, "Template")))
            .and_then(|p| zone.vars.get(p.index as usize))
            .map(|v| v.id_value())
            .and_then(|id| zone.actors.iter().find(|x| x.instance_id == id))
            .and_then(|x| zone.actor_names.get(x.name_index as usize).cloned())
            .unwrap_or_default();
        let holder = zone
            .param(a, "Factories")
            .and_then(|f| zone.var_writer(f.index as usize))
            .and_then(|w| zone.actions.get(w))
            .unwrap_or(a);
        let locs: Vec<String> = zone
            .plugs
            .get(holder.param_range())
            .map(|ps| {
                ps.iter()
                    .filter(|p| {
                        p.name_hash == omnitool_lib::core::zone::name_hash("Locations")
                            || p.name_hash == omnitool_lib::core::zone::name_hash("Location")
                    })
                    .map(|p| describe_binding(&zone, p.index as usize, 0))
                    .collect()
            })
            .unwrap_or_default();
        println!(
            "  node {i:<5} NumSpawns={:?} (var {:?})  {template:<28} groups={groups:?}",
            num.and_then(|(_, v)| v),
            num.map(|(v, _)| v),
        );
        println!("        from: {}", locs.join(" | "));
    }

    println!("\n=== actor assets ===");
    for (i, p) in zone.actor_asset_paths.iter().enumerate() {
        let inst = zone.instances_of_asset(i as u32).join(", ");
        println!("{i:3} {:016X} {p}  [{inst}]", zone.actor_asset_ids[i]);
    }

    println!("\n=== script priuses ===");
    for (i, b) in zone.script_priuses.iter().enumerate() {
        let types: Vec<&str> = b
            .owners
            .iter()
            .filter_map(|o| zone.actions.get(*o))
            .map(|a| zone.action_type(a))
            .collect();
        println!(
            "{i:3} off={} size={} {:?} {}",
            b.offset,
            b.size,
            types.first().unwrap_or(&"?"),
            serde_json::to_string(&b.json).unwrap()
        );
    }

    println!("\n=== actor priuses ===");
    for (i, b) in zone.actor_priuses.iter().enumerate() {
        let name = b
            .owners
            .first()
            .and_then(|o| zone.actor_prius_index.get(*o))
            .and_then(|e| zone.dat1.get_string(e.name_offset))
            .unwrap_or_default();
        println!(
            "{i:3} {name} {}",
            serde_json::to_string(&b.json).unwrap()
        );
    }

    if let Some(p) = args.iter().position(|a| a == "--clone-wave") {
        let source: u32 = args[p + 1].parse().unwrap();
        let new_number: u32 = args[p + 2].parse().unwrap();
        let out_path = args.get(p + 3).cloned().unwrap_or_else(|| "cloned.zone".into());

        let seg = zone.wave_segment(source).unwrap();
        println!(
            "\nwave {source} segment: {} nodes, head={} ({}), tail={} ({}), feeders={:?}",
            seg.nodes.len(),
            seg.head,
            zone.action_type(&zone.actions[seg.head]),
            seg.tail,
            zone.action_type(&zone.actions[seg.tail]),
            seg.feeders
        );

        let count: u32 = args.get(p + 4).and_then(|s| s.parse().ok()).unwrap_or(1);
        let mut edits = ZoneEdits::default();
        for k in 0..count {
            edits
                .clone_waves
                .push(omnitool_lib::core::zone::CloneWaveRequest {
                    source,
                    new_number: new_number + k,
                });
        }
        let before = zone.actions.len();
        let bytes = if args.iter().any(|a| a == "--no-rename") {
            let mut after = None;
            for r in &edits.clone_waves {
                let rep = zone
                    .clone_wave_inner(r.source, r.new_number, after, false)
                    .unwrap();
                after = Some(rep.tail_node);
            }
            zone.save(&ZoneEdits::default()).unwrap()
        } else {
            zone.save(&edits).unwrap()
        };

        // Optional: force every copied spawner's NumSpawns, so the wave's
        // whole budget is satisfied by its first spawn round.
        let bytes = match args.iter().position(|a| a == "--clone-numspawns") {
            None => bytes,
            Some(q) => {
                let want: f64 = args[q + 1].parse().unwrap();
                let probe = Zone::parse(&bytes).unwrap();
                let mut e2 = ZoneEdits::default();
                for (i, act) in probe.actions.iter().enumerate() {
                    if i < before || probe.action_type(act) != "SpawnerScriptAction" {
                        continue;
                    }
                    if let Some(p) = probe.param(act, "NumSpawns") {
                        e2.script_vars.insert(p.index as usize, want);
                        println!("  spawner {i}: NumSpawns var {} -> {want}", p.index);
                    }
                }
                Zone::parse(&bytes).unwrap().save(&e2).unwrap()
            }
        };

        std::fs::write(&out_path, &bytes).unwrap();
        let re = Zone::parse(&bytes).unwrap();
        println!(
            "cloned -> {out_path} ({} bytes)  nodes {before} -> {}",
            bytes.len(),
            re.actions.len()
        );
        return;
    }

    if let Some(p) = args.iter().position(|a| a == "--test-edit") {
        let out_path = args.get(p + 1).cloned().unwrap_or_else(|| "edited.zone".into());
        let mut edits = ZoneEdits::default();

        // Retune the first spawner blob and double the first enemy's health.
        let spawner = zone
            .script_priuses
            .iter()
            .position(|b| b.json.get("MaxSimultaneousSpawns").is_some())
            .expect("no spawner prius found");
        let mut json = zone.script_priuses[spawner].json.clone();
        json["MaxSimultaneousSpawns"]["Value"] = serde_json::json!(12);
        json["SpawnIntervalMin"]["Value"] = serde_json::json!(0.1);
        edits.script_priuses.insert(spawner, json);

        let bot = zone
            .actor_priuses
            .iter()
            .position(|b| b.json.pointer("/BotBaseData/Value/Health").is_some())
            .expect("no bot prius found");
        let mut json = zone.actor_priuses[bot].json.clone();
        json["BotBaseData"]["Value"]["Health"]["Value"] = serde_json::json!(99);
        edits.actor_priuses.insert(bot, json);

        let enemy = zone
            .actor_asset_paths
            .iter()
            .position(|p| p.contains("enm_pirate_robot_shooter"))
            .expect("no shooter asset found");
        edits.actor_assets.insert(
            enemy,
            "characters\\enemy\\enm_bladeball_ps4\\actors\\enm_pirate_bladeball.actor".into(),
        );

        // Triple the first wave spawner's enemy count.
        let (spawner_node, num_var) = zone
            .actions
            .iter()
            .enumerate()
            .filter(|(_, a)| zone.action_type(a) == "SpawnerScriptAction")
            .find_map(|(i, a)| zone.param(a, "NumSpawns").map(|p| (i, p.index as usize)))
            .expect("no NumSpawns binding found");
        let before = zone.vars[num_var].as_number();
        edits.script_vars.insert(num_var, 9.0);

        // Repoint a portal group binding and a direct portal binding at volumes.
        let volume_group = zone
            .actor_groups
            .iter()
            .find(|g| {
                g.members.iter().any(|m| {
                    zone.actors
                        .get(*m as usize)
                        .and_then(|a| zone.actor_asset_paths.get(a.asset_index as usize))
                        .is_some_and(|p| p.to_ascii_lowercase().contains("spawn_volume"))
                })
            })
            .map(|g| g.id);
        let volume_actor = zone
            .actors
            .iter()
            .find(|a| {
                zone.actor_asset_paths
                    .get(a.asset_index as usize)
                    .is_some_and(|p| p.to_ascii_lowercase().contains("spawn_volume"))
            })
            .map(|a| a.instance_id);
        let portal_group_var = zone.vars.iter().position(|v| {
            volume_group.is_some()
                && zone.actor_groups.iter().any(|g| {
                    g.id == v.id_value()
                        && g.members.iter().any(|m| {
                            zone.actors
                                .get(*m as usize)
                                .and_then(|a| zone.actor_asset_paths.get(a.asset_index as usize))
                                .is_some_and(|p| p.contains("spawn_portal_bot"))
                        })
                })
        });
        let portal_actor_var = zone.vars.iter().position(|v| {
            zone.actors.iter().any(|a| {
                a.instance_id == v.id_value()
                    && v.id_value() != 0
                    && zone
                        .actor_asset_paths
                        .get(a.asset_index as usize)
                        .is_some_and(|p| p.contains("spawn_portal_bot"))
            })
        });
        if let (Some(v), Some(g)) = (portal_group_var, volume_group) {
            edits.script_var_ids.insert(v, format!("{g:016X}"));
            println!("\n  repoint group var {v} -> volume group {g:016X}");
        }
        if let (Some(v), Some(a)) = (portal_actor_var, volume_actor) {
            edits.script_var_ids.insert(v, format!("{a:016X}"));
            println!("  repoint actor var {v} -> volume actor {a:016X}");
        }

        let bytes = zone.save(&edits).unwrap();
        std::fs::write(&out_path, &bytes).unwrap();
        let re = Zone::parse(&bytes).unwrap();
        println!(
            "\n  spawner node {spawner_node} NumSpawns var {num_var}: {before:?} -> {:?}",
            re.vars[num_var].as_number()
        );
        println!("\n--test-edit -> {out_path} ({} bytes)", bytes.len());
        println!("  spawner[{spawner}] = {}", re.script_priuses[spawner].json);
        println!(
            "  bot[{bot}].Health = {:?}",
            re.actor_priuses[bot]
                .json
                .pointer("/BotBaseData/Value/Health/Value")
        );
        println!(
            "  enemy[{enemy}] = {} ({:016X})",
            re.actor_asset_paths[enemy], re.actor_asset_ids[enemy]
        );
        println!("  asset_refs = {}", re.asset_refs.len());
        return;
    }

    if args.iter().any(|a| a == "--roundtrip") {
        let before: Vec<(u32, u32)> = zone.dat1.sections.iter().map(|s| (s.tag, s.size)).collect();
        let pool_before = zone.dat1.strings_pool.len();
        let zone_sizes: Vec<u32> = zone.actor_priuses.iter().map(|b| b.size).collect();
        let out = zone.save(&ZoneEdits::default()).unwrap();
        let after = Zone::parse(&out).unwrap();
        for (tag, size) in before {
            let new = after
                .dat1
                .sections
                .iter()
                .find(|s| s.tag == tag)
                .map(|s| s.size)
                .unwrap_or(0);
            if new != size {
                println!("section {tag:08X}: {size} -> {new}");
            }
        }
        println!(
            "strings pool: {pool_before} -> {}",
            after.dat1.strings_pool.len()
        );
        for (i, b) in after.actor_priuses.iter().enumerate() {
            let old = zone_sizes.get(i).copied().unwrap_or(0);
            if old != b.size {
                println!("actor prius {i}: size {old} -> {}", b.size);
            }
        }
        if out == bytes {
            println!("\nroundtrip: byte-identical ({} bytes)", out.len());
        } else {
            let pos = bytes.iter().zip(out.iter()).position(|(a, b)| a != b);
            println!(
                "\nroundtrip: MISMATCH in={} out={} first_diff={:?}",
                bytes.len(),
                out.len(),
                pos
            );
        }
    }
}
