//! Rig-runtime sections that sit alongside the skeleton: ragdoll bodies, IK
//! chains, cloth metadata and anim-dynamics (jiggle) chains. The first three
//! tags have no recovered name string; the layouts are verified on the corpus.

use super::joints::{Joint, JointsTransform, TAG_JOINTS, TAG_JOINTS_TRANSFORM};
use super::skeleton::JointBspheres;
use crate::core::dat1::Dat1;
use crate::core::error::{Result, ToolkitError};
use byteorder::{ReadBytesExt, LE};
use glam::{DQuat, DVec3};
use std::io::Cursor;

pub const TAG_RAGDOLL_META_DATA: u32 = 0x707F1B58;
pub const TAG_IK_SETUP: u32 = 0x9A434B29;
pub const TAG_CLOTH_META_DATA: u32 = 0x5A39FAB7;
pub const TAG_ANIM_DYNAMICS_DEF: u32 = 0xADD1CBD3;

fn read_f32s<const N: usize>(c: &mut Cursor<&[u8]>) -> Result<[f32; N]> {
    let mut out = [0f32; N];
    for f in &mut out {
        *f = c.read_f32::<LE>()?;
    }
    Ok(out)
}

fn at(data: &[u8], offset: usize) -> Cursor<&[u8]> {
    let mut c = Cursor::new(data);
    c.set_position(offset as u64);
    c
}

// ------------------------------------------------------------------- ragdoll

/// `RagdollMetaData::flags`; bits 2 and 4 are verified, 0 and 1 inferred from the builder.
pub mod ragdoll_flags {
    pub const HAS_COLL_MESH_RBS: u32 = 1 << 0;
    pub const HAS_DYNAMIC_RBS: u32 = 1 << 1;
    pub const HAS_JOINT_RBS: u32 = 1 << 2;
    pub const HAS_NON_JOINT_RBS: u32 = 1 << 4;
}

/// 144 bytes: joint→body and body→joint matrices, then the joint binding.
#[derive(Debug, Clone)]
pub struct RigidBodyInfo {
    pub joint_to_body: [f32; 16],
    pub body_to_joint: [f32; 16],
    /// 0xFFFFFFFF for a body with no joint name.
    pub joint_hash: u32,
    /// 0xFFFF when the body is not attached to a joint.
    pub joint: u16,
    /// Index of the joint in name-hash order (what anim clips use).
    pub sorted_joint: u16,
    pub parent_joint: u16,
    pub ancestor_start: u16,
    pub ancestor_count: u16,
}

pub struct RagdollMetaData {
    pub flags: u32,
    pub bodies: Vec<RigidBodyInfo>,
    /// Flattened parent chains; each body's run walks from its parent to the root.
    pub ancestors: Vec<i16>,
}

impl RagdollMetaData {
    pub const BODY_SIZE: usize = 144;

    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut out = Self { flags: 0, bodies: Vec::new(), ancestors: Vec::new() };
        if data.len() < 16 {
            return Ok(out);
        }
        let mut c = Cursor::new(data);
        let infos = c.read_u32::<LE>()? as usize;
        let anc = c.read_u32::<LE>()? as usize;
        let body_count = c.read_u16::<LE>()? as usize;
        let anc_count = c.read_u16::<LE>()? as usize;
        out.flags = c.read_u32::<LE>()?;
        for i in 0..body_count {
            let base = infos + i * Self::BODY_SIZE;
            if base + Self::BODY_SIZE > data.len() {
                break;
            }
            let mut c = at(data, base);
            out.bodies.push(RigidBodyInfo {
                joint_to_body: read_f32s(&mut c)?,
                body_to_joint: read_f32s(&mut c)?,
                joint_hash: c.read_u32::<LE>()?,
                joint: c.read_u16::<LE>()?,
                sorted_joint: c.read_u16::<LE>()?,
                parent_joint: c.read_u16::<LE>()?,
                ancestor_start: c.read_u16::<LE>()?,
                ancestor_count: c.read_u16::<LE>()?,
            });
        }
        let mut c = at(data, anc);
        for _ in 0..anc_count {
            if c.position() as usize + 2 > data.len() {
                break;
            }
            out.ancestors.push(c.read_i16::<LE>()?);
        }
        Ok(out)
    }

    pub fn ancestors_of(&self, body: &RigidBodyInfo) -> &[i16] {
        let a = body.ancestor_start as usize;
        let b = (a + body.ancestor_count as usize).min(self.ancestors.len());
        self.ancestors.get(a..b).unwrap_or(&[])
    }
}

// ------------------------------------------------------------------------ IK

pub mod ik_chain_flags {
    pub const ANIM_PRECONDITIONING: u8 = 1 << 0;
    pub const CURVE_PRECONDITIONING: u8 = 1 << 1;
}

