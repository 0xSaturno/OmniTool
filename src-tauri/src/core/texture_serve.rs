//! Serving `.texture` assets to the level viewport.
//!
//! Two jobs, both read-only against the shipped archives:
//!
//! * [`texture_info`] — width / height / DXGI format for a texture id, read out
//!   of the asset's own 44-byte `Texture Header` section. It is a **partial**
//!   archive read (the first 256 bytes of the asset body), so cataloguing a
//!   zone's textures costs microseconds each instead of decompressing megabytes
//!   of resident pixels.
//! * [`texture_png`] — the smallest resident mip at or above a size cap,
//!   decoded and re-encoded as PNG, memoised in a byte-budgeted LRU exactly the
//!   way [`crate::core::level`] memoises meshes.
//!
//! Layout facts this relies on, all re-measured on the shipped PC build by
//! `bin/texture_probe.rs`:
//!
//! * the asset body is a bare DAT1 holding exactly one section,
//!   `Texture Header` (`0x4EDE3593`, 44 bytes), immediately followed by the
//!   resident pixel bytes;
//! * `sd_len` resident bytes hold `slices` copies of the resident mip chain,
//!   **slice-major** — every mip of slice 0, then every mip of slice 1 …;
//! * the streamed (`hd`) mips are a *separate* TOC entry and are deliberately
//!   never touched here: a viewport filling a few hundred pixels has no use for
//!   a 4K albedo, and the resident chain already starts at `sd_w x sd_h`.
//!
//! # Array slices
//!
//! The slice-major claim above is not a convention this module chose, it is
//! arithmetic that the shipped bytes satisfy exactly. `texture_probe slices`
//! recomputes, for every `.texture` the TOC names, the length of **one**
//! resident mip chain (`sum over m of mip_level_size(sd_w >> m, sd_h >> m)`
//! for `sd_mips` levels) and compares `slices * that` against the header's own
//! `sd_len`: 12,502 of 12,502 parsed headers agree to the byte, 0 mismatches
//! and 0 formats with an unknown block size, including all 561 assets with
//! more than one slice (246 of them four-slice; the rest run 2, 3, 6, 12, 32,
//! 64 and 128).
//!
//! Equal totals alone would not tell slice-major from mip-major, so the
//! decisive evidence is **content**: `texture_probe blend` decodes each slice
//! of a real four-slice colour map separately and gets four distinct,
//! coherent, full-resolution images — an albedo, a red/green gloss-specular
//! map, a magenta misc map whose green channel is a concrete height field with
//! visible aggregate, and a textbook lavender tangent normal. Four fragments of
//! one interleaved mip chain could not each be a whole 512x512 picture.
//!
//! What a colour slot of the `gbl_rgb_blend` family puts in its four slices,
//! read off the shipped G-buffer pixel shader (`bin/shader_dump.rs`):
//!
//! | slice | content                                                        |
//! |-------|----------------------------------------------------------------|
//! | 0     | albedo RGB                                                     |
//! | 1     | gloss / specular (`r` gloss via `sqrt`, `a` alternate gloss)    |
//! | 2     | misc; **`g` is the height**, `sqrt`-decoded                     |
//! | 3     | per-layer tangent normal, stored sRGB-encoded                   |
//!
//! How often that is true of a *bound* layer: **74 of 77** distinct blend
//! colour layers across four art tiles on four different planets are four-slice
//! arrays — Nefarious City `tile_e18_art` 23/24, Sargasso `tile_n17_art` 15/17,
//! Savali `tile_c17_art` 18/18, Megalopolis `tile_m37_art` 18/18. The other
//! pieces of the same materials are flat, which is why the array read has to
//! stay conditional: of the 49 splat masks and 71 shared normal/misc maps those
//! four zones bind and whose headers parse, **every one** has exactly one
//! slice. It is not a blend-only trait either — 76 of the 653 textures their
//! *plain* material slots bind are four-slice too.
//!
//! Slice 2's green really is a height field and not, say, a second mip.
//! Measured at a 512 cap, the mean absolute difference between horizontally
//! adjacent texels, as a fraction of the channel's own standard deviation, is
//! 0.162 / 0.124 / 0.150 / 0.142 on those four zones. Shuffling the very same
//! texels raises it to 0.731 / 0.835 / 0.880 / 0.557, and slice **0**'s green —
//! an albedo, which is smooth but carries detail rather than relief — sits at
//! 0.474 / 0.407 / 0.478 / 0.347. Slice 2's green is thus about 2.8x smoother
//! than the albedo in the same asset on every planet sampled, which is what
//! relief looks like and what noise or a misaligned mip does not. Written out
//! as a greyscale image it is unmistakable: `concrete_001_c` slice 2 green is a
//! concrete height map with individual aggregate stones standing proud,
//! `sand_001_c`'s is a soft dune ripple.
//!
//! What it costs to serve. Over the four-slice colour layers of one busy art
//! tile at a 512 cap, on all cores: slice 0 alone is 6.34 MB of PNG in 10.9 ms
//! (23 layers, Nefarious City), and the `[0, 2, 3]` set a shading viewport
//! actually wants is 21.85 MB in 32.2 ms — call it 3x the bytes and 3x the
//! time, which is what three times the texels costs. The other three zones land
//! at 12.20-18.40 MB and 26.5-27.6 ms. Ask for the slices **together**
//! ([`texture_png_bytes_multi`]) rather than one at a time: see that function
//! for the 2.4x difference in archive bytes read.
//!
//! [`texture_png_bytes_slice`] serves any one of them. The bytes handed over
//! are always the texture's **stored** texels — no sRGB transfer function is
//! applied on this side, for any slice — so the consumer is free to undo the
//! sampler's decode the way the shipped shader does (`pow(|x|, 0.447761)`
//! before the `*2-1` remap on slice 3). See the frontend note in
//! `src/tools/level-editor/viewport/materials.ts`.
//!
//! BCn decoding is not reimplemented — [`crate::tools::texture_converter`]
//! already does BC1-BC7 (including BC6H) and the raw formats, and this module
//! calls straight into it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use dashmap::DashMap;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder};
use log::debug;

