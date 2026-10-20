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
//! which makes the block compress far better, and which the game undoes in
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
//!
//! # Walking the links
//!
//! Two readings of the link array were written independently and they agree:
//! a run of node indices, terminated by a negative value. `-1` is the plain
//! terminator; any other negative value has bit 31 set and its low bits are a
//! base into LC Link Heads, where [`LC_GROUP`] slots select one continuation
//! per lighting condition ([`LightingCondition`], in that order). Both
//! spellings are kept here because they answer different questions:
//!
//! * [`Dag::deps`] / [`Dag::lc_deps`] report the unconditional edges and the
//!   conditional ones separately, which is what `bin/dag_check` audits (every
//!   slot of the link array is claimed exactly once that way); and
//! * [`Dag::direct_dependencies`] / [`Dag::dependencies`] follow the game and
//!   walk *through* the indirection for one condition, or the union of all of
//!   them, which is what the asset-reference and extraction commands want.
//!
//! The group order was pinned from the other side too: `tile_q18_light_grid`'s
//! marker group is `[-1, 923, 925, -1, -1]`, and slots 1 and 2 hold that tile's
//! `_overcast` and `_sunset` `.zonelightbin`s — i.e. slot 1 is
//! [`LightingCondition::Overcast`] and slot 2 is [`LightingCondition::Sunset`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::core::dat1::{Dat1, DAT1_MAGIC};
use crate::core::error::{Result, ToolkitError};

/// First word of the `dag` file, before the DAT1 container.
pub const DAG_MAGIC: u32 = 0xB8EF_3955;
/// The DAT1 type word of the container inside `dag`.
pub const DAG_VERSION: u32 = 0x2A07_7A51;
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
/// multiple of five in `0..12_900`. It is also exactly
/// `LightingCondition::ALL.len()`.
///
/// Of those 12,900 heads, 10,577 are `-1` and 2,323 are positive, the smallest
/// being 923 — **none is zero**. So `-1` is the only "no continuation under
/// this condition" marker, and slot 0 of the link array (which holds node
/// 27,307 on that build) is an ordinary head like any other. The 401,305 link
/// slots split into 335,496 node indices, 63,228 plain `-1` terminators, 2,580
/// condition markers and one zero, which is a node index.
pub const LC_GROUP: usize = 5;

/// Bit 31 of a link entry: the rest of its low bits are an LC Link Heads base.
const LC_LINK_BIT: u32 = 0x8000_0000;
/// The plain end-of-run terminator, i.e. `-1`.
const END_OF_CHAIN: u32 = 0xFFFF_FFFF;

const ASSET_TYPE_NAMES: [&str; 25] = [
    "Level", "Zone", "Actor", "Conduit", "Config", "Cinematic", "Model", "AnimClip", "AnimSet",
    "Material", "MaterialGraph", "Texture", "Atmosphere", "VisualEffect", "SoundBank",
    "Localization", "Unknown16", "Unknown17", "ZoneLighting", "LevelLighting", "NodeGraph",
    "Unknown21", "WwiseLookup", "Unknown23", "Unknown24",
];

pub fn asset_type_name(type_byte: u8) -> &'static str {
    ASSET_TYPE_NAMES.get(type_byte as usize).copied().unwrap_or("Invalid")
}

/// The five lighting conditions, in the order the LC link-head groups use them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LightingCondition {
    Day,
    Overcast,
    Sunset,
    Night,
    Blackout,
}

impl LightingCondition {
    pub const ALL: [Self; 5] = [Self::Day, Self::Overcast, Self::Sunset, Self::Night, Self::Blackout];
}

/// `None` in queries means the union over every lighting condition.
fn conditions(lc: Option<LightingCondition>) -> &'static [LightingCondition] {
    match lc {
        Some(LightingCondition::Day) => &LightingCondition::ALL[0..1],
        Some(LightingCondition::Overcast) => &LightingCondition::ALL[1..2],
        Some(LightingCondition::Sunset) => &LightingCondition::ALL[2..3],
        Some(LightingCondition::Night) => &LightingCondition::ALL[3..4],
        Some(LightingCondition::Blackout) => &LightingCondition::ALL[4..5],
        None => &LightingCondition::ALL,
    }
}

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