/// 108 bytes. `goal_locator_hash` names the effector (`igLoc_foot_l`, …).
#[derive(Debug, Clone)]
pub struct IkChain {
    pub solver_error: f32,
    pub goal_locator_hash: u32,
    pub joint_start: u16,
    pub stick_start: u16,
    pub max_iterations: u8,
    pub flags: u8,
    pub joint_count: u8,
    pub stick_count: u8,
    /// Goal direction from the chain start in bind pose; w = length.
    pub goal_dir_from_start: [f32; 4],
    /// Goal locator relative to the chain end (3x3).
    pub goal_rel_to_end: [f32; 9],
    pub goal_dir_from_end: [f32; 4],
    pub start_to_end_dir: [f32; 3],
    pub curve_precondition_dir: [f32; 3],
}

/// 44 bytes.
#[derive(Debug, Clone)]
pub struct IkJointInfo {
    /// Direction from the chain start; w = length.
    pub start_relative_dir: [f32; 4],
    pub parent_relative_dir: [f32; 3],
    pub solver_bias_dir: [f32; 3],
    /// Index into `IkSetup::setup_joints`.
    pub setup_joint: u8,
    pub chain_parent: u8,
    /// Bit 0 = solver bias.
    pub flags: u8,
}

/// 16 bytes: a distance constraint between two chain joints.
#[derive(Debug, Clone, Copy)]
pub struct IkStick {
    pub joints: [u8; 2],
    pub inv_mass: [f32; 2],
    pub rest_length: f32,
}

pub struct IkSetup {
    pub chains: Vec<IkChain>,
    pub joints: Vec<IkJointInfo>,
    pub sticks: Vec<IkStick>,
    /// Model joint indices the IK joint infos refer to.
    pub setup_joints: Vec<u16>,
}

impl IkSetup {
    pub const CHAIN_SIZE: usize = 108;
    pub const JOINT_SIZE: usize = 44;
    pub const STICK_SIZE: usize = 16;

    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut out = Self { chains: Vec::new(), joints: Vec::new(), sticks: Vec::new(), setup_joints: Vec::new() };
        if data.len() < 24 {
            return Ok(out);
        }
        let mut c = Cursor::new(data);
        let _size = c.read_u32::<LE>()?;
        let chains_off = c.read_u32::<LE>()? as usize;
        let joints_off = c.read_u32::<LE>()? as usize;
        let sticks_off = c.read_u32::<LE>()? as usize;
        let setup_off = c.read_u32::<LE>()? as usize;
        let chain_count = c.read_u8()? as usize;
        let setup_count = c.read_u8()? as usize;

        for i in 0..chain_count {
            let base = chains_off + i * Self::CHAIN_SIZE;
            if base + Self::CHAIN_SIZE > data.len() {
                break;
            }
            let mut c = at(data, base);
            out.chains.push(IkChain {
                solver_error: c.read_f32::<LE>()?,
                goal_locator_hash: c.read_u32::<LE>()?,
                joint_start: c.read_u16::<LE>()?,
                stick_start: c.read_u16::<LE>()?,
                max_iterations: c.read_u8()?,
                flags: c.read_u8()?,
                joint_count: c.read_u8()?,
                stick_count: c.read_u8()?,
                goal_dir_from_start: read_f32s(&mut c)?,
                goal_rel_to_end: read_f32s(&mut c)?,
                goal_dir_from_end: read_f32s(&mut c)?,
                start_to_end_dir: read_f32s(&mut c)?,
                curve_precondition_dir: read_f32s(&mut c)?,
            });
        }
        for i in 0..sticks_off.saturating_sub(joints_off) / Self::JOINT_SIZE {
            let mut c = at(data, joints_off + i * Self::JOINT_SIZE);
            out.joints.push(IkJointInfo {
                start_relative_dir: read_f32s(&mut c)?,
                parent_relative_dir: read_f32s(&mut c)?,
                solver_bias_dir: read_f32s(&mut c)?,
                setup_joint: c.read_u8()?,
                chain_parent: c.read_u8()?,
                flags: c.read_u8()?,
            });
        }
        for i in 0..setup_off.saturating_sub(sticks_off) / Self::STICK_SIZE {
            let mut c = at(data, sticks_off + i * Self::STICK_SIZE);
            let joints = [c.read_u8()?, c.read_u8()?];
            c.read_u16::<LE>()?;
            out.sticks.push(IkStick { joints, inv_mass: read_f32s(&mut c)?, rest_length: c.read_f32::<LE>()? });
        }
        let mut c = at(data, setup_off);
        for _ in 0..setup_count {
            if c.position() as usize + 2 > data.len() {
                break;
            }
            out.setup_joints.push(c.read_u16::<LE>()?);
        }
        Ok(out)
    }

    /// Model joint indices driven by one chain, root first.
    pub fn chain_joints(&self, chain: &IkChain) -> Vec<u16> {
        let a = chain.joint_start as usize;
        self.joints
            .get(a..(a + chain.joint_count as usize).min(self.joints.len()))
            .unwrap_or(&[])
            .iter()
            .filter_map(|j| self.setup_joints.get(j.setup_joint as usize).copied())
            .collect()
    }
}

// --------------------------------------------------------------------- cloth

