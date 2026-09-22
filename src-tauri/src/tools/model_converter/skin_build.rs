//! Rebuilds skin batches for replaced geometry the way the game's builder lays them out.

use std::collections::BTreeSet;

use crate::core::error::{Result, ToolkitError};
use super::sections::skin::{RcraSkinEntry, SkinBatch, SKIN_BATCH_JOINT_MAX, SKIN_BATCH_VERT_MAX};

/// One vertex's influences: model joint indices and weights summing to ~1.
pub type Influences = (Vec<u16>, Vec<f32>);

/// Destination sections. `remap` is `None` when the model has no Skin Joint Remap section,
/// in which case every batch has to address its joints through a base offset.
pub struct SkinOutput<'a> {
    pub data: &'a mut Vec<u8>,
    pub batches: &'a mut Vec<SkinBatch>,
    pub remap: Option<&'a mut Vec<u8>>,
    pub gpu: Option<&'a mut Vec<RcraSkinEntry>>,
}

#[derive(Debug, Default)]
pub struct SubsetSkin {
    pub batches: usize,
    /// Leading batches holding the morphing vertices (the subset's anim-vert batch count).
    pub anim_batches: usize,
    /// Vertices whose joints spanned 256+ indices without a remap section; their lightest out-of-range influences were dropped.
    pub clamped_vertices: usize,
}

/// Splits one subset's vertices into batches (2560 vertices, 256 distinct joints) and appends
/// their records, skin data, remap tables and GPU Skin entries in vertex order. The first
/// `anim_prefix` vertices (the morphing ones) never share a batch with the rest.
pub fn write_subset(verts: &[Influences], anim_prefix: usize, out: &mut SkinOutput) -> Result<SubsetSkin> {
    let prefix = anim_prefix.min(verts.len());
    let head = write_range(verts, 0, prefix, out)?;
    let tail = write_range(verts, prefix, verts.len(), out)?;
    Ok(SubsetSkin {
        batches: head.batches + tail.batches,
        anim_batches: head.batches,
        clamped_vertices: head.clamped_vertices + tail.clamped_vertices,
    })
}

fn write_range(all: &[Influences], from: usize, to: usize, out: &mut SkinOutput) -> Result<SubsetSkin> {
    let verts = &all[from..to];
    let mut result = SubsetSkin::default();
    let clamped;
    let verts = if out.remap.is_none() {
        let (v, n) = clamp_spans(verts);
        result.clamped_vertices = n;
        clamped = v;
        &clamped[..]
    } else {
        verts
    };
    let base_only = out.remap.is_none();

    let mut start = 0;
    while start < verts.len() {
        let mut set: BTreeSet<u16> = BTreeSet::new();
        let mut end = start;
        while end < verts.len() && end - start < SKIN_BATCH_VERT_MAX {
            let new: BTreeSet<u16> = verts[end].0.iter().copied().filter(|j| !set.contains(j)).collect();
            let fits = if base_only {
                let lo = set.first().into_iter().chain(new.first()).min().copied().unwrap_or(0);
                let hi = set.last().into_iter().chain(new.last()).max().copied().unwrap_or(0);
                hi - lo < SKIN_BATCH_JOINT_MAX as u16
            } else {
                set.len() + new.len() <= SKIN_BATCH_JOINT_MAX
            };
            if !fits && end > start {
                break;
            }
            set.extend(new);
            end += 1;
        }
        write_batch(&verts[start..end], from + start, &set, out)?;
        result.batches += 1;
        start = end;
    }
    Ok(result)
}

