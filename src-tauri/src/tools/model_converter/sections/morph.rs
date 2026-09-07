//! Model Anim Morph Info — the directory for the facial/blend-shape system.
//!
//! Layout, confirmed against Rift Apart hero models:
//!
//! ```text
//! MorphInfoHeader              0x00 .. 0x40 (rest of the 0x40 block is zero)
//! (u32 id, u32 offset)[lookup_count + 1]    at lookup_offset, sorted by id
//! (u32 left_id, u32 right_id)[pairs_count + 1]  at pairs_offset
//! MorphEntry[lookup_count]     starting at the first lookup offset
//! ```
//!
//! The terminator entry of each table is `0xFFFFFFFF`. Entry bodies are the
//! variable-size `morph_entry_t` from Rivet's `docs/research/model_morph.c`;
//! the deltas themselves live in Model Anim Morph Data as a packed bit stream.

use crate::core::error::Result;
use byteorder::{ReadBytesExt, LE};
use std::io::Cursor;

pub const TAG_ANIM_MORPH_INFO: u32 = 0x380A5744;
pub const TAG_ANIM_MORPH_DATA: u32 = 0x5E709570;
pub const TAG_ANIM_MORPH_INDICES: u32 = 0xA600C108;

#[derive(Debug, Clone, Copy, Default)]
pub struct MorphInfoHeader {
    pub zero: u32,
    /// Byte size of the packed delta stream this directory describes.
    pub data_size: u32,
    pub lookup_count: u16,
    pub pairs_count: u16,
    pub lookup_offset: u32,
    pub pairs_offset: u32,
    pub unk: u32,
}

/// Header of one morph target. Everything past `index_size` is the variable
/// descriptor block whose length is `descriptor_size`.
#[derive(Debug, Clone)]
pub struct MorphEntry {
    pub id: u32,
    pub name_offset: u32,
    pub vertex_offset: u32,
    pub index_offset: u32,
    pub element_count: u8,
    pub element_bit_size: u8,
    pub component_bit_size: u8,
    /// (max, min) per element, used to unpack the quantised deltas.
    pub ranges: Vec<(f32, f32)>,
    pub subset_count: u16,
    pub descriptor_size: u16,
    pub vertex_size: u32,
    pub index_size: u32,
    /// Offset of this entry inside the section, for callers that want the raw
    /// descriptor bytes.
    pub offset: usize,

    // ---- descriptor block ----
    /// Model Subset indices this morph touches.
    pub subset_ids: Vec<u8>,
    /// Per-subset byte offsets, relative to `vertex_offset`.
    pub subset_vertex_offsets: Vec<u32>,
    /// Per-subset byte offsets, relative to `index_offset`.
    pub subset_index_offsets: Vec<u32>,
    pub subset_vertex_counts: Vec<u16>,
    /// First chunk of each subset; a subset owns chunks up to the next
    /// subset's start (or the end of the table).
    pub subset_chunk_start: Vec<u16>,
    /// (vertex count, index-pair count) per chunk.
    pub chunks: Vec<(u16, u32)>,
}

impl MorphEntry {
    /// Bytes one chunk occupies in Model Anim Morph Data. The bit stream is
    /// padded out to a 4-byte boundary; this reproduces every stored
    /// `vertex_size` and per-subset offset across all sampled models.
    pub fn chunk_bytes(&self, vertex_count: u16) -> usize {
        let bits = vertex_count as usize * self.element_count as usize * self.element_bit_size as usize;
        let bytes = bits.div_ceil(8);
        (bytes + 3) & !3
    }

    /// Half-open chunk range owned by subset `s`.
    pub fn subset_chunks(&self, s: usize) -> (usize, usize) {
        let lo = self.subset_chunk_start[s] as usize;
        let hi = self
            .subset_chunk_start
            .get(s + 1)
            .map(|&x| x as usize)
            .unwrap_or(self.chunks.len());
        (lo, hi)
    }
}

/// MSB-first bit reader. The delta stream is big-endian at the bit level —
/// decoding it LSB-first produces values that are statistically
/// indistinguishable from noise, while MSB-first yields the smooth per-vertex
/// field a morph target is supposed to be.
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u64,
    bits: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8], byte_offset: usize) -> Self {
        Self { data, pos: byte_offset, acc: 0, bits: 0 }
    }

    fn read(&mut self, n: u32) -> u32 {
        while self.bits < n {
            let b = self.data.get(self.pos).copied().unwrap_or(0);
            self.pos += 1;
            self.acc = (self.acc << 8) | b as u64;
            self.bits += 8;
        }
        let v = ((self.acc >> (self.bits - n)) & ((1u64 << n) - 1)) as u32;
        self.bits -= n;
        self.acc &= (1u64 << self.bits) - 1;
        v
    }
}

