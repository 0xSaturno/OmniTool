//! `.zone` reader / editor model for Ratchet & Clank: Rift Apart.
//!
//! A `.zone` asset extracted from the TOC is
//!
//! ```text
//!   [36-byte "Archive TOC Asset Header Data"]   (prepended by Toc::extract_asset)
//!   [block 0]  DAT1 "Zone PID Contents"   <- parsed and edited here
//!   [block 1]  DAT1 "Zone PID Physics"    (48-byte empty DAT1 for gameplay zones)
//!   [block 2]  hlod        (art zones only)
//!   [block 3]  texture     (art zones only)
//! ```
//!
//! The header is `{ u32 type_id, u32 sizes[4], u8 format_data[16] }` and
//! `sum(sizes) == asset size`.
//!
//!
//! ```text
//!   0x06ABCAB2  Zone Scene Objects     byte-packed HEAP of variable-size entries
//!   0x6987F172  Zone Model Insts       u32[] scene-object byte offsets
//!   0xC6A5905E  Zone Model Names       { u64 id[n] }{ u32 string_offset[n] }
//!   0x70682CB8  Zone Actors            32-byte rows
//!   0xDC625B3D  Zone Actor Names       u32[] string offsets
//!   0x78684035  Zone Actor Asset Paths { u64 id[n] }{ u32 string_offset[n] }
//!   0x97FF6EB5  Zone Lights            u32[] scene-object byte offsets
//!   0xBDAB2B0D  Zone Volumes           u32[] scene-object byte offsets
//!   0xBCF43558  Zone Decals            u32[] scene-object byte offsets
//!   0x927C4EC3  Zone Visual Effect Insts u32[] scene-object byte offsets
//! ```
//!
//! `Zone Scene Objects` is *not* an array of fixed-size records: entries have
//! varying lengths and are addressed by byte offset from the tables above.
//! Every entry begins with a 0x80-byte header:
//!
//! ```text
//!   +0x00  f32 m[4][4]   row-major; rows 0..2 = basis, row 3 = translation
//!   +0x5C  u32 flags     HIGH BYTE = entry kind
//!   +0x70  f32 scale[3]
//!   +0x7C  u32 size      low 24 bits = exact byte length of the entry
//!   +0x80  u64 instance id     (kinds 1..6)
//!   +0x100 u64 instance id     (kind 0)
//!   +0x110 u16 model index     (kind 0) -> index into Zone Model Names
//!   +0x112 u16 extra record count (entry size == 0x140 + 0x20 * count)
//! ```
//!
//! Editing is deliberately restricted to **in-place patches of equal length**
//! (transforms and scales). No section ever changes size, so no string offset
//! and no inter-section byte offset can be invalidated, and re-serialising an
//! untouched zone reproduces the original bytes exactly.

use crate::core::dat1::Dat1;
use crate::core::error::{Result, ToolkitError};
use byteorder::{ByteOrder, LE};
use serde::{Deserialize, Serialize};

pub const ZONE_TYPE_ID: u32 = 0x1F390AA0;
pub const ASSET_HEADER_LEN: usize = 36;

pub const T_SCENE_OBJECTS: u32 = 0x06AB_CAB2;
pub const T_MODEL_INSTS: u32 = 0x6987_F172;
pub const T_MODEL_NAMES: u32 = 0xC6A5_905E;
pub const T_ACTORS: u32 = 0x7068_2CB8;
pub const T_ACTOR_NAMES: u32 = 0xDC62_5B3D;
pub const T_ACTOR_REFS: u32 = 0x7868_4035;
pub const T_ACTOR_PRIUSES: u32 = 0x50ED_C53D;
pub const T_LIGHTS: u32 = 0x97FF_6EB5;
pub const T_VOLUMES: u32 = 0xBDAB_2B0D;
pub const T_DECALS: u32 = 0xBCF4_3558;
pub const T_VFX_INSTS: u32 = 0x927C_4EC3;

