//! Skeleton-side sections: locators, hash lookup tables, joint bounding
//! spheres, mirror/leaf id lists and the small joint-hierarchy header.

use crate::core::error::Result;
use byteorder::{ReadBytesExt, LE};
use std::io::Cursor;

pub const TAG_LOCATOR: u32 = 0x9F614FAB;
pub const TAG_LOCATOR_LOOKUP: u32 = 0x731CBC2E;
pub const TAG_JOINT_LOOKUP: u32 = 0xEE31971C;
pub const TAG_JOINT_BSPHERES: u32 = 0x0AD3A708;
pub const TAG_MIRROR_IDS: u32 = 0xC5354B60;
pub const TAG_LEAF_IDS: u32 = 0xB7380E8C;
pub const TAG_JOINT_HIERARCHY: u32 = 0x90CDB60C;

/// A named attach point. 64 bytes: hash, name offset, parent joint, pad, then a
/// parent-relative 3x3 rotation/scale followed by the local position.
#[derive(Debug, Clone)]
pub struct Locator {
    pub hash: u32,
    pub string_offset: u32,
    /// Joint this locator hangs off, or `None` when stored as -1.
    pub joint: Option<u32>,
    pub pad: u32,
    /// `[0..9]` rotation/scale rows, `[9..12]` position.
    pub transform: [f32; 12],
}

impl Locator {
    pub const ENTRY_SIZE: usize = 64;

    pub fn parse_all(data: &[u8]) -> Result<Vec<Self>> {
        let mut cur = Cursor::new(data);
        let mut out = Vec::with_capacity(data.len() / Self::ENTRY_SIZE);
        for _ in 0..data.len() / Self::ENTRY_SIZE {
            let hash = cur.read_u32::<LE>()?;
            let string_offset = cur.read_u32::<LE>()?;
            let joint = cur.read_u32::<LE>()?;
            let pad = cur.read_u32::<LE>()?;
            let mut transform = [0f32; 12];
            for f in &mut transform {
                *f = cur.read_f32::<LE>()?;
            }
            out.push(Self {
                hash,
                string_offset,
                joint: (joint != u32::MAX).then_some(joint),
                pad,
                transform,
            });
        }
        Ok(out)
    }

    pub fn save_all(items: &[Self]) -> Vec<u8> {
        let mut out = Vec::with_capacity(items.len() * Self::ENTRY_SIZE);
        for l in items {
            out.extend_from_slice(&l.hash.to_le_bytes());
            out.extend_from_slice(&l.string_offset.to_le_bytes());
            out.extend_from_slice(&l.joint.unwrap_or(u32::MAX).to_le_bytes());
            out.extend_from_slice(&l.pad.to_le_bytes());
            for f in &l.transform {
                out.extend_from_slice(&f.to_le_bytes());
            }
        }
        out
    }
}

/// Sorted (hash, index) pairs — the same shape backs Model Joint Lookup and
/// Model Locator Lookup, and the engine binary-searches both.
#[derive(Debug, Clone)]
pub struct HashLookup {
    pub entries: Vec<(u32, u32)>,
}

impl HashLookup {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(data);
        let mut entries = Vec::with_capacity(data.len() / 8);
        for _ in 0..data.len() / 8 {
            entries.push((cur.read_u32::<LE>()?, cur.read_u32::<LE>()?));
        }
        Ok(Self { entries })
    }

    pub fn get(&self, hash: u32) -> Option<u32> {
        self.entries
            .binary_search_by_key(&hash, |e| e.0)
            .ok()
            .map(|i| self.entries[i].1)
    }

    pub fn save(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.entries.len() * 8);
        for (h, i) in &self.entries {
            out.extend_from_slice(&h.to_le_bytes());
            out.extend_from_slice(&i.to_le_bytes());
        }
        out
    }
}

/// One culling / hit sphere bound to a joint.
#[derive(Debug, Clone, Copy)]
pub struct JointBsphere {
    pub center: (f32, f32, f32),
    pub radius: f32,
    /// Stored as a byte offset into the 4x4 half of Model Bind Pose.
    pub joint: u32,
}

/// `u32 count; u8 pad[12]; JointBsphere[count]` — 20 bytes per sphere.
#[derive(Debug, Clone, Default)]
pub struct JointBspheres {
    pub spheres: Vec<JointBsphere>,
}

