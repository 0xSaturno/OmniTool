pub const TAG_BUILT: u32 = 0x283D0383;

/// Reads the UV scale stored in the Built section.
pub fn get_uv_scale(built_data: &[u8]) -> f32 {
    const OFFSET: usize = 0x30;
    if built_data.len() < OFFSET + 4 {
        return 1.0 / 16384.0;
    }
    let raw: [u8; 4] = built_data[OFFSET..OFFSET + 4].try_into().unwrap();
    let iuvscale = i32::from_le_bytes(raw);
    let shift = (iuvscale & 0xF) as u32;
    (1u32 << shift) as f32 / 16384.0
}

/// Reads the position scale stored in the Built section at offset 0x2C.
pub fn get_position_scale(built_data: &[u8]) -> f32 {
    const OFFSET: usize = 0x2C;
    if built_data.len() < OFFSET + 4 {
        return 1.0 / 4096.0;
    }
    let raw: [u8; 4] = built_data[OFFSET..OFFSET + 4].try_into().unwrap();
    f32::from_le_bytes(raw)
}

/// Writes the total vertex count and index count into the Built section.
/// reads these as u32 packed into the f32 values array at offsets 0x68 and
/// 0x64 respectively. The game validates section sizes against these, so
/// they MUST be updated whenever VERTEXES / INDEXES grow or shrink.
pub fn set_counts(built_data: &mut [u8], vertex_count: u32, index_count: u32) {
    const IDX_OFFSET: usize = 0x64;
    const VTX_OFFSET: usize = 0x68;
    if built_data.len() >= IDX_OFFSET + 4 {
        built_data[IDX_OFFSET..IDX_OFFSET + 4].copy_from_slice(&index_count.to_le_bytes());
    }
    if built_data.len() >= VTX_OFFSET + 4 {
        built_data[VTX_OFFSET..VTX_OFFSET + 4].copy_from_slice(&vertex_count.to_le_bytes());
    }
}

/// Reads the position offset (x, y, z) stored in the Built section at offsets 0x1C, 0x20, 0x24.
pub fn get_position_offset(built_data: &[u8]) -> (f32, f32, f32) {
    const OFFSET: usize = 0x1C;
    if built_data.len() < OFFSET + 12 {
        return (0.0, 0.0, 0.0);
    }
    let x: [u8; 4] = built_data[OFFSET..OFFSET + 4].try_into().unwrap();
    let y: [u8; 4] = built_data[OFFSET + 4..OFFSET + 8].try_into().unwrap();
    let z: [u8; 4] = built_data[OFFSET + 8..OFFSET + 12].try_into().unwrap();
    (f32::from_le_bytes(x), f32::from_le_bytes(y), f32::from_le_bytes(z))
}
