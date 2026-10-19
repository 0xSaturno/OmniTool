//! Zone Asset References (`0x30DADA09`) — read, repoint, and grow.
//!
//! This is the table a zone uses to name assets that nothing else in the zone
//! names: cinematics it plays, conduits it wires up, atmospheres it swaps to,
//! configs it reads, other zones it hands to the streamer, and Wwise events it
//! posts. It is the only place in a `.zone` where a *new* outbound asset
//! dependency can be declared, which makes it the one table a mod has to grow
//! when a replaced asset starts needing something the zone never shipped with.
//!
//! # Row layout
//!
//! ```text
//!   +0x00  u64 asset_id       packed DAT1 asset id, or a Wwise event id
//!   +0x08  u32 name_offset    offset from the start of the DAT1 container
//!   +0x0C  u32 type_tag       CRC-32 of the target's `.extension`
//! ```
//!
//! Measured on the shipped PC build (`bin/refs_probe census`): 832 of 9,046
//! zones carry the section, 11,515 rows in total, every section size an exact
//! multiple of 16, and every one of the 11,515 `name_offset` values resolves to
//! a non-empty string in the zone's own pool.
//!
//! # Where the id comes from
//!
//! Two families, and the writer has to pick the right one:
//!
//! * **Ordinary assets** — `asset_id == crc64::hash(name)`, on 3,429 of 3,429
//!   non-Wwise rows. The hash normalises the path first (lowercase, forward
//!   slashes), so the spelling stored in the pool does not affect resolution.
//! * **Wwise events** — the high dword is exactly `0xE0000000` and the low
//!   dword is FNV-**1** (multiply, then xor) of the lowercased name, on 8,086
//!   of 8,086 rows. FNV-1**a**, which `core::fnv` implements for the sound
//!   tools, matches 0 of them. These rows always carry type tag
//!   [`TAG_WWISE_EVENT`] and their names carry no extension.
//!
//! # Type tags
//!
//! Eighteen distinct values ship. Seventeen are `crc32(".extension")` of the
//! target's asset class and every row under them has a name with exactly that
//! extension (see [`TYPE_TAGS`]). The eighteenth, [`TAG_WWISE_EVENT`], is
//! carried by all 8,086 Wwise rows and by nothing else; no CRC-32 preimage has
//! been found for it, so it is named here by what it does, not by a recovered
//! string.
//!
//! # What growing this table costs
//!
//! The zone editor's save path is otherwise built on equal-length in-place
//! patches, precisely so that no offset anywhere can be invalidated. Adding a
//! row breaks that rule twice: the section gets 16 bytes longer, and unless the
//! name is already in the pool the pool gets longer too. [`add_reference`]
//! keeps the damage to exactly the two things that must change:
//!
//! * **String offsets do not move.** A DAT1 string offset is
//!   `header_end + position_in_pool`, and `header_end` is
//!   `16 + 12 * section_count + 8 * unknown_count`. No section is added and no
//!   unknown is added, so `header_end` is unchanged; the new name is *appended*
//!   to the pool, so every byte before it keeps its position. Every offset that
//!   resolved before still resolves to the same bytes — including offsets held
//!   by decoders this module has never heard of.
//! * **Section offsets move, and that is fine.** Every section moves later by
//!   the pool growth (rounded to the container's 16-byte alignment) plus, for
//!   sections after the reference table, the 16 new row bytes. Nothing in a
//!   zone stores an absolute section offset except the DAT1 section headers
//!   themselves, which [`crate::core::dat1::Dat1::save`] rewrites. Offsets
//!   *within* a section — the scene-object heap offsets, the prius blob spans,
//!   the script plug ranges — are all section-relative and untouched.
//! * **The 36-byte TOC asset header must be told.** Word 0 of `sizes[]` is the
//!   length of block 0, and [`crate::core::zone::Zone::to_bytes`] rewrites it.
//!   The later words are the sizes of blocks 1.., which do not change. The
//!   block count is recovered as the smallest `k` with `sum(sizes[0..k]) ==
//!   asset size`, so the header stays self-consistent only if word 0 is fixed
//!   up — which is exactly what [`verify_grown`] re-checks.
//!
//! Nothing outside the zone has to change for the *reference list itself* to be
//! correct: the id the loader looks up is in the row, and the TOC already knows
//! where that asset lives. The `dag` is a build-time artefact — see
//! [`crate::core::dag`] — and is not rewritten by this module; see
//! `bin/refs_probe dag` for the measurement of what it does and does not hold.
//!
//! # The cheap route
//!
//! Growing is the expensive answer. [`repoint_reference`] changes an existing
//! row's id, type tag and name in place — 16 bytes for 16 bytes — whenever the
//! replacement name is already somewhere in the pool (including as the tail of
//! a longer string, since pool strings are NUL-terminated). Nothing moves, the
//! asset header is untouched, and the existing equal-length save path carries
//! it unchanged.

