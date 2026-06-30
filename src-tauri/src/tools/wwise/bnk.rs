use crate::core::error::ToolkitError;
use byteorder::{ByteOrder, LittleEndian};
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// Basic BNK header info (unchanged)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
pub struct BnkInfo {
    pub version: u32,
    pub bank_id: u32,
    pub language_id: u32,
    pub project_id: u32,
    pub bnk_size: usize,
}

/// Parses global metadata from Wwise .bnk files.
pub fn parse_bnk(bytes: &[u8]) -> Result<BnkInfo, ToolkitError> {
    if bytes.len() < 32 {
        return Err(ToolkitError::Parse("BNK file too small".into()));
    }
    if &bytes[0..4] != b"BKHD" {
        return Err(ToolkitError::Parse("Invalid BNK magic (expected BKHD)".into()));
    }
    let chunk_size = LittleEndian::read_u32(&bytes[4..8]) as usize;
    if bytes.len() < 8 + chunk_size {
        return Err(ToolkitError::Parse("BKHD chunk size exceeds file size".into()));
    }

    let version = LittleEndian::read_u32(&bytes[8..12]);
    let bank_id = LittleEndian::read_u32(&bytes[12..16]);
    
    // Read language_id (offset 8 in body / offset 16 in file)
    let language_id = if chunk_size >= 12 {
        LittleEndian::read_u32(&bytes[16..20])
    } else {
        0
    };

    // Read project_id (offset 16 in body / offset 24 in file)
    let project_id = if chunk_size >= 20 {
        LittleEndian::read_u32(&bytes[24..28])
    } else {
        0
    };

    Ok(BnkInfo {
        version,
        bank_id,
        language_id,
        project_id,
        bnk_size: bytes.len(),
    })
}

/// Patches the Wwise Project ID at offset 24 in the BKHD chunk to match the 
/// game's expected ID (0x0000187E). Without this, the engine's Wwise SDK 
/// will reject the soundbank due to a project ID mismatch.
pub fn patch_bnk_project_id(bytes: &mut [u8]) -> Result<(), ToolkitError> {
    if bytes.len() < 28 {
        return Err(ToolkitError::Parse("BNK file too small to patch".into()));
    }
    if &bytes[0..4] != b"BKHD" {
        return Err(ToolkitError::Parse("Invalid BNK magic".into()));
    }
    let chunk_size = LittleEndian::read_u32(&bytes[4..8]) as usize;
    if chunk_size < 20 {
        return Err(ToolkitError::Parse(format!(
            "BKHD chunk size ({}) is too small to contain a Project ID field at offset 24",
            chunk_size
        )));
    }
    
    // Overwrite Project ID (file offset 24) with 0x0000187E (LE: 7E 18 00 00)
    LittleEndian::write_u32(&mut bytes[24..28], 0x0000187E);
    Ok(())
}

// ---------------------------------------------------------------------------
// Deep BNK parsing — chunks, DIDX, DATA, HIRC
// ---------------------------------------------------------------------------

/// A top-level chunk found in the BNK file.
#[derive(Debug, Clone)]
pub struct BnkChunk {
    pub magic: [u8; 4],
    pub offset: usize,   // offset of the chunk data (after magic+size)
    pub size: usize,
}

/// An entry in the DIDX (Data Index) chunk — maps a WEM ID to its location in DATA.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DidxEntry {
    pub wem_id: u32,
    pub offset: u32,  // relative to DATA chunk body start
    pub size: u32,
}

/// Codec identification from WEM fmt chunk.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WemInfo {
    pub id: u32,
    pub offset: u32,
    pub size: u32,
    pub codec: String,
    pub codec_id: u16,
    pub sample_rate: u32,
    pub channels: u16,
    pub avg_bitrate: u32,
}

