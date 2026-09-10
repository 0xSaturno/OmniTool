//! `.material` asset format

use serde::{Deserialize, Serialize};

use crate::core::dat1::{Dat1, DAT1_MAGIC};
use crate::core::error::{Result, ToolkitError};

pub const TAG_MATERIAL_HEADER: u32 = 0xE127_5683;
pub const TAG_MATERIAL_SERIALIZED: u32 = 0xF526_0180;
pub const TAG_MATERIAL_FUR: u32 = 0xD9B1_2454;

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

    /// The `.materialgraph` this material instances, taken from the DAT1 string pool.
    pub fn template_path(&self) -> Option<String> {
        self.dat1
            .strings_pool
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .find(|s| s.to_lowercase().ends_with(".materialgraph"))
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

/// Fixed 40-byte Material Header section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialHeaderSection {
    pub unk00: u32,
    pub unk04: u32,
    pub flags: u32,
    pub audio_material_hash: u32,
    pub av_material_hash: u32,
    pub unk14: f32,
    pub unk18: u32,
    pub unk1c: f32,
    pub unk20: u32,
    pub unk24: u32,
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
            unk00: rd_u32(data, 0x00)?,
            unk04: rd_u32(data, 0x04)?,
            flags: rd_u32(data, 0x08)?,
            audio_material_hash: rd_u32(data, 0x0C)?,
            av_material_hash: rd_u32(data, 0x10)?,
            unk14: f32::from_bits(rd_u32(data, 0x14)?),
            unk18: rd_u32(data, 0x18)?,
            unk1c: f32::from_bits(rd_u32(data, 0x1C)?),
            unk20: rd_u32(data, 0x20)?,
            unk24: rd_u32(data, 0x24)?,
        })
    }

    pub fn build(&self) -> Vec<u8> {
        let words = [
            self.unk00,
            self.unk04,
            self.flags,
            self.audio_material_hash,
            self.av_material_hash,
            self.unk14.to_bits(),
            self.unk18,
            self.unk1c.to_bits(),
            self.unk20,
            self.unk24,
        ];
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
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

/// Entry of the 8-byte table that precedes the sampler table. Its records are
/// `<u32 value, u32 name_hash>` where the value is small (0, 1, …), not a
/// string offset, and the hashes are not template sampler slots — so these are
/// not textures. Purpose still unknown; kept verbatim so saving preserves them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialAuxEntry {
    pub value: u32,
    pub name_hash: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MaterialSerialized {
    pub constants: Vec<MaterialConstant>,
    pub samplers: Vec<MaterialSampler>,
    /// Table A — see [`MaterialAuxEntry`]. Empty in most materials.
    pub aux: Vec<MaterialAuxEntry>,
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
        // Two 8-byte tables back to back: the aux table at 0x10 (count at
        // 0x0C) and the sampler table at 0x18 (count at 0x14). They coincide
        // when the aux table is empty, which is the common case.
        let aux_count = rd_u32(data, 0x0C)? as usize;
        let aux_entries_off = rd_u32(data, 0x10)? as usize;
        let sampler_count = rd_u32(data, 0x14)? as usize;
        let sampler_entries_off = rd_u32(data, 0x18)? as usize;
        let sampler_strings_off = rd_u32(data, 0x1C)? as usize;

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

        let mut aux = Vec::with_capacity(aux_count);
        for i in 0..aux_count {
            let base = aux_entries_off + i * SAMPLER_ENTRY_SIZE;
            aux.push(MaterialAuxEntry {
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
            aux,
        })
    }

    pub fn build(&self) -> Vec<u8> {
        let mut value_blob: Vec<u8> = Vec::new();
        let mut const_entries: Vec<u8> = Vec::new();
        for c in &self.constants {
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
        for s in &self.samplers {
            sampler_entries.extend_from_slice(&(string_blob.len() as u32).to_le_bytes());
            sampler_entries.extend_from_slice(&s.name_hash.to_le_bytes());
            string_blob.extend_from_slice(s.path.as_bytes());
            string_blob.push(0);
        }

        let mut aux_entries: Vec<u8> = Vec::new();
        for a in &self.aux {
            aux_entries.extend_from_slice(&a.value.to_le_bytes());
            aux_entries.extend_from_slice(&a.name_hash.to_le_bytes());
        }

        let const_values_off = SERIALIZED_HEADER_SIZE + const_entries.len();
        let aux_entries_off = const_values_off + value_blob.len();
        let sampler_entries_off = aux_entries_off + aux_entries.len();
        let sampler_strings_off = sampler_entries_off + sampler_entries.len();
        let total = align16(sampler_strings_off + string_blob.len());

        let mut out = Vec::with_capacity(total);
        for w in [
            total as u32,
            self.constants.len() as u32,
            const_values_off as u32,
            self.aux.len() as u32,
            aux_entries_off as u32,
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
        out.extend_from_slice(&aux_entries);
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
            aux: vec![
                MaterialAuxEntry {
                    value: 1,
                    name_hash: 0x8D0C_C279,
                },
                MaterialAuxEntry {
                    value: 0,
                    name_hash: 0x9720_83DC,
                },
            ],
        }
    }

    #[test]
    fn serialized_section_round_trips() {
        let built = sample().build();
        assert_eq!(built.len() % 16, 0);
        let parsed = MaterialSerialized::parse(&built).expect("parse");
        assert_eq!(parsed.build(), built);

        let original = sample();
        assert_eq!(parsed.constants.len(), original.constants.len());
        assert_eq!(parsed.constants[1].values, vec![1.0, 0.25, 0.0]);
        assert_eq!(parsed.samplers[0].path, original.samplers[0].path);
        assert_eq!(parsed.samplers[1].name_hash, original.samplers[1].name_hash);

        // A non-empty aux table must not shift the sampler paths — the bug
        // that made a real material report "extures/..." for slot 3.
        assert_eq!(parsed.aux.len(), 2);
        assert_eq!(parsed.aux[0].value, 1);
        assert_eq!(parsed.aux[1].name_hash, 0x9720_83DC);
        assert!(parsed
            .samplers
            .iter()
            .all(|s| s.path.starts_with("characters/")));
    }

    #[test]
    fn edited_values_survive_a_rebuild() {
        let mut doc = sample();
        doc.constants[0].values = vec![0.875];
        doc.samplers[0].path = "characters/hero/textures/custom_c.texture".into();
        let parsed = MaterialSerialized::parse(&doc.build()).expect("parse");
        assert_eq!(parsed.constants[0].values, vec![0.875]);
        assert_eq!(
            parsed.samplers[0].path,
            "characters/hero/textures/custom_c.texture"
        );
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