use crate::core::dat1::{Dat1, DAT1_MAGIC};
use crate::core::error::{Result, ToolkitError};
use crate::core::level::LevelIndex;
use crate::core::toc::ArchiveCache;
use crate::tools::texture_converter::{decode_to_rgba, mip_level_size};

/// `Texture Header` DAT1 section tag.
pub const TAG_TEXTURE_HEADER: u32 = 0x4EDE_3593;

/// How much of a texture asset the header probe reads. The DAT1 is 92 bytes on
/// every shipped texture; 256 leaves room for a layout that grows.
const HEADER_PROBE_BYTES: u32 = 256;

/// Default viewport cap. A 512-pixel mip is already more than a model covering
/// a few hundred screen pixels can show.
pub const DEFAULT_MAX_SIZE: u32 = 512;

/// Clamp range for the caller-supplied cap.
const MIN_CAP: u32 = 16;
const MAX_CAP: u32 = 2048;

/// Decoded-PNG budget. Deliberately smaller than the mesh budget: a zone's
/// textures at a 512 cap measure in the tens of megabytes (see
/// `bin/texture_probe.rs`), so this holds several zones at once.
const PNG_CACHE_BUDGET: usize = 192 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Texture Header
// ---------------------------------------------------------------------------

/// The 44-byte `Texture Header` section, plus what the rest of this module
/// derives from it.
#[derive(Debug, Clone, Copy)]
pub struct TexInfo {
    /// Resident pixel bytes — the span that follows the DAT1 in this asset.
    pub sd_len: u32,
    /// Streamed pixel bytes; they live in a *different* TOC entry.
    pub hd_len: u32,
    /// Size of the first streamed mip, i.e. the texture's native size. Zero
    /// when nothing is streamed, in which case `sd_w`/`sd_h` is native.
    pub hd_w: u32,
    pub hd_h: u32,
    /// Size of the first resident mip.
    pub sd_w: u32,
    pub sd_h: u32,
    /// Array slices: the depth-or-array-size the GPU resource is created
    /// with. A cube map stores 1 here
    /// and has six faces; see [`TexInfo::slices`].
    pub surfaces: u32,
    /// The raw format-flags word at `+0x12`, from which `dimension` and
    /// `content_type` below are sliced. Kept whole because the engine treats it
    /// as one bitfield (`kIs1D`/`kIs2D`/`kIs2DArray`/`kIs3D`/`kIsCube`,
    /// `kIsBaseMap`/`kIsNormalMap`/`kIsGlossMap`, `kIsIBL`, `kDisableStreaming`
    /// …) and this module only claims to have measured two of its fields.
    pub format_flags: u16,
    /// `1` 2D, `2` 3D/volume, `4` cube.
    pub dimension: u8,
    /// Content/usage tag bits; bit 1 marks a two-channel normal map, which
    /// [`decode_to_rgba`] reconstructs Z for.
    pub content_type: u8,
    /// `DXGI_FORMAT`.
    pub format: u16,
    pub sd_mips: u8,
    pub hd_mips: u8,
    /// Byte offset of the resident pixels inside the asset body.
    pub pixel_offset: usize,
}

impl TexInfo {
    /// Cube faces count as extra slices; the header does not say so itself.
    pub fn slices(&self) -> u32 {
        self.surfaces.max(1) * if self.dimension == 4 { 6 } else { 1 }
    }

