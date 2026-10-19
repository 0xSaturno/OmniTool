//! usage: model_import <model.glb> <donor.model> <model asset path> <out mod dir> [<donor.material> <donor.texture>]
//!
//! Writes a loose mod folder holding the donor model with the glTF's meshes and skin weights in
//! place of its geometry, at the asset path. Import options come from `<model.glb>.omni.json` when
//! present. With the two surface donors, every glTF material with a base colour also gets its own
//! material (and texture) beside the model.

use std::path::Path;

use omnitool_lib::tools::model_converter::gltf_import::ImportOptions;
use omnitool_lib::tools::scene_import::import_model;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 && args.len() != 7 {
        eprintln!("Usage: model_import <model.glb> <donor.model> <model asset path> <out mod dir> [<donor.material> <donor.texture>]");
        std::process::exit(1);
    }
    let surface: Vec<Vec<u8>> = args[5..].iter().map(|p| std::fs::read(p).expect("failed to read surface donor")).collect();
    let surface = (surface.len() == 2).then(|| (surface[0].as_slice(), surface[1].as_slice()));
    let donor = std::fs::read(&args[2]).expect("failed to read donor model");
    let opts: ImportOptions = std::fs::read_to_string(format!("{}.omni.json", args[1]))
        .ok()
        .map(|t| serde_json::from_str(&t).expect("bad .omni.json"))
        .unwrap_or_default();

    let out = import_model(&args[1], &donor, &args[3], surface, &opts).expect("model import failed");
    for (asset, bytes) in &out.files {
        let file = Path::new(&args[4]).join(asset.replace('\\', "/"));
        std::fs::create_dir_all(file.parent().unwrap()).expect("failed to create folder");
        std::fs::write(&file, bytes).expect("failed to write file");
    }
    for (w, n) in &out.warnings {
        println!("warn x{n}: {w}");
    }
    println!("{} files: {} model, {} materials", out.files.len(), out.models, out.materials);
}
