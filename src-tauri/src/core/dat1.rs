use std::collections::HashMap;
use crate::core::error::{Result, ToolkitError};
use byteorder::{LE, ReadBytesExt};
use std::io::{Cursor, Read, Seek, SeekFrom};

pub const DAT1_MAGIC: u32 = 0x44415431;
pub const PAD_TO: usize = 16;

/// Sections that start on a 64-byte boundary rather than the usual 16. Both are
/// arrays of 64-byte records — Model Subset and Model Locator — and every sample
/// model places them at a multiple of 64; aligning them to 16 reproduces the
/// right bytes at the wrong offsets.
const ALIGN_64: [u32; 2] = [0x78D9CBDE, 0x9F614FAB];

fn section_alignment(tag: u32) -> usize {
    if ALIGN_64.contains(&tag) { 64 } else { PAD_TO }
}

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
    /// Pointer fixups, 8 bytes each: `u32 pointer location, u32 target`, both DAT1-absolute.
    /// The loader writes the target's address into the pointer location.
    pub fixups: Vec<u8>,
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
        let fixup_count = cur.read_u16::<LE>()? as usize;

        let mut sections = Vec::with_capacity(sections_count);
        for _ in 0..sections_count {
            sections.push(SectionHeader {
                tag: cur.read_u32::<LE>()?,
                offset: cur.read_u32::<LE>()?,
                size: cur.read_u32::<LE>()?,
            });
        }

        let mut fixups = vec![0u8; 8 * fixup_count];
        cur.read_exact(&mut fixups)?;

        // Strings pool: from current pos up to first section offset
        let header_end = Self::header_size(sections_count, fixup_count);
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

        Ok(Self { magic, unk1, total_size, sections, fixups, strings_pool, section_data, sections_map })
    }

    fn header_size(sections_count: usize, fixup_count: usize) -> usize {
        16 + 12 * sections_count + 8 * fixup_count
    }

    pub fn header_end(&self) -> usize {
        Self::header_size(self.sections.len(), self.fixups.len() / 8)
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

    /// Appends a NUL-terminated string to the pool and returns its DAT1-absolute offset.
    /// Existing offsets stay valid: the pool only grows at its end, and the header does not change size.
    pub fn append_string(&mut self, s: &str) -> u32 {
        let offset = (self.header_end() + self.strings_pool.len()) as u32;
        self.strings_pool.extend_from_slice(s.as_bytes());
        self.strings_pool.push(0);
        offset
    }

    /// Offset of an existing pooled string equal to `s`, else a newly appended one.
    pub fn intern_string(&mut self, s: &str) -> u32 {
        let needle: Vec<u8> = s.bytes().chain(std::iter::once(0)).collect();
        let mut start = 0usize;
        for (i, &b) in self.strings_pool.iter().enumerate() {
            if b == 0 {
                if self.strings_pool[start..=i] == needle[..] {
                    return (self.header_end() + start) as u32;
                }
                start = i + 1;
            }
        }
        self.append_string(s)
    }

    pub fn get_section_data(&self, tag: u32) -> Option<&[u8]> {
        self.sections_map.get(&tag).map(|&i| self.section_data[i].as_slice())
    }

    pub fn set_section_data(&mut self, tag: u32, data: Vec<u8>) -> Result<()> {
        let idx = *self.sections_map.get(&tag).ok_or_else(|| ToolkitError::SectionNotFound(tag))?;
        self.section_data[idx] = data;
        Ok(())
    }

    /// Drops sections and every fixup into them. The string pool is front-padded by what the
    /// header loses, so absolute string offsets and the fixups that target them stay valid.
    pub fn remove_sections(&mut self, tags: &[u32]) -> usize {
        let doomed: Vec<usize> = (0..self.sections.len()).filter(|&i| tags.contains(&self.sections[i].tag)).collect();
        if doomed.is_empty() {
            return 0;
        }
        let inside = |x: u32| {
            doomed.iter().any(|&i| {
                let s = &self.sections[i];
                x >= s.offset && x < s.offset + s.size
            })
        };
        let pairs = self.fixup_pairs();
        let kept: Vec<(u32, u32)> = pairs.iter().copied().filter(|&(a, b)| !inside(a) && !inside(b)).collect();
        let shrink = 12 * doomed.len() + 8 * (pairs.len() - kept.len());
        self.set_fixup_pairs(&kept);
        for &i in doomed.iter().rev() {
            self.sections.remove(i);
            self.section_data.remove(i);
        }
        self.sections_map = self.sections.iter().enumerate().map(|(i, s)| (s.tag, i)).collect();
        self.strings_pool.splice(0..0, std::iter::repeat(0u8).take(shrink));
        doomed.len()
    }

    pub fn recalculate_section_headers(&mut self) {
        let old: Vec<(u32, u32)> = self.sections.iter().map(|s| (s.offset, s.size)).collect();
        let header_end = self.header_end();
        let strings_len = self.strings_pool.len();
        let first_offset = header_end + strings_len;

        // sort by original offset to preserve order
        let mut order: Vec<usize> = (0..self.sections.len()).collect();
        order.sort_by_key(|&i| self.sections[i].offset);

        let mut cursor = first_offset;
        for &i in &order {
            let align = section_alignment(self.sections[i].tag);
            if cursor % align != 0 {
                cursor += align - (cursor % align);
            }
            self.sections[i].offset = cursor as u32;
            let sz = self.section_data[i].len();
            self.sections[i].size = sz as u32;
            cursor += sz;
        }
        self.total_size = cursor as u32;
        self.relocate_fixups(&old);
    }

    /// Pointer fixups as `(pointer location, target)` pairs.
    pub fn fixup_pairs(&self) -> Vec<(u32, u32)> {
        self.fixups
            .chunks_exact(8)
            .map(|c| (u32::from_le_bytes(c[0..4].try_into().unwrap()), u32::from_le_bytes(c[4..8].try_into().unwrap())))
            .collect()
    }

    pub fn set_fixup_pairs(&mut self, pairs: &[(u32, u32)]) {
        self.fixups = pairs.iter().flat_map(|(a, b)| a.to_le_bytes().into_iter().chain(b.to_le_bytes())).collect();
    }

    /// Moves fixup offsets that fall inside a section along with it. `old` is each section's
    /// `(offset, size)` before relayout; offsets in the header or string pool stay put.
    fn relocate_fixups(&mut self, old: &[(u32, u32)]) {
        let moved = |x: u32| {
            old.iter()
                .zip(&self.sections)
                .find(|((off, size), _)| x >= *off && x < off + size)
                .map_or(x, |((off, _), s)| s.offset + (x - off))
        };
        let pairs: Vec<(u32, u32)> = self.fixup_pairs().into_iter().map(|(a, b)| (moved(a), moved(b))).collect();
        self.set_fixup_pairs(&pairs);
    }

    pub fn save(&mut self) -> Vec<u8> {
        self.recalculate_section_headers();
        let mut out = Vec::new();

        out.extend_from_slice(&self.magic.to_le_bytes());
        out.extend_from_slice(&self.unk1.to_le_bytes());
        out.extend_from_slice(&self.total_size.to_le_bytes());
        out.extend_from_slice(&(self.sections.len() as u16).to_le_bytes());
        out.extend_from_slice(&((self.fixups.len() / 8) as u16).to_le_bytes());

        let mut sorted_sections = self.sections.clone();
        sorted_sections.sort_by_key(|s| s.tag);
        for s in &sorted_sections {
            out.extend_from_slice(&s.tag.to_le_bytes());
            out.extend_from_slice(&s.offset.to_le_bytes());
            out.extend_from_slice(&s.size.to_le_bytes());
        }
        out.extend_from_slice(&self.fixups);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Two 16-byte sections, a pool holding "abc", and one fixup from B+4 to that string.
    fn sample() -> Dat1 {
        let mut dat1 = Dat1 {
            magic: DAT1_MAGIC,
            unk1: 0,
            total_size: 0,
            sections: vec![
                SectionHeader { tag: 1, offset: 0, size: 0 },
                SectionHeader { tag: 2, offset: 1, size: 0 },
            ],
            fixups: vec![0; 8],
            strings_pool: b"abc\0".to_vec(),
            section_data: vec![vec![0xAA; 16], vec![0xBB; 16]],
            sections_map: HashMap::from([(1, 0), (2, 1)]),
        };
        dat1.recalculate_section_headers();
        let b = dat1.sections[1].offset;
        let s = dat1.header_end() as u32;
        dat1.set_fixup_pairs(&[(b + 4, s)]);
        Dat1::parse(&dat1.save()).unwrap()
    }

    #[test]
    fn fixups_follow_a_moved_section() {
        let mut dat1 = sample();
        let before = dat1.fixup_pairs()[0];
        dat1.set_section_data(1, vec![0xAA; 48]).unwrap();
        let dat1 = Dat1::parse(&dat1.save()).unwrap();
        let (src, dst) = dat1.fixup_pairs()[0];
        assert_eq!(src, dat1.sections[1].offset + 4);
        assert_eq!(src, before.0 + 32);
        assert_eq!(dst, before.1);
        assert_eq!(dat1.get_string(dst).as_deref(), Some("abc"));
    }

    #[test]
    fn fixups_survive_pool_growth() {
        let mut dat1 = sample();
        let added = dat1.append_string("a/longer/path.materialgraph");
        let dat1 = Dat1::parse(&dat1.save()).unwrap();
        let (src, dst) = dat1.fixup_pairs()[0];
        assert_eq!(src, dat1.sections[1].offset + 4);
        assert_eq!(dat1.get_string(dst).as_deref(), Some("abc"));
        assert_eq!(dat1.get_string(added).as_deref(), Some("a/longer/path.materialgraph"));
    }

    #[test]
    fn removed_section_keeps_string_offsets() {
        let mut dat1 = sample();
        let added = dat1.append_string("kept");
        assert_eq!(dat1.remove_sections(&[1]), 1);
        let dat1 = Dat1::parse(&dat1.save()).unwrap();
        assert_eq!(dat1.sections.len(), 1);
        let [(src, dst)] = dat1.fixup_pairs()[..] else { panic!("the fixup in B should remain") };
        assert_eq!(src, dat1.sections[0].offset + 4);
        assert_eq!(dat1.get_string(dst).as_deref(), Some("abc"));
        assert_eq!(dat1.get_string(added).as_deref(), Some("kept"));
    }

    #[test]
    fn removed_section_drops_its_fixups() {
        let mut dat1 = sample();
        let s = dat1.fixup_pairs()[0].1;
        assert_eq!(dat1.remove_sections(&[2]), 1);
        let dat1 = Dat1::parse(&dat1.save()).unwrap();
        assert!(dat1.fixup_pairs().is_empty());
        assert_eq!(dat1.get_string(s).as_deref(), Some("abc"));
    }

    #[test]
    fn unchanged_file_keeps_fixups_verbatim() {
        let mut dat1 = sample();
        let before = dat1.fixups.clone();
        let _ = dat1.save();
        assert_eq!(dat1.fixups, before);
    }
}
