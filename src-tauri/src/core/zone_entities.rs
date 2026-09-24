//! The non-model zone entity kinds: volumes, curves (splines/paths), visual
//! effect instances, decals — and the navigation data that has no scene object
//! at all.
//!
//! # Where the numbers come from
//!
//! Every scene-object entry in `Zone Scene Objects` starts with the same
//! 0x80-byte base header (`crate::core::zone`), and each kind appends its own
//! fields after it. The offsets below were read off the shipped PC build and
//! in the doc comment of each struct. Nothing here allocates unless the caller
//! asks for a specific entity, so a zone list stays cheap.
//!
//! The base header, for reference (all VERIFIED on the shipped build, and the
//! exact field order the engine writes):
//!
//! ```text
//!   +0x00 f32[16] object-to-world, row-major (row 3 = translation)
//!   +0x40 f32[4]  object-space bounding sphere (centre xyz, radius)
//!   +0x50 f32[3]  object-space AABB half-extents about the sphere centre
//!   +0x5C u32     low 24 bits object flags, high byte = kind
//!   +0x60 u32[4]  always zero on disk (runtime handles)
//!   +0x70 f32[3]  cached transform scale
//!   +0x7C u32     low 31 bits entry size, bit 31 = "memory ready"
//! ```
//!
//! Note the size field is 31 bits, not 24: bit 31 is a flag, so masking with
//! `0x7FFF_FFFF` is the correct read. Every shipped entry measured has bit 31
//! clear and a size below 64 KiB, so the difference never bites in practice.

use byteorder::{ByteOrder, LittleEndian as LE};
use serde::Serialize;

use crate::core::zone::{GeoRef, Zone};

// Section tags used here. All five are hash-verified names in
// `crate::core::section_names`.
pub const T_SCENE_OBJECTS: u32 = 0x06AB_CAB2;
pub const T_VOLUMES: u32 = 0xBDAB_2B0D;
pub const T_DECALS: u32 = 0xBCF4_3558;
pub const T_VFX_INSTS: u32 = 0x927C_4EC3;
pub const T_VFX_NAMES: u32 = 0x3D5E_2FEF;

/// `Zone Nav Built Container` — only ever in a `containernavdata.zone`.
pub const T_NAV_CONTAINER: u32 = 0x04C1_9E69;
/// `Zone Nav Built Tile Cache` — the Detour `dtTileCache` layer blobs.
pub const T_NAV_TILECACHE: u32 = 0xA903_D8F1;
/// `Zone Nav Built Unit` — the group grid and the walkable triangle meshes.
pub const T_NAV_UNIT: u32 = 0xD124_430B;

const BASE: usize = 0x80;

// ---------------------------------------------------------------------------
// small readers
// ---------------------------------------------------------------------------

fn vec3(d: &[u8], off: usize) -> [f32; 3] {
    let mut v = [0f32; 3];
    if off + 12 <= d.len() {
        LE::read_f32_into(&d[off..off + 12], &mut v);
    }
    v
}

fn vec4(d: &[u8], off: usize) -> [f32; 4] {
    let mut v = [0f32; 4];
    if off + 16 <= d.len() {
        LE::read_f32_into(&d[off..off + 16], &mut v);
    }
    v
}

fn u16at(d: &[u8], off: usize) -> u16 {
    d.get(off..off + 2).map(LE::read_u16).unwrap_or(0)
}

fn u32at(d: &[u8], off: usize) -> u32 {
    d.get(off..off + 4).map(LE::read_u32).unwrap_or(0)
}

fn u64at(d: &[u8], off: usize) -> u64 {
    d.get(off..off + 8).map(LE::read_u64).unwrap_or(0)
}

fn f32at(d: &[u8], off: usize) -> f32 {
    d.get(off..off + 4).map(LE::read_f32).unwrap_or(0.0)
}

fn cstr(d: &[u8], off: usize, end: usize) -> String {
    let end = end.min(d.len());
    if off >= end {
        return String::new();
    }
    let s = &d[off..end];
    let n = s.iter().position(|&b| b == 0).unwrap_or(s.len());
    String::from_utf8_lossy(&s[..n]).replace('\\', "/")
}

fn align16(n: usize) -> usize {
    (n + 15) & !15
}

/// Transform a point by a row-major 4x4 with the translation in row 3.
pub fn xform_point(m: &[f32; 16], p: [f32; 3]) -> [f32; 3] {
    [
        p[0] * m[0] + p[1] * m[4] + p[2] * m[8] + m[12],
        p[0] * m[1] + p[1] * m[5] + p[2] * m[9] + m[13],
        p[0] * m[2] + p[1] * m[6] + p[2] * m[10] + m[14],
    ]
}

/// Rotate a direction by a row-major 4x4 (no translation).
pub fn xform_dir(m: &[f32; 16], p: [f32; 3]) -> [f32; 3] {
    [
        p[0] * m[0] + p[1] * m[4] + p[2] * m[8],
        p[0] * m[1] + p[1] * m[5] + p[2] * m[9],
        p[0] * m[2] + p[1] * m[6] + p[2] * m[10],
    ]
}

/// Who owns this scene entry: `(actor row, actor name, actor asset path)`.
fn owner(zone: &Zone, scene_offset: u32) -> (i32, String, String) {
    match zone.actors.iter().position(|a| a.scene_offset == scene_offset) {
        Some(i) => (
            i as i32,
            zone.actors[i].name.clone(),
            zone.actors[i].asset_path.clone(),
        ),
        None => (-1, String::new(), String::new()),
    }
}

fn scene(zone: &Zone) -> &[u8] {
    zone.dat1.get_section_data(T_SCENE_OBJECTS).unwrap_or(&[])
}

/// The object-space bounding sphere every scene object carries at +0x40.
fn base_bsphere(s: &[u8], b: usize) -> [f32; 4] {
    vec4(s, b + 0x40)
}

/// The object-space AABB half-extents every scene object carries at +0x50.
fn base_extents(s: &[u8], b: usize) -> [f32; 3] {
    vec3(s, b + 0x50)
}

// ---------------------------------------------------------------------------
// Volumes (kind 2)
// ---------------------------------------------------------------------------

pub const VOLUME_SHAPES: [&str; 5] = ["Point", "Cuboid", "Ellipsoid", "Cylinder", "Capsule"];

pub fn volume_shape_name(shape: u8) -> &'static str {
    VOLUME_SHAPES.get(shape as usize).copied().unwrap_or("?")
}

