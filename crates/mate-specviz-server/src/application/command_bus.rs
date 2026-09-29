use std::sync::Arc;

use tokio::sync::broadcast;

use crate::application::commands::{
    InvalidateSpec, RefreshSpecIndex, invalidate_spec, refresh_spec_index,
};
use crate::application::error::ApplicationError;
use crate::application::events::SpecChanged;
use crate::application::index::SpecIndex;
use crate::infra::spec_repository::SpecRepository;

/// The only way anything outside `application/` mutates app state. axum
/// handlers and the watcher task both go through this — never through
/// `infra::*` directly.
pub struct CommandBus {
    repo: Arc<dyn SpecRepository>,
    index: Arc<SpecIndex>,
    events: broadcast::Sender<SpecChanged>,
}

impl CommandBus {
    pub fn new(repo: Arc<dyn SpecRepository>, index: Arc<SpecIndex>) -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            repo,
            index,
            events,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SpecChanged> {
        self.events.subscribe()
    }

    pub async fn refresh_spec_index(&self, cmd: RefreshSpecIndex) -> Result<(), ApplicationError> {
        refresh_spec_index::handle(self.repo.as_ref(), &self.index, cmd).await?;
        let _ = self.events.send(SpecChanged::IndexRefreshed);
        Ok(())
    }

    pub fn invalidate_spec(&self, cmd: InvalidateSpec) {
        invalidate_spec::handle(&cmd);
        let _ = self.events.send(SpecChanged::Spec(cmd.0));
    }
}
