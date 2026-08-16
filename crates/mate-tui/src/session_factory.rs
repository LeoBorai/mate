//! Builds the pieces `SessionManager::spawn` needs (`M8-3`, `M8-5`): the same [`SessionSpec`] /
//! [`ToolCtx`] assembly `mate-cli`'s `tui.rs` already does once at startup, factored out so the
//! `Ctrl+T` spawn form (`crate::app`) can do it again for every tab opened after that, without
//! `mate-tui` reaching back into `mate-cli`.
//!
//! [`SessionDefaults`] is the knob set every new tab starts from — the loaded `Config`, minus
//! the workspace root and title, which are per-tab. Cloning it per spawn is cheap (a handful of
//! strings and small enums) and keeps the borrow checker out of the spawn form entirely.

use std::path::{Path, PathBuf};

use mate_core::config::{AgentSpec, DelegationPolicy, HttpPolicy, SessionSpec};
use mate_core::preamble::{PreambleRole, render_preamble};
use mate_core::toolset::tool_descriptors;
use mate_tool_api::{AgentId, ToolCtx};
use tokio_util::sync::CancellationToken;

/// Everything a new session's [`AgentSpec`] needs besides its workspace root and title —
/// carried once from `mate-cli`'s loaded `Config` and reused for every tab, including ones
/// opened later through the spawn form.
#[derive(Debug, Clone)]
pub struct SessionDefaults {
    pub model: String,
    pub sub_provider: Option<String>,
    pub temperature: f64,
    pub max_tokens: u64,
    pub max_turns: usize,
    pub http: HttpPolicy,
    pub delegation: DelegationPolicy,
    pub max_output_bytes: usize,
}

impl SessionDefaults {
    /// The provider label shown in a tab's model panel — `Backend` never stores this as a
    /// string (§ providers.md), so it's derived here from config the same way `mate-cli`
    /// already did for the single-tab `M7` wiring.
    pub fn provider_label(&self) -> String {
        self.sub_provider
            .clone()
            .unwrap_or_else(|| "huggingface".to_string())
    }
}

/// Assembles one session's spec (§4, §5.1) from `defaults` plus the per-tab `root` and `title`.
/// `http_enabled` overrides `defaults.http.enabled` only — the rest of the http policy (rate
/// limit, public/localhost) stays shared, matching §7.4's "narrowing-only" rule for anything
/// that scopes a tab down rather than up.
pub fn build_spec(
    defaults: &SessionDefaults,
    root: &Path,
    title: String,
    http_enabled: bool,
) -> SessionSpec {
    let mut http = defaults.http.clone();
    http.enabled = http_enabled;

    let preamble = render_preamble(
        PreambleRole::Root,
        root,
        std::env::consts::OS,
        &tool_descriptors(defaults.delegation.enabled),
    );

    let agent = AgentSpec {
        model: defaults.model.clone(),
        sub_provider: defaults.sub_provider.clone(),
        base_url: None,
        preamble,
        temperature: defaults.temperature,
        max_tokens: defaults.max_tokens,
        max_turns: defaults.max_turns,
        http,
        may_delegate: defaults.delegation.enabled,
        delegation: defaults.delegation.clone(),
    };

    SessionSpec {
        title,
        root: root.to_path_buf(),
        agent,
        delegation: defaults.delegation.clone(),
        max_turns: defaults.max_turns,
    }
}

/// Builds the root agent's [`ToolCtx`] for a new session. The activity sink's receiver is
/// dropped immediately — telemetry has no consumer until `M11`, so every spawn path in this
/// crate (and `mate-cli`'s `M7`/`M5` wiring before it) does the same thing.
pub fn build_tool_ctx(root: PathBuf, max_output_bytes: usize) -> ToolCtx {
    let (activity, _activity_rx) = tokio::sync::mpsc::channel(64);
    ToolCtx {
        agent: AgentId::ROOT,
        root,
        max_output_bytes,
        spawner: None,
        activity,
        cancel: CancellationToken::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> SessionDefaults {
        SessionDefaults {
            model: "org/model".to_string(),
            sub_provider: None,
            temperature: 0.2,
            max_tokens: 512,
            max_turns: 4,
            http: HttpPolicy::default(),
            delegation: DelegationPolicy::default(),
            max_output_bytes: 1_000_000,
        }
    }

    #[test]
    fn provider_label_falls_back_to_huggingface_when_unset() {
        assert_eq!(defaults().provider_label(), "huggingface");
    }

    #[test]
    fn provider_label_reflects_an_explicit_sub_provider() {
        let mut d = defaults();
        d.sub_provider = Some("together".to_string());
        assert_eq!(d.provider_label(), "together");
    }

    #[test]
    fn http_enabled_overrides_only_the_enabled_flag() {
        let mut d = defaults();
        d.http.rate_limit_per_host_per_min = 7;
        let spec = build_spec(&d, Path::new("/tmp"), "t".to_string(), false);
        assert!(!spec.agent.http.enabled);
        assert_eq!(spec.agent.http.rate_limit_per_host_per_min, 7);
    }

    #[test]
    fn build_spec_carries_the_title_and_root_through() {
        let d = defaults();
        let spec = build_spec(&d, Path::new("/work/api"), "api".to_string(), true);
        assert_eq!(spec.title, "api");
        assert_eq!(spec.root, PathBuf::from("/work/api"));
        assert_eq!(spec.agent.model, "org/model");
    }
}
