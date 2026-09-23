//! Does the tool still name assets with no external hash list?
//!
//!   cargo run --bin name_source_check -- "<game dir>"
//!
//! The game ships a name for every asset it knows in the `dag` beside `toc`, so
//! the downloaded hash list should be an optional extra rather than a hard
//! requirement. This points the index at a hash path that does not exist and
//! checks that zones, regions and models still come out named.

use omnitool_lib::core::level;

fn main() {
    let game = std::env::args()
        .nth(1)
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());

    let missing = r"Z:\definitely\not\a\hashes\file.txt";
    let idx = level::get_index(&game, Some(missing)).expect("index without a hash list");

    let zones = idx.zones.len();
    let named_zones = idx.zones.iter().filter(|z| !z.path.is_empty()).count();
    let regions = idx.regions.len();
    let models = idx
        .names
        .values()
        .filter(|p| p.ends_with(".model"))
        .count();

    println!("hash list: {missing} (absent)");
    println!("names resolved : {}", idx.names.len());
    println!("zones          : {zones} ({named_zones} with a path)");
    println!("regions        : {regions}");
    println!("models named   : {models}");

    let sample: Vec<&str> = idx.zones.iter().take(3).map(|z| z.path.as_str()).collect();
    for s in &sample {
        println!("  e.g. {s}");
    }

    let ok = zones > 8000 && regions > 300 && models > 10000 && named_zones == zones;
    println!("\n{}", if ok { "PASS - the dag alone names the world" } else { "FAIL" });
    if !ok {
        std::process::exit(1);
    }
}
