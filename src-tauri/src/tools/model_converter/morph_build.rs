//! Decodes a model's morph targets into per-subset deltas and writes them back in the
//! game's layout (Model Anim Morph Info / Data / Indices).

use std::collections::BTreeMap;

use crate::core::error::{Result, ToolkitError};
use super::sections::meshes::MeshDefinition;
use super::sections::morph::{chunk_bases, AnimMorphInfo};
use super::sections::skin::SkinBatch;

/// One morphed vertex: subset-relative vertex, position delta, normal delta.
pub type Delta = (u32, [f32; 3], [f32; 3]);

#[derive(Debug, Clone)]
pub struct MorphDef {
    pub id: u32,
    pub name: String,
    /// String-pool offset of the name, when it is already pooled.
    pub name_offset: Option<u32>,
    pub component_bits: u8,
    /// Original (step, base) per element, reused when the new values still fit.
    pub ranges: Option<[(f32, f32); 2]>,
    /// Subset index -> deltas.
    pub subsets: BTreeMap<u16, Vec<Delta>>,
}

#[derive(Debug, Clone, Default)]
pub struct MorphSet {
    pub morphs: Vec<MorphDef>,
    pub pairs: Vec<(u32, u32)>,
}

impl MorphSet {
    pub fn decode(info: &[u8], data: &[u8], indices: &[u8], subsets: &[MeshDefinition], batches: &[SkinBatch], name_of: impl Fn(u32) -> Option<String>) -> Result<Self> {
        let mi = AnimMorphInfo::parse(info)?;
        let mut morphs = Vec::with_capacity(mi.entries.len());
        for e in &mi.entries {
            let mut map = BTreeMap::new();
            for s in 0..e.subset_count as usize {
                let sid = e.subset_ids[s] as u16;
                let bases = subsets
                    .get(sid as usize)
                    .map(|m| chunk_bases(m.first_skin_batch, m.skin_batch_count(), batches))
                    .unwrap_or_default();
                let deltas = AnimMorphInfo::decode_subset(e, s, data, indices, &bases)
                    .into_iter()
                    .map(|d| {
                        let p = d.elements.first().copied().unwrap_or([0.0; 3]);
                        let n = d.elements.get(1).copied().unwrap_or([0.0; 3]);
                        (d.vertex, p, n)
                    })
                    .collect();
                map.insert(sid, deltas);
            }
            let r = |i: usize| e.ranges.get(i).copied().unwrap_or((0.0, 0.0));
            morphs.push(MorphDef {
                id: e.id,
                name: name_of(e.name_offset).unwrap_or_else(|| format!("{:08X}", e.id)),
                name_offset: Some(e.name_offset),
                component_bits: e.component_bit_size,
                ranges: (e.element_count == 2).then(|| [r(0), r(1)]),
                subsets: map,
            });
        }
        Ok(Self { morphs, pairs: mi.pairs })
    }
}

/// Output of `encode`: the three sections plus how many deltas could not be placed.
pub struct EncodedMorphs {
    pub info: Vec<u8>,
    pub data: Vec<u8>,
    pub indices: Vec<u8>,
    /// Deltas on vertices outside their subset's morphing (anim-vert) batches.
    pub dropped: usize,
}

struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    bits: u32,
}

impl BitWriter {
    fn put(&mut self, v: u32, n: u32) {
        self.acc = (self.acc << n) | (v as u64 & ((1u64 << n) - 1));
        self.bits += n;
        while self.bits >= 8 {
            self.out.push((self.acc >> (self.bits - 8)) as u8);
            self.bits -= 8;
        }
        self.acc &= (1u64 << self.bits) - 1;
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bits > 0 {
            self.out.push((self.acc << (8 - self.bits)) as u8);
        }
        let pad = (4 - self.out.len() % 4) % 4;
        self.out.resize(self.out.len() + pad, 0);
        self.out
    }
}

/// (step, base) per element that covers every value; the original ranges when they still fit.
fn quantization(m: &MorphDef, bits: u8) -> [(f32, f32); 2] {
    let top = ((1u64 << bits) - 1) as f32;
    let mut lo = [f32::MAX; 2];
    let mut hi = [f32::MIN; 2];
    for d in m.subsets.values().flatten() {
        for (e, v) in [d.1, d.2].iter().enumerate() {
            for c in v {
                lo[e] = lo[e].min(*c);
                hi[e] = hi[e].max(*c);
            }
        }
    }
    let fits = |(step, base): (f32, f32), e: usize| {
        lo[e] > hi[e] || (lo[e] >= base - step * 0.5 && hi[e] <= base + step * (top + 0.5))
    };
    if let Some(r) = m.ranges {
        if fits(r[0], 0) && fits(r[1], 1) {
            return r;
        }
    }
    [0, 1].map(|e| {
        if lo[e] > hi[e] {
            (0.0, 0.0)
        } else {
            ((hi[e] - lo[e]) / top, lo[e])
        }
    })
}