/// Header offsets are in 16-byte units; `flags` bit 0 = has cloth.
#[derive(Debug, Clone, Default)]
pub struct ClothMetaData {
    pub flags: u8,
    pub instance_count: u16,
    pub collidable_count: u8,
    /// Joints that drive or collide with the cloth (the `CLOTH_JOINT` flagged ones).
    pub influence_joints: Vec<u16>,
    /// (cloth name crc, subset index) for vertex-cloth subsets.
    pub vertex_cloth_subsets: Vec<(u32, u16)>,
    /// Collidable ids per cloth instance.
    pub instance_collidables: Vec<Vec<u8>>,
}

impl ClothMetaData {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut out = Self::default();
        if data.len() < 16 {
            return Ok(out);
        }
        let mut c = Cursor::new(data);
        let joints_off = c.read_u16::<LE>()? as usize * 16;
        let joint_count = c.read_u16::<LE>()? as usize;
        let vcs_off = c.read_u16::<LE>()? as usize * 16;
        let vcs_count = c.read_u16::<LE>()? as usize;
        out.instance_count = c.read_u16::<LE>()?;
        let coll_off = c.read_u16::<LE>()? as usize * 16;
        out.collidable_count = c.read_u8()?;
        out.flags = c.read_u8()?;

        let mut c = at(data, joints_off);
        for _ in 0..joint_count {
            if c.position() as usize + 2 > data.len() {
                break;
            }
            out.influence_joints.push(c.read_u16::<LE>()?);
        }
        for i in 0..vcs_count {
            let o = vcs_off + i * 8;
            if o + 8 > data.len() {
                break;
            }
            let mut c = at(data, o);
            out.vertex_cloth_subsets.push((c.read_u32::<LE>()?, c.read_u16::<LE>()?));
        }
        for i in 0..out.instance_count as usize {
            let o = coll_off + i * 4;
            if o + 4 > data.len() {
                break;
            }
            let ids_off = coll_off + u16::from_le_bytes([data[o], data[o + 1]]) as usize;
            let n = data[o + 2] as usize;
            out.instance_collidables.push(data.get(ids_off..ids_off + n).map(|s| s.to_vec()).unwrap_or_default());
        }
        Ok(out)
    }
}

// ------------------------------------------------------------ anim dynamics

/// `DynamicsChain::constraint_type`.
pub mod dynamics_constraint {
    pub const SIMPLE: u8 = 0;
    pub const CONE: u8 = 1;
    pub const LIMITED_CONE: u8 = 2;
}

/// `DynamicsChain::tether_types`.
pub mod dynamics_tether {
    pub const NONE: u8 = 0;
    pub const JOINT: u8 = 1;
    pub const POINT: u8 = 2;
}

/// 64 bytes: a simulated link between two points, with its cone limits.
#[derive(Debug, Clone)]
pub struct DynamicsLink {
    pub point: u16,
    pub point_child: u16,
    pub joint: u16,
    pub joint_child: u16,
    pub joint_parent: i16,
    pub mass_inv: f32,
    pub mass_inv_child: f32,
    pub rest_length: f32,
    pub cone_outer_cos: f32,
    pub cone_outer_sin: f32,
    /// Hinge limits: -Y, +Y, -Z, +Z sines about the cone's X axis.
    pub cone_limits: [f32; 4],
    pub parent_to_cone_rot: [f32; 4],
}

/// 12 bytes: long-range attach constraint.
#[derive(Debug, Clone, Copy)]
pub struct DynamicsAttach {
    pub attach_point: u16,
    pub sim_point: u16,
    pub mass_inv_child: f32,
    pub geodesic_length: f32,
}

