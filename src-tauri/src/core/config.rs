use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read, Seek, SeekFrom};

use byteorder::{ReadBytesExt, LE};
use serde_json::Value;

use crate::core::crc32;
use crate::core::crc64;
use crate::core::dat1::{Dat1, SectionHeader, DAT1_MAGIC};
use crate::core::ddl;
use crate::core::error::{Result, ToolkitError};

pub const CONFIG_MAGIC: u32 = 0x21A56F68;
pub const CONFIG2_MAGIC: u32 = 0x35F7AFA5;

const CONFIG_TYPE_TAG: u32 = 0x4A128222;
const CONFIG_CONTENT_TAG: u32 = 0xE501186F;
const CONFIG_REFS_TAG: u32 = 0x58B8558A;
const CONDUIT_REFS_TAG: u32 = 0x2F4056CE;
const CONDUIT_BUILT_TAG: u32 = 0xCEB30E68;
const ACTOR_MODEL_NAME_TAG: u32 = 0x32FAC8E0;
const ACTOR_ASSET_REFS_TAG: u32 = 0x3AB204B9;
const ACTOR_COMPONENT_DEFS_TAG: u32 = 0x135832C8;
const ACTOR_COMPONENT_DATA_TAG: u32 = 0x6D4301EF;
const ACTOR_CONSTANT_TAG: u32 = 0x364A6C7C;
const OBJECT_MAGIC: u32 = 0x03150044;
const ACTOR_CONFIG_TYPE: &str = "Actor";

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
    pub content_tag: u32,
    pub original_dat1: Option<Dat1>,
}

fn parse_actor_sections(dat1: &Dat1) -> Option<Value> {
    let model_name_data = dat1.get_section_data(ACTOR_MODEL_NAME_TAG)?;
    if model_name_data.len() < 4 {
        return None;
    }

    let model_name_offset = u32::from_le_bytes(model_name_data[0..4].try_into().ok()?);
    let model_name = dat1
        .get_string(model_name_offset)
        .unwrap_or_else(|| format!("<missing string @{}>", model_name_offset));

    let mut root = serde_json::Map::new();

    if let Some(constant_data) = dat1.get_section_data(ACTOR_CONSTANT_TAG) {
        let mut constant = Vec::new();
        for chunk in constant_data.chunks_exact(4) {
            let v = i32::from_le_bytes(chunk.try_into().ok()?);
            constant.push(Value::Number(v.into()));
        }
        root.insert("Constant".to_string(), Value::Array(constant));
    }

    root.insert("Model".to_string(), Value::String(model_name.clone()));

    let mut all_refs: Vec<String> = Vec::new();
    if let Some(refs_data) = dat1.get_section_data(ACTOR_ASSET_REFS_TAG) {
        for chunk in refs_data.chunks_exact(16) {
            let string_offset = u32::from_le_bytes(chunk[8..12].try_into().ok()?);
            let path = dat1
                .get_string(string_offset)
                .unwrap_or_else(|| format!("<missing string @{}>", string_offset));
            all_refs.push(path);
        }
    }

    let mut component_asset_paths = HashSet::new();
    if let Some(defs_data) = dat1.get_section_data(ACTOR_COMPONENT_DEFS_TAG) {
        // Actors whose components carry no fields ship without a DATA section.
        let data_section = dat1.get_section_data(ACTOR_COMPONENT_DATA_TAG).unwrap_or(&[]);
        let this_section_offset = dat1
            .sections
            .iter()
            .find(|s| s.tag == ACTOR_COMPONENT_DATA_TAG)
            .map(|s| s.offset as usize)
            .unwrap_or(0);

        for chunk in defs_data.chunks_exact(32) {
            let id_lo = u32::from_le_bytes(chunk[0..4].try_into().ok()?);
            let id_hi = u32::from_le_bytes(chunk[4..8].try_into().ok()?);
            let class_name_offset = u32::from_le_bytes(chunk[8..12].try_into().ok()?);
            let data_offset = u32::from_le_bytes(chunk[20..24].try_into().ok()?);
            let data_size = u32::from_le_bytes(chunk[24..28].try_into().ok()?);

            let class_name = dat1
                .get_string(class_name_offset)
                .unwrap_or_else(|| format!("<missing class name @{class_name_offset}>"));

            let component_key = (((id_hi as u64) << 32) | (id_lo as u64)).to_string();
            let mut component_obj = serde_json::Map::new();

            if data_size > 0 {
                let rel = (data_offset as usize).saturating_sub(this_section_offset);
                let size_usize = data_size as usize;
                if rel + size_usize > data_section.len() || size_usize < 16 {
                    continue;
                }
                let hdr = &data_section[rel..rel + 16];
                let data_len = u32::from_le_bytes(hdr[12..16].try_into().ok()?);
                let payload_len = data_len as usize;
                if payload_len > size_usize.saturating_sub(16) {
                    continue;
                }
                // Pass the FULL serialized object (header + payload). The typed
                // deserializer reads its own 16-byte header so we feed it from
                // the start, not from the payload offset.
                let full = &data_section[rel..rel + 16 + payload_len];
                let parsed = match deserialize_section_typed(full, dat1) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                if let Some(map) = parsed.as_object() {
                    for (k, v) in map {
                        let promoted = promote_asset_paths(v);
                        collect_actor_asset_paths_typed(&promoted, &mut component_asset_paths);
                        component_obj.insert(k.clone(), promoted);
                    }
                } else {
                    continue;
                }
            }

            // The closed-source converter inlines the class name as "Name"
            // (untyped) at the end of each component object.
            component_obj.insert("Name".to_string(), Value::String(class_name));

            root.insert(component_key, Value::Object(component_obj));
        }
    }

    let mut extra_assets = Vec::new();
    for path in &all_refs {
        if path == &model_name {
            continue;
        }
        if component_asset_paths.contains(path)
            || component_asset_paths.contains(&path.replace('\\', "/"))
        {
            continue;
        }
        extra_assets.push(Value::String(path.clone()));
    }
    root.insert("ExtraAssets".to_string(), Value::Array(extra_assets));

    Some(Value::Object(root))
}

/// Walk a typed JSON node and rewrite `{Type: "STRING", Value: <asset path>}`
/// (or arrays of asset paths) into `{Type: "ASSET PATH", ...}`, normalising
/// path separators to `/` to match the closed-source converter output.
///
/// `OBJECT` values recurse, including arrays of objects.
fn promote_asset_paths(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let type_label = map.get("Type").and_then(Value::as_str);
            let value = map.get("Value");
            match (type_label, value) {
                (Some("STRING"), Some(Value::String(s))) if looks_like_asset_path(s) => {
                    serde_json::json!({
                        "Type": "ASSET PATH",
                        "Value": s.replace('\\', "/"),
                    })
                }
                (Some("STRING"), Some(Value::Array(arr)))
                    if !arr.is_empty()
                        && arr.iter().all(|item| {
                            item.as_str().map(looks_like_asset_path).unwrap_or(false)
                        }) =>
                {
                    let normalized: Vec<Value> = arr
                        .iter()
                        .map(|item| Value::String(item.as_str().unwrap_or("").replace('\\', "/")))
                        .collect();
                    serde_json::json!({
                        "Type": "ASSET PATH",
                        "Value": Value::Array(normalized),
                    })
                }
                (Some("OBJECT"), Some(inner)) => {
                    let promoted_inner = promote_asset_paths_inner(inner);
                    serde_json::json!({ "Type": "OBJECT", "Value": promoted_inner })
                }
                _ => {
                    // Unknown shape: just walk children defensively.
                    let mut out = serde_json::Map::new();
                    for (k, vv) in map {
                        out.insert(k.clone(), promote_asset_paths(vv));
                    }
                    Value::Object(out)
                }
            }
        }
        Value::Array(arr) => Value::Array(arr.iter().map(promote_asset_paths).collect()),
        _ => v.clone(),
    }
}

/// Promote asset paths inside an `OBJECT` `Value`, which can be either a
/// nested object (whose fields are themselves `{Type, Value}`) or an array
/// of such objects.
fn promote_asset_paths_inner(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, vv) in map {
                out.insert(k.clone(), promote_asset_paths(vv));
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(
            arr.iter()
                .map(|item| match item {
                    Value::Object(_) => promote_asset_paths_inner(item),
                    _ => promote_asset_paths(item),
                })
                .collect(),
        ),
        _ => v.clone(),
    }
}

