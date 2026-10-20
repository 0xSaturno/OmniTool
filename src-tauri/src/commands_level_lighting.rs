//! Tauri commands that hand the viewport everything it needs to light a zone.
//!
//! | command                     | answers                                                    |
//! |-----------------------------|------------------------------------------------------------|
//! | `level_zone_lighting`       | the typed light list plus this zone's atmosphere / level-light / light-bin / env-probe references |
//! | `level_atmosphere_lighting` | one `.atmosphere`'s key light, as a ready-to-use sun direction, colour and shadow range |
//! | `level_lighting_conditions` | the five `LightingConditions` items and their serialized hashes |
//!
//! All three are read-only: nothing here writes to the game directory or to a
//! zone. Lighting edits would have to go through [`crate::core::zone`]'s
//! equal-length in-place patch path, which is deliberately not wired up yet.
//!
//! The decode itself, and the evidence behind every offset, lives in
//! [`crate::core::zone_lighting`].

use std::time::Instant;

use log::info;
use serde::{Deserialize, Serialize};

use crate::core::error::ToolkitError;
use crate::core::level::{self, LevelIndex};
use crate::core::zone::Zone;
use crate::core::zone_lighting as zl;

type Result<T> = std::result::Result<T, ToolkitError>;

fn index(game_dir: &str, hashes_path: Option<String>) -> Result<std::sync::Arc<LevelIndex>> {
    level::get_index(game_dir, hashes_path.as_deref())
}

