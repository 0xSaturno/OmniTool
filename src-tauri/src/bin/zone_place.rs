//! usage: zone_place <in.zone> <out.zone> [<model asset path> <model file> <x> <y> <z>]...
//!
//! Adds one static instance per group (no rotation, scale 1) with bounds from the model file's
//! Built section. With no group it only re-saves the zone, which should give the same bytes.

use omnitool_lib::core::zone::{ModelPlacement, Zone, ZoneEdits};
use omnitool_lib::tools::model_converter::{
    model::ModelFile,
    sections::built::{Built, TAG_BUILT},
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 || (args.len() - 3) % 5 != 0 {
        eprintln!("Usage: zone_place <in.zone> <out.zone> [<model asset path> <model file> <x> <y> <z>]...");
        std::process::exit(1);
    }
    let input = std::fs::read(&args[1]).expect("failed to read zone");
    let mut zone = Zone::parse(&input).expect("failed to parse zone");

    // Every slot first: a new model renumbers the ones behind it.
    let groups: Vec<&[String]> = args[3..].chunks(5).collect();
    for g in &groups {
        zone.model_slot(&g[0]).expect("failed to add model");
    }
    let mut placements = Vec::new();
    for g in &groups {
        let model = ModelFile::parse(&std::fs::read(&g[1]).expect("failed to read model")).expect("failed to parse model");
        let b = model.dat1.get_section_data(TAG_BUILT).and_then(Built::parse).expect("model has no Built section");
        let c = b.bsphere_center;
        placements.push(ModelPlacement {
            model: zone.model_slot(&g[0]).unwrap(),
            axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            position: [2, 3, 4].map(|k| g[k].parse().expect("bad coordinate")),
            bsphere: [c[0], c[1], c[2], b.bsphere_radius],
            half_extents: b.aabb_extents,
        });
    }
    zone.add_model_instances(&placements).expect("failed to add instances");
    for (i, n) in zone.model_names.iter().enumerate() {
        println!("slot {i}: {:016X} {n}", zone.model_ids[i]);
    }

    let out = zone.save(&ZoneEdits::default()).expect("failed to save zone");
    Zone::parse(&out).expect("failed to reparse output");
    std::fs::write(&args[2], &out).expect("failed to write zone");
    let same = if out == input { ", identical" } else { "" };
    println!("{} instance(s) added, {} -> {} bytes{same}", placements.len(), input.len(), out.len());
}