    /// Bytes one resident mip chain occupies: `sd_mips` levels starting at
    /// `sd_w x sd_h`. `None` for a DXGI format whose block size is unknown.
    ///
    /// This is the **stride between array slices**, and the claim is checked
    /// rather than assumed: `slices() * resident_chain_bytes() == sd_len` on
    /// 12,266 of 12,266 shipped `.texture` headers (`texture_probe --slices`).
    pub fn resident_chain_bytes(&self) -> Option<usize> {
        let mut total = 0usize;
        for m in 0..self.sd_mips.max(1) as u32 {
            let w = (self.sd_w >> m).max(1);
            let h = (self.sd_h >> m).max(1);
            total += mip_level_size(w, h, self.format as u32)? as usize;
        }
        Some(total)
    }

    /// Where slice `slice` starts inside the resident blob.
    ///
    /// The computed chain length is preferred over `sd_len / slices` so a
    /// header whose `sd_len` was rounded up to a page still lands on the right
    /// texels; the division is the fallback for an unknown format.
    pub fn slice_stride(&self) -> usize {
        self.resident_chain_bytes()
            .unwrap_or_else(|| self.sd_len as usize / self.slices().max(1) as usize)
    }

    /// The texture's native size — the first streamed mip when one exists.
    pub fn native(&self) -> (u32, u32) {
        if self.hd_mips > 0 && self.hd_w > 0 {
            (self.hd_w, self.hd_h)
        } else {
            (self.sd_w, self.sd_h)
        }
    }

    fn parse(section: &[u8], pixel_offset: usize) -> Result<Self> {
        if section.len() < 44 {
            return Err(ToolkitError::Parse(format!(
                "texture header is {} bytes, expected 44",
                section.len()
            )));
        }
        let u32at = |o: usize| u32::from_le_bytes(section[o..o + 4].try_into().unwrap());
        let u16at = |o: usize| u16::from_le_bytes(section[o..o + 2].try_into().unwrap());
        let flags = u16at(0x12);
        Ok(Self {
            sd_len: u32at(0x00),
            hd_len: u32at(0x04),
            hd_w: u16at(0x08) as u32,
            hd_h: u16at(0x0A) as u32,
            sd_w: u16at(0x0C) as u32,
            sd_h: u16at(0x0E) as u32,
            surfaces: u16at(0x10) as u32,
            format_flags: flags,
            dimension: ((flags >> 4) & 0x7) as u8,
            content_type: ((flags >> 7) & 0x7F) as u8,
            format: u16at(0x14),
            sd_mips: section[0x1E],
            hd_mips: section[0x20],
            pixel_offset,
        })
    }
}

/// `content_type` values seen on the shipped build. The field is a clean
/// discriminator, measured over a busy art zone by `bin/texture_probe.rs`:
/// every `_n` map carries `0x02` and nothing else does, every `_c` carries
/// `0x01`, `_g` carries `0x64`, and the data maps (`_m`, `_id`, `_ao`,
/// grunge/detail) carry `0x00`.
pub const CONTENT_COLOR: u8 = 0x01;
pub const CONTENT_NORMAL: u8 = 0x02;

/// `DXGI_FORMAT` BC5_UNORM — [`decode_to_rgba`] already rebuilds Z for it.
const FORMAT_BC5_UNORM: u16 = 83;

/// `DXGI_FORMAT` R8G8_UNORM. [`decode_to_rgba`] has no branch for it, and the
/// 16 shipped assets that use it are all two-channel normal maps (character
/// eyes, one NPC body), so unpack it here rather than lose them.
const FORMAT_R8G8_UNORM: u16 = 49;

fn decode_r8g8(src: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; width * height * 4];
    for i in 0..width * height {
        let s = i * 2;
        let (r, g) = if s + 1 < src.len() {
            (src[s], src[s + 1])
        } else {
            (0, 0)
        };
        out[i * 4] = r;
        out[i * 4 + 1] = g;
        out[i * 4 + 3] = 255;
    }
    out
}

/// Two-channel normal maps store X in R and Y in G and leave B empty.
///
/// [`crate::tools::texture_converter::decode_to_rgba`] rebuilds Z inside its
/// BC5 branch only, but the shipped `_n` maps are overwhelmingly **BC7** (and
/// some BC1), which arrive here with a flat blue channel — they render as an
/// olive-green sheet and a viewport lighting with them gets the surface normal
/// wrong everywhere. Rebuilding it here rather than in `texture_converter`
/// keeps the shipped Texture Tool's output byte-identical to what it is today.
fn rebuild_normal_z(rgba: &mut [u8]) {
    for px in rgba.chunks_exact_mut(4) {
        let x = px[0] as f32 / 127.5 - 1.0;
        let y = px[1] as f32 / 127.5 - 1.0;
        let z = (1.0 - (x * x + y * y).min(1.0)).sqrt();
        px[2] = ((z * 0.5 + 0.5) * 255.0).round().clamp(0.0, 255.0) as u8;
        px[3] = 255;
    }
}

