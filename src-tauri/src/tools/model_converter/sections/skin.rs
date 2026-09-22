use crate::core::error::Result;
use byteorder::{ReadBytesExt, LE};
use std::io::Cursor;

pub const TAG_SKIN_BATCH: u32 = 0xC61B1FF5;
pub const TAG_SKIN_DATA: u32 = 0xDCA379A2;
pub const TAG_RCRA_SKIN: u32 = 0xCCBAFF15;
pub const TAG_SKIN_JOINT_REMAP: u32 = 0x5240C82B;

/// The game starts a new batch at 2560 vertices, at 256 distinct joints, or where morphing vertices end.
pub const SKIN_BATCH_VERT_MAX: usize = 2560;
pub const SKIN_BATCH_JOINT_MAX: usize = 256;

/// Joint ids in skin data and GPU Skin are local to their batch: `local + joint_remap_offset`
/// when `joint_remap_count` is 0, else `u16` entry `local` of the table at byte `joint_remap_offset`
/// of Model Skin Joint Remap.
#[derive(Debug, Clone, Default)]
pub struct SkinBatch {
    /// Offset into Model Skin Data.
    pub offset: u32,
    /// Joint-index base, or a byte offset into Model Skin Joint Remap when `joint_remap_count` > 0.
    pub joint_remap_offset: u32,
    pub joint_remap_count: u16,
    /// Average influences per vertex in 4.12 fixed point (profiling only).
    pub avg_joint_influences: u16,
    pub vertex_count: u16,
    /// First vertex of the batch, relative to the subset.
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
                offset: cur.read_u32::<LE>()?,
                joint_remap_offset: cur.read_u32::<LE>()?,
                joint_remap_count: cur.read_u16::<LE>()?,
                avg_joint_influences: cur.read_u16::<LE>()?,
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
            out.extend_from_slice(&b.joint_remap_offset.to_le_bytes());
            out.extend_from_slice(&b.joint_remap_count.to_le_bytes());
            out.extend_from_slice(&b.avg_joint_influences.to_le_bytes());
            out.extend_from_slice(&b.vertex_count.to_le_bytes());
            out.extend_from_slice(&b.first_vertex.to_le_bytes());
        }
        out
    }
}

/// Model GPU Skin entry: 4 batch-local joint ids (same base / remap as the vertex's skin batch) + 4 weights /256.
#[derive(Debug, Clone, Copy, Default)]
pub struct RcraSkinEntry {
    pub bones: [u8; 4],
    pub weights: [u8; 4],
}

impl RcraSkinEntry {
    pub fn parse_all(data: &[u8]) -> Vec<Self> {
        data.chunks_exact(8)
            .map(|c| Self {
                bones: [c[0], c[1], c[2], c[3]],
                weights: [c[4], c[5], c[6], c[7]],
            })
            .collect()
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

/// Per-vertex skin weights as (model joint index, weight).
pub type VertexWeights = Vec<(u16, f32)>;

/// Resolves a batch-local joint id to a model joint index.
pub fn batch_joint(batch: &SkinBatch, remap: &[u8], local: u8) -> u16 {
    if batch.joint_remap_count == 0 {
        return (batch.joint_remap_offset + local as u32) as u16;
    }
    let o = batch.joint_remap_offset as usize + local as usize * 2;
    remap.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).unwrap_or(0)
}

/// The skin sections of one model, decoded per subset.
pub struct SkinSource {
    pub data: Vec<u8>,
    pub batches: Vec<SkinBatch>,
    pub remap: Vec<u8>,
    pub gpu: Vec<RcraSkinEntry>,
}

impl SkinSource {
    pub fn from_dat1(dat1: &crate::core::dat1::Dat1) -> Option<Self> {
        let data = dat1.get_section_data(TAG_SKIN_DATA)?.to_vec();
        let batches = SkinBatch::parse_all(dat1.get_section_data(TAG_SKIN_BATCH)?).ok()?;
        Some(Self {
            data,
            batches,
            remap: dat1.get_section_data(TAG_SKIN_JOINT_REMAP).map(|d| d.to_vec()).unwrap_or_default(),
            gpu: dat1.get_section_data(TAG_RCRA_SKIN).map(RcraSkinEntry::parse_all).unwrap_or_default(),
        })
    }

    /// Weights of every vertex of `mesh`, indexed from its first vertex; empty for unskinned subsets.
    pub fn subset_weights(&self, mesh: &super::meshes::MeshDefinition) -> Vec<VertexWeights> {
        let count = mesh.skin_batch_count() as usize;
        let first = mesh.first_skin_batch as usize;
        if count == 0 {
            if !mesh.is_rcra_skinned() {
                return Vec::new();
            }
            let a = mesh.first_weight_index as usize;
            let b = (a + mesh.vertex_count as usize).min(self.gpu.len());
            return self.gpu.get(a..b).unwrap_or(&[]).iter().map(decode_gpu_entry).collect();
        }
        let mut out = vec![Vec::new(); mesh.vertex_count as usize];
        for batch in self.batches.get(first..first + count).unwrap_or(&[]) {
            let raw = &self.data;
            let mut p = batch.offset as usize;
            let mut n = 0usize;
            while n < batch.vertex_count as usize && p < raw.len() {
                let joints = raw[p] as usize + 1;
                p += 1;
                for _ in 0..16.min(batch.vertex_count as usize - n) {
                    let mut v: VertexWeights = Vec::with_capacity(joints);
                    if joints == 1 {
                        let Some(&j) = raw.get(p) else { break };
                        v.push((batch_joint(batch, &self.remap, j), 1.0));
                        p += 1;
                    } else {
                        for _ in 0..joints {
                            let (Some(&j), Some(&w)) = (raw.get(p), raw.get(p + 1)) else { break };
                            v.push((batch_joint(batch, &self.remap, j), w as f32 / 256.0));
                            p += 2;
                        }
                    }
                    if let Some(slot) = out.get_mut(batch.first_vertex as usize + n) {
                        *slot = merge_duplicates(v);
                    }
                    n += 1;
                }
            }
        }
        out
    }
}

fn merge_duplicates(v: VertexWeights) -> VertexWeights {
    let mut out: VertexWeights = Vec::with_capacity(v.len());
    for (j, w) in v {
        match out.iter_mut().find(|e| e.0 == j) {
            Some(e) => e.1 += w,
            None => out.push((j, w)),
        }
    }
    out
}

fn decode_gpu_entry(e: &RcraSkinEntry) -> VertexWeights {
    let sum: f32 = e.weights.iter().map(|&w| w as f32).sum();
    let mut v = merge_duplicates(
        (0..4).map(|i| (e.bones[i] as u16, e.weights[i] as f32 / sum.max(1.0))).collect(),
    );
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v.retain(|w| w.1 > 0.0);
    v
}

