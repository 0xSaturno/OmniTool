//! `.material` asset format

use serde::{Deserialize, Serialize};

use crate::core::dat1::{Dat1, DAT1_MAGIC};
use crate::core::error::{Result, ToolkitError};

pub const TAG_MATERIAL_HEADER: u32 = 0xE127_5683;
pub const TAG_MATERIAL_SERIALIZED: u32 = 0xF526_0180;
pub const TAG_MATERIAL_FUR: u32 = 0xD9B1_2454;
pub const TAG_MATERIAL_WATER: u32 = 0x958F_7B33;
/// Present when the material carries its own compiled copy of the template (shader variations).
pub const TAG_MATERIAL_VARIATION: u32 = 0x3E45_AA13;

/// Size of the outer asset wrapper that precedes the DAT1 payload.
pub const WRAPPER_SIZE: usize = 36;

const HEADER_SIZE: usize = 40;
const SERIALIZED_HEADER_SIZE: usize = 0x28;
const CONST_ENTRY_SIZE: usize = 8;
const SAMPLER_ENTRY_SIZE: usize = 8;

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

fn align16(n: usize) -> usize {
    (n + 15) & !15
}

fn cstring_at(b: &[u8], off: usize) -> String {
    let end = b[off.min(b.len())..]
        .iter()
        .position(|&c| c == 0)
        .map(|p| off + p)
        .unwrap_or(b.len());
    String::from_utf8_lossy(&b[off.min(b.len())..end]).into_owned()
}

pub struct MaterialFile {
    pub wrapper: Option<Vec<u8>>,
    pub dat1: Dat1,
}

impl MaterialFile {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 4 {
            return Err(ToolkitError::Parse("material file too small".into()));
        }
        if u32::from_le_bytes(data[0..4].try_into().unwrap()) == DAT1_MAGIC {
            return Ok(Self {
                wrapper: None,
                dat1: Dat1::parse(data)?,
            });
        }
        if data.len() < WRAPPER_SIZE + 4 {
            return Err(ToolkitError::Parse(
                "material file too small for wrapper".into(),
            ));
        }
        let inner = &data[WRAPPER_SIZE..];
        let magic = u32::from_le_bytes(inner[0..4].try_into().unwrap());
        if magic != DAT1_MAGIC {
            return Err(ToolkitError::InvalidMagic {
                expected: DAT1_MAGIC,
                got: magic,
            });
        }
        Ok(Self {
            wrapper: Some(data[..WRAPPER_SIZE].to_vec()),
            dat1: Dat1::parse(inner)?,
        })
    }

    /// The `.materialgraph` this material instances: the string the header's template pointer
    /// fixup targets. Fur and water materials point it at an empty string.
    pub fn template_path(&self) -> Option<String> {
        let header = self.dat1.sections.iter().find(|s| s.tag == TAG_MATERIAL_HEADER)?.offset;
        self.dat1
            .fixup_pairs()
            .into_iter()
            .find(|&(src, _)| src == header)
            .and_then(|(_, dst)| self.dat1.get_string(dst))
            .filter(|s| !s.is_empty())
    }

    /// Whether the material carries its own compiled template (sampler/constant tables and
    /// shaders). The engine then ignores the `.materialgraph` for slots and shaders.
    pub fn has_embedded_template(&self) -> bool {
        self.dat1.get_section_data(TAG_MATERIAL_VARIATION).is_some()
    }

    pub fn save(&mut self) -> Vec<u8> {
        let body = self.dat1.save();
        match &self.wrapper {
            None => body,
            Some(w) => {
                let mut out = w.clone();
                out[4..8].copy_from_slice(&(body.len() as u32).to_le_bytes());
                out.extend_from_slice(&body);
                out
            }
        }
    }
}

/// Material header flag bits (`m_Flags`). Bits 25-31 are runtime-only and clear on disk.
pub mod flags {
    pub const ACCURATE_ALPHA_VELOCITY: u32 = 1 << 1;
    pub const FUR: u32 = 1 << 14;
    pub const WATER: u32 = 1 << 23;
    pub const RUNTIME_MASK: u32 = !0 << 25;

