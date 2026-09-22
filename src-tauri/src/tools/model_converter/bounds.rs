//! Geometry-derived subset and model fields that have to follow replaced geometry.

use super::sections::geo::Vertex;

#[derive(Debug, Clone, Copy)]
pub struct SubsetStats {
    pub bsphere_center: [f32; 3],
    /// Metres = raw * meters_per_unit * 2.
    pub bsphere_radius: i16,
    /// Metres = raw * meters_per_unit.
    pub aabb_extents: [i16; 3],
    /// sqrt(area in m²) in 8.8 fixed point.
    pub surface_area_sqrt: u16,
    /// UV0 units per metre along u and v (before any per-subset streaming bias).
    pub uv_density: (f32, f32),
}

fn quantized(v: &Vertex, mpu: f32) -> [f64; 3] {
    [v.x, v.y, v.z].map(|c| ((c / mpu).round() * mpu) as f64)
}

/// UV change and world distance along the line of constant `v` through the triangle's middle-`v` vertex;
/// when `v` is flat, the edge with the steepest `u` per metre.
fn triangle_du(pos: [[f64; 3]; 3], u: [f64; 3], v: [f64; 3]) -> (f64, f64) {
    const EPS: f64 = 1e-6;
    let span = |x: [f64; 3]| x.iter().cloned().fold(f64::MIN, f64::max) - x.iter().cloned().fold(f64::MAX, f64::min);
    let len = |a: [f64; 3], b: [f64; 3]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
    if span(u) < EPS {
        return (0.0, 0.0);
    }
    if span(v) >= EPS {
        let mut o = [0usize, 1, 2];
        if v[o[0]] < v[o[1]] { o.swap(0, 1); }
        if v[o[0]] < v[o[2]] { o.swap(0, 2); }
        if v[o[1]] < v[o[2]] { o.swap(1, 2); }
        let t = (v[o[1]] - v[o[0]]) / (v[o[2]] - v[o[0]]);
        let p = [0, 1, 2].map(|k| pos[o[0]][k] * (1.0 - t) + pos[o[2]][k] * t);
        let iu = u[o[0]] * (1.0 - t) + u[o[2]] * t;
        return ((u[o[1]] - iu).abs(), len(p, pos[o[1]]));
    }
    let mut best = (-1.0, 0.0, 0.0);
    for i in 0..3 {
        let j = (i + 1) % 3;
        let d = len(pos[j], pos[i]);
        if d < EPS {
            continue;
        }
        let du = (u[j] - u[i]).abs();
        if du / d > best.0 {
            best = (du / d, du, d);
        }
    }
    (best.1, best.2)
}

/// Stats for one subset. `index_base` is added to every index (the subset's first vertex when its indices are local).
pub fn subset_stats(
    vertexes: &[Vertex],
    indices: &[u16],
    vertex_start: usize,
    vertex_count: usize,
    index_start: usize,
    index_count: usize,
    index_base: usize,
    mpu: f32,
) -> Option<SubsetStats> {
    let verts = vertexes.get(vertex_start..vertex_start + vertex_count)?;
    if verts.is_empty() {
        return None;
    }
    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    for v in verts {
        let p = quantized(v, mpu);
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let c = [0, 1, 2].map(|k| (lo[k] + hi[k]) / 2.0);
    let r = verts
        .iter()
        .map(|v| {
            let p = quantized(v, mpu);
            ((p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2) + (p[2] - c[2]).powi(2)).sqrt()
        })
        .fold(0.0, f64::max);
    let q = |x: f64, unit: f64| (x / unit).ceil().clamp(0.0, i16::MAX as f64) as i16;
    let mpu64 = mpu as f64;

    let mut area = 0.0f64;
    let (mut du, mut du_dist, mut dv, mut dv_dist) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let tri = indices.get(index_start..index_start + index_count).unwrap_or(&[]);
    for t in tri.chunks_exact(3) {
        let at = |i: u16| vertexes.get(index_base + i as usize);
        let (Some(a), Some(b), Some(cc)) = (at(t[0]), at(t[1]), at(t[2])) else {
            continue;
        };
        let pos = [quantized(a, mpu), quantized(b, mpu), quantized(cc, mpu)];
        let e1 = [0, 1, 2].map(|k| pos[1][k] - pos[0][k]);
        let e2 = [0, 1, 2].map(|k| pos[2][k] - pos[0][k]);
        let cr = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        area += (cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2]).sqrt() / 2.0;
        let u = [a.u, b.u, cc.u].map(|x| x as f64);
        let v = [a.v, b.v, cc.v].map(|x| x as f64);
        let (x, d) = triangle_du(pos, u, v);
        du += x;
        du_dist += d;
        let (x, d) = triangle_du(pos, v, u);
        dv += x;
        dv_dist += d;
    }
    let density = |x: f64, d: f64| if d < 1e-6 { 10000.0 } else { (x / d) as f32 };
    Some(SubsetStats {
        bsphere_center: c.map(|x| x as f32),
        bsphere_radius: q(r, 2.0 * mpu64),
        aabb_extents: [0, 1, 2].map(|k| q((hi[k] - lo[k]) / 2.0, mpu64)),
        surface_area_sqrt: (area.sqrt() * 256.0).round().clamp(0.0, u16::MAX as f64) as u16,
        uv_density: (density(du, du_dist), density(dv, dv_dist)),
    })
}

