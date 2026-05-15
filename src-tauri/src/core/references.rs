//! Asset reference extraction.
//!
//! Scans a parsed DAT1 for outbound asset references using:
//!   1. Known `ReferencesSection` tags (16-byte entries: u64 asset_id,
//!      u32 string_offset into the DAT1 strings pool, u32 extension type
//!      hash).
//!   2. Path-like strings in the DAT1 strings pool (best-effort), hashed
//!      with the standard DAT1 CRC64 to recover their packed asset id.
//!


use crate::core::crc64;
use crate::core::dat1::{Dat1, DAT1_MAGIC};
use crate::core::error::{Result, ToolkitError};

#[derive(Debug, Clone)]
pub struct RawReference {
    pub asset_id: u64,
    pub filename: Option<String>,
    pub source: String,
}

/// Section tags that store 16-byte reference entries `<u64 asset_id, u32
/// string_offset, u32 ext_hash>`.
const REF_SECTION_TAGS: &[(u32, &str)] = &[
    (0x58B8558A, "Config Refs"),
    (0x2F4056CE, "Conduit Refs"),
    (0x3AB204B9, "Actor Refs"),
    (0xFBD496D6, "NodeGraph Refs"),
    (0x91DE11D9, "Zone Refs"),
];

/// Map CRC32 of `.<ext>` to a human-readable type label. Used to annotate
/// reference entries with their target asset type when the entry carries
/// an extension hash.
const EXT_HASHES: &[(u32, &str)] = &[
    (0x37E72F50, "Actor"),
    (0xA9C3E1B8, "AnimClip"),
    (0xD1AD9F7C, "AnimSet"),
    (0xF56C78E4, "Atmosphere"),
    (0x57B67E8F, "Cinematic2"),
    (0xEA7EFDD4, "Conduit"),
    (0xA9F149C4, "Config"),
    (0xE978B5BA, "Level"),
    (0x08BD74BA, "LevelLight"),
    (0x29E2F18F, "Localization"),
    (0xB5AAFACC, "Material"),
    (0x47048393, "MaterialGraph"),
    (0xA4070B70, "Model"),
    (0x53B8EA03, "NodeGraph"),
    (0x9676F576, "PerformanceClip"),
    (0xD1BF8CDA, "PerformanceSet"),
    (0xFFA86BB6, "Soundbank"),
    (0x95A3A227, "Texture"),
    (0xDABA2AEA, "VisualEffect"),
    (0xD8A92608, "WwiseLookup"),
    (0xE1EE9AA6, "Zone"),
];

pub fn ext_label(hash: u32) -> Option<&'static str> {
    EXT_HASHES.iter().find(|(h, _)| *h == hash).map(|(_, n)| *n)
}

fn is_pathlike(s: &str) -> bool {
    // Must contain an extension dot AND a separator. Reject very short or
    // very long candidates to avoid noise from non-path tokens.
    s.len() >= 5
        && s.len() < 512
        && s.contains('.')
        && (s.contains('/') || s.contains('\\'))
}

/// Iterate null-terminated strings in the DAT1 strings pool, invoking
/// `f(string_offset_in_pool, string)` for each entry.
fn for_each_string(strings: &[u8], mut f: impl FnMut(u32, &str)) {
    let mut start = 0usize;
    while start < strings.len() {
        let rel = strings[start..].iter().position(|&b| b == 0);
        let end = rel.map(|p| start + p).unwrap_or(strings.len());
        if end > start {
            if let Ok(s) = std::str::from_utf8(&strings[start..end]) {
                f(start as u32, s);
            }
        }
        start = end + 1;
    }
}

/// Extract all outbound references from a parsed DAT1. Returns one entry
/// per (asset_id, source) pair; the caller is expected to de-duplicate
/// by asset_id when presenting to the user.
pub fn extract_references(dat1: &Dat1) -> Vec<RawReference> {
    let mut out: Vec<RawReference> = Vec::new();

    // 1) Structured references sections
    for &(tag, label) in REF_SECTION_TAGS {
        if let Some(data) = dat1.get_section_data(tag) {
            const ENTRY: usize = 16;
            for chunk in data.chunks_exact(ENTRY) {
                let aid = u64::from_le_bytes(chunk[0..8].try_into().unwrap());
                let s_off = u32::from_le_bytes(chunk[8..12].try_into().unwrap());
                let ext_hash = u32::from_le_bytes(chunk[12..16].try_into().unwrap());
                let filename = dat1.get_string(s_off);
                let src = match ext_label(ext_hash) {
                    Some(ext) => format!("{label} → {ext} ({:08X})", tag),
                    None => format!("{label} ({:08X})", tag),
                };
                out.push(RawReference {
                    asset_id: aid,
                    filename,
                    source: src,
                });
            }
        }
    }

    // 2) Path-like strings in the strings pool
    for_each_string(&dat1.strings_pool, |_off, s| {
        if is_pathlike(s) {
            let aid = crc64::hash(s);
            out.push(RawReference {
                asset_id: aid,
                filename: Some(s.to_string()),
                source: "Strings Block".into(),
            });
        }
    });

    out
}

/// Convenience: peel the optional 36-byte RCRA wrapper, parse DAT1, and
/// extract references. Returns `Ok(vec![])` for assets that aren't DAT1
/// containers (textures, etc.) rather than an error — callers usually
/// scan many assets at once.
pub fn extract_references_from_bytes(bytes: &[u8]) -> Result<Vec<RawReference>> {
    if bytes.len() < 4 {
        return Ok(Vec::new());
    }
    let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let dat1_slice: &[u8] = if magic == DAT1_MAGIC {
        bytes
    } else if bytes.len() >= 40
        && u32::from_le_bytes(bytes[36..40].try_into().unwrap()) == DAT1_MAGIC
    {
        &bytes[36..]
    } else {
        return Ok(Vec::new());
    };

    let dat1 = Dat1::parse(dat1_slice).map_err(|e| match e {
        ToolkitError::InvalidMagic { .. } => e,
        other => other,
    })?;
    Ok(extract_references(&dat1))
}