fn load_zone(index: &LevelIndex, zone_path: &str) -> Result<(String, Zone)> {
    let path = crate::core::crc64::normalize_path(zone_path);
    let id = crate::core::crc64::hash_raw(path.as_bytes());
    let raw = index.extract(id)?;
    let zone = Zone::parse(&raw)?;
    Ok((path, zone))
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// One asset reference of a lighting-condition block, with the name resolved
/// from the global list when the zone's own string pool does not carry it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LightingAssetRef {
    pub condition: String,
    pub asset_id: String,
    pub path: String,
    /// `crc32` of the referenced extension, e.g. `.atmosphere`.
    pub suffix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneLightingDto {
    pub zone_path: String,
    /// Every kind-1 scene object, decoded into renderer units.
    pub lights: Vec<zl::ZoneLight>,
    /// Count per light type, indexed by the game's type id.
    pub type_counts: [u32; 6],
    pub shadow_casters: u32,
    /// Lights excluded under at least one lighting condition.
    pub condition_dependent_lights: u32,
    /// `Zone Atmosphere Name`, one row per condition (or a single row when the
    /// block is condition-independent).
    pub atmospheres: Vec<LightingAssetRef>,
    pub level_lights: Vec<LightingAssetRef>,
    /// `Zone LightBin Asset` — the baked light grid, per condition.
    pub light_bins: Vec<LightingAssetRef>,
    /// Env-probe cube maps for the requested condition, plus the overrides.
    pub env_probe_textures: Vec<LightingAssetRef>,
    pub env_probes: Vec<zl::EnvProbeRow>,
    /// How long the runtime texture array is, which is the range an
    /// `assetIndex` may use. `-1` on a probe means it has no cube map.
    pub env_probe_texture_count: usize,
    /// `Zone Light Grid Positions` hashes.
    pub light_grid_positions: usize,
    /// Which condition index the per-condition lists above were resolved for.
    pub condition: usize,
    pub condition_name: String,
    pub notes: Vec<String>,
}

/// The part of an atmosphere a viewer can act on today: the key light.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AtmosphereLightingDto {
    pub path: String,
    pub asset_id: String,
    /// The authored colour, as stored (linear, normalised so one channel is 1.0).
    pub key_light_color: [f32; 3],
    /// The authored intensity, which the runtime forces the colour's luminance to.
    pub key_light_intensity: f32,
    /// `key_light_color` rescaled so its luminance equals `key_light_intensity` —
    /// what the game actually multiplies into the G-buffer.
    pub key_light_linear: [f32; 3],
    pub key_light_azimuth_deg: f32,
    pub key_light_elevation_deg: f32,
    /// The direction sunlight **travels**, from `azimuth`/`elevation`.
    /// `(cos az * -cos el, -sin el, sin az * -cos el)`, Y up.
    pub key_light_direction: [f32; 3],
    /// The same for the visible sun disk, which may be offset for looks.
    pub sun_disk_direction: [f32; 3],
    pub sun_disk_offset_azimuth_deg: f32,
    pub sun_disk_offset_elevation_deg: f32,
    pub shadow_draw_dist: f32,
    pub csm_lod_count: i32,
    pub time_of_day: f32,
    pub sun_diameter: f32,
    pub keylight_material_scale: f32,
    pub caster_range_lower: f32,
    pub caster_range_upper: f32,
    pub far_lod_caching: bool,
    pub far_lod_transition_dist: f32,
    pub far_lod_perf_mode_opts: bool,
    /// Every string in the atmosphere's string section, which is where the sky
    /// box, fog maps, env-probe default and keylight material paths live.
    pub strings: Vec<String>,
    pub header_size: usize,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LightingConditionDto {
    pub index: usize,
    pub name: String,
    /// The DDL item name the zone data hashes, e.g. `kDay`.
    pub item_name: String,
    /// `crc32(item_name)`, the word a condition block stores for validation.
    pub name_hash: String,
    /// `1 << index`, the bit a light's `lightingConditions` mask is tested with.
    pub bit: u8,
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

fn refs_of(
    idx: &LevelIndex,
    block: Option<&zl::ConditionBlock>,
    condition: usize,
    all_conditions: bool,
) -> Vec<LightingAssetRef> {
    let Some(b) = block else { return Vec::new() };
    let rows: Vec<(usize, &Vec<zl::AssetRef>)> = if all_conditions || b.condition_count == 0 {
        b.per_condition.iter().enumerate().collect()
    } else {
        let r = condition.min(b.per_condition.len().saturating_sub(1));
        b.per_condition.get(r).map(|v| vec![(r, v)]).unwrap_or_default()
    };
    let mut out = Vec::new();
    for (i, row) in rows {
        for r in row {
            let path = if r.path.is_empty() {
                idx.name_of(r.asset_id).unwrap_or("").to_string()
            } else {
                r.path.clone()
            };
            out.push(LightingAssetRef {
                condition: if b.condition_count == 0 {
                    "any".to_string()
                } else {
                    zl::LIGHTING_CONDITIONS
                        .get(i)
                        .copied()
                        .unwrap_or("?")
                        .to_string()
                },
                asset_id: format!("{:#018X}", r.asset_id),
                path,
                suffix: zl::suffix_name(r.suffix_hash).to_string(),
            });
        }
    }
    out
}

/// Everything needed to light one zone.
///
/// `condition` selects which `LightingConditions` row the per-condition asset
/// lists are resolved for (0 = day, the game's default); the light list always
/// carries every light with its own condition mask, so a viewer can filter
/// client-side without another round trip.
#[tauri::command]
pub async fn level_zone_lighting(
    game_dir: String,
    zone_path: String,
    condition: Option<usize>,
    all_conditions: Option<bool>,
    hashes_path: Option<String>,
) -> Result<ZoneLightingDto> {
    let start = Instant::now();
    let idx = index(&game_dir, hashes_path)?;
    let (path, zone) = load_zone(&idx, &zone_path)?;
    let cond = condition.unwrap_or(0).min(zl::LIGHTING_CONDITION_COUNT - 1);
    let all = all_conditions.unwrap_or(false);

    let report = zl::report_zone_lighting(&zone, &path);
    let probe_assets = zl::read_env_probe_assets(&zone.dat1);

    let mut env_probe_textures = Vec::new();
    let mut env_probe_texture_count = 0usize;
    if let Some(pa) = &probe_assets {
        env_probe_texture_count = pa.total_texture_count;
        for r in &pa.overrides {
            env_probe_textures.push(LightingAssetRef {
                condition: "override".to_string(),
                asset_id: format!("{:#018X}", r.asset_id),
                path: if r.path.is_empty() {
                    idx.name_of(r.asset_id).unwrap_or("").to_string()
                } else {
                    r.path.clone()
                },
                suffix: zl::suffix_name(r.suffix_hash).to_string(),
            });
        }
        env_probe_textures.extend(refs_of(&idx, Some(&pa.per_condition), cond, all));
    }

    let mut notes = report.notes.clone();
    if report.lights.is_empty() {
        notes.push(
            "this zone holds no lights; in shipped content they live in the sibling *_lgt / *_art zone"
                .into(),
        );
    }

    let dto = ZoneLightingDto {
        zone_path: path,
        atmospheres: refs_of(&idx, report.atmosphere.as_ref(), cond, all),
        level_lights: refs_of(&idx, report.level_light.as_ref(), cond, all),
        light_bins: refs_of(&idx, report.light_bin.as_ref(), cond, all),
        env_probe_textures,
        env_probe_texture_count,
        env_probes: report.env_probes,
        light_grid_positions: report.light_grid_positions.len(),
        lights: report.lights,
        type_counts: report.type_counts,
        shadow_casters: report.shadow_casters,
        condition_dependent_lights: report.condition_dependent_lights,
        condition: cond,
        condition_name: zl::LIGHTING_CONDITIONS[cond].to_string(),
        notes,
    };
    info!(
        "level_zone_lighting: {} lights, {} probes in {:?}",
        dto.lights.len(),
        dto.env_probes.len(),
        start.elapsed()
    );
    Ok(dto)
}

/// Read one `.atmosphere` and return its key light in renderer units.
///
/// Accepts either an asset path or a `0x…` asset id.
#[tauri::command]
pub async fn level_atmosphere_lighting(
    game_dir: String,
    atmosphere: String,
    hashes_path: Option<String>,
) -> Result<AtmosphereLightingDto> {
    let idx = index(&game_dir, hashes_path)?;
    let t = atmosphere.trim();
    let id = if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16)
            .map_err(|e| ToolkitError::Parse(format!("invalid asset id {atmosphere:?}: {e}")))?
    } else {
        crate::core::crc64::hash_raw(crate::core::crc64::normalize_path(t).as_bytes())
    };
    let raw = idx.extract(id)?;
    if raw.len() <= ATMOSPHERE_WRAPPER_LEN {
        return Err(ToolkitError::Parse(format!(
            "atmosphere {atmosphere:?} is only {} bytes",
            raw.len()
        )));
    }
    let dat1 = crate::core::dat1::Dat1::parse(&raw[ATMOSPHERE_WRAPPER_LEN..])?;
    let h = dat1
        .get_section_data(TAG_ATMOSPHERE_HEADER)
        .ok_or_else(|| ToolkitError::SectionNotFound(TAG_ATMOSPHERE_HEADER))?;
    if h.len() < KEYLIGHT_END {
        return Err(ToolkitError::Parse(format!(
            "atmosphere header is {} bytes, need at least {KEYLIGHT_END}",
            h.len()
        )));
    }
    let f = |o: usize| f32::from_bits(u32::from_le_bytes([h[o], h[o + 1], h[o + 2], h[o + 3]]));
    let i32at = |o: usize| i32::from_le_bytes([h[o], h[o + 1], h[o + 2], h[o + 3]]);

    let color = [f(112), f(116), f(120)];
    let intensity = f(124);
    let az = f(128);
    let el = f(132);
    let sun_az = az + f(136);
    let sun_el = el + f(140);

    let lum = zl::luminance(color);
    let key_light_linear = if lum > 0.0 {
        let k = intensity / lum;
        [color[0] * k, color[1] * k, color[2] * k]
    } else {
        [0.0; 3]
    };

    let strings: Vec<String> = dat1
        .get_section_data(TAG_ATMOSPHERE_STRINGS)
        .map(|s| {
            s.split(|b| *b == 0)
                .filter(|c| !c.is_empty())
                .map(|c| String::from_utf8_lossy(c).replace('\\', "/"))
                .collect()
        })
        .unwrap_or_default();

    let mut notes = Vec::new();
    notes.push(
        "colour is stored normalised; the engine forces its luminance to keyLightIntensity, which keyLightLinear already does"
            .to_string(),
    );
    if !(-90.0..=90.0).contains(&el) {
        notes.push(format!("elevation {el} is outside [-90, 90]"));
    }

    Ok(AtmosphereLightingDto {
        path: idx.name_of(id).unwrap_or(t).to_string(),
        asset_id: format!("{id:#018X}"),
        key_light_color: color,
        key_light_intensity: intensity,
        key_light_linear,
        key_light_azimuth_deg: az,
        key_light_elevation_deg: el,
        key_light_direction: sun_direction(az, el),
        sun_disk_direction: sun_direction(sun_az, sun_el),
        sun_disk_offset_azimuth_deg: f(136),
        sun_disk_offset_elevation_deg: f(140),
        shadow_draw_dist: f(144),
        csm_lod_count: i32at(148),
        time_of_day: f(152),
        sun_diameter: f(156),
        keylight_material_scale: f(160),
        caster_range_lower: f(164),
        caster_range_upper: f(168),
        far_lod_caching: i32at(172) != 0,
        far_lod_transition_dist: f(176),
        far_lod_perf_mode_opts: i32at(180) != 0,
        strings,
        header_size: h.len(),
        notes,
    })
}