fn write_batch(verts: &[Influences], first_vertex: usize, set: &BTreeSet<u16>, out: &mut SkinOutput) -> Result<()> {
    let pad = (16 - out.data.len() % 16) % 16;
    out.data.resize(out.data.len() + pad, 0);
    let offset = out.data.len();

    let lo = set.first().copied().unwrap_or(0);
    let hi = set.last().copied().unwrap_or(0);
    let (remap_offset, remap_count, table): (u32, u16, Option<Vec<u16>>) = if hi - lo < SKIN_BATCH_JOINT_MAX as u16 {
        let base = (hi as u32 + 1).saturating_sub(SKIN_BATCH_JOINT_MAX as u32);
        (base, 0, None)
    } else {
        let remap = out.remap.as_deref_mut().ok_or_else(|| {
            ToolkitError::Parse("skin batch joints span 256+ indices but the model has no Skin Joint Remap section".into())
        })?;
        remap.resize(remap.len() + (16 - remap.len() % 16) % 16, 0);
        let off = remap.len() as u32;
        let t: Vec<u16> = set.iter().copied().collect();
        for j in &t {
            remap.extend_from_slice(&j.to_le_bytes());
        }
        (off, t.len() as u16, Some(t))
    };
    let local = |j: u16| -> u8 {
        match &table {
            None => (j as u32 - remap_offset) as u8,
            Some(t) => t.binary_search(&j).unwrap_or(0) as u8,
        }
    };

    let mut influences = 0usize;
    for group in verts.chunks(16) {
        let joints = group.iter().map(|w| w.0.len()).max().unwrap_or(1).max(1);
        out.data.push((joints - 1) as u8);
        for w in group {
            influences += w.1.iter().filter(|&&x| x > 0.0).count();
            encode_vertex(w, joints, &local, out.data);
            if let Some(gpu) = out.gpu.as_deref_mut() {
                let mut bones = [local(w.0.first().copied().unwrap_or(0)), 0, 0, 0];
                let mut weights = [255u8, 0, 0, 0];
                for k in 0..w.0.len().min(4) {
                    bones[k] = local(w.0[k]);
                    weights[k] = (w.1.get(k).copied().unwrap_or(0.0) * 256.0).clamp(0.0, 255.0) as u8;
                }
                gpu.push(RcraSkinEntry { bones, weights });
            }
        }
    }

    let too_big = |what: &str, v: usize| ToolkitError::Parse(format!("skin batch {what} {v} does not fit in 16 bits"));
    out.batches.push(SkinBatch {
        offset: offset as u32,
        joint_remap_offset: remap_offset,
        joint_remap_count: remap_count,
        avg_joint_influences: ((influences * 4096) / verts.len().max(1)).min(u16::MAX as usize) as u16,
        vertex_count: u16::try_from(verts.len()).map_err(|_| too_big("vertex count", verts.len()))?,
        first_vertex: u16::try_from(first_vertex).map_err(|_| too_big("first vertex", first_vertex))?,
    });
    Ok(())
}

/// One vertex of a skin-data group: a joint id per slot, plus a /256 weight per slot when the group has several.
fn encode_vertex(w: &Influences, joints: usize, local: &impl Fn(u16) -> u8, sd: &mut Vec<u8>) {
    let first = local(w.0.first().copied().unwrap_or(0));
    if joints == 1 {
        sd.push(first);
        return;
    }
    let mut ids = vec![0u8; joints];
    let mut iw = vec![0i32; joints];
    ids[0] = first;
    iw[0] = 256;
    for k in 1..w.0.len().min(joints) {
        ids[k] = local(w.0[k]);
        let bw = (w.1.get(k).copied().unwrap_or(0.0) * 256.0).clamp(0.0, 255.0).round() as i32;
        iw[k] = bw;
        iw[0] -= bw;
    }
    if iw[0] < 0 {
        iw[0] = 0;
    } else if iw[0] > 255 {
        iw[0] = 255;
        iw[1] = 1;
        ids[1] = ids[0];
    }
    for k in 0..joints {
        if k > 0 && iw[k] == 0 {
            ids[k] = ids[k - 1];
        }
        sd.push(ids[k]);
        sd.push(iw[k] as u8);
    }
}

