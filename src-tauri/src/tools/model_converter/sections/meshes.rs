use byteorder::{LE, ReadBytesExt};
use std::io::Cursor;
use crate::core::error::Result;

pub const TAG_MESHES: u32 = 0x78D9CBDE;

/// Subset flags at 0x24. Shipped models only set 0, 4, 6, 8, 10, 11 and 14.
pub mod subset_flags {
    pub const IS_SKINNED: u16 = 1 << 0;
    pub const VERT_HAS_UV1: u16 = 1 << 1;
    pub const FADE_OUT_PAIR_A: u16 = 1 << 2;
    pub const FADE_OUT_PAIR_B: u16 = 1 << 3;
    pub const LOCAL_INDICES: u16 = 1 << 4;
    pub const LENS_FLARE_OCCLUDER: u16 = 1 << 5;
    pub const SKIP_RAY_TRACING: u16 = 1 << 6;
    pub const LOD_PROXY: u16 = 1 << 7;
    pub const GPU_SKINNING: u16 = 1 << 8;
    pub const SKIP_SKIN_THRESHOLD: u16 = 1 << 9;
    pub const IMPOSTOR_HQ_OVERRIDE: u16 = 1 << 10;
    pub const IMPOSTOR_HQ_STATUS: u16 = 1 << 11;
    pub const SINGLE_SIDED_IN_LOD1: u16 = 1 << 13;
    pub const SINGLE_SIDED: u16 = 1 << 14;

    /// Flags an import may set or clear per material slot; the others follow the geometry.
    pub const OVERRIDABLE: [(&str, u16); 9] = [
        ("fade_out_pair_a", FADE_OUT_PAIR_A),
        ("fade_out_pair_b", FADE_OUT_PAIR_B),
        ("lens_flare_occluder", LENS_FLARE_OCCLUDER),
        ("skip_ray_tracing", SKIP_RAY_TRACING),
        ("skip_skin_threshold", SKIP_SKIN_THRESHOLD),
        ("impostor_hq_override", IMPOSTOR_HQ_OVERRIDE),
        ("impostor_hq_status", IMPOSTOR_HQ_STATUS),
        ("single_sided_in_lod1", SINGLE_SIDED_IN_LOD1),
        ("single_sided", SINGLE_SIDED),
    ];
}

#[derive(Debug, Clone)]
pub struct MeshDefinition {
    /// Object-space bounding-sphere centre of the subset's vertices.
    pub bsphere_center: [f32; 3],
    /// Metres = raw * meters_per_unit * 2.
    pub bsphere_radius: i16,
    /// Half-extents; metres = raw * meters_per_unit.
    pub aabb_extents: [i16; 3],

    pub vertex_start: u32,
    pub index_start: u32,
    pub index_count: u32,
    pub vertex_count: u32,

    pub flags: u16,
    pub material_index: u16,
    pub first_skin_batch: u16,
    /// Two bytes: skin batch count (low) and anim-vert batch count (high); use the accessors.
    pub skin_batches_count: u16,

    /// sqrt(surface area in m²) in 8.8 fixed point, saturating at 0xFFFF.
    pub surface_area_sqrt: u16,
    /// LOD id (bits 0-3), shared-geometry ref (bits 4-14), proxy flag (bit 15).
    pub lod_proxy_id: u16,
    /// UV units per metre along the tangent / bitangent (texture mip streaming).
    pub uv_density_u: f32, pub uv_density_v: f32,

    pub first_weight_index: u32,
    pub fade_out_dist: i16,
    pub material_lod_dist: i16,
}

impl MeshDefinition {
    pub fn get_flags(&self) -> u16 { self.flags }
    pub fn get_material(&self) -> u16 { self.material_index }
    pub fn is_skinned(&self) -> bool { (self.flags & subset_flags::IS_SKINNED) != 0 }
    pub fn is_rcra_skinned(&self) -> bool { (self.flags & subset_flags::GPU_SKINNING) != 0 }
    pub fn has_relative_indices(&self) -> bool { (self.flags & subset_flags::LOCAL_INDICES) != 0 }

    /// Skin batches owned by this subset, starting at `first_skin_batch`.
    pub fn skin_batch_count(&self) -> u8 { self.skin_batches_count as u8 }
    /// How many of those batches (the leading ones) hold morphing vertices.
    pub fn anim_vert_batch_count(&self) -> u8 { (self.skin_batches_count >> 8) as u8 }