/// The five lighting conditions, with the hashes a zone block stores.
#[tauri::command]
pub async fn level_lighting_conditions() -> Result<Vec<LightingConditionDto>> {
    const ITEMS: [&str; 5] = ["kDay", "kOvercast", "kSunset", "kNight", "kBlackout"];
    Ok((0..zl::LIGHTING_CONDITION_COUNT)
        .map(|i| LightingConditionDto {
            index: i,
            name: zl::LIGHTING_CONDITIONS[i].to_string(),
            item_name: ITEMS[i].to_string(),
            name_hash: format!("{:#010X}", crate::core::crc32::hash(ITEMS[i])),
            bit: 1 << i,
        })
        .collect())
}

/// The 36-byte asset wrapper an `.atmosphere` carries, like every other asset.
const ATMOSPHERE_WRAPPER_LEN: usize = 36;
/// The atmosphere's cooked settings block. 1 800 bytes on all 95 shipped
/// atmospheres the PC build actually stores.
const TAG_ATMOSPHERE_HEADER: u32 = 0x02F0_6D4E;
const TAG_ATMOSPHERE_STRINGS: u32 = 0x72F2_8658;
/// The key-light block occupies +112..+184 of that block.
const KEYLIGHT_END: usize = 184;

/// The game's `sSunAzimuthElevationToDirection`: the direction the key light
/// travels, Y up. Elevation is negated on both axes, so a positive elevation
/// points the light downward.
fn sun_direction(azimuth_deg: f32, elevation_deg: f32) -> [f32; 3] {
    let az = azimuth_deg.to_radians();
    let el = elevation_deg.to_radians();
    [az.cos() * -el.cos(), -el.sin(), az.sin() * -el.cos()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sun_points_down_when_high_in_the_sky() {
        let d = sun_direction(135.0, 55.0);
        assert!(d[1] < -0.8, "{d:?}");
        let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        assert!((l - 1.0).abs() < 1e-5, "not unit: {l}");
    }

    #[test]
    fn sun_is_horizontal_at_zero_elevation() {
        let d = sun_direction(0.0, 0.0);
        assert!((d[0] + 1.0).abs() < 1e-5, "{d:?}");
        assert!(d[1].abs() < 1e-6);
    }
}