/// HIRC object types we care about for event→WEM chain resolution.
#[derive(Debug, Clone)]
pub enum HircObject {
    Event { id: u32, action_ids: Vec<u32> },
    Action { id: u32, action_type: u16, target_id: u32 },
    Sound { id: u32, source_id: u32, source_type: u8 },
    Container { id: u32, child_ids: Vec<u32> },
}

/// An event with its resolved WEM IDs.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ResolvedEvent {
    pub id: u32,
    pub name: Option<String>,
    pub wem_ids: Vec<u32>,
}

/// Full parsed BNK information.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BnkFullInfo {
    pub version: u32,
    pub bank_id: u32,
    pub language_id: u32,
    pub bnk_size: usize,
    pub wems: Vec<WemInfo>,
    pub events: Vec<ResolvedEvent>,
}

/// Walk top-level chunks in a BNK file.
fn enumerate_chunks(bytes: &[u8]) -> Vec<BnkChunk> {
    let mut chunks = Vec::new();
    let mut pos = 0;
    while pos + 8 <= bytes.len() {
        let mut magic = [0u8; 4];
        magic.copy_from_slice(&bytes[pos..pos + 4]);
        let size = LittleEndian::read_u32(&bytes[pos + 4..pos + 8]) as usize;
        let data_offset = pos + 8;
        if data_offset + size > bytes.len() {
            break; // truncated chunk
        }
        chunks.push(BnkChunk { magic, offset: data_offset, size });
        pos = data_offset + size;
    }
    chunks
}

/// Parse the DIDX chunk to get WEM index entries.
fn parse_didx(data: &[u8]) -> Vec<DidxEntry> {
    let count = data.len() / 12;
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let base = i * 12;
        if base + 12 > data.len() { break; }
        entries.push(DidxEntry {
            wem_id: LittleEndian::read_u32(&data[base..base + 4]),
            offset: LittleEndian::read_u32(&data[base + 4..base + 8]),
            size: LittleEndian::read_u32(&data[base + 8..base + 12]),
        });
    }
    entries
}

/// Detect codec from a WEM file's fmt chunk.
pub fn detect_wem_codec(wem_bytes: &[u8]) -> (u16, u32, u16, u32) {
    // Default fallback values
    let mut codec_id: u16 = 0;
    let mut sample_rate: u32 = 0;
    let mut channels: u16 = 0;
    let mut avg_bitrate: u32 = 0;

    // WEM files start with RIFF or RIFX
    if wem_bytes.len() < 12 { return (codec_id, sample_rate, channels, avg_bitrate); }
    
    let is_riff = &wem_bytes[0..4] == b"RIFF";
    let is_rifx = &wem_bytes[0..4] == b"RIFX";
    if !is_riff && !is_rifx { return (codec_id, sample_rate, channels, avg_bitrate); }

    // Search for "fmt " sub-chunk
    let mut pos = 12; // skip RIFF header (4 magic + 4 size + 4 format)
    while pos + 8 <= wem_bytes.len() {
        let chunk_id = &wem_bytes[pos..pos + 4];
        let chunk_size = if is_rifx {
            // Big-endian
            u32::from_be_bytes([wem_bytes[pos+4], wem_bytes[pos+5], wem_bytes[pos+6], wem_bytes[pos+7]]) as usize
        } else {
            LittleEndian::read_u32(&wem_bytes[pos + 4..pos + 8]) as usize
        };
        
        if chunk_id == b"fmt " {
            let fmt_data = &wem_bytes[pos + 8..];
            if fmt_data.len() >= 16 {
                if is_rifx {
                    codec_id = u16::from_be_bytes([fmt_data[0], fmt_data[1]]);
                    channels = u16::from_be_bytes([fmt_data[2], fmt_data[3]]);
                    sample_rate = u32::from_be_bytes([fmt_data[4], fmt_data[5], fmt_data[6], fmt_data[7]]);
                    avg_bitrate = u32::from_be_bytes([fmt_data[8], fmt_data[9], fmt_data[10], fmt_data[11]]);
                } else {
                    codec_id = LittleEndian::read_u16(&fmt_data[0..2]);
                    channels = LittleEndian::read_u16(&fmt_data[2..4]);
                    sample_rate = LittleEndian::read_u32(&fmt_data[4..8]);
                    avg_bitrate = LittleEndian::read_u32(&fmt_data[8..12]);
                }
            }
            break;
        }
        pos += 8 + chunk_size;
        // Align to 2-byte boundary
        if pos % 2 != 0 { pos += 1; }
    }

    (codec_id, sample_rate, channels, avg_bitrate)
}

