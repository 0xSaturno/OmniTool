//! Rig-runtime sections that sit alongside the skeleton: joint bind chains, IK
//! chains and the joint-set list. None of the three has a recovered name — the
//! tags are only reachable by hash — so they are named after what they contain.

use crate::core::error::Result;
use byteorder::{ReadBytesExt, LE};
use std::io::Cursor;

/// Unnamed. Bind matrices plus a flattened ancestor chain, for 19 rig joints on
/// both hero models.
pub const TAG_JOINT_BIND_CHAINS: u32 = 0x707F1B58;
/// Unnamed. Limb IK solvers keyed by locator hash.
pub const TAG_IK_CHAINS: u32 = 0x9A434B29;
/// Unnamed. A joint index list plus an identity byte map.
pub const TAG_JOINT_SET: u32 = 0x5A39FAB7;
pub const TAG_ANIM_DYNAMICS_DEF: u32 = 0xADD1CBD3;

// ---------------------------------------------------------------- bind chains

#[derive(Debug, Clone, Copy, Default)]
pub struct JointBindChainsHeader {
    pub header_size: u32,
    /// Section-relative offset of the flattened chain array.
    pub chains_offset: u32,
    pub entry_count: u16,
    pub chain_total: u16,
    pub unk: u32,
}

/// 144 bytes: the joint's bind matrix, its inverse, then the descriptor.
#[derive(Debug, Clone)]
pub struct JointBindChain {
    pub matrix: [f32; 16],
    pub inverse: [f32; 16],
    pub joint_hash: u32,
    pub joint: u16,
    pub unk_b: u16,
    /// Parent joint, or 0xFFFF at the root of the chain.
    pub parent: u16,
    /// First element of this joint's ancestor run in the chain array.
    pub chain_start: u16,
    pub chain_len: u32,
}

pub struct JointBindChains {
    pub header: JointBindChainsHeader,
    pub entries: Vec<JointBindChain>,
    /// Joint indices, walked parent-ward; slice with `chain_start`/`chain_len`.
    pub chains: Vec<u16>,
}

impl JointBindChains {
    pub const ENTRY_SIZE: usize = 144;

    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 16 {
            return Ok(Self {
                header: JointBindChainsHeader::default(),
                entries: Vec::new(),
                chains: Vec::new(),
            });
        }
        let mut cur = Cursor::new(data);
        let header = JointBindChainsHeader {
            header_size: cur.read_u32::<LE>()?,
            chains_offset: cur.read_u32::<LE>()?,
            entry_count: cur.read_u16::<LE>()?,
            chain_total: cur.read_u16::<LE>()?,
            unk: cur.read_u32::<LE>()?,
        };

        let mut entries = Vec::with_capacity(header.entry_count as usize);
        for i in 0..header.entry_count as usize {
            let base = header.header_size as usize + i * Self::ENTRY_SIZE;
            if base + Self::ENTRY_SIZE > data.len() {
                break;
            }
            let mut c = Cursor::new(&data[base..]);
            let mut matrix = [0f32; 16];
            for f in &mut matrix {
                *f = c.read_f32::<LE>()?;
            }
            let mut inverse = [0f32; 16];
            for f in &mut inverse {
                *f = c.read_f32::<LE>()?;
            }
            entries.push(JointBindChain {
                matrix,
                inverse,
                joint_hash: c.read_u32::<LE>()?,
                joint: c.read_u16::<LE>()?,
                unk_b: c.read_u16::<LE>()?,
                parent: c.read_u16::<LE>()?,
                chain_start: c.read_u16::<LE>()?,
                chain_len: c.read_u32::<LE>()?,
            });
        }

        let mut chains = Vec::with_capacity(header.chain_total as usize);
        let mut c = Cursor::new(data);
        c.set_position(header.chains_offset as u64);
        for _ in 0..header.chain_total {
            if c.position() as usize + 2 > data.len() {
                break;
            }
            chains.push(c.read_u16::<LE>()?);
        }
        Ok(Self {
            header,
            entries,
            chains,
        })
    }

    pub fn chain_of(&self, entry: &JointBindChain) -> &[u16] {
        let a = entry.chain_start as usize;
        let b = (a + entry.chain_len as usize).min(self.chains.len());
        if a >= b {
            &[]
        } else {
            &self.chains[a..b]
        }
    }
}

// ------------------------------------------------------------------ IK chains

#[derive(Debug, Clone, Copy, Default)]
pub struct IkChainsHeader {
    pub size: u32,
    pub solvers_offset: u32,
    pub unk_block_offset: u32,
    pub limits_offset: u32,
    pub joints_offset: u32,
    pub solver_count: u8,
    pub limit_count: u8,
}

/// 108 bytes. `locator_hash` names the effector (`igLoc_foot_l`, …) and
/// `first_joint` slices the joint array at `joints_offset`.
#[derive(Debug, Clone)]
pub struct IkSolver {
    pub tolerance: f32,
    pub locator_hash: u32,
    pub first_joint: u16,
    pub first_limit: u16,
    pub tag: [u8; 4],
    /// 23 floats: an orientation quaternion followed by basis rows. Semantics
    /// past the first four are unconfirmed.
    pub params: [f32; 23],
}

/// 16 bytes: which axis is constrained and the angle range allowed on it.
#[derive(Debug, Clone, Copy)]
pub struct IkLimit {
    pub axis: u8,
    pub unk1: u8,
    pub unk2: u16,
    pub min: f32,
    pub max: f32,
    pub value: f32,
}