/// `DXGI_FORMAT` number to the name the UI shows. Only the formats the shipped
/// textures actually use are named; anything else comes back as `DXGI_<n>`.
pub fn format_name(format: u16) -> String {
    let s = match format {
        2 => "R32G32B32A32_FLOAT",
        10 => "R16G16B16A16_FLOAT",
        16 => "R32G32_FLOAT",
        24 => "R10G10B10A2_UNORM",
        28 => "R8G8B8A8_UNORM",
        29 => "R8G8B8A8_UNORM_SRGB",
        34 => "R16G16_UNORM",
        41 => "R32_FLOAT",
        49 => "R8G8_UNORM",
        56 => "R16_UNORM",
        61 => "R8_UNORM",
        71 => "BC1_UNORM",
        72 => "BC1_UNORM_SRGB",
        74 => "BC2_UNORM",
        75 => "BC2_UNORM_SRGB",
        77 => "BC3_UNORM",
        78 => "BC3_UNORM_SRGB",
        80 => "BC4_UNORM",
        81 => "BC4_SNORM",
        83 => "BC5_UNORM",
        84 => "BC5_SNORM",
        87 => "B8G8R8A8_UNORM",
        91 => "B8G8R8A8_UNORM_SRGB",
        95 => "BC6H_UF16",
        96 => "BC6H_SF16",
        98 => "BC7_UNORM",
        99 => "BC7_UNORM_SRGB",
        _ => return format!("DXGI_{format}"),
    };
    s.to_string()
}

/// True when the decoded texels of this format are sRGB-encoded, which the
/// viewport needs in order to choose the right GPU sampler/color space.
pub fn is_srgb(format: u16) -> bool {
    matches!(format, 29 | 72 | 75 | 78 | 91 | 99)
}

// ---------------------------------------------------------------------------
// Partial asset reads
// ---------------------------------------------------------------------------

/// One memory-mapped archive pool per game directory.
///
/// [`LevelIndex`] keeps its own pool for whole-asset reads, but it is private
/// and only reachable through [`LevelIndex::extract`], which always reads the
/// asset entire. Reading the first 256 bytes of a 4 MB texture instead of all
/// of it is the whole point of [`texture_info`], so this module keeps a pool of
/// its own. `mmap` is virtual address space, not resident memory, so mapping
/// the same archive twice costs nothing measurable.
fn archive_pool(index: &LevelIndex) -> Arc<ArchiveCache> {
    static POOL: OnceLock<Mutex<HashMap<PathBuf, Arc<ArchiveCache>>>> = OnceLock::new();
    let pool = POOL.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = pool.lock().unwrap();
    guard
        .entry(index.game_dir.clone())
        .or_insert_with(|| Arc::new(index.toc.archive_cache(&index.game_dir)))
        .clone()
}

/// The first `n` bytes of an asset's **body**, without the 36-byte TOC asset
/// header [`LevelIndex::extract`] prepends.
///
/// The TOC entry is copied and narrowed rather than followed, so a DSAR archive
/// only decompresses the 64 KB chunks the requested range touches.
pub fn read_asset_prefix(index: &LevelIndex, id: u64, n: u32) -> Result<Vec<u8>> {
    let asset = index
        .asset(id)
        .ok_or_else(|| ToolkitError::Parse(format!("asset {id:016X} not found in TOC")))?;
    let mut probe = asset.clone();
    probe.size = asset.size.min(n);
    probe.header_offset = -1; // body only — the DAT1 then starts at byte 0
    index
        .toc
        .extract_asset_with_cache(&probe, &archive_pool(index))
}

// ---------------------------------------------------------------------------
// Header cache
// ---------------------------------------------------------------------------

fn info_cache() -> &'static DashMap<u64, Option<TexInfo>> {
    static CACHE: OnceLock<DashMap<u64, Option<TexInfo>>> = OnceLock::new();
    CACHE.get_or_init(DashMap::new)
}

/// Parse a texture asset body (DAT1 at byte 0) into its header.
fn parse_body(body: &[u8]) -> Result<TexInfo> {
    if body.len() < 4 {
        return Err(ToolkitError::Parse("texture asset too small".into()));
    }
    // Belt and braces: accept a body that still carries the TOC asset header.
    let base = if u32::from_le_bytes(body[0..4].try_into().unwrap()) == DAT1_MAGIC {
        0
    } else if body.len() > 40
        && u32::from_le_bytes(body[36..40].try_into().unwrap()) == DAT1_MAGIC
    {
        36
    } else {
        return Err(ToolkitError::Parse(
            "not a texture asset (no DAT1 at 0 or 36)".into(),
        ));
    };

    let dat1 = Dat1::parse(&body[base..])?;
    let section = dat1
        .get_section_data(TAG_TEXTURE_HEADER)
        .ok_or(ToolkitError::SectionNotFound(TAG_TEXTURE_HEADER))?;

    // Pixels start where the DAT1 ends. `total_size` says so; fall back to the
    // end of the last section when a rebuilt asset leaves it stale.
    let sections_end = dat1
        .sections
        .iter()
        .map(|s| s.offset as usize + s.size as usize)
        .max()
        .unwrap_or(0);
    let dat1_len = (dat1.total_size as usize).max(sections_end);

    TexInfo::parse(section, base + dat1_len)
}