/// Collect all asset paths embedded in a typed JSON tree (as produced by
/// `promote_asset_paths`). Used to derive `ExtraAssets` and to rebuild
/// `Actor Asset Refs` on save.
fn collect_actor_asset_paths_typed(v: &Value, out: &mut HashSet<String>) {
    match v {
        Value::Object(map) => {
            let type_label = map.get("Type").and_then(Value::as_str);
            if matches!(type_label, Some("ASSET PATH")) {
                if let Some(val) = map.get("Value") {
                    match val {
                        Value::String(s) => {
                            out.insert(s.clone());
                            out.insert(s.replace('\\', "/"));
                        }
                        Value::Array(arr) => {
                            for item in arr {
                                if let Some(s) = item.as_str() {
                                    out.insert(s.to_string());
                                    out.insert(s.replace('\\', "/"));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            for vv in map.values() {
                collect_actor_asset_paths_typed(vv, out);
            }
        }
        Value::Array(arr) => {
            for item in arr {
                collect_actor_asset_paths_typed(item, out);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Actor save: rebuild MODEL / CONSTANT / DEFS / DATA / REFS sections from JSON.
// ---------------------------------------------------------------------------

/// Rewrite the five actor sections in `dat1` from the typed JSON `content`.
/// Strings pool is grown as needed via `get_or_add_string_offset`. Component
/// data offsets are patched after the DAT1 layout is recomputed.
fn save_actor_sections(content: &Value, dat1: &mut Dat1) -> Result<()> {
    let root = content
        .as_object()
        .ok_or_else(|| ToolkitError::Parse("actor content must be a JSON object".into()))?;

    // ---- 1. MODEL section: 4-byte string offset (\-form path).
    let model_path = root
        .get("Model")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolkitError::Parse("actor JSON missing 'Model'".into()))?
        .to_string();
    let model_path_backslash = model_path.replace('/', "\\");
    let model_off = get_or_add_string_offset(dat1, &model_path_backslash);
    dat1.set_section_data(ACTOR_MODEL_NAME_TAG, model_off.to_le_bytes().to_vec())?;

    // ---- 2. CONSTANT section: i32 array.
    if let Some(const_arr) = root.get("Constant").and_then(Value::as_array) {
        let mut buf = Vec::with_capacity(const_arr.len() * 4);
        for item in const_arr {
            let n = item
                .as_i64()
                .or_else(|| item.as_u64().map(|u| u as i64))
                .unwrap_or(0) as i32;
            buf.extend_from_slice(&n.to_le_bytes());
        }
        // The constant section is mandatory but its content is opaque to us.
        // If absent in dat1, the round-trip would have already failed above.
        if dat1.get_section_data(ACTOR_CONSTANT_TAG).is_some() {
            dat1.set_section_data(ACTOR_CONSTANT_TAG, buf)?;
        }
    }

    // ---- 3. Components: re-serialize each, build DATA blob + DEFS template.
    //
    // Component keys are u64 instance ids (as decimal strings). Skip the
    // structural keys (Constant / Model / ExtraAssets) and process the rest
    // in JSON declaration order to match the original DEFS ordering.
    struct ComponentEntry {
        instance_id: u64,
        class_name: String,
        /// Bytes of the SerializedObject (header + payload, no inter-component
        /// padding). Empty if the component carries no fields.
        data_blob: Vec<u8>,
    }

    let mut components: Vec<ComponentEntry> = Vec::new();
    let mut asset_paths_in_order: Vec<String> = Vec::new();
    let mut asset_paths_seen: HashSet<String> = HashSet::new();

    for (key, value) in root.iter() {
        if matches!(key.as_str(), "Constant" | "Model" | "ExtraAssets") {
            continue;
        }
        let instance_id: u64 = key
            .parse()
            .map_err(|_| ToolkitError::Parse(format!("invalid component key {key:?}")))?;
        let comp_obj = value.as_object().ok_or_else(|| {
            ToolkitError::Parse(format!("component {key:?} must be a JSON object"))
        })?;
        let class_name = comp_obj
            .get("Name")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ToolkitError::Parse(format!("component {key:?} missing 'Name' (class name)"))
            })?
            .to_string();

        // Walk the component to collect asset paths in declaration order.
        for (k, v) in comp_obj {
            if k == "Name" {
                continue;
            }
            collect_asset_paths_ordered(v, &mut asset_paths_in_order, &mut asset_paths_seen);
        }

        let data_blob = if comp_obj.iter().any(|(k, _)| k != "Name") {
            serialize_component_object(comp_obj, dat1)?
        } else {
            Vec::new()
        };

        components.push(ComponentEntry {
            instance_id,
            class_name,
            data_blob,
        });
    }

    // ---- 4. Build COMPONENT_DATA: concatenate component blobs aligned to 16.
    let mut data_section = Vec::new();
    let mut component_local_offsets = Vec::with_capacity(components.len());
    for c in &components {
        if c.data_blob.is_empty() {
            component_local_offsets.push(None);
            continue;
        }
        // Align current cursor to 16 bytes within the section.
        let cur = data_section.len();
        let pad = (16 - cur % 16) % 16;
        for _ in 0..pad {
            data_section.push(0);
        }
        component_local_offsets.push(Some(data_section.len()));
        data_section.extend_from_slice(&c.data_blob);
    }
    if !set_optional_actor_section(dat1, ACTOR_COMPONENT_DATA_TAG, data_section)? {
        return save_actor_sections(content, dat1);
    }

    // ---- 5. Build COMPONENT_DEFS placeholder of correct size, so that
    // recalculate_section_headers can compute final offsets correctly.
    let defs_size = components.len() * 32;
    dat1.set_section_data(ACTOR_COMPONENT_DEFS_TAG, vec![0u8; defs_size])?;

    // ---- 6. Build ASSET_REFS: model first, then component-asset paths in
    // declaration order, then ExtraAssets. Convert all to \-form for storage.
    let mut ref_paths: Vec<String> = Vec::new();
    let mut ref_seen: HashSet<String> = HashSet::new();
    let mut push_ref = |paths: &mut Vec<String>, seen: &mut HashSet<String>, p: &str| {
        let backslash = p.replace('/', "\\");
        if seen.insert(backslash.clone()) {
            paths.push(backslash);
        }
    };
    // Model-less actors (Model "") ship without a model ref.
    if !model_path.is_empty() {
        push_ref(&mut ref_paths, &mut ref_seen, &model_path);
    }
    for p in &asset_paths_in_order {
        push_ref(&mut ref_paths, &mut ref_seen, p);
    }
    if let Some(extras) = root.get("ExtraAssets").and_then(Value::as_array) {
        for item in extras {
            if let Some(s) = item.as_str() {
                push_ref(&mut ref_paths, &mut ref_seen, s);
            }
        }
    }
    let mut refs_buf = Vec::with_capacity(ref_paths.len() * 16);
    for path in &ref_paths {
        let asset_id = crc64::hash(path);
        let str_off = get_or_add_string_offset(dat1, path);
        let ext_hash = extension_hash(path);
        refs_buf.extend_from_slice(&asset_id.to_le_bytes());
        refs_buf.extend_from_slice(&str_off.to_le_bytes());
        refs_buf.extend_from_slice(&ext_hash.to_le_bytes());
    }
    if !set_optional_actor_section(dat1, ACTOR_ASSET_REFS_TAG, refs_buf)? {
        return save_actor_sections(content, dat1);
    }

    // ---- 7. Recalculate DAT1 layout to learn the absolute offset of the
    // COMPONENT_DATA section (needed to patch DEFS). Absent only when no component has data.
    dat1.recalculate_section_headers();
    let data_section_offset = dat1
        .sections
        .iter()
        .find(|s| s.tag == ACTOR_COMPONENT_DATA_TAG)
        .map_or(0, |s| s.offset as usize);

    // ---- 8. Build the real COMPONENT_DEFS now that DATA's absolute offset
    // is known. We pre-add each class name to the strings pool here too —
    // this is a no-op if the name was already encountered inside the
    // serialized object headers above, but for empty-data components it
    // ensures the class name lives in the pool.
    let mut defs_buf = Vec::with_capacity(defs_size);
    for (i, c) in components.iter().enumerate() {
        let class_off = get_or_add_string_offset(dat1, &c.class_name);
        let class_hash = crc32::hash(&c.class_name);
        let (data_off_abs, data_size) = match component_local_offsets[i] {
            Some(local) => (
                (data_section_offset + local) as u32,
                c.data_blob.len() as u32,
            ),
            None => (0u32, 0u32),
        };
        defs_buf.extend_from_slice(&c.instance_id.to_le_bytes());
        defs_buf.extend_from_slice(&class_off.to_le_bytes());
        defs_buf.extend_from_slice(&class_hash.to_le_bytes());
        defs_buf.extend_from_slice(&0u32.to_le_bytes()); // reserved
        defs_buf.extend_from_slice(&data_off_abs.to_le_bytes());
        defs_buf.extend_from_slice(&data_size.to_le_bytes());
        defs_buf.extend_from_slice(&0xFFFFFFFFu32.to_le_bytes()); // sentinel
    }
    dat1.set_section_data(ACTOR_COMPONENT_DEFS_TAG, defs_buf)?;

    // String additions above (class_off above, ext_hash strings, etc.) only
    // ever EXTEND the strings pool, so the COMPONENT_DATA absolute offset we
    // captured remains valid only if the additions did not cross a section
    // boundary that pushes DATA later. Defs and refs sizes are now stable;
    // class names are ASCII so the only possible expansion happens before
    // DATA in section ordering. Recalculate one more time to update
    // total_size / final offsets, then verify DATA didn't shift.
    let prev_data_off = data_section_offset;
    dat1.recalculate_section_headers();
    let new_data_off = dat1
        .sections
        .iter()
        .find(|s| s.tag == ACTOR_COMPONENT_DATA_TAG)
        .map(|s| s.offset as usize)
        .unwrap_or(prev_data_off);

    if new_data_off != prev_data_off {
        // Re-patch DEFS with the corrected absolute offsets.
        let mut defs_buf = Vec::with_capacity(defs_size);
        for (i, c) in components.iter().enumerate() {
            let class_off = get_or_add_string_offset(dat1, &c.class_name);
            let class_hash = crc32::hash(&c.class_name);
            let (data_off_abs, data_size) = match component_local_offsets[i] {
                Some(local) => ((new_data_off + local) as u32, c.data_blob.len() as u32),
                None => (0u32, 0u32),
            };
            defs_buf.extend_from_slice(&c.instance_id.to_le_bytes());
            defs_buf.extend_from_slice(&class_off.to_le_bytes());
            defs_buf.extend_from_slice(&class_hash.to_le_bytes());
            defs_buf.extend_from_slice(&0u32.to_le_bytes());
            defs_buf.extend_from_slice(&data_off_abs.to_le_bytes());
            defs_buf.extend_from_slice(&data_size.to_le_bytes());
            defs_buf.extend_from_slice(&0xFFFFFFFFu32.to_le_bytes());
        }
        dat1.set_section_data(ACTOR_COMPONENT_DEFS_TAG, defs_buf)?;
        dat1.recalculate_section_headers();
    }

    Ok(())
}

/// Writes an actor section that shipped actors may lack (DATA when no component has fields, REFS when
/// nothing is referenced). An absent one stays absent while empty; a needed one is added and `Ok(false)`
/// asks the caller to start over, as the new header entry moves every absolute string offset written so far.
fn set_optional_actor_section(dat1: &mut Dat1, tag: u32, data: Vec<u8>) -> Result<bool> {
    if dat1.get_section_data(tag).is_some() {
        dat1.set_section_data(tag, data)?;
    } else if !data.is_empty() {
        // Fixups are absolute offsets the header growth would invalidate.
        if !dat1.fixups.is_empty() {
            return Err(ToolkitError::SectionNotFound(tag));
        }
        dat1.add_section(tag);
        return Ok(false);
    }
    Ok(true)
}

/// Walk a typed component field tree and append every asset-path string
/// (`{"Type": "ASSET PATH", "Value": ...}`) to `out`, deduplicated by
/// normalised forward-slash form. Order matches in-order traversal of the
/// JSON, which mirrors how the original converter emits the REFS table.
fn collect_asset_paths_ordered(v: &Value, out: &mut Vec<String>, seen: &mut HashSet<String>) {
    match v {
        Value::Object(map) => {
            let type_label = map.get("Type").and_then(Value::as_str);
            if matches!(type_label, Some("ASSET PATH")) {
                if let Some(val) = map.get("Value") {
                    match val {
                        Value::String(s) => push_unique(s, out, seen),
                        Value::Array(arr) => {
                            for item in arr {
                                if let Some(s) = item.as_str() {
                                    push_unique(s, out, seen);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                return;
            }
            // For OBJECT/STRING/whatever else, recurse.
            if let Some(inner) = map.get("Value") {
                collect_asset_paths_ordered(inner, out, seen);
            }
        }
        Value::Array(arr) => {
            for item in arr {
                collect_asset_paths_ordered(item, out, seen);
            }
        }
        _ => {}
    }
}

fn push_unique(s: &str, out: &mut Vec<String>, seen: &mut HashSet<String>) {
    let key = s.replace('\\', "/");
    if seen.insert(key.clone()) {
        out.push(s.to_string());
    }
}

/// CRC32 of the lowercase file extension, *including* the leading dot.
pub fn extension_hash(path: &str) -> u32 {
    let lower = path.to_ascii_lowercase();
    let dot = lower.rfind('.');
    let ext = match dot {
        Some(idx) => &lower[idx..],
        None => "",
    };
    crc32::hash(ext)
}

/// Serialize a component (typed JSON) into a SerializedObject blob:
/// the 16-byte header plus payload, padded to 4 bytes inside the payload.
/// `comp_obj` includes the synthetic `"Name"` key, which is excluded from
/// the wire format (it lives in COMPONENT_DEFS class_name_offset instead).
fn serialize_component_object(
    comp_obj: &serde_json::Map<String, Value>,
    dat1: &mut Dat1,
) -> Result<Vec<u8>> {
    let mut typed_map = serde_json::Map::new();
    for (k, v) in comp_obj {
        if k == "Name" {
            continue;
        }
        typed_map.insert(k.clone(), v.clone());
    }
    let typed_value = Value::Object(typed_map);
    let mut out = Vec::new();
    serialize_typed_object(&typed_value, &mut out, dat1)?;
    Ok(out)
}

/// Serialize a JSON object whose values are typed `{Type, Value}` envelopes
/// into the SerializedObject wire format. Mirrors `serialize_object_into_dat1`
/// but uses explicit `Type` labels (so UINT8 vs UINT32 etc. are preserved).
fn serialize_typed_object(value: &Value, out: &mut Vec<u8>, dat1: &mut Dat1) -> Result<()> {
    let map = value.as_object().ok_or_else(|| {
        ToolkitError::Parse("typed-object serialization expects a JSON object".into())
    })?;

    let mut children: Vec<(&String, u8, usize, &Value, u32)> = Vec::with_capacity(map.len());
    for (k, entry) in map {
        let entry_obj = entry.as_object().ok_or_else(|| {
            ToolkitError::Parse(format!(
                "field {k:?} must be a typed {{Type, Value}} object"
            ))
        })?;
        let type_label = entry_obj
            .get("Type")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolkitError::Parse(format!("field {k:?} missing Type")))?;
        let node_type = parse_node_type_label(type_label)?;
        let inner = entry_obj
            .get("Value")
            .ok_or_else(|| ToolkitError::Parse(format!("field {k:?} missing Value")))?;
        let count = match inner {
            Value::Array(arr) => arr.len(),
            _ => 1,
        };
        let str_off = get_or_add_string_offset(dat1, k);
        children.push((k, node_type, count, inner, str_off));
    }

    let mut inner_buf: Vec<u8> = Vec::new();

    // Child entries.
    for (k, node_type, count, _, _) in &children {
        let hash = crc32::hash(k);
        let flags: u16 = (*count as u16) << 4;
        inner_buf.extend_from_slice(&hash.to_le_bytes());
        inner_buf.extend_from_slice(&flags.to_le_bytes());
        inner_buf.push(0);
        inner_buf.push(*node_type);
    }

    // Name string offsets.
    for (_, _, _, _, str_off) in &children {
        inner_buf.extend_from_slice(&str_off.to_le_bytes());
    }

    // Field values.
    for (_, node_type, count, inner_value, _) in &children {
        if *count == 0 && *node_type == NT_NULL {
            // The game reads an empty NULL field as one byte.
            inner_buf.push(0);
        } else if *count == 1 && !matches!(inner_value, Value::Array(_)) {
            serialize_typed_value(*node_type, inner_value, &mut inner_buf, dat1)?;
        } else if let Value::Array(arr) = inner_value {
            for item in arr {
                serialize_typed_value(*node_type, item, &mut inner_buf, dat1)?;
            }
        } else {
            // count != 1 but Value isn't an array — degenerate; treat as single.
            serialize_typed_value(*node_type, inner_value, &mut inner_buf, dat1)?;
        }
    }

    let r = inner_buf.len() % 4;
    if r != 0 {
        inner_buf.resize(inner_buf.len() + (4 - r), 0);
    }

    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&OBJECT_MAGIC.to_le_bytes());
    out.extend_from_slice(&(map.len() as u32).to_le_bytes());
    out.extend_from_slice(&(inner_buf.len() as u32).to_le_bytes());
    out.extend_from_slice(&inner_buf);
    Ok(())
}

/// Serialize a single typed value's payload (no tag byte; the node type tag
/// lives in the child entry header).
fn serialize_typed_value(
    node_type: u8,
    v: &Value,
    out: &mut Vec<u8>,
    dat1: &mut Dat1,
) -> Result<()> {
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
        NT_STRING => {
            let s = v.as_str().unwrap_or("");
            serialize_inline_string(s, out);
        }
        NT_OBJECT => {
            // OBJECT payload: nested SerializedObject. The value is itself a
            // typed object map (already pre-promoted), e.g. {Field: {Type, Value}}.
            serialize_typed_object(v, out, dat1)?;
        }
        NT_BOOLEAN => out.push(if v.as_bool().unwrap_or(false) { 1 } else { 0 }),
        NT_INSTANCE_ID => {
            let u = v.as_u64().unwrap_or_else(|| v.as_i64().unwrap_or(0) as u64);
            out.extend_from_slice(&u.to_le_bytes());
        }
        NT_NULL => out.push(0),
        other => {
            return Err(ToolkitError::Parse(format!(
                "unknown typed node label / kind 0x{other:02X}"
            )));
        }
    }
    Ok(())
}

/// Inverse of `config_node_type_label`: parse the label that the typed
/// reader emits (plus the synthetic `ASSET PATH` flavour) into a wire
/// node-type byte.
fn parse_node_type_label(label: &str) -> Result<u8> {
    Ok(match label {
        "UINT8" => NT_UINT8,
        "UINT16" => NT_UINT16,
        "UINT32" => NT_UINT32,
        "INT8" => NT_INT8,
        "INT16" => NT_INT16,
        "INT32" => NT_INT32,
        "FLOAT" => NT_FLOAT,
        // ASSET PATH is just a STRING flavour with display normalisation.
        "STRING" | "ASSET PATH" => NT_STRING,
        "OBJECT" => NT_OBJECT,
        "BOOLEAN" => NT_BOOLEAN,
        "INSTANCE_ID" => NT_INSTANCE_ID,
        "NULL" => NT_NULL,
        // The legacy actor reader used "INT" for numeric values it couldn't
        // infer precisely; default to INT32 to remain compatible with files
        // produced before the typed reader landed.
        "INT" => NT_INT32,
        other => {
            return Err(ToolkitError::Parse(format!(
                "unsupported actor field type label {other:?}"
            )));
        }
    })
}

fn actor_typed_value(v: &Value) -> Value {
    match v {
        Value::Null => serde_json::json!({ "Type": "NULL", "Value": Value::Null }),
        Value::Bool(b) => serde_json::json!({ "Type": "BOOLEAN", "Value": *b }),
        Value::Number(n) => {
            if n.is_f64() {
                serde_json::json!({ "Type": "FLOAT", "Value": n.clone() })
            } else {
                serde_json::json!({ "Type": "INT", "Value": n.clone() })
            }
        }
        Value::String(s) => {
            if looks_like_asset_path(s) {
                serde_json::json!({ "Type": "ASSET PATH", "Value": s.replace('\\', "/") })
            } else {
                serde_json::json!({ "Type": "STRING", "Value": s })
            }
        }
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v2) in map {
                out.insert(k.clone(), actor_typed_value(v2));
            }
            serde_json::json!({ "Type": "OBJECT", "Value": Value::Object(out) })
        }
        Value::Array(arr) => {
            let mut out = Vec::new();
            for item in arr {
                out.push(actor_typed_container_value(item));
            }
            serde_json::json!({ "Type": "OBJECT", "Value": Value::Array(out) })
        }
    }
}

fn actor_typed_container_value(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v2) in map {
                out.insert(k.clone(), actor_typed_value(v2));
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(actor_typed_container_value).collect()),
        _ => actor_typed_value(v),
    }
}

fn collect_actor_asset_paths(v: &Value, out: &mut HashSet<String>) {
    match v {
        Value::String(s) => {
            if looks_like_asset_path(s) {
                out.insert(s.clone());
                out.insert(s.replace('\\', "/"));
            }
        }
        Value::Array(arr) => {
            for item in arr {
                collect_actor_asset_paths(item, out);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                collect_actor_asset_paths(value, out);
            }
        }
        _ => {}
    }
}

/// Keeps a reference table (16-byte entries: u64 asset id, u32 path string offset, u32 extension hash)
/// in step with edited content. The game preloads an asset's dependencies from this list, and it is not
/// every path in the content (RYNO leaves out the actors it spawns on demand), so the list stays as it was
/// and only the edits apply: paths edited away are dropped, paths an edit brought in are added.
/// `old_paths` are the content's paths as loaded; nothing changes when the asset has no such table.
fn follow_ref_edits(dat1: &mut Dat1, tag: u32, old_paths: &[String], content: &Value) -> Result<()> {
    let Some(old_refs) = dat1.get_section_data(tag).map(<[u8]>::to_vec) else { return Ok(()) };
    let mut paths = Vec::new();
    collect_config_ref_paths(content, &mut paths);
    let has = |list: &[String], p: &str| list.iter().any(|x| x.eq_ignore_ascii_case(p));
    let mut refs = Vec::with_capacity(old_refs.len());
    for entry in old_refs.chunks_exact(16) {
        let offset = u32::from_le_bytes(entry[8..12].try_into().unwrap());
        let path = dat1.get_string(offset).unwrap_or_default().replace('/', "\\");
        if has(&paths, &path) || !has(old_paths, &path) {
            refs.extend_from_slice(entry);
        }
    }
    for path in paths.iter().filter(|p| !has(old_paths, p)) {
        refs.extend_from_slice(&crc64::hash(path).to_le_bytes());
        refs.extend_from_slice(&get_or_add_string_offset(dat1, path).to_le_bytes());
        refs.extend_from_slice(&extension_hash(path).to_le_bytes());
    }
    dat1.set_section_data(tag, refs)
}

/// Every asset path in untyped config content, backslashed, once each in first-seen order: what Config
/// Refs lists. A path is a string with a separator whose file name has an extension.
fn collect_config_ref_paths(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => {
            let path = s.replace('/', "\\");
            let named = path.rsplit_once('\\').is_some_and(|(_, file)| file.contains('.'));
            if named && !out.iter().any(|o| o.eq_ignore_ascii_case(&path)) {
                out.push(path);
            }
        }
        Value::Array(items) => items.iter().for_each(|x| collect_config_ref_paths(x, out)),
        Value::Object(map) => map.values().for_each(|x| collect_config_ref_paths(x, out)),
        _ => {}
    }
}

fn looks_like_asset_path(s: &str) -> bool {
    if !(s.contains('/') || s.contains('\\')) {
        return false;
    }
    let lower = s.to_ascii_lowercase();
    [
        ".actor",
        ".animset",
        ".config",
        ".conduit",
        ".model",
        ".performanceset",
        ".visualeffect",
        ".animclip",
        ".material",
        ".texture",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
}

// Conduit handling lives in the canonical DDL bridge below
// (`ddl_object_to_typed_json` / `typed_json_to_ddl_object`).

fn config_node_type_label(node_type: u8) -> &'static str {
    match node_type {
        NT_UINT8 => "UINT8",
        NT_UINT16 => "UINT16",
        NT_UINT32 => "UINT32",
        NT_INT8 => "INT8",
        NT_INT16 => "INT16",
        NT_INT32 => "INT32",
        NT_FLOAT => "FLOAT",
        NT_STRING => "STRING",
        NT_OBJECT => "OBJECT",
        NT_BOOLEAN => "BOOLEAN",
        NT_INSTANCE_ID => "INSTANCE_ID",
        NT_NULL => "NULL",
        _ => "UNKNOWN",
    }
}

fn config_unwrap_typed_value(v: &Value) -> Value {
    if let Some(map) = v.as_object() {
        let type_str = map.get("Type").and_then(|t| t.as_str());
        let maybe_wrapped = map.get("Value");
        if let (Some(_), Some(value)) = (type_str, maybe_wrapped) {
            return config_unwrap_typed_payload(value);
        }

        let mut out = serde_json::Map::new();
        for (k, value) in map {
            out.insert(k.clone(), config_unwrap_typed_value(value));
        }
        return Value::Object(out);
    }

    if let Some(arr) = v.as_array() {
        return Value::Array(arr.iter().map(config_unwrap_typed_value).collect());
    }

    v.clone()
}

fn config_unwrap_typed_payload(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, value) in map {
                out.insert(k.clone(), config_unwrap_typed_value(value));
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(config_unwrap_typed_value).collect()),
        _ => v.clone(),
    }
}

fn config_extract_main_for_save(v: &Value) -> Value {
    if let Some(map) = v.as_object() {
        if let Some(main) = map.get("Main") {
            return config_unwrap_typed_value(main);
        }
    }
    config_unwrap_typed_value(v)
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

        if data.len() < 40 {
            return Err(ToolkitError::InvalidMagic {
                expected: CONFIG_MAGIC,
                got: magic,
            });
        }

        let wrapped_dat1_magic = u32::from_le_bytes(data[36..40].try_into().unwrap());
        if wrapped_dat1_magic != DAT1_MAGIC {
            return Err(ToolkitError::InvalidMagic {
                expected: CONFIG_MAGIC,
                got: magic,
            });
        }

        let mut unk = [0u8; 28];
        unk.copy_from_slice(&data[8..36]);

        let dat1 = Dat1::parse(&data[36..])?;
        Self::from_dat1(dat1, magic, unk)
    }

    fn from_dat1(dat1: Dat1, magic: u32, unk: [u8; 28]) -> Result<Self> {
        if let (Some(type_data), Some(content_data)) = (
            dat1.get_section_data(CONFIG_TYPE_TAG),
            dat1.get_section_data(CONFIG_CONTENT_TAG),
        ) {
            let type_plain = deserialize_section(type_data, &dat1)?;
            let config_type = type_plain
                .get("Type")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown")
                .to_string();

            let type_obj = deserialize_section_typed(type_data, &dat1)?;
            let content_obj = deserialize_section_typed(content_data, &dat1)?;
            let content = serde_json::json!({
                "Header": type_obj,
                "Main": content_obj,
            });
            return Ok(Self {
                magic,
                unk,
                config_type,
                content,
                content_tag: CONFIG_CONTENT_TAG,
                original_dat1: Some(dat1),
            });
        }

        if let Some(content_data) = dat1.get_section_data(CONDUIT_BUILT_TAG) {
            // Route conduit through the canonical DDL parser. This preserves
            // the full DdlTypeKind set (UInt64 / Int64 / Double / Asset / Enum
            // / Bitfield / File / Json / Identifier / Default) on round-trip,
            // which the legacy `deserialize_section` heuristic could not.
            let parsed = ddl::parse(content_data, &dat1)?;
            let content = ddl_object_to_typed_json(&parsed);
            return Ok(Self {
                magic,
                unk,
                config_type: "ConduitBuilt".to_string(),
                content,
                content_tag: CONDUIT_BUILT_TAG,
                original_dat1: Some(dat1),
            });
        }

        if let Some(content) = parse_actor_sections(&dat1) {
            return Ok(Self {
                magic,
                unk,
                config_type: ACTOR_CONFIG_TYPE.to_string(),
                content,
                content_tag: ACTOR_MODEL_NAME_TAG,
                original_dat1: Some(dat1),
            });
        }

        for section in &dat1.sections {
            if let Some(data) = dat1.get_section_data(section.tag) {
                if let Ok(content) = deserialize_section(data, &dat1) {
                    return Ok(Self {
                        magic,
                        unk,
                        config_type: format!("ReadOnly_{:08X}", section.tag),
                        content,
                        content_tag: section.tag,
                        original_dat1: Some(dat1),
                    });
                }
            }
        }

        Err(ToolkitError::SectionNotFound(CONFIG_TYPE_TAG))
    }

    pub fn save(mut self) -> Result<Vec<u8>> {
        if self.config_type == ACTOR_CONFIG_TYPE && self.content_tag == ACTOR_MODEL_NAME_TAG {
            let mut dat1 = self
                .original_dat1
                .take()
                .ok_or_else(|| ToolkitError::Parse("missing source DAT1 for actor save".into()))?;

            save_actor_sections(&self.content, &mut dat1)?;

            let dat1_bytes = dat1.save();

            if self.magic == DAT1_MAGIC {
                return Ok(dat1_bytes);
            }

            // Actor uses the 36-byte preamble (asset_id + raw_size + 28 zero bytes).
            let mut out = Vec::with_capacity(36 + dat1_bytes.len());
            out.extend_from_slice(&self.magic.to_le_bytes());
            out.extend_from_slice(&(dat1_bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(&self.unk);
            out.extend_from_slice(&dat1_bytes);
            return Ok(out);
        }

        if self.content_tag == CONDUIT_BUILT_TAG {
            let mut dat1 = self.original_dat1.take().ok_or_else(|| {
                ToolkitError::Parse("missing source DAT1 for conduit save".into())
            })?;

            // Symmetric with the conduit reader: rebuild a `DdlObject` from
            // the typed JSON shape and emit via `ddl::serialize`. This
            // honors UInt64 / Int64 / Double / Asset / Enum / Bitfield / File
            // / Json / Identifier / Default kinds that the legacy writer
            // could not represent.
            // Asset paths as loaded, before the content is replaced: Conduit Refs follows the edits to them.
            let mut old_paths = Vec::new();
            if let Some(old) = dat1.get_section_data(CONDUIT_BUILT_TAG).and_then(|d| ddl::parse(d, &dat1).ok()) {
                collect_config_ref_paths(&ddl_object_to_typed_json(&old), &mut old_paths);
            }
            let obj = typed_json_to_ddl_object(&self.content)?;
            let content_bytes = ddl::serialize(&obj, &mut dat1);
            dat1.set_section_data(CONDUIT_BUILT_TAG, content_bytes)?;
            follow_ref_edits(&mut dat1, CONDUIT_REFS_TAG, &old_paths, &self.content)?;
            let dat1_bytes = dat1.save();

            if self.magic == DAT1_MAGIC {
                return Ok(dat1_bytes);
            }

            let mut out = Vec::with_capacity(36 + dat1_bytes.len());
            out.extend_from_slice(&self.magic.to_le_bytes());
            out.extend_from_slice(&(dat1_bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(&self.unk);
            out.extend_from_slice(&dat1_bytes);
            return Ok(out);
        }

        if self.content_tag != CONFIG_CONTENT_TAG {
            return Err(ToolkitError::Unsupported(format!(
                "saving not supported for section tag {:#010X}",
                self.content_tag
            )));
        }

        let content_main = config_extract_main_for_save(&self.content);

        let mut dat1 = if let Some(d) = self.original_dat1.take() {
            d
        } else {
            let mut pool = StringsPool::new();
            pool.add("Config Built File");

            let sections_map: HashMap<u32, usize> = [(CONFIG_TYPE_TAG, 0), (CONFIG_CONTENT_TAG, 1)]
                .into_iter()
                .collect();

            // dat1.unk1 stores the wrapper magic (for wrapped files) or CONFIG_MAGIC as a label.
            let dat1_unk1 = if self.magic == DAT1_MAGIC {
                CONFIG_MAGIC
            } else {
                self.magic
            };

            Dat1 {
                magic: DAT1_MAGIC,
                unk1: dat1_unk1,
                total_size: 0,
                sections: vec![
                    SectionHeader {
                        tag: CONFIG_TYPE_TAG,
                        offset: 0,
                        size: 0,
                    },
                    SectionHeader {
                        tag: CONFIG_CONTENT_TAG,
                        offset: 0,
                        size: 0,
                    },
                ],
                fixups: vec![],
                strings_pool: pool.data,
                section_data: vec![Vec::new(), Vec::new()],
                sections_map,
            }
        };

        // Asset paths of the content as loaded, before it is replaced: Config Refs follows the edits to them.
        let mut old_paths = Vec::new();
        if let Some(old) = dat1.get_section_data(CONFIG_CONTENT_TAG).and_then(|d| deserialize_section(d, &dat1).ok()) {
            collect_config_ref_paths(&old, &mut old_paths);
        }
        // Content with asset paths needs Config Refs or the game never loads them. Added before the
        // sections are serialized (the new header entry moves every string offset), listing every path.
        if dat1.get_section_data(CONFIG_REFS_TAG).is_none() && dat1.fixups.is_empty() {
            let mut paths = Vec::new();
            collect_config_ref_paths(&content_main, &mut paths);
            if !paths.is_empty() {
                dat1.add_section(CONFIG_REFS_TAG);
                old_paths.clear();
            }
        }

        let type_obj = serde_json::json!({ "Type": self.config_type });
        let type_bytes = serialize_section_into_dat1(&type_obj, &mut dat1);
        // Written from the typed content so field types survive; content_main (untyped) feeds Config Refs.
        let content_bytes = serialize_section_into_dat1(self.content.get("Main").unwrap_or(&self.content), &mut dat1);
        dat1.set_section_data(CONFIG_TYPE_TAG, type_bytes)?;
        dat1.set_section_data(CONFIG_CONTENT_TAG, content_bytes)?;
        follow_ref_edits(&mut dat1, CONFIG_REFS_TAG, &old_paths, &content_main)?;

        let dat1_bytes = dat1.save();

        if self.magic == DAT1_MAGIC {
            return Ok(dat1_bytes);
        }

        // Wrapped format: 4-byte magic + 4-byte dat1 size + 28-byte unk + DAT1.
        let mut out = Vec::with_capacity(36 + dat1_bytes.len());
        out.extend_from_slice(&self.magic.to_le_bytes());
        out.extend_from_slice(&(dat1_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.unk);
        out.extend_from_slice(&dat1_bytes);
        Ok(out)
    }
}

struct StringsPool {
    data: Vec<u8>,
    offsets: HashMap<String, u32>,
    next_offset: u32,
}

impl StringsPool {
    fn new() -> Self {
        Self {
            data: Vec::new(),
            offsets: HashMap::new(),
            next_offset: 0,
        }
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

fn deserialize_section_typed(data: &[u8], dat1: &Dat1) -> Result<Value> {
    let mut cur = Cursor::new(data);
    deserialize_object_typed(&mut cur, dat1)
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
        let name = dat1
            .get_string(name_offsets[i])
            .unwrap_or_else(|| format!("field_{}", name_offsets[i]));

        let value = if items_count == 0 && node_type == NT_NULL {
            cur.read_u8()?; // an empty NULL field still holds one byte
            Value::Array(Vec::new())
        } else if items_count != 1 {
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

fn deserialize_object_typed(cur: &mut Cursor<&[u8]>, dat1: &Dat1) -> Result<Value> {
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
        let name = dat1
            .get_string(name_offsets[i])
            .unwrap_or_else(|| format!("field_{}", name_offsets[i]));

        let value = if items_count == 0 && node_type == NT_NULL {
            cur.read_u8()?; // an empty NULL field still holds one byte
            Value::Array(Vec::new())
        } else if items_count != 1 {
            let mut arr = Vec::with_capacity(items_count);
            for _ in 0..items_count {
                arr.push(deserialize_node_payload_typed(cur, node_type, dat1)?);
            }
            Value::Array(arr)
        } else {
            deserialize_node_payload_typed(cur, node_type, dat1)?
        };

        map.insert(
            name,
            serde_json::json!({
                "Type": config_node_type_label(node_type),
                "Value": value,
            }),
        );
    }

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
        _ => Err(ToolkitError::Parse(format!(
            "unknown config node type 0x{node_type:02X}"
        ))),
    }
}

fn deserialize_node_payload_typed(
    cur: &mut Cursor<&[u8]>,
    node_type: u8,
    dat1: &Dat1,
) -> Result<Value> {
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
        NT_OBJECT => deserialize_object_typed(cur, dat1),
        NT_BOOLEAN => Ok(Value::Bool(cur.read_u8()? != 0)),
        NT_INSTANCE_ID => Ok(Value::Number(cur.read_u64::<LE>()?.into())),
        NT_NULL => {
            cur.read_u8()?;
            Ok(Value::Null)
        }
        _ => Err(ToolkitError::Parse(format!(
            "unknown config node type 0x{node_type:02X}"
        ))),
    }
}

fn deserialize_inline_string(cur: &mut Cursor<&[u8]>) -> Result<Value> {
    let length = cur.read_u32::<LE>()? as usize;
    let _crc32 = cur.read_u32::<LE>()?;
    let _crc64 = cur.read_u64::<LE>()?;
    let mut bytes = vec![0u8; length];
    cur.read_exact(&mut bytes)?;
    cur.read_u8()?; // null terminator
    // Padded from the string's own start (16-byte header, text, NUL), not to an absolute boundary.
    let pad = (4 - (length + 1) % 4) % 4;
    cur.seek(SeekFrom::Current(pad as i64))?;
    Ok(Value::String(String::from_utf8_lossy(&bytes).into_owned()))
}

fn serialize_section(value: &Value, pool: &mut StringsPool) -> Vec<u8> {
    let mut out = Vec::new();
    serialize_object(value, &mut out, pool);
    out
}

fn serialize_section_into_dat1(value: &Value, dat1: &mut Dat1) -> Vec<u8> {
    let mut out = Vec::new();
    serialize_object_into_dat1(value, &mut out, dat1);
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
        if *count == 0 && *node_type == NT_NULL {
            // The game reads an empty NULL field as one byte.
            inner.push(0);
        } else if *count != 1 {
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

fn serialize_object_into_dat1(obj: &Value, out: &mut Vec<u8>, dat1: &mut Dat1) {
    let map = match obj.as_object() {
        Some(m) => m,
        None => {
            out.extend_from_slice(&[0u8; 4]);
            out.extend_from_slice(&OBJECT_MAGIC.to_le_bytes());
            out.extend_from_slice(&[0u8; 8]);
            return;
        }
    };

    // Polymorphic substruct guard (recommendation #2):
    // a 2-field struct whose key names CRC32-hash to the engine's reserved
    // polymorphic tags is a downcast wrapper. Both fields MUST round-trip — if
    // either was flattened upstream the file will load but behave wrong.
    debug_assert!(
        !looks_polymorphic_json(map) || map.len() == 2,
        "polymorphic substruct lost its 2-field shape during edit"
    );

    let children: Vec<(&String, &Value, u8, usize, u32)> = map
        .iter()
        .map(|(k, v)| {
            // A typed {Type, Value} field keeps its label (INT8 5 must not come back UINT8); plain JSON is inferred.
            let typed = v.get("Type").and_then(Value::as_str).zip(v.get("Value"));
            let v = typed.map_or(v, |(_, inner)| inner);
            let (inferred, count) = infer_type_and_count(v);
            let node_type = typed.and_then(|(label, _)| parse_node_type_label(label).ok()).unwrap_or(inferred);
            let str_off = get_or_add_string_offset(dat1, k);
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

    for (_, v, node_type, _, _) in &children {
        // A one-item list is written like a scalar (count 1) but its item still has to be written.
        match v.as_array() {
            Some(arr) => arr.iter().for_each(|elem| serialize_node_into_dat1(elem, *node_type, &mut inner, dat1)),
            None => serialize_node_into_dat1(v, *node_type, &mut inner, dat1),
        }
    }

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
            let u = v.as_u64().unwrap_or_else(|| v.as_i64().unwrap_or(0) as u64);
            out.extend_from_slice(&u.to_le_bytes());
        }
        NT_NULL | _ => out.push(0),
    }
}

fn serialize_node_into_dat1(v: &Value, node_type: u8, out: &mut Vec<u8>, dat1: &mut Dat1) {
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
        NT_OBJECT => serialize_object_into_dat1(v, out, dat1),
        NT_BOOLEAN => out.push(if v.as_bool().unwrap_or(false) { 1 } else { 0 }),
        NT_INSTANCE_ID => {
            let u = v.as_u64().unwrap_or_else(|| v.as_i64().unwrap_or(0) as u64);
            out.extend_from_slice(&u.to_le_bytes());
        }
        NT_NULL | _ => out.push(0),
    }
}

/// Find or add a string in `dat1.strings_pool` and return an **absolute DAT1
/// offset** (pool offset + `header_end`). Absolute form is required because
/// the DAT1 reader's `get_string` disambiguates pool-relative vs. absolute
/// offsets by comparing to `header_end` — any pool-relative value that lands
/// past that boundary would be silently misinterpreted as absolute.
fn get_or_add_string_offset(dat1: &mut Dat1, s: &str) -> u32 {
    let header_end = dat1.header_end() as u32;
    let target = s.as_bytes();
    let mut i = 0usize;
    while i < dat1.strings_pool.len() {
        let mut end = i;
        while end < dat1.strings_pool.len() && dat1.strings_pool[end] != 0 {
            end += 1;
        }
        if &dat1.strings_pool[i..end] == target {
            return i as u32 + header_end;
        }
        i = end.saturating_add(1);
    }

    let off = dat1.strings_pool.len() as u32;
    dat1.strings_pool.extend_from_slice(target);
    dat1.strings_pool.push(0);
    off + header_end
}

fn serialize_inline_string(s: &str, out: &mut Vec<u8>) {
    let c32 = crc32::hash(s);
    let c64 = crc64::hash(s);
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(&c32.to_le_bytes());
    out.extend_from_slice(&c64.to_le_bytes());
    out.extend_from_slice(s.as_bytes());
    out.push(0);
    // Padded from the string's own start as the game reads it, not to an absolute boundary.
    let r = (s.len() + 1) % 4;
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

// ---------------------------------------------------------------------------
// Conduit ↔ canonical DDL bridge
// ---------------------------------------------------------------------------
// JSON shape for conduit fields (lossless w.r.t. DdlTypeKind / DdlArrayKind):
//
//   "<name>": { "Type": "<kind>", "Value": <scalar | array | object> }
//   "<name>": { "Type": "<kind>", "ArrayKind": "<kind>", "Value": [ ... ] }
//
// `Type` is one of `DdlTypeKind::label()` (e.g. "Float", "Asset", "Struct").
// `ArrayKind` is omitted when "None"; otherwise "Fixed" / "Dynamic" / "Map".
// `Value` is a scalar/string/object when there's exactly one element with
// `ArrayKind = None`; otherwise a JSON array of N elements.
// ---------------------------------------------------------------------------

pub fn ddl_object_to_typed_json(obj: &ddl::DdlObject) -> Value {
    let mut map = serde_json::Map::new();
    for &id in &obj.field_order {
        let Some(field) = obj.fields.get(&id) else {
            continue;
        };
        let mut entry = serde_json::Map::new();
        entry.insert(
            "Type".into(),
            Value::String(field.type_kind.label().to_string()),
        );
        if !matches!(field.array_kind, ddl::DdlArrayKind::None) {
            entry.insert(
                "ArrayKind".into(),
                Value::String(format!("{:?}", field.array_kind)),
            );
        }
        let value =
            if matches!(field.array_kind, ddl::DdlArrayKind::None) && field.values.len() == 1 {
                ddl_value_to_json(&field.values[0])
            } else {
                Value::Array(field.values.iter().map(ddl_value_to_json).collect())
            };
        entry.insert("Value".into(), value);
        map.insert(field.name.clone(), Value::Object(entry));
    }
    Value::Object(map)
}

fn ddl_value_to_json(v: &ddl::DdlValue) -> Value {
    match v {
        ddl::DdlValue::U8(x) => Value::Number((*x).into()),
        ddl::DdlValue::U16(x) => Value::Number((*x).into()),
        ddl::DdlValue::U32(x) => Value::Number((*x).into()),
        ddl::DdlValue::U64(x) => Value::Number((*x).into()),
        ddl::DdlValue::I8(x) => Value::Number((*x).into()),
        ddl::DdlValue::I16(x) => Value::Number((*x).into()),
        ddl::DdlValue::I32(x) => Value::Number((*x).into()),
        ddl::DdlValue::I64(x) => Value::Number((*x).into()),
        ddl::DdlValue::F32(x) => serde_json::Number::from_f64(*x as f64)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        ddl::DdlValue::F64(x) => serde_json::Number::from_f64(*x)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        ddl::DdlValue::Bool(x) => Value::Bool(*x),
        ddl::DdlValue::String { value, .. } => Value::String(value.clone()),
        ddl::DdlValue::Identifier(x) | ddl::DdlValue::Asset(x) => Value::Number((*x).into()),
        ddl::DdlValue::Struct(inner) => ddl_object_to_typed_json(inner),
        ddl::DdlValue::Null => Value::Null,
    }
}

pub fn typed_json_to_ddl_object(v: &Value) -> Result<ddl::DdlObject> {
    let map = v
        .as_object()
        .ok_or_else(|| ToolkitError::Parse("expected JSON object for DDL".into()))?;
    let mut obj = ddl::DdlObject::default();
    for (name, entry) in map {
        let entry_map = entry.as_object().ok_or_else(|| {
            ToolkitError::Parse(format!(
                "field {name:?}: expected typed entry {{Type, Value}}"
            ))
        })?;
        let type_label = entry_map
            .get("Type")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolkitError::Parse(format!("field {name:?}: missing Type")))?;
        let array_label = entry_map
            .get("ArrayKind")
            .and_then(Value::as_str)
            .unwrap_or("None");
        let raw_value = entry_map
            .get("Value")
            .ok_or_else(|| ToolkitError::Parse(format!("field {name:?}: missing Value")))?;

        let type_kind = parse_ddl_type_label(type_label)?;
        let array_kind = parse_ddl_array_label(array_label);

        // Default values are all null placeholders (json_to_ddl_value), but their count is kept: it can be 0.
        let values: Vec<ddl::DdlValue> = match (array_kind, raw_value) {
            (ddl::DdlArrayKind::None, v) if !v.is_array() => {
                vec![json_to_ddl_value(type_kind, v)?]
            }
            (_, Value::Array(arr)) => arr
                .iter()
                .map(|item| json_to_ddl_value(type_kind, item))
                .collect::<Result<Vec<_>>>()?,
            // ArrayKind != None but Value is scalar — wrap as single-element.
            (_, v) => vec![json_to_ddl_value(type_kind, v)?],
        };

        obj.insert(ddl::DdlField {
            id: crc32::hash(name),
            name: name.clone(),
            type_kind,
            array_kind,
            values,
        });
    }
    Ok(obj)
}

fn json_to_ddl_value(kind: ddl::DdlTypeKind, v: &Value) -> Result<ddl::DdlValue> {
    use ddl::DdlTypeKind as K;
    use ddl::DdlValue as V;
    Ok(match kind {
        K::UInt8 => V::U8(v.as_u64().unwrap_or(0) as u8),
        K::UInt16 => V::U16(v.as_u64().unwrap_or(0) as u16),
        K::UInt32 => V::U32(v.as_u64().unwrap_or(0) as u32),
        K::UInt64 => V::U64(v.as_u64().unwrap_or_else(|| v.as_i64().unwrap_or(0) as u64)),
        K::Int8 => V::I8(v.as_i64().unwrap_or(0) as i8),
        K::Int16 => V::I16(v.as_i64().unwrap_or(0) as i16),
        K::Int32 => V::I32(v.as_i64().unwrap_or(0) as i32),
        K::Int64 => V::I64(v.as_i64().unwrap_or(0)),
        K::Float => V::F32(v.as_f64().unwrap_or(0.0) as f32),
        K::Double => V::F64(v.as_f64().unwrap_or(0.0)),
        K::Bool => V::Bool(v.as_bool().unwrap_or(false)),
        K::Identifier => {
            V::Identifier(v.as_u64().unwrap_or_else(|| v.as_i64().unwrap_or(0) as u64))
        }
        K::Asset => V::Asset(v.as_u64().unwrap_or_else(|| v.as_i64().unwrap_or(0) as u64)),
        K::String | K::Enum | K::Bitfield | K::File | K::Json => V::String {
            value: v.as_str().unwrap_or("").to_string(),
            // hash + checksum are regenerated by `ddl::serialize` (recommendation #3)
            hash: 0,
            checksum: 0,
        },
        K::Struct => V::Struct(typed_json_to_ddl_object(v)?),
        K::Default => V::Null,
        K::Unknown => {
            return Err(ToolkitError::Parse(
                "cannot serialize DDL field of kind Unknown".into(),
            ));
        }
    })
}

fn parse_ddl_type_label(s: &str) -> Result<ddl::DdlTypeKind> {
    use ddl::DdlTypeKind as K;
    Ok(match s {
        "UInt8" => K::UInt8,
        "UInt16" => K::UInt16,
        "UInt32" => K::UInt32,
        "UInt64" => K::UInt64,
        "Int8" => K::Int8,
        "Int16" => K::Int16,
        "Int32" => K::Int32,
        "Int64" => K::Int64,
        "Float" => K::Float,
        "Double" => K::Double,
        "String" => K::String,
        "Enum" => K::Enum,
        "Bitfield" => K::Bitfield,
        "Struct" => K::Struct,
        "Bool" => K::Bool,
        "File" => K::File,
        "Identifier" => K::Identifier,
        "Json" => K::Json,
        "Default" => K::Default,
        "Asset" => K::Asset,
        other => {
            return Err(ToolkitError::Parse(format!(
                "unknown DDL type label: {other:?}"
            )));
        }
    })
}

fn parse_ddl_array_label(s: &str) -> ddl::DdlArrayKind {
    match s {
        "Fixed" => ddl::DdlArrayKind::Fixed,
        "Dynamic" => ddl::DdlArrayKind::Dynamic,
        "Map" => ddl::DdlArrayKind::Map,
        _ => ddl::DdlArrayKind::None,
    }
}

/// `true` if `map`'s two keys CRC32-hash to the polymorphic field tags
/// (`POLY_TYPE_FIELD` + `POLY_OBJECT_FIELD`). Mirrors `DDLPolymorphicObject.Check`.
fn looks_polymorphic_json(map: &serde_json::Map<String, Value>) -> bool {
    if map.len() != 2 {
        return false;
    }
    let mut saw_type = false;
    let mut saw_object = false;
    for k in map.keys() {
        let h = crc32::hash(k);
        if h == ddl::POLY_TYPE_FIELD {
            saw_type = true;
        } else if h == ddl::POLY_OBJECT_FIELD {
            saw_object = true;
        }
    }
    saw_type && saw_object
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn resave(cfg: ConfigFile) -> ConfigFile {
        ConfigFile::parse(&cfg.save().unwrap()).unwrap()
    }

    #[test]
    fn one_item_string_list_keeps_its_item() {
        let main = json!({
            "DefaultLocators": { "Type": "STRING", "Value": ["barrelTipBase"] },
            "Locators": { "Type": "STRING", "Value": ["upper_barrel", "lower_barrel"] },
        });
        let cfg = ConfigFile {
            magic: DAT1_MAGIC,
            unk: [0; 28],
            config_type: "Test".into(),
            content: json!({ "Main": main }),
            content_tag: CONFIG_CONTENT_TAG,
            original_dat1: None,
        };
        let once = resave(cfg);
        // The wire has no one-item list: a count of 1 reads back as the item itself.
        assert_eq!(once.content["Main"]["DefaultLocators"], json!({ "Type": "STRING", "Value": "barrelTipBase" }));
        assert_eq!(once.content["Main"]["Locators"], main["Locators"]);
        let content = once.content.clone();
        assert_eq!(resave(once).content, content);
    }

    #[test]
    fn typed_labels_survive_a_save() {
        let main = json!({
            "Signed": { "Type": "INT8", "Value": 5 },
            "Wide": { "Type": "UINT32", "Value": 3 },
            "Whole": { "Type": "FLOAT", "Value": 1 },
            "NoNames": { "Type": "STRING", "Value": [] },
            "Nested": { "Type": "OBJECT", "Value": { "Short": { "Type": "INT16", "Value": 7 } } },
            "Plain": 300,
        });
        let cfg = ConfigFile {
            magic: DAT1_MAGIC,
            unk: [0; 28],
            config_type: "Test".into(),
            content: json!({ "Main": main }),
            content_tag: CONFIG_CONTENT_TAG,
            original_dat1: None,
        };
        let got = resave(cfg).content["Main"].clone();
        assert_eq!(got["Signed"], main["Signed"]);
        assert_eq!(got["Wide"], main["Wide"]);
        assert_eq!(got["Whole"], json!({ "Type": "FLOAT", "Value": 1.0 }));
        assert_eq!(got["NoNames"], main["NoNames"]);
        assert_eq!(got["Nested"], main["Nested"]);
        // Untyped JSON still works: its type is inferred from the value.
        assert_eq!(got["Plain"], json!({ "Type": "UINT16", "Value": 300 }));
    }

    #[test]
    fn empty_null_field_holds_one_byte() {
        let mut dat1 = Dat1 {
            magic: DAT1_MAGIC,
            unk1: 0,
            total_size: 0,
            sections: vec![],
            fixups: vec![],
            strings_pool: vec![],
            section_data: vec![],
            sections_map: HashMap::new(),
        };
        let obj = json!({
            "Empty": { "Type": "NULL", "Value": [] },
            "Flag": { "Type": "BOOLEAN", "Value": true },
        });
        let mut out = Vec::new();
        serialize_typed_object(&obj, &mut out, &mut dat1).unwrap();
        // Header 16, two entries 16, two name offsets 8, then the values: the NULL byte, then the flag.
        assert_eq!(out[40..42], [0, 1]);
        let back = deserialize_object_typed(&mut Cursor::new(&out[..]), &dat1).unwrap();
        assert_eq!(back["Flag"], obj["Flag"]);
        assert_eq!(back["Empty"], obj["Empty"]);
    }

    #[test]
    fn string_after_a_byte_pads_from_its_own_start() {
        let mut dat1 = Dat1 {
            magic: DAT1_MAGIC,
            unk1: 0,
            total_size: 0,
            sections: vec![],
            fixups: vec![],
            strings_pool: vec![],
            section_data: vec![],
            sections_map: HashMap::new(),
        };
        let obj = json!({
            "A": { "Type": "BOOLEAN", "Value": true },
            "B": { "Type": "STRING", "Value": "abc" },
            "C": { "Type": "STRING", "Value": "d" },
        });
        let mut out = Vec::new();
        serialize_typed_object(&obj, &mut out, &mut dat1).unwrap();
        // Values start at 16 + 3 * 12 = 52: the flag, then "abc" in 16 + 4 bytes, so "d" starts at 52 + 21.
        assert_eq!(out[73..77], [1, 0, 0, 0]);
        let back = deserialize_object_typed(&mut Cursor::new(&out[..]), &dat1).unwrap();
        assert_eq!(back, obj);
    }

    #[test]
    fn asset_path_adds_config_refs() {
        let path = "configs/equipment/wpn_test_grid.config";
        let cfg = ConfigFile {
            magic: DAT1_MAGIC,
            unk: [0; 28],
            config_type: "Test".into(),
            content: json!({ "Main": { "GridConfig": { "Type": "STRING", "Value": path } } }),
            content_tag: CONFIG_CONTENT_TAG,
            original_dat1: None,
        };
        let once = resave(cfg);
        let dat1 = once.original_dat1.as_ref().unwrap();
        let refs = dat1.get_section_data(CONFIG_REFS_TAG).expect("Config Refs should be added");
        assert_eq!(refs.len(), 16);
        let offset = u32::from_le_bytes(refs[8..12].try_into().unwrap());
        assert_eq!(dat1.get_string(offset).as_deref(), Some("configs\\equipment\\wpn_test_grid.config"));
        assert_eq!(once.content["Main"]["GridConfig"]["Value"], path);
    }

    #[test]
    fn conduit_field_names_survive_a_pool_past_the_header() {
        let mut dat1 = Dat1 {
            magic: DAT1_MAGIC,
            unk1: 0,
            total_size: 0,
            sections: vec![SectionHeader { tag: CONDUIT_BUILT_TAG, offset: 0, size: 0 }],
            fixups: vec![],
            strings_pool: vec![],
            section_data: vec![vec![]],
            sections_map: HashMap::from([(CONDUIT_BUILT_TAG, 0)]),
        };
        dat1.recalculate_section_headers();
        // Enough name bytes that pool-relative offsets pass the header end.
        let content = json!({
            "FirstLongFieldName": { "Type": "String", "Value": "x" },
            "SecondLongFieldName": { "Type": "Struct", "ArrayKind": "Dynamic", "Value": [
                { "InnerLongFieldNameA": { "Type": "Float", "Value": 1.5 } },
                { "InnerLongFieldNameA": { "Type": "Float", "Value": 2.5 } },
            ] },
            "ThirdLongFieldName": { "Type": "Int32", "Value": -3 },
            "EmptyDefault": { "Type": "Default", "Value": [] },
            "OneDefault": { "Type": "Default", "Value": null },
        });
        assert!("FirstLongFieldName SecondLongFieldName ".len() > dat1.header_end());
        let cfg = ConfigFile {
            magic: DAT1_MAGIC,
            unk: [0; 28],
            config_type: "ConduitBuilt".into(),
            content: content.clone(),
            content_tag: CONDUIT_BUILT_TAG,
            original_dat1: Some(dat1),
        };
        let once = resave(cfg);
        assert_eq!(once.content, content);
        assert_eq!(resave(once).content, content);
    }
}
