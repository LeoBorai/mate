use crate::infra::spec_repository::SpecRepositoryError;

#[derive(Debug, thiserror::Error)]
pub enum ApplicationError {
    #[error(transparent)]
    Repository(#[from] SpecRepositoryError),
}

impl ApplicationError {
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            ApplicationError::Repository(SpecRepositoryError::NotFound(_))
        )
    }
}
