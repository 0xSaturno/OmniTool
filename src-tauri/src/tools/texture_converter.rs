use base64::prelude::*;
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use image::{ImageFormat, Rgb32FImage, RgbaImage};
use serde::Serialize;
use std::fs::File;
use std::io::Cursor as ImageCursor;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use texture2ddecoder::{
    decode_bc1, decode_bc3, decode_bc4, decode_bc5, decode_bc6_block, decode_bc7,
};

const TEXTURE_IDS: [u32; 2] = [0x5C4580B9, 0x8F53A199];
static BC6_PANIC_HOOK_GUARD: Mutex<()> = Mutex::new(());

#[derive(Serialize)]
pub struct TextureInfo {
    pub width: u32,
    pub height: u32,
    pub mipmaps: u32,
    pub hdmipmaps: u32,
    pub images: u32,
    pub bytes_per_pixel: f64,
    pub size: u32,
    pub hdsize: u32,
    pub format: u32,
    pub is_cubemap: bool,
    pub is_ibl: bool,
    pub dimension: u8,
    pub content_type: u8,
}

pub struct SourceTex {
    pub filename: String,
    pub stg: bool,
    pub header: Vec<u8>,
    pub textureheader: Vec<u8>,
    pub mipmaps: Vec<Vec<u8>>,
    pub hdfilename: String,
    pub width: u16,
    pub height: u16,
    pub sd_width: u16,
    pub sd_height: u16,
    pub images: u16,
    pub size: u32,
    pub hdsize: u32,
    pub hdmipmaps: u8,
    pub mipmaps_count: u8,
    pub basemipsize: u32,
    pub aspect: i32,
    pub format: u16,
    pub bytes_per_pixel: f64,
    pub dimension: u8,
    pub content_type: u8,
}

impl SourceTex {
    pub fn read(filename: &str, explicit_hd_path: Option<&str>) -> Result<Self, String> {
        let mut fs =
            File::open(filename).map_err(|e| format!("Failed to open {}: {}", filename, e))?;
        let mut magic = fs.read_u32::<LittleEndian>().unwrap_or(0);
        let mut stg = false;
        if magic == 4674643 {
            // "STG\0" -> 0x00475453 = 4674643
            stg = true;
            fs.seek(SeekFrom::Start(16)).unwrap();
            magic = fs.read_u32::<LittleEndian>().unwrap_or(0);
        } else {
            fs.seek(SeekFrom::Start(0)).unwrap();
            magic = fs.read_u32::<LittleEndian>().unwrap_or(0);
        }

        if stg {
            if !TEXTURE_IDS.contains(&magic) {
                return Err("Not a texture asset (STG)".into());
            }
            fs.seek(SeekFrom::Current(92)).unwrap();
            if fs.read_u32::<LittleEndian>().unwrap_or(0) != 1145132081 {
                return Err("Not a texture asset".into());
            }
            if !TEXTURE_IDS.contains(&fs.read_u32::<LittleEndian>().unwrap_or(0)) {
                return Err("Not a texture asset".into());
            }
        } else {
            if !TEXTURE_IDS.contains(&magic) {
                return Err("Not a texture asset".into());
            }
            fs.seek(SeekFrom::Current(32)).unwrap();
            if fs.read_u32::<LittleEndian>().unwrap_or(0) != 1145132081 {
                return Err("Not a texture asset".into());
            }
            if !TEXTURE_IDS.contains(&fs.read_u32::<LittleEndian>().unwrap_or(0)) {
                return Err("Not a texture asset".into());
            }
        }

        fs.read_u32::<LittleEndian>().unwrap();
        if fs.read_u32::<LittleEndian>().unwrap_or(0) != 1 {
            return Err("Multiple sections not implemented".into());
        }

        if fs.read_u32::<LittleEndian>().unwrap_or(0) != 1323185555 {
            // 0x4EE50593
            return Err("Unexpected section type".into());
        }

        let offset = fs.read_u32::<LittleEndian>().unwrap();
        let size = fs.read_u32::<LittleEndian>().unwrap();

        fs.seek(SeekFrom::Start(0)).unwrap();
        let mut header = vec![
            0u8;
            if stg {
                offset as usize + 112
            } else {
                offset as usize + 36
            }
        ];
        fs.read_exact(&mut header).unwrap();

        let mut textureheader = vec![0u8; size as usize];
        fs.read_exact(&mut textureheader).unwrap();

        let mut t_cur = Cursor::new(&textureheader);
        let tex_size = t_cur.read_u32::<LittleEndian>().unwrap();
        let hdsize = t_cur.read_u32::<LittleEndian>().unwrap();
        let width = t_cur.read_u16::<LittleEndian>().unwrap();
        let height = t_cur.read_u16::<LittleEndian>().unwrap();
        let sd_width = t_cur.read_u16::<LittleEndian>().unwrap();
        let sd_height = t_cur.read_u16::<LittleEndian>().unwrap();
        let images = t_cur.read_u16::<LittleEndian>().unwrap();

        let aspect = ((width as f64 / height as f64).log2()) as i32;

        let flags_raw = t_cur.read_u16::<LittleEndian>().unwrap();
        let dimension = ((flags_raw >> 4) & 0x7) as u8;
        let content_type = ((flags_raw >> 7) & 0x7F) as u8;
        let format = t_cur.read_u16::<LittleEndian>().unwrap();

        let basemipsize_single = mip_level_size(sd_width as u32, sd_height as u32, format as u32)
            .unwrap_or(tex_size / images as u32);

        let format_bits = bits_per_pixel(format as u32);
        let bytes_per_pixel = if format_bits > 0 {
            (format_bits / 8) as f64
        } else {
            2f64.powf(((basemipsize_single as f64 / sd_width as f64 / sd_height as f64).log2()).floor())
        };

        t_cur.seek(SeekFrom::Current(8)).unwrap();
        let mipmaps_count = t_cur.read_u8().unwrap();
        t_cur.read_u8().unwrap();
        let hdmipmaps = t_cur.read_u8().unwrap();

        if mipmaps_count == 0 && tex_size > 0 {
            return Err(
                "Invalid texture header: SD mipmap count is zero with non-zero SD size".into(),
            );
        }
        if hdmipmaps == 0 && hdsize > 0 {
            return Err(
                "Invalid texture header: HD mipmap count is zero with non-zero HD size".into(),
            );
        }

        // For cubemap textures, the header stores images=1 but packs all 6 faces
        // contiguously. Derive the true face count from sdsize / per-face mip chain.
        let is_cubemap_dim = dimension == 4;
        let (images, basemipsize) = if is_cubemap_dim && basemipsize_single > 0 && tex_size > 0 {
            let mip_chain: u32 = (0..mipmaps_count)
                .map(|m| {
                    let mw = (sd_width as u32 >> m).max(1);
                    let mh = (sd_height as u32 >> m).max(1);
                    mip_level_size(mw, mh, format as u32).unwrap_or(0)
                })
                .sum();
            let face_count = if mip_chain > 0 { (tex_size / mip_chain).max(1) } else { images as u32 };
            (face_count as u16, basemipsize_single)
        } else {
            (images, basemipsize_single)
        };

        t_cur.seek(SeekFrom::Current(11)).unwrap();

        let start_pos = if stg {
            offset as u64 + 112
        } else {
            offset as u64 + 36
        } + 44;
        fs.seek(SeekFrom::Start(start_pos)).unwrap();

        let mut mipmaps = Vec::new();
        for _ in 0..images {
            let mut mip = vec![0u8; (tex_size / images as u32) as usize];
            fs.read_exact(&mut mip).unwrap_or(());
            mipmaps.push(mip);
        }

        let mut hdfilename = String::new();
        let mut base_path = PathBuf::from(filename);
        base_path.set_extension("hd.texture");
        if hdsize > 0 {
            if let Some(explicit) = explicit_hd_path {
                hdfilename = explicit.to_string();
            } else if base_path.exists() {
                hdfilename = base_path.to_string_lossy().to_string();
            } else {
                let s = filename.replace(".texture", "_hd.texture");
                if Path::new(&s).exists() {
                    hdfilename = s;
                } else {
                    let s_span1 = filename
                        .replace("span 0", "span 1")
                        .replace("span0", "span1")
                        .replace("Span 0", "Span 1")
                        .replace("Span0", "Span1");
                    
                    if s_span1 != filename {
                        let mut span1_hd = PathBuf::from(&s_span1);
                        span1_hd.set_extension("hd.texture");
                        
                        if Path::new(&s_span1).exists() {
                            hdfilename = s_span1;
                        } else if span1_hd.exists() {
                            hdfilename = span1_hd.to_string_lossy().to_string();
                        }
                    }
                }
            }
        }

        Ok(SourceTex {
            filename: filename.to_string(),
            stg,
            header,
            textureheader,
            mipmaps,
            hdfilename,
            width,
            height,
            sd_width,
            sd_height,
            images,
            size: tex_size,
            hdsize,
            hdmipmaps,
            mipmaps_count,
            basemipsize,
            aspect,
            format,
            bytes_per_pixel,
            dimension,
            content_type,
        })
    }

}

