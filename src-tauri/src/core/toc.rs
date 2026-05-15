use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use byteorder::{ReadBytesExt, LE};
use dashmap::DashMap;
use flate2::read::ZlibDecoder;
use log::{debug, info, warn};
use memmap2::Mmap;

use crate::core::dat1::Dat1;
use crate::core::error::{Result, ToolkitError};

const TOC_MAGIC_OLD: u32 = 0x77AF12AF; // i20 — zlib-compressed
const TOC_MAGIC_RCRA: u32 = 0x34E89035; // i29 (RCRA) — raw/uncompressed
const DSAR_MAGIC: u32 = 0x52415344;

const SECTION_ASSET_IDS: u32 = 0x506D7B8A;
const SECTION_SPANS: u32 = 0xEDE8ADA9;
const SECTION_SIZES: u32 = 0x65BCF461;
const SECTION_ARCHIVES: u32 = 0x398ABFF0;
const SECTION_ASSET_HEADERS: u32 = 0x654BDED9;

const SIZE_ENTRY_LEN: usize = 16;
const ARCHIVE_ENTRY_LEN: usize = 66;
const ARCHIVE_NAME_LEN: usize = 40;
const ASSET_HEADER_LEN: usize = 36;

#[derive(Debug, Clone)]
pub struct TocAsset {
    pub asset_id: u64,
    pub archive_index: u32,
    pub offset: u32,
    pub size: u32,
    pub header_offset: i32,
    pub span_index: u8,
}

#[derive(Debug, Clone)]
struct Span {
    asset_index: u32,
    count: u32,
}

#[derive(Debug, Clone)]
struct SizeEntry {
    size: u32,
    archive_index: u32,
    offset: u32,
    header_offset: i32,
}

#[derive(Debug)]
pub struct Toc {
    asset_ids: Vec<u64>,
    spans: Vec<Span>,
    sizes: Vec<SizeEntry>,
    archive_names: Vec<String>,
    asset_headers: Vec<Vec<u8>>,
}

impl Toc {
    /// Parse a toc file from raw bytes (handles magic check + optional zlib decompression + DAT1 parsing).
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 8 {
            return Err(ToolkitError::Parse("TOC file too small".into()));
        }

        let magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
        let _declared_size = u32::from_le_bytes(data[4..8].try_into().unwrap());

        let dat1_bytes = match magic {
            TOC_MAGIC_RCRA => {
                // i29 (RCRA): raw uncompressed DAT1 after the 8-byte header
                debug!("TOC: RCRA format (magic {:#010X}), raw DAT1", magic);
                data[8..].to_vec()
            }
            TOC_MAGIC_OLD => {
                // i20: zlib-compressed DAT1 after the 8-byte header
                debug!("TOC: old format (magic {:#010X}), zlib-compressed", magic);
                let mut decompressed = Vec::new();
                let mut decoder = ZlibDecoder::new(&data[8..]);
                decoder
                    .read_to_end(&mut decompressed)
                    .map_err(|e| ToolkitError::Parse(format!("zlib decompression failed: {e}")))?;
                decompressed
            }
            _ => {
                return Err(ToolkitError::InvalidMagic {
                    expected: TOC_MAGIC_RCRA,
                    got: magic,
                });
            }
        };

        let dat1 = Dat1::parse(&dat1_bytes)?;

        let asset_ids = Self::parse_asset_ids(&dat1)?;
        let spans = Self::parse_spans(&dat1)?;
        let sizes = Self::parse_sizes(&dat1)?;
        let archive_names = Self::parse_archives(&dat1)?;
        let asset_headers = Self::parse_asset_headers(&dat1);

        info!(
            "TOC parsed: {} assets, {} spans, {} archives",
            asset_ids.len(),
            spans.len(),
            archive_names.len()
        );

