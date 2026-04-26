use byteorder::{LE, ReadBytesExt};
use std::io::Cursor;
use crate::core::error::{Result, ToolkitError};
use crate::core::math::decode_normal;

pub const TAG_INDEXES:   u32 = 0x0859863D;
pub const TAG_VERTEXES:  u32 = 0xA98BE69B;
pub const TAG_UV1:       u32 = 0x6B855EED;

// Vertex

#[derive(Debug, Clone)]
pub struct Vertex {
    pub x: f32, pub y: f32, pub z: f32,
    pub nx: f32, pub ny: f32, pub nz: f32,
    pub u: f32, pub v: f32,
    pub tangent: Option<(f32, f32, f32)>,
    pub bitangent: Option<(f32, f32, f32)>,
    /// Raw packed normal u32 — preserved for lossless RCRA round-trips.
    pub raw_normal: Option<u32>,
    /// Raw W component from the vertex (RCRA i16 at bytes 6..8).
    pub raw_w: i16,
}

impl Vertex {
    pub fn zero() -> Self {
        Self { x: 0.0, y: 0.0, z: 0.0, nx: 1.0, ny: 0.0, nz: 0.0, u: 0.0, v: 0.0, tangent: None, bitangent: None, raw_normal: None, raw_w: 0 }
    }

    pub fn save_rcra(&self) -> [u8; 16] {
        self.save_rcra_scaled(1.0 / 4096.0)
    }

    pub fn save_rcra_scaled(&self, pos_scale: f32) -> [u8; 16] {
        let xi = (self.x / pos_scale).round() as i16;
        let yi = (self.y / pos_scale).round() as i16;
        let zi = (self.z / pos_scale).round() as i16;

        // Priority:
        //   1. raw_normal preserved from decode/ASCII #nrm tag  (round-trip exact)
        //   2. computed tangent+bitangent -> full octahedral+tangent pack
        //   3. normal-only fallback (loses tangent; only safe for untextured meshes)
        let (nxyz, w) = if let Some(rn) = self.raw_normal {
            (rn, self.raw_w)
        } else if let (Some(t), Some(b)) = (self.tangent, self.bitangent) {
            encode_normal_with_tangent((self.nx, self.ny, self.nz), t, b)
        } else {
            (encode_normal(self.nx, self.ny, self.nz), self.raw_w)
        };

        let ui = (self.u * 32768.0).round() as i16;
        let vi = (self.v * 32768.0).round() as i16;

        let mut out = [0u8; 16];
        out[0..2].copy_from_slice(&xi.to_le_bytes());
        out[2..4].copy_from_slice(&yi.to_le_bytes());
        out[4..6].copy_from_slice(&zi.to_le_bytes());
        out[6..8].copy_from_slice(&w.to_le_bytes());
        out[8..12].copy_from_slice(&nxyz.to_le_bytes());
        out[12..14].copy_from_slice(&ui.to_le_bytes());
        out[14..16].copy_from_slice(&vi.to_le_bytes());
        out
    }
}



// IndexesSection

pub struct IndexesSection {
    pub values: Vec<u16>,
}

impl IndexesSection {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let count = data.len() / 2;
        let mut cur = Cursor::new(data);
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            values.push(cur.read_u16::<LE>()?);
        }
        Ok(Self { values })
    }

    pub fn save(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.values.len() * 2);
        for &v in &self.values {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }
}

// VertexesSection

pub struct VertexesSection {
    pub vertexes: Vec<Vertex>,
}

impl VertexesSection {
    pub fn parse(data: &[u8]) -> Result<Self> {
        Self::parse_scaled(data, 1.0 / 4096.0)
    }

    pub fn parse_scaled(data: &[u8], pos_scale: f32) -> Result<Self> {
        if data.len() % 16 != 0 {
            return Err(ToolkitError::Parse(format!("vertex data size {} not divisible by 16", data.len())));
        }
        let count = data.len() / 16;
        let mut cur = Cursor::new(data);
        let mut vertexes = Vec::with_capacity(count);
        for _ in 0..count {
            let xi = cur.read_i16::<LE>()?;
            let yi = cur.read_i16::<LE>()?;
            let zi = cur.read_i16::<LE>()?;
            let w = cur.read_i16::<LE>()?;
            let nxyz = cur.read_u32::<LE>()?;
            let ui = cur.read_i16::<LE>()?;
            let vi = cur.read_i16::<LE>()?;

            let (nx, ny, nz) = decode_normal(nxyz);
            vertexes.push(Vertex {
                x: xi as f32 * pos_scale,
                y: yi as f32 * pos_scale,
                z: zi as f32 * pos_scale,
                nx, ny, nz,
                u: ui as f32 / 32768.0,
                v: vi as f32 / 32768.0,
                tangent: None,
                bitangent: None,
                raw_normal: Some(nxyz),
                raw_w: w,
            });
        }
        Ok(Self { vertexes })
    }

    pub fn save(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.vertexes.len() * 16);
        for v in &self.vertexes {
            out.extend_from_slice(&v.save_rcra());
        }
        out
    }

    pub fn save_scaled(&self, pos_scale: f32) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.vertexes.len() * 16);
        for v in &self.vertexes {
            out.extend_from_slice(&v.save_rcra_scaled(pos_scale));
        }
        out
    }
}

fn encode_normal(nx: f32, ny: f32, nz: f32) -> u32 {
    let nz = nz as f64;
    let nx = nx as f64;
    let ny = ny as f64;
    let flip: u32 = if nz >= 0.0 { 1 } else { 0 };

    let nxxyy = (1.0 - nz.abs()) * 2.0;
    let nw = f64::sqrt(f64::max(0.0, 1.0 - 0.25 * nxxyy));
    let (nx2, ny2) = if nw > 0.0 { (nx / nw, ny / nw) } else { (nx, ny) };

    let c = std::f64::consts::SQRT_2 / (0x3FF as f64 / 2.0);
    let n1 = ((nx2 + std::f64::consts::SQRT_2) / c).round() as u32 & 0x3FF;
    let n2 = ((ny2 + std::f64::consts::SQRT_2) / c).round() as u32 & 0x3FF;

    (flip << 31) | (n2 << 10) | n1
}

