use std::collections::HashMap;

use crate::core::error::{Result, ToolkitError};
use crate::tools::model_converter::sections::meshes::MeshDefinition;

/// Where every subset's geometry lands in the rebuilt pools, one entry per
/// mesh definition.
pub struct BlockLayout {
    pub vertex_start: Vec<u32>,
    pub index_start: Vec<u32>,
    /// The subset whose content fills each subset's block. A subset sharing its
    /// source range with an earlier one points back at that earlier subset.
    pub owner: Vec<usize>,
    /// Block owners in destination order.
    pub order: Vec<usize>,
    pub total_vertices: usize,
    pub total_indices: usize,
}

impl BlockLayout {
    pub fn is_owner(&self, mesh: usize) -> bool {
        self.owner[mesh] == mesh
    }
}

/// Plans the destination layout of the vertex and index pools.
///
/// The pools are addressed by *block*, not by subset: several mesh definitions
/// legitimately point at one range, which is how the game gives LOD and look
/// variants a single copy of shared geometry. wpn_sheepinator's 18 subsets
/// cover 3 blocks; enm_thug_brawler's 1046 cover 1044. Laying subsets out with
/// a plain running sum hands every alias its own copy, inflating the pools (6x
/// on wpn_sheepinator, +72 vertices on enm_thug_brawler) and desynchronising
/// every pointer past the first alias.
///
/// Blocks are allocated in source order, so a model re-imported at its original
/// subset sizes reproduces the vanilla layout exactly.
pub fn plan_blocks(
    meshes: &[MeshDefinition],
    injected_counts: &HashMap<usize, (u32, u32)>,
) -> Result<BlockLayout> {
    let n = meshes.len();

    // Vertex and index ranges alias together in every sample model, so one key
    // covers both. A subset that shared only one of the two would fall out as
    // its own block, which is safe: it just costs a copy.
    let mut groups: HashMap<(u32, u32, u32, u32), Vec<usize>> = HashMap::new();
    for (mi, m) in meshes.iter().enumerate() {
        groups
            .entry((m.vertex_start, m.vertex_count, m.index_start, m.index_count))
            .or_default()
            .push(mi);
    }

    // An injected subset always gets its own block — its new contents no longer
    // match the subsets it used to share a range with.
    let mut blocks: Vec<(usize, Vec<usize>)> = Vec::with_capacity(n);
    for members in groups.into_values() {
        let shared: Vec<usize> = members
            .iter()
            .copied()
            .filter(|mi| !injected_counts.contains_key(mi))
            .collect();
        if let Some(&first) = shared.first() {
            blocks.push((first, shared));
        }
        for mi in members.into_iter().filter(|mi| injected_counts.contains_key(mi)) {
            blocks.push((mi, vec![mi]));
        }
    }
    blocks.sort_by_key(|(owner, _)| {
        (meshes[*owner].vertex_start, meshes[*owner].index_start, *owner)
    });

    let mut vertex_start = vec![0u32; n];
    let mut index_start = vec![0u32; n];
    let mut owner = vec![0usize; n];
    let mut order = Vec::with_capacity(blocks.len());
    let mut cv: u64 = 0;
    let mut ci: u64 = 0;

    for (block_owner, members) in blocks {
        let (vc, ic) = injected_counts.get(&block_owner).copied().unwrap_or((
            meshes[block_owner].vertex_count,
            meshes[block_owner].index_count,
        ));
        let vs = u32::try_from(cv).map_err(|_| {
            ToolkitError::Parse(format!("vertex_start overflow at subset {}", block_owner))
        })?;
        let is = u32::try_from(ci).map_err(|_| {
            ToolkitError::Parse(format!("index_start overflow at subset {}", block_owner))
        })?;
        for mi in members {
            vertex_start[mi] = vs;
            index_start[mi] = is;
            owner[mi] = block_owner;
        }
        order.push(block_owner);
        cv += vc as u64;
        ci += ic as u64;
    }

    Ok(BlockLayout {
        vertex_start,
        index_start,
        owner,
        order,
        total_vertices: cv as usize,
        total_indices: ci as usize,
    })
}
