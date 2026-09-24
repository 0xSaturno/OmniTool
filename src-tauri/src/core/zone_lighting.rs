//! Zone lighting and atmosphere: the typed light list, the lighting-condition
//! asset blocks, and the env-probe table of a `.zone`.
//!
//! # Where the numbers come from
//!
//! A light is a **kind-1 entry of the `Zone Scene Objects` heap** (`0x06ABCAB2`),
//! addressed by the `u32` byte offsets in `Zone Lights` (`0x97FF6EB5`). Every
//! heap entry opens with the same 0x80-byte scene-object header that
//! [`crate::core::zone`] already decodes; a light's own body follows at +0x80
//! and is 0x70 bytes, so a light entry is 0xF0 = 240 bytes plus an optional
//! inline asset path.
//!
//! then re-measured on the shipped PC build (see `bin/light_probe.rs`):
//!
//! ```text
//!   +0x80  u64 tuid                     authoring uid
//!   +0x88  u64 volume_link              uid of a Zone Volumes entry, or 0
//!   +0x90  u16 flags                    see LIGHT_FLAG_NAMES
//!   +0x92  u8  type                     0 point 1 spot 2 pyramid 3 shaft
//!                                       4 modulate 5 lightgridmod
//!   +0x93  u8  lighting_conditions      bitmask over LIGHTING_CONDITIONS
//!   +0x94  u8  shadow_cast_type         0 none 1 cache-static 2 only-static
//!                                       3 only-dynamic
//!   +0x95  u8  shadow_res               0 low 1 normal 2 high 3 cinematic
//!   +0x96  u16 asset_offset             byte offset FROM THE ENTRY START to an
//!                                       inline NUL-terminated asset path, or 0
//!   +0x98  u32 platform_mask
//!   +0x9C  u32 shadow_platform_mask
//!   +0xA0  f32 linear_rgb[3]            colour * intensity, LINEAR space
//!   +0xAC  f16 near_fade_clip_dist
//!   +0xAE  f16 near_fade_start_dist
//!   +0xB0  f32 volumetric_fog_intensity
//!   +0xB4  u32 gobo_custom_data[2]
//!   +0xBC  f32 fade_out_dist            | saturation          (volume lights)
//!   +0xC0  f32 attenuation_radius       | falloff_dist_neg.x  (volume lights)
//!   +0xC4  f32 cut_off_dist             | falloff_dist_neg.y
//!   +0xC8  f32 cut_on_dist              | falloff_dist_neg.z
//!   +0xCC  f32 spec_intensity           | falloff_dist_pos.x
//!   +0xD0  f32 spec_fade_out_dist       | falloff_dist_pos.y
//!   +0xD4  f32 shadow_fade_out_dist     | falloff_dist_pos.z
//!   +0xD8  f32 inner_cone_radians       spot / pyramid only
//!   +0xDC  f32 outer_cone_radians       spot / pyramid only
//!   +0xE0  f32 bulb_radius              area-light bulb, metres
//!   +0xE4  f32 bulb_length
//!   +0xE8  f32 shadow_cut_on_dist
//!   +0xEC  f32 bulb_push_forward
//! ```
//!
//! The unions at +0xBC..+0xD8 are **type-selected, not alternatives to guess**:
//! types 4 (modulate) and 5 (lightgridmod) are box/ellipsoid volume lights and
//! read +0xC0..+0xD8 as per-axis falloff distances; every other type reads the
//! attenuation/spec/shadow set. Only type 5 reinterprets +0xBC as `saturation`
//! light still has a `fade_out_dist` there. A renderer must branch on `type`.
//!
//! field with `buffer->AddElem(...)` and asserts that the bytes written equal
//! +0x7C length).
//!
//! # What was measured on the shipped PC build
//!
//! 9 046 zones parsed; **261** carry a `Zone Lights` section, holding **7 767**
//! lights. 7 044 entries are exactly 240 bytes and the other 723 are
//! 240 + an inline asset path, every one of which read back as a printable
//! `.material` / `.materialgraph` / `.texture` gobo path (0 unreadable).
//!
//! ```text
//!   type        count  shadow casters
//!   0 point       322     26 ( 8.1%)
//!   1 spot       7172   4393 (61.3%)
//!   2 pyramid      28      6 (21.4%)
//!   3 shaft        13      1 ( 7.7%)
//!   4 modulate    232      0
//!   5 lightgridmod  0            <- never authored into a zone
//! ```
//!
//! Shadow cast type is only ever 0 (none, 3 341) or 1 (cache-static, 4 426);
//! shadow res is 0..3 with 7 163 "normal"; spec type is 0..2 with 7 629 "both".
//! **No light carries a flag bit above 9**, i.e. the runtime-only bits are
//! always clear on disk, which is what makes the 16-bit flag word safe to trust.
//! `UseColorTemperature` is set on 4 112 (52.9%) — the colour on disk is already
//! resolved, so a viewer ignores that bit.
//!
//! Two fields the header alone would have you read wrong:
//!
//! * **Cone angles are populated on every type.** All 322 point, 13 shaft and
//!   232 modulate lights carry the builder's defaults (15 deg inner, 45 deg
//!   see [`has_cone`].
//! * **A volume light's extents are not in the header's extents field.** That
//!   field is 0.0 on all 232 modulate lights;
//!   *unnormalised* matrix rows instead, which measure 0.61 m to 916 m
//!   (median 15.0). [`ZoneLight::extents`] does that.
//!
//! `gobo_custom_data` is non-zero on all 7 767 lights, but only in one place:
//! word 0 is always `0x00000000`, and word 1's **high half** holds one of 251
//! distinct `f16` values spread over 0.05..0.9 with its low half always 0. It
//! reaches the renderer as `custom_xyzw` in `DecalSubmitGoboShared`, so it reads
//! as a `float4` packed as four halves with only `.w` authored — consistent with
//! a per-light animation phase. Not needed to light a zone.
//!
//! # What a renderer needs
//!
//! * **Direction.** The scene header's row-major matrix rows are
//!   `0 = right, 1 = up, 2 = forward, 3 = translation`. A spot/pyramid/shaft
//!   points down row 2.
//! * **Radius.** `attenuation_radius` is authoritative when `> 0`. When it is
//!   `0` the engine *derives* one from the colour:
//!   `sqrt(luminance(linear_rgb) / 0.125)` with
//!   `luminance = 0.2126 r + 0.7152 g + 0.0722 b`. [`ZoneLight::effective_radius`]
//!   reproduces that, so a viewer never has to special-case the zero.
//! * **Cones.** `inner_cone_radians` / `outer_cone_radians` are half-angles from
//!   the axis. The engine clamps `inner` to `outer * 0.99` and builds the
//!   falloff as `saturate((cos t - cos outer) / (cos inner' - cos outer))`
//!   **squared**.
//! * **Cut planes.** For spot and pyramid lights `cut_on_dist` / `cut_off_dist`
//!   are *depths along row 2*, not radii; for point lights `cut_off_dist` is a
//!   radius that clamps the bounding sphere.
//!
//! # Lighting conditions
//!
//! `LightingConditions` is a five-item select: day, overcast, sunset, night,
//! blackout. Two mechanisms use it.
//!
//! 1. **Per-light**, through the `lighting_conditions` bitmask at +0x93: the
//!    engine creates every light in `Zone Lights` regardless, then excludes the
//!    ones whose mask misses the scene's current condition bit.
//! 2. **Per-asset-reference**, through a shared block layout used by
//!    `Zone Atmosphere Name`, `Zone Level Light Name`, `Zone EnvProbe Assets`
//!    and the light-grid path block:
//!
//! ```text
//!   u32 condition_count             0 = this block is condition-independent
//!   u32 condition_name_hash[count]  present only when count != 0
//!   <pad to an 8-byte boundary>
//!   AssetRef[count * stride]        stride = 1, except EnvProbe Assets
//!   AssetRef = { u64 asset_id, u32 string_offset, u32 suffix_hash }
//! ```
//!
//!   The engine indexes row `current_condition` of that array, so a zone does
//!   not *choose* a condition: the **scene** holds one and every
//!   condition-dependent block is indexed by it.
//!
//! Measured: `condition_count` is only ever 0 or 5, the five hashes always equal
//! `crc32("kDay"), crc32("kOvercast"), crc32("kSunset"), crc32("kNight"),
//! crc32("kBlackout")` = `0x7F6D76F2, 0xAFE16884, 0xA30EF4DD, 0xFC1CC2E5,
//! 0xA82BE092`, and **row i really is condition i**: over 3 369 rows whose
//! referenced file name mentions a condition word, every single one matches its
//! own row. (The 85 apparent misses are all one directory named
//! `atm_sargasso_sunset/` holding a differently-named atmosphere.) A light grid
//! reference reads, in order,
//! `tile_o28_light_grid_Day/Overcast/Sunset/Night/Blackout.zonelightbin`.
//!
//! Per-light masks in shipped content: 6 578 of 7 767 lights are lit under all
//! five conditions, 1 055 are night-only, and the rest sit on 7 other masks.
//!
//! Nothing here mutates a zone. Lighting edits would have to go through
//! [`crate::core::zone`]'s equal-length in-place patch path.

