//! The Havok 2017.1 tagfile in a zone's tail, read just far enough to move static collision with
//! the model it belongs to. Layout and invariants: docs/ZONE_FORMAT.md "Static collision".

use std::collections::HashMap;

use byteorder::{ByteOrder, BE, LE};

use crate::core::error::{Result, ToolkitError};

const INSTANCE_LEN: usize = 0x70;
const NODE_LEN: usize = 0x70;
/// Matching and verification tolerance, in metres; shipped values agree to ~1e-5.
const TOL: f32 = 1e-3;

fn bad(msg: impl std::fmt::Display) -> ToolkitError {
    ToolkitError::Parse(format!("havok: {msg}"))
}

fn f32_at(buf: &[u8], at: usize) -> f32 {
    LE::read_f32(&buf[at..at + 4])
}

fn vec3(buf: &[u8], at: usize) -> [f32; 3] {
    [f32_at(buf, at), f32_at(buf, at + 4), f32_at(buf, at + 8)]
}

fn put_vec3(buf: &mut [u8], at: usize, v: [f32; 3]) {
    for (k, x) in v.iter().enumerate() {
        LE::write_f32(&mut buf[at + 4 * k..at + 4 * k + 4], *x);
    }
}

/// Tagfile variable-length int: the leading bits of the first byte give the length.
fn packed(buf: &[u8], p: &mut usize) -> Result<u64> {
    let b = *buf.get(*p).ok_or_else(|| bad("packed int past chunk end"))?;
    let (n, mask): (usize, u64) = match b >> 3 {
        0x00..=0x0F => (1, 0x7F),
        0x10..=0x17 => (2, 0x3FFF),
        0x18..=0x1B => (3, 0x1F_FFFF),
        0x1C => (4, 0x7FF_FFFF),
        0x1D => (5, 0x7_FFFF_FFFF),
        0x1E => (8, 0x07FF_FFFF_FFFF_FFFF),
        _ => return Err(bad(format!("packed int prefix {b:#x}"))),
    };
    let bytes = buf.get(*p..*p + n).ok_or_else(|| bad("packed int past chunk end"))?;
    *p += n;
    Ok(bytes.iter().fold(0u64, |v, &x| v << 8 | x as u64) & mask)
}

/// Every chunk under `TAG0`: tag -> (payload start, end) in `buf`.
fn chunks(buf: &[u8], start: usize) -> Result<HashMap<[u8; 4], (usize, usize)>> {
    fn header(buf: &[u8], p: usize) -> Result<(usize, [u8; 4])> {
        let h = buf.get(p..p + 8).ok_or_else(|| bad("chunk header past end"))?;
        let size = (BE::read_u32(&h[0..4]) & 0x3FFF_FFFF) as usize;
        if size < 8 || p + size > buf.len() {
            return Err(bad(format!("chunk size {size} at {p:#x}")));
        }
        Ok((size, [h[4], h[5], h[6], h[7]]))
    }
    fn walk(buf: &[u8], p: usize, out: &mut HashMap<[u8; 4], (usize, usize)>) -> Result<()> {
        let (size, _) = header(buf, p)?;
        let mut q = p + 8;
        while q + 8 <= p + size {
            let (n, tag) = header(buf, q)?;
            out.insert(tag, (q + 8, q + n));
            if &tag == b"TYPE" || &tag == b"INDX" {
                walk(buf, q, out)?;
            }
            q += n;
        }
        Ok(())
    }
    let mut out = HashMap::new();
    walk(buf, start, &mut out)?;
    Ok(out)
}

struct Item {
    ty: usize,
    at: usize,
    count: usize,
}

struct Compound {
    at: usize,
    elems_item: usize,
    points_item: usize,
    elems: usize,
    count: usize,
    nodes: usize,
    node_count: usize,
    points: usize,
    point_count: usize,
    /// instance -> (node, lane) of its leaf
    leaf: HashMap<usize, (usize, usize)>,
    /// child node -> (node, lane) pointing at it
    parent: HashMap<usize, (usize, usize)>,
    root: usize,
}

