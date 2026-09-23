//! Reader for the shipped `dag` — the game's dependency graph.
//!
//! `dag` sits next to `toc` in the game folder: a 12-byte prelude (magic,
//! payload size, one zero word) followed by an ordinary DAT1 container with six
//! sections. Five of them are parallel arrays, one entry per asset in the game:
//!
//! | Tag | Name | Contents |
//! |---|---|---|
//! | `0x933C0D32` | Asset Ids | `u64` asset id — **byte-transposed**, see below |
//! | `0x7A0266BC` | Asset Types | `u8` asset-type enum |
//! | `0xD101A6CC` | Asset Names | `u32` offset into the DAT1 string pool |
//! | `0xF958372E` | Dependency Links Heads | `i32` index into Dependency Links, `-1` = no deps |
//! | `0xBC91D1CC` | Dependency Links | `i32` runs, see [`Dag::deps`] |
//! | `0xBFEC699F` | LC Link Heads | `i32`, [`LC_GROUP`] per lighting-condition group |
//!
//! Measured on the shipped PC `dag`: 63,485 assets have a link head, the link
//! array holds 401,305 slots, and every one of them is reached by walking the
//! runs from the asset link heads and then from the lighting-condition link
//! heads — nothing in the block is left over.
//!
//! (The six names above are CRC-32 preimages of the tags, checked by
//! `section_names`; the layouts are what the shipped PC `dag` actually holds.)
//!
//! # The transposed asset-id block
//!
//! The asset-id block is not a plain `u64[]`. It is stored as eight planes of
//! `count` bytes each — every id's byte 0, then every id's byte 1, and so on —
//! which makes the block compress far better, and which the loader undoes in
//! one pass. So
//!
//! ```text
//! id[i] = Σ over k in 0..8 of  block[k * count + i] << (8 * k)
//! ```
//!
//! Reading the block as a `u64[]` gives garbage, which is why it has been
//! described elsewhere as uninitialised. It is not: on the shipped PC `dag`
//! (19,105,452 bytes, 169,245 assets) every de-transposed id reproduces the
//! CRC-64 of that entry's own name, and the ids come out in ascending order, so
//! the array is directly binary-searchable. `bin/dag_check` re-measures this.

use std::collections::HashMap;
use std::path::Path;

use crate::core::dat1::Dat1;
use crate::core::error::{Result, ToolkitError};

/// First word of the `dag` file, before the DAT1 container.
pub const DAG_MAGIC: u32 = 0xB8EF_3955;
/// Bytes of `dag` preceding the DAT1 container: magic, payload size, one zero.
pub const DAG_PRELUDE: usize = 12;

pub const TAG_ASSET_IDS: u32 = 0x933C_0D32;
pub const TAG_ASSET_TYPES: u32 = 0x7A02_66BC;
pub const TAG_ASSET_NAMES: u32 = 0xD101_A6CC;
pub const TAG_LINK_HEADS: u32 = 0xF958_372E;
pub const TAG_LINKS: u32 = 0xBC91_D1CC;
pub const TAG_LC_LINK_HEADS: u32 = 0xBFEC_699F;

/// Lighting-condition link heads come in fixed groups of this size.
///
/// Measured: the shipped PC `dag` has 12,900 of them and 2,580 distinct
/// lighting-condition markers in the link runs, and every marker's payload is a
/// multiple of five in `0..12_900`.
pub const LC_GROUP: usize = 5;

/// Undo the build-time byte transposition of a `u64` array.
///
/// `block` must be `8 * count` bytes: plane `k` holds byte `k` of every entry,
/// least-significant plane first.
pub fn detranspose_u64(block: &[u8]) -> Vec<u64> {
    let count = block.len() / 8;
    let mut out = vec![0u64; count];
    for k in 0..8 {
        let plane = &block[k * count..(k + 1) * count];
        let shift = 8 * k as u32;
        for (o, &b) in out.iter_mut().zip(plane) {
            *o |= (b as u64) << shift;
        }
    }
    out
}