use crate::core::crc64;
use crate::core::dat1::Dat1;
use crate::core::error::{Result, ToolkitError};
use crate::core::zone::{Zone, ZoneEdit};
use byteorder::{ByteOrder, LE};
use serde::Serialize;

/// `Zone Asset References` — the tag's CRC-32 preimage, from `section_names`.
pub const T_ZONE_ASSET_REFS: u32 = 0x30DA_DA09;
/// Bytes per row.
pub const ROW: usize = 16;

/// The type tag every Wwise-event row carries. Not a `crc32(".ext")` of any
/// extension tried; named for its measured role.
pub const TAG_WWISE_EVENT: u32 = 0x3F08_A054;
/// High dword of every Wwise-event id in this table.
pub const WWISE_ID_HI: u32 = 0xE000_0000;

pub const TAG_ACTOR: u32 = 0x37E7_2F50;
pub const TAG_ANIMSET: u32 = 0xD1AD_9F7C;
pub const TAG_ATMOSPHERE: u32 = 0xF56C_78E4;
pub const TAG_CINEMATIC2: u32 = 0x57B6_7E8F;
pub const TAG_CONDUIT: u32 = 0xEA7E_FDD4;
pub const TAG_CONFIG: u32 = 0xA9F1_49C4;
pub const TAG_MATERIAL: u32 = 0xB5AA_FACC;
pub const TAG_MATERIALGRAPH: u32 = 0x4704_8393;
pub const TAG_MODEL: u32 = 0xA407_0B70;
pub const TAG_MOVIE: u32 = 0x6ECC_8BC6;
pub const TAG_PERFORMANCESET: u32 = 0xD1BF_8CDA;
pub const TAG_PREFAB: u32 = 0xDB71_9344;
pub const TAG_REGION: u32 = 0x7219_97CE;
pub const TAG_SCRIPTGRAPH: u32 = 0xAE37_9B8E;
pub const TAG_SOUNDBANK: u32 = 0xFFA8_6BB6;
pub const TAG_TEXTURE: u32 = 0x95A3_A227;
pub const TAG_ZONE: u32 = 0xE1EE_9AA6;

/// Every type tag that ships in this table, with the extension its rows carry.
///
/// All seventeen file-extension entries reproduce as
/// `crc32::hash(extension)`; the unit test below re-derives them so a typo
/// cannot survive. [`TAG_WWISE_EVENT`] is listed with an empty extension
/// because its rows have no extension and its preimage is unknown.
pub const TYPE_TAGS: &[(u32, &str)] = &[
    (TAG_WWISE_EVENT, ""),
    (TAG_ACTOR, ".actor"),
    (TAG_ANIMSET, ".animset"),
    (TAG_ATMOSPHERE, ".atmosphere"),
    (TAG_CINEMATIC2, ".cinematic2"),
    (TAG_CONDUIT, ".conduit"),
    (TAG_CONFIG, ".config"),
    (TAG_MATERIAL, ".material"),
    (TAG_MATERIALGRAPH, ".materialgraph"),
    (TAG_MODEL, ".model"),
    (TAG_MOVIE, ".movie"),
    (TAG_PERFORMANCESET, ".performanceset"),
    (TAG_PREFAB, ".prefab"),
    (TAG_REGION, ".region"),
    (TAG_SCRIPTGRAPH, ".scriptgraph"),
    (TAG_SOUNDBANK, ".soundbank"),
    (TAG_TEXTURE, ".texture"),
    (TAG_ZONE, ".zone"),
];

/// Human label for a type tag: the extension it stands for, or `"wwise event"`.
pub fn type_tag_label(tag: u32) -> Option<&'static str> {
    TYPE_TAGS.iter().find(|(t, _)| *t == tag).map(|(_, e)| {
        if e.is_empty() {
            "wwise event"
        } else {
            *e
        }
    })
}

/// The type tag an asset path implies, from its extension.
pub fn type_tag_for_name(name: &str) -> Option<u32> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    let dotted = format!(".{ext}");
    TYPE_TAGS
        .iter()
        .find(|(_, e)| *e == dotted)
        .map(|(t, _)| *t)
}

/// FNV-1 32 (multiply, then xor) over the lowercased name.
///
/// Distinct from [`crate::core::fnv::fnv1a_32`], which xors first. The Wwise
/// ids in this table are FNV-1, on 8,086 of 8,086 rows; FNV-1a matches none.
pub fn fnv1_32_lower(s: &str) -> u32 {
    let mut h: u32 = 2_166_136_261;
    for b in s.to_ascii_lowercase().bytes() {
        h = h.wrapping_mul(16_777_619);
        h ^= b as u32;
    }
    h
}

