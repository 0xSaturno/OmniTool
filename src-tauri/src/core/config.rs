use std::collections::HashMap;
use std::io::{Cursor, Read, Seek, SeekFrom};

use byteorder::{LE, ReadBytesExt};
use serde_json::Value;

use crate::core::crc32;
use crate::core::crc64;
use crate::core::dat1::{Dat1, DAT1_MAGIC, SectionHeader};
use crate::core::error::{Result, ToolkitError};

pub const CONFIG_MAGIC: u32 = 0x21A56F68;
pub const CONFIG2_MAGIC: u32 = 0x35F7AFA5;

const CONFIG_TYPE_TAG: u32 = 0x4A128222;
const CONFIG_CONTENT_TAG: u32 = 0xE501186F;
const OBJECT_MAGIC: u32 = 0x03150044;

const NT_UINT8: u8 = 0x00;
const NT_UINT16: u8 = 0x01;
const NT_UINT32: u8 = 0x02;
const NT_INT8: u8 = 0x04;
const NT_INT16: u8 = 0x05;
const NT_INT32: u8 = 0x06;
const NT_FLOAT: u8 = 0x08;
const NT_STRING: u8 = 0x0A;
const NT_OBJECT: u8 = 0x0D;
const NT_BOOLEAN: u8 = 0x0F;
const NT_INSTANCE_ID: u8 = 0x11;
const NT_NULL: u8 = 0x13;

pub struct ConfigFile {
    /// CONFIG_MAGIC, CONFIG2_MAGIC, or DAT1_MAGIC (raw — no wrapper header).
    pub magic: u32,
    /// Preserved only when magic is CONFIG_MAGIC / CONFIG2_MAGIC.
    pub unk: [u8; 28],
    pub config_type: String,
    pub content: Value,
}

impl ConfigFile {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 4 {
            return Err(ToolkitError::Parse("config file too small".into()));
        }

        let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());

        // Raw DAT1: game archives store configs without the 36-byte wrapper.
        if magic == DAT1_MAGIC {
            let dat1 = Dat1::parse(data)?;
            return Self::from_dat1(dat1, DAT1_MAGIC, [0u8; 28]);
        }

        if magic != CONFIG_MAGIC && magic != CONFIG2_MAGIC {
            return Err(ToolkitError::InvalidMagic { expected: CONFIG_MAGIC, got: magic });
        }

        if data.len() < 36 {
            return Err(ToolkitError::Parse("config file too small for wrapper header".into()));
        }

        let mut unk = [0u8; 28];
        unk.copy_from_slice(&data[8..36]);

        let dat1 = Dat1::parse(&data[36..])?;
        Self::from_dat1(dat1, magic, unk)
    }

    fn from_dat1(dat1: Dat1, magic: u32, unk: [u8; 28]) -> Result<Self> {
        let type_data = dat1.get_section_data(CONFIG_TYPE_TAG)
            .ok_or(ToolkitError::SectionNotFound(CONFIG_TYPE_TAG))?;
        let type_obj = deserialize_section(type_data, &dat1)?;
        let config_type = type_obj.get("Type")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string();

        let content_data = dat1.get_section_data(CONFIG_CONTENT_TAG)
            .ok_or(ToolkitError::SectionNotFound(CONFIG_CONTENT_TAG))?;
        let content = deserialize_section(content_data, &dat1)?;

        Ok(Self { magic, unk, config_type, content })
    }

    pub fn save(&self) -> Vec<u8> {
        let mut pool = StringsPool::new();
        pool.add("Config Built File");

        let type_obj = serde_json::json!({ "Type": self.config_type });
        let type_bytes = serialize_section(&type_obj, &mut pool);
        let content_bytes = serialize_section(&self.content, &mut pool);

        let sections_map: HashMap<u32, usize> =
            [(CONFIG_TYPE_TAG, 0), (CONFIG_CONTENT_TAG, 1)].into_iter().collect();

        // dat1.unk1 stores the wrapper magic (for wrapped files) or CONFIG_MAGIC as a label.
        let dat1_unk1 = if self.magic == DAT1_MAGIC { CONFIG_MAGIC } else { self.magic };

        let mut dat1 = Dat1 {
            magic: DAT1_MAGIC,
            unk1: dat1_unk1,
            total_size: 0,
            sections: vec![
                SectionHeader { tag: CONFIG_TYPE_TAG, offset: 0, size: 0 },
                SectionHeader { tag: CONFIG_CONTENT_TAG, offset: 0, size: 0 },
            ],
            unknowns: vec![],
            strings_pool: pool.data,
            section_data: vec![type_bytes, content_bytes],
            sections_map,
        };

        let dat1_bytes = dat1.save();

        if self.magic == DAT1_MAGIC {
            return dat1_bytes;
        }

        // Wrapped format: 4-byte magic + 4-byte dat1 size + 28-byte unk + DAT1.
        let mut out = Vec::with_capacity(36 + dat1_bytes.len());
        out.extend_from_slice(&self.magic.to_le_bytes());
        out.extend_from_slice(&(dat1_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.unk);
        out.extend_from_slice(&dat1_bytes);
        out
    }
}

