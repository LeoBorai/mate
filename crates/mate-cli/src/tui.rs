//! Wires the default frontend (`M7`): the same backend/agent setup `plain.rs` uses, but routed
//! through `mate_core::session::SessionManager` and `mate_tui::run` instead of a bare `Agent`.
//! Tabs and the side panel aren't here yet (`M8`/`M12`) — `M7` spawns exactly one session.

use std::sync::Arc;

use mate_core::backend::Backend;
use mate_core::config::{AgentSpec, SessionSpec};
use mate_core::preamble::{PreambleRole, render_preamble};
use mate_core::provider_error::ProviderError;
use mate_core::session::SessionManager;
use mate_core::toolset::tool_descriptors;
use mate_tool_api::{AgentId, ToolCtx};
use tokio_util::sync::CancellationToken;

use crate::config::{Config, api_token};
use crate::error::MateError;
use crate::plain::{
    DEFAULT_MAX_TOKENS, DEFAULT_TEMPERATURE, classify_verify_error, resolve_workspace_root,
};

pub async fn run(cli: &crate::cli::Cli, config: &Config) -> Result<(), MateError> {
    let token =
        api_token().ok_or_else(|| MateError::Auth(anyhow::anyhow!("API_TOKEN is not set")))?;

    let backend = Backend::huggingface(&token, config.sub_provider.as_deref(), None)
        .map_err(|err| MateError::Provider(anyhow::anyhow!(err)))?;
    backend
        .verify()
        .await
        .map_err(|err| classify_verify_error(ProviderError::classify(&err)))?;

    let workspace_root = resolve_workspace_root(cli)?;
    let title = workspace_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mate".to_string());
    let preamble = render_preamble(
        PreambleRole::Root,
        &workspace_root,
        std::env::consts::OS,
        &tool_descriptors(),
    );

    let spec = SessionSpec {
        title,
        root: workspace_root.clone(),
        agent: AgentSpec {
            model: config.model.clone(),
            sub_provider: config.sub_provider.clone(),
            base_url: None,
            preamble,
            temperature: DEFAULT_TEMPERATURE,
            max_tokens: DEFAULT_MAX_TOKENS,
            max_turns: config.max_turns,
            http: config.http.clone(),
            may_delegate: config.delegation.enabled,
            delegation: config.delegation.clone(),
        },
        delegation: config.delegation.clone(),
        max_turns: config.max_turns,
    };

    let (mut manager, events_rx) = SessionManager::new(Arc::new(backend), config.max_sessions);
    let (activity, _activity_rx) = tokio::sync::mpsc::channel(64);
    let ctx = ToolCtx {
        agent: AgentId::ROOT,
        root: workspace_root,
        max_output_bytes: config.tools.max_output_bytes,
        spawner: None,
        activity,
        cancel: CancellationToken::new(),
    };
    let handle = manager
        .spawn(&spec, ctx)
        .map_err(|err| MateError::Other(anyhow::anyhow!(err)))?;
    let session_id = handle.id;

    mate_tui::run(handle, session_id, events_rx)
        .await
        .map_err(|err| MateError::Io(anyhow::anyhow!(err)))
}