/// Encode a vertex's packed normal *and* tangent into the RCRA 16-byte vertex
/// layout: the upper bits of the `nxyz` u32 (bits 20..30) hold tangent-X +
/// tangent-Z-sign, and the i16 `W` field holds tangent-Y + bitangent-sign.
///
/// `n` is the per-vertex normal, `t` the accumulated tangent, `b` the
/// accumulated bitangent (as produced by the Mikktspace-ish triangle loop in
/// `calculate_tangents`). All three are in object space.
pub(crate) fn encode_normal_with_tangent(
    n: (f32, f32, f32),
    t: (f32, f32, f32),
    b: (f32, f32, f32),
) -> (u32, i16) {
    // Project tangent onto the plane perpendicular to the normal.
    let dot_nt = (n.0 * t.0 + n.1 * t.1 + n.2 * t.2) as f64;
    let proj_t = (
        t.0 as f64 - n.0 as f64 * dot_nt,
        t.1 as f64 - n.1 as f64 * dot_nt,
        t.2 as f64 - n.2 as f64 * dot_nt,
    );
    let tlen = (proj_t.0 * proj_t.0 + proj_t.1 * proj_t.1 + proj_t.2 * proj_t.2).sqrt();
    let thist = if tlen > 1e-20 {
        (proj_t.0 / tlen, proj_t.1 / tlen, proj_t.2 / tlen)
    } else {
        (1.0_f64, 0.0, 0.0)
    };

    // Bitangent sign: sign( dot( cross(t, b), n ) )
    let raw_tlen = ((t.0 * t.0 + t.1 * t.1 + t.2 * t.2) as f64).sqrt();
    let raw_blen = ((b.0 * b.0 + b.1 * b.1 + b.2 * b.2) as f64).sqrt();
    let btsign: i32 = if raw_tlen > 0.0 && raw_blen > 0.0 {
        let tn = (t.0 as f64 / raw_tlen, t.1 as f64 / raw_tlen, t.2 as f64 / raw_tlen);
        let bn = (b.0 as f64 / raw_blen, b.1 as f64 / raw_blen, b.2 as f64 / raw_blen);
        let cr = (
            tn.1 * bn.2 - tn.2 * bn.1,
            tn.2 * bn.0 - tn.0 * bn.2,
            tn.0 * bn.1 - tn.1 * bn.0,
        );
        let bts = cr.0 * n.0 as f64 + cr.1 * n.1 as f64 + cr.2 * n.2 as f64;
        if bts > 0.0 { 1 } else { -1 }
    } else {
        0
    };

    // Hemisphere-projection octahedral for the tangent
    let sqr2 = std::f64::consts::SQRT_2 * 2.0;
    let (t3, tsign) = if thist.2 < 0.0 { (-thist.2, -1i32) } else { (thist.2, 1i32) };
    let sqrxy = f64::sqrt(f64::max(0.0, 1.0 - (1.0 - t3) / 2.0));
    let (tx, ty) = if sqrxy > 0.0 {
        (thist.0 / sqrxy, thist.1 / sqrxy)
    } else {
        (thist.0, thist.1)
    };
    let tn1 = (tx / sqr2 + 0.5).clamp(0.0, 1.0);
    let tn2 = (ty / sqr2 + 0.5).clamp(0.0, 1.0);
    let tn1_bits = ((tn1 * 1023.0).round() as u32) & 0x3FF;
    let tn2_bits = ((tn2 * 1023.0).round() as u32) & 0x3FF;

    // Start from the octahedral normal packing, then overlay tangent data in
    // bits 20..30 (clearing any stale values first; encode_normal leaves them 0
    // already, but be explicit for safety).
    let mut nxyz = encode_normal(n.0, n.1, n.2);
    nxyz &= !(0x7FFu32 << 20);
    nxyz |= tn1_bits << 20;
    if tsign > 0 {
        nxyz |= 1u32 << 30;
    }

    // W i16: tangent-Y (10 bits) | 0x7C00, negated (two's complement) iff
    // bitangent sign is positive.
    let mut w_val: i32 = tn2_bits as i32;
    w_val |= 0x7C00;
    if btsign > 0 {
        w_val = (!w_val).wrapping_add(1);
    }
    let w = w_val.clamp(i16::MIN as i32, i16::MAX as i32) as i16;

    (nxyz, w)
}

// UV1 Section 

pub struct Uv1Section {
    pub uvs: Vec<(i16, i16)>,
}

impl Uv1Section {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let count = data.len() / 4;
        let mut cur = Cursor::new(data);
        let mut uvs = Vec::with_capacity(count);
        for _ in 0..count {
            let u = cur.read_i16::<LE>()?;
            let v = cur.read_i16::<LE>()?;
            uvs.push((u, v));
        }
        Ok(Self { uvs })
    }

    pub fn get_uv(&self, index: usize) -> (f32, f32) {
        let (u, v) = self.uvs[index];
        (u as f32 / 32768.0, v as f32 / 32768.0)
    }

    pub fn set_uv(&mut self, index: usize, u: f32, v: f32) {
        self.uvs[index] = ((u * 32768.0) as i16, (v * 32768.0) as i16);
    }

    pub fn save(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.uvs.len() * 4);
        for (u, v) in &self.uvs {
            out.extend_from_slice(&u.to_le_bytes());
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }
}
