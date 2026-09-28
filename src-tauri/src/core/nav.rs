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
//! | `0x3D8DBDB8` | 4 B    | 916   | four bytes, `0x00000000` on all 916 (see [`NAV_VERSION_TAG`]) |
//!
//! # What is decoded, and what is only framed
//!
//! An earlier version of this doc said the unit section was decoded
//! "completely". It is not, and this is the honest split:
//!
//! | | state |
//! |---|---|
//! | unit: header, group grid, vertices, polygons, edges, bounding spheres, clues | **decoded and measured**, see below |
//! | unit: `+0x04`, the group record's two padding words | read, meaning **not established** |
//! | container: header, bounds, slot array length | **decoded**, both shapes, see [`NavContainer`] |
//! | tile cache: stream framing, per-tile `dtTileCacheLayerHeader` | **framed**: 363,701 tiles located, each one's magic, version, tile coordinates and 16 m AABB read |
//! | tile cache: the compressed payload after each header | **not unpacked** -- a Detour height/area/connection layer that only a rebuild needs |
//! | 3D: the magic and the byte length of the node array | **framed** |
//! | 3D: the built-region header's fields and the octree nodes | **not decoded**. [`Nav3D`] recovers `sizeof(built region) == 160` on 152/152 by finding the word that accounts for the rest of the section, but the float triples it exposes are a guess: they read as plausible world metres on 0/152 |
//!
//! # Does it sit on the world? The art cross-check
//!
//! A nav decode that produces a pretty mesh in the wrong place is worse than no
//! decode, so the test that matters is geometric: take the art of the *same*
//! `tile_<col><row>` directory, plus its eight neighbours because a nav vertex
//! on a tile edge belongs to the neighbour's floor, and ask how far each nav
//! polygon is from the drawn floor under it. `nav_probe art` does exactly that,
//! against a control that keeps the same art and the same vertices but slides
//! every probe 48 m in X and Z first, and a second control that keeps the XZ and
//! takes the probe height from a different polygon of the same unit.
//!
//! Sampling every 13th of the 795 eligible tile units, 31 of the 60 picked
//! produced a test -- the rest have no vertex in them, or no art zone anywhere in
//! their 3x3 tile block. Those 31 gave 192,224 vertices and 14,712 reachable
//! polygons against 271,471,664 art triangles, and `|dy|` is the distance from
//! the probe to the nearest art surface directly above or below it:
//!
//! | probe | n | median | p90 | <= 0.1 m | <= 0.5 m | <= 2 m |
//! |---|---|---|---|---|---|---|
//! | **reachable polygon centroid** | 13,856 | **0.100 m** | 1.11 m | **50.2%** | **82.9%** | 93.2% |
//! | the same, slid 48 m in X and Z | 11,870 | 1.78 m | 34.3 m | 7.3% | 24.3% | 52.7% |
//! | the same, height taken from another polygon | 13,856 | 1.12 m | 12.7 m | 13.0% | 34.6% | 62.4% |
//! | vertex | 132,942 | 0.35 m | 126 m | 25.6% | 52.3% | 58.3% |
//! | unreachable polygon centroid | 87,302 | 0.68 m | 133 m | 24.7% | 48.3% | 53.8% |
//! | the same, slid 48 m | 79,008 | 19.8 m | 154 m | 1.8% | 6.6% | 18.0% |
//!
//! **The decode sits on the world.** A reachable polygon's centre is a tenth of
//! a metre from the drawn floor at the median and inside half a metre four times
//! out of five; destroy the correspondence in XZ and the median goes to 1.8 m,
//! destroy it in Y alone and it goes to 1.1 m, and the fraction inside a tenth of
//! a metre falls from 50% to 7% and 13%. A separate run over 69 tile units and
//! 29,153 centroids gave the same answer -- median 0.105 m, 84.6% inside half a
//! metre -- so this is not one lucky sample.
//!
//! Three caveats, because the table has honest limits:
//!
//! * **A vertex is a much worse probe than a centroid**, and that is geometry,
//!   not decoding: Recast puts a boundary vertex on a voxel edge, so a vertex on
//!   the rim of the walkable area routinely overhangs a cliff and its nearest art
//!   is the canyon floor a hundred metres down. That is the whole p90 of 126 m.
//! * **Coverage is not 100%** -- art lies over or under only 94.2% of centroids.
//!   Part of that is genuine holes in the drawn art, and part is that
//!   [`crate::core::level::model_mesh_bytes`] returns nothing for a
//!   `CollisionOnly` model while navigation is built from collision geometry. A
//!   sharper version of this test would use the models' own `WalkableArea` bit.
//! * **The nav surface sits about 18 cm above the art**: the mean signed offset
//!   over the 12,920 centroids inside 2 m is -0.180 m (art Y minus nav Y). That
//!   is Recast's cell height, and it is why a viewport should draw the nav mesh
//!   with a small `+Y` bias or with depth writes off rather than fighting the
//!   floor for the same pixels.
//!
//! The earlier version of this test scored 0 on both the real case and its
//! control, which proved nothing; the reasons were that it picked its zones from
//! the head of a path-sorted list (so it tested instance containers, not tiles),
//! loaded at most four art zones, and folded "is there art here at all" together
//! with "how far is it" into one windowed search. All three are fixed.
//!
//! # The unit section, byte for byte
//!
//! ```text
//! +0x00  u32   magic          == NAV_UNIT_MAGIC on 673/673 shipped units
//! +0x04  u64   an asset id    NOT this zone's own on any of the 673; see below
//! +0x0C  u64   container id   in the same namespace; see below
//! +0x14  i16   min tile x     inclusive, in TILE units (metres / 16)
//! +0x16  i16   min tile z
//! +0x18  i16   max tile x     exclusive
//! +0x1A  i16   max tile z
//! +0x1C  i16   log2 tile size == 4, i.e. TILE_SIZE_M, on 673/673
//! +0x1E  --    two bytes of alignment padding
//! +0x20  ..    layers * (max_x - min_x) * (max_z - min_z) group records of
//!              GROUP_STRIDE bytes, layer-major then z then x
//!        ..    16-byte aligned, then one payload run per non-empty group
//! ```
//!
//! `+0x04` and `+0x0C` are the two 8-byte words the nav builder calls the unit's
//! and the container's asset id, and on this build **neither resolves**:
//!
//! * `+0x04` equals this zone's own TOC id on 0/673.
//! * `+0x0C` equals the TOC id of the region's `containernavdata.zone` on 0/623.
//! * Neither word names *any* asset in the TOC, on 0/673 each.
//!
//! They are internally consistent -- `+0x04 == +0x0C` on exactly 107 of the 673,
//! which is exactly the 107 single-unit regions, whose unit and container are the
//! same thing -- so the fields are being read at the right offsets. They are
//! simply not keys in the namespace the PC TOC uses. Nothing in this module
//! depends on either word, and a caller should not treat them as asset ids.
//!
//! The header does **not** store the layer count. The region's container does --
//! `layer_count` is 2 on 116/116 -- so [`NavUnit::from_zone`] takes it from the
//! container section whenever the zone has one, and [`NavUnit::parse`] otherwise
//! falls back to [`infer_layers`], which picks the one candidate in `1..=16`
//! whose group array leaves the payload run starting exactly at the next 16-byte
//! boundary and the payload sizes summing to exactly the rest of the section.
//! The inference answers 2 on 673/673, agreeing with every container.
//!
//! A group record:
//!
//! ```text
//! +0x00  u16   group x        world metres + GROUP_BIAS, always a multiple of TILE_SIZE_M
//! +0x02  u16   group z        ditto
//! +0x04  u16   vertex count
//! +0x06  u16   reachable polygon count
//! +0x08  u16   unreachable polygon count
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
//! `a16(12 V) + a16(14 E) + a16(24 (R + U)) + a16(40 C) + a16(16 R)`. That
//! identity is what pins the element sizes down, and it holds on every one of
//! the 254,966 non-empty groups; see `nav_probe sizes`. Shipped totals over the
//! 673 units: 254,966 groups with a payload and 365,930 empty, 5,187,949
//! vertices, 736,943 reachable and 2,885,502 unreachable polygons, and
//! 588,875 / 606,209 / 8,798 / 426,870 edges of the four types.
//!
//! Vertices are `f32 x, y, z` in the **region's** space -- the same space the
//! zone's scene objects are authored in, not a per-group local space -- and
//! every one of the 5,187,949 falls inside its own group's 16 m column.
//!
//! # Two classes of polygon, and only one of them is walkable
//!
//! A group's polygon array is `reachable + unreachable` [`NavPoly`] records: the
//! ones the nav builder flood-filled an island id into, followed by the ones it
//! did not. The second class is the bigger one -- 2,885,502 against 736,943 --
//! and it is *not* the walkable surface:
//!
//! | | reachable | unreachable |
//! |---|---|---|
//! | `island` | non-zero on 736,943/736,943 | zero on 2,885,502/2,885,502 |
//! | `flags` (bot classes) | 1 on 736,033, 0 on 582, 2 on 325, 4 on 3 | 0 on all |
//! | `edges` in range | 736,943/736,943 | 998,433/2,885,502 -- the field is not maintained |
//! | bounding sphere | one each, containing all three verts on 736,943/736,943 | none |
//! | indexes vertices | the low part of the group's array | a disjoint higher part, on 98,697/98,777 groups |
//!
//! `nav_probe tails` measures the two against each other inside the same group.
//! Over 109 units, 255,837 reachable and 1,576,373 unreachable triangles: the
//! unreachable class covers **10.4x** the XZ-projected area, and an unreachable
//! centroid lands inside a reachable triangle in XZ on only 252,603/1,576,373
//! (16.0%) -- and where it does, the two surfaces are a median **29.5 m** apart
//! in Y. So the second class is mostly different ground, and where it is over
//! the same ground it is a different storey. It is not a height-detail
//! subdivision of the first, which was the other candidate.
//!
//! Geometrically it is real -- see the cross-check above, where it separates
//! from its control too -- it simply is not reachable and carries no adjacency,
//! so presenting it as navigation would be a lie.
//! [`NavMeshFilter::include_disconnected`] keeps it in runs of its own so a
//! caller can colour it differently; it is never merged into the walkable
//! surface.
//!
//! One consequence a caller has to plan for: **318 of the 673 units have no
//! reachable polygon at all**, while every one of the 673 has vertices and
//! unreachable ones. `levels/i29/instance/ardolis_passiveshift` is the extreme
//! case, 405,581 vertices and 243,077 polygons, none reachable. That is the
//! shipped data, not a decode failure: island ids are flood-filled from authored
//! seeds, so ground no seed reaches keeps island 0. A viewport that draws only
//! the reachable class draws nothing on nearly half the nav zones.
//!
//! Polygons are triangles, and their winding gives a `+Y` normal on
//! 736,932/736,943 reachable and 2,885,292/2,885,502 unreachable; 3 and 37
//! respectively are degenerate. Edges are half-edges carrying the neighbour they
//! cross to ([`NavEdge`]): an internal edge's target is in its own group on
//! 588,875/588,875, a border edge has no target on 426,870/426,870, and an
//! external edge's target is a 4-neighbour group or the group itself on
//! 606,209/606,209. An edge's owning polygon lists it back on
//! 1,621,954/1,630,752 -- the 8,798 that do not are exactly the custom edges,
//! whose vertex words are a different union arm.
//!
//! # Clues, and the field order that was wrong
//!
//! A custom edge is an authored link out of the mesh and its geometry lives in a
//! 40-byte [`NavClue`] instead of in the edge. The position is at the **end** of
//! that record, not the start. Reading it from `+0x00`, which this module used to
//! do, put the actor uid's bits into the position; the tell was that the
//! "position" fell inside its own group's 16 m column on 14 of 8,798 clues. With
//! the order [`NavClue`] documents it falls there on **8,444/8,798** and within
//! one tile of it on 8,798/8,798, and the
//! clues' actor uids name a real scene-object instance id on **4,808 of 4,972**
//! distinct uids (96.7%), checked against all 1,706,513 instance ids in the
//! shipped zones -- an independent confirmation that the two `u64`s at `+0x00`
//! and `+0x08` are uids and that the record is aligned correctly.
//!
//! # Getting it on screen
//!
//! [`crate::commands_level_nav`] wraps all of this for the front end:
//! [`pack_nav_mesh`] emits the same `"RCMS"` buffer the model path already
//! decodes, and [`pack_nav_edges`] emits an `"RCNL"` line buffer for the mesh
//! rim and the clue links.

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
/// position in the array on all 1,630,752 edges of the shipped build, which is
/// how the count order was fixed.
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
/// The first [`NavGroup::poly_count`] of a group's array are the **reachable**
/// polygons and carry every field below; the
/// [`NavGroup::disconnected_poly_count`] after them are the ground the builder
/// rasterised and found no island for, and on those only `verts` is meaningful
/// -- `island` and `flags` are zero and `edges` is stale. See the module doc for
/// the measurements that separate them.
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
/// +0x00  u64     the clue actor's uid
/// +0x08  u64     the uid of the actor at the far end, 0 when the builder could
///                not resolve one
/// +0x10  u32     required-ability mask: 0x4000 on 6,327 clues, 0 on 2,457,
///                0x4800 on 14, so bit 14 and bit 11 are the only bits used
/// +0x14  u32     required entity, 0 on 2,457 of 8,798
/// +0x18  f32     a distance, FLT_MAX on 3,638 of 8,798
/// +0x1C  f32[3]  position
/// ```
///
/// The position is at the **end** of the record, not the start. Reading it from
/// `+0x00` -- which this module used to do -- put the actor uid's bits in the
/// position and the position's X and Y in the target uid, and the tell was that
/// the "position" landed inside its own group's 16 m column on **14** of 8,798
/// clues. With the layout above it lands there on **8,444/8,798** (96.0%), and
/// inside that column plus one tile of slack on **8,798/8,798**; the 354 that sit
/// just outside are clue actors standing over the edge they link across, which is
/// what a link is.
#[derive(Debug, Clone, Copy)]
pub struct NavClue {
    pub actor_uid: u64,
    pub target_actor_uid: u64,
    pub required_ability: u32,
    pub required_entity: u32,
    /// `FLT_MAX` on most shipped clues: the cost is resolved at runtime.
    pub distance: f32,
    pub position: [f32; 3],
}