use serde::{Deserialize, Serialize};

use crate::core::dat1::Dat1;
use crate::core::zone::{Zone, KIND_LIGHT};
/// `Zone Lights` — `u32[]` of scene-object byte offsets, re-exported so lighting
/// callers do not have to reach into [`crate::core::zone`].
pub use crate::core::zone::T_LIGHTS;

/// `Zone Atmosphere Name` — one atmosphere asset reference per lighting condition.
pub const T_ATMOSPHERE_NAME: u32 = 0xB6A0_B72A;
/// `Zone LevelLight Name` — the `.levellight` asset reference, per condition.
///
/// The 534-name candidate list offers `Zone Level Light Name` = `0x7A6075BE`,
/// which **never appears** in a shipped PC zone. The tag the build actually
/// writes is `0x1951BA1F`, recovered by inverting the hash:
/// `crc32("Zone LevelLight Name") == 0x1951BA1F` (no space in `LevelLight`).
/// Measured on 6 zones; `0x7A6075BE` on 0 of 9 046.
pub const T_LEVEL_LIGHT_NAME: u32 = 0x1951_BA1F;
/// The unused candidate spelling, kept so a reader recognises it.
pub const T_LEVEL_LIGHT_NAME_UNUSED: u32 = 0x7A60_75BE;
/// `Zone EnvProbe Assets` — env-probe texture references, `probe_count` per condition.
pub const T_ENVPROBE_ASSETS: u32 = 0x0277_95C5;
/// `Zone EnvProbe List` — 128-byte `EnvProbeBuilt` rows.
pub const T_ENVPROBE_LIST: u32 = 0x5570_13E9;
/// `Zone Env Probe Draw Lists` — cached per-face model-inst tuid lists.
pub const T_ENVPROBE_DRAW_LISTS: u32 = 0xDA9E_4CDF;
/// `Zone LightBin Asset` — the `.zonelightbin` reference, per condition.
///
/// Same story as the level-light block: the candidate list's
/// `Zone Light Bin Asset` = `0xE14D4601` appears in 0 of 9 046 shipped zones,
/// while `crc32("Zone LightBin Asset") == 0xEDFA607D` appears in 714 of them
/// (668 with `condition_count == 5`, 46 condition-independent) — the same 714
/// zones that carry `Zone Light Grid Positions`.
pub const T_LIGHT_BIN_ASSET: u32 = 0xEDFA_607D;
/// The unused candidate spelling.
pub const T_LIGHT_BIN_ASSET_UNUSED: u32 = 0xE14D_4601;
/// `Zone Light Grid Positions` — `u32[]` position hashes for shared open-world grids.
pub const T_LIGHT_GRID_POSITIONS: u32 = 0x57D2_5F50;
/// `Lighting Condition References`, seen on level/region assets.
pub const T_LIGHTING_CONDITION_REFS: u32 = 0x1999_D0E3;

const OFF_BODY: usize = 0x80;
pub const LIGHT_BODY_LEN: usize = 0x70;
/// A light entry with no inline asset path is exactly this long.
pub const LIGHT_ENTRY_LEN: usize = OFF_BODY + LIGHT_BODY_LEN;

const OFF_TUID: usize = 0x80;
const OFF_VOLUME_LINK: usize = 0x88;
const OFF_FLAGS: usize = 0x90;
const OFF_TYPE: usize = 0x92;
const OFF_CONDITIONS: usize = 0x93;
const OFF_SHADOW_CAST: usize = 0x94;
const OFF_SHADOW_RES: usize = 0x95;
const OFF_ASSET_OFFSET: usize = 0x96;
const OFF_PLATFORM_MASK: usize = 0x98;
const OFF_SHADOW_PLATFORM_MASK: usize = 0x9C;
const OFF_LINEAR_RGB: usize = 0xA0;
const OFF_NEAR_FADE_CLIP: usize = 0xAC;
const OFF_NEAR_FADE_START: usize = 0xAE;
const OFF_VOLUMETRIC_FOG: usize = 0xB0;
const OFF_GOBO: usize = 0xB4;
const OFF_FADE_OUT_DIST: usize = 0xBC;
const OFF_UNION0: usize = 0xC0;
const OFF_INNER_CONE: usize = 0xD8;
const OFF_OUTER_CONE: usize = 0xDC;
const OFF_BULB_RADIUS: usize = 0xE0;
const OFF_BULB_LENGTH: usize = 0xE4;
const OFF_SHADOW_CUT_ON: usize = 0xE8;
const OFF_BULB_PUSH_FORWARD: usize = 0xEC;

pub const LIGHT_TYPE_POINT: u8 = 0;
pub const LIGHT_TYPE_SPOT: u8 = 1;
pub const LIGHT_TYPE_PYRAMID: u8 = 2;
pub const LIGHT_TYPE_SHAFT: u8 = 3;
pub const LIGHT_TYPE_MODULATE: u8 = 4;
pub const LIGHT_TYPE_LIGHTGRIDMOD: u8 = 5;
pub const LIGHT_TYPE_COUNT: u8 = 6;

/// Engine names, in serialized order.
pub const LIGHT_TYPE_NAMES: [&str; 6] = [
    "point",
    "spot",
    "pyramid",
    "shaft",
    "modulate",
    "lightgridmod",
];

pub fn light_type_name(t: u8) -> &'static str {
    LIGHT_TYPE_NAMES.get(t as usize).copied().unwrap_or("unknown")
}

/// True for the two volume lights, which reinterpret the +0xC0..+0xD8 unions as
/// per-axis falloff distances and take their box extents from the *unnormalised*
/// lengths of matrix rows 0..2 rather than from the header's extents field.
///
/// Only `lightgridmod` also reinterprets +0xBC as `saturation`; `modulate` still
pub fn is_volume_light(t: u8) -> bool {
    t == LIGHT_TYPE_MODULATE || t == LIGHT_TYPE_LIGHTGRIDMOD
}

/// True for the types whose cone half-angles are meaningful.
pub fn has_cone(t: u8) -> bool {
    t == LIGHT_TYPE_SPOT || t == LIGHT_TYPE_PYRAMID
}