/// The subset's texture-streaming bias, recovered as the power-of-two ratio between its stored
/// UV density and the density its own vanilla geometry yields; 1 when the ratio is not a power of two.
pub fn stream_bias(
    vertexes: &[Vertex],
    indices: &[u16],
    mesh: &super::sections::meshes::MeshDefinition,
    mpu: f32,
) -> (f32, f32) {
    let base = if mesh.has_relative_indices() { mesh.vertex_start as usize } else { 0 };
    let Some(s) = subset_stats(
        vertexes,
        indices,
        mesh.vertex_start as usize,
        mesh.vertex_count as usize,
        mesh.index_start as usize,
        mesh.index_count as usize,
        base,
        mpu,
    ) else {
        return (1.0, 1.0);
    };
    let bias = |stored: f32, computed: f32| {
        if stored <= 0.0 || computed <= 0.0 {
            return 1.0;
        }
        let l = (stored / computed).log2();
        if l.round() != 0.0 && (l - l.round()).abs() < 0.05 { l.round().exp2() } else { 1.0 }
    };
    (bias(mesh.uv_density_u, s.uv_density.0), bias(mesh.uv_density_v, s.uv_density.1))
}

/// Grows Model Built's bounding sphere and box so they also enclose `vertexes`; never shrinks them.
pub fn grow_built_bounds(built: &mut [u8], vertexes: &[Vertex], mpu: f32) {
    if built.len() < 0x28 || vertexes.is_empty() {
        return;
    }
    let f = |b: &[u8], o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as f64;
    let c0 = [f(built, 0x00), f(built, 0x04), f(built, 0x08)];
    let r0 = f(built, 0x0C);
    let e0 = [f(built, 0x10), f(built, 0x14), f(built, 0x18)];
    let mut lo = [0, 1, 2].map(|k| c0[k] - e0[k]);
    let mut hi = [0, 1, 2].map(|k| c0[k] + e0[k]);
    let mut grew = false;
    for v in vertexes {
        let p = quantized(v, mpu);
        for k in 0..3 {
            if p[k] < lo[k] {
                lo[k] = p[k];
                grew = true;
            }
            if p[k] > hi[k] {
                hi[k] = p[k];
                grew = true;
            }
        }
    }
    let c = if grew { [0, 1, 2].map(|k| (lo[k] + hi[k]) / 2.0) } else { c0 };
    let e = if grew { [0, 1, 2].map(|k| (hi[k] - lo[k]) / 2.0) } else { e0 };
    let dist = |a: [f64; 3], b: [f64; 3]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
    let reach = vertexes.iter().map(|v| dist(quantized(v, mpu), c)).fold(0.0, f64::max);
    let r = (dist(c, c0) + r0).max(reach);
    if !grew && reach <= r0 {
        return;
    }
    let mut put = |o: usize, x: f64| built[o..o + 4].copy_from_slice(&(x as f32).to_le_bytes());
    for k in 0..3 {
        put(k * 4, c[k]);
        put(0x10 + k * 4, e[k]);
    }
    put(0x0C, r);
}