/// Map codec ID to human-readable name.
pub fn codec_name(id: u16) -> &'static str {
    match id {
        0x0001 => "PCM",
        0x0002 => "ADPCM",
        0x0011 => "IMA ADPCM",
        0x0069 => "ADPCM",
        0x0165 => "Opus (WEM)",
        0x3039 => "Opus (NX)",
        0x3040 => "Opus",
        0x3041 => "Opus",
        0xFFFE => "Vorbis",
        0xFFFF => "Vorbis",
        _ => "Unknown",
    }
}

// ---------------------------------------------------------------------------
// HIRC (Hierarchy) parsing — minimal, focused on event→WEM chain
// ---------------------------------------------------------------------------

fn read_7bit_encoded_int_be(bytes: &[u8], pos: &mut usize) -> Result<usize, String> {
    let mut value = 0;
    let mut max = 0;
    loop {
        if *pos >= bytes.len() {
            return Err("Unexpected end of bytes".into());
        }
        let cur = bytes[*pos];
        *pos += 1;
        value = (value << 7) | ((cur & 0x7F) as usize);
        if (cur & 0x80) == 0 {
            break;
        }
        max += 1;
        if max >= 10 {
            return Err("Unexpected variable loop count".into());
        }
    }
    Ok(value)
}

fn parse_hirc(data: &[u8]) -> Vec<HircObject> {
    let mut objects = Vec::new();
    if data.len() < 4 { return objects; }
    
    let count = LittleEndian::read_u32(&data[0..4]) as usize;
    
    // First Pass: Collect all HIRC object IDs
    let mut all_hirc_ids = HashSet::new();
    let mut pos = 4;
    for _ in 0..count {
        if pos + 5 > data.len() { break; }
        let _obj_type = data[pos];
        let obj_size = LittleEndian::read_u32(&data[pos + 1..pos + 5]) as usize;
        let obj_data_start = pos + 5;
        if obj_data_start + obj_size > data.len() { break; }
        if obj_size >= 4 {
            let obj_id = LittleEndian::read_u32(&data[obj_data_start..obj_data_start + 4]);
            all_hirc_ids.insert(obj_id);
        }
        pos = obj_data_start + obj_size;
    }

    // Second Pass: Parse the objects
    let mut pos = 4;
    for _ in 0..count {
        if pos + 5 > data.len() { break; }
        
        let obj_type = data[pos];
        let obj_size = LittleEndian::read_u32(&data[pos + 1..pos + 5]) as usize;
        let obj_data_start = pos + 5;
        let obj_data_end = obj_data_start + obj_size;
        
        if obj_data_end > data.len() { break; }
        
        // Object ID is the first 4 bytes of the object data
        if obj_size < 4 { pos = obj_data_end; continue; }
        let obj_id = LittleEndian::read_u32(&data[obj_data_start..obj_data_start + 4]);
        let obj_body = &data[obj_data_start..obj_data_end];

        match obj_type {
            // Type 2: Sound SFX
            0x02 => {
                if obj_body.len() >= 14 {
                    // After the 4-byte ID, Sound SFX has:
                    // 4 bytes: plugin ID/type indicator
                    // 1 byte: stream type (0=embedded, 1=streamed, 2=prefetched)
                    // 4 bytes: source/audio ID (the WEM ID)
                    // 4 bytes: source file ID
                    let source_type = obj_body[8];
                    let source_id = LittleEndian::read_u32(&obj_body[9..13]);
                    objects.push(HircObject::Sound { id: obj_id, source_id, source_type });
                }
            }
            // Type 3: Action
            0x03 => {
                if obj_body.len() >= 10 {
                    // After the 4-byte ID:
                    // 1 byte: action scope
                    // 1 byte: action type
                    // 4 bytes: target object ID
                    let action_type = obj_body[5] as u16;
                    let target_id = LittleEndian::read_u32(&obj_body[6..10]);
                    objects.push(HircObject::Action { id: obj_id, action_type, target_id });
                }
            }
            // Type 4: Event
            0x04 => {
                // After the 4-byte ID:
                // In Wwise v122+: variable-length 7-bit encoded count, then N x 4-byte action IDs
                // In older: 4 byte count, then N x 4-byte action IDs
                let body = &obj_body[4..];
                let mut action_ids = Vec::new();
                if !body.is_empty() {
                    let mut count_pos = 0;
                    if let Ok(count) = read_7bit_encoded_int_be(body, &mut count_pos) {
                        for i in 0..count {
                            let off = count_pos + i * 4;
                            if off + 4 <= body.len() {
                                action_ids.push(LittleEndian::read_u32(&body[off..off + 4]));
                            }
                        }
                    }
                }
                objects.push(HircObject::Event { id: obj_id, action_ids });
            }
            // Types 5-9: Containers (Random, Sequence, Switch, Blend, Actor-Mixer)
            0x05 | 0x06 | 0x07 | 0x08 | 0x09 => {
                let child_ids = extract_container_children(obj_body, &all_hirc_ids);
                objects.push(HircObject::Container { id: obj_id, child_ids });
            }
            _ => {} // Skip all other types
        }

        pos = obj_data_end;
    }

    objects
}

