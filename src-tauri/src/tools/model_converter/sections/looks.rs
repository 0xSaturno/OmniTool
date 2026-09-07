//! Look system: material table, look groups, per-look built data and the small
//! per-look BVH descriptor.

use crate::core::error::Result;
use byteorder::{ReadBytesExt, LE};
use std::io::Cursor;

pub const TAG_MATERIAL: u32 = 0x3250BB80;
pub const TAG_LOOK_GROUP: u32 = 0x4CCEA4AD;
pub const TAG_LOOK_BUILT: u32 = 0x811902D7;
pub const TAG_LOOK_BVH_INFO: u32 = 0xDF9FDF12;

/// One material slot, in the model's own slot order — this is what Model
/// Subset's `material_index` indexes.
#[derive(Debug, Clone)]
pub struct MaterialSlot {
    pub path_offset: u64,
    pub name_offset: u64,
}

/// One entry of the asset-id lookup table.
#[derive(Debug, Clone, Copy)]
pub struct MaterialId {
    /// 64-bit id of the referenced `.material`; zero for placeholder slots.
    pub asset_id: u64,
    /// `ihash` of the slot name **lowercased**.
    pub name_hash: u32,
    pub unk: u32,
}

/// Model Material is two arrays of `count` entries, not one array of pairs:
/// slot offsets first, then a 16-byte id record per slot.
///
/// `ids[i]` is the record for `slots[i]`, keyed by `ihash` of the slot name
/// **lowercased** — that lowercasing is the easy thing to miss. The table is
/// also sorted by that hash, so `id_for_name` binary-searches instead of
/// indexing: on the two shipped assets where the two disagree it declines
/// rather than handing back another slot's row.
pub struct MaterialSection {
    pub slots: Vec<MaterialSlot>,
    pub ids: Vec<MaterialId>,
}

impl MaterialSection {
    /// 16 bytes of offsets + 16 bytes of ids per slot.
    pub const ENTRY_SIZE: usize = 32;

    pub fn parse(data: &[u8]) -> Result<Self> {
        let count = data.len() / Self::ENTRY_SIZE;
        let tail = count * 16;
        let mut offs = Cursor::new(data);
        let mut ids_cur = Cursor::new(&data[tail..]);
        let mut slots = Vec::with_capacity(count);
        let mut ids = Vec::with_capacity(count);
        for _ in 0..count {
            slots.push(MaterialSlot {
                path_offset: offs.read_u64::<LE>()?,
                name_offset: offs.read_u64::<LE>()?,
            });
            ids.push(MaterialId {
                asset_id: ids_cur.read_u64::<LE>()?,
                name_hash: ids_cur.read_u32::<LE>()?,
                unk: ids_cur.read_u32::<LE>()?,
            });
        }
        Ok(Self { slots, ids })
    }

    /// Looks up the id record for a slot name. Pass the name as authored; the
    /// lowercasing the hash needs is done here.
    pub fn id_for_name(&self, name: &str) -> Option<MaterialId> {
        let want = crate::core::crc32::hash(&name.to_ascii_lowercase());
        self.ids
            .binary_search_by_key(&want, |e| e.name_hash)
            .ok()
            .map(|i| self.ids[i])
    }

    pub fn save(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.slots.len() * Self::ENTRY_SIZE);
        for s in &self.slots {
            out.extend_from_slice(&s.path_offset.to_le_bytes());
            out.extend_from_slice(&s.name_offset.to_le_bytes());
        }
        for e in &self.ids {
            out.extend_from_slice(&e.asset_id.to_le_bytes());
            out.extend_from_slice(&e.name_hash.to_le_bytes());
            out.extend_from_slice(&e.unk.to_le_bytes());
        }
        out
    }
}

/// A named set of looks. Offsets in the group records are relative to byte 1,
/// i.e. to the end of the leading count byte.
#[derive(Debug, Clone)]
pub struct LookGroup {
    pub name_hash: u32,
    pub unk: u32,
    pub looks: Vec<u16>,
}

pub fn parse_look_groups(data: &[u8]) -> Result<Vec<LookGroup>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    let count = data[0] as usize;
    let mut cur = Cursor::new(&data[1..]);
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let offset = cur.read_u64::<LE>()? as usize;
        let n = cur.read_u64::<LE>()? as usize;
        let name_hash = cur.read_u32::<LE>()?;
        let unk = cur.read_u32::<LE>()?;
        let mut looks = Vec::with_capacity(n);
        let base = 1 + offset;
        for i in 0..n {
            let o = base + i * 2;
            if o + 2 > data.len() {
                break;
            }
            looks.push(u16::from_le_bytes(data[o..o + 2].try_into().unwrap()));
        }
        out.push(LookGroup {
            name_hash,
            unk,
            looks,
        });
    }
    Ok(out)
}