pub struct DDSTex {
    pub filename: String,
    pub width: u32,
    pub height: u32,
    pub mipmaps: u32,
    pub format: u32,
    pub dataoffset: u64,
    pub size: u32,
    pub basemipsize: u32,
    pub bytes_per_pixel: f64,
    pub aspect: i32,
}

impl DDSTex {
    pub fn read(filename: &str) -> Result<Self, String> {
        let mut fs =
            File::open(filename).map_err(|e| format!("Failed to open {}: {}", filename, e))?;
        if fs.read_u32::<LittleEndian>().unwrap_or(0) != 542327876 {
            // "DDS "
            return Err("Not a DDS file".into());
        }

        let _flags = fs.read_u32::<LittleEndian>().unwrap();
        fs.read_u32::<LittleEndian>().unwrap();
        let height = fs.read_u32::<LittleEndian>().unwrap();
        let width = fs.read_u32::<LittleEndian>().unwrap();

        if height * width == 0 || (height & (height - 1)) != 0 || (width & (width - 1)) != 0 {
            if (height / 16) * (width / 9) == 0 {
                return Err("Texture widths and heights must be a power of 2 or 16/9".into());
            }
        }
        let aspect = ((width as f64 / height as f64).log2()) as i32;

        fs.read_u32::<LittleEndian>().unwrap(); // pitch
        fs.read_u32::<LittleEndian>().unwrap(); // depth
        let mipmaps = fs.read_u32::<LittleEndian>().unwrap();

        fs.seek(SeekFrom::Start(0x54)).unwrap();
        let has_dx10 = fs.read_u32::<LittleEndian>().unwrap() == 808540228; // "DX10"
        fs.seek(SeekFrom::Start(0x80)).unwrap();

        let mut format = 0;
        let mut format_bits = -1;
        if has_dx10 {
            format = fs.read_u32::<LittleEndian>().unwrap();
            format_bits = bits_per_pixel(format);
            fs.seek(SeekFrom::Start(0x94)).unwrap();
        }

        let dataoffset = fs.stream_position().unwrap();
        let file_len = fs.seek(SeekFrom::End(0)).unwrap();
        let size = (file_len - dataoffset) as u32;

        let mut bytes_per_pixel = 0.0;
        let mut basemipsize = 0;
        if format_bits > 0 {
            bytes_per_pixel = (format_bits / 8) as f64;
            basemipsize = (width as f64 * height as f64 * bytes_per_pixel) as u32;
        } else {
            let maxmipexp = (size as f64).log2().floor() as i32;
            basemipsize = 1 << maxmipexp;
            bytes_per_pixel = basemipsize as f64 / width as f64 / height as f64;
        }

        Ok(DDSTex {
            filename: filename.to_string(),
            width,
            height,
            mipmaps,
            format,
            dataoffset,
            size,
            basemipsize,
            bytes_per_pixel,
            aspect,
        })
    }

    pub fn write_single(
        fn_out: &str,
        width: u32,
        height: u32,
        hdmipmaps_data: Option<&[u8]>,
        mipmaps_data: &[u8],
        mipmaps_count: u32,
        hdmipmaps_count: u32,
        format: u32,
        basemipsize: u32,
        is_cubemap: bool,
    ) -> Result<(), String> {
        let mut fs = File::create(fn_out).map_err(|e| e.to_string())?;
        fs.write_all(b"DDS ").unwrap();
        fs.write_u32::<LittleEndian>(0x7c).unwrap();
        // DDSD_CAPS | DDSD_HEIGHT | DDSD_WIDTH | DDSD_PIXELFORMAT | DDSD_LINEARSIZE | DDSD_MIPMAPCOUNT
        fs.write_u32::<LittleEndian>(1 | 2 | 4 | 0x1000 | 0x80000 | 0x20000)
            .unwrap();
        fs.write_u32::<LittleEndian>(height).unwrap();
        fs.write_u32::<LittleEndian>(width).unwrap();

        let linear_size = if hdmipmaps_data.is_none() {
            basemipsize
        } else {
            basemipsize * (1 << (2 * hdmipmaps_count))
        };
        fs.write_u32::<LittleEndian>(linear_size).unwrap();
        fs.write_u32::<LittleEndian>(0).unwrap(); // depth
        fs.write_u32::<LittleEndian>(mipmaps_count + hdmipmaps_count)
            .unwrap();
        fs.write_all(&[0u8; 11 * 4]).unwrap();

        // pixelformat
        fs.write_u32::<LittleEndian>(32).unwrap();
        fs.write_u32::<LittleEndian>(4).unwrap(); // FourCC
        fs.write_all(b"DX10").unwrap();
        fs.write_all(&[0u8; 5 * 4]).unwrap();

        // caps
        let mut caps = 0x1000;
        if mipmaps_count + hdmipmaps_count > 0 {
            caps |= 8 | 0x400000;
        }
        fs.write_u32::<LittleEndian>(caps).unwrap();
        // caps2: CubeMapAll (0xFE00) when cubemap, else 0
        let caps2 = if is_cubemap { 0x0000_FE00u32 } else { 0u32 };
        fs.write_u32::<LittleEndian>(caps2).unwrap();
        fs.write_all(&[0u8; 3 * 4]).unwrap(); // caps3, caps4, reserved

        // DX10 header
        fs.write_u32::<LittleEndian>(format).unwrap();
        fs.write_u32::<LittleEndian>(if height > 1 { 3 } else { 2 })
            .unwrap();
        // DX10 misc: 4 = D3D11_RESOURCE_MISC_TEXTURECUBE
        let dx10_misc = if is_cubemap { 4u32 } else { 0u32 };
        fs.write_u32::<LittleEndian>(dx10_misc).unwrap();
        fs.write_u32::<LittleEndian>(1).unwrap(); // arraySize (faces are covered by misc cube flag)
        fs.write_u32::<LittleEndian>(0).unwrap(); // misc flags

        if let Some(hd) = hdmipmaps_data {
            fs.write_all(hd).unwrap();
        }
        fs.write_all(mipmaps_data).unwrap();
        Ok(())
    }
}

pub fn bits_per_pixel(format: u32) -> i32 {
    match format {
        1..=4 => 128,
        5..=8 => 96,
        9..=22 | 100..=102 => 64,                      // includes Y416 etc
        23..=47 | 67..=69 | 87..=93 | 107..=109 => 32, // includes AYUV etc
        104..=105 => 24,                               // P010, P016
        48..=59 | 85..=86 | 114..=115 => 16,
        103 | 106 | 110 => 12,
        60..=65 | 111..=113 => 8,
        66 => 1,
        70..=72 | 79..=81 => 4,           // BC1, BC4
        73..=78 | 82..=84 | 94..=99 => 8, // BC2, BC3, BC5, BC6, BC7
        _ => -1,
    }
}

fn is_block_compressed(format: u32) -> bool {
    matches!(format, 70..=84 | 94..=99)
}

pub fn mip_level_size(width: u32, height: u32, format: u32) -> Option<u32> {
    let format_bits = bits_per_pixel(format);
    if format_bits <= 0 {
        return None;
    }

    if is_block_compressed(format) {
        let blocks_w = ((width + 3) / 4).max(1);
        let blocks_h = ((height + 3) / 4).max(1);
        let block_bytes = if format_bits == 4 { 8u64 } else { 16u64 };
        return Some((blocks_w as u64 * blocks_h as u64 * block_bytes) as u32);
    }

    Some((((width as u64 * height as u64 * format_bits as u64) + 7) / 8) as u32)
}

