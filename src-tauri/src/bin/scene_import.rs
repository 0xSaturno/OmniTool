//! usage: scene_import <scene.glb> <donor.model> <host.zone> <zone asset path> <model asset dir> <out mod dir>
//!                     [<donor.material> <donor.texture>]
//!
//! Writes a loose mod folder: one model per mesh under the model dir, the host zone with one
//! instance per node at its asset path, and `<zone>.obj` beside it. Import options (material
//! paths) come from `<scene.glb>.omni.json` when present. With the two surface donors, every glTF
//! material that has a base colour image also gets its own material and texture.

use std::path::Path;

use omnitool_lib::tools::model_converter::gltf_import::ImportOptions;
use omnitool_lib::tools::scene_import::import_scene;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 7 && args.len() != 9 {
        eprintln!("Usage: scene_import <scene.glb> <donor.model> <host.zone> <zone asset path> <model asset dir> <out mod dir> [<donor.material> <donor.texture>]");
        std::process::exit(1);
    }
    let surface: Vec<Vec<u8>> = args[7..].iter().map(|p| std::fs::read(p).expect("failed to read surface donor")).collect();
    let surface = (surface.len() == 2).then(|| (surface[0].as_slice(), surface[1].as_slice()));
    let donor = std::fs::read(&args[2]).expect("failed to read donor model");
    let zone = std::fs::read(&args[3]).expect("failed to read host zone");
    let opts: ImportOptions = std::fs::read_to_string(format!("{}.omni.json", args[1]))
        .ok()
        .map(|t| serde_json::from_str(&t).expect("bad .omni.json"))
        .unwrap_or_default();

    let out = import_scene(&args[1], &donor, &zone, &args[4], &args[5], surface, &opts).expect("scene import failed");
    for (asset, bytes) in &out.files {
        let file = Path::new(&args[6]).join(asset.replace('\\', "/"));
        std::fs::create_dir_all(file.parent().unwrap()).expect("failed to create folder");
        std::fs::write(&file, bytes).expect("failed to write file");
    }
    for (w, n) in &out.warnings {
        println!("warn x{n}: {w}");
    }
    println!(
        "{} files: {} models, {} materials, {} instances, {} collision triangles",
        out.files.len(),
        out.models,
        out.materials,
        out.instances,
        out.collision_triangles
    );
}