/// Static collision of one zone: its compound shapes and their SIMD bounding trees.
pub struct ZoneCollision {
    names: Vec<String>,
    items: Vec<Item>,
    compounds: Vec<Compound>,
    /// Start of the `TAG0` chunk header in the tail.
    tag0: usize,
    data_at: usize,
    /// `(type, DATA offsets)` per PTCH group: pointer fields holding item indices.
    patches: Vec<(u32, Vec<u32>)>,
}

/// A tail grown by [`ZoneCollision::add_copies`].
pub struct Grown {
    pub tail: Vec<u8>,
    /// How much block 1 (the "Zone PID Physics" DAT1) grew; the asset header's size word follows.
    pub delta: usize,
    pub pieces: usize,
    pub warnings: Vec<String>,
}

impl ZoneCollision {
    /// `Ok(None)` when the tail carries no tagfile (the zone has no static collision).
    pub fn parse(tail: &[u8]) -> Result<Option<Self>> {
        let Some(tag0) = tail.windows(4).position(|w| w == b"TAG0") else {
            return Ok(None);
        };
        let tag0 = tag0.checked_sub(4).ok_or_else(|| bad("TAG0 at tail start"))?;
        let ch = chunks(tail, tag0)?;
        let get = |tags: &[&[u8; 4]]| {
            tags.iter()
                .find_map(|t| ch.get(*t).copied())
                .ok_or_else(|| bad(format!("missing chunk {}", String::from_utf8_lossy(tags[0]))))
        };
        let (data_at, data_end) = get(&[b"DATA"])?;

        let strings = |(a, b): (usize, usize)| -> Vec<String> {
            tail[a..b].split(|&c| c == 0).map(|s| String::from_utf8_lossy(s).into_owned()).collect()
        };
        let tstr = strings(get(&[b"TSTR", b"TST1"])?);

        let (a, b) = get(&[b"TNA1", b"TNAM"])?;
        let tna = &tail[a..b];
        let mut p = 0;
        let count = packed(tna, &mut p)? as usize;
        if count > 1 << 16 {
            return Err(bad(format!("{count} types")));
        }
        let mut names = vec![String::new(); count];
        for name in names.iter_mut().skip(1) {
            *name = tstr.get(packed(tna, &mut p)? as usize).cloned().unwrap_or_default();
            for _ in 0..packed(tna, &mut p)? {
                packed(tna, &mut p)?;
                packed(tna, &mut p)?;
            }
        }

        let (a, b) = get(&[b"TBDY", b"TBOD"])?;
        let body = &tail[a..b];
        let mut parent = vec![0usize; count];
        let mut size = vec![None; count];
        let mut p = 0;
        while p < body.len() {
            let idx = packed(body, &mut p)? as usize;
            if idx == 0 {
                continue;
            }
            if idx >= count {
                return Err(bad(format!("type body for type {idx}")));
            }
            parent[idx] = packed(body, &mut p)? as usize;
            let opt = packed(body, &mut p)?;
            for bit in [0x01, 0x02, 0x04] {
                if opt & bit != 0 {
                    packed(body, &mut p)?;
                }
            }
            if opt & 0x08 != 0 {
                size[idx] = Some(packed(body, &mut p)? as usize);
                packed(body, &mut p)?;
            }
            if opt & 0x10 != 0 {
                packed(body, &mut p)?;
            }
            if opt & 0x20 != 0 {
                // Low 16 bits are the field count; hkPropertyId sets 0x30000 above it.
                for _ in 0..(packed(body, &mut p)? & 0xFFFF) * 4 {
                    packed(body, &mut p)?;
                }
            }
            if opt & 0x40 != 0 {
                for _ in 0..packed(body, &mut p)? * 2 {
                    packed(body, &mut p)?;
                }
            }
            if opt & 0x80 != 0 {
                packed(body, &mut p)?;
            }
        }
        let size_of = |mut t: usize| {
            for _ in 0..64 {
                if size[t].is_some() || parent[t] == 0 {
                    return size[t];
                }
                t = parent[t];
            }
            None
        };

        let (a, b) = get(&[b"ITEM"])?;
        let mut items = Vec::with_capacity((b - a) / 12);
        for e in tail[a..b].chunks_exact(12) {
            let ty = (LE::read_u32(&e[0..4]) & 0x00FF_FFFF) as usize;
            let at = data_at + LE::read_u32(&e[4..8]) as usize;
            let count = LE::read_u32(&e[8..12]) as usize;
            let span = if ty < names.len() { size_of(ty).unwrap_or(0).saturating_mul(count) } else { usize::MAX };
            if at.saturating_add(span) > data_end {
                return Err(bad(format!("item {} out of range", items.len())));
            }
            items.push(Item { ty, at, count });
        }

        let (a, b) = get(&[b"PTCH"])?;
        let mut patches = Vec::new();
        let mut p = a;
        while p + 8 <= b {
            let (ty, n) = (LE::read_u32(&tail[p..]), LE::read_u32(&tail[p + 4..]) as usize);
            let end = p + 8 + 4 * n;
            if end > b {
                return Err(bad("patch group past PTCH end"));
            }
            patches.push((ty, tail[p + 8..end].chunks_exact(4).map(LE::read_u32).collect()));
            p = end;
        }

        let mut col = ZoneCollision { names, items, compounds: Vec::new(), tag0, data_at, patches };
        let sized = |item: &Item, want: usize, what: &str| {
            if size_of(item.ty) == Some(want) {
                Ok(())
            } else {
                Err(bad(format!("{what} is {:?} bytes, expected {want:#x}", size_of(item.ty))))
            }
        };
        for i in 1..col.items.len() {
            let item = &col.items[i];
            if col.names[item.ty] != "hknpCompoundShape" {
                continue;
            }
            sized(item, 0xC0, "hknpCompoundShape")?;
            let at = item.at;
            let elems_item = LE::read_u64(&tail[at + 0x50..]) as usize;
            let elems = col.item_at(tail, at + 0x50)?;
            sized(elems, INSTANCE_LEN, "compound instance")?;
            let data = col.item_at(tail, at + 0xB8)?;
            sized(data, 0x68, "hknpCompoundShapeData")?;
            if tail[data.at + 0x60] != 1 {
                return Err(bad("compound uses the dynamic bounding tree, which is not supported"));
            }
            let nodes = col.item_at(tail, data.at + 0x40)?;
            sized(nodes, NODE_LEN, "hkcdSimdTree::Node")?;
            let points_item = LE::read_u64(&tail[data.at + 0x50..]) as usize;
            let points = col.item_at(tail, data.at + 0x50)?;
            sized(points, 16, "tree point")?;
            let mut c = Compound {
                at,
                elems_item,
                points_item,
                elems: elems.at,
                count: elems.count,
                nodes: nodes.at,
                node_count: nodes.count,
                points: points.at,
                point_count: points.count,
                leaf: HashMap::new(),
                parent: HashMap::new(),
                root: 0,
            };
            for n in 0..c.node_count {
                for j in 0..4 {
                    if c.lane_empty(tail, n, j) {
                        continue;
                    }
                    let v = LE::read_u32(&tail[c.nodes + n * NODE_LEN + 0x60 + 4 * j..]) as usize;
                    let map = if v & 1 == 1 { &mut c.leaf } else { &mut c.parent };
                    map.insert(v >> 1, (n, j));
                }
            }
            c.root = (1..c.node_count)
                .find(|n| !c.parent.contains_key(n))
                .ok_or_else(|| bad("bounding tree has no root"))?;
            col.compounds.push(c);
        }
        Ok(Some(col))
    }

