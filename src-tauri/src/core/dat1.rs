use std::collections::HashMap;
use crate::core::error::{Result, ToolkitError};
use byteorder::{LE, ReadBytesExt};
use std::io::{Cursor, Read, Seek, SeekFrom};

pub const DAT1_MAGIC: u32 = 0x44415431;
pub const PAD_TO: usize = 16;

#[derive(Debug, Clone)]
pub struct SectionHeader {
    pub tag: u32,
    pub offset: u32,
    pub size: u32,
}

#[derive(Debug)]
pub struct Dat1 {
    pub magic: u32,
    pub unk1: u32,
    pub total_size: u32,
    pub sections: Vec<SectionHeader>,
    pub unknowns: Vec<u8>,
    pub strings_pool: Vec<u8>,
    pub section_data: Vec<Vec<u8>>,
    pub sections_map: HashMap<u32, usize>,
}

impl Dat1 {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(data);

        let magic = cur.read_u32::<LE>()?;
        if magic != DAT1_MAGIC {
            return Err(ToolkitError::InvalidMagic { expected: DAT1_MAGIC, got: magic });
        }
        let unk1 = cur.read_u32::<LE>()?;
        let total_size = cur.read_u32::<LE>()?;
        let sections_count = cur.read_u16::<LE>()? as usize;
        let unknown_count = cur.read_u16::<LE>()? as usize;

        let mut sections = Vec::with_capacity(sections_count);
        for _ in 0..sections_count {
            sections.push(SectionHeader {
                tag: cur.read_u32::<LE>()?,
                offset: cur.read_u32::<LE>()?,
                size: cur.read_u32::<LE>()?,
            });
        }

        let mut unknowns = vec![0u8; 8 * unknown_count];
        cur.read_exact(&mut unknowns)?;

        // Strings pool: from current pos up to first section offset
        let header_end = Self::header_size(sections_count, unknown_count);
        let min_offset = sections.iter().map(|s| s.offset as usize).min().unwrap_or(data.len());
        let strings_len = if min_offset > header_end { min_offset - header_end } else { 0 };
        let mut strings_pool = vec![0u8; strings_len];
        cur.read_exact(&mut strings_pool)?;

        let mut section_data = Vec::with_capacity(sections_count);
        let mut sections_map = HashMap::new();
        for (i, s) in sections.iter().enumerate() {
            cur.seek(SeekFrom::Start(s.offset as u64))?;
            let mut buf = vec![0u8; s.size as usize];
            cur.read_exact(&mut buf)?;
            sections_map.insert(s.tag, i);
            section_data.push(buf);
        }

        Ok(Self { magic, unk1, total_size, sections, unknowns, strings_pool, section_data, sections_map })
    }

    fn header_size(sections_count: usize, unknown_count: usize) -> usize {
        16 + 12 * sections_count + 8 * unknown_count
    }

    pub fn header_end(&self) -> usize {
        Self::header_size(self.sections.len(), self.unknowns.len() / 8)
    }

    pub fn get_string(&self, raw_string_offset: u32) -> Option<String> {
        let rel = raw_string_offset as usize;
        let header_end = self.header_end();
        let offset = if raw_string_offset as usize >= header_end {
            raw_string_offset as usize - header_end
        } else {
            rel
        };
        if offset >= self.strings_pool.len() {
            return None;
        }
        let end = self.strings_pool[offset..].iter().position(|&b| b == 0).map(|p| offset + p).unwrap_or(self.strings_pool.len());
        String::from_utf8(self.strings_pool[offset..end].to_vec()).ok()
    }

    pub fn get_section_data(&self, tag: u32) -> Option<&[u8]> {
        self.sections_map.get(&tag).map(|&i| self.section_data[i].as_slice())
    }

    pub fn set_section_data(&mut self, tag: u32, data: Vec<u8>) -> Result<()> {
        let idx = *self.sections_map.get(&tag).ok_or_else(|| ToolkitError::SectionNotFound(tag))?;
        self.section_data[idx] = data;
        Ok(())
    }

    pub fn recalculate_section_headers(&mut self) {
        let header_end = self.header_end();
        let strings_len = self.strings_pool.len();
        let first_offset = header_end + strings_len;

        // sort by original offset to preserve order
        let mut order: Vec<usize> = (0..self.sections.len()).collect();
        order.sort_by_key(|&i| self.sections[i].offset);

        let mut cursor = first_offset;
        for &i in &order {
            if cursor % PAD_TO != 0 {
                cursor += PAD_TO - (cursor % PAD_TO);
            }
            self.sections[i].offset = cursor as u32;
            let sz = self.section_data[i].len();
            self.sections[i].size = sz as u32;
            cursor += sz;
        }
        self.total_size = cursor as u32;
    }

    pub fn save(&mut self) -> Vec<u8> {
        self.recalculate_section_headers();
        let mut out = Vec::new();

        out.extend_from_slice(&self.magic.to_le_bytes());
        out.extend_from_slice(&self.unk1.to_le_bytes());
        out.extend_from_slice(&self.total_size.to_le_bytes());
        out.extend_from_slice(&(self.sections.len() as u16).to_le_bytes());
        out.extend_from_slice(&((self.unknowns.len() / 8) as u16).to_le_bytes());

        let mut sorted_sections = self.sections.clone();
        sorted_sections.sort_by_key(|s| s.tag);
        for s in &sorted_sections {
            out.extend_from_slice(&s.tag.to_le_bytes());
            out.extend_from_slice(&s.offset.to_le_bytes());
            out.extend_from_slice(&s.size.to_le_bytes());
        }
        out.extend_from_slice(&self.unknowns);
        out.extend_from_slice(&self.strings_pool);

        let header_end = self.header_end();
        let strings_len = self.strings_pool.len();
        let mut cur_offset = header_end + strings_len;

        let mut offset_order: Vec<usize> = (0..self.sections.len()).collect();
        offset_order.sort_by_key(|&i| self.sections[i].offset);

        for i in offset_order {
            let target = self.sections[i].offset as usize;
            if cur_offset < target {
                out.resize(out.len() + (target - cur_offset), 0);
                cur_offset = target;
            }
            out.extend_from_slice(&self.section_data[i]);
            cur_offset += self.section_data[i].len();
        }

        out
    }
}
