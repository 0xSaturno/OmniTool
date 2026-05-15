//! Asset id packing, asset categories, locales,
//! and the per-category stream-extension table.
//!
//! The raw CRC64 → packed-id transform itself lives in
//! [`crate::core::crc64`]; this module is concerned with what the bits of an
//! already-packed id *mean*, and with conventions used when emitting filenames
//! for extracted assets.

// ---------------------------------------------------------------------------
// AssetIdFlags
// ---------------------------------------------------------------------------

/// Mask covering the flag bits (top 2 of the packed asset id).
pub const ASSET_ID_FLAG_MASK: u64 = 0xC000_0000_0000_0000;

/// Mask covering the 62-bit hash payload (everything except the flag bits).
pub const ASSET_ID_PAYLOAD_MASK: u64 = 0x3FFF_FFFF_FFFF_FFFF;

/// Mask of the `Wwise` discriminator bit ("bit 62" in the doc's
/// 1-indexed-from-the-top numbering, i.e. position 61 in a 0-indexed `u64`).
/// Only meaningful when the `Ext` pattern is set in the top 2 bits.
pub const ASSET_ID_WWISE_BIT: u64 = 1 << 61;

/// Top-bit flags packed into a 64-bit asset id.
///
/// After computing the raw CRC64 of a normalized path the engine does:
///
/// ```text
/// asset_id = (raw_hash >> 2) | (flags << 62)
/// ```
///
/// so the high two bits encode one of the variants below. See the table in
/// [`crate::core::crc64`] for the bit pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssetIdFlags {
    /// Top 2 bits `0b00` — legacy / unflagged. Almost never seen on shipped
    /// assets; kept for completeness.
    None,
    /// Top 2 bits `0b10` — normal 64-bit shipped asset id derived from a
    /// path. Mask: `0x8000_0000_0000_0000`.
    Shipped,
    /// Top 2 bits `0b11`, with the `ASSET_ID_WWISE_BIT` (bit 62) clear.
    /// The id is actually a 32-bit external hash sitting in the low 32 bits;
    /// bits 32..61 hold the hash-type discriminator. Mask:
    /// `0xC000_0000_0000_0000`.
    Ext,
    /// Top 2 bits `0b11`, with `ASSET_ID_WWISE_BIT` (bit 62) also set —
    /// a Wwise FNV id whose payload is a 32-bit FNV hash of a Wwise sound
    /// name. Combined mask: `0xE000_0000_0000_0000`.
    Wwise,
}

impl AssetIdFlags {
    /// Decode the flag bits from a packed asset id.
    pub fn from_id(asset_id: u64) -> Self {
        let top = (asset_id >> 62) & 0b11;
        match top {
            0b00 => AssetIdFlags::None,
            0b10 => AssetIdFlags::Shipped,
            0b11 => {
                if asset_id & ASSET_ID_WWISE_BIT != 0 {
                    AssetIdFlags::Wwise
                } else {
                    AssetIdFlags::Ext
                }
            }
            _ => AssetIdFlags::None, // 0b01 — unused / reserved
        }
    }

    /// Returns the raw 2-bit pattern this flag occupies in the top of the id
    /// (i.e. the value of bits 62..64). For `Wwise`, the extra discriminator
    /// at bit 61 lives outside this 2-bit window — see [`ASSET_ID_WWISE_BIT`].
    pub fn bits(self) -> u64 {
        match self {
            AssetIdFlags::None => 0b00,
            AssetIdFlags::Shipped => 0b10,
            AssetIdFlags::Ext | AssetIdFlags::Wwise => 0b11,
        }
    }
}

/// Recover the raw 62-bit hash payload from a packed asset id.
pub fn payload(asset_id: u64) -> u64 {
    asset_id & ASSET_ID_PAYLOAD_MASK
}

/// Build a packed `Shipped` 64-bit asset id from a raw CRC64 value.
///
/// Equivalent to the post-processing in [`crate::core::crc64::hash_raw`].
pub fn pack_shipped(raw_crc: u64) -> u64 {
    (raw_crc >> 2) | (AssetIdFlags::Shipped.bits() << 62)
}