/// The id this table would store for `name` under `type_tag`.
///
/// Wwise rows get `0xE0000000_<FNV-1 of the lowercased name>`; everything else
/// gets the packed DAT1 CRC-64 of the normalised path.
pub fn asset_id_for(name: &str, type_tag: u32) -> u64 {
    if type_tag == TAG_WWISE_EVENT {
        ((WWISE_ID_HI as u64) << 32) | fnv1_32_lower(name) as u64
    } else {
        crc64::hash(name)
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ZoneRefRow {
    pub index: usize,
    /// Packed asset id as stored, rendered as hex for the front end.
    #[serde(serialize_with = "ser_u64_hex")]
    pub asset_id: u64,
    /// Offset from the start of the DAT1 container, as stored.
    pub name_offset: u32,
    pub type_tag: u32,
    /// The type tag's extension, or `"wwise event"`, or `"?"`.
    pub type_label: &'static str,
    /// The pool string `name_offset` resolves to, exactly as stored.
    pub name: String,
    /// True when the id is the Wwise-event family rather than a DAT1 asset id.
    pub is_wwise: bool,
    /// True when the stored id is reproduced by hashing the stored name. False
    /// here means the row's name is a label only and the id is authoritative.
    pub id_matches_name: bool,
}

fn ser_u64_hex<S: serde::Serializer>(v: &u64, s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_str(&format!("{v:#018X}"))
}

#[derive(Debug, Clone, Serialize)]
pub struct ZoneRefs {
    pub rows: Vec<ZoneRefRow>,
    /// Size of the section as it shipped, for the caller's arithmetic.
    pub section_bytes: usize,
}

impl ZoneRefs {
    /// Decode the section, or `None` when the zone does not carry one.
    pub fn read(dat1: &Dat1) -> Option<Self> {
        let sec = dat1.get_section_data(T_ZONE_ASSET_REFS)?;
        let n = sec.len() / ROW;
        let mut rows = Vec::with_capacity(n);
        for i in 0..n {
            let r = &sec[i * ROW..i * ROW + ROW];
            let asset_id = LE::read_u64(&r[0..8]);
            let name_offset = LE::read_u32(&r[8..12]);
            let type_tag = LE::read_u32(&r[12..16]);
            let name = dat1.get_string(name_offset).unwrap_or_default();
            let is_wwise = (asset_id >> 32) as u32 == WWISE_ID_HI;
            let id_matches_name = !name.is_empty() && asset_id_for(&name, type_tag) == asset_id;
            rows.push(ZoneRefRow {
                index: i,
                asset_id,
                name_offset,
                type_tag,
                type_label: type_tag_label(type_tag).unwrap_or("?"),
                name,
                is_wwise,
                id_matches_name,
            });
        }
        Some(ZoneRefs {
            rows,
            section_bytes: sec.len(),
        })
    }

    pub fn contains(&self, asset_id: u64) -> bool {
        self.rows.iter().any(|r| r.asset_id == asset_id)
    }
}

// ---------------------------------------------------------------------------
// String pool
// ---------------------------------------------------------------------------

/// Find a container-relative offset whose NUL-terminated string equals `want`.
///
/// Matches a whole pool string, and also the *tail* of a longer one: pool
/// strings are NUL-terminated, so any position inside one is itself a valid
/// C string ending at the same NUL. That is what makes repointing possible
/// without growing anything.
pub fn find_string(dat1: &Dat1, want: &str) -> Option<u32> {
    if want.is_empty() {
        return None;
    }
    let pool = &dat1.strings_pool;
    let w = want.as_bytes();
    let mut at = 0usize;
    while at + w.len() < pool.len() + 1 {
        let Some(p) = pool[at..]
            .windows(w.len())
            .position(|s| s == w)
            .map(|p| at + p)
        else {
            return None;
        };
        // The match must be terminated right after, or it is a prefix of a
        // longer string and is not the same C string.
        if pool.get(p + w.len()) == Some(&0) {
            return Some((dat1.header_end() + p) as u32);
        }
        at = p + 1;
    }
    None
}

/// Append `s` (NUL-terminated) to the string pool and return its new
/// container-relative offset.
///
/// This is the only pool mutation in this module, and it is append-only on
/// purpose: nothing before the append can move, so every offset that already
/// resolved keeps resolving to the same bytes.
fn append_string(dat1: &mut Dat1, s: &str) -> u32 {
    let off = (dat1.header_end() + dat1.strings_pool.len()) as u32;
    dat1.strings_pool.extend_from_slice(s.as_bytes());
    dat1.strings_pool.push(0);
    off
}

/// The spelling this table uses in the pool: backslash separators, case left
/// alone. The id is hashed from the normalised path, so this only affects what
/// a human (or a tool) reads back.
pub fn pool_spelling(name: &str) -> String {
    name.replace('/', "\\")
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// What a reference edit did, and what it cost.
#[derive(Debug, Clone, Serialize)]
pub struct RefPlan {
    /// `"repoint"` (equal length, nothing moves) or `"grow"`.
    pub kind: &'static str,
    pub row_index: usize,
    pub name: String,
    #[serde(serialize_with = "ser_u64_hex")]
    pub asset_id: u64,
    pub type_tag: u32,
    pub type_label: &'static str,
    /// True when the name was already in the pool and no pool bytes were added.
    pub string_reused: bool,
    pub name_offset: u32,
    /// Bytes appended to the string pool (0 when the string was reused).
    pub pool_bytes_added: usize,
    /// Bytes appended to the reference section (0 for a repoint, 16 for a grow).
    pub section_bytes_added: usize,
    /// `header_end` before and after. These must be equal or every string
    /// offset in the zone has silently moved.
    pub header_end_before: usize,
    pub header_end_after: usize,
    /// Row count before and after.
    pub rows_before: usize,
    pub rows_after: usize,
}

impl std::fmt::Display for RefPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} row {} -> {} [{}] id {:#018X}; string {} at {:#08X}; pool +{}B, section +{}B; rows {} -> {}; header_end {} -> {}",
            self.kind,
            self.row_index,
            self.name,
            self.type_label,
            self.asset_id,
            if self.string_reused { "reused" } else { "appended" },
            self.name_offset,
            self.pool_bytes_added,
            self.section_bytes_added,
            self.rows_before,
            self.rows_after,
            self.header_end_before,
            self.header_end_after,
        )
    }
}

