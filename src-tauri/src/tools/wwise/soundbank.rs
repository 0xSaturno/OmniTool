use crate::core::error::ToolkitError;
use crate::tools::wwise::bnk::patch_bnk_project_id;
use byteorder::{ByteOrder, LittleEndian};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SoundbankEvent {
    pub id: u32,
    pub name: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SoundbankMetadata {
    pub bank_id: u32,
    pub bank_name: String,
    pub bnk_size: usize,
    pub events: Vec<SoundbankEvent>,
}

/// Builds a `.soundbank` asset from a raw Wwise `.bnk` and an list of events.
/// The output includes a 36-byte wrapper and a standard DAT1 container.
pub fn build_soundbank(
    bnk_bytes: &[u8],
    asset_path: &str,
    events: &[String],
) -> Result<Vec<u8>, ToolkitError> {
    // 1. Patch Wwise Project ID in the .bnk bytes first
    let mut patched_bnk = bnk_bytes.to_vec();
    patch_bnk_project_id(&mut patched_bnk)?;

    // 2. Parse Bank ID from BKHD
    if patched_bnk.len() < 16 {
        return Err(ToolkitError::Parse("BNK file is too short".into()));
    }
    let bnk_id = LittleEndian::read_u32(&patched_bnk[12..16]);

    // 3. Build DAT1 Sections
    let mut mida_info = Vec::new();
    let mut midb_strings = Vec::new();
    let mut midc_built = Vec::new();

    // Section 1: Sound Bank Built (0x4765351A)
    // 64 bytes total: bank_id (4), bnk_size (4), 56 zero bytes
    midc_built.extend_from_slice(&bnk_id.to_le_bytes());
    midc_built.extend_from_slice(&(patched_bnk.len() as u32).to_le_bytes());
    midc_built.extend_from_slice(&[0u8; 56]);

    // Section 2: Sound Bank Strings (0x3E8490A3)
    // The first string is always the soundbank asset path (normalized to backslashes)
    let normalized_path = asset_path.replace('/', "\\");
    midb_strings.extend_from_slice(normalized_path.as_bytes());
    midb_strings.push(0);
    while midb_strings.len() % 4 != 0 {
        midb_strings.push(0);
    }

    // Sort event names by their FNV-1a hash ascending
    let mut sorted_events: Vec<(&String, u32)> = events
        .iter()
        .map(|e| (e, crate::core::fnv::hash_string(e)))
        .collect();
    sorted_events.sort_by_key(|&(_, hash)| hash);

    // Write events into midb_strings and mida_info
    for (event_name, hash) in sorted_events {
        let string_offset_units = (midb_strings.len() / 4) as u16;

        // Section 3: Sound Bank Info (0x0E19E37F)
        // 16 bytes: event_id (4), string_offset_units (2), 0x5180 (2), 8 zeroes
        mida_info.extend_from_slice(&hash.to_le_bytes());
        mida_info.extend_from_slice(&string_offset_units.to_le_bytes());
        mida_info.extend_from_slice(&0x5180u16.to_le_bytes());
        mida_info.extend_from_slice(&[0u8; 8]);

        // Append to Strings
        midb_strings.extend_from_slice(event_name.as_bytes());
        midb_strings.push(0);
        while midb_strings.len() % 4 != 0 {
            midb_strings.push(0);
        }
    }

    // Align sizes
    let mida_len = mida_info.len();
    let midb_len = midb_strings.len();
    let midc_len = midc_built.len();
    let bnk_len = patched_bnk.len();

    // DAT1 Header (16) + 4 Section Headers (48) + Strings Pool (32) = 96
    let totsize = 96 + mida_len + midb_len + midc_len + bnk_len;

    let mut out = Vec::with_capacity(36 + totsize);

    // 36-byte Envelope Wrapper
    out.extend_from_slice(&0xC2841216u32.to_le_bytes()); // magic
    out.extend_from_slice(&(totsize as u32).to_le_bytes()); // size (DAT1 size)
    out.extend_from_slice(&[0u8; 28]); // reserved

    // DAT1 Container (starts at file offset 36)
    out.extend_from_slice(b"DAT1"); // magic
    out.extend_from_slice(&0xC2841216u32.to_le_bytes()); // unk1 (matches wrapper magic)
    out.extend_from_slice(&(totsize as u32).to_le_bytes()); // total size
    out.extend_from_slice(&4u16.to_le_bytes()); // sections count
    out.extend_from_slice(&0u16.to_le_bytes()); // unknowns count

    // Section Headers (sorted by tag value)
    // 1. 0x0E19E37F (Sound Bank Info)
    out.extend_from_slice(&0x0E19E37Fu32.to_le_bytes());
    out.extend_from_slice(&((96 + midc_len + midb_len) as u32).to_le_bytes());
    out.extend_from_slice(&(mida_len as u32).to_le_bytes());

    // 2. 0x3E8490A3 (Sound Bank Strings)
    out.extend_from_slice(&0x3E8490A3u32.to_le_bytes());
    out.extend_from_slice(&((96 + midc_len) as u32).to_le_bytes());
    out.extend_from_slice(&(midb_len as u32).to_le_bytes());

    // 3. 0x4765351A (Sound Bank Built)
    out.extend_from_slice(&0x4765351Au32.to_le_bytes());
    out.extend_from_slice(&96u32.to_le_bytes());
    out.extend_from_slice(&(midc_len as u32).to_le_bytes());

    // 4. 0x53F25238 (Wwise Bank Container)
    out.extend_from_slice(&0x53F25238u32.to_le_bytes());
    out.extend_from_slice(&((96 + midc_len + midb_len + mida_len) as u32).to_le_bytes());
    out.extend_from_slice(&(bnk_len as u32).to_le_bytes());

    // DAT1 strings pool: contains "Sound Bank Built File" + padding (total 32 bytes)
    out.extend_from_slice(b"Sound Bank Built File");
    out.extend_from_slice(&[0u8; 11]);

    // Section Bodies
    out.extend_from_slice(&midc_built);
    out.extend_from_slice(&midb_strings);
    out.extend_from_slice(&mida_info);
    out.extend_from_slice(&patched_bnk);

    Ok(out)
}

/// Parses a `.soundbank` asset, extracting metadata and registered events.
pub fn parse_soundbank(bytes: &[u8]) -> Result<SoundbankMetadata, ToolkitError> {
    if bytes.len() < 36 {
        return Err(ToolkitError::Parse("Soundbank envelope is too small".into()));
    }
    let wrapper_magic = LittleEndian::read_u32(&bytes[0..4]);
    if wrapper_magic != 0xC2841216 {
        return Err(ToolkitError::Parse(format!(
            "Invalid wrapper magic {:#010X}",
            wrapper_magic
        )));
    }

    let dat1_slice = &bytes[36..];
    let dat1 = crate::core::dat1::Dat1::parse(dat1_slice)?;

    // Retrieve sections
    let built_data = dat1
        .get_section_data(0x4765351A)
        .ok_or_else(|| ToolkitError::Parse("Missing Sound Bank Built section".into()))?;
    let strings_data = dat1
        .get_section_data(0x3E8490A3)
        .ok_or_else(|| ToolkitError::Parse("Missing Sound Bank Strings section".into()))?;
    let info_data = dat1
        .get_section_data(0x0E19E37F); // May not be present if 0 events

    let bnk_data = dat1
        .get_section_data(0x53F25238)
        .ok_or_else(|| ToolkitError::Parse("Missing Wwise Bank Container section".into()))?;
    let bnk_size = bnk_data.len();

    if built_data.len() < 8 {
        return Err(ToolkitError::Parse("Built section too small".into()));
    }
    let bank_id = LittleEndian::read_u32(&built_data[0..4]);

    // Parse bank name (first string in Strings section)
    let bank_name = if !strings_data.is_empty() {
        let end = strings_data
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(strings_data.len());
        String::from_utf8(strings_data[..end].to_vec())
            .map_err(|e| ToolkitError::Parse(format!("Invalid bank name string: {e}")))?
    } else {
        String::new()
    };

    // Parse events
    let mut events = Vec::new();
    if let Some(info) = info_data {
        let count = info.len() / 16;
        for i in 0..count {
            let offset = i * 16;
            let event_id = LittleEndian::read_u32(&info[offset..offset + 4]);
            let str_offset_units = LittleEndian::read_u16(&info[offset + 4..offset + 6]) as usize;
            let str_offset = str_offset_units * 4;

            if str_offset < strings_data.len() {
                let end = strings_data[str_offset..]
                    .iter()
                    .position(|&b| b == 0)
                    .map(|p| str_offset + p)
                    .unwrap_or(strings_data.len());
                let event_name = String::from_utf8(strings_data[str_offset..end].to_vec())
                    .unwrap_or_else(|_| format!("Event_{}", event_id));
                events.push(SoundbankEvent {
                    id: event_id,
                    name: event_name,
                });
            }
        }
    }

    Ok(SoundbankMetadata {
        bank_id,
        bank_name,
        bnk_size,
        events,
    })
}