fn read_u32_at(bytes: &[u8], off: usize) -> Result<u32> {
    bytes
        .get(off..off + 4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .ok_or_else(|| ToolkitError::Parse("dag file too small".into()))
}

/// Strip the 12-byte prelude, or accept a bare DAT1 container.
fn unwrap_container(bytes: &[u8]) -> Result<&[u8]> {
    let magic = read_u32_at(bytes, 0)?;
    if magic == DAT1_MAGIC {
        return Ok(bytes);
    }
    if magic != DAG_MAGIC {
        return Err(ToolkitError::InvalidMagic { expected: DAG_MAGIC, got: magic });
    }
    let declared = read_u32_at(bytes, 4)? as usize;
    if read_u32_at(bytes, 8)? != 0 {
        return Err(ToolkitError::Unsupported("compressed dag".into()));
    }
    // The shipped file declares its payload exactly; a short one is truncated.
    bytes
        .get(DAG_PRELUDE..DAG_PRELUDE + declared)
        .ok_or_else(|| {
            ToolkitError::Parse(format!(
                "dag payload size {declared} runs past the file ({} bytes after the prelude)",
                bytes.len().saturating_sub(DAG_PRELUDE)
            ))
        })
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
    /// The DAT1 type word, for tools that report the build's dag version.
    pub version: u32,
    /// Asset ids, de-transposed. Ascending on every shipped `dag` we have seen.
    pub ids: Vec<u64>,
    /// Asset-type enum, one byte per asset.
    pub types: Vec<u8>,
    /// String-pool offsets of the asset paths, one per asset. Empty when the
    /// build ships no name section.
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
    /// Read a `dag`. `path` may be the file itself or the game folder holding
    /// it, since both spellings are in use across the tools.
    pub fn load(path: &Path) -> Result<Self> {
        let file = if path.is_dir() { path.join("dag") } else { path.to_path_buf() };
        Self::parse(&std::fs::read(file)?)
    }

    pub fn parse(data: &[u8]) -> Result<Self> {
        let payload = unwrap_container(data)?;
        let dat1 = Dat1::parse(payload)?;

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
        let link_heads = read_i32s(section(TAG_LINK_HEADS)?);
        let links = read_i32s(section(TAG_LINKS)?);
        // Optional: absent from some builds, so a missing section is not fatal.
        let lc_link_heads = dat1
            .get_section_data(TAG_LC_LINK_HEADS)
            .map(read_i32s)
            .unwrap_or_default();
        let name_offsets = dat1
            .get_section_data(TAG_ASSET_NAMES)
            .map(read_u32s)
            .unwrap_or_default();

        for (what, got) in [("types", types.len()), ("link heads", link_heads.len())] {
            if got != count {
                return Err(ToolkitError::Parse(format!(
                    "dag {what} has {got} entries but there are {count} asset ids"
                )));
            }
        }
        if !name_offsets.is_empty() && name_offsets.len() != count {
            return Err(ToolkitError::Parse(format!(
                "dag names has {} entries but there are {count} asset ids",
                name_offsets.len()
            )));
        }

        let ids_ascending = ids.windows(2).all(|w| w[0] < w[1]);
        let fallback = (!ids_ascending)
            .then(|| ids.iter().enumerate().map(|(i, &id)| (id, i)).collect());

        Ok(Self {
            version: dat1.unk1,
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

    pub fn asset_ids(&self) -> &[u64] {
        &self.ids
    }

    pub fn id(&self, index: usize) -> u64 {
        self.ids[index]
    }

    pub fn type_byte(&self, index: usize) -> u8 {
        self.types[index]
    }

    pub fn type_name(&self, index: usize) -> &'static str {
        asset_type_name(self.types[index])
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

    /// `(asset id, path)` for every named entry, with `/` separators.
    pub fn named_assets(&self) -> impl Iterator<Item = (u64, String)> + '_ {
        (0..self.ids.len()).filter_map(|i| self.name(i).map(|n| (self.ids[i], n.replace('\\', "/"))))
    }

    pub fn node(&self, i: usize) -> Option<DagNode> {
        Some(DagNode {
            id: *self.ids.get(i)?,
            asset_type: self.types[i],
            name_offset: self.name_offsets.get(i).copied().unwrap_or(0),
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
        let base = (marker as u32 & !LC_LINK_BIT) as usize;
        self.lc_link_heads.get(base..base + LC_GROUP)
    }

    /// Walk one asset's links the way the game does for a single lighting
    /// condition: node indices as they come, following an LC marker into that
    /// condition's continuation.
    fn for_each_link(&self, index: usize, lc: LightingCondition, f: &mut impl FnMut(usize)) {
        let Some(&head) = self.link_heads.get(index) else { return };
        if head < 0 {
            return;
        }
        let mut link = head as usize;
        // The link array has no cycles on the shipped build; the bound keeps a
        // damaged one from spinning here forever.
        for _ in 0..self.links.len() {
            let Some(&raw) = self.links.get(link) else { return };
            let entry = raw as u32;
            link += 1;
            if entry & LC_LINK_BIT == 0 {
                if (entry as usize) < self.ids.len() {
                    f(entry as usize);
                }
                continue;
            }
            if entry == END_OF_CHAIN {
                return;
            }
            let slot = (entry & !LC_LINK_BIT) as usize + lc as usize;
            match self.lc_link_heads.get(slot) {
                // `-1` is the absent head; 0 is a real slot of the link array.
                Some(&next) if next >= 0 => link = next as usize,
                _ => return,
            }
        }
    }

    /// Assets `index` loads directly, sorted and de-duplicated.
    pub fn direct_dependencies(&self, index: usize, lc: Option<LightingCondition>) -> Vec<usize> {
        let mut out = Vec::new();
        for &c in conditions(lc) {
            self.for_each_link(index, c, &mut |child| out.push(child));
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Everything the roots load, excluding the roots; one level unless `recursive`.
    pub fn dependencies(&self, roots: &[usize], lc: Option<LightingCondition>, recursive: bool) -> Vec<usize> {
        let mut seen = vec![false; self.ids.len()];
        let mut stack: Vec<usize> = Vec::new();
        for &r in roots.iter().filter(|&&r| r < self.ids.len()) {
            seen[r] = true;
            stack.push(r);
        }
        let mut out = Vec::new();
        let mut first_level = true;
        while !stack.is_empty() {
            let mut next = Vec::new();
            for node in stack.drain(..) {
                for &c in conditions(lc) {
                    self.for_each_link(node, c, &mut |child| {
                        if !seen[child] {
                            seen[child] = true;
                            out.push(child);
                            next.push(child);
                        }
                    });
                }
            }
            if first_level && !recursive {
                break;
            }
            first_level = false;
            stack = next;
        }
        out.sort_unstable();
        out
    }

    /// Reverse edges for "who loads this" queries.
    pub fn dependents_index(&self, lc: Option<LightingCondition>) -> Dependents {
        let mut edges: Vec<(u32, u32)> = Vec::new();
        for parent in 0..self.ids.len() {
            for &c in conditions(lc) {
                self.for_each_link(parent, c, &mut |child| edges.push((child as u32, parent as u32)));
            }
        }
        edges.sort_unstable();
        edges.dedup();

        let mut starts = vec![0u32; self.ids.len() + 1];
        for &(child, _) in &edges {
            starts[child as usize + 1] += 1;
        }
        for i in 1..starts.len() {
            starts[i] += starts[i - 1];
        }
        Dependents { starts, parents: edges.into_iter().map(|(_, p)| p).collect() }
    }
}

pub type NameTable = Arc<HashMap<u64, String>>;

/// A game's dag with its reverse index and name table, shared across commands.
pub struct GameDag {
    pub dag: Dag,
    pub dependents: Dependents,
    pub names: NameTable,
}

struct CachedDag {
    path: PathBuf,
    modified: Option<SystemTime>,
    game_dag: Arc<GameDag>,
}

/// Loads `<game_dir>/dag`, cached until the file changes.
pub fn load_game_dag(game_dir: &Path) -> Result<Arc<GameDag>> {
    static CACHE: Mutex<Option<CachedDag>> = Mutex::new(None);

    let path = game_dir.join("dag");
    let modified = std::fs::metadata(&path)?.modified().ok();
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = cache.as_ref().filter(|c| c.path == path && c.modified == modified) {
        return Ok(c.game_dag.clone());
    }
    let dag = Dag::load(&path)?;
    let dependents = dag.dependents_index(None);
    let names = Arc::new(dag.named_assets().collect());
    let game_dag = Arc::new(GameDag { dag, dependents, names });
    *cache = Some(CachedDag { path, modified, game_dag: game_dag.clone() });
    Ok(game_dag)
}

/// `asset id -> path` from `<game_dir>/dag`.
pub fn name_table(game_dir: &Path) -> Result<NameTable> {
    Ok(load_game_dag(game_dir)?.names.clone())
}

pub struct Dependents {
    starts: Vec<u32>,
    parents: Vec<u32>,
}

impl Dependents {
    pub fn direct(&self, index: usize) -> &[u32] {
        match (self.starts.get(index), self.starts.get(index + 1)) {
            (Some(&a), Some(&b)) => &self.parents[a as usize..b as usize],
            _ => &[],
        }
    }

    /// Every asset that loads `index` directly or indirectly, excluding `index`.
    pub fn transitive(&self, index: usize) -> Vec<usize> {
        let mut seen = vec![false; self.starts.len().saturating_sub(1)];
        if index >= seen.len() {
            return Vec::new();
        }
        seen[index] = true;
        let mut stack = vec![index];
        let mut out = Vec::new();
        while let Some(node) = stack.pop() {
            for &p in self.direct(node) {
                let p = p as usize;
                if !seen[p] {
                    seen[p] = true;
                    out.push(p);
                    stack.push(p);
                }
            }
        }
        out.sort_unstable();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A graph with no container behind it, for the link-walking tests.
    fn dag(link_heads: Vec<i32>, links: Vec<u32>, lc_link_heads: Vec<i32>) -> Dag {
        let n = link_heads.len();
        let empty = Dat1 {
            magic: DAT1_MAGIC,
            unk1: DAG_VERSION,
            total_size: 0,
            sections: Vec::new(),
            fixups: Vec::new(),
            strings_pool: Vec::new(),
            section_data: Vec::new(),
            sections_map: HashMap::new(),
        };
        Dag {
            dat1: empty,
            version: DAG_VERSION,
            ids: (0..n as u64).collect(),
            types: vec![0; n],
            name_offsets: Vec::new(),
            link_heads,
            links: links.into_iter().map(|v| v as i32).collect(),
            lc_link_heads,
            ids_ascending: true,
            fallback: None,
        }
    }

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

    /// Two real ids off the shipped build, transposed by hand.
    #[test]
    fn untransposes_byte_planes() {
        let ids = [0x8000_0DCC_5F02_623Cu64, 0x8000_35F1_EBDC_BCEC];
        let mut block = vec![0u8; 16];
        for (i, id) in ids.iter().enumerate() {
            for k in 0..8 {
                block[k * 2 + i] = (id >> (8 * k)) as u8;
            }
        }
        assert_eq!(detranspose_u64(&block), ids);
    }

    #[test]
    fn detranspose_handles_empty() {
        assert!(detranspose_u64(&[]).is_empty());
    }

    #[test]
    fn walks_plain_and_lighting_chains() {
        // 0 -> 1, then lc group 0: Day -> 2, Night -> 3; 1 -> 3; 2, 3 leaf
        let links = vec![1, LC_LINK_BIT, END_OF_CHAIN, 2, END_OF_CHAIN, 3, END_OF_CHAIN, 3, END_OF_CHAIN];
        let lc_heads = vec![3, -1, -1, 5, -1];
        let d = dag(vec![0, 7, -1, -1], links, lc_heads);

        assert_eq!(d.direct_dependencies(0, Some(LightingCondition::Day)), vec![1, 2]);
        assert_eq!(d.direct_dependencies(0, Some(LightingCondition::Night)), vec![1, 3]);
        assert_eq!(d.direct_dependencies(0, Some(LightingCondition::Sunset)), vec![1]);
        assert_eq!(d.direct_dependencies(0, None), vec![1, 2, 3]);
        assert_eq!(d.dependencies(&[0], Some(LightingCondition::Day), true), vec![1, 2, 3]);
        assert_eq!(d.dependencies(&[0], Some(LightingCondition::Day), false), vec![1, 2]);

        let rev = d.dependents_index(None);
        assert_eq!(rev.direct(3), &[0, 1]);
        assert_eq!(rev.transitive(3), vec![0, 1]);
        assert!(rev.transitive(0).is_empty());
    }

    /// The same links through the other spelling: the unconditional run stops
    /// at the marker, and the marker's group is reported per condition slot.
    #[test]
    fn marker_runs_are_reported_separately() {
        let links = vec![1, LC_LINK_BIT, END_OF_CHAIN, 2, END_OF_CHAIN, 3, END_OF_CHAIN, 3, END_OF_CHAIN];
        let lc_heads = vec![3, -1, -1, 5, -1];
        let d = dag(vec![0, 7, -1, -1], links, lc_heads);

        assert_eq!(d.deps(0), vec![1]);
        assert_eq!(d.lc_marker(0), Some(LC_LINK_BIT as i32));
        assert_eq!(d.lc_deps(0), vec![(0, vec![2]), (3, vec![3])]);
        assert_eq!(d.lc_group(LC_LINK_BIT as i32), Some(&[3, -1, -1, 5, -1][..]));
        // LC_GROUP is the per-condition group size, so the two readings agree.
        assert_eq!(LC_GROUP, LightingCondition::ALL.len());
    }
}