fn row_bytes(asset_id: u64, name_offset: u32, type_tag: u32) -> [u8; ROW] {
    let mut r = [0u8; ROW];
    LE::write_u64(&mut r[0..8], asset_id);
    LE::write_u32(&mut r[8..12], name_offset);
    LE::write_u32(&mut r[12..16], type_tag);
    r
}

/// Where a new row of `type_tag` goes.
///
/// Shipped zones keep equal type tags contiguous (see `refs_probe census`), so
/// a new row joins the run of its own tag when one exists and is appended
/// otherwise. This is cosmetic as far as any measurement here can tell — the
/// loader is given a count and a base pointer — but it costs nothing to keep an
/// invariant the build tool maintains.
fn insert_index(rows: &[ZoneRefRow], type_tag: u32) -> usize {
    match rows.iter().rposition(|r| r.type_tag == type_tag) {
        Some(i) => i + 1,
        None => rows.len(),
    }
}

/// Add a reference the zone does not currently list.
///
/// Grows the reference section by one 16-byte row and, unless the name is
/// already in the pool, the pool by `name.len() + 1`. Fails if the zone has no
/// reference section at all — creating one would add a section header, which
/// moves `header_end` and therefore every string offset in the file; see
/// [`can_add_section`].
pub fn add_reference(zone: &mut Zone, name: &str, type_tag: u32) -> Result<RefPlan> {
    let spelling = pool_spelling(name);
    let asset_id = asset_id_for(&spelling, type_tag);

    let existing = ZoneRefs::read(&zone.dat1)
        .ok_or_else(|| ToolkitError::Parse(format!(
            "this zone has no Zone Asset References section ({T_ZONE_ASSET_REFS:#010X}); \
             adding one would add a DAT1 section header and move every string offset in the file"
        )))?;
    if let Some(r) = existing.rows.iter().find(|r| r.asset_id == asset_id) {
        return Err(ToolkitError::Parse(format!(
            "zone already references {asset_id:#018X} at row {} ({})",
            r.index, r.name
        )));
    }

    let header_end_before = zone.dat1.header_end();
    let old_section = zone
        .dat1
        .get_section_data(T_ZONE_ASSET_REFS)
        .map(<[u8]>::to_vec)
        .unwrap_or_default();

    let (name_offset, reused, pool_added) = match find_string(&zone.dat1, &spelling) {
        Some(off) => (off, true, 0usize),
        None => {
            let off = append_string(&mut zone.dat1, &spelling);
            (off, false, spelling.len() + 1)
        }
    };

    let at = insert_index(&existing.rows, type_tag);
    let mut section = old_section.clone();
    section.splice(
        at * ROW..at * ROW,
        row_bytes(asset_id, name_offset, type_tag),
    );
    let new_section = section.clone();
    zone.dat1.set_section_data(T_ZONE_ASSET_REFS, section)?;

    zone.edits.push(ZoneEdit {
        section: T_ZONE_ASSET_REFS,
        offset: at * ROW,
        old: old_section,
        new: new_section,
        what: format!("add reference {spelling}"),
    });

    Ok(RefPlan {
        kind: "grow",
        row_index: at,
        name: spelling,
        asset_id,
        type_tag,
        type_label: type_tag_label(type_tag).unwrap_or("?"),
        string_reused: reused,
        name_offset,
        pool_bytes_added: pool_added,
        section_bytes_added: ROW,
        header_end_before,
        header_end_after: zone.dat1.header_end(),
        rows_before: existing.rows.len(),
        rows_after: existing.rows.len() + 1,
    })
}