impl JointBspheres {
    pub const HEADER_SIZE: usize = 16;
    pub const ENTRY_SIZE: usize = 20;
    /// Stride of a Model Bind Pose 4x4 matrix, the unit `joint` is measured in.
    pub const JOINT_STRIDE: u32 = 64;

    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < Self::HEADER_SIZE {
            return Ok(Self::default());
        }
        let mut cur = Cursor::new(data);
        let count = cur.read_u32::<LE>()? as usize;
        cur.set_position(Self::HEADER_SIZE as u64);
        let available = (data.len() - Self::HEADER_SIZE) / Self::ENTRY_SIZE;
        let mut spheres = Vec::with_capacity(count.min(available));
        for _ in 0..count.min(available) {
            let center = (
                cur.read_f32::<LE>()?,
                cur.read_f32::<LE>()?,
                cur.read_f32::<LE>()?,
            );
            spheres.push(JointBsphere {
                center,
                radius: cur.read_f32::<LE>()?,
                joint: cur.read_u32::<LE>()?,
            });
        }
        Ok(Self { spheres })
    }

    pub fn joint_index(&self, i: usize) -> u32 {
        self.spheres[i].joint / Self::JOINT_STRIDE
    }

    pub fn save(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::HEADER_SIZE + self.spheres.len() * Self::ENTRY_SIZE);
        out.extend_from_slice(&(self.spheres.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0u8; 12]);
        for s in &self.spheres {
            out.extend_from_slice(&s.center.0.to_le_bytes());
            out.extend_from_slice(&s.center.1.to_le_bytes());
            out.extend_from_slice(&s.center.2.to_le_bytes());
            out.extend_from_slice(&s.radius.to_le_bytes());
            out.extend_from_slice(&s.joint.to_le_bytes());
        }
        out
    }
}

/// How a mirror pair is mirrored (`MirrorId::kind`).
pub mod mirror_kind {
    /// Copy rotation, flip translation.
    pub const PAIRED: u8 = 0;
    /// Rotate 180° about X, flip translation.
    pub const PAIRED_ATTACH: u8 = 1;
    /// A pair whose parent is an unpaired centre joint (clavicles, hips).
    pub const PAIRED_LINK: u8 = 2;
    /// Centre joint mirrored onto itself.
    pub const UNPAIRED: u8 = 3;
}

/// One left/right pairing. Only the canonical side of each pair is stored, so
/// the list is shorter than the joint table.
#[derive(Debug, Clone, Copy)]
pub struct MirrorId {
    pub joint: u16,
    pub kind: u8,
    pub mirror_joint: u16,
}

/// `u16 joint | kind << 12; u16 mirror_joint` per entry.
pub fn parse_mirror_ids(data: &[u8]) -> Result<Vec<MirrorId>> {
    let mut cur = Cursor::new(data);
    let mut out = Vec::with_capacity(data.len() / 4);
    for _ in 0..data.len() / 4 {
        let packed = cur.read_u16::<LE>()?;
        out.push(MirrorId {
            joint: packed & 0x0FFF,
            kind: (packed >> 12) as u8,
            mirror_joint: cur.read_u16::<LE>()?,
        });
    }
    Ok(out)
}

/// Model Leaf Ids — a flat u16 list of joints with no children.
pub fn parse_leaf_ids(data: &[u8]) -> Result<Vec<u16>> {
    let mut cur = Cursor::new(data);
    let mut out = Vec::with_capacity(data.len() / 2);
    for _ in 0..data.len() / 2 {
        out.push(cur.read_u16::<LE>()?);
    }
    Ok(out)
}

/// The 80-byte Model Joint Hierarchy block: counts, the spline-model rig
/// description, then zero padding.
#[derive(Debug, Clone, Copy, Default)]
pub struct JointHierarchy {
    /// Set only for spline-model rigs; 0 on every shipped model.
    pub flags: u16,
    pub joint_count: u16,
    /// Entries in Model Mirror Ids — *not* the locator count. Those two happen
    /// to be equal on hero_ratchet, which is how this was first misread.
    pub mirror_count: u16,
    pub leaf_count: u16,
    /// 0xFFFF when the rig is not a spline model.
    pub spline_root: u16,
    pub spline_joint_count: u16,
    pub spline_radius: f32,
}

impl JointHierarchy {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 16 {
            return Ok(Self::default());
        }
        let mut cur = Cursor::new(data);
        Ok(Self {
            flags: cur.read_u16::<LE>()?,
            joint_count: cur.read_u16::<LE>()?,
            mirror_count: cur.read_u16::<LE>()?,
            leaf_count: cur.read_u16::<LE>()?,
            spline_root: cur.read_u16::<LE>()?,
            spline_joint_count: cur.read_u16::<LE>()?,
            spline_radius: cur.read_f32::<LE>()?,
        })
    }
}