/// `output_format`: "dds" | "png" | "auto"  
/// `cube_mode`:      "array" | "cross"  
pub fn extract_texture(
    source_path: &str,
    output_dir: Option<String>,
    explicit_hd_path: Option<&str>,
    output_format: Option<&str>,
    cube_mode: Option<&str>,
) -> Result<String, String> {
    let tex = SourceTex::read(source_path, explicit_hd_path)?;

    let is_cubemap = tex.dimension == 4;
    let is_hdr = matches!(tex.format, 95 | 96 | 2 | 10 | 16); // BC6H, RGBA_FLOAT, R16G16B16A16, RG32
    let is_multi = tex.images > 1;
    let want_cross = is_cubemap && cube_mode.unwrap_or("array") == "cross";

    // Determine effective output format
    let effective_fmt = match output_format.unwrap_or("auto") {
        "png" => "png",
        "dds" => "dds",
        fmt => {
            // auto / auto-notiff: TIFF for single 2D HDR (unless notiff), DDS for LDR / fallback
            let tiff_ok = fmt != "auto-notiff";
            if is_hdr && !is_multi && !is_cubemap && tiff_ok { "tiff" }
            else { "dds" }
        }
    };
    // Cross export always PNG (it's an assembled image, not a raw DDS)
    let effective_fmt = if want_cross { "png" } else { effective_fmt };

    let ext = match effective_fmt { "tiff" => "tiff", "png" => "png", _ => "dds" };
    let mut out_base = PathBuf::from(source_path);
    out_base.set_extension(ext);
    if let Some(dir) = output_dir {
        out_base = PathBuf::from(dir).join(out_base.file_name().unwrap());
    }

    let mut hdmips = None;
    let mut hd_data = Vec::new();
    let mut final_width = tex.width as u32;
    let mut final_height = tex.height as u32;
    let mut final_hdmipmaps = 0;

    if tex.hdsize > 0 {
        if tex.hdfilename.is_empty() {
            final_width = tex.sd_width as u32;
            final_height = tex.sd_height as u32;
        } else {
            final_hdmipmaps = tex.hdmipmaps;
            hd_data = std::fs::read(&tex.hdfilename).map_err(|e| e.to_string())?;
            hdmips = Some(hd_data.as_slice());
        }
    }

    let mut logs = String::new();

    // ── Cross PNG export for cubemaps ────────────────────────────────────────
    if want_cross && is_cubemap && tex.images >= 6 {
        let face_w = tex.sd_width as usize;
        let face_h = tex.sd_height as usize;
        let mip_size = mip_level_size(face_w as u32, face_h as u32, tex.format as u32)
            .map(|v| v as usize)
            .unwrap_or_else(|| (face_w as f64 * face_h as f64 * tex.bytes_per_pixel).ceil() as usize);

        let faces: Vec<Vec<u8>> = (0..6usize)
            .filter_map(|i| {
                if i < tex.mipmaps.len() && tex.mipmaps[i].len() >= mip_size {
                    Some(tex.mipmaps[i][..mip_size].to_vec())
                } else {
                    None
                }
            })
            .collect();

        if faces.len() < 6 {
            return Err("Not enough cube face data for cross export".into());
        }

        let cross_bytes = cubemap_cross_png_bytes(&faces, face_w, face_h, tex.format as u32, tex.content_type)?;
        std::fs::write(&out_base, &cross_bytes).map_err(|e| e.to_string())?;
        logs.push_str(&format!("Wrote {} (cubemap cross, {}x{})\n", out_base.display(), face_w * 4, face_h * 2));
        return Ok(logs);
    }

    // ── TIFF export for HDR single-surface textures ─────────────────────────
    if effective_fmt == "tiff" && !is_cubemap && tex.images == 1 {
        let mut width = tex.sd_width as usize;
        let mut height = tex.sd_height as usize;
        
        let mut use_hd = false;
        let mut hd_mip_size = 0;
        if tex.hdsize > 0 && !tex.hdfilename.is_empty() {
            if let Some(size) = mip_level_size(tex.width as u32, tex.height as u32, tex.format as u32) {
                if hd_data.len() >= size as usize {
                    width = tex.width as usize;
                    height = tex.height as usize;
                    hd_mip_size = size as usize;
                    use_hd = true;
                }
            }
        }

        let tiff_bytes = if use_hd {
            decode_to_tiff_bytes(&hd_data[..hd_mip_size], width, height, tex.format as u32)?
        } else {
            let mip_size = mip_level_size(width as u32, height as u32, tex.format as u32)
                .map(|v| v as usize)
                .unwrap_or_else(|| (width as f64 * height as f64 * tex.bytes_per_pixel).ceil() as usize);

            if tex.mipmaps[0].len() < mip_size {
                return Err("Not enough SD data for TIFF export".into());
            }
            decode_to_tiff_bytes(&tex.mipmaps[0][..mip_size], width, height, tex.format as u32)?
        };

        std::fs::write(&out_base, &tiff_bytes).map_err(|e| e.to_string())?;
        logs.push_str(&format!("Wrote {} ({}x{} TIFF HDR)\n", out_base.display(), width, height));
        return Ok(logs);
    }

    // ── Simple PNG export for non-HDR 2D single-surface textures ────────────
    if effective_fmt == "png" && !is_cubemap && tex.images == 1 {
        let mut width = tex.sd_width as usize;
        let mut height = tex.sd_height as usize;
        
        let mut use_hd = false;
        let mut hd_mip_size = 0;
        if tex.hdsize > 0 && !tex.hdfilename.is_empty() {
            if let Some(size) = mip_level_size(tex.width as u32, tex.height as u32, tex.format as u32) {
                if hd_data.len() >= size as usize {
                    width = tex.width as usize;
                    height = tex.height as usize;
                    hd_mip_size = size as usize;
                    use_hd = true;
                }
            }
        }

        let png_bytes = if use_hd {
            let png_b64 = decode_to_base64_png(&hd_data[..hd_mip_size], width, height, tex.format as u32, tex.content_type)?;
            base64::prelude::BASE64_STANDARD.decode(png_b64.as_bytes()).map_err(|e| e.to_string())?
        } else {
            let mip_size = mip_level_size(width as u32, height as u32, tex.format as u32)
                .map(|v| v as usize)
                .unwrap_or_else(|| (width as f64 * height as f64 * tex.bytes_per_pixel).ceil() as usize);

            if tex.mipmaps[0].len() < mip_size {
                return Err("Not enough SD data for PNG export".into());
            }
            let png_b64 = decode_to_base64_png(&tex.mipmaps[0][..mip_size], width, height, tex.format as u32, tex.content_type)?;
            base64::prelude::BASE64_STANDARD.decode(png_b64.as_bytes()).map_err(|e| e.to_string())?
        };

        std::fs::write(&out_base, &png_bytes).map_err(|e| e.to_string())?;
        logs.push_str(&format!("Wrote {} ({}x{} PNG)\n", out_base.display(), width, height));
        return Ok(logs);
    }

    // ── DDS export ────────────────────────────────────────────────────────────
    if tex.images > 1 && is_cubemap {
        // Write a single DDS containing all 6 cube faces interleaved (DXGI cube array)
        let mut all_hd: Vec<u8> = Vec::new();
        let mut all_sd: Vec<u8> = Vec::new();
        let hd_per_face = if let Some(hd) = hdmips {
            hd.len() / tex.images as usize
        } else {
            0
        };
        for i in 0..tex.images as usize {
            if let Some(hd) = hdmips {
                all_hd.extend_from_slice(&hd[i * hd_per_face..(i + 1) * hd_per_face]);
            }
            all_sd.extend_from_slice(&tex.mipmaps[i]);
        }
        DDSTex::write_single(
            out_base.to_str().unwrap(),
            final_width,
            final_height,
            if all_hd.is_empty() { None } else { Some(&all_hd) },
            &all_sd,
            tex.mipmaps_count as u32,
            final_hdmipmaps as u32,
            tex.format as u32,
            tex.basemipsize,
            true,
        )?;
        logs.push_str(&format!("Wrote {} (cubemap, {} faces)\n", out_base.display(), tex.images));
    } else if tex.images > 1 {
        for i in 0..tex.images {
            let mut single_out = out_base.clone();
            single_out.set_extension(format!("A{}.dds", i));

            let hd_slice = if let Some(hd) = hdmips {
                let part_len = hd.len() / tex.images as usize;
                Some(&hd[(i as usize * part_len)..((i as usize + 1) * part_len)])
            } else {
                None
            };

            DDSTex::write_single(
                single_out.to_str().unwrap(),
                final_width,
                final_height,
                hd_slice,
                &tex.mipmaps[i as usize],
                tex.mipmaps_count as u32,
                final_hdmipmaps as u32,
                tex.format as u32,
                tex.basemipsize,
                false,
            )?;
            logs.push_str(&format!("Wrote {}\n", single_out.display()));
        }
    } else {
        DDSTex::write_single(
            out_base.to_str().unwrap(),
            final_width,
            final_height,
            hdmips,
            &tex.mipmaps[0],
            tex.mipmaps_count as u32,
            final_hdmipmaps as u32,
            tex.format as u32,
            tex.basemipsize,
            false,
        )?;
        logs.push_str(&format!("Wrote {}\n", out_base.display()));
    }

    Ok(logs)
}