impl NavClue {
    pub fn read(d: &[u8]) -> Self {
        Self {
            actor_uid: LE::read_u64(&d[0x00..]),
            target_actor_uid: LE::read_u64(&d[0x08..]),
            required_ability: LE::read_u32(&d[0x10..]),
            required_entity: LE::read_u32(&d[0x14..]),
            distance: LE::read_f32(&d[0x18..]),
            position: [
                LE::read_f32(&d[0x1C..]),
                LE::read_f32(&d[0x20..]),
                LE::read_f32(&d[0x24..]),
            ],
        }
    }
}

/// `Intersection::Sphere` -- one per **reachable** polygon, in the same order.
/// The unreachable tail of the polygon array has none, which is one of the
/// things that tells the two classes apart. 16 bytes: centre then radius, and
/// the radius is the distance to the farthest of the polygon's own three
/// vertices to within a millimetre on 736,943/736,943 shipped spheres.
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
    /// Reachable polygons: the walkable surface. They come first in the polygon
    /// array and they are the only ones with a bounding sphere.
    pub poly_count: usize,
    /// The unreachable remainder of the array. Real triangles, but island 0,
    /// flags 0, no adjacency and no sphere; see the module doc.
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
    /// Reachable plus unreachable: the length of the polygon array.
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

    /// Parse the section, recovering the layer count from the section's own
    /// geometry. Fails only on a truncated or non-magic buffer, or when no
    /// layer count in `1..=16` explains the section's length.
    ///
    /// Prefer [`Self::parse_with_layers`] or [`Self::from_zone`] when the
    /// region's container is to hand: the container stores the count outright.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        Self::parse_inner(data, None)
    }

    /// Parse with the layer count the region's container states. The count is
    /// checked against the section's length the same way the inference is, and
    /// a count that does not fit is refused rather than silently used.
    pub fn parse_with_layers(data: &'a [u8], layers: usize) -> Result<Self> {
        Self::parse_inner(data, Some(layers))
    }

    fn parse_inner(data: &'a [u8], given: Option<usize>) -> Result<Self> {
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
        let layers = match given {
            Some(l) if layer_count_fits(data, cells, l) => l,
            Some(l) => {
                return Err(ToolkitError::Parse(format!(
                    "nav unit: the container says {l} layers, which does not fit a {cells}-cell grid in {} bytes",
                    data.len()
                )))
            }
            None => infer_layers(data, cells).ok_or_else(|| {
                ToolkitError::Parse(format!(
                    "nav unit: no layer count in 1..={MAX_LAYERS} fits a {cells}-cell grid in {} bytes",
                    data.len()
                ))
            })?,
        };

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

    /// Pull a section out of a zone and parse it, using the zone's own container
    /// section for the layer count when there is one. A single-unit region
    /// writes both sections into the same zone, which covers the 107 regions
    /// where the mesh and the container ship together; a tiled region's unit
    /// zones carry no container and fall back to the inference.
    pub fn from_zone(zone: &'a Zone) -> Result<Self> {
        let d = zone
            .dat1
            .get_section_data(NAV_UNIT_TAG)
            .ok_or_else(|| ToolkitError::Parse("zone has no nav unit section".into()))?;
        let layers = zone
            .dat1
            .get_section_data(NAV_CONTAINER_TAG)
            .and_then(|c| NavContainer::parse(c).ok())
            .and_then(|c| usize::try_from(c.layer_count).ok());
        match layers {
            Some(l) => Self::parse_with_layers(d, l).or_else(|_| Self::parse(d)),
            None => Self::parse(d),
        }
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

/// Does a candidate layer count account for every byte of the section?
///
/// The group array is `layers * cells` records; the payload run starts at the
/// next 16-byte boundary after it, the first payload starts exactly there, and
/// the payload sizes must tile the rest of the section without a gap.
pub fn layer_count_fits(data: &[u8], cells: usize, layers: usize) -> bool {
    if cells == 0 {
        return layers == 0 && data.len() <= a16(GROUP_ARRAY_OFFSET);
    }
    if layers == 0 {
        return false;
    }
    let n = layers * cells;
    let array_end = GROUP_ARRAY_OFFSET + GROUP_STRIDE * n;
    if array_end > data.len() {
        return false;
    }
    let base = a16(array_end);
    let mut first = usize::MAX;
    let mut sum = 0usize;
    for i in 0..n {
        let o = GROUP_ARRAY_OFFSET + GROUP_STRIDE * i;
        let size = LE::read_u32(&data[o + 0x28..]) as usize;
        if size == 0 {
            continue;
        }
        let at = o + LE::read_u32(&data[o + 0x14..]) as usize;
        if at < base || at.saturating_add(size) > data.len() {
            return false;
        }
        first = first.min(at);
        sum += size;
    }
    if sum == 0 {
        return base == data.len();
    }
    first == base && base + sum == data.len()
}

/// The layer count the section's own geometry implies, for a unit whose
/// container is not to hand. Exactly one candidate in `1..=16` satisfies
/// [`layer_count_fits`] on every shipped unit, and the answer agrees with the
/// container's stated `layer_count` of 2 on all of them.
pub fn infer_layers(data: &[u8], cells: usize) -> Option<usize> {
    if cells == 0 {
        // A unit with an empty grid: the section is header-only.
        return if data.len() <= a16(GROUP_ARRAY_OFFSET) {
            Some(0)
        } else {
            None
        };
    }
    (1..=MAX_LAYERS).find(|&l| layer_count_fits(data, cells, l))
}

// ---------------------------------------------------------------------------
// Container
// ---------------------------------------------------------------------------

/// A parsed [`NAV_CONTAINER_TAG`] section: the per-region nav meta and the
/// runtime lookup table over the region's units.
///
/// ```text
/// +0x00  u32    magic == NAV_UNIT_MAGIC
/// +0x04  u64    an asset id in the same unresolved namespace as the unit
/// +0x0C  i32    layer count
/// +0x10  i16    min x, inclusive    the lookup grid over UNITS
/// +0x12  i16    min z
/// +0x14  i16    max x, exclusive
/// +0x16  i16    max z
/// +0x18  i16    log2 of the unit size in metres, or -1
/// +0x1A  --     six bytes of padding to an 8-byte boundary
/// +0x20  u64[]  zeroed slots, one per unit the runtime can load
/// ```
///
/// # The two shapes, and why -1 is not a bad read
///
/// A region is built either as a grid of units or as a single unit, and the two
/// cases write the same twelve bytes to mean different things:
///
/// * **tiled** -- `log_scale >= 0`. The bounds count units of `1 << log_scale`
///   metres and the slot array is `layer_count * nx * nz` long. 9 of the 116
///   shipped containers, all with `log_scale == 7`, i.e. 128 m units.
/// * **single unit** -- `log_scale == -1`, a deliberate sentinel: the whole
///   region is one unit and its mesh is written into this same zone rather than
///   into `unitnavdata` zones of its own. The bounds are then plain **world
///   metres**, rounded out to [`TILE_SIZE_M`], and the slot array is
///   `layer_count` long -- one slot per layer, not one per metre-cell. 107 of
///   the 116 shipped containers.
///
/// Reading the second case as the first is what made an earlier version of this
/// module report a slot count six orders of magnitude out, and call `log_scale`
/// a bad read: a 2.4 km region reads as 151 x 164 *metre* cells times 2 layers
/// -- 49,528 slots -- when the file holds 2. [`Self::implied_slots`] now
/// distinguishes the two shapes. It matches the stored length exactly on all 9
/// tiled containers (1,334, 2,886, 4,100 slots and so on) and on 85 of the 107
/// single-unit ones; the remaining 22 store **one** slot where `layer_count` is
/// 2. Nothing in this module reads the slot array -- it ships zero-filled and
/// the runtime fills it with pointers -- so its length is reported and not
/// relied on.
///
/// `layer_count` is 2 on all 116, and it is the number the unit header does not
/// store -- so a container is the way to avoid inferring it; see
/// [`NavUnit::parse_with_layers`].
#[derive(Debug, Clone, Copy)]
pub struct NavContainer {
    pub magic: u32,
    pub asset_id: u64,
    pub layer_count: i32,
    /// Inclusive. Units of `2 << log_scale` metres when tiled, world metres
    /// when not; [`Self::bounds_metres`] resolves it either way.
    pub min_tile: [i16; 2],
    /// Exclusive, same units as [`Self::min_tile`].
    pub max_tile: [i16; 2],
    /// `log2` of the unit size in metres, or `-1` for a single-unit region.
    pub log_scale: i16,
    /// The stored slot array's length, in `u64`s.
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

    /// Was the region built as a grid of units, or as one unit whose mesh lives
    /// in this same zone?
    pub fn is_tiled(&self) -> bool {
        self.log_scale >= 0
    }

    /// The size of one unit, in metres, or `None` for a single-unit region.
    pub fn unit_size_m(&self) -> Option<i32> {
        self.is_tiled().then(|| 1i32 << self.log_scale)
    }

    /// The lookup grid's extent in world metres, inclusive then exclusive.
    pub fn bounds_metres(&self) -> ([i32; 2], [i32; 2]) {
        let s = self.unit_size_m().unwrap_or(1);
        (
            [self.min_tile[0] as i32 * s, self.min_tile[1] as i32 * s],
            [self.max_tile[0] as i32 * s, self.max_tile[1] as i32 * s],
        )
    }

    /// Units along X and Z. `(1, 1)` for a single-unit region.
    pub fn unit_grid(&self) -> (usize, usize) {
        if !self.is_tiled() {
            return (1, 1);
        }
        (
            (self.max_tile[0] - self.min_tile[0]).max(0) as usize,
            (self.max_tile[1] - self.min_tile[1]).max(0) as usize,
        )
    }

    /// The slot count the header implies: one per unit per layer when tiled, one
    /// per layer when the region is a single unit. Equal to [`Self::slots`] on
    /// 9/9 tiled and 85/107 single-unit shipped containers; the 22 that differ
    /// store one slot for two layers. Compare the two, do not assume them equal.
    pub fn implied_slots(&self) -> usize {
        let (nx, nz) = self.unit_grid();
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
/// which a `u32` reads as exactly the bytes remaining after it. That works on
/// 152/152 shipped sections and always lands on 160, so `sizeof(built region)`
/// is settled.
///
/// **Nothing inside those 160 bytes is.** [`Self::min`], [`Self::extent`],
/// [`Self::geo_extent`] and [`Self::min_voxel_radius`] are read as the first
/// three float triples and the float after them, which is a guess at the field
/// order; they read as plausible world metres on 0/152, so the guess is wrong
/// and the fields are kept only because the probe prints them while looking for
/// the real order. Do not build anything on them. The node array itself is not
/// decoded at all: `node_bytes` is its length and that is all this module
/// claims.
#[derive(Debug, Clone, Copy)]
pub struct Nav3D {
    pub magic: u32,
    pub header_size: usize,
    pub node_bytes: usize,
    /// **Unverified.** Read as the first three float triples after the magic on
    /// are not plausible world metres on any of the 152 shipped sections, so the
    /// field order is wrong and these are diagnostics, not data.
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
    /// One run per `(layer, reachable, poly flags)` class.
    pub runs: Vec<NavMeshRun>,
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl NavMeshOut {
    /// A mesh with nothing in it, so a caller with no unit section can still
    /// hand the viewport a well-formed buffer.
    pub fn empty() -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct NavMeshRun {
    pub first_index: u32,
    pub index_count: u32,
    pub layer: u32,
    pub poly_flags: u32,
    /// True for the reachable class: the walkable surface. False for the
    /// unreachable remainder, which a caller must not present as navigation.
    pub connected: bool,
}

/// Which polygons a caller wants.
#[derive(Debug, Clone, Copy, Default)]
pub struct NavMeshFilter {
    /// Only this layer, when set.
    pub layer: Option<usize>,
    /// Emit the unreachable class as well, in runs of its own. Worth setting:
    /// 318 of the 673 shipped units have no reachable polygon at all, so a
    /// viewport that leaves this off shows nothing on nearly half of them.
    pub include_disconnected: bool,
}

/// Build the triangle soup for one unit.
///
/// Triangles are emitted de-indexed (three fresh vertices each) so every corner
/// can carry its own face normal and its own polygon attributes, and they are
/// sorted into runs by `(layer, reachable, poly flags)` so the viewport can
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
/// `layer << 8 | polyFlags` with bit 0x8000 set when the run is the reachable
/// class, and `uv0` is `(island / 65535, groupIndex / 65535)`.
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

/// Every edge of a unit that has two vertices, as a world-space segment with
/// its [`EdgeType`] and layer.
///
/// Custom edges are **left out**: their two vertex words are a different union
/// arm, so there is no segment to draw. The link they stand for is a point, and
/// it comes from the clue instead -- `level_nav_clues`, or [`NavUnit::clues`].
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
        let custom = g.edge_range(EdgeType::Custom);
        for (ei, e) in edges.iter().enumerate() {
            if custom.contains(&ei) {
                continue;
            }
            let (a, b) = {
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

/// A well-formed `"RCNL"` buffer with no segments.
pub fn pack_nav_edges_empty() -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    out.extend_from_slice(&NAV_LINES_MAGIC.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for _ in 0..6 {
        out.extend_from_slice(&0f32.to_le_bytes());
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
    fn container_slot_shapes() {
        // A single-unit container: log scale -1, bounds in metres, one slot per
        // layer. 2 layers over a 400 x 400 m region is 2 slots, not 160,000.
        let single = NavContainer {
            magic: NAV_UNIT_MAGIC,
            asset_id: 1,
            layer_count: 2,
            min_tile: [-144, -176],
            max_tile: [256, 256],
            log_scale: -1,
            slots: 2,
        };
        assert!(!single.is_tiled());
        assert_eq!(single.unit_size_m(), None);
        assert_eq!(single.unit_grid(), (1, 1));
        assert_eq!(single.implied_slots(), 2);
        assert_eq!(single.bounds_metres(), ([-144, -176], [256, 256]));

        // A tiled container: bounds count 128 m units.
        let tiled = NavContainer {
            log_scale: 7,
            min_tile: [-9, -13],
            max_tile: [20, 10],
            ..single
        };
        assert!(tiled.is_tiled());
        assert_eq!(tiled.unit_size_m(), Some(128));
        assert_eq!(tiled.unit_grid(), (29, 23));
        assert_eq!(tiled.implied_slots(), 2 * 29 * 23);
        assert_eq!(tiled.bounds_metres(), ([-1152, -1664], [2560, 1280]));
    }

    /// A unit section with `cells` cells per layer, `layers` layers and one
    /// non-empty group holding `payload` bytes right after the group array.
    fn synth_unit(cells: usize, layers: usize, payload: usize) -> Vec<u8> {
        let n = layers * cells;
        let array_end = GROUP_ARRAY_OFFSET + GROUP_STRIDE * n;
        let base = a16(array_end);
        let mut d = vec![0u8; base + payload];
        d[0x00..0x04].copy_from_slice(&NAV_UNIT_MAGIC.to_le_bytes());
        // min tile (0, 0), max tile (3, 2): 6 cells per layer
        d[0x18..0x1A].copy_from_slice(&3i16.to_le_bytes());
        d[0x1A..0x1C].copy_from_slice(&2i16.to_le_bytes());
        d[0x1C..0x1E].copy_from_slice(&4i16.to_le_bytes());
        // one group, the first, owns the whole payload
        let o = GROUP_ARRAY_OFFSET;
        d[o + 0x14..o + 0x18].copy_from_slice(&((base - o) as u32).to_le_bytes());
        d[o + 0x28..o + 0x2C].copy_from_slice(&(payload as u32).to_le_bytes());
        d
    }

    #[test]
    fn layer_count_is_pinned_by_the_section_length() {
        // 6 cells, 2 layers: only 2 accounts for every byte.
        let d = synth_unit(6, 2, 64);
        assert!(layer_count_fits(&d, 6, 2));
        assert!(!layer_count_fits(&d, 6, 1));
        assert!(!layer_count_fits(&d, 6, 3));
        assert_eq!(infer_layers(&d, 6), Some(2));
        // and the parse agrees, with and without being told
        let u = NavUnit::parse(&d).expect("parse");
        assert_eq!(u.layers, 2);
        assert_eq!(NavUnit::parse_with_layers(&d, 2).unwrap().layers, 2);
        assert!(NavUnit::parse_with_layers(&d, 1).is_err());
    }

    #[test]
    fn packed_buffers_have_the_documented_header() {
        let empty = pack_nav_mesh(&NavMeshOut::empty());
        assert_eq!(empty.len(), 48, "header plus bounds, nothing else");
        assert_eq!(
            u32::from_le_bytes(empty[0..4].try_into().unwrap()),
            crate::core::level::MESH_MAGIC
        );
        assert_eq!(
            u32::from_le_bytes(empty[4..8].try_into().unwrap()),
            crate::core::level::MESH_VERSION
        );
        for at in [8, 12, 16, 20] {
            assert_eq!(u32::from_le_bytes(empty[at..at + 4].try_into().unwrap()), 0);
        }

        let lines = pack_nav_edges_empty();
        assert_eq!(lines.len(), 40);
        assert_eq!(
            u32::from_le_bytes(lines[0..4].try_into().unwrap()),
            NAV_LINES_MAGIC
        );
        assert_eq!(u32::from_le_bytes(lines[8..12].try_into().unwrap()), 0);
    }

    #[test]
    fn edge_type_names() {
        assert_eq!(EdgeType::from_u16(3), Some(EdgeType::Border));
        assert_eq!(EdgeType::from_u16(4), None);
        assert_eq!(EdgeType::Custom.name(), "custom");
    }
}