    /// The item a tagfile pointer field (an item index) refers to.
    fn item_at(&self, buf: &[u8], field: usize) -> Result<&Item> {
        let idx = LE::read_u64(&buf[field..field + 8]) as usize;
        self.items.get(idx).filter(|_| idx != 0).ok_or_else(|| bad(format!("pointer to item {idx}")))
    }

    /// Start of the mesh data whose domain box bounds this instance's shape, or `None` for a
    /// shape kind this module can't bound (convex polytopes).
    fn mesh_data(&self, buf: &[u8], rec: usize) -> Result<Option<usize>> {
        let shape = self.item_at(buf, rec + 0x50)?;
        if self.names[shape.ty] != "hknpCompressedMeshShape" {
            return Ok(None);
        }
        let data = self.item_at(buf, shape.at + 0x50)?;
        if self.names[data.ty] != "hknpCompressedMeshShapeData" {
            return Err(bad("mesh shape without mesh data"));
        }
        Ok(Some(data.at))
    }

    /// Move every collision piece placed with the model's `old` zone matrix to `new`, then refit
    /// the bounding tree. Nothing is written unless every matched piece can be refit.
    pub fn move_with(&self, buf: &mut [u8], old: &[f32; 16], new: &[f32; 16]) -> Result<usize> {
        let hits = self.pieces_at(buf, old)?;
        for &(ci, rec, data) in &hits {
            place(buf, rec, new)?;
            self.compounds[ci].refit(buf, rec, data)?;
        }
        Ok(hits.len())
    }

