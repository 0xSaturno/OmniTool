//! Which model instances of a region does the viewport NOT draw, and why?
//!
//!   cargo run --bin missing_probe -- "<game dir>" [region substring] [offset]
//!
//! Walks every loadable zone of a region, resolves each kind-0 instance, and
//! bins it by the reason it would or would not reach the screen:
//!
//!   drawn            decoded to a non-empty mesh
//!   skipped-collision model flagged CollisionOnly, deliberately not drawn
//!   no-toc-asset     the model id is not in this build's archives
//!   empty-mesh       decoded, but the look/LOD selection yielded no triangles
//!   parse-failed     the asset would not parse
//!
//! Reasons other than "drawn" are reported per model with instance counts and
//! world extents, largest first, so a whole missing area is obvious.
//!
//! An optional third argument takes a scene-object offset (decimal or 0x-hex),
//! including a composite offset from the viewport, and reports what sits there.

use omnitool_lib::core::level::{self, ModelBuilt};
use omnitool_lib::core::zone::{Zone, KIND_MODEL};
use std::collections::HashMap;

/// Mirrors the shell's composite-scene stride (`1 << 27`).
const OFFSET_STRIDE: u64 = 1 << 27;

#[derive(Default, Clone)]
struct Bin {
    instances: usize,
    zones: std::collections::HashSet<String>,
    min: [f32; 3],
    max: [f32; 3],
    seeded: bool,
}

impl Bin {
    fn add(&mut self, zone: &str, p: [f32; 3]) {
        self.instances += 1;
        self.zones.insert(zone.to_string());
        if !self.seeded {
            self.min = p;
            self.max = p;
            self.seeded = true;
        } else {
            for i in 0..3 {
                self.min[i] = self.min[i].min(p[i]);
                self.max[i] = self.max[i].max(p[i]);
            }
        }
    }
    fn span(&self) -> f32 {
        if !self.seeded {
            return 0.0;
        }
        (self.max[0] - self.min[0])
            .max(self.max[1] - self.min[1])
            .max(self.max[2] - self.min[2])
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let game = args
        .next()
        .unwrap_or_else(|| r"F:\SteamLibrary\steamapps\common\Ratchet & Clank - Rift Apart".into());
    let want = args.next().unwrap_or_else(|| "nefarious_city".into());
    let probe_offset = args.next().and_then(|s| {
        let t = s.trim();
        if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
            u64::from_str_radix(h, 16).ok()
        } else {
            t.parse::<u64>().ok()
        }
    });
    let hashes = std::env::var("RCRA_HASHES").unwrap_or_default();
    let idx = level::get_index(&game, if hashes.is_empty() { None } else { Some(&hashes) })
        .expect("index");

    let zones: Vec<_> = idx
        .zones
        .iter()
        .filter(|z| z.path.contains(&want) && level::zone_has_objects(&idx, z.id))
        .map(|z| (z.path.clone(), z.id))
        .collect();
    println!("{} loadable zones matching {want:?}\n", zones.len());

    let mut reasons: HashMap<&'static str, HashMap<u64, Bin>> = HashMap::new();
    // Parse every zone once, in parallel. Slot order matches the shell: the
    // order zones are listed in.
    use rayon::prelude::*;
    let parsed: Vec<(usize, &String, Option<Zone>)> = zones
        .par_iter()
        .enumerate()
        .map(|(slot, (path, id))| {
            let zone = idx
                .extract(*id)
                .ok()
                .and_then(|raw| Zone::parse(&raw).ok());
            (slot, path, zone)
        })
        .collect();

    // Every distinct model, judged ONCE. Judging per instance instead means
    // thousands of redundant extracts and decodes — that is what made the first
    // version of this probe unable to finish a region inside ten minutes.
    let mut distinct: Vec<u64> = parsed
        .iter()
        .filter_map(|(_, _, z)| z.as_ref())
        .flat_map(|z| z.scene_objects.iter())
        .filter(|o| o.kind == KIND_MODEL)
        .map(|o| o.model_id)
        .collect();
    distinct.sort_unstable();
    distinct.dedup();
    println!("judging {} distinct models...", distinct.len());

    let verdicts: HashMap<u64, &'static str> = distinct
        .par_iter()
        .map(|&mid| {
            let reason = match idx.asset(mid) {
                None => "no-toc-asset",
                Some(_) => match idx
                    .extract(mid)
                    .ok()
                    .and_then(|b| level::parse_model_asset(&b).ok())
                {
                    None => "parse-failed",
                    Some(model) => {
                        if ModelBuilt::of(&model).is_some_and(|b| b.collision_only()) {
                            "skipped-collision"
                        } else {
                            match level::decode_model_mesh(&model, 0) {
                                Ok(m) if !m.indices.is_empty() => "drawn",
                                Ok(_) => "empty-mesh",
                                Err(_) => "parse-failed",
                            }
                        }
                    }
                },
            };
            (mid, reason)
        })
        .collect();

    let mut total = 0usize;
    for (slot, path, zone) in &parsed {
        let Some(zone) = zone else { continue };
        for o in &zone.scene_objects {
            if o.kind != KIND_MODEL {
                continue;
            }
            total += 1;

            if let Some(want_off) = probe_offset {
                let composite = *slot as u64 * OFFSET_STRIDE + o.offset as u64;
                if composite == want_off || o.offset as u64 == want_off {
                    println!("=== object at {want_off} ===");
                    println!("  zone      {path}  (slot {slot})");
                    println!("  offset    {} (0x{:X})", o.offset, o.offset);
                    println!("  model     {} ({:#018X})", o.model_path, o.model_id);
                    println!("  position  {:?}", o.position());
                    println!("  scale     {:?}", o.scale);
                    println!("  verdict   {}\n", verdicts.get(&o.model_id).copied().unwrap_or("?"));
                }
            }

            let reason = verdicts.get(&o.model_id).copied().unwrap_or("parse-failed");
            reasons
                .entry(reason)
                .or_default()
                .entry(o.model_id)
                .or_default()
                .add(path, o.position());
        }
    }

    println!("{total} model instances across {} zones\n", zones.len());
    let mut keys: Vec<_> = reasons.keys().copied().collect();
    keys.sort();
    for k in keys {
        let bins = &reasons[k];
        let insts: usize = bins.values().map(|b| b.instances).sum();
        println!("{k:<18} {insts:>7} instances over {} distinct models", bins.len());
    }

    for k in ["skipped-collision", "no-toc-asset", "empty-mesh", "parse-failed"] {
        let Some(bins) = reasons.get(k) else { continue };
        println!("\n--- {k} ---");
        let mut v: Vec<_> = bins.iter().collect();
        v.sort_by_key(|(_, b)| std::cmp::Reverse(b.instances));
        for (id, b) in v.iter().take(12) {
            let name = idx.name_of(**id).unwrap_or("<unnamed>");
            println!(
                "  {:>6} inst  span {:>8.1} m  {} zones  {}",
                b.instances,
                b.span(),
                b.zones.len(),
                name
            );
        }
        if v.len() > 12 {
            println!("  ... and {} more models", v.len() - 12);
        }
    }
}