/// Serialized light flag bits, bit 0 first. Bits 0..1 are the spec-type field,
/// not independent flags, so they are reported through
/// [`ZoneLight::spec_type_name`] instead and named `SpecType*` here.
pub const LIGHT_FLAG_NAMES: [&str; 16] = [
    "SpecTypeBit0",
    "SpecTypeBit1",
    "Active",
    "SkipLightCapture",
    "OnlyLightCapture",
    "BulbIsRectangle",
    "UseColorTemperature",
    "AllowVFXLightMultiplier",
    "VolumeIsEllipsoid",
    "EarlyModLight",
    "bit10",
    "bit11",
    "bit12",
    "bit13",
    "bit14",
    "bit15",
];

pub const LIGHT_FLAG_SPEC_TYPE_MASK: u16 = 3;
pub const LIGHT_FLAG_ACTIVE: u16 = 1 << 2;
pub const LIGHT_FLAG_SKIP_LIGHT_CAPTURE: u16 = 1 << 3;
pub const LIGHT_FLAG_ONLY_LIGHT_CAPTURE: u16 = 1 << 4;
pub const LIGHT_FLAG_BULB_IS_RECTANGLE: u16 = 1 << 5;
pub const LIGHT_FLAG_USE_COLOR_TEMPERATURE: u16 = 1 << 6;
pub const LIGHT_FLAG_ALLOW_VFX_MULTIPLIER: u16 = 1 << 7;
pub const LIGHT_FLAG_VOLUME_IS_ELLIPSOID: u16 = 1 << 8;
pub const LIGHT_FLAG_EARLY_MOD_LIGHT: u16 = 1 << 9;
/// Bits above 9 are runtime-only in the engine and must be zero on disk.
pub const LIGHT_FLAG_SERIALIZED_MASK: u16 = 0x03FF;

pub fn light_flag_names(flags: u16) -> Vec<&'static str> {
    (2..16)
        .filter(|b| flags & (1 << b) != 0)
        .map(|b| LIGHT_FLAG_NAMES[b])
        .collect()
}

pub const SPEC_TYPE_NAMES: [&str; 3] = ["both", "diffuse-only", "spec-only"];
pub const SHADOW_RES_NAMES: [&str; 4] = ["low", "normal", "high", "cinematic"];
pub const SHADOW_CAST_NAMES: [&str; 4] = [
    "none",
    "cache-static-models",
    "only-static-models",
    "only-dynamic-models",
];

/// The `LightingConditions` select, in serialized index order. The scene holds
/// one index; a light is visible when its mask has `1 << index` set.
pub const LIGHTING_CONDITIONS: [&str; 5] = ["day", "overcast", "sunset", "night", "blackout"];
pub const LIGHTING_CONDITION_COUNT: usize = 5;
pub const LIGHTING_CONDITION_ALL: u8 = (1 << LIGHTING_CONDITION_COUNT) - 1;

pub fn lighting_condition_names(mask: u8) -> Vec<&'static str> {
    if mask == 0 {
        // The engine treats a zero mask on a *volume* as "all"; on a light a
        // zero mask excludes it under every condition.
        return Vec::new();
    }
    (0..LIGHTING_CONDITION_COUNT)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| LIGHTING_CONDITIONS[i])
        .collect()
}

pub fn luminance(rgb: [f32; 3]) -> f32 {
    rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722
}

/// The engine's implicit radius for a light that ships `attenuation_radius == 0`:
/// the distance at which the light's luminance falls to `0.125`.
pub fn derived_radius(rgb: [f32; 3]) -> f32 {
    (luminance(rgb).max(0.0) * 8.0).sqrt()
}

/// IEEE 754 binary16 -> f32. Near-fade distances are stored as halves.
pub fn half_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) & 1) as u32;
    let exp = ((h >> 10) & 0x1F) as u32;
    let mant = (h & 0x3FF) as u32;
    let bits = match exp {
        0 if mant == 0 => sign << 31,
        0 => {
            // Subnormal: renormalise.
            let mut e = -1i32;
            let mut m = mant;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            m &= 0x3FF;
            (sign << 31) | (((e + 127 + 1) as u32) << 23) | (m << 13)
        }
        0x1F => (sign << 31) | 0x7F80_0000 | (mant << 13),
        _ => (sign << 31) | ((exp + 112) << 23) | (mant << 13),
    };
    f32::from_bits(bits)
}

fn rd_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn rd_u64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}
fn rd_f32(b: &[u8], o: usize) -> f32 {
    f32::from_bits(rd_u32(b, o))
}
fn rd_f32x3(b: &[u8], o: usize) -> [f32; 3] {
    [rd_f32(b, o), rd_f32(b, o + 4), rd_f32(b, o + 8)]
}

/// A light of one zone, decoded into units a renderer can use directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneLight {
    /// Byte offset in `Zone Scene Objects`; the light's identity inside the zone.
    pub offset: u32,
    /// Exact entry length from the scene header at +0x7C.
    pub entry_size: u32,
    #[serde(serialize_with = "ser_u64_str")]
    pub tuid: u64,
    /// Uid of the `Zone Volumes` entry that clips this light, 0 when unlinked.
    #[serde(serialize_with = "ser_u64_str")]
    pub volume_link: u64,

    pub light_type: u8,
    pub light_type_name: String,
    pub flags: u16,
    pub flag_names: Vec<String>,
    pub spec_type: u8,
    pub spec_type_name: String,
    pub shadow_cast_type: u8,
    pub shadow_cast_type_name: String,
    pub shadow_res: u8,
    pub shadow_res_name: String,
    pub casts_shadows: bool,
    /// `true` when the serialized `Active` bit is set.
    pub active: bool,
    /// Bulb is a rectangle rather than a capsule/sphere.
    pub bulb_is_rectangle: bool,
    /// The modulate/lightgridmod volume is an ellipsoid rather than a box.
    pub volume_is_ellipsoid: bool,

    pub lighting_conditions: u8,
    pub lighting_condition_names: Vec<String>,
    pub platform_mask: u32,
    pub shadow_platform_mask: u32,

    /// World position: row 3 of the scene-object matrix.
    pub position: [f32; 3],
    /// Row 2 of the matrix, normalised: the axis a spot/pyramid/shaft points down.
    pub direction: [f32; 3],
    /// Row 0 and row 1, normalised, for oriented volumes and rectangular bulbs.
    pub right: [f32; 3],
    pub up: [f32; 3],
    /// The scene header's cached transform scale at +0x70.
    pub transform_scale: [f32; 3],
    /// Object-space aabb extents from the scene header at +0x50, in metres.
    /// Zero on every shipped light; use [`ZoneLight::extents`] instead.
    pub header_extents: [f32; 3],
    /// Half-extents of a volume light's box/ellipsoid, in metres: the lengths of
    /// the unnormalised matrix rows 0..2, exactly as
    /// harmless, normally 1,1,1) for the four non-volume types.
    pub extents: [f32; 3],

    /// Colour premultiplied by intensity, in **linear** space.
    pub linear_rgb: [f32; 3],
    pub luminance: f32,
    /// `attenuation_radius` when authored, otherwise the engine's derived radius.
    pub effective_radius: f32,
    /// `true` when `effective_radius` came from the colour, not from the file.
    pub radius_is_derived: bool,

    pub near_fade_clip_dist: f32,
    pub near_fade_start_dist: f32,
    pub volumetric_fog_intensity: f32,
    pub gobo_custom_data: [u32; 2],

    /// Types 0..3. `fade_out_dist` is the LOD distance at which the light dies.
    pub fade_out_dist: f32,
    pub attenuation_radius: f32,
    /// Point: a radius clamp. Spot / pyramid: a far depth plane along `direction`.
    pub cut_off_dist: f32,
    /// Spot / pyramid: a near depth plane along `direction`.
    pub cut_on_dist: f32,
    pub spec_intensity: f32,
    pub spec_fade_out_dist: f32,
    pub shadow_fade_out_dist: f32,
    pub shadow_cut_on_dist: f32,

    /// Type 5 only: light-grid saturation modifier (shares `fade_out_dist`).
    pub saturation: f32,
    /// Types 4..5 only: falloff distances along the negative / positive axes.
    pub falloff_dist_neg: [f32; 3],
    pub falloff_dist_pos: [f32; 3],

    /// Half-angle from `direction` to the start of the cone falloff, radians.
    pub inner_cone_radians: f32,
    /// Half-angle to the end of the cone falloff, radians.
    pub outer_cone_radians: f32,
    pub inner_cone_degrees: f32,
    pub outer_cone_degrees: f32,

    pub bulb_radius: f32,
    pub bulb_length: f32,
    pub bulb_push_forward: f32,

    /// Raw +0x96 field: byte offset from the entry start to an inline path.
    pub asset_offset: u16,
    /// The inline gobo / projected-material / IES path, when one is present.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub asset_path: String,
}

