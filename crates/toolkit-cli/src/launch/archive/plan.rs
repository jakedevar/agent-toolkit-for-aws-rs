//! The materialisation plan and node budget (R-AR-PLAN, #5066).
//!
//! The record, byte and ratio limits count ZIP records, not what extraction
//! creates: every missing ancestor of a deep name is a directory to create.
//! [`plan`] counts every unique file, explicit directory and implicit
//! ancestor directory below the output root (raw-byte keyed, so it never
//! undercounts what a case-sensitive file system creates), merges explicit
//! and implicit directories, and refuses at the first node past the budget,
//! before anything exists on disk. Planning memory is bounded by the budget:
//! the node vector and the index are reserved once, at most `budget` slots
//! (fewer when the archive names fewer segments), and never grow. Nodes
//! reference their name bytes in the archive buffer; nothing is copied.

use std::collections::HashMap;

use super::zip_read::{Archive, EntryKind};
use crate::launch::LaunchError;

/// The production node budget (Q23, D22).
pub const MAX_SNAPSHOT_NODES: usize = 200_000;
/// The parent id of a top-level node.
pub const ROOT: u32 = u32::MAX;

/// A node's type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// A directory, explicit or implicit.
    Directory,
    /// A regular file.
    File,
}

/// One node to create: fixed size, its name referenced in the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    /// Parent node id, or [`ROOT`].
    pub parent: u32,
    /// Offset of the segment's bytes in the archive.
    pub name_offset: u32,
    /// Segment length (at most 255, R-AR-READ).
    pub name_len: u16,
    /// File or directory.
    pub kind: NodeKind,
}

/// The plan: nodes in creation order (every parent before its children).
#[derive(Debug)]
pub struct Plan {
    nodes: Vec<Node>,
    files: usize,
    directories: usize,
    entry_nodes: Vec<u32>,
}

impl Plan {
    /// The nodes, parents first.
    #[must_use]
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// Files to create.
    #[must_use]
    pub fn files(&self) -> usize {
        self.files
    }

    /// Directories to create (explicit and implicit).
    #[must_use]
    pub fn directories(&self) -> usize {
        self.directories
    }

    /// The node of each archive entry, in central order.
    #[must_use]
    pub fn entry_nodes(&self) -> &[u32] {
        &self.entry_nodes
    }
}

/// Instrumentation for the bounded-memory assertions (T-PLAN-1, -2).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PlanStats {
    /// The highest node number reached, counting the refused candidate.
    pub peak_nodes: usize,
    /// Slots asked of the index (never more than the budget).
    pub index_requested: usize,
    /// Index capacity right after the one reservation.
    pub index_capacity_reserved: usize,
    /// Index capacity at the end (equal: the index never grew).
    pub index_capacity_final: usize,
    /// Node-vector capacity right after the one reservation.
    pub nodes_capacity_reserved: usize,
    /// Node-vector capacity at the end.
    pub nodes_capacity_final: usize,
}

/// The fixed refusal for a production budget.
#[must_use]
pub fn budget_message(budget: usize) -> String {
    format!(
        "The snapshot would create more than {budget} files and directories and was not \
         extracted."
    )
}

/// Plan `archive` under `budget` nodes.
///
/// # Errors
/// [`budget_message`] at the first node past the budget; an internal error
/// if a file node is needed as a directory (R-AR-READ refuses that first).
pub fn plan(
    archive: &Archive<'_>,
    budget: usize,
    stats: &mut PlanStats,
) -> Result<Plan, LaunchError> {
    let data = archive.data();
    // An upper bound on the nodes the archive can name: one per segment.
    // Reserving `min(budget, segments)` keeps planning memory below the
    // budget whatever the archive declares, and never more than it needs.
    let segments: usize = archive
        .entries()
        .iter()
        .map(|entry| archive.name(entry).trim_end_matches('/').split('/').count())
        .fold(0, usize::saturating_add);
    let reserve = budget.min(segments);
    let mut nodes: Vec<Node> = Vec::with_capacity(reserve);
    let mut index: HashMap<(u32, &[u8]), u32> = HashMap::with_capacity(reserve);
    stats.index_requested = reserve;
    stats.index_capacity_reserved = index.capacity();
    stats.nodes_capacity_reserved = nodes.capacity();
    let mut entry_nodes = Vec::with_capacity(archive.entries().len());
    let (mut files, mut directories) = (0_usize, 0_usize);
    let internal =
        || LaunchError::Archive("Internal error: the snapshot plan is inconsistent.".to_string());
    for entry in archive.entries() {
        let name = archive.name(entry);
        let body = name.strip_suffix('/').unwrap_or(name);
        let mut parent = ROOT;
        let mut offset = entry.name_offset;
        let segment_count = body.split('/').count();
        for (position, segment) in body.split('/').enumerate() {
            let kind = if position + 1 == segment_count && entry.kind == EntryKind::File {
                NodeKind::File
            } else {
                NodeKind::Directory
            };
            let bytes = &data[offset..offset + segment.len()];
            let id = if let Some(&existing) = index.get(&(parent, bytes)) {
                if kind == NodeKind::File || nodes[existing as usize].kind == NodeKind::File {
                    return Err(internal());
                }
                existing
            } else {
                stats.peak_nodes = nodes.len() + 1;
                if nodes.len() == budget {
                    stats.index_capacity_final = index.capacity();
                    stats.nodes_capacity_final = nodes.capacity();
                    return Err(LaunchError::Archive(budget_message(budget)));
                }
                let id = u32::try_from(nodes.len()).map_err(|_| internal())?;
                nodes.push(Node {
                    parent,
                    name_offset: u32::try_from(offset).map_err(|_| internal())?,
                    name_len: u16::try_from(segment.len()).map_err(|_| internal())?,
                    kind,
                });
                index.insert((parent, bytes), id);
                match kind {
                    NodeKind::File => files += 1,
                    NodeKind::Directory => directories += 1,
                }
                id
            };
            parent = id;
            offset += segment.len() + 1;
        }
        entry_nodes.push(parent);
    }
    stats.index_capacity_final = index.capacity();
    stats.nodes_capacity_final = nodes.capacity();
    Ok(Plan {
        nodes,
        files,
        directories,
        entry_nodes,
    })
}

#[cfg(test)]
mod tests_plan;
