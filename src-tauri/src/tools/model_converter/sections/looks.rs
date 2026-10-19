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
    /// Per-slot material flags; 0 on 99.4% of slots, otherwise 0x40, 0x1, 0x8, 0x70 or 0x200.
    pub flags: u32,
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
                flags: ids_cur.read_u32::<LE>()?,
            });
        }
        Ok(Self { slots, ids })
    }

    /// Inserts a slot keeping the table sorted by name hash. Returns the old->new slot index map.
    pub fn insert_slot(&mut self, path_offset: u32, name_offset: u32, asset_id: u64, name: &str) -> Vec<u16> {
        let name_hash = crate::core::crc32::hash(&name.to_ascii_lowercase());
        let at = self.ids.partition_point(|e| e.name_hash < name_hash);
        self.slots.insert(at, MaterialSlot { path_offset: path_offset as u64, name_offset: name_offset as u64 });
        self.ids.insert(at, MaterialId { asset_id, name_hash, flags: 0 });
        (0..self.slots.len() as u16 - 1).map(|i| if (i as usize) < at { i } else { i + 1 }).collect()
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
            out.extend_from_slice(&e.flags.to_le_bytes());
        }
        out
    }
}

/// A named set of looks: `u8 count`, then 24-byte records
/// `{u64 indices_offset, u16 look_count, u8 pad[6], u32 name_hash, u32 name_offset}`.
/// `indices_offset` is relative to byte 1, the end of the count byte.
#[derive(Debug, Clone)]
pub struct LookGroup {
    pub name_hash: u32,
    pub name_offset: u32,
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
        let n = cur.read_u16::<LE>()? as usize;
        cur.set_position(cur.position() + 6);
        let name_hash = cur.read_u32::<LE>()?;
        let name_offset = cur.read_u32::<LE>()?;
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
            name_offset,
            looks,
        });
    }
    Ok(out)
}

/// Per-look record, 80 bytes, one per entry in Model Look: seven section-relative
/// offsets, five u16 counts, 2 pad bytes, the cased and lowercased name hashes
/// and the name's string offset.
#[derive(Debug, Clone)]
pub struct LookBuilt {
    pub offsets: [u64; 7],
    pub rigid_body_count: u16,
    pub cloth_count: u16,
    pub cloth_joint_count: u16,
    pub cloth_collidable_count: u16,
    pub joint_bsphere_count: u16,
    /// `ihash` of the look name as authored, e.g. "Body_Default".
    pub name_hash: u32,
    /// `ihash` of the same name lowercased.
    pub name_hash_lower: u32,
    pub name_offset: u32,
    /// Ragdoll rigid bodies this look enables (Ragdoll Meta Data indices).
    pub rigid_bodies: Vec<u16>,
    /// Cloth instances this look enables.
    pub cloths: Vec<u16>,
    /// Cloth influence joints this look enables (Cloth Meta Data indices).
    pub cloth_joints: Vec<u16>,
    pub cloth_collidables: Vec<u16>,
    /// Indices into Model Joint Bspheres that this look keeps alive.
    pub bspheres: Vec<u16>,
    /// Bitfield form of `bspheres`, read as u16 words.
    pub bsphere_mask: Vec<u16>,
    /// Six 256-byte bitfields, one per LOD: bit i = subset i belongs to this look at that LOD.
    pub lod_subset_bits: Vec<u8>,
}

impl LookBuilt {
    /// Subsets flagged in this look's bitfield for one LOD.
    pub fn lod_subsets(&self, lod: usize) -> Vec<u16> {
        let Some(bits) = self.lod_subset_bits.get(lod * 256..(lod + 1) * 256) else {
            return Vec::new();
        };
        (0..2048u16)
            .filter(|&i| bits[i as usize / 8] >> (i % 8) & 1 == 1)
            .collect()
    }
}

pub struct LookBuiltSection {
    pub looks: Vec<LookBuilt>,
}

impl LookBuiltSection {
    pub const RECORD_SIZE: usize = 80;
    pub const LOD_BITFIELD_SIZE: usize = 6 * 256;

