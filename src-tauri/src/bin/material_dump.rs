use omnitool_lib::core::material::{
    MaterialFile, MaterialHeaderSection, MaterialSerialized, TAG_MATERIAL_HEADER,
    TAG_MATERIAL_SERIALIZED,
};
use omnitool_lib::core::material_graph::MaterialTemplate;
use omnitool_lib::core::material_names;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: material_dump <file.material> [file.materialgraph]");
        std::process::exit(1);
    }

    let data = std::fs::read(&args[0]).expect("read material");
    let mut material = MaterialFile::parse(&data).expect("parse material");

    // `--write <out>` exercises the same section-replacement path save_material uses.
    if let Some(i) = args.iter().position(|a| a == "--write") {
        let out = args.get(i + 1).expect("--write needs a path");
        let sec = material
            .dat1
            .get_section_data(TAG_MATERIAL_SERIALIZED)
            .expect("no serialized section");
        let mut parsed = MaterialSerialized::parse(sec).expect("parse serialized");
        if let Some(c) = parsed.constants.first_mut() {
            c.values[0] = 0.125;
        }
        if let Some(s) = parsed.samplers.first_mut() {
            s.path = "characters/hero/textures/edited_test_c.texture".into();
        }
        let rebuilt = parsed.build();
        material
            .dat1
            .set_section_data(TAG_MATERIAL_SERIALIZED, rebuilt)
            .expect("set section");
        std::fs::write(out, material.save()).expect("write");
        println!("wrote {out}");
        return;
    }
    println!("template: {}", material.template_path().unwrap_or_else(|| "<none>".into()));

    let template = args.get(1).map(|p| {
        let bytes = std::fs::read(p).expect("read materialgraph");
        MaterialTemplate::parse(&bytes).expect("parse materialgraph")
    });

    if let Some(sec) = material.dat1.get_section_data(TAG_MATERIAL_HEADER) {
        let h = MaterialHeaderSection::parse(sec).expect("parse header");
        println!(
            "header: flags={:#010X} av={} audio={:#010X} f14={} f1c={}",
            h.flags,
            material_names::label(h.av_material_hash),
            h.audio_material_hash,
            h.unk14,
            h.unk1c
        );
    }

    if let Some(sec) = material.dat1.get_section_data(TAG_MATERIAL_SERIALIZED) {
        let s = MaterialSerialized::parse(sec).expect("parse serialized");
        println!("samplers ({}):", s.samplers.len());
        for e in &s.samplers {
            let slot = template
                .as_ref()
                .and_then(|t| t.samplers.iter().find(|x| x.name_hash == e.name_hash));
            println!(
                "  [{}] {:24} {}{}",
                slot.map(|x| x.slot_index.to_string()).unwrap_or_else(|| "-".into()),
                material_names::label(e.name_hash),
                e.path,
                slot.map(|x| format!("   (default {})", x.default_path)).unwrap_or_default()
            );
        }
        println!("constants ({}):", s.constants.len());
        for e in &s.constants {
            let slot = template
                .as_ref()
                .and_then(|t| t.constants.iter().find(|x| x.name_hash == e.name_hash));
            println!(
                "  {:28} {:?}{}",
                material_names::label(e.name_hash),
                e.values,
                slot.map(|x| format!("   (default {:?})", x.default_values)).unwrap_or_default()
            );
        }
    }

    if let Some(t) = &template {
        let unused: Vec<String> = t
            .samplers
            .iter()
            .filter(|ts| {
                material
                    .dat1
                    .get_section_data(TAG_MATERIAL_SERIALIZED)
                    .and_then(|sec| MaterialSerialized::parse(sec).ok())
                    .map(|s| !s.samplers.iter().any(|x| x.name_hash == ts.name_hash))
                    .unwrap_or(true)
            })
            .map(|ts| material_names::label(ts.name_hash))
            .collect();
        println!("template-only sampler slots: {unused:?}");
    }
}
