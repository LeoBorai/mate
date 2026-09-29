use crate::application::index::SpecIndex;
use crate::domain::SpecTree;

pub struct ListSpecs;

pub fn handle(index: &SpecIndex, _query: ListSpecs) -> SpecTree {
    index.snapshot()
}