/// `VolumeUsageFlags`. Bits 0..=21 are serialised; 22 and 23 only ever exist in
/// the editor's proxy volumes, so a shipped zone should never set them (and
/// none measured does).
pub const VOLUME_FLAG_NAMES: [&str; 24] = [
    "Indoor",
    "NavMeshGen",
    "WeatherBlocker",
    "PostEffects",
    "FullyIndoor",
    "ValidExtents",
    "ForceEmbeddedSamples",
    "AvoidEmbeddedSamples",
    "Water",
    "IndoorWindow",
    "EmissiveBlocker",
    "LightGridPathBlocker",
    "LightGridForceOccluderPlane",
    "LightGridCreator",
    "LightGridSelector",
    "DistantLightingGround",
    "DistantLightingRoof",
    "DistantLightingForceEmbedded",
    "DistantLightingMid",
    "RayTracingBlocker",
    "TranslucentShadow",
    "LightGridSampleAvoid",
    "EnvProbe",
    "LightGrid",
];

pub const VOL_INDOOR: u32 = 1 << 0;
pub const VOL_NAV_MESH_GEN: u32 = 1 << 1;
pub const VOL_WEATHER_BLOCKER: u32 = 1 << 2;
pub const VOL_POST_EFFECTS: u32 = 1 << 3;
pub const VOL_FULLY_INDOOR: u32 = 1 << 4;
pub const VOL_VALID_EXTENTS: u32 = 1 << 5;
pub const VOL_WATER: u32 = 1 << 8;
pub const VOL_INDOOR_WINDOW: u32 = 1 << 9;
pub const VOL_EMISSIVE_BLOCKER: u32 = 1 << 10;
pub const VOL_RAY_TRACING_BLOCKER: u32 = 1 << 19;
pub const VOL_TRANSLUCENT_SHADOW: u32 = 1 << 20;

pub fn volume_flag_names(flags: u32) -> Vec<&'static str> {
    (0..24)
        .filter(|b| flags & (1 << b) != 0)
        .map(|b| VOLUME_FLAG_NAMES[b])
        .collect()
}

/// One kind-2 scene entry, a `VolumeBuilt`.
///
/// ```text
///   +0x80 u64 uid
///   +0x88 u32 usage flags        -- VOLUME_FLAG_NAMES
///   +0x8C u8  shape              -- VOLUME_SHAPES
///   +0x8D u8  lighting-condition mask
///   +0x8E u16 translucent-shadow byte offset from the entry (0     = none)
///   +0x90 u16 water-settings     byte offset from the entry (0xFFFF = none)
///   +0x92 u16 post-effects       byte offset from the entry (0xFFFF = none)
///   +0x94 f32 unit-space cylinder/capsule height
///   +0x98 f32[3] half-extents, in the entry's own local space
///   +0xA4 f32[3] the exact reciprocals of +0x98
///   0xB0 bytes total for a plain volume; the three optional structs follow.
/// ```
///
/// VERIFIED on all 56 679 volumes of the shipped PC build (2 201 zones): +0xA4 is
/// the exact reciprocal of +0x98 on 56 679 of 56 679 (relative error under 1e-5
/// per component), all 56 679 set `ValidExtents`, and every one of the 55 989 that
/// sets neither `Water` nor `PostEffects` nor `TranslucentShadow` is exactly 0xB0
/// bytes long — 0 exceptions. The shapes seen are 52 306 Cuboid, 2 637 Ellipsoid,
/// 1 626 Cylinder, 109 Capsule and 1 Point: every value the enum defines.
///
/// +0x94 equals `1 - 0.5 / extents.y` on 55 555 of 56 679, so the builder
/// normally derives it from the extents; the remaining 2 % are authored.
///
/// The half-extents are *not* the base header's own +0x50 extents: they differ on
/// 56 679 of 56 679 entries measured. Use `extents` for the shape and
/// `base_extents` only for culling.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeEntity {
    /// Byte offset in `Zone Scene Objects` — this entry's identity.
    pub offset: u32,
    pub size: u32,
    #[serde(serialize_with = "ser_u64_str")]
    pub uid: u64,
    /// Bit 63 of the uid: authored into this zone rather than spawned.
    pub is_placed: bool,
    pub flags: u32,
    pub flag_names: Vec<&'static str>,
    pub shape: u8,
    pub shape_name: &'static str,
    /// Which of the level's lighting conditions this volume applies to; 0x1F
    /// ("all five") on all but a handful of shipped volumes.
    pub lighting_conditions: u8,
    /// Local half-extents: for `Cuboid` the box radius on each axis, for
    /// `Ellipsoid` the three radii, for `Cylinder`/`Capsule` x and z are the
    /// radius and y the half-height.
    pub extents: [f32; 3],
    pub inv_extents: [f32; 3],
    /// For `Cylinder` and `Capsule`, the straight section's height as a
    /// fraction of the unit box; the engine defaults it to 0.5.
    pub unit_space_cylinder_height: f32,
    pub has_water: bool,
    pub has_post_effects: bool,
    pub has_translucent_shadow: bool,
    pub water_offset: u16,
    pub post_effects_offset: u16,
    pub translucent_offset: u16,
    /// Row-major 4x4, row 3 = translation.
    pub matrix: [f32; 16],
    pub position: [f32; 3],
    /// The base header's own bounding sphere (centre xyz, radius) in local space.
    pub bsphere: [f32; 4],
    /// The base header's own AABB half-extents in local space.
    pub base_extents: [f32; 3],
    pub actor_index: i32,
    pub actor_name: String,
    pub actor_asset: String,
    /// What the volume is for, in one phrase, from its flags and owning actor.
    pub purpose: String,
}

impl VolumeEntity {
    /// The eight corners of the volume's oriented bounding box in world space.
    /// Every shape fits inside it, so a viewer can draw this for all five.
    pub fn world_box(&self) -> [[f32; 3]; 8] {
        let e = self.extents;
        let mut out = [[0f32; 3]; 8];
        for (i, o) in out.iter_mut().enumerate() {
            let sx = if i & 1 == 0 { -1.0 } else { 1.0 };
            let sy = if i & 2 == 0 { -1.0 } else { 1.0 };
            let sz = if i & 4 == 0 { -1.0 } else { 1.0 };
            *o = xform_point(&self.matrix, [e[0] * sx, e[1] * sy, e[2] * sz]);
        }
        out
    }

    /// World-space AABB of that oriented box.
    pub fn world_aabb(&self) -> ([f32; 3], [f32; 3]) {
        let c = self.world_box();
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for p in &c {
            for i in 0..3 {
                min[i] = min[i].min(p[i]);
                max[i] = max[i].max(p[i]);
            }
        }
        (min, max)
    }
}