        Ok(Self {
            asset_ids,
            spans,
            sizes,
            archive_names,
            asset_headers,
        })
    }

    fn parse_asset_ids(dat1: &Dat1) -> Result<Vec<u64>> {
        let data = dat1
            .get_section_data(SECTION_ASSET_IDS)
            .ok_or(ToolkitError::SectionNotFound(SECTION_ASSET_IDS))?;
        let count = data.len() / 8;
        let mut cur = Cursor::new(data);
        let mut ids = Vec::with_capacity(count);
        for _ in 0..count {
            ids.push(cur.read_u64::<LE>()?);
        }
        debug!("Parsed {} asset IDs", ids.len());
        Ok(ids)
    }

    fn parse_spans(dat1: &Dat1) -> Result<Vec<Span>> {
        let data = dat1
            .get_section_data(SECTION_SPANS)
            .ok_or(ToolkitError::SectionNotFound(SECTION_SPANS))?;
        let count = data.len() / 8;
        let mut cur = Cursor::new(data);
        let mut spans = Vec::with_capacity(count);
        for _ in 0..count {
            spans.push(Span {
                asset_index: cur.read_u32::<LE>()?,
                count: cur.read_u32::<LE>()?,
            });
        }
        debug!("Parsed {} spans", spans.len());
        Ok(spans)
    }

    fn parse_sizes(dat1: &Dat1) -> Result<Vec<SizeEntry>> {
        let data = dat1
            .get_section_data(SECTION_SIZES)
            .ok_or(ToolkitError::SectionNotFound(SECTION_SIZES))?;
        let count = data.len() / SIZE_ENTRY_LEN;
        let mut cur = Cursor::new(data);
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            entries.push(SizeEntry {
                size: cur.read_u32::<LE>()?,
                archive_index: cur.read_u32::<LE>()?,
                offset: cur.read_u32::<LE>()?,
                header_offset: cur.read_i32::<LE>()?,
            });
        }
        debug!("Parsed {} size entries", entries.len());
        Ok(entries)
    }

    fn parse_archives(dat1: &Dat1) -> Result<Vec<String>> {
        let data = dat1
            .get_section_data(SECTION_ARCHIVES)
            .ok_or(ToolkitError::SectionNotFound(SECTION_ARCHIVES))?;
        let count = data.len() / ARCHIVE_ENTRY_LEN;
        let mut names = Vec::with_capacity(count);
        for i in 0..count {
            let start = i * ARCHIVE_ENTRY_LEN;
            let name_bytes = &data[start..start + ARCHIVE_NAME_LEN];
            let end = name_bytes
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(ARCHIVE_NAME_LEN);
            let name = String::from_utf8_lossy(&name_bytes[..end]).into_owned();
            names.push(name);
        }
        debug!("Parsed {} archive names", names.len());
        Ok(names)
    }

    fn parse_asset_headers(dat1: &Dat1) -> Vec<Vec<u8>> {
        let Some(data) = dat1.get_section_data(SECTION_ASSET_HEADERS) else {
            return Vec::new();
        };
        if data.len() % ASSET_HEADER_LEN != 0 {
            warn!(
                "TOC asset header section size {} is not divisible by {}",
                data.len(),
                ASSET_HEADER_LEN
            );
        }
        let count = data.len() / ASSET_HEADER_LEN;
        let mut headers = Vec::with_capacity(count);
        for i in 0..count {
            let start = i * ASSET_HEADER_LEN;
            headers.push(data[start..start + ASSET_HEADER_LEN].to_vec());
        }
        debug!("Parsed {} asset headers", headers.len());
        headers
    }

    /// Get all asset IDs.
    pub fn asset_ids(&self) -> &[u64] {
        &self.asset_ids
    }

    /// Get all assets as TocAsset structs.
    pub fn assets(&self) -> Vec<TocAsset> {
        let mut result = Vec::with_capacity(self.asset_ids.len());
        for (span_idx, span) in self.spans.iter().enumerate() {
            let start = span.asset_index as usize;
            let end = start + span.count as usize;
            for i in start..end {
                if i >= self.asset_ids.len() || i >= self.sizes.len() {
                    break;
                }
                let s = &self.sizes[i];
                result.push(TocAsset {
                    asset_id: self.asset_ids[i],
                    archive_index: s.archive_index,
                    offset: s.offset,
                    size: s.size,
                    header_offset: s.header_offset,
                    span_index: span_idx as u8,
                });
            }
        }
        result
    }

    /// Get list of archive filenames.
    pub fn archive_filenames(&self) -> Vec<String> {
        self.archive_names.clone()
    }

    /// Get the number of archives.
    pub fn archive_count(&self) -> usize {
        self.archive_names.len()
    }

    /// Extract an asset's raw bytes given the archives directory path.
    pub fn extract_asset(&self, asset: &TocAsset, archives_dir: &Path) -> Result<Vec<u8>> {
        let archive_name = self
            .archive_names
            .get(asset.archive_index as usize)
            .ok_or_else(|| {
                ToolkitError::Parse(format!(
                    "archive index {} out of range ({})",
                    asset.archive_index,
                    self.archive_names.len()
                ))
            })?;

        let archive_path = archives_dir.join(archive_name);
        debug!(
            "Extracting asset {:#018X}: archive={}, offset={}, size={}",
            asset.asset_id, archive_name, asset.offset, asset.size
        );

        let archive_data = std::fs::read(&archive_path).map_err(|e| {
            ToolkitError::Parse(format!(
                "failed to read archive {}: {e}",
                archive_path.display()
            ))
        })?;

        self.extract_asset_from_bytes(asset, &archive_data)
    }

    /// Extract an asset using a pre-loaded archive buffer (typically from
    /// an [`ArchiveCache`]). Identical to [`Self::extract_asset`] but skips
    /// the per-call `std::fs::read`, which is the dominant cost when
    /// scanning the whole TOC for inbound references.
    pub fn extract_asset_with_cache(
        &self,
        asset: &TocAsset,
        cache: &ArchiveCache,
    ) -> Result<Vec<u8>> {
        let mmap = cache.get(asset.archive_index)?;
        self.extract_asset_from_bytes(asset, &mmap[..])
    }

    fn extract_asset_from_bytes(
        &self,
        asset: &TocAsset,
        archive_data: &[u8],
    ) -> Result<Vec<u8>> {
        let mut raw =
            extract_from_archive(archive_data, asset.offset as u64, asset.size as usize)?;

        if asset.header_offset >= 0 {
            let header_index = (asset.header_offset as usize) / ASSET_HEADER_LEN;
            let header = self.asset_headers.get(header_index).ok_or_else(|| {
                ToolkitError::Parse(format!(
                    "asset header index {} out of range ({}) for asset {:#018X}",
                    header_index,
                    self.asset_headers.len(),
                    asset.asset_id
                ))
            })?;
            let mut combined = Vec::with_capacity(header.len() + raw.len());
            combined.extend_from_slice(header);
            combined.extend_from_slice(&raw);
            raw = combined;
        }

        Ok(raw)
    }

    /// Build a per-scan archive buffer cache rooted at `archives_dir`. The
    /// cache mmaps each archive lazily on first access and shares it across
    /// callers via `Arc<Mmap>`. Drop the cache at the end of a scan to
    /// release the mappings.
    pub fn archive_cache(&self, archives_dir: &Path) -> ArchiveCache {
        ArchiveCache::new(archives_dir.to_path_buf(), self.archive_names.clone())
    }
}