const OFF_MATRIX: usize = 0x00;
const OFF_FLAGS: usize = 0x5C;
const OFF_SCALE: usize = 0x70;
const OFF_SIZE: usize = 0x7C;
const OFF_INSTANCE_ID: usize = 0x80;
const OFF_INSTANCE_K0: usize = 0x100;
const OFF_MODEL_INDEX: usize = 0x110;
const ENTRY_HEADER_LEN: usize = 0x80;

pub const KIND_MODEL: u8 = 0;
pub const KIND_LIGHT: u8 = 1;
pub const KIND_ACTOR: u8 = 2;
pub const KIND_DECAL: u8 = 3;
pub const KIND_SPLINE: u8 = 4;
pub const KIND_VFX: u8 = 5;

pub fn kind_name(kind: u8) -> &'static str {
    match kind {
        KIND_MODEL => "model",
        KIND_LIGHT => "light",
        KIND_ACTOR => "actor",
        KIND_DECAL => "decal",
        KIND_SPLINE => "spline",
        KIND_VFX => "vfx",
        6 => "unknown6",
        _ => "other",
    }
}

/// One entry of the Zone Scene Objects heap.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneObject {
    /// Byte offset inside the Zone Scene Objects section. This is the entry's identity.
    pub offset: u32,
    pub kind: u8,
    pub kind_name: String,
    pub size: u32,
    pub flags: u32,
    /// Row-major 4x4; row 3 is the translation.
    pub matrix: [f32; 16],
    pub scale: [f32; 3],
    #[serde(serialize_with = "ser_u64_str", deserialize_with = "de_u64_str")]
    pub instance_id: u64,
    /// Index into Zone Model Names, or -1.
    pub model_index: i32,
    /// Resolved model asset id, or 0.
    #[serde(serialize_with = "ser_u64_str", deserialize_with = "de_u64_str")]
    pub model_id: u64,
    /// Model path taken from the zone's own string pool (no external hash list needed).
    #[serde(default)]
    pub model_path: String,
    /// Number of trailing 0x20-byte records on a kind-0 entry.
    #[serde(default)]
    pub extra_count: u16,
    pub name: String,
    pub actor_index: i32,
}

impl SceneObject {
    pub fn position(&self) -> [f32; 3] {
        [self.matrix[12], self.matrix[13], self.matrix[14]]
    }

    pub fn label(&self) -> String {
        if self.name.is_empty() {
            format!("{} @{}", self.kind_name, self.offset)
        } else {
            self.name.clone()
        }
    }
}

/// A 32-byte row of Zone Actors.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorRow {
    /// Row ordinal inside Zone Actors.
    pub index: u32,
    /// Index into Zone Actor Names.
    pub name_index: u32,
    /// Index into Zone Actor Asset Paths.
    pub asset_index: u32,
    pub scene_offset: u32,
    /// First row in Zone Actor Priuses, or -1.
    pub prius_index: i32,
    pub prius_count: u32,
    #[serde(serialize_with = "ser_u64_str")]
    pub instance_id: u64,
    pub name: String,
    #[serde(serialize_with = "ser_u64_str")]
    pub asset_id: u64,
    pub asset_path: String,
}

/// One applied in-place patch, for undo.
#[derive(Debug, Clone)]
pub struct ZoneEdit {
    pub section: u32,
    pub offset: usize,
    pub old: Vec<u8>,
    pub new: Vec<u8>,
    pub what: String,
}

pub struct Zone {
    /// The 36-byte TOC asset header, when the asset carried one.
    pub header: Option<Vec<u8>>,
    pub sizes: [u32; 4],
    pub dat1: Dat1,
    /// Blocks after block 0, carried through verbatim.
    pub tail: Vec<u8>,
    pub scene_objects: Vec<SceneObject>,
    pub actors: Vec<ActorRow>,
    pub model_ids: Vec<u64>,
    /// Model paths parallel to `model_ids`, read from the zone's own string pool.
    pub model_paths: Vec<String>,
    pub edits: Vec<ZoneEdit>,
}

