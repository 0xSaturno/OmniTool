use std::path::Path;

use omnitool_lib::core::material::{
    MaterialFile, MaterialHeaderSection, MaterialSerialized, TAG_MATERIAL_HEADER,
    TAG_MATERIAL_SERIALIZED,
};

fn check(path: &Path) -> Result<(), String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let mut mat = MaterialFile::parse(&data).map_err(|e| e.to_string())?;

    if let Some(sec) = mat.dat1.get_section_data(TAG_MATERIAL_HEADER) {
        let parsed = MaterialHeaderSection::parse(sec).map_err(|e| e.to_string())?;
        if parsed.build() != sec {
            return Err("header section did not round-trip".into());
        }
    }

    if let Some(sec) = mat.dat1.get_section_data(TAG_MATERIAL_SERIALIZED) {
        let parsed = MaterialSerialized::parse(sec).map_err(|e| e.to_string())?;
        let rebuilt = parsed.build();
        if rebuilt != sec {
            return Err(format!(
                "serialized section differs: {} bytes in, {} out ({} constants, {} samplers)",
                sec.len(),
                rebuilt.len(),
                parsed.constants.len(),
                parsed.samplers.len()
            ));
        }
    }

    let out = mat.save();
    if out != data {
        return Err(format!("file differs: {} bytes in, {} out", data.len(), out.len()));
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: roundtrip_material <file-or-dir>...");
        std::process::exit(1);
    }

    let mut files = Vec::new();
    for a in &args {
        let p = Path::new(a);
        if p.is_dir() {
            for entry in walkdir::WalkDir::new(p).into_iter().filter_map(|e| e.ok()) {
                if entry.path().extension().is_some_and(|e| e == "material") {
                    files.push(entry.path().to_path_buf());
                }
            }
        } else {
            files.push(p.to_path_buf());
        }
    }

    let (mut ok, mut bad) = (0, 0);
    for f in &files {
        match check(f) {
            Ok(()) => ok += 1,
            Err(e) => {
                bad += 1;
                println!("FAIL {}: {}", f.display(), e);
            }
        }
    }
    println!("{ok} ok, {bad} failed, {} total", files.len());
}
