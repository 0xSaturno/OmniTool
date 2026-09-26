//! usage: inject_once <mesh.ascii|mesh.glb|mesh.gltf> <base_file.model> <out_file.model>
//!
//! glTF imports use `<mesh>.omni.json` options when present, then print how many triangles of
//! each subset wind counter-clockwise around their normals (game order; should be ~all).

use omnitool_lib::tools::model_converter::{
    ascii_reader::{inject_ascii, parse_ascii},
    gltf_import::{import_gltf, ImportOptions},
    gltf_reader::parse_gltf,
    model::ModelFile,
    sections::{
        geo::{IndexesSection, VertexesSection, TAG_INDEXES, TAG_VERTEXES},
        meshes::{MeshDefinition, TAG_MESHES},
    },
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("Usage: inject_once <mesh.ascii|mesh.glb|mesh.gltf> <base_file.model> <out_file.model>");
        std::process::exit(1);
    }

    let model_data = std::fs::read(&args[2]).expect("failed to read base model file");
    let mut model = ModelFile::parse(&model_data).expect("failed to parse base model");

    let lower = args[1].to_ascii_lowercase();
    if lower.ends_with(".glb") || lower.ends_with(".gltf") {
        let gltf = parse_gltf(&args[1]).expect("failed to parse gltf");
        let opts: ImportOptions = std::fs::read_to_string(format!("{}.omni.json", args[1]))
            .ok()
            .map(|t| serde_json::from_str(&t).expect("bad .omni.json"))
            .unwrap_or_default();
        let report = import_gltf(&mut model, &gltf, &opts).expect("failed to import gltf");
        for s in &report.subsets {
            println!("subset {:>3} <- '{}' {:?}", s.subset, s.primitive, s.tier);
        }
        for w in &report.warnings {
            println!("warn {w}");
        }
    } else {
        let ascii_text = std::fs::read_to_string(&args[1]).expect("failed to read ascii file");
        let ascii = parse_ascii(&ascii_text).expect("failed to parse ascii");
        inject_ascii(&mut model, &ascii).expect("failed to inject ascii");
    }

    let out = model.save();
    std::fs::write(&args[3], &out).expect("failed to write output model");

    let model = ModelFile::parse(&out).expect("failed to reparse output");
    let d = &model.dat1;
    let meshes = MeshDefinition::parse_all(d.get_section_data(TAG_MESHES).unwrap()).unwrap();
    let verts = VertexesSection::parse(d.get_section_data(TAG_VERTEXES).unwrap()).unwrap().vertexes;
    let idx = IndexesSection::parse(d.get_section_data(TAG_INDEXES).unwrap()).unwrap().values;
    for (i, m) in meshes.iter().enumerate() {
        let base = if m.has_relative_indices() { m.vertex_start as usize } else { 0 };
        let (mut with, mut total) = (0usize, 0usize);
        for t in idx[m.index_start as usize..(m.index_start + m.index_count) as usize].chunks_exact(3) {
            let [a, b, c] = [0, 1, 2].map(|k| &verts[base + t[k] as usize]);
            let (u, v) = ([b.x - a.x, b.y - a.y, b.z - a.z], [c.x - a.x, c.y - a.y, c.z - a.z]);
            let g = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
            let n = [a.nx + b.nx + c.nx, a.ny + b.ny + c.ny, a.nz + b.nz + c.nz];
            total += 1;
            with += (g[0] * n[0] + g[1] * n[1] + g[2] * n[2] > 0.0) as usize;
        }
        println!("subset {i:>3}: {with}/{total} triangles wind with their normals");
    }
}