    /// `(compound, record, mesh data)` of every piece placed with the zone matrix `m`. Errors when
    /// one of them can't be bounded, so callers can refuse before writing anything.
    fn pieces_at(&self, buf: &[u8], m: &[f32; 16]) -> Result<Vec<(usize, usize, usize)>> {
        let mut hits = Vec::new();
        for (ci, c) in self.compounds.iter().enumerate() {
            for i in 0..c.count {
                let rec = c.elems + i * INSTANCE_LEN;
                if (0..3).any(|a| f32_at(buf, rec + 0x30 + 4 * a).to_bits() != m[12 + a].to_bits()) {
                    continue;
                }
                let s = vec3(buf, rec + 0x40);
                let same_rotation = (0..3).all(|r| {
                    (0..3).all(|a| (f32_at(buf, rec + 16 * r + 4 * a) * s[r] - m[4 * r + a]).abs() <= TOL)
                });
                if same_rotation {
                    let data = self
                        .mesh_data(buf, rec)?
                        .ok_or_else(|| bad("a piece uses a convex shape, which can't be refit yet"))?;
                    hits.push((ci, rec, data));
                }
            }
        }
        Ok(hits)
    }

    /// Copy the collision of the model at each `(source, copy)` zone matrix pair to the copy,
    /// growing the tagfile: the new pieces reuse the source pieces' shapes, go at the end of the
    /// compound's instance array, and get a tree point and a leaf in a free lane.
    pub fn add_copies(tail: &[u8], copies: &[([f32; 16], [f32; 16])]) -> Result<Grown> {
        let mut grown = Grown { tail: tail.to_vec(), delta: 0, pieces: 0, warnings: Vec::new() };
        let Some(col) = Self::parse(tail)? else {
            return Ok(grown);
        };
        let mut per: Vec<Vec<(usize, [f32; 16])>> = vec![Vec::new(); col.compounds.len()];
        for (src, dst) in copies {
            match col.pieces_at(tail, src) {
                Ok(hits) => {
                    for (ci, rec, _) in hits {
                        per[ci].push(((rec - col.compounds[ci].elems) / INSTANCE_LEN, *dst));
                    }
                }
                Err(e) => grown.warnings.push(format!(
                    "model at ({:.2}, {:.2}, {:.2}): collision not copied ({e})",
                    src[12], src[13], src[14]
                )),
            }
        }
        for (ci, list) in per.iter().enumerate().filter(|(_, l)| !l.is_empty()) {
            // Every growth moves offsets, so each compound is grown from a fresh parse.
            let col = Self::parse(&grown.tail)?.ok_or_else(|| bad("tagfile vanished"))?;
            let sources: Vec<usize> = list.iter().map(|(i, _)| *i).collect();
            let (tail, delta, first, first_point) = col.grow(&grown.tail, ci, &sources)?;
            grown.tail = tail;
            grown.delta += delta;
            let col = Self::parse(&grown.tail)?.ok_or_else(|| bad("tagfile vanished"))?;
            let c = &col.compounds[ci];
            for (e, (si, dst)) in list.iter().enumerate() {
                let (rec, src) = (c.elems + (first + e) * INSTANCE_LEN, c.elems + si * INSTANCE_LEN);
                grown.tail.copy_within(src..src + INSTANCE_LEN, rec);
                place(&mut grown.tail, rec, dst)?;
                LE::write_u32(&mut grown.tail[rec + 0x3C..rec + 0x40], 0x3F00_0000 | (first_point + e) as u32);
                let data = col.mesh_data(&grown.tail, rec)?.ok_or_else(|| bad("copied piece lost its mesh"))?;
                c.add_leaf(&mut grown.tail, first + e, data)?;
            }
            grown.pieces += list.len();
        }
        Ok(grown)
    }