/// Build a packed `Ext` asset id from a 32-bit external hash.
///
/// The 32-bit hash is placed in the low 32 bits; the high 32 bits hold the
/// `Ext` flag (`0b11` in bits 62..64) and an optional hash-type
/// discriminator at bits 32..61.
pub fn pack_ext(raw_hash: u32) -> u64 {
    (raw_hash as u64) | (AssetIdFlags::Ext.bits() << 62)
}

/// Build a packed Wwise asset id from a 32-bit FNV hash.
///
/// Wwise ids are `Ext` ids with the additional bit-62 (`ASSET_ID_WWISE_BIT`)
/// set, which lets the TOC distinguish them from generic 32-bit external ids.
pub fn pack_wwise(fnv_hash: u32) -> u64 {
    pack_ext(fnv_hash) | ASSET_ID_WWISE_BIT
}

// ---------------------------------------------------------------------------
// AssetCategory + StreamExtensions
// ---------------------------------------------------------------------------

/// Categories assigned to TOC asset spans. The numeric value is the
/// span index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AssetCategory {
    /// Built / DAT1 assets — no extension added.
    Built = 0,
    /// Texture stream (`.stream`).
    TextureStream = 1,
    /// Unknown 2 (`.unk2strm`) — DAT1 audio? Always empty in RCRA.
    Unknown2 = 2,
    /// Audio (`.wem`).
    Audio = 3,
    /// Unknown 4 (`.unk4strm`) — DAT1 animstream? Always empty in RCRA.
    Unknown4 = 4,
    /// Animation stream (`.animstrm`).
    AnimationStream = 5,
    /// Unknown 6 (`.unk6strm`) — DAT1 zonelight? Always empty in RCRA.
    Unknown6 = 6,
    /// ZoneGrid / light grid stream (`.lgstream`).
    ZoneGrid = 7,
}

impl AssetCategory {
    /// Decode a span index back into a category. Returns `None` for
    /// out-of-range values.
    pub fn from_index(idx: u8) -> Option<Self> {
        match idx {
            0 => Some(AssetCategory::Built),
            1 => Some(AssetCategory::TextureStream),
            2 => Some(AssetCategory::Unknown2),
            3 => Some(AssetCategory::Audio),
            4 => Some(AssetCategory::Unknown4),
            5 => Some(AssetCategory::AnimationStream),
            6 => Some(AssetCategory::Unknown6),
            7 => Some(AssetCategory::ZoneGrid),
            _ => None,
        }
    }

    /// Filename extension for assets in this category.
    pub fn stream_extension(self) -> &'static str {
        match self {
            AssetCategory::Built => "",
            AssetCategory::TextureStream => ".stream",
            AssetCategory::Unknown2 => ".unk2strm",
            AssetCategory::Audio => ".wem",
            AssetCategory::Unknown4 => ".unk4strm",
            AssetCategory::AnimationStream => ".animstrm",
            AssetCategory::Unknown6 => ".unk6strm",
            AssetCategory::ZoneGrid => ".lgstream",
        }
    }
}

/// The full stream-extension table, indexable by `AssetCategory as usize`.
pub const STREAM_EXTENSIONS: [&str; 8] = [
    "",            // 0 Built (DAT1)
    ".stream",     // 1 Texture stream
    ".unk2strm",   // 2 Unknown 2
    ".wem",        // 3 Audio
    ".unk4strm",   // 4 Unknown 4
    ".animstrm",   // 5 Animation stream
    ".unk6strm",   // 6 Unknown 6
    ".lgstream",   // 7 ZoneGrid
];

/// Append the per-category stream extension to `name` if it is not already
/// present. Idempotent — calling twice yields the same result.
pub fn apply_stream_extension(name: &str, category: AssetCategory) -> String {
    let ext = category.stream_extension();
    if ext.is_empty() || name.ends_with(ext) {
        name.to_owned()
    } else {
        format!("{name}{ext}")
    }
}

// ---------------------------------------------------------------------------
// Locale
// ---------------------------------------------------------------------------

