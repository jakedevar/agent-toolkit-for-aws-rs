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

use super::zip_read::Archive;
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
    let _ = (archive, budget, stats);
    todo!("L2: planner")
}

#[cfg(test)]
mod tests_plan;