/// Read a `u64` array the naive way, for comparison against [`detranspose_u64`].
pub fn plain_u64(block: &[u8]) -> Vec<u64> {
    block
        .chunks_exact(8)
        .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn read_i32s(block: &[u8]) -> Vec<i32> {
    block
        .chunks_exact(4)
        .map(|c| i32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn read_u32s(block: &[u8]) -> Vec<u32> {
    block
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

/// One decoded dependency-graph node.
#[derive(Debug, Clone, Copy)]
pub struct DagNode {
    pub id: u64,
    pub asset_type: u8,
    pub name_offset: u32,
    pub link_head: i32,
}

pub struct Dag {
    dat1: Dat1,
    /// Asset ids, de-transposed. Ascending on every shipped `dag` we have seen.
    pub ids: Vec<u64>,
    /// Asset-type enum, one byte per asset.
    pub types: Vec<u8>,
    /// String-pool offsets of the asset paths, one per asset.
    pub name_offsets: Vec<u32>,
    /// Start index into [`Dag::links`], or `-1` when the asset has no deps.
    pub link_heads: Vec<i32>,
    /// Dependency runs; see [`Dag::deps`].
    pub links: Vec<i32>,
    /// Lighting-condition link heads, [`LC_GROUP`] per group.
    pub lc_link_heads: Vec<i32>,
    /// Whether [`Dag::ids`] is strictly ascending, i.e. binary-searchable.
    pub ids_ascending: bool,
    /// Built only when `ids` is not ascending, so lookups stay O(1) either way.
    fallback: Option<HashMap<u64, usize>>,
}

impl Dag {
    /// Read `<game_dir>/dag`.
    pub fn load(game_dir: &Path) -> Result<Self> {
        let path = game_dir.join("dag");
        let bytes = std::fs::read(&path)?;
        Self::parse(&bytes)
    }

    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < DAG_PRELUDE {
            return Err(ToolkitError::Parse("dag file too small".into()));
        }
        let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
        if magic != DAG_MAGIC {
            return Err(ToolkitError::InvalidMagic { expected: DAG_MAGIC, got: magic });
        }
        let declared = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        if declared != data.len() - DAG_PRELUDE {
            return Err(ToolkitError::Parse(format!(
                "dag payload size {declared} does not match file ({} bytes after the prelude)",
                data.len() - DAG_PRELUDE
            )));
        }
        let dat1 = Dat1::parse(&data[DAG_PRELUDE..])?;

        let section = |tag: u32| -> Result<&[u8]> {
            dat1.get_section_data(tag).ok_or(ToolkitError::SectionNotFound(tag))
        };

        let id_block = section(TAG_ASSET_IDS)?;
        if id_block.len() % 8 != 0 {
            return Err(ToolkitError::Parse(format!(
                "dag asset-id block is {} bytes, not a multiple of 8",
                id_block.len()
            )));
        }
        let ids = detranspose_u64(id_block);
        let count = ids.len();

        let types = section(TAG_ASSET_TYPES)?.to_vec();
        let name_offsets = read_u32s(section(TAG_ASSET_NAMES)?);
        let link_heads = read_i32s(section(TAG_LINK_HEADS)?);
        let links = read_i32s(section(TAG_LINKS)?);
        // Optional: absent from some builds, so a missing section is not fatal.
        let lc_link_heads = dat1
            .get_section_data(TAG_LC_LINK_HEADS)
            .map(read_i32s)
            .unwrap_or_default();

        for (what, got) in [
            ("types", types.len()),
            ("names", name_offsets.len()),
            ("link heads", link_heads.len()),
        ] {
            if got != count {
                return Err(ToolkitError::Parse(format!(
                    "dag {what} has {got} entries but there are {count} asset ids"
                )));
            }
        }

        let ids_ascending = ids.windows(2).all(|w| w[0] < w[1]);
        let fallback = (!ids_ascending)
            .then(|| ids.iter().enumerate().map(|(i, &id)| (id, i)).collect());

        Ok(Self {
            dat1,
            ids,
            types,
            name_offsets,
            link_heads,
            links,
            lc_link_heads,
            ids_ascending,
            fallback,
        })
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// The raw, still-transposed asset-id block, for harnesses that want to
    /// compare decodings.
    pub fn raw_id_block(&self) -> &[u8] {
        self.dat1.get_section_data(TAG_ASSET_IDS).unwrap_or(&[])
    }

    /// Path of asset `i`, exactly as the dag stores it (mixed case, backslash
    /// separators). Normalise with [`crate::core::crc64::normalize_path`]
    /// before comparing against anything.
    pub fn name(&self, i: usize) -> Option<&str> {
        self.str_at(*self.name_offsets.get(i)?)
    }

    fn str_at(&self, raw_offset: u32) -> Option<&str> {
        // Same convention as `Dat1::get_string`: the stored offset is relative
        // to the start of the container, so the header has to come off it.
        let header_end = self.dat1.header_end();
        let raw = raw_offset as usize;
        let off = if raw >= header_end { raw - header_end } else { raw };
        let pool = &self.dat1.strings_pool;
        if off >= pool.len() {
            return None;
        }
        let end = pool[off..]
            .iter()
            .position(|&b| b == 0)
            .map(|p| off + p)
            .unwrap_or(pool.len());
        std::str::from_utf8(&pool[off..end]).ok()
    }

    pub fn node(&self, i: usize) -> Option<DagNode> {
        Some(DagNode {
            id: *self.ids.get(i)?,
            asset_type: self.types[i],
            name_offset: self.name_offsets[i],
            link_head: self.link_heads[i],
        })
    }

    /// Index of an asset id, in O(log n) when the ids are ascending.
    pub fn index_of(&self, id: u64) -> Option<usize> {
        if let Some(map) = &self.fallback {
            return map.get(&id).copied();
        }
        self.ids.binary_search(&id).ok()
    }

    pub fn name_of_id(&self, id: u64) -> Option<&str> {
        self.name(self.index_of(id)?)
    }

    /// Direct, unconditional dependencies of asset `i`, as node indices.
    ///
    /// A link head points at the first slot of a run in [`Dag::links`]; the run
    /// is a list of node indices and ends at the first negative value. Plain
    /// `-1` is just the terminator. Any *other* negative value is a
    /// lighting-condition marker — see [`Dag::lc_deps`] — and also ends the
    /// unconditional list, so it is never reported as an edge here.
    pub fn deps(&self, i: usize) -> Vec<usize> {
        match self.link_heads.get(i) {
            Some(&head) if head >= 0 => self.run(head as usize),
            _ => Vec::new(),
        }
    }

    /// Walk one run of [`Dag::links`] starting at `start`, stopping at the
    /// first negative value.
    pub fn run(&self, start: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut j = start;
        while let Some(&v) = self.links.get(j) {
            if v < 0 {
                break;
            }
            if (v as usize) < self.ids.len() {
                out.push(v as usize);
            }
            j += 1;
        }
        out
    }

    /// The lighting-condition marker that ends asset `i`'s run, if it has one.
    pub fn lc_marker(&self, i: usize) -> Option<i32> {
        let head = *self.link_heads.get(i)?;
        if head < 0 {
            return None;
        }
        let mut j = head as usize;
        while let Some(&v) = self.links.get(j) {
            if v < 0 {
                return (v != -1).then_some(v);
            }
            j += 1;
        }
        None
    }

    /// Dependencies of asset `i` that only exist under some lighting
    /// conditions, as `(condition slot, node indices)`.
    ///
    /// Measured on `tile_q18_light_grid.zone` in Sargasso: its run is a bare
    /// marker, its marker's group is `[-1, 923, 925, -1, -1]`, and slots 923
    /// and 925 start runs holding `tile_q18_light_grid_overcast.zonelightbin`
    /// and `..._sunset.zonelightbin` — one baked light bin per condition.
    pub fn lc_deps(&self, i: usize) -> Vec<(usize, Vec<usize>)> {
        let Some(marker) = self.lc_marker(i) else {
            return Vec::new();
        };
        let Some(group) = self.lc_group(marker) else {
            return Vec::new();
        };
        group
            .iter()
            .enumerate()
            .filter(|(_, &h)| h >= 0)
            .map(|(slot, &h)| (slot, self.run(h as usize)))
            .collect()
    }

    /// The [`LC_GROUP`] lighting-condition link heads a marker value selects.
    pub fn lc_group(&self, marker: i32) -> Option<&[i32]> {
        if marker >= -1 {
            return None;
        }
        let base = (marker as u32 & 0x7FFF_FFFF) as usize;
        self.lc_link_heads.get(base..base + LC_GROUP)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Transpose a `u64[]` the way the build step does, then decode it back.
    #[test]
    fn detranspose_round_trip() {
        let ids: Vec<u64> = (0..64u64).map(|i| 0x8000_0000_0000_0000 | i.wrapping_mul(0x9E37_79B9_7F4A_7C15)).collect();
        let count = ids.len();
        let mut block = vec![0u8; count * 8];
        for (i, id) in ids.iter().enumerate() {
            for k in 0..8 {
                block[k * count + i] = (id >> (8 * k)) as u8;
            }
        }
        assert_eq!(detranspose_u64(&block), ids);
        // And the naive reading must not accidentally agree.
        assert_ne!(plain_u64(&block), ids);
    }

    #[test]
    fn detranspose_handles_empty() {
        assert!(detranspose_u64(&[]).is_empty());
    }
}
