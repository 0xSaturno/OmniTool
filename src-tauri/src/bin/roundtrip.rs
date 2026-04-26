use omnitool_lib::tools::model_converter::{
    model::ModelFile,
    ascii_writer::model_to_ascii,
    ascii_reader::{parse_ascii, inject_ascii},
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: roundtrip <model_file.model>");
        std::process::exit(1);
    }

    let orig_data = std::fs::read(&args[1]).unwrap();

    // Step 1: model -> ascii
    let model = ModelFile::parse(&orig_data).unwrap();
    let ascii_text = model_to_ascii(&model, 0).unwrap();

    // Step 2: ascii -> inject back into model
    let ascii = parse_ascii(&ascii_text).unwrap();
    let mut model2 = ModelFile::parse(&orig_data).unwrap();
    inject_ascii(&mut model2, &ascii).unwrap();
    let mod_bytes = model2.save();

    // Step 3: Compare vertex sections
    let orig_model = ModelFile::parse(&orig_data).unwrap();
    let rt_model = ModelFile::parse(&mod_bytes).unwrap();

    let ov = orig_model.dat1.get_section_data(0xA98BE69B).unwrap();
    let mv = rt_model.dat1.get_section_data(0xA98BE69B).unwrap();
    let vc = ov.len().min(mv.len()) / 16;

    let mut pos_diff = 0u32;
    let mut norm_diff = 0u32;
    let mut uv_diff = 0u32;
    let mut w_diff = 0u32;
    let mut samples = 0;

    for i in 0..vc {
        let o = &ov[i*16..(i+1)*16];
        let m = &mv[i*16..(i+1)*16];
        if o == m { continue; }

        let pos_changed = o[0..6] != m[0..6];
        let w_changed = o[6..8] != m[6..8];
        let norm_changed = o[8..12] != m[8..12];
        let uv_changed = o[12..16] != m[12..16];

        if pos_changed { pos_diff += 1; }
        if norm_changed { norm_diff += 1; }
        if uv_changed { uv_diff += 1; }
        if w_changed { w_diff += 1; }

        if samples < 5 && (pos_changed || norm_changed) {
            let ox = i16::from_le_bytes([o[0],o[1]]);
            let mx = i16::from_le_bytes([m[0],m[1]]);
            let oy = i16::from_le_bytes([o[2],o[3]]);
            let my = i16::from_le_bytes([m[2],m[3]]);
            let oz = i16::from_le_bytes([o[4],o[5]]);
            let mz = i16::from_le_bytes([m[4],m[5]]);
            println!("v[{}]: pos ({},{},{})->({},{},{}) d=({},{},{})",
                i, ox,oy,oz, mx,my,mz, mx as i32-ox as i32, my as i32-oy as i32, mz as i32-oz as i32);
            samples += 1;
        }
    }
    println!("\nTotal verts: {} | pos:{} norm:{} uv:{} w:{}", vc, pos_diff, norm_diff, uv_diff, w_diff);

    // Also check skin sections
    for (tag, name) in [(0xCCBAFF15u32, "rcra_skin"), (0xDCA379A2, "skin_data"), (0xC61B1FF5, "skin_batch")] {
        let os = orig_model.dat1.get_section_data(tag);
        let ms = rt_model.dat1.get_section_data(tag);
        match (os, ms) {
            (Some(o), Some(m)) => {
                let diffs = o.iter().zip(m.iter()).filter(|(a,b)| a != b).count();
                println!("{}: {}/{} bytes differ", name, diffs, o.len());
            }
            _ => println!("{}: missing in one", name),
        }
    }
}