/// Extract child IDs from container objects.
/// Container objects have a NodeBaseParams section followed by child count + IDs.
/// We scan for a child count + child ID list pattern, validating each ID against HIRC objects.
fn extract_container_children(obj_body: &[u8], all_hirc_ids: &HashSet<u32>) -> Vec<u32> {
    let mut children = Vec::new();
    
    // Simple approach: scan all offsets for a plausible child list
    let body = if obj_body.len() > 4 { &obj_body[4..] } else { return children; };
    
    for offset in (0..body.len().saturating_sub(4)).step_by(1) {
        if offset + 4 > body.len() { break; }
        let count = LittleEndian::read_u32(&body[offset..offset + 4]) as usize;
        
        // Plausibility check: count should be small and the array should fit
        if count == 0 || count > 100 { continue; }
        let array_start = offset + 4;
        let array_end = array_start + count * 4;
        if array_end > body.len() { continue; }
        
        // Check that all IDs exist in our top-level HIRC IDs
        let mut ids = Vec::with_capacity(count);
        let mut valid = true;
        for i in 0..count {
            let id = LittleEndian::read_u32(&body[array_start + i * 4..array_start + i * 4 + 4]);
            if !all_hirc_ids.contains(&id) {
                valid = false;
                break;
            }
            ids.push(id);
        }
        if !valid { continue; }
        
        // We found a plausible child list — use the first one we find
        // that has children (prefer larger lists)
        if ids.len() > children.len() {
            children = ids;
        }
    }
    
    children
}

// ---------------------------------------------------------------------------
// Chain resolver: Event → Action → Sound/Container → WEM ID
// ---------------------------------------------------------------------------

