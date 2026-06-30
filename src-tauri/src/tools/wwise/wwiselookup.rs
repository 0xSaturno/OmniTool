use crate::core::error::ToolkitError;
use byteorder::{ByteOrder, LittleEndian};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WwiseLookupBank {
    pub bank_asset_id: u64,
    pub string_offset: u32,
    pub bank_name: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WwiseLookupEventMeta {
    pub bank_asset_id: u64,
    pub unknown1: u64,
    pub padding: u64,
    pub event_name_offset: u32,
    pub unknown2: u32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WwiseLookupEvent {
    pub event_id: u32,
    pub event_name: String,
    pub bank_asset_id: u64,
    pub meta: WwiseLookupEventMeta,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WwiseLookupMetadata {
    pub banks: Vec<WwiseLookupBank>,
    pub events: Vec<WwiseLookupEvent>,
}

/// Parses the global `events.wwiselookup` asset.
pub fn parse_wwiselookup(bytes: &[u8]) -> Result<WwiseLookupMetadata, ToolkitError> {
    if bytes.len() < 36 {
        return Err(ToolkitError::Parse("Lookup file is too small".into()));
    }
    let wrapper_magic = LittleEndian::read_u32(&bytes[0..4]);
    if wrapper_magic != 0xFA1D5989 {
        return Err(ToolkitError::Parse(format!(
            "Invalid wwiselookup wrapper magic {:#010X}",
            wrapper_magic
        )));
    }

    let dat1_slice = &bytes[36..];
    let dat1 = crate::core::dat1::Dat1::parse(dat1_slice)?;

    let banks_data = dat1
        .get_section_data(0x52B343E8)
        .ok_or_else(|| ToolkitError::Parse("Missing Banks section".into()))?;
    let metas_data = dat1
        .get_section_data(0x739B21E0)
        .ok_or_else(|| ToolkitError::Parse("Missing Event Metas section".into()))?;
    let ids_data = dat1
        .get_section_data(0x7F9A96AA)
        .ok_or_else(|| ToolkitError::Parse("Missing Event IDs section".into()))?;

    // Parse Banks (12 bytes/entry)
    let banks_count = banks_data.len() / 12;
    let mut banks = Vec::with_capacity(banks_count);
    for i in 0..banks_count {
        let offset = i * 12;
        let bank_asset_id = LittleEndian::read_u64(&banks_data[offset..offset + 8]);
        let string_offset = LittleEndian::read_u32(&banks_data[offset + 8..offset + 12]);
        let bank_name = dat1.get_string(string_offset).unwrap_or_default();
        banks.push(WwiseLookupBank {
            bank_asset_id,
            string_offset,
            bank_name,
        });
    }

    // Parse Events (parallel arrays: IDs is 4 bytes/entry, Metas is 32 bytes/entry)
    let events_count = ids_data.len() / 4;
    let mut events = Vec::with_capacity(events_count);
    for i in 0..events_count {
        let id_offset = i * 4;
        let meta_offset = i * 32;

        if meta_offset + 32 > metas_data.len() {
            break;
        }

        let event_id = LittleEndian::read_u32(&ids_data[id_offset..id_offset + 4]);

        let bank_asset_id = LittleEndian::read_u64(&metas_data[meta_offset..meta_offset + 8]);
        let unknown1 = LittleEndian::read_u64(&metas_data[meta_offset + 8..meta_offset + 16]);
        let padding = LittleEndian::read_u64(&metas_data[meta_offset + 16..meta_offset + 24]);
        let event_name_offset = LittleEndian::read_u32(&metas_data[meta_offset + 24..meta_offset + 28]);
        let unknown2 = LittleEndian::read_u32(&metas_data[meta_offset + 28..meta_offset + 32]);

        let event_name = dat1.get_string(event_name_offset).unwrap_or_default();

        events.push(WwiseLookupEvent {
            event_id,
            event_name,
            bank_asset_id,
            meta: WwiseLookupEventMeta {
                bank_asset_id,
                unknown1,
                padding,
                event_name_offset,
                unknown2,
            },
        });
    }

    Ok(WwiseLookupMetadata { banks, events })
}

/// Patches the global `events.wwiselookup` asset with new soundbanks and their event lists.
/// Ensures that bank entries and event lists are sorted correctly as required by the game engine.
pub fn patch_wwiselookup(
    vanilla_bytes: &[u8],
    new_assets: &[(String, Vec<String>)],
) -> Result<Vec<u8>, ToolkitError> {
    // 1. Parse Vanilla lookup metadata and DAT1
    let vanilla_metadata = parse_wwiselookup(vanilla_bytes)?;
    let dat1_slice = &vanilla_bytes[36..];
    let mut dat1 = crate::core::dat1::Dat1::parse(dat1_slice)?;

    let mut new_strings_pool = dat1.strings_pool.clone();
    let mut merged_banks = vanilla_metadata.banks.clone();
    let mut merged_events = vanilla_metadata.events.clone();

    // 2. Insert new assets
    for (bank_path, event_names) in new_assets {
        let normalized_path = bank_path.replace('/', "\\");
        let bank_asset_id = crate::core::crc64::hash(&normalized_path);

        // Check if bank already exists
        if !merged_banks.iter().any(|b| b.bank_asset_id == bank_asset_id) {
            // String offset relative to start of DAT1 container (header is 52 bytes)
            let bank_offset = (new_strings_pool.len() + 52) as u32;
            new_strings_pool.extend_from_slice(normalized_path.as_bytes());
            new_strings_pool.push(0);

            merged_banks.push(WwiseLookupBank {
                bank_asset_id,
                string_offset: bank_offset,
                bank_name: normalized_path.clone(),
            });
        }

        // Add events
        for event_name in event_names {
            let event_id = crate::core::fnv::hash_string(event_name);

            // Check if event already exists
            if merged_events.iter().any(|e| e.event_id == event_id) {
                continue; // Avoid duplicating events
            }

            let event_name_offset = (new_strings_pool.len() + 52) as u32;
            new_strings_pool.extend_from_slice(event_name.as_bytes());
            new_strings_pool.push(0);

            merged_events.push(WwiseLookupEvent {
                event_id,
                event_name: event_name.clone(),
                bank_asset_id,
                meta: WwiseLookupEventMeta {
                    bank_asset_id,
                    unknown1: 0,
                    padding: 0,
                    event_name_offset,
                    unknown2: 0,
                },
            });
        }
    }

    // Align strings pool to a 4-byte boundary
    while new_strings_pool.len() % 4 != 0 {
        new_strings_pool.push(0);
    }

    // 3. Sort merged lists
    // Banks are sorted by bank_asset_id ascending
    merged_banks.sort_by_key(|b| b.bank_asset_id);
    // Events are sorted by event_id ascending
    merged_events.sort_by_key(|e| e.event_id);

    // 4. Serialize section bodies
    let mut banks_section = Vec::with_capacity(merged_banks.len() * 12);
    for b in &merged_banks {
        banks_section.extend_from_slice(&b.bank_asset_id.to_le_bytes());
        banks_section.extend_from_slice(&b.string_offset.to_le_bytes());
    }

    let mut ids_section = Vec::with_capacity(merged_events.len() * 4);
    let mut metas_section = Vec::with_capacity(merged_events.len() * 32);
    for e in &merged_events {
        ids_section.extend_from_slice(&e.event_id.to_le_bytes());

        metas_section.extend_from_slice(&e.meta.bank_asset_id.to_le_bytes());
        metas_section.extend_from_slice(&e.meta.unknown1.to_le_bytes());
        metas_section.extend_from_slice(&e.meta.padding.to_le_bytes());
        metas_section.extend_from_slice(&e.meta.event_name_offset.to_le_bytes());
        metas_section.extend_from_slice(&e.meta.unknown2.to_le_bytes());
    }

    // 5. Update and rebuild DAT1 container
    dat1.strings_pool = new_strings_pool;
    dat1.set_section_data(0x52B343E8, banks_section)?;
    dat1.set_section_data(0x739B21E0, metas_section)?;
    dat1.set_section_data(0x7F9A96AA, ids_section)?;

    let dat1_bytes = dat1.save();
    let dat1_len = dat1_bytes.len();

    // 6. Build Envelope Wrapper
    let mut out = Vec::with_capacity(36 + dat1_len);
    out.extend_from_slice(&0xFA1D5989u32.to_le_bytes()); // magic
    out.extend_from_slice(&(dat1_len as u32).to_le_bytes()); // size (DAT1 size)
    out.extend_from_slice(&[0u8; 28]); // reserved
    out.extend_from_slice(&dat1_bytes);

    Ok(out)
}
