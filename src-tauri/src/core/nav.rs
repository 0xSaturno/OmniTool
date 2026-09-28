//! Navigation data: the walkable surface of every zone that has one.
//!
//! `*navdata.zone` assets hold five sections. Four of them are navigation and
//! together they are the largest block of zone bytes nothing in this project
//! used to read -- 684 MB over 916 zones on the shipped PC build:
//!
//! | tag          | bytes  | zones | what it is                                  |
//! |--------------|--------|-------|---------------------------------------------|
//! | `0x04C19E69` | 105 KB | 116   | nav **container** -- the per-region meta and the unit lookup grid |
//! | `0xD124430B` | 216 MB | 673   | nav **unit** -- the group grid and *the mesh itself* |
//! | `0xA903D8F1` | 341 MB | 673   | nav **tile cache** -- Detour `dtCompressedTile` stream, the rebuild source |
//! | `0x339AEC45` | 127 MB | 152   | nav **3D** -- a sparse voxel octree for flight, not a surface |
//! | `0x3D8DBDB8` | 4 B    | 916   | four bytes, present whenever any of the above is (see [`NAV_VERSION_TAG`]) |
//!
//! Only `0xD124430B` carries vertices and triangles, and this module decodes it
//! completely. The other three are framed and validated here but not unpacked:
//! the tile cache's payloads are Detour-compressed height layers that only a
//! rebuild needs, and the 3D section is a voxel tree, not geometry.
//!
//! # The unit section, byte for byte
//!
//! ```text
//! +0x00  u32   magic          == NAV_UNIT_MAGIC on 673/673 shipped units
//! +0x04  u64   asset id       this navdata zone's own id
//! +0x0C  u64   container id   the region's containernavdata.zone
//! +0x14  i16   min tile x     inclusive, in TILE units (metres / 16)
//! +0x16  i16   min tile z
//! +0x18  i16   max tile x     exclusive
//! +0x1A  i16   max tile z
//! +0x1C  i16   log2 tile size == 4, i.e. TILE_SIZE_M
//! +0x1E  --    two bytes of alignment padding
//! +0x20  ..    layers * (max_x - min_x) * (max_z - min_z) group records of
//!              GROUP_STRIDE bytes, layer-major then z then x
//!        ..    16-byte aligned, then one payload run per non-empty group
//! ```
//!
//! The header does **not** store the layer count -- that lives in the region's
//! container -- so [`NavUnit::parse`] recovers it from the section instead, by
//! the one candidate that makes every group's payload offset land inside the
//! run that starts right after the group array and makes the payload sizes sum
//! to exactly the remaining bytes. It is 2 on 673/673 shipped units.
//!
//! A group record:
//!
//! ```text
//! +0x00  u16   group x        world metres + GROUP_BIAS, always a multiple of TILE_SIZE_M
//! +0x02  u16   group z        ditto
//! +0x04  u16   vertex count
//! +0x06  u16   connected poly count
//! +0x08  u16   disconnected poly count
//! +0x0A  u16   edge count, type Internal
//! +0x0C  u16   edge count, type External
//! +0x0E  u16   edge count, type Custom  (== the clue count)
//! +0x10  u16   edge count, type Border
//! +0x12  u16   padding, zero on 254,966/254,966
//! +0x14  u32   vertex offset      all five offsets are relative to the START
//! +0x18  u32   edge offset        of this 48-byte record, not to the section
//! +0x1C  u32   poly offset
//! +0x20  u32   clue offset
//! +0x24  u32   bounding-sphere offset
//! +0x28  u32   payload size       0 for an empty group
//! +0x2C  u32   padding, zero on 254,966/254,966
//! ```
//!
//! Each of the five arrays is padded to a 16-byte multiple and they appear in
//! offset order, so `payload size` is exactly
//! `a16(12 V) + a16(14 E) + a16(24 (P + D)) + a16(40 C) + a16(16 P)`. That
//! identity is what pins the element sizes down, and it holds on every one of
//! the 254,966 non-empty groups; see `nav_probe sizes`.
//!
//! Vertices are `f32 x, y, z` in the **region's** space -- the same space the
//! zone's scene objects are authored in, not a per-group local space -- and
//! every one of them falls inside its own group's 16 m column.
//!
//! Polygons are triangles ([`NavPoly`]), edges are half-edges carrying the
//! neighbour they cross to ([`NavEdge`]), and the connected polygons are
//! followed by the disconnected ones in the same array.

use byteorder::{ByteOrder, LE};

use crate::core::error::ToolkitError;
use crate::core::zone::Zone;

type Result<T> = std::result::Result<T, ToolkitError>;

// ---------------------------------------------------------------------------
// Section tags
// ---------------------------------------------------------------------------

/// `kZoneNavBuiltContainer` -- "Container-wide navmesh meta information".
pub const NAV_CONTAINER_TAG: u32 = 0x04C1_9E69;
/// `kZoneNavBuiltUnit` -- "Nav mesh data for a single unit (tile, region, etc)".
pub const NAV_UNIT_TAG: u32 = 0xD124_430B;
/// The Detour tile-cache stream. Written immediately after the unit buffer, and
/// present on exactly the same 673 zones.
pub const NAV_TILE_CACHE_TAG: u32 = 0xA903_D8F1;
/// The 3D (flight) navigation buffer: a built-region header plus a sparse voxel
/// octree. Present on 152 zones, the ones with 3D navigation switched on.
pub const NAV_3D_TAG: u32 = 0x339A_EC45;
/// Four bytes, on all 916 nav zones including the 234 that hold nothing else.
/// Not written by the nav builder's own buffers, so it is the zone pipeline's
/// own marker rather than part of the nav payload; [`nav_version_word`] returns
/// it without claiming to know the name.
pub const NAV_VERSION_TAG: u32 = 0x3D8D_BDB8;