fn resolve_event_wem_chain(objects: &[HircObject]) -> Vec<ResolvedEvent> {
    // Build lookup maps
    let mut actions: HashMap<u32, (u16, u32)> = HashMap::new();   // id → (type, target_id)
    let mut sounds: HashMap<u32, (u32, u8)> = HashMap::new();     // id → (source_id, source_type)
    let mut containers: HashMap<u32, Vec<u32>> = HashMap::new();   // id → child_ids

    for obj in objects {
        match obj {
            HircObject::Action { id, action_type, target_id } => {
                actions.insert(*id, (*action_type, *target_id));
            }
            HircObject::Sound { id, source_id, source_type } => {
                sounds.insert(*id, (*source_id, *source_type));
            }
            HircObject::Container { id, child_ids } => {
                containers.insert(*id, child_ids.clone());
            }
            _ => {}
        }
    }

    let mut events = Vec::new();
    
    for obj in objects {
        if let HircObject::Event { id, action_ids } = obj {
            let mut wem_ids = Vec::new();
            let mut visited = HashSet::new();
            
            for action_id in action_ids {
                if let Some((_action_type, target_id)) = actions.get(action_id) {
                    collect_wem_ids(*target_id, &sounds, &containers, &mut wem_ids, &mut visited);
                }
            }
            
            // Deduplicate WEM IDs
            wem_ids.sort();
            wem_ids.dedup();
            
            events.push(ResolvedEvent {
                id: *id,
                name: None, // Will be resolved later from STID or external data
                wem_ids,
            });
        }
    }

    events
}

/// Recursively collect WEM IDs by walking the object graph.
fn collect_wem_ids(
    target_id: u32,
    sounds: &HashMap<u32, (u32, u8)>,
    containers: &HashMap<u32, Vec<u32>>,
    wem_ids: &mut Vec<u32>,
    visited: &mut HashSet<u32>,
) {
    if !visited.insert(target_id) { return; } // cycle prevention
    
    // Is it a Sound SFX?
    if let Some((source_id, _source_type)) = sounds.get(&target_id) {
        if *source_id != 0 {
            wem_ids.push(*source_id);
        }
        return;
    }
    
    // Is it a Container?
    if let Some(child_ids) = containers.get(&target_id) {
        for child_id in child_ids {
            collect_wem_ids(*child_id, sounds, containers, wem_ids, visited);
        }
    }
}

// ---------------------------------------------------------------------------
// STID (String ID) chunk — maps object IDs to names
// ---------------------------------------------------------------------------

fn parse_stid(data: &[u8]) -> HashMap<u32, String> {
    let mut names = HashMap::new();
    if data.len() < 8 { return names; }
    
    // STID format:
    // 4 bytes: string type (1 = UTF8)
    // 4 bytes: number of entries
    // Then for each entry:
    //   4 bytes: object ID
    //   1 byte: string length
    //   N bytes: string data
    let count = LittleEndian::read_u32(&data[4..8]) as usize;
    let mut pos = 8;
    
    for _ in 0..count {
        if pos + 5 > data.len() { break; }
        let obj_id = LittleEndian::read_u32(&data[pos..pos + 4]);
        let str_len = data[pos + 4] as usize;
        pos += 5;
        if pos + str_len > data.len() { break; }
        if let Ok(name) = std::str::from_utf8(&data[pos..pos + str_len]) {
            names.insert(obj_id, name.to_string());
        }
        pos += str_len;
    }
    
    names
}

// ---------------------------------------------------------------------------
// Public API: Full BNK parse
// ---------------------------------------------------------------------------