    /// The tail with room for one more instance and tree point per source in compound `ci`, plus
    /// `(tail, growth, first new instance, first new point)`. New records are zeroed except their
    /// `shape` pointer's patch entry, which joins the group its source's pointer is in.
    fn grow(&self, tail: &[u8], ci: usize, sources: &[usize]) -> Result<(Vec<u8>, usize, usize, usize)> {
        let c = &self.compounds[ci];
        let n = sources.len();
        if c.count + n > 1usize << tail[c.at + 0x23] {
            return Err(bad("more instances than the compound's shape key bits allow"));
        }
        let rel = |abs: usize| u32::try_from(abs - self.data_at).map_err(|_| bad("DATA offset too large"));
        // New instances go after the last instance, new points after the last point.
        let p1 = rel(c.elems + c.count * INSTANCE_LEN)?;
        let mut inserts = [(p1, n * INSTANCE_LEN), (rel(c.points + c.point_count * 16)?, n * 16)];
        inserts.sort();
        let shift = |off: u32| off + inserts.iter().filter(|(p, _)| off >= *p).map(|(_, l)| *l as u32).sum::<u32>();
        let first_rel = p1 + inserts.iter().filter(|(p, _)| *p < p1).map(|(_, l)| *l as u32).sum::<u32>();
        let src_field = rel(c.elems + sources[0] * INSTANCE_LEN + 0x50)?;
        let group = self
            .patches
            .iter()
            .position(|(_, offs)| offs.binary_search(&src_field).is_ok())
            .ok_or_else(|| bad("source piece's shape pointer has no patch entry"))?;

        let mut tag0 = Vec::new();
        let (flags, kids) = children(tail, self.tag0)?;
        for (tag, fl, a, b) in kids {
            let payload = match &tag {
                b"DATA" => {
                    let mut out = Vec::with_capacity(b - a + n * (INSTANCE_LEN + 16));
                    let mut last = 0usize;
                    for &(p, l) in &inserts {
                        out.extend_from_slice(&tail[a + last..a + p as usize]);
                        out.resize(out.len() + l, 0);
                        last = p as usize;
                    }
                    out.extend_from_slice(&tail[a + last..b]);
                    out
                }
                b"INDX" => {
                    let mut out = Vec::new();
                    for (stag, sfl, sa, sb) in children(tail, a - 8)?.1 {
                        let spay = match &stag {
                            b"ITEM" => {
                                let mut items = tail[sa..sb].to_vec();
                                for (idx, e) in items.chunks_exact_mut(12).enumerate() {
                                    if idx != 0 {
                                        let off = LE::read_u32(&e[4..8]);
                                        LE::write_u32(&mut e[4..8], shift(off));
                                    }
                                    if idx == c.elems_item || idx == c.points_item {
                                        let cnt = LE::read_u32(&e[8..12]);
                                        LE::write_u32(&mut e[8..12], cnt + n as u32);
                                    }
                                }
                                items
                            }
                            b"PTCH" => {
                                let mut groups = Vec::new();
                                for (g, (ty, offs)) in self.patches.iter().enumerate() {
                                    let mut offs: Vec<u32> = offs.iter().map(|&o| shift(o)).collect();
                                    if g == group {
                                        offs.extend((0..n).map(|e| first_rel + (e * INSTANCE_LEN) as u32 + 0x50));
                                        offs.sort_unstable();
                                    }
                                    groups.extend_from_slice(&ty.to_le_bytes());
                                    groups.extend_from_slice(&(offs.len() as u32).to_le_bytes());
                                    offs.iter().for_each(|o| groups.extend_from_slice(&o.to_le_bytes()));
                                }
                                groups
                            }
                            _ => tail[sa..sb].to_vec(),
                        };
                        out.extend(chunk(&stag, sfl, &spay));
                    }
                    out
                }
                _ => tail[a..b].to_vec(),
            };
            tag0.extend(chunk(&tag, fl, &payload));
        }
        let tag0 = chunk(b"TAG0", flags, &tag0);

        // Block 1 is a DAT1 whose one section, "Zone PID Physics", is the tagfile.
        if tail.len() < 16 || LE::read_u32(&tail[0..4]) != crate::core::dat1::DAT1_MAGIC {
            return Err(bad("the tagfile is not inside a DAT1 block"));
        }
        let old_len = (BE::read_u32(&tail[self.tag0..]) & 0x3FFF_FFFF) as usize;
        let delta = tag0.len() - old_len;
        let mut out = Vec::with_capacity(tail.len() + delta);
        out.extend_from_slice(&tail[..self.tag0]);
        out.extend_from_slice(&tag0);
        out.extend_from_slice(&tail[self.tag0 + old_len..]);
        let sections = LE::read_u16(&out[12..14]) as usize;
        let mut found = false;
        for s in 0..sections {
            let h = 16 + 12 * s;
            if LE::read_u32(&out[h + 4..]) as usize == self.tag0 {
                let size = LE::read_u32(&out[h + 8..]);
                LE::write_u32(&mut out[h + 8..h + 12], size + delta as u32);
                found = true;
            }
        }
        if !found {
            return Err(bad("no DAT1 section starts at the tagfile"));
        }
        let total = LE::read_u32(&out[8..12]);
        LE::write_u32(&mut out[8..12], total + delta as u32);
        Ok((out, delta, c.count, c.point_count))
    }