    pub const NAMES: [&str; 25] = [
        "DoubleSided", "AccurateAlphaVelocity", "SkipShadowCast", "ShadowCastOnly",
        "CastOpaqueShadow", "SkipLightCapture", "SkipEmbeddedTest", "AdditiveBlend", "AlphaBlend",
        "ModulateBlend", "HybridBlend", "AlphaLit", "SSRDisabled", "UseAoOnDecals", "Fur",
        "AllowTriangleSorting", "OverlapColorOnly", "ForceOpaqueLoDs", "AlphaDepthPass",
        "ImpostorHQModel", "OverlapNormalOnly", "DisableLensFlareOcclusion",
        "AlphaTemporalAAResponsive", "Water", "SSRFidelityOnly",
    ];
}

/// Fixed 40-byte Material Header section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialHeaderSection {
    /// Template path pointer: 0 on disk, filled in through the DAT1 fixup table.
    pub template_ptr: u64,
    pub flags: u32,
    /// Physics A/V material; 0 means unset and reads as `kNone`.
    pub av_material_hash: u32,
    /// Never 0 in shipped files: without an audio material the builder stores the A/V one (or `kNone`).
    pub audio_material_hash: u32,
    pub alpha: f32,
    pub alpha_test: f32,
    /// Material LOD switch distance; 0 uses the default.
    pub lod_dist: f32,
    /// Stored with a +128 bias, so 128 is neutral.
    pub voxelization_order_bias: u8,
    /// Zero apart from a few shipped materials that set bytes 0x21/0x23; kept verbatim.
    pub pad: [u8; 7],
}

impl MaterialHeaderSection {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < HEADER_SIZE {
            return Err(ToolkitError::Parse(format!(
                "material header section is {} bytes, expected {HEADER_SIZE}",
                data.len()
            )));
        }
        Ok(Self {
            template_ptr: u64::from_le_bytes(data[0..8].try_into().unwrap()),
            flags: rd_u32(data, 0x08)?,
            av_material_hash: rd_u32(data, 0x0C)?,
            audio_material_hash: rd_u32(data, 0x10)?,
            alpha: f32::from_bits(rd_u32(data, 0x14)?),
            alpha_test: f32::from_bits(rd_u32(data, 0x18)?),
            lod_dist: f32::from_bits(rd_u32(data, 0x1C)?),
            voxelization_order_bias: data[0x20],
            pad: data[0x21..0x28].try_into().unwrap(),
        })
    }

    pub fn build(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_SIZE);
        out.extend_from_slice(&self.template_ptr.to_le_bytes());
        for w in [
            self.flags,
            self.av_material_hash,
            self.audio_material_hash,
            self.alpha.to_bits(),
            self.alpha_test.to_bits(),
            self.lod_dist.to_bits(),
        ] {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out.push(self.voxelization_order_bias);
        out.extend_from_slice(&self.pad);
        out
    }
}

fn rd_words(data: &[u8], n: usize, what: &str) -> Result<Vec<u32>> {
    if data.len() < n * 4 {
        return Err(ToolkitError::Parse(format!("{what} is {} bytes, expected {}", data.len(), n * 4)));
    }
    Ok(data[..n * 4].chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect())
}

fn words_to_bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// String offset value meaning "no path" in the fur and water info blocks.
pub const NO_PATH: u32 = u32::MAX;

/// Material Fur Info (52 bytes), present exactly when the Fur flag is set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialFurInfo {
    pub layer_count: u32,
    pub lod_reduction: f32,
    pub length: f32,
    pub density: f32,
    pub offset_scale: f32,
    pub gloss_scale: f32,
    pub specular_scale: f32,
    pub transmittance_scale: f32,
    pub wetness: f32,
    /// Base, normal, gloss and control map paths as DAT1-absolute string offsets, or [`NO_PATH`].
    pub map_offsets: [u32; 4],
}