/// Point an existing row at a different asset, in place.
///
/// Equal length in every case: 16 bytes are overwritten and nothing else in the
/// file changes, so the zone editor's existing save path carries this unchanged
/// and no offset anywhere can be invalidated. The catch is the name: the new
/// asset's path has to be reachable in the pool already (as a whole string or
/// as the tail of a longer one). When it is not, `allow_pool_growth` decides
/// whether to append it — appending is still far cheaper than a grow, because
/// the section keeps its size and only the pool and the section offsets move.
pub fn repoint_reference(
    zone: &mut Zone,
    row_index: usize,
    name: &str,
    type_tag: u32,
    allow_pool_growth: bool,
) -> Result<RefPlan> {
    let spelling = pool_spelling(name);
    let asset_id = asset_id_for(&spelling, type_tag);

    let existing = ZoneRefs::read(&zone.dat1).ok_or(ToolkitError::SectionNotFound(
        T_ZONE_ASSET_REFS,
    ))?;
    if row_index >= existing.rows.len() {
        return Err(ToolkitError::Parse(format!(
            "row {row_index} out of range (the zone has {} reference rows)",
            existing.rows.len()
        )));
    }

    let header_end_before = zone.dat1.header_end();
    let (name_offset, reused, pool_added) = match find_string(&zone.dat1, &spelling) {
        Some(off) => (off, true, 0usize),
        None if allow_pool_growth => {
            let off = append_string(&mut zone.dat1, &spelling);
            (off, false, spelling.len() + 1)
        }
        None => {
            return Err(ToolkitError::Unsupported(format!(
                "{spelling:?} is not in this zone's string pool; repointing to it would have to \
                 append to the pool, which moves every section offset in the file"
            )))
        }
    };

    let mut section = zone
        .dat1
        .get_section_data(T_ZONE_ASSET_REFS)
        .map(<[u8]>::to_vec)
        .ok_or(ToolkitError::SectionNotFound(T_ZONE_ASSET_REFS))?;
    let at = row_index * ROW;
    let old = section[at..at + ROW].to_vec();
    let new = row_bytes(asset_id, name_offset, type_tag).to_vec();
    section[at..at + ROW].copy_from_slice(&new);
    zone.dat1.set_section_data(T_ZONE_ASSET_REFS, section)?;

    zone.edits.push(ZoneEdit {
        section: T_ZONE_ASSET_REFS,
        offset: at,
        old,
        new,
        what: format!("repoint reference row {row_index} to {spelling}"),
    });

    Ok(RefPlan {
        kind: "repoint",
        row_index,
        name: spelling,
        asset_id,
        type_tag,
        type_label: type_tag_label(type_tag).unwrap_or("?"),
        string_reused: reused,
        name_offset,
        pool_bytes_added: pool_added,
        section_bytes_added: 0,
        header_end_before,
        header_end_after: zone.dat1.header_end(),
        rows_before: existing.rows.len(),
        rows_after: existing.rows.len(),
    })
}

/// Whether a zone could be given a reference section it does not have.
///
/// Always `false`, stated as a function so the caller does not have to know
/// why: a new section adds 12 bytes to the DAT1 header, which moves
/// `header_end`, which changes the meaning of **every** `u32` string offset
/// stored in every section of the file. Rewriting all of them is possible only
/// for the offsets we can enumerate, and the zone format has sections no
/// decoder in this project reads. 8,214 of 9,046 shipped zones have no
/// reference section, so this is the common case and it is a real limit.
pub fn can_add_section() -> bool {
    false
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

/// A single checked claim.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub pass: bool,
    pub detail: String,
}

impl Check {
    fn new(name: &str, pass: bool, detail: String) -> Self {
        Check {
            name: name.into(),
            pass,
            detail,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyReport {
    pub checks: Vec<Check>,
    pub passed: usize,
    pub failed: usize,
}

impl std::fmt::Display for VerifyReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "=== grown-zone verification ===")?;
        for c in &self.checks {
            writeln!(
                f,
                "  [{}] {:<42} {}",
                if c.pass { "PASS" } else { "FAIL" },
                c.name,
                c.detail
            )?;
        }
        write!(f, "  {} passed, {} failed", self.passed, self.failed)
    }
}

