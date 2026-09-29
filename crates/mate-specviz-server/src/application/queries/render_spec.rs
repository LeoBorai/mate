use serde::Serialize;

use crate::application::error::ApplicationError;
use crate::domain::SpecId;
use crate::infra::markdown::MarkdownRenderer;
use crate::infra::spec_repository::SpecRepository;

pub struct RenderSpec(pub SpecId);

/// Wire shape for `GET /api/specs/*id`.
#[derive(Debug, Clone, Serialize)]
pub struct RenderedSpec {
    pub title: String,
    pub html: String,
    pub path: String,
}

pub async fn handle(
    repo: &dyn SpecRepository,
    renderer: &dyn MarkdownRenderer,
    query: RenderSpec,
) -> Result<RenderedSpec, ApplicationError> {
    let spec = repo.read_spec(&query.0).await?;
    let html = renderer.render(&spec.raw_markdown, spec.kind);

    Ok(RenderedSpec {
        title: spec.meta.title,
        html,
        path: spec.meta.relative_path.to_string_lossy().into_owned(),
    })
}