/// Header-only lookup for one texture id. Memoised, including the failures, so
/// a zone full of missing textures does not re-probe the archives per frame.
pub fn texture_info(index: &LevelIndex, id: u64) -> Option<TexInfo> {
    if let Some(hit) = info_cache().get(&id) {
        return *hit;
    }
    let info = match read_asset_prefix(index, id, HEADER_PROBE_BYTES).and_then(|b| parse_body(&b)) {
        Ok(i) => Some(i),
        Err(e) => {
            debug!("texture {id:016X}: {e}");
            None
        }
    };
    info_cache().insert(id, info);
    info
}

/// Entries currently held by the header cache, for diagnostics.
pub fn info_cache_len() -> usize {
    info_cache().len()
}

// ---------------------------------------------------------------------------
// Mip selection
// ---------------------------------------------------------------------------

/// Byte span of resident mip `level` of slice 0, relative to the resident blob.
fn mip_span(info: &TexInfo, level: u32) -> Option<(usize, usize)> {
    let mut off = 0usize;
    for m in 0..=level {
        let w = (info.sd_w >> m).max(1);
        let h = (info.sd_h >> m).max(1);
        let size = mip_level_size(w, h, info.format as u32)? as usize;
        if m == level {
            return Some((off, size));
        }
        off += size;
    }
    None
}

/// Byte span of resident mip `level` of array slice `slice`, relative to the
/// resident blob. Slice-major, so it is slice 0's span shifted by
/// `slice * slice_stride()`.
fn slice_mip_span(info: &TexInfo, slice: u32, level: u32) -> Option<(usize, usize)> {
    let (off, size) = mip_span(info, level)?;
    Some((off + slice as usize * info.slice_stride(), size))
}

/// The smallest resident mip whose larger side is still at or above `cap`.
///
/// Returns the base mip when the resident chain never reaches `cap` — that is
/// the best the resident data can do, and chasing the streamed tiles for the
/// difference is not worth a viewport's while.
pub fn pick_mip(info: &TexInfo, cap: u32) -> u32 {
    let mips = info.sd_mips.max(1) as u32;
    let mut m = 0u32;
    while m + 1 < mips {
        let w = (info.sd_w >> (m + 1)).max(1);
        let h = (info.sd_h >> (m + 1)).max(1);
        if w.max(h) >= cap {
            m += 1;
        } else {
            break;
        }
    }
    m
}

// ---------------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------------

/// What [`decode_texture`] resolved, for the probe's accounting.
#[derive(Debug, Clone, Copy)]
pub struct DecodeStats {
    pub width: u32,
    pub height: u32,
    pub mip: u32,
    /// Which array slice was decoded.
    pub slice: u32,
    /// How many slices the texture has, so a caller can tell "asked for 3, got
    /// 3" from "asked for 3 on a flat texture".
    pub slices: u32,
    pub png_bytes: usize,
    /// Bytes of the asset body this decode had to read. A later slice sits
    /// further into the blob, so it costs more: see `texture_probe --blend`.
    pub read_bytes: usize,
    /// Always true today — see the module docs on streamed mips.
    pub from_resident: bool,
}

/// Decode slice 0 of one texture to a PNG of the mip chosen by [`pick_mip`].
pub fn decode_texture(
    index: &LevelIndex,
    id: u64,
    max_size: u32,
) -> Result<(Vec<u8>, DecodeStats)> {
    decode_texture_slice(index, id, 0, max_size)
}