/// One morphed vertex: `element_count` elements of three components each.
/// Element 0 is the position delta, element 1 the normal delta.
#[derive(Debug, Clone)]
pub struct MorphDelta {
    pub vertex: u32,
    pub elements: Vec<[f32; 3]>,
}

impl AnimMorphInfo {
    /// Decodes one subset of one morph into per-vertex deltas.
    ///
    /// `data` is Model Anim Morph Data, `indices` Model Anim Morph Indices.
    /// Vertex ids are relative to the subset's `vertex_start` in Model Subset.
    pub fn decode_subset(
        entry: &MorphEntry,
        subset: usize,
        data: &[u8],
        indices: &[u8],
    ) -> Vec<MorphDelta> {
        let (lo, hi) = entry.subset_chunks(subset);
        let mut out = Vec::with_capacity(entry.subset_vertex_counts[subset] as usize);

        // Index side: (u16 gap, u16 run) pairs, cursor carried across the
        // subset's chunks. A run of 0 means 32 — a zero-length run would be
        // meaningless, and this reproduces every chunk's vertex count exactly.
        let mut idx_pos = (entry.index_offset + entry.subset_index_offsets[subset]) as usize;
        let mut cursor: u32 = 0;
        let mut ids: Vec<u32> = Vec::new();
        for ci in lo..hi {
            for _ in 0..entry.chunks[ci].1 {
                if idx_pos + 4 > indices.len() {
                    break;
                }
                let gap = u16::from_le_bytes(indices[idx_pos..idx_pos + 2].try_into().unwrap()) as u32;
                let run = u16::from_le_bytes(indices[idx_pos + 2..idx_pos + 4].try_into().unwrap());
                idx_pos += 4;
                let run = if run == 0 { 32u32 } else { run as u32 };
                cursor += gap;
                ids.extend(cursor..cursor + run);
                cursor += run;
            }
        }

        // Delta side: one MSB-first bit stream per chunk.
        let mut off = (entry.vertex_offset + entry.subset_vertex_offsets[subset]) as usize;
        let mut next = 0usize;
        for ci in lo..hi {
            let nv = entry.chunks[ci].0;
            let mut br = BitReader::new(data, off);
            for _ in 0..nv {
                let mut elements = Vec::with_capacity(entry.element_count as usize);
                for el in 0..entry.element_count as usize {
                    let (max, min) = entry.ranges.get(el).copied().unwrap_or((0.0, 0.0));
                    let mut v = [0f32; 3];
                    for c in &mut v {
                        *c = br.read(entry.component_bit_size as u32) as f32 * max + min;
                    }
                    elements.push(v);
                }
                if let Some(&vertex) = ids.get(next) {
                    out.push(MorphDelta { vertex, elements });
                }
                next += 1;
            }
            off += entry.chunk_bytes(nv);
        }
        out
    }
}

pub struct AnimMorphInfo {
    pub header: MorphInfoHeader,
    /// (morph id, byte offset of its MorphEntry), sorted by id.
    pub lookup: Vec<(u32, u32)>,
    /// LF_/RT_ morph ids driven together by one slider.
    pub pairs: Vec<(u32, u32)>,
    pub entries: Vec<MorphEntry>,
}

impl AnimMorphInfo {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 0x40 {
            return Ok(Self {
                header: MorphInfoHeader::default(),
                lookup: Vec::new(),
                pairs: Vec::new(),
                entries: Vec::new(),
            });
        }
        let mut cur = Cursor::new(data);
        let header = MorphInfoHeader {
            zero: cur.read_u32::<LE>()?,
            data_size: cur.read_u32::<LE>()?,
            lookup_count: cur.read_u16::<LE>()?,
            pairs_count: cur.read_u16::<LE>()?,
            lookup_offset: cur.read_u32::<LE>()?,
            pairs_offset: cur.read_u32::<LE>()?,
            unk: cur.read_u32::<LE>()?,
        };

