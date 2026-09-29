use crate::application::error::ApplicationError;
use crate::domain::{Spec, SpecId};
use crate::infra::spec_repository::SpecRepository;

pub struct GetSpec(pub SpecId);

pub async fn handle(repo: &dyn SpecRepository, query: GetSpec) -> Result<Spec, ApplicationError> {
    repo.read_spec(&query.0).await.map_err(Into::into)
}