impl MaterialFurInfo {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let w = rd_words(data, 13, "fur info")?;
        let f = |i: usize| f32::from_bits(w[i]);
        Ok(Self {
            layer_count: w[0],
            lod_reduction: f(1),
            length: f(2),
            density: f(3),
            offset_scale: f(4),
            gloss_scale: f(5),
            specular_scale: f(6),
            transmittance_scale: f(7),
            wetness: f(8),
            map_offsets: [w[9], w[10], w[11], w[12]],
        })
    }

    pub fn build(&self) -> Vec<u8> {
        let mut w = vec![self.layer_count];
        w.extend(
            [
                self.lod_reduction,
                self.length,
                self.density,
                self.offset_scale,
                self.gloss_scale,
                self.specular_scale,
                self.transmittance_scale,
                self.wetness,
            ]
            .map(f32::to_bits),
        );
        w.extend(self.map_offsets);
        words_to_bytes(&w)
    }
}

/// Material Water Info (76 bytes), present exactly when the Water flag is set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialWaterInfo {
    pub water_color: [f32; 3],
    pub water_scale: f32,
    pub foam_color: [f32; 3],
    pub foam_amp: f32,
    pub foam_power: f32,
    pub water_depth: f32,
    pub darkening: f32,
    pub flow_rate: f32,
    pub flow_phase: f32,
    pub flow_noise: f32,
    pub caustics_intensity: f32,
    pub caustics_depth_bias: f32,
    pub water_gloss: f32,
    pub water_color_scale: f32,
    /// Flow map path as a DAT1-absolute string offset, or [`NO_PATH`].
    pub flow_map_offset: u32,
}

impl MaterialWaterInfo {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let w = rd_words(data, 19, "water info")?;
        let f = |i: usize| f32::from_bits(w[i]);
        Ok(Self {
            water_color: [f(0), f(1), f(2)],
            water_scale: f(3),
            foam_color: [f(4), f(5), f(6)],
            foam_amp: f(7),
            foam_power: f(8),
            water_depth: f(9),
            darkening: f(10),
            flow_rate: f(11),
            flow_phase: f(12),
            flow_noise: f(13),
            caustics_intensity: f(14),
            caustics_depth_bias: f(15),
            water_gloss: f(16),
            water_color_scale: f(17),
            flow_map_offset: w[18],
        })
    }

    pub fn build(&self) -> Vec<u8> {
        let c = self.water_color;
        let fc = self.foam_color;
        let floats = [
            c[0], c[1], c[2], self.water_scale, fc[0], fc[1], fc[2], self.foam_amp,
            self.foam_power, self.water_depth, self.darkening, self.flow_rate, self.flow_phase,
            self.flow_noise, self.caustics_intensity, self.caustics_depth_bias, self.water_gloss,
            self.water_color_scale,
        ];
        let mut w: Vec<u32> = floats.iter().map(|v| v.to_bits()).collect();
        w.push(self.flow_map_offset);
        words_to_bytes(&w)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialConstant {
    pub name_hash: u32,
    pub values: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialSampler {
    pub name_hash: u32,
    pub path: String,
}

/// A shader variation switch (0/1). Variations are compiled into the material's embedded
/// template, so changing one has no effect without rebuilding its shaders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialVariation {
    pub value: u32,
    pub name_hash: u32,
}

/// Overrides applied on top of the template. The engine merges each table against the
/// template's by ascending name hash, so [`MaterialSerialized::build`] writes them sorted.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MaterialSerialized {
    pub constants: Vec<MaterialConstant>,
    pub samplers: Vec<MaterialSampler>,
    pub variations: Vec<MaterialVariation>,
}

impl MaterialSerialized {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < SERIALIZED_HEADER_SIZE {
            return Err(ToolkitError::Parse(
                "serialized data section too small".into(),
            ));
        }
        let const_count = rd_u32(data, 0x04)? as usize;
        let const_values_off = rd_u32(data, 0x08)? as usize;
        let variation_count = rd_u32(data, 0x0C)? as usize;
        let variations_off = rd_u32(data, 0x10)? as usize;
        let sampler_count = rd_u32(data, 0x14)? as usize;
        let sampler_entries_off = rd_u32(data, 0x18)? as usize;
        let sampler_strings_off = rd_u32(data, 0x1C)? as usize;
        // Model-slot exclusion list: empty in every shipped material, and not written back.
        if rd_u32(data, 0x20)? != 0 {
            return Err(ToolkitError::Unsupported(
                "material has a model-slot exclusion list, which OmniTool can't rewrite yet".into(),
            ));
        }