/// Parse a BNK file fully: header, DIDX/DATA (WEMs), HIRC (events), STID (names).
pub fn parse_bnk_full(bytes: &[u8]) -> Result<BnkFullInfo, ToolkitError> {
    let header = parse_bnk(bytes)?;
    let chunks = enumerate_chunks(bytes);

    // Find important chunks
    let mut didx_entries = Vec::new();
    let mut data_chunk: Option<&BnkChunk> = None;
    let mut hirc_objects = Vec::new();
    let mut stid_names = HashMap::new();

    for chunk in &chunks {
        match &chunk.magic {
            b"DIDX" => {
                didx_entries = parse_didx(&bytes[chunk.offset..chunk.offset + chunk.size]);
            }
            b"DATA" => {
                data_chunk = Some(chunk);
            }
            b"HIRC" => {
                hirc_objects = parse_hirc(&bytes[chunk.offset..chunk.offset + chunk.size]);
            }
            b"STID" => {
                stid_names = parse_stid(&bytes[chunk.offset..chunk.offset + chunk.size]);
            }
            _ => {}
        }
    }

    // Build WEM info list with codec detection
    let mut wems = Vec::new();
    if let Some(data_c) = data_chunk {
        for entry in &didx_entries {
            let wem_start = data_c.offset + entry.offset as usize;
            let wem_end = wem_start + entry.size as usize;
            
            let (codec_id, sample_rate, channels, avg_bitrate) = if wem_end <= bytes.len() {
                detect_wem_codec(&bytes[wem_start..wem_end])
            } else {
                (0, 0, 0, 0)
            };

            wems.push(WemInfo {
                id: entry.wem_id,
                offset: entry.offset,
                size: entry.size,
                codec: codec_name(codec_id).to_string(),
                codec_id,
                sample_rate,
                channels,
                avg_bitrate,
            });
        }
    }

    // Resolve event → WEM chains
    let mut events = resolve_event_wem_chain(&hirc_objects);
    
    // Apply names from STID
    for event in &mut events {
        if let Some(name) = stid_names.get(&event.id) {
            event.name = Some(name.clone());
        }
    }

    // Sort events by name (named first, then by ID)
    events.sort_by(|a, b| {
        match (&a.name, &b.name) {
            (Some(na), Some(nb)) => na.to_lowercase().cmp(&nb.to_lowercase()),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.id.cmp(&b.id),
        }
    });

    Ok(BnkFullInfo {
        version: header.version,
        bank_id: header.bank_id,
        language_id: header.language_id,
        bnk_size: header.bnk_size,
        wems,
        events,
    })
}

/// Extract raw WEM bytes for a specific WEM ID from a parsed BNK.
pub fn extract_wem_bytes(bnk_bytes: &[u8], wem_id: u32) -> Result<Vec<u8>, ToolkitError> {
    let chunks = enumerate_chunks(bnk_bytes);
    
    let mut didx_entries = Vec::new();
    let mut data_offset: Option<usize> = None;

    for chunk in &chunks {
        match &chunk.magic {
            b"DIDX" => {
                didx_entries = parse_didx(&bnk_bytes[chunk.offset..chunk.offset + chunk.size]);
            }
            b"DATA" => {
                data_offset = Some(chunk.offset);
            }
            _ => {}
        }
    }

    let data_start = data_offset
        .ok_or_else(|| ToolkitError::Parse("BNK has no DATA chunk".into()))?;

    let entry = didx_entries
        .iter()
        .find(|e| e.wem_id == wem_id)
        .ok_or_else(|| ToolkitError::Parse(format!("WEM ID {} not found in DIDX", wem_id)))?;

    let wem_start = data_start + entry.offset as usize;
    let wem_end = wem_start + entry.size as usize;
    
    if wem_end > bnk_bytes.len() {
        return Err(ToolkitError::Parse(format!(
            "WEM {} data extends beyond file bounds", wem_id
        )));
    }

    Ok(bnk_bytes[wem_start..wem_end].to_vec())
}

