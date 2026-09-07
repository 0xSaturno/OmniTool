//! Model Spline* — the hair / fur strand system.
//!
//! Every hair group in a model is one `SplineSubset`, named by a
//! `HairDescription_*` string, and owns a contiguous run of strands. Strand
//! runs tile the whole strand array back to back, so `first_strand + count` of
//! one subset is the `first_strand` of the next and the last one ends exactly
//! at the strand total.

use crate::core::error::Result;
use byteorder::{ReadBytesExt, LE};
use std::io::Cursor;

pub const TAG_SPLINE_SUBSETS: u32 = 0x3C9DABDF;
pub const TAG_SPLINES: u32 = 0x27CA5246;
pub const TAG_SPLINE_SKIN_BINDING: u32 = 0xBB7303D5;
/// Unnamed. Per-control-point stream, roughly 8-9 points per strand.
pub const TAG_SPLINE_POINTS: u32 = 0xB25B3163;

/// One hair group. The record is 1256 bytes; only the leading identity fields
/// and the strand range are understood, the rest is a parameter block.
#[derive(Debug, Clone)]
pub struct SplineSubset {
    pub name_hash: u32,
    pub name_offset: u32,
    pub strand_count: u32,
    pub first_strand: u32,
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
            let base = i * Self::RECORD_SIZE;
            let mut cur = Cursor::new(&data[base..]);
            subsets.push(SplineSubset {
                name_hash: cur.read_u32::<LE>()?,
                name_offset: cur.read_u32::<LE>()?,
                strand_count: cur.read_u32::<LE>()?,
                first_strand: cur.read_u32::<LE>()?,
                params: data[base + 16..base + Self::RECORD_SIZE].to_vec(),
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

/// 12 bytes per strand. `point_count` slices the control-point stream: strands
/// consume it in order, and the counts sum to exactly the number of point
/// records on every sampled model.
#[derive(Debug, Clone, Copy)]
pub struct Spline {
    pub unk0: u16,
    pub flags: u8,
    pub point_count: u8,
    pub packed1: u32,
    pub packed2: u32,
}

pub fn parse_splines(data: &[u8]) -> Result<Vec<Spline>> {
    let mut cur = Cursor::new(data);
    let mut out = Vec::with_capacity(data.len() / 12);
    for _ in 0..data.len() / 12 {
        out.push(Spline {
            unk0: cur.read_u16::<LE>()?,
            flags: cur.read_u8()?,
            point_count: cur.read_u8()?,
            packed1: cur.read_u32::<LE>()?,
            packed2: cur.read_u32::<LE>()?,
        });
    }
    Ok(out)
}

/// First control point of each strand, i.e. the running sum of `point_count`.
pub fn spline_point_offsets(splines: &[Spline]) -> Vec<u32> {
    let mut out = Vec::with_capacity(splines.len() + 1);
    let mut acc = 0u32;
    out.push(0);
    for s in splines {
        acc += s.point_count as u32;
        out.push(acc);
    }
    out
}

/// One strand control point, 8 bytes. Positions are object space, quantised at
/// **twice the Model Built position scale** — verified on a model whose scale
/// is not 1/4096, where only the doubled scale puts each hair group in its
/// anatomically correct place.
#[derive(Debug, Clone, Copy)]
pub struct SplinePoint {
    pub position: (i16, i16, i16),
    /// Two per-point parameters that rise monotonically from root to tip;
    /// consistent with arc length and taper, though unconfirmed.
    pub params: (u8, u8),
}

impl SplinePoint {
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

pub fn parse_spline_points(data: &[u8]) -> Result<Vec<SplinePoint>> {
    let mut cur = Cursor::new(data);
    let mut out = Vec::with_capacity(data.len() / 8);
    for _ in 0..data.len() / 8 {
        out.push(SplinePoint {
            position: (
                cur.read_i16::<LE>()?,
                cur.read_i16::<LE>()?,
                cur.read_i16::<LE>()?,
            ),
            params: (cur.read_u8()?, cur.read_u8()?),
        });
    }
    Ok(out)
}

/// 8 bytes per strand: the three mesh vertices the strand root is barycentric
/// against, plus a packed weight/flag word.
#[derive(Debug, Clone, Copy)]
pub struct SplineSkinBinding {
    pub vertices: [u16; 3],
    pub packed: u16,
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