/// Read a "struct of arrays" name table: `{ u64 id[n] }{ u32 string_offset[n] }`.
/// Used by Zone Model Names, Zone Actor Asset Paths and Zone Visual Effect Names.
fn read_soa_table(dat1: &Dat1, tag: u32) -> (Vec<u64>, Vec<String>) {
    let Some(sec) = dat1.get_section_data(tag) else {
        return (Vec::new(), Vec::new());
    };
    let n = sec.len() / 12;
    let mut ids = Vec::with_capacity(n);
    let mut paths = Vec::with_capacity(n);
    for i in 0..n {
        ids.push(LE::read_u64(&sec[i * 8..i * 8 + 8]));
        let so = LE::read_u32(&sec[n * 8 + i * 4..n * 8 + i * 4 + 4]);
        paths.push(
            dat1.get_string(so)
                .unwrap_or_default()
                .replace('\\', "/"),
        );
    }
    (ids, paths)
}

impl Zone {
    /// Parse an asset as extracted by `Toc::extract_asset` (with or without the
    /// 36-byte asset header prepended).
    pub fn parse(asset: &[u8]) -> Result<Self> {
        let (header, body) = split_header(asset)?;
        let sizes = match &header {
            Some(h) => {
                let mut s = [0u32; 4];
                for (i, v) in s.iter_mut().enumerate() {
                    *v = LE::read_u32(&h[4 + i * 4..8 + i * 4]);
                }
                s
            }
            None => [body.len() as u32, 0, 0, 0],
        };

        let block0_len = if sizes[0] as usize <= body.len() && sizes[0] > 0 {
            sizes[0] as usize
        } else {
            body.len()
        };
        let dat1 = Dat1::parse(&body[..block0_len])?;
        if dat1.unk1 != ZONE_TYPE_ID {
            return Err(ToolkitError::Parse(format!(
                "not a zone DAT1 (type {:#010x})",
                dat1.unk1
            )));
        }
        let tail = body[block0_len..].to_vec();

        let mut zone = Zone {
            header,
            sizes,
            dat1,
            tail,
            scene_objects: Vec::new(),
            actors: Vec::new(),
            model_ids: Vec::new(),
            model_paths: Vec::new(),
            edits: Vec::new(),
        };
        zone.build_index();
        Ok(zone)
    }

