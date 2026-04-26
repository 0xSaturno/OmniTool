use std::io::{Read, Cursor};
use byteorder::{LE, ReadBytesExt};
use crate::core::dat1::{Dat1, DAT1_MAGIC};
use crate::core::codec::detect_and_decompress;
use crate::core::error::{Result, ToolkitError};

pub const MAGIC_RCRA: u32 = 0x9D2C0FA9;

pub struct ModelFile {
    pub magic: u32,
    pub offset_to_stream_sections: u32,
    pub stream_sections_size: u32,
    pub unk: [u8; 24],
    pub dat1: Dat1,
}

impl ModelFile {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 4 {
            return Err(ToolkitError::Parse("model file too small".into()));
        }

        let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());

        // Archive-stored models are raw DAT1 with no outer wrapper.
        if magic == DAT1_MAGIC {
            let dat1 = Dat1::parse(data)?;
            return Ok(Self {
                magic: DAT1_MAGIC,
                offset_to_stream_sections: 0,
                stream_sections_size: 0,
                unk: [0u8; 24],
                dat1,
            });
        }

        if magic != MAGIC_RCRA {
            return Err(ToolkitError::UnknownVersion(magic));
        }

        let mut cur = Cursor::new(data);
        cur.read_u32::<LE>()?; // magic already read
        let offset_to_stream_sections = cur.read_u32::<LE>()?;
        let stream_sections_size = cur.read_u32::<LE>()?;

        let mut unk = [0u8; 24];
        cur.read_exact(&mut unk)?;

        let mut rest = Vec::new();
        cur.read_to_end(&mut rest)?;

        let decompressed = detect_and_decompress(&rest)?;
        let dat1 = Dat1::parse(&decompressed)?;

        Ok(Self { magic, offset_to_stream_sections, stream_sections_size, unk, dat1 })
    }

    pub fn save(&mut self) -> Vec<u8> {
        const TAG_INDEXES: u32 = 0x0859863D;
        self.dat1.recalculate_section_headers();

        // Raw DAT1: no outer wrapper, save as-is.
        if self.magic == DAT1_MAGIC {
            return self.dat1.save();
        }

        if let Some(s) = self.dat1.sections.iter().find(|s| s.tag == TAG_INDEXES) {
            self.offset_to_stream_sections = s.offset;
        }
        let dat1_bytes = self.dat1.save();
        self.stream_sections_size = (dat1_bytes.len() as u32).saturating_sub(self.offset_to_stream_sections);

        let mut out = Vec::new();
        out.extend_from_slice(&self.magic.to_le_bytes());
        out.extend_from_slice(&self.offset_to_stream_sections.to_le_bytes());
        out.extend_from_slice(&self.stream_sections_size.to_le_bytes());
        out.extend_from_slice(&self.unk);
        out.extend_from_slice(&dat1_bytes);
        out
    }
}