// ---------------------------------------------------------------------------
// ArchiveCache — shared, lazy, mmap-backed archive buffer pool
// ---------------------------------------------------------------------------

/// Reference-counted, lazily-populated cache of memory-mapped archive
/// files keyed by archive index. Designed to live for the duration of a
/// single inbound scan: the first asset out of each archive triggers an
/// `mmap`; every subsequent asset reuses the mapping with no I/O. The
/// underlying `Arc<Mmap>` is cheap to clone and safe to share across
/// rayon workers.
pub struct ArchiveCache {
    archives_dir: PathBuf,
    archive_names: Vec<String>,
    mmaps: DashMap<u32, Arc<Mmap>>,
}

impl ArchiveCache {
    fn new(archives_dir: PathBuf, archive_names: Vec<String>) -> Self {
        Self {
            archives_dir,
            archive_names,
            mmaps: DashMap::new(),
        }
    }

    /// Get the mmap for `archive_index`, opening it on first access.
    pub fn get(&self, archive_index: u32) -> Result<Arc<Mmap>> {
        if let Some(existing) = self.mmaps.get(&archive_index) {
            return Ok(existing.clone());
        }

        let name = self
            .archive_names
            .get(archive_index as usize)
            .ok_or_else(|| {
                ToolkitError::Parse(format!(
                    "archive index {} out of range ({})",
                    archive_index,
                    self.archive_names.len()
                ))
            })?;
        let path = self.archives_dir.join(name);
        let file = std::fs::File::open(&path).map_err(|e| {
            ToolkitError::Parse(format!("failed to open archive {}: {e}", path.display()))
        })?;
        // SAFETY: the archive files are read-only data; we never mutate
        // through the mapping. Concurrent readers are safe.
        let mmap = unsafe { Mmap::map(&file) }.map_err(|e| {
            ToolkitError::Parse(format!("failed to mmap archive {}: {e}", path.display()))
        })?;

        let arc = Arc::new(mmap);
        // Insert may race with another worker; either inserts the same
        // semantic mapping. Re-fetch to share whichever won.
        self.mmaps.insert(archive_index, arc.clone());
        Ok(arc)
    }