    /// The invariants every shipped tree satisfies; checked after a refit, before saving.
    pub fn verify(&self, buf: &[u8]) -> Result<()> {
        fn near(a: &[f32], b: &[f32]) -> bool {
            a.iter().zip(b).all(|(x, y)| (x - y).abs() <= TOL)
        }
        for c in &self.compounds {
            if c.leaf.len() != c.count {
                return Err(bad(format!("{} leaves for {} instances", c.leaf.len(), c.count)));
            }
            for (&i, &(n, j)) in &c.leaf {
                let rec = c.elems + i * INSTANCE_LEN;
                if i >= c.count {
                    return Err(bad(format!("leaf for missing instance {i}")));
                }
                if let Some(data) = self.mesh_data(buf, rec)? {
                    let b = leaf_box(buf, rec, data);
                    if !near(&b, &c.lane(buf, n, j)) {
                        return Err(bad(format!("leaf box of instance {i}")));
                    }
                    let k = c.point_index(buf, rec)?;
                    let pt = vec3(buf, c.points + 16 * k);
                    if !near(&pt, &[b[0] + b[1], b[2] + b[3], b[4] + b[5]]) {
                        return Err(bad(format!("tree point {k}")));
                    }
                }
            }
            for (&child, &(n, j)) in &c.parent {
                if !near(&c.lane(buf, n, j), &c.union(buf, child)?) {
                    return Err(bad(format!("node {n} lane {j}")));
                }
            }
            let u = c.union(buf, c.root)?;
            let (lo, hi) = (vec3(buf, c.at + 0x80), vec3(buf, c.at + 0x90));
            if !near(&lo, &[u[0], u[2], u[4]]) || !near(&hi, &[u[1], u[3], u[5]]) {
                return Err(bad("compound aabb"));
            }
        }
        Ok(())
    }
}