fn ser_u64_str<S: serde::Serializer>(v: &u64, s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_str(&v.to_string())
}

fn norm(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l > 1e-9 {
        [v[0] / l, v[1] / l, v[2] / l]
    } else {
        [0.0, 0.0, 0.0]
    }
}

impl ZoneLight {
    /// Decode one light body out of the raw `Zone Scene Objects` bytes.
    ///
    /// Returns `None` when the offset does not hold a complete light entry.
    pub fn decode(scene: &[u8], offset: u32) -> Option<ZoneLight> {
        let base = offset as usize;
        let e = scene.get(base..base.checked_add(LIGHT_ENTRY_LEN)?)?;

        let entry_size = rd_u32(e, 0x7C) & 0x00FF_FFFF;
        let flags_word = rd_u32(e, 0x5C);
        let kind = (flags_word >> 24) as u8;
        if kind != KIND_LIGHT {
            return None;
        }

        let light_type = e[OFF_TYPE];
        let flags = rd_u16(e, OFF_FLAGS);
        let spec_type = (flags & LIGHT_FLAG_SPEC_TYPE_MASK) as u8;
        let shadow_cast_type = e[OFF_SHADOW_CAST];
        let shadow_res = e[OFF_SHADOW_RES];
        let conditions = e[OFF_CONDITIONS];

        let m: Vec<f32> = (0..16).map(|i| rd_f32(e, i * 4)).collect();
        let linear_rgb = rd_f32x3(e, OFF_LINEAR_RGB);
        let attenuation_radius = rd_f32(e, OFF_UNION0);
        let volume = is_volume_light(light_type);
        let radius_is_derived = !volume && !(attenuation_radius > 0.0);
        let effective_radius = if volume {
            0.0
        } else if attenuation_radius > 0.0 {
            attenuation_radius.max(0.01)
        } else {
            derived_radius(linear_rgb)
        };

        let inner = rd_f32(e, OFF_INNER_CONE);
        let outer = rd_f32(e, OFF_OUTER_CONE);

        let asset_offset = rd_u16(e, OFF_ASSET_OFFSET);
        let asset_path = if asset_offset as usize >= LIGHT_ENTRY_LEN
            && (asset_offset as u32) < entry_size
        {
            let s = base + asset_offset as usize;
            let end = (base + entry_size as usize).min(scene.len());
            scene
                .get(s..end)
                .map(|t| {
                    let n = t.iter().position(|&b| b == 0).unwrap_or(t.len());
                    String::from_utf8_lossy(&t[..n]).replace('\\', "/")
                })
                .unwrap_or_default()
        } else {
            String::new()
        };

        Some(ZoneLight {
            offset,
            entry_size,
            tuid: rd_u64(e, OFF_TUID),
            volume_link: rd_u64(e, OFF_VOLUME_LINK),
            light_type,
            light_type_name: light_type_name(light_type).to_string(),
            flags,
            flag_names: light_flag_names(flags).iter().map(|s| s.to_string()).collect(),
            spec_type,
            spec_type_name: SPEC_TYPE_NAMES
                .get(spec_type as usize)
                .copied()
                .unwrap_or("invalid")
                .to_string(),
            shadow_cast_type,
            shadow_cast_type_name: SHADOW_CAST_NAMES
                .get(shadow_cast_type as usize)
                .copied()
                .unwrap_or("invalid")
                .to_string(),
            shadow_res,
            shadow_res_name: SHADOW_RES_NAMES
                .get(shadow_res as usize)
                .copied()
                .unwrap_or("invalid")
                .to_string(),
            casts_shadows: shadow_cast_type != 0,
            active: flags & LIGHT_FLAG_ACTIVE != 0,
            bulb_is_rectangle: flags & LIGHT_FLAG_BULB_IS_RECTANGLE != 0,
            volume_is_ellipsoid: flags & LIGHT_FLAG_VOLUME_IS_ELLIPSOID != 0,
            lighting_conditions: conditions,
            lighting_condition_names: lighting_condition_names(conditions)
                .iter()
                .map(|s| s.to_string())
                .collect(),
            platform_mask: rd_u32(e, OFF_PLATFORM_MASK),
            shadow_platform_mask: rd_u32(e, OFF_SHADOW_PLATFORM_MASK),
            position: [m[12], m[13], m[14]],
            direction: norm([m[8], m[9], m[10]]),
            right: norm([m[0], m[1], m[2]]),
            up: norm([m[4], m[5], m[6]]),
            transform_scale: rd_f32x3(e, 0x70),
            header_extents: rd_f32x3(e, 0x50),
            extents: [
                (m[0] * m[0] + m[1] * m[1] + m[2] * m[2]).sqrt(),
                (m[4] * m[4] + m[5] * m[5] + m[6] * m[6]).sqrt(),
                (m[8] * m[8] + m[9] * m[9] + m[10] * m[10]).sqrt(),
            ],
            linear_rgb,
            luminance: luminance(linear_rgb),
            effective_radius,
            radius_is_derived,
            near_fade_clip_dist: half_to_f32(rd_u16(e, OFF_NEAR_FADE_CLIP)),
            near_fade_start_dist: half_to_f32(rd_u16(e, OFF_NEAR_FADE_START)),
            volumetric_fog_intensity: rd_f32(e, OFF_VOLUMETRIC_FOG),
            gobo_custom_data: [rd_u32(e, OFF_GOBO), rd_u32(e, OFF_GOBO + 4)],
            fade_out_dist: if light_type == LIGHT_TYPE_LIGHTGRIDMOD {
                0.0
            } else {
                rd_f32(e, OFF_FADE_OUT_DIST)
            },
            attenuation_radius: if volume { 0.0 } else { attenuation_radius },
            cut_off_dist: if volume { 0.0 } else { rd_f32(e, OFF_UNION0 + 4) },
            cut_on_dist: if volume { 0.0 } else { rd_f32(e, OFF_UNION0 + 8) },
            spec_intensity: if volume { 1.0 } else { rd_f32(e, OFF_UNION0 + 12) },
            spec_fade_out_dist: if volume { 0.0 } else { rd_f32(e, OFF_UNION0 + 16) },
            shadow_fade_out_dist: if volume { 0.0 } else { rd_f32(e, OFF_UNION0 + 20) },
            shadow_cut_on_dist: rd_f32(e, OFF_SHADOW_CUT_ON),
            saturation: if light_type == LIGHT_TYPE_LIGHTGRIDMOD {
                rd_f32(e, OFF_FADE_OUT_DIST)
            } else {
                0.0
            },
            falloff_dist_neg: if volume { rd_f32x3(e, OFF_UNION0) } else { [0.0; 3] },
            falloff_dist_pos: if volume { rd_f32x3(e, OFF_UNION0 + 12) } else { [0.0; 3] },
            inner_cone_radians: inner,
            outer_cone_radians: outer,
            inner_cone_degrees: inner.to_degrees(),
            outer_cone_degrees: outer.to_degrees(),
            bulb_radius: rd_f32(e, OFF_BULB_RADIUS),
            bulb_length: rd_f32(e, OFF_BULB_LENGTH),
            bulb_push_forward: rd_f32(e, OFF_BULB_PUSH_FORWARD),
            asset_offset,
            asset_path,
        })
    }