    /// `look_count` comes from Model Look (its length / 32).
    pub fn parse(data: &[u8], look_count: usize) -> Result<Self> {
        let read_u16s = |from: u64, count: usize| -> Vec<u16> {
            let a = from as usize;
            let b = a + count * 2;
            if b > data.len() {
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
            let rigid_body_count = cur.read_u16::<LE>()?;
            let cloth_count = cur.read_u16::<LE>()?;
            let cloth_joint_count = cur.read_u16::<LE>()?;
            let cloth_collidable_count = cur.read_u16::<LE>()?;
            let joint_bsphere_count = cur.read_u16::<LE>()?;
            cur.read_u16::<LE>()?;
            let name_hash = cur.read_u32::<LE>()?;
            let name_hash_lower = cur.read_u32::<LE>()?;
            let name_offset = cur.read_u32::<LE>()?;

            let mask_words = offsets[6].saturating_sub(offsets[5]) as usize / 2;
            let lod_start = offsets[6] as usize;
            let lod_subset_bits = data
                .get(lod_start..lod_start + Self::LOD_BITFIELD_SIZE)
                .map(|s| s.to_vec())
                .unwrap_or_default();
            looks.push(LookBuilt {
                offsets,
                rigid_body_count,
                cloth_count,
                cloth_joint_count,
                cloth_collidable_count,
                joint_bsphere_count,
                name_hash,
                name_hash_lower,
                name_offset,
                rigid_bodies: read_u16s(offsets[0], rigid_body_count as usize),
                cloths: read_u16s(offsets[1], cloth_count as usize),
                cloth_joints: read_u16s(offsets[2], cloth_joint_count as usize),
                cloth_collidables: read_u16s(offsets[3], cloth_collidable_count as usize),
                bspheres: read_u16s(offsets[4], joint_bsphere_count as usize),
                bsphere_mask: read_u16s(offsets[5], mask_words),
                lod_subset_bits,
            });
        }
        Ok(Self { looks })
    }
}

/// Rewrites every look's six LOD subset bitfields from its (first, count) LOD ranges.
pub fn write_lod_subset_bits(section: &mut [u8], look_ranges: &[Vec<(u16, u16)>]) -> Result<()> {
    for (look, lods) in look_ranges.iter().enumerate() {
        let rec = look * LookBuiltSection::RECORD_SIZE;
        let Some(off) = section.get(rec + 48..rec + 56).map(|b| u64::from_le_bytes(b.try_into().unwrap()) as usize) else {
            continue;
        };
        let Some(bits) = section.get_mut(off..off + LookBuiltSection::LOD_BITFIELD_SIZE) else {
            continue;
        };
        bits.fill(0);
        for (lod, &(first, count)) in lods.iter().take(6).enumerate() {
            for s in first as usize..first as usize + count as usize {
                if s >= 2048 {
                    return Err(crate::core::error::ToolkitError::Parse(format!(
                        "look {look} references subset {s}; Look Built bitfields hold at most 2048"
                    )));
                }
                bits[lod * 256 + s / 8] |= 1 << (s % 8);
            }
        }
    }
    Ok(())
}

/// Rewrites Look Built's LOD subset bitfields from Model Look's current ranges.
pub fn sync_lod_subset_bits(dat1: &mut crate::core::dat1::Dat1) -> Result<()> {
    let Some(look) = dat1.get_section_data(super::look::TAG_LOOK) else { return Ok(()) };
    let ranges: Vec<Vec<(u16, u16)>> = super::look::LookSection::parse(look)?
        .looks
        .iter()
        .map(|l| l.lods.iter().take(6).map(|r| (r.start, r.count)).collect())
        .collect();
    let Some(built) = dat1.get_section_data(TAG_LOOK_BUILT) else { return Ok(()) };
    let mut built = built.to_vec();
    write_lod_subset_bits(&mut built, &ranges)?;
    dat1.set_section_data(TAG_LOOK_BUILT, built)
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

/// Ray-tracing uses a look's BVH for (bits of `LookBvhInfo::usage`).
pub mod bvh_usage {
    pub const SKIP_IF_IMPOSTORED: u32 = 1 << 0;
    pub const AMBIENT_OCCLUSION: u32 = 1 << 1;
    pub const AMBIENT_SHADOWS: u32 = 1 << 2;
    pub const REFLECTIONS: u32 = 1 << 3;
    pub const SHADOWS: u32 = 1 << 4;
    pub const BLOCKER: u32 = 1 << 5;
    pub const DECAL: u32 = 1 << 6;
    pub const SKIP_CHECKERBOARD: u32 = 1 << 29;
    pub const USER: u32 = 1 << 30;
}

/// 16 bytes per look. `offset` and `size` are filled in at load time and are 0 on disk.
#[derive(Debug, Clone, Copy)]
pub struct LookBvhInfo {
    pub offset: u32,
    pub usage: u32,
    pub size: u32,
    /// LOD the BVH is built from.
    pub lod: u32,
}

pub fn parse_look_bvh_info(data: &[u8]) -> Result<Vec<LookBvhInfo>> {
    let mut cur = Cursor::new(data);
    let mut out = Vec::with_capacity(data.len() / 16);
    for _ in 0..data.len() / 16 {
        out.push(LookBvhInfo {
            offset: cur.read_u32::<LE>()?,
            usage: cur.read_u32::<LE>()?,
            size: cur.read_u32::<LE>()?,
            lod: cur.read_u32::<LE>()?,
        });
    }
    Ok(out)
}