/// `(flags, [(tag, flags, payload start, payload end)])` of the chunks directly inside the chunk at `p`.
#[allow(clippy::type_complexity)]
fn children(buf: &[u8], p: usize) -> Result<(u32, Vec<([u8; 4], u32, usize, usize)>)> {
    let word = BE::read_u32(&buf[p..p + 4]);
    let end = p + (word & 0x3FFF_FFFF) as usize;
    let mut out = Vec::new();
    let mut q = p + 8;
    while q + 8 <= end {
        let w = BE::read_u32(&buf[q..q + 4]);
        let n = (w & 0x3FFF_FFFF) as usize;
        if n < 8 || q + n > end {
            return Err(bad(format!("chunk size {n} at {q:#x}")));
        }
        out.push(([buf[q + 4], buf[q + 5], buf[q + 6], buf[q + 7]], w & 0xC000_0000, q + 8, q + n));
        q += n;
    }
    Ok((word & 0xC000_0000, out))
}

fn chunk(tag: &[u8; 4], flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&(flags | (payload.len() as u32 + 8)).to_be_bytes());
    out.extend_from_slice(tag);
    out.extend_from_slice(payload);
    out
}

/// Write a zone matrix into an instance: unit rotation columns, translation, per-axis scale (the
/// 4th float of each row is other data and is kept).
fn place(buf: &mut [u8], rec: usize, m: &[f32; 16]) -> Result<()> {
    for r in 0..3 {
        let row = [m[4 * r], m[4 * r + 1], m[4 * r + 2]];
        let s = row.iter().map(|x| x * x).sum::<f32>().sqrt();
        if !(s > 1e-6) {
            return Err(bad("matrix has a zero-length axis"));
        }
        put_vec3(buf, rec + 16 * r, row.map(|x| x / s));
        LE::write_f32(&mut buf[rec + 0x40 + 4 * r..rec + 0x44 + 4 * r], s);
    }
    put_vec3(buf, rec + 0x30, [m[12], m[13], m[14]]);
    Ok(())
}

/// World box of an instance: the mesh's local domain corners under scale, rotation, translation.
fn leaf_box(buf: &[u8], rec: usize, data: usize) -> [f32; 6] {
    let (lo, hi) = (vec3(buf, data + 0x30), vec3(buf, data + 0x40));
    let cols = [vec3(buf, rec), vec3(buf, rec + 0x10), vec3(buf, rec + 0x20)];
    let (t, s) = (vec3(buf, rec + 0x30), vec3(buf, rec + 0x40));
    let mut b = [f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY];
    for corner in 0..8 {
        let pick = |bit: usize, a: usize| (if corner & bit == 0 { lo[a] } else { hi[a] }) * s[a];
        let p = [pick(1, 0), pick(2, 1), pick(4, 2)];
        for a in 0..3 {
            let w = p[0] * cols[0][a] + p[1] * cols[1][a] + p[2] * cols[2][a] + t[a];
            b[2 * a] = b[2 * a].min(w);
            b[2 * a + 1] = b[2 * a + 1].max(w);
        }
    }
    b
}

impl Compound {
    /// One lane of a node as lx, hx, ly, hy, lz, hz (`hkcdFourAabb` is six 4-wide vectors).
    fn lane(&self, buf: &[u8], n: usize, j: usize) -> [f32; 6] {
        let base = self.nodes + n * NODE_LEN + 4 * j;
        std::array::from_fn(|k| f32_at(buf, base + 16 * k))
    }

    fn lane_empty(&self, buf: &[u8], n: usize, j: usize) -> bool {
        let b = self.lane(buf, n, j);
        b[0] > b[1]
    }

    fn set_lane(&self, buf: &mut [u8], n: usize, j: usize, b: &[f32; 6]) {
        let base = self.nodes + n * NODE_LEN + 4 * j;
        for (k, x) in b.iter().enumerate() {
            LE::write_f32(&mut buf[base + 16 * k..base + 16 * k + 4], *x);
        }
    }

    fn union(&self, buf: &[u8], n: usize) -> Result<[f32; 6]> {
        let mut u = [f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY];
        let mut any = false;
        for j in 0..4 {
            if self.lane_empty(buf, n, j) {
                continue;
            }
            let b = self.lane(buf, n, j);
            for a in 0..3 {
                u[2 * a] = u[2 * a].min(b[2 * a]);
                u[2 * a + 1] = u[2 * a + 1].max(b[2 * a + 1]);
            }
            any = true;
        }
        if any { Ok(u) } else { Err(bad(format!("node {n} has no boxes"))) }
    }