pub struct IkChains {
    pub header: IkChainsHeader,
    pub solvers: Vec<IkSolver>,
    pub limits: Vec<IkLimit>,
    /// Joint indices; each solver takes a run of `tag[2]` of them.
    pub joints: Vec<u16>,
}

impl IkChains {
    pub const SOLVER_SIZE: usize = 108;
    pub const LIMIT_SIZE: usize = 16;

    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 24 {
            return Ok(Self {
                header: IkChainsHeader::default(),
                solvers: Vec::new(),
                limits: Vec::new(),
                joints: Vec::new(),
            });
        }
        let mut cur = Cursor::new(data);
        let header = IkChainsHeader {
            size: cur.read_u32::<LE>()?,
            solvers_offset: cur.read_u32::<LE>()?,
            unk_block_offset: cur.read_u32::<LE>()?,
            limits_offset: cur.read_u32::<LE>()?,
            joints_offset: cur.read_u32::<LE>()?,
            solver_count: cur.read_u8()?,
            limit_count: cur.read_u8()?,
        };

        let mut solvers = Vec::with_capacity(header.solver_count as usize);
        for i in 0..header.solver_count as usize {
            let base = header.solvers_offset as usize + i * Self::SOLVER_SIZE;
            if base + Self::SOLVER_SIZE > data.len() {
                break;
            }
            let mut c = Cursor::new(&data[base..]);
            let tolerance = c.read_f32::<LE>()?;
            let locator_hash = c.read_u32::<LE>()?;
            let first_joint = c.read_u16::<LE>()?;
            let first_limit = c.read_u16::<LE>()?;
            let mut tag = [0u8; 4];
            for t in &mut tag {
                *t = c.read_u8()?;
            }
            let mut params = [0f32; 23];
            for p in &mut params {
                *p = c.read_f32::<LE>()?;
            }
            solvers.push(IkSolver {
                tolerance,
                locator_hash,
                first_joint,
                first_limit,
                tag,
                params,
            });
        }

        // The header's second count byte does not match the block, so the limit
        // count comes from the gap between the limit and joint blocks instead.
        let limit_count = header
            .joints_offset
            .saturating_sub(header.limits_offset) as usize
            / Self::LIMIT_SIZE;
        let mut limits = Vec::with_capacity(limit_count);
        let mut c = Cursor::new(data);
        c.set_position(header.limits_offset as u64);
        for _ in 0..limit_count {
            if c.position() as usize + Self::LIMIT_SIZE > data.len() {
                break;
            }
            limits.push(IkLimit {
                axis: c.read_u8()?,
                unk1: c.read_u8()?,
                unk2: c.read_u16::<LE>()?,
                min: c.read_f32::<LE>()?,
                max: c.read_f32::<LE>()?,
                value: c.read_f32::<LE>()?,
            });
        }

        let mut joints = Vec::new();
        let mut c = Cursor::new(data);
        c.set_position(header.joints_offset as u64);
        while (c.position() as usize) + 2 <= data.len() {
            joints.push(c.read_u16::<LE>()?);
        }
        Ok(Self {
            header,
            solvers,
            limits,
            joints,
        })
    }

    /// Joints driven by one solver — `tag[2]` is the chain length (3 on every
    /// sample: upper limb, lower limb, end effector).
    pub fn joints_of(&self, s: &IkSolver) -> &[u16] {
        let a = s.first_joint as usize;
        let b = (a + s.tag[2] as usize).min(self.joints.len());
        if a >= b {
            &[]
        } else {
            &self.joints[a..b]
        }
    }
}

// ------------------------------------------------------------------ joint set

/// Two arrays behind a 16-byte header: a u16 joint index list, then a byte list
/// that is the identity map `0..count` on every sample seen. The second array
/// starts at the next 16-byte boundary after the first and carries its own
/// `(u16 kind, u16 count)` sub-header.
pub struct JointSet {
    pub version: u16,
    pub kind: u32,
    pub joints: Vec<u16>,
    pub map_kind: u16,
    pub map: Vec<u8>,
}

impl JointSet {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 16 {
            return Ok(Self {
                version: 0,
                kind: 0,
                joints: Vec::new(),
                map_kind: 0,
                map: Vec::new(),
            });
        }
        let mut cur = Cursor::new(data);
        let version = cur.read_u16::<LE>()?;
        let count = cur.read_u16::<LE>()? as usize;
        let kind = cur.read_u32::<LE>()?;

        cur.set_position(16);
        let mut joints = Vec::with_capacity(count);
        for _ in 0..count {
            if cur.position() as usize + 2 > data.len() {
                break;
            }
            joints.push(cur.read_u16::<LE>()?);
        }

        let mut p = 16 + count * 2;
        p = (p + 15) & !15;
        let (mut map_kind, mut map) = (0u16, Vec::new());
        if p + 4 <= data.len() {
            map_kind = u16::from_le_bytes(data[p..p + 2].try_into().unwrap());
            let n = u16::from_le_bytes(data[p + 2..p + 4].try_into().unwrap()) as usize;
            let end = (p + 4 + n).min(data.len());
            map = data[p + 4..end].to_vec();
        }
        Ok(Self {
            version,
            kind,
            joints,
            map_kind,
            map,
        })
    }
}
