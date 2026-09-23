//! `.materialgraph` template tables — the slot list a `.material` overrides.
//!
//! A built material template carries four tables the editor cares about:
//!
//! | tag | contents |
//! |---|---|
//! | `0x1CAFE804` | samplers — 16 bytes: `{u32 path_off, u16 user_exposed, u16 index, u32 hash, u32 type}` |
//! | `0x45C4F4C0` | constants — 8 bytes: `{u16 buffer_offset, u16 buffer_size, u32 hash}` |
//! | `0xA59F667B` | constant content — the shader constant buffer holding the defaults |
//! | `0xBC93FB5E` | variations — 8 bytes: `{u32 value, u32 hash}`, the graph's checkbox switches |
//!
//! The template's `buffer_offset` is a **register offset** (`register * 16 +
//! component * 4`), not a packed index, so the constant table doubles as the
//! shader's constant-buffer layout: constants are emitted widest-first, so the
//! `float2`s occupy the low registers and the `float`s follow. A *material*
//! instance stores the same `{offset, size, hash}` shape but its offsets are
//! packed, which is why the two tables are parsed by different modules.
//!
//! Only the hashes ship. The builder names a slot
//! `"<node variable name>_<port internal name>"` with spaces turned into
//! underscores and then CRC32s it, and neither string survives into the built
//! file, so a name can only be recovered by hashing a candidate.

use serde::{Deserialize, Serialize};

use crate::core::dat1::Dat1;
use crate::core::error::{Result, ToolkitError};
use crate::core::material::MaterialFile;

pub const TAG_TEMPLATE_SAMPLERS: u32 = 0x1CAF_E804;
pub const TAG_TEMPLATE_CONSTANTS: u32 = 0x45C4_F4C0;
pub const TAG_TEMPLATE_CONSTANTS_CONTENT: u32 = 0xA59F_667B;
pub const TAG_TEMPLATE_VARIATIONS: u32 = 0xBC93_FB5E;

const SAMPLER_ENTRY_SIZE: usize = 16;
const CONSTANT_ENTRY_SIZE: usize = 8;
const VARIATION_ENTRY_SIZE: usize = 8;
/// The variations table is reserved at a fixed 32 entries; the bytes after it
/// belong to other template fields, starting with the real entry count.
const VARIATION_TABLE_BYTES: usize = 0x100;

fn rd_u32(b: &[u8], off: usize) -> Result<u32> {
    b.get(off..off + 4)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
        .ok_or_else(|| ToolkitError::Parse(format!("truncated read at {off:#X}")))
}

