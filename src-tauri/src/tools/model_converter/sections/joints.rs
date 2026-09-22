use byteorder::{LE, ReadBytesExt};
use std::io::Cursor;
use crate::core::error::Result;

pub const TAG_JOINTS:           u32 = 0x15DF9D3B;
pub const TAG_JOINTS_TRANSFORM: u32 = 0xDCC88A19;

/// Joint flags; bit 0 is named by elimination, the others are corpus-verified.
pub mod joint_flags {
    pub const SEGMENT_SCALE_COMPENSATE: u16 = 1 << 0;
    /// Left side of a Mirror Ids pair.
    pub const MIRROR_PAIRED_LEFT: u16 = 1 << 1;
    pub const MIRROR_PAIRED_RIGHT: u16 = 1 << 2;
    /// Maya end joint (the `*_end` tips).
    pub const END_JOINT: u16 = 1 << 3;
    /// Drives or collides with cloth (Cloth Meta Data influence joints).
    pub const CLOTH_JOINT: u16 = 1 << 4;
    /// A later joint shares this one's parent; never set on root-level joints.
    pub const HAS_NEXT_SIBLING: u16 = 1 << 5;
}

#[derive(Debug, Clone)]
pub struct Joint {
    pub parent: i16,
    pub index: u16,
    /// Descendants in depth-first order; the next sibling sits at index + 1 + this.
    pub subtree_count: u16,
    pub flags: u16,
    pub hash: u32,
    pub string_offset: u32,
}

impl Joint {
    pub fn parse_all(data: &[u8]) -> Result<Vec<Self>> {
        const ENTRY_SIZE: usize = 16;
        let count = data.len() / ENTRY_SIZE;
        let mut joints = Vec::with_capacity(count);
        let mut cur = Cursor::new(data);
        for _ in 0..count {
            joints.push(Self {
                parent:        cur.read_i16::<LE>()?,
                index:         cur.read_u16::<LE>()?,
                subtree_count: cur.read_u16::<LE>()?,
                flags:         cur.read_u16::<LE>()?,
                hash:          cur.read_u32::<LE>()?,
                string_offset: cur.read_u32::<LE>()?,
            });
        }
        Ok(joints)
    }
}

/// Model Bind Pose: per joint a local (scale.xyz0, rotation quat, translation.xyz0), then inverse bind 4x4s at the next 64-byte boundary.
pub struct JointsTransform {
    pub matrixes34: Vec<[f32; 12]>,
    pub matrixes44: Vec<[f32; 16]>,
}

impl JointsTransform {
    pub fn parse(data: &[u8]) -> Result<Self> {
        const E1: usize = 12 * 4; // 48 bytes per 3x4 matrix
        const E2: usize = 16 * 4; // 64 bytes per 4x4 matrix

        let count = data.len() / (E1 + E2);
        let mut cur = Cursor::new(data);
        let mut m34 = Vec::with_capacity(count);
        for _ in 0..count {
            let mut m = [0f32; 12];
            for f in &mut m { *f = cur.read_f32::<LE>()?; }
            m34.push(m);
        }

        let offset34 = E1 * count;
        let align = offset34 % E2;
        let offset44 = if align != 0 { offset34 + E2 - align } else { offset34 };
        cur.set_position(offset44 as u64);

        let mut m44 = Vec::with_capacity(count);
        for _ in 0..count {
            let mut m = [0f32; 16];
            for f in &mut m { *f = cur.read_f32::<LE>()?; }
            m44.push(m);
        }

        Ok(Self { matrixes34: m34, matrixes44: m44 })
    }

    pub fn get_scale(&self, index: usize) -> (f32, f32, f32) {
        let m = &self.matrixes34[index];
        (m[0], m[1], m[2])
    }

    pub fn get_position(&self, index: usize) -> (f32, f32, f32) {
        let m = &self.matrixes34[index];
        (m[8], m[9], m[10])
    }

    pub fn get_quaternion(&self, index: usize) -> (f32, f32, f32, f32) {
        let m = &self.matrixes34[index];
        (m[4], m[5], m[6], m[7])
    }
}