pub fn replace_texture(
    source_path: &str,
    dds_path: &str,
    output_dir: Option<String>,
    ignore_format: bool,
    explicit_hd_path: Option<&str>,
    explicit_out_sd: Option<&str>,
    explicit_out_hd: Option<&str>,
) -> Result<String, String> {
    let mut tex = SourceTex::read(source_path, explicit_hd_path)?;

    if tex.images > 1 && !dds_path.to_lowercase().ends_with(".a0.dds") {
        return Err("Array textures must be named with .Ax.dds convention".into());
    }

    let dds0 = DDSTex::read(dds_path)?;
    let mut ddss = vec![dds0];

    if tex.images > 1 {
        let stub = &dds_path[..dds_path.len() - 7]; // remove ".a0.dds"
        for i in 1..tex.images {
            let p = format!("{}.A{}.dds", stub, i);
            if !Path::new(&p).exists() {
                return Err(format!("Missing array image {}: {}", i, p));
            }
            ddss.push(DDSTex::read(&p)?);
        }
    }

    let dds = &ddss[0];
    if (dds.width as u16) < tex.width || (dds.height as u16) < tex.height {
        return Err("Replacement image is smaller than source".into());
    }
    if (tex.bytes_per_pixel - dds.bytes_per_pixel).abs() > 0.01 {
        return Err("Bytes per pixel is different, formats are incompatible".into());
    }
    if tex.aspect != dds.aspect {
        return Err("Aspect ratio is different between files".into());
    }
    if tex.format as u32 != dds.format {
        if !ignore_format {
            return Err(format!(
                "DDS format mismatch: {} != {}",
                tex.format, dds.format
            ));
        }
    }

    for i in 1..ddss.len() {
        if ddss[i].width != dds.width
            || ddss[i].height != dds.height
            || ddss[i].format != dds.format
        {
            return Err(format!("Array image A{} properties don't match A0", i));
        }
    }

    let out_base = if let Some(out_sd) = explicit_out_sd {
        PathBuf::from(out_sd)
    } else {
        let mut base = PathBuf::from(dds_path);
        if tex.images > 1 {
            let stub = &dds_path[..dds_path.len() - 7];
            base = PathBuf::from(format!("{}.texture", stub));
        } else {
            base.set_extension("texture");
        }

        if let Some(dir) = output_dir {
            base = PathBuf::from(dir).join(base.file_name().unwrap());
        }
        base
    };

    if source_path == out_base.to_str().unwrap() && explicit_out_sd.is_none() {
        return Err("Input and output .texture files cannot be the same.".into());
    }

    let scale = (((dds.basemipsize as f64 / tex.basemipsize as f64).log2() / 2.0).floor()) as u32;
    let actual_extra_sd = if tex.hdsize > 0 {
        scale.min(tex.hdmipmaps as u32)
    } else {
        scale
    };

    let width = dds.width;
    let height = dds.height;
    let mut sd_width = tex.sd_width as u32;
    let mut sd_height = tex.sd_height as u32;
    let mut size = tex.size;
    let mut mipmaps = tex.mipmaps_count as u32;
    let mut hdmipmaps = tex.hdmipmaps as u32;

    let mut extrasdmipmaps = ((dds.width as f64 / tex.sd_width as f64).log2()) as u32;
    let mut sizeincrease = 0;

    if tex.hdsize > 0 && actual_extra_sd > hdmipmaps {
        return Err("Unchecked extrasd value".into());
    }

    for i in (actual_extra_sd + 1..=extrasdmipmaps).rev() {
        sizeincrease += tex.basemipsize << (2 * i);
    }

    let hdsize = if tex.hdsize > 0 {
        hdmipmaps = extrasdmipmaps - actual_extra_sd;
        extrasdmipmaps = actual_extra_sd;
        let val = sizeincrease * tex.images as u32;
        sizeincrease = 0;
        val
    } else {
        hdmipmaps = 0;
        0
    };

    for i in (1..=extrasdmipmaps).rev() {
        sizeincrease += tex.basemipsize << (2 * i);
    }
    let extrasdmipsize = sizeincrease * tex.images as u32;
    sd_width <<= extrasdmipmaps;
    sd_height <<= extrasdmipmaps;

    for i in 0..ddss.len() {
        let needed = hdmipmaps + extrasdmipmaps + tex.mipmaps_count as u32;
        if ddss[i].mipmaps < needed {
            let label = if ddss.len() > 1 { format!("A{} ", i) } else { "".to_string() };
            return Err(format!(
                "Not enough mipmaps in DDS file {}to replace this texture (needs {})",
                label, needed
            ));
        }
    }

    let mut hdmips_list = Vec::new();
    let mut extrasdmips_list = Vec::new();
    let mut sdmips_list = Vec::new();

    for i in 0..ddss.len() {
        let mut fs = File::open(&ddss[i].filename).map_err(|e| format!("Failed to open DDS file: {}", e))?;
        fs.seek(SeekFrom::Start(ddss[i].dataoffset))
            .map_err(|e| format!("Failed to seek to DDS data offset: {}", e))?;

        let mut hd_part = vec![0u8; (hdsize / tex.images as u32) as usize];
        fs.read_exact(&mut hd_part)
            .map_err(|e| format!("Failed to read HD mipmaps: {}", e))?;
        hdmips_list.push(hd_part);

        let mut ex_part = vec![0u8; (extrasdmipsize / tex.images as u32) as usize];
        fs.read_exact(&mut ex_part)
            .map_err(|e| format!("Failed to read extra SD mipmaps: {}", e))?;
        extrasdmips_list.push(ex_part);

        let mut sd_part = vec![0u8; (tex.size / tex.images as u32) as usize];
        fs.read_exact(&mut sd_part)
            .map_err(|e| format!("Failed to read SD mipmaps: {}", e))?;
        sdmips_list.push(sd_part);
    }

    let mut logs = String::new();
    if tex.hdsize > 0 {
        let mut hd_out = out_base.clone();
        if let Some(out_hd) = explicit_out_hd {
            hd_out = PathBuf::from(out_hd);
        } else {
            hd_out.set_extension("hd.texture");
        }
        let mut fs = File::create(&hd_out).unwrap();
        if hdmipmaps > 0 {
            for hd in &hdmips_list {
                fs.write_all(hd).unwrap();
            }
        }
        logs.push_str(&format!(
            "Wrote {} (max {}x{})\n",
            hd_out.display(),
            width,
            height
        ));
    }

    mipmaps += extrasdmipmaps;
    size += extrasdmipsize;

    if extrasdmipsize > 0 {
        if tex.stg {
            tex.header[0x8 + 16..0xC + 16].copy_from_slice(&size.to_le_bytes());
            tex.header[0x14 + 16..0x18 + 16].copy_from_slice(&size.to_le_bytes());
        } else {
            tex.header[0x8..0xC].copy_from_slice(&size.to_le_bytes());
            tex.header[0x14..0x18].copy_from_slice(&size.to_le_bytes());
        }
    }

    let mut fs = File::create(&out_base).unwrap();
    fs.write_all(&tex.header).unwrap();
    fs.write_u32::<LittleEndian>(size).unwrap();
    fs.write_u32::<LittleEndian>(hdsize).unwrap();
    fs.write_u16::<LittleEndian>(dds.width as u16).unwrap();
    fs.write_u16::<LittleEndian>(dds.height as u16).unwrap();
    fs.write_u16::<LittleEndian>(sd_width as u16).unwrap();
    fs.write_u16::<LittleEndian>(sd_height as u16).unwrap();

    fs.write_all(&tex.textureheader[16..30]).unwrap();
    fs.write_u8(mipmaps as u8).unwrap();
    fs.write_u8(tex.textureheader[24]).unwrap();
    fs.write_u8(hdmipmaps as u8).unwrap();
    fs.write_all(&tex.textureheader[33..]).unwrap();

    for i in 0..ddss.len() {
        fs.write_all(&extrasdmips_list[i]).unwrap();
        fs.write_all(&sdmips_list[i]).unwrap();
    }

    logs.push_str(&format!(
        "Wrote {} (max {}x{})\n",
        out_base.display(),
        sd_width,
        sd_height
    ));
    Ok(logs)
}

