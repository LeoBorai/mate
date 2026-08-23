//! Wires the default frontend (`M7`/`M8`): the same backend/agent setup `plain.rs` uses, routed
//! through `mate_core::session::SessionManager` and `mate_tui::run`. `M8-5` opens one tab per
//! `-C`/`--dir` path; `mate_tui::SessionDefaults` carries everything a tab opened later, via the
//! TUI's own `Ctrl+T` spawn form, needs to build the same kind of session.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use mate_core::backend::Backend;
use mate_core::cost::ModelRate;
use mate_core::provider_error::ProviderError;
use mate_core::session::SessionManager;
use mate_tool_http::HttpShared;
use mate_tui::{InitialSession, SessionDefaults};

use crate::config::{Config, api_token};
use crate::error::MateError;
use crate::plain::{
    DEFAULT_MAX_TOKENS, DEFAULT_TEMPERATURE, classify_verify_error, resolve_workspace_roots,
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

    let roots = resolve_workspace_roots(cli)?;
    let defaults = SessionDefaults {
        model: config.model.clone(),
        sub_provider: config.sub_provider.clone(),
        temperature: DEFAULT_TEMPERATURE,
        max_tokens: DEFAULT_MAX_TOKENS,
        max_turns: config.max_turns,
        http: config.http.clone(),
        delegation: config.delegation.clone(),
        max_output_bytes: config.tools.max_output_bytes,
        agents_md_enabled: config.agents_md.enabled,
        agents_md_max_bytes: config.agents_md.max_bytes,
    };

    let http = Arc::new(
        HttpShared::new(config.http.rate_limit_per_host_per_min)
            .map_err(|err| MateError::Other(anyhow::anyhow!(err)))?,
    );
    let (mut manager, events_rx) =
        SessionManager::new(Arc::new(backend), http, config.max_sessions);
    let provider = defaults.provider_label();
    let subagent_model = defaults.subagent_model_label();

    let mut sessions = Vec::with_capacity(roots.len());
    for root in &roots {
        let title = title_for(root);
        let spec = mate_tui::build_spec(&defaults, root, title.clone(), defaults.http.enabled);
        let ctx = mate_tui::build_tool_ctx(
            root.clone(),
            defaults.max_output_bytes,
            defaults.agents_md_enabled,
            defaults.agents_md_max_bytes,
        );
        let skills = ctx.skills.to_vec();
        let handle = manager
            .spawn(&spec, ctx)
            .map_err(|err| MateError::Other(anyhow::anyhow!(err)))?;
        sessions.push(InitialSession {
            session_id: handle.id,
            handle,
            title,
            model: defaults.model.clone(),
            provider: provider.clone(),
            root: root.clone(),
            subagent_model: subagent_model.clone(),
            http_enabled: defaults.http.enabled,
            may_delegate: defaults.delegation.enabled,
            skills,
        });
    }

    let pricing: HashMap<String, ModelRate> = config
        .pricing
        .iter()
        .map(|(model, entry)| {
            (
                model.clone(),
                ModelRate {
                    input_per_million: entry.input,
                    output_per_million: entry.output,
                },
            )
        })
        .collect();

    mate_tui::run(manager, events_rx, sessions, defaults, pricing)
        .await
        .map_err(|err| MateError::Io(anyhow::anyhow!(err)))
}

fn title_for(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mate".to_string())
}
