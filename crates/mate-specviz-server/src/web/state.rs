use std::sync::Arc;

use crate::application::{CommandBus, QueryBus};

#[derive(Clone)]
pub struct AppState {
    pub commands: Arc<CommandBus>,
    pub queries: Arc<QueryBus>,
}
