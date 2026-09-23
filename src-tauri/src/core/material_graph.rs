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