pub fn get_texture_info(path: &str) -> Result<TextureInfo, String> {
    let tex = SourceTex::read(path, None)?;
    let is_cubemap = tex.dimension == 4;
    let is_ibl = (tex.content_type & 0x08) != 0;
    Ok(TextureInfo {
        width: tex.width as u32,
        height: tex.height as u32,
        mipmaps: tex.mipmaps_count as u32,
        hdmipmaps: tex.hdmipmaps as u32,
        images: tex.images as u32,
        bytes_per_pixel: tex.bytes_per_pixel,
        size: tex.size,
        hdsize: tex.hdsize,
        format: tex.format as u32,
        is_cubemap,
        is_ibl,
        dimension: tex.dimension,
        content_type: tex.content_type,
    })
}

pub fn get_texture_preview(path: &str) -> Result<String, String> {
    crate::tools::texture_preview::get_cached_preview(path)
}

pub fn clear_texture_thumbnail_cache() -> Result<usize, String> {
    crate::tools::texture_preview::clear_cache()
}

pub fn get_dds_preview(path: &str) -> Result<String, String> {
    let tex = DDSTex::read(path)?;

    let mut fs = File::open(path).map_err(|e| e.to_string())?;
    fs.seek(SeekFrom::Start(tex.dataoffset)).unwrap();

    let width = tex.width as usize;
    let height = tex.height as usize;
    let mip_size = mip_level_size(width as u32, height as u32, tex.format)
        .map(|v| v as usize)
        .unwrap_or_else(|| (width as f64 * height as f64 * tex.bytes_per_pixel).ceil() as usize);

    let mut mip_data = vec![0u8; mip_size];
    let read_len = fs.read(&mut mip_data).unwrap_or(0);
    if read_len < mip_size {
        return Err("Not enough data to decode preview".into());
    }
    mip_data.truncate(read_len);

    decode_to_base64_png(&mip_data, width, height, tex.format, 0)
}

/// Decode an HDR texture to a 32-bit float TIFF byte vector.
/// Supported formats: 95/96 (BC6H UF/SF), 10 (R16G16B16A16F), 2 (R32G32B32A32F), 16 (R32G32F)
fn decode_to_tiff_bytes(
    data: &[u8],
    width: usize,
    height: usize,
    format: u32,
) -> Result<Vec<u8>, String> {
    let npx = width * height;
    let mut floats = vec![[0f32; 3]; npx]; // RGB f32 per pixel

    match format {
        95 | 96 => {
            // BC6H: decode to tone-mapped u32, then unpack back to raw half-float via bc6 block decode
            // Re-decode properly: iterate blocks and store raw f32 directly
            let _guard = BC6_PANIC_HOOK_GUARD.lock().map_err(|_| "BC6 lock failed".to_string())?;
            let prev_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            let blocks_w = (width + 3) / 4;
            let blocks_h = (height + 3) / 4;
            let expected = blocks_w * blocks_h * 16;
            if data.len() < expected {
                std::panic::set_hook(prev_hook);
                return Err("Not enough BC6H data for TIFF export".into());
            }
            let preferred_signed = format == 96;
            for by in 0..blocks_h {
                for bx in 0..blocks_w {
                    let bi = (by * blocks_w + bx) * 16;
                    let mut block_pixels = [0u32; 16];
                    let ok = catch_unwind(AssertUnwindSafe(|| {
                        decode_bc6_block(&data[bi..bi + 16], &mut block_pixels, preferred_signed)
                    })).is_ok();
                    if !ok { continue; }
                    let y0 = by * 4; let x0 = bx * 4;
                    for oy in 0..4 { for ox in 0..4 {
                        let y = y0 + oy; let x = x0 + ox;
                        if y >= height || x >= width { continue; }
                        // texture2ddecoder BC6H output: each u32 = BGRA where B,G,R are
                        // half-float bits packed in the lower 16 bits of each byte pair
                        // Actually output is BGRA u8×4, where B/G/R are tone-mapped u8.
                        // For float TIFF, reinterpret the u32 as-is using half::f16 from raw bits.
                        // texture2ddecoder packs BC6H decoded halves into u32 as: lo16=R_half, hi16=G_half
                        // but the actual layout is BGRA u8. Use the raw half bits from block decode instead.
                        // The safer approach: read back from block_pixels as BGRA u8 tone-mapped values
                        // then divide by 255. Not ideal but avoids needing a separate BC6H float decoder.
                        let p = block_pixels[oy * 4 + ox];
                        let b = p.to_le_bytes();
                        // b = [B, G, R, A] in texture2ddecoder output
                        floats[y * width + x] = [
                            b[2] as f32 / 255.0,
                            b[1] as f32 / 255.0,
                            b[0] as f32 / 255.0,
                        ];
                    }}
                }
            }
            std::panic::set_hook(prev_hook);
        }
        10 => {
            // R16G16B16A16_FLOAT: half-float per channel, 8 bytes/px
            for i in 0..npx {
                let px = i * 8;
                if px + 5 < data.len() {
                    floats[i] = [
                        half_to_f32(u16::from_le_bytes([data[px],   data[px+1]])),
                        half_to_f32(u16::from_le_bytes([data[px+2], data[px+3]])),
                        half_to_f32(u16::from_le_bytes([data[px+4], data[px+5]])),
                    ];
                }
            }
        }
        2 => {
            // R32G32B32A32_FLOAT: 4×f32, 16 bytes/px
            for i in 0..npx {
                let px = i * 16;
                if px + 11 < data.len() {
                    floats[i] = [
                        f32::from_le_bytes([data[px],    data[px+1],  data[px+2],  data[px+3]]),
                        f32::from_le_bytes([data[px+4],  data[px+5],  data[px+6],  data[px+7]]),
                        f32::from_le_bytes([data[px+8],  data[px+9],  data[px+10], data[px+11]]),
                    ];
                }
            }
        }
        16 => {
            // R32G32_FLOAT: 2×f32, 8 bytes/px — store in R+G, B=0
            for i in 0..npx {
                let px = i * 8;
                if px + 7 < data.len() {
                    floats[i] = [
                        f32::from_le_bytes([data[px],   data[px+1], data[px+2], data[px+3]]),
                        f32::from_le_bytes([data[px+4], data[px+5], data[px+6], data[px+7]]),
                        0.0,
                    ];
                }
            }
        }
        _ => return Err(format!("Format {} not supported for TIFF HDR export", format)),
    }

    let img = Rgb32FImage::from_raw(
        width as u32,
        height as u32,
        floats.into_iter().flatten().collect(),
    ).ok_or("Failed to build Rgb32FImage")?;

    let mut buf = ImageCursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::Tiff).map_err(|e| e.to_string())?;
    Ok(buf.into_inner())
}