/// Decode one **array slice** of a texture to a PNG of the mip [`pick_mip`]
/// chooses.
///
/// A `slice` past the texture's own slice count is an error rather than a
/// silent clamp to 0: a caller asking for the height field of a colour map that
/// turns out to be flat needs to know it got nothing, not a second copy of the
/// albedo.
pub fn decode_texture_slice(
    index: &LevelIndex,
    id: u64,
    slice: u32,
    max_size: u32,
) -> Result<(Vec<u8>, DecodeStats)> {
    let cap = max_size.clamp(MIN_CAP, MAX_CAP);
    let info =
        texture_info(index, id).ok_or_else(|| ToolkitError::Parse("no texture header".into()))?;
    if info.sd_len == 0 || info.sd_w == 0 || info.sd_h == 0 {
        return Err(ToolkitError::Parse("texture has no resident mips".into()));
    }
    let slices = info.slices().max(1);
    if slice >= slices {
        return Err(ToolkitError::Parse(format!(
            "slice {slice} requested, texture has {slices}"
        )));
    }

    let level = pick_mip(&info, cap);
    let (mip_off, mip_size) = slice_mip_span(&info, slice, level)
        .ok_or_else(|| ToolkitError::Unsupported(format!("DXGI format {}", info.format)))?;

    if mip_off + mip_size > info.sd_len as usize {
        return Err(ToolkitError::Parse(format!(
            "slice {slice} mip {level} span {mip_off}+{mip_size} past the \
             {}-byte resident blob",
            info.sd_len
        )));
    }

    // Read only as far as the mip we want — for slice N that is N strides in,
    // so a later slice reads more of the asset. The TOC entry is narrowed, not
    // followed, so a DSAR archive still decompresses only the 64 KB chunks the
    // range touches.
    let want = (info.pixel_offset + mip_off + mip_size) as u32;
    let body = read_asset_prefix(index, id, want)?;
    let read_bytes = body.len();

    let w = (info.sd_w >> level).max(1) as usize;
    let h = (info.sd_h >> level).max(1) as usize;
    let png = decode_one(&info, &body, slice, slices, level, w, h, mip_size)?;

    let stats = DecodeStats {
        width: w as u32,
        height: h as u32,
        mip: level,
        slice,
        slices,
        png_bytes: png.len(),
        read_bytes,
        from_resident: true,
    };
    Ok((png, stats))
}

// ---------------------------------------------------------------------------
// PNG LRU
// ---------------------------------------------------------------------------

/// `(asset id, size cap, array slice)`.
type PngKey = (u64, u32, u32);

struct PngCache {
    map: DashMap<PngKey, Arc<Vec<u8>>>,
    order: Mutex<Vec<PngKey>>,
    bytes: AtomicUsize,
}

fn png_cache() -> &'static PngCache {
    static CACHE: OnceLock<PngCache> = OnceLock::new();
    CACHE.get_or_init(|| PngCache {
        map: DashMap::new(),
        order: Mutex::new(Vec::new()),
        bytes: AtomicUsize::new(0),
    })
}

fn cache_get(key: PngKey) -> Option<Arc<Vec<u8>>> {
    let hit = png_cache().map.get(&key).map(|v| Arc::clone(&v))?;
    let mut order = png_cache().order.lock().unwrap();
    if let Some(pos) = order.iter().position(|k| *k == key) {
        let k = order.remove(pos);
        order.push(k);
    }
    Some(hit)
}

fn cache_put(key: PngKey, data: Arc<Vec<u8>>) {
    let cache = png_cache();
    let len = data.len();
    if cache.map.insert(key, data).is_none() {
        cache.bytes.fetch_add(len, Ordering::Relaxed);
    }
    let mut order = cache.order.lock().unwrap();
    if let Some(pos) = order.iter().position(|k| *k == key) {
        order.remove(pos);
    }
    order.push(key);
    while cache.bytes.load(Ordering::Relaxed) > PNG_CACHE_BUDGET && order.len() > 1 {
        let victim = order.remove(0);
        if let Some((_, v)) = cache.map.remove(&victim) {
            cache.bytes.fetch_sub(v.len(), Ordering::Relaxed);
        }
    }
}

pub fn png_cache_stats() -> (usize, usize) {
    let c = png_cache();
    (c.map.len(), c.bytes.load(Ordering::Relaxed))
}

/// Drop every decoded PNG. Only a harness measuring a cold decode wants this;
/// the served path never calls it.
pub fn png_cache_clear() {
    let c = png_cache();
    let mut order = c.order.lock().unwrap();
    order.clear();
    c.map.clear();
    c.bytes.store(0, Ordering::Relaxed);
}

/// PNG bytes for slice 0 of one texture at a size cap.
pub fn texture_png_bytes(index: &LevelIndex, id: u64, max_size: u32) -> Arc<Vec<u8>> {
    texture_png_bytes_slice(index, id, 0, max_size)
}

/// PNG bytes for one **array slice** of a texture at a size cap, decoded at
/// most once per `(id, cap, slice)`.
///
/// Never fails: a texture that cannot be found or decoded, or a slice the
/// texture does not have, yields an **empty** buffer, which the viewport reads
/// as "keep the fallback for this slot". One unsupported format, or one colour
/// map that turns out to be flat, must not abort a zone's upload loop.
pub fn texture_png_bytes_slice(
    index: &LevelIndex,
    id: u64,
    slice: u32,
    max_size: u32,
) -> Arc<Vec<u8>> {
    let cap = max_size.clamp(MIN_CAP, MAX_CAP);
    let key = (id, cap, slice);
    if let Some(hit) = cache_get(key) {
        return hit;
    }
    let png = match decode_texture_slice(index, id, slice, cap) {
        Ok((bytes, _)) => bytes,
        Err(e) => {
            debug!("texture {id:016X} slice {slice} @{cap}: {e}");
            Vec::new()
        }
    };
    let arc = Arc::new(png);
    cache_put(key, Arc::clone(&arc));
    arc
}

