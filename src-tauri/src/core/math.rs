pub type Vec3 = (f64, f64, f64);
pub type Quat = (f64, f64, f64, f64); // x, y, z, w

pub fn quat_mul(q: Quat, p: Quat) -> Quat {
    let (aq, bq, cq, dq) = q;
    let (ap, bp, cp, dp) = p;
    (
        dq * ap + aq * dp + bq * cp - cq * bp,
        dq * bp - aq * cp + bq * dp + cq * ap,
        dq * cp + aq * bp - bq * ap + cq * dp,
        dq * dp - aq * ap - bq * bp - cq * cp,
    )
}

pub fn quat_mul_vec(q: Quat, v: Vec3) -> (f64, f64, f64, f64) {
    let (ax, ay, az, aw) = q;
    let (bx, by, bz) = v;
    (
        aw * bx + ay * bz - az * by,
        aw * by - ax * bz + az * bx,
        aw * bz + ax * by - ay * bx,
        -ax * bx - ay * by - az * bz,
    )
}

pub fn quat_inv(q: Quat) -> Quat {
    let (x, y, z, w) = q;
    (-x, -y, -z, w)
}

pub fn rotate_vec(v: Vec3, q: Quat) -> Vec3 {
    let t = quat_mul_vec(q, v);
    let t_q = (t.0, t.1, t.2, t.3);
    let r = quat_mul(t_q, quat_inv(q));
    (r.0, r.1, r.2)
}

pub fn vec_add(a: Vec3, b: Vec3) -> Vec3 {
    (a.0 + b.0, a.1 + b.1, a.2 + b.2)
}

pub fn decode_normal(norm: u32) -> (f32, f32, f32) {
    let norm = norm & 0xFFFF_FFFF;
    let nx = (norm & 0x3FF) as f64 * 0.002_764_835_95 - std::f64::consts::SQRT_2;
    let ny = ((norm >> 10) & 0x3FF) as f64 * 0.002_764_835_95 - std::f64::consts::SQRT_2;
    let flip = (norm >> 31) == 0;

    let nxxyy = nx * nx + ny * ny;
    let nw = f64::sqrt(f64::max(0.0, 1.0 - 0.25 * nxxyy));

    let nx = (nx * nw) as f32;
    let ny = (ny * nw) as f32;
    let nz_raw = (1.0 - 0.5 * nxxyy) as f32;
    let nz = if flip { -nz_raw } else { nz_raw };

    (nx, ny, nz)
}