fn rd_u16(b: &[u8], off: usize) -> Result<u16> {
    b.get(off..off + 2)
        .map(|s| u16::from_le_bytes(s.try_into().unwrap()))
        .ok_or_else(|| ToolkitError::Parse(format!("truncated read at {off:#X}")))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateSampler {
    pub name_hash: u32,
    /// `HWRegister - TEX_UNIT_BASE`: the texture unit the shader samples.
    pub slot_index: u16,
    /// Non-zero when the graph exposes this sampler in the material editor.
    pub user_exposed: u16,
    pub type_hash: u32,
    pub default_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateConstant {
    pub name_hash: u32,
    /// Byte offset into the shader constant buffer (`register * 16 + comp * 4`).
    pub buffer_offset: u16,
    /// Byte size — 4 / 8 / 12 / 16 for `float` … `float4`.
    pub buffer_size: u16,
    pub default_values: Vec<f32>,
}

/// A graph checkbox baked into the template: a static switch whose default the
/// material may flip. Same `{value, hash}` shape as the material's own
/// variation table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateVariation {
    pub name_hash: u32,
    pub value: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MaterialTemplate {
    pub samplers: Vec<TemplateSampler>,
    pub constants: Vec<TemplateConstant>,
    pub variations: Vec<TemplateVariation>,
}

impl MaterialTemplate {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let graph = MaterialFile::parse(data)?;
        Self::from_dat1(&graph.dat1)
    }

    pub fn from_dat1(dat1: &Dat1) -> Result<Self> {
        let mut samplers = Vec::new();
        if let Some(sec) = dat1.get_section_data(TAG_TEMPLATE_SAMPLERS) {
            for base in (0..sec.len()).step_by(SAMPLER_ENTRY_SIZE) {
                if base + SAMPLER_ENTRY_SIZE > sec.len() {
                    break;
                }
                let string_off = rd_u32(sec, base)?;
                samplers.push(TemplateSampler {
                    user_exposed: rd_u16(sec, base + 4)?,
                    slot_index: rd_u16(sec, base + 6)?,
                    name_hash: rd_u32(sec, base + 8)?,
                    type_hash: rd_u32(sec, base + 12)?,
                    default_path: dat1.get_string(string_off).unwrap_or_default(),
                });
            }
        }

        let content = dat1.get_section_data(TAG_TEMPLATE_CONSTANTS_CONTENT).unwrap_or(&[]);
        let mut constants = Vec::new();
        if let Some(sec) = dat1.get_section_data(TAG_TEMPLATE_CONSTANTS) {
            for base in (0..sec.len()).step_by(CONSTANT_ENTRY_SIZE) {
                if base + CONSTANT_ENTRY_SIZE > sec.len() {
                    break;
                }
                let value_off = rd_u16(sec, base)?;
                let byte_len = rd_u16(sec, base + 2)?;
                let name_hash = rd_u32(sec, base + 4)?;
                let mut default_values = Vec::with_capacity(byte_len as usize / 4);
                for k in 0..byte_len as usize / 4 {
                    default_values.push(f32::from_bits(
                        rd_u32(content, value_off as usize + k * 4).unwrap_or(0),
                    ));
                }
                constants.push(TemplateConstant {
                    name_hash,
                    buffer_offset: value_off,
                    buffer_size: byte_len,
                    default_values,
                });
            }
        }

        let mut variations = Vec::new();
        if let Some(sec) = dat1.get_section_data(TAG_TEMPLATE_VARIATIONS) {
            // The section reserves `VARIATION_TABLE_BYTES` for the table and
            // then continues with unrelated template fields, the first of which
            // is the real entry count. Trust that count when it is present and
            // sane, and otherwise stop at the zero padding.
            let declared = rd_u32(sec, VARIATION_TABLE_BYTES).ok().map(|n| n as usize);
            let count = match declared {
                Some(n) if n <= VARIATION_TABLE_BYTES / VARIATION_ENTRY_SIZE => n,
                _ => sec.len().min(VARIATION_TABLE_BYTES) / VARIATION_ENTRY_SIZE,
            };
            for i in 0..count {
                let base = i * VARIATION_ENTRY_SIZE;
                if base + VARIATION_ENTRY_SIZE > sec.len() {
                    break;
                }
                let name_hash = rd_u32(sec, base + 4)?;
                if name_hash == 0 {
                    break;
                }
                variations.push(TemplateVariation {
                    name_hash,
                    value: rd_u32(sec, base)?,
                });
            }
        }

        Ok(Self { samplers, constants, variations })
    }

    /// The sampler bound to a given texture unit, if the template declares one.
    pub fn sampler_at(&self, unit: u16) -> Option<&TemplateSampler> {
        self.samplers.iter().find(|s| s.slot_index == unit)
    }

    pub fn sampler_by_hash(&self, hash: u32) -> Option<&TemplateSampler> {
        self.samplers.iter().find(|s| s.name_hash == hash)
    }

    pub fn constant_by_hash(&self, hash: u32) -> Option<&TemplateConstant> {
        self.constants.iter().find(|c| c.name_hash == hash)
    }
}

// ---------------------------------------------------------------------------
// gbl_rgb_blend — the arithmetic, read off the shipped pixel shader
// ---------------------------------------------------------------------------

/// The `.materialgraph` sections that carry the compiled programs. `shader_dump`
/// extracts them; see that binary for the container layout.
pub const TAG_SHADER_GPU_PROGRAM_DATA: u32 = 0xBBFC_8900;
pub const TAG_SHADER_PROGRAM_INDEX: u32 = 0x8C04_9CCA;

/// `HWRegister - slot_index`: the first texture unit a material's samplers use.
/// A disassembly's `t5` is therefore sampler slot 0, and `cb2[r].c` is the
/// template constant whose `buffer_offset` is `r * 16 + c * 4`.
pub const TEX_UNIT_BASE: u16 = 5;

/// One colour layer of `gbl_rgb_blend`, with every constant the shader reads for
/// it. Hashes are CRC32s of slot names, so they are stable across the whole
/// template family even though the constant-buffer register each one lands in is
/// not — always resolve by hash, never by offset.
///
/// **The blend, exactly as the G-buffer pixel shader computes it** (program slot
/// 4 of `gbl_rgb_blend.materialgraph`, the pixel shader with four render
/// targets; slot 5 and slot 14 agree):
///
/// ```text
/// mask   = tex2D(splat_mask, TEXCOORD0 * SPLAT_MASK_UV_TILING).rgb   // raw, mask is UNORM
/// rgb    = tint(base)                                               // see tint() below
/// for L in [RED, GREEN, BLUE]:                                      // this order
///     m    = lerp(mask[L.channel], 1 - mask[L.channel], ceil(L.invert))
///     lo   = saturate(height - L.blend.y)                           // height: see below
///     span = height - lo
///     t    = saturate((L.blend.x * m - lo) / span)
///     rgb  = lerp(rgb, tint(L), t * t * (3 - 2 * t))
/// ```
///
/// `height` is **not** the shared `_h` slot — that sampler is never read by this
/// pass. It is `sqrt(slice2.g)` of the colour layer currently on top, i.e. the
/// base's to begin with. `span` is zero when `height` is zero, so clamp it
/// (`max(span, 1e-5)`) or the divide produces `NaN`.
///
/// `tint()` is a **HSV** round trip, despite the slot names saying HSL — the
/// third axis is `V = max(r,g,b)`, not lightness:
///
/// ```text
/// hsv = rgb_to_hsv(colour)             // H = hue6 form, S = d / max, V = max
/// h   = frac(hsv.x + HSL_HUE - 0.5)    // so 0.5 is the neutral hue
/// s   = 2 * HSL_SATURATION * hsv.y     // NOT clamped here
/// v   = 2 * HSL_LUMINANCE * hsv.z      // NOT clamped here
/// out = saturate(v * mix(vec3(1), hue_ramp(h), s))
/// ```
///
/// with `hue_ramp(h) = saturate(abs(mod(h * 6 + vec3(0, 4, 2), 6) - 3) - 1)`.
/// All three constants default to 0.5, which is the identity. `s` and `v` are
/// deliberately left unclamped before the `mix`/multiply — values above 1
/// extrapolate past the ramp and only the final `saturate` clamps, so clamping
/// them early changes the result.
///
/// The colour textures are `*_UNORM_SRGB`, so the sampler hands the shader
/// **linear** RGB and the whole tint runs in linear space.
///
/// Each colour slot is a **four-slice `Texture2DArray`**, not a flat colour map:
/// slice 0 albedo, slice 1 gloss/specular, slice 2 misc (`g` height, `b` to the
/// G-buffer), slice 3 tangent normal `xy`. Only slice 0 feeds the blend above.
#[derive(Debug, Clone, Copy)]
pub struct BlendLayer {
    /// Splat-mask channel that drives this layer: 0 R, 1 G, 2 B. The base layer
    /// has no channel — it is what the other three are blended over.
    pub mask_channel: Option<u8>,
    /// Sampler slot holding this layer's colour array.
    pub color_sampler: u32,
    /// `float2` UV tiling, applied to **TEXCOORD1**.
    pub uv_tiling: u32,
    /// The HSV triple: hue, saturation, value. Named `..._HSL_...` in the graph.
    pub hsl: [u32; 3],
    /// `float` normal-map intensity; scales the decoded slice-3 `xy` before it is
    /// halved and added onto the shared normal.
    pub normal_scale: u32,
    /// `float2` gloss remap `(range, position)`; `1.0, 0.5` is the identity.
    pub gloss_range_position: u32,
    /// `float` switch, `>= 0.5` meaning the gloss map's RGB is a specular colour
    /// and its alpha the gloss; below that, gloss is `sqrt(r)` and the specular
    /// colour is `g` broadcast.
    pub gloss_use_color: u32,
    /// `float2` `(mask gain, height softness)`; only the three splat layers have
    /// one. Default `1.0, 0.33`.
    pub blend: Option<u32>,
    /// `float`, `ceil()`ed to a flag: flips this layer's mask channel.
    pub splat_mask_invert: Option<u32>,
}

/// The shared slots of `gbl_rgb_blend`.
///
/// `SPLAT_MASK_UV_TILING` drives the mask **and** the shared normal, both of
/// which sample **TEXCOORD0** — the colour layers sample TEXCOORD1, which is a
/// different vertex UV set.
pub const SPLAT_MASK_SAMPLER: u32 = 0x205D_ECC3;
pub const SHARED_NORMAL_SAMPLER: u32 = 0x61D0_637F;
/// Declared, and never read by any compiled program of this graph.
pub const SHARED_HEIGHT_SAMPLER: u32 = 0x3C65_1BCB;
pub const SPLAT_MASK_UV_TILING: u32 = 0x28BB_963E;

/// `gbl_rgb_blend`'s four colour layers, in the order the shader composites them:
/// base first, then red, green, blue.
pub const GBL_RGB_BLEND_LAYERS: [BlendLayer; 4] = [
    BlendLayer {
        mask_channel: None,
        color_sampler: 0x9E34_A894,
        uv_tiling: 0x8D25_66DE,
        hsl: [0x886E_717D, 0x6A81_9148, 0x92F0_4E42],
        normal_scale: 0x2356_CD0C,
        gloss_range_position: 0x2842_7330,
        gloss_use_color: 0xD440_4077,
        blend: None,
        splat_mask_invert: None,
    },
    BlendLayer {
        mask_channel: Some(0),
        color_sampler: 0x54C8_3B26,
        uv_tiling: 0x1D93_CDB7,
        hsl: [0x18D8_DA14, 0x25B3_6543, 0x7FF6_17E1],
        normal_scale: 0x1BD0_45A3,
        gloss_range_position: 0xC544_2A93,
        gloss_use_color: 0x6BB5_844F,
        blend: Some(0xFAE5_46A6),
        splat_mask_invert: Some(0xE2F0_4E17),
    },
    BlendLayer {
        mask_channel: Some(1),
        color_sampler: 0xE4CF_0EAD,
        uv_tiling: 0x62D7_112C,
        hsl: [0x679C_068F, 0x33AF_D592, 0x0F89_833E],
        normal_scale: 0xBBAB_D142,
        gloss_range_position: 0xB53B_BE4C,
        gloss_use_color: 0xE571_B1EB,
        blend: Some(0x3124_B6DD),
        splat_mask_invert: Some(0x9E7F_E3F1),
    },
    BlendLayer {
        mask_channel: Some(2),
        color_sampler: 0x013D_D88D,
        uv_tiling: 0x4BAF_5B9E,
        hsl: [0x4EE4_4C3D, 0x3E1E_E9AB, 0xD3DF_84B4],
        normal_scale: 0xD1C7_B633,
        gloss_range_position: 0x696D_B9C6,
        gloss_use_color: 0x1F45_075A,
        blend: Some(0xDB96_0F93),
        splat_mask_invert: Some(0x7A50_C706),
    },
];

impl BlendLayer {
    /// The layer whose colour sampler is `hash`, if `gbl_rgb_blend` declares one.
    pub fn by_color_sampler(hash: u32) -> Option<&'static BlendLayer> {
        GBL_RGB_BLEND_LAYERS.iter().find(|l| l.color_sampler == hash)
    }
}