    /// The two cone-falloff constants the engine derives, so a viewer can use
    /// the same maths: `intensity = saturate(dot * x + y)` squared, with `dot`
    /// taken between `direction` and the light-to-sample direction.
    pub fn cone_falloff_consts(&self) -> Option<[f32; 2]> {
        if !has_cone(self.light_type) {
            return None;
        }
        if self.inner_cone_radians == 0.0 && self.outer_cone_radians == 0.0 {
            return None;
        }
        let inner = self
            .inner_cone_radians
            .clamp(0.0, self.outer_cone_radians * 0.99);
        let cos_inner = inner.cos();
        let cos_outer = self.outer_cone_radians.cos();
        let d = cos_inner - cos_outer;
        if d.abs() < 1e-9 {
            return None;
        }
        let f = 1.0 / d;
        Some([f, -cos_outer * f])
    }

    /// A conservative world-space bounding sphere radius for culling.
    pub fn bounding_radius(&self) -> f32 {
        if is_volume_light(self.light_type) {
            let e = self.extents;
            if self.volume_is_ellipsoid {
                e[0].max(e[1]).max(e[2])
            } else {
                (e[0] * e[0] + e[1] * e[1] + e[2] * e[2]).sqrt()
            }
        } else if self.cut_off_dist > 0.0 && self.light_type == LIGHT_TYPE_POINT {
            self.effective_radius.min(self.cut_off_dist)
        } else {
            self.effective_radius
        }
    }
}

/// Read the typed light list of a parsed zone.
pub fn read_zone_lights(zone: &Zone) -> Vec<ZoneLight> {
    read_zone_lights_from_dat1(&zone.dat1)
}

pub fn read_zone_lights_from_dat1(dat1: &Dat1) -> Vec<ZoneLight> {
    let Some(scene) = dat1.get_section_data(crate::core::zone::T_SCENE_OBJECTS) else {
        return Vec::new();
    };
    let Some(list) = dat1.get_section_data(T_LIGHTS) else {
        return Vec::new();
    };
    list.chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .filter_map(|off| ZoneLight::decode(scene, off))
        .collect()
}

// ---------------------------------------------------------------------------
// Lighting-condition asset blocks
// ---------------------------------------------------------------------------

/// One `AssetReferenceBuilt`: `{ u64 asset_id, u32 string_offset, u32 suffix_hash }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetRef {
    #[serde(serialize_with = "ser_u64_str")]
    pub asset_id: u64,
    pub string_offset: u32,
    pub suffix_hash: u32,
    /// Resolved out of the zone's own string pool.
    pub path: String,
}

pub const ASSET_REF_LEN: usize = 16;

/// `AssetRef::suffix_hash` is `crc32` of the referenced file's **extension,
/// including the dot**. Verified by inverting the hashes seen on disk.
pub const SUFFIX_ATMOSPHERE: u32 = 0xF56C_78E4; // ".atmosphere"
pub const SUFFIX_ZONELIGHTBIN: u32 = 0x8A82_587C; // ".zonelightbin"
pub const SUFFIX_LEVELLIGHT: u32 = 0x08BD_74BA; // ".levellight"
pub const SUFFIX_TEXTURE: u32 = 0x95A3_A227; // ".texture"

pub fn suffix_name(h: u32) -> &'static str {
    match h {
        SUFFIX_ATMOSPHERE => ".atmosphere",
        SUFFIX_ZONELIGHTBIN => ".zonelightbin",
        SUFFIX_LEVELLIGHT => ".levellight",
        SUFFIX_TEXTURE => ".texture",
        0 => "",
        _ => "?",
    }
}

/// A decoded lighting-condition asset block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConditionBlock {
    pub tag: u32,
    /// 0 means the block is condition-independent and holds exactly `stride` refs.
    pub condition_count: u32,
    /// The select's own item-name hashes, as serialized, for validation.
    pub condition_hashes: Vec<u32>,
    /// Refs per condition. 1 for atmosphere / level light / light bin.
    pub stride: usize,
    /// `condition_count.max(1)` rows of `stride` refs each.
    pub per_condition: Vec<Vec<AssetRef>>,
    /// True when every condition names the same asset id.
    pub uniform: bool,
    /// Bytes the block declares but that the decode did not consume.
    pub trailing_bytes: usize,
}

impl ConditionBlock {
    /// The refs the engine would use under one lighting condition index.
    pub fn refs_for(&self, condition: usize) -> &[AssetRef] {
        let row = if self.condition_count == 0 {
            0
        } else {
            condition.min(self.per_condition.len().saturating_sub(1))
        };
        self.per_condition.get(row).map(|v| v.as_slice()).unwrap_or(&[])
    }
}

/// Decode one of the lighting-condition asset blocks.
///
/// `stride` is the number of refs per condition: 1 for `Zone Atmosphere Name`,
/// `Zone Level Light Name` and `Zone Light Bin Asset`; for `Zone EnvProbe
/// Assets` it is the probe count, which the caller derives from
/// `Zone EnvProbe List`.
pub fn read_condition_block(dat1: &Dat1, tag: u32, stride: usize) -> Option<ConditionBlock> {
    let sec = dat1.get_section_data(tag)?;
    decode_condition_block(dat1, tag, sec, stride)
}

fn read_asset_ref(dat1: &Dat1, sec: &[u8], o: usize) -> AssetRef {
    let string_offset = rd_u32(sec, o + 8);
    AssetRef {
        asset_id: rd_u64(sec, o),
        string_offset,
        suffix_hash: rd_u32(sec, o + 12),
        path: dat1
            .get_string(string_offset)
            .unwrap_or_default()
            .replace('\\', "/"),
    }
}

/// Decode a lighting-condition block out of `sec`, which must start **at** the
/// block's `condition_count` word and be 8-byte aligned inside its section.
pub fn decode_condition_block(
    dat1: &Dat1,
    tag: u32,
    sec: &[u8],
    stride: usize,
) -> Option<ConditionBlock> {
    if sec.len() < 4 {
        return None;
    }
    let condition_count = rd_u32(sec, 0);
    let mut p = 4usize;
    let mut hashes = Vec::new();
    if condition_count != 0 {
        if condition_count as usize > 64 {
            return None;
        }
        for i in 0..condition_count as usize {
            let o = p + i * 4;
            if o + 4 > sec.len() {
                return None;
            }
            hashes.push(rd_u32(sec, o));
        }
        p += condition_count as usize * 4;
    }
    // ALIGN_8 on the pointer. Section data is placed at a 16-byte-aligned file
    // offset, so section-relative alignment matches absolute alignment.
    if p % 8 != 0 {
        p += 8 - (p % 8);
    }

    let rows = condition_count.max(1) as usize;
    let stride = stride.max(1);
    let mut per_condition = Vec::with_capacity(rows);
    for r in 0..rows {
        let mut refs = Vec::with_capacity(stride);
        for s in 0..stride {
            let o = p + (r * stride + s) * ASSET_REF_LEN;
            if o + ASSET_REF_LEN > sec.len() {
                break;
            }
            refs.push(read_asset_ref(dat1, sec, o));
        }
        per_condition.push(refs);
    }
    let consumed = p + rows * stride * ASSET_REF_LEN;
    let first = per_condition.first().and_then(|r| r.first()).map(|r| r.asset_id);
    let uniform = per_condition
        .iter()
        .all(|r| r.first().map(|x| x.asset_id) == first);

    Some(ConditionBlock {
        tag,
        condition_count,
        condition_hashes: hashes,
        stride,
        per_condition,
        uniform,
        trailing_bytes: sec.len().saturating_sub(consumed),
    })
}