/// A one-line read of what a volume is for. The flags say it outright for the
/// rendering and navigation roles; for a gameplay trigger only the owning actor
/// and its prius know, so the actor asset name is the fallback.
fn volume_purpose(flags: u32, actor_asset: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if flags & VOL_WATER != 0 {
        parts.push("water");
    }
    if flags & VOL_POST_EFFECTS != 0 {
        parts.push("post-effect / atmosphere override");
    }
    if flags & (VOL_INDOOR | VOL_FULLY_INDOOR) != 0 {
        parts.push("indoor (keylight shadow control)");
    }
    if flags & VOL_INDOOR_WINDOW != 0 {
        parts.push("indoor window");
    }
    if flags & VOL_NAV_MESH_GEN != 0 {
        parts.push("nav-mesh generation bound");
    }
    if flags & VOL_WEATHER_BLOCKER != 0 {
        parts.push("weather blocker");
    }
    if flags & VOL_EMISSIVE_BLOCKER != 0 {
        parts.push("emissive blocker");
    }
    if flags & VOL_RAY_TRACING_BLOCKER != 0 {
        parts.push("ray-tracing blocker");
    }
    if flags & VOL_TRANSLUCENT_SHADOW != 0 {
        parts.push("translucent shadow / gobo");
    }
    if flags & (1 << 13) != 0 || flags & (1 << 14) != 0 {
        parts.push("light-grid authoring");
    }
    if flags & 0x000F_0000 != 0 {
        parts.push("distant-lighting hint");
    }
    if parts.is_empty() {
        // No rendering role: it is a gameplay region and the actor names it.
        let stem = actor_asset
            .rsplit('/')
            .next()
            .unwrap_or("")
            .trim_end_matches(".actor");
        if stem.is_empty() {
            "trigger region (no owning actor)".to_string()
        } else {
            format!("gameplay region: {stem}")
        }
    } else {
        parts.join(" + ")
    }
}