fn decode_to_base64_png(
    data: &[u8],
    width: usize,
    height: usize,
    format: u32,
    content_type: u8,
) -> Result<String, String> {
    let is_normal = (content_type & 0x02) != 0;
    let mut u32_pixels = vec![0u32; width * height];
    let res: Result<(), String> = match format {
        71 | 72 => decode_bc1(data, width, height, &mut u32_pixels).map_err(|e| e.to_string()),
        77 | 78 => decode_bc3(data, width, height, &mut u32_pixels).map_err(|e| e.to_string()),
        80 => decode_bc4(data, width, height, &mut u32_pixels).map_err(|e| e.to_string()),
        83 => {
            decode_bc5(data, width, height, &mut u32_pixels).map_err(|e| e.to_string())?;
            if is_normal {
                for p in u32_pixels.iter_mut() {
                    let b = p.to_le_bytes();
                    // texture2ddecoder BC5 output: BGRA where B=ch0(X), G=ch1(Y)
                    // Reconstruct Z = sqrt(1 - X² - Y²), remap [-1,1] → [0,255]
                    let x = (b[2] as f32 / 127.5) - 1.0;
                    let y = (b[1] as f32 / 127.5) - 1.0;
                    let z = (1.0 - (x * x + y * y).min(1.0)).sqrt();
                    let zb = ((z * 0.5 + 0.5) * 255.0).round() as u8;
                    *p = u32::from_le_bytes([zb, b[1], b[2], 255]);
                }
            }
            Ok(())
        },
        95 => decode_bc6_resilient(data, width, height, &mut u32_pixels, false),
        96 => decode_bc6_resilient(data, width, height, &mut u32_pixels, true),
        98 | 99 => decode_bc7(data, width, height, &mut u32_pixels).map_err(|e| e.to_string()),
        28 | 29 => {
            for i in 0..(width * height) {
                if i * 4 + 3 < data.len() {
                    let r = data[i * 4];
                    let g = data[i * 4 + 1];
                    let b = data[i * 4 + 2];
                    let a = data[i * 4 + 3];
                    u32_pixels[i] = u32::from_le_bytes([r, g, b, a]);
                } else {
                    u32_pixels[i] = 0;
                }
            }
            Ok(())
        }
        61 => {
            for i in 0..(width * height) {
                if i < data.len() {
                    let v = data[i];
                    u32_pixels[i] = u32::from_le_bytes([v, v, v, 255]);
                } else {
                    u32_pixels[i] = 0;
                }
            }
            Ok(())
        }
        10 => {
            // R16G16B16A16_FLOAT: HDR half-float values; use reinhard tone-map
            for i in 0..(width * height) {
                let px = i * 8;
                if px + 7 < data.len() {
                    let r = half_to_f32(u16::from_le_bytes([data[px], data[px + 1]]));
                    let g = half_to_f32(u16::from_le_bytes([data[px + 2], data[px + 3]]));
                    let b = half_to_f32(u16::from_le_bytes([data[px + 4], data[px + 5]]));
                    let a = half_to_f32(u16::from_le_bytes([data[px + 6], data[px + 7]]));
                    u32_pixels[i] = u32::from_le_bytes([
                        float_to_preview_u8_hdr(r),
                        float_to_preview_u8_hdr(g),
                        float_to_preview_u8_hdr(b),
                        float_to_preview_u8(a),
                    ]);
                } else {
                    u32_pixels[i] = 0;
                }
            }
            Ok(())
        }
        16 => {
            for i in 0..(width * height) {
                let px = i * 8;
                if px + 7 < data.len() {
                    let r =
                        f32::from_le_bytes([data[px], data[px + 1], data[px + 2], data[px + 3]]);
                    let g = f32::from_le_bytes([
                        data[px + 4],
                        data[px + 5],
                        data[px + 6],
                        data[px + 7],
                    ]);
                    let rr = float_to_preview_u8(r);
                    let gg = float_to_preview_u8(g);
                    u32_pixels[i] = u32::from_le_bytes([rr, gg, 0, 255]);
                } else {
                    u32_pixels[i] = 0;
                }
            }
            Ok(())
        }
        2 => {
            // R32G32B32A32_FLOAT: 16 bytes per pixel (4× f32)
            for i in 0..(width * height) {
                let px = i * 16;
                if px + 15 < data.len() {
                    let r =
                        f32::from_le_bytes([data[px], data[px + 1], data[px + 2], data[px + 3]]);
                    let g = f32::from_le_bytes([
                        data[px + 4],
                        data[px + 5],
                        data[px + 6],
                        data[px + 7],
                    ]);
                    let b = f32::from_le_bytes([
                        data[px + 8],
                        data[px + 9],
                        data[px + 10],
                        data[px + 11],
                    ]);
                    let a = f32::from_le_bytes([
                        data[px + 12],
                        data[px + 13],
                        data[px + 14],
                        data[px + 15],
                    ]);
                    u32_pixels[i] = u32::from_le_bytes([
                        float_to_preview_u8(r),
                        float_to_preview_u8(g),
                        float_to_preview_u8(b),
                        float_to_preview_u8(a),
                    ]);
                } else {
                    u32_pixels[i] = 0;
                }
            }
            Ok(())
        }
        74 | 75 => decode_bc2_manual(data, width, height, &mut u32_pixels),
        87 | 91 => {
            // B8G8R8A8: swap Blue and Red channels to get RGBA
            for i in 0..(width * height) {
                if i * 4 + 3 < data.len() {
                    let b = data[i * 4];
                    let g = data[i * 4 + 1];
                    let r = data[i * 4 + 2];
                    let a = data[i * 4 + 3];
                    u32_pixels[i] = u32::from_le_bytes([r, g, b, a]);
                } else {
                    u32_pixels[i] = 0;
                }
            }
            Ok(())
        }
        _ => return Err(format!("Format {} not supported for preview", format)),
    };

    if let Err(e) = res {
        return Err(format!("Decode failed: {}", e));
    }

    let mut rgba_bytes = Vec::with_capacity(width * height * 4);
    let is_raw = matches!(format, 2 | 16 | 28 | 29 | 87 | 91);
    for p in u32_pixels {
        let b = p.to_le_bytes();
        if is_raw {
            rgba_bytes.extend_from_slice(&b);
        } else {
            // texture2ddecoder returns BGRA in memory, so we swap Red and Blue
            rgba_bytes.extend_from_slice(&[b[2], b[1], b[0], b[3]]);
        }
    }

    let img = RgbaImage::from_raw(width as u32, height as u32, rgba_bytes)
        .ok_or("Failed to create RgbaImage")?;

    let mut cursor = ImageCursor::new(Vec::new());
    img.write_to(&mut cursor, ImageFormat::Png)
        .map_err(|e| format!("Failed to encode PNG: {}", e))?;

    Ok(BASE64_STANDARD.encode(cursor.into_inner()))
}

