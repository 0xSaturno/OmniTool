//! Read an .actor file, dump its JSON representation, then save back and
//! verify the roundtrip parses to identical JSON.
//!
//! Usage: roundtrip_actor <path.actor> [reference.actor.json]

use omnitool_lib::core::config::ConfigFile;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: roundtrip_actor <path.actor> [reference.actor.json]");
        std::process::exit(1);
    }
    let path = PathBuf::from(&args[1]);
    let reference_json_path = args.get(2).map(PathBuf::from);

    let original = std::fs::read(&path).expect("read actor");
    println!("[1] Read {} ({} bytes)", path.display(), original.len());

    // Parse.
    let cfg = ConfigFile::parse(&original).expect("parse");
    println!("    config_type = {:?}", cfg.config_type);
    let parsed_json = serde_json::to_string_pretty(&cfg.content).expect("ser");

    // Write the parsed JSON next to the .actor for inspection.
    let dump_path = path.with_extension("actor.amp.json");
    std::fs::write(&dump_path, &parsed_json).expect("write json");
    println!("[2] Wrote our parsed JSON -> {}", dump_path.display());

    // Compare to the reference (closed-source converter output) if provided.
    let original_content = cfg.content.clone();
    if let Some(ref_path) = reference_json_path.as_ref() {
        let ref_text = std::fs::read_to_string(ref_path).expect("read reference json");
        let ref_value: serde_json::Value =
            serde_json::from_str(&ref_text).expect("parse reference json");
        if original_content == ref_value {
            println!(
                "[3] PARSE MATCH: our JSON == reference {}",
                ref_path.display()
            );
        } else {
            println!(
                "[3] PARSE DIFF vs reference {} (see *.actor.amp.json vs reference)",
                ref_path.display()
            );
        }
    } else {
        println!("[3] no reference JSON supplied; skipping parse-match check");
    }

    // Roundtrip: save and re-parse.
    let saved = cfg.save().expect("save");
    let saved_path = path.with_extension("actor.amp.out");
    std::fs::write(&saved_path, &saved).expect("write saved");
    println!(
        "[4] Wrote re-saved actor -> {} ({} bytes)",
        saved_path.display(),
        saved.len()
    );

    let cfg2 = ConfigFile::parse(&saved).expect("parse re-saved");
    let parsed2_json = serde_json::to_string_pretty(&cfg2.content).expect("ser re-saved");

    if original_content == cfg2.content {
        println!("[5] ROUNDTRIP OK: re-parsed JSON identical to original parse");
    } else {
        println!("[5] ROUNDTRIP DIFF (see *.actor.amp.json vs *.actor.amp.json2)");
        let diff_path = path.with_extension("actor.amp.json2");
        std::fs::write(&diff_path, &parsed2_json).expect("write diff json");
    }

    println!(
        "    original size: {}, re-saved size: {}",
        original.len(),
        saved.len()
    );
    if original == saved {
        println!("    BYTE-EXACT roundtrip");
    } else {
        let mut first_diff = original.len().min(saved.len());
        for i in 0..original.len().min(saved.len()) {
            if original[i] != saved[i] {
                first_diff = i;
                break;
            }
        }
        println!(
            "    bytes differ; first diff at offset 0x{:X}; lengths differ by {}",
            first_diff,
            saved.len() as i64 - original.len() as i64
        );
    }
}
