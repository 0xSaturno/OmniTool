pub const TAG_BUILT: u32 = 0x283D0383;

/// Feature bits at offset 0x28. Verified by correlating the field against which
/// DAT1 sections each sample model actually carries; the four bits that always
/// move together (5/17/18/24) cannot be told apart from the samples available.
pub mod flags {
    /// Model Col Vert + Model UV1 Vert are present.
    pub const UV1_AND_COLOR: u32 = 0x0000_0020;
    pub const UV1_AND_COLOR_B: u32 = 0x0002_0000;
    pub const UV1_AND_COLOR_C: u32 = 0x0004_0000;
    pub const UV1_AND_COLOR_D: u32 = 0x0100_0000;
    /// Always set on every sample; presumed "has geometry".
    pub const BASE: u32 = 0x0001_0000;
    /// Model Anim Morph* and Model Spline* are present.
    pub const MORPH_AND_SPLINES: u32 = 0x0010_0000;
    /// Model Joint Bind Chains (0x707F1B58) + Model IK Chains (0x9A434B29).
    pub const IK_CHAINS: u32 = 0x4000_0000;
    /// Model Anim Dynamics Def is present.
    pub const ANIM_DYNAMICS: u32 = 0x8000_0000;
}

/// The whole 120-byte Built header, as far as it has been reversed.
#[derive(Debug, Clone, Default)]
pub struct Built {
    /// Bounding-volume floats at 0x00..0x1C — center-ish xyz, a radius and three
    /// half-extents, but only the X extent reproduces a full-mesh AABB scan, so
    /// they are exposed raw rather than named.
    pub bounds: [f32; 7],
    pub position_offset: (f32, f32, f32),
    pub feature_flags: u32,
    pub position_scale: f32,
    /// Low nibble is the UV0 shift, next nibble the UV1 shift.
    pub uv_shifts: u32,
    /// Screen distances at which each LOD takes over.
    pub lod_distances: [f32; 5],
    pub unk_0x48: [f32; 6],
    pub unk_0x60: u32,
    pub index_count: u32,
    pub vertex_count: u32,
    pub unk_0x6c: u32,
    pub unk_0x70: u32,
    pub unk_0x74: u32,
}

impl Built {
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 0x78 {
            return None;
        }
        let f = |o: usize| f32::from_le_bytes(data[o..o + 4].try_into().unwrap());
        let u = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
        let mut bounds = [0f32; 7];
        for (i, b) in bounds.iter_mut().enumerate() {
            *b = f(i * 4);
        }
        let mut lod_distances = [0f32; 5];
        for (i, l) in lod_distances.iter_mut().enumerate() {
            *l = f(0x34 + i * 4);
        }
        let mut unk_0x48 = [0f32; 6];
        for (i, v) in unk_0x48.iter_mut().enumerate() {
            *v = f(0x48 + i * 4);
        }
        Some(Self {
            bounds,
            position_offset: (f(0x1C), f(0x20), f(0x24)),
            feature_flags: u(0x28),
            position_scale: f(0x2C),
            uv_shifts: u(0x30),
            lod_distances,
            unk_0x48,
            unk_0x60: u(0x60),
            index_count: u(0x64),
            vertex_count: u(0x68),
            unk_0x6c: u(0x6C),
            unk_0x70: u(0x70),
            unk_0x74: u(0x74),
        })
    }

    pub fn uv0_scale(&self) -> f32 {
        (1u32 << (self.uv_shifts & 0xF)) as f32 / 16384.0
    }

    pub fn uv1_scale(&self) -> f32 {
        (1u32 << ((self.uv_shifts >> 4) & 0xF)) as f32 / 16384.0
    }
}

/// Reads the UV0 scale stored in the Built section.
///
/// The base quantum is 1/16384, not 1/32768: a model whose shift nibble is 0
/// has raw UV components that reach 16384, i.e. exactly 1.0.
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

/// Reads the UV1 scale — the second nibble of the same field.
pub fn get_uv1_scale(built_data: &[u8]) -> f32 {
    const OFFSET: usize = 0x30;
    if built_data.len() < OFFSET + 4 {
        return 1.0 / 16384.0;
    }
    let raw: [u8; 4] = built_data[OFFSET..OFFSET + 4].try_into().unwrap();
    let shift = ((i32::from_le_bytes(raw) >> 4) & 0xF) as u32;
    (1u32 << shift) as f32 / 16384.0
}

/// Reads the five LOD switch distances at 0x34..0x48.
pub fn get_lod_distances(built_data: &[u8]) -> [f32; 5] {
    let mut out = [0f32; 5];
    if built_data.len() >= 0x48 {
        for (i, v) in out.iter_mut().enumerate() {
            let o = 0x34 + i * 4;
            *v = f32::from_le_bytes(built_data[o..o + 4].try_into().unwrap());
        }
    }
    out
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