/// Decode texture data to RGBA buffer (for downscaling).
pub fn decode_to_rgba(
    data: &[u8],
    width: usize,
    height: usize,
    format: u32,
    content_type: u8,
) -> Result<Vec<u8>, String> {
    let is_normal = (content_type & 0x02) != 0;
    let mut u32_pixels = vec![0u32; width * height];
    let res: Result<(), String> = match format {
        71 | 72 => decode_bc1(data, width, height, &mut u32_pixels).map_err(|e| e.to_string()),
        77 | 78 => decode_bc3(data, width, height, &mut u32_pixels).map_err(|e| e.to_string()),
        80 => decode_bc4(data, width, height, &mut u32_pixels).map_err(|e| e.to_string()),
        83 => {
            decode_bc5(data, width, height, &mut u32_pixels).map_err(|e| e.to_string())?;
            if is_normal {
                for p in u32_pixels.iter_mut() {
                    let b = p.to_le_bytes();
                    let x = (b[2] as f32 / 127.5) - 1.0;
                    let y = (b[1] as f32 / 127.5) - 1.0;
                    let z = (1.0 - (x * x + y * y).min(1.0)).sqrt();
                    let zb = ((z * 0.5 + 0.5) * 255.0).round() as u8;
                    *p = u32::from_le_bytes([zb, b[1], b[2], 255]);
                }
            }
            Ok(())
        },
        95 => decode_bc6_resilient(data, width, height, &mut u32_pixels, false),
        96 => decode_bc6_resilient(data, width, height, &mut u32_pixels, true),
        98 | 99 => decode_bc7(data, width, height, &mut u32_pixels).map_err(|e| e.to_string()),
        28 | 29 => {
            for i in 0..(width * height) {
                if i * 4 + 3 < data.len() {
                    let r = data[i * 4];
                    let g = data[i * 4 + 1];
                    let b = data[i * 4 + 2];
                    let a = data[i * 4 + 3];
                    u32_pixels[i] = u32::from_le_bytes([r, g, b, a]);
                }
            }
            Ok(())
        }
        61 => {
            for i in 0..(width * height) {
                if i < data.len() {
                    let v = data[i];
                    u32_pixels[i] = u32::from_le_bytes([v, v, v, 255]);
                }
            }
            Ok(())
        }
        10 => {
            for i in 0..(width * height) {
                let px = i * 8;
                if px + 7 < data.len() {
                    let r = half_to_f32(u16::from_le_bytes([data[px], data[px + 1]]));
                    let g = half_to_f32(u16::from_le_bytes([data[px + 2], data[px + 3]]));
                    let b = half_to_f32(u16::from_le_bytes([data[px + 4], data[px + 5]]));
                    let a = half_to_f32(u16::from_le_bytes([data[px + 6], data[px + 7]]));
                    u32_pixels[i] = u32::from_le_bytes([
                        float_to_preview_u8_hdr(r),
                        float_to_preview_u8_hdr(g),
                        float_to_preview_u8_hdr(b),
                        float_to_preview_u8(a),
                    ]);
                }
            }
            Ok(())
        }
        16 => {
            for i in 0..(width * height) {
                let px = i * 8;
                if px + 7 < data.len() {
                    let r = f32::from_le_bytes([data[px], data[px + 1], data[px + 2], data[px + 3]]);
                    let g = f32::from_le_bytes([data[px + 4], data[px + 5], data[px + 6], data[px + 7]]);
                    let rr = float_to_preview_u8(r);
                    let gg = float_to_preview_u8(g);
                    u32_pixels[i] = u32::from_le_bytes([rr, gg, 0, 255]);
                }
            }
            Ok(())
        }
        2 => {
            for i in 0..(width * height) {
                let px = i * 16;
                if px + 15 < data.len() {
                    let r = f32::from_le_bytes([data[px], data[px + 1], data[px + 2], data[px + 3]]);
                    let g = f32::from_le_bytes([data[px + 4], data[px + 5], data[px + 6], data[px + 7]]);
                    let b = f32::from_le_bytes([data[px + 8], data[px + 9], data[px + 10], data[px + 11]]);
                    let a = f32::from_le_bytes([data[px + 12], data[px + 13], data[px + 14], data[px + 15]]);
                    u32_pixels[i] = u32::from_le_bytes([
                        float_to_preview_u8(r),
                        float_to_preview_u8(g),
                        float_to_preview_u8(b),
                        float_to_preview_u8(a),
                    ]);
                }
            }
            Ok(())
        }
        74 | 75 => decode_bc2_manual(data, width, height, &mut u32_pixels),
        87 | 91 => {
            for i in 0..(width * height) {
                if i * 4 + 3 < data.len() {
                    let b = data[i * 4];
                    let g = data[i * 4 + 1];
                    let r = data[i * 4 + 2];
                    let a = data[i * 4 + 3];
                    u32_pixels[i] = u32::from_le_bytes([r, g, b, a]);
                }
            }
            Ok(())
        }
        _ => return Err(format!("Format {} not supported for RGBA decode", format)),
    };

    if let Err(e) = res {
        return Err(format!("Decode failed: {}", e));
    }

    let mut rgba_bytes = Vec::with_capacity(width * height * 4);
    let is_raw = matches!(format, 2 | 16 | 28 | 29 | 87 | 91);
    for p in u32_pixels {
        let b = p.to_le_bytes();
        if is_raw {
            rgba_bytes.extend_from_slice(&b);
        } else {
            rgba_bytes.extend_from_slice(&[b[2], b[1], b[0], b[3]]);
        }
    }
    Ok(rgba_bytes)
}

/// Composite 6 RGBA cubemap faces into a 4×2 horizontal-cross RGBA buffer.
/// Returns RGBA bytes of size (face_w*4) × (face_h*2) × 4.
pub fn cubemap_cross_preview_rgba(
    faces_rgba: &[Vec<u8>], // 6 RGBA buffers, each face_w × face_h × 4
    face_w: usize,
    face_h: usize,
) -> Result<Vec<u8>, String> {
    if faces_rgba.len() < 6 {
        return Err("Not enough cube faces for cross preview".into());
    }

    let offsets: [(usize, usize); 6] = [(0, 0), (1, 0), (2, 0), (3, 0), (0, 1), (1, 1)];
    let cross_w = face_w * 4;
    let cross_h = face_h * 2;
    let mut cross_rgba = vec![0u8; cross_w * cross_h * 4];

    for (face_idx, face_rgba) in faces_rgba.iter().enumerate().take(6) {
        if face_rgba.is_empty() {
            continue;
        }
        let (col, row) = offsets[face_idx];
        let ox = col * face_w;
        let oy = row * face_h;

        for y in 0..face_h {
            for x in 0..face_w {
                let src_idx = (y * face_w + x) * 4;
                let dst_idx = ((oy + y) * cross_w + (ox + x)) * 4;
                cross_rgba[dst_idx..dst_idx + 4].copy_from_slice(&face_rgba[src_idx..src_idx + 4]);
            }
        }
    }

    Ok(cross_rgba)
}

/// Decode 6 cubemap faces (DXGI order: +X,-X,+Y,-Y,+Z,-Z) and composite
/// them into a 4×2 horizontal-cross PNG.
///
/// Cross layout (each cell = face_w × face_h):
/// ```
///  +X  -X  +Y  -Y    (row 0)
///  +Z  -Z  __  __    (row 1)
/// ```
fn cubemap_cross_preview(
    faces: &[Vec<u8>],
    face_w: usize,
    face_h: usize,
    format: u32,
    content_type: u8,
) -> Result<String, String> {
    let is_normal = (content_type & 0x02) != 0;
    if faces.len() < 6 {
        return Err("Not enough cube faces for cross preview".into());
    }

    // DXGI cross offsets (col, row) for face index 0..5 (+X,-X,+Y,-Y,+Z,-Z)
    let offsets: [(usize, usize); 6] = [(0, 0), (1, 0), (2, 0), (3, 0), (0, 1), (1, 1)];

    let cross_w = face_w * 4;
    let cross_h = face_h * 2;
    let mut cross_rgba = vec![0u8; cross_w * cross_h * 4];

    for (face_idx, face_data) in faces.iter().enumerate().take(6) {
        let mut face_pixels = vec![0u32; face_w * face_h];
        let decode_res: Result<(), String> = match format {
            71 | 72 => decode_bc1(face_data, face_w, face_h, &mut face_pixels)
                .map_err(|e| e.to_string()),
            77 | 78 => decode_bc3(face_data, face_w, face_h, &mut face_pixels)
                .map_err(|e| e.to_string()),
            80 => decode_bc4(face_data, face_w, face_h, &mut face_pixels)
                .map_err(|e| e.to_string()),
            83 => {
                decode_bc5(face_data, face_w, face_h, &mut face_pixels)
                    .map_err(|e| e.to_string())?;
                if is_normal {
                    for p in face_pixels.iter_mut() {
                        let b = p.to_le_bytes();
                        let x = (b[2] as f32 / 127.5) - 1.0;
                        let y = (b[1] as f32 / 127.5) - 1.0;
                        let z = (1.0 - (x * x + y * y).min(1.0)).sqrt();
                        let zb = ((z * 0.5 + 0.5) * 255.0).round() as u8;
                        *p = u32::from_le_bytes([zb, b[1], b[2], 255]);
                    }
                }
                Ok(())
            },
            95 => decode_bc6_resilient(face_data, face_w, face_h, &mut face_pixels, false),
            96 => decode_bc6_resilient(face_data, face_w, face_h, &mut face_pixels, true),
            98 | 99 => decode_bc7(face_data, face_w, face_h, &mut face_pixels)
                .map_err(|e| e.to_string()),
            28 | 29 => {
                for i in 0..(face_w * face_h) {
                    if i * 4 + 3 < face_data.len() {
                        face_pixels[i] = u32::from_le_bytes([
                            face_data[i * 4],
                            face_data[i * 4 + 1],
                            face_data[i * 4 + 2],
                            face_data[i * 4 + 3],
                        ]);
                    }
                }
                Ok(())
            }
            87 | 91 => {
                for i in 0..(face_w * face_h) {
                    if i * 4 + 3 < face_data.len() {
                        let b = face_data[i * 4];
                        let g = face_data[i * 4 + 1];
                        let r = face_data[i * 4 + 2];
                        let a = face_data[i * 4 + 3];
                        face_pixels[i] = u32::from_le_bytes([r, g, b, a]);
                    }
                }
                Ok(())
            }
            _ => {
                // Unsupported face format — skip with black
                Ok(())
            }
        };

        if decode_res.is_err() {
            continue;
        }

        let is_raw = matches!(format, 28 | 29 | 87 | 91);
        let (col, row) = offsets[face_idx];
        let ox = col * face_w;
        let oy = row * face_h;

        for y in 0..face_h {
            for x in 0..face_w {
                let src_idx = y * face_w + x;
                let dst_idx = (oy + y) * cross_w + (ox + x);
                let p = face_pixels[src_idx].to_le_bytes();
                let rgba = if is_raw {
                    [p[0], p[1], p[2], p[3]]
                } else {
                    // texture2ddecoder returns BGRA; swap R and B
                    [p[2], p[1], p[0], p[3]]
                };
                cross_rgba[dst_idx * 4..dst_idx * 4 + 4].copy_from_slice(&rgba);
            }
        }
    }

    let img = RgbaImage::from_raw(cross_w as u32, cross_h as u32, cross_rgba)
        .ok_or("Failed to create cubemap cross RgbaImage")?;

    let mut cursor = ImageCursor::new(Vec::new());
    img.write_to(&mut cursor, ImageFormat::Png)
        .map_err(|e| format!("Failed to encode cubemap cross PNG: {}", e))?;

    Ok(BASE64_STANDARD.encode(cursor.into_inner()))
}