/// 32 bytes. `flags` bit 0 = parent is a joint, not a particle.
#[derive(Debug, Clone, Copy)]
pub struct DynamicsBend {
    pub parent: u16,
    pub base: u16,
    pub child: u16,
    pub flags: u8,
    pub mass_inv: f32,
    pub mass_inv_child: f32,
    pub bind_distance: f32,
    pub cos_angle: f32,
    pub stiffness: f32,
    pub damping: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct DynamicsTether {
    pub point: u16,
    pub source: u16,
    pub parent_joint: u16,
    pub parent_to_joint_pos: [f32; 3],
}

/// 108 bytes: one named jiggle chain and the runs of the other arrays it owns.
#[derive(Debug, Clone)]
pub struct DynamicsChain {
    pub constraint_type: u8,
    pub links: (u16, u16),
    pub attaches: (u16, u16),
    pub bends: (u16, u16),
    pub points: (u16, u16),
    pub tether_types: [u8; 2],
    pub tethers: [DynamicsTether; 2],
    pub joint_elems: (u16, u16),
    pub leaf_joint: i16,
    pub leaf_joint_parent: i16,
    pub leaf_parent_to_joint_rot: [f32; 4],
    pub gravity: [f32; 3],
    pub damping: f32,
    pub name_offset: u32,
}

/// 40 bytes: how a simulated point drives a joint.
#[derive(Debug, Clone)]
pub struct DynamicsJointElem {
    pub point_local: u16,
    pub point_child_local: u16,
    pub joint: u16,
    pub joint_child: u16,
    pub joint_parent: i16,
    pub joint_to_child_dir: [f32; 3],
    pub parent_to_joint_rot: [f32; 4],
}

/// Header: seven u16 counts, pad, seven u32 offsets relative to byte 48.
/// The shipped builder sizes the attach block by the *bend* count, so the
/// block is padded past `attaches.len()` whenever the two differ.
#[derive(Debug, Clone, Default)]
pub struct AnimDynamicsDef {
    pub header_pad: [u8; 6],
    pub points: Vec<[f32; 3]>,
    pub links: Vec<DynamicsLink>,
    pub attaches: Vec<DynamicsAttach>,
    /// The attach block as shipped: per chain, the model's *first* attaches, so later chains' runs
    /// point at misplaced entries. Kept for exact round trips; `None` writes `attaches` contiguously.
    pub attach_block: Option<Vec<u8>>,
    pub bends: Vec<DynamicsBend>,
    pub collider_count: u16,
    pub chains: Vec<DynamicsChain>,
    pub joint_elems: Vec<DynamicsJointElem>,
}

impl AnimDynamicsDef {
    pub const HEADER_SIZE: usize = 48;

    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut out = Self::default();
        if data.len() < Self::HEADER_SIZE {
            return Ok(out);
        }
        let mut c = Cursor::new(data);
        let mut counts = [0usize; 7];
        for n in &mut counts {
            *n = c.read_u16::<LE>()? as usize;
        }
        out.header_pad.copy_from_slice(&data[14..20]);
        c.set_position(20);
        let mut offs = [0usize; 7];
        for o in &mut offs {
            *o = c.read_u32::<LE>()? as usize;
        }
        out.collider_count = counts[4] as u16;
        out.attach_block = data.get(Self::HEADER_SIZE + offs[2]..Self::HEADER_SIZE + offs[3]).map(<[u8]>::to_vec);
        let block = |k: usize, i: usize, size: usize| -> Option<Cursor<&[u8]>> {
            let o = Self::HEADER_SIZE.checked_add(offs[k])?.checked_add(i * size)?;
            (offs[k] != u32::MAX as usize && o + size <= data.len()).then(|| at(data, o))
        };

        for i in 0..counts[0] {
            let Some(mut c) = block(0, i, 12) else { break };
            out.points.push(read_f32s(&mut c)?);
        }
        for i in 0..counts[1] {
            let Some(mut c) = block(1, i, 64) else { break };
            let (point, point_child, joint, joint_child) =
                (c.read_u16::<LE>()?, c.read_u16::<LE>()?, c.read_u16::<LE>()?, c.read_u16::<LE>()?);
            let joint_parent = c.read_i16::<LE>()?;
            c.read_u16::<LE>()?;
            out.links.push(DynamicsLink {
                point,
                point_child,
                joint,
                joint_child,
                joint_parent,
                mass_inv: c.read_f32::<LE>()?,
                mass_inv_child: c.read_f32::<LE>()?,
                rest_length: c.read_f32::<LE>()?,
                cone_outer_cos: c.read_f32::<LE>()?,
                cone_outer_sin: c.read_f32::<LE>()?,
                cone_limits: read_f32s(&mut c)?,
                parent_to_cone_rot: read_f32s(&mut c)?,
            });
        }
        for i in 0..counts[2] {
            let Some(mut c) = block(2, i, 12) else { break };
            out.attaches.push(DynamicsAttach {
                attach_point: c.read_u16::<LE>()?,
                sim_point: c.read_u16::<LE>()?,
                mass_inv_child: c.read_f32::<LE>()?,
                geodesic_length: c.read_f32::<LE>()?,
            });
        }
        for i in 0..counts[3] {
            let Some(mut c) = block(3, i, 32) else { break };
            let (parent, base, child) = (c.read_u16::<LE>()?, c.read_u16::<LE>()?, c.read_u16::<LE>()?);
            let flags = c.read_u8()?;
            c.read_u8()?;
            out.bends.push(DynamicsBend {
                parent,
                base,
                child,
                flags,
                mass_inv: c.read_f32::<LE>()?,
                mass_inv_child: c.read_f32::<LE>()?,
                bind_distance: c.read_f32::<LE>()?,
                cos_angle: c.read_f32::<LE>()?,
                stiffness: c.read_f32::<LE>()?,
                damping: c.read_f32::<LE>()?,
            });
        }
        for i in 0..counts[5] {
            let Some(mut c) = block(5, i, 108) else { break };
            let constraint_type = c.read_u8()?;
            c.read_u8()?;
            let mut run = || -> Result<(u16, u16)> { Ok((c.read_u16::<LE>()?, c.read_u16::<LE>()?)) };
            let (links, attaches, bends, points) = (run()?, run()?, run()?, run()?);
            c.read_u32::<LE>()?;
            let tether_types = [c.read_u8()?, c.read_u8()?];
            let mut tether = || -> Result<DynamicsTether> {
                let (point, source, parent_joint) = (c.read_u16::<LE>()?, c.read_u16::<LE>()?, c.read_u16::<LE>()?);
                c.read_u16::<LE>()?;
                Ok(DynamicsTether { point, source, parent_joint, parent_to_joint_pos: read_f32s(&mut c)? })
            };
            let tethers = [tether()?, tether()?];
            out.chains.push(DynamicsChain {
                constraint_type,
                links,
                attaches,
                bends,
                points,
                tether_types,
                tethers,
                joint_elems: (c.read_u16::<LE>()?, c.read_u16::<LE>()?),
                leaf_joint: c.read_i16::<LE>()?,
                leaf_joint_parent: c.read_i16::<LE>()?,
                leaf_parent_to_joint_rot: read_f32s(&mut c)?,
                gravity: read_f32s(&mut c)?,
                damping: c.read_f32::<LE>()?,
                name_offset: c.read_u32::<LE>()?,
            });
        }
        for i in 0..counts[6] {
            let Some(mut c) = block(6, i, 40) else { break };
            let (point_local, point_child_local, joint, joint_child) =
                (c.read_u16::<LE>()?, c.read_u16::<LE>()?, c.read_u16::<LE>()?, c.read_u16::<LE>()?);
            let joint_parent = c.read_i16::<LE>()?;
            c.read_u16::<LE>()?;
            out.joint_elems.push(DynamicsJointElem {
                point_local,
                point_child_local,
                joint,
                joint_child,
                joint_parent,
                joint_to_child_dir: read_f32s(&mut c)?,
                parent_to_joint_rot: read_f32s(&mut c)?,
            });
        }
        Ok(out)
    }