        let mut constants = Vec::with_capacity(const_count);
        for i in 0..const_count {
            let base = SERIALIZED_HEADER_SIZE + i * CONST_ENTRY_SIZE;
            let value_off = rd_u16(data, base)? as usize;
            let byte_len = rd_u16(data, base + 2)? as usize;
            let name_hash = rd_u32(data, base + 4)?;
            if byte_len % 4 != 0 {
                return Err(ToolkitError::Parse(format!(
                    "constant {name_hash:#010X} has non-float size {byte_len}"
                )));
            }
            let mut values = Vec::with_capacity(byte_len / 4);
            for k in 0..byte_len / 4 {
                values.push(f32::from_bits(rd_u32(
                    data,
                    const_values_off + value_off + k * 4,
                )?));
            }
            constants.push(MaterialConstant { name_hash, values });
        }

        let mut variations = Vec::with_capacity(variation_count);
        for i in 0..variation_count {
            let base = variations_off + i * SAMPLER_ENTRY_SIZE;
            variations.push(MaterialVariation {
                value: rd_u32(data, base)?,
                name_hash: rd_u32(data, base + 4)?,
            });
        }

        let mut samplers = Vec::with_capacity(sampler_count);
        for i in 0..sampler_count {
            let base = sampler_entries_off + i * SAMPLER_ENTRY_SIZE;
            let string_off = rd_u32(data, base)? as usize;
            let name_hash = rd_u32(data, base + 4)?;
            samplers.push(MaterialSampler {
                name_hash,
                path: cstring_at(data, sampler_strings_off + string_off),
            });
        }

