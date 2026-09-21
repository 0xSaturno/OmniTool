use omnitool_lib::core::math::decode_normal;
use omnitool_lib::tools::model_converter::{
    gltf_reader::{inject_gltf, parse_gltf},
    gltf_writer::model_to_glb_for_looks,
    model::ModelFile,
};

fn tag_name(tag: u32) -> &'static str {
    match tag {
        0x283D0383 => "Model Built",
        0xA98BE69B => "Model Std Vert",
        0x6B855EED => "Model UV1 Vert",
        0x5CBA9DE9 => "Model Col Vert",
        0x0859863D => "Model Index",
        0xCCBAFF15 => "Model GPU Skin",
        0xDCA379A2 => "Model Skin Data",
        0xC61B1FF5 => "Model Skin Batch",
        0x78D9CBDE => "Model Meshes",
        0x06EB7EFC => "Model Look",
        _ => "",
    }
}

fn main() {
    let all: Vec<String> = std::env::args().collect();
    let args: Vec<String> = all.iter().filter(|a| !a.starts_with("--")).cloned().collect();
    let perturb = all.iter().any(|a| a == "--perturb");
    if args.len() < 2 {
        eprintln!("Usage: roundtrip_gltf <model_file.model> [existing.glb] [looks,csv]");
        std::process::exit(1);
    }

    let orig_data = std::fs::read(&args[1]).unwrap();
    let orig = ModelFile::parse(&orig_data).unwrap();

    // Export, unless the caller handed us a glb to re-import.
    let glb_path = if args.len() > 2 && !args[2].is_empty() {
        args[2].clone()
    } else {
        let looks: Vec<usize> = args
            .get(3)
            .map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect())
            .unwrap_or_else(|| vec![0]);
        let glb = model_to_glb_for_looks(&orig, &looks).unwrap();
        let p = std::env::temp_dir().join("roundtrip_gltf.glb");
        std::fs::write(&p, &glb).unwrap();
        println!("exported {} bytes -> {}", glb.len(), p.display());
        p.to_string_lossy().into_owned()
    };

    let mut gltf = parse_gltf(&glb_path).unwrap();
    println!("glb: {} meshes, {} bones", gltf.meshes.len(), gltf.bones.len());

    // --perturb exercises the edited path: nudge one vertex and drop one face
    // of the first mesh, so the model has both a moved and a resized subset.
    if perturb {
        if let Some(m) = gltf.meshes.first_mut() {
            if let Some(v) = m.vertexes.first_mut() {
                v.position.0 += 0.25;
            }
            m.faces.pop();
            println!("perturbed mesh '{}': moved vertex 0, dropped one face", m.name);
        }
    }

    let mut rebuilt = ModelFile::parse(&orig_data).unwrap();
    inject_gltf(&mut rebuilt, &gltf).unwrap();
    let out_bytes = rebuilt.save();
    let rt = ModelFile::parse(&out_bytes).unwrap();

    println!(
        "\nfile size {} -> {} ({:+})",
        orig_data.len(),
        out_bytes.len(),
        out_bytes.len() as i64 - orig_data.len() as i64
    );

    let mut tags: Vec<u32> = orig.dat1.sections.iter().map(|s| s.tag).collect();
    tags.sort();
    println!("\n{:>10} {:>12} {:>12} {:>10}  {:<6} {}", "tag", "orig", "rebuilt", "delta", "equal", "name");
    let mut differing = 0;
    for tag in tags {
        let a = orig.dat1.get_section_data(tag).unwrap_or(&[]);
        let b = rt.dat1.get_section_data(tag).unwrap_or(&[]);
        let equal = a == b;
        if !equal {
            differing += 1;
        }
        println!(
            "{:08X} {:>12} {:>12} {:>10}  {:<6} {}",
            tag,
            a.len(),
            b.len(),
            b.len() as i64 - a.len() as i64,
            if equal { "yes" } else { "NO" },
            tag_name(tag)
        );
    }
    println!("\n{} of {} sections differ", differing, orig.dat1.sections.len());

    if let (Some(a), Some(b)) = (
        orig.dat1.get_section_data(0xA98BE69B),
        rt.dat1.get_section_data(0xA98BE69B),
    ) {
        let n = a.len().min(b.len()) / 16;
        let (mut pos, mut w, mut nrm, mut uv) = (0u32, 0u32, 0u32, 0u32);
        for i in 0..n {
            let (x, y) = (&a[i * 16..i * 16 + 16], &b[i * 16..i * 16 + 16]);
            if x == y {
                continue;
            }
            if x[0..6] != y[0..6] { pos += 1; }
            if x[6..8] != y[6..8] { w += 1; }
            if x[8..12] != y[8..12] { nrm += 1; }
            if x[12..16] != y[12..16] { uv += 1; }
        }
        println!(
            "\nStd Vert field diffs over {} vertices: position={} w={} normal/tangent={} uv={}",
            n, pos, w, nrm, uv
        );

        // How far the repacked normals actually moved, in degrees.
        let mut worst = 0f64;
        let mut over_1deg = 0u32;
        for i in 0..n {
            let (x, y) = (&a[i * 16..i * 16 + 16], &b[i * 16..i * 16 + 16]);
            if x[8..12] == y[8..12] {
                continue;
            }
            let na = decode_normal(u32::from_le_bytes(x[8..12].try_into().unwrap()));
            let nb = decode_normal(u32::from_le_bytes(y[8..12].try_into().unwrap()));
            let dot = (na.0 * nb.0 + na.1 * nb.1 + na.2 * nb.2).clamp(-1.0, 1.0) as f64;
            let deg = dot.acos().to_degrees();
            if deg > 1.0 {
                over_1deg += 1;
            }
            worst = worst.max(deg);
        }
        println!(
            "  normal repack error: worst {:.3} deg, {} vertices over 1 deg",
            worst, over_1deg
        );
    }

    if let (Some(a), Some(b)) = (
        orig.dat1.get_section_data(0x283D0383),
        rt.dat1.get_section_data(0x283D0383),
    ) {
        let f = |d: &[u8], o: usize| f32::from_le_bytes(d[o..o + 4].try_into().unwrap());
        let u = |d: &[u8], o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
        let lods = |d: &[u8]| (0..5).map(|i| f(d, 0x34 + i * 4)).collect::<Vec<_>>();
        println!("\nBUILT lod distances  {:?} -> {:?}", lods(a), lods(b));
        println!("BUILT index_count    {} -> {}", u(a, 0x64), u(b, 0x64));
        println!("BUILT vertex_count   {} -> {}", u(a, 0x68), u(b, 0x68));
    }

    validate(&rt);
}