struct StringsPool {
    data: Vec<u8>,
    offsets: HashMap<String, u32>,
    next_offset: u32,
}

impl StringsPool {
    fn new() -> Self {
        Self { data: Vec::new(), offsets: HashMap::new(), next_offset: 0 }
    }

    fn add(&mut self, s: &str) -> u32 {
        if let Some(&off) = self.offsets.get(s) {
            return off;
        }
        let off = self.next_offset;
        self.offsets.insert(s.to_string(), off);
        self.data.extend_from_slice(s.as_bytes());
        self.data.push(0);
        self.next_offset += (s.len() + 1) as u32;
        off
    }
}

fn deserialize_section(data: &[u8], dat1: &Dat1) -> Result<Value> {
    let mut cur = Cursor::new(data);
    deserialize_object(&mut cur, dat1)
}

fn deserialize_object(cur: &mut Cursor<&[u8]>, dat1: &Dat1) -> Result<Value> {
    let _zero = cur.read_u32::<LE>()?;
    let _magic = cur.read_u32::<LE>()?;
    let children_count = cur.read_u32::<LE>()? as usize;
    let data_len = cur.read_u32::<LE>()? as usize;
    let start = cur.position() as usize;

    let mut descriptors: Vec<(u16, u8)> = Vec::with_capacity(children_count);
    for _ in 0..children_count {
        let _hash = cur.read_u32::<LE>()?;
        let flags = cur.read_u16::<LE>()?;
        let _unk = cur.read_u8()?;
        let node_type = cur.read_u8()?;
        descriptors.push((flags, node_type));
    }

    let mut name_offsets: Vec<u32> = Vec::with_capacity(children_count);
    for _ in 0..children_count {
        name_offsets.push(cur.read_u32::<LE>()?);
    }

    let mut map = serde_json::Map::new();
    for i in 0..children_count {
        let (flags, node_type) = descriptors[i];
        let items_count = (flags >> 4) as usize;
        let name = dat1.get_string(name_offsets[i])
            .unwrap_or_else(|| format!("field_{}", name_offsets[i]));

        let value = if items_count != 1 {
            let mut arr = Vec::with_capacity(items_count);
            for _ in 0..items_count {
                arr.push(deserialize_node(cur, node_type, dat1)?);
            }
            Value::Array(arr)
        } else {
            deserialize_node(cur, node_type, dat1)?
        };

        map.insert(name, value);
    }

    // Align to 4 bytes (absolute position within section data)
    let pos = cur.position();
    let aligned = (pos + 3) & !3;
    if aligned > pos {
        cur.seek(SeekFrom::Start(aligned))?;
    }

    let finish = cur.position() as usize;
    let expected_end = start + data_len;
    if finish < expected_end {
        cur.seek(SeekFrom::Start(expected_end as u64))?;
    }

    Ok(Value::Object(map))
}