/// PNG bytes for **several** slices of one texture, from a single archive read.
///
/// [`texture_png_bytes_slice`] narrows its read to "everything up to the end of
/// the slice I want", so slice 3 re-reads slices 0-2 on the way past. Asking
/// for four slices one at a time therefore reads the prefix four times over:
/// measured on the 23 four-slice colour layers of Nefarious City
/// `tile_e18_art` at a 512 cap, 4.26 + 9.94 + 15.62 + 21.30 = **51.1 MB** of
/// asset body for 21.3 MB of distinct bytes. This reads once and decodes each
/// requested slice out of that one buffer.
///
/// Results come back in the order asked for, and every one is also put in the
/// same LRU [`texture_png_bytes_slice`] reads, so a later single-slice request
/// for any of them is a cache hit. A slice the texture does not have, or that
/// will not decode, comes back empty — same contract as the single call.
pub fn texture_png_bytes_multi(
    index: &LevelIndex,
    id: u64,
    slices: &[u32],
    max_size: u32,
) -> Vec<Arc<Vec<u8>>> {
    let cap = max_size.clamp(MIN_CAP, MAX_CAP);
    let empty = || Arc::new(Vec::new());
    if slices.is_empty() {
        return Vec::new();
    }

    // Anything already decoded costs nothing; only miss on the rest.
    let cached: Vec<Option<Arc<Vec<u8>>>> =
        slices.iter().map(|&s| cache_get((id, cap, s))).collect();
    if cached.iter().all(Option::is_some) {
        return cached.into_iter().map(Option::unwrap).collect();
    }

    let Some(info) = texture_info(index, id) else {
        return slices.iter().map(|_| empty()).collect();
    };
    let count = info.slices().max(1);
    let level = pick_mip(&info, cap);
    let Some((_, mip_size)) = mip_span(&info, level) else {
        return slices.iter().map(|_| empty()).collect();
    };

    // One read, sized by the furthest slice actually wanted.
    let furthest = slices.iter().copied().filter(|&s| s < count).max();
    let Some(furthest) = furthest else {
        return slices.iter().map(|_| empty()).collect();
    };
    let Some((last_off, _)) = slice_mip_span(&info, furthest, level) else {
        return slices.iter().map(|_| empty()).collect();
    };
    let body = match read_asset_prefix(index, id, (info.pixel_offset + last_off + mip_size) as u32) {
        Ok(b) => b,
        Err(e) => {
            debug!("texture {id:016X} multi-slice @{cap}: {e}");
            return slices.iter().map(|_| empty()).collect();
        }
    };

    let w = (info.sd_w >> level).max(1) as usize;
    let h = (info.sd_h >> level).max(1) as usize;
    slices
        .iter()
        .zip(cached)
        .map(|(&slice, hit)| {
            if let Some(h) = hit {
                return h;
            }
            let png = decode_one(&info, &body, slice, count, level, w, h, mip_size)
                .unwrap_or_else(|e| {
                    debug!("texture {id:016X} slice {slice} @{cap}: {e}");
                    Vec::new()
                });
            let arc = Arc::new(png);
            cache_put((id, cap, slice), Arc::clone(&arc));
            arc
        })
        .collect()
}

/// The pixels-to-PNG half of [`decode_texture_slice`], against a body already
/// in hand. Kept separate so [`texture_png_bytes_multi`] can reuse one read.
#[allow(clippy::too_many_arguments)]
fn decode_one(
    info: &TexInfo,
    body: &[u8],
    slice: u32,
    count: u32,
    level: u32,
    w: usize,
    h: usize,
    mip_size: usize,
) -> Result<Vec<u8>> {
    if slice >= count {
        return Err(ToolkitError::Parse(format!(
            "slice {slice} requested, texture has {count}"
        )));
    }
    let (off, _) = slice_mip_span(info, slice, level)
        .ok_or_else(|| ToolkitError::Unsupported(format!("DXGI format {}", info.format)))?;
    let start = info.pixel_offset + off;
    let end = start + mip_size;
    if end > body.len() {
        return Err(ToolkitError::Parse(format!(
            "texture body is {} bytes, needed {end}",
            body.len()
        )));
    }
    let mut rgba = match info.format {
        FORMAT_R8G8_UNORM => decode_r8g8(&body[start..end], w, h),
        _ => decode_to_rgba(
            &body[start..end],
            w,
            h,
            info.format as u32,
            info.content_type,
        )
        .map_err(ToolkitError::Parse)?,
    };
    if info.content_type == CONTENT_NORMAL && info.format != FORMAT_BC5_UNORM {
        rebuild_normal_z(&mut rgba);
    }
    let mut png = Vec::with_capacity(w * h);
    PngEncoder::new_with_quality(&mut png, CompressionType::Fast, FilterType::Adaptive)
        .write_image(&rgba, w as u32, h as u32, ExtendedColorType::Rgba8)
        .map_err(|e| ToolkitError::Parse(format!("PNG encode failed: {e}")))?;
    Ok(png)
}

