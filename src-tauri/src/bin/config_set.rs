//! Sets string fields of a config and saves it.
//!
//! usage: config_set <in.config> <out.config> [@content.json] [<Field.Path=value> ...]
//!
//! A path walks Main's objects by field name, e.g. `AmmoList.Ammo.Obj.AmmoItemConfig`.
//! `@content.json` (typed JSON as roundtrip_actor writes it) replaces the whole content and type first,
//! so `in.config` only lends its container: a config of another game or type saved in Rift Apart's.

use omnitool_lib::core::config::ConfigFile;
use serde_json::Value;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("usage: config_set <in.config> <out.config> [@content.json] [<Field.Path=value> ...]");
        std::process::exit(1);
    }
    let mut cfg = ConfigFile::parse(&std::fs::read(&args[0]).expect("read config")).expect("parse config");
    let mut edits = &args[2..];
    if let Some(json) = edits.first().and_then(|a| a.strip_prefix('@')) {
        let content: Value = serde_json::from_str(&std::fs::read_to_string(json).expect("read content json")).expect("parse content json");
        // Configs carry their type in Header; actors have none and keep the container's.
        if let Some(ty) = content["Header"]["Type"]["Value"].as_str() {
            cfg.config_type = ty.to_string();
        }
        cfg.content = content;
        edits = &edits[1..];
    }
    for edit in edits {
        let (path, value) = edit.split_once('=').expect("edits are Field.Path=value");
        let mut node = cfg.content.get_mut("Main").expect("config has no Main");
        for part in path.split('.') {
            // Typed values wrap their content as {"Type": ..., "Value": ...}.
            if node.get("Type").is_some() && node.get("Value").is_some() {
                node = node.get_mut("Value").unwrap();
            }
            node = node.get_mut(part).unwrap_or_else(|| panic!("no field {part} in Main.{path}"));
        }
        assert_eq!(node.get("Type").and_then(Value::as_str), Some("STRING"), "Main.{path} is not a string field");
        println!("{path}: {} -> {value}", node["Value"]);
        node["Value"] = Value::String(value.to_string());
    }
    std::fs::write(&args[1], cfg.save().expect("save config")).expect("write config");
}
