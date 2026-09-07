//! `.materialgraph` template tables — the slot list a `.material` overrides.

use serde::{Deserialize, Serialize};

use crate::core::dat1::Dat1;
use crate::core::error::{Result, ToolkitError};
use crate::core::material::MaterialFile;

pub const TAG_TEMPLATE_SAMPLERS: u32 = 0x1CAF_E804;
pub const TAG_TEMPLATE_CONSTANTS: u32 = 0x45C4_F4C0;
pub const TAG_TEMPLATE_CONSTANTS_CONTENT: u32 = 0xA59F_667B;

const SAMPLER_ENTRY_SIZE: usize = 16;
const CONSTANT_ENTRY_SIZE: usize = 8;

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
    pub slot_index: u16,
    pub type_hash: u32,
    pub default_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateConstant {
    pub name_hash: u32,
    pub default_values: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MaterialTemplate {
    pub samplers: Vec<TemplateSampler>,
    pub constants: Vec<TemplateConstant>,
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
                let value_off = rd_u16(sec, base)? as usize;
                let byte_len = rd_u16(sec, base + 2)? as usize;
                let name_hash = rd_u32(sec, base + 4)?;
                let mut default_values = Vec::with_capacity(byte_len / 4);
                for k in 0..byte_len / 4 {
                    default_values
                        .push(f32::from_bits(rd_u32(content, value_off + k * 4).unwrap_or(0)));
                }
                constants.push(TemplateConstant { name_hash, default_values });
            }
        }

        Ok(Self { samplers, constants })
    }
}
