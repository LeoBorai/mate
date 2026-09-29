use crate::application::error::ApplicationError;
use crate::application::index::SpecIndex;
use crate::infra::spec_repository::SpecRepository;

/// Re-walks every source and rebuilds the in-memory tree. Triggered at startup
/// and by the watcher on any create/rename/remove.
pub struct RefreshSpecIndex;

pub async fn handle(
    repo: &dyn SpecRepository,
    index: &SpecIndex,
    _cmd: RefreshSpecIndex,
) -> Result<(), ApplicationError> {
    let tree = repo.walk_specs().await?;
    index.replace(tree);
    Ok(())
}