    pub fn lod_id(&self) -> u8 { (self.lod_proxy_id & 0xF) as u8 }
    pub fn lod_ref(&self) -> u16 { (self.lod_proxy_id >> 4) & 0x7FF }
    pub fn is_lod_proxy(&self) -> bool { self.lod_proxy_id & 0x8000 != 0 }

    pub fn bsphere_radius_m(&self, meters_per_unit: f32) -> f32 {
        self.bsphere_radius as f32 * meters_per_unit * 2.0
    }

    pub fn aabb_extents_m(&self, meters_per_unit: f32) -> [f32; 3] {
        self.aabb_extents.map(|e| e as f32 * meters_per_unit)
    }

    pub fn surface_area_m2(&self) -> f32 {
        let s = self.surface_area_sqrt as f32 / 256.0;
        s * s
    }

    pub fn parse_all(data: &[u8]) -> Result<Vec<Self>> {
        const ENTRY_SIZE: usize = 64;
        let count = data.len() / ENTRY_SIZE;
        let mut meshes = Vec::with_capacity(count);
        for i in 0..count {
            meshes.push(Self::parse(&data[i * ENTRY_SIZE..(i + 1) * ENTRY_SIZE])?);
        }
        Ok(meshes)
    }

    fn parse(data: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(data);
        let bsphere_center = [cur.read_f32::<LE>()?, cur.read_f32::<LE>()?, cur.read_f32::<LE>()?];
        let bsphere_radius = cur.read_i16::<LE>()?;
        let aabb_extents = [cur.read_i16::<LE>()?, cur.read_i16::<LE>()?, cur.read_i16::<LE>()?];
        let vertex_start = cur.read_u32::<LE>()?;
        let index_start  = cur.read_u32::<LE>()?;
        let index_count  = cur.read_u32::<LE>()?;
        let vertex_count = cur.read_u32::<LE>()?;
        let flags              = cur.read_u16::<LE>()?;
        let material_index     = cur.read_u16::<LE>()?;
        let first_skin_batch   = cur.read_u16::<LE>()?;
        let skin_batches_count = cur.read_u16::<LE>()?;
        let surface_area_sqrt = cur.read_u16::<LE>()?;
        let lod_proxy_id      = cur.read_u16::<LE>()?;
        let uv_density_u = cur.read_f32::<LE>()?;
        let uv_density_v = cur.read_f32::<LE>()?;
        let first_weight_index = cur.read_u32::<LE>()?;
        let fade_out_dist      = cur.read_i16::<LE>()?;
        let material_lod_dist  = cur.read_i16::<LE>()?;

        Ok(Self {
            bsphere_center, bsphere_radius, aabb_extents,
            vertex_start, index_start, index_count, vertex_count,
            flags, material_index, first_skin_batch, skin_batches_count,
            surface_area_sqrt, lod_proxy_id, uv_density_u, uv_density_v,
            first_weight_index, fade_out_dist, material_lod_dist,
        })
    }

    pub fn save_all(meshes: &[Self]) -> Vec<u8> {
        let mut out = Vec::with_capacity(meshes.len() * 64);
        for m in meshes { out.extend_from_slice(&m.save()); }
        out
    }

    pub fn save(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64);
        for c in &self.bsphere_center { out.extend_from_slice(&c.to_le_bytes()); }
        out.extend_from_slice(&self.bsphere_radius.to_le_bytes());
        for e in &self.aabb_extents { out.extend_from_slice(&e.to_le_bytes()); }
        out.extend_from_slice(&self.vertex_start.to_le_bytes());
        out.extend_from_slice(&self.index_start.to_le_bytes());
        out.extend_from_slice(&self.index_count.to_le_bytes());
        out.extend_from_slice(&self.vertex_count.to_le_bytes());
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.material_index.to_le_bytes());
        out.extend_from_slice(&self.first_skin_batch.to_le_bytes());
        out.extend_from_slice(&self.skin_batches_count.to_le_bytes());
        out.extend_from_slice(&self.surface_area_sqrt.to_le_bytes());
        out.extend_from_slice(&self.lod_proxy_id.to_le_bytes());
        out.extend_from_slice(&self.uv_density_u.to_le_bytes());
        out.extend_from_slice(&self.uv_density_v.to_le_bytes());
        out.extend_from_slice(&self.first_weight_index.to_le_bytes());
        out.extend_from_slice(&self.fade_out_dist.to_le_bytes());
        out.extend_from_slice(&self.material_lod_dist.to_le_bytes());
        out
    }
}
