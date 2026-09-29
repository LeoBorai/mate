use std::sync::Arc;

use crate::application::error::ApplicationError;
use crate::application::index::SpecIndex;
use crate::application::queries::{
    GetSpec, ListSpecs, RenderSpec, RenderedSpec, get_spec, list_specs, render_spec,
};
use crate::domain::{Spec, SpecTree};
use crate::infra::markdown::MarkdownRenderer;
use crate::infra::spec_repository::SpecRepository;

/// The only way anything outside `application/` reads app state. axum's
/// JSON route handlers are the sole callers.
pub struct QueryBus {
    repo: Arc<dyn SpecRepository>,
    index: Arc<SpecIndex>,
    renderer: Arc<dyn MarkdownRenderer>,
}

impl QueryBus {
    pub fn new(
        repo: Arc<dyn SpecRepository>,
        index: Arc<SpecIndex>,
        renderer: Arc<dyn MarkdownRenderer>,
    ) -> Self {
        Self {
            repo,
            index,
            renderer,
        }
    }

    pub fn list_specs(&self, query: ListSpecs) -> SpecTree {
        list_specs::handle(&self.index, query)
    }

    /// Not exposed over HTTP yet — used internally by `render_spec`. Kept
    /// for parity with the rest of the query set.
    #[allow(dead_code)]
    pub async fn get_spec(&self, query: GetSpec) -> Result<Spec, ApplicationError> {
        get_spec::handle(self.repo.as_ref(), query).await
    }

    pub async fn render_spec(&self, query: RenderSpec) -> Result<RenderedSpec, ApplicationError> {
        render_spec::handle(self.repo.as_ref(), self.renderer.as_ref(), query).await
    }
}
