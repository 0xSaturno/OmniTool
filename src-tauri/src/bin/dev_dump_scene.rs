//! Dev helper: dump one real zone as the exact JSON the `level_load_zone`
//! command returns, plus the region list, the zone list of its region and the
//! RCMS mesh buffer of every model it uses. Lets the Level Editor front end be
//! exercised against real game data without a Tauri host, and gives a quick way
//! to inspect what the commands actually return.
//!
//!   cargo run --bin dev_dump_scene -- "<game dir>" [zone path] [out dir]
//!
//! The asset name list comes from $RCRA_HASHES, else OmniTool's app-dir hashes file.

use omnitool_lib::commands_level_editor as cle;
use omnitool_lib::core::level;

fn main() {
    let mut args = std::env::args().skip(1);
    let game = args
        .next()
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let zone_path = args
        .next()
        .unwrap_or_else(|| "levels/i29/openarea/sargasso/global_art.zone".into());
    let out_dir = args.next().unwrap_or_else(|| "devdata".into());

    let hashes = std::env::var("RCRA_HASHES").unwrap_or_else(|_| {
        omnitool_lib::core::filesystem::hashes_path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    let scene = tauri::async_runtime::block_on(cle::level_load_zone(
        game.clone(),
        zone_path.clone(),
        Some(hashes.clone()),
    ))
    .expect("load zone");

    let json = serde_json::to_vec_pretty(&scene).expect("serialize scene");
    let scene_file = format!("{out_dir}/scene.json");
    std::fs::write(&scene_file, &json).expect("write scene.json");
    println!(
        "{zone_path}\n  {} objects, {} models, {} actors -> {scene_file} ({} KiB)",
        scene.objects.len(),
        scene.models.len(),
        scene.actors.len(),
        json.len() / 1024
    );

    // Region list and the zone list of the region this zone belongs to, so the
    // full editor shell can be exercised in a browser with a static invoke shim.
    let regions = tauri::async_runtime::block_on(cle::level_list_planets(
        game.clone(),
        Some(hashes.clone()),
    ))
    .expect("list planets");
    std::fs::write(
        format!("{out_dir}/planets.json"),
        serde_json::to_vec_pretty(&regions).unwrap(),
    )
    .unwrap();
    let region_key = regions
        .iter()
        .filter(|r| zone_path.starts_with(&format!("{}/", r.key.replace("/openarea/", "/openarea/"))) || zone_path.contains(&r.key))
        .max_by_key(|r| r.key.len())
        .map(|r| r.key.clone())
        .unwrap_or_else(|| "i29/openarea/sargasso".to_string());
    let zones = tauri::async_runtime::block_on(cle::level_list_zones(
        game.clone(),
        region_key.clone(),
        Some(hashes.clone()),
    ))
    .expect("list zones");
    std::fs::write(
        format!("{out_dir}/zones.json"),
        serde_json::to_vec_pretty(&zones).unwrap(),
    )
    .unwrap();
    println!(
        "  {} regions -> planets.json; region {region_key} has {} zones -> zones.json",
        regions.len(),
        zones.len()
    );

    let index = level::get_index(&game, Some(&hashes)).expect("index");
    let mut total = 0usize;
    let mut written = 0usize;
    for m in &scene.models {
        let id = u64::from_str_radix(m.id.trim_start_matches("0x").trim_start_matches("0X"), 16)
            .expect("model id");
        let bytes = level::model_mesh_bytes(&index, id, 0);
        let file = format!("{out_dir}/mesh_{}.bin", m.id.to_lowercase());
        std::fs::write(&file, &*bytes).expect("write mesh");
        total += bytes.len();
        written += 1;
    }
    println!("  {written} meshes, {} KiB total", total / 1024);
    println!("  open http://localhost:1420/devpreview.html");
}