    /// Number of archives currently held in the cache (for diagnostics).
    pub fn len(&self) -> usize {
        self.mmaps.len()
    }
}

// ---------------------------------------------------------------------------
// DSAR compressed archive support
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct DsarBlockHeader {
    real_offset: u32,
    comp_offset: u32,
    real_size: u32,
    comp_size: u32,
    compression_type: u8,
}

fn parse_dsar_blocks(data: &[u8]) -> Result<(Vec<DsarBlockHeader>, u32)> {
    let mut cur = Cursor::new(data);

    let magic = cur.read_u32::<LE>()?;
    if magic != DSAR_MAGIC {
        return Err(ToolkitError::InvalidMagic {
            expected: DSAR_MAGIC,
            got: magic,
        });
    }
    let _version = cur.read_u32::<LE>()?;
    let _block_count = cur.read_u32::<LE>()?;
    let blocks_header_end = cur.read_u32::<LE>()?;
    let _original_size = cur.read_u64::<LE>()?;
    // 8 bytes padding
    cur.seek(SeekFrom::Current(8))?;

    let mut blocks = Vec::new();
    while (cur.position() as u32) < blocks_header_end {
        let real_offset = cur.read_u32::<LE>()?;
        let _unk1 = cur.read_u32::<LE>()?;
        let comp_offset = cur.read_u32::<LE>()?;
        let _unk2 = cur.read_u32::<LE>()?;
        let real_size = cur.read_u32::<LE>()?;
        let comp_size = cur.read_u32::<LE>()?;
        let compression_type = cur.read_u8()?;
        // 7 bytes padding
        cur.seek(SeekFrom::Current(7))?;

        blocks.push(DsarBlockHeader {
            real_offset,
            comp_offset,
            real_size,
            comp_size,
            compression_type,
        });
    }

    blocks.sort_by_key(|b| b.real_offset);

    debug!(
        "DSAR: {} blocks, header_end={}",
        blocks.len(),
        blocks_header_end
    );
    Ok((blocks, blocks_header_end))
}