    /// Blocks are 16-aligned and padded; colliders are never stored.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut blocks: [Vec<u8>; 6] = Default::default();
        for p in &self.points {
            put_f32s(&mut blocks[0], p);
        }
        for l in &self.links {
            let o = &mut blocks[1];
            put_u16s(o, &[l.point, l.point_child, l.joint, l.joint_child, l.joint_parent as u16, 0]);
            put_f32s(o, &[l.mass_inv, l.mass_inv_child, l.rest_length, l.cone_outer_cos, l.cone_outer_sin]);
            put_f32s(o, &l.cone_limits);
            put_f32s(o, &l.parent_to_cone_rot);
        }
        match &self.attach_block {
            Some(raw) => blocks[2] = raw.clone(),
            None => {
                for a in &self.attaches {
                    put_u16s(&mut blocks[2], &[a.attach_point, a.sim_point]);
                    put_f32s(&mut blocks[2], &[a.mass_inv_child, a.geodesic_length]);
                }
                blocks[2].resize(12 * self.attaches.len().max(self.bends.len()), 0);
            }
        }
        for b in &self.bends {
            let o = &mut blocks[3];
            put_u16s(o, &[b.parent, b.base, b.child]);
            o.extend_from_slice(&[b.flags, 0]);
            put_f32s(o, &[b.mass_inv, b.mass_inv_child, b.bind_distance, b.cos_angle, b.stiffness, b.damping]);
        }
        for c in &self.chains {
            let o = &mut blocks[4];
            o.extend_from_slice(&[c.constraint_type, 0]);
            for (start, count) in [c.links, c.attaches, c.bends, c.points] {
                put_u16s(o, &[start, count]);
            }
            o.extend_from_slice(&[0; 4]);
            o.extend_from_slice(&c.tether_types);
            for t in &c.tethers {
                put_u16s(o, &[t.point, t.source, t.parent_joint, 0]);
                put_f32s(o, &t.parent_to_joint_pos);
            }
            put_u16s(o, &[c.joint_elems.0, c.joint_elems.1, c.leaf_joint as u16, c.leaf_joint_parent as u16]);
            put_f32s(o, &c.leaf_parent_to_joint_rot);
            put_f32s(o, &c.gravity);
            put_f32s(o, &[c.damping]);
            o.extend_from_slice(&c.name_offset.to_le_bytes());
        }
        for e in &self.joint_elems {
            let o = &mut blocks[5];
            put_u16s(o, &[e.point_local, e.point_child_local, e.joint, e.joint_child, e.joint_parent as u16, 0]);
            put_f32s(o, &e.joint_to_child_dir);
            put_f32s(o, &e.parent_to_joint_rot);
        }

