//! Small render-side sections: per-model material overrides, texture preloads,
//! ambient-shadow capsules and per-performance-mode LOD clamps.

use crate::core::error::Result;
use byteorder::{ReadBytesExt, LE};
use std::io::Cursor;

pub const TAG_RENDER_OVERRIDES: u32 = 0xBCE86B01;
pub const TAG_TEXTURE_OVERRIDES: u32 = 0x00823787;
/// Name not recovered; content matches the engine's ambient-shadow primitive list.
pub const TAG_AMBIENT_SHADOW_PRIMS: u32 = 0x7CA37DA0;
/// Name not recovered; content matches the engine's perf-profile max-LOD table.
pub const TAG_PERF_PROFILE_MAX_LOD: u32 = 0x665DA362;

/// 32 bytes: a material parameter override applied to one material slot.
#[derive(Debug, Clone, Copy)]
pub struct RenderOverride {
    pub value: [f32; 4],
    /// Hash of the material parameter name.
    pub name_hash: u32,
    /// Material mapping (slot) name hash the override targets.
    pub mapping_name_hash: u32,
    /// Texture asset for texture overrides, 0 otherwise.
    pub texture_id: u64,
}

pub fn parse_render_overrides(data: &[u8]) -> Result<Vec<RenderOverride>> {
    let mut cur = Cursor::new(data);
    let mut out = Vec::with_capacity(data.len() / 32);
    for _ in 0..data.len() / 32 {
        let mut value = [0f32; 4];
        for v in &mut value {
            *v = cur.read_f32::<LE>()?;
        }
        out.push(RenderOverride {
            value,
            name_hash: cur.read_u32::<LE>()?,
            mapping_name_hash: cur.read_u32::<LE>()?,
            texture_id: cur.read_u64::<LE>()?,
        });
    }
    Ok(out)
}

/// Model Texture Overrides: u32 string offsets of textures the model preloads.
pub fn parse_texture_overrides(data: &[u8]) -> Result<Vec<u32>> {
    let mut cur = Cursor::new(data);
    let mut out = Vec::with_capacity(data.len() / 4);
    for _ in 0..data.len() / 4 {
        out.push(cur.read_u32::<LE>()?);
    }
    Ok(out)
}

/// 48 bytes: a capsule (centre ± offsets, radius) that casts soft ambient shadow.
#[derive(Debug, Clone, Copy)]
pub struct AmbientShadowPrim {
    pub center: [f32; 3],
    pub radius: f32,
    pub offset_a: [f32; 3],
    /// Joint index, or a fade attenuation float, depending on the primitive.
    pub joint_or_fade: u32,
    pub offset_b: [f32; 3],
    /// Non-zero on the last primitive of the list.
    pub is_terminator: i32,
}

fn v3(c: &mut Cursor<&[u8]>) -> Result<[f32; 3]> {
    Ok([c.read_f32::<LE>()?, c.read_f32::<LE>()?, c.read_f32::<LE>()?])
}

pub fn parse_ambient_shadow_prims(data: &[u8]) -> Result<Vec<AmbientShadowPrim>> {
    let mut cur = Cursor::new(data);
    let mut out = Vec::with_capacity(data.len() / 48);
    for _ in 0..data.len() / 48 {
        let center = v3(&mut cur)?;
        let radius = cur.read_f32::<LE>()?;
        let offset_a = v3(&mut cur)?;
        let joint_or_fade = cur.read_u32::<LE>()?;
        let offset_b = v3(&mut cur)?;
        out.push(AmbientShadowPrim {
            center,
            radius,
            offset_a,
            joint_or_fade,
            offset_b,
            is_terminator: cur.read_i32::<LE>()?,
        });
    }
    Ok(out)
}

/// 8 bytes: the highest LOD the model may use under one performance mode.
#[derive(Debug, Clone, Copy)]
pub struct PerfProfileMaxLod {
    pub spec_hash: u32,
    pub max_lod: u32,
}

impl PerfProfileMaxLod {
    pub fn spec_name(&self) -> Option<&'static str> {
        match self.spec_hash {
            0xC4306C1C => Some("PS5_30_Spec"),
            0x960843BB => Some("PS5_60_Spec"),
            _ => None,
        }
    }
}

pub fn parse_perf_profile_max_lod(data: &[u8]) -> Result<Vec<PerfProfileMaxLod>> {
    let mut cur = Cursor::new(data);
    let mut out = Vec::with_capacity(data.len() / 8);
    for _ in 0..data.len() / 8 {
        out.push(PerfProfileMaxLod {
            spec_hash: cur.read_u32::<LE>()?,
            max_lod: cur.read_u32::<LE>()?,
        });
    }
    Ok(out)
}