/// Repacks a raw .bnk buffer by replacing specific WEM files.
/// Updates the DIDX metadata offsets/sizes and rebuilds the DATA chunk with 16-byte alignment.
pub fn repack_bnk_wems(
    bnk_bytes: &[u8],
    replacements: &HashMap<u32, Vec<u8>>,
) -> Result<Vec<u8>, ToolkitError> {
    let chunks = enumerate_chunks(bnk_bytes);
    
    // Parse original DIDX entries to find all WEM IDs and their order
    let mut didx_entries = Vec::new();
    let mut data_offset: Option<usize> = None;

    for chunk in &chunks {
        match &chunk.magic {
            b"DIDX" => {
                didx_entries = parse_didx(&bnk_bytes[chunk.offset..chunk.offset + chunk.size]);
            }
            b"DATA" => {
                data_offset = Some(chunk.offset);
            }
            _ => {}
        }
    }

    let data_start = data_offset
        .ok_or_else(|| ToolkitError::Parse("BNK has no DATA chunk".into()))?;

    // Rebuild DIDX and DATA
    let mut data_body = Vec::new();
    let mut new_didx_body = Vec::new();

    for (idx, entry) in didx_entries.iter().enumerate() {
        // Get the WEM data: either from replacements or extracted from the original BNK
        let wem_data = if let Some(replaced_bytes) = replacements.get(&entry.wem_id) {
            replaced_bytes.clone()
        } else {
            let original_start = data_start + entry.offset as usize;
            let original_end = original_start + entry.size as usize;
            if original_end > bnk_bytes.len() {
                return Err(ToolkitError::Parse(format!(
                    "Original WEM {} extends beyond file bounds",
                    entry.wem_id
                )));
            }
            bnk_bytes[original_start..original_end].to_vec()
        };

        let current_offset = data_body.len() as u32;
        let wem_size = wem_data.len() as u32;

        // Write DIDX entry: ID (4 bytes), Offset (4 bytes), Size (4 bytes)
        let mut entry_bytes = [0u8; 12];
        LittleEndian::write_u32(&mut entry_bytes[0..4], entry.wem_id);
        LittleEndian::write_u32(&mut entry_bytes[4..8], current_offset);
        LittleEndian::write_u32(&mut entry_bytes[8..12], wem_size);
        new_didx_body.extend_from_slice(&entry_bytes);

        // Append WEM data
        data_body.extend_from_slice(&wem_data);

        // Add 16-byte alignment zero-padding if not the last WEM
        if idx < didx_entries.len() - 1 {
            let rem = data_body.len() % 16;
            if rem > 0 {
                let pad = 16 - rem;
                data_body.extend(std::iter::repeat(0u8).take(pad));
            }
        }
    }

    // Now re-assemble the BNK file.
    // In BNK files:
    // - BKHD must be first
    // - DIDX and DATA chunks are inserted/overwritten
    // - All other chunks are appended in their original order.
    let mut out = Vec::new();

    let write_chunk = |out_buf: &mut Vec<u8>, magic: &[u8; 4], body: &[u8]| {
        out_buf.extend_from_slice(magic);
        let size_bytes = (body.len() as u32).to_le_bytes();
        out_buf.extend_from_slice(&size_bytes);
        out_buf.extend_from_slice(body);
    };

    // Write BKHD first
    let mut written_bkhd = false;
    for chunk in &chunks {
        if chunk.magic == *b"BKHD" {
            let body = &bnk_bytes[chunk.offset..chunk.offset + chunk.size];
            write_chunk(&mut out, b"BKHD", body);
            written_bkhd = true;
            break;
        }
    }
    if !written_bkhd {
        return Err(ToolkitError::Parse("BNK has no BKHD chunk".into()));
    }

    // Write DIDX and DATA chunks
    if !new_didx_body.is_empty() {
        write_chunk(&mut out, b"DIDX", &new_didx_body);
        write_chunk(&mut out, b"DATA", &data_body);
    }

    // Write remaining non-BKHD, non-DIDX, non-DATA chunks in original order
    for chunk in &chunks {
        if chunk.magic != *b"BKHD" && chunk.magic != *b"DIDX" && chunk.magic != *b"DATA" {
            let body = &bnk_bytes[chunk.offset..chunk.offset + chunk.size];
            write_chunk(&mut out, &chunk.magic, body);
        }
    }

    Ok(out)
}