        let counts = [
            self.points.len(),
            self.links.len(),
            self.attaches.len(),
            self.bends.len(),
            self.collider_count as usize,
            self.chains.len(),
            self.joint_elems.len(),
        ];
        let mut out = Vec::new();
        put_u16s(&mut out, &counts.map(|n| n as u16));
        out.extend_from_slice(&self.header_pad);
        let mut body = Vec::new();
        for (i, b) in blocks.iter().enumerate() {
            if i == 4 {
                out.extend_from_slice(&u32::MAX.to_le_bytes());
            }
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            body.extend_from_slice(b);
            body.resize(body.len().next_multiple_of(16), 0);
        }
        out.extend(body);
        out
    }

    /// A section of untethered chains derived from the bind pose, in byte-wise name order, with
    /// each chain's attaches contiguous at its own run start.
    pub fn build(skel: &BindSkeleton, specs: &[ChainSpec]) -> Result<Self> {
        let mut order: Vec<&ChainSpec> = specs.iter().collect();
        order.sort_by(|a, b| a.name.cmp(&b.name));
        let mut out = Self::default();
        for s in order {
            out.push_chain(skel, s)?;
        }
        Ok(out)
    }

    fn push_chain(&mut self, skel: &BindSkeleton, spec: &ChainSpec) -> Result<()> {
        let err = |m: String| ToolkitError::Parse(format!("chain {}: {m}", spec.name));
        let js: Vec<usize> = spec.joints.iter().map(|&j| j as usize).collect();
        if js.len() < 2 || js.iter().any(|&j| j >= skel.pos.len()) {
            return Err(err("needs two or more existing joints".into()));
        }
        if let Some(w) = js.windows(2).find(|w| skel.parent(w[1]) != Some(w[0])) {
            return Err(err(format!("joint {} is not the parent of joint {}", w[0], w[1])));
        }
        let root_parent = skel.parent(js[0]).ok_or_else(|| err(format!("joint {} has no parent to tether to", js[0])))?;
        let n = js.len() - 1;
        let p = &spec.params;
        let mi = (1.0 / p.mass) as f32;
        let mut sorted = js.clone();
        sorted.sort_unstable();
        let ps = self.points.len();
        let point = |j: usize| (ps + sorted.binary_search(&j).unwrap_or(0)) as u16;
        self.points.extend(sorted.iter().map(|&j| skel.pos[j].as_vec3().to_array()));

        let (l0, a0, b0, e0) = (self.links.len(), self.attaches.len(), self.bends.len(), self.joint_elems.len());
        let mut geodesic = 0.0;
        for k in 0..n {
            let (j, jc) = (js[k], js[k + 1]);
            let rest = skel.pos[jc].distance(skel.pos[j]);
            let (sin, cos) = cone_angle(p, k, n).sin_cos();
            self.links.push(DynamicsLink {
                point: point(j),
                point_child: point(jc),
                joint: j as u16,
                joint_child: jc as u16,
                joint_parent: skel.parents[j],
                mass_inv: if k == 0 { 0.0 } else { mi },
                mass_inv_child: mi,
                rest_length: rest as f32,
                cone_outer_cos: cos as f32,
                cone_outer_sin: sin as f32,
                cone_limits: cone_limits(sin, &p.limit_scales),
                parent_to_cone_rot: (skel.local_rot[j] * arc_from_x(skel.local_pos[jc].normalize_or_zero()))
                    .as_quat()
                    .to_array(),
            });
            // bend 0 hangs off the tether's parent joint, later bends off the previous point
            let (parent, parent_pos, flags) = match k {
                0 => (root_parent as u16, skel.pos[root_parent], 1),
                _ => (point(js[k - 1]), skel.pos[js[k - 1]], 0),
            };
            let (base, child) = (skel.pos[j], skel.pos[jc]);
            self.bends.push(DynamicsBend {
                parent,
                base: point(j),
                child: point(jc),
                flags,
                mass_inv: if k < 2 { 0.0 } else { mi },
                mass_inv_child: mi,
                bind_distance: parent_pos.distance(child) as f32,
                cos_angle: (parent_pos - base).normalize_or_zero().dot((child - base).normalize_or_zero()) as f32,
                stiffness: 0.8,
                damping: 0.9,
            });
            geodesic += rest;
            if k % 2 == 0 {
                self.attaches.push(DynamicsAttach {
                    attach_point: point(js[0]),
                    sim_point: point(jc),
                    mass_inv_child: mi,
                    geodesic_length: (geodesic * 1.01) as f32,
                });
            }
            let (a, b) = (j.min(jc), j.max(jc));
            self.joint_elems.push(DynamicsJointElem {
                point_local: point(a) - ps as u16,
                point_child_local: point(b) - ps as u16,
                joint: a as u16,
                joint_child: b as u16,
                joint_parent: skel.parents[a],
                joint_to_child_dir: skel.local_pos[b].normalize_or_zero().as_vec3().to_array(),
                parent_to_joint_rot: skel.local_rot[a].as_quat().to_array(),
            });
        }
        let (j0, jn) = (js[0], js[n]);
        self.chains.push(DynamicsChain {
            constraint_type: p.constraint_type,
            links: (l0 as u16, n as u16),
            attaches: (a0 as u16, (self.attaches.len() - a0) as u16),
            bends: (b0 as u16, n as u16),
            points: (ps as u16, js.len() as u16),
            tether_types: [dynamics_tether::JOINT, dynamics_tether::NONE],
            tethers: [
                DynamicsTether {
                    point: point(j0),
                    source: j0 as u16,
                    parent_joint: root_parent as u16,
                    parent_to_joint_pos: skel.local_pos[j0].as_vec3().to_array(),
                },
                DynamicsTether { point: u16::MAX, source: u16::MAX, parent_joint: 0, parent_to_joint_pos: [0.0; 3] },
            ],
            joint_elems: (e0 as u16, n as u16),
            leaf_joint: jn as i16,
            leaf_joint_parent: js[n - 1] as i16,
            leaf_parent_to_joint_rot: skel.local_rot[jn].as_quat().to_array(),
            gravity: p.gravity,
            damping: p.damping,
            name_offset: spec.name_offset,
        });
        Ok(())
    }

    /// Joints (j0 → jn) and params of chain `i`, the inverse of `build`. The joint list only means
    /// j0 → jn on untethered chains; double-tethered halves run toward each other.
    pub fn recover(&self, i: usize) -> Option<(Vec<u16>, ChainParams)> {
        let c = self.chains.get(i)?;
        let links = self.links.get(run(c.links))?;
        let (first, last) = (links.first()?, links.last()?);
        let angle = |l: &DynamicsLink| (l.cone_outer_sin as f64).atan2(l.cone_outer_cos as f64).to_degrees();
        let s = first.cone_outer_sin as f64;
        let l = first.cone_limits.map(|x| x as f64);
        let mut joints = vec![first.joint];
        joints.extend(links.iter().map(|l| l.joint_child));
        let params = ChainParams {
            constraint_type: c.constraint_type,
            mass: 1.0 / first.mass_inv_child as f64,
            damping: c.damping,
            gravity: c.gravity,
            cone_outer: angle(first),
            cone_outer_end: angle(last),
            limit_scales: if s == 0.0 { [1.0; 4] } else { [-l[0] / s, l[1] / s, -l[2] / s, l[3] / s] },
        };
        Some((joints, params))
    }

    /// Every chain as a `build` spec; fails on chains `build` can't express (a tethered end).
    pub fn specs(&self, dat1: &Dat1) -> Result<Vec<ChainSpec>> {
        (0..self.chains.len())
            .map(|i| {
                let c = &self.chains[i];
                let name = dat1.get_string(c.name_offset).unwrap_or_default();
                let (joints, params) = self
                    .recover(i)
                    .filter(|_| c.tether_types[1] == dynamics_tether::NONE)
                    .ok_or_else(|| ToolkitError::Unsupported(format!("chain {name}: only untethered chains can be rebuilt")))?;
                Ok(ChainSpec { name, name_offset: c.name_offset, joints, params })
            })
            .collect()
    }

    /// Re-applies params to chain `i` in place, keeping its layout and pinned points. The cone lerp
    /// restarts at every pinned link, as on the shipped double-tethered halves.
    pub fn retune(&mut self, i: usize, p: &ChainParams) -> Result<()> {
        let c = self.chains.get_mut(i).ok_or_else(|| ToolkitError::Parse(format!("no chain {i}")))?;
        c.constraint_type = p.constraint_type;
        c.damping = p.damping;
        c.gravity = p.gravity;
        let (links, bends, points) = (run(c.links), run(c.bends), run(c.points));
        let mi = (1.0 / p.mass) as f32;
        let links = self
            .links
            .get_mut(links)
            .ok_or_else(|| ToolkitError::Parse(format!("chain {i}: link run out of range")))?;
        let mut cuts: Vec<usize> = (0..links.len()).filter(|&k| k == 0 || links[k].mass_inv == 0.0).collect();
        cuts.push(links.len());
        for w in cuts.windows(2) {
            for k in w[0]..w[1] {
                let (sin, cos) = cone_angle(p, k - w[0], w[1] - w[0]).sin_cos();
                let l = &mut links[k];
                l.cone_outer_cos = cos as f32;
                l.cone_outer_sin = sin as f32;
                l.cone_limits = cone_limits(sin, &p.limit_scales);
                if l.mass_inv != 0.0 {
                    l.mass_inv = mi;
                }
                l.mass_inv_child = mi;
            }
        }
        for b in self.bends.get_mut(bends).into_iter().flatten() {
            if b.mass_inv != 0.0 {
                b.mass_inv = mi;
            }
            b.mass_inv_child = mi;
        }
        // attaches belong to the chain owning their target point; zero fillers target their own source
        let mine = |a: u16, s: u16| a != s && points.contains(&(s as usize));
        for a in &mut self.attaches {
            if mine(a.attach_point, a.sim_point) {
                a.mass_inv_child = mi;
            }
        }
        for e in self.attach_block.iter_mut().flat_map(|b| b.chunks_exact_mut(12)) {
            if mine(u16::from_le_bytes([e[0], e[1]]), u16::from_le_bytes([e[2], e[3]])) {
                e[4..8].copy_from_slice(&mi.to_le_bytes());
            }
        }
        Ok(())
    }
}