/// Checks the invariants the game relies on: every subset points inside the
/// pools, the Built totals agree with the section sizes, and absolute indices
/// stay within the subset that owns them.
fn validate(model: &ModelFile) {
    use omnitool_lib::tools::model_converter::sections::meshes::MeshDefinition;

    let built = model.dat1.get_section_data(0x283D0383).unwrap();
    let built_ic = u32::from_le_bytes(built[0x64..0x68].try_into().unwrap()) as usize;
    let built_vc = u32::from_le_bytes(built[0x68..0x6C].try_into().unwrap()) as usize;
    let verts = model.dat1.get_section_data(0xA98BE69B).map(|d| d.len() / 16).unwrap_or(0);
    let idxs = model.dat1.get_section_data(0x0859863D).map(|d| d.len() / 2).unwrap_or(0);
    let idx_data = model.dat1.get_section_data(0x0859863D).unwrap_or(&[]);
    let rcra = model.dat1.get_section_data(0xCCBAFF15).map(|d| d.len() / 8).unwrap_or(0);
    let batches = model.dat1.get_section_data(0xC61B1FF5).map(|d| d.len() / 16).unwrap_or(0);
    let meshes = MeshDefinition::parse_all(model.dat1.get_section_data(0x78D9CBDE).unwrap()).unwrap();

    let mut problems: Vec<String> = Vec::new();
    if built_vc != verts {
        problems.push(format!("BUILT vertex_count {} != Std Vert entries {}", built_vc, verts));
    }
    if built_ic != idxs {
        problems.push(format!("BUILT index_count {} != Index entries {}", built_ic, idxs));
    }
    for (mi, m) in meshes.iter().enumerate() {
        if m.vertex_start as usize + m.vertex_count as usize > verts {
            problems.push(format!("subset {} vertex range past end of pool", mi));
        }
        if m.index_start as usize + m.index_count as usize > idxs {
            problems.push(format!("subset {} index range past end of pool", mi));
        }
        if m.is_rcra_skinned() && m.first_weight_index as usize + m.vertex_count as usize > rcra {
            problems.push(format!("subset {} weight range past end of GPU Skin", mi));
        }
        if m.is_skinned() && m.first_skin_batch as usize > batches {
            problems.push(format!("subset {} first_skin_batch past end of Skin Batch", mi));
        }
        if m.has_relative_indices() {
            continue;
        }
        let (lo, hi) = (m.vertex_start as usize, m.vertex_start as usize + m.vertex_count as usize);
        for k in 0..m.index_count as usize {
            let p = (m.index_start as usize + k) * 2;
            let Some(b) = idx_data.get(p..p + 2) else { continue };
            let v = u16::from_le_bytes([b[0], b[1]]) as usize;
            if v < lo || v >= hi {
                problems.push(format!("subset {} absolute index {} outside its own vertices {}..{}", mi, v, lo, hi));
                break;
            }
        }
    }

    if problems.is_empty() {
        println!("\nvalidation: OK ({} subsets)", meshes.len());
    } else {
        println!("\nvalidation: {} PROBLEMS", problems.len());
        for p in problems.iter().take(12) {
            println!("  {}", p);
        }
    }
}
