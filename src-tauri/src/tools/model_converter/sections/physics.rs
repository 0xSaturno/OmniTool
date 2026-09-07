//! Model Physics Data.
//!
//! This section is not an Insomniac structure at all — it is a stock Havok
//! tagfile (`TAG0` container, `SDKV` version stamp, then `DATA`/`TYPE`/`TPTR`
//! chunks). Rift Apart ships Havok 2017 here, carrying `hknpPhysicsSceneData`
//! for the collision proxies and `hclClothContainer` for cloth. Editing it
//! means going through Havok's own serialisation, so this module only
//! identifies the payload rather than parsing it.

use crate::core::error::Result;

pub const TAG_PHYSICS_DATA: u32 = 0xEFD92E68;

#[derive(Debug, Clone, Default)]
pub struct PhysicsData {
    pub is_havok_tagfile: bool,
    /// e.g. "20170100".
    pub sdk_version: Option<String>,
    /// Havok class names found in the tagfile's type table.
    pub classes: Vec<String>,
}

/// Havok chunk headers are big-endian: 24-bit size, then a 4-byte tag.
fn chunk_tag(data: &[u8], at: usize) -> Option<&[u8]> {
    data.get(at + 4..at + 8)
}

impl PhysicsData {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut out = Self::default();
        if chunk_tag(data, 0) != Some(b"TAG0") {
            return Ok(out);
        }
        out.is_havok_tagfile = true;
        if chunk_tag(data, 8) == Some(b"SDKV") && data.len() >= 24 {
            out.sdk_version = String::from_utf8(data[16..24].to_vec()).ok();
        }

        // The type table stores class names as NUL-terminated ASCII; scanning for
        // the known prefixes is enough to report what the payload holds.
        for needle in [
            &b"hknpPhysicsSceneData"[..],
            &b"hclClothContainer"[..],
            &b"hkaSkeleton"[..],
            &b"hknpShape"[..],
        ] {
            if data
                .windows(needle.len())
                .any(|w| w == needle)
            {
                out.classes.push(String::from_utf8_lossy(needle).into_owned());
            }
        }
        Ok(out)
    }
}
