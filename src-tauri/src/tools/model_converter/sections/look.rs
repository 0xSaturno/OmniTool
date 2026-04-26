use byteorder::{LE, ReadBytesExt};
use std::io::Cursor;
use crate::core::error::Result;

pub const TAG_LOOK: u32 = 0x06EB7EFC;

#[derive(Debug, Clone, Copy)]
pub struct Lod { pub start: u16, pub count: u16 }

#[derive(Debug, Clone)]
pub struct Look { pub lods: Vec<Lod> }

pub struct LookSection {
    pub looks: Vec<Look>,
    pub raw: Vec<u8>,
}

impl LookSection {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let lods_per_look = if data.len() == 16 { 4usize } else { 8usize };
        let look_size = lods_per_look * 4; // each LOD = 4 bytes (2xu16)
        let count = data.len() / look_size;
        let mut cur = Cursor::new(data);
        let mut looks = Vec::with_capacity(count);
        for _ in 0..count {
            let mut lods = Vec::with_capacity(lods_per_look);
            for _ in 0..lods_per_look {
                lods.push(Lod {
                    start: cur.read_u16::<LE>()?,
                    count: cur.read_u16::<LE>()?,
                });
            }
            looks.push(Look { lods });
        }
        Ok(Self { looks, raw: data.to_vec() })
    }

    pub fn save(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for look in &self.looks {
            for lod in &look.lods {
                out.extend_from_slice(&lod.start.to_le_bytes());
                out.extend_from_slice(&lod.count.to_le_bytes());
            }
        }
        out
    }
}
