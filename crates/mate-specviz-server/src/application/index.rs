use std::sync::RwLock;

use crate::domain::SpecTree;

/// The one piece of mutable state the application layer owns: a cached
/// snapshot of the spec tree, rebuilt by `RefreshSpecIndex`. Queries read
/// this instead of hitting the filesystem on every `ListSpecs` call.
#[derive(Default)]
pub struct SpecIndex(RwLock<SpecTree>);

impl SpecIndex {
    pub fn empty() -> Self {
        Self(RwLock::new(SpecTree::empty()))
    }

    pub fn replace(&self, tree: SpecTree) {
        *self.0.write().expect("SpecIndex lock poisoned") = tree;
    }

    pub fn snapshot(&self) -> SpecTree {
        self.0.read().expect("SpecIndex lock poisoned").clone()
    }
}