/// `AnimDynamicsChainDef` authoring values; `Default` is the DDL's.
#[derive(Debug, Clone, Copy)]
pub struct ChainParams {
    pub constraint_type: u8,
    pub mass: f64,
    pub damping: f32,
    pub gravity: [f32; 3],
    /// Degrees, lerped from the first link to the last.
    pub cone_outer: f64,
    pub cone_outer_end: f64,
    /// Multipliers of the cone sine on the -Y, +Y, -Z, +Z limits; Half-Cone is (1, 1, 0, 1).
    pub limit_scales: [f64; 4],
}

impl Default for ChainParams {
    fn default() -> Self {
        Self {
            constraint_type: dynamics_constraint::SIMPLE,
            mass: 0.1,
            damping: 0.01,
            gravity: [0.0, -9.80665, 0.0],
            cone_outer: 30.0,
            cone_outer_end: 20.0,
            limit_scales: [1.0; 4],
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChainSpec {
    pub name: String,
    /// From `Dat1::intern_string`.
    pub name_offset: u32,
    /// j0 → jn, each the parent of the next; j0 stays on its animated parent, jn swings free.
    pub joints: Vec<u16>,
    pub params: ChainParams,
}

/// Bind pose composed into model space, in f64.
pub struct BindSkeleton {
    pub parents: Vec<i16>,
    pub local_rot: Vec<DQuat>,
    pub local_pos: Vec<DVec3>,
    pub rot: Vec<DQuat>,
    pub pos: Vec<DVec3>,
}

impl BindSkeleton {
    pub fn new(parents: &[i16], bind: &JointsTransform) -> Self {
        let mut s = Self { parents: parents.to_vec(), local_rot: Vec::new(), local_pos: Vec::new(), rot: Vec::new(), pos: Vec::new() };
        for (i, m) in bind.matrixes34.iter().enumerate().take(parents.len()) {
            let lr = DQuat::from_xyzw(m[4] as f64, m[5] as f64, m[6] as f64, m[7] as f64);
            let lp = DVec3::new(m[8] as f64, m[9] as f64, m[10] as f64);
            let (r, p) = match s.parent(i) {
                Some(p) => (s.rot[p] * lr, s.pos[p] + s.rot[p] * lp),
                None => (lr, lp),
            };
            s.local_rot.push(lr);
            s.local_pos.push(lp);
            s.rot.push(r);
            s.pos.push(p);
        }
        s.parents.truncate(s.pos.len());
        s
    }

    pub fn from_dat1(dat1: &Dat1) -> Result<Self> {
        let section = |tag| dat1.get_section_data(tag).ok_or(ToolkitError::SectionNotFound(tag));
        let parents: Vec<i16> = Joint::parse_all(section(TAG_JOINTS)?)?.iter().map(|j| j.parent).collect();
        Ok(Self::new(&parents, &JointsTransform::parse(section(TAG_JOINTS_TRANSFORM)?)?))
    }

    /// `None` for roots, and for a parent stored after its child (which composition can't use).
    pub fn parent(&self, j: usize) -> Option<usize> {
        usize::try_from(self.parents[j]).ok().filter(|&p| p < j)
    }
}

/// Moves joints to new model-space bind positions. Every joint keeps its model rotation and every
/// other joint its position, so only Bind Pose translations, the moved joints' inverse binds and
/// their bspheres (held in place in model space) change.
pub fn repose_joints(parents: &[i16], bind: &mut JointsTransform, bspheres: &mut JointBspheres, moves: &[(usize, [f64; 3])]) {
    let skel = BindSkeleton::new(parents, bind);
    let mut pos = skel.pos.clone();
    for &(j, p) in moves {
        pos[j] = DVec3::from_array(p);
    }
    let moved = |j: usize| moves.iter().any(|m| m.0 == j);
    for j in 0..pos.len() {
        let parent = skel.parent(j);
        if !moved(j) && !parent.is_some_and(|p| moved(p)) {
            continue;
        }
        let local = match parent {
            Some(p) => skel.rot[p].conjugate() * (pos[j] - pos[p]),
            None => pos[j],
        };
        bind.matrixes34[j][8..11].copy_from_slice(&local.as_vec3().to_array());
    }
    for &(j, _) in moves {
        // inverse binds are row-vector: joint = model * m[0..12] + m[12..15]
        let m = &mut bind.matrixes44[j];
        let t: [f32; 3] = std::array::from_fn(|c| -(0..3).map(|k| pos[j][k] * m[4 * k + c] as f64).sum::<f64>() as f32);
        let d = [t[0] - m[12], t[1] - m[13], t[2] - m[14]];
        m[12..15].copy_from_slice(&t);
        for s in bspheres.spheres.iter_mut().filter(|s| (s.joint / JointBspheres::JOINT_STRIDE) as usize == j) {
            s.center = (s.center.0 + d[0], s.center.1 + d[1], s.center.2 + d[2]);
        }
    }
}

fn run((start, count): (u16, u16)) -> std::ops::Range<usize> {
    start as usize..start as usize + count as usize
}

fn cone_angle(p: &ChainParams, k: usize, n: usize) -> f64 {
    let t = if n > 1 { k as f64 / (n - 1) as f64 } else { 0.0 };
    (p.cone_outer + (p.cone_outer_end - p.cone_outer) * t).to_radians()
}

fn cone_limits(sin: f64, s: &[f64; 4]) -> [f32; 4] {
    [(-sin * s[0]) as f32, (sin * s[1]) as f32, (-sin * s[2]) as f32, (sin * s[3]) as f32]
}

/// Shortest arc from +X to `d` (unit or zero).
fn arc_from_x(d: DVec3) -> DQuat {
    if 1.0 + d.x < 1e-6 {
        return DQuat::from_xyzw(0.0, 0.0, 1.0, 0.0);
    }
    let c = DVec3::X.cross(d);
    DQuat::from_xyzw(c.x, c.y, c.z, 1.0 + d.x).normalize()
}

fn put_u16s(out: &mut Vec<u8>, v: &[u16]) {
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
}

fn put_f32s(out: &mut Vec<u8>, v: &[f32]) {
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
}