/// Per-look record, 80 bytes, one per entry in Model Look. The offsets are
/// section-relative and delimit six u16 arrays.
#[derive(Debug, Clone)]
pub struct LookBuilt {
    pub offsets: [u64; 7],
    /// Element counts of the four optional arrays delimited by `offsets[0..4]`.
    pub sub_counts: [u16; 4],
    /// Number of entries in the array at `offsets[4]` — always the joint
    /// bounding-sphere count of the model for the "Default" look.
    pub bsphere_count: u32,
    /// `ihash` of the look name as authored, e.g. "Body_Default".
    pub name_hash: u32,
    /// `ihash` of the same name lowercased.
    pub name_hash_lower: u32,
    pub unk: u32,
    /// Indices into Model Joint Bspheres that this look keeps alive.
    pub bspheres: Vec<u16>,
    /// Bitmask form of `bspheres`, 8 u16 words = up to 128 spheres.
    pub bsphere_mask: Vec<u16>,
    pub sub_arrays: [Vec<u16>; 4],
}

pub struct LookBuiltSection {
    pub looks: Vec<LookBuilt>,
}

impl LookBuiltSection {
    pub const RECORD_SIZE: usize = 80;

    /// `look_count` comes from Model Look (its length / 32).
    pub fn parse(data: &[u8], look_count: usize) -> Result<Self> {
        let read_u16s = |from: u64, to: u64| -> Vec<u16> {
            let (a, b) = (from as usize, to as usize);
            if b <= a || b > data.len() {
                return Vec::new();
            }
            data[a..b]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect()
        };

        let mut looks = Vec::with_capacity(look_count);
        for r in 0..look_count {
            let base = r * Self::RECORD_SIZE;
            if base + Self::RECORD_SIZE > data.len() {
                break;
            }
            let mut cur = Cursor::new(&data[base..]);
            let mut offsets = [0u64; 7];
            for o in &mut offsets {
                *o = cur.read_u64::<LE>()?;
            }
            let mut sub_counts = [0u16; 4];
            for c in &mut sub_counts {
                *c = cur.read_u16::<LE>()?;
            }
            let bsphere_count = cur.read_u32::<LE>()?;
            let name_hash = cur.read_u32::<LE>()?;
            let name_hash_lower = cur.read_u32::<LE>()?;
            let unk = cur.read_u32::<LE>()?;

            let sub_arrays = [
                read_u16s(offsets[0], offsets[1]),
                read_u16s(offsets[1], offsets[2]),
                read_u16s(offsets[2], offsets[3]),
                read_u16s(offsets[3], offsets[4]),
            ];
            looks.push(LookBuilt {
                offsets,
                sub_counts,
                bsphere_count,
                name_hash,
                name_hash_lower,
                unk,
                bspheres: read_u16s(offsets[4], offsets[5]),
                bsphere_mask: read_u16s(offsets[5], offsets[6]),
                sub_arrays,
            });
        }
        Ok(Self { looks })
    }
}

/// Expands a Look Built bitmask back into sphere indices; the mask and the
/// explicit list are redundant, so this doubles as a validity check.
pub fn expand_mask(mask: &[u16]) -> Vec<u16> {
    let mut out = Vec::new();
    for (w, &word) in mask.iter().enumerate() {
        for bit in 0..16 {
            if word >> bit & 1 == 1 {
                out.push((w * 16 + bit) as u16);
            }
        }
    }
    out
}

/// 16 bytes per look. Only `node_count` and `depth` ever move between models.
#[derive(Debug, Clone, Copy)]
pub struct LookBvhInfo {
    pub unk0: u32,
    pub node_count: u32,
    pub unk2: u32,
    pub depth: u32,
}

pub fn parse_look_bvh_info(data: &[u8]) -> Result<Vec<LookBvhInfo>> {
    let mut cur = Cursor::new(data);
    let mut out = Vec::with_capacity(data.len() / 16);
    for _ in 0..data.len() / 16 {
        out.push(LookBvhInfo {
            unk0: cur.read_u32::<LE>()?,
            node_count: cur.read_u32::<LE>()?,
            unk2: cur.read_u32::<LE>()?,
            depth: cur.read_u32::<LE>()?,
        });
    }
    Ok(out)
}