fn extract_from_archive(archive_data: &[u8], offset: u64, size: usize) -> Result<Vec<u8>> {
    if archive_data.len() < 4 {
        return Err(ToolkitError::Parse("archive file too small".into()));
    }

    let magic = u32::from_le_bytes(archive_data[0..4].try_into().unwrap());
    if magic != DSAR_MAGIC {
        // Uncompressed archive: raw read
        let start = offset as usize;
        let end = start + size;
        if end > archive_data.len() {
            return Err(ToolkitError::Parse(format!(
                "read past end of uncompressed archive: offset={}, size={}, archive_len={}",
                offset,
                size,
                archive_data.len()
            )));
        }
        return Ok(archive_data[start..end].to_vec());
    }

    // DSAR compressed archive
    let (blocks, blocks_header_end) = parse_dsar_blocks(archive_data)?;
    let asset_start = offset;
    let asset_end = offset + size as u64;

    // Binary search for the first block whose real_offset region covers asset_start
    let first =
        blocks.partition_point(|b| (b.real_offset as u64 + b.real_size as u64) <= asset_start);

    let mut result = vec![0u8; size];
    let mut filled = 0usize;

    for block in &blocks[first..] {
        let block_real_start = block.real_offset as u64;
        let block_real_end = block_real_start + block.real_size as u64;

        if block_real_start >= asset_end {
            break;
        }

        debug!(
            "DSAR block: real_offset={}, real_size={}, comp_offset={}, comp_size={}, type={}",
            block.real_offset,
            block.real_size,
            block.comp_offset,
            block.comp_size,
            block.compression_type
        );

        let decompressed = decompress_block(archive_data, block, blocks_header_end)?;

        // Calculate overlap between this block and our requested range
        let overlap_start = asset_start.max(block_real_start);
        let overlap_end = asset_end.min(block_real_end);
        let src_offset = (overlap_start - block_real_start) as usize;
        let dst_offset = (overlap_start - asset_start) as usize;
        let copy_len = (overlap_end - overlap_start) as usize;

        if src_offset + copy_len > decompressed.len() {
            return Err(ToolkitError::Parse(format!(
                "DSAR block decompressed data too short: expected {}, got {}",
                src_offset + copy_len,
                decompressed.len()
            )));
        }

        result[dst_offset..dst_offset + copy_len]
            .copy_from_slice(&decompressed[src_offset..src_offset + copy_len]);
        filled += copy_len;
    }

    if filled != size {
        return Err(ToolkitError::Parse(format!(
            "incomplete asset extraction: expected {} bytes, got {}",
            size, filled
        )));
    }

    Ok(result)
}

fn decompress_block(
    archive_data: &[u8],
    block: &DsarBlockHeader,
    _header_end: u32,
) -> Result<Vec<u8>> {
    // comp_offset is an absolute offset into the archive file
    let data_start = block.comp_offset as usize;
    let data_end = data_start + block.comp_size as usize;

    if data_end > archive_data.len() {
        return Err(ToolkitError::Parse(format!(
            "DSAR block data out of bounds: {}..{} (archive len {})",
            data_start,
            data_end,
            archive_data.len()
        )));
    }

    let compressed = &archive_data[data_start..data_end];

    match block.compression_type {
        0 => Ok(compressed.to_vec()),
        2 => decompress_gdeflate(compressed, block.real_size as usize),
        3 => lz4_flex::decompress(compressed, block.real_size as usize)
            .map_err(|e| ToolkitError::Parse(format!("LZ4 decompression failed: {e}"))),
        other => Err(ToolkitError::Unsupported(format!(
            "DSAR compression type {other}"
        ))),
    }
}