fn deserialize_node(cur: &mut Cursor<&[u8]>, node_type: u8, dat1: &Dat1) -> Result<Value> {
    match node_type {
        NT_UINT8 => Ok(Value::Number(cur.read_u8()?.into())),
        NT_UINT16 => Ok(Value::Number(cur.read_u16::<LE>()?.into())),
        NT_UINT32 => Ok(Value::Number(cur.read_u32::<LE>()?.into())),
        NT_INT8 => Ok(Value::Number(cur.read_i8()?.into())),
        NT_INT16 => Ok(Value::Number(cur.read_i16::<LE>()?.into())),
        NT_INT32 => Ok(Value::Number(cur.read_i32::<LE>()?.into())),
        NT_FLOAT => {
            let bits = cur.read_u32::<LE>()?;
            let f = f32::from_bits(bits) as f64;
            Ok(Value::Number(
                serde_json::Number::from_f64(f).unwrap_or(serde_json::Number::from(0)),
            ))
        }
        NT_STRING => deserialize_inline_string(cur),
        NT_OBJECT => deserialize_object(cur, dat1),
        NT_BOOLEAN => Ok(Value::Bool(cur.read_u8()? != 0)),
        NT_INSTANCE_ID => Ok(Value::Number(cur.read_u64::<LE>()?.into())),
        NT_NULL => {
            cur.read_u8()?;
            Ok(Value::Null)
        }
        _ => Err(ToolkitError::Parse(format!("unknown config node type 0x{node_type:02X}"))),
    }
}

fn deserialize_inline_string(cur: &mut Cursor<&[u8]>) -> Result<Value> {
    let length = cur.read_u32::<LE>()? as usize;
    let _crc32 = cur.read_u32::<LE>()?;
    let _crc64 = cur.read_u64::<LE>()?;
    let mut bytes = vec![0u8; length];
    cur.read_exact(&mut bytes)?;
    cur.read_u8()?; // null terminator
    let pos = cur.position();
    let aligned = (pos + 3) & !3;
    if aligned > pos {
        cur.seek(SeekFrom::Start(aligned))?;
    }
    Ok(Value::String(String::from_utf8_lossy(&bytes).into_owned()))
}

fn serialize_section(value: &Value, pool: &mut StringsPool) -> Vec<u8> {
    let mut out = Vec::new();
    serialize_object(value, &mut out, pool);
    out
}

fn serialize_object(obj: &Value, out: &mut Vec<u8>, pool: &mut StringsPool) {
    let map = match obj.as_object() {
        Some(m) => m,
        None => {
            out.extend_from_slice(&[0u8; 4]);
            out.extend_from_slice(&OBJECT_MAGIC.to_le_bytes());
            out.extend_from_slice(&[0u8; 8]);
            return;
        }
    };

    let children: Vec<(&String, &Value, u8, usize, u32)> = map
        .iter()
        .map(|(k, v)| {
            let (node_type, count) = infer_type_and_count(v);
            let str_off = pool.add(k);
            (k, v, node_type, count, str_off)
        })
        .collect();

    let mut inner: Vec<u8> = Vec::new();

    for (k, _, node_type, count, _) in &children {
        let hash = crc32::hash(k);
        let flags: u16 = (*count as u16) << 4;
        inner.extend_from_slice(&hash.to_le_bytes());
        inner.extend_from_slice(&flags.to_le_bytes());
        inner.push(0);
        inner.push(*node_type);
    }

    for (_, _, _, _, str_off) in &children {
        inner.extend_from_slice(&str_off.to_le_bytes());
    }

    for (_, v, node_type, count, _) in &children {
        if *count != 1 {
            if let Some(arr) = v.as_array() {
                for elem in arr {
                    serialize_node(elem, *node_type, &mut inner, pool);
                }
            }
        } else {
            serialize_node(v, *node_type, &mut inner, pool);
        }
    }

    // Pad inner to 4-byte alignment (position relative to inner[0])
    let r = inner.len() % 4;
    if r != 0 {
        inner.resize(inner.len() + (4 - r), 0);
    }

    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&OBJECT_MAGIC.to_le_bytes());
    out.extend_from_slice(&(map.len() as u32).to_le_bytes());
    out.extend_from_slice(&(inner.len() as u32).to_le_bytes());
    out.extend_from_slice(&inner);
}

