use byteorder::{LE, ReadBytesExt};
use std::io::Cursor;
use crate::core::error::Result;

pub const TAG_SKIN_BATCH: u32 = 0xC61B1FF5;
pub const TAG_SKIN_DATA:  u32 = 0xDCA379A2;
pub const TAG_RCRA_SKIN:  u32 = 0xCCBAFF15;

// Python struct: "<IIHHHH" = u32, u32, u16, u16, u16, u16 = 16 bytes
#[derive(Debug, Clone, Default)]
pub struct SkinBatch {
    pub offset: u32,
    pub z1: u32,        // u32, NOT u16 — second field is also 4 bytes
    pub z2: u16,
    pub unk1: u16,
    pub vertex_count: u16,
    pub first_vertex: u16,
}

impl SkinBatch {
    pub fn parse_all(data: &[u8]) -> Result<Vec<Self>> {
        const ENTRY_SIZE: usize = 16;
        let count = data.len() / ENTRY_SIZE;
        let mut batches = Vec::with_capacity(count);
        let mut cur = Cursor::new(data);
        for _ in 0..count {
            batches.push(Self {
                offset:       cur.read_u32::<LE>()?,
                z1:           cur.read_u32::<LE>()?,  // u32
                z2:           cur.read_u16::<LE>()?,
                unk1:         cur.read_u16::<LE>()?,
                vertex_count: cur.read_u16::<LE>()?,
                first_vertex: cur.read_u16::<LE>()?,
            });
        }
        Ok(batches)
    }

    pub fn save_all(batches: &[Self]) -> Vec<u8> {
        let mut out = Vec::with_capacity(batches.len() * 16);
        for b in batches {
            out.extend_from_slice(&b.offset.to_le_bytes());
            out.extend_from_slice(&b.z1.to_le_bytes());  // 4 bytes
            out.extend_from_slice(&b.z2.to_le_bytes());
            out.extend_from_slice(&b.unk1.to_le_bytes());
            out.extend_from_slice(&b.vertex_count.to_le_bytes());
            out.extend_from_slice(&b.first_vertex.to_le_bytes());
        }
        out
    }
}

/// RCRA 4-bone skin entry: 4 bone indices + 4 weights (u8 each)
#[derive(Debug, Clone, Copy, Default)]
pub struct RcraSkinEntry {
    pub bones: [u8; 4],
    pub weights: [u8; 4],
}

impl RcraSkinEntry {
    pub fn parse_all(data: &[u8]) -> Vec<Self> {
        data.chunks_exact(8).map(|c| Self {
            bones:   [c[0], c[1], c[2], c[3]],
            weights: [c[4], c[5], c[6], c[7]],
        }).collect()
    }

    pub fn save_all(entries: &[Self]) -> Vec<u8> {
        let mut out = Vec::with_capacity(entries.len() * 8);
        for e in entries {
            out.extend_from_slice(&e.bones);
            out.extend_from_slice(&e.weights);
        }
        out
    }
}

/// Per-vertex skin weights decoded from the raw skin data stream
pub type VertexWeights = Vec<(u8, f32)>; // (bone_index, weight)

pub fn decode_skin_data(
    raw: &[u8],
    batches: &[SkinBatch],
) -> Vec<VertexWeights> {
    let mut skin: Vec<VertexWeights> = Vec::new();

    for batch in batches {
        let count = batch.vertex_count as usize;
        let mut offset = batch.offset as usize;

        let mut j = 0;
        while j < count {
            if offset >= raw.len() { break; }
            let groups = (raw[offset] as usize) + 1;
            offset += 1;

            for z in 0..16 {
                if j + z >= count { break; }
                let mut vertex: VertexWeights = Vec::new();

                if groups == 1 {
                    if offset >= raw.len() { break; }
                    let bone = raw[offset];
                    offset += 1;
                    vertex.push((bone, 1.0));
                } else {
                    for _ in 0..groups {
                        if offset + 1 >= raw.len() { break; }
                        let bone   = raw[offset];
                        let weight = raw[offset + 1];
                        offset += 2;
                        vertex.push((bone, weight as f32 / 256.0));
                    }
                }
                skin.push(fix_weights(vertex));
            }
            j += 16;
        }
    }
    skin
}

fn fix_weights(v: VertexWeights) -> VertexWeights {
    let mut map: std::collections::HashMap<u8, f32> = std::collections::HashMap::new();
    let mut order = Vec::new();
    for (b, w) in v {
        *map.entry(b).or_insert(0.0) += w;
        if !order.contains(&b) { order.push(b); }
    }
    order.into_iter().map(|b| (b, map[&b])).collect()
}

pub fn decode_rcra_skin(entries: &[RcraSkinEntry]) -> Vec<VertexWeights> {
    entries.iter().map(|e| {
        let mut map: std::collections::HashMap<u8, f32> = std::collections::HashMap::new();
        for i in 0..4 {
            *map.entry(e.bones[i]).or_insert(0.0) += e.weights[i] as f32;
        }
        let sum: f32 = e.weights.iter().map(|&w| w as f32).sum();
        let mut weights: Vec<(u8, f32)> = map.into_iter()
            .map(|(b, w)| (b, if sum > 0.0 && sum != 1.0 { w / sum } else { w }))
            .collect();
        weights.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        weights.retain(|w| w.1 > 0.0);
        weights
    }).collect()
}