    fn build_index(&mut self) {
        let scene = match self.dat1.get_section_data(T_SCENE_OBJECTS) {
            Some(s) => s.to_vec(),
            None => return,
        };

        // Zone Model Names is { u64 id[n] }{ u32 string_offset[n] }, not a flat u64 array.
        let (ids, paths) = read_soa_table(&self.dat1, T_MODEL_NAMES);
        self.model_ids = ids;
        self.model_paths = paths;

        let mut offsets: Vec<u32> = Vec::new();
        for tag in [T_MODEL_INSTS, T_LIGHTS, T_VOLUMES, T_DECALS, T_VFX_INSTS] {
            if let Some(sec) = self.dat1.get_section_data(tag) {
                offsets.extend(sec.chunks_exact(4).map(LE::read_u32));
            }
        }

        // Zone Actors: 32-byte rows
        //   { u32 name_index, u32 asset_index, u32 heap_offset, i32 prius_index,
        //     u32 prius_count, u32 (always 8), u64 instance_id }
        let raw_rows: Vec<[u8; 32]> = self
            .dat1
            .get_section_data(T_ACTORS)
            .map(|a| {
                a.chunks_exact(32)
                    .map(|r| {
                        let mut b = [0u8; 32];
                        b.copy_from_slice(r);
                        b
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Zone Actor Names is a plain u32[] of string offsets.
        let name_offsets: Vec<u32> = self
            .dat1
            .get_section_data(T_ACTOR_NAMES)
            .map(|n| n.chunks_exact(4).map(LE::read_u32).collect())
            .unwrap_or_default();
        // Zone Actor Asset Paths is a struct-of-arrays table like Zone Model Names.
        let (actor_ids, actor_paths) = read_soa_table(&self.dat1, T_ACTOR_REFS);

        for (i, r) in raw_rows.iter().enumerate() {
            let name_index = LE::read_u32(&r[0..4]);
            let asset_index = LE::read_u32(&r[4..8]);
            let scene_offset = LE::read_u32(&r[8..12]);
            let prius_index = LE::read_i32(&r[12..16]);
            let prius_count = LE::read_u32(&r[16..20]);
            let instance_id = LE::read_u64(&r[24..32]);

            let name = name_offsets
                .get(name_index as usize)
                .and_then(|&o| self.dat1.get_string(o))
                .unwrap_or_default();
            let asset_id = actor_ids.get(asset_index as usize).copied().unwrap_or(0);
            let asset_path = actor_paths
                .get(asset_index as usize)
                .cloned()
                .unwrap_or_default();

            self.actors.push(ActorRow {
                index: i as u32,
                name_index,
                asset_index,
                scene_offset,
                prius_index,
                prius_count,
                instance_id,
                name,
                asset_id,
                asset_path,
            });
            offsets.push(scene_offset);
        }

        offsets.sort_unstable();
        offsets.dedup();

        for off in offsets {
            let o = off as usize;
            if o + ENTRY_HEADER_LEN > scene.len() {
                continue;
            }
            let mut obj = self.read_object(&scene, o);
            if let Some(ai) = self.actors.iter().position(|a| a.scene_offset == off) {
                obj.actor_index = ai as i32;
                obj.name = self.actors[ai].name.clone();
            }
            self.scene_objects.push(obj);
        }
    }

    fn read_object(&self, scene: &[u8], off: usize) -> SceneObject {
        let flags = LE::read_u32(&scene[off + OFF_FLAGS..off + OFF_FLAGS + 4]);
        let kind = (flags >> 24) as u8;
        let mut size = LE::read_u32(&scene[off + OFF_SIZE..off + OFF_SIZE + 4]) & 0x00FF_FFFF;
        if (size as usize) < ENTRY_HEADER_LEN || off + size as usize > scene.len() {
            size = ENTRY_HEADER_LEN as u32;
        }

        let mut matrix = [0f32; 16];
        LE::read_f32_into(&scene[off + OFF_MATRIX..off + OFF_MATRIX + 64], &mut matrix);
        let mut scale = [0f32; 3];
        LE::read_f32_into(&scene[off + OFF_SCALE..off + OFF_SCALE + 12], &mut scale);

        let mut instance_id = 0u64;
        let mut model_index = -1i32;
        let mut model_id = 0u64;
        let mut model_path = String::new();
        let mut extra_count = 0u16;
        let end = off + size as usize;

        if kind == KIND_MODEL {
            if off + OFF_INSTANCE_K0 + 8 <= end {
                instance_id = LE::read_u64(&scene[off + OFF_INSTANCE_K0..off + OFF_INSTANCE_K0 + 8]);
            }
            // +0x110 is a u16 index and +0x112 a u16 count of trailing 0x20-byte
            // records (entry size == 0x140 + 0x20 * extra_count). Reading the pair
            // as one u32 makes every entry with extras look out of range.
            if off + OFF_MODEL_INDEX + 4 <= end {
                let mi = LE::read_u16(&scene[off + OFF_MODEL_INDEX..off + OFF_MODEL_INDEX + 2]);
                extra_count = LE::read_u16(&scene[off + OFF_MODEL_INDEX + 2..off + OFF_MODEL_INDEX + 4]);
                if let Some(&id) = self.model_ids.get(mi as usize) {
                    model_index = mi as i32;
                    model_id = id;
                    model_path = self.model_paths.get(mi as usize).cloned().unwrap_or_default();
                }
            }
        } else if off + OFF_INSTANCE_ID + 8 <= end {
            instance_id = LE::read_u64(&scene[off + OFF_INSTANCE_ID..off + OFF_INSTANCE_ID + 8]);
        }

        SceneObject {
            offset: off as u32,
            kind,
            kind_name: kind_name(kind).to_string(),
            size,
            flags,
            matrix,
            scale,
            instance_id,
            model_index,
            model_id,
            model_path,
            extra_count,
            name: String::new(),
            actor_index: -1,
        }
    }

    // -- queries ---------------------------------------------------------------------------

    pub fn model_instances(&self) -> impl Iterator<Item = &SceneObject> {
        self.scene_objects
            .iter()
            .filter(|o| o.kind == KIND_MODEL && o.model_id != 0)
    }

    pub fn entities(&self) -> impl Iterator<Item = &SceneObject> {
        self.scene_objects.iter().filter(|o| o.kind != KIND_MODEL)
    }

    /// Axis-aligned bounds over every scene object position.
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let mut it = self.scene_objects.iter();
        let first = it.next()?;
        let mut min = first.position();
        let mut max = min;
        for o in it {
            let p = o.position();
            for i in 0..3 {
                min[i] = min[i].min(p[i]);
                max[i] = max[i].max(p[i]);
            }
        }
        Some((min, max))
    }

    pub fn find(&self, offset: u32) -> Option<&SceneObject> {
        self.scene_objects.iter().find(|o| o.offset == offset)
    }

    // -- editing ---------------------------------------------------------------------------

    /// Patch a scene object's 4x4 transform (row-major, row 3 = translation).
    pub fn set_matrix(&mut self, offset: u32, matrix: [f32; 16]) -> Result<()> {
        let mut buf = [0u8; 64];
        LE::write_f32_into(&matrix, &mut buf);
        self.patch(offset, OFF_MATRIX, &buf, "transform")?;
        if let Some(o) = self.scene_objects.iter_mut().find(|o| o.offset == offset) {
            o.matrix = matrix;
        }
        Ok(())
    }

    /// Patch a scene object's translation only.
    pub fn set_position(&mut self, offset: u32, pos: [f32; 3]) -> Result<()> {
        let obj = self
            .find(offset)
            .ok_or_else(|| ToolkitError::Parse(format!("no scene object at {offset}")))?;
        let mut m = obj.matrix;
        m[12] = pos[0];
        m[13] = pos[1];
        m[14] = pos[2];
        self.set_matrix(offset, m)
    }

    /// Patch a scene object's scale triple.
    pub fn set_scale(&mut self, offset: u32, scale: [f32; 3]) -> Result<()> {
        let mut buf = [0u8; 12];
        LE::write_f32_into(&scale, &mut buf);
        self.patch(offset, OFF_SCALE, &buf, "scale")?;
        if let Some(o) = self.scene_objects.iter_mut().find(|o| o.offset == offset) {
            o.scale = scale;
        }
        Ok(())
    }

    fn patch(&mut self, obj_offset: u32, field: usize, bytes: &[u8], what: &str) -> Result<()> {
        let mut scene = self
            .dat1
            .get_section_data(T_SCENE_OBJECTS)
            .ok_or(ToolkitError::SectionNotFound(T_SCENE_OBJECTS))?
            .to_vec();
        let at = obj_offset as usize + field;
        if at + bytes.len() > scene.len() {
            return Err(ToolkitError::Parse(format!(
                "patch at {at} exceeds scene section ({})",
                scene.len()
            )));
        }
        let old = scene[at..at + bytes.len()].to_vec();
        if old == bytes {
            return Ok(());
        }
        scene[at..at + bytes.len()].copy_from_slice(bytes);
        self.dat1.set_section_data(T_SCENE_OBJECTS, scene)?;
        self.edits.push(ZoneEdit {
            section: T_SCENE_OBJECTS,
            offset: at,
            old,
            new: bytes.to_vec(),
            what: format!("{what} @{obj_offset}"),
        });
        Ok(())
    }

    /// Revert the most recent patch. Returns its description.
    pub fn undo(&mut self) -> Option<String> {
        let e = self.edits.pop()?;
        if let Some(sec) = self.dat1.get_section_data(e.section) {
            let mut scene = sec.to_vec();
            scene[e.offset..e.offset + e.old.len()].copy_from_slice(&e.old);
            let _ = self.dat1.set_section_data(e.section, scene);
        }
        let refreshed = self
            .dat1
            .get_section_data(T_SCENE_OBJECTS)
            .map(|s| s.to_vec())
            .unwrap_or_default();
        for i in 0..self.scene_objects.len() {
            let off = self.scene_objects[i].offset as usize;
            if off + ENTRY_HEADER_LEN <= refreshed.len() {
                let fresh = self.read_object(&refreshed, off);
                self.scene_objects[i].matrix = fresh.matrix;
                self.scene_objects[i].scale = fresh.scale;
            }
        }
        Some(e.what)
    }

    pub fn is_dirty(&self) -> bool {
        !self.edits.is_empty()
    }

    // -- serialisation ---------------------------------------------------------------------

    /// The full asset: 36-byte header (sizes fixed up), block 0, then every
    /// trailing block unchanged.
    pub fn to_bytes(&mut self) -> Vec<u8> {
        let block0 = self.dat1.save();
        let mut out = Vec::with_capacity(ASSET_HEADER_LEN + block0.len() + self.tail.len());
        if let Some(h) = &self.header {
            let mut hdr = h.clone();
            LE::write_u32(&mut hdr[4..8], block0.len() as u32);
            out.extend_from_slice(&hdr);
        }
        out.extend_from_slice(&block0);
        out.extend_from_slice(&self.tail);
        out
    }

    /// Asset bytes without the 36-byte TOC header (what goes into an archive).
    pub fn to_asset_bytes(&mut self) -> Vec<u8> {
        let all = self.to_bytes();
        if self.header.is_some() {
            all[ASSET_HEADER_LEN..].to_vec()
        } else {
            all
        }
    }
}

/// Split an extracted asset into its optional 36-byte header and the body.
fn split_header(asset: &[u8]) -> Result<(Option<Vec<u8>>, &[u8])> {
    if asset.len() < 8 {
        return Err(ToolkitError::Parse("zone asset too small".into()));
    }
    // The body always starts with the DAT1 magic; if the first four bytes are
    // not it, the 36-byte asset header is present.
    let magic = LE::read_u32(&asset[0..4]);
    if magic == crate::core::dat1::DAT1_MAGIC {
        Ok((None, asset))
    } else if asset.len() > ASSET_HEADER_LEN
        && LE::read_u32(&asset[ASSET_HEADER_LEN..ASSET_HEADER_LEN + 4])
            == crate::core::dat1::DAT1_MAGIC
    {
        Ok((
            Some(asset[..ASSET_HEADER_LEN].to_vec()),
            &asset[ASSET_HEADER_LEN..],
        ))
    } else {
        Err(ToolkitError::Parse(
            "zone asset has neither a DAT1 magic nor a 36-byte asset header".into(),
        ))
    }
}

fn ser_u64_str<S: serde::Serializer>(v: &u64, s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_str(&format!("{v:#018X}"))
}

fn de_u64_str<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<u64, D::Error> {
    use serde::Deserialize;
    let s = String::deserialize(d)?;
    let t = s.trim_start_matches("0x").trim_start_matches("0X");
    u64::from_str_radix(t, 16).map_err(serde::de::Error::custom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_names_are_stable() {
        assert_eq!(kind_name(KIND_MODEL), "model");
        assert_eq!(kind_name(KIND_ACTOR), "actor");
    }
}