fn quantize(v: f32, (step, base): (f32, f32), bits: u8) -> u32 {
    if step <= 0.0 {
        return 0;
    }
    (((v - base) / step).round().max(0.0) as u64).min((1u64 << bits) - 1) as u32
}

/// `(u16 gap, u16 run)` pairs over sorted batch-local ids; runs are capped at 32, which is stored as 0.
fn index_runs(ids: &[u32]) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    let mut cursor = 0u32;
    let mut i = 0;
    while i < ids.len() {
        let start = ids[i];
        let mut end = start + 1;
        i += 1;
        while i < ids.len() && ids[i] == end {
            end += 1;
            i += 1;
        }
        let mut gap = start - cursor;
        let mut s = start;
        while s < end {
            let run = (end - s).min(32);
            out.push((gap as u16, if run == 32 { 0 } else { run as u16 }));
            gap = 0;
            s += run;
        }
        cursor = end;
    }
    out
}

fn put_u16(o: &mut Vec<u8>, v: u16) {
    o.extend_from_slice(&v.to_le_bytes());
}
fn put_u32(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&v.to_le_bytes());
}
fn pad4(o: &mut Vec<u8>) {
    o.resize(o.len() + (4 - o.len() % 4) % 4, 0);
}

/// Writes the three morph sections. Each morph lists, for every subset it touches, one chunk per
/// morphing batch of that subset (chunk k = batch k, ids relative to the batch start).
/// `name_offset` must supply a pooled string offset for every morph without one.
pub fn encode(set: &MorphSet, subsets: &[MeshDefinition], batches: &[SkinBatch], mut name_offset: impl FnMut(&str) -> u32) -> Result<EncodedMorphs> {
    let mut morphs: Vec<&MorphDef> = set.morphs.iter().collect();
    morphs.sort_by_key(|m| m.id);
    let ids: Vec<u32> = morphs.iter().map(|m| m.id).collect();
    // Mirror pairs are listed in both directions, sorted by the first id.
    let mut pairs: Vec<(u32, u32)> = set
        .pairs
        .iter()
        .flat_map(|&(l, r)| [(l, r), (r, l)])
        .filter(|(l, r)| ids.binary_search(l).is_ok() && ids.binary_search(r).is_ok())
        .collect();
    pairs.sort();
    pairs.dedup();

    let (mut data, mut indices) = (Vec::new(), Vec::new());
    let mut dropped = 0usize;
    let mut entries: Vec<Vec<u8>> = Vec::with_capacity(morphs.len());
    for m in &morphs {
        let bits = m.component_bits.clamp(1, 10);
        let q = quantization(m, bits);
        let (vertex_offset, index_offset) = (data.len() as u32, indices.len() as u32);

        struct Sub {
            id: u8,
            voff: u32,
            ioff: u32,
            count: u16,
            chunks: Vec<(u16, u32)>,
        }
        let mut subs: Vec<Sub> = Vec::new();
        for (&sid, deltas) in &m.subsets {
            if deltas.is_empty() {
                continue;
            }
            let id = u8::try_from(sid).map_err(|_| {
                ToolkitError::Parse(format!("morph '{}' touches subset {sid}; morph subsets must be below 256", m.name))
            })?;
            let mesh = subsets.get(sid as usize).ok_or_else(|| ToolkitError::Parse(format!("morph subset {sid} out of range")))?;
            let avc = mesh.anim_vert_batch_count() as usize;
            let base = mesh.first_skin_batch as usize;
            let windows: Vec<(u32, u32)> = batches
                .get(base..base + avc.min(mesh.skin_batch_count() as usize))
                .unwrap_or(&[])
                .iter()
                .map(|b| (b.first_vertex as u32, b.vertex_count as u32))
                .collect();
            if windows.is_empty() {
                dropped += deltas.len();
                continue;
            }
            let mut per_chunk: Vec<Vec<&Delta>> = vec![Vec::new(); windows.len()];
            for d in deltas {
                match windows.iter().position(|&(f, n)| d.0 >= f && d.0 < f + n) {
                    Some(k) => per_chunk[k].push(d),
                    None => dropped += 1,
                }
            }
            let (voff, ioff) = (data.len() as u32 - vertex_offset, indices.len() as u32 - index_offset);
            let mut chunks = Vec::with_capacity(windows.len());
            let mut count = 0u16;
            for (k, list) in per_chunk.iter_mut().enumerate() {
                list.sort_by_key(|d| d.0);
                list.dedup_by_key(|d| d.0);
                let mut bw = BitWriter { out: Vec::new(), acc: 0, bits: 0 };
                for d in list.iter() {
                    for c in d.1 {
                        bw.put(quantize(c, q[0], bits), bits as u32);
                    }
                    for c in d.2 {
                        bw.put(quantize(c, q[1], bits), bits as u32);
                    }
                }
                data.extend(bw.finish());
                let local: Vec<u32> = list.iter().map(|d| d.0 - windows[k].0).collect();
                let runs = index_runs(&local);
                for (g, r) in &runs {
                    put_u16(&mut indices, *g);
                    put_u16(&mut indices, *r);
                }
                chunks.push((list.len() as u16, runs.len() as u32));
                count += list.len() as u16;
            }
            subs.push(Sub { id, voff, ioff, count, chunks });
        }

        let mut desc = Vec::new();
        desc.extend(subs.iter().map(|s| s.id));
        pad4(&mut desc);
        subs.iter().for_each(|s| put_u32(&mut desc, s.voff));
        subs.iter().for_each(|s| put_u32(&mut desc, s.ioff));
        subs.iter().for_each(|s| put_u16(&mut desc, s.count));
        pad4(&mut desc);
        let mut start = 0u16;
        for s in &subs {
            put_u16(&mut desc, start);
            start += s.chunks.len() as u16;
        }
        pad4(&mut desc);
        for s in &subs {
            for &(nv, ni) in &s.chunks {
                put_u16(&mut desc, nv);
                put_u32(&mut desc, ni);
            }
        }
        pad4(&mut desc);
        let desc_size = u16::try_from(desc.len()).map_err(|_| ToolkitError::Parse(format!("morph '{}' descriptor too large", m.name)))?;

        let mut e = Vec::new();
        put_u32(&mut e, m.id);
        put_u32(&mut e, m.name_offset.unwrap_or_else(|| name_offset(&m.name)));
        put_u32(&mut e, vertex_offset);
        put_u32(&mut e, index_offset);
        e.extend_from_slice(&[2, bits * 3, bits, 0]);
        for (step, base) in q {
            e.extend_from_slice(&step.to_le_bytes());
            e.extend_from_slice(&base.to_le_bytes());
        }
        put_u16(&mut e, subs.len() as u16);
        put_u16(&mut e, desc_size);
        put_u32(&mut e, data.len() as u32 - vertex_offset);
        put_u32(&mut e, indices.len() as u32 - index_offset);
        e.extend(desc);
        entries.push(e);
    }

    // Header (0x40), lookup + terminator, pairs + terminator, then 16-aligned entries.
    let lookup_offset = 0x40usize;
    let pairs_offset = lookup_offset + 8 * (morphs.len() + 1);
    let mut cursor = pairs_offset + 8 * (pairs.len() + 1);
    let mut entry_offsets = Vec::with_capacity(entries.len());
    for e in &entries {
        cursor = (cursor + 15) & !15;
        entry_offsets.push(cursor);
        cursor += e.len();
    }
    let mut info = vec![0u8; 0x40];
    info[4..8].copy_from_slice(&((data.len() + indices.len()) as u32).to_le_bytes());
    info[8..10].copy_from_slice(&(morphs.len() as u16).to_le_bytes());
    info[10..12].copy_from_slice(&(pairs.len() as u16).to_le_bytes());
    info[12..16].copy_from_slice(&(lookup_offset as u32).to_le_bytes());
    info[16..20].copy_from_slice(&(pairs_offset as u32).to_le_bytes());
    info[20..24].copy_from_slice(&2u32.to_le_bytes());
    for (m, off) in morphs.iter().zip(&entry_offsets) {
        put_u32(&mut info, m.id);
        put_u32(&mut info, *off as u32);
    }
    info.extend_from_slice(&[0xFF; 8]);
    for (l, r) in &pairs {
        put_u32(&mut info, *l);
        put_u32(&mut info, *r);
    }
    info.extend_from_slice(&[0xFF; 8]);
    for (e, off) in entries.iter().zip(&entry_offsets) {
        info.resize(*off, 0);
        info.extend_from_slice(e);
    }
    pad4(&mut info);
    Ok(EncodedMorphs { info, data, indices, dropped })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_cap_at_32() {
        let ids: Vec<u32> = (5..45).chain(50..52).collect();
        assert_eq!(index_runs(&ids), vec![(5, 0), (0, 8), (5, 2)]);
    }

    #[test]
    fn bits_are_msb_first_and_padded() {
        let mut bw = BitWriter { out: Vec::new(), acc: 0, bits: 0 };
        bw.put(0b101, 3);
        bw.put(0b1, 1);
        assert_eq!(bw.finish(), vec![0b1011_0000, 0, 0, 0]);
    }
}