        Ok(Self {
            constants,
            samplers,
            variations,
        })
    }

    pub fn build(&self) -> Vec<u8> {
        let mut constants: Vec<&MaterialConstant> = self.constants.iter().collect();
        let mut samplers: Vec<&MaterialSampler> = self.samplers.iter().collect();
        let mut variations: Vec<&MaterialVariation> = self.variations.iter().collect();
        constants.sort_by_key(|c| c.name_hash);
        samplers.sort_by_key(|s| s.name_hash);
        variations.sort_by_key(|v| v.name_hash);

        let mut value_blob: Vec<u8> = Vec::new();
        let mut const_entries: Vec<u8> = Vec::new();
        for c in constants {
            let off = value_blob.len();
            const_entries.extend_from_slice(&(off as u16).to_le_bytes());
            const_entries.extend_from_slice(&((c.values.len() * 4) as u16).to_le_bytes());
            const_entries.extend_from_slice(&c.name_hash.to_le_bytes());
            for v in &c.values {
                value_blob.extend_from_slice(&v.to_bits().to_le_bytes());
            }
        }

        let mut string_blob: Vec<u8> = Vec::new();
        let mut sampler_entries: Vec<u8> = Vec::new();
        for s in samplers {
            sampler_entries.extend_from_slice(&(string_blob.len() as u32).to_le_bytes());
            sampler_entries.extend_from_slice(&s.name_hash.to_le_bytes());
            string_blob.extend_from_slice(s.path.as_bytes());
            string_blob.push(0);
        }

        let mut variation_entries: Vec<u8> = Vec::new();
        for v in variations {
            variation_entries.extend_from_slice(&v.value.to_le_bytes());
            variation_entries.extend_from_slice(&v.name_hash.to_le_bytes());
        }

        let const_values_off = SERIALIZED_HEADER_SIZE + const_entries.len();
        let variations_off = const_values_off + value_blob.len();
        let sampler_entries_off = variations_off + variation_entries.len();
        let sampler_strings_off = sampler_entries_off + sampler_entries.len();
        let total = align16(sampler_strings_off + string_blob.len());

        let mut out = Vec::with_capacity(total);
        for w in [
            total as u32,
            self.constants.len() as u32,
            const_values_off as u32,
            self.variations.len() as u32,
            variations_off as u32,
            self.samplers.len() as u32,
            sampler_entries_off as u32,
            sampler_strings_off as u32,
            0,
            0,
        ] {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out.extend_from_slice(&const_entries);
        out.extend_from_slice(&value_blob);
        out.extend_from_slice(&variation_entries);
        out.extend_from_slice(&sampler_entries);
        out.extend_from_slice(&string_blob);
        out.resize(total, 0);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MaterialSerialized {
        MaterialSerialized {
            constants: vec![
                MaterialConstant {
                    name_hash: 0x24F4_CA4C,
                    values: vec![0.5],
                },
                MaterialConstant {
                    name_hash: 0x4BAA_D667,
                    values: vec![1.0, 0.25, 0.0],
                },
            ],
            samplers: vec![
                MaterialSampler {
                    name_hash: 0xABDA_D780,
                    path: "characters/hero/textures/body_c.texture".into(),
                },
                MaterialSampler {
                    name_hash: 0x7AE3_4E7A,
                    path: "characters/hero/textures/body_n.texture".into(),
                },
            ],
            variations: vec![
                MaterialVariation {
                    value: 1,
                    name_hash: 0x8D0C_C279,
                },
                MaterialVariation {
                    value: 0,
                    name_hash: 0x9720_83DC,
                },
            ],
        }
    }

    fn sampler_path(s: &MaterialSerialized, hash: u32) -> &str {
        &s.samplers.iter().find(|x| x.name_hash == hash).unwrap().path
    }

    #[test]
    fn serialized_section_round_trips() {
        let built = sample().build();
        assert_eq!(built.len() % 16, 0);
        let parsed = MaterialSerialized::parse(&built).expect("parse");
        assert_eq!(parsed.build(), built);

        assert_eq!(parsed.constants.len(), 2);
        assert_eq!(parsed.constants[1].values, vec![1.0, 0.25, 0.0]);
        assert_eq!(sampler_path(&parsed, 0xABDA_D780), "characters/hero/textures/body_c.texture");
        assert_eq!(sampler_path(&parsed, 0x7AE3_4E7A), "characters/hero/textures/body_n.texture");

        // A non-empty variation table must not shift the sampler paths — the bug
        // that made a real material report "extures/..." for slot 3.
        assert_eq!(parsed.variations.len(), 2);
        assert_eq!(parsed.variations[0].value, 1);
        assert_eq!(parsed.variations[1].name_hash, 0x9720_83DC);
    }

    #[test]
    fn build_sorts_every_table_by_hash() {
        let parsed = MaterialSerialized::parse(&sample().build()).expect("parse");
        let sorted = |v: Vec<u32>| v.windows(2).all(|w| w[0] < w[1]);
        assert!(sorted(parsed.samplers.iter().map(|s| s.name_hash).collect()));
        assert!(sorted(parsed.constants.iter().map(|c| c.name_hash).collect()));
        assert!(sorted(parsed.variations.iter().map(|v| v.name_hash).collect()));
    }

    #[test]
    fn edited_values_survive_a_rebuild() {
        let mut doc = sample();
        doc.constants[0].values = vec![0.875];
        doc.samplers[0].path = "characters/hero/textures/custom_c.texture".into();
        let parsed = MaterialSerialized::parse(&doc.build()).expect("parse");
        assert_eq!(parsed.constants[0].values, vec![0.875]);
        assert_eq!(sampler_path(&parsed, 0xABDA_D780), "characters/hero/textures/custom_c.texture");
    }

    #[test]
    fn fur_and_water_info_round_trip() {
        let fur: Vec<u8> = (0..13u32).flat_map(|i| (i * 0x0101_0101).to_le_bytes()).collect();
        assert_eq!(MaterialFurInfo::parse(&fur).unwrap().build(), fur);
        let water: Vec<u8> = (0..19u32).flat_map(|i| (i * 0x0301_0101).to_le_bytes()).collect();
        assert_eq!(MaterialWaterInfo::parse(&water).unwrap().build(), water);
    }

    #[test]
    fn header_section_round_trips() {
        let bytes: Vec<u8> = (0..10u32).flat_map(|i| (i * 7).to_le_bytes()).collect();
        let parsed = MaterialHeaderSection::parse(&bytes).expect("parse");
        assert_eq!(parsed.build(), bytes);
    }

    #[test]
    fn empty_section_is_valid() {
        let built = MaterialSerialized::default().build();
        let parsed = MaterialSerialized::parse(&built).expect("parse");
        assert!(parsed.constants.is_empty() && parsed.samplers.is_empty());
    }
}
