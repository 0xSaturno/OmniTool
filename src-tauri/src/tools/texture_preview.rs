use base64::prelude::*;
use image::{ImageFormat, RgbaImage};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Cursor as ImageCursor;
use std::path::PathBuf;

use crate::tools::texture_converter::{
    decode_to_rgba, SourceTex, mip_level_size, cubemap_cross_preview_rgba,
};

const PREVIEW_MAX_SIZE: usize = 256;

fn get_cache_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("omnitool").join("texture_thumbnails"))
}

fn cache_key(path: &str) -> String {
    let normalized = if cfg!(windows) {
        path.to_lowercase().replace('\\', "/")
    } else {
        path.to_string()
    };

    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

fn downscale_rgba(src: &[u8], src_w: usize, src_h: usize, dst_w: usize, dst_h: usize) -> Vec<u8> {
    if src_w == dst_w && src_h == dst_h {
        return src.to_vec();
    }
    let mut dst = vec![0u8; dst_w * dst_h * 4];
    let x_ratio = src_w as f32 / dst_w as f32;
    let y_ratio = src_h as f32 / dst_h as f32;

    for y in 0..dst_h {
        for x in 0..dst_w {
            let src_x = (x as f32 * x_ratio) as usize;
            let src_y = (y as f32 * y_ratio) as usize;
            let src_idx = (src_y * src_w + src_x) * 4;
            let dst_idx = (y * dst_w + x) * 4;
            dst[dst_idx..dst_idx + 4].copy_from_slice(&src[src_idx..src_idx + 4]);
        }
    }
    dst
}

fn preview_dimensions(width: usize, height: usize) -> (usize, usize) {
    if width <= PREVIEW_MAX_SIZE && height <= PREVIEW_MAX_SIZE {
        return (width, height);
    }
    let aspect = width as f32 / height as f32;
    if width > height {
        let w = PREVIEW_MAX_SIZE;
        let h = (w as f32 / aspect) as usize;
        (w, h.max(1))
    } else {
        let h = PREVIEW_MAX_SIZE;
        let w = (h as f32 * aspect) as usize;
        (w.max(1), h)
    }
}

fn encode_png_base64(rgba: &[u8], width: usize, height: usize) -> Result<String, String> {
    let img = RgbaImage::from_raw(width as u32, height as u32, rgba.to_vec())
        .ok_or("Failed to create RgbaImage")?;
    let mut cursor = ImageCursor::new(Vec::new());
    img.write_to(&mut cursor, ImageFormat::Png)
        .map_err(|e| format!("Failed to encode PNG: {}", e))?;
    Ok(BASE64_STANDARD.encode(cursor.into_inner()))
}

pub fn get_cached_preview(path: &str) -> Result<String, String> {
    if let Some(cache_dir) = get_cache_dir() {
        let key = cache_key(path);
        let cache_file = cache_dir.join(format!("{}.png.b64", key));

        if cache_file.exists() {
            if let Ok(cached) = std::fs::read_to_string(&cache_file) {
                return Ok(cached);
            }
        }

        let preview = generate_preview(path)?;

        if let Err(e) = std::fs::create_dir_all(&cache_dir) {
            eprintln!("Failed to create cache dir: {}", e);
        } else if let Err(e) = std::fs::write(&cache_file, &preview) {
            eprintln!("Failed to write cache file: {}", e);
        }

        Ok(preview)
    } else {
        generate_preview(path)
    }
}

fn generate_preview(path: &str) -> Result<String, String> {
    let tex = SourceTex::read(path, None)?; // No explicit HD path = SD-only for fast preview
    if tex.images == 0 || tex.mipmaps.is_empty() {
        return Err("No images available for preview".into());
    }

    let is_cubemap = tex.dimension == 4 && tex.images >= 6;
    let (target_w, target_h) = preview_dimensions(tex.sd_width as usize, tex.sd_height as usize);

    if is_cubemap && tex.mipmaps.len() >= 6 {
        let face_w = tex.sd_width as usize;
        let face_h = tex.sd_height as usize;
        let mip_size = mip_level_size(face_w as u32, face_h as u32, tex.format as u32)
            .map(|v| v as usize)
            .unwrap_or_else(|| (face_w as f64 * face_h as f64 * tex.bytes_per_pixel).ceil() as usize);

        let mut face_rgba: Vec<Vec<u8>> = Vec::new();
        for i in 0..6usize {
            if tex.mipmaps[i].len() >= mip_size {
                let rgba = decode_to_rgba(&tex.mipmaps[i][..mip_size], face_w, face_h, tex.format as u32, tex.content_type)?;
                let downscaled = if target_w != face_w || target_h != face_h {
                    downscale_rgba(&rgba, face_w, face_h, target_w, target_h)
                } else {
                    rgba
                };
                face_rgba.push(downscaled);
            } else {
                face_rgba.push(vec![]);
            }
        }

        let cross_rgba = cubemap_cross_preview_rgba(&face_rgba, target_w, target_h)?;
        return encode_png_base64(&cross_rgba, target_w * 4, target_h * 2);
    }

    let width = tex.sd_width as usize;
    let height = tex.sd_height as usize;
    let mip_size = mip_level_size(width as u32, height as u32, tex.format as u32)
        .map(|v| v as usize)
        .unwrap_or_else(|| (width as f64 * height as f64 * tex.bytes_per_pixel).ceil() as usize);

    if tex.mipmaps[0].len() < mip_size {
        return Err("Not enough data to decode preview".into());
    }

    let rgba = decode_to_rgba(&tex.mipmaps[0][..mip_size], width, height, tex.format as u32, tex.content_type)?;

    if target_w != width || target_h != height {
        let downscaled = downscale_rgba(&rgba, width, height, target_w, target_h);
        encode_png_base64(&downscaled, target_w, target_h)
    } else {
        encode_png_base64(&rgba, width, height)
    }
}

pub fn clear_cache() -> Result<usize, String> {
    if let Some(cache_dir) = get_cache_dir() {
        if !cache_dir.exists() {
            return Ok(0);
        }
        let mut count = 0;
        for entry in std::fs::read_dir(&cache_dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if path.is_file() && path.extension().map(|e| e == "b64").unwrap_or(false) {
                if std::fs::remove_file(&path).is_ok() {
                    count += 1;
                }
            }
        }
        Ok(count)
    } else {
        Err("Could not determine cache directory".into())
    }
}