/// Sentinel locale byte meaning "all locales".
pub const LOCALE_ALL: u8 = 0xFF;

/// Locale tag stored as a single byte in TOC File Metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Locale {
    Unlocalized = 0,
    English = 1,
    BritishEnglish = 2,
    Danish = 3,
    Dutch = 4,
    Finnish = 5,
    French = 6,
    German = 7,
    Italian = 8,
    Japanese = 9,
    Korean = 10,
    Norwegian = 11,
    Polish = 12,
    Portuguese = 13,
    Russian = 14,
    Spanish = 15,
    Swedish = 16,
    BrazilianPortuguese = 17,
    Arabic = 18,
    Turkish = 19,
    LatinAmericanSpanish = 20,
    SimplifiedChinese = 21,
    TraditionalChinese = 22,
    CanadianFrench = 23,
    Czech = 24,
    Hungarian = 25,
    Greek = 26,
    Romanian = 27,
    Thai = 28,
    Vietnamese = 29,
    Indonesian = 30,
    Croatian = 31,
}

impl Locale {
    /// Decode the locale byte stored in TOC File Metadata. Returns `None`
    /// for unknown values; the `LOCALE_ALL` sentinel (`0xFF`) also returns
    /// `None` since it is not a real per-asset locale.
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Locale::Unlocalized),
            1 => Some(Locale::English),
            2 => Some(Locale::BritishEnglish),
            3 => Some(Locale::Danish),
            4 => Some(Locale::Dutch),
            5 => Some(Locale::Finnish),
            6 => Some(Locale::French),
            7 => Some(Locale::German),
            8 => Some(Locale::Italian),
            9 => Some(Locale::Japanese),
            10 => Some(Locale::Korean),
            11 => Some(Locale::Norwegian),
            12 => Some(Locale::Polish),
            13 => Some(Locale::Portuguese),
            14 => Some(Locale::Russian),
            15 => Some(Locale::Spanish),
            16 => Some(Locale::Swedish),
            17 => Some(Locale::BrazilianPortuguese),
            18 => Some(Locale::Arabic),
            19 => Some(Locale::Turkish),
            20 => Some(Locale::LatinAmericanSpanish),
            21 => Some(Locale::SimplifiedChinese),
            22 => Some(Locale::TraditionalChinese),
            23 => Some(Locale::CanadianFrench),
            24 => Some(Locale::Czech),
            25 => Some(Locale::Hungarian),
            26 => Some(Locale::Greek),
            27 => Some(Locale::Romanian),
            28 => Some(Locale::Thai),
            29 => Some(Locale::Vietnamese),
            30 => Some(Locale::Indonesian),
            31 => Some(Locale::Croatian),
            _ => None,
        }
    }

    /// The 2-letter TOC code for this locale (`"us"`, `"fr"`, …).
    /// `Unlocalized` is `"none"`.
    pub fn toc_code(self) -> &'static str {
        match self {
            Locale::Unlocalized => "none",
            Locale::English => "us",
            Locale::BritishEnglish => "gb",
            Locale::Danish => "dk",
            Locale::Dutch => "nl",
            Locale::Finnish => "fi",
            Locale::French => "fr",
            Locale::German => "de",
            Locale::Italian => "it",
            Locale::Japanese => "jp",
            Locale::Korean => "kr",
            Locale::Norwegian => "no",
            Locale::Polish => "pl",
            Locale::Portuguese => "pt",
            Locale::Russian => "ru",
            Locale::Spanish => "es",
            Locale::Swedish => "se",
            Locale::BrazilianPortuguese => "br",
            Locale::Arabic => "ar",
            Locale::Turkish => "tr",
            Locale::LatinAmericanSpanish => "la",
            Locale::SimplifiedChinese => "cs",
            Locale::TraditionalChinese => "ct",
            Locale::CanadianFrench => "fc",
            Locale::Czech => "cz",
            Locale::Hungarian => "hu",
            Locale::Greek => "el",
            Locale::Romanian => "ro",
            Locale::Thai => "th",
            Locale::Vietnamese => "vi",
            Locale::Indonesian => "id",
            Locale::Croatian => "hr",
        }
    }

    /// Locale fallback chain:
    ///
    /// 1. requested locale
    /// 2. English (if requested isn't already English/Unlocalized)
    /// 3. Unlocalized (if requested isn't already Unlocalized)
    ///
    /// Returns the locales to try in order. The first entry is always
    /// `self`.
    pub fn fallback_chain(self) -> Vec<Locale> {
        let mut chain = Vec::with_capacity(3);
        chain.push(self);
        if self != Locale::English && self != Locale::Unlocalized {
            chain.push(Locale::English);
        }
        if self != Locale::Unlocalized {
            chain.push(Locale::Unlocalized);
        }
        chain
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_round_trip_shipped() {
        let raw_crc: u64 = 0xDEAD_BEEF_CAFE_BABE;
        let id = pack_shipped(raw_crc);
        assert_eq!(AssetIdFlags::from_id(id), AssetIdFlags::Shipped);
        assert_eq!(payload(id), (raw_crc >> 2) & ASSET_ID_PAYLOAD_MASK);
    }

    #[test]
    fn flags_round_trip_ext() {
        let id = pack_ext(0x1234_5678);
        assert_eq!(AssetIdFlags::from_id(id), AssetIdFlags::Ext);
        assert_eq!(id & 0xFFFF_FFFF, 0x1234_5678);
    }

    #[test]
    fn flags_round_trip_wwise() {
        let id = pack_wwise(0xAABB_CCDD);
        assert_eq!(AssetIdFlags::from_id(id), AssetIdFlags::Wwise);
        assert_eq!(id & 0xFFFF_FFFF, 0xAABB_CCDD);
        assert_ne!(id & ASSET_ID_WWISE_BIT, 0);
    }

    #[test]
    fn stream_extensions_table_matches_enum() {
        for i in 0..STREAM_EXTENSIONS.len() {
            let cat = AssetCategory::from_index(i as u8).unwrap();
            assert_eq!(cat.stream_extension(), STREAM_EXTENSIONS[i]);
        }
    }

    #[test]
    fn apply_stream_extension_is_idempotent() {
        let once = apply_stream_extension("foo/bar", AssetCategory::Audio);
        assert_eq!(once, "foo/bar.wem");
        let twice = apply_stream_extension(&once, AssetCategory::Audio);
        assert_eq!(twice, "foo/bar.wem");
    }

    #[test]
    fn apply_stream_extension_built_is_noop() {
        assert_eq!(
            apply_stream_extension("foo/bar", AssetCategory::Built),
            "foo/bar"
        );
    }

    #[test]
    fn locale_codes_match_table() {
        assert_eq!(Locale::Unlocalized.toc_code(), "none");
        assert_eq!(Locale::English.toc_code(), "us");
        assert_eq!(Locale::SimplifiedChinese.toc_code(), "cs");
        assert_eq!(Locale::Croatian.toc_code(), "hr");
    }

    #[test]
    fn fallback_chain_french() {
        assert_eq!(
            Locale::French.fallback_chain(),
            vec![Locale::French, Locale::English, Locale::Unlocalized]
        );
    }

    #[test]
    fn fallback_chain_english() {
        assert_eq!(
            Locale::English.fallback_chain(),
            vec![Locale::English, Locale::Unlocalized]
        );
    }

    #[test]
    fn fallback_chain_unlocalized() {
        assert_eq!(
            Locale::Unlocalized.fallback_chain(),
            vec![Locale::Unlocalized]
        );
    }

    #[test]
    fn locale_from_byte_handles_known_and_unknown() {
        assert_eq!(Locale::from_byte(0), Some(Locale::Unlocalized));
        assert_eq!(Locale::from_byte(1), Some(Locale::English));
        assert_eq!(Locale::from_byte(31), Some(Locale::Croatian));
        assert_eq!(Locale::from_byte(32), None);
        assert_eq!(Locale::from_byte(LOCALE_ALL), None);
    }
}