/// Decode every kind-2 entry the zone's `Zone Volumes` list points at.
pub fn volumes(zone: &Zone) -> Vec<VolumeEntity> {
    let s = scene(zone);
    let mut out = Vec::new();
    for o in zone.scene_objects.iter().filter(|o| o.kind == 2) {
        let b = o.offset as usize;
        if b + 0xB0 > s.len() {
            continue;
        }
        let flags = u32at(s, b + 0x88);
        let (actor_index, actor_name, actor_asset) = owner(zone, o.offset);
        let translucent_offset = u16at(s, b + 0x8E);
        let water_offset = u16at(s, b + 0x90);
        let post_effects_offset = u16at(s, b + 0x92);
        let uid = u64at(s, b + BASE);
        out.push(VolumeEntity {
            offset: o.offset,
            size: o.size,
            uid,
            is_placed: uid & (1 << 63) != 0,
            flags,
            flag_names: volume_flag_names(flags),
            shape: s[b + 0x8C],
            shape_name: volume_shape_name(s[b + 0x8C]),
            lighting_conditions: s[b + 0x8D],
            extents: vec3(s, b + 0x98),
            inv_extents: vec3(s, b + 0xA4),
            unit_space_cylinder_height: f32at(s, b + 0x94),
            has_water: flags & VOL_WATER != 0 && water_offset != 0xFFFF,
            has_post_effects: flags & VOL_POST_EFFECTS != 0 && post_effects_offset != 0xFFFF,
            has_translucent_shadow: flags & VOL_TRANSLUCENT_SHADOW != 0 && translucent_offset != 0,
            water_offset,
            post_effects_offset,
            translucent_offset,
            matrix: o.matrix,
            position: o.position(),
            bsphere: base_bsphere(s, b),
            base_extents: base_extents(s, b),
            actor_index,
            actor_name,
            purpose: volume_purpose(flags, &actor_asset),
            actor_asset,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Curves / splines / paths (kind 4)
// ---------------------------------------------------------------------------

/// engine; 1 is the remaining interpolation mode. Measured over all 8 758 shipped
/// curves: 7 241 Centripetal, 977 type 1, 489 Linear, 48 CatmullRom and 3 whose
/// type byte is outside 0..=3 at all, which [`spline_type_name`] reports as `?`.
pub const SPLINE_TYPES: [&str; 4] = ["Centripetal", "Type1", "Linear", "CatmullRom"];

pub fn spline_type_name(t: i8) -> &'static str {
    SPLINE_TYPES.get(t as usize).copied().unwrap_or("?")
}

pub const SPLINE_LOOPING: u8 = 1 << 0;

///
/// ```text
///   +0x80 f32[4] curve bounding sphere in object space
///   +0x90 f32[3] curve AABB half-extents about that sphere's centre
///   +0x9C u16 point count
///   +0x9E u16 points        byte offset from the entry
///   +0xA0 u16 normals       byte offset, 0xFFFF when absent
///   +0xA2 u16 point coeffs  byte offset
///   +0xA4 u16 segment lens  byte offset
///   +0xA6 u16 distance tbl  byte offset
///   +0xA8 u16 distance count
///   +0xAA u16 point-coeff count
///   +0xAC u16 segment-length count
///   +0xAE u8  spline flags  -- bit 0 = looping
///   +0xAF i8  spline type   -- SPLINE_TYPES
///   +0xB0 u16 loop start point index
///   +0xB2 u8[2] pad
///   0xB4 bytes of header, then the five arrays back to back:
///     f32[3] x point_count            control points, entry-local
///     f32[3] x point_count            per-point normals ("up" for the path)
///     f32[3][4] x coeff count         cubic coefficients per segment
///     f32   x segment-length count    arc length of each segment
///     f32   x distance count          arc-length lookup table
/// ```
///
/// VERIFIED on all 8 758 curves of the shipped PC build (349 zones): 8 676 have
/// `points` at exactly 0xB4 with every following array starting exactly where the
/// previous one ends, so the whole entry is accounted for; the other 82 are empty
/// curves whose entry is exactly 0xB4 bytes with every offset 0xFFFF — which is
/// fails: 0 of 8 758 are unexplained.
///
/// The stored `segment_lengths` are real arc lengths: on all 435 `Linear` curves
/// whose segment count is `point_count - 1`, every stored length matches the
/// straight-line distance between its two control points to 0.1 %.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurveEntity {
    pub offset: u32,
    pub size: u32,
    pub point_count: u16,
    pub spline_type: i8,
    pub spline_type_name: &'static str,
    pub spline_flags: u8,
    /// Bit 0 of the spline flags: the last point wraps back to `loop_start`.
    pub looping: bool,
    pub loop_start_index: u16,
    /// Control points in the entry's own local space.
    pub points: Vec<[f32; 3]>,
    /// The same points pushed through the entry's matrix — what a viewer draws.
    pub world_points: Vec<[f32; 3]>,
    /// Per-point normal ("up"), absent on a curve built without them.
    pub normals: Option<Vec<[f32; 3]>>,
    /// Arc length of each segment, when the builder stored them.
    pub segment_lengths: Vec<f32>,
    /// Sum of `segment_lengths`: the path's total length in metres.
    pub length: f32,
    pub bsphere: [f32; 4],
    pub aabb_extents: [f32; 3],
    pub matrix: [f32; 16],
    pub position: [f32; 3],
    pub actor_index: i32,
    pub actor_name: String,
    pub actor_asset: String,
    /// True when every array lands exactly where the header says it should.
    pub layout_exact: bool,
}

impl CurveEntity {
    /// The polyline a viewer draws: world control points, closed when looping.
    pub fn world_polyline(&self) -> Vec<[f32; 3]> {
        let mut p = self.world_points.clone();
        if self.looping && p.len() > 2 {
            let start = self.loop_start_index as usize;
            if let Some(&w) = p.get(start) {
                p.push(w);
            }
        }
        p
    }
}

/// Decode every kind-4 entry in the zone.
pub fn curves(zone: &Zone) -> Vec<CurveEntity> {
    let s = scene(zone);
    let mut out = Vec::new();
    for o in zone.scene_objects.iter().filter(|o| o.kind == 4) {
        let b = o.offset as usize;
        let end = (b + o.size as usize).min(s.len());
        if b + 0xB4 > end {
            continue;
        }
        let pc = u16at(s, b + 0x9C) as usize;
        let po = u16at(s, b + 0x9E) as usize;
        let no = u16at(s, b + 0xA0) as usize;
        let so = u16at(s, b + 0xA4) as usize;
        let dc = u16at(s, b + 0xA8) as usize;
        let cc = u16at(s, b + 0xAA) as usize;
        let sc = u16at(s, b + 0xAC) as usize;
        let sflags = s[b + 0xAE];
        let stype = s[b + 0xAF] as i8;

        let exp_p = 0xB4usize;
        let exp_n = exp_p + pc * 12;
        let exp_c = exp_n + pc * 12;
        let exp_s = exp_c + cc * 48;
        let exp_d = exp_s + sc * 4;
        let layout_exact = pc > 0
            && po == exp_p
            && (no == exp_n || no == 0xFFFF)
            && (cc == 0 || u16at(s, b + 0xA2) as usize == exp_c)
            && (sc == 0 || so == exp_s)
            && (dc == 0 || u16at(s, b + 0xA6) as usize == exp_d)
            && b + exp_d + dc * 4 <= end;

        let read_pts = |base: usize, n: usize| -> Vec<[f32; 3]> {
            (0..n)
                .filter(|i| b + base + i * 12 + 12 <= end)
                .map(|i| vec3(s, b + base + i * 12))
                .collect()
        };
        let points = if po != 0xFFFF { read_pts(po, pc) } else { Vec::new() };
        let normals = if no != 0xFFFF && pc > 0 {
            Some(read_pts(no, pc))
        } else {
            None
        };
        let segment_lengths: Vec<f32> = if so != 0xFFFF && sc > 0 {
            (0..sc)
                .filter(|i| b + so + i * 4 + 4 <= end)
                .map(|i| f32at(s, b + so + i * 4))
                .collect()
        } else {
            Vec::new()
        };
        let matrix = o.matrix;
        let world_points = points.iter().map(|&p| xform_point(&matrix, p)).collect();
        let (actor_index, actor_name, actor_asset) = owner(zone, o.offset);
        out.push(CurveEntity {
            offset: o.offset,
            size: o.size,
            point_count: pc as u16,
            spline_type: stype,
            spline_type_name: spline_type_name(stype),
            spline_flags: sflags,
            looping: sflags & SPLINE_LOOPING != 0,
            loop_start_index: u16at(s, b + 0xB0),
            points,
            world_points,
            normals,
            length: segment_lengths.iter().sum(),
            segment_lengths,
            bsphere: vec4(s, b + 0x80),
            aabb_extents: vec3(s, b + 0x90),
            matrix,
            position: o.position(),
            actor_index,
            actor_name,
            actor_asset,
            layout_exact,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Visual effect instances (kind 5)
// ---------------------------------------------------------------------------

/// One kind-5 scene entry, a `VisualEffectInstBuilt`. 5 049 of the 5 107 shipped
/// instances are exactly 0xA0 bytes; 49 are 0xB0, one 0xC0 and 8 0x100, so the
/// struct has an optional tail that is not decoded here.
///
/// ```text
///   +0x80 f32[3] tint, linear 0..1
///   +0x8C u32    0xFFFFFFFF on all 5 049 of the 0xA0-sized instances
///   +0x90 u32    0 on all 5 049 of them
///   +0x94 f32    1.0 on 5 066 of 5 107 (a scale or intensity)
///   +0x98 u32    0 on all 5 049 of them
///   +0x9C u24    asset index into Zone Visual Effect Names
///   +0x9F u8     an unidentified byte: 0xFF on 4 622 of 5 107, otherwise spread
///                over the whole 0..255 range, which is what a normalised 0..1
///                quantity looks like. Treated as opaque.
/// ```
///
/// The asset index is the field `Zone::UpdateVisualEffectInstantiation` reads as
/// byte is a separate field because every instance measured has bytes +0x9D and
/// +0x9E zero while the largest index in the whole game is 22.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VfxEntity {
    pub offset: u32,
    pub size: u32,
    pub asset_index: u32,
    #[serde(serialize_with = "ser_u64_str")]
    pub asset_id: u64,
    /// `visualeffect/...` path, taken from the zone's own string pool.
    pub asset_path: String,
    /// Short label for a viewport marker: the effect's file stem.
    pub label: String,
    pub tint: [f32; 3],
    /// The unidentified byte at +0x9F. See the struct docs.
    pub byte_9f: u8,
    /// +0x8C: 0xFFFFFFFF on every 0xA0-sized shipped instance.
    pub slot_8c: u32,
    /// +0x94: 1.0 on 5 066 of the 5 107 shipped instances.
    pub scale_94: f32,
    pub matrix: [f32; 16],
    pub position: [f32; 3],
    /// Object-space bounding sphere from the base header; radius 0 when the
    /// builder did not precompute one.
    pub bsphere: [f32; 4],
    pub actor_index: i32,
    pub actor_name: String,
    pub actor_asset: String,
}

/// `Zone Visual Effect Names`, a struct-of-arrays `{u64 id[n]}{u32 strofs[n]}`.
pub fn vfx_names(zone: &Zone) -> (Vec<u64>, Vec<String>) {
    let Some(sec) = zone.dat1.get_section_data(T_VFX_NAMES) else {
        return (Vec::new(), Vec::new());
    };
    let n = sec.len() / 12;
    let mut ids = Vec::with_capacity(n);
    let mut paths = Vec::with_capacity(n);
    for i in 0..n {
        ids.push(LE::read_u64(&sec[i * 8..i * 8 + 8]));
        let so = LE::read_u32(&sec[n * 8 + i * 4..n * 8 + i * 4 + 4]);
        paths.push(
            zone.dat1
                .get_string(so)
                .unwrap_or_default()
                .replace('\\', "/"),
        );
    }
    (ids, paths)
}

pub fn vfx_instances(zone: &Zone) -> Vec<VfxEntity> {
    let s = scene(zone);
    let (ids, paths) = vfx_names(zone);
    let mut out = Vec::new();
    for o in zone.scene_objects.iter().filter(|o| o.kind == 5) {
        let b = o.offset as usize;
        if b + 0xA0 > s.len() {
            continue;
        }
        let w = u32at(s, b + 0x9C);
        let ai = (w & 0x00FF_FFFF) as usize;
        let asset_path = paths.get(ai).cloned().unwrap_or_default();
        let label = asset_path
            .rsplit('/')
            .next()
            .unwrap_or("")
            .trim_end_matches(".visualeffect")
            .to_string();
        let (actor_index, actor_name, actor_asset) = owner(zone, o.offset);
        out.push(VfxEntity {
            offset: o.offset,
            size: o.size,
            asset_index: ai as u32,
            asset_id: ids.get(ai).copied().unwrap_or(0),
            asset_path,
            label,
            tint: vec3(s, b + 0x80),
            byte_9f: (w >> 24) as u8,
            slot_8c: u32at(s, b + 0x8C),
            scale_94: f32at(s, b + 0x94),
            matrix: o.matrix,
            position: o.position(),
            bsphere: base_bsphere(s, b),
            actor_index,
            actor_name,
            actor_asset,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Decals (kind 3)
// ---------------------------------------------------------------------------

/// `DecalAssetType`, the low two bits of the byte at +0x2B7. The engine numbers
/// them 1 = material graph, 2 = material, 3 = texture, leaving 0 unused —
/// VERIFIED, because 16 384 of 16 386 shipped decals read 2 and carry a
/// `.material` path, and the 2 that read 3 carry a `.texture` path.
pub const DECAL_ASSET_TYPES: [&str; 4] = ["None", "MaterialGraph", "Material", "Texture"];

/// The six flag bits packed above the asset type in the same byte.
pub const DECAL_FLAG_NAMES: [&str; 6] = [
    "Mesh",
    "ScreenSpaceDynamicModels",
    "ScreenSpaceStaticModels",
    "ScreenSpaceTerrain",
    "AllowBVHBuffer",
    "Flag5",
];

/// One kind-3 scene entry, a `DecalBuilt`. Every shipped decal is 0x2C0 bytes.
///
/// ```text
///   +0x080 char[512] inline .material (or mesh) asset path
///   +0x280 u64[2]  AV-material masks: which surface materials accept it
///   +0x290 f32[3]  instance custom (shader-driven per-instance values)
///   +0x29C f32[3]  instance colour / tint, 0..1
///   +0x2A8 f32     projection limit (cos of the max slope it wraps onto)
///   +0x2AC f32     fade distance, metres
///   +0x2B0 f32     lod distance, metres
///   +0x2B4 u16     blend override
///   +0x2B6 u8      overlap override
///   +0x2B7 u8      low 2 bits asset type, high 6 bits flags
///   +0x2B8 u16     timestamp (authoring order, for overlap resolution)
///   +0x2BA u8      alpha test
///   +0x2BB u8      alpha, 0..255
///   +0x2BC u32     Zone Decal Geometry row index, 0xFFFFFFFF = no mesh
/// ```
///
/// The 0x2C0 entry size closes exactly on this field list, and the three fields
/// that were already known by measurement (tint at +0x29C, fade at +0x2AC, the
/// geometry row at +0x2BC) all land where it predicts.
///
/// VERIFIED on all 16 386 decals of the shipped PC build (335 zones): every entry
/// is exactly 0x2C0 bytes, and on 16 386 of 16 386 the inline path is
/// NUL-terminated inside its 512-byte field *and* carries the extension its asset
/// type implies — 16 384 read type 2 with a `.material` path and 2 read type 3
/// `Mesh` flag and have an all-ones AV material mask; 5 313 also set
/// `AllowBVHBuffer`; 14 548 own a geometry row and 1 838 project no mesh. Fade
/// distance spans 25..1000 m, lod 15..150 m and the projection limit 0..0.9.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecalEntity {
    pub offset: u32,
    pub size: u32,
    /// The inline `.material` path.
    pub material_path: String,
    #[serde(serialize_with = "ser_u64_str")]
    pub av_material_mask_lo: u64,
    #[serde(serialize_with = "ser_u64_str")]
    pub av_material_mask_hi: u64,
    pub inst_custom: [f32; 3],
    pub tint: [f32; 3],
    pub projection_limit: f32,
    pub fade_distance: f32,
    pub lod_distance: f32,
    pub blend_override: u16,
    pub overlap_override: u8,
    pub asset_type: u8,
    pub asset_type_name: &'static str,
    pub decal_flags: u8,
    pub decal_flag_names: Vec<&'static str>,
    pub timestamp: u16,
    pub alpha_test: u8,
    pub alpha: u8,
    /// `Zone Decal Geometry` row, or `None` when the decal projects no mesh.
    pub geometry_row: Option<u32>,
    /// Where that row's mesh lives inside the zone's raw block 2.
    pub geometry: Option<GeoRef>,
    pub matrix: [f32; 16],
    pub position: [f32; 3],
    pub bsphere: [f32; 4],
    pub base_extents: [f32; 3],
}

pub fn decals(zone: &Zone) -> Vec<DecalEntity> {
    let s = scene(zone);
    let mut out = Vec::new();
    for o in zone.scene_objects.iter().filter(|o| o.kind == 3) {
        let b = o.offset as usize;
        let end = (b + o.size as usize).min(s.len());
        if b + 0x2C0 > end {
            continue;
        }
        let tf = s[b + 0x2B7];
        let flags = tf >> 2;
        let row = u32at(s, b + 0x2BC);
        out.push(DecalEntity {
            offset: o.offset,
            size: o.size,
            material_path: cstr(s, b + 0x80, b + 0x280),
            av_material_mask_lo: u64at(s, b + 0x280),
            av_material_mask_hi: u64at(s, b + 0x288),
            inst_custom: vec3(s, b + 0x290),
            tint: vec3(s, b + 0x29C),
            projection_limit: f32at(s, b + 0x2A8),
            fade_distance: f32at(s, b + 0x2AC),
            lod_distance: f32at(s, b + 0x2B0),
            blend_override: u16at(s, b + 0x2B4),
            overlap_override: s[b + 0x2B6],
            asset_type: tf & 3,
            asset_type_name: DECAL_ASSET_TYPES[(tf & 3) as usize],
            decal_flags: flags,
            decal_flag_names: (0..6)
                .filter(|i| flags & (1 << i) != 0)
                .map(|i| DECAL_FLAG_NAMES[i])
                .collect(),
            timestamp: u16at(s, b + 0x2B8),
            alpha_test: s[b + 0x2BA],
            alpha: s[b + 0x2BB],
            geometry_row: if row == u32::MAX { None } else { Some(row) },
            geometry: zone.decal_geometry.get(&o.offset).copied(),
            matrix: o.matrix,
            position: o.position(),
            bsphere: base_bsphere(s, b),
            base_extents: base_extents(s, b),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Navigation
// ---------------------------------------------------------------------------

/// Nav-local metres are stored biased into a `u16`. The bias is
/// `kWorldMaxTiles / 2 * kTileSizeMeters`, and both constants are fixed for this
/// game: 4096 tiles and 16 m, hence 32768. VERIFIED against the tile cache,
/// whose layer headers carry the same tile's world AABB as plain floats and
/// agree exactly on every tile measured.
pub const NAV_POS_BIAS: i32 = 32768;

///
/// ```text
///   +0x00 u32 magic (the engine's built-nav version hash)
///   +0x04 u64 this container's asset id
///   +0x0C i32 nav layer count (agent classes)
///   +0x10 i16 grid min x, +0x12 min z, +0x14 max x, +0x16 max z  (tile units)
///   +0x18 i16 log2 of the tile size in metres
///   +0x20 pointer slots, one per (layer, tile), zero on disk
/// ```
///
/// A container comes in two flavours, and the sign of `log_scale` tells them
/// apart. A *tiled* container (`log_scale >= 0`, 9 of the 116 shipped ones, all
/// of them open-world planets with a 128 m unit grid) keeps one pointer slot per
/// (layer, unit tile), so the section is
/// `0x20 + 8 * layer_count * (max_x - min_x) * (max_z - min_z)` bytes — exact on
/// 9 of 9. A *single-unit* container (`log_scale == -1`, the other 107) stores its
/// bounds in metres instead of tiles and keeps one slot per layer, so the section
/// is `0x20 + 8 * layer_count`: exact on 85 of 107. The remaining 22 are 40 bytes
/// where their two nav layers imply 48 — one pointer slot short. That costs
/// nothing, because every slot is zero on disk and only filled in at load, but it
/// is why `size_exact` is a reported field rather than an assertion.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavContainer {
    pub magic: u32,
    #[serde(serialize_with = "ser_u64_str")]
    pub asset_id: u64,
    pub layer_count: i32,
    pub min_x: i32,
    pub min_z: i32,
    pub max_x: i32,
    pub max_z: i32,
    pub log_scale: i32,
    pub tile_size_m: i32,
    pub tile_count: usize,
    /// True when this container is a tiled open-world grid rather than a single
    /// unit whose bounds happen to be stored in metres.
    pub tiled: bool,
    /// True when the section length matches the computed tile count exactly.
    pub size_exact: bool,
    pub section_len: usize,
}

pub fn nav_container(zone: &Zone) -> Option<NavContainer> {
    let c = zone.dat1.get_section_data(T_NAV_CONTAINER)?;
    if c.len() < 0x20 {
        return None;
    }
    let g = |o: usize| LE::read_i16(&c[o..o + 2]) as i32;
    let layer_count = LE::read_i32(&c[12..16]);
    let (min_x, min_z, max_x, max_z, log_scale) = (g(16), g(18), g(20), g(22), g(24));
    let tile_count = if log_scale >= 0 {
        layer_count.max(0) as usize
            * (max_x - min_x).max(0) as usize
            * (max_z - min_z).max(0) as usize
    } else {
        // Single-unit container: the bounds are metres, not tiles, and there is
        // one slot per nav layer.
        layer_count.max(0) as usize
    };
    Some(NavContainer {
        magic: LE::read_u32(&c[0..4]),
        asset_id: LE::read_u64(&c[4..12]),
        layer_count,
        min_x,
        min_z,
        max_x,
        max_z,
        log_scale,
        tile_size_m: if log_scale >= 0 { 1 << log_scale } else { -1 },
        tile_count,
        tiled: log_scale >= 0,
        size_exact: c.len() == 0x20 + 8 * tile_count,
        section_len: c.len(),
    })
}

///
/// ```text
///   +0x00 u16 nav-local x in metres + NAV_POS_BIAS
///   +0x02 u16 nav-local z in metres + NAV_POS_BIAS
///   +0x04 u16 vertex count
///   +0x06 u16 second polygon count (non-zero only on link groups)
///   +0x08 u16 first polygon count
///   +0x0A u16 a fourth count, only non-zero on link groups
///   +0x0C u32 / +0x10 u32  two more counts, zero on a plain group
///   +0x14 u32 vertex array   byte offset from this row
///   +0x18 u32 edge array     byte offset  (== vertex + align16(verts*12))
///   +0x1C u32 polygon array  byte offset
///   +0x20 u32 clue data      byte offset  (== polygon + align16(polys*24))
///   +0x24 u32 poly bspheres  byte offset
///   +0x28 u32 total bytes of this group's data
///   +0x2C u32 zero on every row measured
/// ```
///
/// A row with `data_size == 0` is an empty cell of the grid.
///
/// VERIFIED on all 254 966 groups with data in the shipped PC build (673 unit
/// sections): 111 950 are "plain" — the whole data blob is exactly
/// `align16(verts * 12) + align16(polys * 24)` with the edge offset equal to the
/// polygon offset — and 143 016 carry extra inter-group link arrays between the
/// two. In both cases the polygon array is at +0x1C and its length is
/// `(u16@+0x06 + u16@+0x08) * 24`, and that closes against the next offset on
/// every row measured.
///
/// The decode is right, by two independent geometric tests over the whole game:
/// all 3 622 445 triangle vertex indices are inside their own group's vertex
/// array (0 out of range, 0 degenerate), and all 5 187 949 vertices fall inside
/// their own group's 16 m square footprint (0 outside). A random misread would
/// fail both immediately. On one 128 m tile of Ardolis, 95.8 % of the triangles
/// sit within 45 degrees of horizontal — which is what a walkable surface looks
/// like and what Recast's default slope limit produces.
///
/// The link arrays themselves are *not* decoded. They hold `{target group
/// position, target polygon, target edge}` records joining a group's border
/// triangles to its neighbours', which pathfinding needs and a viewport does
/// not.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavGroup {
    /// Row ordinal in the unit's group array.
    pub row: usize,
    /// Which nav layer (agent class) this row belongs to.
    pub layer: usize,
    /// South-west corner of this group's square footprint, nav-local metres.
    pub x: i32,
    pub z: i32,
    pub vert_count: u16,
    pub poly_count: u32,
    pub vert_offset: u32,
    pub poly_offset: u32,
    pub data_size: u32,
    /// True when the blob is nothing but the vertex and polygon arrays.
    pub plain: bool,
}

/// the world, as a triangle soup per group per layer.
///
/// ```text
///   +0x00 u32 magic (same built-nav version hash as the container)
///   +0x04 u64 this unit zone's own asset id
///   +0x0C u64 the container zone's asset id
///   +0x14 i16 group grid min x, +0x16 min z, +0x18 max x, +0x1A max z
///   +0x1C i16 log2 of the group (tile) size in metres
///         then every non-empty group's vertex and polygon arrays
/// ```
///
/// The layer count is not stored: the group array's own length gives it away,
/// because the first non-empty row's vertex offset points at the first byte after
/// the array. VERIFIED against the tile cache, which writes one `u32 0`
/// terminator per nav layer: the two counts agree on 673 of 673 shipped unit
/// sections, and every one of them comes out as 2 layers of 16 m groups.
///
/// `container_id` is the key that joins a unit to its container: it resolves to
/// the `asset_id` a `containernavdata.zone` stores on 673 of 673 units, across
/// 116 distinct containers, so an open-world planet's 150-odd tiles can be
/// stitched into one mesh with no name list at all.
///
/// Both ids live in their own id space, *not* the PC TOC's: neither
/// `asset_id` nor `container_id` equals the CRC-64 of the zone's own path on any
/// of the 673 units (0 of 673 match). They are still stable and unique, so they
/// work as join keys; they just cannot be looked up as assets.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavUnit {
    pub magic: u32,
    #[serde(serialize_with = "ser_u64_str")]
    pub asset_id: u64,
    #[serde(serialize_with = "ser_u64_str")]
    pub container_id: u64,
    pub min_x: i32,
    pub min_z: i32,
    pub max_x: i32,
    pub max_z: i32,
    pub log_scale: i32,
    pub group_size_m: i32,
    pub layer_count: usize,
    pub groups_per_layer: usize,
    pub groups: Vec<NavGroup>,
    pub section_len: usize,
}

/// A walkable triangle mesh, ready for a viewport. One 128 m unit tile of Ardolis
/// yields 2 917 triangles over 13 794 m^2 across its two nav layers.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavMesh {
    /// Nav-local positions. The nav-to-world matrix is the zone's own offset
    /// matrix, which is the identity for every shipped nav zone measured.
    pub verts: Vec<[f32; 3]>,
    /// Triangle vertex indices into `verts`.
    pub tris: Vec<[u32; 3]>,
    /// One entry per triangle: the index into `NavUnit::groups` it came from.
    pub tri_group: Vec<u32>,
}

pub fn nav_unit(zone: &Zone) -> Option<NavUnit> {
    let u = zone.dat1.get_section_data(T_NAV_UNIT)?;
    if u.len() < 0x20 {
        return None;
    }
    let g = |o: usize| LE::read_i16(&u[o..o + 2]) as i32;
    let (min_x, min_z, max_x, max_z, log_scale) = (g(0x14), g(0x16), g(0x18), g(0x1A), g(0x1C));
    let per_layer = (max_x - min_x).max(0) as usize * (max_z - min_z).max(0) as usize;
    if per_layer == 0 {
        return None;
    }
    // The first non-empty row's vertex array starts right after the whole group
    // array, so its offset divided by the per-layer row size is the layer count.
    let mut layer_count = 0usize;
    let row_bytes = 48usize;
    for i in 0..per_layer * 64 {
        let b = 0x20 + i * row_bytes;
        if b + row_bytes > u.len() {
            break;
        }
        if u32at(u, b + 0x28) == 0 {
            continue;
        }
        let arr_end = b + u32at(u, b + 0x14) as usize;
        if arr_end >= 0x20 && (arr_end - 0x20) % (row_bytes * per_layer) == 0 {
            layer_count = (arr_end - 0x20) / (row_bytes * per_layer);
        }
        break;
    }
    if layer_count == 0 {
        layer_count = 1;
    }

    let mut groups = Vec::new();
    for row in 0..layer_count * per_layer {
        let b = 0x20 + row * row_bytes;
        if b + row_bytes > u.len() {
            break;
        }
        let data_size = u32at(u, b + 0x28);
        if data_size == 0 {
            continue;
        }
        let pos = u32at(u, b);
        let vert_count = u16at(u, b + 0x04);
        let poly_count = u16at(u, b + 0x06) as u32 + u16at(u, b + 0x08) as u32;
        let vert_offset = u32at(u, b + 0x14);
        let poly_offset = u32at(u, b + 0x1C);
        let plain = u32at(u, b + 0x18) == vert_offset + align16(vert_count as usize * 12) as u32
            && u32at(u, b + 0x20) == poly_offset + align16(poly_count as usize * 24) as u32
            && data_size as usize
                == align16(vert_count as usize * 12) + align16(poly_count as usize * 24);
        groups.push(NavGroup {
            row,
            layer: row / per_layer,
            x: (pos & 0xFFFF) as i32 - NAV_POS_BIAS,
            z: (pos >> 16) as i32 - NAV_POS_BIAS,
            vert_count,
            poly_count,
            vert_offset: b as u32 + vert_offset,
            poly_offset: b as u32 + poly_offset,
            data_size,
            plain,
        });
    }
    Some(NavUnit {
        magic: LE::read_u32(&u[0..4]),
        asset_id: LE::read_u64(&u[4..12]),
        container_id: LE::read_u64(&u[12..20]),
        min_x,
        min_z,
        max_x,
        max_z,
        log_scale,
        group_size_m: if log_scale >= 0 { 1 << log_scale } else { -1 },
        layer_count,
        groups_per_layer: per_layer,
        groups,
        section_len: u.len(),
    })
}

/// Build one merged walkable mesh from a unit. `layer` selects a nav layer, or
/// `None` merges them all.
pub fn nav_mesh(zone: &Zone, unit: &NavUnit, layer: Option<usize>) -> NavMesh {
    let Some(u) = zone.dat1.get_section_data(T_NAV_UNIT) else {
        return NavMesh::default();
    };
    let mut out = NavMesh::default();
    for (gi, g) in unit.groups.iter().enumerate() {
        if layer.is_some_and(|l| l != g.layer) {
            continue;
        }
        let vb = g.vert_offset as usize;
        let pb = g.poly_offset as usize;
        let vc = g.vert_count as usize;
        let pc = g.poly_count as usize;
        if vb + vc * 12 > u.len() || pb + pc * 24 > u.len() {
            continue;
        }
        let base = out.verts.len() as u32;
        for i in 0..vc {
            out.verts.push(vec3(u, vb + i * 12));
        }
        for i in 0..pc {
            let o = pb + i * 24;
            let t = [
                LE::read_u16(&u[o..o + 2]) as u32,
                LE::read_u16(&u[o + 2..o + 4]) as u32,
                LE::read_u16(&u[o + 4..o + 6]) as u32,
            ];
            if t.iter().any(|&v| v as usize >= vc) {
                continue;
            }
            out.tris.push([base + t[0], base + t[1], base + t[2]]);
            out.tri_group.push(gi as u32);
        }
    }
    out
}

/// One `dtTileCacheLayerHeader` out of `Zone Nav Built Tile Cache`.
///
/// The section is a flat run of `{u32 compressed_size; header+payload;
/// pad to 4; u32 runtime ref slot}` records, with a single `u32 0` terminator
/// after each nav layer's run — which is how the layer count can be
/// cross-checked without reading the unit.
///
/// The header itself is stock Recast/Detour: magic `'DTLR'`, version 1, the
/// tile's (tx, ty, layer), the tile's world AABB as floats, a quantised height
/// range and the 128x128 cell grid's used sub-rectangle. The payload after it is
/// here; the header alone already gives a viewer the exact footprint and height
/// span of every navigation tile.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavTile {
    pub record_offset: u32,
    pub compressed_size: u32,
    pub magic_ok: bool,
    pub version: u32,
    pub tx: i32,
    pub ty: i32,
    pub tlayer: i32,
    pub bmin: [f32; 3],
    pub bmax: [f32; 3],
    pub hmin: u16,
    pub hmax: u16,
    pub width: u8,
    pub height: u8,
    pub minx: u8,
    pub maxx: u8,
    pub miny: u8,
    pub maxy: u8,
}

/// `DT_TILECACHE_MAGIC` = `'D'<<24 | 'T'<<16 | 'L'<<8 | 'R'`, which lands on
/// disk as the four bytes `RLTD`.
pub const DT_TILECACHE_MAGIC: u32 = 0x4454_4C52;
/// 4 (size word) + 56 (header) is the smallest useful record.
const TC_HEADER: usize = 56;

/// Walk the tile cache. Returns the tiles and the number of layer terminators,
/// which equals the unit's nav layer count.
pub fn nav_tiles(zone: &Zone) -> (Vec<NavTile>, usize) {
    let Some(tc) = zone.dat1.get_section_data(T_NAV_TILECACHE) else {
        return (Vec::new(), 0);
    };
    let mut out = Vec::new();
    let mut terminators = 0usize;
    let mut p = 0usize;
    while p + 4 <= tc.len() {
        let sz = LE::read_u32(&tc[p..p + 4]) as usize;
        if sz == 0 {
            terminators += 1;
            p += 4;
            continue;
        }
        if p + 4 + sz > tc.len() {
            break;
        }
        if sz >= TC_HEADER {
            let h = &tc[p + 4..p + 4 + TC_HEADER];
            out.push(NavTile {
                record_offset: p as u32,
                compressed_size: sz as u32,
                magic_ok: LE::read_u32(&h[0..4]) == DT_TILECACHE_MAGIC,
                version: LE::read_u32(&h[4..8]),
                tx: LE::read_i32(&h[8..12]),
                ty: LE::read_i32(&h[12..16]),
                tlayer: LE::read_i32(&h[16..20]),
                bmin: vec3(h, 20),
                bmax: vec3(h, 32),
                hmin: LE::read_u16(&h[44..46]),
                hmax: LE::read_u16(&h[46..48]),
                width: h[48],
                height: h[49],
                minx: h[50],
                maxx: h[51],
                miny: h[52],
                maxy: h[53],
            });
        }
        p += 4 + ((sz + 3) & !3) + 4;
    }
    (out, terminators)
}

/// Does this zone hold navigation data rather than scene objects?
pub fn has_nav(zone: &Zone) -> bool {
    zone.dat1.get_section_data(T_NAV_UNIT).is_some()
        || zone.dat1.get_section_data(T_NAV_CONTAINER).is_some()
}

// ---------------------------------------------------------------------------
// one-call summary for the UI
// ---------------------------------------------------------------------------

/// Everything the viewport needs for a zone's non-model entities.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneEntities {
    pub volumes: Vec<VolumeEntity>,
    pub curves: Vec<CurveEntity>,
    pub vfx: Vec<VfxEntity>,
    pub decals: Vec<DecalEntity>,
    pub nav_container: Option<NavContainer>,
    pub nav_unit: Option<NavUnit>,
    pub nav_tile_count: usize,
}

pub fn zone_entities(zone: &Zone) -> ZoneEntities {
    let (tiles, _) = nav_tiles(zone);
    ZoneEntities {
        volumes: volumes(zone),
        curves: curves(zone),
        vfx: vfx_instances(zone),
        decals: decals(zone),
        nav_container: nav_container(zone),
        nav_unit: nav_unit(zone),
        nav_tile_count: tiles.len(),
    }
}

fn ser_u64_str<S: serde::Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&format!("{v:016X}"))
}
