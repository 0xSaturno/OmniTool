use base64::prelude::*;
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use image::{ImageFormat, RgbaImage};
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

        t_cur.read_u8().unwrap();
        let _channels = t_cur.read_u8().unwrap();
        let format = t_cur.read_u16::<LittleEndian>().unwrap();

        let basemipsize = mip_level_size(sd_width as u32, sd_height as u32, format as u32)
            .unwrap_or(tex_size / images as u32);

        let format_bits = bits_per_pixel(format as u32);
        let bytes_per_pixel = if format_bits > 0 {
            (format_bits / 8) as f64
        } else {
            2f64.powf(((basemipsize as f64 / sd_width as f64 / sd_height as f64).log2()).floor())
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
            let read_len = fs.read(&mut mip).unwrap_or(0);
            mip.truncate(read_len);
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
        fs.write_all(&[0u8; 4 * 4]).unwrap(); // caps2-4, reserved

        // DX10 header
        fs.write_u32::<LittleEndian>(format).unwrap();
        fs.write_u32::<LittleEndian>(if height > 1 { 3 } else { 2 })
            .unwrap();
        fs.write_u32::<LittleEndian>(0).unwrap(); // misc
        fs.write_u32::<LittleEndian>(1).unwrap(); // arraySize
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

fn mip_level_size(width: u32, height: u32, format: u32) -> Option<u32> {
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

pub fn extract_texture(
    source_path: &str,
    output_dir: Option<String>,
    explicit_hd_path: Option<&str>,
) -> Result<String, String> {
    let tex = SourceTex::read(source_path, explicit_hd_path)?;
    let mut out_base = PathBuf::from(source_path);
    out_base.set_extension("dds");
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
    if tex.images > 1 {
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

    let mut out_base = if let Some(out_sd) = explicit_out_sd {
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

    let mut width = dds.width;
    let mut height = dds.height;
    let mut sd_width = tex.sd_width as u32;
    let mut sd_height = tex.sd_height as u32;
    let mut size = tex.size;
    let mut hdsize = tex.hdsize;
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

    if tex.hdsize > 0 {
        hdmipmaps = extrasdmipmaps - actual_extra_sd;
        extrasdmipmaps = actual_extra_sd;
        hdsize = sizeincrease * tex.images as u32;
        sizeincrease = 0;
    } else {
        hdsize = 0;
        hdmipmaps = 0;
    }

    for i in (1..=extrasdmipmaps).rev() {
        sizeincrease += tex.basemipsize << (2 * i);
    }
    let extrasdmipsize = sizeincrease * tex.images as u32;
    sd_width <<= extrasdmipmaps;
    sd_height <<= extrasdmipmaps;

    for i in 0..ddss.len() {
        if ddss[i].mipmaps < hdmipmaps + extrasdmipmaps + tex.mipmaps_count as u32 {
            return Err(format!("Not enough mipmaps in DDS file A{} to replace", i));
        }
    }

    let mut hdmips_list = Vec::new();
    let mut extrasdmips_list = Vec::new();
    let mut sdmips_list = Vec::new();

    for i in 0..ddss.len() {
        let mut fs = File::open(&ddss[i].filename).unwrap();
        fs.seek(SeekFrom::Start(ddss[i].dataoffset)).unwrap();

        let mut hd_part = vec![0u8; (hdsize / tex.images as u32) as usize];
        let read_len = fs.read(&mut hd_part).unwrap_or(0);
        hd_part.truncate(read_len);
        hdmips_list.push(hd_part);

        let mut ex_part = vec![0u8; (extrasdmipsize / tex.images as u32) as usize];
        let read_len = fs.read(&mut ex_part).unwrap_or(0);
        ex_part.truncate(read_len);
        extrasdmips_list.push(ex_part);

        let mut sd_part = vec![0u8; (tex.size / tex.images as u32) as usize];
        let read_len = fs.read(&mut sd_part).unwrap_or(0);
        sd_part.truncate(read_len);
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
    })
}

pub fn get_texture_preview(path: &str) -> Result<String, String> {
    let tex = SourceTex::read(path, None)?;
    if tex.images == 0 || tex.mipmaps.is_empty() {
        return Err("No images available for preview".into());
    }

    if tex.hdsize > 0 && tex.hdmipmaps > 0 && !tex.hdfilename.is_empty() {
        let hd_bytes = std::fs::read(&tex.hdfilename)
            .map_err(|e| format!("Failed to read {}: {}", tex.hdfilename, e))?;
        let width = tex.width as usize;
        let height = tex.height as usize;
        let mip_size = mip_level_size(width as u32, height as u32, tex.format as u32)
            .map(|v| v as usize)
            .unwrap_or_else(|| {
                (width as f64 * height as f64 * tex.bytes_per_pixel).ceil() as usize
            });

        let per_image_len = hd_bytes.len() / tex.images as usize;
        if per_image_len < mip_size {
            return Err("Not enough HD data to decode preview".into());
        }

        let mip_data = &hd_bytes[..mip_size];
        return decode_to_base64_png(mip_data, width, height, tex.format as u32);
    }

    let width = tex.sd_width as usize;
    let height = tex.sd_height as usize;
    let mip_size = mip_level_size(width as u32, height as u32, tex.format as u32)
        .map(|v| v as usize)
        .unwrap_or_else(|| (width as f64 * height as f64 * tex.bytes_per_pixel).ceil() as usize);

    if tex.mipmaps[0].len() < mip_size {
        return Err("Not enough data to decode preview".into());
    }

    let mip_data = &tex.mipmaps[0][..mip_size];
    decode_to_base64_png(mip_data, width, height, tex.format as u32)
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

    decode_to_base64_png(&mip_data, width, height, tex.format)
}

fn decode_to_base64_png(
    data: &[u8],
    width: usize,
    height: usize,
    format: u32,
) -> Result<String, String> {
    let mut u32_pixels = vec![0u32; width * height];
    let res: Result<(), String> = match format {
        71 | 72 => decode_bc1(data, width, height, &mut u32_pixels).map_err(|e| e.to_string()),
        77 | 78 => decode_bc3(data, width, height, &mut u32_pixels).map_err(|e| e.to_string()),
        80 => decode_bc4(data, width, height, &mut u32_pixels).map_err(|e| e.to_string()),
        83 => decode_bc5(data, width, height, &mut u32_pixels).map_err(|e| e.to_string()),
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
            for i in 0..(width * height) {
                let px = i * 8;
                if px + 7 < data.len() {
                    let r = half_to_f32(u16::from_le_bytes([data[px], data[px + 1]]));
                    let g = half_to_f32(u16::from_le_bytes([data[px + 2], data[px + 3]]));
                    let b = half_to_f32(u16::from_le_bytes([data[px + 4], data[px + 5]]));
                    let a = half_to_f32(u16::from_le_bytes([data[px + 6], data[px + 7]]));
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

fn float_to_preview_u8(v: f32) -> u8 {
    if !v.is_finite() {
        return 0;
    }

    let mapped = if (-1.0..=1.0).contains(&v) {
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
    })
}
