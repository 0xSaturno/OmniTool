pub const TAG_BUILT: u32 = 0x283D0383;

/// Model-wide flags at 0x28; bits 0, 3, 17, 18, 20 and 31 are corpus-verified against what they announce.
pub mod flags {
    pub const AMBIENT_ANIMATION: u32 = 1 << 0;
    pub const SKIP_SHADOW_CAST: u32 = 1 << 1;
    pub const SKIP_FAR_LOD_CACHE: u32 = 1 << 2;
    pub const SHADOW_HAS_FADE_DIST: u32 = 1 << 3;
    pub const PHYSICS_LOOK_LOCKED: u32 = 1 << 4;
    pub const SKIP_DECALS: u32 = 1 << 5;
    pub const SKIP_IMPOSTOR: u32 = 1 << 6;
    pub const SKIP_LIGHT_CAPTURE: u32 = 1 << 7;
    pub const ONLY_LIGHT_CAPTURE: u32 = 1 << 8;
    pub const NO_EMBEDDED_SAMPLES: u32 = 1 << 9;
    pub const NEVER_OCCLUDE: u32 = 1 << 10;
    pub const SKIP_RAIN_SPLASHES: u32 = 1 << 11;
    pub const DELAY_STREAMING: u32 = 1 << 12;
    pub const STATIC_COLL_ONLY: u32 = 1 << 13;
    pub const COLLISION_ONLY: u32 = 1 << 14;
    pub const SKIP_NAV_GENERATION: u32 = 1 << 15;
    pub const WALKABLE_AREA: u32 = 1 << 16;
    pub const VERT_HAS_UV1: u32 = 1 << 17;
    pub const VERT_HAS_COLOR: u32 = 1 << 18;
    pub const SKIP_SKIN_THRESHOLD: u32 = 1 << 19;
    pub const ANIM_VERT: u32 = 1 << 20;
    pub const HAS_HAIR_CARDS_LOOK: u32 = 1 << 21;
    pub const HIBERNATE_NEVER: u32 = 1 << 22;
    pub const CENTERED_VERTS: u32 = 1 << 23;
    pub const SKIP_STATIC_DECALS: u32 = 1 << 24;
    pub const HAS_CUSTOM_JOINT_BSPHERES: u32 = 1 << 25;
    pub const SPLINE_MODEL: u32 = 1 << 26;
    pub const HIBERNATE: u32 = 1 << 27;
    pub const HIBERNATE_LONG_DIST: u32 = 1 << 28;
    pub const HAS_CINEMATIC_SHADOW: u32 = 1 << 30;
    pub const ANIM_DYNAMICS: u32 = 1 << 31;

    pub const NAMES: [(u32, &str); 31] = [
        (AMBIENT_ANIMATION, "Ambient animation"),
        (SKIP_SHADOW_CAST, "Skip shadow cast"),
        (SKIP_FAR_LOD_CACHE, "Skip far LOD cache"),
        (SHADOW_HAS_FADE_DIST, "Shadow fade distance"),
        (PHYSICS_LOOK_LOCKED, "Physics look locked"),
        (SKIP_DECALS, "Skip decals"),
        (SKIP_IMPOSTOR, "Skip impostor"),
        (SKIP_LIGHT_CAPTURE, "Skip light capture"),
        (ONLY_LIGHT_CAPTURE, "Only light capture"),
        (NO_EMBEDDED_SAMPLES, "No embedded light samples"),
        (NEVER_OCCLUDE, "Never occlude"),
        (SKIP_RAIN_SPLASHES, "Skip rain splashes"),
        (DELAY_STREAMING, "Delay streaming"),
        (STATIC_COLL_ONLY, "Static collision only"),
        (COLLISION_ONLY, "Collision only"),
        (SKIP_NAV_GENERATION, "Skip nav generation"),
        (WALKABLE_AREA, "Walkable area"),
        (VERT_HAS_UV1, "UV1 stream"),
        (VERT_HAS_COLOR, "Vertex colour stream"),
        (SKIP_SKIN_THRESHOLD, "Skip skin threshold"),
        (ANIM_VERT, "Anim-vert (morph) subsets"),
        (HAS_HAIR_CARDS_LOOK, "Hair-cards look"),
        (HIBERNATE_NEVER, "Never hibernate"),
        (CENTERED_VERTS, "Centred vertices"),
        (SKIP_STATIC_DECALS, "Skip static decals"),
        (HAS_CUSTOM_JOINT_BSPHERES, "Custom joint bspheres"),
        (SPLINE_MODEL, "Spline model"),
        (HIBERNATE, "Hibernate"),
        (HIBERNATE_LONG_DIST, "Hibernate (long distance)"),
        (HAS_CINEMATIC_SHADOW, "Cinematic shadow"),
        (ANIM_DYNAMICS, "Anim dynamics"),
    ];
}

/// Content flags at 0x74; only `MORPH` ships, and it tracks Model Anim Morph Info exactly.
pub mod content_flags {
    pub const MORPH: u32 = 1 << 0;
    pub const ANIM_GEOM: u32 = 1 << 1;
    pub const ZIVA: u32 = 1 << 2;
    pub const ANIM_VERT: u32 = 1 << 3;
}

