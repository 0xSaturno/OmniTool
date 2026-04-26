use byteorder::{LE, ReadBytesExt};
use std::io::Cursor;
use crate::core::error::Result;

pub const TAG_MESHES: u32 = 0x78D9CBDE;

#[derive(Debug, Clone)]
pub struct MeshDefinition {
    /// Per-subset object-space origin (added to each vertex position before scaling).
    pub obj_origin_x: f32,
    pub obj_origin_y: f32,
    pub obj_origin_z: f32,
    pub unk2: u16, pub unk3: u16, pub unk4: u16, pub unk5: u16,

    pub vertex_start: u32,
    pub index_start: u32,
    pub index_count: u32,
    pub vertex_count: u32,

    pub flags: u16,
    pub material_index: u16,
    pub first_skin_batch: u16,
    pub skin_batches_count: u16,

    pub unk9: u16, pub unk10: u16,
    pub unk11: f32, pub unk12: f32,

    pub first_weight_index: u32,
    pub unk3_last: u32,
}

impl MeshDefinition {
    pub fn get_flags(&self) -> u16 { self.flags }
    pub fn get_material(&self) -> u16 { self.material_index }
    pub fn is_skinned(&self) -> bool { (self.flags & 0x1) != 0 }
    pub fn is_rcra_skinned(&self) -> bool { (self.flags & 0x100) != 0 }
    pub fn has_relative_indices(&self) -> bool { (self.flags & 0x10) != 0 }

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
        let obj_origin_x = cur.read_f32::<LE>()?;
        let obj_origin_y = cur.read_f32::<LE>()?;
        let obj_origin_z = cur.read_f32::<LE>()?;
        let unk2 = cur.read_u16::<LE>()?;
        let unk3 = cur.read_u16::<LE>()?;
        let unk4 = cur.read_u16::<LE>()?;
        let unk5 = cur.read_u16::<LE>()?;
        // bytes 20..64
        let vertex_start = cur.read_u32::<LE>()?;
        let index_start  = cur.read_u32::<LE>()?;
        let index_count  = cur.read_u32::<LE>()?;
        let vertex_count = cur.read_u32::<LE>()?;
        let flags              = cur.read_u16::<LE>()?;
        let material_index     = cur.read_u16::<LE>()?;
        let first_skin_batch   = cur.read_u16::<LE>()?;
        let skin_batches_count = cur.read_u16::<LE>()?;
        let unk9  = cur.read_u16::<LE>()?;
        let unk10 = cur.read_u16::<LE>()?;
        let unk11 = cur.read_f32::<LE>()?;
        let unk12 = cur.read_f32::<LE>()?;
        let first_weight_index = cur.read_u32::<LE>()?;
        let unk3_last          = cur.read_u32::<LE>()?;

        Ok(Self {
            obj_origin_x, obj_origin_y, obj_origin_z, unk2, unk3, unk4, unk5,
            vertex_start, index_start, index_count, vertex_count,
            flags, material_index, first_skin_batch, skin_batches_count,
            unk9, unk10, unk11, unk12,
            first_weight_index, unk3_last,
        })
    }

    pub fn save_all(meshes: &[Self]) -> Vec<u8> {
        let mut out = Vec::with_capacity(meshes.len() * 64);
        for m in meshes { out.extend_from_slice(&m.save()); }
        out
    }

    pub fn save(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64);
        out.extend_from_slice(&self.obj_origin_x.to_le_bytes());
        out.extend_from_slice(&self.obj_origin_y.to_le_bytes());
        out.extend_from_slice(&self.obj_origin_z.to_le_bytes());
        out.extend_from_slice(&self.unk2.to_le_bytes());
        out.extend_from_slice(&self.unk3.to_le_bytes());
        out.extend_from_slice(&self.unk4.to_le_bytes());
        out.extend_from_slice(&self.unk5.to_le_bytes());
        out.extend_from_slice(&self.vertex_start.to_le_bytes());
        out.extend_from_slice(&self.index_start.to_le_bytes());
        out.extend_from_slice(&self.index_count.to_le_bytes());
        out.extend_from_slice(&self.vertex_count.to_le_bytes());
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.material_index.to_le_bytes());
        out.extend_from_slice(&self.first_skin_batch.to_le_bytes());
        out.extend_from_slice(&self.skin_batches_count.to_le_bytes());
        out.extend_from_slice(&self.unk9.to_le_bytes());
        out.extend_from_slice(&self.unk10.to_le_bytes());
        out.extend_from_slice(&self.unk11.to_le_bytes());
        out.extend_from_slice(&self.unk12.to_le_bytes());
        out.extend_from_slice(&self.first_weight_index.to_le_bytes());
        out.extend_from_slice(&self.unk3_last.to_le_bytes());
        out
    }
}