fn serialize_node(v: &Value, node_type: u8, out: &mut Vec<u8>, pool: &mut StringsPool) {
    match node_type {
        NT_UINT8 => out.push(v.as_u64().unwrap_or(0) as u8),
        NT_UINT16 => out.extend_from_slice(&(v.as_u64().unwrap_or(0) as u16).to_le_bytes()),
        NT_UINT32 => out.extend_from_slice(&(v.as_u64().unwrap_or(0) as u32).to_le_bytes()),
        NT_INT8 => out.push(v.as_i64().unwrap_or(0) as i8 as u8),
        NT_INT16 => out.extend_from_slice(&(v.as_i64().unwrap_or(0) as i16 as u16).to_le_bytes()),
        NT_INT32 => out.extend_from_slice(&(v.as_i64().unwrap_or(0) as i32 as u32).to_le_bytes()),
        NT_FLOAT => {
            let f = v.as_f64().unwrap_or(0.0) as f32;
            out.extend_from_slice(&f.to_bits().to_le_bytes());
        }
        NT_STRING => serialize_inline_string(v.as_str().unwrap_or(""), out),
        NT_OBJECT => serialize_object(v, out, pool),
        NT_BOOLEAN => out.push(if v.as_bool().unwrap_or(false) { 1 } else { 0 }),
        NT_INSTANCE_ID => {
            let u = v.as_u64()
                .unwrap_or_else(|| v.as_i64().unwrap_or(0) as u64);
            out.extend_from_slice(&u.to_le_bytes());
        }
        NT_NULL | _ => out.push(0),
    }
}

fn serialize_inline_string(s: &str, out: &mut Vec<u8>) {
    let c32 = crc32::hash(s);
    let c64 = crc64::hash(s);
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(&c32.to_le_bytes());
    out.extend_from_slice(&c64.to_le_bytes());
    out.extend_from_slice(s.as_bytes());
    out.push(0);
    let r = out.len() % 4;
    if r != 0 {
        out.resize(out.len() + (4 - r), 0);
    }
}

fn infer_type_and_count(v: &Value) -> (u8, usize) {
    match v {
        Value::Array(arr) => {
            if arr.is_empty() {
                (NT_NULL, 0)
            } else {
                (infer_scalar_type(&arr[0]), arr.len())
            }
        }
        other => (infer_scalar_type(other), 1),
    }
}

fn infer_scalar_type(v: &Value) -> u8 {
    match v {
        Value::Null => NT_NULL,
        Value::Bool(_) => NT_BOOLEAN,
        Value::String(_) => NT_STRING,
        Value::Object(_) => NT_OBJECT,
        Value::Array(_) => NT_NULL,
        Value::Number(n) => {
            if n.is_f64() {
                NT_FLOAT
            } else if let Some(u) = n.as_u64() {
                if u <= 0xFF {
                    NT_UINT8
                } else if u <= 0xFFFF {
                    NT_UINT16
                } else if u <= 0xFFFF_FFFF {
                    NT_UINT32
                } else {
                    NT_INSTANCE_ID
                }
            } else if let Some(i) = n.as_i64() {
                if i >= -128 && i <= 127 {
                    NT_INT8
                } else if i >= -32768 && i <= 32767 {
                    NT_INT16
                } else if i >= -(1i64 << 31) && i <= (1i64 << 31) - 1 {
                    NT_INT32
                } else {
                    NT_INSTANCE_ID
                }
            } else {
                NT_FLOAT
            }
        }
    }
}