/// `kBuiltNavVersionHash`, at `+0x00` of both the container and the unit.
pub const NAV_UNIT_MAGIC: u32 = 0x35F6_19FA;
/// `kBuiltNav3DVersionHash`, at `+0x00` of the 3D buffer.
pub const NAV_3D_MAGIC: u32 = 0xAA31_8918;
/// `DT_TILECACHE_MAGIC`: the ASCII bytes `R L T D` that open every compressed
/// tile inside [`NAV_TILE_CACHE_TAG`].
pub const TILE_CACHE_MAGIC: u32 = 0x4454_4C52;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// A nav group covers a 16 m x 16 m column of the world, unbounded in Y.
pub const TILE_SIZE_M: i32 = 16;
/// Group positions are stored biased so the whole `i16` metre range is positive.
pub const GROUP_BIAS: i32 = 0x8000;
pub const INVALID_INDEX: u16 = 0xFFFF;

pub const GROUP_STRIDE: usize = 48;
pub const VERT_STRIDE: usize = 12;
pub const EDGE_STRIDE: usize = 14;
pub const POLY_STRIDE: usize = 24;
pub const CLUE_STRIDE: usize = 40;
pub const BSPHERE_STRIDE: usize = 16;

/// Offset of the group array inside the unit section.
pub const GROUP_ARRAY_OFFSET: usize = 0x20;
/// The largest layer count [`NavUnit::parse`] will consider.
const MAX_LAYERS: usize = 16;

const fn a16(n: usize) -> usize {
    (n + 15) & !15usize
}

// ---------------------------------------------------------------------------
// Edge types
// ---------------------------------------------------------------------------

/// The four edge classes, in the order their counts appear in a group record
/// and in the order the edges themselves are stored.
///
/// The type is repeated in each edge's own `+0x00`, and it agrees with the
/// position in the array on 1,178,644/1,178,644 edges measured, which is how
/// the count order was fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u16)]
pub enum EdgeType {
    /// Between two triangles of this group; shared, stored once.
    Internal = 0,
    /// Leaves the group, or crosses between two Detour layers inside it.
    External = 1,
    /// A "clue": an authored link (vault, jump, grind) with a [`NavClue`].
    Custom = 2,
    /// The outer rim of the mesh. No neighbour.
    Border = 3,
}

impl EdgeType {
    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            0 => Some(Self::Internal),
            1 => Some(Self::External),
            2 => Some(Self::Custom),
            3 => Some(Self::Border),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::External => "external",
            Self::Custom => "custom",
            Self::Border => "border",
        }
    }
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// One navigation triangle. 24 bytes.
///
/// ```text
/// +0x00  u16[3]  vertex indices into the group's vertex array
/// +0x06  u16[3]  edge indices into the group's edge array, INVALID_INDEX if none
/// +0x0C  u16     island
/// +0x0E  u8      poly flags -- the bot-class mask a path query filters on
/// +0x0F  u8      padding
/// +0x10  u64     a runtime handle, zero in the shipped data except on the
///                polygons a nav-modifying volume owns
/// ```
#[derive(Debug, Clone, Copy)]
pub struct NavPoly {
    pub verts: [u16; 3],
    pub edges: [u16; 3],
    pub island: u16,
    pub flags: u8,
    pub pad: u8,
    pub modify_handle: u64,
}

impl NavPoly {
    pub fn read(d: &[u8]) -> Self {
        Self {
            verts: [
                LE::read_u16(&d[0x00..]),
                LE::read_u16(&d[0x02..]),
                LE::read_u16(&d[0x04..]),
            ],
            edges: [
                LE::read_u16(&d[0x06..]),
                LE::read_u16(&d[0x08..]),
                LE::read_u16(&d[0x0A..]),
            ],
            island: LE::read_u16(&d[0x0C..]),
            flags: d[0x0E],
            pad: d[0x0F],
            modify_handle: LE::read_u64(&d[0x10..]),
        }
    }
}

/// One half-edge. 14 bytes.
///
/// ```text
/// +0x00  u16  edge type (EdgeType)
/// +0x02  u16  the polygon this edge belongs to
/// +0x04  u16  first vertex           |  for a Custom edge these four
/// +0x06  u16  second vertex          |  words are a different union arm
/// +0x08  u16  target group x, biased |  and the geometry comes from the
/// +0x0A  u16  target group z, biased |  NavClue instead
/// +0x0C  u16  target polygon, INVALID_INDEX on a Border edge
/// ```
///
/// The target group is `(0x8000 + metres)` on both axes and the metres are
/// always a multiple of 16, which is visible in the raw bytes: the low byte of
/// `+0x08` takes exactly 16 distinct values game-wide, all multiples of 16.
#[derive(Debug, Clone, Copy)]
pub struct NavEdge {
    pub edge_type: u16,
    pub poly: u16,
    pub vert_a: u16,
    pub vert_b: u16,
    pub target_group_x_raw: u16,
    pub target_group_z_raw: u16,
    pub target_poly: u16,
}