/// How many `AssetRef`s a condition block actually holds per condition, derived
/// from the section size rather than guessed. Used for `Zone EnvProbe Assets`,
/// whose stride is the zone's probe count.
pub fn condition_block_stride(dat1: &Dat1, tag: u32) -> Option<usize> {
    let sec = dat1.get_section_data(tag)?;
    if sec.len() < 4 {
        return None;
    }
    let count = rd_u32(sec, 0);
    let mut p = 4usize + if count == 0 { 0 } else { count as usize * 4 };
    if p % 8 != 0 {
        p += 8 - (p % 8);
    }
    let rows = count.max(1) as usize;
    let refs = sec.len().saturating_sub(p) / ASSET_REF_LEN;
    Some(refs / rows.max(1))
}

/// `Zone EnvProbe Assets` (`0x027795C5`) is a lighting-condition block with a
/// **two-word prefix**, which is why decoding it as a bare condition block
/// mis-strides:
///
/// ```text
///   u32 override_texture_count
///   u32 probe_asset_count
///   <pad to 8>
///   AssetRef override_names[override_texture_count]   condition-independent
///   u32 condition_count                               the usual block starts here
///   u32 condition_name_hash[condition_count]
///   <pad to 8>
///   AssetRef per_condition[condition_count * probe_asset_count]
/// ```
///
/// The engine builds one texture array per zone as
/// `[overrides..., this condition's probe textures...]`, and
/// `EnvProbeRow::asset_index` indexes **that** array, so the valid range is
/// `0 .. override_texture_count + probe_asset_count`, with `-1` meaning
/// "no cube map of its own".
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvProbeAssets {
    pub override_texture_count: usize,
    pub probe_asset_count: usize,
    /// Condition-independent override cube maps.
    pub overrides: Vec<AssetRef>,
    /// The per-condition cube maps, `probe_asset_count` per condition.
    pub per_condition: ConditionBlock,
    /// `override_texture_count + probe_asset_count`: the length of the runtime
    /// texture array an `asset_index` points into.
    pub total_texture_count: usize,
}

pub fn read_env_probe_assets(dat1: &Dat1) -> Option<EnvProbeAssets> {
    let sec = dat1.get_section_data(T_ENVPROBE_ASSETS)?;
    if sec.len() < 8 {
        return None;
    }
    let override_texture_count = rd_u32(sec, 0) as usize;
    let probe_asset_count = rd_u32(sec, 4) as usize;
    let mut p = 8usize;
    if p % 8 != 0 {
        p += 8 - (p % 8);
    }
    let mut overrides = Vec::with_capacity(override_texture_count);
    for i in 0..override_texture_count {
        let o = p + i * ASSET_REF_LEN;
        if o + ASSET_REF_LEN > sec.len() {
            break;
        }
        overrides.push(read_asset_ref(dat1, sec, o));
    }
    p += override_texture_count * ASSET_REF_LEN;
    let per_condition = decode_condition_block(dat1, T_ENVPROBE_ASSETS, &sec[p..], probe_asset_count.max(1))?;
    Some(EnvProbeAssets {
        override_texture_count,
        probe_asset_count,
        overrides,
        per_condition,
        total_texture_count: override_texture_count + probe_asset_count,
    })
}

// ---------------------------------------------------------------------------
// Env probes
// ---------------------------------------------------------------------------

/// A 128-byte `EnvProbeBuilt` row of `Zone EnvProbe List` (`0x557013E9`).
///
/// ```text
///   +0x00 f32 pos[3]           +0x0C f32 axis_x[3]
///   +0x18 f32 axis_y[3]        +0x24 f32 axis_z[3]
///   +0x30 f32 extents[3]       +0x3C f32 probe_offset[3]
///   +0x48 f32 falloff_neg[3]   +0x54 f32 falloff_pos[3]
///   +0x60 u16 proxy_dist_neg[3]  +0x66 u16 proxy_dist_pos[3]
///   +0x6C u8 volume_shape  +0x6D u8 flags  +0x6E u16 pad
///   +0x70 f32 weight  +0x74 i16 asset_index  +0x76 i16 draw_list_index
///   +0x78 u64 tuid
/// ```
///
/// `flags` bit 0 is `DiffuseContribution`. `asset_index` selects the probe's
/// cube map out of `Zone EnvProbe Assets`; the cube map itself is 256x256x6
/// BC6H with a 6-mip GGX-convolved chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvProbeRow {
    pub index: usize,
    pub position: [f32; 3],
    pub axis_x: [f32; 3],
    pub axis_y: [f32; 3],
    pub axis_z: [f32; 3],
    pub extents: [f32; 3],
    pub probe_offset: [f32; 3],
    pub falloff_dist_neg: [f32; 3],
    pub falloff_dist_pos: [f32; 3],
    pub proxy_dist_neg: [u16; 3],
    pub proxy_dist_pos: [u16; 3],
    pub volume_shape: u8,
    pub flags: u8,
    pub diffuse_contribution: bool,
    pub weight: f32,
    pub asset_index: i16,
    pub draw_list_index: i16,
    #[serde(serialize_with = "ser_u64_str")]
    pub tuid: u64,
}

pub const ENV_PROBE_ROW_LEN: usize = 0x80;
pub const ENV_PROBE_FLAG_DIFFUSE_CONTRIBUTION: u8 = 1 << 0;