/// GDeflate (type 2) — tiled streaming compression used for textures.
///
/// Header layout (8 bytes):
///   libid u8 | magic u8 | num_tiles u16 | _pad u32
/// Tile offset table (num_tiles × u32):
///   offsets[0]   = compressed size of the LAST tile
///   offsets[i>0] = cumulative byte start of tile i within the data region
///
/// Each tile is a libdeflate "gdeflate page" — NOT plain DEFLATE.
fn decompress_gdeflate(compressed: &[u8], output_size: usize) -> Result<Vec<u8>> {
    if compressed.len() < 8 {
        return Err(ToolkitError::Parse(
            "gdeflate: buffer too small for header".into(),
        ));
    }

    let lib_id = compressed[0];
    let magic = compressed[1];
    if lib_id != 4 || (lib_id ^ magic) != 0xFF {
        return Err(ToolkitError::Parse(format!(
            "gdeflate: bad header (libid={lib_id:#x} magic={magic:#x})"
        )));
    }

    let num_tiles = u16::from_le_bytes([compressed[2], compressed[3]]) as usize;
    if num_tiles == 0 {
        return Ok(vec![0u8; output_size]);
    }

    let offsets_end = 8 + num_tiles * 4;
    if compressed.len() < offsets_end {
        return Err(ToolkitError::Parse(
            "gdeflate: truncated tile offset table".into(),
        ));
    }

    let offsets: Vec<usize> = (0..num_tiles)
        .map(|i| {
            let o = 8 + i * 4;
            u32::from_le_bytes(compressed[o..o + 4].try_into().unwrap()) as usize
        })
        .collect();

    // Reconstruct tile byte ranges
    let mut tile_slices: Vec<&[u8]> = Vec::with_capacity(num_tiles);
    let mut pos = offsets_end;
    for tile_idx in 0..num_tiles {
        let sz = if num_tiles == 1 {
            offsets[0]
        } else if tile_idx == num_tiles - 1 {
            offsets[0]
        } else if tile_idx == 0 {
            offsets[1]
        } else {
            offsets[tile_idx + 1] - offsets[tile_idx]
        };
        let end = pos + sz;
        if end > compressed.len() {
            return Err(ToolkitError::Parse(format!(
                "gdeflate: tile {tile_idx} out of bounds ({end} > {})",
                compressed.len()
            )));
        }
        tile_slices.push(&compressed[pos..end]);
        pos = end;
    }

    debug!(
        "gdeflate: {} tile(s), output_size={}",
        num_tiles, output_size
    );

    #[repr(C)]
    struct InPage {
        data: *const u8,
        nbytes: usize,
    }

    type AllocFn = unsafe extern "C" fn() -> *mut std::ffi::c_void;
    type DecompFn = unsafe extern "C" fn(
        *mut std::ffi::c_void,
        *const InPage,
        usize,
        *mut u8,
        usize,
        *mut usize,
    ) -> i32;
    type FreeFn = unsafe extern "C" fn(*mut std::ffi::c_void);

    // Cache the loaded library for the process lifetime.
    static LIB: OnceLock<Option<libloading::Library>> = OnceLock::new();

    let lib = LIB
        .get_or_init(|| {
            let candidates = [
                // Next to the exe (dev + installed)
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.join("libdeflate.dll"))),
                // Tauri resource dir (bundled release)
                Some(std::path::PathBuf::from("libdeflate.dll")),
            ];
            for path in candidates.into_iter().flatten() {
                if let Ok(lib) = unsafe { libloading::Library::new(&path) } {
                    info!("gdeflate: loaded libdeflate from {}", path.display());
                    return Some(lib);
                }
            }
            warn!(
                "gdeflate: libdeflate.dll not found — type-2 compressed assets cannot be extracted"
            );
            None
        })
        .as_ref()
        .ok_or_else(|| {
            ToolkitError::Parse(
                "gdeflate: libdeflate.dll not found. \
        Please ensure libdeflate.dll is present in the application directory."
                    .into(),
            )
        })?;

    let result = unsafe {
        let alloc: libloading::Symbol<AllocFn> = lib
            .get(b"libdeflate_alloc_gdeflate_decompressor\0")
            .map_err(|e| ToolkitError::Parse(format!("gdeflate: {e}")))?;
        let decomp: libloading::Symbol<DecompFn> = lib
            .get(b"libdeflate_gdeflate_decompress\0")
            .map_err(|e| ToolkitError::Parse(format!("gdeflate: {e}")))?;
        let free: libloading::Symbol<FreeFn> = lib
            .get(b"libdeflate_free_gdeflate_decompressor\0")
            .map_err(|e| ToolkitError::Parse(format!("gdeflate: {e}")))?;

        let alloc_fn: AllocFn = *alloc;
        let decomp_fn: DecompFn = *decomp;
        let free_fn: FreeFn = *free;

        let ctx = alloc_fn();
        if ctx.is_null() {
            return Err(ToolkitError::Parse("gdeflate: alloc returned null".into()));
        }

        let pages: Vec<InPage> = tile_slices
            .iter()
            .map(|s| InPage {
                data: s.as_ptr(),
                nbytes: s.len(),
            })
            .collect();

        let mut out = vec![0u8; output_size];
        let mut actual = 0usize;
        let rc = decomp_fn(
            ctx,
            pages.as_ptr(),
            pages.len(),
            out.as_mut_ptr(),
            out.len(),
            &mut actual,
        );
        free_fn(ctx);

        if rc != 0 {
            return Err(ToolkitError::Parse(format!(
                "gdeflate: libdeflate_gdeflate_decompress returned error {rc}"
            )));
        }
        debug!("gdeflate: ok — {} bytes", actual);
        out
    };

    Ok(result)
}