/// Keeps each vertex's joints within a 256-wide index window (heaviest first) and renormalizes.
fn clamp_spans(verts: &[Influences]) -> (Vec<Influences>, usize) {
    let mut clamped = 0;
    let out = verts
        .iter()
        .map(|(j, w)| {
            let lo = j.iter().min().copied().unwrap_or(0);
            let hi = j.iter().max().copied().unwrap_or(0);
            if hi - lo < SKIN_BATCH_JOINT_MAX as u16 {
                return (j.clone(), w.clone());
            }
            clamped += 1;
            let mut order: Vec<usize> = (0..j.len()).collect();
            order.sort_by(|&a, &b| w.get(b).unwrap_or(&0.0).total_cmp(w.get(a).unwrap_or(&0.0)));
            let (mut kj, mut kw) = (Vec::new(), Vec::new());
            let (mut klo, mut khi) = (u16::MAX, 0u16);
            for i in order {
                let (nlo, nhi) = (klo.min(j[i]), khi.max(j[i]));
                if nhi - nlo < SKIN_BATCH_JOINT_MAX as u16 {
                    kj.push(j[i]);
                    kw.push(w.get(i).copied().unwrap_or(0.0));
                    (klo, khi) = (nlo, nhi);
                }
            }
            let sum: f32 = kw.iter().sum();
            if sum > 0.0 {
                kw.iter_mut().for_each(|x| *x /= sum);
            }
            (kj, kw)
        })
        .collect();
    (out, clamped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::model_converter::sections::meshes::MeshDefinition;
    use crate::tools::model_converter::sections::skin::SkinSource;

    fn subset(vertex_count: u32, batches: u8) -> MeshDefinition {
        let mut raw = vec![0u8; 64];
        raw[0x20..0x24].copy_from_slice(&vertex_count.to_le_bytes());
        raw[0x24] = 1;
        raw[0x2A] = batches;
        MeshDefinition::parse_all(&raw).unwrap().remove(0)
    }

    fn roundtrip(verts: &[Influences], with_remap: bool) -> (Vec<SkinBatch>, Vec<crate::tools::model_converter::sections::skin::VertexWeights>) {
        let (mut data, mut batches, mut remap) = (Vec::new(), Vec::new(), Vec::new());
        let mut out = SkinOutput { data: &mut data, batches: &mut batches, remap: with_remap.then_some(&mut remap), gpu: None };
        let r = write_subset(verts, 0, &mut out).unwrap();
        let src = SkinSource { data, batches: batches.clone(), remap, gpu: Vec::new() };
        (batches, src.subset_weights(&subset(verts.len() as u32, r.batches as u8)))
    }

    #[test]
    fn splits_on_vertex_limit_and_keeps_joints() {
        let verts: Vec<Influences> = (0..6000).map(|i| (vec![(i % 40) as u16, 3], vec![0.75, 0.25])).collect();
        let (batches, back) = roundtrip(&verts, false);
        assert_eq!(batches.iter().map(|b| b.vertex_count as usize).collect::<Vec<_>>(), vec![2560, 2560, 880]);
        assert_eq!(back[4321][0].0, (4321 % 40) as u16);
        assert_eq!(back[4321][1].0, 3);
    }

    #[test]
    fn morphing_prefix_gets_its_own_batches() {
        let verts: Vec<Influences> = (0..6000).map(|_| (vec![1], vec![1.0])).collect();
        let (mut data, mut batches) = (Vec::new(), Vec::new());
        let mut out = SkinOutput { data: &mut data, batches: &mut batches, remap: None, gpu: None };
        let r = write_subset(&verts, 3000, &mut out).unwrap();
        assert_eq!(r.anim_batches, 2);
        assert_eq!(batches.iter().map(|b| (b.first_vertex, b.vertex_count)).collect::<Vec<_>>(),
            vec![(0, 2560), (2560, 440), (3000, 2560), (5560, 440)]);
    }

    #[test]
    fn high_joints_use_a_base_offset() {
        let verts: Vec<Influences> = (0..100).map(|i| (vec![300 + (i % 10) as u16], vec![1.0])).collect();
        let (batches, back) = roundtrip(&verts, false);
        assert_eq!(batches[0].joint_remap_count, 0);
        assert!(batches[0].joint_remap_offset > 0);
        assert_eq!(back[7][0].0, 307);
    }

    #[test]
    fn wide_spans_use_a_remap_table_or_split() {
        let verts: Vec<Influences> = (0..600).map(|i| (vec![(i % 300) as u16 * 1, 0], vec![0.5, 0.5])).collect();
        let (batches, back) = roundtrip(&verts, true);
        assert!(batches.iter().any(|b| b.joint_remap_count > 0));
        assert!(batches.iter().all(|b| b.joint_remap_count as usize <= SKIN_BATCH_JOINT_MAX));
        assert_eq!(back[299][0].0, 299);
        let (batches, back) = roundtrip(&verts, false);
        assert!(batches.iter().all(|b| b.joint_remap_count == 0));
        assert_eq!(back[299][0].0, 299);
    }
}