pub fn read_env_probes(dat1: &Dat1) -> Vec<EnvProbeRow> {
    let Some(sec) = dat1.get_section_data(T_ENVPROBE_LIST) else {
        return Vec::new();
    };
    sec.chunks_exact(ENV_PROBE_ROW_LEN)
        .enumerate()
        .map(|(i, r)| EnvProbeRow {
            index: i,
            position: rd_f32x3(r, 0x00),
            axis_x: rd_f32x3(r, 0x0C),
            axis_y: rd_f32x3(r, 0x18),
            axis_z: rd_f32x3(r, 0x24),
            extents: rd_f32x3(r, 0x30),
            probe_offset: rd_f32x3(r, 0x3C),
            falloff_dist_neg: rd_f32x3(r, 0x48),
            falloff_dist_pos: rd_f32x3(r, 0x54),
            proxy_dist_neg: [rd_u16(r, 0x60), rd_u16(r, 0x62), rd_u16(r, 0x64)],
            proxy_dist_pos: [rd_u16(r, 0x66), rd_u16(r, 0x68), rd_u16(r, 0x6A)],
            volume_shape: r[0x6C],
            flags: r[0x6D],
            diffuse_contribution: r[0x6D] & ENV_PROBE_FLAG_DIFFUSE_CONTRIBUTION != 0,
            weight: rd_f32(r, 0x70),
            asset_index: rd_u16(r, 0x74) as i16,
            draw_list_index: rd_u16(r, 0x76) as i16,
            tuid: rd_u64(r, 0x78),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Baked light grid (.zonelightbin)
// ---------------------------------------------------------------------------

/// One grid block is 16x16x16 samples at **one sample per metre**, so a block
/// comment says the width is "in both meters and samples currently").
pub const LIGHT_GRID_WIDTH: usize = 16;
pub const LIGHT_GRID_SAMPLE_COUNT: usize = 16 * 16 * 16; // 4096
/// 16 bytes per sample: three `u32` of packed radiance plus one occlusion word.
pub const LIGHT_GRID_CHANNELS: usize = 16;
/// 4096 * 16 = 65 536 bytes of sample data per grid block.
pub const LIGHT_GRID_BLOCK_BYTES: usize = LIGHT_GRID_SAMPLE_COUNT * LIGHT_GRID_CHANNELS;
/// The radiance exponent is stored biased by this.
pub const LIGHT_GRID_RADIANCE_BIAS: i32 = 10;
/// `kLightGridBinVersion`.
pub const LIGHT_GRID_BIN_VERSION: u32 = 2;
/// A zone may hold at most this many grid blocks.
pub const LIGHT_GRID_MAX_PER_ZONE: usize = 65535;

/// What a shipped `.zonelightbin` actually contains, measured on 354 of the 768
/// named ones (46 of the 400 sampled ids are not in the TOC at all):
///
/// ```text
///   [36-byte asset header]  { u32 0xFA8D90B3, u32 block0_size, u32 block1_size, ... }
///   block 0: DAT1, type 0xFA8D90B3
///       0x27204B67                      Vec3[n] grid-block world positions
///       0x101A2196 Light Bin Header Data 8 bytes, ZERO on all 354
///   block 1: 2 filler bytes, then a SECOND DAT1, also type 0xFA8D90B3
///       0x13F4AF3B Light Grid Sample Data   compressed sample stream
///       0xC72A514C Light Grid Data Offsets  per-block offsets into it
/// ```
///
/// The positions array is 12-byte `Vec3` rows on all 354 files, every component
/// a multiple of 8 m and stepping by 16 m — i.e. the **centres** of the 16 m grid
/// blocks, matching `kLightGridWidth`.
///
/// **The sample stream is compressed and the shipped span-0 asset is short of
/// it.** Declared sample bytes average 6 761 per 16 m block where an
/// uncompressed block is 65 536, and 0 of 338 files store every declared byte —
/// `Light Grid Data Offsets` sits past the end of the stored bytes, so span 0
/// alone cannot even locate a block. 16 of 354 ids also appear in TOC span 7 at
/// roughly 400x the size, which is where the rest lives. Unpacking additionally
/// needs the engine's two-stage neighbour reconstruction
/// (`LightGridUnpacker`), because `derived` samples carry no data of their own.
/// [`decode_light_grid_sample`] is exact for a *runtime* sample and is the last
/// step of that pipeline, not the first.
///
/// `.zonelightbin` DAT1 section tags, with the names the engine's tag list gives.
pub const T_LIGHT_BIN_HEADER_DATA: u32 = 0x101A_2196; // "Light Bin Header Data"
pub const T_LIGHT_GRID_SAMPLE_DATA: u32 = 0x13F4_AF3B; // "Light Grid Sample Data"
pub const T_LIGHT_GRID_DATA_OFFSETS: u32 = 0xC72A_514C; // "Light Grid Data Offsets"
/// Present in the first DAT1 of every shipped `.zonelightbin`; not in the
/// 534-name candidate list.
pub const T_LIGHT_BIN_UNNAMED: u32 = 0x2720_4B67;

/// The six cardinal directions a light-grid sample stores radiance for, in the
pub const LIGHT_GRID_CARDINALS: [[f32; 3]; 6] = [
    [-1.0, 0.0, 0.0],
    [0.0, -1.0, 0.0],
    [0.0, 0.0, -1.0],
    [1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.0, 1.0],
];

pub const LIGHT_GRID_SAMPLE_TYPES: [&str; 4] =
    ["embedded", "derived", "explicit", "explicit-occluder"];

const OCCL_MASK: u32 = 0xFFFF_C03F;
const OCCL_EMBEDDED: u32 = 0x83F8_0000;
const OCCL_UNOCCLUDED: u32 = 0x83F8_003F;
const OCCL_DERIVED: u32 = 0x8008_003F;
const OCCL_DERIVED_ALT: u32 = 0x8408_003F;
const OCCL_DERIVED_ALT2: u32 = 0x8408_403F;

/// One decoded 16-byte light-grid sample: an ambient cube plus an occluder plane.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LightGridSample {
    /// Linear RGB radiance arriving from each of [`LIGHT_GRID_CARDINALS`].
    pub cardinal_radiance: [[f32; 3]; 6],
    /// The sample's occluder plane, in grid-local metres.
    pub occluder_normal: [f32; 3],
    pub occluder_d: f32,
    /// How strongly this sample blends toward the atmosphere's default
    /// environment rather than its own baked radiance, 0..1.
    pub defaultness: f32,
    /// Index into [`LIGHT_GRID_SAMPLE_TYPES`].
    pub sample_type: u8,
    /// The 4-bit stored exponent, unbiased: `radiance *= 2^radiance_log`.
    pub radiance_log: i32,
}

fn u6(v: u32) -> f32 {
    (v & 0x3F) as f32 * (1.0 / 63.0)
}
fn u5(v: u32) -> f32 {
    (v & 0x1F) as f32 * (1.0 / 31.0)
}
fn u4(v: u32) -> f32 {
    (v & 0x0F) as f32 * (1.0 / 15.0)
}

/// `NormalizedYCgCoToRGB` — the engine's exact reconstruction.
fn ycgco_to_rgb(y: f32, cg: f32, co: f32) -> [f32; 3] {
    let g = y + cg * y;
    let mut r = y - cg * y;
    let b = r - co * y;
    r += co * y;
    [r, g, b]
}

/// Unpack one 6.5.5 YCgCo half-word. The chrominance pair is stored
/// `sqrt`-companded, so the decode squares it back.
pub fn unpack_light_grid_radiance(v: u16, lum_scale: f32) -> [f32; 3] {
    let x = v as u32;
    let y = u6(x) * lum_scale;
    let mut cg = u5(x >> 6) * 2.0 - 1.0;
    let mut co = u5(x >> 11) * 2.0 - 1.0;
    cg *= cg.abs();
    co *= co.abs();
    ycgco_to_rgb(y, cg, co)
}

/// Decode one 16-byte light-grid sample.
///
/// Byte layout, straight out of `LightGridEncodeSample` / `LightGridDecodeSample`:
///
/// ```text
///   +0x0  u32 = (neg_x << 16) | pos_x     each half a 6.5.5 YCgCo word
///   +0x4  u32 = (neg_y << 16) | pos_y
///   +0x8  u32 = (neg_z << 16) | pos_z
///   +0xC  u32 occl word:
///           bits 26..31  occluder normal x, 6 bits, v*2/63 - 1
///           bits 20..25  occluder normal y
///           bits 14..19  occluder normal z
///           bits 10..13  radiance exponent, biased by +10
///           bits  6..9   defaultness, /15
///           bits  0..5   occluder plane d, v*4/63 - 2
/// ```
///
/// Three magic occl words replace the plane to mean *embedded* (inside
/// geometry, contributes nothing), *derived* (reconstruct by blending the 26
/// neighbours) and *explicit-unoccluded*.
pub fn decode_light_grid_sample(data: &[u8]) -> Option<LightGridSample> {
    if data.len() < LIGHT_GRID_CHANNELS {
        return None;
    }
    let w: Vec<u16> = (0..6).map(|i| rd_u16(data, i * 2)).collect();
    let occl = rd_u32(data, 12);
    let radiance_log = ((occl >> 10) & 0xF) as i32 - LIGHT_GRID_RADIANCE_BIAS;
    let lum_scale = (radiance_log as f32).exp2();

    // The writer packs (neg, pos) into one u32 with neg in the HIGH half, so
    // little-endian u16 reads give [pos_x, neg_x, pos_y, neg_y, pos_z, neg_z].
    let cardinal_radiance = [
        unpack_light_grid_radiance(w[1], lum_scale), // -X
        unpack_light_grid_radiance(w[3], lum_scale), // -Y
        unpack_light_grid_radiance(w[5], lum_scale), // -Z
        unpack_light_grid_radiance(w[0], lum_scale), // +X
        unpack_light_grid_radiance(w[2], lum_scale), // +Y
        unpack_light_grid_radiance(w[4], lum_scale), // +Z
    ];

    let plane = occl & OCCL_MASK;
    let sample_type = match plane {
        OCCL_EMBEDDED => 0u8,
        OCCL_DERIVED => 1,
        OCCL_UNOCCLUDED | OCCL_DERIVED_ALT | OCCL_DERIVED_ALT2 => 2,
        _ => 3,
    };

    Some(LightGridSample {
        cardinal_radiance,
        occluder_normal: [
            u6(occl >> 26) * 2.0 - 1.0,
            u6(occl >> 20) * 2.0 - 1.0,
            u6(occl >> 14) * 2.0 - 1.0,
        ],
        occluder_d: u6(occl) * 4.0 - 2.0,
        defaultness: u4(occl >> 6),
        sample_type,
        radiance_log,
    })
}

impl LightGridSample {
    /// Ambient-cube evaluation: the irradiance a surface with this world normal
    /// receives — the cheapest useful thing a viewer can do with a baked grid.
    pub fn evaluate(&self, normal: [f32; 3]) -> [f32; 3] {
        let w = [
            (-normal[0]).max(0.0),
            (-normal[1]).max(0.0),
            (-normal[2]).max(0.0),
            normal[0].max(0.0),
            normal[1].max(0.0),
            normal[2].max(0.0),
        ];
        let sum: f32 = w.iter().sum();
        let inv = if sum > 1e-6 { 1.0 / sum } else { 0.0 };
        let mut out = [0.0f32; 3];
        for i in 0..6 {
            for c in 0..3 {
                out[c] += self.cardinal_radiance[i][c] * w[i] * inv;
            }
        }
        out
    }

    /// The flat average of the six cardinals: a single ambient colour for a
    /// whole grid block.
    pub fn average(&self) -> [f32; 3] {
        let mut out = [0.0f32; 3];
        for r in &self.cardinal_radiance {
            for c in 0..3 {
                out[c] += r[c] / 6.0;
            }
        }
        out
    }
}

/// Sample index inside a grid block: the engine walks voxels as
/// `x + width * (y + width * z)`.
pub fn light_grid_sample_index(x: usize, y: usize, z: usize) -> usize {
    x + LIGHT_GRID_WIDTH * (y + LIGHT_GRID_WIDTH * z)
}

// ---------------------------------------------------------------------------
// Per-zone summary
// ---------------------------------------------------------------------------

/// Everything a viewer needs to light one zone.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneLightingReport {
    pub zone_path: String,
    pub lights: Vec<ZoneLight>,
    /// Count per light type, indexed by type id.
    pub type_counts: [u32; 6],
    pub shadow_casters: u32,
    /// Lights that are excluded under at least one lighting condition.
    pub condition_dependent_lights: u32,
    /// `Zone Atmosphere Name`, `Zone Level Light Name`, `Zone Light Bin Asset`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atmosphere: Option<ConditionBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level_light: Option<ConditionBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub light_bin: Option<ConditionBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_probe_assets: Option<EnvProbeAssets>,
    pub env_probes: Vec<EnvProbeRow>,
    /// `Zone Light Grid Positions` hashes, for shared open-world light grids.
    pub light_grid_positions: Vec<u32>,
    pub notes: Vec<String>,
}

pub fn report_zone_lighting(zone: &Zone, zone_path: &str) -> ZoneLightingReport {
    let lights = read_zone_lights(zone);
    let mut type_counts = [0u32; 6];
    let mut shadow_casters = 0u32;
    let mut cond = 0u32;
    for l in &lights {
        if let Some(c) = type_counts.get_mut(l.light_type as usize) {
            *c += 1;
        }
        if l.casts_shadows {
            shadow_casters += 1;
        }
        if l.lighting_conditions & LIGHTING_CONDITION_ALL != LIGHTING_CONDITION_ALL {
            cond += 1;
        }
    }
    let env_probes = read_env_probes(&zone.dat1);
    let mut notes = Vec::new();

    let atmosphere = read_condition_block(&zone.dat1, T_ATMOSPHERE_NAME, 1);
    let level_light = read_condition_block(&zone.dat1, T_LEVEL_LIGHT_NAME, 1);
    let light_bin = read_condition_block(&zone.dat1, T_LIGHT_BIN_ASSET, 1);
    let env_probe_assets = read_env_probe_assets(&zone.dat1);

    if atmosphere.is_none() {
        notes.push("no Zone Atmosphere Name block; the zone inherits its region's".into());
    }
    let light_grid_positions = zone
        .dat1
        .get_section_data(T_LIGHT_GRID_POSITIONS)
        .map(|s| {
            s.chunks_exact(4)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect()
        })
        .unwrap_or_default();

    ZoneLightingReport {
        zone_path: zone_path.to_string(),
        lights,
        type_counts,
        shadow_casters,
        condition_dependent_lights: cond,
        atmosphere,
        level_light,
        light_bin,
        env_probe_assets,
        env_probes,
        light_grid_positions,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_round_trip() {
        assert_eq!(half_to_f32(0x0000), 0.0);
        assert_eq!(half_to_f32(0x3C00), 1.0);
        assert_eq!(half_to_f32(0x4000), 2.0);
        assert_eq!(half_to_f32(0xC000), -2.0);
        assert!((half_to_f32(0x3555) - 0.333).abs() < 0.001);
        // 65504 is the largest finite half.
        assert_eq!(half_to_f32(0x7BFF), 65504.0);
    }

    #[test]
    fn derived_radius_matches_engine() {
        // luminance 0.125 -> radius 1.0
        let c = [0.125 / 0.2126, 0.0, 0.0];
        assert!((derived_radius(c) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cone_consts_saturate_at_the_cone_edges() {
        let mut buf = [0u8; LIGHT_ENTRY_LEN];
        buf[0x5F] = KIND_LIGHT; // high byte of the flags word at +0x5C
        buf[0x7C..0x80].copy_from_slice(&(LIGHT_ENTRY_LEN as u32).to_le_bytes());
        let mut l = ZoneLight::decode(&buf, 0).expect("kind-1 entry decodes");
        l.light_type = LIGHT_TYPE_SPOT;
        l.inner_cone_radians = 0.2;
        l.outer_cone_radians = 0.6;
        let [x, y] = l.cone_falloff_consts().unwrap();
        let at_inner = (0.2f32.cos() * x + y).clamp(0.0, 1.0);
        let at_outer = (0.6f32.cos() * x + y).clamp(0.0, 1.0);
        assert!(at_inner > 0.9, "inner {at_inner}");
        assert!(at_outer < 0.01, "outer {at_outer}");
    }
}
