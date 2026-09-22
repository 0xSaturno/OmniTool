//! Rig-runtime sections that sit alongside the skeleton: ragdoll bodies, IK
//! chains, cloth metadata and anim-dynamics (jiggle) chains. The first three
//! tags have no recovered name string; the layouts are verified on the corpus.

use crate::core::error::Result;
use byteorder::{ReadBytesExt, LE};
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
    pub points: Vec<[f32; 3]>,
    pub links: Vec<DynamicsLink>,
    pub attaches: Vec<DynamicsAttach>,
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
        c.set_position(20);
        let mut offs = [0usize; 7];
        for o in &mut offs {
            *o = c.read_u32::<LE>()? as usize;
        }
        out.collider_count = counts[4] as u16;
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
}