/// How many array slices a texture has, or 0 when its header will not parse.
/// Header-only, so a caller can skip a request it knows will come back empty.
pub fn texture_slices(index: &LevelIndex, id: u64) -> u32 {
    texture_info(index, id).map(|i| i.slices()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(sd_w: u32, sd_h: u32, sd_mips: u8, format: u16) -> TexInfo {
        TexInfo {
            sd_len: 0,
            hd_len: 0,
            hd_w: 0,
            hd_h: 0,
            sd_w,
            sd_h,
            surfaces: 1,
            format_flags: 0x0010,
            dimension: 1,
            content_type: 0,
            format,
            sd_mips,
            hd_mips: 0,
            pixel_offset: 92,
        }
    }

    #[test]
    fn picks_the_smallest_mip_still_at_or_above_the_cap() {
        let i = info(512, 512, 10, 98); // BC7, 512 -> 1
        assert_eq!(pick_mip(&i, 512), 0);
        assert_eq!(pick_mip(&i, 256), 1);
        assert_eq!(pick_mip(&i, 128), 2);
        // Below the whole chain: clamp at the last mip, never past it.
        assert!(pick_mip(&i, 1) <= 9);
    }

    #[test]
    fn a_texture_smaller_than_the_cap_uses_its_base_mip() {
        let i = info(4, 4, 1, 71);
        assert_eq!(pick_mip(&i, 512), 0);
    }

    #[test]
    fn mip_spans_are_contiguous_and_block_sized() {
        let i = info(512, 512, 4, 98); // BC7 = 16 bytes per 4x4 block
        assert_eq!(mip_span(&i, 0), Some((0, 512 * 512)));
        assert_eq!(mip_span(&i, 1), Some((512 * 512, 256 * 256)));
        let (o2, s2) = mip_span(&i, 2).unwrap();
        assert_eq!(o2, 512 * 512 + 256 * 256);
        assert_eq!(s2, 128 * 128);
    }

    #[test]
    fn the_slice_stride_is_one_whole_mip_chain() {
        // BC7, 4 mips at 512: 512^2 + 256^2 + 128^2 + 64^2 bytes.
        let mut i = info(512, 512, 4, 98);
        let chain = 512 * 512 + 256 * 256 + 128 * 128 + 64 * 64;
        assert_eq!(i.resident_chain_bytes(), Some(chain));
        assert_eq!(i.slice_stride(), chain);
        // A four-slice array is four chains, and that is what the header says.
        i.surfaces = 4;
        i.sd_len = (chain * 4) as u32;
        assert_eq!(i.slices(), 4);
        assert_eq!(i.slice_stride() * i.slices() as usize, i.sd_len as usize);
    }

    #[test]
    fn slice_spans_are_slice_major() {
        let mut i = info(512, 512, 4, 98);
        i.surfaces = 4;
        let stride = i.slice_stride();
        // Slice 0 is the plain mip chain.
        assert_eq!(slice_mip_span(&i, 0, 0), Some((0, 512 * 512)));
        assert_eq!(slice_mip_span(&i, 0, 1), Some((512 * 512, 256 * 256)));
        // Every later slice is the same chain shifted by one stride.
        for s in 1..4u32 {
            let (off, size) = slice_mip_span(&i, s, 1).unwrap();
            assert_eq!(off, s as usize * stride + 512 * 512);
            assert_eq!(size, 256 * 256);
        }
        // Slice 3, mip 0 must still end inside the blob.
        let (off, size) = slice_mip_span(&i, 3, 0).unwrap();
        assert!(off + size <= stride * 4);
    }

    #[test]
    fn cube_maps_count_six_faces() {
        let mut i = info(64, 64, 1, 71);
        i.dimension = 4;
        assert_eq!(i.slices(), 6);
        i.dimension = 1;
        assert_eq!(i.slices(), 1);
    }

    #[test]
    fn native_size_prefers_the_streamed_chain() {
        let mut i = info(512, 512, 3, 98);
        assert_eq!(i.native(), (512, 512));
        i.hd_w = 2048;
        i.hd_h = 2048;
        i.hd_mips = 2;
        assert_eq!(i.native(), (2048, 2048));
    }

    #[test]
    fn format_names_cover_the_shipped_set() {
        assert_eq!(format_name(98), "BC7_UNORM");
        assert_eq!(format_name(95), "BC6H_UF16");
        assert_eq!(format_name(4242), "DXGI_4242");
        assert!(is_srgb(99) && !is_srgb(98));
    }
}