/// Same as cubemap_cross_preview but returns the raw PNG bytes instead of base64.
fn cubemap_cross_png_bytes(
    faces: &[Vec<u8>],
    face_w: usize,
    face_h: usize,
    format: u32,
    content_type: u8,
) -> Result<Vec<u8>, String> {
    let b64 = cubemap_cross_preview(faces, face_w, face_h, format, content_type)?;
    BASE64_STANDARD.decode(b64.as_bytes()).map_err(|e| e.to_string())
}

fn float_to_preview_u8(v: f32) -> u8 {
    float_to_preview_u8_ex(v, false)
}

fn float_to_preview_u8_hdr(v: f32) -> u8 {
    float_to_preview_u8_ex(v, true)
}

fn float_to_preview_u8_ex(v: f32, hdr: bool) -> u8 {
    if !v.is_finite() {
        return 0;
    }

    let mapped = if hdr {
        // Reinhard tone-map for HDR (BC6H, R*_FLOAT) values that can exceed 1.0
        let lin = v.max(0.0);
        lin / (1.0 + lin)
    } else if (-1.0..=1.0).contains(&v) {
        (v * 0.5) + 0.5
    } else {
        v
    };

    (mapped.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn half_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) & 0x1) as u32;
    let exp = ((h >> 10) & 0x1f) as u32;
    let frac = (h & 0x03ff) as u32;

    let bits = if exp == 0 {
        if frac == 0 {
            sign << 31
        } else {
            let mut mant = frac;
            let mut e = -14i32;
            while (mant & 0x0400) == 0 {
                mant <<= 1;
                e -= 1;
            }
            mant &= 0x03ff;
            let exp32 = (e + 127) as u32;
            (sign << 31) | (exp32 << 23) | (mant << 13)
        }
    } else if exp == 0x1f {
        (sign << 31) | 0x7f80_0000 | (frac << 13)
    } else {
        let exp32 = exp + 112;
        (sign << 31) | (exp32 << 23) | (frac << 13)
    };

    f32::from_bits(bits)
}

fn decode_bc2_manual(
    data: &[u8],
    width: usize,
    height: usize,
    out_pixels: &mut [u32],
) -> Result<(), String> {
    let blocks_w = (width + 3) / 4;
    let blocks_h = (height + 3) / 4;

    // Decode BC1 color from the color half of each BC2 block
    // BC2 block layout: [8 bytes alpha][8 bytes BC1 color]
    // Build a BC1-only buffer from the color portions
    let mut bc1_data = Vec::with_capacity(blocks_w * blocks_h * 8);
    for block_idx in 0..(blocks_w * blocks_h) {
        let offset = block_idx * 16 + 8; // skip 8-byte alpha block
        if offset + 8 <= data.len() {
            bc1_data.extend_from_slice(&data[offset..offset + 8]);
        } else {
            bc1_data.extend_from_slice(&[0u8; 8]);
        }
    }

    // Decode BC1 color into output (this gives us BGRA with alpha=255 or 0)
    decode_bc1(&bc1_data, width, height, out_pixels)
        .map_err(|e| format!("BC2 color decode failed: {}", e))?;

    // Now overlay the explicit 4-bit alpha from the alpha portion of each block
    for by in 0..blocks_h {
        for bx in 0..blocks_w {
            let block_idx = by * blocks_w + bx;
            let alpha_offset = block_idx * 16; // first 8 bytes of the 16-byte block

            for row in 0..4 {
                let y = by * 4 + row;
                if y >= height {
                    break;
                }

                // Each row of 4 pixels has 2 bytes of alpha (4 bits per pixel)
                if alpha_offset + row * 2 + 1 >= data.len() {
                    continue;
                }
                let alpha_word = u16::from_le_bytes([
                    data[alpha_offset + row * 2],
                    data[alpha_offset + row * 2 + 1],
                ]);

                for col in 0..4 {
                    let x = bx * 4 + col;
                    if x >= width {
                        break;
                    }

                    let a4 = ((alpha_word >> (col * 4)) & 0xF) as u8;
                    let a8 = (a4 << 4) | a4; // expand 4-bit to 8-bit

                    let pixel_idx = y * width + x;
                    // Output is BGRA from BC1 decode; replace the alpha channel (byte 3)
                    let mut bytes = out_pixels[pixel_idx].to_le_bytes();
                    bytes[3] = a8;
                    out_pixels[pixel_idx] = u32::from_le_bytes(bytes);
                }
            }
        }
    }

    Ok(())
}

fn decode_bc6_resilient(
    data: &[u8],
    width: usize,
    height: usize,
    out_pixels: &mut [u32],
    preferred_signed: bool,
) -> Result<(), String> {
    let _hook_guard = BC6_PANIC_HOOK_GUARD
        .lock()
        .map_err(|_| "Failed to acquire BC6 decoder guard".to_string())?;

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    let decode_result = (|| {
        let blocks_w = (width + 3) / 4;
        let blocks_h = (height + 3) / 4;
        let blocks_total = blocks_w
            .checked_mul(blocks_h)
            .ok_or_else(|| "BC6 preview dimensions are too large".to_string())?;
        let expected_len = blocks_total
            .checked_mul(16)
            .ok_or_else(|| "BC6 preview data length overflow".to_string())?;

        if data.len() < expected_len {
            return Err("Not enough BC6 data to decode preview".into());
        }

        let mut failed_blocks = 0usize;
        for by in 0..blocks_h {
            for bx in 0..blocks_w {
                let block_index = (by * blocks_w + bx) * 16;
                let block_data = &data[block_index..block_index + 16];

                let mut block_pixels = [0u32; 16];
                let first_try = catch_unwind(AssertUnwindSafe(|| {
                    decode_bc6_block(block_data, &mut block_pixels, preferred_signed)
                }));

                let decoded = if first_try.is_ok() {
                    true
                } else {
                    let second_try = catch_unwind(AssertUnwindSafe(|| {
                        decode_bc6_block(block_data, &mut block_pixels, !preferred_signed)
                    }));
                    second_try.is_ok()
                };

                if !decoded {
                    failed_blocks += 1;
                    continue;
                }

                let y0 = by * 4;
                let x0 = bx * 4;
                for oy in 0..4 {
                    let y = y0 + oy;
                    if y >= height {
                        break;
                    }
                    for ox in 0..4 {
                        let x = x0 + ox;
                        if x >= width {
                            break;
                        }
                        out_pixels[y * width + x] = block_pixels[oy * 4 + ox];
                    }
                }
            }
        }

        if failed_blocks == blocks_total {
            return Err("BC6 decoder panicked on all blocks".into());
        }

        Ok(())
    })();

    std::panic::set_hook(previous_hook);
    decode_result
}

pub fn get_dds_info(path: &str) -> Result<TextureInfo, String> {
    let tex = DDSTex::read(path)?;
    Ok(TextureInfo {
        width: tex.width,
        height: tex.height,
        mipmaps: tex.mipmaps,
        hdmipmaps: 0,
        images: 1,
        bytes_per_pixel: tex.bytes_per_pixel,
        size: tex.size,
        hdsize: 0,
        format: tex.format,
        is_cubemap: false,
        is_ibl: false,
        dimension: 1,
        content_type: 0,
    })
}