impl NavEdge {
    pub fn read(d: &[u8]) -> Self {
        Self {
            edge_type: LE::read_u16(&d[0x00..]),
            poly: LE::read_u16(&d[0x02..]),
            vert_a: LE::read_u16(&d[0x04..]),
            vert_b: LE::read_u16(&d[0x06..]),
            target_group_x_raw: LE::read_u16(&d[0x08..]),
            target_group_z_raw: LE::read_u16(&d[0x0A..]),
            target_poly: LE::read_u16(&d[0x0C..]),
        }
    }
    pub fn kind(&self) -> Option<EdgeType> {
        EdgeType::from_u16(self.edge_type)
    }
    /// The group this edge crosses into, in world metres, or `None` when the
    /// stored position is the invalid `(0, 0)`.
    pub fn target_group(&self) -> Option<(i32, i32)> {
        if self.target_group_x_raw == 0 && self.target_group_z_raw == 0 {
            None
        } else {
            Some((
                self.target_group_x_raw as i32 - GROUP_BIAS,
                self.target_group_z_raw as i32 - GROUP_BIAS,
            ))
        }
    }
}

/// A custom-edge "clue": the authored link a bot uses to leave the mesh.
/// 40 bytes, one per [`EdgeType::Custom`] edge, in the same order.
///
/// ```text
/// +0x00  f32[3]  position
/// +0x0C  f32     padding or a fourth component, zero throughout
/// +0x10  u64     the clue actor's uid
/// +0x18  u64     the uid of the actor at the far end
/// +0x20  f32     distance / weight
/// +0x24  u16     required ability
/// +0x26  u16     required entity
/// ```
///
/// The two uids are what a prius `TargetNavClue` field points at.
#[derive(Debug, Clone, Copy)]
pub struct NavClue {
    pub position: [f32; 3],
    pub w: f32,
    pub actor_uid: u64,
    pub target_actor_uid: u64,
    pub distance: f32,
    pub required_ability: u16,
    pub required_entity: u16,
}

impl NavClue {
    pub fn read(d: &[u8]) -> Self {
        Self {
            position: [
                LE::read_f32(&d[0x00..]),
                LE::read_f32(&d[0x04..]),
                LE::read_f32(&d[0x08..]),
            ],
            w: LE::read_f32(&d[0x0C..]),
            actor_uid: LE::read_u64(&d[0x10..]),
            target_actor_uid: LE::read_u64(&d[0x18..]),
            distance: LE::read_f32(&d[0x20..]),
            required_ability: LE::read_u16(&d[0x24..]),
            required_entity: LE::read_u16(&d[0x26..]),
        }
    }
}

/// `Intersection::Sphere` -- one per *connected* polygon, in the same order.
/// 16 bytes: centre then radius.
#[derive(Debug, Clone, Copy)]
pub struct NavSphere {
    pub centre: [f32; 3],
    pub radius: f32,
}

impl NavSphere {
    pub fn read(d: &[u8]) -> Self {
        Self {
            centre: [
                LE::read_f32(&d[0x00..]),
                LE::read_f32(&d[0x04..]),
                LE::read_f32(&d[0x08..]),
            ],
            radius: LE::read_f32(&d[0x0C..]),
        }
    }
}

// ---------------------------------------------------------------------------
// Group
// ---------------------------------------------------------------------------

/// One 48-byte group record, with its payload offsets already made absolute.
#[derive(Debug, Clone, Copy)]
pub struct NavGroup {
    /// Index in the unit's group array, layer-major.
    pub index: usize,
    /// Which layer (Detour template / bot size class) this group belongs to.
    pub layer: usize,
    /// South-west corner of the group's column, in world metres.
    pub x: i32,
    pub z: i32,
    pub vert_count: usize,
    pub poly_count: usize,
    pub disconnected_poly_count: usize,
    /// Internal, External, Custom, Border.
    pub edge_counts: [usize; 4],
    pub pad12: u16,
    pub pad2c: u32,
    /// Absolute offsets into the unit section.
    pub vert_offset: usize,
    pub edge_offset: usize,
    pub poly_offset: usize,
    pub clue_offset: usize,
    pub bsphere_offset: usize,
    pub data_size: usize,
}

impl NavGroup {
    pub fn is_empty(&self) -> bool {
        self.data_size == 0
    }
    /// Connected plus disconnected: the length of the polygon array.
    pub fn total_poly_count(&self) -> usize {
        self.poly_count + self.disconnected_poly_count
    }
    pub fn edge_count(&self) -> usize {
        self.edge_counts.iter().sum()
    }
    /// Index of the first edge of a given type.
    pub fn edge_range(&self, kind: EdgeType) -> std::ops::Range<usize> {
        let k = kind as usize;
        let start: usize = self.edge_counts[..k].iter().sum();
        start..start + self.edge_counts[k]
    }
    /// The payload size the five array lengths imply. Equal to `data_size` on
    /// every non-empty shipped group.
    pub fn implied_data_size(&self) -> usize {
        a16(VERT_STRIDE * self.vert_count)
            + a16(EDGE_STRIDE * self.edge_count())
            + a16(POLY_STRIDE * self.total_poly_count())
            + a16(CLUE_STRIDE * self.edge_counts[EdgeType::Custom as usize])
            + a16(BSPHERE_STRIDE * self.poly_count)
    }
}

// ---------------------------------------------------------------------------
// Unit
// ---------------------------------------------------------------------------