/// Re-check a grown zone against the original, as far as is possible without
/// running the game.
///
/// Every claim this module makes about what a grow does and does not disturb is
/// re-measured here on the actual bytes:
///
///   * the grown asset re-parses as a zone, and every decoder the project has
///     reports the same counts as before;
///   * `header_end` is unchanged, so every stored string offset still means the
///     same position;
///   * the string pool's original bytes are a byte-identical prefix of the new
///     one, which proves *every* pre-existing string offset — including ones no
///     decoder here reads — resolves to the same string;
///   * every section except the reference table is byte-identical;
///   * the reference table is the original with exactly one 16-byte row
///     inserted, and that row's name offset resolves to the expected name and
///     its id is the expected hash;
///   * section offsets are ascending, 16-aligned, non-overlapping and inside
///     the file, and `total_size` matches;
///   * the 36-byte asset header's word 0 equals the new block-0 length, the
///     later block sizes are unchanged, and the block split still resolves.
pub fn verify_grown(original: &[u8], grown: &[u8], expect_id: u64, expect_name: &str) -> VerifyReport {
    let mut checks: Vec<Check> = Vec::new();
    let spelling = pool_spelling(expect_name);

    let before = match Zone::parse(original) {
        Ok(z) => z,
        Err(e) => {
            checks.push(Check::new("original parses", false, e.to_string()));
            return finish(checks);
        }
    };
    let after = match Zone::parse(grown) {
        Ok(z) => z,
        Err(e) => {
            checks.push(Check::new("grown asset re-parses", false, e.to_string()));
            return finish(checks);
        }
    };
    checks.push(Check::new(
        "grown asset re-parses as a zone",
        true,
        format!("{} bytes -> {} bytes", original.len(), grown.len()),
    ));

    // --- header_end -------------------------------------------------------
    let he_b = before.dat1.header_end();
    let he_a = after.dat1.header_end();
    checks.push(Check::new(
        "header_end unchanged",
        he_b == he_a,
        format!(
            "{he_b} -> {he_a} ({} sections, {} unknown words)",
            after.dat1.sections.len(),
            after.dat1.fixups.len() / 8
        ),
    ));

    // --- string pool ------------------------------------------------------
    let pb = &before.dat1.strings_pool;
    let pa = &after.dat1.strings_pool;
    let prefix_ok = pa.len() >= pb.len() && &pa[..pb.len()] == pb.as_slice();
    checks.push(Check::new(
        "old string pool is a byte-identical prefix",
        prefix_ok,
        format!(
            "{} -> {} bytes; every offset < {:#X} unmoved",
            pb.len(),
            pa.len(),
            he_b + pb.len()
        ),
    ));

    // Independent, exhaustive: re-resolve every offset in the old pool.
    let mut resolved = 0usize;
    let mut mismatched = 0usize;
    for off in 0..pb.len() {
        let want = before.dat1.get_string((he_b + off) as u32);
        let got = after.dat1.get_string((he_b + off) as u32);
        if want == got {
            resolved += 1;
        } else {
            mismatched += 1;
        }
    }
    checks.push(Check::new(
        "every old string offset resolves the same",
        mismatched == 0,
        format!("{resolved} offsets re-read identically, {mismatched} changed"),
    ));

    // --- sections ---------------------------------------------------------
    let mut same = 0usize;
    let mut differing: Vec<String> = Vec::new();
    for sh in &before.dat1.sections {
        let b = before.dat1.get_section_data(sh.tag).unwrap_or(&[]);
        let a = match after.dat1.get_section_data(sh.tag) {
            Some(a) => a,
            None => {
                differing.push(format!("{:08X} vanished", sh.tag));
                continue;
            }
        };
        if b == a {
            same += 1;
        } else {
            differing.push(format!("{:08X} ({} -> {} bytes)", sh.tag, b.len(), a.len()));
        }
    }
    let only_refs = differing.len() == 1 && differing[0].starts_with(&format!("{T_ZONE_ASSET_REFS:08X}"));
    checks.push(Check::new(
        "only the reference section changed",
        only_refs,
        format!(
            "{same} of {} sections byte-identical; changed: [{}]",
            before.dat1.sections.len(),
            differing.join(", ")
        ),
    ));
    checks.push(Check::new(
        "section set unchanged",
        before.dat1.sections.len() == after.dat1.sections.len(),
        format!(
            "{} -> {} sections",
            before.dat1.sections.len(),
            after.dat1.sections.len()
        ),
    ));

    // --- the reference table ---------------------------------------------
    let rb = before.dat1.get_section_data(T_ZONE_ASSET_REFS).unwrap_or(&[]);
    let ra = after.dat1.get_section_data(T_ZONE_ASSET_REFS).unwrap_or(&[]);
    checks.push(Check::new(
        "reference section grew by exactly one row",
        ra.len() == rb.len() + ROW,
        format!(
            "{} -> {} bytes ({} -> {} rows)",
            rb.len(),
            ra.len(),
            rb.len() / ROW,
            ra.len() / ROW
        ),
    ));

    let refs_a = ZoneRefs::read(&after.dat1);
    let (added, untouched_rows) = match &refs_a {
        Some(r) => {
            let new_rows: Vec<&ZoneRefRow> = r.rows.iter().filter(|x| x.asset_id == expect_id).collect();
            // Every other row must be byte-identical to a row of the original,
            // in order, with exactly one insertion.
            let mut ok = true;
            if ra.len() == rb.len() + ROW {
                let mut found_gap = false;
                let mut j = 0usize;
                for i in 0..ra.len() / ROW {
                    let row = &ra[i * ROW..i * ROW + ROW];
                    if j * ROW < rb.len() && row == &rb[j * ROW..j * ROW + ROW] {
                        j += 1;
                    } else if !found_gap {
                        found_gap = true;
                    } else {
                        ok = false;
                    }
                }
                ok = ok && j * ROW == rb.len() && found_gap;
            } else {
                ok = false;
            }
            (new_rows.len(), ok)
        }
        None => (0, false),
    };
    checks.push(Check::new(
        "every pre-existing row survives unchanged",
        untouched_rows,
        "old rows reproduce in order with exactly one insertion".into(),
    ));
    checks.push(Check::new(
        "the new row is present exactly once",
        added == 1,
        format!("{added} row(s) carry id {expect_id:#018X}"),
    ));
    if let Some(r) = refs_a.as_ref().and_then(|r| r.rows.iter().find(|x| x.asset_id == expect_id)) {
        checks.push(Check::new(
            "new row's name offset resolves",
            r.name == spelling,
            format!("{:#08X} -> {:?} (wanted {:?})", r.name_offset, r.name, spelling),
        ));
        checks.push(Check::new(
            "new row's id is the hash of its name",
            r.id_matches_name,
            format!(
                "{:#018X} vs {:#018X}",
                r.asset_id,
                asset_id_for(&r.name, r.type_tag)
            ),
        ));
    }

    // --- container geometry ----------------------------------------------
    let mut geo_ok = true;
    let mut geo_detail = String::new();
    let mut offs: Vec<(u32, u32)> = after
        .dat1
        .sections
        .iter()
        .map(|s| (s.offset, s.size))
        .collect();
    offs.sort();
    let mut cursor = (he_a + pa.len()) as u32;
    for (o, s) in &offs {
        if o % 16 != 0 {
            geo_ok = false;
            geo_detail = format!("section at {o:#X} is not 16-aligned");
            break;
        }
        if *o < cursor {
            geo_ok = false;
            geo_detail = format!("section at {o:#X} overlaps the previous one (ends {cursor:#X})");
            break;
        }
        cursor = o + s;
    }
    let block0_len = after.dat1.total_size as usize;
    if geo_ok {
        geo_detail = format!(
            "{} sections, first at {:#X}, last ends {:#X}, total_size {:#X}",
            offs.len(),
            offs.first().map(|x| x.0).unwrap_or(0),
            cursor,
            block0_len
        );
    }
    checks.push(Check::new("section offsets well-formed", geo_ok, geo_detail));
    checks.push(Check::new(
        "total_size equals the end of the last section",
        block0_len == cursor as usize,
        format!("{block0_len} vs {cursor}"),
    ));

    // --- the 36-byte asset header ----------------------------------------
    match (&before.header, &after.header) {
        (Some(hb), Some(ha)) => {
            let type_ok = LE::read_u32(&hb[0..4]) == LE::read_u32(&ha[0..4]);
            checks.push(Check::new(
                "asset header type id unchanged",
                type_ok,
                format!("{:#010X}", LE::read_u32(&ha[0..4])),
            ));

            // Word 0 of sizes[] must be the new block-0 length.
            let body_len = grown.len() - crate::core::zone::ASSET_HEADER_LEN;
            let declared0 = LE::read_u32(&ha[4..8]) as usize;
            checks.push(Check::new(
                "asset header block-0 size updated",
                declared0 == block0_len,
                format!(
                    "header says {declared0}, container says {block0_len} (was {})",
                    LE::read_u32(&hb[4..8])
                ),
            ));

            let later_same = hb[8..] == ha[8..];
            checks.push(Check::new(
                "asset header words 1..7 unchanged",
                later_same,
                format!(
                    "{:?}",
                    (1..8)
                        .map(|i| LE::read_u32(&ha[4 + i * 4..8 + i * 4]))
                        .collect::<Vec<_>>()
                ),
            ));

            // The block split must still resolve: the smallest k with
            // sum(sizes[0..k]) == body length.
            let mut acc = 0usize;
            let mut k = 0usize;
            for i in 0..8 {
                let s = LE::read_u32(&ha[4 + i * 4..8 + i * 4]) as usize;
                acc += s;
                if acc == body_len {
                    k = i + 1;
                    break;
                }
                if acc > body_len || s == 0 {
                    break;
                }
            }
            checks.push(Check::new(
                "block split still resolves",
                k > 0,
                format!(
                    "body {body_len} bytes = sum of the first {k} size words; {} trailing block(s) kept, {} tail bytes",
                    after.block_spans.len(),
                    after.tail.len()
                ),
            ));
            checks.push(Check::new(
                "trailing blocks byte-identical",
                before.tail == after.tail,
                format!("{} tail bytes", after.tail.len()),
            ));
        }
        (None, None) => checks.push(Check::new(
            "asset header",
            true,
            "asset carried no 36-byte header".into(),
        )),
        _ => checks.push(Check::new(
            "asset header",
            false,
            "header presence changed across the edit".into(),
        )),
    }

    // --- decoders still agree --------------------------------------------
    let counts = |z: &Zone| {
        (
            z.scene_objects.len(),
            z.actors.len(),
            z.actor_groups.len(),
            z.actions.len(),
            z.model_ids.len(),
            z.vertex_paint.len(),
            z.decal_geometry.len(),
        )
    };
    let cb = counts(&before);
    let ca = counts(&after);
    checks.push(Check::new(
        "every other decoder reports the same counts",
        cb == ca,
        format!(
            "scene {} actors {} groups {} script {} models {} vpaint {} decalgeo {}",
            ca.0, ca.1, ca.2, ca.3, ca.4, ca.5, ca.6
        ),
    ));
    let paths_same = before.model_names == after.model_names;
    checks.push(Check::new(
        "model name table resolves identically",
        paths_same,
        format!("{} model paths", after.model_names.len()),
    ));

    finish(checks)
}