    fn point_index(&self, buf: &[u8], rec: usize) -> Result<usize> {
        let k = (LE::read_u32(&buf[rec + 0x3C..rec + 0x40]) & 0x00FF_FFFF) as usize;
        if k < self.point_count { Ok(k) } else { Err(bad(format!("tree point {k} out of range"))) }
    }

    /// New leaf box, its tree point, every ancestor lane up to the root, and the compound aabb.
    fn refit(&self, buf: &mut [u8], rec: usize, data: usize) -> Result<()> {
        let i = (rec - self.elems) / INSTANCE_LEN;
        let b = leaf_box(buf, rec, data);
        let &(n, j) = self.leaf.get(&i).ok_or_else(|| bad(format!("instance {i} has no leaf")))?;
        self.set_lane(buf, n, j, &b);
        let k = self.point_index(buf, rec)?;
        put_vec3(buf, self.points + 16 * k, [b[0] + b[1], b[2] + b[3], b[4] + b[5]]);
        self.propagate(buf, n)
    }

    /// Give a new instance a leaf in the free trailing lane whose node grows least, then refit.
    fn add_leaf(&self, buf: &mut [u8], i: usize, data: usize) -> Result<()> {
        let rec = self.elems + i * INSTANCE_LEN;
        let b = leaf_box(buf, rec, data);
        let volume = |u: &[f32; 6]| (u[1] - u[0]).max(0.0) * (u[3] - u[2]).max(0.0) * (u[5] - u[4]).max(0.0);
        let mut best: Option<(f32, usize, usize)> = None;
        for n in 1..self.node_count {
            // Empty lanes are always trailing; a node with lane 0 empty is unused.
            let Some(j) = (1..4).find(|&j| self.lane_empty(buf, n, j)) else { continue };
            if self.lane_empty(buf, n, 0) {
                continue;
            }
            let u = self.union(buf, n)?;
            let mut g = u;
            for a in 0..3 {
                g[2 * a] = g[2 * a].min(b[2 * a]);
                g[2 * a + 1] = g[2 * a + 1].max(b[2 * a + 1]);
            }
            let cost = volume(&g) - volume(&u);
            if best.map_or(true, |(c, _, _)| cost < c) {
                best = Some((cost, n, j));
            }
        }
        let (_, n, j) = best.ok_or_else(|| bad("the bounding tree has no free lane left"))?;
        self.set_lane(buf, n, j, &b);
        let at = self.nodes + n * NODE_LEN + 0x60 + 4 * j;
        LE::write_u32(&mut buf[at..at + 4], ((i << 1) | 1) as u32);
        let k = self.point_index(buf, rec)?;
        put_vec3(buf, self.points + 16 * k, [b[0] + b[1], b[2] + b[3], b[4] + b[5]]);
        self.propagate(buf, n)
    }

    /// Re-derive every lane above node `n` and the compound aabb.
    fn propagate(&self, buf: &mut [u8], mut n: usize) -> Result<()> {
        for _ in 0..self.node_count {
            let Some(&(pn, pj)) = self.parent.get(&n) else { break };
            let u = self.union(buf, n)?;
            self.set_lane(buf, pn, pj, &u);
            n = pn;
        }
        let u = self.union(buf, self.root)?;
        put_vec3(buf, self.at + 0x80, [u[0], u[2], u[4]]);
        put_vec3(buf, self.at + 0x90, [u[1], u[3], u[5]]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_ints_decode_by_prefix() {
        let read = |bytes: &[u8]| {
            let mut p = 0;
            (packed(bytes, &mut p).unwrap(), p)
        };
        assert_eq!(read(&[0x29]), (0x29, 1));
        assert_eq!(read(&[0x81, 0x20]), (0x120, 2));
        assert_eq!(read(&[0xC0, 0x82, 0x04]), (0x8204, 3));
        assert_eq!(read(&[0xC3, 0x00, 0x01]), (0x30001, 3));
        assert!(packed(&[0xFF], &mut 0).is_err());
    }
}
