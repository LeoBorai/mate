//! Wires the default frontend (`M7`/`M8`): the same backend/agent setup `plain.rs` uses, routed
//! through `mate_core::session::SessionManager` and `mate_tui::run`. `M8-5` opens one tab per
//! `-C`/`--dir` path; `mate_tui::SessionDefaults` carries everything a tab opened later, via the
//! TUI's own `Ctrl+T` spawn form, needs to build the same kind of session.
//!
//! With `API_TOKEN` set, sessions are started up front and handed to `mate_tui::run`. Without
//! it, [`run`] hands `mate_tui::run_with_onboarding` a closure instead: the TUI walks the user
//! through backend → model → token, then calls that closure — the very same [`start_sessions`]
//! the token-set path calls itself — with the choices and the entered token.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use mate_core::cost::ModelRate;
use mate_core::model_catalog::CatalogBackend;
use mate_core::provider_error::ProviderError;
use mate_core::session::SessionManager;
use mate_tool_http::HttpShared;
use mate_tool_mcp::McpServers;
use mate_tui::{
    CompleteOnboarding, InitialSession, PendingOnboarding, SessionDefaults, StartedSessions,
};

use crate::config::{BackendKind, Config, api_token};
use crate::error::MateError;
use crate::plain::{
    DEFAULT_MAX_TOKENS, DEFAULT_TEMPERATURE, build_backend, classify_verify_error,
    mcp_server_specs, resolve_workspace_roots,
};

pub async fn run(cli: &crate::cli::Cli, config: &Config) -> Result<(), MateError> {
    match api_token() {
        Some(token) => {
            let started = start_sessions(cli, config, &token).await?;
            mate_tui::run(
                started.manager,
                started.events,
                started.sessions,
                started.defaults,
                started.pricing,
            )
            .await
            .map_err(|err| MateError::Io(anyhow::anyhow!(err)))
        }
        None => run_with_onboarding(cli, config).await,
    }
}

/// No `API_TOKEN` at startup: render the TUI anyway and let it collect a backend, model and
/// token. Workspace roots are resolved up front so a bad `-C` path fails before the UI opens
/// rather than after a token has been typed. Nothing else happens until onboarding completes —
/// no backend is built and nothing touches the network.
async fn run_with_onboarding(cli: &crate::cli::Cli, config: &Config) -> Result<(), MateError> {
    resolve_workspace_roots(cli)?;

    let cli = cli.clone();
    let config = config.clone();
    let complete: CompleteOnboarding = Box::new(move |backend, model, token| {
        let cli = cli.clone();
        let config = config.with_backend_and_model(backend_kind(backend), model);
        Box::pin(async move {
            start_sessions(&cli, &config, &token)
                .await
                .map_err(|err| err.to_string())
        })
    });

    mate_tui::run_with_onboarding(PendingOnboarding { complete })
        .await
        .map_err(|err| MateError::Io(anyhow::anyhow!(err)))
}

/// The one place a catalog backend becomes the CLI's `BackendKind` (see
/// `mate_core::model_catalog` for why they are distinct types).
fn backend_kind(backend: CatalogBackend) -> BackendKind {
    match backend {
        CatalogBackend::Huggingface => BackendKind::Huggingface,
        CatalogBackend::Gemini => BackendKind::Gemini,
    }
}

/// Builds the backend, verifies `token` against it, and spawns one session per workspace root:
/// everything the frontend needs before its first frame when a token is available. Shared by
/// the token-set path (called directly, in the same order as always) and onboarding (called
/// once the user has entered a token).
async fn start_sessions(
    cli: &crate::cli::Cli,
    config: &Config,
    token: &str,
) -> Result<StartedSessions, MateError> {
    let backend =
        build_backend(config, token).map_err(|err| MateError::Provider(anyhow::anyhow!(err)))?;
    backend
        .verify()
        .await
        .map_err(|err| classify_verify_error(ProviderError::classify(&err)))?;

    let roots = resolve_workspace_roots(cli)?;
    let mcp = Arc::new(McpServers::connect(mcp_server_specs(config)).await);
    let defaults = SessionDefaults {
        model: config.model.clone(),
        backend_name: config.backend.label().to_string(),
        sub_provider: config.sub_provider.clone(),
        temperature: DEFAULT_TEMPERATURE,
        max_tokens: DEFAULT_MAX_TOKENS,
        max_turns: config.max_turns,
        http: config.http.clone(),
        mcp_enabled: mcp.has_active_servers(),
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
        SessionManager::new(Arc::new(backend), http, mcp, config.max_sessions);
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
        let agents_md = ctx.agents_md.as_ref().map(|s| s.filename.to_string());
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
            agents_md,
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

    Ok(StartedSessions {
        manager,
        events: events_rx,
        sessions,
        defaults,
        pricing,
    })
}

fn title_for(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mate".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_backend_maps_to_its_matching_backend_kind() {
        assert_eq!(
            backend_kind(CatalogBackend::Huggingface),
            BackendKind::Huggingface,
            "the Hugging Face catalog backend selects the Hugging Face path"
        );
        assert_eq!(
            backend_kind(CatalogBackend::Gemini),
            BackendKind::Gemini,
            "the Gemini catalog backend selects the Gemini path"
        );
    }
}
