//! One held directory descriptor moved around a tree of recorded nodes
//! (R-AR-STAGE creation and cleanup, R-AR-APPLY's two phases).
//!
//! Holding one descriptor per path component would need as many open
//! descriptors as the deepest path (up to 2 048), past the usual soft limit
//! of 1 024. The navigator instead holds the root and one current
//! directory, and moves between nodes through their lowest common
//! ancestor: up with `openat(cur, "..", O_DIRECTORY | O_NOFOLLOW)`, down
//! with `openat(cur, name, O_DIRECTORY | O_NOFOLLOW)`. Every step compares
//! the entered directory's `(st_dev, st_ino)` with the identity recorded
//! for that node (the root's own for the top), so a directory renamed or
//! swapped for a symlink stops the walk instead of redirecting it. A step
//! into a node with no recorded identity records it (the first entry of
//! R-AR-APPLY's phase 1). Visiting nodes in depth-first order makes the
//! total number of steps linear in the number of nodes.

use std::ffi::CString;

use super::fsops::{enter_dir, DirHandle, OpenError};

/// `(st_dev, st_ino)`.
pub type Ident = (u64, u64);

/// The parent of a top-level node.
pub const ROOT: u32 = u32::MAX;

/// The shape the navigator walks.
pub trait Tree {
    /// Parent node, or [`ROOT`].
    fn parent(&self, node: u32) -> u32;
    /// Components below the root (a top-level node is 1, [`ROOT`] is 0).
    fn depth(&self, node: u32) -> usize;
    /// The node's own name.
    fn name(&self, node: u32) -> CString;
}

/// The moving descriptor.
#[derive(Debug)]
pub struct Navigator {
    root: DirHandle,
    at: u32,
    current: Option<DirHandle>,
}

fn changed() -> OpenError {
    OpenError::Changed
}

impl Navigator {
    /// A navigator over `root` (held for the navigator's life).
    #[must_use]
    pub fn new(root: DirHandle) -> Self {
        Self {
            root,
            at: ROOT,
            current: None,
        }
    }

    /// The root.
    #[must_use]
    pub fn root(&self) -> &DirHandle {
        &self.root
    }

    /// Move to directory node `target` ([`ROOT`] for the root) and return
    /// its held descriptor. `idents[node]` is the node's recorded identity.
    ///
    /// # Errors
    /// [`OpenError::Changed`] when a step meets a symlink, a non-directory,
    /// a vanished entry or another identity; the next call restarts from the
    /// root.
    pub fn goto(
        &mut self,
        tree: &dyn Tree,
        target: u32,
        idents: &mut [Option<Ident>],
    ) -> Result<&DirHandle, OpenError> {
        let result = self.walk(tree, target, idents);
        if result.is_err() {
            self.current = None;
            self.at = ROOT;
            return Err(changed());
        }
        Ok(self.current.as_ref().unwrap_or(&self.root))
    }

    fn walk(
        &mut self,
        tree: &dyn Tree,
        target: u32,
        idents: &mut [Option<Ident>],
    ) -> Result<(), OpenError> {
        if self.current.is_none() {
            self.at = ROOT;
        }
        let depth = |node: u32| if node == ROOT { 0 } else { tree.depth(node) };
        let (mut a, mut b) = (self.at, target);
        let mut ups = 0_usize;
        let mut down = Vec::new();
        while depth(a) > depth(b) {
            a = tree.parent(a);
            ups += 1;
        }
        while depth(b) > depth(a) {
            down.push(b);
            b = tree.parent(b);
        }
        while a != b {
            a = tree.parent(a);
            ups += 1;
            down.push(b);
            b = tree.parent(b);
        }
        if a == ROOT && ups > 0 {
            // Back to the root: the held root, never `..` past it.
            self.current = None;
            self.at = ROOT;
        } else {
            let dotdot = CString::new("..").map_err(|_| changed())?;
            for _ in 0..ups {
                let parent = tree.parent(self.at);
                let current = self.current.as_ref().ok_or_else(changed)?;
                let entered = enter_dir(current, &dotdot)?;
                let expected = idents.get(parent as usize).copied().flatten();
                if expected != Some(entered.identity()) {
                    return Err(changed());
                }
                self.current = Some(entered);
                self.at = parent;
            }
        }
        for node in down.into_iter().rev() {
            let from = self.current.as_ref().unwrap_or(&self.root);
            let entered = enter_dir(from, &tree.name(node))?;
            let slot = idents.get_mut(node as usize).ok_or_else(changed)?;
            match slot {
                Some(expected) if *expected != entered.identity() => return Err(changed()),
                Some(_) => {}
                None => *slot = Some(entered.identity()),
            }
            self.current = Some(entered);
            self.at = node;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests_nav;