#[cfg(test)]
mod blend_tests {
    use super::*;

    /// Every hash in the table is distinct, and the four layers cover the R, G
    /// and B mask channels exactly once each over a single base.
    #[test]
    fn layer_table_is_consistent() {
        let mut seen = std::collections::BTreeSet::new();
        for l in &GBL_RGB_BLEND_LAYERS {
            for h in [l.color_sampler, l.uv_tiling, l.normal_scale, l.gloss_range_position, l.gloss_use_color] {
                assert!(seen.insert(h), "{h:08X} appears twice");
            }
            for h in l.hsl {
                assert!(seen.insert(h), "{h:08X} appears twice");
            }
            for h in [l.blend, l.splat_mask_invert].into_iter().flatten() {
                assert!(seen.insert(h), "{h:08X} appears twice");
            }
        }
        let channels: Vec<Option<u8>> = GBL_RGB_BLEND_LAYERS.iter().map(|l| l.mask_channel).collect();
        assert_eq!(channels, vec![None, Some(0), Some(1), Some(2)]);
        // The shared slots are not layer slots.
        for h in [SPLAT_MASK_SAMPLER, SHARED_NORMAL_SAMPLER, SHARED_HEIGHT_SAMPLER, SPLAT_MASK_UV_TILING] {
            assert!(!seen.contains(&h), "{h:08X} is both shared and per-layer");
        }
    }

    /// The name CRC32s that the shader's own use of each constant has to agree
    /// with: the layer the shader drives from mask channel G is the one whose
    /// three tint constants hash to `Green_HSL_*`.
    #[test]
    fn hsl_names_match_mask_channels() {
        for (name, layer) in [
            ("Base", &GBL_RGB_BLEND_LAYERS[0]),
            ("Red", &GBL_RGB_BLEND_LAYERS[1]),
            ("Green", &GBL_RGB_BLEND_LAYERS[2]),
            ("Blue", &GBL_RGB_BLEND_LAYERS[3]),
        ] {
            for (axis, hash) in ["Hue", "Saturation", "Luminance"].iter().zip(layer.hsl) {
                let slot = format!("{name}_HSL_{axis}");
                assert_eq!(crate::core::crc32::hash(&slot), hash, "{slot}");
            }
            if let Some(inv) = layer.splat_mask_invert {
                let slot = format!("{name}_Splat_Mask_Invert");
                assert_eq!(crate::core::crc32::hash(&slot), inv, "{slot}");
            }
        }
    }
}
