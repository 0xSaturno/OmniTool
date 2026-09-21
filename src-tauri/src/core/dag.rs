//! Dependency DAG (`dag` at the game root): every shipped asset, its type, its name, and what it
//! loads. Link walking follows the engine: a chain ends at `0xFFFFFFFF`, and an entry with bit 31
//! set jumps into the per-lighting-condition link heads.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::core::dat1::{Dat1, DAT1_MAGIC};
use crate::core::error::{Result, ToolkitError};

pub const DAG_MAGIC: u32 = 0xB8EF3955;
pub const DAG_VERSION: u32 = 0x2A077A51;

const TAG_ASSET_IDS: u32 = 0x933C0D32;
const TAG_ASSET_NAMES: u32 = 0xD101A6CC;
const TAG_ASSET_TYPES: u32 = 0x7A0266BC;
const TAG_LINKS: u32 = 0xBC91D1CC;
const TAG_LINK_HEADS: u32 = 0xF958372E;
const TAG_LC_LINK_HEADS: u32 = 0xBFEC699F;

const LC_LINK_BIT: u32 = 0x8000_0000;
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

#[derive(Debug)]
pub struct Dag {
    pub version: u32,
    ids: Vec<u64>,
    types: Vec<u8>,
    name_offsets: Vec<u32>,
    strings: Vec<u8>,
    strings_base: usize,
    link_heads: Vec<i32>,
    links: Vec<u32>,
    lc_link_heads: Vec<i32>,
    sorted: bool,
}

fn read_u32_at(bytes: &[u8], off: usize) -> Result<u32> {
    bytes
        .get(off..off + 4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .ok_or_else(|| ToolkitError::Parse("dag: file too small".into()))
}

fn u32s(bytes: &[u8]) -> Vec<u32> {
    bytes.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect()
}

fn i32s(bytes: &[u8]) -> Vec<i32> {
    bytes.chunks_exact(4).map(|c| i32::from_le_bytes(c.try_into().unwrap())).collect()
}

/// The id block is stored as eight byte planes (all byte 0s, then all byte 1s, ...) for compression.
fn untranspose_ids(block: &[u8], count: usize) -> Vec<u64> {
    (0..count)
        .map(|i| (0..8).fold(0u64, |id, k| id | (block[k * count + i] as u64) << (8 * k)))
        .collect()
}

fn unwrap_container(bytes: &[u8]) -> Result<&[u8]> {
    let magic = read_u32_at(bytes, 0)?;
    if magic == DAT1_MAGIC {
        return Ok(bytes);
    }
    if magic != DAG_MAGIC {
        return Err(ToolkitError::InvalidMagic { expected: DAG_MAGIC, got: magic });
    }
    let size = read_u32_at(bytes, 4)? as usize;
    if read_u32_at(bytes, 8)? != 0 {
        return Err(ToolkitError::Unsupported("compressed dag".into()));
    }
    bytes.get(12..12 + size).ok_or_else(|| ToolkitError::Parse("dag: truncated payload".into()))
}

impl Dag {
    pub fn load(path: &Path) -> Result<Self> {
        Self::parse(&std::fs::read(path)?)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let mut dat1 = Dat1::parse(unwrap_container(bytes)?)?;
        let section = |tag| dat1.get_section_data(tag).ok_or(ToolkitError::SectionNotFound(tag));

        let id_block = section(TAG_ASSET_IDS)?;
        if id_block.len() % 8 != 0 {
            return Err(ToolkitError::Parse("dag: asset id block is not a multiple of 8".into()));
        }
        let count = id_block.len() / 8;
        let ids = untranspose_ids(id_block, count);
        let types = section(TAG_ASSET_TYPES)?.to_vec();
        let link_heads = i32s(section(TAG_LINK_HEADS)?);
        let links = u32s(section(TAG_LINKS)?);
        let lc_link_heads = dat1.get_section_data(TAG_LC_LINK_HEADS).map(i32s).unwrap_or_default();
        let name_offsets = dat1.get_section_data(TAG_ASSET_NAMES).map(u32s).unwrap_or_default();

        if types.len() != count || link_heads.len() != count {
            return Err(ToolkitError::Parse(format!(
                "dag: {count} ids but {} types and {} link heads",
                types.len(),
                link_heads.len()
            )));
        }
        if !name_offsets.is_empty() && name_offsets.len() != count {
            return Err(ToolkitError::Parse(format!(
                "dag: {count} ids but {} names",
                name_offsets.len()
            )));
        }

        let sorted = ids.windows(2).all(|w| w[0] < w[1]);
        let strings_base = dat1.header_end();
        Ok(Self {
            version: dat1.unk1,
            ids,
            types,
            name_offsets,
            strings: std::mem::take(&mut dat1.strings_pool),
            strings_base,
            link_heads,
            links,
            lc_link_heads,
            sorted,
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

    pub fn name(&self, index: usize) -> Option<&str> {
        let offset = (*self.name_offsets.get(index)? as usize).checked_sub(self.strings_base)?;
        let tail = self.strings.get(offset..)?;
        let end = tail.iter().position(|&b| b == 0)?;
        std::str::from_utf8(&tail[..end]).ok()
    }

    /// `(asset id, path)` for every named entry, with `/` separators.
    pub fn named_assets(&self) -> impl Iterator<Item = (u64, String)> + '_ {
        (0..self.ids.len()).filter_map(|i| self.name(i).map(|n| (self.ids[i], n.replace('\\', "/"))))
    }

    pub fn index_of(&self, asset_id: u64) -> Option<usize> {
        if self.sorted {
            self.ids.binary_search(&asset_id).ok()
        } else {
            self.ids.iter().position(|&id| id == asset_id)
        }
    }

    fn for_each_link(&self, index: usize, lc: LightingCondition, f: &mut impl FnMut(usize)) {
        let Some(&head) = self.link_heads.get(index) else { return };
        if head < 0 {
            return;
        }
        let mut link = head as usize;
        for _ in 0..self.links.len() {
            let Some(&entry) = self.links.get(link) else { return };
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
                Some(&next) if next > 0 => link = next as usize,
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

    fn dag(link_heads: Vec<i32>, links: Vec<u32>, lc_link_heads: Vec<i32>) -> Dag {
        let n = link_heads.len();
        Dag {
            version: DAG_VERSION,
            ids: (0..n as u64).collect(),
            types: vec![0; n],
            name_offsets: Vec::new(),
            strings: Vec::new(),
            strings_base: 0,
            link_heads,
            links,
            lc_link_heads,
            sorted: true,
        }
    }

    #[test]
    fn untransposes_byte_planes() {
        let ids = [0x8000_0DCC_5F02_623Cu64, 0x8000_35F1_EBDC_BCEC];
        let mut block = vec![0u8; 16];
        for (i, id) in ids.iter().enumerate() {
            for k in 0..8 {
                block[k * 2 + i] = (id >> (8 * k)) as u8;
            }
        }
        assert_eq!(untranspose_ids(&block, 2), ids);
    }

    #[test]
    fn walks_plain_and_lighting_chains() {
        // 0 -> 1, then lc group 0: Day -> 2, Night -> 3; 1 -> 3; 2, 3 leaf
        let links = vec![1, LC_LINK_BIT, END_OF_CHAIN, 2, END_OF_CHAIN, 3, END_OF_CHAIN, 3, END_OF_CHAIN];
        let lc_heads = vec![3, 0, 0, 5, 0];
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
}
