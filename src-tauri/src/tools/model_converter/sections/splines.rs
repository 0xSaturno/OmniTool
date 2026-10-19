//! Model Spline* — the hair / fur strand system.
//!
//! Every hair group in a model is one `SplineSubset`, named by a
//! `HairDescription_*` string, and owns a contiguous run of strands. Strand
//! runs tile the whole strand array back to back, and each group's control
//! points form one contiguous block — but strands inside a group point at
//! their CVs in arbitrary order, so always slice through `Spline::cv_range`.

use crate::core::error::Result;
use byteorder::{ReadBytesExt, LE};
use std::io::Cursor;

pub const TAG_SPLINE_SUBSETS: u32 = 0x3C9DABDF;
pub const TAG_SPLINES: u32 = 0x27CA5246;
pub const TAG_SPLINE_SKIN_BINDING: u32 = 0xBB7303D5;
pub const TAG_SPLINE_CVS: u32 = 0xB25B3163;

/// Texture slots of a hair group, in record order.
pub const SPLINE_TEXTURE_SLOTS: [&str; 5] = ["Diffuse tint", "Specular", "Normal", "Gloss", "Thickness bias"];

/// One hair group, 1256 bytes. The identity fields, LOD / tessellation header
/// and the texture / config references are named; the envelope block between
/// them is kept raw.
#[derive(Debug, Clone)]
pub struct SplineSubset {
    pub name_hash: u32,
    pub name_offset: u32,
    pub strand_count: u32,
    pub first_strand: u32,
    // Order of the next four is inferred from the description defaults (tess min 4 on most groups).
    pub lod_distance: f32,
    pub lod_reduction: f32,
    pub tess_max: u16,
    pub tess_min: u16,
    /// String offsets of the five texture paths (empty string when unused).
    pub texture_path_offsets: [u32; 5],
    /// String offset of the description `.config`.
    pub config_path_offset: u32,
    /// Asset ids of the five textures; 0 when unused.
    pub texture_ids: [u64; 5],
    /// Bytes 0x10.. of the record, kept whole for round-tripping.
    pub params: Vec<u8>,
}

pub struct SplineSubsets {
    pub subsets: Vec<SplineSubset>,
}

impl SplineSubsets {
    pub const RECORD_SIZE: usize = 1256;

    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut subsets = Vec::with_capacity(data.len() / Self::RECORD_SIZE);
        for i in 0..data.len() / Self::RECORD_SIZE {
            let rec = &data[i * Self::RECORD_SIZE..(i + 1) * Self::RECORD_SIZE];
            let u = |o: usize| u32::from_le_bytes(rec[o..o + 4].try_into().unwrap());
            let f = |o: usize| f32::from_le_bytes(rec[o..o + 4].try_into().unwrap());
            let h = |o: usize| u16::from_le_bytes(rec[o..o + 2].try_into().unwrap());
            let q = |o: usize| u64::from_le_bytes(rec[o..o + 8].try_into().unwrap());
            subsets.push(SplineSubset {
                name_hash: u(0),
                name_offset: u(4),
                strand_count: u(8),
                first_strand: u(12),
                lod_distance: f(16),
                lod_reduction: f(20),
                tess_max: h(24),
                tess_min: h(26),
                texture_path_offsets: std::array::from_fn(|k| u(1128 + k * 4)),
                config_path_offset: u(1168),
                texture_ids: std::array::from_fn(|k| q(1176 + k * 8)),
                params: rec[16..].to_vec(),
            });
        }
        Ok(Self { subsets })
    }

    /// Total strands implied by the subset ranges.
    pub fn strand_total(&self) -> u32 {
        self.subsets
            .iter()
            .map(|s| s.first_strand + s.strand_count)
            .max()
            .unwrap_or(0)
    }
}

/// 12 bytes per strand: `u32 cv_start:24 | cv_count:8`, the root UV as two
/// u16s, and a packed tangent basis the GPU re-encodes when skinning.
#[derive(Debug, Clone, Copy)]
pub struct Spline {
    pub cv_start: u32,
    pub cv_count: u8,
    pub root_uv: [u16; 2],
    pub basis: u32,
}

impl Spline {
    pub fn cv_range(&self) -> std::ops::Range<usize> {
        self.cv_start as usize..self.cv_start as usize + self.cv_count as usize
    }
}

pub fn parse_splines(data: &[u8]) -> Result<Vec<Spline>> {
    let mut cur = Cursor::new(data);
    let mut out = Vec::with_capacity(data.len() / 12);
    for _ in 0..data.len() / 12 {
        let x = cur.read_u32::<LE>()?;
        out.push(Spline {
            cv_start: x & 0xFF_FFFF,
            cv_count: (x >> 24) as u8,
            root_uv: [cur.read_u16::<LE>()?, cur.read_u16::<LE>()?],
            basis: cur.read_u32::<LE>()?,
        });
    }
    Ok(out)
}

/// One strand control point, 8 bytes: position, then a two-byte encoded
/// normal. Positions are object space at **twice the Model Built position
/// scale** — verified on a model whose scale is not 1/4096.
#[derive(Debug, Clone, Copy)]
pub struct SplineCv {
    pub position: (i16, i16, i16),
    pub normal: [u8; 2],
}

impl SplineCv {
    /// `position_scale` is Model Built's value; splines use double it.
    pub fn decode(&self, position_scale: f32) -> (f32, f32, f32) {
        let s = position_scale * 2.0;
        (
            self.position.0 as f32 * s,
            self.position.1 as f32 * s,
            self.position.2 as f32 * s,
        )
    }
}

pub fn parse_spline_cvs(data: &[u8]) -> Result<Vec<SplineCv>> {
    let mut cur = Cursor::new(data);
    let mut out = Vec::with_capacity(data.len() / 8);
    for _ in 0..data.len() / 8 {
        out.push(SplineCv {
            position: (
                cur.read_i16::<LE>()?,
                cur.read_i16::<LE>()?,
                cur.read_i16::<LE>()?,
            ),
            normal: [cur.read_u8()?, cur.read_u8()?],
        });
    }
    Ok(out)
}

/// 8 bytes per strand: the three subset-relative vertices the root is bound
/// to, and a packed word — bits 12-15 pick the skinned subset slot, bits 0-11
/// carry the barycentric weights (encoding not yet decoded).
#[derive(Debug, Clone, Copy)]
pub struct SplineSkinBinding {
    pub vertices: [u16; 3],
    pub packed: u16,
}

impl SplineSkinBinding {
    pub fn subset_slot(&self) -> u8 {
        (self.packed >> 12) as u8
    }
}

pub fn parse_spline_skin_binding(data: &[u8]) -> Result<Vec<SplineSkinBinding>> {
    let mut cur = Cursor::new(data);
    let mut out = Vec::with_capacity(data.len() / 8);
    for _ in 0..data.len() / 8 {
        out.push(SplineSkinBinding {
            vertices: [
                cur.read_u16::<LE>()?,
                cur.read_u16::<LE>()?,
                cur.read_u16::<LE>()?,
            ],
            packed: cur.read_u16::<LE>()?,
        });
    }
    Ok(out)
}
