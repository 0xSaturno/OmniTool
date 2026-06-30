use crate::core::error::ToolkitError;
use std::io::{BufReader, Cursor};
use base64::Engine;

/// Helper: parse RIFF/RIFX container to extract format tag and data chunk bytes.
pub fn parse_riff_container(wem_bytes: &[u8]) -> Result<(u16, Vec<u8>), ToolkitError> {
    if wem_bytes.len() < 12 {
        return Err(ToolkitError::Parse("WEM data too small to be a RIFF/RIFX container".into()));
    }
    
    let is_riff = &wem_bytes[0..4] == b"RIFF";
    let is_rifx = &wem_bytes[0..4] == b"RIFX";
    
    if !is_riff && !is_rifx {
        return Err(ToolkitError::Parse("WEM is not a RIFF or RIFX container".into()));
    }
    
    let big_endian = is_rifx;
    
    let read_u32 = |offset: usize| -> Result<u32, ToolkitError> {
        if offset + 4 > wem_bytes.len() {
            return Err(ToolkitError::Parse("Unexpected EOF reading u32".into()));
        }
        let arr = [
            wem_bytes[offset],
            wem_bytes[offset + 1],
            wem_bytes[offset + 2],
            wem_bytes[offset + 3],
        ];
        if big_endian {
            Ok(u32::from_be_bytes(arr))
        } else {
            Ok(u32::from_le_bytes(arr))
        }
    };
    
    let read_u16 = |offset: usize| -> Result<u16, ToolkitError> {
        if offset + 2 > wem_bytes.len() {
            return Err(ToolkitError::Parse("Unexpected EOF reading u16".into()));
        }
        let arr = [wem_bytes[offset], wem_bytes[offset + 1]];
        if big_endian {
            Ok(u16::from_be_bytes(arr))
        } else {
            Ok(u16::from_le_bytes(arr))
        }
    };
    
    let mut offset = 12;
    let mut format_tag = None;
    let mut data_bytes = None;
    
    while offset + 8 <= wem_bytes.len() {
        let chunk_id = &wem_bytes[offset..offset + 4];
        let chunk_size = read_u32(offset + 4)? as usize;
        let chunk_data_offset = offset + 8;
        
        let mut actual_chunk_size = chunk_size;
        if chunk_data_offset + chunk_size > wem_bytes.len() {
            // Cap to available bytes to handle truncated or padded files gracefully
            actual_chunk_size = wem_bytes.len().saturating_sub(chunk_data_offset);
        }
        
        if chunk_id == b"fmt " {
            if actual_chunk_size >= 2 {
                let tag = read_u16(chunk_data_offset)?;
                format_tag = Some(tag);
            }
        } else if chunk_id == b"data" {
            data_bytes = Some(wem_bytes[chunk_data_offset..chunk_data_offset + actual_chunk_size].to_vec());
        }
        
        let aligned_size = (actual_chunk_size + 1) & !1;
        offset = chunk_data_offset + aligned_size;
    }
    
    let tag = format_tag.ok_or_else(|| ToolkitError::Parse("No fmt chunk found in WEM".into()))?;
    let data = data_bytes.ok_or_else(|| ToolkitError::Parse("No data chunk found in WEM".into()))?;
    
    Ok((tag, data))
}

/// Decode a Wwise WEM to OGG bytes.
/// Supports both Vorbis (via ww2ogg) and Wwise Opus (by extracting direct OggS stream).
/// Returns the raw OGG bytes on success.
pub fn decode_wem_to_ogg(wem_bytes: &[u8]) -> Result<Vec<u8>, ToolkitError> {
    // Parse RIFF container to detect format tag
    let (format_tag, data_chunk) = parse_riff_container(wem_bytes)?;

    // Wwise Opus (0x3040 = AK_WAVE_FORMAT_OPUS, 0x3041 = AK_WAVE_FORMAT_OPUS_WEM)
    if format_tag == 0x3040 || format_tag == 0x3041 {
        if data_chunk.len() >= 4 && &data_chunk[0..4] == b"OggS" {
            return Ok(data_chunk);
        } else {
            return Err(ToolkitError::Parse(
                "Wwise Opus data chunk does not start with OggS".into()
            ));
        }
    }

    // Try default codebooks first, fall back to aoTuV
    let result = try_decode_with_codebook(wem_bytes, false);
    match result {
        Ok(ogg) => Ok(ogg),
        Err(_first_err) => {
            // Try aoTuV codebooks
            match try_decode_with_codebook(wem_bytes, true) {
                Ok(ogg) => Ok(ogg),
                Err(_) => Err(ToolkitError::Parse(format!(
                    "Failed to decode WEM with both standard and aoTuV codebooks: {}",
                    _first_err
                ))),
            }
        }
    }
}