fn finish(checks: Vec<Check>) -> VerifyReport {
    let passed = checks.iter().filter(|c| c.pass).count();
    let failed = checks.len() - passed;
    VerifyReport {
        checks,
        passed,
        failed,
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::crc32;

    /// Every non-Wwise type tag must be the CRC-32 of the extension it claims.
    #[test]
    fn type_tags_are_extension_hashes() {
        for &(tag, ext) in TYPE_TAGS {
            if ext.is_empty() {
                assert_eq!(tag, TAG_WWISE_EVENT);
                continue;
            }
            assert_eq!(
                crc32::hash(ext),
                tag,
                "type tag {tag:#010X} does not hash from {ext:?}"
            );
        }
    }

    #[test]
    fn type_tag_for_name_round_trips() {
        assert_eq!(type_tag_for_name("a\\b\\c.model"), Some(TAG_MODEL));
        assert_eq!(type_tag_for_name("A/B/C.MATERIAL"), Some(TAG_MATERIAL));
        assert_eq!(type_tag_for_name("play_something"), None);
    }

    /// FNV-1, not FNV-1a. The canonical FNV-1 vector for "a" is 0x050C5D7E;
    /// FNV-1a gives 0xE40C292C, which `core::fnv` is tested against.
    #[test]
    fn fnv1_is_not_fnv1a() {
        assert_eq!(fnv1_32_lower("a"), 0x050C_5D7E);
        assert_ne!(fnv1_32_lower("a"), crate::core::fnv::fnv1a_32(b"a"));
        assert_eq!(fnv1_32_lower("Play_X"), fnv1_32_lower("play_x"));
    }

    #[test]
    fn wwise_ids_carry_the_family_high_dword() {
        let id = asset_id_for("play_ambient_sargasso", TAG_WWISE_EVENT);
        assert_eq!((id >> 32) as u32, WWISE_ID_HI);
        assert_eq!(id as u32, fnv1_32_lower("play_ambient_sargasso"));
    }

    #[test]
    fn ordinary_ids_are_the_packed_crc64() {
        let name = "models\\foo\\bar.model";
        assert_eq!(asset_id_for(name, TAG_MODEL), crc64::hash(name));
        // Spelling does not change the id; the hash normalises the path.
        assert_eq!(
            asset_id_for("Models/Foo/Bar.model", TAG_MODEL),
            asset_id_for(name, TAG_MODEL)
        );
    }
}