/// A parsed [`NAV_UNIT_TAG`] section: the group grid, and a borrow of the bytes
/// the groups point into.
#[derive(Debug, Clone)]
pub struct NavUnit<'a> {
    pub data: &'a [u8],
    pub magic: u32,
    pub asset_id: u64,
    pub container_id: u64,
    /// Inclusive, in tile units.
    pub min_tile: [i16; 2],
    /// Exclusive, in tile units.
    pub max_tile: [i16; 2],
    pub log_scale: i16,
    pub layers: usize,
    pub groups: Vec<NavGroup>,
}

impl<'a> NavUnit<'a> {
    /// Tiles along X and Z.
    pub fn grid(&self) -> (usize, usize) {
        (
            (self.max_tile[0] - self.min_tile[0]).max(0) as usize,
            (self.max_tile[1] - self.min_tile[1]).max(0) as usize,
        )
    }

    /// Parse the section. Fails only on a truncated or non-magic buffer, or
    /// when no layer count in `1..=16` explains the section's length.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        if data.len() < GROUP_ARRAY_OFFSET {
            return Err(ToolkitError::Parse(format!(
                "nav unit section is {} bytes, shorter than its header",
                data.len()
            )));
        }
        let magic = LE::read_u32(&data[0x00..]);
        if magic != NAV_UNIT_MAGIC {
            return Err(ToolkitError::Parse(format!(
                "nav unit magic {magic:08X}, expected {NAV_UNIT_MAGIC:08X}"
            )));
        }
        let min_tile = [LE::read_i16(&data[0x14..]), LE::read_i16(&data[0x16..])];
        let max_tile = [LE::read_i16(&data[0x18..]), LE::read_i16(&data[0x1A..])];
        let nx = (max_tile[0] - min_tile[0]).max(0) as usize;
        let nz = (max_tile[1] - min_tile[1]).max(0) as usize;
        let cells = nx * nz;
        let layers = infer_layers(data, cells).ok_or_else(|| {
            ToolkitError::Parse(format!(
                "nav unit: no layer count in 1..={MAX_LAYERS} fits a {cells}-cell grid in {} bytes",
                data.len()
            ))
        })?;

        let mut groups = Vec::with_capacity(layers * cells);
        for i in 0..layers * cells {
            let o = GROUP_ARRAY_OFFSET + GROUP_STRIDE * i;
            let rel = |off: usize| o + LE::read_u32(&data[o + off..]) as usize;
            groups.push(NavGroup {
                index: i,
                layer: if cells == 0 { 0 } else { i / cells },
                x: LE::read_u16(&data[o..]) as i32 - GROUP_BIAS,
                z: LE::read_u16(&data[o + 0x02..]) as i32 - GROUP_BIAS,
                vert_count: LE::read_u16(&data[o + 0x04..]) as usize,
                poly_count: LE::read_u16(&data[o + 0x06..]) as usize,
                disconnected_poly_count: LE::read_u16(&data[o + 0x08..]) as usize,
                edge_counts: [
                    LE::read_u16(&data[o + 0x0A..]) as usize,
                    LE::read_u16(&data[o + 0x0C..]) as usize,
                    LE::read_u16(&data[o + 0x0E..]) as usize,
                    LE::read_u16(&data[o + 0x10..]) as usize,
                ],
                pad12: LE::read_u16(&data[o + 0x12..]),
                pad2c: LE::read_u32(&data[o + 0x2C..]),
                vert_offset: rel(0x14),
                edge_offset: rel(0x18),
                poly_offset: rel(0x1C),
                clue_offset: rel(0x20),
                bsphere_offset: rel(0x24),
                data_size: LE::read_u32(&data[o + 0x28..]) as usize,
            });
        }

        Ok(Self {
            data,
            magic,
            asset_id: LE::read_u64(&data[0x04..]),
            container_id: LE::read_u64(&data[0x0C..]),
            min_tile,
            max_tile,
            log_scale: LE::read_i16(&data[0x1C..]),
            layers,
            groups,
        })
    }

    /// Pull a section out of a zone and parse it.
    pub fn from_zone(zone: &'a Zone) -> Result<Self> {
        let d = zone
            .dat1
            .get_section_data(NAV_UNIT_TAG)
            .ok_or_else(|| ToolkitError::Parse("zone has no nav unit section".into()))?;
        Self::parse(d)
    }

    fn slice(&self, at: usize, stride: usize, n: usize) -> &'a [u8] {
        let end = at.saturating_add(stride * n);
        if n == 0 || end > self.data.len() {
            &[]
        } else {
            &self.data[at..end]
        }
    }

    pub fn verts(&self, g: &NavGroup) -> Vec<[f32; 3]> {
        let d = self.slice(g.vert_offset, VERT_STRIDE, g.vert_count);
        d.chunks_exact(VERT_STRIDE)
            .map(|c| {
                [
                    LE::read_f32(&c[0..]),
                    LE::read_f32(&c[4..]),
                    LE::read_f32(&c[8..]),
                ]
            })
            .collect()
    }

    pub fn polys(&self, g: &NavGroup) -> Vec<NavPoly> {
        let d = self.slice(g.poly_offset, POLY_STRIDE, g.total_poly_count());
        d.chunks_exact(POLY_STRIDE).map(NavPoly::read).collect()
    }

    pub fn edges(&self, g: &NavGroup) -> Vec<NavEdge> {
        let d = self.slice(g.edge_offset, EDGE_STRIDE, g.edge_count());
        d.chunks_exact(EDGE_STRIDE).map(NavEdge::read).collect()
    }

    pub fn clues(&self, g: &NavGroup) -> Vec<NavClue> {
        let n = g.edge_counts[EdgeType::Custom as usize];
        let d = self.slice(g.clue_offset, CLUE_STRIDE, n);
        d.chunks_exact(CLUE_STRIDE).map(NavClue::read).collect()
    }

    pub fn bspheres(&self, g: &NavGroup) -> Vec<NavSphere> {
        let d = self.slice(g.bsphere_offset, BSPHERE_STRIDE, g.poly_count);
        d.chunks_exact(BSPHERE_STRIDE).map(NavSphere::read).collect()
    }

    /// Every array of every non-empty group lands inside the section, the five
    /// offsets ascend, and the implied payload size matches the stored one.
    pub fn validate(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut covered = 0usize;
        for g in &self.groups {
            if g.is_empty() {
                if g.vert_count | g.poly_count | g.disconnected_poly_count != 0 {
                    out.push(format!("group {} has no payload but counts set", g.index));
                }
                continue;
            }
            covered += g.data_size;
            if g.implied_data_size() != g.data_size {
                out.push(format!(
                    "group {} payload {} but arrays imply {}",
                    g.index,
                    g.data_size,
                    g.implied_data_size()
                ));
            }
            if !(g.vert_offset <= g.edge_offset
                && g.edge_offset <= g.poly_offset
                && g.poly_offset <= g.clue_offset
                && g.clue_offset <= g.bsphere_offset)
            {
                out.push(format!("group {} offsets out of order", g.index));
            }
            if g.vert_offset + g.data_size > self.data.len() {
                out.push(format!("group {} payload runs past the section", g.index));
            }
            if g.x % TILE_SIZE_M != 0 || g.z % TILE_SIZE_M != 0 {
                out.push(format!(
                    "group {} position ({}, {}) is not on the 16 m lattice",
                    g.index, g.x, g.z
                ));
            }
        }
        let array_end = a16(GROUP_ARRAY_OFFSET + GROUP_STRIDE * self.groups.len());
        if array_end + covered != self.data.len() {
            out.push(format!(
                "payloads cover {} of {} bytes after the group array",
                covered,
                self.data.len() - array_end
            ));
        }
        out
    }

    /// Totals, for reporting.
    pub fn totals(&self) -> NavTotals {
        let mut t = NavTotals::default();
        for g in &self.groups {
            if g.is_empty() {
                t.empty_groups += 1;
                continue;
            }
            t.groups += 1;
            t.verts += g.vert_count;
            t.polys += g.poly_count;
            t.disconnected += g.disconnected_poly_count;
            for i in 0..4 {
                t.edges[i] += g.edge_counts[i];
            }
        }
        t
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NavTotals {
    pub groups: usize,
    pub empty_groups: usize,
    pub verts: usize,
    pub polys: usize,
    pub disconnected: usize,
    pub edges: [usize; 4],
}

/// The layer count the section's own geometry implies.
///
/// The group array is `layers * cells` records; the payload run starts at the
/// next 16-byte boundary after it and the payload sizes must tile it exactly.
/// Only one candidate satisfies both on real data.
pub fn infer_layers(data: &[u8], cells: usize) -> Option<usize> {
    if cells == 0 {
        // A unit with an empty grid: the section is header-only.
        return if data.len() <= a16(GROUP_ARRAY_OFFSET) {
            Some(0)
        } else {
            None
        };
    }
    for layers in 1..=MAX_LAYERS {
        let n = layers * cells;
        let array_end = GROUP_ARRAY_OFFSET + GROUP_STRIDE * n;
        if array_end > data.len() {
            break;
        }
        let base = a16(array_end);
        let mut first = usize::MAX;
        let mut sum = 0usize;
        let mut ok = true;
        for i in 0..n {
            let o = GROUP_ARRAY_OFFSET + GROUP_STRIDE * i;
            let size = LE::read_u32(&data[o + 0x28..]) as usize;
            if size == 0 {
                continue;
            }
            let at = o + LE::read_u32(&data[o + 0x14..]) as usize;
            if at < base || at.saturating_add(size) > data.len() {
                ok = false;
                break;
            }
            first = first.min(at);
            sum += size;
        }
        if !ok {
            continue;
        }
        if sum == 0 {
            if base == data.len() {
                return Some(layers);
            }
            continue;
        }
        if first == base && base + sum == data.len() {
            return Some(layers);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Container
// ---------------------------------------------------------------------------

/// A parsed [`NAV_CONTAINER_TAG`] section.
///
/// ```text
/// +0x00  u32    magic == NAV_UNIT_MAGIC
/// +0x04  u64    the containernavdata zone's own asset id
/// +0x0C  i32    layer count
/// +0x10  i16    min tile x, inclusive        the lookup grid over UNITS. For a
/// +0x12  i16    min tile z                   region built as one unit the four
/// +0x14  i16    max tile x, exclusive        bounds are in METRES and the log
/// +0x16  i16    max tile z                   scale is -1; otherwise they are in
/// +0x18  i16    log2 of the unit size        units of 2^log_scale metres.
/// +0x1A  --     six bytes of padding to an 8-byte boundary
/// +0x20  u64[]  layers * nx * nz zeroed slots, filled in at load time
/// ```
#[derive(Debug, Clone, Copy)]
pub struct NavContainer {
    pub magic: u32,
    pub asset_id: u64,
    pub layer_count: i32,
    pub min_tile: [i16; 2],
    pub max_tile: [i16; 2],
    pub log_scale: i16,
    pub slots: usize,
}

impl NavContainer {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 0x20 {
            return Err(ToolkitError::Parse(format!(
                "nav container section is {} bytes",
                data.len()
            )));
        }
        let magic = LE::read_u32(&data[0x00..]);
        if magic != NAV_UNIT_MAGIC {
            return Err(ToolkitError::Parse(format!(
                "nav container magic {magic:08X}, expected {NAV_UNIT_MAGIC:08X}"
            )));
        }
        Ok(Self {
            magic,
            asset_id: LE::read_u64(&data[0x04..]),
            layer_count: LE::read_i32(&data[0x0C..]),
            min_tile: [LE::read_i16(&data[0x10..]), LE::read_i16(&data[0x12..])],
            max_tile: [LE::read_i16(&data[0x14..]), LE::read_i16(&data[0x16..])],
            log_scale: LE::read_i16(&data[0x18..]),
            slots: (data.len() - 0x20) / 8,
        })
    }

    /// `layer_count * nx * nz`, the slot count the bounds imply.
    pub fn implied_slots(&self) -> usize {
        let nx = (self.max_tile[0] - self.min_tile[0]).max(0) as usize;
        let nz = (self.max_tile[1] - self.min_tile[1]).max(0) as usize;
        self.layer_count.max(0) as usize * nx * nz
    }
}

// ---------------------------------------------------------------------------
// Tile cache
// ---------------------------------------------------------------------------

/// One entry of the [`NAV_TILE_CACHE_TAG`] stream.
///
/// The stream is, per layer: `{ u32 size, size bytes, pad to 4, u32 0 }*`
/// followed by a `u32 0` terminator. A tile's own bytes are a Detour
/// `dtTileCacheLayerHeader` (magic [`TILE_CACHE_MAGIC`], version 1) followed by
/// the compressed height/area/connection layers, so a tile carries its own tile
/// coordinates and 16 m AABB.
#[derive(Debug, Clone, Copy)]
pub struct TileCacheEntry {
    pub layer: usize,
    /// Offset of the tile's own bytes (past the size word).
    pub offset: usize,
    pub size: usize,
    pub magic: u32,
    pub version: u32,
    pub tx: i32,
    pub tz: i32,
    pub tlayer: i32,
    pub bmin: [f32; 3],
    pub bmax: [f32; 3],
}

/// Walk the tile-cache stream. Returns the entries and the number of layer
/// terminators seen; an error means the framing did not reach the end cleanly.
pub fn tile_cache_entries(data: &[u8]) -> Result<(Vec<TileCacheEntry>, usize)> {
    let mut out = Vec::new();
    let mut layers = 0usize;
    let mut at = 0usize;
    while at + 4 <= data.len() {
        let size = LE::read_u32(&data[at..]) as usize;
        at += 4;
        if size == 0 {
            layers += 1;
            continue;
        }
        if at + size > data.len() {
            return Err(ToolkitError::Parse(format!(
                "tile cache entry at {at} claims {size} bytes, {} left",
                data.len() - at
            )));
        }
        let h = &data[at..at + size];
        let mut e = TileCacheEntry {
            layer: layers,
            offset: at,
            size,
            magic: 0,
            version: 0,
            tx: 0,
            tz: 0,
            tlayer: 0,
            bmin: [0.0; 3],
            bmax: [0.0; 3],
        };
        if h.len() >= 0x2C {
            e.magic = LE::read_u32(&h[0x00..]);
            e.version = LE::read_u32(&h[0x04..]);
            e.tx = LE::read_i32(&h[0x08..]);
            e.tz = LE::read_i32(&h[0x0C..]);
            e.tlayer = LE::read_i32(&h[0x10..]);
            for i in 0..3 {
                e.bmin[i] = LE::read_f32(&h[0x14 + 4 * i..]);
                e.bmax[i] = LE::read_f32(&h[0x20 + 4 * i..]);
            }
        }
        out.push(e);
        at += a4(size);
        // the four bytes the runtime keeps a dtCompressedTileRef in
        at += 4;
    }
    if at != data.len() {
        return Err(ToolkitError::Parse(format!(
            "tile cache framing stopped at {at} of {} bytes",
            data.len()
        )));
    }
    Ok((out, layers))
}

const fn a4(n: usize) -> usize {
    (n + 3) & !3usize
}

// ---------------------------------------------------------------------------
// 3D navigation
// ---------------------------------------------------------------------------

/// The header of a [`NAV_3D_TAG`] section: one built region, then a `u32` byte
/// count, then that many bytes of sparse-voxel-octree nodes.
///
/// The region header's own length is not written down anywhere, so
/// [`Nav3D::parse`] recovers it the only way the bytes allow: by the offset at
/// which a `u32` reads as exactly the bytes remaining after it.
#[derive(Debug, Clone, Copy)]
pub struct Nav3D {
    pub magic: u32,
    pub header_size: usize,
    pub node_bytes: usize,
    /// triples after the magic; see the probe for the evidence.
    pub min: [f32; 3],
    pub extent: [f32; 3],
    pub geo_extent: [f32; 3],
    pub min_voxel_radius: f32,
}

impl Nav3D {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 0x40 {
            return Err(ToolkitError::Parse(format!(
                "nav 3d section is {} bytes",
                data.len()
            )));
        }
        let magic = LE::read_u32(&data[0..]);
        if magic != NAV_3D_MAGIC {
            return Err(ToolkitError::Parse(format!(
                "nav 3d magic {magic:08X}, expected {NAV_3D_MAGIC:08X}"
            )));
        }
        let mut header_size = None;
        let mut at = 4;
        while at + 4 <= data.len() {
            if LE::read_u32(&data[at..]) as usize == data.len() - at - 4 {
                header_size = Some(at);
                break;
            }
            at += 4;
        }
        let header_size = header_size.ok_or_else(|| {
            ToolkitError::Parse("nav 3d: no node-size word explains the section length".into())
        })?;
        let f = |o: usize| LE::read_f32(&data[o..]);
        Ok(Self {
            magic,
            header_size,
            node_bytes: data.len() - header_size - 4,
            min: [f(0x04), f(0x08), f(0x0C)],
            extent: [f(0x10), f(0x14), f(0x18)],
            geo_extent: [f(0x1C), f(0x20), f(0x24)],
            min_voxel_radius: f(0x28),
        })
    }
}

/// The four bytes of [`NAV_VERSION_TAG`], if present.
pub fn nav_version_word(zone: &Zone) -> Option<u32> {
    let d = zone.dat1.get_section_data(NAV_VERSION_TAG)?;
    (d.len() >= 4).then(|| LE::read_u32(d))
}

// ---------------------------------------------------------------------------
// Packed output for the viewport
// ---------------------------------------------------------------------------

/// A triangle soup built out of one unit, ready to pack.
#[derive(Debug, Default)]
pub struct NavMeshOut {
    /// 9 floats per triangle: the three corners, de-indexed.
    pub positions: Vec<f32>,
    /// One face normal per corner.
    pub normals: Vec<f32>,
    /// Per corner: `(island / 65535, group index / 65535)`.
    pub uvs: Vec<f32>,
    pub indices: Vec<u32>,
    /// One run per `(layer, connected, poly flags)` class.
    pub runs: Vec<NavMeshRun>,
    pub min: [f32; 3],
    pub max: [f32; 3],
}

#[derive(Debug, Clone, Copy)]
pub struct NavMeshRun {
    pub first_index: u32,
    pub index_count: u32,
    pub layer: u32,
    pub poly_flags: u32,
    pub connected: bool,
}

/// Which polygons a caller wants.
#[derive(Debug, Clone, Copy, Default)]
pub struct NavMeshFilter {
    /// Only this layer, when set.
    pub layer: Option<usize>,
    /// Include the polygons that are not on the connected island.
    pub include_disconnected: bool,
}

/// Build the triangle soup for one unit.
///
/// Triangles are emitted de-indexed (three fresh vertices each) so every corner
/// can carry its own face normal and its own polygon attributes, and they are
/// sorted into runs by `(layer, connected, poly flags)` so the viewport can
/// colour or hide a class without re-reading anything.
pub fn nav_mesh_of(unit: &NavUnit<'_>, filter: NavMeshFilter) -> NavMeshOut {
    let mut buckets: std::collections::BTreeMap<(u32, bool, u32), Vec<[[f32; 3]; 3]>> =
        Default::default();
    let mut attrs: std::collections::BTreeMap<(u32, bool, u32), Vec<[f32; 2]>> = Default::default();

    for g in &unit.groups {
        if g.is_empty() {
            continue;
        }
        if filter.layer.is_some_and(|l| l != g.layer) {
            continue;
        }
        let verts = unit.verts(g);
        if verts.len() != g.vert_count {
            continue;
        }
        let polys = unit.polys(g);
        for (pi, p) in polys.iter().enumerate() {
            let connected = pi < g.poly_count;
            if !connected && !filter.include_disconnected {
                continue;
            }
            if p.verts.iter().any(|&v| v as usize >= verts.len()) {
                continue;
            }
            let tri = [
                verts[p.verts[0] as usize],
                verts[p.verts[1] as usize],
                verts[p.verts[2] as usize],
            ];
            let key = (g.layer as u32, connected, p.flags as u32);
            buckets.entry(key).or_default().push(tri);
            let a = [
                p.island as f32 / 65535.0,
                g.index as f32 / 65535.0,
            ];
            attrs.entry(key).or_default().push(a);
        }
    }

    let mut out = NavMeshOut {
        min: [f32::INFINITY; 3],
        max: [f32::NEG_INFINITY; 3],
        ..Default::default()
    };
    for (key, tris) in &buckets {
        let first_index = out.indices.len() as u32;
        let a = &attrs[key];
        for (t, at) in tris.iter().zip(a.iter()) {
            let e1 = sub(t[1], t[0]);
            let e2 = sub(t[2], t[0]);
            let n = normalise(cross(e1, e2));
            for corner in t {
                let base = out.positions.len() as u32 / 3;
                out.positions.extend_from_slice(corner);
                out.normals.extend_from_slice(&n);
                out.uvs.extend_from_slice(at);
                out.indices.push(base);
                for i in 0..3 {
                    out.min[i] = out.min[i].min(corner[i]);
                    out.max[i] = out.max[i].max(corner[i]);
                }
            }
        }
        out.runs.push(NavMeshRun {
            first_index,
            index_count: out.indices.len() as u32 - first_index,
            layer: key.0,
            poly_flags: key.2,
            connected: key.1,
        });
    }
    if out.positions.is_empty() {
        out.min = [0.0; 3];
        out.max = [0.0; 3];
    }
    out
}

/// Pack as the `"RCMS"` buffer the viewport's `decodeRcms` already reads, so a
/// nav mesh draws through the same path as a model.
///
/// `submesh.materialIndex` is the run's ordinal, `submesh.flags` is
/// `layer << 8 | polyFlags` with bit 0x8000 set when the run is the connected
/// island, and `uv0` is `(island / 65535, groupIndex / 65535)`.
pub fn pack_nav_mesh(m: &NavMeshOut) -> Vec<u8> {
    let vc = m.positions.len() / 3;
    let mut out = Vec::with_capacity(48 + vc * 32 + m.indices.len() * 4 + m.runs.len() * 16);
    out.extend_from_slice(&crate::core::level::MESH_MAGIC.to_le_bytes());
    out.extend_from_slice(&crate::core::level::MESH_VERSION.to_le_bytes());
    out.extend_from_slice(&(vc as u32).to_le_bytes());
    out.extend_from_slice(&(m.indices.len() as u32).to_le_bytes());
    out.extend_from_slice(&(m.runs.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for v in m.min.iter().chain(m.max.iter()) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for v in &m.positions {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for v in &m.normals {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for v in &m.uvs {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for i in &m.indices {
        out.extend_from_slice(&i.to_le_bytes());
    }
    for (i, r) in m.runs.iter().enumerate() {
        out.extend_from_slice(&r.first_index.to_le_bytes());
        out.extend_from_slice(&r.index_count.to_le_bytes());
        out.extend_from_slice(&(i as u32).to_le_bytes());
        let flags = (r.layer << 8) | r.poly_flags | if r.connected { 0x8000 } else { 0 };
        out.extend_from_slice(&flags.to_le_bytes());
    }
    out
}

/// `"RCNL"` -- the boundary-line buffer: header, bounds, then
/// `2 * 3` floats per segment and one `u32` class per segment.
pub const NAV_LINES_MAGIC: u32 = 0x4C4E_4352;

/// Every non-shared edge of a unit as a world-space segment, with its
/// [`EdgeType`] and layer.
pub fn pack_nav_edges(unit: &NavUnit<'_>, filter: NavMeshFilter) -> Vec<u8> {
    let mut pts: Vec<f32> = Vec::new();
    let mut class: Vec<u32> = Vec::new();
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for g in &unit.groups {
        if g.is_empty() {
            continue;
        }
        if filter.layer.is_some_and(|l| l != g.layer) {
            continue;
        }
        let verts = unit.verts(g);
        let edges = unit.edges(g);
        let clues = unit.clues(g);
        let custom = g.edge_range(EdgeType::Custom);
        for (ei, e) in edges.iter().enumerate() {
            let (a, b) = if custom.contains(&ei) {
                // A custom edge's two vertex words are a different union arm;
                // its geometry is the clue's own position.
                match clues.get(ei - custom.start) {
                    Some(c) => (c.position, c.position),
                    None => continue,
                }
            } else {
                let (ia, ib) = (e.vert_a as usize, e.vert_b as usize);
                if ia >= verts.len() || ib >= verts.len() {
                    continue;
                }
                (verts[ia], verts[ib])
            };
            for p in [a, b] {
                pts.extend_from_slice(&p);
                for i in 0..3 {
                    min[i] = min[i].min(p[i]);
                    max[i] = max[i].max(p[i]);
                }
            }
            class.push((g.layer as u32) << 8 | e.edge_type as u32);
        }
    }
    if pts.is_empty() {
        min = [0.0; 3];
        max = [0.0; 3];
    }
    let mut out = Vec::with_capacity(32 + pts.len() * 4 + class.len() * 4);
    out.extend_from_slice(&NAV_LINES_MAGIC.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&(class.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for v in min.iter().chain(max.iter()) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for v in &pts {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for c in &class {
        out.extend_from_slice(&c.to_le_bytes());
    }
    out
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn normalise(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l > 1e-12 {
        [v[0] / l, v[1] / l, v[2] / l]
    } else {
        [0.0, 1.0, 0.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn align_helpers() {
        assert_eq!(a16(0), 0);
        assert_eq!(a16(1), 16);
        assert_eq!(a16(48), 48);
        assert_eq!(a4(5), 8);
    }

    #[test]
    fn group_edge_ranges() {
        let g = NavGroup {
            index: 0,
            layer: 0,
            x: 0,
            z: 0,
            vert_count: 4,
            poly_count: 1,
            disconnected_poly_count: 1,
            edge_counts: [2, 3, 1, 4],
            pad12: 0,
            pad2c: 0,
            vert_offset: 0,
            edge_offset: 0,
            poly_offset: 0,
            clue_offset: 0,
            bsphere_offset: 0,
            data_size: 1,
        };
        assert_eq!(g.edge_count(), 10);
        assert_eq!(g.edge_range(EdgeType::Internal), 0..2);
        assert_eq!(g.edge_range(EdgeType::External), 2..5);
        assert_eq!(g.edge_range(EdgeType::Custom), 5..6);
        assert_eq!(g.edge_range(EdgeType::Border), 6..10);
        // 4 verts, 10 edges, 2 polys, 1 clue, 1 bsphere
        assert_eq!(g.implied_data_size(), 48 + 144 + 48 + 48 + 16);
    }

    #[test]
    fn edge_type_names() {
        assert_eq!(EdgeType::from_u16(3), Some(EdgeType::Border));
        assert_eq!(EdgeType::from_u16(4), None);
        assert_eq!(EdgeType::Custom.name(), "custom");
    }
}