fn try_decode_with_codebook(wem_bytes: &[u8], use_aotuv: bool) -> Result<Vec<u8>, String> {
    let input = BufReader::new(Cursor::new(wem_bytes));
    
    let codebooks = if use_aotuv {
        ww2ogg::CodebookLibrary::aotuv_codebooks()
    } else {
        ww2ogg::CodebookLibrary::default_codebooks()
    }.map_err(|e| format!("Failed to load codebooks: {}", e))?;

    let mut converter = ww2ogg::WwiseRiffVorbis::new(input, codebooks)
        .map_err(|e| format!("Failed to parse WEM: {}", e))?;

    let mut ogg_buf = Vec::new();
    let mut cursor = Cursor::new(&mut ogg_buf);
    converter.generate_ogg(&mut cursor)
        .map_err(|e| format!("Failed to generate OGG: {}", e))?;

    // Validate the generated OGG audio to ensure packets decode correctly (e.g. correct codebook selection)
    ww2ogg::validate(&ogg_buf)
        .map_err(|e| format!("Audio validation failed: {}", e))?;

    Ok(ogg_buf)
}

/// Decode a WEM to OGG and return as base64 string for frontend <audio> playback.
pub fn decode_wem_to_base64_ogg(wem_bytes: &[u8]) -> Result<String, ToolkitError> {
    let ogg_bytes = decode_wem_to_ogg(wem_bytes)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&ogg_bytes))
}

/// Write WEM bytes to a file on disk.
pub fn write_wem_to_file(wem_bytes: &[u8], output_path: &str) -> Result<(), ToolkitError> {
    std::fs::write(output_path, wem_bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode() {
        let path = "B:\\Rift Apart Modding\\II\\Extracts\\940365818.wem";
        if !std::path::Path::new(path).exists() {
            println!("Skipping test_decode: file does not exist");
            return;
        }
        let wem_bytes = std::fs::read(path).unwrap();
        match decode_wem_to_ogg(&wem_bytes) {
            Ok(ogg) => {
                println!("Success! OGG size: {}", ogg.len());
                std::fs::write("B:\\Rift Apart Modding\\II\\Extracts\\940365818.ogg", ogg).unwrap();
            }
            Err(e) => {
                panic!("Failed to decode: {:?}", e);
            }
        }
    }

    #[test]
    fn test_decode_opus() {
        let path = "B:\\Rift Apart Modding\\II\\Extracts\\99587406.wem";
        if !std::path::Path::new(path).exists() {
            println!("Skipping test_decode_opus: file does not exist");
            return;
        }
        let wem_bytes = std::fs::read(path).unwrap();
        match decode_wem_to_ogg(&wem_bytes) {
            Ok(ogg) => {
                println!("Success! OGG size: {}", ogg.len());
                std::fs::write("B:\\Rift Apart Modding\\II\\Extracts\\99587406.ogg", ogg).unwrap();
            }
            Err(e) => {
                panic!("Failed to decode: {:?}", e);
            }
        }
    }

    #[test]
    fn test_decode_opus_truncated() {
        let path = "B:\\Rift Apart Modding\\II\\Extracts\\63650798.wem";
        if !std::path::Path::new(path).exists() {
            println!("Skipping test_decode_opus_truncated: file does not exist");
            return;
        }
        let wem_bytes = std::fs::read(path).unwrap();
        match decode_wem_to_ogg(&wem_bytes) {
            Ok(ogg) => {
                println!("Success! OGG size: {}", ogg.len());
                std::fs::write("B:\\Rift Apart Modding\\II\\Extracts\\63650798.ogg", ogg).unwrap();
            }
            Err(e) => {
                panic!("Failed to decode: {:?}", e);
            }
        }
    }
}
