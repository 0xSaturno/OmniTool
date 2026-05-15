//! Diagnostic: verify CRC64 hashes for actor component instance IDs.
use omnitool_lib::core::{crc32, crc64};

fn main() {
    // Known mappings from wpn_ryno.actor (class_name -> u64 instance ID)
    let cases: &[(&str, u64)] = &[
        ("EmergentVODataContainer", 16985930341382286056),
        ("SoundSourceComponent",     2714848687305658898),
        ("AnimControllerComponent",  4005768117623023628),
        ("ConduitComponent",         5268024850096038587),
        ("GameWeaponAnimControllerStandard", 18381955217928403521),
        ("TargetData",               5001284555083246638),
        ("AccessibilityHighlightComponent", 7486995302204495432),
        ("WaterImpulseGenerator",    11532772055630022756),
        ("MeleeWeaponSkin",          11444595384520868630),
        ("PickupWeaponItem",         3295571178908929668),
        ("AttachToWaterSurface",     5612868931624342598),
        ("WeaponTrajectoryDrawer",   2708075267638373994),
    ];

    for (n, want) in cases {
        let lower = n.to_lowercase();
        let upper = n.to_uppercase();
        let c64a = crc64::hash(n);
        let c64b = crc64::hash_raw(n.as_bytes());
        let c64c = crc64::hash_raw(lower.as_bytes());
        let c64d = crc64::hash_raw(upper.as_bytes());
        let c32 = crc32::hash(n);
        let c32_lo = crc32::hash(&lower);
        // Check if u64 = (crc32(name) | (crc32(lowercase) << 32)) or similar
        let combined1 = (c32 as u64) | ((c32_lo as u64) << 32);
        let combined2 = (c32_lo as u64) | ((c32 as u64) << 32);

        println!("=== {n} (want 0x{want:016X}) ===");
        println!("  crc64(normalized) = 0x{c64a:016X}");
        println!("  crc64(raw)        = 0x{c64b:016X}");
        println!("  crc64(raw lower)  = 0x{c64c:016X}");
        println!("  crc64(raw upper)  = 0x{c64d:016X}");
        println!("  crc32             = 0x{c32:08X}");
        println!("  crc32(lower)      = 0x{c32_lo:08X}");
        println!("  combined hi=lo|lo<<32 = 0x{combined1:016X}");
        println!("  combined hi=hi|lo<<32 = 0x{combined2:016X}");
    }
}