        let read_pairs = |off: u32, count: usize| -> Result<Vec<(u32, u32)>> {
            let mut c = Cursor::new(data);
            c.set_position(off as u64);
            let mut v = Vec::with_capacity(count);
            for _ in 0..count {
                if c.position() as usize + 8 > data.len() {
                    break;
                }
                v.push((c.read_u32::<LE>()?, c.read_u32::<LE>()?));
            }
            Ok(v)
        };

        // Both tables carry an extra 0xFFFFFFFF terminator, which is dropped here.
        let lookup = read_pairs(header.lookup_offset, header.lookup_count as usize)?;
        let pairs = read_pairs(header.pairs_offset, header.pairs_count as usize)?;

        let mut entries = Vec::with_capacity(lookup.len());
        for &(_, off) in &lookup {
            if let Some(e) = Self::parse_entry(data, off as usize)? {
                entries.push(e);
            }
        }
        Ok(Self {
            header,
            lookup,
            pairs,
            entries,
        })
    }

    fn parse_entry(data: &[u8], offset: usize) -> Result<Option<MorphEntry>> {
        if offset + 20 > data.len() {
            return Ok(None);
        }
        let mut cur = Cursor::new(data);
        cur.set_position(offset as u64);
        let id = cur.read_u32::<LE>()?;
        let name_offset = cur.read_u32::<LE>()?;
        let vertex_offset = cur.read_u32::<LE>()?;
        let index_offset = cur.read_u32::<LE>()?;
        let element_count = cur.read_u8()?;
        let element_bit_size = cur.read_u8()?;
        let component_bit_size = cur.read_u8()?;
        cur.read_u8()?; // padding

        let mut ranges = Vec::with_capacity(element_count as usize);
        for _ in 0..element_count {
            if cur.position() as usize + 8 > data.len() {
                return Ok(None);
            }
            ranges.push((cur.read_f32::<LE>()?, cur.read_f32::<LE>()?));
        }
        if cur.position() as usize + 12 > data.len() {
            return Ok(None);
        }
        let subset_count = cur.read_u16::<LE>()? as usize;
        let descriptor_size = cur.read_u16::<LE>()?;
        let vertex_size = cur.read_u32::<LE>()?;
        let index_size = cur.read_u32::<LE>()?;

        // Descriptor block. Each sub-array is padded up to a 4-byte boundary
        // before the next one starts.
        let desc = cur.position() as usize;
        let end = (desc + descriptor_size as usize).min(data.len());
        let a4 = |x: usize| (x + 3) & !3;
        let mut p = desc;
        let take_u8 = |p: usize, n: usize| -> Vec<u8> {
            data.get(p..p + n).map(|s| s.to_vec()).unwrap_or_default()
        };
        let take_u32 = |p: usize, n: usize| -> Vec<u32> {
            (0..n)
                .filter_map(|i| data.get(p + i * 4..p + i * 4 + 4))
                .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
                .collect()
        };
        let take_u16 = |p: usize, n: usize| -> Vec<u16> {
            (0..n)
                .filter_map(|i| data.get(p + i * 2..p + i * 2 + 2))
                .map(|s| u16::from_le_bytes(s.try_into().unwrap()))
                .collect()
        };

        let subset_ids = take_u8(p, subset_count);
        p += a4(subset_count);
        let subset_vertex_offsets = take_u32(p, subset_count);
        p += 4 * subset_count;
        let subset_index_offsets = take_u32(p, subset_count);
        p += 4 * subset_count;
        let subset_vertex_counts = take_u16(p, subset_count);
        p += a4(2 * subset_count);
        let subset_chunk_start = take_u16(p, subset_count);
        p += a4(2 * subset_count);

        let mut chunks = Vec::new();
        while p + 6 <= end {
            let nv = u16::from_le_bytes(data[p..p + 2].try_into().unwrap());
            let ni = u32::from_le_bytes(data[p + 2..p + 6].try_into().unwrap());
            chunks.push((nv, ni));
            p += 6;
        }

        Ok(Some(MorphEntry {
            id,
            name_offset,
            vertex_offset,
            index_offset,
            element_count,
            element_bit_size,
            component_bit_size,
            ranges,
            subset_count: subset_count as u16,
            descriptor_size,
            vertex_size,
            index_size,
            offset,
            subset_ids,
            subset_vertex_offsets,
            subset_index_offsets,
            subset_vertex_counts,
            subset_chunk_start,
            chunks,
        }))
    }
}