/// The 120-byte Model Built header.
#[derive(Debug, Clone, Default)]
pub struct Built {
    pub bsphere_center: [f32; 3],
    pub bsphere_radius: f32,
    /// Conservative half-extents around `bsphere_center`.
    pub aabb_extents: [f32; 3],
    /// Added to positions when `flags::CENTERED_VERTS` is set; zero otherwise.
    pub mesh_center: [f32; 3],
    pub flags: u32,
    /// Position quantum: vertex i16s times this are metres.
    pub meters_per_unit: f32,
    /// Low nibble is the UV0 shift, next nibble the UV1 shift.
    pub uv_log_scales: u32,
    /// Camera distances at which LODs 1..5 take over.
    pub lod_distances: [f32; 5],
    /// Non-zero exactly when `flags::AMBIENT_ANIMATION` is set.
    pub ambient_animation: f32,
    pub max_displacement: f32,
    pub max_dynamic_force: f32,
    pub min_dynamic_force: f32,
    /// Raw fixed point; see `z_bias()`.
    pub z_bias: i16,
    /// Raw fixed point; see `alpha_sort_bias()`.
    pub alpha_sort_bias: i16,
    pub fade_out_dist: u16,
    /// Non-zero exactly when `flags::SHADOW_HAS_FADE_DIST` is set.
    pub shadow_fade_dist: u16,
    /// -1 on most models, otherwise a LOD index 0..5.
    pub shadow_casting_lod: i16,
    /// Equals the number of Model Spline Subsets records.
    pub strand_subset_count: u8,
    pub index_count: u32,
    pub vertex_count: u32,
    pub av_material_hash: u32,
    pub audio_material_hash: u32,
    pub content_flags: u32,
}

impl Built {
    pub const SIZE: usize = 0x78;

    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < Self::SIZE {
            return None;
        }
        let f = |o: usize| f32::from_le_bytes(data[o..o + 4].try_into().unwrap());
        let u = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
        let h = |o: usize| u16::from_le_bytes(data[o..o + 2].try_into().unwrap());
        let v3 = |o: usize| [f(o), f(o + 4), f(o + 8)];
        let mut lod_distances = [0f32; 5];
        for (i, l) in lod_distances.iter_mut().enumerate() {
            *l = f(0x34 + i * 4);
        }
        Some(Self {
            bsphere_center: v3(0x00),
            bsphere_radius: f(0x0C),
            aabb_extents: v3(0x10),
            mesh_center: v3(0x1C),
            flags: u(0x28),
            meters_per_unit: f(0x2C),
            uv_log_scales: u(0x30),
            lod_distances,
            ambient_animation: f(0x48),
            max_displacement: f(0x4C),
            max_dynamic_force: f(0x50),
            min_dynamic_force: f(0x54),
            z_bias: h(0x58) as i16,
            alpha_sort_bias: h(0x5A) as i16,
            fade_out_dist: h(0x5C),
            shadow_fade_dist: h(0x5E),
            shadow_casting_lod: h(0x60) as i16,
            strand_subset_count: data[0x62],
            index_count: u(0x64),
            vertex_count: u(0x68),
            av_material_hash: u(0x6C),
            audio_material_hash: u(0x70),
            content_flags: u(0x74),
        })
    }

    pub fn uv0_scale(&self) -> f32 {
        (1u32 << (self.uv_log_scales & 0xF)) as f32 / 16384.0
    }

    pub fn uv1_scale(&self) -> f32 {
        (1u32 << ((self.uv_log_scales >> 4) & 0xF)) as f32 / 16384.0
    }

    pub fn z_bias(&self) -> f32 {
        self.z_bias as f32 / 16384.0
    }

    pub fn alpha_sort_bias(&self) -> f32 {
        self.alpha_sort_bias as f32 / 512.0
    }

    pub fn flag_names(&self) -> Vec<&'static str> {
        flags::NAMES
            .iter()
            .filter(|(bit, _)| self.flags & bit != 0)
            .map(|(_, n)| *n)
            .collect()
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

/// Reads the position quantum (meters per unit) at offset 0x2C.
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

/// Reads the mesh centre at 0x1C; applied only under `flags::CENTERED_VERTS`, which no shipped model sets.
pub fn get_mesh_center(built_data: &[u8]) -> (f32, f32, f32) {
    const OFFSET: usize = 0x1C;
    if built_data.len() < OFFSET + 12 {
        return (0.0, 0.0, 0.0);
    }
    let x: [u8; 4] = built_data[OFFSET..OFFSET + 4].try_into().unwrap();
    let y: [u8; 4] = built_data[OFFSET + 4..OFFSET + 8].try_into().unwrap();
    let z: [u8; 4] = built_data[OFFSET + 8..OFFSET + 12].try_into().unwrap();
    (f32::from_le_bytes(x), f32::from_le_bytes(y), f32::from_le_bytes(z))
}
